// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Sunrise, sunset and twilight times (port of timelib's `astro.c`, after
//! Paul Schlyter's public-domain `sunriset.c`).

use crate::time::{Time, ZoneType};

#[allow(clippy::excessive_precision, clippy::approx_constant)]
const PI: f64 = 3.141_592_653_589_793_238_4;
const RADEG: f64 = 180.0 / PI;
const DEGRAD: f64 = PI / 180.0;
const INV360: f64 = 1.0 / 360.0;

fn sind(x: f64) -> f64 {
    (x * DEGRAD).sin()
}
fn cosd(x: f64) -> f64 {
    (x * DEGRAD).cos()
}
fn atan2d(y: f64, x: f64) -> f64 {
    RADEG * y.atan2(x)
}
fn acosd(x: f64) -> f64 {
    RADEG * x.acos()
}

fn revolution(x: f64) -> f64 {
    x - 360.0 * (x * INV360).floor()
}

fn rev180(x: f64) -> f64 {
    x - 360.0 * (x * INV360 + 0.5).floor()
}

#[allow(clippy::unreadable_literal)]
fn gmst0(d: f64) -> f64 {
    revolution((180.0 + 356.0470 + 282.9404) + (0.9856002585 + 4.70935E-5) * d)
}

#[allow(clippy::unreadable_literal, clippy::many_single_char_names)]
fn sunpos(d: f64) -> (f64, f64) {
    let m = revolution(356.0470 + 0.9856002585 * d);
    let w = 282.9404 + 4.70935E-5 * d;
    let e = 0.016709 - 1.151E-9 * d;
    let ea = m + e * RADEG * sind(m) * (1.0 + e * cosd(m));
    let x = cosd(ea) - e;
    let y = (1.0 - e * e).sqrt() * sind(ea);
    let r = (x * x + y * y).sqrt();
    let v = atan2d(y, x);
    let mut lon = v + w;
    if lon >= 360.0 {
        lon -= 360.0;
    }
    (lon, r)
}

#[allow(clippy::unreadable_literal)]
fn sun_ra_dec(d: f64) -> (f64, f64, f64) {
    let (lon, r) = sunpos(d);
    let x = r * cosd(lon);
    let mut y = r * sind(lon);
    let obl_ecl = 23.4393 - 3.563E-7 * d;
    let z = y * sind(obl_ecl);
    y *= cosd(obl_ecl);
    let ra = atan2d(y, x);
    let dec = atan2d(z, (x * x + y * y).sqrt());
    (ra, dec, r)
}

/// Julian day of a timestamp.
#[allow(clippy::cast_precision_loss)]
pub fn ts_to_julianday(ts: i64) -> f64 {
    let mut tmp = ts as f64;
    tmp /= 86400.0;
    tmp += 2_440_587.5;
    tmp
}

/// Days since J2000.0 of a timestamp.
pub fn ts_to_j2000(ts: i64) -> f64 {
    ts_to_julianday(ts) - 2_451_545.0
}

/// Result of [`rise_set_altitude`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiseSet {
    /// 0 normally, -1 if the sun stays below `altit`, +1 if always above.
    pub rc: i32,
    /// Rise time in decimal UT hours (only meaningful when `rc == 0`).
    pub h_rise: f64,
    /// Set time in decimal UT hours (only meaningful when `rc == 0`).
    pub h_set: f64,
    /// Rise timestamp.
    pub ts_rise: i64,
    /// Set timestamp.
    pub ts_set: i64,
    /// Transit (solar noon) timestamp.
    pub ts_transit: i64,
}

/// `timelib_astro_rise_set_altitude`: rise/set times for the local day of
/// `t_loc` at the given altitude (degrees; `upper_limb` corrects for the
/// sun's radius). `t_loc` is normalised to local noon as a side effect,
/// keeping its original `sse`, like timelib does.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::similar_names
)]
pub fn rise_set_altitude(
    t_loc: &mut Time,
    lon: f64,
    lat: f64,
    mut altit: f64,
    upper_limb: bool,
) -> RiseSet {
    let old_sse = t_loc.sse;
    t_loc.h = 12;
    t_loc.i = 0;
    t_loc.s = 0;
    t_loc.update_ts(None);

    let mut t_utc = Time {
        y: t_loc.y,
        m: t_loc.m,
        d: t_loc.d,
        zone_type: ZoneType::None,
        ..Time::default()
    };
    t_utc.update_ts(None);

    let timestamp = t_utc.sse;
    let d = ts_to_j2000(timestamp) + 2.0 - lon / 360.0;
    let sidtime = revolution(gmst0(d) + 180.0 + lon);
    let (s_ra, sdec, sr) = sun_ra_dec(d);
    let tsouth = 12.0 - rev180(sidtime - s_ra) / 15.0;
    let sradius = 0.2666 / sr;
    if upper_limb {
        altit -= sradius;
    }

    let utc = t_utc.sse as f64;
    let cost = (sind(altit) - sind(lat) * sind(sdec)) / (cosd(lat) * cosd(sdec));
    let ts_transit = (utc + tsouth * 3600.0) as i64;
    let mut out = RiseSet {
        rc: 0,
        h_rise: 0.0,
        h_set: 0.0,
        ts_rise: 0,
        ts_set: 0,
        ts_transit,
    };
    if cost >= 1.0 {
        out.rc = -1;
        out.ts_rise = (utc + tsouth * 3600.0) as i64;
        out.ts_set = out.ts_rise;
    } else if cost <= -1.0 {
        out.rc = 1;
        out.ts_rise = t_loc.sse - 12 * 3600;
        out.ts_set = t_loc.sse + 12 * 3600;
    } else {
        let t = acosd(cost) / 15.0;
        out.ts_rise = ((tsouth - t) * 3600.0 + utc) as i64;
        out.ts_set = ((tsouth + t) * 3600.0 + utc) as i64;
        out.h_rise = tsouth - t;
        out.h_set = tsouth + t;
    }
    t_loc.sse = old_sse;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz;

    #[test]
    fn julian() {
        assert!((ts_to_julianday(0) - 2_440_587.5).abs() < 1e-9);
        assert!((ts_to_j2000(946_728_000)).abs() < 1e-9);
    }

    #[test]
    fn oslo_summer() {
        let tz = tz::get("Europe/Oslo").unwrap();
        let mut t = Time::from_timestamp_in(1_151_712_000, &tz); // 2006-07-01
        let r = rise_set_altitude(&mut t, 10.75, 59.91, -35.0 / 60.0, true);
        assert_eq!(r.rc, 0);
        assert!(r.ts_rise < r.ts_transit && r.ts_transit < r.ts_set);
        // Far north in summer: always above the civil twilight altitude.
        let mut t = Time::from_timestamp_in(1_151_712_000, &tz);
        let r = rise_set_altitude(&mut t, 18.95, 69.65, -6.0, false);
        assert_eq!(r.rc, 1);
    }
}
