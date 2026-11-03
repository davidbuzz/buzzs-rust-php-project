//! `IntlCalendar` / `IntlGregorianCalendar` / `intlcal_*` / `intlgregcal_*`
//! (php-src `ext/intl/calendar`).

use crate::common;
use crate::error::{Error, Result, Scope, StatusExt, UErrorCode};
use crate::sys::ucal::{self, caltype, Calendar};
use crate::timezone::{self, DateTimeZoneSpec, PhpZone, TimeZone};
use crate::ustr;

/// `UCAL_FIELD_COUNT` with ICU 74 (includes `UCAL_ORDINAL_MONTH`).
pub const FIELD_COUNT: i64 = 24;

/// `UCalendarDateFields`.
pub mod field {
    /// `FIELD_ERA`
    pub const ERA: i32 = 0;
    /// `FIELD_YEAR`
    pub const YEAR: i32 = 1;
    /// `FIELD_MONTH`
    pub const MONTH: i32 = 2;
    /// `FIELD_DATE`
    pub const DATE: i32 = 5;
    /// `FIELD_HOUR_OF_DAY`
    pub const HOUR_OF_DAY: i32 = 11;
    /// `FIELD_MINUTE`
    pub const MINUTE: i32 = 12;
    /// `FIELD_SECOND`
    pub const SECOND: i32 = 13;
}

/// `UCalendarAttribute`.
mod attr {
    pub const LENIENT: i32 = 0;
    pub const FIRST_DAY_OF_WEEK: i32 = 1;
    pub const MINIMAL_DAYS_IN_FIRST_WEEK: i32 = 2;
    pub const REPEATED_WALL_TIME: i32 = 3;
    pub const SKIPPED_WALL_TIME: i32 = 4;
}

/// `UCalendarWallTimeOption`.
pub mod walltime {
    /// `WALLTIME_LAST`
    pub const LAST: i64 = 0;
    /// `WALLTIME_FIRST`
    pub const FIRST: i64 = 1;
    /// `WALLTIME_NEXT_VALID`
    pub const NEXT_VALID: i64 = 2;
}

/// `UCalendarLimitType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Limit {
    /// `getMinimum`
    Minimum = 0,
    /// `getMaximum`
    Maximum = 1,
    /// `getGreatestMinimum`
    GreatestMinimum = 2,
    /// `getLeastMaximum`
    LeastMaximum = 3,
    /// `getActualMinimum`
    ActualMinimum = 4,
    /// `getActualMaximum`
    ActualMaximum = 5,
}

/// A decomposed PHP `DateTimeInterface`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpDateTime {
    /// `getTimestamp()`.
    pub timestamp: i64,
    /// Microseconds of the second.
    pub microseconds: i64,
    /// The zone when `is_localtime`, `None` for UTC/GMT.
    pub zone: Option<PhpZone>,
}

impl PhpDateTime {
    /// The instant in ICU milliseconds (`intl_datetime_decompose`).
    #[allow(clippy::cast_precision_loss)]
    pub fn millis(&self) -> f64 {
        1000.0 * self.timestamp as f64 + (self.microseconds / 1000) as f64
    }

    /// The zone as an ICU zone (GMT when not local time).
    pub fn time_zone(&self) -> Result<TimeZone> {
        match &self.zone {
            None => Ok(TimeZone::gmt()),
            Some(z) => timezone::from_php_zone(z).map_err(|_| {
                Error::intl(
                    UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                    "could not convert DateTime's time zone",
                )
            }),
        }
    }
}

/// `"must be between -2147483648 and 2147483647"`.
pub(crate) fn int32_arg(v: i64, arg: u32) -> Result<i32> {
    i32::try_from(v).map_err(|_| {
        Error::arg_value(
            arg,
            format!("must be between {} and {}", i32::MIN, i32::MAX),
        )
    })
}

