// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Julian calendar <-> serial day number conversions (port of `julian.c`).

use crate::Ymd;

const JULIAN_SDN_OFFSET: i64 = 32083;
const DAYS_PER_5_MONTHS: i64 = 153;
const DAYS_PER_4_YEARS: i64 = 1461;
const INT_MAX: i64 = i32::MAX as i64;

/// Converts a serial day number to a Julian calendar date (`0/0/0` if invalid).
pub fn sdn_to_julian(sdn: i64) -> Ymd {
    if sdn <= 0 {
        return Ymd::ZERO;
    }
    if !(i64::MIN / 4..=(i64::MAX - JULIAN_SDN_OFFSET * 4 + 1) / 4).contains(&sdn) {
        return Ymd::ZERO;
    }
    let temp = sdn * 4 + (JULIAN_SDN_OFFSET * 4 - 1);
    let yearl = temp / DAYS_PER_4_YEARS;
    let Ok(mut year) = i32::try_from(yearl) else {
        return Ymd::ZERO;
    };
    let day_of_year = (temp % DAYS_PER_4_YEARS) / 4 + 1;

    let temp = day_of_year * 5 - 3;
    let mut month = temp / DAYS_PER_5_MONTHS;
    let day = (temp % DAYS_PER_5_MONTHS) / 5 + 1;

    if month < 10 {
        month += 3;
    } else {
        // C `int` arithmetic: wraps in practice when the year is INT_MAX.
        year = year.wrapping_add(1);
        month -= 9;
    }
    year = year.wrapping_sub(4800);
    if year <= 0 {
        year = year.wrapping_sub(1);
    }
    Ymd::from_wide(i64::from(year), month, day)
}

/// Converts a Julian calendar date to a serial day number (0 = invalid).
pub fn julian_to_sdn(input_year: i64, input_month: i64, input_day: i64) -> i64 {
    if input_year == 0
        || !(-4713..=INT_MAX - 4800).contains(&input_year)
        || input_month <= 0
        || input_month > 12
        || input_day <= 0
        || input_day > 31
    {
        return 0;
    }
    if input_year == -4713 && input_month == 1 && input_day == 1 {
        return 0;
    }
    let mut year = if input_year < 0 {
        input_year + 4801
    } else {
        input_year + 4800
    };
    let month = if input_month > 2 {
        input_month - 3
    } else {
        year -= 1;
        input_month + 9
    };
    (year * DAYS_PER_4_YEARS) / 4 + (month * DAYS_PER_5_MONTHS + 2) / 5 + input_day
        - JULIAN_SDN_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for sdn in (1..3_000_000).step_by(991) {
            let d = sdn_to_julian(sdn);
            assert_eq!(
                julian_to_sdn(d.year.into(), d.month.into(), d.day.into()),
                sdn
            );
        }
    }

    #[test]
    fn limits() {
        assert_eq!(julian_to_sdn(2_147_478_847, 5, 5), 784_368_370_049);
        assert_eq!(julian_to_sdn(6_000_000_000, 5, 5), 0);
        assert_eq!(julian_to_sdn(-4713, 1, 2), 1);
        assert_eq!(sdn_to_julian(1), Ymd::new(-4713, 1, 2));
    }
}
