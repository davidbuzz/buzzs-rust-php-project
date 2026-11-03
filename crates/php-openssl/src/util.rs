//! Small helpers shared by the function families: PHP's base64 codec,
//! `bin2hex`, path expansion and ASN.1 time conversion.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `php_base64_encode`.
pub fn base64_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(B64[usize::from(b[0] >> 2)]);
        out.push(B64[usize::from(((b[0] & 0x03) << 4) | (b[1] >> 4))]);
        out.push(if chunk.len() > 1 {
            B64[usize::from(((b[1] & 0x0f) << 2) | (b[2] >> 6))]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            B64[usize::from(b[2] & 0x3f)]
        } else {
            b'='
        });
    }
    out
}

/// Non-strict `php_base64_decode`: skips unknown characters and padding,
/// never fails.
pub fn base64_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let mut i = 0usize;
    for &c in data {
        if c == b'=' {
            continue;
        }
        let Some(v) = B64.iter().position(|&x| x == c) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let v = v as u8;
        match i % 4 {
            0 => out.push(v << 2),
            1 => {
                if let Some(last) = out.last_mut() {
                    *last |= v >> 4;
                }
                out.push((v & 0x0f) << 4);
            }
            2 => {
                if let Some(last) = out.last_mut() {
                    *last |= v >> 2;
                }
                out.push((v & 0x03) << 6);
            }
            _ => {
                if let Some(last) = out.last_mut() {
                    *last |= v;
                }
            }
        }
        i += 1;
    }
    // PHP's result length is the number of completed bytes (`j`).
    let full = i / 4 * 3
        + match i % 4 {
            2 => 1,
            3 => 2,
            _ => 0,
        };
    out.truncate(full);
    out
}

/// Lower-case hex (`zend_bin2hex`).
pub fn bin2hex(data: &[u8]) -> Vec<u8> {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in data {
        out.push(H[usize::from(b >> 4)]);
        out.push(H[usize::from(b & 15)]);
    }
    out
}

/// `expand_filepath`: makes a path absolute against the current working
/// directory and collapses `.`/`..`/duplicate separators (no symlink
/// resolution). Returns `None` when the CWD is unavailable.
pub fn expand_filepath(path: &[u8]) -> Option<Vec<u8>> {
    let mut full: Vec<u8> = if path.first() == Some(&b'/') {
        path.to_vec()
    } else {
        let cwd = std::env::current_dir().ok()?;
        let mut v = cwd.to_string_lossy().into_owned().into_bytes();
        v.push(b'/');
        v.extend_from_slice(path);
        v
    };
    let mut parts: Vec<&[u8]> = Vec::new();
    for seg in full.split(|&c| c == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = Vec::with_capacity(full.len());
    for p in &parts {
        out.push(b'/');
        out.extend_from_slice(p);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    full.clear();
    Some(out)
}

/// Days before each month in a non-leap year.
const CUM_DAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Seconds since the epoch for a broken-down UTC time (normalising
/// out-of-range fields the way `mktime` + `tm_gmtoff` correction does).
pub fn timegm(year: i64, mon0: i64, mday: i64, hour: i64, min: i64, sec: i64) -> i64 {
    let y = year + mon0.div_euclid(12);
    let m = mon0.rem_euclid(12);
    let mut days = (y - 1970) * 365 + ((y - 1) / 4 - 1969 / 4) - ((y - 1) / 100 - 1969 / 100)
        + ((y - 1) / 400 - 1969 / 400);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    {
        days += CUM_DAYS[m as usize];
    }
    if m > 1 && is_leap(y) {
        days += 1;
    }
    days += mday - 1;
    days * 86400 + hour * 3600 + min * 60 + sec
}

/// C `atoi` on a byte slice (leading whitespace, optional sign, digits).
pub fn atoi(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut v: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add(i64::from(s[i] - b'0'));
        i += 1;
    }
    if neg {
        -v
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_roundtrip() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(base64_decode(&base64_encode(s)), s);
        }
        assert_eq!(base64_encode(b"foobar"), b"Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), b"Zm8=");
    }

    #[test]
    fn b64_lenient() {
        assert_eq!(base64_decode(b"Zm9v\nYmFy!"), b"foobar");
        assert_eq!(base64_decode(b"Zg"), b"f");
        assert_eq!(base64_decode(b"Z"), b"");
    }

    #[test]
    fn hex() {
        assert_eq!(bin2hex(b"\x00\xff\x10"), b"00ff10");
    }

    #[test]
    fn expand() {
        assert_eq!(expand_filepath(b"/a/./b//c/../d").unwrap(), b"/a/b/d");
        assert_eq!(expand_filepath(b"/").unwrap(), b"/");
    }

    #[test]
    fn timegm_values() {
        assert_eq!(timegm(1970, 0, 1, 0, 0, 0), 0);
        assert_eq!(timegm(2000, 1, 29, 12, 0, 0), 951_825_600);
        assert_eq!(timegm(2038, 0, 19, 3, 14, 8), 2_147_483_648);
    }

    #[test]
    fn atoi_c() {
        assert_eq!(atoi(b" 42x"), 42);
        assert_eq!(atoi(b"-7"), -7);
        assert_eq!(atoi(b"x"), 0);
    }
}
