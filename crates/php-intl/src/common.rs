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
}
