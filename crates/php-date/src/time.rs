// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The broken-down time structure (`timelib_time`), relative times, and the
//! conversions between broken-down time and Unix timestamps (ports of
//! timelib's `tm2unixtime.c`, `unixtime2tm.c` and parts of `timelib.c`).
//!
//! Arithmetic deliberately wraps on overflow, mirroring the C code's
//! two's-complement behaviour for absurd inputs (e.g. `PHP_INT_MAX` years).

use std::sync::Arc;

use crate::dow::{day_of_week, is_leap};
use crate::tz::TzInfo;
use crate::UNSET;

/// How a time's zone is specified (`TIMELIB_ZONETYPE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZoneType {
    /// No zone information.
    #[default]
    None = 0,
    /// A fixed UTC offset (`+02:00`).
    Offset = 1,
    /// A timezone abbreviation (`CEST`).
    Abbr = 2,
    /// A timezone identifier (`Europe/Amsterdam`).
    Id = 3,
}

impl ZoneType {
    /// The numeric value PHP exposes as `timezone_type`.
    pub fn as_i64(self) -> i64 {
        self as i64
    }
}

/// `TIMELIB_SPECIAL_WEEKDAY`
pub const SPECIAL_WEEKDAY: u32 = 1;
/// `TIMELIB_SPECIAL_DAY_OF_WEEK_IN_MONTH`
pub const SPECIAL_DAY_OF_WEEK_IN_MONTH: u32 = 2;
/// `TIMELIB_SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH`
pub const SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH: u32 = 3;
/// `first day of`
pub const FIRST_DAY_OF_MONTH: i32 = 1;
/// `last day of`
pub const LAST_DAY_OF_MONTH: i32 = 2;

/// A relative time / interval (`timelib_rel_time`, i.e. `DateInterval`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelTime {
    /// Years.
    pub y: i64,
    /// Months.
    pub m: i64,
    /// Days.
    pub d: i64,
    /// Hours.
    pub h: i64,
    /// Minutes.
    pub i: i64,
    /// Seconds.
    pub s: i64,
    /// Microseconds.
    pub us: i64,
    /// Target day of week for "next monday" style relatives (0 = Sunday).
    pub weekday: i32,
    /// 0: don't count the current day, 1: count it, 2: "this week" semantics.
    pub weekday_behavior: i32,
    /// 0, [`FIRST_DAY_OF_MONTH`] or [`LAST_DAY_OF_MONTH`].
    pub first_last_day_of: i32,
    /// Non-zero if the interval is negative (PHP stores whatever integer is
    /// assigned to `$interval->invert`).
    pub invert: i32,
    /// Total number of days for a `diff()` result, [`UNSET`] otherwise.
    pub days: i64,
    /// Special relative type (`SPECIAL_*`), 0 if none.
    pub special_type: u32,
    /// Amount for the special relative.
    pub special_amount: i64,
    /// Whether `weekday` is in use.
    pub have_weekday_relative: bool,
    /// Whether the special relative is in use.
    pub have_special_relative: bool,
}

/// A broken-down date/time (`timelib_time`).
#[derive(Debug, Clone, PartialEq)]
pub struct Time {
    /// Year ([`UNSET`] when not parsed).
    pub y: i64,
    /// Month.
    pub m: i64,
    /// Day.
    pub d: i64,
    /// Hour.
    pub h: i64,
    /// Minute.
    pub i: i64,
    /// Second.
    pub s: i64,
    /// Microsecond.
    pub us: i64,
    /// UTC offset in seconds (for offset/abbreviation zones, excluding DST).
    pub z: i64,
    /// Timezone abbreviation (upper case).
    pub tz_abbr: Option<String>,
    /// Timezone (for [`ZoneType::Id`]).
    pub tz_info: Option<Arc<TzInfo>>,
    /// DST flag.
    pub dst: i64,
    /// Relative part.
    pub relative: RelTime,
    /// Seconds since the epoch.
    pub sse: i64,
    /// Time was specified (may be 2 after "gnunocolon" handling).
    pub have_time: i32,
    /// Date was specified.
    pub have_date: i32,
    /// Zone was specified (counts repeats).
    pub have_zone: i32,
    /// Relative part present.
    pub have_relative: bool,
    /// Week-number/day present.
    pub have_weeknr_day: bool,
    /// `sse` is in sync with the fields.
    pub sse_uptodate: bool,
    /// The fields are in sync with `sse`.
    pub tim_uptodate: bool,
    /// Whether this is local time (has a zone).
    pub is_localtime: bool,
    /// Zone kind.
    pub zone_type: ZoneType,
}

