//! `utrans_*` — transliterators.

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UParseError, UTransliterator};

/// Owned `UTransliterator*`.
#[derive(Debug)]
pub struct Transliterator(*mut UTransliterator);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for Transliterator {}

fn wrap(p: *mut UTransliterator, st: UErrorCode) -> Result<Transliterator, UErrorCode> {
    if st.is_failure() || p.is_null() {
        if !p.is_null() {
            // SAFETY: closed once.
            unsafe { ffi::utrans_close(p) };
        }
        return Err(if st.is_failure() {
            st
        } else {
            UErrorCode::MEMORY_ALLOCATION_ERROR
        });
    }
    Ok(Transliterator(p))
}

impl Transliterator {
    /// `utrans_openU(id, dir, rules)`.
    pub fn open(
        id: &[u16],
        dir: i32,
        rules: Option<&[u16]>,
    ) -> Result<Self, (UErrorCode, UParseError)> {
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        let (rp, rl) = rules.map_or((std::ptr::null(), -1), |r| (r.as_ptr(), len32(r)));
        // SAFETY: id and rules describe live slices (or null/-1).
        let p = unsafe { ffi::utrans_openU(id.as_ptr(), len32(id), dir, rp, rl, &mut pe, &mut st) };
        wrap(p, st).map_err(|e| (e, pe))
    }

    /// `utrans_openInverse()`.
    pub fn inverse(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::utrans_openInverse(self.0, &mut st) };
        wrap(p, st)
    }

    /// `utrans_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::utrans_clone(self.0, &mut st) };
        wrap(p, st)
    }

    /// `utrans_getUnicodeID()`.
    pub fn id(&self) -> Vec<u16> {
        let mut len = 0;
        // SAFETY: self.0 is live; the ID is owned by the transliterator and copied.
        let p = unsafe { ffi::utrans_getUnicodeID(self.0, &mut len) };
        if p.is_null() {
            return Vec::new();
        }
        // SAFETY: ICU guarantees `len` UChars at p.
        unsafe { std::slice::from_raw_parts(p, usize::try_from(len).unwrap_or(0)) }.to_vec()
    }

    /// `utrans_toRules()`.
    pub fn to_rules(&self, escape_unprintable: bool) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        ffi::fill_u16(|b, c, st| unsafe {
            ffi::utrans_toRules(self.0, i8::from(escape_unprintable), b, c, st)
        })
    }

    /// `utrans_transUChars()` on a copy of `text` between `start` and
    /// `limit`, growing the buffer as php-src does.
    pub fn trans(&self, text: &[u16], start: i32, limit: i32) -> Result<Vec<u16>, UErrorCode> {
        let mut capacity = text.len() + 1;
        loop {
            let mut buf = vec![0u16; capacity];
            buf[..text.len()].copy_from_slice(text);
            let mut len = len32(text);
            let mut lim = limit;
            let mut st = UErrorCode::ZERO;
            // SAFETY: buf has `capacity` UChars, `len` of them initialised.
            unsafe {
                ffi::utrans_transUChars(
                    self.0,
                    buf.as_mut_ptr(),
                    &mut len,
                    len32(&buf),
                    start,
                    &mut lim,
                    &mut st,
                );
            }
            if st == UErrorCode::BUFFER_OVERFLOW_ERROR {
                capacity = usize::try_from(len).unwrap_or(0) + 1;
                continue;
            }
            if st != UErrorCode::STRING_NOT_TERMINATED_WARNING {
                st.check()?;
            }
            buf.truncate(usize::try_from(len).unwrap_or(0));
            return Ok(buf);
        }
    }
}

impl Drop for Transliterator {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::utrans_close(self.0) }
    }
}

/// `utrans_openIDs()` drained as UTF-16.
pub fn ids() -> Result<Vec<Vec<u16>>, UErrorCode> {
    let mut st = UErrorCode::ZERO;
    // SAFETY: no preconditions.
    let p = unsafe { ffi::utrans_openIDs(&mut st) };
    // SAFETY: p is a freshly opened enumeration owned from here on.
    let mut e = unsafe { ffi::Enumeration::from_raw(p, st) }?;
    let mut out = Vec::new();
    while let Some(s) = e.next_u16()? {
        out.push(s);
    }
    Ok(out)
}
