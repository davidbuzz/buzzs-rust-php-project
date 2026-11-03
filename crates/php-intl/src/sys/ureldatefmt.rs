//! `ureldatefmt_*` and the `ucasemap_toTitle()` used for sentence-start
//! capitalization.

use crate::error::UErrorCode;
use crate::ffi::{self, len32, URelativeDateTimeFormatter};
use crate::sys::unum::NumberFormat;

/// `UBRK_SENTENCE`.
const UBRK_SENTENCE: i32 = 3;
/// `U_TITLECASE_NO_LOWERCASE | U_TITLECASE_NO_BREAK_ADJUSTMENT`.
const TITLE_OPTIONS: u32 = 0x100 | 0x200;

/// Owned `URelativeDateTimeFormatter*`.
#[derive(Debug)]
pub struct RelDateFormatter(*mut URelativeDateTimeFormatter);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for RelDateFormatter {}

impl Drop for RelDateFormatter {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::ureldatefmt_close(self.0) }
    }
}

impl RelDateFormatter {
    /// `ureldatefmt_open(locale, nf, width, capitalizationContext)`; the
    /// number format (if any) is adopted by ICU.
    pub fn open(
        locale: &str,
        nf: Option<NumberFormat>,
        width: i32,
        capitalization: i32,
    ) -> Result<Self, UErrorCode> {
        let l = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        let nf = nf.map_or(std::ptr::null_mut(), NumberFormat::into_raw);
        // SAFETY: l is NUL-terminated; nf is null or an owned handle that
        // ICU adopts (the callers only pass valid styles and contexts, for
        // which the constructor always takes ownership).
        let p = unsafe { ffi::ureldatefmt_open(l.as_ptr(), nf, width, capitalization, &mut st) };
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

    /// `ureldatefmt_format()`.
    pub fn format(&self, offset: f64, unit: i32) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: self.0 is live; buffer contract handled by fill_u16.
        ffi::fill_u16(|b, c, st| unsafe { ffi::ureldatefmt_format(self.0, offset, unit, b, c, st) })
    }

    /// `ureldatefmt_formatNumeric()`.
    pub fn format_numeric(&self, offset: f64, unit: i32) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: as above.
        ffi::fill_u16(|b, c, st| unsafe {
            ffi::ureldatefmt_formatNumeric(self.0, offset, unit, b, c, st)
        })
    }

    /// `ureldatefmt_combineDateAndTime()`.
    pub fn combine(&self, date: &[u16], time: &[u16]) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: date/time are live slices; buffer contract via fill_u16.
        ffi::fill_u16(|b, c, st| unsafe {
            ffi::ureldatefmt_combineDateAndTime(
                self.0,
                date.as_ptr(),
                len32(date),
                time.as_ptr(),
                len32(time),
                b,
                c,
                st,
            )
        })
    }
}

/// Checks that a sentence break iterator can be created for `locale`
/// (`BreakIterator::createSentenceInstance`).
pub fn check_sentence_iterator(locale: &str) -> Result<(), UErrorCode> {
    crate::sys::ubrk::BreakIterator::open(UBRK_SENTENCE, locale).map(|_| ())
}

/// `UnicodeString::toTitle(sentenceIterator, locale,
/// U_TITLECASE_NO_LOWERCASE | U_TITLECASE_NO_BREAK_ADJUSTMENT)`.
pub fn title_sentences(text: &[u16], locale: &str) -> Result<Vec<u16>, UErrorCode> {
    struct CaseMap(*mut ffi::UCaseMap);
    impl Drop for CaseMap {
        fn drop(&mut self) {
            // SAFETY: owned handle (it also owns the adopted iterator).
            unsafe { ffi::ucasemap_close(self.0) }
        }
    }
    let l = ffi::cstring(locale);
    let mut st = UErrorCode::ZERO;
    // SAFETY: l is NUL-terminated.
    let csm = CaseMap(unsafe { ffi::ucasemap_open(l.as_ptr(), TITLE_OPTIONS, &mut st) });
    st.check()?;
    // SAFETY: l is NUL-terminated; null text is allowed.
    let bi = unsafe { ffi::ubrk_open(UBRK_SENTENCE, l.as_ptr(), std::ptr::null(), 0, &mut st) };
    if st.is_failure() {
        if !bi.is_null() {
            // SAFETY: bi was just opened and is not adopted.
            unsafe { ffi::ubrk_close(bi) };
        }
        return Err(st);
    }
    // SAFETY: csm is live; ICU adopts bi.
    unsafe { ffi::ucasemap_setBreakIterator(csm.0, bi, &mut st) };
    st.check()?;
    // SAFETY: text is a live slice; buffer contract via fill_u16.
    ffi::fill_u16(|b, c, s| unsafe {
        ffi::ucasemap_toTitle(csm.0, b, c, text.as_ptr(), len32(text), s)
    })
}

/// `u_islower()`.
pub fn is_lower(c: u32) -> bool {
    // SAFETY: pure property lookup.
    #[allow(clippy::cast_possible_wrap)]
    let r = unsafe { ffi::u_islower(c as i32) };
    r != 0
}
