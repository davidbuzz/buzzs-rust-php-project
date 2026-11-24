// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP operators (`zend_operators.c`): arithmetic, bitwise, concatenation,
//! comparison, increment/decrement and logical operators.
//!
//! Every function dereferences [`Value::Reference`] operands, returns the
//! result value, reports diagnostics through the [`RuntimeContext`] and throws
//! via `Err(RuntimeError)` with PHP's exact messages.

#![allow(
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::single_match_else,
    clippy::match_same_arms
)]

use crate::array::{HashTable, ZArray};
use crate::convert::{
    double_to_zstr, dval_to_lval_safe, long_to_zstr, nan_coerced_warning, number_value,
    try_convert_to_number, try_get_long,
};
use crate::error::{BinaryOp, CastTarget, ErrorLevel, RtResult, RuntimeContext, RuntimeError};
use crate::float::{format_double_buf, DoubleBuf};
use crate::numeric::{is_numeric_string_ex, Number};
use crate::string::{binary_strcmp, format_long, ZStr, LONG_MAX_LEN};
use crate::value::{Obj, Value};
use std::borrow::Cow;
use std::cell::RefCell;

/// Dereferences a value without cloning when it is not a reference.
fn deref(v: &Value) -> Cow<'_, Value> {
    match v {
        Value::Reference(r) => Cow::Owned(r.get()),
        other => Cow::Borrowed(other),
    }
}

fn binop_error(op: BinaryOp, a: &Value, b: &Value) -> RuntimeError {
    RuntimeError::TypeError(format!(
        "Unsupported operand types: {} {} {}",
        a.type_name(),
        op.sigil(),
        b.type_name()
    ))
}

/// Tries the `do_operation` handler of object operands (op1 first).
fn try_object_op(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Option<Value>> {
    if matches!(a, Value::Object(_)) || matches!(b, Value::Object(_)) {
        return ctx.do_operation(op, a, b);
    }
    Ok(None)
}

#[inline]
fn three_way_f(a: f64, b: f64) -> i32 {
    // ZEND_THREEWAY_COMPARE: NaN compares as "greater".
    if a == b {
        0
    } else if a < b {
        -1
    } else {
        1
    }
}

#[inline]
fn three_way_i(a: i64, b: i64) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

#[allow(clippy::cast_precision_loss)]
fn arith_fast(op: BinaryOp, a: &Value, b: &Value) -> Option<Value> {
    use Value::{Float, Int};
    Some(match (op, a, b) {
        (BinaryOp::Add, Int(x), Int(y)) => match x.checked_add(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 + *y as f64),
        },
        (BinaryOp::Sub, Int(x), Int(y)) => match x.checked_sub(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 - *y as f64),
        },
        (BinaryOp::Mul, Int(x), Int(y)) => match x.checked_mul(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 * *y as f64),
        },
        (_, Float(_) | Int(_), Float(_) | Int(_)) => {
            let (x, y) = (as_f64(a), as_f64(b));
            Float(match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                _ => return None,
            })
        }
        _ => return None,
    })
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

fn arith(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    if let Some(r) = arith_fast(op, a, b) {
        return Ok(r);
    }
    let (a, b) = (deref(a), deref(b));
    if let Some(r) = arith_fast(op, &a, &b) {
        return Ok(r);
    }
    if op == BinaryOp::Add {
        if let (Value::Array(x), Value::Array(y)) = (&*a, &*b) {
            return Ok(Value::Array(array_union(x, y)));
        }
    }
    if let Some(r) = try_object_op(op, &a, &b, ctx)? {
        return Ok(r);
    }
    let Some(x) = try_convert_to_number(&a, ctx)? else {
        return Err(binop_error(op, &a, &b));
    };
    let Some(y) = try_convert_to_number(&b, ctx)? else {
        return Err(binop_error(op, &a, &b));
    };
    arith_fast(op, &x, &y).ok_or_else(|| binop_error(op, &a, &b))
}

/// The array union `$a + $b`: keys of `$b` missing from `$a` are appended.
#[must_use]
pub fn array_union(a: &ZArray, b: &ZArray) -> ZArray {
    if ZArray::ptr_eq(a, b) {
        return a.clone();
    }
    let mut out = a.clone();
    out.make_mut().merge(b, false);
    out
}

/// `+` (`add_function`).
pub fn add(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    arith(BinaryOp::Add, a, b, ctx)
}

/// `-` (`sub_function`).
pub fn sub(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    arith(BinaryOp::Sub, a, b, ctx)
}

/// `*` (`mul_function`).
pub fn mul(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    arith(BinaryOp::Mul, a, b, ctx)
}

fn safe_pow(base: f64, exp: f64, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<f64> {
    if base == 0.0 && exp < 0.0 {
        ctx.diagnostic(
            ErrorLevel::Deprecated,
            "Power of base 0 and negative exponent is deprecated",
        )?;
    }
    Ok(base.powf(exp))
}

#[allow(clippy::cast_precision_loss)]
fn pow_base(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Option<Value>> {
    Ok(Some(match (a, b) {
        (Value::Int(base), Value::Int(exp)) => {
            if *exp >= 0 {
                let (mut l1, mut l2, mut i) = (1i64, *base, *exp);
                if i == 0 {
                    return Ok(Some(Value::Int(1)));
                } else if l2 == 0 {
                    return Ok(Some(Value::Int(0)));
                }
                while i >= 1 {
                    if i % 2 != 0 {
                        i -= 1;
                        match l1.checked_mul(l2) {
                            Some(r) => l1 = r,
                            None => {
                                let d = l1 as f64 * l2 as f64;
                                return Ok(Some(Value::Float(
                                    d * safe_pow(l2 as f64, i as f64, ctx)?,
                                )));
                            }
                        }
                    } else {
                        i /= 2;
                        match l2.checked_mul(l2) {
                            Some(r) => l2 = r,
                            None => {
                                let d = l2 as f64 * l2 as f64;
                                return Ok(Some(Value::Float(
                                    l1 as f64 * safe_pow(d, i as f64, ctx)?,
                                )));
                            }
                        }
                    }
                }
                Value::Int(l1)
            } else {
                Value::Float(safe_pow(*base as f64, *exp as f64, ctx)?)
            }
        }
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            Value::Float(safe_pow(as_f64(a), as_f64(b), ctx)?)
        }
        _ => return Ok(None),
    }))
}

/// `**` (`pow_function`).
pub fn pow(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let (a, b) = (deref(a), deref(b));
    if let Some(r) = pow_base(&a, &b, ctx)? {
        return Ok(r);
    }
    if let Some(r) = try_object_op(BinaryOp::Pow, &a, &b, ctx)? {
        return Ok(r);
    }
    let Some(x) = try_convert_to_number(&a, ctx)? else {
        return Err(binop_error(BinaryOp::Pow, &a, &b));
    };
    let Some(y) = try_convert_to_number(&b, ctx)? else {
        return Err(binop_error(BinaryOp::Pow, &a, &b));
    };
    pow_base(&x, &y, ctx)?.ok_or_else(|| binop_error(BinaryOp::Pow, &a, &b))
}

enum DivStatus {
    Ok(Value),
    ByZero,
    Unhandled,
}

#[allow(clippy::cast_precision_loss)]
fn div_base(a: &Value, b: &Value) -> DivStatus {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => {
            if *y == 0 {
                DivStatus::ByZero
            } else if *y == -1 && *x == i64::MIN {
                DivStatus::Ok(Value::Float(i64::MIN as f64 / -1.0))
            } else if x % y == 0 {
                DivStatus::Ok(Value::Int(x / y))
            } else {
                DivStatus::Ok(Value::Float(*x as f64 / *y as f64))
            }
        }
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let y = as_f64(b);
            if y == 0.0 {
                DivStatus::ByZero
            } else {
                DivStatus::Ok(Value::Float(as_f64(a) / y))
            }
        }
        _ => DivStatus::Unhandled,
    }
}

