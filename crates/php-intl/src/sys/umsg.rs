//! `umsg_*` — used to validate MessageFormat patterns exactly as ICU does
//! (formatting itself is implemented in Rust, see `crate::msgformat`).

use std::ffi::c_void;

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UParseError};

/// Owned `UMessageFormat*`.
#[derive(Debug)]
pub struct MessageFormat(*mut *mut c_void);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for MessageFormat {}

impl MessageFormat {
    /// `umsg_open(pattern, locale)`.
    pub fn open(pattern: &[u16], locale: &str) -> Result<Self, (UErrorCode, UParseError)> {
        let l = ffi::cstring(locale);
        let mut pe = UParseError::default();
        let mut st = UErrorCode::ZERO;
        let ptr = if pattern.is_empty() { std::ptr::null() } else { pattern.as_ptr() };
        // SAFETY: pattern pointer/length describe a live slice (or null/0);
        // l is NUL-terminated.
        let p = unsafe { ffi::umsg_open(ptr, len32(pattern), l.as_ptr(), &mut pe, &mut st) };
        if st.is_failure() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::umsg_close(p) };
            }
            return Err((st, pe));
        }
        if p.is_null() {
            return Err((UErrorCode::MEMORY_ALLOCATION_ERROR, pe));
        }
        Ok(Self(p))
    }

    /// `umsg_applyPattern()`.
    pub fn apply_pattern(&mut self, pattern: &[u16]) -> Result<(), (UErrorCode, UParseError)> {
        let mut pe = UParseError::default();
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; pattern is a live slice.
        unsafe { ffi::umsg_applyPattern(self.0, pattern.as_ptr(), len32(pattern), &mut pe, &mut st) };
        if st.is_failure() {
            Err((st, pe))
        } else {
            Ok(())
        }
    }

    /// `umsg_getLocale()`.
    pub fn locale(&self) -> String {
        // SAFETY: self.0 is live; the result is owned by the formatter and copied.
        unsafe { ffi::cstr_to_string(ffi::umsg_getLocale(self.0)) }.unwrap_or_default()
    }

    /// `umsg_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::umsg_clone(self.0, &mut st) };
        if st.is_failure() || p.is_null() {
            return Err(if st.is_failure() { st } else { UErrorCode::MEMORY_ALLOCATION_ERROR });
        }
        Ok(Self(p.cast()))
    }
}

impl Drop for MessageFormat {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::umsg_close(self.0) }
    }
}
