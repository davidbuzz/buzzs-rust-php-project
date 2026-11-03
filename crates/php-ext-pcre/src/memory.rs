//! `memory_limit` reservations for the (potentially huge) values the
//! `preg_*` functions build, through `php-memlimit`.
//!
//! Before a result is turned into PHP values, the bytes PHP would allocate
//! for it are reserved, so PHP's "Allowed memory size of N bytes exhausted"
//! fatal error is raised at the call like in PHP. Sizes follow the Zend
//! allocator: `zend_string`s of length 0 or 1 are interned
//! (free), longer ones take `ZEND_MM_ALIGNED_SIZE(24 + len + 1)` bytes;
//! packed arrays take 16 bytes per element, hashes 32 bytes per bucket
//! plus their hash slots.

use php_pcre::{
    Key, MatchArray, MatchConfig, MatchRef, MatchSpans, Regex, Replacement, ReplacementPiece,
    Value as PValue, PREG_OFFSET_CAPTURE, PREG_PATTERN_ORDER, PREG_SET_ORDER,
    PREG_UNMATCHED_AS_NULL,
};
use php_vm::ext::VmResult;
use php_vm::Vm;

/// Size of a `zend_array` header.
const ARRAY_HEADER: u64 = 56;

/// Bytes PHP would allocate for a string of `len` bytes.
pub(crate) fn string_size(len: usize) -> u64 {
    if len <= 1 {
        0
    } else {
        (24 + (len as u64).saturating_add(1 + 7)) & !7
    }
}

/// Bytes PHP would allocate for `m` and everything it contains.
pub(crate) fn array_size(m: &MatchArray) -> u64 {
    let packed = m
        .keys()
        .enumerate()
        .all(|(i, k)| matches!(k, Key::Index(n) if usize::try_from(*n) == Ok(i)));
    let n = m.len() as u64;
    let table = if packed {
        n * 16
    } else {
        n * 32 + n.next_power_of_two() * 4
    };
    ARRAY_HEADER + table + m.values().map(value_size).sum::<u64>()
}

fn value_size(v: &PValue) -> u64 {
    match v {
        PValue::Str(s) => string_size(s.len()),
        PValue::Array(a) => array_size(a),
        PValue::Null | PValue::Int(_) => 0,
    }
}

/// Reserves `bytes` against `memory_limit`, raising PHP's out-of-memory
/// fatal error when they do not fit.
pub(crate) fn check(vm: &mut Vm, bytes: u64) -> VmResult<()> {
    let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    if let Err(over) = php_memlimit::try_reserve(bytes) {
        // Handled here, so the VM's post-call poll does not raise it again.
        let _ = php_memlimit::take();
        return Err(vm.fatal(&over.to_string()));
    }
    Ok(())
}

/// Number of matches of `re` in `subject` (an extra matching pass, only
/// made when a result might not fit in the memory limit).
fn count_matches(re: &Regex, cfg: &MatchConfig, subject: &[u8]) -> u64 {
    php_pcre::match_all(re, cfg, subject, false, 0, 0)
        .result
        .map_or(0, |n| n as u64)
}

/// `memory_limit` pre-check for `preg_replace()`: `php-pcre` builds the
/// result in one go, so a result PHP would refuse to allocate must be
/// caught before the library allocates it. When even the largest possible
/// result fits, nothing is done; otherwise the matches are counted and the
/// smallest result they imply is reserved.
pub(crate) fn precheck_replace(
    vm: &mut Vm,
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    replacement: &Replacement,
    limit: usize,
) -> VmResult<()> {
    let literal: u64 = replacement
        .pieces()
        .iter()
        .map(|p| match p {
            ReplacementPiece::Literal(l) => l.len() as u64,
            ReplacementPiece::Group(_) => 0,
        })
        .sum();
    if literal == 0 {
        return Ok(());
    }
    let len = subject.len() as u64;
    let max_matches = (len + 1).min(limit as u64);
    let has_groups = replacement
        .pieces()
        .iter()
        .any(|p| matches!(p, ReplacementPiece::Group(_)));
    let upper = len.saturating_add(max_matches.saturating_mul(literal));
    if !has_groups && fits(upper) {
        return Ok(());
    }
    let n = count_matches(re, cfg, subject).min(limit as u64);
    check(vm, string_size_u64(n.saturating_mul(literal)))
}

/// `memory_limit` pre-check for `preg_split()` (see [`precheck_replace`]):
/// every piece costs at least one packed array slot.
pub(crate) fn precheck_split(
    vm: &mut Vm,
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    no_empty: bool,
) -> VmResult<()> {
    let len = subject.len() as u64;
    let pieces = (len + 2).saturating_mul(re.capture_count() as u64 + 1);
    if no_empty || fits(ARRAY_HEADER + pieces * (16 + 32) + len) {
        return Ok(());
    }
    let n = count_matches(re, cfg, subject);
    check(vm, ARRAY_HEADER + (n + 1) * 16)
}

