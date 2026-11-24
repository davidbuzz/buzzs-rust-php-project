// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `fnmatch()` — shell wildcard matching with glibc semantics.
//!
//! PHP's `fnmatch()` (`ext/standard/file.c`) calls the C library's
//! `fnmatch(3)` on POSIX systems. This module ports the single-byte (C
//! locale) code path of glibc's `posix/fnmatch_loop.c`, including its quirks:
//!
//! * `^` negates a bracket expression like `!` (glibc does this unless
//!   `POSIXLY_CORRECT` is set, which PHP never relies on);
//! * an unterminated `[` matches a literal `[`;
//! * a trailing backslash never matches;
//! * `[:class:]`, `[.c.]` and `[=c=]` work inside brackets (C locale only
//!   knows single-character collating elements);
//! * [`FNM_PATHNAME`] keeps `*`, `?` and brackets from matching `/`, and
//!   [`FNM_PERIOD`] requires a leading period (at the start, or after `/`
//!   when combined with [`FNM_PATHNAME`]) to be matched literally.

/// `FNM_PATHNAME` (`FNM_FILE_NAME`): wildcards never match `/`.
pub const FNM_PATHNAME: i32 = 1;
/// `FNM_NOESCAPE`: backslash is an ordinary character.
pub const FNM_NOESCAPE: i32 = 2;
/// `FNM_PERIOD`: a leading period must be matched explicitly.
pub const FNM_PERIOD: i32 = 4;
/// `FNM_LEADING_DIR` (glibc extension, not exposed as a PHP constant):
/// ignore a `/…` suffix of the string after a successful match.
pub const FNM_LEADING_DIR: i32 = 8;
/// `FNM_CASEFOLD`: case-insensitive matching (ASCII, C locale).
pub const FNM_CASEFOLD: i32 = 16;

/// `MAXPATHLEN` on Linux; PHP rejects longer patterns and file names.
pub const MAXPATHLEN: usize = 4096;

/// glibc's `CHAR_CLASS_MAX_LENGTH` for the single-byte matcher.
const CHAR_CLASS_MAX_LENGTH: usize = 256;

/// Matches `string` against the shell wildcard `pattern` like glibc's
/// `fnmatch(pattern, string, flags) == 0`.
///
/// Both inputs are treated as C strings: matching stops at the first NUL
/// byte, exactly as the C function would see them.
pub fn fnmatch(pattern: &[u8], string: &[u8], flags: i32) -> bool {
    let pattern = until_nul(pattern);
    let string = until_nul(string);
    Matcher {
        p: pattern,
        s: string,
    }
    .run(0, 0, flags & FNM_PERIOD != 0, flags)
}

/// PHP's `fnmatch()` function body: the length checks of
/// `PHP_FUNCTION(fnmatch)` followed by [`fnmatch`].
///
/// Returns `Err(message)` with the exact warning text (to be reported as
/// `fnmatch(): <message>`, after which PHP returns `false`).
pub fn php_fnmatch(pattern: &[u8], string: &[u8], flags: i32) -> Result<bool, String> {
    if string.len() >= MAXPATHLEN {
        return Err(format!(
            "Filename exceeds the maximum allowed length of {MAXPATHLEN} characters"
        ));
    }
    if pattern.len() >= MAXPATHLEN {
        return Err(format!(
            "Pattern exceeds the maximum allowed length of {MAXPATHLEN} characters"
        ));
    }
    Ok(fnmatch(pattern, string, flags))
}

fn until_nul(s: &[u8]) -> &[u8] {
    s.iter().position(|&b| b == 0).map_or(s, |n| &s[..n])
}

