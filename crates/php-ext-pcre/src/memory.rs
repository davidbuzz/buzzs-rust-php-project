//! An approximation of the Zend memory manager's `memory_limit` check for
//! the (potentially huge) values the `preg_*` functions build.
//!
//! The VM does not account memory, so PHP's "Allowed memory size of N
//! bytes exhausted" fatal error is reproduced here by estimating what the
//! result would cost in PHP: `zend_string`s of length 0 or 1 are interned
//! (free), longer ones take `ZEND_MM_ALIGNED_SIZE(24 + len + 1)` bytes;
//! packed arrays take 16 bytes per element, hashes 32 bytes per bucket
//! plus their hash slots.

use php_pcre::{Key, MatchArray, Value as PValue};
use php_vm::ext::VmResult;
use php_vm::Vm;

/// `E_ERROR`.
const E_ERROR: i64 = 1;

/// Size of a `zend_array` header.
const ARRAY_HEADER: u64 = 56;

/// Bytes PHP would allocate for a string of `len` bytes.
pub(crate) fn string_size(len: usize) -> u64 {
    if len <= 1 {
        0
    } else {
        (24 + len as u64 + 1 + 7) & !7
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

/// Raises PHP's out-of-memory fatal error when allocating `bytes` would
/// exceed `memory_limit`.
pub(crate) fn check(vm: &mut Vm, bytes: u64) -> VmResult<()> {
    let limit = vm.ini_long("memory_limit");
    let Ok(limit) = u64::try_from(limit) else {
        return Ok(());
    };
    if limit == 0 || bytes <= limit {
        return Ok(());
    }
    vm.raise(
        E_ERROR,
        &format!("Allowed memory size of {limit} bytes exhausted (tried to allocate {bytes} bytes)"),
    )
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
