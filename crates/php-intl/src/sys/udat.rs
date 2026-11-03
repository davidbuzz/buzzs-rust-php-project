//! `udat_*` — the classic date formatter.

use crate::error::UErrorCode;
use crate::ffi::{self, fill_u16, len32, UDateFormat};
use crate::sys::ucal::Calendar;

/// Owned `UDateFormat*`.
#[derive(Debug)]
pub struct DateFormat(*mut UDateFormat);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for DateFormat {}

impl DateFormat {
    /// `udat_open(timeStyle, dateStyle, locale, tz, pattern)`.
    pub fn open(
        time_style: i32,
        date_style: i32,
        locale: Option<&str>,
        tz: Option<&[u16]>,
        pattern: Option<&[u16]>,
    ) -> Result<Self, UErrorCode> {
        let l = locale.map(ffi::cstring);
        let (tp, tl) = tz.map_or((std::ptr::null(), -1), |t| (t.as_ptr(), len32(t)));
        let (pp, pl) = pattern.map_or((std::ptr::null(), -1), |p| (p.as_ptr(), len32(p)));
        let mut st = UErrorCode::ZERO;
        // SAFETY: pointer/length pairs describe live slices or are null/-1;
        // the locale is NUL-terminated or null.
        let p = unsafe {
            ffi::udat_open(
                time_style,
                date_style,
                l.as_ref().map_or(std::ptr::null(), |l| l.as_ptr()),
                tp,
                tl,
                pp,
                pl,
                &mut st,
            )
        };
        if st.is_failure() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::udat_close(p) };
            }
            return Err(st);
        }
        if p.is_null() {
            return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
        }
        Ok(Self(p))
    }

    /// `udat_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::udat_clone(self.0, &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::udat_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `udat_format()`.
    pub fn format(&self, date: f64) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, c, st| unsafe {
            ffi::udat_format(self.0, date, b, c, std::ptr::null_mut(), st)
        })
    }

    /// `udat_formatCalendar()` (formats `cal`'s time in `cal`'s zone and
    /// calendar system).
    pub fn format_calendar(&self, cal: &mut Calendar) -> Result<Vec<u16>, UErrorCode> {
        let p = cal.as_ptr().cast_mut();
        // SAFETY: buffer contract handled by fill_u16; cal is uniquely borrowed.
        fill_u16(|b, c, st| unsafe {
            ffi::udat_formatCalendar(self.0, p, b, c, std::ptr::null_mut(), st)
        })
    }

    /// `udat_parse()`; `pos` receives the end index (or error index).
    pub fn parse(&self, text: &[u16], pos: &mut i32) -> Result<f64, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: text is a live slice; pos a valid in/out pointer.
        let v = unsafe { ffi::udat_parse(self.0, text.as_ptr(), len32(text), pos, &mut st) };
        st.check().map(|()| v)
    }

    /// `udat_parseCalendar()`.
    pub fn parse_calendar(
        &self,
        cal: &mut Calendar,
        text: &[u16],
        pos: &mut i32,
    ) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let p = cal.as_ptr().cast_mut();
        // SAFETY: as in parse(); cal uniquely borrowed.
        unsafe { ffi::udat_parseCalendar(self.0, p, text.as_ptr(), len32(text), pos, &mut st) };
        st.check()
    }

    /// `udat_toPattern()`.
    pub fn to_pattern(&self, localized: bool) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, c, st| unsafe { ffi::udat_toPattern(self.0, i8::from(localized), b, c, st) })
    }

    /// `udat_applyPattern()`.
    pub fn apply_pattern(&mut self, localized: bool, pattern: &[u16]) {
        // SAFETY: pattern is a live slice; self.0 uniquely borrowed.
        unsafe {
            ffi::udat_applyPattern(
                self.0,
                i8::from(localized),
                pattern.as_ptr(),
                len32(pattern),
            );
        }
    }

    /// A copy of the formatter's calendar (`ucal_clone(udat_getCalendar())`).
    pub fn calendar(&self) -> Result<Calendar, UErrorCode> {
        // SAFETY: the calendar is owned by the formatter; it is cloned
        // immediately and the borrowed wrapper is forgotten.
        let p = unsafe { ffi::udat_getCalendar(self.0) };
        if p.is_null() {
            return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
        }
        let mut st = UErrorCode::ZERO;
        // SAFETY: p is live while self is.
        let c = unsafe { ffi::ucal_clone(p, &mut st) };
        if st.is_failure() || c.is_null() {
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        // SAFETY: c is a fresh clone owned by the wrapper.
        Ok(unsafe { Calendar::from_raw(c) })
    }

    /// `udat_setCalendar()` (copies `cal`).
    pub fn set_calendar(&mut self, cal: &Calendar) {
        // SAFETY: both handles are live; ICU copies the calendar.
        unsafe { ffi::udat_setCalendar(self.0, cal.as_ptr()) }
    }

    /// `udat_isLenient()`.
    pub fn is_lenient(&self) -> bool {
        // SAFETY: self.0 is live.
        unsafe { ffi::udat_isLenient(self.0) != 0 }
    }

    /// `udat_setLenient()`.
    pub fn set_lenient(&mut self, v: bool) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::udat_setLenient(self.0, i8::from(v)) }
    }

    /// `udat_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is copied.
        let p = unsafe { ffi::udat_getLocaleByType(self.0, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `udat_setContext()`.
    pub fn set_context(&mut self, value: i32) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::udat_setContext(self.0, value, &mut st) };
        st.check()
    }
}