/// Looks up a POSIX character class by name (C locale).
pub(crate) fn char_class(name: &[u8]) -> Option<fn(u8) -> bool> {
    Some(match name {
        b"alnum" => |c: u8| c.is_ascii_alphanumeric(),
        b"alpha" => |c: u8| c.is_ascii_alphabetic(),
        b"blank" => |c: u8| c == b' ' || c == b'\t',
        b"cntrl" => |c: u8| c.is_ascii_control(),
        b"digit" => |c: u8| c.is_ascii_digit(),
        b"graph" => |c: u8| c.is_ascii_graphic(),
        b"lower" => |c: u8| c.is_ascii_lowercase(),
        b"print" => |c: u8| c == b' ' || c.is_ascii_graphic(),
        b"punct" => |c: u8| c.is_ascii_punctuation(),
        b"space" => |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b"upper" => |c: u8| c.is_ascii_uppercase(),
        b"xdigit" => |c: u8| c.is_ascii_hexdigit(),
        _ => return None,
    })
}

/// Outcome of matching one bracket expression.
enum Bracket {
    /// The expression cannot match here.
    NoMatch,
    /// The expression matched; pattern continues at this index.
    Matched(usize),
    /// Unterminated `[`: match it as a literal character.
    Literal,
}

struct Matcher<'a> {
    p: &'a [u8],
    s: &'a [u8],
}

