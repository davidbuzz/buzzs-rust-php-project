//! Jewish calendar conversions (port of `jewish.c`).
//!
//! The C implementation mixes `int` and `zend_long` arithmetic; where an `int`
//! result is stored the value is truncated with wrapping semantics so that
//! out-of-range inputs produce the same numbers as PHP on LP64 platforms.

use crate::Ymd;

const HALAKIM_PER_HOUR: i64 = 1080;
const HALAKIM_PER_DAY: i64 = 25920;
const HALAKIM_PER_LUNAR_CYCLE: i64 = (29 * HALAKIM_PER_DAY) + 13753;
const HALAKIM_PER_METONIC_CYCLE: i64 = HALAKIM_PER_LUNAR_CYCLE * (12 * 19 + 7);

const JEWISH_SDN_OFFSET: i64 = 347_997;
/// Largest serial day number accepted by [`sdn_to_jewish`] (12/13/887605).
pub const JEWISH_SDN_MAX: i64 = 324_542_846;
const NEW_MOON_OF_CREATION: i64 = 31524;

const SUNDAY: i64 = 0;
const MONDAY: i64 = 1;
const TUESDAY: i64 = 2;
const WEDNESDAY: i64 = 3;
const FRIDAY: i64 = 5;

const NOON: i64 = 18 * HALAKIM_PER_HOUR;
const AM3_11_20: i64 = (9 * HALAKIM_PER_HOUR) + 204;
const AM9_32_43: i64 = (15 * HALAKIM_PER_HOUR) + 589;

/// Number of months in each year of the 19-year metonic cycle.
pub const MONTHS_PER_YEAR: [i64; 19] = [
    12, 12, 13, 12, 12, 13, 12, 13, 12, 12, 13, 12, 12, 13, 12, 12, 13, 12, 13,
];

const YEAR_OFFSET: [i64; 19] = [
    0, 12, 24, 37, 49, 61, 74, 86, 99, 111, 123, 136, 148, 160, 173, 185, 197, 210, 222,
];

/// Month names for leap (13-month) years.
pub const JEWISH_MONTH_NAME_LEAP: [&str; 14] = [
    "", "Tishri", "Heshvan", "Kislev", "Tevet", "Shevat", "Adar I", "Adar II", "Nisan", "Iyyar",
    "Sivan", "Tammuz", "Av", "Elul",
];

/// Month names for regular (12-month) years.
pub const JEWISH_MONTH_NAME: [&str; 14] = [
    "", "Tishri", "Heshvan", "Kislev", "Tevet", "Shevat", "", "Adar", "Nisan", "Iyyar", "Sivan",
    "Tammuz", "Av", "Elul",
];

/// ISO-8859-8 Hebrew month names for leap years.
pub const JEWISH_MONTH_HEB_NAME_LEAP: [&[u8]; 14] = [
    b"",
    b"\xFA\xF9\xF8\xE9",
    b"\xE7\xF9\xE5\xEF",
    b"\xEB\xF1\xEC\xE5",
    b"\xE8\xE1\xFA",
    b"\xF9\xE1\xE8",
    b"\xE0\xE3\xF8 \xE0'",
    b"\xE0\xE3\xF8 \xE1'",
    b"\xF0\xE9\xF1\xEF",
    b"\xE0\xE9\xE9\xF8",
    b"\xF1\xE9\xE5\xEF",
    b"\xFA\xEE\xE5\xE6",
    b"\xE0\xE1",
    b"\xE0\xEC\xE5\xEC",
];

/// ISO-8859-8 Hebrew month names for regular years.
pub const JEWISH_MONTH_HEB_NAME: [&[u8]; 14] = [
    b"",
    b"\xFA\xF9\xF8\xE9",
    b"\xE7\xF9\xE5\xEF",
    b"\xEB\xF1\xEC\xE5",
    b"\xE8\xE1\xFA",
    b"\xF9\xE1\xE8",
    b"",
    b"\xE0\xE3\xF8",
    b"\xF0\xE9\xF1\xEF",
    b"\xE0\xE9\xE9\xF8",
    b"\xF1\xE9\xE5\xEF",
    b"\xFA\xEE\xE5\xE6",
    b"\xE0\xE1",
    b"\xE0\xEC\xE5\xEC",
];

