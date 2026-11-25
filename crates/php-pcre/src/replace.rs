// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `preg_replace()`, `preg_filter()`, `preg_replace_callback()` and
//! `preg_replace_callback_array()` (`php_pcre_replace_impl`,
//! `php_pcre_replace_func_impl`, `php_pcre_replace_array`).

use std::sync::Arc;

use crate::cache::{MatchConfig, Pcre};
use crate::error::{CompileError, PregError};
use crate::exec::{pairs, populate_subpat_array, Exec};
use crate::ffi;
use crate::regex::Regex;
use crate::value::MatchArray;

/// One piece of a parsed replacement string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacementPiece {
    /// Bytes copied verbatim.
    Literal(Vec<u8>),
    /// A back-reference (`\n`, `$n`, `${n}`, 0 ≤ n ≤ 99).
    Group(usize),
}

/// A replacement string parsed per PHP's rules (`preg_get_backref` and the
/// copy loop of `php_pcre_replace_impl`):
///
/// * `\0`–`\99`, `$0`–`$99` and `${0}`–`${99}` are back-references (at most
///   two digits are consumed);
/// * a `\` or `$` directly following a literal backslash replaces that
///   backslash (so `\\` → `\` and `\$1` → `$1`);
/// * everything else is literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pieces: Vec<ReplacementPiece>,
    simple: bool,
}

/// `preg_get_backref`: `at(i)` yields 0 past the end, like the C string's NUL.
fn get_backref(s: &[u8], pos: usize) -> Option<(usize, usize)> {
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    if at(pos + 1) == 0 {
        return None;
    }
    let mut walk = pos;
    let mut in_brace = false;
    if at(walk) == b'$' && at(walk + 1) == b'{' {
        in_brace = true;
        walk += 1;
    }
    walk += 1;
    let mut backref = if at(walk).is_ascii_digit() {
        usize::from(at(walk) - b'0')
    } else {
        return None;
    };
    walk += 1;
    if at(walk).is_ascii_digit() {
        backref = backref * 10 + usize::from(at(walk) - b'0');
        walk += 1;
    }
    if in_brace {
        if at(walk) != b'}' {
            return None;
        }
        walk += 1;
    }
    Some((backref, walk))
}

impl Replacement {
    /// Parses a replacement string.
    pub fn parse(replacement: &[u8]) -> Self {
        // The usual replacement has no `\` or `$`: one literal, made in one
        // allocation instead of grown byte by byte.
        if !replacement.iter().any(|&c| c == b'\\' || c == b'$') {
            let pieces = if replacement.is_empty() {
                Vec::new()
            } else {
                vec![ReplacementPiece::Literal(replacement.to_vec())]
            };
            return Self {
                pieces,
                simple: true,
            };
        }
        let mut pieces = Vec::new();
        let mut lit: Vec<u8> = Vec::new();
        let mut simple = true;
        let mut walk = 0;
        let mut walk_last = 0u8;
        while walk < replacement.len() {
            let c = replacement[walk];
            if c == b'\\' || c == b'$' {
                simple = false;
                if walk_last == b'\\' {
                    // `*(walkbuf-1) = *walk++`: overwrite the literal backslash.
                    if let Some(last) = lit.last_mut() {
                        *last = c;
                    }
                    walk += 1;
                    walk_last = 0;
                    continue;
                }
                if let Some((n, next)) = get_backref(replacement, walk) {
                    if !lit.is_empty() {
                        pieces.push(ReplacementPiece::Literal(std::mem::take(&mut lit)));
                    }
                    pieces.push(ReplacementPiece::Group(n));
                    walk = next;
                    continue;
                }
            }
            lit.push(c);
            walk += 1;
            walk_last = c;
        }
        if !lit.is_empty() {
            pieces.push(ReplacementPiece::Literal(lit));
        }
        Self { pieces, simple }
    }

    /// The parsed pieces.
    pub fn pieces(&self) -> &[ReplacementPiece] {
        &self.pieces
    }

    /// True when the string contains no `\` or `$` at all.
    pub fn is_simple(&self) -> bool {
        self.simple
    }

