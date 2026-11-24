// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ucol_*` — collation.

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UCollator};

/// Owned `UCollator*`.
#[derive(Debug)]
pub struct Collator(*mut UCollator);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for Collator {}

impl Collator {
    /// `ucol_open(locale)` → the collator and the (warning) status.
    pub fn open(locale: &str) -> Result<(Self, UErrorCode), UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: l is NUL-terminated.
        let p = unsafe { ffi::ucol_open(l.as_ptr(), &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::ucol_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok((Self(p), st))
    }

    /// `ucol_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::ucol_clone(self.0, &mut st) };
        if st.is_failure() || p.is_null() {
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `ucol_strcoll()` → -1/0/1.
    pub fn strcoll(&self, a: &[u16], b: &[u16]) -> i32 {
        // SAFETY: both slices are live.
        unsafe { ffi::ucol_strcoll(self.0, a.as_ptr(), len32(a), b.as_ptr(), len32(b)) }
    }

    /// `ucol_getAttribute()`.
    pub fn attribute(&self, attr: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucol_getAttribute(self.0, attr, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucol_setAttribute()`.
    pub fn set_attribute(&mut self, attr: i32, value: i32) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucol_setAttribute(self.0, attr, value, &mut st) };
        st.check()
    }

    /// `ucol_getStrength()`.
    pub fn strength(&self) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::ucol_getStrength(self.0) }
    }

    /// `ucol_setStrength()`.
    pub fn set_strength(&mut self, s: i32) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucol_setStrength(self.0, s) }
    }

    /// `ucol_getRules()`: the tailoring rules (empty for the root collator).
    pub fn rules(&self) -> Vec<u16> {
        let mut len: i32 = 0;
        // SAFETY: self.0 is live; ICU returns a pointer into the collator's
        // own data (valid while it lives) and its length, copied at once.
        let p = unsafe { ffi::ucol_getRules(self.0, &mut len) };
        let n = usize::try_from(len).unwrap_or(0);
        if p.is_null() || n == 0 {
            return Vec::new();
        }
        // SAFETY: `p` points to `len` UTF-16 units owned by the collator.
        unsafe { std::slice::from_raw_parts(p, n) }.to_vec()
    }

    /// `ucol_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is copied immediately.
        let p = unsafe { ffi::ucol_getLocaleByType(self.0, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `ucol_getSortKey()` (without the trailing NUL); `None` when ICU
    /// reports a zero length.
    pub fn sort_key(&self, s: &[u16]) -> Option<Vec<u8>> {
        // SAFETY: preflight with a null buffer of size 0 is allowed.
        let n =
            unsafe { ffi::ucol_getSortKey(self.0, s.as_ptr(), len32(s), std::ptr::null_mut(), 0) };
        let len = usize::try_from(n).ok().filter(|&n| n > 0)?;
        let mut buf = vec![0u8; len];
        // SAFETY: buf has `len` bytes.
        let m = unsafe {
            ffi::ucol_getSortKey(self.0, s.as_ptr(), len32(s), buf.as_mut_ptr(), len32(&buf))
        };
        let m = usize::try_from(m).ok().filter(|&m| m > 0)?;
        buf.truncate(m.saturating_sub(1));
        Some(buf)
    }
}

impl Drop for Collator {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::ucol_close(self.0) }
    }
}
