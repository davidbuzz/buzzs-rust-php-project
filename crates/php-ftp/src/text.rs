//! Small C-library text helpers ported with their exact edge cases:
//! C-string truncation, `strtol`, the `sscanf` patterns ext/ftp uses for
//! `PASV`/`EPSV`/`MDTM` replies, and `timegm`.

/// The bytes of `s` up to (not including) the first NUL, as C sees them.
pub(crate) fn cstr(s: &[u8]) -> &[u8] {
    s.iter().position(|&c| c == 0).map_or(s, |p| &s[..p])
}

/// `strtol(s, NULL, 10)` (`ZEND_ATOL`): leading whitespace, optional sign,
/// decimal digits; saturates on overflow; 0 when nothing parses.
pub(crate) fn strtol(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut v: i64 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        let d = i64::from(d - b'0');
        v = if neg {
            v.saturating_mul(10).saturating_sub(d)
        } else {
            v.saturating_mul(10).saturating_add(d)
        };
        i += 1;
    }
    v
}

/// C `isspace` in the "C" locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// A `sscanf` cursor.
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    /// `%lu`: whitespace, optional sign, digits (wrapping like `strtoul`).
    fn ulong(&mut self) -> Option<u64> {
        while self.i < self.s.len() && is_space(self.s[self.i]) {
            self.i += 1;
        }
        let neg = match self.s.get(self.i) {
            Some(b'-') => {
                self.i += 1;
                true
            }
            Some(b'+') => {
                self.i += 1;
                false
            }
            _ => false,
        };
        let start = self.i;
        let mut v: u64 = 0;
        while let Some(d) = self.s.get(self.i).filter(|c| c.is_ascii_digit()) {
            v = v.wrapping_mul(10).wrapping_add(u64::from(d - b'0'));
            self.i += 1;
        }
        (self.i > start).then(|| if neg { v.wrapping_neg() } else { v })
    }

    /// `%Nd`: whitespace (not counted), then at most `width` characters of
    /// optional sign and digits.
    fn int_width(&mut self, width: usize) -> Option<i64> {
        while self.i < self.s.len() && is_space(self.s[self.i]) {
            self.i += 1;
        }
        let mut left = width;
        let neg = match self.s.get(self.i) {
            Some(b'-') if left > 0 => {
                self.i += 1;
                left -= 1;
                true
            }
            Some(b'+') if left > 0 => {
                self.i += 1;
                left -= 1;
                false
            }
            _ => false,
        };
        let start = self.i;
        let mut v: i64 = 0;
        while left > 0 {
            let Some(d) = self.s.get(self.i).filter(|c| c.is_ascii_digit()) else {
                break;
            };
            v = v.wrapping_mul(10).wrapping_add(i64::from(d - b'0'));
            self.i += 1;
            left -= 1;
        }
        (self.i > start).then_some(if neg { -v } else { v })
    }

    /// A literal character.
    fn lit(&mut self, c: u8) -> bool {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
}

/// The `PASV` reply: skips to the first digit and scans
/// `"%lu,%lu,%lu,%lu,%lu,%lu"`; each value is truncated to a byte
/// (`h1,h2,h3,h4,p1,p2`).
pub(crate) fn parse_pasv(reply: &[u8]) -> Option<[u8; 6]> {
    let reply = cstr(reply);
    let start = reply.iter().position(u8::is_ascii_digit)?;
    let mut sc = Scan {
        s: &reply[start..],
        i: 0,
    };
    let mut out = [0u8; 6];
    for (n, slot) in out.iter_mut().enumerate() {
        if n > 0 && !sc.lit(b',') {
            return None;
        }
        // `(unsigned char) b[n]`: keep the low byte.
        *slot = sc.ulong()?.to_le_bytes()[0];
    }
    Some(out)
}

