//! [`CountingAlloc`]: the accounting [`GlobalAlloc`] wrapper.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::fmt;
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU8, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::OnceLock;

use crate::limit::{
    clamp_to_max_warning, hard_limit_for, limit_from_ini, round_to_chunks, HardLimitExceeded,
    HardLimitPercentError, MemoryLimitIni, OverLimit, SetLimitError, LIMIT_MAX, PHP_BASELINE,
};

/// Signature of the hard-ceiling handler (see
/// [`CountingAlloc::set_hard_limit_handler`]). It must not return.
pub type HardLimitHandler = fn(HardLimitExceeded) -> !;

/// `over_state` values.
const NONE: u8 = 0;
const RECORDING: u8 = 1;
const PENDING: u8 = 2;

/// Sentinel for "no limit" in the `limit` and threshold atomics.
const UNLIMITED: usize = usize::MAX;

/// Increments a statistics counter with a plain load/store instead of a
/// locked read-modify-write: concurrent bumps from several threads may be
/// lost (the counts are informational), but the operation stays sound and
/// keeps the allocation fast path down to a single locked instruction.
#[inline]
fn bump(counter: &AtomicUsize) {
    counter.store(counter.load(Relaxed).wrapping_add(1), Relaxed);
}

thread_local! {
    /// Nesting depth of [`crate::fallible`] on this thread. Const-initialised
    /// and without a destructor, so reading it from inside the allocator
    /// never allocates.
    static FALLIBLE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Runs `f` with soft-limit-exceeding allocations made *fallible*: instead
/// of being granted (and flagged), they return null, which Rust's
/// `try_reserve`/`try_with_capacity` APIs turn into an `Err`. The violation
/// is still recorded, exactly like PHP's `zend_*_falloc` + `RETURN_OOM()`.
///
/// Only use fallible collection APIs inside `f`: any other allocation that
/// fails there aborts the process (`handle_alloc_error`).
pub fn fallible<R>(f: impl FnOnce() -> R) -> R {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            FALLIBLE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    FALLIBLE_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
    let _guard = Guard;
    f()
}

/// A non-zero value unique to each live thread: the address of this
/// thread's (destructor-free, const-initialised) `FALLIBLE_DEPTH` slot.
#[inline]
fn thread_token() -> usize {
    FALLIBLE_DEPTH.with(|d| ptr::from_ref(d).addr())
}

fn in_fallible_scope() -> bool {
    FALLIBLE_DEPTH.try_with(|d| d.get() > 0).unwrap_or(false)
}

/// Raw counter snapshot of a [`CountingAlloc`] (bytes are requested
/// `Layout` sizes, not the inner allocator's usable sizes). The byte counts
/// are exact; the call counts may under-count when several threads allocate
/// concurrently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Live bytes.
    pub current: usize,
    /// Highest `current` since the last peak reset.
    pub peak: usize,
    /// Successful `alloc`/`alloc_zeroed` calls.
    pub allocs: usize,
    /// `dealloc` calls.
    pub frees: usize,
    /// Successful `realloc` calls.
    pub reallocs: usize,
}

