//! String comparison functions: `strcmp()` and friends
//! (`Zend/zend_operators.c` `zend_binary_str*cmp`), `strnatcmp()` /
//! `strnatcasecmp()` (`ext/standard/strnatcmp.c`) and `substr_compare()`.
//!
//! The return values follow the C implementation exactly: when two strings
//! differ at some byte, the result is the difference of those (possibly
//! ASCII-lowercased) bytes, as glibc's `memcmp` returns it; when one is a
//! prefix of the other the result is `-1`/`1` (PHP ≥ 8.2).
//!
//! The magnitude of a `memcmp`-based result (`strcmp`, `strncmp`,
//! case-sensitive `substr_compare`) is implementation-defined in C; glibc's
//! vectorised `memcmp` returns other non-zero values for some short
//! inputs.  The sign always agrees, and PHP's tests only rely on the sign.

use std::cmp::Ordering;

use crate::error::{StdError, StdResult};

fn three_way(a: usize, b: usize) -> i64 {
    match a.cmp(&b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn diff_bytes(a: &[u8], b: &[u8], fold: bool) -> Option<i64> {
    a.iter().zip(b).find_map(|(&x, &y)| {
        let (x, y) = if fold {
            (x.to_ascii_lowercase(), y.to_ascii_lowercase())
        } else {
            (x, y)
        };
        (x != y).then(|| i64::from(x) - i64::from(y))
    })
}

/// `zend_binary_strcmp()`; backs `strcmp(string $string1, string $string2): int`.
pub fn strcmp(s1: &[u8], s2: &[u8]) -> i64 {
    diff_bytes(s1, s2, false).unwrap_or_else(|| three_way(s1.len(), s2.len()))
}

/// `zend_binary_strcasecmp()` (ASCII case folding); backs
/// `strcasecmp(string $string1, string $string2): int`.
pub fn strcasecmp(s1: &[u8], s2: &[u8]) -> i64 {
    diff_bytes(s1, s2, true).unwrap_or_else(|| three_way(s1.len(), s2.len()))
}

/// `zend_binary_strncmp()` for a non-negative length.
pub fn binary_strncmp(s1: &[u8], s2: &[u8], length: usize) -> i64 {
    let a = &s1[..length.min(s1.len())];
    let b = &s2[..length.min(s2.len())];
    diff_bytes(a, b, false).unwrap_or_else(|| three_way(a.len(), b.len()))
}

/// `zend_binary_strncasecmp()` for a non-negative length.
pub fn binary_strncasecmp(s1: &[u8], s2: &[u8], length: usize) -> i64 {
    let a = &s1[..length.min(s1.len())];
    let b = &s2[..length.min(s2.len())];
    diff_bytes(a, b, true).unwrap_or_else(|| three_way(a.len(), b.len()))
}

fn check_length(func: &'static str, length: i64) -> StdResult<usize> {
    usize::try_from(length)
        .map_err(|_| StdError::arg_value(func, 3, "length", "must be greater than or equal to 0"))
}

/// Backs `strncmp(string $string1, string $string2, int $length): int`.
/// Throws `ValueError` for a negative length.
pub fn strncmp(s1: &[u8], s2: &[u8], length: i64) -> StdResult<i64> {
    Ok(binary_strncmp(s1, s2, check_length("strncmp", length)?))
}

/// Backs `strncasecmp(string $string1, string $string2, int $length): int`.
/// Throws `ValueError` for a negative length.
pub fn strncasecmp(s1: &[u8], s2: &[u8], length: i64) -> StdResult<i64> {
    Ok(binary_strncasecmp(
        s1,
        s2,
        check_length("strncasecmp", length)?,
    ))
}

/// Port of `strnatcmp_ex()`; backs `strnatcmp()` (`case_insensitive =
/// false`) and `strnatcasecmp()` (`true`).  Returns -1, 0 or 1.
///
/// Leading zeros are skipped, runs of whitespace are ignored, digit runs
/// are compared numerically (right-aligned) unless one starts with `0`, in
/// which case they are compared as fractions (left-aligned).
pub fn strnatcmp(a: &[u8], b: &[u8], case_insensitive: bool) -> i64 {
    if a.is_empty() || b.is_empty() {
        return three_way(a.len(), b.len());
    }
    // The C code walks NUL-terminated buffers and may peek at the terminator.
    let at_a = |i: usize| a.get(i).copied().unwrap_or(0);
    let at_b = |i: usize| b.get(i).copied().unwrap_or(0);
    let (aend, bend) = (a.len(), b.len());
    let mut ap = 0;
    let mut bp = 0;
    let mut ca = a[0];
    let mut cb = b[0];

    while ca == b'0' && ap + 1 < aend && a[ap + 1].is_ascii_digit() {
        ap += 1;
        ca = a[ap];
    }
    while cb == b'0' && bp + 1 < bend && b[bp + 1].is_ascii_digit() {
        bp += 1;
        cb = b[bp];
    }

    loop {
        while is_c_space(ca) {
            ap += 1;
            ca = at_a(ap);
        }
        while is_c_space(cb) {
            bp += 1;
            cb = at_b(bp);
        }

        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let fractional = ca == b'0' || cb == b'0';
            let result = if fractional {
                compare_left(a, &mut ap, b, &mut bp)
            } else {
                compare_right(a, &mut ap, b, &mut bp)
            };
            if result != 0 {
                return result;
            } else if ap == aend && bp == bend {
                return 0;
            } else if ap == aend {
                return -1;
            } else if bp == bend {
                return 1;
            }
            ca = at_a(ap);
            cb = at_b(bp);
        }

        if case_insensitive {
            ca = ca.to_ascii_uppercase();
            cb = cb.to_ascii_uppercase();
        }
        match ca.cmp(&cb) {
            Ordering::Less => return -1,
            Ordering::Greater => return 1,
            Ordering::Equal => {}
        }
        ap += 1;
        bp += 1;
        if ap >= aend && bp >= bend {
            return 0;
        } else if ap >= aend {
            return -1;
        } else if bp >= bend {
            return 1;
        }
        ca = a[ap];
        cb = b[bp];
    }
}

/// C-locale `isspace()`.
fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn digit_at(s: &[u8], i: usize) -> Option<u8> {
    s.get(i).copied().filter(u8::is_ascii_digit)
}

/// `compare_right()`: right-aligned numeric comparison of digit runs.
fn compare_right(a: &[u8], ap: &mut usize, b: &[u8], bp: &mut usize) -> i64 {
    let mut bias = 0;
    loop {
        match (digit_at(a, *ap), digit_at(b, *bp)) {
            (None, None) => return bias,
            (None, Some(_)) => return -1,
            (Some(_), None) => return 1,
            (Some(x), Some(y)) => {
                if bias == 0 {
                    bias = match x.cmp(&y) {
                        Ordering::Less => -1,
                        Ordering::Greater => 1,
                        Ordering::Equal => 0,
                    };
                }
            }
        }
        *ap += 1;
        *bp += 1;
    }
}

/// `compare_left()`: left-aligned (fractional) comparison of digit runs.
fn compare_left(a: &[u8], ap: &mut usize, b: &[u8], bp: &mut usize) -> i64 {
    loop {
        match (digit_at(a, *ap), digit_at(b, *bp)) {
            (None, None) => return 0,
            (None, Some(_)) => return -1,
            (Some(_), None) => return 1,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Less => return -1,
                Ordering::Greater => return 1,
                Ordering::Equal => {}
            },
        }
        *ap += 1;
        *bp += 1;
    }
}