    /// The length of [`Replacement::expand`]'s output for one match.
    pub(crate) fn expanded_len(&self, ov: &[usize], count: usize) -> usize {
        self.pieces
            .iter()
            .map(|piece| match piece {
                ReplacementPiece::Literal(l) => l.len(),
                ReplacementPiece::Group(n) => {
                    if *n < count && ov[2 * n] != ffi::PCRE2_UNSET {
                        ov[2 * n + 1].saturating_sub(ov[2 * n])
                    } else {
                        0
                    }
                }
            })
            .sum()
    }

    /// Appends the substitution for one match: `count` is PCRE2's return
    /// (references to groups `>= count` or unset expand to nothing).
    pub(crate) fn expand(&self, out: &mut Vec<u8>, subject: &[u8], ov: &[usize], count: usize) {
        for piece in &self.pieces {
            match piece {
                ReplacementPiece::Literal(l) => out.extend_from_slice(l),
                ReplacementPiece::Group(n) => {
                    if *n < count && ov[2 * n] != ffi::PCRE2_UNSET {
                        if let Some(group) = subject.get(ov[2 * n]..ov[2 * n + 1]) {
                            out.extend_from_slice(group);
                        }
                    }
                }
            }
        }
    }
}

/// Result of replacing in one subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceOutcome {
    /// The new string, or the error that made PHP return `null` for it.
    /// `preg_last_error()` becomes `NoError` or this error.
    pub result: Result<Vec<u8>, PregError>,
    /// Replacements performed (also counted when the call later failed).
    pub count: usize,
}

impl ReplaceOutcome {
    /// The `preg_last_error()` value after this call.
    pub fn last_error(&self) -> PregError {
        self.result
            .as_ref()
            .err()
            .copied()
            .unwrap_or(PregError::NoError)
    }
}

/// A replacement's result before it is put together: what
/// `php_pcre_replace_impl` has in `result` and `last_end_offset` when it
/// reaches `not_matched:`. The caller makes the final string from it in
/// one allocation of the exact size (php-src's closing
/// `zend_string_realloc(result, alloc_len)` + `memcpy` of the tail), or
/// returns the subject itself when nothing was replaced
/// (`zend_string_copy(subject_str)`), instead of copying the subject into a
/// buffer that is then thrown away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceSplit {
    /// `Ok(None)`: nothing was replaced, the result is the subject.
    /// `Ok(Some((head, rest)))`: the result is `head` followed by
    /// `subject[rest..]`. `Err`: PHP's `null`, with this error.
    pub result: Result<Option<(Vec<u8>, usize)>, PregError>,
    /// Replacements performed (also counted when the call later failed).
    pub count: usize,
}

impl ReplaceSplit {
    /// The `preg_last_error()` value after this call.
    pub fn last_error(&self) -> PregError {
        self.result
            .as_ref()
            .err()
            .copied()
            .unwrap_or(PregError::NoError)
    }

    /// The result's length: the head and the subject's tail after it
    /// (the subject's own length when nothing was replaced).
    pub fn result_len(&self, subject: &[u8]) -> Option<usize> {
        match &self.result {
            Ok(None) => Some(subject.len()),
            Ok(Some((head, rest))) => Some(head.len() + subject.len().saturating_sub(*rest)),
            Err(_) => None,
        }
    }

    /// The whole result as one byte vector.
    pub fn into_outcome(self, subject: &[u8]) -> ReplaceOutcome {
        let result = self.result.map(|parts| match parts {
            None => subject.to_vec(),
            Some((mut head, rest)) => {
                head.extend_from_slice(subject.get(rest..).unwrap_or_default());
                head
            }
        });
        ReplaceOutcome {
            result,
            count: self.count,
        }
    }
}

/// Converts PHP's `int $limit` to the `size_t` PHP uses internally
/// (`-1`, and any negative value, means unlimited).
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn limit_from_php(limit: i64) -> usize {
    limit as usize
}

/// What to do for each match in [`replace_loop`].
trait Substitute {
    /// Appends the replacement; `false` aborts (callback threw).
    fn substitute(&mut self, ex: &Exec<'_>, count: usize, out: &mut Vec<u8>) -> bool;

    /// The length the substitution will have, when known before it is
    /// made (0 otherwise): it sizes the result on the first match.
    fn len_hint(&self, _ex: &Exec<'_>, _count: usize) -> usize {
        0
    }
}

struct Static<'r>(&'r Replacement);

