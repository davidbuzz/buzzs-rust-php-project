//! `Spoofchecker` (php-src `ext/intl/spoofchecker`).
//!
//! The object status is reset at the start of every method (as by
//! `INTL_METHOD_FETCH_OBJECT`).  Failures of the checking methods are
//! reported as `E_WARNING`s (`"(code) U_NAME"`, see [`Report`]) plus the
//! *global* intl error code (php-src master; 8.3 only warns); the checking
//! methods then return `true`.

use crate::error::{Error, Result, UErrorCode};
use crate::sys::uspoof::{self, SpoofChecker as Raw, UnicodeSet};
use crate::ustr;

/// Check flags (`USpoofChecks`).
pub mod checks {
    /// `Spoofchecker::SINGLE_SCRIPT_CONFUSABLE`
    pub const SINGLE_SCRIPT_CONFUSABLE: i64 = 1;
    /// `Spoofchecker::MIXED_SCRIPT_CONFUSABLE`
    pub const MIXED_SCRIPT_CONFUSABLE: i64 = 2;
    /// `Spoofchecker::WHOLE_SCRIPT_CONFUSABLE`
    pub const WHOLE_SCRIPT_CONFUSABLE: i64 = 4;
    /// `Spoofchecker::ANY_CASE`
    pub const ANY_CASE: i64 = 8;
    /// `Spoofchecker::SINGLE_SCRIPT` (`USPOOF_SINGLE_SCRIPT`, an alias of
    /// `USPOOF_RESTRICTION_LEVEL`)
    pub const SINGLE_SCRIPT: i64 = 16;
    /// `Spoofchecker::INVISIBLE`
    pub const INVISIBLE: i64 = 32;
    /// `Spoofchecker::CHAR_LIMIT`
    pub const CHAR_LIMIT: i64 = 64;
    /// `Spoofchecker::MIXED_NUMBERS`
    pub const MIXED_NUMBERS: i64 = 128;
    /// `Spoofchecker::HIDDEN_OVERLAY`
    pub const HIDDEN_OVERLAY: i64 = 256;
}

/// Restriction levels (`URestrictionLevel`).
pub mod level {
    /// `Spoofchecker::ASCII`
    pub const ASCII: i64 = 0x1000_0000;
    /// `Spoofchecker::SINGLE_SCRIPT_RESTRICTIVE`
    pub const SINGLE_SCRIPT_RESTRICTIVE: i64 = 0x2000_0000;
    /// `Spoofchecker::HIGHLY_RESTRICTIVE`
    pub const HIGHLY_RESTRICTIVE: i64 = 0x3000_0000;
    /// `Spoofchecker::MODERATELY_RESTRICTIVE`
    pub const MODERATELY_RESTRICTIVE: i64 = 0x4000_0000;
    /// `Spoofchecker::MINIMALLY_RESTRICTIVE`
    pub const MINIMALLY_RESTRICTIVE: i64 = 0x5000_0000;
    /// `Spoofchecker::UNRESTRICTIVE`
    pub const UNRESTRICTIVE: i64 = 0x6000_0000;
}

/// `USET_*` pattern options accepted by `setAllowedChars()`.
pub mod pattern_option {
    /// `Spoofchecker::IGNORE_SPACE`
    pub const IGNORE_SPACE: i64 = 1;
    /// `Spoofchecker::CASE_INSENSITIVE`
    pub const CASE_INSENSITIVE: i64 = 2;
    /// `Spoofchecker::ADD_CASE_MAPPINGS`
    pub const ADD_CASE_MAPPINGS: i64 = 4;
    /// `Spoofchecker::SIMPLE_CASE_INSENSITIVE` (ICU ≥ 73)
    pub const SIMPLE_CASE_INSENSITIVE: i64 = 6;
}

/// `Spoofchecker::LTR`
pub const LTR: i64 = 0;
/// `Spoofchecker::RTL`
pub const RTL: i64 = 1;

/// Message of the `Error` thrown by a second `__construct()` call.
pub const ALREADY_CONSTRUCTED: &str = "Spoofchecker object is already constructed";
/// Message of the `Error` thrown when using an unconstructed object.
pub const UNCONSTRUCTED: &str = "Found unconstructed Spoofchecker";
/// Message of the `Error` thrown when cloning an unconstructed object.
pub const CLONE_UNINITIALIZED: &str = "Cannot clone uninitialized SpoofChecker";
/// Message of the `Error` thrown when `uspoof_clone()` fails.
pub const CLONE_FAILED: &str = "Failed to clone SpoofChecker";

