//! Numeric-string classification and string → number conversion.
//!
//! Ports of `_is_numeric_string_ex`, `zend_strtod` (prefix parsing) and
//! `_zend_handle_numeric_str_ex` (canonical integer array keys) with PHP 8
//! semantics.

/// `MAX_LENGTH_OF_LONG` on 64-bit platforms.
const MAX_LENGTH_OF_LONG: usize = 20;
/// `LONG_MIN_DIGITS`.
const LONG_MIN_DIGITS: &[u8] = b"9223372036854775808";

/// A number extracted from a numeric string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    /// An integer that fits in `i64` (`IS_LONG`).
    Int(i64),
    /// A float (`IS_DOUBLE`): has a decimal point/exponent or overflowed.
    Float(f64),
}

impl Number {
    /// Returns the value as a float.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_f64(self) -> f64 {
        match self {
            Number::Int(i) => i as f64,
            Number::Float(f) => f,
        }
    }
}

/// Result of [`is_numeric_string_ex`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericString {
    /// The parsed number.
    pub value: Number,
    /// `oflow_info`: `1` if the string is an integer above `i64::MAX`, `-1` if
    /// below `i64::MIN`, `0` otherwise.
    pub overflow: i8,
    /// Whether there was trailing non-whitespace data (only possible when
    /// `allow_errors` is true): the string is "leading-numeric".
    pub trailing_data: bool,
}

#[inline]
fn byte_at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

#[inline]
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Port of `_is_numeric_string_ex` (always computing the value).
///
/// Returns `None` if the string is not numeric. With `allow_errors == false`
/// only fully numeric strings (leading and trailing whitespace allowed) are
/// accepted; with `allow_errors == true` leading-numeric strings such as
/// `"123abc"` are accepted and flagged with [`NumericString::trailing_data`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn is_numeric_string_ex(s: &[u8], allow_errors: bool) -> Option<NumericString> {
    if s.is_empty() {
        return None;
    }
    // Skip leading whitespace.
    let mut start = 0;
    while start < s.len() && is_ws(s[start]) {
        start += 1;
    }
    let str_begin = start; // `str` in the C code (after whitespace)
    let mut ptr = start;
    let mut neg = false;
    match byte_at(s, ptr) {
        b'-' => {
            neg = true;
            ptr += 1;
        }
        b'+' => ptr += 1,
        _ => {}
    }

    let mut overflow: i8 = 0;
    let mut is_double = false;
    let mut digits = 0usize;
    let mut tmp_lval: u64 = 0;

    if byte_at(s, ptr).is_ascii_digit() {
        while byte_at(s, ptr) == b'0' {
            ptr += 1;
        }
        loop {
            if digits >= MAX_LENGTH_OF_LONG {
                // Too many digits for a long: overflow, parse as double.
                overflow = if byte_at(s, str_begin) == b'-' { -1 } else { 1 };
                is_double = true;
                break;
            }
            let c = byte_at(s, ptr);
            if c.is_ascii_digit() {
                tmp_lval = tmp_lval.wrapping_mul(10).wrapping_add(u64::from(c - b'0'));
                digits += 1;
                ptr += 1;
                continue;
            } else if c == b'.' {
                is_double = true;
                break;
            } else if c == b'e' || c == b'E' {
                let mut e = ptr + 1;
                if matches!(byte_at(s, e), b'-' | b'+') {
                    ptr = e;
                    e += 1;
                }
                if byte_at(s, e).is_ascii_digit() {
                    is_double = true;
                    break;
                }
            }
            break;
        }
    } else if byte_at(s, ptr) == b'.' && byte_at(s, ptr + 1).is_ascii_digit() {
        is_double = true;
    } else {
        return None;
    }

    let mut dval = 0.0;
    if is_double {
        let (v, end) = strtod(&s[str_begin..]);
        dval = v;
        ptr = str_begin + end;
    }

    let mut trailing_data = false;
    if ptr != s.len() {
        let mut endptr = ptr;
        while endptr < s.len() && is_ws(s[endptr]) {
            endptr += 1;
        }
        if endptr != s.len() {
            if !allow_errors {
                return None;
            }
            trailing_data = true;
        }
    }

    if !is_double {
        if digits == MAX_LENGTH_OF_LONG - 1 {
            // strcmp(&ptr[-digits], long_min_digits): compares the C string
            // starting at the digits (up to the NUL terminator).
            let dstart = ptr - digits;
            let mut cmp = s[dstart..ptr].cmp(LONG_MIN_DIGITS);
            if cmp == std::cmp::Ordering::Equal && byte_at(s, ptr) != 0 {
                cmp = std::cmp::Ordering::Greater;
            }
            let fits = cmp == std::cmp::Ordering::Less
                || (cmp == std::cmp::Ordering::Equal && byte_at(s, str_begin) == b'-');
            if !fits {
                let (v, _) = strtod(&s[str_begin..]);
                return Some(NumericString {
                    value: Number::Float(v),
                    overflow: if byte_at(s, str_begin) == b'-' { -1 } else { 1 },
                    trailing_data,
                });
            }
        }
        let mut l = tmp_lval;
        if neg {
            l = l.wrapping_neg();
        }
        #[allow(clippy::cast_possible_wrap)]
        let lval = l as i64;
        return Some(NumericString {
            value: Number::Int(lval),
            overflow: 0,
            trailing_data,
        });
    }
    Some(NumericString {
        value: Number::Float(dval),
        overflow,
        trailing_data,
    })
}