/// The `EPSV` (229) reply: the port between the delimiters of
/// `(|||port|)`.
pub(crate) fn parse_epsv(reply: &[u8]) -> Option<u16> {
    let reply = cstr(reply);
    let open = reply.iter().position(|&c| c == b'(')?;
    let delim = *reply.get(open + 1)?;
    let mut i = open + 1;
    let mut n = 0;
    while i < reply.len() && n < 3 {
        if reply[i] == delim {
            n += 1;
        }
        i += 1;
    }
    let mut sc = Scan { s: reply, i };
    let v = sc.ulong()?;
    if reply.get(sc.i) != Some(&delim) {
        return None;
    }
    // `htons((unsigned short) strtoul(...))`
    Some(u16::from_le_bytes([v.to_le_bytes()[0], v.to_le_bytes()[1]]))
}

/// The `MDTM` (213) reply: skips to the first digit, scans
/// `"%4d%2d%2d%2d%2d%2d"` and converts the UTC broken-down time to a
/// timestamp; `-1` when fewer than six fields parse.
pub(crate) fn parse_mdtm(reply: &[u8]) -> i64 {
    let reply = cstr(reply);
    let Some(start) = reply.iter().position(u8::is_ascii_digit) else {
        return -1;
    };
    let mut sc = Scan {
        s: &reply[start..],
        i: 0,
    };
    let mut f = [0i64; 6];
    for (slot, width) in f.iter_mut().zip([4, 2, 2, 2, 2, 2]) {
        match sc.int_width(width) {
            Some(v) => *slot = v,
            None => return -1,
        }
    }
    timegm(f[0], f[1], f[2], f[3], f[4], f[5])
}

/// `timegm` with `mktime`'s normalisation of out-of-range fields.
fn timegm(year: i64, mon: i64, mday: i64, hour: i64, min: i64, sec: i64) -> i64 {
    // Normalise the month into the year (as mktime does); the other
    // fields simply add up.
    let m0 = mon - 1;
    let year = year + m0.div_euclid(12);
    let mon = m0.rem_euclid(12) + 1;
    days_from_civil(year, mon, 1) + (mday - 1) * 86_400 + hour * 3600 + min * 60 + sec
}

/// Seconds since the epoch of `y-m-d 00:00:00 UTC` (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtol_edges() {
        assert_eq!(strtol(b"5368709120"), 5_368_709_120);
        assert_eq!(strtol(b"  -12abc"), -12);
        assert_eq!(strtol(b"x"), 0);
        assert_eq!(strtol(b"99999999999999999999999"), i64::MAX);
    }

    #[test]
    fn pasv_reply() {
        assert_eq!(
            parse_pasv(b"Entering Passive Mode. (127,0,0,1,195,80)"),
            Some([127, 0, 0, 1, 195, 80])
        );
        assert_eq!(parse_pasv(b"Entering Passive Mode (1,2,3)"), None);
        assert_eq!(parse_pasv(b"no digits"), None);
        assert_eq!(parse_pasv(b"(1, 2, 3, 4, 5, 6)"), Some([1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn epsv_reply() {
        assert_eq!(
            parse_epsv(b"Entering Extended Passive Mode (|||6446|)"),
            Some(6446)
        );
        assert_eq!(
            parse_epsv(b"Entering Extended Passive Mode (|||6446)"),
            None
        );
        assert_eq!(parse_epsv(b"no paren"), None);
    }

    #[test]
    fn mdtm_reply() {
        assert_eq!(parse_mdtm(b"19980615100045.014"), 897_904_845);
        assert_eq!(parse_mdtm(b"19991005213102"), 939_159_062);
        assert_eq!(parse_mdtm(b"1998061510"), -1);
        assert_eq!(parse_mdtm(b"No file"), -1);
        assert_eq!(parse_mdtm(b"19700101000000"), 0);
    }

    #[test]
    fn cstr_truncates() {
        assert_eq!(cstr(b"ab\0cd"), b"ab");
        assert_eq!(cstr(b"abc"), b"abc");
    }
}
