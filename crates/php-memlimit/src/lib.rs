// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP `memory_limit` accounting for phprust.
//!
//! [`CountingAlloc`] wraps a [`GlobalAlloc`](std::alloc::GlobalAlloc) and
//! keeps live/peak byte counters, each block charged its php-src size
//! class ([`zend_size`]: bin, pages or page-rounded huge block); [`GLOBAL`] is the process-wide instance
//! that `php-cli` installs with [`install_global_allocator!`]. The free
//! functions in this crate operate on [`GLOBAL`].
//!
//! The model mirrors `Zend/zend_alloc.c`'s delayed OOM handling:
//!
//! * an allocation that crosses the soft limit still succeeds but records an
//!   [`OverLimit`]; the VM polls [`is_pending`] at safe points and raises
//!   `Fatal error: Allowed memory size of N bytes exhausted (tried to
//!   allocate M bytes)` with the record obtained from [`take`]; recording
//!   stays suspended until [`clear`] at request shutdown;
//! * builtins about to allocate a known large amount call [`try_reserve`]
//!   first (PHP's fallible allocations + `RETURN_OOM()`), or wrap Rust's
//!   `try_reserve` APIs in [`fallible`];
//! * huge allocations made inside [`uncounted`] (an extension's library
//!   allocating through its own `malloc`, like libgmp under `ext/gmp`) are
//!   left out of usage and both limits for their whole lifetime;
//! * an allocation that crosses the hard ceiling (2× the limit by default)
//!   calls the [hard-limit handler](set_hard_limit_handler) or fails, which
//!   aborts the process — the backstop against runaway memory use;
//! * a [heap observer](set_heap_observer) can be told about each
//!   allocation and free of the request thread while
//!   [observing](observe_heap) is on (`ext/zend_test`'s custom ZendMM
//!   handlers); while it is off the fast paths do no extra work.
//!
//! The memory itself comes from a port of `zend_mm`'s small-block heap for
//! the request thread (the thread [bound](bind_to_current_thread) by
//! [`request_start`]: size bins carved out of 2 MiB chunks, unlocked free
//! lists, empty runs and chunks given back by [`gc_mem_caches`],
//! [`collect_heap`] and the end of a request-bound thread's request) and
//! from [`MiMalloc`] for larger blocks and other threads; see
//! [`CountingAlloc`].
//!
//! See `INTEGRATION.md` in this crate for how the VM and bindings use it.

mod counting;
mod heap;
mod huge_pages;
mod limit;

pub use huge_pages::{huge_pages_from_env, set_huge_pages};
pub use mimalloc::MiMalloc;

pub use counting::{
    fallible, uncounted, CountingAlloc, HardLimitHandler, HeapEvent, HeapObserver, PendingHook,
    Stats,
};
pub use heap::{zend_size, MAX_SMALL_ALIGN, MAX_SMALL_SIZE};
pub use limit::{
    clamp_to_max_warning, hard_limit_for, limit_from_ini, round_to_chunks, scale_by_percent,
    HardLimitExceeded, HardLimitPercentError, MemoryLimitIni, OverLimit, SetLimitError, CHUNK_SIZE,
    DEFAULT_LIMIT, LIMIT_MAX, PHP_BASELINE,
};

/// The process-wide accounting allocator: the request thread's small-block
/// heap, over [`MiMalloc`] (mimalloc) for everything else.
///
/// It only sees allocations once installed with
/// [`install_global_allocator!`]; until then usage reads as
/// [`PHP_BASELINE`] and only [`try_reserve`] enforces the limit.
pub static GLOBAL: CountingAlloc<MiMalloc> = CountingAlloc::new(MiMalloc);

