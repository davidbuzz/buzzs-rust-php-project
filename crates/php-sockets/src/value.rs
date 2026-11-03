//! A minimal model of the PHP values ext/sockets inspects
//! (`socket_set_option()` values, `socket_sendmsg()`/`socket_recvmsg()`
//! message arrays) and produces (`socket_get_option()` arrays, received
//! messages), with the Zend conversions the C code applies to them.
//!
//! The VM converts its own values to [`Value`] (references dereferenced;
//! objects with their class name, properties for array casts and, when the
//! class has `__toString()`, its result) and [`OutValue`] back.

use crate::Socket;

/// A PHP array key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
}

impl Key {
    /// A string key.
    pub fn s(k: &str) -> Self {
        Self::Str(k.as_bytes().to_vec())
    }
}

/// An input PHP value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `float`.
    Float(f64),
    /// `string`.
    Str(Vec<u8>),
    /// `array` (insertion order).
    Array(Vec<(Key, Value)>),
    /// Any object other than a `Socket`.
    Object {
        /// Class name.
        class: String,
        /// Properties as an `(array)` cast would expose them.
        props: Vec<(Key, Value)>,
        /// `__toString()` result, when the class has one.
        string: Option<Vec<u8>>,
    },
    /// A `Socket` object; `fd` is `-1` once closed.
    Socket {
        /// The descriptor.
        fd: i32,
    },
    /// A stream resource; `fd` when it can be cast to a descriptor.
    Stream {
        /// The descriptor.
        fd: Option<i32>,
    },
    /// Any other resource.
    Resource,
}

/// A string conversion that throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionError(pub String);

impl Value {
    /// Shorthand for a string value.
    pub fn str(s: &str) -> Self {
        Self::Str(s.as_bytes().to_vec())
    }

    /// An array from `(key, value)` pairs with string keys.
    pub fn assoc(pairs: Vec<(&str, Value)>) -> Self {
        Self::Array(pairs.into_iter().map(|(k, v)| (Key::s(k), v)).collect())
    }

    /// A list array.
    pub fn list(items: Vec<Value>) -> Self {
        Self::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Key::Int(i64::try_from(i).unwrap_or(i64::MAX)), v))
                .collect(),
        )
    }

    /// `zend_zval_value_name()`.
    pub fn value_name(&self) -> String {
        match self {
            Self::Bool(true) => "true".into(),
            Self::Bool(false) => "false".into(),
            _ => self.type_name(),
        }
    }

    /// `zend_zval_type_name()`.
    pub fn type_name(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(_) => "bool".into(),
            Self::Int(_) => "int".into(),
            Self::Float(_) => "float".into(),
            Self::Str(_) => "string".into(),
            Self::Array(_) => "array".into(),
            Self::Object { class, .. } => class.clone(),
            Self::Socket { .. } => "Socket".into(),
            Self::Stream { .. } | Self::Resource => "resource".into(),
        }
    }

    /// The hash table seen by `Z_ARRVAL`/properties (arrays and objects).
    pub fn table(&self) -> Option<&[(Key, Value)]> {
        match self {
            Self::Array(t) | Self::Object { props: t, .. } => Some(t),
            _ => None,
        }
    }

    /// `zend_hash_str_find()` on an array.
    pub fn get(&self, key: &str) -> Option<&Value> {
        let Self::Array(t) = self else { return None };
        find(t, key)
    }

    /// `zend_is_true()`.
    pub fn is_true(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Int(i) => *i != 0,
            Self::Float(f) => *f != 0.0,
            Self::Str(s) => !(s.is_empty() || s == b"0"),
            Self::Array(a) => !a.is_empty(),
            _ => true,
        }
    }

    /// `zval_get_long()` (lenient: leading-numeric strings, arrays as 0/1).
    pub fn to_long(&self) -> i64 {
        match self {
            Self::Null => 0,
            Self::Bool(b) => i64::from(*b),
            Self::Int(i) => *i,
            Self::Float(f) => dval_to_lval(*f),
            Self::Str(s) => match numeric_prefix(s) {
                Some(Numeric::Int(i)) => i,
                Some(Numeric::Float(f)) => dval_to_lval_cap(f),
                None => 0,
            },
            Self::Array(a) => i64::from(!a.is_empty()),
            _ => 1,
        }
    }

    /// `zval_try_get_long()`: `None` when PHP would throw a `TypeError`
    /// (arrays, objects, resources, non-numeric strings).
    pub fn try_long(&self) -> Option<i64> {
        match self {
            Self::Null | Self::Bool(_) | Self::Int(_) | Self::Float(_) => Some(self.to_long()),
            Self::Str(s) => match is_numeric(s) {
                Some(Numeric::Int(i)) => Some(i),
                Some(Numeric::Float(f)) => Some(dval_to_lval_cap(f)),
                None => None,
            },
            _ => None,
        }
    }

    /// `zval_try_get_string()`; the `Ok` flag is true when PHP also emits
    /// the "Array to string conversion" warning.
    pub fn to_php_string(&self) -> Result<(Vec<u8>, bool), ConversionError> {
        Ok(match self {
            Self::Null | Self::Bool(false) => (Vec::new(), false),
            Self::Bool(true) => (b"1".to_vec(), false),
            Self::Int(i) => (i.to_string().into_bytes(), false),
            Self::Float(f) => (float_to_string(*f).into_bytes(), false),
            Self::Str(s) => (s.clone(), false),
            Self::Array(_) => (b"Array".to_vec(), true),
            Self::Object { class, string, .. } => match string {
                Some(s) => (s.clone(), false),
                None => {
                    return Err(ConversionError(format!(
                        "Object of class {class} could not be converted to string"
                    )))
                }
            },
            Self::Socket { .. } => {
                return Err(ConversionError(
                    "Object of class Socket could not be converted to string".into(),
                ))
            }
            Self::Stream { .. } | Self::Resource => (b"Resource id #0".to_vec(), false),
        })
    }
}