/// A [`GlobalAlloc`] wrapper that counts live bytes and enforces PHP's
/// `memory_limit`.
///
/// # Accounting
///
/// The hot path on the request's thread (see
/// [`bind_to_current_thread`](Self::bind_to_current_thread)) uses no locked
/// instructions at all: a thread-local address compare, relaxed
/// loads/stores of the live-byte and call counters, a relaxed load of the
/// precomputed soft threshold and a relaxed peak compare. Other threads pay
/// one `fetch_add`/`fetch_sub` on a separate counter; everything about limits happens on a `#[cold]`
/// path. All atomics are `Relaxed` except the over-limit record, which is
/// published with release/acquire so [`check`](Self::check) sees consistent
/// fields.
///
/// "Reported" usage (what `memory_get_usage()` returns) is
/// `raw − baseline + PHP_BASELINE` (saturating at 0), where `baseline` is the
/// raw usage captured by [`set_baseline`](Self::set_baseline) at request
/// start; the limit applies to reported usage, so the VM's own startup
/// structures do not count against a script's `memory_limit`.
///
/// "Real" usage (`memory_get_usage(true)`) approximates Zend's
/// `heap->real_size` — memory obtained from the OS in 2 MiB chunks — as the
/// reported usage rounded up to whole chunks (minimum one chunk). Zend maps
/// huge (> 2 MiB) blocks page-granular and caches freed chunks, so its real
/// figure can be a few chunks off ours; tests only compare these with `%d`
/// or relative to each other.
///
/// # Over-limit handling
///
/// Allocation sites cannot unwind, so crossing the soft limit does **not**
/// fail the allocation: it records an [`OverLimit`] (first one wins until
/// [`clear`](Self::clear)) that the VM raises as a fatal error at its next
/// safe point via [`check`](Self::check) — the same "delayed OOM" model as
/// `zend_mm_delayed_error()`. Builtins that know they are about to allocate
/// a lot call [`try_reserve`](Self::try_reserve) first (PHP's fallible
/// allocations) so the fatal is raised before anything is allocated.
///
/// Crossing the *hard* ceiling ([`hard_limit_for`]: 2× the limit, or the
/// limit plus 1 GiB above 1 GiB, tunable via `memory_hard_limit_percent`)
/// calls the
/// [hard-limit handler](Self::set_hard_limit_handler) if one is installed
/// (PHP prints "Hard memory limit …" and exits 137), otherwise returns null,
/// whereupon Rust's `handle_alloc_error` prints
/// `memory allocation of N bytes failed` and aborts. That is the backstop
/// that keeps a runaway builtin from taking the machine down.
pub struct CountingAlloc<A> {
    inner: A,
    /// Live bytes allocated/freed by the owner thread (written only by it,
    /// with plain load/store), and by every other thread (locked RMW). Both
    /// wrap; their wrapping sum is the exact live-byte count.
    owned: AtomicUsize,
    shared: AtomicUsize,
    /// [`thread_token`] of the owner thread, 0 for none.
    owner: AtomicUsize,
    peak: AtomicUsize,
    allocs: AtomicUsize,
    frees: AtomicUsize,
    reallocs: AtomicUsize,
    /// Raw usage at request start.
    baseline: AtomicUsize,
    /// Soft limit in reported bytes, `UNLIMITED` for none.
    limit: AtomicUsize,
    /// `memory_hard_limit_percent` (-1 = PHP default policy).
    hard_percent: AtomicI64,
    /// Raw-usage thresholds derived from limit/baseline (`UNLIMITED` = none).
    soft_raw: AtomicUsize,
    hard_raw: AtomicUsize,
    over_state: AtomicU8,
    over_limit: AtomicUsize,
    over_tried: AtomicUsize,
    hard_handler: OnceLock<HardLimitHandler>,
    in_hard_handler: AtomicBool,
}

