// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Gregorian calendar <-> serial day number conversions (port of `gregor.c`).

use crate::Ymd;

const GREGOR_SDN_OFFSET: i64 = 32045;
const DAYS_PER_5_MONTHS: i64 = 153;
const DAYS_PER_4_YEARS: i64 = 1461;
const DAYS_PER_400_YEARS: i64 = 146_097;
const INT_MAX: i64 = i32::MAX as i64;

/// Abbreviated Gregorian/Julian month names, indexed 1..=12 (index 0 is empty).
pub const MONTH_NAME_SHORT: [&str; 13] = [
    "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Full Gregorian/Julian month names, indexed 1..=12 (index 0 is empty).
pub const MONTH_NAME_LONG: [&str; 13] = [
    "",
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Converts a serial day number to a proleptic Gregorian date.
///
/// Out-of-range input yields `0/0/0`, exactly like `SdnToGregorian()`.
pub fn sdn_to_gregorian(sdn: i64) -> Ymd {
    if sdn <= 0 || sdn > (i64::MAX - 4 * GREGOR_SDN_OFFSET) / 4 {
        return Ymd::ZERO;
    }
    let mut temp = (sdn + GREGOR_SDN_OFFSET) * 4 - 1;
    if temp < 0 || temp / DAYS_PER_400_YEARS > INT_MAX {
        return Ymd::ZERO;
    }
    let century = temp / DAYS_PER_400_YEARS;
    temp = ((temp % DAYS_PER_400_YEARS) / 4) * 4 + 3;
    if century > (INT_MAX / 100) - (temp / DAYS_PER_4_YEARS) {
        return Ymd::ZERO;
    }
    let mut year = century * 100 + temp / DAYS_PER_4_YEARS;
    let day_of_year = (temp % DAYS_PER_4_YEARS) / 4 + 1;

    let temp = day_of_year * 5 - 3;
    let mut month = temp / DAYS_PER_5_MONTHS;
    let day = (temp % DAYS_PER_5_MONTHS) / 5 + 1;

    if month < 10 {
        month += 3;
    } else {
        year += 1;
        month -= 9;
    }
    year -= 4800;
    if year <= 0 {
        year -= 1;
    }
    Ymd::from_wide(year, month, day)
}

/// Converts a proleptic Gregorian date to a serial day number (0 = invalid).
pub fn gregorian_to_sdn(input_year: i64, input_month: i64, input_day: i64) -> i64 {
    if input_year == 0
        || !(-4714..=INT_MAX - 4800).contains(&input_year)
        || input_month <= 0
        || input_month > 12
        || input_day <= 0
        || input_day > 31
    {
        return 0;
    }
    if input_year == -4714 && (input_month < 11 || (input_month == 11 && input_day < 25)) {
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
    ((year / 100) * DAYS_PER_400_YEARS) / 4
        + ((year % 100) * DAYS_PER_4_YEARS) / 4
        + (month * DAYS_PER_5_MONTHS + 2) / 5
        + input_day
        - GREGOR_SDN_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for sdn in (1..3_000_000).step_by(997) {
            let d = sdn_to_gregorian(sdn);
            assert_eq!(
                gregorian_to_sdn(d.year.into(), d.month.into(), d.day.into()),
                sdn
            );
        }
    }

    #[test]
    fn known_dates() {
        assert_eq!(gregorian_to_sdn(1970, 1, 1), 2_440_588);
        assert_eq!(gregorian_to_sdn(-4714, 11, 25), 1);
        assert_eq!(gregorian_to_sdn(-4714, 11, 24), 0);
        assert_eq!(gregorian_to_sdn(6_000_000, 5, 5), 2_193_176_185);
        assert_eq!(sdn_to_gregorian(536_838_867), Ymd::new(1_465_102, 10, 18));
        assert_eq!(sdn_to_gregorian(882_858_030), Ymd::new(2_412_471, 1, 31));
        assert_eq!(sdn_to_gregorian(9_223_372_036_854_743_639), Ymd::ZERO);
    }
}