/// Side effects of a call that reports ICU failures as warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// When set, `intl_error_set_code(NULL, code)` (global code only; the
    /// global message stays empty, so `intl_get_error_message()` yields the
    /// bare error name).
    pub error: Option<UErrorCode>,
    /// `php_error_docref(NULL, E_WARNING, …)` texts, in order (the VM adds
    /// the `"Spoofchecker::method(): "` prefix).
    pub warnings: Vec<String>,
}

impl Report {
    fn fail(&mut self, code: UErrorCode) {
        self.error = Some(code);
        self.warnings.push(format!("({}) {}", code.0, code.name()));
    }
}

/// Result of `isSuspicious()` / `areConfusable()` / `areBidiConfusable()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The method's return value.
    pub value: bool,
    /// Value to assign to the by-reference `$errorCode` argument (when it
    /// was passed); `None` leaves it untouched.
    pub error_code: Option<i64>,
    /// Warnings / global error code.
    pub report: Report,
}

impl Check {
    fn from_status(ret: i64, status: UErrorCode) -> Self {
        let mut report = Report::default();
        if status.is_failure() {
            report.fail(status);
            return Self {
                value: true,
                error_code: None,
                report,
            };
        }
        Self {
            value: ret != 0,
            error_code: Some(ret),
            report,
        }
    }
}

/// A constructed PHP `Spoofchecker` object.
#[derive(Debug)]
pub struct Spoofchecker {
    raw: Raw,
    status: UErrorCode,
}

fn check_direction(direction: i64) -> Result<i32> {
    match direction {
        LTR => Ok(0),
        RTL => Ok(1),
        _ => Err(Error::arg_value(
            1,
            "must be either Spoofchecker::LTR or Spoofchecker::RTL",
        )),
    }
}

fn fits_i32(s: &[u8]) -> bool {
    i32::try_from(s.len()).is_ok()
}

/// `intl_icu_compat_uspoof_is_allowed_chars_pattern_option()`.
fn is_allowed_pattern_option(o: i64) -> bool {
    use pattern_option::{
        ADD_CASE_MAPPINGS, CASE_INSENSITIVE, IGNORE_SPACE, SIMPLE_CASE_INSENSITIVE,
    };
    o == 0
        || o == IGNORE_SPACE
        || o == IGNORE_SPACE | SIMPLE_CASE_INSENSITIVE
        || o == IGNORE_SPACE | CASE_INSENSITIVE
        || o == IGNORE_SPACE | ADD_CASE_MAPPINGS
}

impl Spoofchecker {
    /// `new Spoofchecker()`.
    pub fn new() -> Result<Self> {
        let mut status = UErrorCode::ZERO;
        let Some(mut raw) = Raw::open(&mut status).filter(|_| status.is_success()) else {
            return Err(Error::IntlException(
                "Spoofchecker::__construct(): unable to open ICU Spoof Checker".into(),
            ));
        };
        raw.init_default(&mut status);
        Ok(Self { raw, status })
    }

    /// `clone $checker`: a copy with a fresh status and no extended check
    /// result.
    pub fn try_clone(&self) -> Result<Self> {
        let raw = self
            .raw
            .try_clone()
            .map_err(|_| Error::Thrown(CLONE_FAILED.into()))?;
        Ok(Self {
            raw,
            status: UErrorCode::ZERO,
        })
    }

    /// The object's ICU status after the last call (`err.code`).
    #[must_use]
    pub fn status(&self) -> UErrorCode {
        self.status
    }

    /// `isSuspicious($string, &$errorCode)`.
    pub fn is_suspicious(&mut self, text: &[u8]) -> Check {
        self.status = UErrorCode::ZERO;
        let ret = self.raw.check_utf8(text, &mut self.status);
        if self.status.is_failure() {
            let mut report = Report::default();
            report.fail(self.status);
            let errmask = self.raw.result_checks(&mut self.status);
            if errmask != ret {
                report.warnings.push(format!(
                    "unexpected error ({ret}), does not relate to the flags passed to setChecks ({errmask})"
                ));
            }
            return Check {
                value: true,
                error_code: None,
                report,
            };
        }
        Check::from_status(i64::from(ret), self.status)
    }

    /// `areConfusable($string1, $string2, &$errorCode)`.
    pub fn are_confusable(&mut self, a: &[u8], b: &[u8]) -> Check {
        self.status = UErrorCode::ZERO;
        let mut ret = 0;
        if fits_i32(a) && fits_i32(b) {
            ret = self.raw.are_confusable(a, b, &mut self.status);
        } else {
            self.status = UErrorCode::BUFFER_OVERFLOW_ERROR;
        }
        Check::from_status(i64::from(ret), self.status)
    }