impl<A> CountingAlloc<A> {
    /// Wraps `inner` with no limit and a zero baseline.
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            owned: AtomicUsize::new(0),
            shared: AtomicUsize::new(0),
            owner: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            allocs: AtomicUsize::new(0),
            frees: AtomicUsize::new(0),
            reallocs: AtomicUsize::new(0),
            baseline: AtomicUsize::new(0),
            limit: AtomicUsize::new(UNLIMITED),
            hard_percent: AtomicI64::new(-1),
            soft_raw: AtomicUsize::new(UNLIMITED),
            hard_raw: AtomicUsize::new(UNLIMITED),
            over_state: AtomicU8::new(NONE),
            over_limit: AtomicUsize::new(0),
            over_tried: AtomicUsize::new(0),
            hard_handler: OnceLock::new(),
            in_hard_handler: AtomicBool::new(false),
        }
    }

    /// The wrapped allocator.
    pub const fn inner(&self) -> &A {
        &self.inner
    }

    // ---- accounting -------------------------------------------------

    /// Raw counter snapshot.
    pub fn stats(&self) -> Stats {
        Stats {
            current: self.raw_usage(),
            peak: self.peak.load(Relaxed),
            allocs: self.allocs.load(Relaxed),
            frees: self.frees.load(Relaxed),
            reallocs: self.reallocs.load(Relaxed),
        }
    }

    /// Whether this allocator has served any allocation, i.e. whether it is
    /// actually installed as the global allocator. When it is not, usage
    /// stays at [`PHP_BASELINE`] and only [`try_reserve`](Self::try_reserve)
    /// enforces the limit.
    pub fn is_active(&self) -> bool {
        self.allocs.load(Relaxed) != 0
    }

    fn reported(&self, raw: usize) -> usize {
        raw.saturating_add(PHP_BASELINE)
            .saturating_sub(self.baseline.load(Relaxed))
    }

    /// `memory_get_usage(false)`: live bytes since the baseline plus
    /// [`PHP_BASELINE`].
    pub fn usage(&self) -> usize {
        self.reported(self.raw_usage())
    }

    /// `memory_get_usage(true)`: [`usage`](Self::usage) rounded up to 2 MiB
    /// chunks.
    pub fn real_usage(&self) -> usize {
        round_to_chunks(self.usage())
    }

    /// `memory_get_peak_usage(false)`.
    pub fn peak(&self) -> usize {
        self.reported(self.peak.load(Relaxed))
    }

    /// `memory_get_peak_usage(true)`.
    pub fn real_peak(&self) -> usize {
        round_to_chunks(self.peak())
    }

    /// `memory_get_usage($real_usage)`.
    pub fn memory_usage(&self, real_usage: bool) -> usize {
        if real_usage {
            self.real_usage()
        } else {
            self.usage()
        }
    }

    /// `memory_get_peak_usage($real_usage)`.
    pub fn peak_usage(&self, real_usage: bool) -> usize {
        if real_usage {
            self.real_peak()
        } else {
            self.peak()
        }
    }

    /// `memory_reset_peak_usage()`: the peak becomes the current usage.
    pub fn reset_peak(&self) {
        self.peak.store(self.raw_usage(), Relaxed);
    }

    /// Makes the current raw usage the zero point of reported usage (which
    /// then equals [`PHP_BASELINE`]) and resets the peak.
    pub fn set_baseline(&self) {
        let now = self.raw_usage();
        self.baseline.store(now, Relaxed);
        self.peak.store(now, Relaxed);
        self.update_thresholds();
    }

    // ---- limits -----------------------------------------------------

    /// The soft limit in bytes, `None` when unlimited.
    pub fn limit(&self) -> Option<usize> {
        match self.limit.load(Relaxed) {
            UNLIMITED => None,
            l => Some(l),
        }
    }

    /// The hard ceiling in bytes, `None` when unlimited.
    pub fn hard_limit(&self) -> Option<usize> {
        self.limit()
            .map(|l| hard_limit_for(l, self.hard_percent.load(Relaxed)))
    }

    /// `zend_set_memory_limit()`: sets the soft limit (`None` = unlimited;
    /// values ≥ [`LIMIT_MAX`] also mean unlimited). Fails, leaving the limit
    /// unchanged, if the new limit is below the current real usage.
    pub fn set_limit(&self, limit: Option<usize>) -> Result<(), SetLimitError> {
        if let Some(requested) = limit.filter(|&l| l < LIMIT_MAX) {
            let current = self.real_usage();
            if requested < current {
                return Err(SetLimitError { requested, current });
            }
        }
        self.force_limit(limit);
        Ok(())
    }

    /// Sets the soft limit without the current-usage check (for request
    /// shutdown, where PHP ignores the failure too).
    pub fn force_limit(&self, limit: Option<usize>) {
        let l = limit.filter(|&l| l < LIMIT_MAX).unwrap_or(UNLIMITED);
        self.limit.store(l, Relaxed);
        self.update_thresholds();
    }

    /// `memory_hard_limit_percent` (`-1` = PHP's default policy, otherwise
    /// ≥ 100). Fails like `OnChangeMemoryHardLimitPercent`.
    pub fn set_hard_limit_percent(&self, percent: i64) -> Result<(), HardLimitPercentError> {
        if percent < 100 && percent != -1 {
            return Err(HardLimitPercentError::Invalid(percent));
        }
        if self.limit().is_some_and(|l| self.real_usage() > l) {
            return Err(HardLimitPercentError::OverSoftLimit(percent));
        }
        self.hard_percent.store(percent, Relaxed);
        self.update_thresholds();
        Ok(())
    }

    /// `OnChangeMemoryLimit` for an already-parsed `memory_limit` value
    /// (`php_support::ini::parse_uquantity`, so `-1` is `u64::MAX`) against
    /// `max_memory_limit` (likewise parsed; `u64::MAX` when unset).
    ///
    /// Divergence: on clamping PHP updates only the INI value and leaves the
    /// heap limit as it was; we apply `max_memory_limit` as the limit.
    pub fn apply_memory_limit_ini(
        &self,
        value: u64,
        max_memory_limit: u64,
    ) -> Result<MemoryLimitIni, SetLimitError> {
        if value > max_memory_limit {
            self.force_limit(limit_from_ini(max_memory_limit));
            let warning =
                (value != u64::MAX).then(|| clamp_to_max_warning(value, max_memory_limit));
            return Ok(MemoryLimitIni::ClampedToMax {
                max: max_memory_limit,
                warning,
            });
        }
        self.set_limit(limit_from_ini(value))?;
        Ok(MemoryLimitIni::Applied)
    }

    fn update_thresholds(&self) {
        let (soft, hard) = match self.limit() {
            None => (UNLIMITED, UNLIMITED),
            Some(l) => {
                // reported > limit  <=>  raw > limit + baseline - PHP_BASELINE
                let base = self.baseline.load(Relaxed);
                let to_raw = |v: usize| v.saturating_add(base).saturating_sub(PHP_BASELINE);
                let hard = hard_limit_for(l, self.hard_percent.load(Relaxed));
                (to_raw(l), to_raw(hard))
            }
        };
        self.soft_raw.store(soft, Relaxed);
        self.hard_raw.store(hard, Relaxed);
    }

    // ---- over-limit record -------------------------------------------

    fn record(&self, over: OverLimit) {
        if self
            .over_state
            .compare_exchange(NONE, RECORDING, Acquire, Relaxed)
            .is_ok()
        {
            self.over_limit.store(over.limit, Relaxed);
            self.over_tried.store(over.tried, Relaxed);
            self.over_state.store(PENDING, Release);
        }
    }

    /// The pending soft-limit violation, if any. Cheap (one load); the VM
    /// calls this at safe points and raises
    /// `Fatal error: {over}` when it returns `Some`.
    #[inline]
    pub fn check(&self) -> Option<OverLimit> {
        if self.over_state.load(Acquire) == PENDING {
            Some(OverLimit {
                limit: self.over_limit.load(Relaxed),
                tried: self.over_tried.load(Relaxed),
            })
        } else {
            None
        }
    }

    /// Whether a violation is pending (`ZEND_HAS_OOM()`).
    #[inline]
    pub fn is_pending(&self) -> bool {
        self.over_state.load(Relaxed) == PENDING
    }

    /// Forgets the pending violation (after the fatal has been reported) so
    /// the next one can be recorded.
    pub fn clear(&self) {
        let _ = self
            .over_state
            .compare_exchange(PENDING, NONE, Release, Relaxed);
    }

    /// Whether `bytes` more would still fit under the soft limit.
    pub fn would_fit(&self, bytes: usize) -> bool {
        self.limit()
            .is_none_or(|l| self.usage().saturating_add(bytes) <= l)
    }

    /// Fallible-allocation check for builtins: if allocating `bytes` more
    /// would exceed the soft limit, records the violation (so the VM's next
    /// [`check`](Self::check) raises the fatal) and returns it; the builtin
    /// should then return without allocating (PHP's `RETURN_OOM()`).
    pub fn try_reserve(&self, bytes: usize) -> Result<(), OverLimit> {
        match self.limit() {
            Some(limit) if !self.would_fit(bytes) => {
                let over = OverLimit {
                    limit,
                    tried: bytes,
                };
                self.record(over);
                Err(over)
            }
            _ => Ok(()),
        }
    }

    /// [`try_reserve`](Self::try_reserve) for `count` elements of
    /// `elem_size` bytes plus `extra`, treating arithmetic overflow as a
    /// violation of `usize::MAX` bytes.
    pub fn try_reserve_n(
        &self,
        count: usize,
        elem_size: usize,
        extra: usize,
    ) -> Result<(), OverLimit> {
        let bytes = count
            .checked_mul(elem_size)
            .and_then(|b| b.checked_add(extra))
            .unwrap_or(usize::MAX);
        self.try_reserve(bytes)
    }

    /// Installs the function called (once) when an allocation would cross
    /// the hard ceiling. It runs inside the allocator with the ceiling
    /// disabled, may allocate, and must terminate the process — the PHP
    /// behaviour is to print `"{exceeded} in {file} on line {line}\n"` to
    /// stderr and exit with status 137. Returns `false` if a handler was
    /// already installed.
    pub fn set_hard_limit_handler(&self, handler: HardLimitHandler) -> bool {
        self.hard_handler.set(handler).is_ok()
    }

    // ---- request lifecycle ------------------------------------------

    /// Request startup: baseline the counters, clear any stale violation and
    /// apply the INI `memory_limit`. On error the limit is left unlimited
    /// and the caller should warn and fall back to [`DEFAULT_LIMIT`]
    /// (PHP keeps the previous value, which at startup is the default).
    ///
    /// [`DEFAULT_LIMIT`]: crate::DEFAULT_LIMIT
    pub fn request_start(&self, limit: Option<usize>) -> Result<(), SetLimitError> {
        self.bind_to_current_thread();
        self.force_limit(None);
        self.set_baseline();
        self.clear();
        self.set_limit(limit)
    }

    /// Request shutdown: lift the limit, clear any violation, reset baseline
    /// and peak.
    pub fn request_end(&self) {
        self.force_limit(None);
        self.clear();
        self.baseline.store(0, Relaxed);
        self.reset_peak();
        self.update_thresholds();
    }

    // ---- allocator hot path -----------------------------------------

    /// Accounts `size` more live bytes; `false` if the allocation must fail.
    #[inline]
    fn admit(&self, size: usize) -> bool {
        let new = self.add_live(size);
        if new > self.soft_raw.load(Relaxed) && !self.admit_over_soft(new, size) {
            self.release(size);
            return false;
        }
        if new > self.peak.load(Relaxed) {
            self.peak.fetch_max(new, Relaxed);
        }
        true
    }

    #[cold]
    #[inline(never)]
    fn admit_over_soft(&self, new: usize, size: usize) -> bool {
        let in_handler = self.in_hard_handler.load(Relaxed);
        if new > self.hard_raw.load(Relaxed) && !in_handler {
            if let Some(handler) = self.hard_handler.get() {
                if !self.in_hard_handler.swap(true, Relaxed) {
                    let hard_limit = self.hard_limit().unwrap_or(LIMIT_MAX);
                    handler(HardLimitExceeded {
                        hard_limit,
                        tried: size,
                    });
                }
            }
            return false;
        }
        if in_handler {
            return true;
        }
        let limit = self.limit.load(Relaxed);
        if limit != UNLIMITED {
            self.record(OverLimit { limit, tried: size });
        }
        !in_fallible_scope()
    }

    #[inline]
    fn is_owner(&self) -> bool {
        self.owner.load(Relaxed) == thread_token()
    }

    /// Adds `size` live bytes, returning the new total.
    #[inline]
    fn add_live(&self, size: usize) -> usize {
        if self.is_owner() {
            let owned = self.owned.load(Relaxed).wrapping_add(size);
            self.owned.store(owned, Relaxed);
            owned.wrapping_add(self.shared.load(Relaxed))
        } else {
            let shared = self.shared.fetch_add(size, Relaxed).wrapping_add(size);
            shared.wrapping_add(self.owned.load(Relaxed))
        }
    }

    #[inline]
    fn release(&self, size: usize) {
        if self.is_owner() {
            self.owned
                .store(self.owned.load(Relaxed).wrapping_sub(size), Relaxed);
        } else {
            self.shared.fetch_sub(size, Relaxed);
        }
    }

    /// Live bytes, exact.
    pub fn raw_usage(&self) -> usize {
        self.owned
            .load(Relaxed)
            .wrapping_add(self.shared.load(Relaxed))
    }

    /// Makes the calling thread the *owner*: its allocations and frees are
    /// accounted with plain loads/stores instead of locked instructions
    /// (PHP runs a request on one thread, so this is the fast path). Other
    /// threads keep using atomic read-modify-writes, so totals stay exact.
    /// [`request_start`](Self::request_start) calls this; do not move
    /// ownership while the previous owner thread is still allocating.
    pub fn bind_to_current_thread(&self) {
        self.owner.store(thread_token(), Relaxed);
    }
}

