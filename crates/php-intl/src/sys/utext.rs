// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `utext_*` over UTF-8 text owned by Rust.

use std::sync::Arc;

use crate::error::UErrorCode;
use crate::ffi::{self, UText};

/// A `UText` opened over UTF-8 bytes; keeps the bytes alive.
#[derive(Debug)]
pub struct Utf8Text {
    ut: *mut UText,
    bytes: Arc<[u8]>,
}

// SAFETY: the UText only references the Arc'd bytes; it is never shared.
unsafe impl Send for Utf8Text {}

impl Utf8Text {
    /// `utext_openUTF8()`.
    pub fn open(bytes: Arc<[u8]>) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let len = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        // SAFETY: bytes are kept alive by self for the UText's lifetime.
        let ut = unsafe {
            ffi::utext_openUTF8(std::ptr::null_mut(), bytes.as_ptr().cast(), len, &mut st)
        };
        if st.is_failure() || ut.is_null() {
            if !ut.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::utext_close(ut) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self { ut, bytes })
    }

    /// The raw handle (borrowed).
    pub fn as_ptr(&self) -> *mut UText {
        self.ut
    }

    /// The underlying bytes.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// `utext_nativeLength()`.
    pub fn native_length(&self) -> i64 {
        // SAFETY: self.ut is live.
        unsafe { ffi::utext_nativeLength(self.ut) }
    }

    /// `utext_getNativeIndex()`.
    pub fn native_index(&self) -> i64 {
        // SAFETY: self.ut is live.
        unsafe { ffi::utext_getNativeIndex(self.ut) }
    }

    /// `utext_setNativeIndex()`.
    pub fn set_native_index(&mut self, i: i64) {
        // SAFETY: self.ut is live and uniquely borrowed.
        unsafe { ffi::utext_setNativeIndex(self.ut, i) }
    }

    /// `utext_next32()`.
    pub fn next32(&mut self) -> i32 {
        // SAFETY: as above.
        unsafe { ffi::utext_next32(self.ut) }
    }

    /// `utext_previous32()`.
    pub fn previous32(&mut self) -> i32 {
        // SAFETY: as above.
        unsafe { ffi::utext_previous32(self.ut) }
    }

    /// `utext_next32From()`.
    pub fn next32_from(&mut self, i: i64) -> i32 {
        // SAFETY: as above.
        unsafe { ffi::utext_next32From(self.ut, i) }
    }

    /// `utext_previous32From()`.
    pub fn previous32_from(&mut self, i: i64) -> i32 {
        // SAFETY: as above.
        unsafe { ffi::utext_previous32From(self.ut, i) }
    }

    /// `utext_moveIndex32()`.
    pub fn move_index32(&mut self, n: i32) -> bool {
        // SAFETY: as above.
        unsafe { ffi::utext_moveIndex32(self.ut, n) != 0 }
    }

    /// `utext_current32()`.
    pub fn current32(&mut self) -> i32 {
        // SAFETY: as above.
        unsafe { ffi::utext_current32(self.ut) }
    }
}

impl Drop for Utf8Text {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once (the bytes outlive it).
        unsafe { ffi::utext_close(self.ut) };
    }
}
