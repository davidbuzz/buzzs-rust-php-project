//! `--EXPECTF--` / `--EXPECTREGEX--` matching (ref: `run-tests.php` `expectf_to_regex()`).
//!
//! run-tests.php builds `'/^' . $re . '$/s'` and calls `preg_match()`, so matching is done with
//! PCRE2 in non-UTF (byte) mode with `DOTALL`, exactly like PHP.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use pcre2::bytes::{Regex, RegexBuilder};

/// Characters escaped by PHP's `preg_quote()` (in addition to NUL and the delimiter).
const PREG_QUOTE_CHARS: &[u8] = b".\\+*?[^]$(){}=!<>|:-#";

/// PHP `preg_quote($s, $delimiter)`.
pub fn preg_quote(s: &[u8], delimiter: Option<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for &b in s {
        if b == 0 {
            out.extend_from_slice(b"\\000");
        } else if PREG_QUOTE_CHARS.contains(&b) || Some(b) == delimiter {
            out.push(b'\\');
            out.push(b);
        } else {
            out.push(b);
        }
    }
    out
}

/// Replacement table applied with `strtr()` after quoting.
fn placeholder(c: u8) -> Option<&'static [u8]> {
    Some(match c {
        b'e' => b"\\/",
        b's' => b"[^\\r\\n]+",
        b'S' => b"[^\\r\\n]*",
        b'a' => b".+?",
        b'A' => b".*?",
        b'w' => b"\\s*",
        b'i' => b"[+-]?\\d+",
        b'd' => b"\\d+",
        b'x' => b"[0-9a-fA-F]+",
        b'f' => b"[+-]?(?:\\d+|(?=\\.\\d))(?:\\.\\d+)?(?:[Ee][+-]?\\d+)?",
        b'c' => b".",
        b'0' => b"\\x00",
        _ => return None,
    })
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Port of `expectf_to_regex()`: quote everything except `%r...%r` sections (which are wrapped in
/// a group), then expand the `%x` placeholders in a single left-to-right `strtr()` pass.
pub fn expectf_to_regex(wanted: &[u8]) -> Vec<u8> {
    let wanted = crate::bytes::normalize_crlf(wanted);
    let length = wanted.len();
    let mut temp = Vec::with_capacity(length * 2);
    let mut start_offset = 0;
    while start_offset < length {
        let (start, end) = match find(&wanted, b"%r", start_offset) {
            Some(start) => match find(&wanted, b"%r", start + 2) {
                Some(end) => (start, end),
                // Unbalanced tag: ignore it.
                None => (length, length),
            },
            None => (length, length),
        };
        temp.extend(preg_quote(&wanted[start_offset..start], Some(b'/')));
        if end > start {
            temp.push(b'(');
            temp.extend_from_slice(&wanted[start + 2..end]);
            temp.push(b')');
        }
        start_offset = end + 2;
    }

    let mut out = Vec::with_capacity(temp.len() * 2);
    let mut i = 0;
    while i < temp.len() {
        if temp[i] == b'%' {
            if let Some(rep) = temp.get(i + 1).copied().and_then(placeholder) {
                out.extend_from_slice(rep);
                i += 2;
                continue;
            }
        }
        out.push(temp[i]);
        i += 1;
    }
    out
}

/// Emulates PHP's delimiter scan for `'/^' . $re . '$/s'`: an unescaped `/` inside `$re` ends the
/// pattern early, the rest is parsed as (invalid) modifiers and `preg_match()` returns false.
fn has_unescaped_delimiter(re: &[u8]) -> bool {
    let mut i = 0;
    while i < re.len() {
        match re[i] {
            b'\\' if i + 1 < re.len() => i += 2,
            b'/' => return true,
            _ => i += 1,
        }
    }
    false
}

/// Convert a byte pattern into an ASCII pattern string PCRE2 (non-UTF mode) understands
/// identically: bytes >= 0x80 become `\x{hh}`.
fn pattern_to_ascii(re: &[u8]) -> String {
    let mut out = String::with_capacity(re.len() + 2);
    let mut i = 0;
    while i < re.len() {
        let b = re[i];
        if b == b'\\' && re.get(i + 1).is_some_and(|n| *n >= 0x80) {
            // An escaped high byte is just that literal byte.
            let _ = write!(out, "\\x{{{:02x}}}", re[i + 1]);
            i += 2;
            continue;
        }
        if b == b'\\' {
            out.push('\\');
            if let Some(&n) = re.get(i + 1) {
                out.push(char::from(n));
            }
            i += 2;
            continue;
        }
        if b >= 0x80 {
            let _ = write!(out, "\\x{{{b:02x}}}");
        } else {
            out.push(char::from(b));
        }
        i += 1;
    }
    out
}

/// Compile a pattern. No custom JIT stack is configured: a dedicated stack is an mmap
/// reservation per match-data object, and with the pcre2 crate's per-thread pools those
/// reservations were never returned when matching from worker threads (tens of GB of virtual
/// memory after a thousand tests, which made `fork` fail). If the default JIT stack is too small
/// for a match, [`regex_matches`] falls back to the interpreter.
fn build(pattern: &str, jit: bool) -> Option<Regex> {
    RegexBuilder::new()
        .dotall(true)
        .jit_if_available(jit)
        .build(pattern)
        .ok()
}

/// `preg_match('/^' . $re . '$/s', $subject)`. Returns false for patterns PHP would reject.
pub fn regex_matches(re: &[u8], subject: &[u8]) -> bool {
    if has_unescaped_delimiter(re) {
        return false;
    }
    let pattern = format!("^{}$", pattern_to_ascii(re));
    let Some(regex) = build(&pattern, true) else {
        return false;
    };
    match regex.is_match(subject) {
        Ok(m) => m,
        // JIT stack exhaustion etc.: retry with the interpreter before giving up.
        Err(_) => build(&pattern, false).is_some_and(|r| r.is_match(subject).unwrap_or(false)),
    }
}

/// `EXPECTF` comparison.
pub fn expectf_matches(wanted: &[u8], output: &[u8]) -> bool {
    regex_matches(&expectf_to_regex(wanted), output)
}

/// Line matcher used by the diff for EXPECTF/EXPECTREGEX tests, caching compiled patterns.
#[derive(Debug, Default)]
pub struct LineMatcher {
    cache: Mutex<HashMap<Vec<u8>, Option<Regex>>>,
}

impl LineMatcher {
    /// Does `line` match `expected` treated as an EXPECTF line?
    pub fn matches(&self, expected: &[u8], line: &[u8]) -> bool {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let regex = cache.entry(expected.to_vec()).or_insert_with(|| {
            let re = expectf_to_regex(expected);
            if has_unescaped_delimiter(&re) {
                None
            } else {
                // Lines are short: the interpreter is fine and needs no JIT stack.
                build(&format!("^{}$", pattern_to_ascii(&re)), false)
            }
        });
        regex
            .as_ref()
            .is_some_and(|r| r.is_match(line).unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote() {
        assert_eq!(preg_quote(b"a.b/c#\0", Some(b'/')), b"a\\.b\\/c\\#\\000");
        assert_eq!(preg_quote(b"%d", Some(b'/')), b"%d");
    }

    #[test]
    fn placeholders() {
        assert!(expectf_matches(b"%d %i %f %x %c", b"12 -3 1.5e3 fF z"));
        assert!(!expectf_matches(b"%d", b"-1"));
        assert!(expectf_matches(b"%s|%S|", b"abc||"));
        assert!(!expectf_matches(b"%s", b""));
        assert!(expectf_matches(b"a%aB", b"a\nxB"));
        assert!(expectf_matches(b"a%AB", b"aB"));
        assert!(expectf_matches(b"%w", b" \n\t"));
        assert!(expectf_matches(b"%f", b".5"));
        assert!(!expectf_matches(b"%f", b"."));
        assert!(expectf_matches(b"x%ey", b"x/y"));
        assert!(expectf_matches(b"%0", b"\0"));
    }

    #[test]
    fn literal_escaping() {
        assert!(expectf_matches(b"a.b (c) [d] $e ^f", b"a.b (c) [d] $e ^f"));
        assert!(!expectf_matches(b"a.b", b"axb"));
        assert!(expectf_matches(b"100%", b"100%"));
        assert!(expectf_matches(b"%%d", b"%5"));
    }

    #[test]
    fn regex_sections() {
        assert!(expectf_matches(b"int(%r(1|2)%r)", b"int(2)"));
        assert!(expectf_matches(b"x %ra|b%r y", b"x b y"));
        assert!(!expectf_matches(b"x %ra|b%r y", b"x c y"));
        // Unbalanced %r is treated literally.
        assert!(expectf_matches(b"x %r y", b"x %r y"));
        // Placeholders are also expanded inside %r sections.
        assert!(expectf_matches(b"%r%d%r", b"42"));
    }

    #[test]
    fn regex_delimiter_and_bytes() {
        assert!(!regex_matches(b"a/b", b"a/b"));
        assert!(regex_matches(b"a\\/b", b"a/b"));
        assert!(expectf_matches(b"caf\xe9 %s", b"caf\xe9 au lait"));
        assert!(regex_matches(b"[\x80-\xff]+", b"\xe9\xe8"));
        assert!(regex_matches(
            b".*Division by zero.*",
            b"Fatal error: Division by zero in"
        ));
    }

    #[test]
    fn line_matcher() {
        let m = LineMatcher::default();
        assert!(m.matches(b"int(%d)", b"int(5)"));
        assert!(!m.matches(b"int(%d)", b"int(x)"));
        assert!(m.matches(b"int(%d)", b"int(7)"));
    }
}