/// `memory_limit` pre-check for `preg_match_all()` with `$matches` (see
/// [`precheck_replace`]): every match costs at least one slot per group.
pub(crate) fn precheck_match_all(
    vm: &mut Vm,
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
) -> VmResult<()> {
    let len = subject.len() as u64;
    let groups = re.capture_count() as u64 + 1;
    let upper = (len + 1).saturating_mul(groups * (16 + 32)) + groups * len;
    if fits(upper) {
        return Ok(());
    }
    let n = count_matches(re, cfg, subject);
    check(vm, groups * (ARRAY_HEADER + n * 16))
}

/// Bytes of an array table with `entries` slots (packed or hashed).
fn table_size(entries: u64, hashed: bool) -> u64 {
    if hashed {
        entries * 32 + entries.next_power_of_two() * 4
    } else {
        entries * 16
    }
}

/// Bytes PHP would allocate for the `$matches` that
/// [`MatchSpans::into_outcome`] is about to build (computed from the
/// offsets alone, before any capture is copied; mirrors [`array_size`]).
pub(crate) fn spans_size(spans: &MatchSpans<'_>, re: &Regex, global: bool, flags: i64) -> u64 {
    let offset_capture = flags & PREG_OFFSET_CAPTURE != 0;
    let unmatched_as_null = flags & PREG_UNMATCHED_AS_NULL != 0;
    let order = match flags & 0xff {
        0 if global => PREG_PATTERN_ORDER,
        o => o,
    };
    let groups = re.capture_count() as u64 + 1;
    let names = re.name_count() as u64;
    // A matched value: its string, plus the `[string, offset]` pair array.
    let value = |span: Option<(usize, usize)>| -> u64 {
        let s = span.map_or(0, |(s, e)| string_size(e - s));
        if offset_capture && span.is_some() {
            ARRAY_HEADER + table_size(2, false) + s
        } else {
            s
        }
    };
    let set = |m: &MatchRef<'_>| -> u64 {
        let n = if unmatched_as_null {
            groups
        } else {
            m.count() as u64
        };
        let hashed = names > 0 || m.mark().is_some();
        let keys = n + if hashed {
            names + u64::from(m.mark().is_some())
        } else {
            0
        };
        let values: u64 = (0..m.count()).map(|i| value(m.span(i))).sum();
        ARRAY_HEADER
            + table_size(keys, hashed)
            + values
            + m.mark().map_or(0, |k| string_size(k.len()))
    };
    let n = spans.match_count() as u64;
    if !global {
        spans.matches().next().map_or(0, |m| set(&m))
    } else if order == PREG_SET_ORDER {
        ARRAY_HEADER + table_size(n, false) + spans.matches().map(|m| set(&m)).sum::<u64>()
    } else {
        let values: u64 = spans
            .matches()
            .map(|m| (0..m.count()).map(|i| value(m.span(i))).sum::<u64>())
            .sum();
        ARRAY_HEADER
            + table_size(groups + names, names > 0)
            + groups * (ARRAY_HEADER + table_size(n, false))
            + values
    }
}

/// Whether `bytes` more would stay within `memory_limit`.
fn fits(bytes: u64) -> bool {
    php_memlimit::would_fit(usize::try_from(bytes).unwrap_or(usize::MAX))
}

fn string_size_u64(len: u64) -> u64 {
    string_size(usize::try_from(len).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interned_and_aligned_strings() {
        assert_eq!(string_size(0), 0);
        assert_eq!(string_size(1), 0);
        assert_eq!(string_size(2), 32);
        assert_eq!(string_size(8), 40);
    }

    #[test]
    fn spans_size_counts_captures_before_copying() {
        let re = Regex::compile(b"/(a+)(b)?/").unwrap();
        let cfg = MatchConfig::default();
        let subject = vec![b'a'; 100];
        let spans = php_pcre::match_one_spans(&re, &cfg, &subject, true, 0, 0);
        // [0] and [1] are both 100 bytes; [2] is trimmed.
        assert_eq!(
            spans_size(&spans, &re, false, 0),
            ARRAY_HEADER + 32 + 2 * string_size(100)
        );
        let built = spans.into_outcome().matches.unwrap();
        assert_eq!(array_size(&built), ARRAY_HEADER + 32 + 2 * string_size(100));

        let spans = php_pcre::match_all_spans(&re, &cfg, b"ab a", true, 0, 0);
        let estimate = spans_size(&spans, &re, true, 0);
        let built = spans.into_outcome().matches.unwrap();
        assert_eq!(estimate, array_size(&built));

        let spans = php_pcre::match_all_spans(&re, &cfg, b"aab a", true, PREG_SET_ORDER, 0);
        let estimate = spans_size(&spans, &re, true, PREG_SET_ORDER);
        let built = spans.into_outcome().matches.unwrap();
        assert_eq!(estimate, array_size(&built));
    }

    #[test]
    fn packed_vs_hash() {
        let mut list = MatchArray::new();
        list.push(PValue::Str(b"a".to_vec()));
        list.push(PValue::Str(b"b".to_vec()));
        assert_eq!(array_size(&list), ARRAY_HEADER + 32);
        let mut hash = MatchArray::new();
        hash.set(Key::name(b"x"), PValue::Null);
        assert_eq!(array_size(&hash), ARRAY_HEADER + 32 + 4);
    }
}