impl Default for Time {
    fn default() -> Self {
        Self {
            y: 0,
            m: 0,
            d: 0,
            h: 0,
            i: 0,
            s: 0,
            us: 0,
            z: 0,
            tz_abbr: None,
            tz_info: None,
            dst: 0,
            relative: RelTime::default(),
            sse: 0,
            have_time: 0,
            have_date: 0,
            have_zone: 0,
            have_relative: false,
            have_weeknr_day: false,
            sse_uptodate: false,
            tim_uptodate: false,
            is_localtime: false,
            zone_type: ZoneType::None,
        }
    }
}

/// Seconds for `h:m:s`.
pub fn hms_to_seconds(h: i64, m: i64, s: i64) -> i64 {
    h.wrapping_mul(3600)
        .wrapping_add(m.wrapping_mul(60))
        .wrapping_add(s)
}

/// Decimal hours from h/m/s/µs (`timelib_hmsf_to_decimal_hour`).
#[allow(clippy::cast_precision_loss)]
pub fn hmsf_to_decimal_hour(hour: i64, min: i64, sec: i64, us: i64) -> f64 {
    let (h, m, s, u) = (hour as f64, min as f64, sec as f64, us as f64);
    if hour >= 0 {
        (h + m / 60.0 + s / 3600.0) + u / 3_600_000_000.0
    } else {
        (h - m / 60.0 - s / 3600.0) - u / 3_600_000_000.0
    }
}

// ---------------------------------------------------------------------------
// unixtime2tm.c
// ---------------------------------------------------------------------------

const DAYS_PER_ERA: i64 = 146_097;
const YEARS_PER_ERA: i64 = 400;
const HINNANT_EPOCH_SHIFT: i64 = 719_468;

/// Converts days since 1970-01-01 to `(y, m, d)` (Hinnant's algorithm).
pub fn date_from_epoch_days(epoch_days: i64) -> (i64, i64, i64) {
    let days = epoch_days.wrapping_add(HINNANT_EPOCH_SHIFT);
    let era = if days >= 0 {
        days
    } else {
        days - DAYS_PER_ERA + 1
    } / DAYS_PER_ERA;
    let day_of_era = days.wrapping_sub(era.wrapping_mul(DAYS_PER_ERA));
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let mut y = year_of_era.wrapping_add(era.wrapping_mul(YEARS_PER_ERA));
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_portion = (5 * day_of_year + 2) / 153;
    let d = day_of_year - (153 * month_portion + 2) / 5 + 1;
    let m = month_portion + if month_portion < 10 { 3 } else { -9 };
    if m <= 2 {
        y = y.wrapping_add(1);
    }
    (y, m, d)
}

/// The UTC calendar date of a timestamp.
pub fn unixtime2date(ts: i64) -> (i64, i64, i64) {
    let mut epoch_days = ts / 86400;
    if ts % 86400 < 0 {
        epoch_days -= 1;
    }
    date_from_epoch_days(epoch_days)
}

