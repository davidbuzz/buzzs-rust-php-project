// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `preg_match()` / `preg_match_all()` (`php_pcre_match_impl`).
//!
//! Matching runs in two phases so that callers can account for memory
//! before any capture is copied:
//!
//! 1. [`match_one_spans`] / [`match_all_spans`] run PCRE2 and record only
//!    byte offsets (plus `MARK` names) in a [`MatchSpans`] that borrows the
//!    subject;
//! 2. [`MatchSpans::into_outcome`] materialises PHP's `$matches` array.
//!
//! [`match_one`] / [`match_all`] simply do both.

use std::sync::Arc;

use crate::cache::MatchConfig;
use crate::error::PregError;
use crate::exec::{match_value, off, offset_pair, pairs, populate_subpat_array, Exec};
use crate::ffi;
use crate::regex::Regex;
use crate::value::{Key, MatchArray, Value};
use crate::{PREG_OFFSET_CAPTURE, PREG_PATTERN_ORDER, PREG_SET_ORDER, PREG_UNMATCHED_AS_NULL};

/// Why a match call returned something other than an int.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFailure {
    /// `ValueError`: `Argument #4 ($flags) must be a PREG_* constant`.
    InvalidFlags,
    /// `ValueError`: `Argument #5 ($offset) must be greater than -9223372036854775808`.
    InvalidOffset,
    /// The function returns `false` (see [`MatchOutcome::last_error`] / [`MatchOutcome::warning`]).
    Failed,
}

/// Everything `preg_match()` / `preg_match_all()` produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    /// `Ok(n)` → `int(n)`; `Err(_)` → see [`MatchFailure`].
    pub result: Result<usize, MatchFailure>,
    /// New value of `$matches` when the caller passed it; `None` means PHP
    /// leaves the variable untouched (only for [`MatchFailure::InvalidOffset`]).
    /// Note that it can be partially filled even when `result` is an error.
    pub matches: Option<MatchArray>,
    /// The value to store into `preg_last_error()`, or `None` to leave it as is.
    pub last_error: Option<PregError>,
    /// An `E_WARNING` to emit (`"Get subpatterns list failed"`).
    pub warning: Option<&'static str>,
    /// PCRE2 validated the whole subject as UTF-8 (a `/u` pattern matched
    /// from offset 0 without error): the caller may now flag the string as
    /// known-valid, like PHP's `GC_ADD_FLAGS(subject, IS_STR_VALID_UTF8)`.
    pub subject_valid_utf8: bool,
}

/// Offsets of the single match of `preg_match()` kept in place in
/// [`MatchSpans`] (eight pairs: the whole match and seven groups) instead
/// of a vector: php-src reads them from its global match data, without an
/// allocation.
const INLINE_OV: usize = 16;

/// One recorded match: its ovector (`count` pairs) and `MARK`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    /// Index of the first offset in [`MatchSpans::ov`], or `None` when
    /// the offsets are in [`MatchSpans::ov_inline`].
    at: Option<usize>,
    /// Pairs filled by PCRE2 (highest matched group + 1).
    count: usize,
    mark: Option<Vec<u8>>,
}

/// How the matching phase ended before any array is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Early {
    /// Matching ran (successfully or not).
    No,
    /// `$matches` is left untouched and nothing was matched.
    Untouched,
    /// `$matches` becomes `[]` and nothing was matched.
    Empty,
}

/// The offsets-only result of a `preg_match()` / `preg_match_all()` run,
/// borrowing the subject.  Nothing proportional to the captured text has
/// been allocated yet: inspect [`MatchSpans::matches`] /
/// [`MatchSpans::captured_bytes`] (e.g. to reserve against `memory_limit`)
/// and then call [`MatchSpans::into_outcome`].
#[derive(Debug, Clone)]
pub struct MatchSpans<'s> {
    re: Regex,
    subject: &'s [u8],
    capture: bool,
    global: bool,
    order: i64,
    flags: i64,
    early: Early,
    result: Result<usize, MatchFailure>,
    last_error: Option<PregError>,
    warning: Option<&'static str>,
    subject_valid_utf8: bool,
    ov: Vec<usize>,
    /// The offsets of `single` when they fit.
    ov_inline: [usize; INLINE_OV],
    /// The match of `preg_match()` (no vector for one match).
    single: Option<Recorded>,
    /// The matches of `preg_match_all()`.
    recorded: Vec<Recorded>,
}

