//! Type conversions (`zend_operators.c`: `zval_get_long`, `zval_get_double`,
//! `zval_try_get_string`, `zend_is_true`, `convert_to_array`, ...).
//!
//! All conversions take a [`RuntimeContext`] for diagnostics and object hooks.

#![allow(clippy::many_single_char_names, clippy::single_match_else)]

use crate::array::{ArrayKey, HashTable, ZArray};
use crate::error::{
    CastTarget, ErrorLevel, PropertyPurpose, RtResult, RuntimeContext, RuntimeError,
};
use crate::float::{format_double_h, format_double_zstr};
use crate::numeric::{is_numeric_string_ex, str_to_f64, Number};
use crate::string::{format_long, ZStr, LONG_MAX_LEN};
use crate::value::{Obj, Value};

/// `(double)ZEND_LONG_MAX` (= 2^63).
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// `ZEND_DOUBLE_FITS_LONG(d)`.
#[inline]
#[must_use]
#[allow(clippy::manual_range_contains)]
pub fn double_fits_long(d: f64) -> bool {
    !(d >= TWO_POW_63 || d < -TWO_POW_63)
}

/// `zend_dval_to_lval_slow`: modular conversion of an out-of-range float.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
pub fn dval_to_lval_modular(d: f64) -> i64 {
    let two_pow_64 = 18_446_744_073_709_551_616.0_f64;
    let mut dmod = d % two_pow_64;
    if dmod < 0.0 {
        dmod += two_pow_64;
    }
    (dmod as u64) as i64
}

/// `zend_dval_to_lval_silent`: NaN/Inf give 0, out-of-range values wrap.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn dval_to_lval_silent(d: f64) -> i64 {
    if !d.is_finite() {
        0
    } else if !double_fits_long(d) {
        dval_to_lval_modular(d)
    } else {
        d as i64
    }
}

/// `zend_dval_to_lval_cap`: saturating conversion (used for float strings).
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn dval_to_lval_cap(d: f64) -> i64 {
    if !d.is_finite() {
        0
    } else if !double_fits_long(d) {
        if d > 0.0 {
            i64::MAX
        } else {
            i64::MIN
        }
    } else {
        d as i64
    }
}

/// `zend_is_long_compatible`.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::float_cmp)]
pub fn is_long_compatible(d: f64, l: i64) -> bool {
    l as f64 == d
}

/// `zend_dval_to_lval`: like [`dval_to_lval_silent`] but warns
/// "The float X is not representable as an int, cast occurred".
pub fn dval_to_lval(d: f64, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<i64> {
    if !d.is_finite() || !double_fits_long(d) {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!(
                "The float {} is not representable as an int, cast occurred",
                format_double_h(d, -1)
            ),
        )?;
    }
    Ok(dval_to_lval_silent(d))
}

/// `zend_dval_to_lval_safe`: [`dval_to_lval`] plus the "Implicit conversion
/// from float X to int loses precision" deprecation for fractional values.
pub fn dval_to_lval_safe(d: f64, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<i64> {
    let l = dval_to_lval(d, ctx)?;
    if !is_long_compatible(d, l) && double_fits_long(d) {
        ctx.diagnostic(
            ErrorLevel::Deprecated,
            &format!(
                "Implicit conversion from float {} to int loses precision",
                format_double_h(d, -1)
            ),
        )?;
    }
    Ok(l)
}

/// `zend_nan_coerced_to_type_warning`.
pub fn nan_coerced_warning(
    ctx: &mut (impl RuntimeContext + ?Sized),
    type_name: &str,
) -> RtResult<()> {
    ctx.diagnostic(
        ErrorLevel::Warning,
        &format!("unexpected NAN value was coerced to {type_name}"),
    )
}

/// Formats an integer (`zend_long_to_str`).
///
/// The digits are written into a stack buffer (`zend_print_long_to_buf`)
/// and copied into an exactly sized string: `i64::to_string` reserves the
/// worst-case 20 bytes for every number, which made each int-to-string
/// conversion (array keys, property names, concatenation operands) cost
/// up to 19 bytes more than php-src's exactly sized `zend_string`.
#[must_use]
pub fn long_to_zstr(n: i64) -> ZStr {
    if (0..=9).contains(&n) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        return ZStr::from_byte(b'0' + n as u8);
    }
    let mut buf = [0u8; LONG_MAX_LEN];
    ZStr::from(format_long(n, &mut buf))
}