/// `is_numeric_string(str, len, &l, &d, allow_errors)`: returns just the number.
#[must_use]
pub fn is_numeric_string(s: &[u8], allow_errors: bool) -> Option<Number> {
    is_numeric_string_ex(s, allow_errors).map(|n| n.value)
}

/// Whether `s` is a numeric string in the sense of PHP's `is_numeric()`
/// (fully numeric, whitespace allowed around).
#[must_use]
pub fn is_numeric(s: &[u8]) -> bool {
    is_numeric_string_ex(s, false).is_some()
}

/// Port of `zend_strtod`: parses the longest valid decimal float prefix of `s`
/// (after optional leading whitespace and sign).
///
/// Returns the value and the number of bytes consumed; when no number could be
/// parsed the result is `(0.0, 0)`. Hexadecimal floats, `inf` and `nan` are not
/// recognized (as in PHP).
#[must_use]
pub fn strtod(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && is_ws(s[i]) {
        i += 1;
    }
    let num_start = i;
    let mut negative = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        negative = s[i] == b'-';
        i += 1;
    }
    let int_start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < s.len() && s[i] == b'.' {
        let mut j = i + 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        frac_digits = j - i - 1;
        if int_digits > 0 || frac_digits > 0 {
            i = j;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return (0.0, 0);
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut j = i + 1;
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            j += 1;
        }
        if j < s.len() && s[j].is_ascii_digit() {
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let text = &s[num_start..i];
    // The slice only contains ASCII sign/digits/./e characters.
    let parsed = std::str::from_utf8(text)
        .ok()
        .and_then(|t| parse_decimal(t, negative));
    (parsed.unwrap_or(0.0), i)
}

/// Parses a validated decimal literal, guarding against pathological exponents.
fn parse_decimal(text: &str, negative: bool) -> Option<f64> {
    if let Ok(v) = text.parse::<f64>() {
        return Some(v);
    }
    // Rust's parser rejects exponents that do not fit its internal type; fall
    // back to manual saturation in that case.
    let lower = text.to_ascii_lowercase();
    let (mantissa, exp) = lower.split_once('e')?;
    let mant: f64 = mantissa.parse().ok()?;
    if mant == 0.0 {
        return Some(if negative { -0.0 } else { 0.0 });
    }
    let exp_negative = exp.starts_with('-');
    let v = if exp_negative { 0.0 } else { f64::INFINITY };
    Some(if negative { -v } else { v })
}

/// Parses `s` like `(float)$s` / `zval_get_double` on a string (`zend_strtod`
/// on the whole string; non-numeric prefixes yield `0.0`).
#[must_use]
pub fn str_to_f64(s: &[u8]) -> f64 {
    strtod(s).0
}

/// Port of `ZEND_HANDLE_NUMERIC_STR`: returns `Some(i)` when `key` is the
/// canonical decimal representation of an integer (no leading zeros, no `+`,
/// no whitespace, `-0` excluded, within `i64` range) and therefore must be
/// stored as an integer array key.
#[must_use]
pub fn handle_numeric_str(key: &[u8]) -> Option<i64> {
    let first = *key.first()?;
    let mut tmp = 0;
    if first > b'9' {
        return None;
    } else if first < b'0' {
        if first != b'-' {
            return None;
        }
        tmp = 1;
        let c = *key.get(1)?;
        if !c.is_ascii_digit() {
            return None;
        }
    }
    let digits = &key[tmp..];
    if (digits[0] == b'0' && key.len() > 1) || digits.len() > MAX_LENGTH_OF_LONG - 1 {
        return None;
    }
    let mut idx: u64 = 0;
    for &c in digits {
        if !c.is_ascii_digit() {
            return None;
        }
        idx = idx * 10 + u64::from(c - b'0');
    }
    if tmp == 1 {
        if idx.wrapping_sub(1) > i64::MAX as u64 {
            return None;
        }
        #[allow(clippy::cast_possible_wrap)]
        Some(0u64.wrapping_sub(idx) as i64)
    } else if idx > i64::MAX as u64 {
        None
    } else {
        #[allow(clippy::cast_possible_wrap)]
        Some(idx as i64)
    }
}

/// Formats an integer in decimal (`zend_long_to_str`).
#[must_use]
pub fn long_to_bytes(n: i64) -> Vec<u8> {
    n.to_string().into_bytes()
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn num(s: &str, allow: bool) -> Option<(Number, i8, bool)> {
        is_numeric_string_ex(s.as_bytes(), allow).map(|n| (n.value, n.overflow, n.trailing_data))
    }

    #[test]
    fn integers() {
        assert_eq!(num("123", false), Some((Number::Int(123), 0, false)));
        assert_eq!(num("  123  ", false), Some((Number::Int(123), 0, false)));
        assert_eq!(
            num("\t\n\r\x0b\x0c-5", false),
            Some((Number::Int(-5), 0, false))
        );
        assert_eq!(num("+5", false), Some((Number::Int(5), 0, false)));
        assert_eq!(num("0005", false), Some((Number::Int(5), 0, false)));
        assert_eq!(num("0", false), Some((Number::Int(0), 0, false)));
        assert_eq!(num("-0", false), Some((Number::Int(0), 0, false)));
        assert_eq!(
            num("9223372036854775807", false),
            Some((Number::Int(i64::MAX), 0, false))
        );
        assert_eq!(
            num("-9223372036854775808", false),
            Some((Number::Int(i64::MIN), 0, false))
        );
    }

    #[test]
    fn overflow() {
        assert_eq!(
            num("9223372036854775808", false),
            Some((Number::Float(9_223_372_036_854_775_808.0), 1, false))
        );
        assert_eq!(
            num("-9223372036854775809", false),
            Some((Number::Float(-9_223_372_036_854_775_809.0), -1, false))
        );
        assert_eq!(
            num("99999999999999999999", false),
            Some((Number::Float(1e20), 1, false))
        );
        // quirk: trailing whitespace makes LONG_MIN overflow to float
        assert_eq!(
            num("-9223372036854775808 ", false),
            Some((Number::Float(-9.223_372_036_854_776e18), -1, false))
        );
    }

    #[test]
    fn floats() {
        assert_eq!(num("1.5", false), Some((Number::Float(1.5), 0, false)));
        assert_eq!(num(".5", false), Some((Number::Float(0.5), 0, false)));
        assert_eq!(num("1.", false), Some((Number::Float(1.0), 0, false)));
        assert_eq!(num("1e3", false), Some((Number::Float(1000.0), 0, false)));
        assert_eq!(num("1E-3", false), Some((Number::Float(0.001), 0, false)));
        assert_eq!(
            num("-1.5e+3 ", false),
            Some((Number::Float(-1500.0), 0, false))
        );
        assert_eq!(
            num("1e400", false),
            Some((Number::Float(f64::INFINITY), 0, false))
        );
    }

    #[test]
    fn non_numeric() {
        for s in [
            "", " ", "abc", ".", "-", "+", "1e", "0x1A", "1 2", " .e1", "e5", "-.e3", "1.5.5",
        ] {
            assert_eq!(num(s, false), None, "{s:?}");
        }
    }

    #[test]
    fn leading_numeric() {
        assert_eq!(num("123abc", true), Some((Number::Int(123), 0, true)));
        assert_eq!(num("1e", true), Some((Number::Int(1), 0, true)));
        assert_eq!(num("1e+", true), Some((Number::Int(1), 0, true)));
        assert_eq!(num("1.5x", true), Some((Number::Float(1.5), 0, true)));
        assert_eq!(num("12 ", true), Some((Number::Int(12), 0, false)));
        assert_eq!(num("12 a", true), Some((Number::Int(12), 0, true)));
        assert_eq!(num("abc", true), None);
        assert_eq!(num("1\0", true), Some((Number::Int(1), 0, true)));
    }

    #[test]
    fn strtod_prefixes() {
        assert_eq!(strtod(b"0x1A"), (0.0, 1));
        assert_eq!(strtod(b" 1e5x"), (100_000.0, 4));
        assert_eq!(strtod(b"-.e3"), (0.0, 0));
        assert_eq!(strtod(b"1e"), (1.0, 1));
        assert_eq!(strtod(b"abc"), (0.0, 0));
        assert!(strtod(b"-0").0.is_sign_negative());
        assert_eq!(strtod(b"1e-400").0, 0.0);
        assert_eq!(strtod(b"1e99999999999999999999").0, f64::INFINITY);
        assert_eq!(strtod(b"1e-99999999999999999999").0, 0.0);
    }

    #[test]
    fn numeric_keys() {
        assert_eq!(handle_numeric_str(b"0"), Some(0));
        assert_eq!(handle_numeric_str(b"123"), Some(123));
        assert_eq!(handle_numeric_str(b"-5"), Some(-5));
        assert_eq!(handle_numeric_str(b"-0"), None);
        assert_eq!(handle_numeric_str(b"01"), None);
        assert_eq!(handle_numeric_str(b" 1"), None);
        assert_eq!(handle_numeric_str(b"1 "), None);
        assert_eq!(handle_numeric_str(b"+1"), None);
        assert_eq!(handle_numeric_str(b""), None);
        assert_eq!(handle_numeric_str(b"-"), None);
        assert_eq!(handle_numeric_str(b"1.0"), None);
        assert_eq!(handle_numeric_str(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(handle_numeric_str(b"9223372036854775808"), None);
        assert_eq!(handle_numeric_str(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(handle_numeric_str(b"-9223372036854775809"), None);
        assert_eq!(handle_numeric_str(b"99999999999999999999"), None);
    }
}
