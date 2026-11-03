//! `unum_*` — the classic number formatter.

use crate::error::UErrorCode;
use crate::ffi::{self, fill_u16, len32, UNumberFormat, UParseError};

/// A number parsed into a `Formattable`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Parsed {
    /// `kDouble`
    Double(f64),
    /// `kLong` (int32)
    Long(i32),
    /// `kInt64`
    Int64(i64),
}

/// Owned `UNumberFormat*`.
#[derive(Debug)]
pub struct NumberFormat(*mut UNumberFormat);

// SAFETY: ICU formatters are not thread-safe for concurrent use but may be
// moved between threads; the wrapper is not `Sync`.
unsafe impl Send for NumberFormat {}

impl NumberFormat {
    /// `unum_open(style, pattern, locale)`.
    pub fn open(
        style: i32,
        pattern: Option<&[u16]>,
        locale: &str,
    ) -> Result<Self, (UErrorCode, UParseError)> {
        let l = ffi::cstring(locale);
        let mut pe = UParseError::default();
        let mut st = UErrorCode::ZERO;
        let (pp, plen) = match pattern {
            Some(p) => (p.as_ptr(), len32(p)),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: pattern pointer/length describe a live slice (or null/0);
        // l is NUL-terminated; ownership of the result is taken below.
        let p = unsafe { ffi::unum_open(style, pp, plen, l.as_ptr(), &mut pe, &mut st) };
        if st.is_failure() {
            if !p.is_null() {
                // SAFETY: p came from unum_open and is closed once.
                unsafe { ffi::unum_close(p) };
            }
            return Err((st, pe));
        }
        if p.is_null() {
            return Err((UErrorCode::MEMORY_ALLOCATION_ERROR, pe));
        }
        Ok(Self(p))
    }

    /// The raw handle (borrowed).
    pub fn as_ptr(&self) -> *const UNumberFormat {
        self.0
    }

    /// `unum_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::unum_clone(self.0, &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: p came from unum_clone.
                unsafe { ffi::unum_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `unum_format()` (int32).
    pub fn format_i32(&self, n: i32) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16; self.0 is live.
        fill_u16(|b, cap, st| unsafe {
            ffi::unum_format(self.0, n, b, cap, std::ptr::null_mut(), st)
        })
    }

    /// `unum_formatInt64()`.
    pub fn format_i64(&self, n: i64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: as above.
        fill_u16(|b, cap, st| unsafe {
            ffi::unum_formatInt64(self.0, n, b, cap, std::ptr::null_mut(), st)
        })
    }

    /// `unum_formatDouble()`.
    pub fn format_f64(&self, n: f64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: as above.
        fill_u16(|b, cap, st| unsafe {
            ffi::unum_formatDouble(self.0, n, b, cap, std::ptr::null_mut(), st)
        })
    }

    /// `unum_formatDecimal()` with a decimal string.
    pub fn format_decimal(&self, n: &str) -> Result<Vec<u16>, UErrorCode> {
        let c = ffi::cstring(n);
        // SAFETY: c is NUL-terminated and its length is passed explicitly.
        fill_u16(|b, cap, st| unsafe {
            ffi::unum_formatDecimal(
                self.0,
                c.as_ptr(),
                len32(n.as_bytes()),
                b,
                cap,
                std::ptr::null_mut(),
                st,
            )
        })
    }

    /// `unum_formatDoubleCurrency()`; `currency` must be NUL-terminated.
    pub fn format_currency(&self, n: f64, currency: &[u16]) -> Result<Vec<u16>, UErrorCode> {
        let mut cur = ffi::u16z(currency);
        // SAFETY: cur is a NUL-terminated buffer ICU only reads.
        fill_u16(|b, cap, st| unsafe {
            ffi::unum_formatDoubleCurrency(
                self.0,
                n,
                cur.as_mut_ptr(),
                b,
                cap,
                std::ptr::null_mut(),
                st,
            )
        })
    }

    /// `unum_parse()` (int32). `pos` is updated only on success.
    pub fn parse_i32(&self, text: &[u16], pos: &mut i32) -> Result<i32, UErrorCode> {
        let mut p = *pos;
        let mut st = UErrorCode::ZERO;
        // SAFETY: text is a live slice; p is a valid in/out position.
        let r = unsafe { ffi::unum_parse(self.0, text.as_ptr(), len32(text), &mut p, &mut st) };
        st.check()?;
        *pos = p;
        Ok(r)
    }

    /// `unum_parseInt64()`.
    pub fn parse_i64(&self, text: &[u16], pos: &mut i32) -> Result<i64, UErrorCode> {
        let mut p = *pos;
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in parse_i32.
        let r =
            unsafe { ffi::unum_parseInt64(self.0, text.as_ptr(), len32(text), &mut p, &mut st) };
        st.check()?;
        *pos = p;
        Ok(r)
    }

    /// `unum_parseDouble()`.
    pub fn parse_f64(&self, text: &[u16], pos: &mut i32) -> Result<f64, UErrorCode> {
        let mut p = *pos;
        let mut st = UErrorCode::ZERO;
        // SAFETY: as in parse_i32.
        let r =
            unsafe { ffi::unum_parseDouble(self.0, text.as_ptr(), len32(text), &mut p, &mut st) };
        st.check()?;
        *pos = p;
        Ok(r)
    }

    /// `unum_parseDoubleCurrency()` → `(value, ISO code)`.
    pub fn parse_currency(
        &self,
        text: &[u16],
        pos: &mut i32,
    ) -> Result<(f64, Vec<u16>), UErrorCode> {
        let mut p = *pos;
        let mut st = UErrorCode::ZERO;
        let mut cur = [0u16; 4];
        // SAFETY: cur has the 4 UChars the API requires.
        let r = unsafe {
            ffi::unum_parseDoubleCurrency(
                self.0,
                text.as_ptr(),
                len32(text),
                &mut p,
                cur.as_mut_ptr(),
                &mut st,
            )
        };
        st.check()?;
        *pos = p;
        let n = cur.iter().position(|&c| c == 0).unwrap_or(3).min(3);
        Ok((r, cur[..n].to_vec()))
    }

    /// `unum_parseToUFormattable()` → the parsed value with its
    /// `Formattable` type.
    pub fn parse_formattable(&self, text: &[u16], pos: &mut i32) -> Result<Parsed, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: ufmt_open has no preconditions; closed below.
        let f = unsafe { ffi::ufmt_open(&mut st) };
        st.check()?;
        let mut p = *pos;
        // SAFETY: f is live; text is a live slice; p a valid in/out pointer.
        unsafe {
            ffi::unum_parseToUFormattable(self.0, f, text.as_ptr(), len32(text), &mut p, &mut st)
        };
        let r = if st.is_failure() {
            Err(st)
        } else {
            *pos = p;
            // SAFETY: f holds the parse result.
            let ty = unsafe { ffi::ufmt_getType(f, &mut st) };
            // SAFETY: getters on a live formattable of the matching type.
            match ty {
                2 => Ok(Parsed::Long(unsafe { ffi::ufmt_getLong(f, &mut st) })),
                5 => Ok(Parsed::Int64(unsafe { ffi::ufmt_getInt64(f, &mut st) })),
                _ => Ok(Parsed::Double(unsafe { ffi::ufmt_getDouble(f, &mut st) })),
            }
        };
        // SAFETY: f is owned here and closed once.
        unsafe { ffi::ufmt_close(f) };
        r
    }

