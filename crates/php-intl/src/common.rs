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