/// Finds a string key in a table.
pub fn find<'a>(t: &'a [(Key, Value)], key: &str) -> Option<&'a Value> {
    // Numeric-string keys are stored as ints in PHP.
    let as_int = handle_numeric(key.as_bytes());
    t.iter()
        .find(|(k, _)| match (k, as_int) {
            (Key::Int(i), Some(n)) => *i == n,
            (Key::Str(s), None) => s == key.as_bytes(),
            _ => false,
        })
        .map(|(_, v)| v)
}

/// `ZEND_HANDLE_NUMERIC_STR`: canonical decimal integer keys.
pub fn handle_numeric(s: &[u8]) -> Option<i64> {
    let digits = s.strip_prefix(b"-").unwrap_or(s);
    if digits.is_empty()
        || digits.len() > 20
        || !digits.iter().all(u8::is_ascii_digit)
        || (digits[0] == b'0' && (digits.len() > 1 || digits.len() != s.len()))
    {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

/// A parsed numeric string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Numeric {
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Parses the longest numeric prefix (after leading whitespace); returns
/// it and the number of bytes consumed.
fn parse_numeric(s: &[u8]) -> Option<(Numeric, usize)> {
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
    let mut is_float = false;
    let mut frac_digits = 0;
    if i < s.len() && s[i] == b'.' {
        let mut j = i + 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
            frac_digits += 1;
        }
        if frac_digits > 0 || i > int_start {
            is_float = true;
            i = j;
        }
    }
    if i == int_start && frac_digits == 0 {
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
            is_float = true;
            i = j;
        }
    }
    let text = std::str::from_utf8(&s[start..i]).ok()?;
    if !is_float {
        if let Ok(v) = text.parse::<i64>() {
            return Some((Numeric::Int(v), i));
        }
    }
    text.parse::<f64>().ok().map(|f| (Numeric::Float(f), i))
}

/// `is_numeric_string()` (whole string, trailing whitespace allowed).
pub fn is_numeric(s: &[u8]) -> Option<Numeric> {
    let (n, used) = parse_numeric(s)?;
    s[used..].iter().all(|&b| is_ws(b)).then_some(n)
}

/// `is_numeric_string(..., allow_errors)`: the leading numeric prefix.
pub fn numeric_prefix(s: &[u8]) -> Option<Numeric> {
    parse_numeric(s).map(|(n, _)| n)
}

