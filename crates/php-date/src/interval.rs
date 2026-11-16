// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Date arithmetic with intervals: `DateTime::diff()`, `add()` and `sub()`
//! (port of timelib's `interval.c`).

use crate::time::{
    do_range_limit, do_rel_normalize, hms_to_seconds, hmsf_to_decimal_hour, same_timezone, RelTime,
    Time, ZoneType,
};

fn same_tzid(one: &Time, two: &Time) -> bool {
    one.zone_type == ZoneType::Id
        && two.zone_type == ZoneType::Id
        && match (&one.tz_info, &two.tz_info) {
            (Some(a), Some(b)) => a.name == b.name,
            _ => false,
        }
}

/// Orders `(one, two)` oldest first; returns whether they were swapped.
fn sort_old_to_new(one: &mut Time, two: &mut Time) -> bool {
    let swap = if same_tzid(one, two) {
        (one.y, one.m, one.d, one.h, one.i, one.s, one.us)
            > (two.y, two.m, two.d, two.h, two.i, two.s, two.us)
    } else {
        (one.sse, one.us) > (two.sse, two.us)
    };
    if swap {
        std::mem::swap(one, two);
    }
    swap
}

/// `timelib_diff_days`: whole days between two times.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn diff_days(one: &Time, two: &Time) -> i64 {
    if same_timezone(one, two) {
        let (earliest, latest) = if one.compare(two).is_lt() {
            (one, two)
        } else {
            (two, one)
        };
        let earliest_time = hmsf_to_decimal_hour(earliest.h, earliest.i, earliest.s, earliest.us);
        let latest_time = hmsf_to_decimal_hour(latest.h, latest.i, latest.s, latest.us);
        // C computes llabs() of the difference and stores it in an int.
        let mut days = i64::from(
            one.epoch_days()
                .wrapping_sub(two.epoch_days())
                .wrapping_abs() as i32,
        );
        if latest_time < earliest_time && days > 0 {
            days -= 1;
        }
        days
    } else {
        let ddays = ((one.sse.wrapping_sub(two.sse)) as f64).floor() / 86400.0;
        let ddays = ddays.abs();
        if ddays <= f64::from(i32::MAX) {
            ddays as i64
        } else {
            i64::from(i32::MAX)
        }
    }
}

fn diff_with_tzid(one: &Time, two: &Time) -> RelTime {
    let mut one = one.clone();
    let mut two = two.clone();
    let mut rt = RelTime {
        invert: i32::from(sort_old_to_new(&mut one, &mut two)),
        ..RelTime::default()
    };
    let mut dst_corr = two.z.wrapping_sub(one.z);
    let dst_h_corr = dst_corr / 3600;
    let dst_m_corr = (dst_corr % 3600) / 60;

    rt.y = two.y.wrapping_sub(one.y);
    rt.m = two.m.wrapping_sub(one.m);
    rt.d = two.d.wrapping_sub(one.d);
    rt.h = two.h.wrapping_sub(one.h);
    rt.i = two.i.wrapping_sub(one.i);
    rt.s = two.s.wrapping_sub(one.s);
    rt.us = two.us.wrapping_sub(one.us);
    rt.days = diff_days(&one, &two);

    if two.sse < one.sse {
        let flipped =
            rt.i.wrapping_mul(60)
                .wrapping_add(rt.s)
                .wrapping_sub(dst_corr)
                .wrapping_abs();
        rt.h = flipped / 3600;
        rt.i = (flipped - rt.h * 3600) / 60;
        rt.s = flipped % 60;
        rt.invert = 1 - rt.invert;
    }

    if rt.invert != 0 {
        do_rel_normalize(&mut one, &mut rt);
    } else {
        do_rel_normalize(&mut two, &mut rt);
    }

    if one.dst == 1 && two.dst == 0 {
        if two.tz_info.is_some() && two.sse.wrapping_sub(one.sse).wrapping_add(dst_corr) < 86400 {
            rt.h -= dst_h_corr;
            rt.i -= dst_m_corr;
        }
    } else if one.dst == 0 && two.dst == 1 {
        if let Some(tz) = &two.tz_info {
            if let Some((_, trans_tt, _)) = tz.offset_info(two.sse) {
                let one_next = one.sse.wrapping_add(86400);
                if !((one_next > trans_tt) && (one_next <= trans_tt.wrapping_add(dst_corr)))
                    && two.sse >= trans_tt
                    && (two.sse.wrapping_sub(one.sse).wrapping_add(dst_corr) % 86400)
                        > two.sse.wrapping_sub(trans_tt)
                {
                    rt.h -= dst_h_corr;
                    rt.i -= dst_m_corr;
                }
            }
        }
    } else if two.sse.wrapping_sub(one.sse) >= 86400 {
        if let Some(tz) = &two.tz_info {
            if let Some((trans_offset, trans_tt, _)) = tz.offset_info(two.sse.wrapping_sub(two.z)) {
                dst_corr = one.z.wrapping_sub(i64::from(trans_offset));
                if two.sse >= trans_tt.wrapping_sub(dst_corr) && two.sse < trans_tt {
                    rt.d -= 1;
                    rt.h = 24;
                }
            }
        }
    }
    rt
}

