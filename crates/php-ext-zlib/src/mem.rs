// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A model of the Zend allocator's `memory_limit` for results whose size
//! the (possibly hostile) input controls: decompression output and
//! `gzfile()` arrays.
//!
//! The VM does not account memory, so PHP's "Allowed memory size of N
//! bytes exhausted" fatal error is reproduced from the sizes PHP would
//! have to allocate.

use php_runtime::Value;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// Memory in use before the script allocates anything.
const BASELINE: usize = 400 * 1024;

/// Bytes of a `zend_string` of `len` bytes (`_ZSTR_STRUCT_SIZE`, rounded).
pub(crate) fn zstr(len: usize) -> usize {
    (24 + len + 1 + 7) & !7
}

/// The effective `memory_limit` in bytes (`None` for unlimited).
pub(crate) fn limit(vm: &Vm) -> Option<usize> {
    let raw = vm
        .ini_string("memory_limit")
        .unwrap_or_else(|| b"128M".to_vec());
    let (v, _) = php_support::ini::parse_quantity(&raw);
    usize::try_from(v).ok().filter(|&v| v > 0)
}

/// Whether an allocation of `alloc` bytes would exceed `limit` (from
/// [`limit`]).
pub(crate) fn exceeds(limit: Option<usize>, alloc: usize) -> bool {
    limit.is_some_and(|l| BASELINE.saturating_add(alloc) > l)
}

/// Raises PHP's out-of-memory fatal error when an allocation of `alloc`
/// bytes exceeds `memory_limit`.
pub(crate) fn check(vm: &mut Vm, alloc: usize) -> VmResult<()> {
    if let Some(limit) = limit(vm) {
        if BASELINE.saturating_add(alloc) > limit {
            return Err(vm.fatal(&format!(
                "Allowed memory size of {limit} bytes exhausted (tried to allocate {} bytes)",
                zstr(alloc)
            )));
        }
    }
    Ok(())
}

/// `PHP_ZLIB_BUFFER_SIZE_GUESS()`: the output buffer the zlib encoders
/// pre-allocate for `len` input bytes.
pub(crate) fn zlib_guess(len: usize) -> usize {
    len + len / 200 * 3 + 23
}

/// [`check`] for a string result.
pub(crate) fn check_str(vm: &mut Vm, v: &Value) -> VmResult<()> {
    match v {
        Value::Str(s) => check(vm, zstr(s.len())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(zstr(0), 32);
        assert_eq!(zstr(7), 32);
        assert_eq!(zstr(8), 40);
        assert_eq!(zlib_guess(1000), 1038);
    }

    #[test]
    fn limits() {
        assert!(!exceeds(None, usize::MAX));
        assert!(exceeds(Some(1 << 20), 1 << 20));
        assert!(!exceeds(Some(4 << 20), 1 << 20));
    }
}
