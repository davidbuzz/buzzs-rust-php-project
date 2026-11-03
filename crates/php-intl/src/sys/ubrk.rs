//! `ubrk_*` — break iterators.

use std::sync::Arc;

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UBreakIterator, UParseError};
use crate::sys::utext::Utf8Text;

/// `UBRK_DONE`.
pub const DONE: i32 = -1;

/// Owned `UBreakIterator*` plus the text it iterates.
#[derive(Debug)]
pub struct BreakIterator {
    bi: *mut UBreakIterator,
    /// Keeps the UTF-8 buffer referenced by ICU's (cloned) `UText` alive.
    text: Option<Arc<[u8]>>,
    /// Compiled rules ICU references (`ubrk_openBinaryRules`).
    binary: Option<Arc<[u8]>>,
}

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for BreakIterator {}

fn wrap(p: *mut UBreakIterator, st: UErrorCode) -> Result<BreakIterator, UErrorCode> {
    if st.is_failure() || p.is_null() {
        if !p.is_null() {
            // SAFETY: closed once.
            unsafe { ffi::ubrk_close(p) };
        }
        return Err(if st.is_failure() {
            st
        } else {
            UErrorCode::MEMORY_ALLOCATION_ERROR
        });
    }
    Ok(BreakIterator {
        bi: p,
        text: None,
        binary: None,
    })
}

impl BreakIterator {
    /// `ubrk_open(type, locale)` without text.
    pub fn open(ty: i32, locale: &str) -> Result<(Self, UErrorCode), UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: l NUL-terminated; null text is allowed.
        let p = unsafe { ffi::ubrk_open(ty, l.as_ptr(), std::ptr::null(), 0, &mut st) };
        let warn = st;
        wrap(p, st).map(|b| (b, warn))
    }

    /// `ubrk_openRules(rules)`.
    pub fn open_rules(rules: &[u16]) -> Result<Self, (UErrorCode, UParseError)> {
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        // SAFETY: rules is a live slice; null text is allowed.
        let p = unsafe {
            ffi::ubrk_openRules(
                rules.as_ptr(),
                len32(rules),
                std::ptr::null(),
                0,
                &mut pe,
                &mut st,
            )
        };
        wrap(p, st).map_err(|e| (e, pe))
    }

    /// `ubrk_openBinaryRules(binary)`.
    pub fn open_binary_rules(rules: Arc<[u8]>) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: ICU references the rule bytes; they are kept alive in
        // `binary` for the iterator's lifetime.
        let p = unsafe {
            ffi::ubrk_openBinaryRules(rules.as_ptr(), len32(&rules), std::ptr::null(), 0, &mut st)
        };
        let mut b = wrap(p, st)?;
        b.binary = Some(rules);
        Ok(b)
    }

    /// `ubrk_clone()` (sharing the text buffer).
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.bi is live.
        let p = unsafe { ffi::ubrk_clone(self.bi, &mut st) };
        let mut b = wrap(p, st)?;
        b.text.clone_from(&self.text);
        b.binary.clone_from(&self.binary);
        Ok(b)
    }

    /// `ubrk_setUText()` over UTF-8 bytes.
    pub fn set_text(&mut self, bytes: Arc<[u8]>) -> Result<(), (UErrorCode, bool)> {
        let ut = Utf8Text::open(bytes.clone()).map_err(|e| (e, true))?;
        let mut st = UErrorCode::ZERO;
        // SAFETY: both handles live; ICU shallow-clones the UText, whose
        // bytes we keep alive in self.text.
        unsafe { ffi::ubrk_setUText(self.bi, ut.as_ptr(), &mut st) };
        if st.is_failure() {
            return Err((st, false));
        }
        self.text = Some(bytes);
        Ok(())
    }

    /// `ubrk_first()`.
    pub fn first(&mut self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_first(self.bi) }
    }

    /// `ubrk_last()`.
    pub fn last(&mut self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_last(self.bi) }
    }

    /// `ubrk_next()`.
    pub fn next_boundary(&mut self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_next(self.bi) }
    }

    /// `ubrk_previous()`.
    pub fn previous(&mut self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_previous(self.bi) }
    }

    /// `ubrk_current()`.
    pub fn current(&self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_current(self.bi) }
    }

    /// `ubrk_following()`.
    pub fn following(&mut self, o: i32) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_following(self.bi, o) }
    }

    /// `ubrk_preceding()`.
    pub fn preceding(&mut self, o: i32) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_preceding(self.bi, o) }
    }

    /// `ubrk_isBoundary()`.
    pub fn is_boundary(&mut self, o: i32) -> bool {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_isBoundary(self.bi, o) != 0 }
    }

    /// `ubrk_getRuleStatus()`.
    pub fn rule_status(&mut self) -> i32 {
        // SAFETY: self.bi is live.
        unsafe { ffi::ubrk_getRuleStatus(self.bi) }
    }

    /// `ubrk_getRuleStatusVec()`.
    pub fn rule_status_vec(&mut self) -> Result<Vec<i32>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: preflight with a null buffer.
        let n = unsafe { ffi::ubrk_getRuleStatusVec(self.bi, std::ptr::null_mut(), 0, &mut st) };
        let mut v = vec![0i32; usize::try_from(n).unwrap_or(0)];
        let mut st = UErrorCode::ZERO;
        // SAFETY: v has `n` slots.
        let m = unsafe { ffi::ubrk_getRuleStatusVec(self.bi, v.as_mut_ptr(), len32(&v), &mut st) };
        st.check()?;
        v.truncate(usize::try_from(m).unwrap_or(0));
        Ok(v)
    }

    /// `ubrk_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.bi is live; the result is copied.
        let p = unsafe { ffi::ubrk_getLocaleByType(self.bi, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `ubrk_getBinaryRules()`.
    pub fn binary_rules(&mut self) -> Result<Vec<u8>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: preflight with a null buffer.
        let n = unsafe { ffi::ubrk_getBinaryRules(self.bi, std::ptr::null_mut(), 0, &mut st) };
        if st.is_failure() && st != UErrorCode::BUFFER_OVERFLOW_ERROR {
            return Err(st);
        }
        let mut v = vec![0u8; usize::try_from(n).unwrap_or(0)];
        let mut st = UErrorCode::ZERO;
        // SAFETY: v has `n` bytes.
        let m = unsafe { ffi::ubrk_getBinaryRules(self.bi, v.as_mut_ptr(), len32(&v), &mut st) };
        st.check()?;
        v.truncate(usize::try_from(m).unwrap_or(0));
        Ok(v)
    }
}

impl Drop for BreakIterator {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::ubrk_close(self.bi) }
    }
}