/// Whether the (positive) Jewish `year` is a leap year with 13 months.
pub fn is_leap_year(year: i64) -> bool {
    MONTHS_PER_YEAR[metonic_index(year - 1)] == 13
}

/// `x % 19` for the C expressions `monthsPerYear[(year - 1) % 19]`
/// (callers guarantee a non-negative argument; `rem_euclid` keeps it in range).
fn metonic_index(x: i64) -> usize {
    usize::try_from(x.rem_euclid(19)).unwrap_or(0)
}

/// English month names for the given Jewish year.
pub fn month_names(year: i64) -> &'static [&'static str; 14] {
    if is_leap_year(year) {
        &JEWISH_MONTH_NAME_LEAP
    } else {
        &JEWISH_MONTH_NAME
    }
}

/// Hebrew (ISO-8859-8) month names for the given Jewish year.
pub fn heb_month_names(year: i64) -> &'static [&'static [u8]; 14] {
    if is_leap_year(year) {
        &JEWISH_MONTH_HEB_NAME_LEAP
    } else {
        &JEWISH_MONTH_HEB_NAME
    }
}

/// Truncates a C `zend_long` to `int` (two's complement wrap).
#[allow(clippy::cast_possible_truncation)]
fn to_int(v: i64) -> i32 {
    v as i32
}

fn tishri1(metonic_year: i64, molad_day: i64, molad_halakim: i64) -> i64 {
    let mut tishri1 = molad_day;
    let mut dow = tishri1 % 7;
    let leap_year = matches!(metonic_year, 2 | 5 | 7 | 10 | 13 | 16 | 18);
    let last_was_leap_year = matches!(metonic_year, 3 | 6 | 8 | 11 | 14 | 17 | 0);

    if molad_halakim >= NOON
        || (!leap_year && dow == TUESDAY && molad_halakim >= AM3_11_20)
        || (last_was_leap_year && dow == MONDAY && molad_halakim >= AM9_32_43)
    {
        tishri1 += 1;
        dow += 1;
        if dow == 7 {
            dow = 0;
        }
    }
    if dow == WEDNESDAY || dow == FRIDAY || dow == SUNDAY {
        tishri1 += 1;
    }
    tishri1
}

/// Returns `(molad_day, molad_halakim)` of the molad starting `metonic_cycle`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn molad_of_metonic_cycle(metonic_cycle: i64) -> (i64, i64) {
    const LO: i64 = HALAKIM_PER_METONIC_CYCLE & 0xFFFF;
    const HI: i64 = (HALAKIM_PER_METONIC_CYCLE >> 16) & 0xFFFF;
    let chk = metonic_cycle;
    if chk > (i64::MAX - NEW_MOON_OF_CREATION) / LO {
        return (0, 0);
    }
    // The C code uses unsigned `zend_ulong` registers from here on.
    let mut r1 = (NEW_MOON_OF_CREATION as u64).wrapping_add(chk.wrapping_mul(LO) as u64);
    if (chk as u64) > ((i64::MAX as u64).wrapping_sub(r1 >> 16)) / (HI as u64) {
        return (0, 0);
    }
    let mut r2 = r1 >> 16;
    r2 = r2.wrapping_add(chk.wrapping_mul(HI) as u64);
    let hpd = HALAKIM_PER_DAY as u64;
    let d2 = r2 / hpd;
    r2 -= d2 * hpd;
    r1 = (r2 << 16) | (r1 & 0xFFFF);
    let d1 = r1 / hpd;
    r1 -= d1 * hpd;
    (((d2 << 16) | d1) as i64, r1 as i64)
}

struct Molad {
    metonic_cycle: i64,
    metonic_year: i64,
    day: i64,
    halakim: i64,
}

fn find_tishri_molad(input_day: i64) -> Molad {
    // `metonicCycle` is an `int` in C.
    let mut metonic_cycle = i64::from(to_int((input_day + 310) / 6940));
    let (mut molad_day, mut molad_halakim) = molad_of_metonic_cycle(metonic_cycle);
    while molad_day < input_day - 6940 + 310 {
        metonic_cycle += 1;
        molad_halakim += HALAKIM_PER_METONIC_CYCLE;
        molad_day += molad_halakim / HALAKIM_PER_DAY;
        molad_halakim %= HALAKIM_PER_DAY;
    }
    let mut metonic_year = 0;
    while metonic_year < 18 {
        if molad_day > input_day - 74 {
            break;
        }
        molad_halakim += HALAKIM_PER_LUNAR_CYCLE * MONTHS_PER_YEAR[metonic_index(metonic_year)];
        molad_day += molad_halakim / HALAKIM_PER_DAY;
        molad_halakim %= HALAKIM_PER_DAY;
        metonic_year += 1;
    }
    Molad {
        metonic_cycle,
        metonic_year,
        day: molad_day,
        halakim: molad_halakim,
    }
}