/// Days since the epoch for the time's y/m/d (`timelib_epoch_days_from_time`).
pub fn epoch_days_from_ymd(y: i64, m: i64, d: i64) -> i64 {
    let y = y.wrapping_sub(i64::from(m <= 2));
    let era = if y >= 0 { y } else { y.wrapping_sub(399) } / YEARS_PER_ERA;
    let year_of_era = y.wrapping_sub(era.wrapping_mul(YEARS_PER_ERA));
    let day_of_year = (153_i64
        .wrapping_mul(m.wrapping_add(if m > 2 { -3 } else { 9 }))
        .wrapping_add(2))
        / 5
        + d.wrapping_sub(1);
    let day_of_era = year_of_era
        .wrapping_mul(365)
        .wrapping_add(year_of_era / 4)
        .wrapping_sub(year_of_era / 100)
        .wrapping_add(day_of_year);
    era.wrapping_mul(DAYS_PER_ERA)
        .wrapping_add(day_of_era)
        .wrapping_sub(HINNANT_EPOCH_SHIFT)
}

/// Seconds since epoch of the y-m-d h:i:s fields treated as UTC.
pub fn ymdhis_to_sse(y: i64, m: i64, d: i64, h: i64, i: i64, s: i64) -> i64 {
    let days = epoch_days_from_ymd(y, m, d);
    hms_to_seconds(h, i, s)
        .wrapping_add(days.wrapping_mul(43200))
        .wrapping_add(days.wrapping_mul(43200))
}

impl Time {
    /// A fresh structure with every field set to [`UNSET`], as the parsers
    /// initialise it.
    pub fn new_unset() -> Self {
        Self {
            y: UNSET,
            m: UNSET,
            d: UNSET,
            h: UNSET,
            i: UNSET,
            s: UNSET,
            us: UNSET,
            z: UNSET,
            dst: UNSET,
            ..Self::default()
        }
    }

    /// A UTC time for `ts` (`timelib_unixtime2gmt` on a fresh struct).
    pub fn from_gmt(ts: i64) -> Self {
        let mut t = Self::default();
        t.unixtime2gmt(ts);
        t
    }

    /// A local time in zone `tz` for `ts`.
    pub fn from_timestamp_in(ts: i64, tz: &Arc<TzInfo>) -> Self {
        let mut t = Self {
            tz_info: Some(Arc::clone(tz)),
            zone_type: ZoneType::Id,
            ..Self::default()
        };
        t.unixtime2local(ts);
        t
    }

    /// `timelib_unixtime2gmt`.
    pub fn unixtime2gmt(&mut self, ts: i64) {
        let (y, m, d) = unixtime2date(ts);
        self.y = y;
        self.m = m;
        self.d = d;
        let mut remainder = ts % 86400;
        if remainder < 0 {
            remainder += 86400;
        }
        let hours = remainder / 3600;
        self.h = hours;
        self.i = (remainder - hours * 3600) / 60;
        self.s = remainder % 60;
        self.z = 0;
        self.dst = 0;
        self.sse = ts;
        self.sse_uptodate = true;
        self.tim_uptodate = true;
        self.is_localtime = false;
    }

    /// `timelib_update_from_sse`: recompute the fields from `sse`.
    pub fn update_from_sse(&mut self) {
        let z = self.z;
        let dst = self.dst;
        let sse = self.sse;
        match self.zone_type {
            ZoneType::Abbr | ZoneType::Offset => {
                let ts = self
                    .sse
                    .wrapping_add(self.z)
                    .wrapping_add(self.dst.wrapping_mul(3600));
                self.unixtime2gmt(ts);
            }
            ZoneType::Id => {
                let offset = self
                    .tz_info
                    .as_ref()
                    .and_then(|tz| tz.offset_info(self.sse))
                    .map_or(0, |o| o.0);
                self.unixtime2gmt(self.sse.wrapping_add(i64::from(offset)));
            }
            ZoneType::None => self.unixtime2gmt(self.sse),
        }
        self.sse = sse;
        self.is_localtime = true;
        self.have_zone = 1;
        self.z = z;
        self.dst = dst;
    }

