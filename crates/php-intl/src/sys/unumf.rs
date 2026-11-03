//! `unumf_*` — skeleton-based number formatting (ICU's `NumberFormatter`).

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UFormattedNumber, UNumberFormatter, UParseError};

/// Owned `UNumberFormatter*`.
#[derive(Debug)]
pub struct SkeletonFormatter(*mut UNumberFormatter);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for SkeletonFormatter {}

/// Owned `UFormattedNumber*`.
struct Result_(*mut UFormattedNumber);

impl Drop for Result_ {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumf_closeResult(self.0) }
    }
}

impl SkeletonFormatter {
    /// `unumf_openForSkeletonAndLocaleWithError()`.
    pub fn open(skeleton: &[u16], locale: &str) -> Result<Self, (UErrorCode, UParseError)> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        // SAFETY: skeleton is a live slice; l NUL-terminated.
        let p = unsafe {
            ffi::unumf_openForSkeletonAndLocaleWithError(
                skeleton.as_ptr(),
                len32(skeleton),
                l.as_ptr(),
                &mut pe,
                &mut st,
            )
        };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::unumf_close(p) };
            }
            return Err((
                if st.is_failure() {
                    st
                } else {
                    UErrorCode::MEMORY_ALLOCATION_ERROR
                },
                pe,
            ));
        }
        Ok(Self(p))
    }

    fn run(f: impl FnOnce(*mut UFormattedNumber, &mut UErrorCode)) -> Result<Vec<u16>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: no preconditions; ownership is taken by Result_.
        let r = Result_(unsafe { ffi::unumf_openResult(&mut st) });
        st.check()?;
        f(r.0, &mut st);
        st.check()?;
        // SAFETY: r.0 is a live result; buffer contract handled by fill_u16.
        ffi::fill_u16(|b, c, s| unsafe { ffi::unumf_resultToString(r.0, b, c, s) })
    }

    /// `unumf_formatDouble()`.
    pub fn format_f64(&self, v: f64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 and the result are live.
        Self::run(|r, st| unsafe { ffi::unumf_formatDouble(self.0, v, r, st) })
    }

    /// `unumf_formatInt()`.
    pub fn format_i64(&self, v: i64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 and the result are live.
        Self::run(|r, st| unsafe { ffi::unumf_formatInt(self.0, v, r, st) })
    }

    /// `unumf_formatDecimal()`.
    pub fn format_decimal(&self, v: &[u8]) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 and the result are live; v is passed with its
        // explicit length.
        Self::run(|r, st| unsafe {
            ffi::unumf_formatDecimal(self.0, v.as_ptr().cast(), len32(v), r, st);
        })
    }
}

impl Drop for SkeletonFormatter {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumf_close(self.0) }
    }
}

/// Copy the text of a `UFormattedValue*` (`ufmtval_getString()`).
///
/// # Safety
/// `v` must be a live formatted value (or null with `st` failing).
unsafe fn formatted_value_string(
    v: *const ffi::UFormattedValue,
    st: &mut UErrorCode,
) -> Result<Vec<u16>, UErrorCode> {
    st.check()?;
    let mut len = 0;
    // SAFETY: v is live per the contract.
    let p = unsafe { ffi::ufmtval_getString(v, &mut len, st) };
    st.check()?;
    if p.is_null() {
        return Ok(Vec::new());
    }
    let n = usize::try_from(len).unwrap_or(0);
    // SAFETY: ICU returns a buffer of `len` UChars that lives as long as
    // the formatted value.
    Ok(unsafe { std::slice::from_raw_parts(p, n) }.to_vec())
}

/// A range endpoint for [`RangeFormatter::format`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeValue {
    /// `Formattable::setInt64`
    Int(i64),
    /// `Formattable::setDouble`
    Float(f64),
}

/// Owned `UNumberRangeFormatter*`.
#[derive(Debug)]
pub struct RangeFormatter(*mut ffi::UNumberRangeFormatter);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for RangeFormatter {}

impl Drop for RangeFormatter {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumrf_close(self.0) }
    }
}

struct RangeResult(*mut ffi::UFormattedNumberRange);

impl Drop for RangeResult {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::unumrf_closeResult(self.0) }
    }
}

impl RangeFormatter {
    /// `unumrf_openForSkeletonWithCollapseAndIdentityFallback()`.
    pub fn open(
        skeleton: &[u16],
        collapse: i32,
        identity_fallback: i32,
        locale: &str,
    ) -> Result<Self, UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        let mut pe = UParseError::default();
        // SAFETY: skeleton is a live slice; l is NUL-terminated.
        let p = unsafe {
            ffi::unumrf_openForSkeletonWithCollapseAndIdentityFallback(
                skeleton.as_ptr(),
                len32(skeleton),
                collapse,
                identity_fallback,
                l.as_ptr(),
                &mut pe,
                &mut st,
            )
        };
        if p.is_null() {
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        let out = Self(p);
        st.check()?;
        Ok(out)
    }

    /// `formatFormattableRange()`: two doubles go through
    /// `unumrf_formatDoubleRange`; otherwise both endpoints are passed as
    /// exact decimal strings (keeping full `int64` precision; a double's
    /// shortest round-trip representation is what `Formattable` uses).
    /// An int and a float are never "equal before rounding", as with
    /// `Formattable::operator==`.
    #[allow(clippy::many_single_char_names)]
    pub fn format(&self, a: RangeValue, b: RangeValue) -> Result<Vec<u16>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: no preconditions; ownership is taken by RangeResult.
        let r = RangeResult(unsafe { ffi::unumrf_openResult(&mut st) });
        st.check()?;
        let as_f64 = |v: RangeValue| match v {
            #[allow(clippy::cast_precision_loss)]
            RangeValue::Int(i) => i as f64,
            RangeValue::Float(f) => f,
        };
        let non_finite = |v: RangeValue| matches!(v, RangeValue::Float(f) if !f.is_finite());
        match (a, b) {
            (RangeValue::Float(_), RangeValue::Float(_)) => {}
            _ if non_finite(a) || non_finite(b) => {}
            _ => {
                let dec = |v: RangeValue| match v {
                    RangeValue::Int(i) => i.to_string(),
                    RangeValue::Float(f) => format!("{f}"),
                };
                let (x, y) = (dec(a), dec(b));
                // `Formattable::operator==` (same type and value) decides
                // the identity fallback; the C API compares the string
                // pointers, so equal endpoints share one buffer.
                let same = matches!((a, b), (RangeValue::Int(p), RangeValue::Int(q)) if p == q);
                let y: &str = if same { &x } else { &y };
                // SAFETY: x/y are live and passed with explicit lengths.
                unsafe {
                    ffi::unumrf_formatDecimalRange(
                        self.0,
                        x.as_ptr().cast(),
                        len32(x.as_bytes()),
                        y.as_ptr().cast(),
                        len32(y.as_bytes()),
                        r.0,
                        &mut st,
                    );
                    let v = ffi::unumrf_resultAsValue(r.0, &mut st);
                    return formatted_value_string(v, &mut st);
                }
            }
        }
        // SAFETY: self.0 and r.0 are live.
        unsafe {
            ffi::unumrf_formatDoubleRange(self.0, as_f64(a), as_f64(b), r.0, &mut st);
            let v = ffi::unumrf_resultAsValue(r.0, &mut st);
            formatted_value_string(v, &mut st)
        }
    }
}
