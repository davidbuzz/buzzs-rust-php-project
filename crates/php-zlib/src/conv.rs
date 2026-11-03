//! PHP value conversions used when reading option arrays and filter
//! parameters (`zval_try_get_long()`, `zval_try_get_string()`,
//! `zend_zval_type_name()`, `zend_zval_value_name()`).
//!
//! Side diagnostics (`"A non-numeric value encountered"`, deprecations)
//! go through [`php_streams::diag`], the channel the VM listens on.

use php_streams::diag::{self, Diagnostic, Severity};
use php_streams::value::{Key, Value};

use crate::error::Error;

/// `zend_zval_value_name()`: `"true"`/`"false"` for booleans and the class
/// name for objects.
pub fn value_name(v: &Value) -> String {
    v.type_name()
}

/// `zend_zval_type_name()`: `"bool"` for booleans, the class name for
/// objects.
pub fn type_name(v: &Value) -> String {
    match v {
        Value::Bool(_) => "bool".into(),
        _ => v.type_name(),
    }
}

/// Looks up a string key in an option table (`zend_hash_str_find_ind`).
pub fn find<'a>(options: &'a [(Key, Value)], name: &str) -> Option<&'a Value> {
    options.iter().find(|(k, _)| k.is(name)).map(|(_, v)| v)
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Result of `_is_numeric_string_ex()` with `allow_errors`.
enum Numeric {
    Long(i64),
    Double(f64),
}

/// `_is_numeric_string_ex(…, allow_errors = true, …)`: returns the value
/// and whether trailing data followed the number.
fn numeric_prefix(s: &[u8]) -> Option<(Numeric, bool)> {
    if s.first().is_some_and(|&b| b > b'9') {
        return None;
    }
    let mut i = 0;
    while i < s.len() && is_ws(s[i]) {
        i += 1;
    }
    let start = i;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - int_start;
    let mut is_double = false;
    if i < s.len() && s[i] == b'.' {
        let frac_start = i + 1;
        let mut j = frac_start;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if digits > 0 || j > frac_start {
            digits += j - frac_start;
            is_double = true;
            i = j;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut j = i + 1;
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            is_double = true;
            i = j;
        }
    }
    let num = std::str::from_utf8(&s[start..i]).ok()?;
    let mut end = i;
    while end < s.len() && is_ws(s[end]) {
        end += 1;
    }
    let trailing = end < s.len();
    if !is_double {
        if let Ok(l) = num.parse::<i64>() {
            return Some((Numeric::Long(l), trailing));
        }
    }
    let d: f64 = num.parse().ok()?;
    Some((Numeric::Double(d), trailing))
}

fn fits_long(d: f64) -> bool {
    // ZEND_DOUBLE_FITS_LONG(): !(d >= (double)ZEND_LONG_MAX || d < (double)ZEND_LONG_MIN)
    #[allow(clippy::cast_precision_loss)]
    let max = i64::MAX as f64;
    #[allow(clippy::cast_precision_loss)]
    let min = i64::MIN as f64;
    !(d >= max || d < min)
}

#[allow(clippy::cast_possible_truncation)]
fn truncate(d: f64) -> i64 {
    d as i64
}

/// Whether converting `dval` to `lval` lost information
/// (`!zend_is_long_compatible()`).
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
fn loses_precision(dval: f64, lval: i64) -> bool {
    lval as f64 != dval
}

fn float_text(dval: f64) -> String {
    String::from_utf8_lossy(&Value::Float(dval).to_bytes().unwrap_or_default()).into_owned()
}