    /// `timelib_unixtime2local`.
    pub fn unixtime2local(&mut self, ts: i64) {
        match self.zone_type {
            ZoneType::Abbr | ZoneType::Offset => {
                let z = self.z;
                let dst = self.dst;
                self.unixtime2gmt(
                    ts.wrapping_add(self.z)
                        .wrapping_add(self.dst.wrapping_mul(3600)),
                );
                self.sse = ts;
                self.z = z;
                self.dst = dst;
            }
            ZoneType::Id => {
                let Some(tz) = self.tz_info.clone() else {
                    self.is_localtime = false;
                    self.have_zone = 0;
                    return;
                };
                let off = tz.time_zone_info(ts);
                self.unixtime2gmt(ts.wrapping_add(i64::from(off.offset)));
                self.sse = ts;
                self.dst = i64::from(off.is_dst);
                self.z = i64::from(off.offset);
                self.tz_info = Some(tz);
                self.tz_abbr = Some(off.abbr.to_ascii_uppercase());
            }
            ZoneType::None => {
                self.is_localtime = false;
                self.have_zone = 0;
                return;
            }
        }
        self.is_localtime = true;
        self.have_zone = 1;
    }

    /// `timelib_set_timezone_from_offset`.
    pub fn set_timezone_from_offset(&mut self, utc_offset: i64) {
        self.tz_abbr = None;
        self.z = utc_offset;
        self.have_zone = 1;
        self.zone_type = ZoneType::Offset;
        self.dst = 0;
        self.tz_info = None;
    }

    /// `timelib_set_timezone_from_abbr`.
    pub fn set_timezone_from_abbr(&mut self, abbr: &str, utc_offset: i64, dst: i64) {
        self.tz_abbr = Some(abbr.to_string());
        self.z = utc_offset;
        self.have_zone = 1;
        self.zone_type = ZoneType::Abbr;
        self.dst = dst;
        self.tz_info = None;
    }

    /// `timelib_set_timezone`: switch to zone `tz`, keeping `sse`.
    pub fn set_timezone(&mut self, tz: &Arc<TzInfo>) {
        let off = tz.time_zone_info(self.sse);
        self.z = i64::from(off.offset);
        self.dst = i64::from(off.is_dst);
        self.tz_info = Some(Arc::clone(tz));
        self.tz_abbr = Some(off.abbr);
        self.have_zone = 1;
        self.zone_type = ZoneType::Id;
    }

    /// `timelib_get_current_offset`: total UTC offset at `sse`.
    pub fn current_offset(&self) -> i64 {
        match self.zone_type {
            ZoneType::Abbr | ZoneType::Offset => self.z + self.dst * 3600,
            ZoneType::Id => self
                .tz_info
                .as_ref()
                .and_then(|tz| tz.offset_info(self.sse))
                .map_or(0, |o| i64::from(o.0)),
            ZoneType::None => 0,
        }
    }

