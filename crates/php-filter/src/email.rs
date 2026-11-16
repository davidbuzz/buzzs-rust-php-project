// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `FILTER_VALIDATE_EMAIL` (`php_filter_validate_email`).
//!
//! php-src matches the address against a fixed PCRE (based on Michael
//! Rushton's), with `/iD` or, with `FILTER_FLAG_EMAIL_UNICODE`, `/iDu`. This
//! module reproduces what that regex accepts without a regex engine:
//!
//! ```text
//! ^(?!(?:(?:\x22?\x5C[\x00-\x7E]\x22?)|(?:\x22?[^\x5C\x22]\x22?)){255,})
//!  (?!(?:(?:\x22?\x5C[\x00-\x7E]\x22?)|(?:\x22?[^\x5C\x22]\x22?)){65,}@)
//!  WORD (?:\.WORD)* @ (HOSTNAME | \[ADDRESS-LITERAL\]) $
//! ```
//!
//! * The two negative lookaheads limit the address to fewer than 255 and the
//!   local part to fewer than 65 "units" (a unit is one character or a
//!   backslash pair, optionally wrapped in double quotes); they are decided
//!   with a longest-path count over all unit decompositions, which is exactly
//!   what the backtracking matcher explores.
//! * `WORD` is an atom (`[!#-'*+\-/-9=?^-~]` plus, case-insensitively, `A-Z`,
//!   plus `\pL\pN` in Unicode mode) or a quoted string.
//! * `HOSTNAME` is two or more dot-separated labels of `[a-z0-9]` with inner
//!   hyphens, none longer than 63 characters, the last starting with a
//!   letter. Under `/iu` PCRE2 also folds U+017F (ſ) to `s` and U+212A
//!   (Kelvin sign) to `k`.
//! * `ADDRESS-LITERAL` is an IPv4 address, `IPv6:` plus a full or compressed
//!   IPv6 address (at most six groups when compressed), or `IPv6:` plus an
//!   IPv6 prefix (six groups, or at most four when compressed) and an IPv4
//!   address.
//!
//! In Unicode mode the subject must be valid UTF-8 (otherwise PCRE fails
//! with a UTF error and validation fails) and all counting is per code point.

use crate::unicode_table::LETTER_OR_NUMBER;

/// The maximum length of an e-mail address (RFC 2821).
pub const MAX_EMAIL_LENGTH: usize = 320;

const QUOTE: u32 = 0x22;
const BACKSLASH: u32 = 0x5c;

fn is_letter_or_number(c: u32) -> bool {
    LETTER_OR_NUMBER
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

struct Matcher<'a> {
    s: &'a [u32],
    unicode: bool,
}