fn division_by_zero() -> RuntimeError {
    RuntimeError::DivisionByZeroError("Division by zero".to_string())
}

/// `/` (`div_function`).
pub fn div(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let (a, b) = (deref(a), deref(b));
    match div_base(&a, &b) {
        DivStatus::Ok(v) => return Ok(v),
        DivStatus::ByZero => return Err(division_by_zero()),
        DivStatus::Unhandled => {}
    }
    if let Some(r) = try_object_op(BinaryOp::Div, &a, &b, ctx)? {
        return Ok(r);
    }
    let Some(x) = try_convert_to_number(&a, ctx)? else {
        return Err(binop_error(BinaryOp::Div, &a, &b));
    };
    let Some(y) = try_convert_to_number(&b, ctx)? else {
        return Err(binop_error(BinaryOp::Div, &a, &b));
    };
    match div_base(&x, &y) {
        DivStatus::Ok(v) => Ok(v),
        _ => Err(division_by_zero()),
    }
}

/// `convert_op1_op2_long`: converts both operands of an integer operator.
fn long_operands(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Result<(i64, i64), Value>> {
    if let (Value::Int(x), Value::Int(y)) = (a, b) {
        return Ok(Ok((*x, *y)));
    }
    let (a, b) = (deref(a), deref(b));
    let x = if let Value::Int(x) = &*a {
        *x
    } else {
        if let Value::Object(_) = &*a {
            if let Some(r) = ctx.do_operation(op, &a, &b)? {
                return Ok(Err(r));
            }
        }
        match try_get_long(&a, ctx)? {
            Some(x) => x,
            None => return Err(binop_error(op, &a, &b)),
        }
    };
    let y = if let Value::Int(y) = &*b {
        *y
    } else {
        if let Value::Object(_) = &*b {
            if let Some(r) = ctx.do_operation(op, &a, &b)? {
                return Ok(Err(r));
            }
        }
        match try_get_long(&b, ctx)? {
            Some(y) => y,
            None => return Err(binop_error(op, &a, &b)),
        }
    };
    Ok(Ok((x, y)))
}

/// `%` (`mod_function`). Throws `DivisionByZeroError("Modulo by zero")`.
pub fn modulo(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let (x, y) = match long_operands(BinaryOp::Mod, a, b, ctx)? {
        Ok(p) => p,
        Err(v) => return Ok(v),
    };
    if y == 0 {
        return Err(RuntimeError::DivisionByZeroError(
            "Modulo by zero".to_string(),
        ));
    }
    if y == -1 {
        return Ok(Value::Int(0));
    }
    Ok(Value::Int(x % y))
}

/// `intdiv()` semantics on integers.
pub fn intdiv(a: i64, b: i64) -> RtResult<i64> {
    if b == 0 {
        return Err(division_by_zero());
    }
    if b == -1 && a == i64::MIN {
        return Err(RuntimeError::ArithmeticError(
            "Division of PHP_INT_MIN by -1 is not an integer".to_string(),
        ));
    }
    Ok(a / b)
}

/// `fmod()` semantics (C `fmod`).
#[must_use]
pub fn fmod(a: f64, b: f64) -> f64 {
    a % b
}

fn negative_shift() -> RuntimeError {
    RuntimeError::ArithmeticError("Bit shift by negative number".to_string())
}

/// `<<` (`shift_left_function`).
pub fn shift_left(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    let (x, y) = match long_operands(BinaryOp::ShiftLeft, a, b, ctx)? {
        Ok(p) => p,
        Err(v) => return Ok(v),
    };
    if !(0..64).contains(&y) {
        return if y > 0 {
            Ok(Value::Int(0))
        } else {
            Err(negative_shift())
        };
    }
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation
    )]
    Ok(Value::Int(((x as u64) << (y as u32)) as i64))
}

/// `>>` (`shift_right_function`).
pub fn shift_right(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    let (x, y) = match long_operands(BinaryOp::ShiftRight, a, b, ctx)? {
        Ok(p) => p,
        Err(v) => return Ok(v),
    };
    if !(0..64).contains(&y) {
        return if y > 0 {
            Ok(Value::Int(if x < 0 { -1 } else { 0 }))
        } else {
            Err(negative_shift())
        };
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(Value::Int(x >> (y as u32)))
}

fn bitwise(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    let f = |x: i64, y: i64| match op {
        BinaryOp::BitOr => x | y,
        BinaryOp::BitAnd => x & y,
        _ => x ^ y,
    };
    if let (Value::Int(x), Value::Int(y)) = (a, b) {
        return Ok(Value::Int(f(*x, *y)));
    }
    let (da, db) = (deref(a), deref(b));
    if let (Value::Str(x), Value::Str(y)) = (&*da, &*db) {
        let (x, y) = (x.as_bytes(), y.as_bytes());
        let (longer, shorter) = if x.len() >= y.len() { (x, y) } else { (y, x) };
        let fb = |p: u8, q: u8| match op {
            BinaryOp::BitOr => p | q,
            BinaryOp::BitAnd => p & q,
            _ => p ^ q,
        };
        let mut out: Vec<u8> = shorter
            .iter()
            .zip(longer)
            .map(|(p, q)| fb(*p, *q))
            .collect();
        if op == BinaryOp::BitOr {
            out.extend_from_slice(&longer[shorter.len()..]);
        }
        return Ok(Value::Str(ZStr::new(out)));
    }
    match long_operands(op, &da, &db, ctx)? {
        Ok((x, y)) => Ok(Value::Int(f(x, y))),
        Err(v) => Ok(v),
    }
}

/// `|` (`bitwise_or_function`), including the string variant.
pub fn bit_or(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    bitwise(BinaryOp::BitOr, a, b, ctx)
}

/// `&` (`bitwise_and_function`), including the string variant.
pub fn bit_and(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    bitwise(BinaryOp::BitAnd, a, b, ctx)
}

/// `^` (`bitwise_xor_function`), including the string variant.
pub fn bit_xor(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    bitwise(BinaryOp::BitXor, a, b, ctx)
}

/// `~` (`bitwise_not_function`).
pub fn bit_not(a: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let a = deref(a);
    match &*a {
        Value::Int(i) => Ok(Value::Int(!i)),
        Value::Float(d) => Ok(Value::Int(!dval_to_lval_safe(*d, ctx)?)),
        Value::Str(s) => Ok(Value::Str(ZStr::new(
            s.as_bytes().iter().map(|c| !c).collect::<Vec<u8>>(),
        ))),
        other => {
            if let Value::Object(_) = other {
                if let Some(r) = ctx.do_operation(BinaryOp::BitNot, other, &Value::Null)? {
                    return Ok(r);
                }
            }
            Err(RuntimeError::TypeError(format!(
                "Cannot perform bitwise not on {}",
                other.value_name()
            )))
        }
    }
}

/// `!` (`boolean_not_function`).
pub fn bool_not(a: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    Ok(Value::Bool(!a.to_bool(ctx)?))
}

/// `xor` (`boolean_xor_function`).
pub fn bool_xor(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let x = a.to_bool(ctx)?;
    let y = b.to_bool(ctx)?;
    Ok(Value::Bool(x ^ y))
}

/// Stack room for the string form of a scalar operand.
#[derive(Debug, Default)]
struct ScalarText {
    long: [u8; LONG_MAX_LEN],
    double: DoubleBuf,
}

/// The string form of a value whose conversion runs no user code (`Str`,
/// `Int`, `Float`, `Bool`, `Null`), borrowed or formatted into `buf`
/// (`zend_print_long_to_buf`, `zend_gcvt` into a stack buffer) without
/// allocating. `None` for other values and for floats under a `precision`
/// above 17. A NaN float's warning is left to the caller.
#[inline]
fn plain_str_bytes<'a>(v: &'a Value, precision: i64, buf: &'a mut ScalarText) -> Option<&'a [u8]> {
    match v {
        Value::Str(s) => Some(s.as_bytes()),
        Value::Int(i) => Some(format_long(*i, &mut buf.long)),
        Value::Float(d) => format_double_buf(*d, precision, false, &mut buf.double),
        Value::Null | Value::Bool(false) => Some(b""),
        Value::Bool(true) => Some(b"1"),
        _ => None,
    }
}

/// [`Value::to_zstr`] of a value [`plain_str_bytes`] accepted, from its
/// text, without diagnostics (a float's NaN warning was already raised).
fn plain_to_zstr(v: &Value, text: &[u8]) -> ZStr {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(i) => long_to_zstr(*i),
        Value::Bool(true) => ZStr::from_byte(b'1'),
        Value::Null | Value::Bool(false) => ZStr::empty(),
        // `zend_double_to_str`: ASCII, flagged `IS_STR_VALID_UTF8`.
        _ => ZStr::from_ascii(text),
    }
}

