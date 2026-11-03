//! `ucal_*` — calendars and the time-zone functions of the C API.

use crate::error::UErrorCode;
use crate::ffi::{self, fill_u16, len32, UCalendar};

/// `UCalendarType`.
pub mod caltype {
    /// `UCAL_TRADITIONAL` / `UCAL_DEFAULT`
    pub const TRADITIONAL: i32 = 0;
    /// `UCAL_GREGORIAN`
    pub const GREGORIAN: i32 = 1;
}

/// Owned `UCalendar*`.
#[derive(Debug)]
pub struct Calendar(*mut UCalendar);

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for Calendar {}

impl Calendar {
    /// `ucal_open(zone, locale, type)`; `zone = None` uses ICU's default
    /// zone and `locale = None` ICU's default locale.
    pub fn open(zone: Option<&[u16]>, locale: Option<&str>, ty: i32) -> Result<Self, UErrorCode> {
        let l = locale.map(ffi::cstring);
        let mut st = UErrorCode::ZERO;
        let (zp, zl) = zone.map_or((std::ptr::null(), 0), |z| (z.as_ptr(), len32(z)));
        // SAFETY: zone pointer/length describe a live slice (or null);
        // the locale is NUL-terminated or null.
        let p = unsafe {
            ffi::ucal_open(
                zp,
                zl,
                l.as_ref().map_or(std::ptr::null(), |l| l.as_ptr()),
                ty,
                &mut st,
            )
        };
        Self::wrap(p, st)
    }

    fn wrap(p: *mut UCalendar, st: UErrorCode) -> Result<Self, UErrorCode> {
        if st.is_failure() {
            if !p.is_null() {
                // SAFETY: p came from ICU and is closed once.
                unsafe { ffi::ucal_close(p) };
            }
            return Err(st);
        }
        if p.is_null() {
            return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
        }
        Ok(Self(p))
    }

    /// Wraps a calendar pointer owned by the caller.
    ///
    /// # Safety
    /// `p` must be a live calendar nobody else closes.
    pub unsafe fn from_raw(p: *mut UCalendar) -> Self {
        Self(p)
    }

    /// The raw handle (borrowed).
    pub fn as_ptr(&self) -> *const UCalendar {
        self.0
    }