/// `timelib_diff`: the difference `two - one` as a `DateInterval`.
pub fn diff(one: &Time, two: &Time) -> RelTime {
    if same_tzid(one, two) {
        return diff_with_tzid(one, two);
    }
    let mut one = one.clone();
    let mut two = two.clone();
    let mut rt = RelTime {
        invert: i32::from(sort_old_to_new(&mut one, &mut two)),
        ..RelTime::default()
    };
    rt.y = two.y.wrapping_sub(one.y);
    rt.m = two.m.wrapping_sub(one.m);
    rt.d = two.d.wrapping_sub(one.d);
    rt.h = two.h.wrapping_sub(one.h);
    if one.zone_type != ZoneType::Id {
        rt.h = rt.h.wrapping_add(one.dst);
    }
    if two.zone_type != ZoneType::Id {
        rt.h = rt.h.wrapping_sub(two.dst);
    }
    rt.i = two.i.wrapping_sub(one.i);
    rt.s = two
        .s
        .wrapping_sub(one.s)
        .wrapping_sub(two.z)
        .wrapping_add(one.z);
    rt.us = two.us.wrapping_sub(one.us);
    rt.days = diff_days(&one, &two);
    if rt.invert != 0 {
        do_rel_normalize(&mut one, &mut rt);
    } else {
        do_rel_normalize(&mut two, &mut rt);
    }
    rt
}

fn scaled(interval: &RelTime, bias: i64) -> RelTime {
    RelTime {
        y: interval.y.wrapping_mul(bias),
        m: interval.m.wrapping_mul(bias),
        d: interval.d.wrapping_mul(bias),
        h: interval.h.wrapping_mul(bias),
        i: interval.i.wrapping_mul(bias),
        s: interval.s.wrapping_mul(bias),
        us: interval.us.wrapping_mul(bias),
        ..RelTime::default()
    }
}

/// `timelib_add`: civil addition (used for intervals from `diff()` and
/// `createFromDateString()`).
pub fn add(old: &Time, interval: &RelTime) -> Time {
    let mut t = old.clone();
    if interval.have_weekday_relative || interval.have_special_relative {
        t.relative = interval.clone();
    } else {
        let bias = if interval.invert != 0 { -1 } else { 1 };
        t.relative = scaled(interval, bias);
    }
    t.have_relative = true;
    t.sse_uptodate = false;
    t.update_ts(None);
    t.update_from_sse();
    t.have_relative = false;
    t
}

/// `timelib_sub`: civil subtraction.
pub fn sub(old: &Time, interval: &RelTime) -> Time {
    let mut t = old.clone();
    let bias: i64 = if interval.invert != 0 { -1 } else { 1 };
    t.relative = scaled(interval, -bias);
    t.have_relative = true;
    t.sse_uptodate = false;
    t.update_ts(None);
    t.update_from_sse();
    t.have_relative = false;
    t
}