/// `zval_try_get_long()`. Returns `None` when the value cannot be used as
/// an integer (non-numeric strings, arrays, objects, resources).
pub fn try_get_long(value: &Value) -> Option<i64> {
    match value {
        Value::Null | Value::Bool(false) => Some(0),
        Value::Bool(true) => Some(1),
        Value::Int(i) => Some(*i),
        Value::Float(dval) => {
            let dval = *dval;
            if !dval.is_finite() || !fits_long(dval) {
                diag::warning(format!(
                    "The float {} is not representable as an int, cast occurred",
                    float_text(dval)
                ));
                return Some(0);
            }
            let lval = truncate(dval);
            if loses_precision(dval, lval) {
                diag::emit(Diagnostic::new(
                    Severity::Deprecated,
                    format!(
                        "Implicit conversion from float {} to int loses precision",
                        float_text(dval)
                    ),
                ));
            }
            Some(lval)
        }
        Value::Str(bytes) => {
            let (num, trailing) = numeric_prefix(bytes)?;
            if trailing {
                diag::warning("A non-numeric value encountered");
            }
            match num {
                Numeric::Long(lval) => Some(lval),
                Numeric::Double(dval) => {
                    let lval = if dval.is_nan() {
                        0
                    } else if dval >= 9.223_372_036_854_776e18 {
                        i64::MAX
                    } else if dval < -9.223_372_036_854_776e18 {
                        i64::MIN
                    } else {
                        truncate(dval)
                    };
                    if !dval.is_finite() || loses_precision(dval, lval) {
                        diag::emit(Diagnostic::new(
                            Severity::Deprecated,
                            format!(
                                "Implicit conversion from float-string \"{}\" to int loses precision",
                                String::from_utf8_lossy(bytes)
                            ),
                        ));
                    }
                    Some(lval)
                }
            }
        }
        Value::Array(_) | Value::Object { .. } | Value::Stream(_) | Value::Resource(_) => None,
    }
}

/// `zval_try_get_string()`: arrays convert to `"Array"` with a warning,
/// objects without `__toString()` throw.
pub fn try_get_string(v: &Value) -> Result<Vec<u8>, Error> {
    match v {
        Value::Array(_) => {
            diag::warning("Array to string conversion");
            Ok(b"Array".to_vec())
        }
        Value::Object {
            class,
            string_value,
        } => string_value.clone().ok_or_else(|| {
            Error::Error(format!(
                "Object of class {class} could not be converted to string"
            ))
        }),
        other => Ok(other.to_bytes().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longs() {
        let _ = diag::take();
        assert_eq!(try_get_long(&Value::Int(5)), Some(5));
        assert_eq!(try_get_long(&Value::str("15")), Some(15));
        assert_eq!(try_get_long(&Value::str(" -3 ")), Some(-3));
        assert_eq!(try_get_long(&Value::str("foo")), None);
        assert_eq!(try_get_long(&Value::str("")), None);
        assert_eq!(try_get_long(&Value::Array(vec![])), None);
        assert_eq!(try_get_long(&Value::Null), Some(0));
        assert_eq!(try_get_long(&Value::Bool(true)), Some(1));
        assert!(diag::take().is_empty());
        assert_eq!(try_get_long(&Value::str("12abc")), Some(12));
        assert_eq!(diag::take()[0].text(), "A non-numeric value encountered");
        assert_eq!(try_get_long(&Value::str("1e3")), Some(1000));
        assert_eq!(try_get_long(&Value::str("1.5")), Some(1));
        assert_eq!(diag::take().len(), 1);
        assert_eq!(try_get_long(&Value::Float(3.0)), Some(3));
        assert!(diag::take().is_empty());
    }

    #[test]
    fn strings() {
        let _ = diag::take();
        assert_eq!(try_get_string(&Value::Int(8)).unwrap(), b"8");
        let e = try_get_string(&Value::Object {
            class: "stdClass".into(),
            string_value: None,
        })
        .unwrap_err();
        assert_eq!(
            e,
            Error::Error("Object of class stdClass could not be converted to string".into())
        );
        assert_eq!(try_get_string(&Value::Array(vec![])).unwrap(), b"Array");
        assert_eq!(diag::take()[0].text(), "Array to string conversion");
    }

    #[test]
    fn names() {
        assert_eq!(value_name(&Value::Bool(true)), "true");
        assert_eq!(type_name(&Value::Bool(true)), "bool");
        assert_eq!(type_name(&Value::str("x")), "string");
    }
}