    /// `ucal_clone()`.
    pub fn try_clone(&self) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let p = unsafe { ffi::ucal_clone(self.0, &mut st) };
        Self::wrap(p, st)
    }

    /// `ucal_getMillis()`.
    pub fn millis(&self) -> Result<f64, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_getMillis(self.0, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_setMillis()`.
    pub fn set_millis(&mut self, t: f64) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_setMillis(self.0, t, &mut st) };
        st.check()
    }

    /// `ucal_get()`.
    pub fn get(&self, field: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live (ucal_get may complete fields internally).
        let v = unsafe { ffi::ucal_get(self.0, field, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_set()`.
    pub fn set(&mut self, field: i32, value: i32) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_set(self.0, field, value) }
    }

    /// `ucal_isSet()`.
    pub fn is_set(&self, field: i32) -> bool {
        // SAFETY: self.0 is live.
        unsafe { ffi::ucal_isSet(self.0, field) != 0 }
    }

    /// `ucal_clear()`.
    pub fn clear(&mut self) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_clear(self.0) }
    }

    /// `ucal_clearField()`.
    pub fn clear_field(&mut self, field: i32) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_clearField(self.0, field) }
    }

    /// `ucal_add()`.
    pub fn add(&mut self, field: i32, amount: i32) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_add(self.0, field, amount, &mut st) };
        st.check()
    }

    /// `ucal_roll()`.
    pub fn roll(&mut self, field: i32, amount: i32) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_roll(self.0, field, amount, &mut st) };
        st.check()
    }

    /// `ucal_getLimit()`.
    pub fn limit(&self, field: i32, ty: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_getLimit(self.0, field, ty, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_getAttribute()`.
    pub fn attribute(&self, attr: i32) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::ucal_getAttribute(self.0, attr) }
    }

    /// `ucal_setAttribute()`.
    pub fn set_attribute(&mut self, attr: i32, v: i32) {
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_setAttribute(self.0, attr, v) }
    }

    /// `ucal_getTimeZoneID()`.
    pub fn time_zone_id(&self) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16.
        fill_u16(|b, c, st| unsafe { ffi::ucal_getTimeZoneID(self.0, b, c, st) })
    }

    /// `ucal_setTimeZone()`.
    pub fn set_time_zone(&mut self, id: &[u16]) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: id is a live slice; self.0 uniquely borrowed.
        unsafe { ffi::ucal_setTimeZone(self.0, id.as_ptr(), len32(id), &mut st) };
        st.check()
    }

    /// `ucal_getTimeZoneDisplayName()`.
    pub fn time_zone_display_name(&self, ty: i32, locale: &str) -> Result<Vec<u16>, UErrorCode> {
        let l = ffi::cstring(locale);
        // SAFETY: buffer contract handled by fill_u16; l NUL-terminated.
        fill_u16(|b, c, st| unsafe {
            ffi::ucal_getTimeZoneDisplayName(self.0, ty, l.as_ptr(), b, c, st)
        })
    }

    /// `ucal_inDaylightTime()`.
    pub fn in_daylight_time(&self) -> Result<bool, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_inDaylightTime(self.0, &mut st) };
        st.check().map(|()| v != 0)
    }

    /// `ucal_getGregorianChange()`.
    pub fn gregorian_change(&self) -> Result<f64, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_getGregorianChange(self.0, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_setGregorianChange()`.
    pub fn set_gregorian_change(&mut self, d: f64) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        unsafe { ffi::ucal_setGregorianChange(self.0, d, &mut st) };
        st.check()
    }

    /// `ucal_equivalentTo()`.
    pub fn equivalent_to(&self, other: &Calendar) -> bool {
        // SAFETY: both handles are live.
        unsafe { ffi::ucal_equivalentTo(self.0, other.0) != 0 }
    }

    /// `ucal_getLocaleByType()`.
    pub fn locale_by_type(&self, ty: i32) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is owned by the calendar and copied.
        let p = unsafe { ffi::ucal_getLocaleByType(self.0, ty, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `ucal_getType()`.
    pub fn cal_type(&self) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the result is static.
        let p = unsafe { ffi::ucal_getType(self.0, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `ucal_getDayOfWeekType()`.
    pub fn day_of_week_type(&self, dow: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_getDayOfWeekType(self.0, dow, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_getWeekendTransition()`.
    pub fn weekend_transition(&self, dow: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_getWeekendTransition(self.0, dow, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_isWeekend()`.
    pub fn is_weekend(&self, date: f64) -> Result<bool, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        let v = unsafe { ffi::ucal_isWeekend(self.0, date, &mut st) };
        st.check().map(|()| v != 0)
    }

    /// `ucal_getFieldDifference()` (advances the calendar).
    pub fn field_difference(&mut self, target: f64, field: i32) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live and uniquely borrowed.
        let v = unsafe { ffi::ucal_getFieldDifference(self.0, target, field, &mut st) };
        st.check().map(|()| v)
    }

    /// `ucal_getTimeZoneTransitionDate()`; `None` when there is none.
    pub fn transition(&self, ty: i32) -> Result<Option<f64>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let mut d = 0.0;
        // SAFETY: self.0 is live; d is a valid out pointer.
        let ok = unsafe { ffi::ucal_getTimeZoneTransitionDate(self.0, ty, &mut d, &mut st) };
        st.check()?;
        Ok((ok != 0).then_some(d))
    }

    /// `ucal_getTimeZoneOffsetFromLocal()` → `(raw, dst)`.
    pub fn offset_from_local(
        &self,
        non_existing: i32,
        duplicated: i32,
    ) -> Result<(i32, i32), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let (mut raw, mut dst) = (0, 0);
        // SAFETY: self.0 is live; out pointers are valid.
        unsafe {
            ffi::ucal_getTimeZoneOffsetFromLocal(
                self.0,
                non_existing,
                duplicated,
                &mut raw,
                &mut dst,
                &mut st,
            );
        };
        st.check().map(|()| (raw, dst))
    }
}

impl Drop for Calendar {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::ucal_close(self.0) }
    }
}

/// `ucal_getNow()`.
pub fn now() -> f64 {
    // SAFETY: no preconditions.
    unsafe { ffi::ucal_getNow() }
}

/// `ucal_getDefaultTimeZone()`.
pub fn default_time_zone() -> Result<Vec<u16>, UErrorCode> {
    // SAFETY: buffer contract handled by fill_u16.
    fill_u16(|b, c, st| unsafe { ffi::ucal_getDefaultTimeZone(b, c, st) })
}

/// `ucal_getDSTSavings()`.
pub fn dst_savings(id: &[u16]) -> Result<i32, UErrorCode> {
    let z = ffi::u16z(id);
    let mut st = UErrorCode::ZERO;
    // SAFETY: z is NUL-terminated.
    let v = unsafe { ffi::ucal_getDSTSavings(z.as_ptr(), &mut st) };
    st.check().map(|()| v)
}

/// `ucal_getCanonicalTimeZoneID()` → `(id, is_system_id)`.
pub fn canonical_id(id: &[u16]) -> Result<(Vec<u16>, bool), UErrorCode> {
    let mut sys = 0i8;
    // SAFETY: buffer contract handled by fill_u16; id is a live slice.
    let v = fill_u16(|b, c, st| unsafe {
        ffi::ucal_getCanonicalTimeZoneID(id.as_ptr(), len32(id), b, c, &mut sys, st)
    })?;
    Ok((v, sys != 0))
}