/// Whether the text [`plain_str_bytes`] gives for `v` is known to be valid
/// UTF-8 (`ZSTR_GET_COPYABLE_CONCAT_PROPERTIES` of `zval_get_string(v)`):
/// a string's flag; the formatted numbers and bools are ASCII.
#[inline]
fn plain_valid_utf8(v: &Value) -> bool {
    match v {
        Value::Str(s) => s.is_valid_utf8(),
        _ => true,
    }
}

/// `.` (`concat_function`).
pub fn concat(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let (da, db) = (deref(a), deref(b));
    // Strings, numbers, bools and null: one exactly sized result, with no
    // intermediate string for a number operand (php-src converts with
    // `zval_get_string`, whose `zend_long_to_str` / `zend_double_to_str`
    // format into a stack buffer first).
    let precision = if matches!(&*da, Value::Float(_)) || matches!(&*db, Value::Float(_)) {
        ctx.precision()
    } else {
        14
    };
    let (mut ba, mut bb) = (ScalarText::default(), ScalarText::default());
    if let (Some(x), Some(y)) = (
        plain_str_bytes(&da, precision, &mut ba),
        plain_str_bytes(&db, precision, &mut bb),
    ) {
        // `zend_double_to_str`'s NaN warning, operand by operand.
        for v in [&*da, &*db] {
            if matches!(v, Value::Float(d) if d.is_nan()) {
                nan_coerced_warning(ctx, "string")?;
            }
        }
        if x.is_empty() {
            return Ok(Value::Str(plain_to_zstr(&db, y)));
        }
        if y.is_empty() {
            return Ok(Value::Str(plain_to_zstr(&da, x)));
        }
        // `GC_ADD_FLAGS(result_str, ZSTR_GET_COPYABLE_CONCAT_PROPERTIES_BOTH)`.
        let valid_utf8 = plain_valid_utf8(&da) && plain_valid_utf8(&db);
        return Ok(Value::Str(ZStr::concat_utf8(x, y, valid_utf8)));
    }
    if !matches!(&*da, Value::Str(_)) || !matches!(&*db, Value::Str(_)) {
        if let Some(r) = try_object_op(BinaryOp::Concat, &da, &db, ctx)? {
            return Ok(r);
        }
    }
    let x = da.to_zstr(ctx)?;
    let y = db.to_zstr(ctx)?;
    if x.is_empty() {
        return Ok(Value::Str(y));
    }
    if y.is_empty() {
        return Ok(Value::Str(x));
    }
    Ok(Value::Str(ZStr::concat_str(&x, &y)))
}

/// In-place `.=` that appends to a uniquely owned string buffer when
/// possible (amortized O(1) appends in loops).
pub fn concat_assign(
    target: &mut Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<()> {
    if let Value::Reference(r) = target {
        let mut inner = r.get();
        concat_assign(&mut inner, b, ctx)?;
        r.set(inner);
        return Ok(());
    }
    if let (Value::Str(_), Value::Str(y)) = (&*target, &*deref(b)) {
        if let Value::Str(x) = target {
            x.append_str(y);
        }
        return Ok(());
    }
    let r = concat(target, b, ctx)?;
    *target = r;
    Ok(())
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// `zendi_smart_strcmp`: compares two strings numerically when both are
/// numeric, else bytewise. Returns -1, 0 or 1.
#[must_use]
pub fn smart_strcmp(s1: &[u8], s2: &[u8]) -> i32 {
    // As in `smart_streq`: a string starting above `'9'` is not numeric,
    // and the second string is parsed only when the first one is.
    if s1.first().is_some_and(|&c| c > b'9') || s2.first().is_some_and(|&c| c > b'9') {
        return binary_strcmp(s1, s2).signum();
    }
    if let Some((n1, n2)) = is_numeric_string_ex(s1, false)
        .and_then(|n1| is_numeric_string_ex(s2, false).map(|n2| (n1, n2)))
    {
        let (o1, o2) = (n1.overflow, n2.overflow);
        let (d1, d2) = (n1.value.to_f64(), n2.value.to_f64());
        if !(o1 != 0 && o1 == o2 && d1 - d2 == 0.0) {
            match (n1.value, n2.value) {
                (Number::Int(l1), Number::Int(l2)) => return three_way_i(l1, l2),
                (Number::Int(_), Number::Float(_)) if o2 != 0 => return -i32::from(o2),
                (Number::Float(_), Number::Int(_)) if o1 != 0 => return i32::from(o1),
                (Number::Float(a), Number::Float(b)) if a == b && !a.is_finite() => {}
                _ => {
                    let d = d1 - d2;
                    return i32::from(d > 0.0) - i32::from(d < 0.0);
                }
            }
        }
    }
    binary_strcmp(s1, s2).signum()
}

/// `zendi_smart_streq`: `==` between two strings, behind the test of
/// `zend_fast_equal_strings`: a string whose first byte is above `'9'`
/// cannot be numeric (a numeric string starts with whitespace, a sign, a
/// digit or a dot), so the bytes decide, and the second string is parsed
/// only when the first one is numeric.
#[must_use]
pub fn smart_streq(s1: &[u8], s2: &[u8]) -> bool {
    if s1.first().is_some_and(|&c| c > b'9') || s2.first().is_some_and(|&c| c > b'9') {
        return s1 == s2;
    }
    if let Some((n1, n2)) = is_numeric_string_ex(s1, false)
        .and_then(|n1| is_numeric_string_ex(s2, false).map(|n2| (n1, n2)))
    {
        let (o1, o2) = (n1.overflow, n2.overflow);
        let (d1, d2) = (n1.value.to_f64(), n2.value.to_f64());
        if !(o1 != 0 && o1 == o2 && d1 - d2 == 0.0) {
            match (n1.value, n2.value) {
                (Number::Int(l1), Number::Int(l2)) => return l1 == l2,
                (Number::Int(_), Number::Float(_)) if o2 != 0 => return false,
                (Number::Float(_), Number::Int(_)) if o1 != 0 => return false,
                (Number::Float(a), Number::Float(b)) if a == b && !a.is_finite() => {}
                _ => return d1 == d2,
            }
        }
    }
    s1 == s2
}

#[allow(clippy::cast_precision_loss)]
fn compare_long_to_string(l: i64, s: &ZStr) -> i32 {
    match is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value) {
        Some(Number::Int(sl)) => three_way_i(l, sl),
        Some(Number::Float(sd)) => three_way_f(l as f64, sd),
        None => binary_strcmp(long_to_zstr(l).as_bytes(), s.as_bytes()).signum(),
    }
}

#[allow(clippy::cast_precision_loss)]
fn compare_double_to_string(
    d: f64,
    s: &ZStr,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<i32> {
    Ok(
        match is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value) {
            Some(Number::Int(sl)) => three_way_f(d, sl as f64),
            Some(Number::Float(sd)) => three_way_f(d, sd),
            None => binary_strcmp(double_to_zstr(d, ctx)?.as_bytes(), s.as_bytes()).signum(),
        },
    )
}

thread_local! {
    /// Arrays/objects currently protected, per guard kind.
    static PROTECTED: RefCell<Vec<(GuardKind, usize)>> = const { RefCell::new(Vec::new()) };
}

/// Which recursion protection a [`RecursionGuard`] uses. PHP keeps separate
/// flags per purpose (`GC_PROTECT_RECURSION` for arrays and comparisons,
/// object guards for debug/export/json), so nesting e.g. a comparison inside
/// `json_encode` does not report false recursion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuardKind {
    /// Array/object comparison (`zend_hash_compare`, `zend_std_compare_objects`).
    Compare,
    /// `var_dump` / `print_r` / `debug_zval_dump`.
    Debug,
    /// `var_export`.
    Export,
    /// `json_encode`.
    Json,
    /// `serialize`.
    Serialize,
}