fn field_arg(f: i64, arg: u32) -> Result<i32> {
    if !(0..FIELD_COUNT).contains(&f) {
        return Err(Error::arg_value(arg, "must be a valid field"));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(f as i32)
}

fn dow_arg(d: i64, arg: u32) -> Result<i32> {
    if !(1..=7).contains(&d) {
        return Err(Error::arg_value(arg, "must be a valid day of the week"));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(d as i32)
}

/// A PHP `IntlCalendar` (or `IntlGregorianCalendar`) object.
#[derive(Debug)]
pub struct IntlCalendar {
    cal: Calendar,
}

/// The `var_dump()` view of a calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarDebugInfo {
    /// `type`
    pub cal_type: String,
    /// `timeZone` (`id` and, when available, `(rawOffset, currentOffset)`).
    pub time_zone: (String, Option<(i64, i64)>),
    /// `locale` (or the error name).
    pub locale: String,
    /// `fields`: name → value or error name.
    pub fields: Vec<(&'static str, std::result::Result<i64, String>)>,
}

const DEBUG_FIELDS: [(i32, &str); 23] = [
    (0, "era"),
    (1, "year"),
    (2, "month"),
    (3, "week of year"),
    (4, "week of month"),
    (6, "day of year"),
    (5, "day of month"),
    (7, "day of week"),
    (8, "day of week in month"),
    (9, "AM/PM"),
    (10, "hour"),
    (11, "hour of day"),
    (12, "minute"),
    (13, "second"),
    (14, "millisecond"),
    (15, "zone offset"),
    (16, "DST offset"),
    (17, "year for week of year"),
    (18, "localized day of week"),
    (19, "extended year"),
    (20, "julian day"),
    (21, "milliseconds in day"),
    (22, "is leap month"),
];

impl IntlCalendar {
    /// Wraps an ICU calendar.
    #[allow(dead_code)]
    pub(crate) fn from_calendar(cal: Calendar) -> Self {
        Self { cal }
    }

    /// The wrapped ICU calendar.
    #[allow(dead_code)]
    pub(crate) fn raw(&self) -> &Calendar {
        &self.cal
    }

    /// `IntlCalendar::createInstance($timezone, $locale)`; the time zone has
    /// been processed with [`timezone::process_timezone_argument`].
    pub fn create_instance(tz: &TimeZone, locale: Option<&str>) -> Result<Self> {
        let locale = locale.map_or_else(common::default_locale, str::to_owned);
        let cal = Calendar::open(Some(tz.id_u16()), Some(&locale), caltype::TRADITIONAL)
            .map_err(|c| Error::global(c, "Error creating ICU Calendar object"))?;
        Ok(Self { cal })
    }

    /// `IntlCalendar::fromDateTime($datetime, $locale)` (the VM constructs
    /// the `DateTime` from a string argument first).
    pub fn from_date_time(dt: &PhpDateTime, locale: Option<&str>) -> Result<Self> {
        let tz = match &dt.zone {
            None => TimeZone::gmt(),
            Some(z) => {
                timezone::from_php_zone(z).map_err(|e| timezone::rescope(e, Scope::Global))?
            }
        };
        let locale = locale.map_or_else(common::default_locale, str::to_owned);
        let mut cal = Calendar::open(Some(tz.id_u16()), Some(&locale), caltype::TRADITIONAL)
            .map_err(|c| Error::global(c, "error creating ICU Calendar object"))?;
        #[allow(clippy::cast_precision_loss)]
        cal.set_millis(dt.timestamp as f64 * 1000.0)
            .map_err(|c| Error::global(c, "error creating ICU Calendar::setTime()"))?;
        Ok(Self { cal })
    }

    /// Whether PHP exposes this calendar as `IntlGregorianCalendar`.
    pub fn is_gregorian(&self) -> bool {
        self.cal.cal_type().is_ok_and(|t| t == "gregorian")
    }

    /// Clone (`clone $cal`).
    pub fn try_clone(&self) -> Result<Self> {
        let cal = self
            .cal
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone IntlCalendar".into()))?;
        Ok(Self { cal })
    }

    /// `IntlCalendar::get($field)`.
    pub fn get(&self, f: i64) -> Result<i64> {
        let f = field_arg(f, 1)?;
        self.cal
            .get(f)
            .map(i64::from)
            .or_intl("Call to ICU method has failed")
    }

    /// `IntlCalendar::getTime()`.
    pub fn get_time(&self) -> Result<f64> {
        self.cal
            .millis()
            .or_intl("error calling ICU Calendar::getTime")
    }

    /// `IntlCalendar::setTime($timestamp)`.
    pub fn set_time(&mut self, t: f64) -> Result<()> {
        self.cal
            .set_millis(t)
            .or_intl("Call to underlying method failed")
    }

    /// `IntlCalendar::add($field, $value)`.
    pub fn add(&mut self, f: i64, amount: i64) -> Result<()> {
        let f = field_arg(f, 1)?;
        let amount = int32_arg(amount, 2)?;
        self.cal
            .add(f, amount)
            .or_intl("Call to underlying method failed")
    }

    /// `IntlCalendar::setTimeZone($tz)` with an already-processed zone
    /// (`None` = the `null` argument, which does nothing).  Errors from
    /// processing are object-scoped intl errors.
    pub fn set_time_zone(&mut self, tz: Option<&TimeZone>) -> Result<()> {
        if let Some(tz) = tz {
            self.cal
                .set_time_zone(tz.id_u16())
                .or_intl("Error setting time zone")?;
        }
        Ok(())
    }

    /// `IntlCalendar::after($other)`.
    pub fn after(&self, other: &IntlCalendar) -> Result<bool> {
        let a = self.cal.millis().or_intl("Error calling ICU method")?;
        let b = other.cal.millis().or_intl("Error calling ICU method")?;
        Ok(a > b)
    }

    /// `IntlCalendar::before($other)`.
    pub fn before(&self, other: &IntlCalendar) -> Result<bool> {
        let a = self.cal.millis().or_intl("Error calling ICU method")?;
        let b = other.cal.millis().or_intl("Error calling ICU method")?;
        Ok(a < b)
    }

    /// `IntlCalendar::equals($other)`.
    pub fn equals(&self, other: &IntlCalendar) -> Result<bool> {
        let a = self
            .cal
            .millis()
            .or_intl("error calling ICU Calendar::equals")?;
        let b = other
            .cal
            .millis()
            .or_intl("error calling ICU Calendar::equals")?;
        #[allow(clippy::float_cmp)]
        Ok(a == b)
    }

    /// `IntlCalendar::isEquivalentTo($other)`.
    pub fn is_equivalent_to(&self, other: &IntlCalendar) -> bool {
        self.cal.equivalent_to(&other.cal)
    }

    /// `IntlCalendar::set(...)` with 2, 3, 5 or 6 integer arguments.
    /// Argument errors are numbered in method form.  The VM emits the
    /// "more than 2 arguments is deprecated" notice for the method form.
    pub fn set(&mut self, args: &[i64]) -> Result<()> {
        let mut v = [0i32; 6];
        for (i, a) in args.iter().enumerate().take(6) {
            #[allow(clippy::cast_possible_truncation)]
            {
                v[i] = int32_arg(*a, i as u32 + 1)?;
            }
        }
        match args.len() {
            2 => {
                let f = field_arg(args[0], 1)?;
                self.cal.set(f, v[1]);
            }
            3 => self.set_ymd(v[0], v[1], v[2]),
            4 => {
                return Err(Error::ArgumentCount(
                    "IntlCalendar::set() has no variant with exactly 4 parameters".into(),
                ))
            }
            5 => {
                self.set_ymd(v[0], v[1], v[2]);
                self.cal.set(field::HOUR_OF_DAY, v[3]);
                self.cal.set(field::MINUTE, v[4]);
            }
            _ => {
                self.set_ymd(v[0], v[1], v[2]);
                self.cal.set(field::HOUR_OF_DAY, v[3]);
                self.cal.set(field::MINUTE, v[4]);
                self.cal.set(field::SECOND, v[5]);
            }
        }
        Ok(())
    }

    fn set_ymd(&mut self, y: i32, m: i32, d: i32) {
        self.cal.set(field::YEAR, y);
        self.cal.set(field::MONTH, m);
        self.cal.set(field::DATE, d);
    }

    /// `IntlCalendar::setDate($year, $month, $dayOfMonth)`.
    pub fn set_date(&mut self, y: i64, m: i64, d: i64) -> Result<()> {
        let (y, m, d) = (int32_arg(y, 1)?, int32_arg(m, 2)?, int32_arg(d, 3)?);
        self.set_ymd(y, m, d);
        Ok(())
    }

    /// `IntlCalendar::setDateTime($y, $m, $d, $h, $i, $s = null)`.
    pub fn set_date_time(
        &mut self,
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: Option<i64>,
    ) -> Result<()> {
        let (year, month, day) = (
            int32_arg(year, 1)?,
            int32_arg(month, 2)?,
            int32_arg(day, 3)?,
        );
        let (hour, minute) = (int32_arg(hour, 4)?, int32_arg(minute, 5)?);
        let second = second.map(|v| int32_arg(v, 6)).transpose()?;
        self.set_ymd(year, month, day);
        self.cal.set(field::HOUR_OF_DAY, hour);
        self.cal.set(field::MINUTE, minute);
        if let Some(sec) = second {
            self.cal.set(field::SECOND, sec);
        }
        Ok(())
    }

    /// `IntlCalendar::roll($field, $value)`; booleans are mapped by the VM
    /// to `1`/`-1` (with the deprecation notice).
    pub fn roll(&mut self, f: i64, amount: i64) -> Result<()> {
        let f = field_arg(f, 1)?;
        let amount = int32_arg(amount, 2)?;
        self.cal
            .roll(f, amount)
            .or_intl("Error calling ICU Calendar::roll")
    }

    /// `IntlCalendar::clear($field = null)`.
    pub fn clear(&mut self, f: Option<i64>) -> Result<()> {
        match f {
            None => self.cal.clear(),
            Some(f) => {
                let f = field_arg(f, 1)?;
                self.cal.clear_field(f);
            }
        }
        Ok(())
    }

    /// `IntlCalendar::fieldDifference($timestamp, $field)`.
    pub fn field_difference(&mut self, when: f64, f: i64) -> Result<i64> {
        let f = field_arg(f, 2)?;
        self.cal
            .field_difference(when, f)
            .map(i64::from)
            .or_intl("Call to ICU method has failed")
    }

    /// `getActualMaximum`/`getActualMinimum`/`getMaximum`/`getMinimum`/
    /// `getGreatestMinimum`/`getLeastMaximum`.
    pub fn limit(&self, f: i64, which: Limit) -> Result<i64> {
        let f = field_arg(f, 1)?;
        self.cal
            .limit(f, which as i32)
            .map(i64::from)
            .or_intl("Call to ICU method has failed")
    }

    /// `IntlCalendar::getDayOfWeekType($dayOfWeek)`.
    pub fn day_of_week_type(&self, dow: i64) -> Result<i64> {
        let d = dow_arg(dow, 1)?;
        self.cal
            .day_of_week_type(d)
            .map(i64::from)
            .or_intl("Call to ICU method has failed")
    }

    /// `IntlCalendar::getWeekendTransition($dayOfWeek)`.
    pub fn weekend_transition(&self, dow: i64) -> Result<i64> {
        let d = dow_arg(dow, 1)?;
        self.cal
            .weekend_transition(d)
            .map(i64::from)
            .or_intl("Error calling ICU method")
    }

    /// `IntlCalendar::getFirstDayOfWeek()`.
    pub fn first_day_of_week(&self) -> i64 {
        i64::from(self.cal.attribute(attr::FIRST_DAY_OF_WEEK))
    }

    /// `IntlCalendar::setFirstDayOfWeek($dayOfWeek)`.
    pub fn set_first_day_of_week(&mut self, dow: i64) -> Result<()> {
        let d = dow_arg(dow, 1)?;
        self.cal.set_attribute(attr::FIRST_DAY_OF_WEEK, d);
        Ok(())
    }

    /// `IntlCalendar::getMinimalDaysInFirstWeek()`.
    pub fn minimal_days_in_first_week(&self) -> i64 {
        i64::from(self.cal.attribute(attr::MINIMAL_DAYS_IN_FIRST_WEEK))
    }

    /// `IntlCalendar::setMinimalDaysInFirstWeek($days)`.
    pub fn set_minimal_days_in_first_week(&mut self, days: i64) -> Result<()> {
        if !(1..=7).contains(&days) {
            return Err(Error::arg_value(1, "must be between 1 and 7"));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.cal
            .set_attribute(attr::MINIMAL_DAYS_IN_FIRST_WEEK, days as i32);
        Ok(())
    }

    /// `IntlCalendar::getLocale($type)`.
    pub fn locale(&self, ty: i64) -> Result<String> {
        if ty != 0 && ty != 1 {
            return Err(Error::arg_value(
                1,
                "must be either Locale::ACTUAL_LOCALE or Locale::VALID_LOCALE",
            ));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.cal
            .locale_by_type(ty as i32)
            .or_intl("Call to ICU method has failed")
    }

    /// `IntlCalendar::getTimeZone()`.
    pub fn time_zone(&self) -> Result<TimeZone> {
        let id = self.cal.time_zone_id().map_err(|_| {
            Error::intl(
                UErrorCode::MEMORY_ALLOCATION_ERROR,
                "could not clone TimeZone",
            )
        })?;
        Ok(TimeZone::from_resolved_id(id))
    }

    /// `IntlCalendar::getType()`.
    pub fn cal_type(&self) -> String {
        self.cal.cal_type().unwrap_or_default()
    }

    /// `IntlCalendar::inDaylightTime()`.
    pub fn in_daylight_time(&self) -> Result<bool> {
        self.cal
            .in_daylight_time()
            .or_intl("Error calling ICU method")
    }

    /// `IntlCalendar::isLenient()`.
    pub fn is_lenient(&self) -> bool {
        self.cal.attribute(attr::LENIENT) != 0
    }

    /// `IntlCalendar::setLenient($lenient)`.
    pub fn set_lenient(&mut self, v: bool) {
        self.cal.set_attribute(attr::LENIENT, i32::from(v));
    }

    /// `IntlCalendar::isSet($field)`.
    pub fn is_set(&self, f: i64) -> Result<bool> {
        let f = field_arg(f, 1)?;
        Ok(self.cal.is_set(f))
    }

    /// `IntlCalendar::isWeekend($timestamp = null)`.
    pub fn is_weekend(&self, date: Option<f64>) -> Result<bool> {
        if let Some(d) = date {
            self.cal.is_weekend(d).or_intl("Error calling ICU method")
        } else {
            let now = self.cal.millis().unwrap_or(0.0);
            Ok(self.cal.is_weekend(now).unwrap_or(false))
        }
    }

    /// `IntlCalendar::getRepeatedWallTimeOption()`.
    pub fn repeated_wall_time_option(&self) -> i64 {
        i64::from(self.cal.attribute(attr::REPEATED_WALL_TIME))
    }

    /// `IntlCalendar::getSkippedWallTimeOption()`.
    pub fn skipped_wall_time_option(&self) -> i64 {
        i64::from(self.cal.attribute(attr::SKIPPED_WALL_TIME))
    }

    /// `IntlCalendar::setRepeatedWallTimeOption($option)`.
    pub fn set_repeated_wall_time_option(&mut self, o: i64) -> Result<()> {
        if o != walltime::FIRST && o != walltime::LAST {
            return Err(Error::arg_value(
                1,
                "must be either IntlCalendar::WALLTIME_FIRST or IntlCalendar::WALLTIME_LAST",
            ));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.cal.set_attribute(attr::REPEATED_WALL_TIME, o as i32);
        Ok(())
    }

    /// `IntlCalendar::setSkippedWallTimeOption($option)`.
    pub fn set_skipped_wall_time_option(&mut self, o: i64) -> Result<()> {
        if !(walltime::LAST..=walltime::NEXT_VALID).contains(&o) {
            return Err(Error::arg_value(
                1,
                "must be one of IntlCalendar::WALLTIME_FIRST, IntlCalendar::WALLTIME_LAST, or IntlCalendar::WALLTIME_NEXT_VALID",
            ));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.cal.set_attribute(attr::SKIPPED_WALL_TIME, o as i32);
        Ok(())
    }

    /// `IntlCalendar::toDateTime()` → `(timestamp, zone)`; the VM builds
    /// `new DateTime("@$timestamp", $tz)` and calls `setTimeZone($tz)`.
    pub fn to_date_time(&self) -> Result<(i64, DateTimeZoneSpec)> {
        let date = self.cal.millis().or_intl("Call to ICU method has failed")? / 1000.0;
        #[allow(clippy::cast_precision_loss)]
        if date > u64::MAX as f64
            || date < 0.0 - u64::MAX as f64
            || !(-9.3e18..9.3e18).contains(&date)
        {
            return Err(Error::intl(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "The calendar date is out of the range for a 64-bit integer",
            ));
        }
        let ts = crate::numfmt::c_double_to_i64(date);
        let tz = self.time_zone()?;
        Ok((ts, tz.to_date_time_zone()?))
    }

    /// The `var_dump()` view.
    pub fn debug_info(&self) -> CalendarDebugInfo {
        let tz = self.time_zone().unwrap_or_else(|_| TimeZone::unknown());
        let locale = self.cal.locale_by_type(1).unwrap_or_else(UErrorCode::name);
        let fields = DEBUG_FIELDS
            .iter()
            .map(|&(f, name)| {
                (
                    name,
                    self.cal.get(f).map(i64::from).map_err(UErrorCode::name),
                )
            })
            .collect();
        CalendarDebugInfo {
            cal_type: self.cal_type(),
            time_zone: tz.debug_info(),
            locale,
            fields,
        }
    }

    // ---------------------------------------------------------------
    // IntlGregorianCalendar
    // ---------------------------------------------------------------

    /// `new IntlGregorianCalendar($timezone, $locale)` /
    /// `intlgregcal_create_instance()` (0–2 argument variant).
    pub fn gregorian(tz: &TimeZone, locale: Option<&str>) -> Result<Self> {
        let locale = locale.map_or_else(common::default_locale, str::to_owned);
        let cal =
            Calendar::open(Some(tz.id_u16()), Some(&locale), caltype::GREGORIAN).map_err(|c| {
                Error::global(
                    c,
                    "error creating ICU GregorianCalendar from time zone and locale",
                )
            })?;
        Ok(Self { cal })
    }

    /// The date/time variants (`createFromDate`, `createFromDateTime`, and
    /// the deprecated 3/5/6-argument constructor): fields are set on a
    /// cleared calendar in ICU's default locale, then the zone is set to
    /// PHP's default time zone (`php_default_tz`).
    pub fn gregorian_from_fields(args: &[i64], php_default_tz: &str) -> Result<Self> {
        let mut v = [0i32; 6];
        for (i, a) in args.iter().enumerate().take(6) {
            #[allow(clippy::cast_possible_truncation)]
            {
                v[i] = int32_arg(*a, i as u32 + 1)?;
            }
        }
        let mut cal = Calendar::open(None, None, caltype::GREGORIAN)
            .map_err(|c| Error::global(c, "Error creating ICU GregorianCalendar from date"))?;
        cal.clear();
        cal.set(field::ERA, 1);
        cal.set(field::YEAR, v[0]);
        cal.set(field::MONTH, v[1]);
        cal.set(field::DATE, v[2]);
        if args.len() >= 5 {
            cal.set(field::HOUR_OF_DAY, v[3]);
            cal.set(field::MINUTE, v[4]);
        }
        if args.len() >= 6 {
            cal.set(field::SECOND, v[5]);
        }
        let tz = TimeZone::create_u16(&ustr::str_u16(php_default_tz));
        cal.set_time_zone(tz.id_u16())
            .or_intl("Error creating ICU GregorianCalendar from date")?;
        Ok(Self { cal })
    }

    /// `IntlGregorianCalendar::setGregorianChange($timestamp)`.
    pub fn set_gregorian_change(&mut self, d: f64) -> Result<()> {
        self.cal
            .set_gregorian_change(d)
            .or_intl("error calling ICU method")
    }

    /// `IntlGregorianCalendar::getGregorianChange()`.
    pub fn gregorian_change(&self) -> f64 {
        self.cal.gregorian_change().unwrap_or(f64::NAN)
    }

    /// `IntlGregorianCalendar::isLeapYear($year)`.
    pub fn is_leap_year(&self, year: i64) -> Result<bool> {
        let y = int32_arg(year, 1)?;
        let cutover_year = self.cutover_year();
        Ok(if y >= cutover_year {
            y.rem_euclid(4) == 0 && (y % 100 != 0 || y % 400 == 0)
        } else {
            y.rem_euclid(4) == 0
        })
    }

    fn cutover_year(&self) -> i32 {
        const DAY: f64 = 86_400_000.0;
        let change = self.cal.gregorian_change().unwrap_or(-12_219_292_800_000.0);
        let day = (change / DAY).floor();
        let date = if day <= f64::from(i32::MIN) {
            f64::from(i32::MIN) * DAY
        } else if day >= f64::from(i32::MAX) {
            f64::from(i32::MAX) * DAY
        } else {
            change
        };
        let zone = self.cal.time_zone_id().unwrap_or_default();
        let Ok(mut g) = Calendar::open(Some(&zone), Some("en_US"), caltype::GREGORIAN) else {
            return 1582;
        };
        let _ = g.set_millis(date);
        let y = g.get(field::YEAR).unwrap_or(1582);
        if g.get(field::ERA).unwrap_or(1) == 0 {
            1 - y
        } else {
            y
        }
    }
}

/// `IntlCalendar::getKeywordValuesForLocale($keyword, $locale, $onlyCommon)`.
pub fn keyword_values_for_locale(
    key: &str,
    locale: &str,
    commonly_used: bool,
) -> Result<Vec<String>> {
    ucal::keyword_values_for_locale(key, locale, commonly_used)
        .map_err(|c| Error::global(c, "error calling underlying method"))
}

/// `IntlCalendar::getNow()`.
pub fn now() -> f64 {
    ucal::now()
}

/// `IntlCalendar::getAvailableLocales()`.
pub fn available_locales() -> Vec<String> {
    ucal::available_locales()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::create_time_zone(b"UTC").unwrap()
    }

    #[test]
    fn create_and_fields() {
        let mut c = IntlCalendar::create_instance(&utc(), Some("en_US")).unwrap();
        assert!(c.is_gregorian());
        c.set_time(1_356_998_400_000.0).unwrap(); // 2013-01-01
        assert_eq!(c.get(1).unwrap(), 2013);
        assert_eq!(c.get(2).unwrap(), 0);
        c.add(2, 1).unwrap();
        assert_eq!(c.get(2).unwrap(), 1);
        assert_eq!(c.get(99), Err(Error::arg_value(1, "must be a valid field")));
        assert_eq!(c.limit(5, Limit::ActualMaximum).unwrap(), 28);
        let jp = IntlCalendar::create_instance(&utc(), Some("ja_JP@calendar=japanese")).unwrap();
        assert!(!jp.is_gregorian());
        assert_eq!(jp.cal_type(), "japanese");
    }

    #[test]
    fn gregorian_bits() {
        let c = IntlCalendar::gregorian(&utc(), Some("en_US")).unwrap();
        assert!(c.is_leap_year(2000).unwrap());
        assert!(!c.is_leap_year(1900).unwrap());
        assert!(c.is_leap_year(1500).unwrap());
        let g = IntlCalendar::gregorian_from_fields(&[2012, 1, 29], "Europe/Lisbon").unwrap();
        assert_eq!(g.get(5).unwrap(), 29);
        assert_eq!(g.time_zone().unwrap().id().unwrap(), "Europe/Lisbon");
        assert_eq!(g.get(11).unwrap(), 0);
    }

    #[test]
    fn to_date_time() {
        let mut c =
            IntlCalendar::create_instance(&TimeZone::create_time_zone(b"GMT+01:00").unwrap(), None)
                .unwrap();
        c.set_time(1000.0 * 3600.0).unwrap();
        assert_eq!(
            c.to_date_time().unwrap(),
            (3600, DateTimeZoneSpec::Offset(3600))
        );
    }
}
