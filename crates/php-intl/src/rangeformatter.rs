//! `IntlNumberRangeFormatter` (php-src `ext/intl/rangeformatter`, PHP 8.5).
//!
//! ICU failures are recorded with `intl.use_exceptions` forced on and
//! `intl.error_level` forced off, so they always throw `IntlException`
//! (the returned [`IntlError`](crate::error::IntlError)s have
//! `always_throw` set).  Creation errors are global; `format()` errors are
//! recorded on the object and globally.

use crate::common::{default_locale, validate_locale_arg, INTL_MAX_LOCALE_LEN};
use crate::error::{Error, IntlError, Result, Scope};
use crate::sys::unumf::RangeFormatter;
use crate::ustr;

pub use crate::sys::unumf::RangeValue;

/// `IntlNumberRangeFormatter::COLLAPSE_AUTO`
pub const COLLAPSE_AUTO: i64 = 0;
/// `IntlNumberRangeFormatter::COLLAPSE_NONE`
pub const COLLAPSE_NONE: i64 = 1;
/// `IntlNumberRangeFormatter::COLLAPSE_UNIT`
pub const COLLAPSE_UNIT: i64 = 2;
/// `IntlNumberRangeFormatter::COLLAPSE_ALL`
pub const COLLAPSE_ALL: i64 = 3;
/// `IntlNumberRangeFormatter::IDENTITY_FALLBACK_SINGLE_VALUE`
pub const IDENTITY_FALLBACK_SINGLE_VALUE: i64 = 0;
/// `IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE`
pub const IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE: i64 = 1;
/// `IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY`
pub const IDENTITY_FALLBACK_APPROXIMATELY: i64 = 2;
/// `IntlNumberRangeFormatter::IDENTITY_FALLBACK_RANGE`
pub const IDENTITY_FALLBACK_RANGE: i64 = 3;

/// Message of the `Error` thrown by the private constructor.
pub const DIRECT_CONSTRUCTION: &str =
    "Cannot directly construct IntlNumberRangeFormatter, use createFromSkeleton method instead";

/// A PHP `IntlNumberRangeFormatter` object (not cloneable).
#[derive(Debug)]
pub struct IntlNumberRangeFormatter {
    fmt: RangeFormatter,
}

fn throwing(code: crate::UErrorCode, msg: &'static str) -> Error {
    Error::Intl(IntlError::new(code, msg).throwing())
}

impl IntlNumberRangeFormatter {
    /// `IntlNumberRangeFormatter::createFromSkeleton($skeleton, $locale,
    /// $collapse, $identityFallback)`.
    pub fn create_from_skeleton(
        skeleton: &[u8],
        locale: &str,
        collapse: i64,
        identity_fallback: i64,
    ) -> Result<Self> {
        if locale.len() > INTL_MAX_LOCALE_LEN {
            return Err(Error::arg_value(
                2,
                format!("must be no longer than {INTL_MAX_LOCALE_LEN} characters"),
            ));
        }
        let locale = if locale.is_empty() {
            default_locale()
        } else {
            locale.split('\0').next().unwrap_or_default().to_owned()
        };
        validate_locale_arg(&locale, 2)?;
        if !(COLLAPSE_AUTO..=COLLAPSE_ALL).contains(&collapse) {
            return Err(Error::arg_value(
                3,
                "must be one of IntlNumberRangeFormatter::COLLAPSE_AUTO, \
                 IntlNumberRangeFormatter::COLLAPSE_NONE, IntlNumberRangeFormatter::COLLAPSE_UNIT, \
                 or IntlNumberRangeFormatter::COLLAPSE_ALL",
            ));
        }
        if !(IDENTITY_FALLBACK_SINGLE_VALUE..=IDENTITY_FALLBACK_RANGE).contains(&identity_fallback)
        {
            return Err(Error::arg_value(
                4,
                "must be one of IntlNumberRangeFormatter::IDENTITY_FALLBACK_SINGLE_VALUE, \
                 IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE, \
                 IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY, or \
                 IntlNumberRangeFormatter::IDENTITY_FALLBACK_RANGE",
            ));
        }
        // `UnicodeString(char*, len)` converts with the default codepage
        // (UTF-8 here), substituting ill-formed sequences.
        let usk = ustr::str_u16(&String::from_utf8_lossy(skeleton));
        #[allow(clippy::cast_possible_truncation)]
        let fmt = RangeFormatter::open(&usk, collapse as i32, identity_fallback as i32, &locale)
            .map_err(|c| {
                Error::Intl(
                    IntlError::new(c, "Failed to create the number skeleton")
                        .throwing()
                        .with_scope(Scope::Global),
                )
            })?;
        Ok(Self { fmt })
    }

    /// `format($start, $end)`.
    pub fn format(&self, start: RangeValue, end: RangeValue) -> Result<Vec<u8>> {
        let out = self
            .fmt
            .format(start, end)
            .map_err(|c| throwing(c, "Failed to format number range"))?;
        ustr::from_u16(&out)
            .map(String::into_bytes)
            .map_err(|c| throwing(c, "Failed to convert result to UTF-8"))
    }
}