/// Returns `(metonic_year, molad_day, molad_halakim, tishri1 as C int)`.
fn find_start_of_year(year: i32) -> (i64, i64, i64, i32) {
    let metonic_cycle = i64::from((year - 1) / 19);
    let metonic_year = i64::from((year - 1) % 19);
    let (mut molad_day, mut molad_halakim) = molad_of_metonic_cycle(metonic_cycle);
    molad_halakim += HALAKIM_PER_LUNAR_CYCLE * YEAR_OFFSET[metonic_index(metonic_year)];
    molad_day += molad_halakim / HALAKIM_PER_DAY;
    molad_halakim %= HALAKIM_PER_DAY;
    let t = to_int(tishri1(metonic_year, molad_day, molad_halakim));
    (metonic_year, molad_day, molad_halakim, t)
}

/// Converts a serial day number to a Jewish date (`0/0/0` when out of range).
pub fn sdn_to_jewish(sdn: i64) -> Ymd {
    if sdn <= JEWISH_SDN_OFFSET || sdn > JEWISH_SDN_MAX {
        return Ymd::ZERO;
    }
    let input_day = sdn - JEWISH_SDN_OFFSET;

    let m = find_tishri_molad(input_day);
    let (metonic_cycle, metonic_year) = (m.metonic_cycle, m.metonic_year);
    let (mut day, mut halakim) = (m.day, m.halakim);
    let mut tishri1_v = tishri1(metonic_year, day, halakim);
    let tishri1_after;
    let year;

    if input_day >= tishri1_v {
        year = metonic_cycle * 19 + metonic_year + 1;
        if input_day < tishri1_v + 59 {
            if input_day < tishri1_v + 30 {
                return Ymd::from_wide(year, 1, input_day - tishri1_v + 1);
            }
            return Ymd::from_wide(year, 2, input_day - tishri1_v - 29);
        }
        halakim += HALAKIM_PER_LUNAR_CYCLE * MONTHS_PER_YEAR[metonic_index(metonic_year)];
        day += halakim / HALAKIM_PER_DAY;
        halakim %= HALAKIM_PER_DAY;
        tishri1_after = tishri1((metonic_year + 1) % 19, day, halakim);
    } else {
        year = metonic_cycle * 19 + metonic_year;
        if input_day >= tishri1_v - 177 {
            let (month, d) = if input_day > tishri1_v - 30 {
                (13, input_day - tishri1_v + 30)
            } else if input_day > tishri1_v - 60 {
                (12, input_day - tishri1_v + 60)
            } else if input_day > tishri1_v - 89 {
                (11, input_day - tishri1_v + 89)
            } else if input_day > tishri1_v - 119 {
                (10, input_day - tishri1_v + 119)
            } else if input_day > tishri1_v - 148 {
                (9, input_day - tishri1_v + 148)
            } else {
                (8, input_day - tishri1_v + 178)
            };
            return Ymd::from_wide(year, month, d);
        }
        let mut month;
        let mut d;
        if MONTHS_PER_YEAR[metonic_index(year - 1)] == 13 {
            month = 7;
            d = input_day - tishri1_v + 207;
            if d > 0 {
                return Ymd::from_wide(year, month, d);
            }
            month -= 1;
            d += 30;
            if d > 0 {
                return Ymd::from_wide(year, month, d);
            }
            month -= 1;
            d += 30;
        } else {
            month = 7;
            d = input_day - tishri1_v + 207;
            if d > 0 {
                return Ymd::from_wide(year, month, d);
            }
            month -= 2;
            d += 30;
        }
        if d > 0 {
            return Ymd::from_wide(year, month, d);
        }
        month -= 1;
        d += 29;
        if d > 0 {
            return Ymd::from_wide(year, month, d);
        }
        tishri1_after = tishri1_v;
        let m = find_tishri_molad(day - 365);
        tishri1_v = tishri1(m.metonic_year, m.day, m.halakim);
    }

    let year_length = tishri1_after - tishri1_v;
    let mut day = input_day - tishri1_v - 29;
    if year_length == 355 || year_length == 385 {
        if day <= 30 {
            return Ymd::from_wide(year, 2, day);
        }
        day -= 30;
    } else {
        if day <= 29 {
            return Ymd::from_wide(year, 2, day);
        }
        day -= 29;
    }
    Ymd::from_wide(year, 3, day)
}