impl Substitute for Static<'_> {
    fn substitute(&mut self, ex: &Exec<'_>, count: usize, out: &mut Vec<u8>) -> bool {
        self.0.expand(out, ex.subject, ex.ov(), count);
        true
    }

    fn len_hint(&self, ex: &Exec<'_>, count: usize) -> usize {
        self.0.expanded_len(ex.ov(), count)
    }
}

struct Callback<'n, F> {
    f: F,
    names: Option<&'n [Option<Arc<[u8]>>]>,
    num_subpats: usize,
    flags: i64,
}

impl<F: FnMut(MatchArray) -> Option<Vec<u8>>> Substitute for Callback<'_, F> {
    fn substitute(&mut self, ex: &Exec<'_>, count: usize, out: &mut Vec<u8>) -> bool {
        let arr = populate_subpat_array(
            ex.subject,
            ex.ov(),
            self.names,
            self.num_subpats,
            count,
            ex.mark(),
            self.flags,
        );
        match (self.f)(arr) {
            Some(s) => {
                out.extend_from_slice(&s);
                true
            }
            None => false,
        }
    }
}

/// The loop shared by `php_pcre_replace_impl` and `php_pcre_replace_func_impl`.
fn replace_loop(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    mut limit: usize,
    sub: &mut dyn Substitute,
) -> ReplaceSplit {
    let mut replaced = 0usize;
    let num_subpats = re.capture_count() + 1;
    let Some(mut ex) = Exec::new(re, cfg, subject) else {
        return ReplaceSplit {
            result: Err(PregError::Internal),
            count: 0,
        };
    };
    let len = subject.len();
    let mut out: Option<Vec<u8>> = None;
    let mut last_end = 0usize;
    let mut start = 0usize;

    let mut count = ex.run(start, ex.first_options(start));
    loop {
        if count >= 0 && limit > 0 {
            let mut n = pairs(count, num_subpats);
            // `matched:` label.
            loop {
                let (s, e) = ex.span();
                if e < s {
                    return ReplaceSplit {
                        result: Err(PregError::Internal),
                        count: replaced,
                    };
                }
                replaced += 1;
                // The result is made on the first match with room for
                // twice what it then needs, as `php_pcre_replace_impl`
                // allocates `2 * new_len`; it grows by doubling after that
                // and is cut to its length at the end.
                let buf = out.get_or_insert_with(|| {
                    let new_len = (s - last_end.min(s)).saturating_add(sub.len_hint(&ex, n));
                    Vec::with_capacity(new_len.saturating_mul(2))
                });
                buf.extend_from_slice(subject.get(last_end..s).unwrap_or_default());
                if !sub.substitute(&ex, n, buf) {
                    // Exception in the callback: `goto error` with count >= 0.
                    return ReplaceSplit {
                        result: Err(PregError::Internal),
                        count: replaced,
                    };
                }
                limit -= 1;
                start = e;
                last_end = e;
                if start == s {
                    count = ex.retry_not_empty(start);
                    if count >= 0 && limit > 0 {
                        n = pairs(count, num_subpats);
                        continue;
                    } else if count == ffi::PCRE2_ERROR_NOMATCH || limit == 0 {
                        if start < len {
                            start += ex.unit_length(start);
                        } else {
                            return finish(out, last_end, replaced);
                        }
                    } else {
                        return ReplaceSplit {
                            result: Err(PregError::from_pcre2(count)),
                            count: replaced,
                        };
                    }
                }
                break;
            }
        } else if count == ffi::PCRE2_ERROR_NOMATCH || limit == 0 {
            return finish(out, last_end, replaced);
        } else {
            return ReplaceSplit {
                result: Err(PregError::from_pcre2(count)),
                count: replaced,
            };
        }
        count = ex.next(start);
    }
}

/// `not_matched:` — the result so far and where the subject's tail starts
/// (none: the subject untouched).
fn finish(out: Option<Vec<u8>>, last_end: usize, count: usize) -> ReplaceSplit {
    ReplaceSplit {
        result: Ok(out.map(|head| (head, last_end))),
        count,
    }
}