/// A borrowed view of one recorded match.
#[derive(Debug, Clone, Copy)]
pub struct MatchRef<'a> {
    subject: &'a [u8],
    ov: &'a [usize],
    mark: Option<&'a [u8]>,
}

impl<'a> MatchRef<'a> {
    /// Number of groups PCRE2 reported (highest matched group + 1); later
    /// groups are unmatched.
    pub fn count(&self) -> usize {
        self.ov.len() / 2
    }

    /// `(start, end)` byte offsets of group `i`, or `None` when unmatched.
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        let s = *self.ov.get(2 * i)?;
        if s == ffi::PCRE2_UNSET {
            None
        } else {
            Some((s, self.ov[2 * i + 1]))
        }
    }

    /// The bytes of group `i`, or `None` when unmatched.
    pub fn group(&self, i: usize) -> Option<&'a [u8]> {
        self.span(i).map(|(s, e)| &self.subject[s..e])
    }

    /// The `(*MARK)` name, if any.
    pub fn mark(&self) -> Option<&'a [u8]> {
        self.mark
    }
}

impl MatchSpans<'_> {
    /// What the PHP function returns (`Ok(n)` → `int(n)`).
    pub fn result(&self) -> Result<usize, MatchFailure> {
        self.result
    }

    /// The `preg_last_error()` value to store, or `None` to leave it as is.
    pub fn last_error(&self) -> Option<PregError> {
        self.last_error
    }

    /// An `E_WARNING` to emit.
    pub fn warning(&self) -> Option<&'static str> {
        self.warning
    }

    /// See [`MatchOutcome::subject_valid_utf8`].
    pub fn subject_valid_utf8(&self) -> bool {
        self.subject_valid_utf8
    }

    /// The recorded matches (empty unless `capture` was requested), in order.
    pub fn matches(&self) -> impl Iterator<Item = MatchRef<'_>> + '_ {
        self.single
            .iter()
            .chain(self.recorded.iter())
            .map(|r| MatchRef {
                subject: self.subject,
                ov: self.ov_of(r),
                mark: r.mark.as_deref(),
            })
    }

    /// The offsets of `r`.
    fn ov_of(&self, r: &Recorded) -> &[usize] {
        match r.at {
            Some(at) => &self.ov[at..at + 2 * r.count],
            None => &self.ov_inline[..2 * r.count],
        }
    }

    /// Number of recorded matches.
    pub fn match_count(&self) -> usize {
        self.recorded.len() + usize::from(self.single.is_some())
    }

    /// Total length of the strings [`MatchSpans::into_outcome`] will copy:
    /// every matched group once (named keys share the value with their index
    /// key, as PHP's refcounted zvals do) plus `MARK` names.
    pub fn captured_bytes(&self) -> u64 {
        self.matches()
            .map(|m| {
                let groups: u64 = (0..m.count())
                    .filter_map(|i| m.span(i))
                    .map(|(s, e)| (e - s) as u64)
                    .sum();
                groups + m.mark().map_or(0, |k| k.len() as u64)
            })
            .sum()
    }

    /// Materialises PHP's result (`$matches` and the return value).
    #[allow(clippy::too_many_lines)]
    pub fn into_outcome(self) -> MatchOutcome {
        let mut out = MatchOutcome {
            result: self.result,
            matches: None,
            last_error: self.last_error,
            warning: self.warning,
            subject_valid_utf8: self.subject_valid_utf8,
        };
        match self.early {
            Early::Untouched => return out,
            Early::Empty => {
                out.matches = Some(MatchArray::new());
                return out;
            }
            Early::No => {}
        }
        let subject = self.subject;
        let flags = self.flags;
        let offset_capture = flags & PREG_OFFSET_CAPTURE != 0;
        let unmatched_as_null = flags & PREG_UNMATCHED_AS_NULL != 0;
        let num_subpats = self.re.capture_count() + 1;
        let names: Option<&[Option<Arc<[u8]>>]> =
            (self.capture && self.re.name_count() > 0).then(|| self.re.group_names());
        let mut subpats = MatchArray::new();

        if !self.capture {
            out.matches = Some(subpats);
            return out;
        }
        let ov_of = |r: &Recorded| self.ov_of(r);

        if !self.global {
            if let Some(r) = &self.single {
                subpats = populate_subpat_array(
                    subject,
                    ov_of(r),
                    names,
                    num_subpats,
                    r.count,
                    r.mark.clone(),
                    flags,
                );
            }
        } else if self.order == PREG_PATTERN_ORDER {
            // A "Get subpatterns list failed" abort discards the sets.
            if self.warning.is_none() {
                let mut sets = vec![MatchArray::with_capacity(self.recorded.len()); num_subpats];
                let mut marks: Option<MatchArray> = None;
                for (idx, r) in self.recorded.iter().enumerate() {
                    let ov = ov_of(r);
                    for (i, set) in sets.iter_mut().enumerate() {
                        let (s, e) = if i < r.count {
                            (ov[2 * i], ov[2 * i + 1])
                        } else {
                            (ffi::PCRE2_UNSET, ffi::PCRE2_UNSET)
                        };
                        set.push(if offset_capture {
                            offset_pair(subject, s, e, unmatched_as_null)
                        } else {
                            match_value(subject, s, e, unmatched_as_null)
                        });
                    }
                    if let Some(mark) = &r.mark {
                        marks
                            .get_or_insert_with(MatchArray::new)
                            .set(Key::Index(off(idx)), Value::Str(mark.clone()));
                    }
                }
                for (i, set) in sets.into_iter().enumerate() {
                    if let Some(name) = names.and_then(|n| n[i].as_ref()) {
                        subpats.set(Key::Name(Arc::clone(name)), Value::Array(set.clone()));
                    }
                    subpats.push(Value::Array(set));
                }
                if let Some(marks) = marks {
                    subpats.set(Key::name(b"MARK"), Value::Array(marks));
                }
            }
        } else {
            for r in &self.recorded {
                let set = populate_subpat_array(
                    subject,
                    ov_of(r),
                    names,
                    num_subpats,
                    r.count,
                    r.mark.clone(),
                    flags,
                );
                subpats.push(Value::Array(set));
            }
        }
        out.matches = Some(subpats);
        out
    }
}

