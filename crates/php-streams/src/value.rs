// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A minimal PHP value type used at the crate boundary.
//!
//! Stream context options, filter parameters and the arguments / return
//! values of user-space stream wrappers and filters are PHP values. The VM
//! converts its own values to and from [`Value`]; this crate applies PHP's
//! conversion rules (`zend_is_true()`, `zval_get_long()`, string conversion)
//! where the C code does.

use std::fmt;

/// An array key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
}

impl Key {
    /// Builds a string key.
    pub fn str(s: impl Into<Vec<u8>>) -> Self {
        Key::Str(s.into())
    }

    /// Returns true when the key equals the string `s`.
    pub fn is(&self, s: &str) -> bool {
        matches!(self, Key::Str(k) if k == s.as_bytes())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Int(i) => write!(f, "{i}"),
            Key::Str(s) => f.write_str(&String::from_utf8_lossy(s)),
        }
    }
}

/// A PHP value as seen by the streams layer.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// `null`.
    #[default]
    Null,
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `float`.
    Float(f64),
    /// `string` (binary safe).
    Str(Vec<u8>),
    /// An ordered array.
    Array(Vec<(Key, Value)>),
    /// An object. `string_value` is the result of `__toString()` when the
    /// class has one (used when PHP converts the object to a string).
    Object {
        /// Class name, used in "%s given" messages.
        class: String,
        /// `__toString()` result, if the class is stringable.
        string_value: Option<Vec<u8>>,
    },
    /// A stream resource, identified by [`crate::stream::Stream::id`].
    Stream(u64),
    /// Any other resource.
    Resource(i64),
}

impl Value {
    /// Builds a string value.
    pub fn str(s: impl Into<Vec<u8>>) -> Self {
        Value::Str(s.into())
    }

    /// `zend_is_true()`.
    pub fn is_true(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !(s.is_empty() || s == b"0"),
            Value::Array(a) => !a.is_empty(),
            Value::Object { .. } | Value::Stream(_) | Value::Resource(_) => true,
        }
    }

    /// `zval_get_long()`: PHP's integer conversion (numeric-prefix rules for
    /// strings, truncation for floats, `count` for arrays is not used here so
    /// arrays give 0/1).
    pub fn to_long(&self) -> i64 {
        match self {
            Value::Null => 0,
            Value::Bool(b) => i64::from(*b),
            Value::Int(i) => *i,
            Value::Float(f) => float_to_long(*f),
            Value::Str(s) => str_to_long(s),
            Value::Array(a) => i64::from(!a.is_empty()),
            Value::Object { .. } => 1,
            Value::Stream(id) => i64::try_from(*id).unwrap_or(i64::MAX),
            Value::Resource(id) => *id,
        }
    }

    /// `zval_get_double()`: strings use their numeric prefix
    /// (`zend_strtod`).
    pub fn to_double(&self) -> f64 {
        match self {
            Value::Float(f) => *f,
            Value::Str(s) => str_to_double(s),
            #[allow(clippy::cast_precision_loss)] // PHP's (double) cast
            other => other.to_long() as f64,
        }
    }

    /// String conversion (`zval_get_string()` / `try_convert_to_string()`).
    /// Returns `None` for values PHP cannot convert (arrays convert to
    /// `"Array"` with a warning in PHP; objects without `__toString`).
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Value::Null | Value::Bool(false) => Some(Vec::new()),
            Value::Bool(true) => Some(b"1".to_vec()),
            Value::Int(i) => Some(i.to_string().into_bytes()),
            Value::Float(f) => Some(format_float(*f).into_bytes()),
            Value::Str(s) => Some(s.clone()),
            Value::Array(_) => Some(b"Array".to_vec()),
            Value::Object { string_value, .. } => string_value.clone(),
            Value::Stream(id) => Some(format!("Resource id #{id}").into_bytes()),
            Value::Resource(id) => Some(format!("Resource id #{id}").into_bytes()),
        }
    }

    /// `zend_zval_value_name()`: the type name used in "%s given" messages.
    pub fn type_name(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(true) => "true".into(),
            Value::Bool(false) => "false".into(),
            Value::Int(_) => "int".into(),
            Value::Float(_) => "float".into(),
            Value::Str(_) => "string".into(),
            Value::Array(_) => "array".into(),
            Value::Object { class, .. } => class.clone(),
            Value::Stream(_) | Value::Resource(_) => "resource".into(),
        }
    }

    /// Looks up a string key in an array value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Array(items) => items.iter().find(|(k, _)| k.is(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Returns the array entries, if this is an array.
    pub fn as_array(&self) -> Option<&[(Key, Value)]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// The outcome of calling a user-space method (`zend_call_method_if_exists`).
#[derive(Debug, Clone, PartialEq)]
pub enum UserCall {
    /// The method does not exist / is not callable (`FAILURE`).
    NotImplemented,
    /// The call raised an exception (the return value is `UNDEF`).
    Threw,
    /// The method returned a value.
    Returned(Value),
}

