// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ures_*` — resource bundles (also used to read `zoneinfo64`).

use crate::error::UErrorCode;
use crate::ffi::{self, cstring, UResourceBundle};

/// `UResType` values.
pub mod restype {
    /// `URES_NONE`
    pub const NONE: i32 = -1;
    /// `URES_STRING`
    pub const STRING: i32 = 0;
    /// `URES_BINARY`
    pub const BINARY: i32 = 1;
    /// `URES_TABLE`
    pub const TABLE: i32 = 2;
    /// `URES_ALIAS`
    pub const ALIAS: i32 = 3;
    /// `URES_INT`
    pub const INT: i32 = 7;
    /// `URES_ARRAY`
    pub const ARRAY: i32 = 8;
    /// `URES_INT_VECTOR`
    pub const INT_VECTOR: i32 = 14;
}

/// Owned `UResourceBundle*`.
#[derive(Debug)]
pub struct Bundle(*mut UResourceBundle);

// SAFETY: a bundle handle may move between threads; it is not shared.
unsafe impl Send for Bundle {}

fn wrap(p: *mut UResourceBundle, st: UErrorCode) -> Result<(Bundle, UErrorCode), UErrorCode> {
    if st.is_failure() {
        if !p.is_null() {
            // SAFETY: p came from ICU and is closed once.
            unsafe { ffi::ures_close(p) };
        }
        return Err(st);
    }
    if p.is_null() {
        return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
    }
    Ok((Bundle(p), st))
}

impl Bundle {
    /// `ures_open(package, locale)`; returns the (possibly warning) status.
    pub fn open(package: Option<&str>, locale: &str) -> Result<(Self, UErrorCode), UErrorCode> {
        let p = package.map(cstring);
        let l = cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: strings are NUL-terminated; ownership of the result is taken.
        let r = unsafe {
            ffi::ures_open(
                p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                l.as_ptr(),
                &mut st,
            )
        };
        wrap(r, st)
    }

    /// `ures_openDirect(package, locale)` (no fallback).
    pub fn open_direct(
        package: Option<&str>,
        locale: &str,
    ) -> Result<(Self, UErrorCode), UErrorCode> {
        let p = package.map(cstring);
        let l = cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in open.
        let r = unsafe {
            ffi::ures_openDirect(
                p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                l.as_ptr(),
                &mut st,
            )
        };
        wrap(r, st)
    }

    /// `ures_getByKey()`.
    pub fn by_key(&self, key: &str) -> Result<(Self, UErrorCode), UErrorCode> {
        let k = cstring(key);
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; a fresh bundle is allocated (fillIn null).
        let r = unsafe { ffi::ures_getByKey(self.0, k.as_ptr(), std::ptr::null_mut(), &mut st) };
        wrap(r, st)
    }

    /// `ures_getByIndex()`.
    pub fn by_index(&self, idx: i32) -> Result<(Self, UErrorCode), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; a fresh bundle is allocated.
        let r = unsafe { ffi::ures_getByIndex(self.0, idx, std::ptr::null_mut(), &mut st) };
        wrap(r, st)
    }

    /// `ures_getType()`.
    pub fn res_type(&self) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::ures_getType(self.0) }
    }

    /// `ures_getSize()`.
    pub fn size(&self) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::ures_getSize(self.0) }
    }

    /// `ures_getKey()`.
    pub fn key(&self) -> Option<String> {
        // SAFETY: the key is a static string owned by the bundle data.
        unsafe { ffi::cstr_to_string(ffi::ures_getKey(self.0)) }
    }

    /// `ures_getString()`.
    pub fn string(&self) -> Result<Vec<u16>, UErrorCode> {
        let mut len = 0;
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; ICU returns `len` UChars valid while the bundle lives.
        let p = unsafe { ffi::ures_getString(self.0, &mut len, &mut st) };
        st.check()?;
        Ok(slice_copy(p, len))
    }

    /// `ures_getStringByIndex()`.
    pub fn string_by_index(&self, idx: i32) -> Result<Vec<u16>, UErrorCode> {
        let mut len = 0;
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in string().
        let p = unsafe { ffi::ures_getStringByIndex(self.0, idx, &mut len, &mut st) };
        st.check()?;
        Ok(slice_copy(p, len))
    }

    /// `ures_getInt()`.
    pub fn int(&self) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ures_getInt(self.0, &mut st) };
        st.check().map(|()| v)
    }

    /// `ures_getIntVector()`.
    pub fn int_vector(&self) -> Result<Vec<i32>, UErrorCode> {
        let mut len = 0;
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in string().
        let p = unsafe { ffi::ures_getIntVector(self.0, &mut len, &mut st) };
        st.check()?;
        Ok(slice_copy(p, len))
    }

    /// `ures_getBinary()`.
    pub fn binary(&self) -> Result<Vec<u8>, UErrorCode> {
        let mut len = 0;
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in string().
        let p = unsafe { ffi::ures_getBinary(self.0, &mut len, &mut st) };
        st.check()?;
        Ok(slice_copy(p, len))
    }

    /// `ures_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is owned by the bundle and copied.
        let p = unsafe { ffi::ures_getLocaleByType(self.0, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated (or null).
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }
}

fn slice_copy<T: Copy>(p: *const T, len: i32) -> Vec<T> {
    if p.is_null() {
        return Vec::new();
    }
    let n = usize::try_from(len).unwrap_or(0);
    // SAFETY: callers pass a pointer/length pair returned by ICU that is
    // valid while the owning bundle is alive (it is, during this call).
    unsafe { std::slice::from_raw_parts(p, n) }.to_vec()
}

impl Drop for Bundle {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::ures_close(self.0) }
    }
}

/// `ures_openAvailableLocales(package)` drained.
pub fn available_locales(package: Option<&str>) -> Result<Vec<String>, UErrorCode> {
    let p = package.map(cstring);
    let mut st = UErrorCode::ZERO;
    // SAFETY: p is NUL-terminated or null.
    let e = unsafe {
        ffi::ures_openAvailableLocales(p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()), &mut st)
    };
    // SAFETY: e is a freshly opened enumeration owned from here on.
    let e = unsafe { ffi::Enumeration::from_raw(e, st) }?;
    e.collect_strings()
}
