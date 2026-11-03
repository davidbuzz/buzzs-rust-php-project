//! `uspoof_*` and the `uset_*` calls needed to build an allowed set.
//!
//! The ext/intl Spoofchecker threads a single, never-reset object status
//! through every ICU call, so these wrappers take the status by reference
//! instead of starting from `U_ZERO_ERROR`.

use crate::error::UErrorCode;
use crate::ffi::{self, USet, USpoofCheckResult, USpoofChecker};

/// An owned `USpoofChecker*` plus the (optional) extended check result
/// used by `uspoof_check2UTF8()`.
#[derive(Debug)]
pub struct SpoofChecker {
    sc: *mut USpoofChecker,
    res: *mut USpoofCheckResult,
}

impl Drop for SpoofChecker {
    fn drop(&mut self) {
        // SAFETY: both pointers are owned by self (or null, which the
        // close functions accept).
        unsafe {
            if !self.res.is_null() {
                ffi::uspoof_closeCheckResult(self.res);
            }
            ffi::uspoof_close(self.sc);
        }
    }
}

/// Convert a byte length to ICU's `int32_t`; callers reject longer input.
fn ilen(s: &[u8]) -> i32 {
    ffi::len32(s)
}

impl SpoofChecker {
    /// `uspoof_open()`.  Returns `None` when ICU hands back no checker.
    pub fn open(st: &mut UErrorCode) -> Option<Self> {
        // SAFETY: st is a valid out-pointer.
        let sc = unsafe { ffi::uspoof_open(st) };
        if sc.is_null() {
            return None;
        }
        Some(Self {
            sc,
            res: std::ptr::null_mut(),
        })
    }

    /// `intl_icu_compat_uspoof_init_checker()`: highly restrictive level
    /// plus a freshly opened check result.
    pub fn init_default(&mut self, st: &mut UErrorCode) {
        // SAFETY: self.sc is a live checker; st is a valid out-pointer.
        unsafe {
            ffi::uspoof_setRestrictionLevel(self.sc, RESTRICTION_HIGHLY_RESTRICTIVE);
            let res = ffi::uspoof_openCheckResult(st);
            if !self.res.is_null() {
                ffi::uspoof_closeCheckResult(self.res);
            }
            self.res = res;
        }
    }

    /// `uspoof_clone()`; the copy has no check result (as in PHP's
    /// clone handler).
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.sc is live; st is a valid out-pointer.
        let sc = unsafe { ffi::uspoof_clone(self.sc, &mut st) };
        if sc.is_null() {
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        let out = Self {
            sc,
            res: std::ptr::null_mut(),
        };
        st.check()?;
        Ok(out)
    }

    /// `uspoof_check2UTF8()` (with the stored check result).
    pub fn check_utf8(&self, text: &[u8], st: &mut UErrorCode) -> i32 {
        // SAFETY: text is a live slice; res may be null (allowed).
        unsafe { ffi::uspoof_check2UTF8(self.sc, text.as_ptr().cast(), ilen(text), self.res, st) }
    }

    /// `uspoof_getCheckResultChecks()` on the stored result.
    pub fn result_checks(&self, st: &mut UErrorCode) -> i32 {
        if st.is_failure() {
            return 0;
        }
        if self.res.is_null() {
            *st = UErrorCode::ILLEGAL_ARGUMENT_ERROR;
            return 0;
        }
        // SAFETY: res is a live check result.
        unsafe { ffi::uspoof_getCheckResultChecks(self.res, st) }
    }

    /// `uspoof_areConfusableUTF8()`.
    pub fn are_confusable(&self, a: &[u8], b: &[u8], st: &mut UErrorCode) -> i32 {
        // SAFETY: a and b are live slices.
        unsafe {
            ffi::uspoof_areConfusableUTF8(
                self.sc,
                a.as_ptr().cast(),
                ilen(a),
                b.as_ptr().cast(),
                ilen(b),
                st,
            )
        }
    }

    /// `uspoof_areBidiConfusableUTF8()`.
    pub fn are_bidi_confusable(&self, dir: i32, a: &[u8], b: &[u8], st: &mut UErrorCode) -> u32 {
        // SAFETY: a and b are live slices; dir was validated by the caller.
        unsafe {
            ffi::uspoof_areBidiConfusableUTF8(
                self.sc,
                dir,
                a.as_ptr().cast(),
                ilen(a),
                b.as_ptr().cast(),
                ilen(b),
                st,
            )
        }
    }

    /// `uspoof_setAllowedLocales()`.
    pub fn set_allowed_locales(&mut self, locales: &[u8], st: &mut UErrorCode) {
        // Like ZSTR_VAL, the string ends at its first NUL.
        let end = locales
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(locales.len());
        let c = std::ffi::CString::new(&locales[..end]).unwrap_or_default();
        // SAFETY: c is NUL-terminated and outlives the call.
        unsafe { ffi::uspoof_setAllowedLocales(self.sc, c.as_ptr(), st) }
    }