/// `php_pcre_replace_impl`: replace up to `limit` matches (see
/// [`limit_from_php`]) of `re` in `subject` by `replacement`.
pub fn replace(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    replacement: &Replacement,
    limit: usize,
) -> ReplaceOutcome {
    replace_split(re, cfg, subject, replacement, limit).into_outcome(subject)
}

/// [`replace`] with the result left in its parts ([`ReplaceSplit`]).
pub fn replace_split(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    replacement: &Replacement,
    limit: usize,
) -> ReplaceSplit {
    replace_loop(re, cfg, subject, limit, &mut Static(replacement))
}

/// `php_pcre_replace_func_impl`: like [`replace`] but each replacement comes
/// from `callback`, which receives the capture array PHP passes to the user
/// callback (named groups, `MARK`, and the `PREG_OFFSET_CAPTURE` /
/// `PREG_UNMATCHED_AS_NULL` bits of `flags`).  Returning `None` signals that
/// the callback threw; the call then fails with [`PregError::Internal`].
///
/// The callback may freely re-enter the VM (and other `preg_*` calls): this
/// function only borrows the regex and a copy of the config.  To reproduce
/// PHP's `preg_last_error()` exactly, store `NoError` *before* calling and
/// store the error (if any) afterwards.
pub fn replace_callback<F>(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    limit: usize,
    flags: i64,
    callback: F,
) -> ReplaceOutcome
where
    F: FnMut(MatchArray) -> Option<Vec<u8>>,
{
    replace_callback_split(re, cfg, subject, limit, flags, callback).into_outcome(subject)
}

/// [`replace_callback`] with the result left in its parts
/// ([`ReplaceSplit`]).
pub fn replace_callback_split<F>(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    limit: usize,
    flags: i64,
    callback: F,
) -> ReplaceSplit
where
    F: FnMut(MatchArray) -> Option<Vec<u8>>,
{
    let names = (re.name_count() > 0).then(|| re.group_names());
    let mut cb = Callback {
        f: callback,
        names,
        num_subpats: re.capture_count() + 1,
        flags,
    };
    replace_loop(re, cfg, subject, limit, &mut cb)
}

/// The replacement argument of `preg_replace()` when the pattern is an array.
#[derive(Debug, Clone, Copy)]
pub enum Replacements<'a> {
    /// A single string used for every pattern.
    Single(&'a [u8]),
    /// One string per pattern (in iteration order); missing ones are `""`.
    PerPattern(&'a [&'a [u8]]),
}