    /// `unum_getAttribute()`.
    pub fn attribute(&self, attr: i32) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::unum_getAttribute(self.0, attr) }
    }

    /// `unum_setAttribute()`.
    pub fn set_attribute(&mut self, attr: i32, v: i32) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::unum_setAttribute(self.0, attr, v) }
    }

    /// `unum_getDoubleAttribute()`.
    pub fn double_attribute(&self, attr: i32) -> f64 {
        // SAFETY: self.0 is live.
        unsafe { ffi::unum_getDoubleAttribute(self.0, attr) }
    }

    /// `unum_setDoubleAttribute()`.
    pub fn set_double_attribute(&mut self, attr: i32, v: f64) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::unum_setDoubleAttribute(self.0, attr, v) }
    }

    /// `unum_getTextAttribute()`.
    pub fn text_attribute(&self, attr: i32) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, cap, st| unsafe { ffi::unum_getTextAttribute(self.0, attr, b, cap, st) })
    }

    /// `unum_setTextAttribute()`.
    pub fn set_text_attribute(&mut self, attr: i32, v: &[u16]) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: v is a live slice.
        unsafe { ffi::unum_setTextAttribute(self.0, attr, v.as_ptr(), len32(v), &mut st) };
        st.check()
    }

    /// `unum_getSymbol()`.
    pub fn symbol(&self, sym: i32) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, cap, st| unsafe { ffi::unum_getSymbol(self.0, sym, b, cap, st) })
    }

    /// `unum_setSymbol()`.
    pub fn set_symbol(&mut self, sym: i32, v: &[u16]) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: v is a live slice.
        unsafe { ffi::unum_setSymbol(self.0, sym, v.as_ptr(), len32(v), &mut st) };
        st.check()
    }

    /// `unum_toPattern()`.
    pub fn to_pattern(&self) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, cap, st| unsafe { ffi::unum_toPattern(self.0, 0, b, cap, st) })
    }

    /// `unum_applyPattern()`.
    pub fn apply_pattern(&mut self, p: &[u16]) -> Result<(), (UErrorCode, UParseError)> {
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        // SAFETY: p is a live slice.
        unsafe { ffi::unum_applyPattern(self.0, 0, p.as_ptr(), len32(p), &mut pe, &mut st) };
        if st.is_failure() {
            Err((st, pe))
        } else {
            Ok(())
        }
    }

    /// `unum_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is owned by the formatter and copied.
        let p = unsafe { ffi::unum_getLocaleByType(self.0, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated and owned by the live formatter.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }
}

impl Drop for NumberFormat {
    fn drop(&mut self) {
        // SAFETY: self.0 is owned and closed exactly once.
        unsafe { ffi::unum_close(self.0) }
    }
}
