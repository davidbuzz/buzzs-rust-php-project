// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `uloc_*` — locale identifiers.

use std::ffi::c_char;

use crate::error::UErrorCode;
use crate::ffi::{self, cstring, fill_c, fill_u16};

/// `uloc_getDefault()`.
pub fn get_default() -> String {
    // SAFETY: returns a static NUL-terminated string.
    unsafe { ffi::cstr_to_string(ffi::uloc_getDefault()) }.unwrap_or_default()
}

/// `uloc_setDefault()`.
pub fn set_default(locale: Option<&str>) -> Result<(), UErrorCode> {
    let c = locale.map(cstring);
    let mut st = UErrorCode::ZERO;
    // SAFETY: a null pointer resets to the host default; otherwise c is NUL-terminated.
    unsafe { ffi::uloc_setDefault(c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), &mut st) };
    st.check()
}

type CharGetter = unsafe extern "C" fn(*const c_char, *mut c_char, i32, *mut UErrorCode) -> i32;

fn get_c(f: CharGetter, locale: &str) -> Result<String, UErrorCode> {
    let c = cstring(locale);
    let v = fill_c(|b, cap, st| {
        // SAFETY: b has `cap` bytes, c is NUL-terminated.
        unsafe { f(c.as_ptr(), b, cap, st) }
    })?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// Runs a `uloc_get*`-style function with a fixed buffer of `cap` bytes
/// (php-src often uses fixed-size buffers; overflow is then an error).
fn get_c_fixed(f: CharGetter, locale: &str, cap: usize) -> (String, UErrorCode) {
    let c = cstring(locale);
    let mut buf = vec![0u8; cap];
    let mut st = UErrorCode::ZERO;
    // SAFETY: buf has `cap` bytes, c is NUL-terminated.
    let n = unsafe {
        f(
            c.as_ptr(),
            buf.as_mut_ptr().cast(),
            ffi::len32(&buf),
            &mut st,
        )
    };
    let n = usize::try_from(n).unwrap_or(0).min(cap);
    (String::from_utf8_lossy(&buf[..n]).into_owned(), st)
}

macro_rules! getter {
    ($($(#[$m:meta])* $name:ident => $f:ident;)*) => {
        $(
            $(#[$m])*
            pub fn $name(locale: &str) -> Result<String, UErrorCode> {
                get_c(ffi::$f, locale)
            }
        )*
    };
}

getter! {
    /// `uloc_getLanguage()`.
    language => uloc_getLanguage;
    /// `uloc_getScript()`.
    script => uloc_getScript;
    /// `uloc_getCountry()`.
    country => uloc_getCountry;
    /// `uloc_getVariant()`.
    variant => uloc_getVariant;
    /// `uloc_getName()`.
    name => uloc_getName;
    /// `uloc_canonicalize()`.
    canonicalize => uloc_canonicalize;
    /// `uloc_getBaseName()`.
    base_name => uloc_getBaseName;
    /// `uloc_getParent()`.
    parent => uloc_getParent;
    /// `uloc_addLikelySubtags()`.
    add_likely_subtags => uloc_addLikelySubtags;
    /// `uloc_minimizeSubtags()`.
    minimize_subtags => uloc_minimizeSubtags;
}

/// The getters by tag, using a fixed-size buffer like php-src does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// `uloc_getLanguage`
    Language,
    /// `uloc_getScript`
    Script,
    /// `uloc_getCountry`
    Country,
    /// `uloc_getVariant`
    Variant,
    /// `uloc_canonicalize`
    Canonicalize,
}

/// Calls the getter for `part` with a buffer of `cap` bytes, returning the
/// raw length-truncated value and the status (php-src's
/// `get_icu_value_internal` loop semantics are implemented by the caller).
pub fn get_part_raw(part: Part, locale: &str, cap: usize) -> (String, i32, UErrorCode) {
    let f: CharGetter = match part {
        Part::Language => ffi::uloc_getLanguage,
        Part::Script => ffi::uloc_getScript,
        Part::Country => ffi::uloc_getCountry,
        Part::Variant => ffi::uloc_getVariant,
        Part::Canonicalize => ffi::uloc_canonicalize,
    };
    let c = cstring(locale);
    let mut buf = vec![0u8; cap];
    let mut st = UErrorCode::ZERO;
    // SAFETY: buf has `cap` bytes, c is NUL-terminated.
    let n = unsafe {
        f(
            c.as_ptr(),
            buf.as_mut_ptr().cast(),
            ffi::len32(&buf),
            &mut st,
        )
    };
    let m = usize::try_from(n).unwrap_or(0).min(cap);
    (String::from_utf8_lossy(&buf[..m]).into_owned(), n, st)
}

/// `uloc_canonicalize` into a fixed buffer (php's `canonicalize_locale_string`).
pub fn canonicalize_fixed(locale: &str, cap: usize) -> (String, UErrorCode) {
    get_c_fixed(ffi::uloc_canonicalize, locale, cap)
}

/// `uloc_getISO3Language()`.
pub fn iso3_language(locale: &str) -> String {
    let c = cstring(locale);
    // SAFETY: c is NUL-terminated; the result is static.
    unsafe { ffi::cstr_to_string(ffi::uloc_getISO3Language(c.as_ptr())) }.unwrap_or_default()
}

/// `uloc_getISO3Country()`.
pub fn iso3_country(locale: &str) -> String {
    let c = cstring(locale);
    // SAFETY: c is NUL-terminated; the result is static.
    unsafe { ffi::cstr_to_string(ffi::uloc_getISO3Country(c.as_ptr())) }.unwrap_or_default()
}

/// `uloc_getKeywordValue()`; `Ok("")` when absent.
pub fn keyword_value(locale: &str, keyword: &str) -> Result<String, UErrorCode> {
    let l = cstring(locale);
    let k = cstring(keyword);
    let v = fill_c(|b, cap, st| {
        // SAFETY: b has `cap` bytes; l and k are NUL-terminated.
        unsafe { ffi::uloc_getKeywordValue(l.as_ptr(), k.as_ptr(), b, cap, st) }
    })?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// `uloc_setKeywordValue()` on `locale` (returns the new locale ID).
pub fn set_keyword_value(
    locale: &str,
    keyword: &str,
    value: Option<&str>,
) -> Result<String, UErrorCode> {
    let k = cstring(keyword);
    let v = value.map(cstring);
    let mut buf = vec![0u8; locale.len() + keyword.len() + value.map_or(0, str::len) + 64];
    buf[..locale.len()].copy_from_slice(locale.as_bytes());
    let mut st = UErrorCode::ZERO;
    // SAFETY: buf holds a NUL-terminated locale and has room for the
    // keyword and value; the capacity passed is the buffer length.
    let n = unsafe {
        ffi::uloc_setKeywordValue(
            k.as_ptr(),
            v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            buf.as_mut_ptr().cast(),
            ffi::len32(&buf),
            &mut st,
        )
    };
    st.check()?;
    let n = usize::try_from(n).unwrap_or(0).min(buf.len());
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// `uloc_openKeywords()` drained into `(keyword, value)` pairs.  `Ok(None)`
/// when ICU returns no enumeration (no keywords, or a malformed list).
pub fn keywords(locale: &str) -> Result<Option<Vec<(String, String)>>, UErrorCode> {
    let l = cstring(locale);
    let mut st = UErrorCode::ZERO;
    // SAFETY: l is NUL-terminated; ownership of the enumeration is taken.
    let p = unsafe { ffi::uloc_openKeywords(l.as_ptr(), &mut st) };
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: p comes from uloc_openKeywords and is owned from here on.
    let Ok(e) = (unsafe { ffi::Enumeration::from_raw(p, st) }) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for k in e.collect_strings()? {
        let v = keyword_value(locale, &k)?;
        out.push((k, v));
    }
    Ok(Some(out))
}

type DisplayFn =
    unsafe extern "C" fn(*const c_char, *const c_char, *mut u16, i32, *mut UErrorCode) -> i32;

/// Which `uloc_getDisplay*` function to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// `uloc_getDisplayName`
    Name,
    /// `uloc_getDisplayLanguage`
    Language,
    /// `uloc_getDisplayScript`
    Script,
    /// `uloc_getDisplayCountry`
    Region,
    /// `uloc_getDisplayVariant`
    Variant,
}

/// `uloc_getDisplay*(locale, in_locale)` returning UTF-16 and the status
/// (warnings such as `U_USING_DEFAULT_WARNING` are reported).
pub fn display(kind: Display, locale: &str, in_locale: &str) -> (Vec<u16>, UErrorCode) {
    let f: DisplayFn = match kind {
        Display::Name => ffi::uloc_getDisplayName,
        Display::Language => ffi::uloc_getDisplayLanguage,
        Display::Script => ffi::uloc_getDisplayScript,
        Display::Region => ffi::uloc_getDisplayCountry,
        Display::Variant => ffi::uloc_getDisplayVariant,
    };
    let l = cstring(locale);
    let d = cstring(in_locale);
    ffi::fill_u16_status(|b, cap, st| {
        // SAFETY: b has `cap` UChars; l and d are NUL-terminated.
        unsafe { f(l.as_ptr(), d.as_ptr(), b, cap, st) }
    })
}

/// `uloc_getDisplayKeyword(keyword, displayLocale)`.
pub fn display_keyword(keyword: &str, in_locale: &str) -> Result<Vec<u16>, UErrorCode> {
    let k = cstring(keyword);
    let d = cstring(in_locale);
    fill_u16(|b, cap, st| {
        // SAFETY: b has `cap` UChars; k and d are NUL-terminated.
        unsafe { ffi::uloc_getDisplayKeyword(k.as_ptr(), d.as_ptr(), b, cap, st) }
    })
}

/// `uloc_getDisplayKeywordValue(locale, keyword, displayLocale)`.
pub fn display_keyword_value(
    locale: &str,
    keyword: &str,
    in_locale: &str,
) -> Result<Vec<u16>, UErrorCode> {
    let l = cstring(locale);
    let k = cstring(keyword);
    let d = cstring(in_locale);
    fill_u16(|b, cap, st| {
        // SAFETY: b has `cap` UChars; all strings are NUL-terminated.
        unsafe { ffi::uloc_getDisplayKeywordValue(l.as_ptr(), k.as_ptr(), d.as_ptr(), b, cap, st) }
    })
}

/// `uloc_toLanguageTag(locale, strict)`.
pub fn to_language_tag(locale: &str, strict: bool) -> Result<String, UErrorCode> {
    let l = cstring(locale);
    let v = fill_c(|b, cap, st| {
        // SAFETY: b has `cap` bytes; l is NUL-terminated.
        unsafe { ffi::uloc_toLanguageTag(l.as_ptr(), b, cap, i8::from(strict), st) }
    })?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// `uloc_forLanguageTag(tag)` → `(locale id, parsed length)`.
pub fn for_language_tag(tag: &str) -> Result<(String, i32), UErrorCode> {
    let t = cstring(tag);
    let mut parsed = 0i32;
    let v = fill_c(|b, cap, st| {
        // SAFETY: b has `cap` bytes; t is NUL-terminated.
        unsafe { ffi::uloc_forLanguageTag(t.as_ptr(), b, cap, &mut parsed, st) }
    })?;
    Ok((String::from_utf8_lossy(&v).into_owned(), parsed))
}

/// `uloc_countAvailable()` / `uloc_getAvailable()`.
pub fn available() -> Vec<String> {
    // SAFETY: no preconditions.
    let n = unsafe { ffi::uloc_countAvailable() };
    (0..n)
        // SAFETY: i is within [0, count).
        .filter_map(|i| unsafe { ffi::cstr_to_string(ffi::uloc_getAvailable(i)) })
        .collect()
}

/// `uloc_isRightToLeft()`.
pub fn is_right_to_left(locale: &str) -> bool {
    let l = cstring(locale);
    // SAFETY: l is NUL-terminated.
    unsafe { ffi::uloc_isRightToLeft(l.as_ptr()) != 0 }
}

/// `uloc_acceptLanguageFromHTTP()` against `available` (or all available
/// locales from `ures_openAvailableLocales`).  Returns `(result,
/// UAcceptResult, status)`; a failure to open the list is reported with
/// `UAcceptResult == -1`.
pub fn accept_language_from_http(
    header: &str,
    available: Option<&[&str]>,
    cap: usize,
) -> (String, i32, UErrorCode) {
    let h = cstring(header);
    let mut st = UErrorCode::ZERO;
    let owned: Vec<std::ffi::CString> = available
        .unwrap_or(&[])
        .iter()
        .map(|s| cstring(s))
        .collect();
    let ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    let raw = match available {
        // SAFETY: ptrs points to NUL-terminated strings that outlive the
        // enumeration (it copies nothing but is closed before they drop).
        Some(_) => unsafe {
            ffi::uenum_openCharStringsEnumeration(ptrs.as_ptr(), ffi::len32(&ptrs), &mut st)
        },
        // SAFETY: no preconditions.
        None => unsafe { ffi::ures_openAvailableLocales(std::ptr::null(), &mut st) },
    };
    // SAFETY: raw is a freshly opened enumeration owned from here on.
    let avail = match unsafe { ffi::Enumeration::from_raw(raw, st) } {
        Ok(e) => e,
        Err(e) => return (String::new(), -1, e),
    };
    let mut buf = vec![0u8; cap + 1];
    let mut outres = 0i32;
    let mut st = UErrorCode::ZERO;
    // SAFETY: buf has more than `cap` bytes, h is NUL-terminated, avail is live.
    let n = unsafe {
        ffi::uloc_acceptLanguageFromHTTP(
            buf.as_mut_ptr().cast(),
            i32::try_from(cap).unwrap_or(i32::MAX),
            &mut outres,
            h.as_ptr(),
            avail.as_ptr(),
            &mut st,
        )
    };
    drop(avail);
    drop(owned);
    let m = usize::try_from(n).unwrap_or(0).min(cap);
    (
        String::from_utf8_lossy(&buf[..m]).into_owned(),
        if n < 0 { -2 } else { outres },
        st,
    )
}
