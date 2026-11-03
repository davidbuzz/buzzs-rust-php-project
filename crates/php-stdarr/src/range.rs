//! `range()` with the PHP 8.3+ algorithm: int, float and byte ranges, step
//! validation and the exact `ValueError`s and warnings.

use crate::util::{arg_value_error, warn};
use crate::HT_MAX_SIZE;
use php_runtime::convert::{dval_to_lval_silent, is_long_compatible};
use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{HashTable, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr};

/// The interpretation of a `range()` bound (`php_range_process_input`).
/// The order matters: it mirrors `IS_LONG < IS_DOUBLE < IS_STRING < IS_ARRAY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Long,
    Double,
    /// A non-numeric string (only its first byte is used).
    Str,
    /// A one-byte numeric string (a digit): ambiguous between int and char.
    Digit,
}

struct Input {
    kind: Kind,
    lval: i64,
    dval: f64,
}

fn finite_check(arg: usize, d: f64) -> RtResult<()> {
    let name = if arg == 1 { "start" } else { "end" };
    if d.is_infinite() {
        return Err(arg_value_error(
            "range",
            arg,
            Some(name),
            "must be a finite number, INF provided",
        ));
    }
    if d.is_nan() {
        return Err(arg_value_error(
            "range",
            arg,
            Some(name),
            "must be a finite number, NAN provided",
        ));
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn process_input<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    v: &Value,
    arg: usize,
) -> RtResult<Input> {
    let name = if arg == 1 { "start" } else { "end" };
    match v {
        Value::Int(l) => Ok(Input {
            kind: Kind::Long,
            lval: *l,
            dval: *l as f64,
        }),
        Value::Float(d) => {
            finite_check(arg, *d)?;
            Ok(Input {
                kind: Kind::Double,
                lval: 0,
                dval: *d,
            })
        }
        Value::Str(s) => {
            if s.is_empty() {
                warn(
                    ctx,
                    "range",
                    &format!("Argument #{arg} (${name}) must not be empty, casted to 0"),
                )?;
                return Ok(Input {
                    kind: Kind::Long,
                    lval: 0,
                    dval: 0.0,
                });
            }
            match is_numeric_string(s.as_bytes(), false) {
                Some(Number::Float(d)) => {
                    finite_check(arg, d)?;
                    Ok(Input {
                        kind: Kind::Double,
                        lval: 0,
                        dval: d,
                    })
                }
                Some(Number::Int(l)) => Ok(Input {
                    kind: if s.len() == 1 {
                        Kind::Digit
                    } else {
                        Kind::Long
                    },
                    lval: l,
                    dval: l as f64,
                }),
                None => {
                    if s.len() != 1 {
                        warn(
                            ctx,
                            "range",
                            &format!(
                                "Argument #{arg} (${name}) must be a single byte, subsequent bytes are ignored"
                            ),
                        )?;
                    }
                    Ok(Input {
                        kind: Kind::Str,
                        lval: 0,
                        dval: 0.0,
                    })
                }
            }
        }
        Value::Reference(r) => process_input(ctx, &r.get(), arg),
        other => Err(RuntimeError::TypeError(format!(
            "range(): Argument #{arg} (${name}) must be of type string|int|float, {} given",
            other.value_name()
        ))),
    }
}

fn boundary_error() -> RuntimeError {
    arg_value_error(
        "range",
        3,
        Some("step"),
        "must be less than the range spanned by argument #1 ($start) and argument #2 ($end)",
    )
}

fn negative_step_error() -> RuntimeError {
    arg_value_error(
        "range",
        3,
        Some("step"),
        "must be greater than 0 for increasing ranges",
    )
}

fn first_byte(v: &Value) -> u8 {
    match v {
        Value::Str(s) => s.as_bytes().first().copied().unwrap_or(0),
        Value::Reference(r) => first_byte(&r.get()),
        _ => 0,
    }
}

/// `RANGE_CHECK_DOUBLE_INIT_ARRAY(start, end, step)`: the element count.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names
)]
fn double_size(start: f64, end: f64, step: f64) -> RtResult<u32> {
    let calc = ((start - end) / step) + 1.0;
    let max = f64::from(HT_MAX_SIZE);
    if calc >= max {
        return Err(RuntimeError::ValueError(format!(
            "The supplied range exceeds the maximum array size by {:.1} elements: start={:.1}, end={:.1}, step={:.1}. Max size: {:.0}",
            calc - max,
            end,
            start,
            step,
            max
        )));
    }
    Ok(calc.round() as u32)
}

/// `RANGE_CHECK_LONG_INIT_ARRAY(start, end, step)`: the element count.
#[allow(clippy::cast_sign_loss, clippy::similar_names)]
fn long_size(start: i64, end: i64, step: u64) -> RtResult<u32> {
    let calc = (start as u64).wrapping_sub(end as u64) / step;
    let limit = u64::from(HT_MAX_SIZE) - 1;
    if calc >= limit {
        return Err(RuntimeError::ValueError(format!(
            "The supplied range exceeds the maximum array size by {} elements: start={end}, end={start}, step={step}. Calculated size: {calc}. Maximum size: {HT_MAX_SIZE}.",
            calc - limit
        )));
    }
    Ok(u32::try_from(calc + 1).unwrap_or(u32::MAX))
}

