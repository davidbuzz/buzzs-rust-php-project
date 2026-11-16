// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `uplrules_*` — plural rules.

use crate::error::UErrorCode;
use crate::ffi::{self, fill_u16, UPluralRules};
use crate::sys::unum::NumberFormat;

/// Owned `UPluralRules*`.
#[derive(Debug)]
pub struct PluralRules(*mut UPluralRules);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for PluralRules {}

impl PluralRules {
    /// `uplrules_openForType(locale, type)` (0 = cardinal, 1 = ordinal).
    pub fn open(locale: &str, ty: i32) -> Result<Self, UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: l is NUL-terminated.
        let p = unsafe { ffi::uplrules_openForType(l.as_ptr(), ty, &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::uplrules_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `uplrules_select()`.
    pub fn select(&self, n: f64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, c, st| unsafe { ffi::uplrules_select(self.0, n, b, c, st) })
    }

    /// `uplrules_selectWithFormat()` (uses the visible fraction digits of
    /// `fmt` when it is a `DecimalFormat`).
    pub fn select_with_format(&self, n: f64, fmt: &NumberFormat) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16; both handles live.
        fill_u16(|b, c, st| unsafe {
            ffi::uplrules_selectWithFormat(self.0, n, fmt.as_ptr(), b, c, st)
        })
    }
}

impl Drop for PluralRules {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::uplrules_close(self.0) }
    }
}