/// Declares [`GLOBAL`] as the program's `#[global_allocator]`. Invoke once,
/// at item level, in the binary crate (`php-cli`'s `main.rs`).
#[macro_export]
macro_rules! install_global_allocator {
    () => {
        #[global_allocator]
        static PHP_MEMLIMIT_GLOBAL_ALLOCATOR: &'static $crate::CountingAlloc<$crate::MiMalloc> =
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

/// [`CountingAlloc::uncounted_bytes`] on [`GLOBAL`].
pub fn uncounted_bytes() -> usize {
    GLOBAL.uncounted_bytes()
}

/// `memory_get_usage($real_usage)` on [`GLOBAL`].
pub fn memory_get_usage(real_usage: bool) -> usize {
    GLOBAL.memory_usage(real_usage)
}

/// [`CountingAlloc::set_tracked`] on [`GLOBAL`].
pub fn set_tracked(tracked: bool) {
    GLOBAL.set_tracked(tracked);
}

/// Zend's allocator selection from the environment (`zend_mm_init` /
/// `alloc_globals_ctor`): tracked mode when `USE_ZEND_ALLOC` parses (like
/// `ZEND_ATOL`) to 0 and `USE_TRACKED_ALLOC` to non-zero.
pub fn tracked_from_env(use_zend_alloc: Option<&[u8]>, use_tracked_alloc: Option<&[u8]>) -> bool {
    use_zend_alloc.is_some_and(|v| atol(v) == 0) && use_tracked_alloc.is_some_and(|v| atol(v) != 0)
}

/// `ZEND_ATOL`/`strtol(s, NULL, 10)`: leading whitespace, optional sign,
/// decimal digits; anything unparsable is 0.
pub(crate) fn atol(s: &[u8]) -> i64 {
    let s = s.trim_ascii_start();
    let (neg, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, s),
    };
    let mut n: i64 = 0;
    for &b in digits.iter().take_while(|b| b.is_ascii_digit()) {
        n = n.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    if neg {
        -n
    } else {
        n
    }
}

/// [`CountingAlloc::transient`] on [`GLOBAL`].
#[inline]
#[must_use]
pub fn transient(layout: std::alloc::Layout) -> bool {
    GLOBAL.transient(layout)
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

/// [`CountingAlloc::gc_mem_caches`] on [`GLOBAL`] (`gc_mem_caches()`:
/// `zend_mm_gc`, the bytes of the heap's empty runs given back).
pub fn gc_mem_caches() -> usize {
    GLOBAL.gc_mem_caches()
}

/// [`CountingAlloc::collect_heap`] on [`GLOBAL`]: `zend_mm_gc` without the
/// observer report. [`request_end`] runs it for a thread that
/// [`request_start`] bound; a long-running SAPI that tears its `Vm` down
/// after `request_end` can call it again afterwards, so that the blocks the
/// teardown freed are collected too.
pub fn collect_heap() -> usize {
    GLOBAL.collect_heap()
}

/// [`CountingAlloc::set_observer`] on [`GLOBAL`].
pub fn set_heap_observer(observer: HeapObserver) -> bool {
    GLOBAL.set_observer(observer)
}

/// [`CountingAlloc::observe`] on [`GLOBAL`].
pub fn observe_heap(on: bool) {
    GLOBAL.observe(on);
}

/// [`CountingAlloc::is_observing`] on [`GLOBAL`].
pub fn is_observing_heap() -> bool {
    GLOBAL.is_observing()
}

/// [`CountingAlloc::bind_to_current_thread`] on [`GLOBAL`]:
/// [`request_start`] binds the request thread for the request; a SAPI
/// that runs its requests on one thread (php-cli) calls this earlier, on
/// that thread (e.g. before module startup), so that startup allocations
/// use the heap too and the binding outlives [`request_end`] (the teardown
/// stays on the owner path). `false` while another thread owns it.
pub fn bind_to_current_thread() -> bool {
    GLOBAL.bind_to_current_thread()
}

/// [`CountingAlloc::unbind_current_thread`] on [`GLOBAL`] ([`request_end`]
/// does it for a thread that [`request_start`] bound; an explicitly bound
/// thread calls it when it is done with the heap).
pub fn unbind_current_thread() {
    GLOBAL.unbind_current_thread();
}

/// [`CountingAlloc::set_baseline`] on [`GLOBAL`].
pub fn set_baseline() {
    GLOBAL.set_baseline();
}

/// [`CountingAlloc::persistent`] on [`GLOBAL`]: runs `f` with the bytes it
/// allocates or frees (net) left out of `memory_get_usage()`, like
/// php-src's persistent (`pemalloc`) tables. Wrap only the buffer
/// operation (a `Vec`'s growth or release) in it.
pub fn persistent<R>(f: impl FnOnce() -> R) -> R {
    GLOBAL.persistent(f)
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

/// [`CountingAlloc::take`] on [`GLOBAL`].
pub fn take() -> Option<OverLimit> {
    GLOBAL.take()
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

/// [`CountingAlloc::set_pending_hook`] on [`GLOBAL`].
pub fn set_pending_hook(hook: PendingHook) -> bool {
    GLOBAL.set_pending_hook(hook)
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
