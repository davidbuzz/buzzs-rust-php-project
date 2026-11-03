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
    Key, MatchArray, MatchConfig, Regex, Replacement, ReplacementPiece, Value as PValue,
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
