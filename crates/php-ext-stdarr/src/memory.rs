//! A model of the Zend allocator's limits for the array functions that
//! build arrays whose size the caller controls (`array_fill`, `range`,
//! `array_pad`, `array_chunk`, `array_map(null, ...)`).
//!
//! The VM does not account memory, so PHP's fatal errors — "Possible
//! integer overflow in memory allocation" for tables above `HT_MAX_SIZE`
//! and "Allowed memory size of N bytes exhausted" past `memory_limit` —
//! are reproduced from the size PHP would allocate. This also keeps a
//! script from making the process allocate gigabytes.

use php_vm::ext::VmResult;
use php_vm::Vm;

/// `E_ERROR`.
const E_ERROR: i64 = 1;
/// `HT_MAX_SIZE` on 64-bit platforms.
pub const HT_MAX_SIZE: u64 = 0x4000_0000;
/// `sizeof(zend_array)`.
pub const ARRAY_HEADER: u64 = 56;
/// `HT_MIN_SIZE`.
const HT_MIN_SIZE: u64 = 8;
/// `ZEND_MM_MAX_LARGE_SIZE`: larger blocks are page-aligned huge blocks.
const MAX_LARGE_SIZE: u64 = 2 * 1024 * 1024 - 4096;

/// `zend_hash_check_size`: the table size for `n` elements.
fn table_size(n: u64) -> u64 {
    n.max(HT_MIN_SIZE).next_power_of_two()
}

/// What the allocator really takes for a request of `bytes`.
fn rounded(bytes: u64) -> u64 {
    if bytes > MAX_LARGE_SIZE {
        bytes.div_ceil(4096) * 4096
    } else {
        bytes
    }
}

/// Bytes of the element storage of a packed array of `n` elements
/// (`HT_PACKED_SIZE_EX`).
#[must_use]
pub fn packed_data(n: u64) -> u64 {
    rounded(table_size(n) * 16 + 8)
}

/// Bytes of the bucket and hash storage of a hash array of `n` elements
/// (`HT_SIZE_EX`).
#[must_use]
pub fn hash_data(n: u64) -> u64 {
    rounded(table_size(n) * 40)
}

/// Raises "Possible integer overflow in memory allocation" (`E_ERROR`)
/// when a table of `n` elements exceeds `HT_MAX_SIZE`.
pub fn check_table(vm: &mut Vm, n: u64) -> VmResult<()> {
    if n > HT_MAX_SIZE {
        vm.raise(
            E_ERROR,
            &format!("Possible integer overflow in memory allocation ({n} * 32 + 32)"),
        )?;
    }
    Ok(())
}

/// Raises PHP's out-of-memory fatal error when allocating `bytes` would
/// exceed `memory_limit`.
pub fn check(vm: &mut Vm, bytes: u64) -> VmResult<()> {
    let Ok(limit) = u64::try_from(vm.ini_long("memory_limit")) else {
        return Ok(());
    };
    if limit == 0 || bytes <= limit {
        return Ok(());
    }
    vm.raise(
        E_ERROR,
        &format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {bytes} bytes)"
        ),
    )
}

/// Checks a list of `n` arrays of `each` packed elements (plus the outer
/// packed list), as built by `array_chunk()` and `array_map(null, ...)`.
pub fn check_nested(vm: &mut Vm, n: u64, each: u64) -> VmResult<()> {
    let inner = ARRAY_HEADER + packed_data(each);
    let total = packed_data(n).saturating_add(n.saturating_mul(inner));
    check(vm, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_zend_allocator() {
        // range(1, 1000000): 2^20 zvals + 8 bytes, page-rounded.
        assert_eq!(packed_data(1_000_000), 16_781_312);
        // array_chunk() of 250000 elements: the outer list.
        assert_eq!(packed_data(250_000), 4_198_400);
        // array_fill(-5, 1073741823): a 2^30-bucket hash.
        assert_eq!(hash_data(1_073_741_823), 42_949_672_960);
        assert_eq!(packed_data(0), 136);
    }
}
