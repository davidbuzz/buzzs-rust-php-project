//! `IntlListFormatter` (php-src `ext/intl/listformatter`, PHP 8.5).
//!
//! The VM converts each array element to a string first (`zval_try_get_tmp_string`,
//! so objects without `__toString` throw before this module is called) and
//! keeps the object's error state: every call resets it, failures are
//! recorded on the object and globally.

use crate::common::{default_locale, validate_locale_arg, INTL_MAX_LOCALE_LEN};
use crate::error::{Error, Result};
use crate::sys::ulistfmt::ListFormatter;
use crate::ustr;

/// `IntlListFormatter::TYPE_AND`
pub const TYPE_AND: i64 = 0;
/// `IntlListFormatter::TYPE_OR`
pub const TYPE_OR: i64 = 1;
/// `IntlListFormatter::TYPE_UNITS`
pub const TYPE_UNITS: i64 = 2;
/// `IntlListFormatter::WIDTH_WIDE`
pub const WIDTH_WIDE: i64 = 0;
/// `IntlListFormatter::WIDTH_SHORT`
pub const WIDTH_SHORT: i64 = 1;
/// `IntlListFormatter::WIDTH_NARROW`
pub const WIDTH_NARROW: i64 = 2;

/// Message of the `Error` thrown by a second `__construct()` call.
pub const ALREADY_CONSTRUCTED: &str = "IntlListFormatter object is already constructed";

/// A PHP `IntlListFormatter` object (not cloneable: the VM throws
/// `"Trying to clone an uncloneable object of class IntlListFormatter"`).
#[derive(Debug)]
pub struct IntlListFormatter {
    fmt: ListFormatter,
}

impl IntlListFormatter {
    /// `new IntlListFormatter($locale, $type, $width)`.
    ///
    /// ICU failure → global error `"Constructor failed"` plus an
    /// `IntlException("Constructor failed")` ([`Error::IntlException`]).
    pub fn new(locale: &str, kind: i64, width: i64) -> Result<Self> {
        if locale.len() > INTL_MAX_LOCALE_LEN {
            return Err(Error::arg_value(
                1,
                format!("must be less than or equal to {INTL_MAX_LOCALE_LEN} characters"),
            ));
        }
        let locale = if locale.is_empty() {
            default_locale()
        } else {
            locale.split('\0').next().unwrap_or_default().to_owned()
        };
        validate_locale_arg(&locale, 1)?;
        if ![TYPE_AND, TYPE_OR, TYPE_UNITS].contains(&kind) {
            return Err(Error::arg_value(
                2,
                "must be one of IntlListFormatter::TYPE_AND, IntlListFormatter::TYPE_OR, or \
                 IntlListFormatter::TYPE_UNITS",
            ));
        }
        if ![WIDTH_WIDE, WIDTH_SHORT, WIDTH_NARROW].contains(&width) {
            return Err(Error::arg_value(
                3,
                "must be one of IntlListFormatter::WIDTH_WIDE, IntlListFormatter::WIDTH_SHORT, \
                 or IntlListFormatter::WIDTH_NARROW",
            ));
        }
        #[allow(clippy::cast_possible_truncation)]
        let fmt = ListFormatter::open(&locale, kind as i32, width as i32)
            .map_err(|_| Error::IntlException("Constructor failed".into()))?;
        Ok(Self { fmt })
    }

    /// `format($strings)` over the already stringified elements.
    pub fn format<S: AsRef<[u8]>>(&self, items: &[S]) -> Result<Vec<u8>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut u = Vec::with_capacity(items.len());
        for s in items {
            u.push(
                ustr::to_u16(s.as_ref())
                    .map_err(|c| Error::intl(c, "Failed to convert string to UTF-16"))?,
            );
        }
        let out = self
            .fmt
            .format(&u)
            .map_err(|c| Error::intl(c, "Failed to format list"))?;
        ustr::from_u16(&out)
            .map(String::into_bytes)
            .map_err(|c| Error::intl(c, "Failed to convert result to UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UErrorCode;

    #[test]
    fn basics() {
        let f = IntlListFormatter::new("EN_US", TYPE_AND, WIDTH_WIDE).unwrap();
        assert_eq!(f.format(&["1", "2", "3"]).unwrap(), b"1, 2, and 3");
        assert_eq!(f.format::<&str>(&[]).unwrap(), b"");
        let e = f.format(&[b"\x80".as_slice()]).unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
    }
}