impl Matcher<'_> {
    fn at(&self, i: usize) -> Option<u32> {
        self.s.get(i).copied()
    }

    /// `[\x21\x23-\x27\x2A\x2B\x2D\x2F-\x39\x3D\x3F\x5E-\x7E(\pL\pN)]` with `/i`.
    fn is_atom(&self, c: u32) -> bool {
        matches!(c, 0x21 | 0x23..=0x27 | 0x2a | 0x2b | 0x2d | 0x2f..=0x39 | 0x3d | 0x3f | 0x5e..=0x7e)
            || (0x41..=0x5a).contains(&c)
            || (self.unicode && c >= 0x80 && is_letter_or_number(c))
    }

    /// `[\x01-\x08\x0B\x0C\x0E-\x1F\x21\x23-\x5B\x5D-\x7F(\pL\pN)]`.
    fn is_qtext(&self, c: u32) -> bool {
        matches!(c, 0x01..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x21 | 0x23..=0x5b | 0x5d..=0x7f)
            || (self.unicode && c >= 0x80 && is_letter_or_number(c))
    }

    /// `[a-z0-9]` with `/i` (`/iu` adds U+017F and U+212A).
    fn is_alnum(&self, c: u32) -> bool {
        self.is_alpha(c) || (0x30..=0x39).contains(&c)
    }

    /// `[a-z]` with `/i`.
    fn is_alpha(&self, c: u32) -> bool {
        (0x41..=0x5a).contains(&c)
            || (0x61..=0x7a).contains(&c)
            || (self.unicode && (c == 0x17f || c == 0x212a))
    }

    /// The two leading negative lookaheads: `true` when the address has 255
    /// or more units, or 65 or more units before some `@`.
    fn too_many_units(&self) -> bool {
        let n = self.s.len();
        // reach[i]: the largest number of units that end exactly at i.
        let mut reach: Vec<Option<usize>> = vec![None; n + 1];
        reach[0] = Some(0);
        for i in 0..n {
            let Some(count) = reach[i] else { continue };
            let mut starts = vec![i];
            if self.s[i] == QUOTE {
                starts.push(i + 1);
            }
            for j in starts {
                let core_end = match self.at(j) {
                    Some(BACKSLASH) => match self.at(j + 1) {
                        Some(c) if c <= 0x7e => j + 2,
                        _ => continue,
                    },
                    Some(QUOTE) | None => continue,
                    Some(_) => j + 1,
                };
                let mut ends = vec![core_end];
                if self.at(core_end) == Some(QUOTE) {
                    ends.push(core_end + 1);
                }
                for e in ends {
                    let c = count + 1;
                    if reach[e].is_none_or(|r| r < c) {
                        reach[e] = Some(c);
                    }
                }
            }
        }
        reach.iter().flatten().any(|&c| c >= 255)
            || (0..n).any(|i| self.s[i] == u32::from(b'@') && reach[i].is_some_and(|c| c >= 65))
    }

    /// An atom or a quoted string starting at `i`; returns its end.
    fn word(&self, mut i: usize) -> Option<usize> {
        let c = self.at(i)?;
        if self.is_atom(c) {
            while self.at(i).is_some_and(|c| self.is_atom(c)) {
                i += 1;
            }
            return Some(i);
        }
        if c != QUOTE {
            return None;
        }
        i += 1;
        loop {
            match self.at(i)? {
                QUOTE => return Some(i + 1),
                BACKSLASH => {
                    if self.at(i + 1)? > 0x7f {
                        return None;
                    }
                    i += 2;
                }
                c if self.is_qtext(c) => i += 1,
                _ => return None,
            }
        }
    }

    /// A label: `[a-z0-9]+(?:-+[a-z0-9]+)*`.
    fn is_label(&self, l: &[u32]) -> bool {
        match (l.first(), l.last()) {
            (Some(&f), Some(&e)) => {
                self.is_alnum(f)
                    && self.is_alnum(e)
                    && l.iter().all(|&c| c == u32::from(b'-') || self.is_alnum(c))
            }
            _ => false,
        }
    }

    fn hostname(&self, d: &[u32]) -> bool {
        // (?!.*[^.]{64,}): no label of 64 or more characters (a newline
        // makes the hostname invalid anyway).
        let labels: Vec<&[u32]> = d.split(|&c| c == u32::from(b'.')).collect();
        if labels.len() < 2 || labels.iter().any(|l| l.len() >= 64) {
            return false;
        }
        let (tld, rest) = labels.split_last().unwrap_or((&labels[0], &[]));
        rest.iter().all(|l| self.is_label(l)) && self.is_label(tld) && self.is_alpha(tld[0])
    }
}

fn is_hex(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| c.is_ascii_hexdigit())
}

/// `[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){min-1,max-1}` matching all of `s`
/// (`s` may be empty only if `allow_empty`); returns the group count.
fn hex_groups(s: &[u32], allow_empty: bool) -> Option<usize> {
    if s.is_empty() {
        return allow_empty.then_some(0);
    }
    let groups: Vec<&[u32]> = s.split(|&c| c == u32::from(b':')).collect();
    groups
        .iter()
        .all(|g| (1..=4).contains(&g.len()) && g.iter().all(|&c| is_hex(c)))
        .then_some(groups.len())
}

fn find_double_colon(s: &[u32]) -> Option<usize> {
    s.windows(2)
        .position(|w| w[0] == u32::from(b':') && w[1] == u32::from(b':'))
}

/// `(?:25[0-5])|(?:2[0-4][0-9])|(?:1[0-9]{2})|(?:[1-9]?[0-9])` four times.
fn is_ipv4(s: &[u32]) -> bool {
    let parts: Vec<&[u32]> = s.split(|&c| c == u32::from(b'.')).collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            let digits: Option<Vec<u8>> = p
                .iter()
                .map(|&c| u8::try_from(c).ok().filter(u8::is_ascii_digit))
                .collect();
            match digits.as_deref() {
                Some([_]) => true,
                Some([a, _]) => *a != b'0',
                Some([a, b, c]) => {
                    *a == b'1' || (*a == b'2' && (*b <= b'4' || (*b == b'5' && *c <= b'5')))
                }
                _ => false,
            }
        })
}

fn strip_ipv6_tag(s: &[u32]) -> Option<&[u32]> {
    let tag = b"ipv6:";
    (s.len() >= tag.len()
        && s.iter()
            .zip(tag)
            .all(|(&c, &t)| u8::try_from(c).is_ok_and(|c| c.to_ascii_lowercase() == t)))
    .then(|| &s[tag.len()..])
}

