//! Calendar helpers: day of week, day of year, ISO weeks (port of timelib's
//! `dow.c`).

/// Gregorian leap year test (`timelib_is_leap`).
pub fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

const M_TABLE_COMMON: [i64; 13] = [-1, 0, 3, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5];
const M_TABLE_LEAP: [i64; 13] = [-1, 6, 2, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5];
const D_TABLE_COMMON: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const D_TABLE_LEAP: [i64; 13] = [0, 0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];
const ML_TABLE_COMMON: [i64; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const ML_TABLE_LEAP: [i64; 13] = [0, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn table(t: &[i64; 13], m: i64) -> i64 {
    usize::try_from(m).ok().and_then(|i| t.get(i)).copied().unwrap_or(0)
}

fn positive_mod(x: i64, y: i64) -> i64 {
    x.rem_euclid(y)
}

fn day_of_week_ex(y: i64, m: i64, d: i64, iso: bool) -> i64 {
    let c1 = 6 - positive_mod(positive_mod(y, 400) / 100, 4) * 2;
    let y1 = positive_mod(y, 100);
    let m1 = if is_leap(y) {
        table(&M_TABLE_LEAP, m)
    } else {
        table(&M_TABLE_COMMON, m)
    };
    let dow = positive_mod(c1 + y1 + m1 + (y1 / 4) + (d % 7), 7);
    if iso && dow == 0 {
        7
    } else {
        dow
    }
}

/// Day of week, 0 = Sunday .. 6 = Saturday.
pub fn day_of_week(y: i64, m: i64, d: i64) -> i64 {
    day_of_week_ex(y, m, d, false)
}

/// ISO day of week, 1 = Monday .. 7 = Sunday.
pub fn iso_day_of_week(y: i64, m: i64, d: i64) -> i64 {
    day_of_week_ex(y, m, d, true)
}

/// Zero-based day of the year.
pub fn day_of_year(y: i64, m: i64, d: i64) -> i64 {
    let base = if is_leap(y) {
        table(&D_TABLE_LEAP, m)
    } else {
        table(&D_TABLE_COMMON, m)
    };
    base.wrapping_add(d).wrapping_sub(1)
}

/// Number of days in month `m` of year `y`.
pub fn days_in_month(y: i64, m: i64) -> i64 {
    if is_leap(y) {
        table(&ML_TABLE_LEAP, m)
    } else {
        table(&ML_TABLE_COMMON, m)
    }
}

/// ISO-8601 week number and week-numbering year: `(week, year)`.
pub fn isoweek_from_date(y: i64, m: i64, d: i64) -> (i64, i64) {
    let y_leap = i64::from(is_leap(y));
    let prev_y_leap = is_leap(y.wrapping_sub(1));
    let mut doy = day_of_year(y, m, d) + 1;
    if y_leap == 1 && m > 2 {
        doy += 1;
    }
    let mut jan1weekday = day_of_week(y, 1, 1);
    let mut weekday = day_of_week(y, m, d);
    if weekday == 0 {
        weekday = 7;
    }
    if jan1weekday == 0 {
        jan1weekday = 7;
    }
    let mut iw = 0;
    let iy;
    if doy <= (8 - jan1weekday) && jan1weekday > 4 {
        iy = y.wrapping_sub(1);
        iw = if jan1weekday == 5 || (jan1weekday == 6 && prev_y_leap) {
            53
        } else {
            52
        };
    } else {
        iy = y;
    }
    if iy == y {
        let i = if y_leap == 1 { 366 } else { 365 };
        if (i - (doy - y_leap)) < (4 - weekday) {
            return (1, y.wrapping_add(1));
        }
        let j = doy + (7 - weekday) + (jan1weekday - 1);
        iw = j / 7;
        if jan1weekday > 4 {
            iw -= 1;
        }
    }
    (iw, iy)
}

/// ISO year, week and day: `(iso_year, iso_week, iso_day)`.
pub fn isodate_from_date(y: i64, m: i64, d: i64) -> (i64, i64, i64) {
    let (iw, iy) = isoweek_from_date(y, m, d);
    (iy, iw, iso_day_of_week(y, m, d))
}

/// Zero-based day-of-year offset (relative to January 1st of `iy`) of ISO
/// week `iw`, day `id`. May be negative or exceed the year length.
pub fn daynr_from_weeknr(iy: i64, iw: i64, id: i64) -> i64 {
    let dow = day_of_week(iy, 1, 1);
    let day = 0 - if dow > 4 { dow - 7 } else { dow };
    day.wrapping_add(iw.wrapping_sub(1).wrapping_mul(7)).wrapping_add(id)
}

/// Calendar date `(y, m, d)` of ISO year/week/day.
pub fn date_from_isodate(iy: i64, iw: i64, id: i64) -> (i64, i64, i64) {
    let mut daynr = daynr_from_weeknr(iy, iw, id) + 1;
    let mut y = iy;
    let mut leap = is_leap(y);
    while daynr <= 0 {
        y -= 1;
        leap = is_leap(y);
        daynr += if leap { 366 } else { 365 };
    }
    while daynr > if leap { 366 } else { 365 } {
        daynr -= if leap { 366 } else { 365 };
        y += 1;
        leap = is_leap(y);
    }
    let t = if leap { &ML_TABLE_LEAP } else { &ML_TABLE_COMMON };
    let mut m = 1;
    while m < 12 && daynr > table(t, m) {
        daynr -= table(t, m);
        m += 1;
    }
    (y, m, daynr)
}

/// `timelib_valid_time`: 00:00:00 .. 23:59:59.
pub fn valid_time(h: i64, i: i64, s: i64) -> bool {
    (0..=23).contains(&h) && (0..=59).contains(&i) && (0..=59).contains(&s)
}

/// `timelib_valid_date`: month 1..12 and day within the month.
pub fn valid_date(y: i64, m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

/// `checkdate($month, $day, $year)`.
pub fn checkdate(m: i64, d: i64, y: i64) -> bool {
    (1..=32767).contains(&y) && valid_date(y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dow() {
        assert_eq!(day_of_week(1978, 12, 22), 5);
        assert_eq!(day_of_week(2005, 2, 19), 6);
        assert_eq!(iso_day_of_week(2023, 1, 1), 7);
        assert_eq!(day_of_week(-1, 1, 1), day_of_week(399, 1, 1));
    }

    #[test]
    fn iso_weeks() {
        assert_eq!(isoweek_from_date(2008, 12, 29), (1, 2009));
        assert_eq!(isoweek_from_date(2010, 1, 3), (53, 2009));
        assert_eq!(isoweek_from_date(2005, 1, 1), (53, 2004));
        assert_eq!(isoweek_from_date(2024, 6, 15), (24, 2024));
        assert_eq!(date_from_isodate(2009, 1, 1), (2008, 12, 29));
        assert_eq!(date_from_isodate(2009, 53, 7), (2010, 1, 3));
    }

    #[test]
    fn validity() {
        assert!(valid_date(2000, 2, 29));
        assert!(!valid_date(1900, 2, 29));
        assert!(!valid_time(24, 0, 0));
        assert!(checkdate(2, 29, 2004));
        assert!(!checkdate(2, 29, 2003));
        assert!(!checkdate(1, 1, 0));
        assert!(!checkdate(1, 1, 32768));
    }

    #[test]
    fn days() {
        assert_eq!(day_of_year(2004, 3, 1), 60);
        assert_eq!(days_in_month(2004, 2), 29);
        assert_eq!(days_in_month(2003, 2), 28);
    }
}