impl<A> fmt::Debug for CountingAlloc<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountingAlloc")
            .field("stats", &self.stats())
            .field("usage", &self.usage())
            .field("limit", &self.limit())
            .field("pending", &self.check())
            .finish_non_exhaustive()
    }
}

// SAFETY: every method forwards to `inner` with the caller's arguments
// unchanged and returns its result unchanged, or returns null (a permitted
// "allocation failed" result) before calling it. The counters never affect
// the memory handed out, so `inner`'s guarantees carry over.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAlloc<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.admit(layout.size()) {
            return ptr::null_mut();
        }
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
        let p = unsafe { self.inner.alloc(layout) };
        if p.is_null() {
            self.release(layout.size());
        } else {
            bump(&self.allocs);
        }
        p
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !self.admit(layout.size()) {
            return ptr::null_mut();
        }
        // SAFETY: caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let p = unsafe { self.inner.alloc_zeroed(layout) };
        if p.is_null() {
            self.release(layout.size());
        } else {
            bump(&self.allocs);
        }
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller guarantees `ptr` was allocated by this allocator
        // (hence by `inner`) with `layout`.
        unsafe { self.inner.dealloc(ptr, layout) };
        self.release(layout.size());
        bump(&self.frees);
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        if new_size > old_size {
            let grow = new_size - old_size;
            if !self.admit(grow) {
                return ptr::null_mut();
            }
            // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
            let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
            if p.is_null() {
                self.release(grow);
            } else {
                bump(&self.reallocs);
            }
            p
        } else {
            // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
            let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
            if !p.is_null() {
                self.release(old_size - new_size);
                bump(&self.reallocs);
            }
            p
        }
    }
}

// SAFETY: forwards to the `GlobalAlloc` impl of `CountingAlloc<A>` above.
// This impl lets `#[global_allocator]` name a `&'static` to a shared
// instance such as `crate::GLOBAL`.
unsafe impl<A: GlobalAlloc> GlobalAlloc for &CountingAlloc<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract, forwarded.
        unsafe { (**self).alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract, forwarded.
        unsafe { (**self).alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract, forwarded.
        unsafe { (**self).dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract, forwarded.
        unsafe { (**self).realloc(ptr, layout, new_size) }
    }
}
