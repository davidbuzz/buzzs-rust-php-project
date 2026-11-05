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
    chunk_floor, clamp_to_max_warning, hard_limit_for, limit_from_ini, round_to_chunks,
    zend_limit_allows, HardLimitExceeded, HardLimitPercentError, MemoryLimitIni, OverLimit,
    SetLimitError, LIMIT_MAX, MAX_LARGE_SIZE, PHP_BASELINE,
};

/// Signature of the hard-ceiling handler (see
/// [`CountingAlloc::set_hard_limit_handler`]). It must not return.
pub type HardLimitHandler = fn(HardLimitExceeded) -> !;

/// `over_state` values.
const NONE: u8 = 0;
const RECORDING: u8 = 1;
const PENDING: u8 = 2;
const HANDLED: u8 = 3;

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

/// Capacity of the registry of [uncounted](crate::uncounted) blocks. Huge
/// allocations made in an uncounted scope while it is full are counted
/// as usual.
const UNCOUNTED_SLOTS: usize = 64;

/// Registry slot states besides a block address.
const SLOT_FREE: usize = 0;
/// Reserved for a block being (re)allocated; never a block address.
const SLOT_CLAIMED: usize = usize::MAX;

thread_local! {
    /// Nesting depth of [`crate::fallible`] on this thread. Const-initialised
    /// and without a destructor, so reading it from inside the allocator
    /// never allocates.
    static FALLIBLE_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Nesting depth of [`crate::uncounted`] on this thread (same
    /// constraints as `FALLIBLE_DEPTH`).
    static UNCOUNTED_DEPTH: Cell<u32> = const { Cell::new(0) };
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

/// Runs `f` with *huge* allocations (above `ZEND_MM_MAX_LARGE_SIZE`,
/// 2 MiB − 4 KiB) left out of
/// the accounting: they neither count toward `memory_get_usage()` and the
/// peak nor are checked against the soft limit or the hard ceiling, for
/// their whole lifetime — reallocations and the final free stay uncounted
/// wherever they happen. Smaller allocations are counted as usual.
///
/// This models memory a PHP extension obtains from a library's own
/// `malloc` rather than from the Zend heap: `ext/gmp` leaves libgmp on the
/// system allocator (it never calls `mp_set_memory_functions`), so a GMP
/// number of any size is invisible to `memory_limit`. Only the address
/// space limit (`RLIMIT_AS`, set by `php-cli`) bounds such blocks.
///
/// At most 64 uncounted blocks are live at a time; beyond that, huge
/// allocations made in the scope are counted normally.
pub fn uncounted<R>(f: impl FnOnce() -> R) -> R {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            UNCOUNTED_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    UNCOUNTED_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
    let _guard = Guard;
    f()
}

fn in_uncounted_scope() -> bool {
    UNCOUNTED_DEPTH.try_with(|d| d.get() > 0).unwrap_or(false)
}

/// What [`CountingAlloc::admit`] decided about an allocation.
enum Admit {
    /// Counted; proceed.
    Counted,
    /// Must fail (hard ceiling, or a fallible soft overrun).
    Refused,
    /// Huge and [uncounted](uncounted): not accounted; the registry slot
    /// reserved for it.
    Uncounted(usize),
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
    /// Bytes of the allocation that recorded the request's memory-limit
    /// violation. Zend never makes that allocation (the fatal unwinds
    /// first); we must grant it, so it is left out of later limit checks.
    forgiven: AtomicUsize,
    /// Of [`forgiven`](Self::forgiven), the part that was a huge block.
    forgiven_huge: AtomicUsize,
    /// Live counted bytes in huge blocks (requests over
    /// `ZEND_MM_MAX_LARGE_SIZE`), which Zend maps on their own instead of
    /// carving out of chunks.
    huge: AtomicUsize,
    /// [`huge`](Self::huge) at the baseline: huge blocks allocated before
    /// the request (VM structures) do not count, like the rest of the
    /// baseline.
    huge_base: AtomicUsize,
    hard_handler: OnceLock<HardLimitHandler>,
    in_hard_handler: AtomicBool,
    /// Zend's tracked allocator (`USE_ZEND_ALLOC=0 USE_TRACKED_ALLOC=1`):
    /// real usage is the exact live-byte count, not whole chunks.
    tracked: AtomicBool,
    /// Addresses of live [uncounted](uncounted) blocks (`SLOT_FREE`,
    /// `SLOT_CLAIMED` or an address).
    uncounted_slots: [AtomicUsize; UNCOUNTED_SLOTS],
    /// Slots not `SLOT_FREE`: lets frees of huge counted blocks skip the
    /// registry scan while no uncounted block is live.
    uncounted_used: AtomicUsize,
    /// Bytes held by live uncounted blocks.
    uncounted_bytes: AtomicUsize,
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
            forgiven: AtomicUsize::new(0),
            forgiven_huge: AtomicUsize::new(0),
            huge: AtomicUsize::new(0),
            huge_base: AtomicUsize::new(0),
            hard_handler: OnceLock::new(),
            in_hard_handler: AtomicBool::new(false),
            tracked: AtomicBool::new(false),
            uncounted_slots: [const { AtomicUsize::new(SLOT_FREE) }; UNCOUNTED_SLOTS],
            uncounted_used: AtomicUsize::new(0),
            uncounted_bytes: AtomicUsize::new(0),
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

    /// Bytes held by live [uncounted](uncounted) blocks (not part of any
    /// usage figure).
    pub fn uncounted_bytes(&self) -> usize {
        self.uncounted_bytes.load(Relaxed)
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
    /// chunks, or exactly [`usage`](Self::usage) in
    /// [tracked mode](Self::set_tracked).
    pub fn real_usage(&self) -> usize {
        if self.is_tracked() {
            return self.usage();
        }
        // `heap->real_size`: whole chunks for small/large blocks plus the
        // huge blocks' own pages.
        let huge = self.request_huge();
        round_to_chunks(self.usage().saturating_sub(huge)).saturating_add(huge)
    }

    fn to_real(&self, bytes: usize) -> usize {
        if self.is_tracked() {
            bytes
        } else {
            round_to_chunks(bytes)
        }
    }

    /// Emulates Zend's tracked allocator (`tracked_malloc`, selected by
    /// `USE_ZEND_ALLOC=0` plus `USE_TRACKED_ALLOC=1`): `heap->real_size`
    /// equals `heap->size`, so real usage — which `memory_limit` changes are
    /// checked against — is no longer rounded to [`CHUNK_SIZE`](crate::CHUNK_SIZE).
    pub fn set_tracked(&self, tracked: bool) {
        self.tracked.store(tracked, Relaxed);
        self.update_thresholds();
    }

    /// Whether [tracked mode](Self::set_tracked) is on.
    pub fn is_tracked(&self) -> bool {
        self.tracked.load(Relaxed)
    }

    /// `memory_get_peak_usage(false)`.
    pub fn peak(&self) -> usize {
        self.reported(self.peak.load(Relaxed))
    }

    /// `memory_get_peak_usage(true)`.
    pub fn real_peak(&self) -> usize {
        self.to_real(self.peak())
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
        self.huge_base.store(self.huge.load(Relaxed), Relaxed);
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
                // Small and large blocks only fail when a new chunk would
                // pass the limit (`zend_limit_allows`); huge ones are routed
                // to the slow path by size in `admit`.
                // The forgiven block does not exist for Zend: it counts
                // against neither the soft limit nor the hard ceiling.
                let forgiven = self.forgiven.load(Relaxed);
                let soft = if self.is_tracked() {
                    l.saturating_add(forgiven)
                } else {
                    // A small block fails once the small/large usage would
                    // need a chunk past what the huge blocks leave free.
                    let huge = self.request_huge();
                    let forgiven_huge = self.forgiven_huge.load(Relaxed);
                    let huge_eff = huge.saturating_sub(forgiven_huge);
                    chunk_floor(l.saturating_sub(huge_eff))
                        .saturating_add(huge)
                        .saturating_add(forgiven.saturating_sub(forgiven_huge))
                };
                (to_raw(soft), to_raw(hard).saturating_add(forgiven))
            }
        };
        self.soft_raw.store(soft, Relaxed);
        self.hard_raw.store(hard, Relaxed);
    }

    // ---- over-limit record -------------------------------------------

    /// Records `over` unless a violation is already recorded or handled;
    /// whether it was recorded.
    fn record(&self, over: OverLimit) -> bool {
        if self
            .over_state
            .compare_exchange(NONE, RECORDING, Acquire, Relaxed)
            .is_ok()
        {
            self.over_limit.store(over.limit, Relaxed);
            self.over_tried.store(over.tried, Relaxed);
            self.over_state.store(PENDING, Release);
            true
        } else {
            false
        }
    }

    /// The pending soft-limit violation, if any, without consuming it.
    /// Cheap (one load). The VM uses [`take`](Self::take) to report it.
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

    /// Whether a violation is pending and not yet taken. One relaxed load:
    /// this is the per-opcode poll.
    #[inline]
    pub fn is_pending(&self) -> bool {
        self.over_state.load(Relaxed) == PENDING
    }

    /// Takes the pending violation for reporting (`zend_handle_delayed_oom`):
    /// returns it and moves to the *handled* state, in which
    /// [`check`](Self::check) and [`is_pending`](Self::is_pending) report
    /// nothing and no new violation is recorded — PHP raises at most one
    /// memory-limit fatal per request phase, so the unwinding that follows
    /// the fatal cannot trigger another. [`clear`](Self::clear) (at the
    /// start of request shutdown) re-arms recording.
    pub fn take(&self) -> Option<OverLimit> {
        let over = self.check()?;
        self.over_state
            .compare_exchange(PENDING, HANDLED, Relaxed, Relaxed)
            .ok()
            .map(|_| over)
    }

    /// Whether a violation was taken and recording is suspended until
    /// [`clear`](Self::clear).
    pub fn is_handled(&self) -> bool {
        self.over_state.load(Relaxed) == HANDLED
    }

    /// Forgets any pending or handled violation so the next one can be
    /// recorded (`EG(delayed_oom).state = ZEND_OOM_STATE_NONE` at the start
    /// of `php_request_shutdown`).
    pub fn clear(&self) {
        for from in [PENDING, HANDLED] {
            let _ = self
                .over_state
                .compare_exchange(from, NONE, Release, Relaxed);
        }
    }

    /// Whether `bytes` more would still fit under the soft limit.
    pub fn would_fit(&self, bytes: usize) -> bool {
        self.limit()
            .is_none_or(|l| self.limit_allows(self.usage(), bytes, bytes > MAX_LARGE_SIZE, l))
    }

    /// Whether `size` more bytes at reported usage `usage` (not counting
    /// them) pass `zend_mm_check_limit` under `limit` (see
    /// [`zend_limit_allows`]; exact bytes in [tracked mode](Self::set_tracked)).
    fn limit_allows(&self, usage: usize, size: usize, huge_request: bool, limit: usize) -> bool {
        let usage = usage.saturating_sub(self.forgiven.load(Relaxed));
        if self.is_tracked() {
            return usage.saturating_add(size) <= limit;
        }
        let huge = self
            .request_huge()
            .saturating_sub(self.forgiven_huge.load(Relaxed));
        let small = usage.saturating_sub(huge);
        // A realloc growing into a huge block is checked as a huge request.
        let size_class = if huge_request && size <= MAX_LARGE_SIZE {
            MAX_LARGE_SIZE + 1
        } else {
            size
        };
        if size_class != size {
            let block = size.div_ceil(crate::limit::PAGE_SIZE) * crate::limit::PAGE_SIZE;
            return round_to_chunks(small)
                .saturating_add(huge)
                .saturating_add(block)
                <= limit;
        }
        zend_limit_allows(small, huge, size, limit)
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
        self.forgiven.store(0, Relaxed);
        self.forgiven_huge.store(0, Relaxed);
        self.force_limit(None);
        self.set_baseline();
        self.clear();
        self.set_limit(limit)
    }

    /// Request shutdown: lift the limit, clear any violation, reset baseline
    /// and peak.
    pub fn request_end(&self) {
        self.forgiven.store(0, Relaxed);
        self.forgiven_huge.store(0, Relaxed);
        self.force_limit(None);
        self.clear();
        self.baseline.store(0, Relaxed);
        self.huge_base.store(0, Relaxed);
        self.reset_peak();
        self.update_thresholds();
    }

    // ---- allocator hot path -----------------------------------------

    /// Accounts `size` more live bytes, unless the allocation must fail or
    /// is a huge one made in an [uncounted](uncounted) scope.
    #[inline]
    fn admit(&self, size: usize, huge: bool) -> Admit {
        let new = self.add_live(size);
        if new > self.soft_raw.load(Relaxed) || huge {
            return self.admit_slow(new, size, huge);
        }
        if new > self.peak.load(Relaxed) {
            self.peak.fetch_max(new, Relaxed);
        }
        Admit::Counted
    }

    /// [`admit`](Self::admit) past the soft threshold or for a huge block.
    #[cold]
    #[inline(never)]
    fn admit_slow(&self, new: usize, size: usize, huge: bool) -> Admit {
        if size > MAX_LARGE_SIZE && in_uncounted_scope() {
            if let Some(slot) = self.claim_uncounted_slot() {
                self.release(size);
                return Admit::Uncounted(slot);
            }
        }
        if !self.admit_over_soft(new, size, huge) {
            self.release(size);
            return Admit::Refused;
        }
        if new > self.peak.load(Relaxed) {
            self.peak.fetch_max(new, Relaxed);
        }
        Admit::Counted
    }

    #[cold]
    #[inline(never)]
    fn admit_over_soft(&self, new: usize, size: usize, huge: bool) -> bool {
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
        if limit == UNLIMITED {
            return true;
        }
        let before = self.reported(new).saturating_sub(size);
        if self.limit_allows(before, size, huge, limit) {
            return true;
        }
        let recorded = self.record(OverLimit { limit, tried: size });
        let granted = !in_fallible_scope();
        if recorded && granted {
            // Zend's failing allocation never happens; ours does, so leave
            // it out of the checks for the rest of the request (shutdown
            // functions and destructors still run under the limit).
            self.forgiven.fetch_add(size, Relaxed);
            if huge {
                self.forgiven_huge.fetch_add(size, Relaxed);
            }
            self.update_thresholds();
        }
        granted
    }

    /// Bookkeeping for huge blocks (cold: they are 2 MiB or more): `add`
    /// and `sub` live bytes, then re-derive the thresholds.
    #[cold]
    #[inline(never)]
    fn huge_change(&self, add: usize, sub: usize) {
        if add >= sub {
            self.huge.fetch_add(add - sub, Relaxed);
        } else {
            let _ = self
                .huge
                .fetch_update(Relaxed, Relaxed, |h| Some(h.saturating_sub(sub - add)));
        }
        self.update_thresholds();
    }

    /// Live huge-block bytes allocated since the baseline.
    fn request_huge(&self) -> usize {
        self.huge
            .load(Relaxed)
            .saturating_sub(self.huge_base.load(Relaxed))
    }

    // ---- uncounted-block registry ------------------------------------

    /// Reserves a free registry slot (`SLOT_CLAIMED`), `None` when full.
    fn claim_uncounted_slot(&self) -> Option<usize> {
        let slot = self.uncounted_slots.iter().position(|s| {
            s.compare_exchange(SLOT_FREE, SLOT_CLAIMED, Relaxed, Relaxed)
                .is_ok()
        })?;
        self.uncounted_used.fetch_add(1, Relaxed);
        Some(slot)
    }

    /// Returns `slot` to the free pool.
    fn free_uncounted_slot(&self, slot: usize) {
        self.uncounted_slots[slot].store(SLOT_FREE, Relaxed);
        self.uncounted_used.fetch_sub(1, Relaxed);
    }

    /// Records the uncounted block `p` of `size` bytes in its claimed
    /// `slot`, or frees the slot if the inner allocation failed.
    fn register_uncounted(&self, slot: usize, p: *mut u8, size: usize) {
        if p.is_null() {
            self.free_uncounted_slot(slot);
        } else {
            self.uncounted_slots[slot].store(p.addr(), Relaxed);
            self.uncounted_bytes.fetch_add(size, Relaxed);
        }
    }

    /// The registry slot holding block `p`, if it is uncounted.
    fn uncounted_slot_of(&self, p: *mut u8) -> Option<usize> {
        if self.uncounted_used.load(Relaxed) == 0 {
            return None;
        }
        let addr = p.addr();
        self.uncounted_slots
            .iter()
            .position(|s| s.load(Relaxed) == addr)
    }

    /// If the huge block `p` of `size` bytes about to be freed is
    /// uncounted, drops it from the registry and returns `true`.
    #[cold]
    #[inline(never)]
    fn forget_uncounted(&self, p: *mut u8, size: usize) -> bool {
        let Some(slot) = self.uncounted_slot_of(p) else {
            return false;
        };
        self.free_uncounted_slot(slot);
        self.uncounted_bytes.fetch_sub(size, Relaxed);
        true
    }

    /// Accounts `size` live bytes without any limit check (a block that
    /// leaves the registry by shrinking).
    fn count_granted(&self, size: usize) {
        let new = self.add_live(size);
        if new > self.peak.load(Relaxed) {
            self.peak.fetch_max(new, Relaxed);
        }
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

impl<A: GlobalAlloc> CountingAlloc<A> {
    /// `realloc` when the old or the new size is huge: keeps uncounted
    /// blocks out of the accounting, moves a counted block that grows huge
    /// inside an [uncounted](uncounted) scope into the registry, and
    /// otherwise defers to [`realloc_counted`](Self::realloc_counted).
    ///
    /// # Safety
    ///
    /// The caller upholds `GlobalAlloc::realloc`'s contract.
    #[cold]
    #[inline(never)]
    unsafe fn realloc_huge(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        if old_size > MAX_LARGE_SIZE {
            if let Some(slot) = self.uncounted_slot_of(ptr) {
                // Park the slot while the block moves, so a concurrent free
                // of a counted block reusing the old address cannot match.
                self.uncounted_slots[slot].store(SLOT_CLAIMED, Relaxed);
                // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
                let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
                if p.is_null() {
                    self.uncounted_slots[slot].store(ptr.addr(), Relaxed);
                    return p;
                }
                bump(&self.reallocs);
                self.uncounted_bytes.fetch_sub(old_size, Relaxed);
                if new_size > MAX_LARGE_SIZE {
                    self.register_uncounted(slot, p, new_size);
                } else {
                    // `dealloc` only consults the registry for huge sizes.
                    self.free_uncounted_slot(slot);
                    self.count_granted(new_size);
                }
                return p;
            }
        }
        if new_size > old_size && new_size > MAX_LARGE_SIZE && in_uncounted_scope() {
            if let Some(slot) = self.claim_uncounted_slot() {
                // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
                let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
                self.register_uncounted(slot, p, new_size);
                if !p.is_null() {
                    self.release(old_size);
                    bump(&self.reallocs);
                    if old_size > MAX_LARGE_SIZE {
                        self.huge_change(0, old_size);
                    }
                }
                return p;
            }
        }
        // SAFETY: forwarded unchanged.
        unsafe { self.realloc_counted(ptr, layout, new_size) }
    }

    /// `realloc` of a counted block.
    ///
    /// # Safety
    ///
    /// The caller upholds `GlobalAlloc::realloc`'s contract.
    #[inline]
    unsafe fn realloc_counted(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        if new_size > old_size {
            let grow = new_size - old_size;
            match self.admit(grow, new_size > MAX_LARGE_SIZE) {
                Admit::Counted => {}
                Admit::Refused => return ptr::null_mut(),
                // Only a huge growth in an uncounted scope whose registry
                // was full a moment ago gets here: count it after all.
                Admit::Uncounted(slot) => {
                    self.free_uncounted_slot(slot);
                    self.count_granted(grow);
                }
            }
            // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
            let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
            if p.is_null() {
                self.release(grow);
            } else {
                bump(&self.reallocs);
                self.huge_realloc(old_size, new_size);
            }
            p
        } else {
            // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
            let p = unsafe { self.inner.realloc(ptr, layout, new_size) };
            if !p.is_null() {
                self.release(old_size - new_size);
                bump(&self.reallocs);
                self.huge_realloc(old_size, new_size);
            }
            p
        }
    }

    /// Huge-block bookkeeping after a counted realloc from `old_size` to
    /// `new_size` bytes.
    #[inline]
    fn huge_realloc(&self, old_size: usize, new_size: usize) {
        let old_huge = old_size > MAX_LARGE_SIZE;
        let new_huge = new_size > MAX_LARGE_SIZE;
        if old_huge || new_huge {
            self.huge_change(
                if new_huge { new_size } else { 0 },
                if old_huge { old_size } else { 0 },
            );
        }
    }

    /// `alloc`/`alloc_zeroed` of an [uncounted](uncounted) block whose
    /// registry `slot` [`admit`](Self::admit) reserved.
    ///
    /// # Safety
    ///
    /// The caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
    #[cold]
    #[inline(never)]
    unsafe fn alloc_uncounted(&self, layout: Layout, slot: usize, zeroed: bool) -> *mut u8 {
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        let p = unsafe {
            if zeroed {
                self.inner.alloc_zeroed(layout)
            } else {
                self.inner.alloc(layout)
            }
        };
        self.register_uncounted(slot, p, layout.size());
        if !p.is_null() {
            bump(&self.allocs);
        }
        p
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
        let huge = layout.size() > MAX_LARGE_SIZE;
        match self.admit(layout.size(), huge) {
            Admit::Counted => {}
            Admit::Refused => return ptr::null_mut(),
            // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
            Admit::Uncounted(slot) => return unsafe { self.alloc_uncounted(layout, slot, false) },
        }
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
        let p = unsafe { self.inner.alloc(layout) };
        if p.is_null() {
            self.release(layout.size());
        } else {
            bump(&self.allocs);
            if huge {
                self.huge_change(layout.size(), 0);
            }
        }
        p
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let huge = layout.size() > MAX_LARGE_SIZE;
        match self.admit(layout.size(), huge) {
            Admit::Counted => {}
            Admit::Refused => return ptr::null_mut(),
            // SAFETY: caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
            Admit::Uncounted(slot) => return unsafe { self.alloc_uncounted(layout, slot, true) },
        }
        // SAFETY: caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let p = unsafe { self.inner.alloc_zeroed(layout) };
        if p.is_null() {
            self.release(layout.size());
        } else {
            bump(&self.allocs);
            if huge {
                self.huge_change(layout.size(), 0);
            }
        }
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        // Unregister before the block can be reused by another thread.
        let uncounted = size > MAX_LARGE_SIZE && self.forget_uncounted(ptr, size);
        // SAFETY: caller guarantees `ptr` was allocated by this allocator
        // (hence by `inner`) with `layout`.
        unsafe { self.inner.dealloc(ptr, layout) };
        if !uncounted {
            self.release(size);
            if size > MAX_LARGE_SIZE {
                self.huge_change(0, size);
            }
        }
        bump(&self.frees);
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size().max(new_size) > MAX_LARGE_SIZE {
            // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
            return unsafe { self.realloc_huge(ptr, layout, new_size) };
        }
        // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { self.realloc_counted(ptr, layout, new_size) }
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
