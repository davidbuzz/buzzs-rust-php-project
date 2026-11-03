//! PHP `memory_limit` accounting for phprust.
//!
//! [`CountingAlloc`] wraps a [`GlobalAlloc`](std::alloc::GlobalAlloc) and
//! keeps live/peak byte counters; [`GLOBAL`] is the process-wide instance
//! that `php-cli` installs with [`install_global_allocator!`]. The free
//! functions in this crate operate on [`GLOBAL`].
//!
//! The model mirrors `Zend/zend_alloc.c`'s delayed OOM handling:
//!
//! * an allocation that crosses the soft limit still succeeds but records an
//!   [`OverLimit`]; the VM polls [`check`] at safe points and raises
//!   `Fatal error: Allowed memory size of N bytes exhausted (tried to
//!   allocate M bytes)`, then calls [`clear`];
//! * builtins about to allocate a known large amount call [`try_reserve`]
//!   first (PHP's fallible allocations + `RETURN_OOM()`), or wrap Rust's
//!   `try_reserve` APIs in [`fallible`];
//! * an allocation that crosses the hard ceiling (2× the limit by default)
//!   calls the [hard-limit handler](set_hard_limit_handler) or fails, which
//!   aborts the process — the backstop against runaway memory use.
//!
//! See `INTEGRATION.md` in this crate for how the VM and bindings use it.

mod counting;
mod limit;

use std::alloc::System;

pub use counting::{fallible, CountingAlloc, HardLimitHandler, Stats};
pub use limit::{
    clamp_to_max_warning, hard_limit_for, limit_from_ini, round_to_chunks, scale_by_percent,
    HardLimitExceeded, HardLimitPercentError, MemoryLimitIni, OverLimit, SetLimitError, CHUNK_SIZE,
    DEFAULT_LIMIT, LIMIT_MAX, PHP_BASELINE,
};

/// The process-wide accounting allocator over [`System`].
///
/// It only sees allocations once installed with
/// [`install_global_allocator!`]; until then usage reads as
/// [`PHP_BASELINE`] and only [`try_reserve`] enforces the limit.
pub static GLOBAL: CountingAlloc<System> = CountingAlloc::new(System);

/// Declares [`GLOBAL`] as the program's `#[global_allocator]`. Invoke once,
/// at item level, in the binary crate (`php-cli`'s `main.rs`).
#[macro_export]
macro_rules! install_global_allocator {
    () => {
        #[global_allocator]
        static PHP_MEMLIMIT_GLOBAL_ALLOCATOR: &'static $crate::CountingAlloc<::std::alloc::System> =
            &$crate::GLOBAL;
    };
}

/// `memory_get_usage(false)` on [`GLOBAL`].
pub fn usage() -> usize {
    GLOBAL.usage()
}

/// `memory_get_usage(true)` on [`GLOBAL`].
pub fn real_usage() -> usize {
    GLOBAL.real_usage()
}

/// `memory_get_usage($real_usage)` on [`GLOBAL`].
pub fn memory_get_usage(real_usage: bool) -> usize {
    GLOBAL.memory_usage(real_usage)
}

/// `memory_get_peak_usage(false)` on [`GLOBAL`].
pub fn peak() -> usize {
    GLOBAL.peak()
}

/// `memory_get_peak_usage($real_usage)` on [`GLOBAL`].
pub fn memory_get_peak_usage(real_usage: bool) -> usize {
    GLOBAL.peak_usage(real_usage)
}

/// `memory_reset_peak_usage()` on [`GLOBAL`].
pub fn reset_peak() {
    GLOBAL.reset_peak();
}

/// `gc_mem_caches()`: Zend returns the bytes released from its chunk
/// caches; we keep no allocator-level caches, so nothing is ever freed.
pub fn gc_mem_caches() -> usize {
    0
}

/// [`CountingAlloc::set_baseline`] on [`GLOBAL`].
pub fn set_baseline() {
    GLOBAL.set_baseline();
}

/// The current soft limit of [`GLOBAL`] (`None` = unlimited).
pub fn limit() -> Option<usize> {
    GLOBAL.limit()
}

/// [`CountingAlloc::set_limit`] on [`GLOBAL`].
pub fn set_limit(limit: Option<usize>) -> Result<(), SetLimitError> {
    GLOBAL.set_limit(limit)
}

/// [`CountingAlloc::hard_limit`] on [`GLOBAL`].
pub fn hard_limit() -> Option<usize> {
    GLOBAL.hard_limit()
}

/// [`CountingAlloc::set_hard_limit_percent`] on [`GLOBAL`].
pub fn set_hard_limit_percent(percent: i64) -> Result<(), HardLimitPercentError> {
    GLOBAL.set_hard_limit_percent(percent)
}

/// [`CountingAlloc::apply_memory_limit_ini`] on [`GLOBAL`].
pub fn apply_memory_limit_ini(
    value: u64,
    max_memory_limit: u64,
) -> Result<MemoryLimitIni, SetLimitError> {
    GLOBAL.apply_memory_limit_ini(value, max_memory_limit)
}

/// [`CountingAlloc::check`] on [`GLOBAL`].
#[inline]
pub fn check() -> Option<OverLimit> {
    GLOBAL.check()
}

/// [`CountingAlloc::is_pending`] on [`GLOBAL`].
#[inline]
pub fn is_pending() -> bool {
    GLOBAL.is_pending()
}

/// [`CountingAlloc::clear`] on [`GLOBAL`].
pub fn clear() {
    GLOBAL.clear();
}

/// [`CountingAlloc::try_reserve`] on [`GLOBAL`].
pub fn try_reserve(bytes: usize) -> Result<(), OverLimit> {
    GLOBAL.try_reserve(bytes)
}

/// [`CountingAlloc::try_reserve_n`] on [`GLOBAL`].
pub fn try_reserve_n(count: usize, elem_size: usize, extra: usize) -> Result<(), OverLimit> {
    GLOBAL.try_reserve_n(count, elem_size, extra)
}

/// [`CountingAlloc::would_fit`] on [`GLOBAL`].
pub fn would_fit(bytes: usize) -> bool {
    GLOBAL.would_fit(bytes)
}

/// [`CountingAlloc::set_hard_limit_handler`] on [`GLOBAL`].
pub fn set_hard_limit_handler(handler: HardLimitHandler) -> bool {
    GLOBAL.set_hard_limit_handler(handler)
}

/// [`CountingAlloc::request_start`] on [`GLOBAL`].
pub fn request_start(limit: Option<usize>) -> Result<(), SetLimitError> {
    GLOBAL.request_start(limit)
}

/// [`CountingAlloc::request_end`] on [`GLOBAL`].
pub fn request_end() {
    GLOBAL.request_end();
}

#[cfg(test)]
mod tests;