/// Converts a Jewish date to a serial day number (0 when detected invalid).
///
/// As in PHP, some invalid dates still produce a positive value.
pub fn jewish_to_sdn(year: i64, month: i64, day: i64) -> i64 {
    if year <= 0 || year >= i64::from(i32::MAX) - 1 || day <= 0 || day > 30 {
        return 0;
    }
    let int_year = to_int(year);
    let sdn = match month {
        1 | 2 => {
            let (_, _, _, t1) = find_start_of_year(int_year);
            if month == 1 {
                i64::from(t1) + day - 1
            } else {
                i64::from(t1) + day + 29
            }
        }
        3 => {
            let (metonic_year, mut molad_day, mut molad_halakim, t1) = find_start_of_year(int_year);
            molad_halakim += HALAKIM_PER_LUNAR_CYCLE * MONTHS_PER_YEAR[metonic_index(metonic_year)];
            molad_day += molad_halakim / HALAKIM_PER_DAY;
            molad_halakim %= HALAKIM_PER_DAY;
            let t1_after = to_int(tishri1((metonic_year + 1) % 19, molad_day, molad_halakim));
            let year_length = t1_after.wrapping_sub(t1);
            if year_length == 355 || year_length == 385 {
                i64::from(t1) + day + 59
            } else {
                i64::from(t1) + day + 58
            }
        }
        4..=6 => {
            let (_, _, _, t1_after) = find_start_of_year(int_year + 1);
            let length_of_adar_i_and_ii =
                if MONTHS_PER_YEAR[metonic_index(i64::from(int_year) - 1)] == 12 {
                    29
                } else {
                    59
                };
            let base = i64::from(t1_after) + day - length_of_adar_i_and_ii;
            match month {
                4 => base - 237,
                5 => base - 208,
                _ => base - 178,
            }
        }
        _ => {
            let (_, _, _, t1_after) = find_start_of_year(int_year + 1);
            let t = i64::from(t1_after) + day;
            match month {
                7 => t - 207,
                8 => t - 178,
                9 => t - 148,
                10 => t - 119,
                11 => t - 89,
                12 => t - 60,
                13 => t - 30,
                _ => return 0,
            }
        }
    };
    sdn + JEWISH_SDN_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for sdn in (JEWISH_SDN_OFFSET + 1..3_000_000).step_by(37) {
            let d = sdn_to_jewish(sdn);
            assert_eq!(
                jewish_to_sdn(d.year.into(), d.month.into(), d.day.into()),
                sdn,
                "{d:?}"
            );
        }
    }

    #[test]
    fn gh21557() {
        let expected = [
            2_537_279, 2_537_633, 2_538_016, 2_538_371, 2_538_725, 2_539_110, 2_539_463, 2_539_818,
            2_540_202, 2_540_557,
        ];
        for (year, e) in (5995..).zip(expected) {
            assert_eq!(jewish_to_sdn(year, 1, 1), e);
        }
    }

    #[test]
    fn limits() {
        assert_eq!(sdn_to_jewish(JEWISH_SDN_MAX), Ymd::new(887_605, 12, 13));
        assert_eq!(sdn_to_jewish(JEWISH_SDN_MAX + 1), Ymd::ZERO);
        assert_eq!(sdn_to_jewish(38_245_310), Ymd::new(103_759, 2, 22));
        assert_eq!(sdn_to_jewish(JEWISH_SDN_OFFSET), Ymd::ZERO);
        // Must not panic.
        jewish_to_sdn(1_218_182_888, 1_218_182_888, 1);
        jewish_to_sdn(2_147_483_647, 10, 6);
        jewish_to_sdn(2_147_483_645, 3, 6);
    }
}
