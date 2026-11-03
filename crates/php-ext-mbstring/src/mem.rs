//! A model of the Zend allocator's `memory_limit` for the `mb_*` functions
//! whose output size the caller controls.
//!
//! The VM does not account memory, so PHP's "Allowed memory size of N
//! bytes exhausted" fatal error is reproduced from the sizes PHP would
//! allocate: the inputs still referenced plus the result being built.

use php_runtime::Value;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// Memory in use before the script allocates anything
/// (`memory_get_usage()` at startup).
const BASELINE: usize = 400 * 1024;

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
    match v {
        Value::Str(s) if s.as_bytes().len() > 1 => zstr(s.as_bytes().len()),
        Value::Array(a) => {
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
            base + a.values().map(value_size).sum::<usize>()
        }
        _ => 0,
    }
}

/// The effective `memory_limit` in bytes (`None` for unlimited).
fn limit(vm: &Vm) -> Option<usize> {
    let raw = vm
        .ini_string("memory_limit")
        .unwrap_or_else(|| b"128M".to_vec());
    let (v, _) = php_support::ini::parse_quantity(&raw);
    usize::try_from(v).ok().filter(|&v| v > 0)
}

/// Raises PHP's out-of-memory fatal error when `live` bytes plus a new
/// allocation of `alloc` bytes exceed `memory_limit`.
pub(crate) fn check(vm: &mut Vm, live: usize, alloc: usize) -> VmResult<()> {
    let Some(limit) = limit(vm) else {
        return Ok(());
    };
    if BASELINE.saturating_add(live).saturating_add(alloc) > limit {
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {alloc} bytes)"
        )));
    }
    Ok(())
}

/// [`check`] for a computed result value.
pub(crate) fn result(vm: &mut Vm, live: usize, v: Value) -> VmResult<Value> {
    check(vm, live, value_size(&v))?;
    Ok(v)
}
