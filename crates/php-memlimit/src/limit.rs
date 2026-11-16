// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Limit values, PHP's hard-limit policy and the error types whose `Display`
//! output is the exact PHP diagnostic text.
//!
//! Everything here is pure arithmetic over byte counts; the stateful parts
//! live in [`crate::CountingAlloc`].

use std::fmt;

/// Zend MM chunk size (`ZEND_MM_CHUNK_SIZE`): `memory_get_usage(true)` grows
/// in steps of this many bytes.
pub const CHUNK_SIZE: usize = 2 * 1024 * 1024;

/// Largest limit Zend accepts (`ZEND_MM_LIMIT_MAX`, `SIZE_MAX >> 1`). Any
/// limit at or above this is treated as "unlimited".
pub const LIMIT_MAX: usize = usize::MAX >> 1;

/// Default value of the `memory_limit` INI setting (`"128M"`).
pub const DEFAULT_LIMIT: usize = 128 * 1024 * 1024;

/// Memory that PHP's CLI reports via `memory_get_usage()` at the first
/// statement of a script.
///
/// Measured with PHP 8.3 CLI: 402 560 – 404 288 bytes depending on the
/// loaded extensions. We use 98 pages (401 408 bytes) so that a fresh request
/// reports a little under PHP's figure and the first few allocations bring it
/// into the same range. Tests compare deltas or use `%d`, so only the order
/// of magnitude matters; [`real_usage`](crate::CountingAlloc::real_usage)
/// rounds this up to one [`CHUNK_SIZE`], which *is* exactly what PHP reports.
pub const PHP_BASELINE: usize = 98 * 4096;

/// Fixed slack PHP adds above the soft limit once the limit exceeds 1 GiB
/// (`zend_mm_update_hard_limit`).
const LARGE_LIMIT_SLACK: usize = 1 << 30;

/// `ZEND_MM_LIMIT_MAX`-aware conversion of an INI quantity to a limit.
///
/// `value` is what `php_support::ini::parse_uquantity` returns for the
/// `memory_limit` string: `"-1"` parses to `u64::MAX`, which (like every
/// value at or above [`LIMIT_MAX`]) means "no limit" and yields `None`.
pub fn limit_from_ini(value: u64) -> Option<usize> {
    match usize::try_from(value) {
        Ok(v) if v < LIMIT_MAX => Some(v),
        _ => None,
    }
}

/// `memory_get_usage(true)`-style rounding: up to whole [`CHUNK_SIZE`]
/// chunks, at least one chunk (Zend always holds its first chunk).
pub fn round_to_chunks(bytes: usize) -> usize {
    bytes.max(1).div_ceil(CHUNK_SIZE).saturating_mul(CHUNK_SIZE)
}

/// Zend MM page size (`ZEND_MM_PAGE_SIZE`); huge blocks are mapped in
/// multiples of it.
pub const PAGE_SIZE: usize = 4096;

/// `ZEND_MM_MAX_LARGE_SIZE`: requests above this many bytes are *huge*
/// blocks (`zend_mm_alloc_huge`), mapped on their own; smaller ones are
/// carved out of 2 MiB chunks.
pub const MAX_LARGE_SIZE: usize = CHUNK_SIZE - PAGE_SIZE;

/// Whether a request of `size` bytes passes `zend_mm_check_limit` under
/// `limit`, with `small` bytes live in small/large blocks and `huge` bytes in
/// huge blocks (neither counting the request).
///
/// Zend checks the limit against `heap->real_size` — whole 2 MiB chunks for
/// small and large blocks plus each huge block's own pages — and only when it
/// needs more memory from the OS: a new chunk for small and large requests
/// (`zend_mm_alloc_pages`), the page-aligned block itself for huge ones
/// (`zend_mm_alloc_huge`). A small request served from an existing chunk
/// never fails.
pub fn zend_limit_allows(small: usize, huge: usize, size: usize, limit: usize) -> bool {
    if size > MAX_LARGE_SIZE {
        let block = size.div_ceil(PAGE_SIZE).saturating_mul(PAGE_SIZE);
        round_to_chunks(small)
            .saturating_add(huge)
            .saturating_add(block)
            <= limit
    } else {
        round_to_chunks(small.saturating_add(size)).saturating_add(huge) <= limit
    }
}

/// The most small/large-block usage `limit` allows: its whole chunks
/// (`round_to_chunks(u) <= limit` exactly when `u <= chunk_floor(limit)`).
pub fn chunk_floor(limit: usize) -> usize {
    limit / CHUNK_SIZE * CHUNK_SIZE
}

/// `scale_by_percent()` from `Zend/zend_alloc.c`: `value * percent / 100`
/// without intermediate overflow, saturating at `usize::MAX`.
pub fn scale_by_percent(value: usize, percent: i64) -> usize {
    let Ok(p) = usize::try_from(percent) else {
        return 0;
    };
    if p == 0 {
        return 0;
    }
    let (q, r) = (value / 100, value % 100);
    let Some(hi) = q.checked_mul(p) else {
        return usize::MAX;
    };
    let lo = r * (p / 100) + r * (p % 100) / 100;
    hi.saturating_add(lo)
}

