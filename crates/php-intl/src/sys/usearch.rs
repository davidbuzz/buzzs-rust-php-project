// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `usearch_*` — collation-based string search.

use std::sync::Arc;

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UStringSearch};
use crate::sys::ubrk::BreakIterator;

/// `USEARCH_DONE`.
pub const DONE: i32 = -1;

/// An owned `UStringSearch*` borrowing a break iterator.
#[derive(Debug)]
pub struct StringSearch<'a> {
    s: *mut UStringSearch,
    bi: *mut ffi::UBreakIterator,
    _pattern: Arc<[u16]>,
    _text: Arc<[u16]>,
    _bi: std::marker::PhantomData<&'a mut BreakIterator>,
}

impl<'a> StringSearch<'a> {
    /// `usearch_open(pattern, text, locale, breakiter)`.
    pub fn open(
        pattern: Arc<[u16]>,
        text: Arc<[u16]>,
        locale: &str,
        bi: &'a mut BreakIterator,
    ) -> Result<Self, UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: pattern/text are kept alive by self; bi is borrowed for 'a.
        let s = unsafe {
            ffi::usearch_open(
                pattern.as_ptr(),
                len32(&pattern),
                text.as_ptr(),
                len32(&text),
                l.as_ptr(),
                bi.as_ptr(),
                &mut st,
            )
        };
        if st.is_failure() || s.is_null() {
            if !s.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::usearch_close(s) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self {
            s,
            bi: bi.as_ptr(),
            _pattern: pattern,
            _text: text,
            _bi: std::marker::PhantomData,
        })
    }

    /// Sets the collator strength to secondary (case-insensitive) and resets.
    pub fn set_secondary_strength(&mut self) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: the collator is owned by the live search object.
        unsafe {
            let coll = ffi::usearch_getCollator(self.s);
            ffi::ucol_setAttribute(coll, 5, 1, &mut st); // UCOL_STRENGTH, UCOL_SECONDARY
        }
        st.check()?;
        // SAFETY: self.s is live.
        unsafe { ffi::usearch_reset(self.s) };
        Ok(())
    }

    /// `usearch_setOffset()`.
    pub fn set_offset(&mut self, pos: i32) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.s is live.
        unsafe { ffi::usearch_setOffset(self.s, pos, &mut st) };
        st.check()
    }

    /// `usearch_next()`.
    pub fn next_match(&mut self) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.s is live.
        let r = unsafe { ffi::usearch_next(self.s, &mut st) };
        st.check().map(|()| r)
    }

    /// `usearch_last()`.
    pub fn last_match(&mut self) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.s is live.
        let r = unsafe { ffi::usearch_last(self.s, &mut st) };
        st.check().map(|()| r)
    }

    /// `ubrk_following()` on the borrowed iterator.
    pub fn following(&mut self, pos: i32) -> i32 {
        // SAFETY: the iterator is borrowed (mutably) for 'a and still live.
        unsafe { ffi::ubrk_following(self.bi, pos) }
    }

    /// `ubrk_isBoundary()` on the borrowed iterator.
    pub fn is_boundary(&mut self, pos: i32) -> bool {
        // SAFETY: as above.
        unsafe { ffi::ubrk_isBoundary(self.bi, pos) != 0 }
    }
}

impl Drop for StringSearch<'_> {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::usearch_close(self.s) }
    }
}