    /// `timelib_time_compare`.
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        (self.sse, self.us).cmp(&(other.sse, other.us))
    }

    /// `timelib_epoch_days_from_time`.
    pub fn epoch_days(&self) -> i64 {
        epoch_days_from_ymd(self.y, self.m, self.d)
    }

    /// `timelib_do_normalize`.
    pub fn do_normalize(&mut self) {
        if self.us != UNSET {
            do_range_limit(0, 1_000_000, 1_000_000, &mut self.us, &mut self.s);
        }
        if self.s != UNSET {
            do_range_limit(0, 60, 60, &mut self.s, &mut self.i);
        }
        if self.s != UNSET {
            do_range_limit(0, 60, 60, &mut self.i, &mut self.h);
        }
        if self.s != UNSET {
            do_range_limit(0, 24, 24, &mut self.h, &mut self.d);
        }
        do_range_limit(1, 13, 12, &mut self.m, &mut self.y);

        if self.y == 1970 && self.m == 1 {
            let (y, m, d) = date_from_epoch_days(self.d.wrapping_sub(1));
            self.y = y;
            self.m = m;
            self.d = d;
            return;
        }
        while do_range_limit_days(&mut self.y, &mut self.m, &mut self.d) {}
        do_range_limit(1, 13, 12, &mut self.m, &mut self.y);
    }

    fn do_adjust_for_weekday(&mut self) {
        let current_dow = day_of_week(self.y, self.m, self.d);
        let rel = &mut self.relative;
        if rel.weekday_behavior == 2 {
            if current_dow == 0 && rel.weekday != 0 {
                rel.weekday -= 7;
            }
            if rel.weekday == 0 && current_dow != 0 {
                rel.weekday = 7;
            }
            self.d = self
                .d
                .wrapping_sub(current_dow)
                .wrapping_add(i64::from(rel.weekday));
            return;
        }
        let mut difference = i64::from(rel.weekday) - current_dow;
        if (rel.d < 0 && difference < 0)
            || (rel.d >= 0 && difference <= -i64::from(rel.weekday_behavior))
        {
            difference += 7;
        }
        if rel.weekday >= 0 {
            self.d = self.d.wrapping_add(difference);
        } else {
            self.d = self
                .d
                .wrapping_sub(7 - (i64::from(rel.weekday.unsigned_abs()) - current_dow));
        }
        rel.have_weekday_relative = false;
    }

    fn do_adjust_relative(&mut self) {
        if self.relative.have_weekday_relative {
            self.do_adjust_for_weekday();
        }
        self.do_normalize();
        if self.have_relative {
            let r = &self.relative;
            self.us = self.us.wrapping_add(r.us);
            self.s = self.s.wrapping_add(r.s);
            self.i = self.i.wrapping_add(r.i);
            self.h = self.h.wrapping_add(r.h);
            self.d = self.d.wrapping_add(r.d);
            self.m = self.m.wrapping_add(r.m);
            self.y = self.y.wrapping_add(r.y);
        }
        match self.relative.first_last_day_of {
            FIRST_DAY_OF_MONTH => self.d = 1,
            LAST_DAY_OF_MONTH => {
                self.d = 0;
                self.m = self.m.wrapping_add(1);
            }
            _ => {}
        }
        self.do_normalize();
    }

    fn do_adjust_special_weekday(&mut self) {
        let count = self.relative.special_amount;
        let dow = day_of_week(self.y, self.m, self.d);
        self.d = self.d.wrapping_add((count / 5).wrapping_mul(7));
        let rem = count % 5;
        if count > 0 {
            if rem == 0 {
                if dow == 0 {
                    self.d -= 2;
                } else if dow == 6 {
                    self.d -= 1;
                }
            } else if dow == 6 {
                self.d += 1;
            } else if dow + rem > 5 {
                self.d += 2;
            }
        } else if rem == 0 {
            if dow == 6 {
                self.d += 2;
            } else if dow == 0 {
                self.d += 1;
            }
        } else if dow == 0 {
            self.d -= 1;
        } else if dow + rem < 1 {
            self.d -= 2;
        }
        self.d = self.d.wrapping_add(rem);
    }

    fn do_adjust_special(&mut self) {
        if self.relative.have_special_relative && self.relative.special_type == SPECIAL_WEEKDAY {
            self.do_adjust_special_weekday();
        }
        self.do_normalize();
        self.relative.special_type = 0;
        self.relative.special_amount = 0;
    }

    fn do_adjust_special_early(&mut self) {
        if self.relative.have_special_relative {
            match self.relative.special_type {
                SPECIAL_DAY_OF_WEEK_IN_MONTH => {
                    self.d = 1;
                    self.m = self.m.wrapping_add(self.relative.m);
                    self.relative.m = 0;
                }
                SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH => {
                    self.d = 1;
                    self.m = self.m.wrapping_add(self.relative.m).wrapping_add(1);
                    self.relative.m = 0;
                }
                _ => {}
            }
        }
        match self.relative.first_last_day_of {
            FIRST_DAY_OF_MONTH => self.d = 1,
            LAST_DAY_OF_MONTH => {
                self.d = 0;
                self.m = self.m.wrapping_add(1);
            }
            _ => {}
        }
        self.do_normalize();
    }

    fn do_adjust_timezone(&mut self, tzi: Option<&Arc<TzInfo>>) {
        match self.zone_type {
            ZoneType::Offset => {
                self.is_localtime = true;
                self.sse = self.sse.wrapping_sub(self.z);
            }
            ZoneType::Abbr => {
                self.is_localtime = true;
                self.sse = self
                    .sse
                    .wrapping_sub(self.z)
                    .wrapping_sub(self.dst.wrapping_mul(3600));
            }
            ZoneType::Id | ZoneType::None => {
                let tzi = if self.zone_type == ZoneType::Id {
                    self.tz_info.clone()
                } else {
                    tzi.cloned()
                };
                let Some(tzi) = tzi else {
                    return;
                };
                let info = |ts: i64| tzi.offset_info(ts).unwrap_or((0, 0, false));
                let (current_offset, _current_tt, current_is_dst) = info(self.sse);
                let current_offset = i64::from(current_offset);
                let (after_offset, after_tt, _) = info(self.sse.wrapping_sub(current_offset));
                let mut actual_offset = i64::from(after_offset);
                let mut actual_tt = after_tt;
                if current_offset == actual_offset && self.have_zone != 0 {
                    if current_offset >= 0 && self.dst != 0 && !current_is_dst {
                        let (earlier_offset, earlier_tt, _) =
                            info(self.sse.wrapping_sub(current_offset).wrapping_sub(7200));
                        let earlier_offset = i64::from(earlier_offset);
                        if earlier_offset != i64::from(after_offset)
                            && self.sse.wrapping_sub(earlier_offset) < after_tt
                        {
                            actual_offset = earlier_offset;
                            actual_tt = earlier_tt;
                        }
                    } else if current_offset <= 0 && current_is_dst && self.dst == 0 {
                        let (later_offset, later_tt, _) =
                            info(self.sse.wrapping_sub(current_offset).wrapping_add(7200));
                        let later_offset = i64::from(later_offset);
                        if later_offset != i64::from(after_offset)
                            && self.sse.wrapping_sub(later_offset) >= later_tt
                        {
                            actual_offset = later_offset;
                            actual_tt = later_tt;
                        }
                    }
                }
                self.is_localtime = true;
                let local = self.sse.wrapping_sub(actual_offset);
                let in_transition = actual_tt != i64::MIN
                    && local >= actual_tt.wrapping_add(current_offset - actual_offset)
                    && local < actual_tt;
                let adjustment = if current_offset != actual_offset && !in_transition {
                    -actual_offset
                } else {
                    -current_offset
                };
                self.sse = self.sse.wrapping_add(adjustment);
                self.set_timezone(&tzi);
            }
        }
    }

    /// `timelib_update_ts`: apply the relative part and compute `sse`,
    /// interpreting the fields in this time's zone (or `tzi` if it has none).
    pub fn update_ts(&mut self, tzi: Option<&Arc<TzInfo>>) {
        self.do_adjust_special_early();
        self.do_adjust_relative();
        self.do_adjust_special();

        let days = self.epoch_days();
        self.sse = hms_to_seconds(self.h, self.i, self.s)
            .wrapping_add(days.wrapping_mul(43200))
            .wrapping_add(days.wrapping_mul(43200));

        self.do_adjust_timezone(tzi);

        self.sse_uptodate = true;
        self.have_relative = false;
        self.relative.have_weekday_relative = false;
        self.relative.have_special_relative = false;
        self.relative.first_last_day_of = 0;
    }

    /// `timelib_fill_holes`: fill unset fields from `now`.
    ///
    /// `override_time` corresponds to `TIMELIB_OVERRIDE_TIME`.
    pub fn fill_holes(&mut self, now: &Self, override_time: bool) {
        if !override_time && self.have_date != 0 && self.have_time == 0 {
            self.h = 0;
            self.i = 0;
            self.s = 0;
            self.us = 0;
        }
        if self.y != UNSET
            || self.m != UNSET
            || self.d != UNSET
            || self.h != UNSET
            || self.i != UNSET
            || self.s != UNSET
        {
            if self.us == UNSET {
                self.us = 0;
            }
        } else if self.us == UNSET {
            self.us = if now.us == UNSET { 0 } else { now.us };
        }
        let pick = |v: i64, n: i64| {
            if v == UNSET {
                if n == UNSET {
                    0
                } else {
                    n
                }
            } else {
                v
            }
        };
        self.y = pick(self.y, now.y);
        self.m = pick(self.m, now.m);
        self.d = pick(self.d, now.d);
        self.h = pick(self.h, now.h);
        self.i = pick(self.i, now.i);
        self.s = pick(self.s, now.s);
        if self.tz_info.is_none() {
            self.tz_info.clone_from(&now.tz_info);
            self.z = pick(self.z, now.z);
            self.dst = pick(self.dst, now.dst);
            if self.tz_abbr.is_none() {
                self.tz_abbr.clone_from(&now.tz_abbr);
            }
        }
        if self.zone_type == ZoneType::None && now.zone_type != ZoneType::None {
            self.zone_type = now.zone_type;
            self.is_localtime = true;
        }
    }
}