impl Matcher<'_> {
    /// Pattern byte at `i`, or NUL past the end (C string semantics).
    fn pat(&self, i: usize) -> u8 {
        self.p.get(i).copied().unwrap_or(0)
    }

    /// `internal_fnmatch()`: returns true on a match.
    fn run(&self, mut pi: usize, mut ni: usize, mut no_leading_period: bool, flags: i32) -> bool {
        let file_name = flags & FNM_PATHNAME != 0;
        let noescape = flags & FNM_NOESCAPE != 0;
        let s = self.s;
        loop {
            let raw = self.pat(pi);
            pi += 1;
            if raw == 0 {
                break;
            }
            let mut c = fold(raw, flags);
            let mut new_no_leading_period = false;
            match c {
                b'?' => {
                    if ni == s.len()
                        || (s[ni] == b'/' && file_name)
                        || (s[ni] == b'.' && no_leading_period)
                    {
                        return false;
                    }
                }
                b'\\' if !noescape => {
                    c = self.pat(pi);
                    pi += 1;
                    if c == 0 {
                        // Trailing backslash loses.
                        return false;
                    }
                    c = fold(c, flags);
                    if ni == s.len() || fold(s[ni], flags) != c {
                        return false;
                    }
                }
                b'*' => return self.star(pi, ni, no_leading_period, flags),
                b'[' => match self.bracket(pi, ni, no_leading_period, flags) {
                    Bracket::NoMatch => return false,
                    Bracket::Matched(next) => pi = next,
                    Bracket::Literal => {
                        if ni == s.len() || fold(s[ni], flags) != b'[' {
                            return false;
                        }
                    }
                },
                b'/' if no_leading_period_flags(flags) => {
                    if ni == s.len() || s[ni] != c {
                        return false;
                    }
                    new_no_leading_period = true;
                }
                _ => {
                    if ni == s.len() || c != fold(s[ni], flags) {
                        return false;
                    }
                }
            }
            no_leading_period = new_no_leading_period;
            ni += 1;
        }
        ni == s.len() || (flags & FNM_LEADING_DIR != 0 && s[ni] == b'/')
    }

    /// The `*` case; `pi` points just past the star.
    fn star(&self, mut pi: usize, mut ni: usize, no_leading_period: bool, flags: i32) -> bool {
        let s = self.s;
        let file_name = flags & FNM_PATHNAME != 0;
        if ni < s.len() && s[ni] == b'.' && no_leading_period {
            return false;
        }
        let mut c = self.pat(pi);
        pi += 1;
        while c == b'?' || c == b'*' {
            if c == b'?' {
                if ni == s.len() || (s[ni] == b'/' && file_name) {
                    return false;
                }
                ni += 1;
            }
            c = self.pat(pi);
            pi += 1;
        }

        if c == 0 {
            // The wildcards are the last element of the pattern.
            if !file_name || flags & FNM_LEADING_DIR != 0 {
                return true;
            }
            return !s[ni..].contains(&b'/');
        }

        let endp = if file_name {
            s[ni..]
                .iter()
                .position(|&b| b == b'/')
                .map_or(s.len(), |n| ni + n)
        } else {
            s.len()
        };
        let flags2 = if file_name {
            flags
        } else {
            flags & !FNM_PERIOD
        };

        if c == b'[' {
            let start = pi - 1;
            let mut nlp = no_leading_period;
            while ni < endp {
                if self.run(start, ni, nlp, flags2) {
                    return true;
                }
                ni += 1;
                nlp = false;
            }
        } else if c == b'/' && file_name {
            while ni < s.len() && s[ni] != b'/' {
                ni += 1;
            }
            if ni < s.len() && self.run(pi, ni + 1, flags & FNM_PERIOD != 0, flags) {
                return true;
            }
        } else {
            let start = pi - 1;
            if c == b'\\' && flags & FNM_NOESCAPE == 0 {
                c = self.pat(pi);
            }
            let c = fold(c, flags);
            let mut nlp = no_leading_period;
            while ni < endp {
                if fold(s[ni], flags) == c && self.run(start, ni, nlp, flags2) {
                    return true;
                }
                ni += 1;
                nlp = false;
            }
        }
        false
    }

    /// The `[` case; `pi` points just past the bracket.
    #[allow(clippy::too_many_lines)]
    fn bracket(&self, mut pi: usize, ni: usize, no_leading_period: bool, flags: i32) -> Bracket {
        let s = self.s;
        let noescape = flags & FNM_NOESCAPE != 0;
        if ni == s.len() {
            return Bracket::NoMatch;
        }
        let n = s[ni];
        if (n == b'.' && no_leading_period) || (n == b'/' && flags & FNM_PATHNAME != 0) {
            return Bracket::NoMatch;
        }
        let not = matches!(self.pat(pi), b'!' | b'^');
        if not {
            pi += 1;
        }
        let fnc = fold(n, flags);

        let mut c = self.pat(pi);
        pi += 1;
        let matched = 'outer: loop {
            // `Some(ch)` means "continue at normal_bracket with c = ch".
            let mut normal: Option<u8> = None;
            if !noescape && c == b'\\' {
                if self.pat(pi) == 0 {
                    return Bracket::NoMatch;
                }
                normal = Some(fold(self.pat(pi), flags));
                pi += 1;
            } else if c == b'[' && self.pat(pi) == b':' {
                let startp = pi;
                let mut name = Vec::new();
                let mut is_class = true;
                loop {
                    if name.len() == CHAR_CLASS_MAX_LENGTH {
                        return Bracket::NoMatch;
                    }
                    pi += 1;
                    let ch = self.pat(pi);
                    if ch == b':' && self.pat(pi + 1) == b']' {
                        pi += 2;
                        break;
                    }
                    // glibc's test is `c < 'a' || c >= 'z'` (sic).
                    if !is_class_name_char(ch) {
                        // Not a class name: match '[' as a normal character.
                        pi = startp;
                        is_class = false;
                        break;
                    }
                    name.push(ch);
                }
                if is_class {
                    let Some(class) = char_class(&name) else {
                        return Bracket::NoMatch;
                    };
                    if class(n) {
                        break 'outer true;
                    }
                    c = self.pat(pi);
                    pi += 1;
                } else {
                    normal = Some(b'[');
                }
            } else if c == 0 {
                return Bracket::Literal;
            } else if c == b'[' && self.pat(pi) == b'.' {
                // Collating symbol: only single characters in the C locale.
                let startp = pi;
                let mut len = 0usize;
                loop {
                    pi += 1;
                    let ch = self.pat(pi);
                    if ch == b'.' && self.pat(pi + 1) == b']' {
                        pi += 2;
                        break;
                    }
                    if ch == 0 {
                        return Bracket::NoMatch;
                    }
                    len += 1;
                }
                let is_range = self.pat(pi) == b'-' && self.pat(pi + 1) != 0;
                if len != 1 {
                    return Bracket::NoMatch;
                }
                let sym = self.pat(startp + 1);
                if !is_range && n == sym {
                    break 'outer true;
                }
                let cold = sym;
                c = self.pat(pi);
                pi += 1;
                match self.range(&mut pi, &mut c, cold, fnc, flags) {
                    Some(true) => break 'outer true,
                    Some(false) => {}
                    None => return Bracket::NoMatch,
                }
            } else if c == b'[' && self.pat(pi) == b'=' {
                let startp = pi;
                let sym = self.pat(pi + 1);
                if sym == 0 || self.pat(pi + 2) != b'=' || self.pat(pi + 3) != b']' {
                    pi = startp;
                    normal = Some(b'[');
                } else {
                    pi += 4;
                    if n == sym {
                        break 'outer true;
                    }
                    c = self.pat(pi);
                    pi += 1;
                }
            } else {
                normal = Some(fold(c, flags));
            }

            if let Some(ch) = normal {
                // normal_bracket:
                let is_range =
                    self.pat(pi) == b'-' && self.pat(pi + 1) != 0 && self.pat(pi + 1) != b']';
                if !is_range && ch == fnc {
                    break 'outer true;
                }
                let cold = ch;
                c = self.pat(pi);
                pi += 1;
                match self.range(&mut pi, &mut c, cold, fnc, flags) {
                    Some(true) => break 'outer true,
                    Some(false) => {}
                    None => return Bracket::NoMatch,
                }
            }

            if c == b']' {
                break false;
            }
        };

        if !matched {
            return if not {
                Bracket::Matched(pi)
            } else {
                Bracket::NoMatch
            };
        }

        // Skip the rest of the [...] that already matched.
        loop {
            c = self.pat(pi);
            pi += 1;
            if c == 0 {
                // [... (unterminated) loses.
                return Bracket::NoMatch;
            }
            if !noescape && c == b'\\' {
                if self.pat(pi) == 0 {
                    return Bracket::NoMatch;
                }
                pi += 1;
            } else if c == b'[' && self.pat(pi) == b':' {
                let startp = pi;
                let mut count = 0usize;
                let mut is_class = true;
                loop {
                    pi += 1;
                    let ch = self.pat(pi);
                    count += 1;
                    if count == CHAR_CLASS_MAX_LENGTH {
                        return Bracket::NoMatch;
                    }
                    if ch == b':' && self.pat(pi + 1) == b']' {
                        break;
                    }
                    // glibc's test is `c < 'a' || c >= 'z'` (sic).
                    if !is_class_name_char(ch) {
                        pi = startp;
                        is_class = false;
                        break;
                    }
                }
                if !is_class {
                    // ignore_next: re-read from the ':'.
                    continue;
                }
                pi += 2;
                c = self.pat(pi);
                pi += 1;
            } else if c == b'[' && self.pat(pi) == b'=' {
                if self.pat(pi + 1) == 0 {
                    return Bracket::NoMatch;
                }
                if self.pat(pi + 2) != b'=' || self.pat(pi + 3) != b']' {
                    return Bracket::NoMatch;
                }
                pi += 4;
                c = self.pat(pi);
                pi += 1;
            } else if c == b'[' && self.pat(pi) == b'.' {
                loop {
                    pi += 1;
                    let ch = self.pat(pi);
                    if ch == 0 {
                        return Bracket::NoMatch;
                    }
                    if ch == b'.' && self.pat(pi + 1) == b']' {
                        break;
                    }
                }
                pi += 2;
                c = self.pat(pi);
                pi += 1;
            }
            if c == b']' {
                break;
            }
        }
        if not {
            Bracket::NoMatch
        } else {
            Bracket::Matched(pi)
        }
    }

    /// The range tail shared by ordinary and collating bracket elements:
    /// if `c` is `-` (and not followed by `]`) consume a range ending.
    /// Returns `Some(true)` when the range matched, `None` for a malformed
    /// pattern (no match), `Some(false)` otherwise (with `c` advanced).
    fn range(&self, pi: &mut usize, c: &mut u8, cold: u8, fnc: u8, flags: i32) -> Option<bool> {
        if *c == b'-' && self.pat(*pi) != b']' {
            let mut cend = self.pat(*pi);
            *pi += 1;
            if flags & FNM_NOESCAPE == 0 && cend == b'\\' {
                cend = self.pat(*pi);
                *pi += 1;
            }
            if cend == 0 {
                return None;
            }
            let cend = fold(cend, flags);
            if cold <= fnc && fnc <= cend {
                return Some(true);
            }
            *c = self.pat(*pi);
            *pi += 1;
        }
        Some(false)
    }
}