/// `zend_double_to_str`: formats a float with the `precision` setting and
/// warns when it is NaN. The text is built in a stack buffer and copied
/// once into an exactly sized string.
pub fn double_to_zstr(d: f64, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<ZStr> {
    let s = format_double_zstr(d, ctx.precision(), false);
    if d.is_nan() {
        nan_coerced_warning(ctx, "string")?;
    }
    Ok(s)
}

fn object_cast_warning(
    ctx: &mut (impl RuntimeContext + ?Sized),
    obj: &Obj,
    target: CastTarget,
) -> RtResult<()> {
    ctx.diagnostic(
        ErrorLevel::Warning,
        &format!(
            "Object of class {} could not be converted to {}",
            obj.class_name(),
            target.type_name()
        ),
    )
}

impl Value {
    /// `zend_is_true`: boolean conversion.
    pub fn to_bool(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<bool> {
        Ok(match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => {
                if f.is_nan() {
                    nan_coerced_warning(ctx, "bool")?;
                }
                *f != 0.0
            }
            Value::Str(s) => !(s.is_empty() || s.as_bytes() == b"0"),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => match ctx.cast_object(o, CastTarget::Bool)? {
                Some(v) => matches!(v, Value::Bool(true)),
                None => {
                    ctx.diagnostic(
                        ErrorLevel::RecoverableError,
                        &format!(
                            "Object of class {} could not be converted to bool",
                            o.class_name()
                        ),
                    )?;
                    false
                }
            },
            Value::Resource(r) => r.handle() != 0,
            Value::Reference(r) => {
                let v = r.get();
                return v.to_bool(ctx);
            }
        })
    }

    /// `zval_get_long`: integer conversion as done by `(int)` and `intval()`.
    pub fn to_long(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<i64> {
        self.to_long_ex(false, ctx)
    }

    /// `zval_get_long_ex(op, is_strict)`: with `is_strict`, lossy float
    /// conversions emit the "Implicit conversion" deprecation.
    pub fn to_long_ex(
        &self,
        is_strict: bool,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<i64> {
        Ok(match self {
            Value::Null | Value::Bool(false) => 0,
            Value::Bool(true) => 1,
            Value::Resource(r) => r.handle(),
            Value::Int(i) => *i,
            Value::Float(d) => {
                let l = dval_to_lval(*d, ctx)?;
                if is_strict && !is_long_compatible(*d, l) {
                    ctx.diagnostic(
                        ErrorLevel::Deprecated,
                        &format!(
                            "Implicit conversion from float {} to int loses precision",
                            format_double_h(*d, -1)
                        ),
                    )?;
                }
                l
            }
            Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
                None => 0,
                Some(n) => match n.value {
                    Number::Int(i) => i,
                    Number::Float(d) => {
                        let l = dval_to_lval_cap(d);
                        if is_strict && !is_long_compatible(d, l) {
                            ctx.diagnostic(
                                ErrorLevel::Deprecated,
                                &format!(
                                    "Implicit conversion from float-string \"{s}\" to int loses precision"
                                ),
                            )?;
                        }
                        l
                    }
                },
            },
            Value::Array(a) => i64::from(!a.is_empty()),
            Value::Object(o) => match ctx.cast_object(o, CastTarget::Int)? {
                Some(Value::Int(i)) => i,
                _ => {
                    object_cast_warning(ctx, o, CastTarget::Int)?;
                    1
                }
            },
            Value::Reference(r) => {
                let v = r.get();
                return v.to_long_ex(is_strict, ctx);
            }
        })
    }

    /// `zval_get_double`: float conversion as done by `(float)`.
    #[allow(clippy::cast_precision_loss)]
    pub fn to_double(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<f64> {
        Ok(match self {
            Value::Null | Value::Bool(false) => 0.0,
            Value::Bool(true) => 1.0,
            Value::Resource(r) => r.handle() as f64,
            Value::Int(i) => *i as f64,
            Value::Float(d) => *d,
            Value::Str(s) => str_to_f64(s.as_bytes()),
            Value::Array(a) => {
                if a.is_empty() {
                    0.0
                } else {
                    1.0
                }
            }
            Value::Object(o) => match ctx.cast_object(o, CastTarget::Float)? {
                Some(Value::Float(d)) => d,
                _ => {
                    object_cast_warning(ctx, o, CastTarget::Float)?;
                    1.0
                }
            },
            Value::Reference(r) => {
                let v = r.get();
                return v.to_double(ctx);
            }
        })
    }

    /// `zval_try_get_string`: string conversion (`(string)`, `echo`, `.`).
    ///
    /// Arrays give `"Array"` with a warning; objects use `__toString` via the
    /// context's `cast_object` hook and otherwise throw
    /// "Object of class X could not be converted to string".
    pub fn to_zstr(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<ZStr> {
        Ok(match self {
            Value::Null | Value::Bool(false) => ZStr::empty(),
            Value::Bool(true) => ZStr::from_byte(b'1'),
            Value::Resource(r) => ZStr::from(format!("Resource id #{}", r.handle())),
            Value::Int(i) => long_to_zstr(*i),
            Value::Float(d) => double_to_zstr(*d, ctx)?,
            Value::Str(s) => s.clone(),
            Value::Array(_) => {
                ctx.diagnostic(ErrorLevel::Warning, "Array to string conversion")?;
                ZStr::from("Array")
            }
            Value::Object(o) => match ctx.cast_object(o, CastTarget::String)? {
                Some(Value::Str(s)) => s,
                _ => {
                    return Err(RuntimeError::Error(format!(
                        "Object of class {} could not be converted to string",
                        o.class_name()
                    )))
                }
            },
            Value::Reference(r) => {
                let v = r.get();
                return v.to_zstr(ctx);
            }
        })
    }

    /// `convert_scalar_to_number`: the value as int or float (non-numeric
    /// strings give 0 silently, leading-numeric strings are accepted).
    pub fn to_number(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
        Ok(match self {
            Value::Int(_) | Value::Float(_) => self.clone(),
            Value::Null | Value::Bool(false) => Value::Int(0),
            Value::Bool(true) => Value::Int(1),
            Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
                None => Value::Int(0),
                Some(n) => number_value(n.value),
            },
            Value::Resource(r) => Value::Int(r.handle()),
            Value::Array(a) => Value::Int(i64::from(!a.is_empty())),
            Value::Object(o) => match ctx.cast_object(o, CastTarget::Number)? {
                Some(v @ (Value::Int(_) | Value::Float(_))) => v,
                _ => {
                    object_cast_warning(ctx, o, CastTarget::Number)?;
                    Value::Int(1)
                }
            },
            Value::Reference(r) => {
                let v = r.get();
                return v.to_number(ctx);
            }
        })
    }

    /// `convert_to_array`: the `(array)` cast.
    pub fn to_array(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<ZArray> {
        Ok(match self {
            Value::Array(a) => a.clone(),
            Value::Null => ZArray::new(),
            // `convert_to_array()`: a Closure is wrapped like a scalar
            // (`convert_scalar_to_array`), every other object exports its
            // properties.
            Value::Object(o) if ctx.is_closure(o) => {
                let mut t = HashTable::with_capacity(1);
                t.insert(0, self.clone());
                ZArray::from_table(t)
            }
            Value::Object(o) => match ctx.object_properties(o, PropertyPurpose::ArrayCast)? {
                Some(props) => ZArray::from_table(props.to_symtable()),
                None => ZArray::new(),
            },
            Value::Reference(r) => {
                let v = r.get();
                return v.to_array(ctx);
            }
            other => {
                if let Value::Float(f) = other {
                    if f.is_nan() {
                        nan_coerced_warning(ctx, "array")?;
                    }
                }
                let mut t = HashTable::with_capacity(1);
                t.insert(0, other.clone());
                ZArray::from_table(t)
            }
        })
    }

    /// `convert_to_object`: the `(object)` cast. Creates `stdClass`
    /// instances through [`RuntimeContext::create_std_object`].
    pub fn to_object(&self, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
        match self {
            Value::Object(_) => Ok(self.clone()),
            Value::Array(a) => ctx.create_std_object(ZArray::from_table(a.to_proptable())),
            Value::Null => ctx.create_std_object(ZArray::new()),
            Value::Reference(r) => {
                let v = r.get();
                v.to_object(ctx)
            }
            other => {
                if let Value::Float(f) = other {
                    if f.is_nan() {
                        nan_coerced_warning(ctx, "object")?;
                    }
                }
                let mut t = HashTable::with_capacity(1);
                t.insert_str_raw(ZStr::from("scalar"), other.clone());
                ctx.create_std_object(ZArray::from_table(t))
            }
        }
    }

    /// Converts a value used as an array offset to a key, following
    /// `slow_index_convert`: `null` → `""` (deprecated), bools → 0/1, floats →
    /// truncated (with the precision-loss deprecation), resources → their
    /// handle (with a warning), numeric strings → integers.
    ///
    /// Returns `Ok(None)` for illegal offset types (arrays, objects); the
    /// caller emits the context-specific error ("Cannot access offset of type
    /// %s on %s", "Illegal offset type", ...).
    pub fn to_array_key(
        &self,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<Option<ArrayKey>> {
        Ok(Some(match self {
            Value::Int(i) => ArrayKey::Int(*i),
            Value::Str(s) => ArrayKey::from_zstr(s),
            Value::Null => {
                ctx.diagnostic(
                    ErrorLevel::Deprecated,
                    "Using null as an array offset is deprecated, use an empty string instead",
                )?;
                ArrayKey::Str(ZStr::empty())
            }
            Value::Bool(b) => ArrayKey::Int(i64::from(*b)),
            Value::Float(d) => ArrayKey::Int(dval_to_lval_safe(*d, ctx)?),
            Value::Resource(r) => {
                ctx.diagnostic(
                    ErrorLevel::Warning,
                    &format!(
                        "Resource ID#{} used as offset, casting to integer ({})",
                        r.handle(),
                        r.handle()
                    ),
                )?;
                ArrayKey::Int(r.handle())
            }
            Value::Array(_) | Value::Object(_) => return Ok(None),
            Value::Reference(r) => {
                let v = r.get();
                return v.to_array_key(ctx);
            }
        }))
    }
}

/// Converts a parsed [`Number`] to a value.
#[must_use]
pub fn number_value(n: Number) -> Value {
    match n {
        Number::Int(i) => Value::Int(i),
        Number::Float(f) => Value::Float(f),
    }
}

/// `zendi_try_convert_scalar_to_number`: converts an arithmetic operand.
/// Returns `Ok(None)` when the operand type is unsupported (arrays,
/// resources, non-numeric strings, objects without a numeric cast); warns
/// "A non-numeric value encountered" for leading-numeric strings.
pub fn try_convert_to_number(
    v: &Value,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Option<Value>> {
    Ok(match v {
        Value::Int(_) | Value::Float(_) => Some(v.clone()),
        Value::Null | Value::Bool(false) => Some(Value::Int(0)),
        Value::Bool(true) => Some(Value::Int(1)),
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
            None => None,
            Some(n) => {
                if n.trailing_data {
                    ctx.diagnostic(ErrorLevel::Warning, "A non-numeric value encountered")?;
                }
                Some(number_value(n.value))
            }
        },
        Value::Object(o) => match ctx.cast_object(o, CastTarget::Number)? {
            Some(v @ (Value::Int(_) | Value::Float(_))) => Some(v),
            _ => None,
        },
        Value::Resource(_) | Value::Array(_) => None,
        Value::Reference(r) => {
            let inner = r.get();
            return try_convert_to_number(&inner, ctx);
        }
    })
}

/// `zendi_try_get_long`: integer operand conversion for `%`, `<<`, `>>`,
/// `|`, `&`, `^`. Returns `Ok(None)` on unsupported types.
pub fn try_get_long(v: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Option<i64>> {
    Ok(match v {
        Value::Null | Value::Bool(false) => Some(0),
        Value::Bool(true) => Some(1),
        Value::Int(i) => Some(*i),
        Value::Float(d) => Some(dval_to_lval_safe(*d, ctx)?),
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
            None => None,
            Some(n) => {
                if n.trailing_data {
                    ctx.diagnostic(ErrorLevel::Warning, "A non-numeric value encountered")?;
                }
                match n.value {
                    Number::Int(i) => Some(i),
                    Number::Float(d) => {
                        let l = dval_to_lval_cap(d);
                        if !is_long_compatible(d, l) {
                            ctx.diagnostic(
                                ErrorLevel::Deprecated,
                                &format!(
                                    "Implicit conversion from float-string \"{s}\" to int loses precision"
                                ),
                            )?;
                        }
                        Some(l)
                    }
                }
            }
        },
        Value::Object(o) => match ctx.cast_object(o, CastTarget::Int)? {
            Some(Value::Int(i)) => Some(i),
            _ => None,
        },
        Value::Resource(_) | Value::Array(_) => None,
        Value::Reference(r) => {
            let inner = r.get();
            return try_get_long(&inner, ctx);
        }
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::error::CollectingContext;

    #[test]
    fn long_to_zstr_formats_every_width() {
        for n in [
            0,
            7,
            9,
            10,
            -1,
            -9,
            -10,
            42,
            1_000_000,
            -1_234_567,
            i64::MAX,
            i64::MIN,
            i64::MIN + 1,
        ] {
            assert_eq!(long_to_zstr(n).as_bytes(), n.to_string().as_bytes(), "{n}");
        }
    }

    #[test]
    fn long_conversion() {
        let mut c = CollectingContext::new();
        assert_eq!(
            Value::Float(1e20).to_long(&mut c).unwrap(),
            7_766_279_631_452_241_920
        );
        assert_eq!(
            c.messages(),
            ["Warning: The float 1.0E+20 is not representable as an int, cast occurred"]
        );
        assert_eq!(
            Value::Float(-1e20).to_long(&mut c).unwrap(),
            -7_766_279_631_452_241_920
        );
        assert_eq!(Value::from("1e20").to_long(&mut c).unwrap(), i64::MAX);
        assert_eq!(Value::Float(f64::NAN).to_long(&mut c).unwrap(), 0);
        assert_eq!(
            Value::Float(1.9e19).to_long(&mut c).unwrap(),
            553_255_926_290_448_384
        );
        assert_eq!(Value::from(" 12abc").to_long(&mut c).unwrap(), 12);
        assert_eq!(Value::from("abc").to_long(&mut c).unwrap(), 0);
        assert_eq!(Value::Float(-1.9).to_long(&mut c).unwrap(), -1);
    }

    #[test]
    fn string_conversion() {
        let mut c = CollectingContext::new();
        assert_eq!(Value::Float(0.1 + 0.2).to_zstr(&mut c).unwrap(), "0.3");
        assert_eq!(Value::Float(1e15).to_zstr(&mut c).unwrap(), "1.0E+15");
        assert_eq!(Value::Bool(true).to_zstr(&mut c).unwrap(), "1");
        assert_eq!(Value::Null.to_zstr(&mut c).unwrap(), "");
        assert_eq!(Value::Int(-42).to_zstr(&mut c).unwrap(), "-42");
        assert!(c.take().is_empty());
        assert_eq!(Value::empty_array().to_zstr(&mut c).unwrap(), "Array");
        assert_eq!(c.messages(), ["Warning: Array to string conversion"]);
        c.precision = Some(-1);
        assert_eq!(
            Value::Float(0.1 + 0.2).to_zstr(&mut c).unwrap(),
            "0.30000000000000004"
        );
    }

    #[test]
    fn bool_and_double() {
        let mut c = CollectingContext::new();
        for (v, b) in [
            (Value::from("0"), false),
            (Value::from(""), false),
            (Value::from("0.0"), true),
            (Value::from(" "), true),
            (Value::Float(-0.0), false),
            (Value::Int(0), false),
            (Value::empty_array(), false),
        ] {
            assert_eq!(v.to_bool(&mut c).unwrap(), b, "{v:?}");
        }
        assert!(Value::Float(f64::NAN).to_bool(&mut c).unwrap());
        assert_eq!(
            c.messages(),
            ["Warning: unexpected NAN value was coerced to bool"]
        );
        assert_eq!(Value::from("1e3abc").to_double(&mut c).unwrap(), 1000.0);
        assert_eq!(Value::from(".5").to_double(&mut c).unwrap(), 0.5);
    }

    #[test]
    fn array_keys() {
        let mut c = CollectingContext::new();
        assert_eq!(
            Value::from("5").to_array_key(&mut c).unwrap(),
            Some(ArrayKey::Int(5))
        );
        assert_eq!(
            Value::from("05").to_array_key(&mut c).unwrap(),
            Some(ArrayKey::Str(ZStr::from("05")))
        );
        assert_eq!(
            Value::Bool(true).to_array_key(&mut c).unwrap(),
            Some(ArrayKey::Int(1))
        );
        assert_eq!(
            Value::Float(1.7).to_array_key(&mut c).unwrap(),
            Some(ArrayKey::Int(1))
        );
        assert_eq!(
            c.messages(),
            ["Deprecated: Implicit conversion from float 1.7 to int loses precision"]
        );
        assert_eq!(Value::empty_array().to_array_key(&mut c).unwrap(), None);
    }

    #[test]
    fn array_cast() {
        let mut c = CollectingContext::new();
        let a = Value::Int(3).to_array(&mut c).unwrap();
        assert_eq!(a.get_int(0).and_then(Value::as_int), Some(3));
        assert!(Value::Null.to_array(&mut c).unwrap().is_empty());
    }
}