/// The hard ceiling PHP derives from a soft `limit` and the
/// `memory_hard_limit_percent` INI value (`zend_mm_update_hard_limit`).
///
/// `percent == -1` (the default) means 200 % for limits up to 1 GiB and a
/// fixed 1 GiB of slack above that. The result never exceeds [`LIMIT_MAX`].
pub fn hard_limit_for(limit: usize, percent: i64) -> usize {
    let percent = if percent == -1 {
        if limit > LARGE_LIMIT_SLACK {
            return limit.saturating_add(LARGE_LIMIT_SLACK).min(LIMIT_MAX);
        }
        200
    } else {
        percent
    };
    scale_by_percent(limit, percent).min(LIMIT_MAX)
}

/// A soft-limit violation: PHP's "Allowed memory size … exhausted" fatal.
///
/// `Display` renders the message exactly as a release build of PHP does
/// (`zend_mm_check_limit`): the debug-only `" at file:line"` part is omitted,
/// which the `.phpt` `%S`/`%s` placeholders accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OverLimit {
    /// The configured `memory_limit` in bytes.
    pub limit: usize,
    /// Size of the allocation (or reservation) that crossed the limit.
    pub tried: usize,
}

impl fmt::Display for OverLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Allowed memory size of {} bytes exhausted (tried to allocate {} bytes)",
            self.limit, self.tried
        )
    }
}

impl std::error::Error for OverLimit {}

/// The hard ceiling was hit: PHP prints this to stderr followed by
/// `" in %s on line %d\n"` and `_exit(137)`s (`zend_mm_hard_limit_error`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HardLimitExceeded {
    /// The hard ceiling in bytes.
    pub hard_limit: usize,
    /// Size of the allocation that would have crossed it.
    pub tried: usize,
}

impl fmt::Display for HardLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Hard memory limit of {} bytes exhausted (tried to allocate {} bytes)",
            self.hard_limit, self.tried
        )
    }
}

impl std::error::Error for HardLimitExceeded {}

/// `zend_set_memory_limit()` refused a limit below the current real usage.
///
/// `Display` is the `E_WARNING` text `OnChangeMemoryLimit` raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetLimitError {
    /// The rejected limit in bytes.
    pub requested: usize,
    /// `memory_get_usage(true)` at the time of the request.
    pub current: usize,
}

impl fmt::Display for SetLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to set memory limit to {} bytes (Current memory usage is {} bytes)",
            self.requested, self.current
        )
    }
}

impl std::error::Error for SetLimitError {}

/// Why `memory_hard_limit_percent` could not be changed
/// (`OnChangeMemoryHardLimitPercent`); `Display` is the `E_WARNING` text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HardLimitPercentError {
    /// The value is below 100 and not -1.
    Invalid(i64),
    /// Real usage currently exceeds the soft limit.
    OverSoftLimit(i64),
}

impl fmt::Display for HardLimitPercentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(_) => f.write_str("memory_hard_limit_percent must be >= 100, or -1"),
            Self::OverSoftLimit(p) => write!(f, "Failed to set memory_hard_limit_percent to {p}"),
        }
    }
}

impl std::error::Error for HardLimitPercentError {}

/// Successful outcome of [`CountingAlloc::apply_memory_limit_ini`](crate::CountingAlloc::apply_memory_limit_ini).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryLimitIni {
    /// The requested value is now the limit.
    Applied,
    /// The value exceeded `max_memory_limit`; the limit (and the INI entry's
    /// string value, which the caller must overwrite with the
    /// `max_memory_limit` string) is `max` instead. `warning` is the
    /// `E_WARNING` text, absent when the request was `-1`.
    ClampedToMax {
        /// The `max_memory_limit` value now in force.
        max: u64,
        /// Warning to raise, if any.
        warning: Option<String>,
    },
}

