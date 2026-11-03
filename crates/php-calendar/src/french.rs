//! French republican calendar conversions (port of `french.c`).

use crate::Ymd;

const FRENCH_SDN_OFFSET: i64 = 2_375_474;
const DAYS_PER_4_YEARS: i64 = 1461;
const DAYS_PER_MONTH: i64 = 30;
/// First serial day number representable in the French republican calendar.
pub const FIRST_VALID: i64 = 2_375_840;
/// Last serial day number representable in the French republican calendar.
pub const LAST_VALID: i64 = 2_380_952;

/// French republican month names, indexed 1..=13 (index 0 is empty).
pub const FRENCH_MONTH_NAME: [&str; 14] = [
    "",
    "Vendemiaire",
    "Brumaire",
    "Frimaire",
    "Nivose",
    "Pluviose",
    "Ventose",
    "Germinal",
    "Floreal",
    "Prairial",
    "Messidor",
    "Thermidor",
    "Fructidor",
    "Extra",
];

/// Converts a serial day number to a French republican date (`0/0/0` if invalid).
pub fn sdn_to_french(sdn: i64) -> Ymd {
    if !(FIRST_VALID..=LAST_VALID).contains(&sdn) {
        return Ymd::ZERO;
    }
    let temp = (sdn - FRENCH_SDN_OFFSET) * 4 - 1;
    let year = temp / DAYS_PER_4_YEARS;
    let day_of_year = (temp % DAYS_PER_4_YEARS) / 4;
    Ymd::from_wide(
        year,
        day_of_year / DAYS_PER_MONTH + 1,
        day_of_year % DAYS_PER_MONTH + 1,
    )
}

/// Converts a French republican date to a serial day number (0 = invalid).
pub fn french_to_sdn(year: i64, month: i64, day: i64) -> i64 {
    if !(1..=14).contains(&year) || !(1..=13).contains(&month) || !(1..=30).contains(&day) {
        return 0;
    }
    (year * DAYS_PER_4_YEARS) / 4 + (month - 1) * DAYS_PER_MONTH + day + FRENCH_SDN_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for sdn in FIRST_VALID..=LAST_VALID {
            let d = sdn_to_french(sdn);
            assert_eq!(
                french_to_sdn(d.year.into(), d.month.into(), d.day.into()),
                sdn
            );
        }
        assert_eq!(sdn_to_french(FIRST_VALID - 1), Ymd::ZERO);
        assert_eq!(sdn_to_french(LAST_VALID + 1), Ymd::ZERO);
        assert_eq!(french_to_sdn(1, 1, 1), FIRST_VALID);
    }
}
