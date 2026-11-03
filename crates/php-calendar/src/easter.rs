//! `easter_date()` / `easter_days()` (port of `easter.c`).

use crate::{CalError, CAL_EASTER_ALWAYS_GREGORIAN, CAL_EASTER_ALWAYS_JULIAN, CAL_EASTER_ROMAN};

/// Largest year accepted by both functions: `(ZEND_LONG_MAX / 5) * 4`.
pub const MAX_YEAR: i64 = (i64::MAX / 5) * 4;

fn check_year(year: i64, gm: bool) -> Result<(), CalError> {
    let err = |m: String| CalError::Argument {
        arg: 1,
        name: "year",
        message: m,
    };
    if year <= 0 || year > MAX_YEAR {
        return Err(err(format!("must be between 1 and {MAX_YEAR}")));
    }
    if gm && year < 1970 {
        return Err(err("must be a year after 1970 (inclusive)".into()));
    }
    if gm && year > 2_000_000_000 {
        return Err(err("must be a year before 2.000.000.000 (inclusive)".into()));
    }
    Ok(())
}

/// Days after March 21 (C `easter` variable) once the year is validated.
fn compute(year: i64, method: i64) -> i64 {
    let golden = (year % 19) + 1;
    let (dom, mut pfm);
    if (year <= 1582 && method != CAL_EASTER_ALWAYS_GREGORIAN)
        || ((1583..=1752).contains(&year)
            && method != CAL_EASTER_ROMAN
            && method != CAL_EASTER_ALWAYS_GREGORIAN)
        || method == CAL_EASTER_ALWAYS_JULIAN
    {
        // Julian calendar.  The sum wraps for years near MAX_YEAR in C.
        dom = year.wrapping_add(year / 4).wrapping_add(5) % 7;
        pfm = (3 - (11 * golden) - 7) % 30;
    } else {
        dom = year
            .wrapping_add(year / 4)
            .wrapping_sub(year / 100)
            .wrapping_add(year / 400)
            % 7;
        let solar = (year - 1600) / 100 - (year - 1600) / 400;
        let lunar = (((year - 1400) / 100) * 8) / 25;
        pfm = (3 - (11 * golden) + solar - lunar) % 30;
    }
    let dom = if dom < 0 { dom + 7 } else { dom };
    if pfm < 0 {
        pfm += 30;
    }
    if pfm == 29 || (pfm == 28 && golden > 11) {
        pfm -= 1;
    }
    let mut tmp = (4 - pfm - dom) % 7;
    if tmp < 0 {
        tmp += 7;
    }
    pfm + tmp + 1
}

/// `easter_days(?int $year = null, int $mode = CAL_EASTER_DEFAULT): int`.
///
/// The caller substitutes the current local year for `null`.
pub fn easter_days(year: i64, mode: i64) -> Result<i64, CalError> {
    check_year(year, false)?;
    Ok(compute(year, mode))
}

/// Month (1-based) and day of Easter Sunday in `year`, as used by `easter_date()`.
pub fn easter_month_day(year: i64, mode: i64) -> (i64, i64) {
    let easter = compute(year, mode);
    if easter < 11 {
        (3, easter + 21)
    } else {
        (4, easter - 10)
    }
}

/// `easter_date(?int $year = null, int $mode = CAL_EASTER_DEFAULT): int`.
///
/// `mktime(year, month, day)` must return the timestamp of local midnight
/// for that date (PHP calls `mktime()` with `tm_isdst = -1`).
pub fn easter_date(
    year: i64,
    mode: i64,
    mktime: impl FnOnce(i64, i64, i64) -> i64,
) -> Result<i64, CalError> {
    check_year(year, true)?;
    let (month, day) = easter_month_day(year, mode);
    Ok(mktime(year, month, day))
}