    /// `areBidiConfusable($direction, $string1, $string2, &$errorCode)`.
    pub fn are_bidi_confusable(&mut self, direction: i64, a: &[u8], b: &[u8]) -> Result<Check> {
        self.status = UErrorCode::ZERO;
        let dir = check_direction(direction)?;
        let mut ret = 0;
        if fits_i32(a) && fits_i32(b) {
            ret = self.raw.are_bidi_confusable(dir, a, b, &mut self.status);
        } else {
            self.status = UErrorCode::BUFFER_OVERFLOW_ERROR;
        }
        Ok(Check::from_status(i64::from(ret), self.status))
    }

    fn report(&self) -> Report {
        let mut r = Report::default();
        if self.status.is_failure() {
            r.fail(self.status);
        }
        r
    }

    /// `setAllowedLocales($locales)`.
    pub fn set_allowed_locales(&mut self, locales: &[u8]) -> Report {
        self.status = UErrorCode::ZERO;
        self.raw.set_allowed_locales(locales, &mut self.status);
        self.report()
    }

    /// `setChecks($checks)` (`zend_long` truncated to `int32_t`).
    #[allow(clippy::cast_possible_truncation)]
    pub fn set_checks(&mut self, checks: i64) -> Report {
        self.status = UErrorCode::ZERO;
        self.raw.set_checks(checks as i32, &mut self.status);
        self.report()
    }

    /// `uspoof_getChecks()` (not exposed to PHP; for tests/debugging).
    pub fn checks(&mut self) -> i32 {
        let mut st = UErrorCode::ZERO;
        self.raw.checks(&mut st)
    }

    /// `setRestrictionLevel($level)`.
    pub fn set_restriction_level(&mut self, lvl: i64) -> Result<()> {
        use level::{
            ASCII, HIGHLY_RESTRICTIVE, MINIMALLY_RESTRICTIVE, MODERATELY_RESTRICTIVE,
            SINGLE_SCRIPT_RESTRICTIVE, UNRESTRICTIVE,
        };
        let ok = [
            ASCII,
            SINGLE_SCRIPT_RESTRICTIVE,
            HIGHLY_RESTRICTIVE,
            MODERATELY_RESTRICTIVE,
            MINIMALLY_RESTRICTIVE,
            UNRESTRICTIVE,
        ];
        let Some(v) = ok.contains(&lvl).then(|| i32::try_from(lvl).ok()).flatten() else {
            return Err(Error::arg_value(
                1,
                "must be one of Spoofchecker::ASCII, Spoofchecker::SINGLE_SCRIPT_RESTRICTIVE, \
                 Spoofchecker::HIGHLY_RESTRICTIVE, Spoofchecker::MODERATELY_RESTRICTIVE, \
                 Spoofchecker::MINIMALLY_RESTRICTIVE, or Spoofchecker::UNRESTRICTIVE",
            ));
        };
        self.raw.set_restriction_level(v);
        Ok(())
    }

    /// `uspoof_getRestrictionLevel()` (not exposed to PHP).
    #[must_use]
    pub fn restriction_level(&self) -> i32 {
        self.raw.restriction_level()
    }

    /// `setAllowedChars($pattern, $patternOptions = 0)`.
    pub fn set_allowed_chars(&mut self, pattern: &[u8], options: i64) -> Result<Report> {
        self.status = UErrorCode::ZERO;
        if !fits_i32(pattern) {
            return Err(Error::arg_value(
                1,
                format!("must be less than or equal to {} bytes long", i32::MAX),
            ));
        }
        if pattern.first() != Some(&b'[') || pattern.last() != Some(&b']') {
            return Err(Error::arg_value(
                1,
                "must be a valid regular expression character set pattern",
            ));
        }
        let upattern = match ustr::to_u16(pattern) {
            Ok(u) => {
                self.status = UErrorCode::ZERO;
                u
            }
            Err(c) => {
                self.status = c;
                return Err(Error::arg_value(
                    1,
                    format!(
                        "string conversion to unicode encoding failed ({}) {}",
                        c.0,
                        c.name()
                    ),
                ));
            }
        };
        if !is_allowed_pattern_option(options) {
            return Err(Error::arg_value(
                2,
                "must be a valid pattern option, 0 or (SpoofChecker::IGNORE_SPACE|(<none> or \
                 SpoofChecker::CASE_INSENSITIVE or SpoofChecker::ADD_CASE_MAPPINGS or \
                 SpoofChecker::SIMPLE_CASE_INSENSITIVE))",
            ));
        }
        let mut set = UnicodeSet::empty().map_err(Error::code)?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        set.apply_pattern(&upattern, options as u32, &mut self.status);
        if self.status.is_failure() {
            return Err(Error::arg_value(
                1,
                format!(
                    "must be a valid regular expression character set pattern ({}) {}",
                    self.status.0,
                    self.status.name()
                ),
            ));
        }
        set.compact();
        self.raw.set_allowed_chars(&set, &mut self.status);
        Ok(self.report())
    }