/// RAII recursion protection keyed by an identity address (see
/// [`crate::ZArray::addr`], [`Obj::addr`]); dropping the guard unprotects.
#[derive(Debug)]
pub struct RecursionGuard {
    kind: GuardKind,
    addr: usize,
}

impl RecursionGuard {
    /// Protects `addr`, or returns `None` if it is already protected for
    /// `kind` (i.e. recursion was detected).
    #[must_use]
    pub fn try_enter(kind: GuardKind, addr: usize) -> Option<Self> {
        PROTECTED.with(|c| {
            let mut c = c.borrow_mut();
            if c.contains(&(kind, addr)) {
                return None;
            }
            c.push((kind, addr));
            Some(RecursionGuard { kind, addr })
        })
    }

    /// Whether `addr` is currently protected for `kind`.
    #[must_use]
    pub fn is_protected(kind: GuardKind, addr: usize) -> bool {
        PROTECTED.with(|c| c.borrow().contains(&(kind, addr)))
    }

    /// Comparison variant: fails with "Nesting level too deep - recursive
    /// dependency?" on recursion.
    fn enter(addr: usize) -> RtResult<Self> {
        Self::try_enter(GuardKind::Compare, addr).ok_or_else(|| {
            RuntimeError::Error("Nesting level too deep - recursive dependency?".to_string())
        })
    }
}

impl Drop for RecursionGuard {
    fn drop(&mut self) {
        PROTECTED.with(|c| {
            let mut c = c.borrow_mut();
            if let Some(p) = c.iter().rposition(|e| *e == (self.kind, self.addr)) {
                c.remove(p);
            }
        });
    }
}

/// Type order used by the comparison fallbacks (`Z_TYPE`).
fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 1,
        Value::Bool(false) => 2,
        Value::Bool(true) => 3,
        Value::Int(_) => 4,
        Value::Float(_) => 5,
        Value::Str(_) => 6,
        Value::Array(_) => 7,
        Value::Object(_) => 8,
        Value::Resource(_) => 9,
        Value::Reference(_) => 10,
    }
}

/// `_zendi_convert_scalar_to_number_silent`.
fn to_number_silent(v: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    Ok(match v {
        Value::Null | Value::Bool(false) => Value::Int(0),
        Value::Bool(true) => Value::Int(1),
        Value::Str(s) => is_numeric_string_ex(s.as_bytes(), true)
            .map_or(Value::Int(0), |n| number_value(n.value)),
        Value::Resource(r) => Value::Int(r.handle()),
        Value::Object(o) => match ctx.cast_object(o, CastTarget::Number)? {
            Some(v @ (Value::Int(_) | Value::Float(_))) => v,
            _ => Value::Int(1),
        },
        other => other.clone(),
    })
}

