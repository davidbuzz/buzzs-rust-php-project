//! `IntlNumberFormatter` (php-src `ext/intl/numberformatter`, PHP 8.5):
//! ICU's skeleton-based `LocalizedNumberFormatter`.
//!
//! Creation errors are global (`createFromSkeleton()` returns `null`);
//! `format()` errors are recorded on the object and globally (it returns
//! `false`).  The VM owns the error state and the `intl.use_exceptions`
//! handling; messages are unprefixed.

use crate::common::{
    canonicalize_locale_string, default_locale, validate_locale_arg, INTL_MAX_LOCALE_LEN,
};
use crate::error::{Error, Result, UErrorCode};
use crate::sys::unumf::SkeletonFormatter;
use crate::ustr;

/// Message of the `Error` thrown by `new IntlNumberFormatter()` when the
/// private constructor is reached (e.g. via reflection).
pub const DIRECT_CONSTRUCTION: &str =
    "Cannot directly construct IntlNumberFormatter, use createFromSkeleton method instead";

/// A `format()` argument (`Z_PARAM_NUMBER_OR_STR`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatValue<'a> {
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
    /// `string` (a decimal number; no exponents)
    Str(&'a [u8]),
}

/// A PHP `IntlNumberFormatter` object (not cloneable).
#[derive(Debug)]
pub struct IntlNumberFormatter {
    fmt: SkeletonFormatter,
}

/// php-src rejects `scale/<digits>e…` before ICU sees it.
fn scale_has_exponent(skeleton: &[u8]) -> bool {
    const KEY: &[u8] = b"scale/";
    let Some(pos) = skeleton.windows(KEY.len()).position(|w| w == KEY) else {
        return false;
    };
    let rest = &skeleton[pos + KEY.len()..];
    let n = rest
        .iter()
        .take_while(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-'))
        .count();
    matches!(rest.get(n), Some(b'e' | b'E'))
}

impl IntlNumberFormatter {
    /// `IntlNumberFormatter::createFromSkeleton($skeleton, $locale)`.
    ///
    /// Global intl errors (`Error::global`) mean "return `null`".
    pub fn create_from_skeleton(skeleton: &[u8], locale: &str) -> Result<Self> {
        if locale.len() > INTL_MAX_LOCALE_LEN {
            return Err(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                format!(
                    "Locale string too long, should be no longer than {INTL_MAX_LOCALE_LEN} characters"
                ),
            ));
        }
        let locale = if locale.is_empty() {
            default_locale()
        } else {
            locale.split('\0').next().unwrap_or_default().to_owned()
        };
        validate_locale_arg(&locale, 2)?;
        if scale_has_exponent(skeleton) {
            return Err(Error::global(
                UErrorCode::NUMBER_SKELETON_SYNTAX_ERROR,
                "Number formatter creation failed",
            ));
        }
        let usk = ustr::to_u16(skeleton)
            .map_err(|c| Error::global(c, "Error converting skeleton to UTF-16"))?;
        let canonical = canonicalize_locale_string(&locale).unwrap_or(locale);
        let fmt = SkeletonFormatter::open(&usk, &canonical)
            .map_err(|(c, _)| Error::global(c, "Number formatter creation failed"))?;
        Ok(Self { fmt })
    }

    /// `format($value)`.
    pub fn format(&self, value: FormatValue<'_>) -> Result<Vec<u8>> {
        const FAILED: &str = "Number formatting failed";
        let out = match value {
            FormatValue::Str(s) => {
                if s.contains(&0) {
                    return Err(Error::arg_value(1, "must not contain any null bytes"));
                }
                if i32::try_from(s.len()).is_err() {
                    return Err(Error::arg_value(1, "is too long"));
                }
                if s.iter().any(|&b| b == b'e' || b == b'E') {
                    return Err(Error::intl(UErrorCode::DECIMAL_NUMBER_SYNTAX_ERROR, FAILED));
                }
                self.fmt.format_decimal(s)
            }
            FormatValue::Float(f) => self.fmt.format_f64(f),
            FormatValue::Int(i) => self.fmt.format_i64(i),
        }
        .map_err(|c| Error::intl(c, FAILED))?;
        ustr::from_u16(&out)
            .map(String::into_bytes)
            .map_err(|c| Error::intl(c, "Error converting value to UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_exponent_detection() {
        assert!(scale_has_exponent(b"precision-unlimited scale/1e3000000"));
        assert!(scale_has_exponent(b"scale/1.5E3"));
        assert!(!scale_has_exponent(b"scale/1000"));
        assert!(!scale_has_exponent(b"percent"));
    }

    #[test]
    fn formats() {
        let f = IntlNumberFormatter::create_from_skeleton(b"percent", "en_US").unwrap();
        assert_eq!(f.format(FormatValue::Int(25)).unwrap(), b"25%");
    }
}
