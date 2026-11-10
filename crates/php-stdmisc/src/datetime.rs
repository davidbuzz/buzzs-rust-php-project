//! `strptime()` (ref: ext/standard/datetime.c), deprecated since PHP 8.2.
//!
//! The parsing is libc's `strptime(3)`, as in PHP; this module shapes the
//! result.

/// The fields `strptime()` returns, in PHP's key order, and the unparsed
/// rest of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTime {
    /// `(name, value)` for `tm_sec`, `tm_min`, `tm_hour`, `tm_mday`,
    /// `tm_mon`, `tm_year`, `tm_wday`, `tm_yday`.
    pub fields: [(&'static str, i64); 8],
    /// The part of the input after the last parsed character (`unparsed`).
    pub unparsed: Vec<u8>,
}

/// `PHP_FUNCTION(strptime)`: `None` when the input does not match the
/// format (PHP returns `false`).
#[must_use]
pub fn strptime(timestamp: &[u8], format: &[u8]) -> Option<ParsedTime> {
    let (tm, offset) = crate::sys::strptime(timestamp, format)?;
    let input: &[u8] = timestamp
        .iter()
        .position(|&c| c == 0)
        .map_or(timestamp, |i| &timestamp[..i]);
    Some(ParsedTime {
        fields: [
            ("tm_sec", i64::from(tm.tm_sec)),
            ("tm_min", i64::from(tm.tm_min)),
            ("tm_hour", i64::from(tm.tm_hour)),
            ("tm_mday", i64::from(tm.tm_mday)),
            ("tm_mon", i64::from(tm.tm_mon)),
            ("tm_year", i64::from(tm.tm_year)),
            ("tm_wday", i64::from(tm.tm_wday)),
            ("tm_yday", i64::from(tm.tm_yday)),
        ],
        unparsed: input.get(offset..).unwrap_or_default().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(p: &ParsedTime) -> Vec<i64> {
        p.fields.iter().map(|(_, v)| *v).collect()
    }

    // Expectations from /usr/bin/php (glibc strptime, C locale).
    #[test]
    fn parses_with_unparsed_rest() {
        let p = strptime(b"10:00:00 AM July 02 1963 GMT", b"%H:%M:%S %p %B %d %Y").unwrap();
        assert_eq!(values(&p), [0, 0, 10, 2, 6, 63, 2, 182]);
        assert_eq!(p.unparsed, b" GMT");
        let names: Vec<&str> = p.fields.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            ["tm_sec", "tm_min", "tm_hour", "tm_mday", "tm_mon", "tm_year", "tm_wday", "tm_yday"]
        );
    }

    #[test]
    fn two_digit_years_and_partial_dates() {
        let p = strptime(b"10:00:00 07/02/63", b"%H:%M:%S %m/%d/%y").unwrap();
        assert_eq!(values(&p), [0, 0, 10, 2, 6, 163, 1, 182]);
        assert!(p.unparsed.is_empty());
        let p = strptime(b"Tuesday July  2 10:00", b"%A %B %e %R").unwrap();
        assert_eq!(values(&p), [0, 0, 10, 2, 6, 0, 2, 182]);
    }

    #[test]
    fn mismatch_and_nul() {
        assert!(strptime(b"nope", b"%Y").is_none());
        let p = strptime(b"2020\0junk", b"%Y").unwrap();
        assert_eq!(p.fields[5].1, 120);
        assert!(p.unparsed.is_empty());
    }
}