fn wall(old: &Time, interval: &RelTime, sign: i64) -> Time {
    let mut t = old.clone();
    t.have_relative = true;
    t.sse_uptodate = false;
    if interval.have_weekday_relative || interval.have_special_relative {
        t.relative = interval.clone();
        t.update_ts(None);
        t.update_from_sse();
    } else {
        let bias = if interval.invert != 0 { -1 } else { 1 };
        t.relative = RelTime {
            y: interval.y.wrapping_mul(bias).wrapping_mul(sign),
            m: interval.m.wrapping_mul(bias).wrapping_mul(sign),
            d: interval.d.wrapping_mul(bias).wrapping_mul(sign),
            ..RelTime::default()
        };
        if t.relative.y != 0 || t.relative.m != 0 || t.relative.d != 0 {
            t.update_ts(None);
        }
        if interval.us == 0 {
            let secs = hms_to_seconds(interval.h, interval.i, interval.s);
            t.sse = t
                .sse
                .wrapping_add(bias.wrapping_mul(sign).wrapping_mul(secs));
            t.update_from_sse();
        } else {
            let mut temp = interval.clone();
            do_range_limit(0, 1_000_000, 1_000_000, &mut temp.us, &mut temp.s);
            let secs = hms_to_seconds(temp.h, temp.i, temp.s);
            t.sse = t
                .sse
                .wrapping_add(bias.wrapping_mul(sign).wrapping_mul(secs));
            t.update_from_sse();
            t.us =
                t.us.wrapping_add(temp.us.wrapping_mul(bias).wrapping_mul(sign));
            t.do_normalize();
            t.update_ts(None);
        }
        t.do_normalize();
    }
    if t.zone_type == ZoneType::Id {
        if let Some(tz) = t.tz_info.clone() {
            t.set_timezone(&tz);
        }
    }
    t.have_relative = false;
    t
}

/// `timelib_add_wall`: wall-clock addition (used for intervals constructed
/// from ISO 8601 strings).
pub fn add_wall(old: &Time, interval: &RelTime) -> Time {
    wall(old, interval, 1)
}

/// `timelib_sub_wall`: wall-clock subtraction.
pub fn sub_wall(old: &Time, interval: &RelTime) -> Time {
    wall(old, interval, -1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz;

    fn local(y: i64, m: i64, d: i64, h: i64, i: i64, s: i64, zone: &str) -> Time {
        let tz = tz::get(zone).unwrap();
        let mut t = Time {
            y,
            m,
            d,
            h,
            i,
            s,
            ..Time::default()
        };
        t.update_ts(Some(&tz));
        t.update_from_sse();
        t
    }

    #[test]
    fn simple_diff() {
        let a = local(2000, 1, 1, 0, 0, 0, "UTC");
        let b = local(2001, 3, 4, 5, 6, 7, "UTC");
        let r = diff(&a, &b);
        assert_eq!(
            (r.y, r.m, r.d, r.h, r.i, r.s, r.days, r.invert != 0),
            (1, 2, 3, 5, 6, 7, 428, false)
        );
        let r = diff(&b, &a);
        assert_eq!(r.invert, 1);
        assert_eq!(r.days, 428);
    }

    #[test]
    fn month_end_diff() {
        let a = local(2010, 1, 31, 0, 0, 0, "Europe/Amsterdam");
        let b = local(2010, 3, 1, 0, 0, 0, "Europe/Amsterdam");
        let r = diff(&a, &b);
        assert_eq!((r.y, r.m, r.d, r.days), (0, 0, 29, 29));
    }

    #[test]
    fn add_and_sub() {
        let a = local(2010, 1, 31, 12, 0, 0, "UTC");
        let one_month = RelTime {
            m: 1,
            ..RelTime::default()
        };
        let b = add(&a, &one_month);
        assert_eq!((b.y, b.m, b.d), (2010, 3, 3));
        let c = sub(&b, &one_month);
        assert_eq!((c.y, c.m, c.d), (2010, 2, 3));
        let w = add_wall(
            &a,
            &RelTime {
                h: 25,
                ..RelTime::default()
            },
        );
        assert_eq!((w.m, w.d, w.h), (2, 1, 13));
    }
}
