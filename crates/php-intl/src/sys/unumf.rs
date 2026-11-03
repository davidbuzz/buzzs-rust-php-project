//! `unumf_*` — skeleton-based number formatting (ICU's `NumberFormatter`).

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UFormattedNumber, UNumberFormatter, UParseError};

/// Owned `UNumberFormatter*`.
#[derive(Debug)]
pub struct SkeletonFormatter(*mut UNumberFormatter);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for SkeletonFormatter {}

/// Owned `UFormattedNumber*`.
struct Result_(*mut UFormattedNumber);

impl Drop for Result_ {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumf_closeResult(self.0) }
    }
}

impl SkeletonFormatter {
    /// `unumf_openForSkeletonAndLocaleWithError()`.
    pub fn open(skeleton: &[u16], locale: &str) -> Result<Self, (UErrorCode, UParseError)> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        // SAFETY: skeleton is a live slice; l NUL-terminated.
        let p = unsafe {
            ffi::unumf_openForSkeletonAndLocaleWithError(
                skeleton.as_ptr(),
                len32(skeleton),
                l.as_ptr(),
                &mut pe,
                &mut st,
            )
        };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::unumf_close(p) };
            }
            return Err((
                if st.is_failure() {
                    st
                } else {
                    UErrorCode::MEMORY_ALLOCATION_ERROR
                },
                pe,
            ));
        }
        Ok(Self(p))
    }

    fn run(f: impl FnOnce(*mut UFormattedNumber, &mut UErrorCode)) -> Result<Vec<u16>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: no preconditions; ownership is taken by Result_.
        let r = Result_(unsafe { ffi::unumf_openResult(&mut st) });
        st.check()?;
        f(r.0, &mut st);
        st.check()?;
        // SAFETY: r.0 is a live result; buffer contract handled by fill_u16.
        ffi::fill_u16(|b, c, s| unsafe { ffi::unumf_resultToString(r.0, b, c, s) })
    }

    /// `unumf_formatDouble()`.
    pub fn format_f64(&self, v: f64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 and the result are live.
        Self::run(|r, st| unsafe { ffi::unumf_formatDouble(self.0, v, r, st) })
    }

    /// `unumf_formatInt()`.
    pub fn format_i64(&self, v: i64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 and the result are live.
        Self::run(|r, st| unsafe { ffi::unumf_formatInt(self.0, v, r, st) })
    }

    /// `unumf_formatDecimal()`.
    pub fn format_decimal(&self, v: &str) -> Result<Vec<u16>, UErrorCode> {
        let c = ffi::cstring(v);
        // SAFETY: self.0 and the result are live; c is NUL-terminated.
        Self::run(|r, st| unsafe {
            ffi::unumf_formatDecimal(self.0, c.as_ptr(), len32(v.as_bytes()), r, st);
        })
    }
}

impl Drop for SkeletonFormatter {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumf_close(self.0) }
    }
}