impl UserCall {
    /// The returned value, if any.
    pub fn value(&self) -> Option<&Value> {
        match self {
            UserCall::Returned(v) => Some(v),
            _ => None,
        }
    }

    /// `call_result == SUCCESS && retval != UNDEF && zend_is_true(retval)`.
    pub fn is_true(&self) -> bool {
        self.value().is_some_and(Value::is_true)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn float_to_long(f: f64) -> i64 {
    if !f.is_finite() {
        return 0;
    }
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f) {
        f as i64
    } else {
        // zend_dval_to_lval_cap is not used by zval_get_long; PHP 8 returns
        // the modular result. Approximate with wrapping arithmetic.
        let m = f.rem_euclid(18_446_744_073_709_551_616.0);
        (m as u64) as i64
    }
}

/// Integer conversion of a string following `ZEND_STRTOL`-like numeric
/// prefix rules used by `zval_get_long()` (`_zend_handle_numeric_str_ex`).
pub(crate) fn str_to_long(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let start = i;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let mut is_float = false;
    let mut j = i;
    if j < s.len() && s[j] == b'.' {
        j += 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        is_float = j > digits_start + 1 || i > digits_start;
    }
    if (j < s.len()) && (s[j] == b'e' || s[j] == b'E') && (i > digits_start || is_float) {
        let mut k = j + 1;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            k += 1;
        }
        if k < s.len() && s[k].is_ascii_digit() {
            while k < s.len() && s[k].is_ascii_digit() {
                k += 1;
            }
            is_float = true;
            j = k;
        }
    }
    if is_float {
        let text = std::str::from_utf8(&s[start..j]).unwrap_or("0");
        return float_to_long(text.parse::<f64>().unwrap_or(0.0));
    }
    if i == digits_start {
        return 0;
    }
    let text = std::str::from_utf8(&s[start..i]).unwrap_or("0");
    match text.parse::<i64>() {
        Ok(v) => v,
        Err(_) => float_to_long(text.parse::<f64>().unwrap_or(0.0)),
    }
}

/// Formats a float like PHP's default string conversion (precision 17
/// shortest round-trip representation, `serialize_precision = -1`).
#[allow(clippy::float_cmp)] // exact integral check, as in smart_str_append_double
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == f.trunc() && f.abs() < 1e15 {
        #[allow(clippy::cast_possible_truncation)]
        let i = f as i64;
        if i == 0 && f.is_sign_negative() {
            return "-0".into();
        }
        return i.to_string();
    }
    let s = format!("{f:?}");
    if let Some(pos) = s.find('e') {
        let (mant, exp) = s.split_at(pos);
        let exp = &exp[1..];
        let (sign, digits) = if let Some(d) = exp.strip_prefix('-') {
            ('-', d)
        } else {
            ('+', exp)
        };
        format!("{mant}E{sign}{digits}")
    } else {
        s
    }
}

/// `zend_strtod()` over the numeric prefix of `s` (leading whitespace
/// allowed); 0.0 when there is none.
#[allow(clippy::many_single_char_names)] // index arithmetic over one buffer
fn str_to_double(s: &[u8]) -> f64 {
    let mut i = s
        .iter()
        .take_while(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c'))
        .count();
    let start = i;
    if matches!(s.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let b = *i;
        while s.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - b
    };
    let mut n = digits(&mut i);
    if s.get(i) == Some(&b'.') {
        let mut j = i + 1;
        let f = digits(&mut j);
        if n + f > 0 {
            n += f;
            i = j;
        }
    }
    if n == 0 {
        return 0.0;
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(s.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if digits(&mut j) > 0 {
            i = j;
        }
    }
    std::str::from_utf8(&s[start..i])
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness() {
        assert!(!Value::str("0").is_true());
        assert!(!Value::str("").is_true());
        assert!(Value::str("0.0").is_true());
        assert!(!Value::Float(0.0).is_true());
        assert!(!Value::Array(vec![]).is_true());
        assert!(Value::Int(-1).is_true());
    }

    #[test]
    fn long_conversion() {
        assert_eq!(Value::str("12abc").to_long(), 12);
        assert_eq!(Value::str("  -7").to_long(), -7);
        assert_eq!(Value::str("1e3").to_long(), 1000);
        assert_eq!(Value::str("1.9").to_long(), 1);
        assert_eq!(Value::str("abc").to_long(), 0);
        assert_eq!(Value::Float(3.99).to_long(), 3);
        assert_eq!(Value::Bool(true).to_long(), 1);
    }

    #[test]
    fn string_conversion() {
        assert_eq!(Value::Int(5).to_bytes().unwrap(), b"5");
        assert_eq!(Value::Float(1.5).to_bytes().unwrap(), b"1.5");
        assert_eq!(Value::Float(2.0).to_bytes().unwrap(), b"2");
        assert_eq!(Value::Bool(false).to_bytes().unwrap(), b"");
        assert_eq!(
            Value::Object {
                class: "X".into(),
                string_value: None
            }
            .to_bytes(),
            None
        );
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Int(1).type_name(), "int");
        assert_eq!(Value::Bool(true).type_name(), "true");
    }
}