/// `preg_match($pattern, $subject, $matches, $flags, $offset)`.
///
/// `capture` says whether the PHP call passed `$matches` (it changes PHP's
/// behaviour slightly, e.g. named-group tables are only built then).
pub fn match_one(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchOutcome {
    match_one_spans(re, cfg, subject, capture, flags, offset).into_outcome()
}

/// `preg_match_all($pattern, $subject, $matches, $flags, $offset)`.
pub fn match_all(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchOutcome {
    match_all_spans(re, cfg, subject, capture, flags, offset).into_outcome()
}

/// The matching phase of [`match_one`]: offsets only.
pub fn match_one_spans<'s>(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &'s [u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchSpans<'s> {
    match_impl(re, cfg, subject, capture, false, flags, offset)
}

/// The matching phase of [`match_all`]: offsets only.
pub fn match_all_spans<'s>(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &'s [u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchSpans<'s> {
    match_impl(re, cfg, subject, capture, true, flags, offset)
}

/// Port of `php_pcre_match_impl` (the matching loop; arrays are built by
/// [`MatchSpans::into_outcome`]).
#[allow(clippy::too_many_lines)]
fn match_impl<'s>(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &'s [u8],
    capture: bool,
    global: bool,
    flags: i64,
    start_offset: i64,
) -> MatchSpans<'s> {
    let mut spans = MatchSpans {
        re: re.clone(),
        subject,
        capture,
        global,
        order: if global { PREG_PATTERN_ORDER } else { 0 },
        flags,
        early: Early::No,
        result: Err(MatchFailure::Failed),
        last_error: None,
        warning: None,
        subject_valid_utf8: false,
        ov: Vec::new(),
        ov_inline: [0; INLINE_OV],
        single: None,
        recorded: Vec::new(),
    };
    if start_offset == i64::MIN {
        spans.early = Early::Untouched;
        spans.result = Err(MatchFailure::InvalidOffset);
        return spans;
    }

    if flags & 0xff != 0 {
        let order = flags & 0xff;
        spans.order = order;
        if (global && !(PREG_PATTERN_ORDER..=PREG_SET_ORDER).contains(&order))
            || (!global && order != 0)
        {
            spans.early = Early::Empty;
            spans.result = Err(MatchFailure::InvalidFlags);
            return spans;
        }
    }

    let len = subject.len();
    let start = if start_offset < 0 {
        let back = start_offset.unsigned_abs();
        if back <= len as u64 {
            len - usize::try_from(back).unwrap_or(len)
        } else {
            0
        }
    } else {
        usize::try_from(start_offset).unwrap_or(usize::MAX)
    };
    if start > len {
        spans.early = Early::Empty;
        spans.last_error = Some(PregError::Internal);
        return spans;
    }

    let num_subpats = re.capture_count() + 1;
    let mut matched = 0usize;
    let mut error = PregError::NoError;

    let Some(mut ex) = Exec::new(re, cfg, subject) else {
        spans.early = Early::Empty;
        spans.last_error = Some(PregError::Internal);
        return spans;
    };

    let mut pos = start;
    let mut count = ex.run(pos, ex.first_options(pos));

    'outer: loop {
        if count >= 0 {
            // `matched:` label — re-entered after a successful NOTEMPTY retry.
            loop {
                matched += 1;
                if capture {
                    let (s, e) = ex.span();
                    if e < s {
                        spans.warning = Some("Get subpatterns list failed");
                        spans.last_error = Some(error);
                        spans.result = Err(MatchFailure::Failed);
                        return spans;
                    }
                    let n = pairs(count, num_subpats);
                    let ov = &ex.ov()[..2 * n];
                    if !global {
                        let at = if ov.len() <= INLINE_OV {
                            spans.ov_inline[..ov.len()].copy_from_slice(ov);
                            None
                        } else {
                            spans.ov.extend_from_slice(ov);
                            Some(0)
                        };
                        spans.single = Some(Recorded {
                            at,
                            count: n,
                            mark: ex.mark(),
                        });
                        break 'outer;
                    }
                    let at = spans.ov.len();
                    spans.ov.extend_from_slice(ov);
                    spans.recorded.push(Recorded {
                        at: Some(at),
                        count: n,
                        mark: ex.mark(),
                    });
                }

                let (s, e) = ex.span();
                pos = e;
                if pos == s {
                    count = ex.retry_not_empty(pos);
                    if count >= 0 {
                        if global {
                            continue;
                        }
                        break 'outer;
                    } else if count == ffi::PCRE2_ERROR_NOMATCH {
                        if pos < len {
                            pos += ex.unit_length(pos);
                        } else {
                            break 'outer;
                        }
                    } else {
                        error = PregError::from_pcre2(count);
                        break 'outer;
                    }
                }
                break;
            }
        } else if count == ffi::PCRE2_ERROR_NOMATCH {
            break;
        } else {
            error = PregError::from_pcre2(count);
            break;
        }

        if !global {
            break;
        }
        count = ex.next(pos);
    }

    spans.last_error = Some(error);
    if error == PregError::NoError {
        spans.result = Ok(matched);
        spans.subject_valid_utf8 = re.is_utf() && start_offset == 0;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_expose_offsets_before_materialising() {
        let re = Regex::compile(b"/(a)(x)?(b)?/").unwrap();
        let cfg = MatchConfig::default();
        let spans = match_all_spans(&re, &cfg, b"za ab", true, 0, 0);
        assert_eq!(spans.result(), Ok(2));
        assert_eq!(spans.match_count(), 2);
        let ms: Vec<_> = spans.matches().collect();
        assert_eq!(ms[0].count(), 2);
        assert_eq!(ms[0].span(1), Some((1, 2)));
        assert_eq!(ms[0].group(2), None);
        assert_eq!(ms[1].count(), 4);
        assert_eq!(ms[1].group(0), Some(&b"ab"[..]));
        assert_eq!(ms[1].group(3), Some(&b"b"[..]));
        assert_eq!(spans.captured_bytes(), 1 + 1 + 2 + 1 + 1);
        let out = spans.into_outcome();
        assert_eq!(out.result, Ok(2));
        assert_eq!(out.matches.unwrap().len(), 4);
    }

    /// `preg_match()` keeps a match of up to eight pairs in place and a
    /// larger one in the vector; both read back the same.
    #[test]
    fn single_match_offsets_inline_and_heap() {
        let cfg = MatchConfig::default();
        let small = Regex::compile(b"/(b)(c)?/").unwrap();
        let spans = match_one_spans(&small, &cfg, b"abc", true, 0, 0);
        assert!(spans.ov.is_empty() && spans.recorded.is_empty());
        assert_eq!(spans.match_count(), 1);
        let m = spans.matches().next().unwrap();
        assert_eq!(
            (m.count(), m.span(0), m.span(2)),
            (3, Some((1, 3)), Some((2, 3)))
        );
        let big = Regex::compile(b"/(a)(b)(c)(d)(e)(f)(g)(h)(i)/").unwrap();
        let spans = match_one_spans(&big, &cfg, b"xabcdefghi", true, 0, 0);
        assert_eq!(spans.ov.len(), 20);
        let m = spans.matches().next().unwrap();
        assert_eq!(m.count(), 10);
        assert_eq!(m.group(9), Some(&b"i"[..]));
        assert_eq!(m.span(0), Some((1, 10)));
        let out = spans.into_outcome();
        assert_eq!(out.matches.unwrap().len(), 10);
        let spans = match_one_spans(&small, &cfg, b"zzz", true, 0, 0);
        assert_eq!((spans.result(), spans.match_count()), (Ok(0), 0));
    }

    #[test]
    fn spans_without_capture_record_nothing() {
        let re = Regex::compile(b"/a/").unwrap();
        let spans = match_all_spans(&re, &MatchConfig::default(), b"aaa", false, 0, 0);
        assert_eq!(spans.result(), Ok(3));
        assert_eq!(spans.match_count(), 0);
        assert_eq!(spans.captured_bytes(), 0);
    }

    #[test]
    fn marks_are_counted() {
        let re = Regex::compile(b"/(*MARK:xy)a/").unwrap();
        let spans = match_one_spans(&re, &MatchConfig::default(), b"a", true, 0, 0);
        assert_eq!(spans.matches().next().unwrap().mark(), Some(&b"xy"[..]));
        assert_eq!(spans.captured_bytes(), 3);
    }

    #[test]
    fn known_valid_utf8_hint() {
        let re = Regex::compile(b"/./u").unwrap();
        let s = "éa".as_bytes();
        let base = MatchConfig::default();
        let hinted = base.with_known_valid_utf8(true);
        for cfg in [base, hinted] {
            // Offset inside a character is still reported.
            let o = match_one(&re, &cfg, s, true, 0, 1);
            assert_eq!(o.last_error, Some(PregError::BadUtf8Offset));
            let o = match_one(&re, &cfg, s, true, 0, 2);
            assert_eq!(o.result, Ok(1));
            assert!(!o.subject_valid_utf8);
            let o = match_one(&re, &cfg, s, false, 0, 0);
            assert!(o.subject_valid_utf8);
            let o = match_one(&re, &cfg, s, false, 0, 3);
            assert_eq!(o.result, Ok(0));
        }
        let o = match_one(&re, &base, b"\xff", false, 0, 0);
        assert_eq!(o.last_error, Some(PregError::BadUtf8));
        assert!(!o.subject_valid_utf8);
        let o = match_one(&Regex::compile(b"/./").unwrap(), &base, b"a", false, 0, 0);
        assert!(!o.subject_valid_utf8);
    }
}