    /// `uspoof_setChecks()`.
    pub fn set_checks(&mut self, checks: i32, st: &mut UErrorCode) {
        // SAFETY: self.sc is live.
        unsafe { ffi::uspoof_setChecks(self.sc, checks, st) }
    }

    /// `uspoof_getChecks()`.
    pub fn checks(&self, st: &mut UErrorCode) -> i32 {
        // SAFETY: self.sc is live.
        unsafe { ffi::uspoof_getChecks(self.sc, st) }
    }

    /// `uspoof_setRestrictionLevel()`.
    pub fn set_restriction_level(&mut self, level: i32) {
        // SAFETY: self.sc is live; level was validated by the caller.
        unsafe { ffi::uspoof_setRestrictionLevel(self.sc, level) }
    }

    /// `uspoof_getRestrictionLevel()`.
    pub fn restriction_level(&self) -> i32 {
        // SAFETY: self.sc is live.
        unsafe { ffi::uspoof_getRestrictionLevel(self.sc) }
    }

    /// `uspoof_setAllowedChars()`.
    pub fn set_allowed_chars(&mut self, set: &UnicodeSet, st: &mut UErrorCode) {
        // SAFETY: both handles are live; ICU copies the set.
        unsafe { ffi::uspoof_setAllowedChars(self.sc, set.0, st) }
    }

    /// `uspoof_getSkeletonUTF8(sc, 0, …)` into `dest` (null/0 to
    /// preflight).
    pub fn skeleton_into(&self, text: &[u8], dest: &mut [u8], st: &mut UErrorCode) -> i32 {
        let (p, cap) = dest_ptr(dest);
        // SAFETY: text is live; p/cap describe a writable buffer (or null/0).
        unsafe {
            ffi::uspoof_getSkeletonUTF8(self.sc, 0, text.as_ptr().cast(), ilen(text), p, cap, st)
        }
    }

    /// `uspoof_getBidiSkeletonUTF8()` into `dest`.
    pub fn bidi_skeleton_into(
        &self,
        dir: i32,
        text: &[u8],
        dest: &mut [u8],
        st: &mut UErrorCode,
    ) -> i32 {
        let (p, cap) = dest_ptr(dest);
        // SAFETY: as in skeleton_into.
        unsafe {
            ffi::uspoof_getBidiSkeletonUTF8(
                self.sc,
                dir,
                text.as_ptr().cast(),
                ilen(text),
                p,
                cap,
                st,
            )
        }
    }
}

fn dest_ptr(dest: &mut [u8]) -> (*mut std::os::raw::c_char, i32) {
    if dest.is_empty() {
        (std::ptr::null_mut(), 0)
    } else {
        (dest.as_mut_ptr().cast(), ffi::len32(dest))
    }
}

/// `u_strFromUTF8(NULL, 0, NULL, s, len, st)`: the UTF-16 length
/// preflight (reports `U_BUFFER_OVERFLOW_ERROR` for non-empty input and
/// `U_INVALID_CHAR_FOUND` for ill-formed UTF-8).
pub fn utf16_preflight(s: &[u8], st: &mut UErrorCode) -> i32 {
    let mut len = 0;
    // SAFETY: s is live; a null destination with capacity 0 is allowed.
    unsafe {
        ffi::u_strFromUTF8(
            std::ptr::null_mut(),
            0,
            &mut len,
            s.as_ptr().cast(),
            ilen(s),
            st,
        );
    }
    len
}

/// `USPOOF_HIGHLY_RESTRICTIVE`.
pub const RESTRICTION_HIGHLY_RESTRICTIVE: i32 = 0x3000_0000;

/// An owned `USet*`.
#[derive(Debug)]
pub struct UnicodeSet(*mut USet);

impl Drop for UnicodeSet {
    fn drop(&mut self) {
        // SAFETY: owned pointer.
        unsafe { ffi::uset_close(self.0) }
    }
}

impl UnicodeSet {
    /// `uset_openEmpty()`.
    pub fn empty() -> Result<Self, UErrorCode> {
        // SAFETY: no arguments.
        let p = unsafe { ffi::uset_openEmpty() };
        if p.is_null() {
            Err(UErrorCode::MEMORY_ALLOCATION_ERROR)
        } else {
            Ok(Self(p))
        }
    }

    /// `uset_applyPattern()`.
    pub fn apply_pattern(&mut self, pattern: &[u16], options: u32, st: &mut UErrorCode) {
        // SAFETY: pattern is a live slice.
        unsafe {
            ffi::uset_applyPattern(self.0, pattern.as_ptr(), ffi::len32(pattern), options, st);
        }
    }

    /// `uset_compact()`.
    pub fn compact(&mut self) {
        // SAFETY: owned live set.
        unsafe { ffi::uset_compact(self.0) }
    }
}