/// A borrowed view of the formatter's own calendar.
#[derive(Debug)]
pub struct OwnCalendar<'a> {
    cal: std::mem::ManuallyDrop<Calendar>,
    _fmt: std::marker::PhantomData<&'a DateFormat>,
}

impl std::ops::Deref for OwnCalendar<'_> {
    type Target = Calendar;
    fn deref(&self) -> &Calendar {
        &self.cal
    }
}

impl DateFormat {
    /// The formatter's own calendar, borrowed (never closed by the view).
    pub fn own_calendar(&self) -> OwnCalendar<'_> {
        // SAFETY: the calendar is owned by the live formatter and outlives
        // the returned view, which never closes it (ManuallyDrop).
        let p = unsafe { ffi::udat_getCalendar(self.0) }.cast_mut();
        OwnCalendar {
            // SAFETY: see above; p is non-null for a constructed formatter.
            cal: std::mem::ManuallyDrop::new(unsafe { Calendar::from_raw(p) }),
            _fmt: std::marker::PhantomData,
        }
    }

    /// `udat_parseCalendar()` into the formatter's own calendar, as
    /// php-src's `parseToCalendar()`/`localtime()` do.
    pub fn parse_own_calendar(
        &mut self,
        text: &[u16],
        pos: Option<&mut i32>,
    ) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: the formatter owns its calendar; mutating it in place is
        // what php-src does (casting away const).  `self` is uniquely borrowed.
        let cal = unsafe { ffi::udat_getCalendar(self.0) }.cast_mut();
        let pp = pos.map_or(std::ptr::null_mut(), std::ptr::from_mut::<i32>);
        // SAFETY: text is a live slice; pp is null or a valid in/out pointer.
        unsafe { ffi::udat_parseCalendar(self.0, cal, text.as_ptr(), len32(text), pp, &mut st) };
        st.check()
    }

    /// `udat_parse()` with an optional position (`NULL` when absent).
    pub fn parse_opt(&self, text: &[u16], pos: Option<&mut i32>) -> Result<f64, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let pp = pos.map_or(std::ptr::null_mut(), std::ptr::from_mut::<i32>);
        // SAFETY: text is a live slice; pp is null or a valid in/out pointer.
        let v = unsafe { ffi::udat_parse(self.0, text.as_ptr(), len32(text), pp, &mut st) };
        st.check().map(|()| v)
    }
}

impl Drop for DateFormat {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::udat_close(self.0) }
    }
}
