//! Small byte-string helpers that mirror the PHP functions `run-tests.php` relies on.

/// Characters stripped by PHP's `trim()` with the default character list.
const TRIM_CHARS: &[u8] = b" \t\n\r\0\x0B";

/// PHP `trim()` with the default character list.
pub fn trim(s: &[u8]) -> &[u8] {
    trim_end(trim_start(s))
}

/// PHP `ltrim()` with the default character list.
pub fn trim_start(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !TRIM_CHARS.contains(b)).unwrap_or(s.len());
    &s[start..]
}

/// PHP `rtrim()` with the default character list.
pub fn trim_end(s: &[u8]) -> &[u8] {
    let end = s.iter().rposition(|b| !TRIM_CHARS.contains(b)).map_or(0, |p| p + 1);
    &s[..end]
}

/// PHP `empty()` applied to a string: `""` and `"0"` are empty.
pub fn php_empty(s: &[u8]) -> bool {
    s.is_empty() || s == b"0"
}

/// `preg_replace("/\r\n/", "\n", $s)`.
pub fn normalize_crlf(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\r' && s.get(i + 1) == Some(&b'\n') {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Replace every occurrence of `from` with `to` (PHP `str_replace` with scalar arguments).
pub fn replace(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// Case-insensitive ASCII prefix test (`!strncasecmp($prefix, $s, strlen($prefix))`).
pub fn starts_with_ignore_case(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Substring search.
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// Lossy conversion for display purposes.
pub fn lossy(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// Split on `\n` like PHP `explode("\n", $s)` (always yields at least one element).
pub fn explode_lines(s: &[u8]) -> Vec<&[u8]> {
    s.split(|&b| b == b'\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_matches_php() {
        assert_eq!(trim(b" \t\nabc \0\x0B\r\n"), b"abc");
        assert_eq!(trim(b"   "), b"");
        assert_eq!(trim_start(b"  a "), b"a ");
        assert_eq!(trim_end(b"  a "), b"  a");
    }

    #[test]
    fn php_empty_semantics() {
        assert!(php_empty(b""));
        assert!(php_empty(b"0"));
        assert!(!php_empty(b"00"));
        assert!(!php_empty(b" "));
    }

    #[test]
    fn crlf_and_replace() {
        assert_eq!(normalize_crlf(b"a\r\nb\rc\n"), b"a\nb\rc\n");
        assert_eq!(replace(b"a..b..", b"..", b""), b"ab");
        assert_eq!(replace(b"{PWD}/x", b"{PWD}", b"/tmp"), b"/tmp/x");
    }

    #[test]
    fn prefix_and_explode() {
        assert!(starts_with_ignore_case(b"SKIP foo", b"skip"));
        assert!(!starts_with_ignore_case(b"sk", b"skip"));
        assert_eq!(explode_lines(b"a\nb"), vec![&b"a"[..], &b"b"[..]]);
        assert_eq!(explode_lines(b""), vec![&b""[..]]);
    }
}