    fn skeleton_with(
        &mut self,
        text: &[u8],
        mut f: impl FnMut(&Raw, &mut [u8], &mut UErrorCode) -> i32,
    ) -> Result<Vec<u8>> {
        const CONVERT: &str = "Failed to convert input string to UTF-16";
        const GENERATE: &str = "Failed to generate skeleton";
        if !fits_i32(text) {
            self.status = UErrorCode::BUFFER_OVERFLOW_ERROR;
            return Err(Error::intl(self.status, CONVERT));
        }
        uspoof::utf16_preflight(text, &mut self.status);
        if self.status.is_failure() && self.status != UErrorCode::BUFFER_OVERFLOW_ERROR {
            return Err(Error::intl(self.status, CONVERT));
        }
        self.status = UErrorCode::ZERO;
        let len = f(&self.raw, &mut [], &mut self.status);
        if self.status.is_failure() && self.status != UErrorCode::BUFFER_OVERFLOW_ERROR {
            return Err(Error::intl(self.status, GENERATE));
        }
        let len = usize::try_from(len).unwrap_or(0);
        let mut buf = vec![0u8; len + 1];
        self.status = UErrorCode::ZERO;
        let len = f(&self.raw, &mut buf, &mut self.status);
        if self.status.is_failure() {
            return Err(Error::intl(self.status, GENERATE));
        }
        self.status = UErrorCode::ZERO;
        buf.truncate(usize::try_from(len).unwrap_or(0));
        Ok(buf)
    }

    /// `getSkeleton($string)`; errors are recorded on the object and
    /// globally, and the method returns `false`.
    pub fn get_skeleton(&mut self, text: &[u8]) -> Result<Vec<u8>> {
        self.status = UErrorCode::ZERO;
        self.skeleton_with(text, |raw, dest, st| raw.skeleton_into(text, dest, st))
    }

    /// `getBidiSkeleton($direction, $string)`.
    pub fn get_bidi_skeleton(&mut self, direction: i64, text: &[u8]) -> Result<Vec<u8>> {
        self.status = UErrorCode::ZERO;
        let dir = check_direction(direction)?;
        self.skeleton_with(text, |raw, dest, st| {
            raw.bidi_skeleton_into(dir, text, dest, st)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confusable_basics() {
        let mut x = Spoofchecker::new().unwrap();
        assert!(!x.are_confusable(b"hello, world", b"goodbye, world").value);
        assert!(x.are_confusable(b"hello, world", b"hello, world").value);
        assert!(x.are_confusable(b"hello, world", b"he11o, wor1d").value);
    }

    #[test]
    fn set_checks_error_is_reported() {
        let mut x = Spoofchecker::new().unwrap();
        let r = x.set_checks(1 << 20);
        assert_eq!(r.error, Some(UErrorCode::ILLEGAL_ARGUMENT_ERROR));
        assert_eq!(r.warnings, vec!["(1) U_ILLEGAL_ARGUMENT_ERROR".to_string()]);
        let c = x.is_suspicious(b"abc");
        assert!(!c.value);
        assert_eq!(c.error_code, Some(0));
    }

    #[test]
    fn skeleton() {
        let mut x = Spoofchecker::new().unwrap();
        assert_eq!(x.get_skeleton(b"").unwrap(), b"");
        assert_eq!(x.get_skeleton("\u{FB01}".as_bytes()).unwrap(), b"fi");
        assert_eq!(x.get_skeleton(b"a\0b").unwrap(), b"a\0b");
        let e = x.get_skeleton(b"\x80").unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
        assert_eq!(x.get_skeleton(b"abc").unwrap(), b"abc");
    }

    #[test]
    fn bad_direction_and_level() {
        let mut x = Spoofchecker::new().unwrap();
        assert!(matches!(
            x.get_bidi_skeleton(2, b"a"),
            Err(Error::ArgumentValue { arg: 1, .. })
        ));
        assert!(matches!(
            x.set_restriction_level(checks::SINGLE_SCRIPT),
            Err(Error::ArgumentValue { arg: 1, .. })
        ));
    }
}
