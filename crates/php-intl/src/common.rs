//! Process-wide intl state and helpers shared by several classes:
//! the `intl.default_locale` INI value, version constants and locale
//! validation helpers.

use std::sync::Mutex;

/// `INTL_ICU_VERSION`.
pub const ICU_VERSION: &str = env!("PHP_INTL_ICU_VERSION");
/// `INTL_ICU_DATA_VERSION` (the bundled data matches the library).
pub const ICU_DATA_VERSION: &str = env!("PHP_INTL_ICU_VERSION");
/// `ULOC_FULLNAME_CAPACITY`.
pub const ULOC_FULLNAME_CAPACITY: usize = 157;
/// `INTL_MAX_LOCALE_LEN`.
pub const INTL_MAX_LOCALE_LEN: usize = ULOC_FULLNAME_CAPACITY - 1;

static DEFAULT_LOCALE: Mutex<Option<String>> = Mutex::new(None);

/// Sets the `intl.default_locale` INI value (`None` = unset).  The VM calls
/// this whenever the INI entry changes (including via
/// `Locale::setDefault()`).
pub fn set_ini_default_locale(value: Option<&str>) {
    if let Ok(mut g) = DEFAULT_LOCALE.lock() {
        *g = value.filter(|v| !v.is_empty()).map(str::to_owned);
    }
}

/// `intl_locale_get_default()`: the INI value, or ICU's default locale.
pub fn default_locale() -> String {
    if let Ok(g) = DEFAULT_LOCALE.lock() {
        if let Some(v) = g.as_ref() {
            return v.clone();
        }
    }
    icu_default_locale()
}

/// `uloc_getDefault()`.
pub fn icu_default_locale() -> String {
    crate::sys::loc::get_default()
}

/// The `"Locale string too long, should be no longer than 156 characters"`
/// check (`INTL_CHECK_LOCALE_LEN`).
pub fn check_locale_len(locale: &str) -> crate::Result<()> {
    if locale.len() > INTL_MAX_LOCALE_LEN {
        return Err(crate::Error::intl(
            crate::UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            format!(
                "Locale string too long, should be no longer than {INTL_MAX_LOCALE_LEN} characters"
            ),
        ));
    }
    Ok(())
}

/// Resolves an empty locale argument to the default locale.
pub fn locale_or_default(locale: &str) -> String {
    if locale.is_empty() {
        default_locale()
    } else {
        locale.to_owned()
    }
}

/// `canonicalize_locale_string()` (`uloc_canonicalize` into a
/// `ULOC_FULLNAME_CAPACITY` buffer; `None` on failure or empty result).
pub fn canonicalize_locale_string(locale: &str) -> Option<String> {
    let (s, st) = crate::sys::loc::canonicalize_fixed(locale, ULOC_FULLNAME_CAPACITY);
    if st.is_failure() || s.is_empty() {
        return None;
    }
    Some(s)
}

/// The master-branch locale validation shared by `NumberFormatter` and
/// `IntlDateFormatter`: `icu::Locale(locale).getISO3Language()` must be
/// non-empty, otherwise `ValueError: Argument #N ($locale) "x" is invalid`.
pub fn validate_locale_arg(locale: &str, arg: u32) -> crate::Result<()> {
    if crate::sys::loc::iso3_language(locale).is_empty() {
        return Err(crate::Error::arg_value(
            arg,
            format!("\"{locale}\" is invalid"),
        ));
    }
    Ok(())
}

/// `uloc_getKeywordValue(locale, keyword)`; `None` when absent.
pub fn locale_keyword_value(locale: &str, keyword: &str) -> Option<String> {
    crate::sys::loc::keyword_value(locale, keyword)
        .ok()
        .filter(|v| !v.is_empty())
}

/// The result of PHP's `is_numeric_string()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Numeric {
    /// `IS_LONG`
    Long(i64),
    /// `IS_DOUBLE`
    Double(f64),
}