/// Backs `substr_compare(string $haystack, string $needle, int $offset,
/// ?int $length = null, bool $case_insensitive = false): int`.
///
/// A negative offset counts from the end (clamped to 0); an offset past the
/// end throws `ValueError`; an explicit length of 0 returns 0 and a
/// negative one throws.  `length: None` (or `Some(0)` is handled first)
/// compares `max(strlen($needle), strlen($haystack) - $offset)` bytes.
pub fn substr_compare(
    haystack: &[u8],
    needle: &[u8],
    offset: i64,
    length: Option<i64>,
    case_insensitive: bool,
) -> StdResult<i64> {
    if let Some(len) = length {
        if len == 0 {
            return Ok(0);
        }
        if len < 0 {
            return Err(StdError::arg_value(
                "substr_compare",
                4,
                "length",
                "must be greater than or equal to 0",
            ));
        }
    }
    let hlen = i64::try_from(haystack.len()).unwrap_or(i64::MAX);
    let mut offset = offset;
    if offset < 0 {
        offset = hlen.saturating_add(offset).max(0);
    }
    if offset > hlen {
        return Err(StdError::arg_value(
            "substr_compare",
            3,
            "offset",
            "must be contained in argument #1 ($haystack)",
        ));
    }
    let offset = usize::try_from(offset).unwrap_or(0);
    let rest = &haystack[offset..];
    let cmp_len = match length {
        Some(l) => usize::try_from(l).unwrap_or(usize::MAX),
        None => needle.len().max(rest.len()),
    };
    Ok(if case_insensitive {
        binary_strncasecmp(rest, needle, cmp_len)
    } else {
        binary_strncmp(rest, needle, cmp_len)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_cmp() {
        assert_eq!(strcmp(b"a", b"c"), -2);
        assert_eq!(strcmp(b"ab", b"abcd"), -1);
        assert_eq!(strcmp(b"abcd", b"ab"), 1);
        assert_eq!(strcmp(b"", b""), 0);
        assert_eq!(strcasecmp(b"HELLO", b"hello"), 0);
        assert_eq!(strcasecmp(b"A", b"c"), -2);
        assert_eq!(strncmp(b"abc", b"abd", 2), Ok(0));
        assert_eq!(strncasecmp(b"abc", b"ABD", 3), Ok(-1));
        assert_eq!(
            strncmp(b"a", b"b", -1).map_err(|e| e.to_string()),
            Err("strncmp(): Argument #3 ($length) must be greater than or equal to 0".into())
        );
    }

    #[test]
    fn natural() {
        assert_eq!(strnatcmp(b"img12.png", b"img10.png", false), 1);
        assert_eq!(strnatcmp(b"img2", b"img10", false), -1);
        assert_eq!(strnatcmp(b"0.1", b"0.01", false), 1);
        assert_eq!(strnatcmp(b"A", b"a", true), 0);
        assert_eq!(strnatcmp(b"", b"a", false), -1);
        assert_eq!(strnatcmp(b"  a", b"a", false), 0);
    }

    #[test]
    fn substr_cmp() {
        assert_eq!(substr_compare(b"abcde", b"bc", 1, Some(2), false), Ok(0));
        assert_eq!(substr_compare(b"abcde", b"de", -2, Some(2), false), Ok(0));
        assert_eq!(substr_compare(b"abcde", b"BC", 1, Some(2), true), Ok(0));
        assert_eq!(substr_compare(b"abcde", b"x", 1, Some(0), false), Ok(0));
        assert!(substr_compare(b"abcde", b"x", 6, None, false).is_err());
        assert!(substr_compare(b"abcde", b"x", 1, Some(-1), false).is_err());
    }
}