/// `ucal_getIanaTimeZoneID()`.
pub fn iana_id(id: &[u16]) -> Result<Vec<u16>, UErrorCode> {
    // SAFETY: buffer contract handled by fill_u16; id is a live slice.
    fill_u16(|b, c, st| unsafe { ffi::ucal_getIanaTimeZoneID(id.as_ptr(), len32(id), b, c, st) })
}

/// `ucal_getWindowsTimeZoneID()`.
pub fn windows_id(id: &[u16]) -> Result<Vec<u16>, UErrorCode> {
    // SAFETY: as above.
    fill_u16(|b, c, st| unsafe { ffi::ucal_getWindowsTimeZoneID(id.as_ptr(), len32(id), b, c, st) })
}

/// `ucal_getTimeZoneIDForWindowsID()`.
pub fn id_for_windows_id(winid: &[u16], region: Option<&str>) -> Result<Vec<u16>, UErrorCode> {
    let r = region.map(ffi::cstring);
    // SAFETY: as above; region NUL-terminated or null.
    fill_u16(|b, c, st| unsafe {
        ffi::ucal_getTimeZoneIDForWindowsID(
            winid.as_ptr(),
            len32(winid),
            r.as_ref().map_or(std::ptr::null(), |r| r.as_ptr()),
            b,
            c,
            st,
        )
    })
}

/// `ucal_getTZDataVersion()`.
pub fn tz_data_version() -> Result<String, UErrorCode> {
    let mut st = UErrorCode::ZERO;
    // SAFETY: returns a static string.
    let p = unsafe { ffi::ucal_getTZDataVersion(&mut st) };
    st.check()?;
    // SAFETY: p is NUL-terminated or null.
    Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
}

fn drain(p: *mut ffi::UEnumeration, st: UErrorCode) -> Result<Vec<String>, UErrorCode> {
    // SAFETY: callers pass a freshly opened enumeration.
    let e = unsafe { ffi::Enumeration::from_raw(p, st) }?;
    e.collect_strings()
}

/// `ucal_openTimeZones()`.
pub fn all_time_zones() -> Result<Vec<String>, UErrorCode> {
    let mut st = UErrorCode::ZERO;
    // SAFETY: no preconditions.
    let p = unsafe { ffi::ucal_openTimeZones(&mut st) };
    drain(p, st)
}

/// `ucal_openCountryTimeZones()`.
pub fn country_time_zones(country: &str) -> Result<Vec<String>, UErrorCode> {
    let c = ffi::cstring(country);
    let mut st = UErrorCode::ZERO;
    // SAFETY: c is NUL-terminated.
    let p = unsafe { ffi::ucal_openCountryTimeZones(c.as_ptr(), &mut st) };
    drain(p, st)
}

/// `ucal_openTimeZoneIDEnumeration()`.
pub fn time_zone_id_enumeration(
    zone_type: i32,
    region: Option<&str>,
    raw_offset: Option<i32>,
) -> Result<Vec<String>, UErrorCode> {
    let r = region.map(ffi::cstring);
    let off = raw_offset.unwrap_or(0);
    let mut st = UErrorCode::ZERO;
    // SAFETY: region NUL-terminated or null; off lives across the call.
    let p = unsafe {
        ffi::ucal_openTimeZoneIDEnumeration(
            zone_type,
            r.as_ref().map_or(std::ptr::null(), |r| r.as_ptr()),
            if raw_offset.is_some() {
                &off
            } else {
                std::ptr::null()
            },
            &mut st,
        )
    };
    drain(p, st)
}

/// `ucal_getKeywordValuesForLocale()`.
pub fn keyword_values_for_locale(
    key: &str,
    locale: &str,
    commonly_used: bool,
) -> Result<Vec<String>, UErrorCode> {
    let k = ffi::cstring(key);
    let l = ffi::cstring(locale);
    let mut st = UErrorCode::ZERO;
    // SAFETY: strings are NUL-terminated.
    let p = unsafe {
        ffi::ucal_getKeywordValuesForLocale(
            k.as_ptr(),
            l.as_ptr(),
            i8::from(commonly_used),
            &mut st,
        )
    };
    drain(p, st)
}

/// `ucal_countAvailable()` / `ucal_getAvailable()`.
pub fn available_locales() -> Vec<String> {
    // SAFETY: no preconditions.
    let n = unsafe { ffi::ucal_countAvailable() };
    // SAFETY: i is within [0, count); results are static strings.
    (0..n)
        .filter_map(|i| unsafe { ffi::cstr_to_string(ffi::ucal_getAvailable(i)) })
        .collect()
}