/// PHP 8's `is_numeric_string()` (no errors allowed): optional leading and
/// trailing whitespace, a decimal integer or float; integers that overflow
/// become doubles.
#[allow(clippy::many_single_char_names)]
pub fn numeric_string(s: &[u8]) -> Option<Numeric> {
    let ws = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = s.iter().position(|&c| !ws(c))?;
    let end = s.iter().rposition(|&c| !ws(c)).map_or(start, |e| e + 1);
    let t = &s[start..end];
    let mut i = 0;
    if i < t.len() && (t[i] == b'+' || t[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < t.len() && t[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut is_double = false;
    let mut frac_digits = 0;
    if i < t.len() && t[i] == b'.' {
        is_double = true;
        i += 1;
        let f = i;
        while i < t.len() && t[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - f;
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }
    if i < t.len() && (t[i] == b'e' || t[i] == b'E') {
        let mut j = i + 1;
        if j < t.len() && (t[j] == b'+' || t[j] == b'-') {
            j += 1;
        }
        let d = j;
        while j < t.len() && t[j].is_ascii_digit() {
            j += 1;
        }
        if j > d {
            is_double = true;
            i = j;
        }
    }
    if i != t.len() {
        return None;
    }
    let text = std::str::from_utf8(t).ok()?;
    if !is_double {
        if let Ok(v) = text.parse::<i64>() {
            return Some(Numeric::Long(v));
        }
    }
    text.parse::<f64>().ok().map(Numeric::Double)
}

fn is_php_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Length of the longest float literal prefix of `s` (after `start`):
/// `[+-]?digits[.digits][(e|E)[+-]digits]`.
fn float_prefix_len(s: &[u8]) -> (usize, bool) {
    let mut i = 0;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let d0 = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - d0;
    let mut is_double = false;
    if i < s.len() && s[i] == b'.' {
        let f0 = i + 1;
        let mut j = f0;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if digits > 0 || j > f0 {
            digits += j - f0;
            is_double = true;
            i = j;
        }
    }
    if digits == 0 {
        return (0, false);
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut j = i + 1;
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            j += 1;
        }
        let e0 = j;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            i = j;
            is_double = true;
        }
    }
    (i, is_double)
}

/// `zend_strtod(str, NULL)` as used by `zval_get_double()` on strings:
/// leading whitespace, then the longest float prefix (0 if none).
pub fn php_strtod(s: &[u8]) -> f64 {
    let start = s.iter().position(|&c| !is_php_ws(c)).unwrap_or(s.len());
    let t = &s[start..];
    let (n, _) = float_prefix_len(t);
    std::str::from_utf8(&t[..n])
        .ok()
        .and_then(|x| x.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// `zval_get_long()` on a string: `is_numeric_string` allowing trailing
/// garbage; doubles are converted with `zend_dval_to_lval_cap`.
pub fn php_str_to_long(s: &[u8]) -> i64 {
    let start = s.iter().position(|&c| !is_php_ws(c)).unwrap_or(s.len());
    let t = &s[start..];
    let (n, is_double) = float_prefix_len(t);
    if n == 0 {
        return 0;
    }
    let text = std::str::from_utf8(&t[..n]).unwrap_or("0");
    if !is_double {
        if let Ok(v) = text.parse::<i64>() {
            return v;
        }
    }
    let d = text.parse::<f64>().unwrap_or(0.0);
    dval_to_lval_cap(d)
}

/// `zend_dval_to_lval_cap()`: saturating conversion (0 for NaN).
#[allow(clippy::cast_possible_truncation)]
pub fn dval_to_lval_cap(d: f64) -> i64 {
    if d.is_nan() {
        return 0;
    }
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        return if d > 0.0 { i64::MAX } else { i64::MIN };
    }
    d as i64
}

/// PHP's `(string)$float` (`zend_gcvt` with `precision = 14`).
pub fn php_float_to_string(v: f64) -> String {
    php_gcvt(v, 14)
}

/// `zend_gcvt(value, ndigit, '.', 'E')`.
pub fn php_gcvt(value: f64, ndigit: usize) -> String {
    if value.is_nan() {
        return "NAN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "INF".into()
        } else {
            "-INF".into()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let ndigit = ndigit.max(1);
    let e = format!("{:.*e}", ndigit - 1, value.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let decpt = exp + 1;
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    let nd = i32::try_from(ndigit).unwrap_or(i32::MAX);
    if if decpt < 0 { decpt < -3 } else { decpt > nd } {
        let mut d = decpt - 1;
        let neg = d < 0;
        if neg {
            d = -d;
        }
        out.push_str(&digits[..1]);
        out.push('.');
        if digits.len() == 1 {
            out.push('0');
        } else {
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if neg { '-' } else { '+' });
        out.push_str(&d.to_string());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in decpt..0 {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let dp = usize::try_from(decpt).unwrap_or(0);
        if digits.len() <= dp {
            out.push_str(&digits);
            for _ in digits.len()..dp {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..dp]);
            out.push('.');
            out.push_str(&digits[dp..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric() {
        assert_eq!(numeric_string(b" 12 "), Some(Numeric::Long(12)));
        assert_eq!(numeric_string(b"1.5e3"), Some(Numeric::Double(1500.0)));
        assert_eq!(numeric_string(b".5"), Some(Numeric::Double(0.5)));
        assert_eq!(numeric_string(b"12abc"), None);
        assert_eq!(numeric_string(b""), None);
        assert_eq!(
            numeric_string(b"99999999999999999999"),
            Some(Numeric::Double(1e20))
        );
        assert_eq!(numeric_string(b"-"), None);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn conversions() {
        assert_eq!(php_strtod(b" 1.5abc"), 1.5);
        assert_eq!(php_strtod(b".5"), 0.5);
        assert_eq!(php_strtod(b"abc"), 0.0);
        assert_eq!(php_str_to_long(b"1e3"), 1000);
        assert_eq!(php_str_to_long(b"12abc"), 12);
        assert_eq!(php_str_to_long(b".5"), 0);
        assert_eq!(php_str_to_long(b"99999999999999999999"), i64::MAX);
        assert_eq!(php_float_to_string(1.5), "1.5");
        assert_eq!(php_float_to_string(0.1 + 0.2), "0.3");
        assert_eq!(php_float_to_string(1e20), "1.0E+20");
        assert_eq!(php_float_to_string(-1e-5), "-1.0E-5");
        assert_eq!(
            php_float_to_string(123_456_789_012_345.67),
            "1.2345678901235E+14"
        );
        assert_eq!(php_float_to_string(0.0001), "0.0001");
        assert_eq!(php_float_to_string(100.0), "100");
        assert_eq!(php_float_to_string(1e14), "1.0E+14");
        assert_eq!(php_float_to_string(12_345_678_901_234.0), "12345678901234");
    }
}
