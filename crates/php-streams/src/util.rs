//! Small byte-string helpers.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Views a byte path as a `Path`.
pub fn os_path(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

/// Converts a `Path` to bytes.
pub fn path_bytes(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

/// Case-insensitive ASCII prefix test (`strncasecmp(s, prefix, len) == 0`).
pub fn starts_with_ci(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `memchr`.
pub fn find_byte(s: &[u8], b: u8) -> Option<usize> {
    s.iter().position(|&c| c == b)
}

/// `php_memnstr` / `strstr` for byte strings.
pub fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `ZEND_STRTOL(s, &end, 10)`: returns the value and the number of bytes
/// consumed (0 when no digits were found).
pub fn strtol(s: &[u8]) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut val: i64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        let d = i64::from(s[i] - b'0');
        if !overflow {
            match val.checked_mul(10).and_then(|v| {
                if neg {
                    v.checked_sub(d)
                } else {
                    v.checked_add(d)
                }
            }) {
                Some(v) => val = v,
                None => overflow = true,
            }
        }
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    if overflow {
        val = if neg { i64::MIN } else { i64::MAX };
    }
    (val, i)
}

/// Lossy display of bytes.
pub fn lossy(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtol_cases() {
        assert_eq!(strtol(b"123abc"), (123, 3));
        assert_eq!(strtol(b"  -5"), (-5, 4));
        assert_eq!(strtol(b"x"), (0, 0));
        assert_eq!(strtol(b"99999999999999999999").0, i64::MAX);
    }

    #[test]
    fn finders() {
        assert_eq!(find_bytes(b"abcabc", b"ca"), Some(2));
        assert_eq!(find_bytes(b"ab", b"abc"), None);
        assert!(starts_with_ci(b"FILE://x", b"file://"));
    }
}
