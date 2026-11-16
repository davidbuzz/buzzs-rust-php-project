// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `udatpg_*` — date/time pattern generator.

use crate::error::UErrorCode;
use crate::ffi::{self, fill_u16, len32, UDateTimePatternGenerator};

/// Owned `UDateTimePatternGenerator*`.
#[derive(Debug)]
pub struct PatternGenerator(*mut UDateTimePatternGenerator);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for PatternGenerator {}

impl PatternGenerator {
    /// `udatpg_open(locale)`.
    pub fn open(locale: &str) -> Result<Self, UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: l is NUL-terminated.
        let p = unsafe { ffi::udatpg_open(l.as_ptr(), &mut st) };
        Self::wrap(p, st)
    }

    fn wrap(p: *mut UDateTimePatternGenerator, st: UErrorCode) -> Result<Self, UErrorCode> {
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::udatpg_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `udatpg_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::udatpg_clone(self.0, &mut st) };
        Self::wrap(p, st)
    }

    /// `udatpg_getBestPattern()`.
    pub fn best_pattern(&mut self, skeleton: &[u16]) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16; skeleton is live.
        fill_u16(|b, c, st| unsafe {
            ffi::udatpg_getBestPattern(self.0, skeleton.as_ptr(), len32(skeleton), b, c, st)
        })
    }
}

impl Drop for PatternGenerator {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::udatpg_close(self.0) }
    }
}

/// `udatpg_getSkeleton(NULL, pattern)`.
pub fn skeleton(pattern: &[u16]) -> Result<Vec<u16>, UErrorCode> {
    // SAFETY: buffer contract handled by fill_u16; a NULL generator is allowed.
    fill_u16(|b, c, st| unsafe {
        ffi::udatpg_getSkeleton(
            std::ptr::null_mut(),
            pattern.as_ptr(),
            len32(pattern),
            b,
            c,
            st,
        )
    })
}

/// `udatpg_getBaseSkeleton(NULL, pattern)`.
pub fn base_skeleton(pattern: &[u16]) -> Result<Vec<u16>, UErrorCode> {
    // SAFETY: as above.
    fill_u16(|b, c, st| unsafe {
        ffi::udatpg_getBaseSkeleton(
            std::ptr::null_mut(),
            pattern.as_ptr(),
            len32(pattern),
            b,
            c,
            st,
        )
    })
}