/// `zend_compare`: the spaceship operator `<=>`. Returns -1, 0 or 1
/// (uncomparable values return 1).
pub fn compare(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<i32> {
    let (a, b) = (deref(a), deref(b));
    compare_inner(&a, &b, false, ctx)
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn compare_inner(
    a: &Value,
    b: &Value,
    converted: bool,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<i32> {
    use Value::{Array, Bool, Float, Int, Null, Object, Str};
    match (a, b) {
        (Int(x), Int(y)) => return Ok(three_way_i(*x, *y)),
        (Float(x), Int(y)) => return Ok(three_way_f(*x, *y as f64)),
        (Int(x), Float(y)) => return Ok(three_way_f(*x as f64, *y)),
        (Float(x), Float(y)) => return Ok(three_way_f(*x, *y)),
        (Array(x), Array(y)) => return compare_arrays(x, y, ctx),
        (Null | Bool(false), Null | Bool(false)) | (Bool(true), Bool(true)) => return Ok(0),
        (Null, Bool(true)) => return Ok(-1),
        (Bool(true), Null) => return Ok(1),
        (Str(x), Str(y)) => {
            if ZStr::ptr_eq(x, y) {
                return Ok(0);
            }
            return Ok(smart_strcmp(x.as_bytes(), y.as_bytes()));
        }
        (Null, Str(y)) => return Ok(if y.is_empty() { 0 } else { -1 }),
        (Str(x), Null) => return Ok(i32::from(!x.is_empty())),
        (Int(x), Str(y)) => return Ok(compare_long_to_string(*x, y)),
        (Str(x), Int(y)) => return Ok(-compare_long_to_string(*y, x)),
        (Float(x), Str(y)) => {
            if x.is_nan() {
                return Ok(1);
            }
            return compare_double_to_string(*x, y, ctx);
        }
        (Str(x), Float(y)) => {
            if y.is_nan() {
                return Ok(1);
            }
            return Ok(-compare_double_to_string(*y, x, ctx)?);
        }
        (Object(_), Null) => return Ok(1),
        (Null, Object(_)) => return Ok(-1),
        _ => {}
    }
    if let (Object(_), _) | (_, Object(_)) = (a, b) {
        let (object, other, obj_lhs) = match a {
            Object(o) => (o, b, true),
            _ => match b {
                Object(o) => (o, a, false),
                _ => unreachable!("one operand is an object"),
            },
        };
        if let Object(o2) = other {
            if Obj::ptr_eq(object, o2) {
                return Ok(0);
            }
        } else if let Bool(bv) = other {
            let Some(casted) = ctx.cast_object(object, CastTarget::Bool)? else {
                return Ok(if obj_lhs { 1 } else { -1 });
            };
            let other = Bool(*bv);
            return if obj_lhs {
                compare_inner(&casted, &other, false, ctx)
            } else {
                compare_inner(&other, &casted, false, ctx)
            };
        }
        return ctx.compare_objects(a, b);
    }
    if !converted {
        let nan = |v: &Value| matches!(v, Float(f) if f.is_nan());
        let (ra, rb) = (type_rank(a), type_rank(b));
        if nan(a) || nan(b) {
            if ra < 3 {
                return Ok(-1);
            } else if ra == 3 || rb == 3 {
                return Ok(0);
            } else if rb < 3 {
                return Ok(1);
            } else if !matches!(a, Float(_)) {
                let na = to_number_silent(a, ctx)?;
                return compare_inner(&na, b, true, ctx);
            } else if !matches!(b, Float(_)) {
                let nb = to_number_silent(b, ctx)?;
                return compare_inner(a, &nb, true, ctx);
            }
            return Ok(1);
        } else if ra < 3 {
            return Ok(if b.to_bool(ctx)? { -1 } else { 0 });
        } else if ra == 3 {
            return Ok(i32::from(!b.to_bool(ctx)?));
        } else if rb < 3 {
            return Ok(i32::from(a.to_bool(ctx)?));
        } else if rb == 3 {
            return Ok(if a.to_bool(ctx)? { 0 } else { -1 });
        }
        let na = to_number_silent(a, ctx)?;
        let nb = to_number_silent(b, ctx)?;
        return compare_inner(&na, &nb, true, ctx);
    }
    if matches!(a, Array(_)) {
        Ok(1)
    } else if matches!(b, Array(_)) {
        Ok(-1)
    } else {
        Err(RuntimeError::Error("Unsupported operand types".to_string()))
    }
}

/// `zend_compare_arrays`: compares element counts, then each key of `a`
/// looked up in `b` (missing keys make the arrays uncomparable: 1).
pub fn compare_arrays(
    a: &ZArray,
    b: &ZArray,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<i32> {
    if ZArray::ptr_eq(a, b) {
        return Ok(0);
    }
    compare_tables(a, b, ctx)
}

thread_local! {
    /// Current nesting of array comparisons and its limit (see
    /// [`set_compare_depth_limit`]).
    static COMPARE_DEPTH: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, usize::MAX)) };
}

/// Sets how deeply array comparisons (`==`, `<=>`, `===`) may nest before
/// failing with "Maximum call stack size reached during comparison", the
/// embedder's model of `zend_call_stack_overflowed()` in
/// `zend_hash_compare` (`Zend/zend_hash.c`). Unlimited by default.
pub fn set_compare_depth_limit(limit: usize) {
    COMPARE_DEPTH.with(|c| c.set((c.get().0, limit)));
}

/// One nesting level of an array comparison.
struct CompareDepth;

impl CompareDepth {
    fn enter() -> RtResult<Self> {
        COMPARE_DEPTH.with(|c| {
            let (depth, limit) = c.get();
            if depth >= limit {
                return Err(RuntimeError::Error(
                    "Maximum call stack size reached during comparison".to_string(),
                ));
            }
            c.set((depth + 1, limit));
            Ok(CompareDepth)
        })
    }
}

impl Drop for CompareDepth {
    fn drop(&mut self) {
        COMPARE_DEPTH.with(|c| {
            let (depth, limit) = c.get();
            c.set((depth.saturating_sub(1), limit));
        });
    }
}

/// `zend_hash_compare(ht1, ht2, zend_compare, ordered = 0)` on tables.
pub fn compare_tables(
    a: &HashTable,
    b: &HashTable,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<i32> {
    if std::ptr::eq(a, b) {
        return Ok(0);
    }
    let _depth = CompareDepth::enter()?;
    let _guard = RecursionGuard::enter(std::ptr::from_ref(a) as usize)?;
    if a.len() != b.len() {
        return Ok(if a.len() > b.len() { 1 } else { -1 });
    }
    for (k, v1) in a {
        let Some(v2) = b.get(k) else {
            return Ok(1);
        };
        let r = compare(v1, v2, ctx)?;
        if r != 0 {
            return Ok(r);
        }
    }
    Ok(0)
}

/// `php_array_key_compare_unstable_i`: the default `ksort()` key order.
/// Two integer keys never compare equal (`h1 > h2 ? 1 : -1`), two string
/// keys use [`smart_strcmp`], mixed keys use [`compare`].
pub fn compare_keys(
    a: &crate::array::ArrayKey,
    b: &crate::array::ArrayKey,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<i32> {
    use crate::array::ArrayKey;
    match (a, b) {
        (ArrayKey::Int(x), ArrayKey::Int(y)) => Ok(if x > y { 1 } else { -1 }),
        (ArrayKey::Str(x), ArrayKey::Str(y)) => Ok(smart_strcmp(x.as_bytes(), y.as_bytes())),
        _ => compare(&a.to_value(), &b.to_value(), ctx),
    }
}

/// `==` (`zend_compare(a, b) == 0`, with the string fast path).
pub fn is_equal(a: &Value, b: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<bool> {
    let (da, db) = (deref(a), deref(b));
    if let (Value::Str(x), Value::Str(y)) = (&*da, &*db) {
        return Ok(ZStr::ptr_eq(x, y) || smart_streq(x.as_bytes(), y.as_bytes()));
    }
    Ok(compare_inner(&da, &db, false, ctx)? == 0)
}

/// `!=` / `<>`.
pub fn is_not_equal(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<bool> {
    Ok(!is_equal(a, b, ctx)?)
}

/// `<` (`is_smaller_function`). `a > b` is compiled as `b < a`.
pub fn is_smaller(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<bool> {
    Ok(compare(a, b, ctx)? < 0)
}

/// `<=` (`is_smaller_or_equal_function`). `a >= b` is compiled as `b <= a`.
pub fn is_smaller_or_equal(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<bool> {
    Ok(compare(a, b, ctx)? <= 0)
}

/// `<=>` as a value.
pub fn spaceship(
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    Ok(Value::Int(i64::from(compare(a, b, ctx)?)))
}

/// `===` (`zend_is_identical`). Only fails with "Nesting level too deep"
/// on recursive arrays.
pub fn is_identical(a: &Value, b: &Value) -> RtResult<bool> {
    let (a, b) = (deref(a), deref(b));
    Ok(match (&*a, &*b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        #[allow(clippy::float_cmp)]
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => ZArray::ptr_eq(x, y) || tables_identical(x, y)?,
        (Value::Object(x), Value::Object(y)) => Obj::ptr_eq(x, y),
        (Value::Resource(x), Value::Resource(y)) => crate::value::Resource::ptr_eq(x, y),
        _ => false,
    })
}

fn tables_identical(a: &HashTable, b: &HashTable) -> RtResult<bool> {
    let _depth = CompareDepth::enter()?;
    let _guard = RecursionGuard::enter(std::ptr::from_ref(a) as usize)?;
    if a.len() != b.len() {
        return Ok(false);
    }
    for ((k1, v1), (k2, v2)) in a.iter().zip(b.iter()) {
        if k1 != k2 || !is_identical(v1, v2)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The non-object branch of `zend_std_compare_objects`: compares an object
/// with a scalar by casting the object to the scalar's type. When both
/// operands are objects, `both_objects` decides (it is only called for
/// distinct objects).
pub fn std_compare_objects<C, F>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
    both_objects: F,
) -> RtResult<i32>
where
    C: RuntimeContext + ?Sized,
    F: FnOnce(&mut C, &Obj, &Obj) -> RtResult<i32>,
{
    let (a, b) = (deref(a), deref(b));
    match (&*a, &*b) {
        (Value::Object(x), Value::Object(y)) => {
            if Obj::ptr_eq(x, y) {
                Ok(0)
            } else {
                both_objects(ctx, x, y)
            }
        }
        (Value::Object(o), other) | (other, Value::Object(o)) => {
            let obj_lhs = matches!(&*a, Value::Object(_));
            let target = match other {
                Value::Null => CastTarget::Null,
                Value::Bool(_) => CastTarget::Bool,
                Value::Int(_) => CastTarget::Int,
                Value::Float(_) => CastTarget::Float,
                Value::Str(_) => CastTarget::String,
                Value::Array(_) => CastTarget::Array,
                Value::Resource(_) => return Ok(if obj_lhs { 1 } else { -1 }),
                Value::Object(_) | Value::Reference(_) => {
                    unreachable!("object and references were handled above")
                }
            };
            let casted = match ctx.cast_object(o, target)? {
                Some(v) => v,
                None => match target {
                    CastTarget::Int | CastTarget::Float => {
                        ctx.diagnostic(
                            ErrorLevel::Notice,
                            &format!(
                                "Object of class {} could not be converted to {}",
                                o.class_name(),
                                target.type_name()
                            ),
                        )?;
                        if target == CastTarget::Int {
                            Value::Int(1)
                        } else {
                            Value::Float(1.0)
                        }
                    }
                    _ => return Ok(if obj_lhs { 1 } else { -1 }),
                },
            };
            if obj_lhs {
                compare(&casted, other, ctx)
            } else {
                compare(other, &casted, ctx)
            }
        }
        _ => compare(&a, &b, ctx),
    }
}

// ---------------------------------------------------------------------------
// Increment / decrement
// ---------------------------------------------------------------------------

/// Perl-style string increment (`increment_string`) without the deprecation.
#[must_use]
pub fn str_increment_bytes(s: &[u8]) -> Vec<u8> {
    #[derive(Clone, Copy)]
    enum Last {
        None,
        Lower,
        Upper,
        Numeric,
    }
    if s.is_empty() {
        return b"1".to_vec();
    }
    let mut out = s.to_vec();
    let mut last = Last::None;
    let mut carry = false;
    let mut pos = out.len();
    while pos > 0 {
        pos -= 1;
        let ch = out[pos];
        match ch {
            b'a'..=b'z' => {
                carry = ch == b'z';
                out[pos] = if carry { b'a' } else { ch + 1 };
                last = Last::Lower;
            }
            b'A'..=b'Z' => {
                carry = ch == b'Z';
                out[pos] = if carry { b'A' } else { ch + 1 };
                last = Last::Upper;
            }
            b'0'..=b'9' => {
                carry = ch == b'9';
                out[pos] = if carry { b'0' } else { ch + 1 };
                last = Last::Numeric;
            }
            _ => {
                carry = false;
                break;
            }
        }
        if !carry {
            break;
        }
    }
    if carry {
        let first = match last {
            Last::Numeric => b'1',
            Last::Upper => b'A',
            Last::Lower => b'a',
            Last::None => return out,
        };
        out.insert(0, first);
    }
    out
}

/// `++` (`increment_function`), applied in place (references are followed).
#[allow(clippy::cast_precision_loss)]
pub fn increment(v: &mut Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<()> {
    if let Value::Reference(r) = v {
        let mut inner = r.get();
        let res = increment(&mut inner, ctx);
        r.set(inner);
        return res;
    }
    let new = match &*v {
        Value::Int(i) => match i.checked_add(1) {
            Some(n) => Value::Int(n),
            None => Value::Float(*i as f64 + 1.0),
        },
        Value::Float(f) => Value::Float(f + 1.0),
        Value::Null => Value::Int(1),
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value) {
            Some(Number::Int(i)) => match i.checked_add(1) {
                Some(n) => Value::Int(n),
                None => Value::Float(i as f64 + 1.0),
            },
            Some(Number::Float(f)) => Value::Float(f + 1.0),
            None => {
                let s = s.clone();
                ctx.diagnostic(
                    ErrorLevel::Deprecated,
                    "Increment on non-numeric string is deprecated, use str_increment() instead",
                )?;
                Value::Str(ZStr::new(str_increment_bytes(s.as_bytes())))
            }
        },
        Value::Bool(_) => {
            ctx.diagnostic(
                ErrorLevel::Warning,
                "Increment on type bool has no effect, this will change in the next major version of PHP",
            )?;
            return Ok(());
        }
        Value::Object(o) => {
            let o = o.clone();
            let obj = Value::Object(o.clone());
            if let Some(r) = ctx.do_operation(BinaryOp::Add, &obj, &Value::Int(1))? {
                *v = r;
                return Ok(());
            }
            match ctx.cast_object(&o, CastTarget::Number)? {
                Some(n @ (Value::Int(_) | Value::Float(_))) => {
                    *v = n;
                    return increment(v, ctx);
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "Cannot increment {}",
                        obj.value_name()
                    )))
                }
            }
        }
        other => {
            return Err(RuntimeError::TypeError(format!(
                "Cannot increment {}",
                other.value_name()
            )))
        }
    };
    *v = new;
    Ok(())
}

/// `--` (`decrement_function`), applied in place (references are followed).
#[allow(clippy::cast_precision_loss)]
pub fn decrement(v: &mut Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<()> {
    if let Value::Reference(r) = v {
        let mut inner = r.get();
        let res = decrement(&mut inner, ctx);
        r.set(inner);
        return res;
    }
    let new = match &*v {
        Value::Int(i) => match i.checked_sub(1) {
            Some(n) => Value::Int(n),
            None => Value::Float(*i as f64 - 1.0),
        },
        Value::Float(f) => Value::Float(f - 1.0),
        Value::Str(s) if s.is_empty() => {
            ctx.diagnostic(
                ErrorLevel::Deprecated,
                "Decrement on empty string is deprecated as non-numeric",
            )?;
            Value::Int(-1)
        }
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value) {
            Some(Number::Int(i)) => match i.checked_sub(1) {
                Some(n) => Value::Int(n),
                None => Value::Float(i as f64 - 1.0),
            },
            Some(Number::Float(f)) => Value::Float(f - 1.0),
            None => {
                ctx.diagnostic(
                    ErrorLevel::Deprecated,
                    "Decrement on non-numeric string has no effect and is deprecated",
                )?;
                return Ok(());
            }
        },
        Value::Null => {
            ctx.diagnostic(
                ErrorLevel::Warning,
                "Decrement on type null has no effect, this will change in the next major version of PHP",
            )?;
            return Ok(());
        }
        Value::Bool(_) => {
            ctx.diagnostic(
                ErrorLevel::Warning,
                "Decrement on type bool has no effect, this will change in the next major version of PHP",
            )?;
            return Ok(());
        }
        Value::Object(o) => {
            let o = o.clone();
            let obj = Value::Object(o.clone());
            if let Some(r) = ctx.do_operation(BinaryOp::Sub, &obj, &Value::Int(1))? {
                *v = r;
                return Ok(());
            }
            match ctx.cast_object(&o, CastTarget::Number)? {
                Some(n @ (Value::Int(_) | Value::Float(_))) => {
                    *v = n;
                    return decrement(v, ctx);
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "Cannot decrement {}",
                        obj.value_name()
                    )))
                }
            }
        }
        other => {
            return Err(RuntimeError::TypeError(format!(
                "Cannot decrement {}",
                other.value_name()
            )))
        }
    };
    *v = new;
    Ok(())
}

/// Applies a binary operator by kind (convenience dispatcher for the VM's
/// compound assignments).
pub fn binary_op(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    match op {
        BinaryOp::Add => add(a, b, ctx),
        BinaryOp::Sub => sub(a, b, ctx),
        BinaryOp::Mul => mul(a, b, ctx),
        BinaryOp::Div => div(a, b, ctx),
        BinaryOp::Mod => modulo(a, b, ctx),
        BinaryOp::Pow => pow(a, b, ctx),
        BinaryOp::ShiftLeft => shift_left(a, b, ctx),
        BinaryOp::ShiftRight => shift_right(a, b, ctx),
        BinaryOp::BitOr => bit_or(a, b, ctx),
        BinaryOp::BitAnd => bit_and(a, b, ctx),
        BinaryOp::BitXor => bit_xor(a, b, ctx),
        BinaryOp::Concat => concat(a, b, ctx),
        BinaryOp::BoolXor => bool_xor(a, b, ctx),
        BinaryOp::BitNot => bit_not(a, ctx),
        BinaryOp::BoolNot => bool_not(a, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CollectingContext;

    fn inc(v: Value) -> (Value, Vec<String>) {
        let mut c = CollectingContext::new();
        let mut v = v;
        increment(&mut v, &mut c).unwrap();
        (v, c.messages())
    }

    fn dec(v: Value) -> (Value, Vec<String>) {
        let mut c = CollectingContext::new();
        let mut v = v;
        decrement(&mut v, &mut c).unwrap();
        (v, c.messages())
    }

    #[test]
    fn string_increment() {
        for (from, to) in [
            ("z", "aa"),
            ("Az", "Ba"),
            ("zz", "aaa"),
            ("a9", "b0"),
            ("Zz", "AAa"),
            ("", "1"),
            ("-", "-"),
            ("a-z", "a-a"),
            ("abc ", "abc "),
            ("9z", "10a"),
        ] {
            let (v, msgs) = inc(Value::from(from));
            assert_eq!(
                v.as_str().map(ZStr::to_string_lossy).as_deref(),
                Some(to),
                "{from}"
            );
            assert_eq!(
                msgs,
                ["Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead"]
            );
        }
    }

    #[test]
    fn numeric_increment_decrement() {
        assert_eq!(inc(Value::from("9")).0.as_int(), Some(10));
        assert_eq!(inc(Value::from(" 1")).0.as_int(), Some(2));
        assert_eq!(inc(Value::from("1.5")).0.as_float(), Some(2.5));
        assert_eq!(inc(Value::from("1e2")).0.as_float(), Some(101.0));
        assert_eq!(
            inc(Value::Int(i64::MAX)).0.as_float(),
            Some(9.223_372_036_854_776e18)
        );
        assert_eq!(inc(Value::Null).0.as_int(), Some(1));
        assert_eq!(
            dec(Value::Int(i64::MIN)).0.as_float(),
            Some(-9.223_372_036_854_776e18)
        );
        assert_eq!(dec(Value::from("5")).0.as_int(), Some(4));
        let (v, m) = dec(Value::from(""));
        assert_eq!(v.as_int(), Some(-1));
        assert_eq!(
            m,
            ["Deprecated: Decrement on empty string is deprecated as non-numeric"]
        );
        let (v, m) = dec(Value::from("abc"));
        assert_eq!(
            v.as_str().map(ZStr::to_string_lossy).as_deref(),
            Some("abc")
        );
        assert_eq!(
            m,
            ["Deprecated: Decrement on non-numeric string has no effect and is deprecated"]
        );
        let (v, m) = dec(Value::Null);
        assert!(v.is_null());
        assert_eq!(
            m,
            ["Warning: Decrement on type null has no effect, this will change in the next major version of PHP"]
        );
        let (v, m) = inc(Value::Bool(false));
        assert!(matches!(v, Value::Bool(false)));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn increment_errors() {
        let mut c = CollectingContext::new();
        let mut v = Value::empty_array();
        let e = increment(&mut v, &mut c).unwrap_err();
        assert_eq!(e.message(), Some("Cannot increment array"));
        let e = decrement(&mut v, &mut c).unwrap_err();
        assert_eq!(e.message(), Some("Cannot decrement array"));
    }

    #[test]
    fn increment_through_reference() {
        let mut c = CollectingContext::new();
        let mut v = Value::Int(1);
        let r = v.make_ref();
        increment(&mut v, &mut c).unwrap();
        assert_eq!(r.get().as_int(), Some(2));
    }

    #[test]
    fn recursion_guard_kinds_are_independent() {
        let g = RecursionGuard::try_enter(GuardKind::Json, 42).unwrap();
        assert!(RecursionGuard::try_enter(GuardKind::Json, 42).is_none());
        assert!(RecursionGuard::is_protected(GuardKind::Json, 42));
        let other = RecursionGuard::try_enter(GuardKind::Debug, 42);
        assert!(other.is_some());
        drop(g);
        assert!(!RecursionGuard::is_protected(GuardKind::Json, 42));
    }

    #[test]
    fn intdiv_and_shifts() {
        assert_eq!(intdiv(7, 2).unwrap(), 3);
        assert_eq!(
            intdiv(1, 0).unwrap_err().message(),
            Some("Division by zero")
        );
        assert_eq!(
            intdiv(i64::MIN, -1).unwrap_err().message(),
            Some("Division of PHP_INT_MIN by -1 is not an integer")
        );
        let mut c = CollectingContext::new();
        let e = shift_left(&Value::Int(1), &Value::Int(-1), &mut c).unwrap_err();
        assert_eq!(e.class_name(), Some("ArithmeticError"));
        assert_eq!(e.message(), Some("Bit shift by negative number"));
        assert_eq!(
            shift_left(&Value::Int(1), &Value::Int(64), &mut c)
                .unwrap()
                .as_int(),
            Some(0)
        );
        assert_eq!(
            shift_right(&Value::Int(-8), &Value::Int(65), &mut c)
                .unwrap()
                .as_int(),
            Some(-1)
        );
    }

    #[test]
    fn concat_assign_appends_in_place() {
        let mut c = CollectingContext::new();
        let mut v = Value::from("ab");
        concat_assign(&mut v, &Value::Int(1), &mut c).unwrap();
        concat_assign(&mut v, &Value::from("c"), &mut c).unwrap();
        assert_eq!(
            v.as_str().map(ZStr::to_string_lossy).as_deref(),
            Some("ab1c")
        );
    }

    /// `IS_STR_VALID_UTF8` through `concat_function` and the conversions:
    /// the cases of ext/zend_test/tests/strings_marked_as_utf8.phpt and
    /// strings_not_marked_as_utf8.phpt this layer sees.
    #[test]
    fn concat_and_conversions_carry_the_valid_utf8_flag() {
        let mut c = CollectingContext::new();
        let flag = |v: &Value| v.as_str().is_some_and(ZStr::is_valid_utf8);
        // Integer and float casts (`zend_long_to_str`, `zend_double_to_str`).
        assert!(flag(&Value::Str(Value::Int(2563).to_zstr(&mut c).unwrap())));
        assert!(flag(&Value::Str(
            Value::Float(26.7).to_zstr(&mut c).unwrap()
        )));
        assert!(flag(&Value::Str(
            Value::Float(2e100).to_zstr(&mut c).unwrap()
        )));
        assert!(flag(&Value::Str(Value::Int(7).to_zstr(&mut c).unwrap())));
        let (f, o) = (
            Value::Str(ZStr::from_byte(b'f')),
            Value::Str(ZStr::from_byte(b'o')),
        );
        let fo = concat(&f, &o, &mut c).unwrap();
        assert!(flag(&fo));
        let foo = concat(&fo, &o, &mut c).unwrap();
        assert!(flag(&foo));
        assert!(flag(
            &concat(&f, &Value::Str(ZStr::empty()), &mut c).unwrap()
        ));
        assert!(flag(
            &concat(&Value::Str(ZStr::empty()), &f, &mut c).unwrap()
        ));
        assert!(flag(&concat(&f, &Value::Int(12), &mut c).unwrap()));
        assert!(flag(&concat(&Value::Float(1.5), &f, &mut c).unwrap()));
        // Invalid UTF-8 (and strings nothing is known about) is not flagged.
        let invalid = Value::Str(ZStr::new(b"\xc3\x28".to_vec()));
        assert!(!flag(&concat(&fo, &invalid, &mut c).unwrap()));
        assert!(!flag(&concat(&Value::Int(2563), &invalid, &mut c).unwrap()));
        assert!(!flag(&concat(&f, &Value::from("unknown"), &mut c).unwrap()));
        // `.=` in place and on a shared string.
        let mut s = Value::Str(ZStr::from_byte(b'a'));
        for _ in 1..100 {
            concat_assign(&mut s, &Value::Str(ZStr::from_byte(b'a')), &mut c).unwrap();
        }
        assert!(flag(&s));
        let shared = s.clone();
        concat_assign(&mut s, &Value::Int(5), &mut c).unwrap();
        assert!(flag(&s) && flag(&shared));
        concat_assign(&mut s, &invalid, &mut c).unwrap();
        assert!(!flag(&s));
        assert!(flag(&shared));
        // `"Array"` is a known string (`ZSTR_KNOWN`).
        let arr = Value::Array(crate::ZArray::new()).to_zstr(&mut c).unwrap();
        assert!(arr.is_valid_utf8());
    }

    #[test]
    fn concat_scalars_without_temporaries() {
        let mut c = CollectingContext::new();
        let cat = |a: Value, b: Value, c: &mut CollectingContext| {
            concat(&a, &b, c)
                .unwrap()
                .as_str()
                .map(ZStr::to_string_lossy)
                .unwrap()
        };
        assert_eq!(cat(Value::from("v"), Value::Int(-15), &mut c), "v-15");
        assert_eq!(
            cat(Value::Int(i64::MIN), Value::from("!"), &mut c),
            "-9223372036854775808!"
        );
        assert_eq!(cat(Value::Int(7), Value::Int(42), &mut c), "742");
        assert_eq!(cat(Value::Bool(true), Value::Null, &mut c), "1");
        assert_eq!(cat(Value::Null, Value::Bool(false), &mut c), "");
        assert_eq!(cat(Value::from(""), Value::Int(3), &mut c), "3");
        assert_eq!(cat(Value::Float(1.5), Value::from("x"), &mut c), "1.5x");
        // An empty operand gives the other string itself.
        let s = Value::from("keep");
        let r = concat(&s, &Value::from(""), &mut c).unwrap();
        assert!(ZStr::ptr_eq(r.as_str().unwrap(), s.as_str().unwrap()));
        assert!(c.diagnostics.is_empty());
        // Floats follow `precision`; NaN warns once per NaN operand, in order.
        c.precision = Some(3);
        assert_eq!(
            cat(Value::Float(1234.5), Value::from(""), &mut c),
            "1.23E+3"
        );
        assert_eq!(cat(Value::from(""), Value::Float(-0.0), &mut c), "-0");
        c.precision = Some(20);
        assert_eq!(
            cat(Value::Float(0.1), Value::Null, &mut c),
            "0.10000000000000000555"
        );
        c.precision = None;
        assert!(c.diagnostics.is_empty());
        assert_eq!(
            cat(Value::Float(f64::NAN), Value::Float(f64::INFINITY), &mut c),
            "NANINF"
        );
        assert_eq!(c.diagnostics.len(), 1);
        assert_eq!(
            c.diagnostics[0].message,
            "unexpected NAN value was coerced to string"
        );
    }

    fn self_referencing() -> Value {
        // $a = []; $a[] = &$a;
        let mut a = Value::empty_array();
        let r = a.make_ref();
        let mut inner = r.get();
        if let Value::Array(arr) = &mut inner {
            arr.make_mut().push(Value::Reference(r.clone())).unwrap();
        }
        r.set(inner);
        r.get()
    }

    #[test]
    fn recursive_array_comparison_throws() {
        let mut c = CollectingContext::new();
        let (a, b) = (self_referencing(), self_referencing());
        assert!(is_equal(&a, &a, &mut c).unwrap());
        let e = compare(&a, &b, &mut c).unwrap_err();
        assert_eq!(
            e.message(),
            Some("Nesting level too deep - recursive dependency?")
        );
        let e = is_identical(&a, &b).unwrap_err();
        assert_eq!(
            e.message(),
            Some("Nesting level too deep - recursive dependency?")
        );
    }

    #[test]
    fn deep_array_comparison_hits_the_depth_limit() {
        fn nested(depth: usize) -> Value {
            let mut v = Value::empty_array();
            for _ in 0..depth {
                let mut a = ZArray::new();
                a.make_mut().push(v).unwrap();
                v = Value::Array(a);
            }
            v
        }
        let mut c = CollectingContext::new();
        let (a, b) = (nested(3), nested(3));
        set_compare_depth_limit(2);
        let eq = compare(&a, &b, &mut c).unwrap_err();
        let id = is_identical(&a, &b).unwrap_err();
        set_compare_depth_limit(5);
        let ok = is_identical(&a, &b);
        set_compare_depth_limit(usize::MAX);
        let msg = Some("Maximum call stack size reached during comparison");
        assert_eq!(eq.message(), msg);
        assert_eq!(id.message(), msg);
        assert!(ok.unwrap());
    }
}