/// `zend_dval_to_lval()`: 0 for non-finite values, modular for out of
/// range ones.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn dval_to_lval(d: f64) -> i64 {
    if !d.is_finite() {
        return 0;
    }
    if d >= -9.223_372_036_854_775_808e18 && d < 9.223_372_036_854_775_808e18 {
        return d as i64;
    }
    // zend_dval_to_lval_slow: modular arithmetic on the truncated value.
    let two64 = 18_446_744_073_709_551_616.0_f64;
    let mut dmod = d.trunc() % two64;
    if dmod < 0.0 {
        dmod += two64;
    }
    if dmod >= 9.223_372_036_854_775_808e18 {
        dmod -= two64;
    }
    dmod as i64
}

/// `zend_dval_to_lval_cap()` (numeric strings): saturating.
#[allow(clippy::cast_possible_truncation)]
pub fn dval_to_lval_cap(d: f64) -> i64 {
    if d.is_nan() {
        0
    } else {
        d as i64
    }
}

/// PHP's float-to-string conversion (`precision`-independent shortest
/// round-trip form, as `serialize_precision = -1`).
pub fn float_to_string(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let s = format!("{f:e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (neg, mant) = mant.strip_prefix('-').map_or((false, mant), |m| (true, m));
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let sign = if neg { "-" } else { "" };
    if exp < -5 || exp >= 15 {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{first}.{rest}E{esign}{}", exp.abs())
    } else if exp < 0 {
        let zeros = "0".repeat(usize::try_from(-exp - 1).unwrap_or(0));
        format!("{sign}0.{zeros}{digits}")
    } else {
        let point = usize::try_from(exp + 1).unwrap_or(0);
        if digits.len() <= point {
            format!("{sign}{digits}{}", "0".repeat(point - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..point], &digits[point..])
        }
    }
}

/// An output PHP value.
#[derive(Debug)]
pub enum OutValue {
    /// `null`.
    Null,
    /// `false` (e.g. a `$message` whose conversion failed).
    False,
    /// `int`.
    Int(i64),
    /// `string`.
    Str(Vec<u8>),
    /// `array`.
    Array(Vec<(Key, OutValue)>),
    /// A new `Socket` object (received through `SCM_RIGHTS`).
    Socket(Socket),
    /// A new stream resource over a received non-socket descriptor
    /// (`php_stream_fopen_from_fd(fd, "rw")`).
    StreamFd(i32),
}

impl OutValue {
    /// An array with string keys.
    pub fn assoc(pairs: Vec<(&str, OutValue)>) -> Self {
        Self::Array(pairs.into_iter().map(|(k, v)| (Key::s(k), v)).collect())
    }

    /// A string value.
    pub fn str(s: &str) -> Self {
        Self::Str(s.as_bytes().to_vec())
    }

    /// Looks up a string key (tests and VM convenience).
    pub fn get(&self, key: &str) -> Option<&OutValue> {
        let Self::Array(t) = self else { return None };
        t.iter().find(|(k, _)| *k == Key::s(key)).map(|(_, v)| v)
    }

    /// The int payload.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The string payload.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(Value::str("1e3").to_long(), 1000);
        assert_eq!(Value::str(" 12abc").to_long(), 12);
        assert_eq!(Value::str("abc").to_long(), 0);
        assert_eq!(Value::Float(1e20).to_long(), 7_766_279_631_452_241_920);
        assert_eq!(Value::Float(f64::NAN).to_long(), 0);
        assert_eq!(Value::str("12").try_long(), Some(12));
        assert_eq!(Value::str(" 12 ").try_long(), Some(12));
        assert_eq!(Value::str("12abc").try_long(), None);
        assert_eq!(Value::Array(vec![]).try_long(), None);
        assert!(Value::str("0.0").is_true());
        assert!(!Value::str("0").is_true());
        assert_eq!(Value::Bool(true).value_name(), "true");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(float_to_string(1.5), "1.5");
        assert_eq!(float_to_string(100.0), "100");
        assert_eq!(float_to_string(1e25), "1.0E+25");
        assert_eq!(float_to_string(0.0001), "0.0001");
        assert_eq!(float_to_string(-2.5e-7), "-2.5E-7");
        assert_eq!(handle_numeric(b"12"), Some(12));
        assert_eq!(handle_numeric(b"012"), None);
        let a = Value::Array(vec![
            (Key::Int(5), Value::Int(1)),
            (Key::s("x"), Value::Int(2)),
        ]);
        assert_eq!(a.get("5"), Some(&Value::Int(1)));
        assert_eq!(a.get("x"), Some(&Value::Int(2)));
        assert_eq!(a.get("y"), None);
    }
}