/// `range(string|int|float $start, string|int|float $end, int|float $step = 1): array`.
#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub fn range<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    start: &Value,
    end: &Value,
    step: Option<&Value>,
) -> RtResult<ZArray> {
    let mut is_step_double = false;
    let mut is_step_negative = false;
    let mut step_double = 1.0f64;
    let mut step_long: i64 = 1;
    if let Some(s) = step {
        match crate::util::deref(s) {
            Value::Float(d) => {
                if d.is_infinite() {
                    return Err(arg_value_error(
                        "range",
                        3,
                        Some("step"),
                        "must be a finite number, INF provided",
                    ));
                }
                if d.is_nan() {
                    return Err(arg_value_error(
                        "range",
                        3,
                        Some("step"),
                        "must be a finite number, NAN provided",
                    ));
                }
                step_double = d;
                if step_double < 0.0 {
                    is_step_negative = true;
                    step_double = -step_double;
                }
                step_long = dval_to_lval_silent(step_double);
                if !is_long_compatible(step_double, step_long) {
                    is_step_double = true;
                }
            }
            Value::Int(l) => {
                step_long = l;
                if step_long < 0 {
                    if step_long == i64::MIN {
                        return Err(arg_value_error(
                            "range",
                            3,
                            Some("step"),
                            &format!("must be greater than {step_long}"),
                        ));
                    }
                    is_step_negative = true;
                    step_long = -step_long;
                }
                step_double = step_long as f64;
            }
            other => {
                return Err(RuntimeError::TypeError(format!(
                    "range(): Argument #3 ($step) must be of type int|float, {} given",
                    other.value_name()
                )))
            }
        }
        if step_double == 0.0 {
            return Err(arg_value_error("range", 3, Some("step"), "cannot be 0"));
        }
    }

    let mut s = process_input(ctx, start, 1)?;
    let mut e = process_input(ctx, end, 2)?;

    if s.kind >= Kind::Str || e.kind >= Kind::Str {
        if s.kind < Kind::Str || e.kind < Kind::Str {
            if s.kind < Kind::Str {
                if e.kind != Kind::Digit {
                    warn(
                        ctx,
                        "range",
                        "Argument #1 ($start) must be a single byte string if argument #2 ($end) is a single byte string, argument #2 ($end) converted to 0",
                    )?;
                }
                e.kind = Kind::Long;
            } else if e.kind < Kind::Str {
                if s.kind != Kind::Digit {
                    warn(
                        ctx,
                        "range",
                        "Argument #2 ($end) must be a single byte string if argument #1 ($start) is a single byte string, argument #1 ($start) converted to 0",
                    )?;
                }
                s.kind = Kind::Long;
            }
        } else if is_step_double {
            if s.kind == Kind::Str || e.kind == Kind::Str {
                warn(
                    ctx,
                    "range",
                    "Argument #3 ($step) must be of type int when generating an array of characters, inputs converted to 0",
                )?;
            }
            s.kind = Kind::Long;
            e.kind = Kind::Long;
        } else {
            let mut low = i64::from(first_byte(start));
            let high = i64::from(first_byte(end));
            let mut out = HashTable::new();
            let push = |out: &mut HashTable, c: i64| {
                let b = u8::try_from(c).unwrap_or(0);
                let _ = out.push(Value::Str(ZStr::from_byte(b)));
            };
            if low > high {
                if low - high < step_long {
                    return Err(boundary_error());
                }
                while low >= high {
                    push(&mut out, low);
                    low -= step_long;
                }
            } else if high > low {
                if is_step_negative {
                    return Err(negative_step_error());
                }
                if high - low < step_long {
                    return Err(boundary_error());
                }
                while low <= high {
                    push(&mut out, low);
                    low += step_long;
                }
            } else {
                push(&mut out, low);
            }
            return Ok(ZArray::from_table(out));
        }
    }

    // handle_numeric_inputs
    let mut out = HashTable::new();
    if s.kind == Kind::Double || e.kind == Kind::Double || is_step_double {
        let (sd, ed) = (s.dval, e.dval);
        if sd > ed {
            if sd - ed < step_double {
                return Err(boundary_error());
            }
            let size = double_size(sd, ed, step_double)?;
            let mut i = 0u32;
            let mut element = sd;
            while i < size && element >= ed {
                let _ = out.push(Value::Float(element));
                i += 1;
                element = sd - (f64::from(i) * step_double);
            }
        } else if ed > sd {
            if is_step_negative {
                return Err(negative_step_error());
            }
            if ed - sd < step_double {
                return Err(boundary_error());
            }
            let size = double_size(ed, sd, step_double)?;
            let mut i = 0u32;
            let mut element = sd;
            while i < size && element <= ed {
                let _ = out.push(Value::Float(element));
                i += 1;
                element = sd + (f64::from(i) * step_double);
            }
        } else {
            let _ = out.push(Value::Float(sd));
        }
    } else {
        let (sl, el) = (s.lval, e.lval);
        let ustep = step_long as u64;
        if sl > el {
            if (sl as u64).wrapping_sub(el as u64) < ustep {
                return Err(boundary_error());
            }
            let size = long_size(sl, el, ustep)?;
            for i in 0..u64::from(size) {
                let _ = out.push(Value::Int(
                    (sl as u64).wrapping_sub(i.wrapping_mul(ustep)) as i64
                ));
            }
        } else if el > sl {
            if is_step_negative {
                return Err(negative_step_error());
            }
            if (el as u64).wrapping_sub(sl as u64) < ustep {
                return Err(boundary_error());
            }
            let size = long_size(el, sl, ustep)?;
            for i in 0..u64::from(size) {
                let _ = out.push(Value::Int(
                    (sl as u64).wrapping_add(i.wrapping_mul(ustep)) as i64
                ));
            }
        } else {
            let _ = out.push(Value::Int(sl));
        }
    }
    Ok(ZArray::from_table(out))
}