/// The warning `OnChangeMemoryLimit` raises when `value` exceeds
/// `max_memory_limit` (both formatted as signed, like PHP's `%zd`).
pub fn clamp_to_max_warning(value: u64, max: u64) -> String {
    format!(
        "Failed to set memory_limit to {} bytes. Setting to max_memory_limit instead (currently: {} bytes)",
        i64::from_ne_bytes(value.to_ne_bytes()),
        i64::from_ne_bytes(max.to_ne_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: usize = 1024 * 1024;

    /// `zend_mm_check_limit` granularity: small requests only fail when a
    /// new chunk would pass the limit, huge ones when their pages would;
    /// huge blocks count at their own size, not rounded into chunks.
    #[test]
    fn zend_limit_is_chunk_and_page_granular() {
        // 8M = 4 chunks: small blocks fit until usage would pass 8M.
        assert!(zend_limit_allows(8 * M - 11, 0, 11, 8 * M));
        assert!(!zend_limit_allows(8 * M - 10, 0, 11, 8 * M));
        // 9M still holds only 4 whole chunks.
        assert!(!zend_limit_allows(8 * M, 0, 1, 9 * M));
        assert_eq!(chunk_floor(9 * M), 8 * M);
        // Huge: whole chunks in use plus the page-aligned block.
        assert!(zend_limit_allows(M, 0, 6 * M, 8 * M)); // 2M + 6M
        assert!(!zend_limit_allows(2 * M + 1, 0, 6 * M, 8 * M)); // 4M + 6M
        assert!(!zend_limit_allows(M, 0, 6 * M + 1, 8 * M)); // 2M + 6M + 4K

        // Live huge blocks add their own size (oom_json_decode_007: two 3M
        // strings next to a few hundred KiB of small blocks fit in 10M).
        assert!(zend_limit_allows(M, 3 * M + 4096, 3 * M + 4, 10 * M));
        assert!(zend_limit_allows(M - 64, 6 * M, 64, 8 * M));
        assert!(!zend_limit_allows(2 * M, 6 * M, 1, 8 * M));
        assert_eq!(MAX_LARGE_SIZE, 2 * M - PAGE_SIZE);
    }

    #[test]
    fn ini_conversion() {
        assert_eq!(limit_from_ini(u64::MAX), None);
        assert_eq!(limit_from_ini(LIMIT_MAX as u64), None);
        assert_eq!(limit_from_ini(LIMIT_MAX as u64 - 1), Some(LIMIT_MAX - 1));
        assert_eq!(limit_from_ini(128 << 20), Some(128 * M));
        assert_eq!(limit_from_ini(0), Some(0));
    }

    #[test]
    fn chunk_rounding() {
        assert_eq!(round_to_chunks(0), CHUNK_SIZE);
        assert_eq!(round_to_chunks(1), CHUNK_SIZE);
        assert_eq!(round_to_chunks(PHP_BASELINE), CHUNK_SIZE);
        assert_eq!(round_to_chunks(CHUNK_SIZE), CHUNK_SIZE);
        assert_eq!(round_to_chunks(CHUNK_SIZE + 1), 2 * CHUNK_SIZE);
        assert_eq!(round_to_chunks(usize::MAX), usize::MAX);
    }

    #[test]
    fn percent_scaling() {
        assert_eq!(scale_by_percent(10 * M, 200), 20 * M);
        assert_eq!(scale_by_percent(10 * M, 100), 10 * M);
        assert_eq!(scale_by_percent(10 * M, 150), 15 * M);
        assert_eq!(scale_by_percent(199, 150), 298);
        assert_eq!(scale_by_percent(123, 0), 0);
        assert_eq!(scale_by_percent(123, -5), 0);
        assert_eq!(scale_by_percent(usize::MAX, 200), usize::MAX);
        assert_eq!(scale_by_percent(usize::MAX / 2, 201), usize::MAX);
    }

    #[test]
    fn hard_limit_policy() {
        // oom_hard_limit_exceeded.phpt: memory_limit=10M -> 20971520.
        assert_eq!(hard_limit_for(10 * M, -1), 20_971_520);
        assert_eq!(hard_limit_for(1024 * M, -1), 2048 * M);
        assert_eq!(hard_limit_for(1025 * M, -1), 2049 * M);
        assert_eq!(hard_limit_for(10 * M, 150), 15 * M);
        assert_eq!(hard_limit_for(LIMIT_MAX - 5, -1), LIMIT_MAX);
        assert_eq!(hard_limit_for(LIMIT_MAX / 2, 400), LIMIT_MAX);
    }

    #[test]
    fn messages() {
        assert_eq!(
            OverLimit {
                limit: 104_857_600,
                tried: 314_572_832
            }
            .to_string(),
            "Allowed memory size of 104857600 bytes exhausted (tried to allocate 314572832 bytes)"
        );
        assert_eq!(
            HardLimitExceeded {
                hard_limit: 20_971_520,
                tried: 1 << 30
            }
            .to_string(),
            "Hard memory limit of 20971520 bytes exhausted (tried to allocate 1073741824 bytes)"
        );
        assert_eq!(
            SetLimitError {
                requested: 1_048_576,
                current: 2_097_152
            }
            .to_string(),
            "Failed to set memory limit to 1048576 bytes (Current memory usage is 2097152 bytes)"
        );
        assert_eq!(
            HardLimitPercentError::Invalid(50).to_string(),
            "memory_hard_limit_percent must be >= 100, or -1"
        );
        assert_eq!(
            HardLimitPercentError::OverSoftLimit(150).to_string(),
            "Failed to set memory_hard_limit_percent to 150"
        );
        assert_eq!(
            clamp_to_max_warning(1 << 30, 512 << 20),
            "Failed to set memory_limit to 1073741824 bytes. Setting to max_memory_limit instead (currently: 536870912 bytes)"
        );
    }
}
