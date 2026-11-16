// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `memory_limit` checks for the `mb_*` functions whose output size the
//! caller controls.
//!
//! Live memory is counted by `php-memlimit`'s allocator; an allocation that
//! crosses the limit is turned into PHP's fatal error by the VM after the
//! call. Where PHP allocates a buffer up front (or a larger one than this
//! implementation needs), [`check`] reserves that size first with
//! [`php_memlimit::try_reserve`], as PHP's fallible allocations do.

use php_runtime::Value;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// `sizeof(Bucket)` of a packed array element (a `zval`).
const PACKED_SLOT: usize = 16;
/// `sizeof(Bucket)` + hash slot of a hash array element.
const HASH_SLOT: usize = 40;

/// Bytes of a `zend_string` of `len` bytes (`_ZSTR_STRUCT_SIZE`, rounded).
pub(crate) fn zstr(len: usize) -> usize {
    (24 + len + 1 + 7) & !7
}

/// Bytes of a packed array of `n` elements.
pub(crate) fn packed(n: usize) -> usize {
    56 + n.max(8).next_power_of_two() * PACKED_SLOT
}

/// Bytes of a hash array of `n` elements.
pub(crate) fn hashed(n: usize) -> usize {
    56 + n.max(8).next_power_of_two() * HASH_SLOT
}

/// Approximate bytes PHP allocates for `v` (strings of one byte or less
/// are interned).
pub(crate) fn value_size(v: &Value) -> usize {
    value_size_in(v, &mut Vec::new())
}

/// [`value_size`]; `open` holds the arrays being measured, so an array
/// that contains itself (`$t = &$a; $a[] = $t;`) is counted once.
fn value_size_in(v: &Value, open: &mut Vec<usize>) -> usize {
    match v {
        Value::Str(s) if s.as_bytes().len() > 1 => zstr(s.as_bytes().len()),
        Value::Array(a) => {
            if open.contains(&a.addr()) {
                return 0;
            }
            let base = if a.is_list() {
                packed(a.len())
            } else {
                hashed(a.len())
                    + a.keys()
                        .map(|k| match k {
                            php_runtime::ArrayKey::Str(s) => zstr(s.as_bytes().len()),
                            php_runtime::ArrayKey::Int(_) => 0,
                        })
                        .sum::<usize>()
            };
            open.push(a.addr());
            let nested = a.values().map(|e| value_size_in(e, open)).sum::<usize>();
            open.pop();
            base + nested
        }
        _ => 0,
    }
}

/// Reserves `alloc` bytes PHP would allocate now (`live` documents what
/// PHP holds at that point; the allocator already counts it). Over the
/// limit, raises `Allowed memory size of N bytes exhausted (tried to
/// allocate M bytes)` at the current line.
pub(crate) fn check(vm: &mut Vm, live: usize, alloc: usize) -> VmResult<()> {
    let _ = live;
    if let Err(over) = php_memlimit::try_reserve(alloc) {
        // Handled here so the VM's post-call poll does not raise it twice.
        let _ = php_memlimit::take();
        return Err(vm.fatal(&over.to_string()));
    }
    Ok(())
}

/// Whether an allocation already crossed `memory_limit` (the VM raises
/// the fatal error after the call; loops building results stop early).
pub(crate) fn pending() -> bool {
    php_memlimit::is_pending()
}

/// A computed result: its allocation is already accounted for (the VM
/// raises the fatal error after the call if it crossed the limit).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn result(_vm: &mut Vm, _live: usize, v: Value) -> VmResult<Value> {
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::{ZArray, ZStr};

    #[test]
    fn zend_sizes() {
        assert_eq!(zstr(0), 32);
        assert_eq!(zstr(7), 32);
        assert_eq!(zstr(8), 40);
        assert_eq!(packed(3), 56 + 8 * 16);
        assert_eq!(hashed(9), 56 + 16 * 40);
    }

    #[test]
    fn value_sizes() {
        assert_eq!(value_size(&Value::Str(ZStr::from("a"))), 0);
        assert_eq!(value_size(&Value::Str(ZStr::from("abc"))), 32);
        let arr = ZArray::from_values([Value::from("abc"), Value::Int(1)]);
        assert_eq!(value_size(&Value::Array(arr)), packed(2) + 32);
        // An array containing itself is measured once (leaks the cycle).
        let mut own = ZArray::from_values([Value::from("abc"), Value::Null]);
        assert!(own.store_self_at(&[None]).is_some());
        assert_eq!(value_size(&Value::Array(own)), packed(2) + 32);
    }
}