/// `php_pcre_replace_array` / `php_replace_in_subject` for one subject: apply
/// each pattern in turn to the running result.  Compile failures are
/// reported through `on_compile_error` (emit the warning there) and make the
/// subject's result `null` with `PREG_INTERNAL_ERROR`.
///
/// For `preg_filter()`, keep the result only when the returned `count` is > 0.
pub fn replace_array(
    pcre: &mut Pcre,
    patterns: &[&[u8]],
    replacements: Replacements<'_>,
    subject: &[u8],
    limit: usize,
    on_compile_error: &mut dyn FnMut(&CompileError),
) -> ReplaceOutcome {
    let mut current = subject.to_vec();
    let mut total = 0usize;
    for (i, pattern) in patterns.iter().enumerate() {
        let rep_bytes: &[u8] = match replacements {
            Replacements::Single(s) => s,
            Replacements::PerPattern(list) => list.get(i).copied().unwrap_or(b""),
        };
        let re = match pcre.compile(pattern) {
            Ok(re) => re,
            Err(e) => {
                on_compile_error(&e);
                return ReplaceOutcome {
                    result: Err(PregError::Internal),
                    count: total,
                };
            }
        };
        let cfg = pcre.config();
        let outcome = replace(&re, &cfg, &current, &Replacement::parse(rep_bytes), limit);
        total += outcome.count;
        match outcome.result {
            Ok(s) => current = s,
            Err(e) => {
                return ReplaceOutcome {
                    result: Err(e),
                    count: total,
                }
            }
        }
    }
    ReplaceOutcome {
        result: Ok(current),
        count: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReplacementPiece::{Group, Literal};

    fn lit(s: &str) -> ReplacementPiece {
        Literal(s.as_bytes().to_vec())
    }

    /// A replacement without `\` or `$` is one literal (none when
    /// empty), simple, as the byte-by-byte walk makes it.
    #[test]
    fn parse_plain_literal() {
        let r = Replacement::parse(b"N-x y");
        assert_eq!(r.pieces(), &[lit("N-x y")]);
        assert!(r.is_simple());
        let r = Replacement::parse(b"");
        assert!(r.pieces().is_empty() && r.is_simple());
        assert!(!Replacement::parse(b"a$").is_simple());
    }

    /// `expanded_len` is the length `expand` appends, unset and missing
    /// groups included; the result is the same with the up-front room.
    #[test]
    fn expanded_len_matches_expand() {
        let r = Replacement::parse(b"<$1|$2|$9>");
        let subject = b"xxabyy";
        // Group 1 = "ab" (2..4), group 2 unset; count 3.
        let ov = [2, 4, 2, 4, ffi::PCRE2_UNSET, ffi::PCRE2_UNSET];
        let mut out = Vec::new();
        r.expand(&mut out, subject, &ov, 3);
        assert_eq!(out, b"<ab||>");
        assert_eq!(r.expanded_len(&ov, 3), out.len());
        let re = Regex::compile(b"/(a)(b)?/").unwrap();
        let cfg = MatchConfig::default();
        let out = replace(
            &re,
            &cfg,
            b"zaab",
            &Replacement::parse(b"[$2$1]"),
            usize::MAX,
        );
        assert_eq!(out.result, Ok(b"z[a][ba]".to_vec()));
        assert_eq!(out.count, 2);
    }

    #[test]
    fn parse_backrefs() {
        assert_eq!(
            Replacement::parse(b"a$1b").pieces(),
            &[lit("a"), Group(1), lit("b")]
        );
        assert_eq!(
            Replacement::parse(b"\\12x").pieces(),
            &[Group(12), lit("x")]
        );
        assert_eq!(Replacement::parse(b"$123").pieces(), &[Group(12), lit("3")]);
        assert_eq!(Replacement::parse(b"${1}0").pieces(), &[Group(1), lit("0")]);
        assert_eq!(Replacement::parse(b"${1").pieces(), &[lit("${1")]);
        assert_eq!(Replacement::parse(b"${a}").pieces(), &[lit("${a}")]);
        assert_eq!(Replacement::parse(b"$").pieces(), &[lit("$")]);
        assert_eq!(Replacement::parse(b"\\").pieces(), &[lit("\\")]);
        assert!(Replacement::parse(b"abc").is_simple());
        assert!(!Replacement::parse(b"a$").is_simple());
    }

    #[test]
    fn parse_escapes() {
        assert_eq!(Replacement::parse(b"\\\\").pieces(), &[lit("\\")]);
        assert_eq!(Replacement::parse(b"\\$1").pieces(), &[lit("$1")]);
        assert_eq!(Replacement::parse(b"\\\\1").pieces(), &[lit("\\1")]);
        assert_eq!(
            Replacement::parse(b"\\\\\\1").pieces(),
            &[lit("\\"), Group(1)]
        );
        assert_eq!(Replacement::parse(b"a\\b").pieces(), &[lit("a\\b")]);
        // A NUL right after the sigil stops back-reference parsing, like C.
        assert_eq!(Replacement::parse(b"$\x001").pieces(), &[lit("$\x001")]);
    }

    #[test]
    fn simple_replace() {
        let re = Regex::compile(b"/(a)(x)?/").unwrap();
        let cfg = MatchConfig::default();
        let out = replace(
            &re,
            &cfg,
            b"banana",
            &Replacement::parse(b"[$1|$2|$3]"),
            usize::MAX,
        );
        assert_eq!(out.result.unwrap(), b"b[a||]n[a||]n[a||]");
        assert_eq!(out.count, 3);
        let out = replace(&re, &cfg, b"banana", &Replacement::parse(b"_"), 2);
        assert_eq!(out.result.unwrap(), b"b_n_na");
        assert_eq!(out.count, 2);
        let out = replace(&re, &cfg, b"xyz", &Replacement::parse(b"_"), usize::MAX);
        assert_eq!(out.result.unwrap(), b"xyz");
        assert_eq!(out.count, 0);
    }

    /// The split result: no match leaves the subject alone (no copy), a
    /// match gives the head and where the subject's tail starts, and
    /// [`ReplaceSplit::into_outcome`] gives [`replace`]'s bytes.
    #[test]
    fn split_result() {
        let re = Regex::compile(b"/o/").unwrap();
        let cfg = MatchConfig::default();
        let rep = Replacement::parse(b"0");
        let out = replace_split(&re, &cfg, b"  Hello World  ", &rep, usize::MAX);
        assert_eq!(out.count, 2);
        assert_eq!(out.result, Ok(Some((b"  Hell0 W0".to_vec(), 9))));
        assert_eq!(out.result_len(b"  Hello World  "), Some(15));
        assert_eq!(
            out.into_outcome(b"  Hello World  ").result.unwrap(),
            b"  Hell0 W0rld  "
        );
        let out = replace_split(&re, &cfg, b"abc", &rep, usize::MAX);
        assert_eq!((out.result.clone(), out.count), (Ok(None), 0));
        assert_eq!(out.result_len(b"abc"), Some(3));
        assert_eq!(out.into_outcome(b"abc").result.unwrap(), b"abc");
        let out = replace_split(&re, &cfg, b"foo", &rep, 1);
        assert_eq!(out.result, Ok(Some((b"f0".to_vec(), 2))));
        let out =
            replace_callback_split(&re, &cfg, b"oxo", usize::MAX, 0, |_| Some(b"[]".to_vec()));
        assert_eq!(out.result, Ok(Some((b"[]x[]".to_vec(), 3))));
        assert_eq!(out.result_len(b"oxo"), Some(5));
    }

    #[test]
    fn empty_matches() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"/x*/").unwrap();
        let out = replace(&re, &cfg, b"abc", &Replacement::parse(b"-"), usize::MAX);
        assert_eq!(out.result.unwrap(), b"-a-b-c-");
        let re = Regex::compile(b"//u").unwrap();
        let out = replace(
            &re,
            &cfg,
            "é€".as_bytes(),
            &Replacement::parse(b"|"),
            usize::MAX,
        );
        assert_eq!(out.result.unwrap(), "|é|€|".as_bytes());
        assert_eq!(out.count, 3);
        let re = Regex::compile(b"/x*/").unwrap();
        let out = replace(&re, &cfg, b"abc", &Replacement::parse(b"-"), 1);
        assert_eq!(out.result.unwrap(), b"-abc");
    }

    #[test]
    fn callback_and_errors() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"/\\d+/").unwrap();
        let out = replace_callback(&re, &cfg, b"a1b22", usize::MAX, 0, |m| {
            let mut v = m.get_index(0).unwrap().as_bytes().unwrap().to_vec();
            v.reverse();
            v.push(b'!');
            Some(v)
        });
        assert_eq!(out.result.unwrap(), b"a1!b22!");
        let out = replace_callback(&re, &cfg, b"a1b22", usize::MAX, 0, |_| None);
        assert_eq!(out.result, Err(PregError::Internal));
        assert_eq!(out.count, 1);
        let re = Regex::compile(b"/./u").unwrap();
        let out = replace(&re, &cfg, b"\xff", &Replacement::parse(b""), usize::MAX);
        assert_eq!(out.result, Err(PregError::BadUtf8));
    }

    #[test]
    fn limits() {
        assert_eq!(limit_from_php(-1), usize::MAX);
        assert_eq!(limit_from_php(0), 0);
        assert_eq!(limit_from_php(3), 3);
    }

    #[test]
    fn array_form() {
        let mut pcre = Pcre::new();
        let mut errs = Vec::new();
        let out = replace_array(
            &mut pcre,
            &[b"/a/", b"/b/", b"/c/"],
            Replacements::PerPattern(&[b"b", b"c"]),
            b"abc",
            usize::MAX,
            &mut |e| errs.push(e.to_string()),
        );
        assert_eq!(out.result.unwrap(), b"");
        assert_eq!(out.count, 6);
        let out = replace_array(
            &mut pcre,
            &[b"/a/", b"bad"],
            Replacements::Single(b"x"),
            b"abc",
            usize::MAX,
            &mut |e| errs.push(e.to_string()),
        );
        assert_eq!(out.result, Err(PregError::Internal));
        assert_eq!(
            errs,
            vec!["Delimiter must not be alphanumeric, backslash, or NUL byte"]
        );
    }
}