/// The address literal between `[` and `]`.
fn address_literal(inner: &[u32]) -> bool {
    let Some(v6) = strip_ipv6_tag(inner) else {
        return is_ipv4(inner);
    };
    // IPv6:<full or compressed IPv6>
    if hex_groups(v6, false) == Some(8) {
        return true;
    }
    if let Some(dc) = find_double_colon(v6) {
        if let (Some(l), Some(r)) = (hex_groups(&v6[..dc], true), hex_groups(&v6[dc + 2..], true)) {
            if l <= 6 && r <= 6 && l + r <= 6 {
                return true;
            }
        }
    }
    // IPv6:<prefix>:<IPv4>
    let Some(last_colon) = v6.iter().rposition(|&c| c == u32::from(b':')) else {
        return false;
    };
    if !is_ipv4(&v6[last_colon + 1..]) {
        return false;
    }
    let prefix = &v6[..last_colon]; // without the final ':'
    if hex_groups(prefix, false) == Some(6) {
        return true;
    }
    let Some(dc) = find_double_colon(&v6[..=last_colon]) else {
        return false;
    };
    let Some(l) = hex_groups(&v6[..dc], true) else {
        return false;
    };
    let after = &v6[dc + 2..=last_colon];
    let r = if after.is_empty() {
        0
    } else {
        match after.split_last() {
            Some((&c, body)) if c == u32::from(b':') => match hex_groups(body, false) {
                Some(n) => n,
                None => return false,
            },
            _ => return false,
        }
    };
    l <= 4 && r <= 4 && l + r <= 4
}

/// Whether `s` is a valid e-mail address for `FILTER_VALIDATE_EMAIL`
/// (`unicode`: `FILTER_FLAG_EMAIL_UNICODE`).
#[must_use]
pub fn validate_email(s: &[u8], unicode: bool) -> bool {
    if s.len() > MAX_EMAIL_LENGTH {
        return false;
    }
    let units: Vec<u32> = if unicode {
        match std::str::from_utf8(s) {
            Ok(t) => t.chars().map(u32::from).collect(),
            Err(_) => return false,
        }
    } else {
        s.iter().map(|&b| u32::from(b)).collect()
    };
    let m = Matcher { s: &units, unicode };
    if m.too_many_units() {
        return false;
    }
    let Some(mut i) = m.word(0) else {
        return false;
    };
    while m.at(i) == Some(u32::from(b'.')) {
        match m.word(i + 1) {
            Some(e) => i = e,
            None => return false,
        }
    }
    if m.at(i) != Some(u32::from(b'@')) {
        return false;
    }
    let domain = &units[i + 1..];
    if m.hostname(domain) {
        return true;
    }
    matches!(
        (domain.first(), domain.last()),
        (Some(&0x5b), Some(&0x5d)) if domain.len() >= 2
    ) && address_literal(&domain[1..domain.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple() {
        assert!(validate_email(b"a@b.cd", false));
        assert!(validate_email(b"A.B-C+d@EXAMPLE.co.uk", false));
        assert!(!validate_email(b"a@b", false));
        assert!(!validate_email(b"a@b.c-", false));
        assert!(!validate_email(b"a@-b.cd", false));
        assert!(!validate_email(b"a@b.1d", false));
        assert!(validate_email(b"a@b.xn--p1ai", false));
        assert!(!validate_email(b"\"a b\"@c.de", false));
        assert!(validate_email(b"\"a\\\"b\"@c.de", false));
        assert!(!validate_email(b"a..b@c.de", false));
        assert!(!validate_email(b".a@c.de", false));
        assert!(!validate_email(b"a@c.de\n", false));
        assert!(validate_email(b"a@[1.2.3.4]", false));
        assert!(!validate_email(b"a@[1.2.3.04]", false));
        assert!(validate_email(b"a@[IPv6:::1]", false));
        assert!(validate_email(b"a@[ipv6:1:2:3:4:5:6:7:8]", false));
        assert!(!validate_email(b"a@[IPv6:1:2:3:4:5:6:7::]", false));
        assert!(validate_email(b"a@[IPv6:::ffff:1.2.3.4]", false));
        assert!(validate_email(b"a@[IPv6:1:2:3:4:5:6:1.2.3.4]", false));
        assert!(!validate_email(b"a@[IPv6:1:2:3::4:5:1.2.3.4]", false));
    }

    #[test]
    fn lengths() {
        let local = "a".repeat(64);
        assert!(validate_email(format!("{local}@b.cd").as_bytes(), false));
        let local = "a".repeat(65);
        assert!(!validate_email(format!("{local}@b.cd").as_bytes(), false));
        let label = "b".repeat(63);
        assert!(validate_email(format!("a@{label}.cd").as_bytes(), false));
        let label = "b".repeat(64);
        assert!(!validate_email(format!("a@{label}.cd").as_bytes(), false));
    }

    #[test]
    fn unicode() {
        assert!(!validate_email("ä@b.cd".as_bytes(), false));
        assert!(validate_email("ä@b.cd".as_bytes(), true));
        assert!(!validate_email(b"\xff@b.cd", true));
        assert!(validate_email("a@b.\u{17f}e".as_bytes(), true));
        assert!(!validate_email("a@b.ä".as_bytes(), true));
    }
}