/// `timelib_same_timezone`.
pub fn same_timezone(one: &Time, two: &Time) -> bool {
    if one.zone_type != two.zone_type {
        return false;
    }
    match one.zone_type {
        ZoneType::Abbr | ZoneType::Offset => one.z + one.dst * 3600 == two.z + two.dst * 3600,
        ZoneType::Id => match (&one.tz_info, &two.tz_info) {
            (Some(a), Some(b)) => a.name == b.name,
            _ => false,
        },
        ZoneType::None => false,
    }
}

/// `do_range_limit` from tm2unixtime.c: bring `a` into `[start, end)`
/// carrying into `b`.
pub(crate) fn do_range_limit(start: i64, end: i64, adj: i64, a: &mut i64, b: &mut i64) {
    if *a < start {
        let a_plus_1 = a.wrapping_add(1);
        *b = b.wrapping_sub(start.wrapping_sub(a_plus_1) / adj + 1);
        *a = a.wrapping_add(adj.wrapping_mul(start.wrapping_sub(a_plus_1) / adj));
        *a = a.wrapping_add(adj);
    }
    if *a >= end {
        *b = b.wrapping_add(*a / adj);
        *a -= adj * (*a / adj);
    }
}

const DAYS_IN_MONTH_LEAP: [i64; 13] = [31, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const DAYS_IN_MONTH: [i64; 13] = [31, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn month_len(year: i64, month: i64) -> i64 {
    let t = if is_leap(year) {
        &DAYS_IN_MONTH_LEAP
    } else {
        &DAYS_IN_MONTH
    };
    usize::try_from(month)
        .ok()
        .and_then(|i| t.get(i))
        .copied()
        .unwrap_or(31)
}

fn do_range_limit_days(y: &mut i64, m: &mut i64, d: &mut i64) -> bool {
    if *d >= DAYS_PER_ERA || *d <= -DAYS_PER_ERA {
        *y = y.wrapping_add(YEARS_PER_ERA.wrapping_mul(*d / DAYS_PER_ERA));
        *d -= DAYS_PER_ERA * (*d / DAYS_PER_ERA);
    }
    do_range_limit(1, 13, 12, m, y);
    let year = *y;
    let mut retval = false;
    while *d <= 0 && *m > 0 {
        let (pm, py) = if *m - 1 < 1 {
            (*m - 1 + 12, y.wrapping_sub(1))
        } else {
            (*m - 1, *y)
        };
        *d += month_len(py, pm);
        *m -= 1;
        retval = true;
    }
    while *d > 0 && *m <= 12 && *d > month_len(year, *m) {
        *d -= month_len(year, *m);
        *m += 1;
        retval = true;
    }
    retval
}

fn inc_month(y: &mut i64, m: &mut i64) {
    *m += 1;
    if *m > 12 {
        *m -= 12;
        *y = y.wrapping_add(1);
    }
}

fn dec_month(y: &mut i64, m: &mut i64) {
    *m -= 1;
    if *m < 1 {
        *m += 12;
        *y = y.wrapping_sub(1);
    }
}

fn do_range_limit_days_relative(base_y: &mut i64, base_m: &mut i64, rt: &mut RelTime) {
    do_range_limit(1, 13, 12, base_m, base_y);
    let mut year = *base_y;
    let mut month = *base_m;
    if rt.invert != 0 {
        while rt.d < 0 {
            rt.d += month_len(year, month);
            rt.m -= 1;
            inc_month(&mut year, &mut month);
        }
    } else {
        while rt.d < 0 {
            dec_month(&mut year, &mut month);
            rt.d += month_len(year, month);
            rt.m -= 1;
        }
    }
}

/// `timelib_do_rel_normalize`: normalise `rt` relative to `base`'s month.
pub fn do_rel_normalize(base: &mut Time, rt: &mut RelTime) {
    do_range_limit(0, 1_000_000, 1_000_000, &mut rt.us, &mut rt.s);
    do_range_limit(0, 60, 60, &mut rt.s, &mut rt.i);
    do_range_limit(0, 60, 60, &mut rt.i, &mut rt.h);
    do_range_limit(0, 24, 24, &mut rt.h, &mut rt.d);
    do_range_limit(0, 12, 12, &mut rt.m, &mut rt.y);
    do_range_limit_days_relative(&mut base.y, &mut base.m, rt);
    do_range_limit(0, 12, 12, &mut rt.m, &mut rt.y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz;

    #[test]
    fn gmt_roundtrip() {
        let t = Time::from_gmt(1_234_567_890);
        assert_eq!((t.y, t.m, t.d, t.h, t.i, t.s), (2009, 2, 13, 23, 31, 30));
        let t = Time::from_gmt(-1);
        assert_eq!((t.y, t.m, t.d, t.h, t.i, t.s), (1969, 12, 31, 23, 59, 59));
        assert_eq!(ymdhis_to_sse(2009, 2, 13, 23, 31, 30), 1_234_567_890);
        let t = Time::from_gmt(i64::MIN);
        assert_eq!(t.y, -292_277_022_657);
        let t = Time::from_gmt(i64::MAX);
        assert_eq!(t.y, 292_277_026_596);
    }

    #[test]
    fn normalize_overflowing_fields() {
        let mut t = Time {
            y: 2005,
            m: 14,
            d: 35,
            h: 25,
            i: 61,
            s: 61,
            ..Time::default()
        };
        t.do_normalize();
        assert_eq!((t.y, t.m, t.d, t.h, t.i, t.s), (2006, 3, 8, 2, 2, 1));
        let mut t = Time {
            y: 2005,
            m: 0,
            d: 0,
            ..Time::default()
        };
        t.do_normalize();
        assert_eq!((t.y, t.m, t.d), (2004, 11, 30));
    }

    #[test]
    fn update_ts_in_zone() {
        let tz = tz::get("Europe/Amsterdam").unwrap();
        let mut t = Time {
            y: 2005,
            m: 7,
            d: 14,
            h: 22,
            i: 30,
            s: 41,
            ..Time::default()
        };
        t.update_ts(Some(&tz));
        assert_eq!(t.sse, 1_121_373_041);
        assert_eq!(t.tz_abbr.as_deref(), Some("CEST"));
        assert_eq!(t.z, 7200);
    }

    #[test]
    fn local_conversion() {
        let tz = tz::get("America/New_York").unwrap();
        let t = Time::from_timestamp_in(0, &tz);
        assert_eq!((t.y, t.m, t.d, t.h), (1969, 12, 31, 19));
        assert_eq!(t.tz_abbr.as_deref(), Some("EST"));
        assert_eq!(t.z, -18000);
    }
}