/// glibc accepts class-name characters with `c >= 'a' && c < 'z'` (sic:
/// `z` excluded), stopping the class parse otherwise.
fn is_class_name_char(c: u8) -> bool {
    matches!(c, b'a'..=b'y')
}

fn fold(c: u8, flags: i32) -> u8 {
    if flags & FNM_CASEFOLD != 0 {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

fn no_leading_period_flags(flags: i32) -> bool {
    flags & (FNM_PATHNAME | FNM_PERIOD) == (FNM_PATHNAME | FNM_PERIOD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert!(fnmatch(b"*.txt", b"file.txt", 0));
        assert!(!fnmatch(b"*.txt", b"file.txd", 0));
        assert!(fnmatch(b"f?le", b"file", 0));
        assert!(fnmatch(b"[a-c]x", b"bx", 0));
        assert!(fnmatch(b"[!a-c]x", b"dx", 0));
        assert!(fnmatch(b"[^a-c]x", b"dx", 0));
        assert!(fnmatch(b"[[:digit:]]", b"7", 0));
        assert!(fnmatch(b"\\*", b"*", 0));
        assert!(!fnmatch(b"\\*", b"a", 0));
        assert!(fnmatch(b"\\*", b"\\a", FNM_NOESCAPE) == fnmatch(b"\\*", b"\\a", FNM_NOESCAPE));
        assert!(!fnmatch(b"*", b"a/b", FNM_PATHNAME));
        assert!(fnmatch(b"*/*", b"a/b", FNM_PATHNAME));
        assert!(!fnmatch(b"*", b".x", FNM_PERIOD));
        assert!(fnmatch(b"*", b".x", 0));
        assert!(fnmatch(b"ABC", b"abc", FNM_CASEFOLD));
        assert!(fnmatch(b"[", b"[", 0));
        assert!(!fnmatch(b"a\\", b"a\\", 0));
    }

    #[test]
    fn php_length_checks() {
        let long = vec![b'a'; MAXPATHLEN];
        assert_eq!(
            php_fnmatch(b"*", &long, 0),
            Err("Filename exceeds the maximum allowed length of 4096 characters".to_string())
        );
        assert_eq!(
            php_fnmatch(&long, b"a", 0),
            Err("Pattern exceeds the maximum allowed length of 4096 characters".to_string())
        );
        assert_eq!(php_fnmatch(b"a*", b"abc", 0), Ok(true));
    }

    /// Oracle table generated with `/usr/bin/php8.5` (glibc fnmatch).
    #[test]
    fn oracle_table() {
        let data = include_str!("../tests/data/fnmatch_oracle.txt");
        let mut count = 0;
        for line in data.lines() {
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let pattern = unhex(parts.next().unwrap());
            let string = unhex(parts.next().unwrap());
            let flags: i32 = parts.next().unwrap().parse().unwrap();
            let expected = parts.next().unwrap() == "1";
            assert_eq!(
                fnmatch(&pattern, &string, flags),
                expected,
                "fnmatch({:?}, {:?}, {flags})",
                String::from_utf8_lossy(&pattern),
                String::from_utf8_lossy(&string)
            );
            count += 1;
        }
        assert!(count > 1000);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
