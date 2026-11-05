//! Unit tests over private `CountingAlloc` instances (never `GLOBAL`, which
//! is not installed in the library's test binary).

use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;

const M: usize = 1024 * 1024;

/// An allocator that always fails, to exercise the rollback paths.
struct FailingAlloc;

// SAFETY: never hands out memory; `dealloc`/`realloc` are unreachable for
// pointers it did not produce, and returning null is always permitted.
unsafe impl GlobalAlloc for FailingAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        ptr::null_mut()
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

/// Live allocation made through a test instance; freed on drop.
struct Block<'a> {
    a: &'a CountingAlloc<System>,
    p: *mut u8,
    layout: Layout,
}

impl Drop for Block<'_> {
    fn drop(&mut self) {
        if !self.p.is_null() {
            // SAFETY: `p` came from `a.alloc(layout)`/`a.realloc` with `layout`.
            unsafe { self.a.dealloc(self.p, self.layout) };
        }
    }
}

fn alloc(a: &CountingAlloc<System>, size: usize) -> Block<'_> {
    let layout = Layout::from_size_align(size, 8).expect("layout");
    // SAFETY: `layout` has non-zero size in every caller.
    let p = unsafe { a.alloc(layout) };
    Block { a, p, layout }
}

fn grow(b: &mut Block<'_>, new_size: usize) {
    // SAFETY: `b.p` is live, allocated with `b.layout`; new_size > 0.
    let p = unsafe { b.a.realloc(b.p, b.layout, new_size) };
    if !p.is_null() {
        b.p = p;
        b.layout = Layout::from_size_align(new_size, 8).expect("layout");
    }
}

#[test]
fn accounting_math() {
    let a = CountingAlloc::new(System);
    assert!(!a.is_active());
    assert_eq!(a.usage(), PHP_BASELINE);
    {
        let _b1 = alloc(&a, 1000);
        let _b2 = alloc(&a, 24);
        assert_eq!(a.stats().current, 1024);
        assert_eq!(a.usage(), PHP_BASELINE + 1024);
        assert!(a.is_active());
    }
    let s = a.stats();
    assert_eq!((s.current, s.peak, s.allocs, s.frees), (0, 1024, 2, 2));
    assert_eq!(a.peak(), PHP_BASELINE + 1024);
}

#[test]
fn realloc_accounting() {
    let a = CountingAlloc::new(System);
    let mut b = alloc(&a, 100);
    grow(&mut b, 5000);
    assert_eq!(a.stats().current, 5000);
    grow(&mut b, 10);
    assert_eq!(a.stats().current, 10);
    assert_eq!(a.stats().peak, 5000);
    assert_eq!(a.stats().reallocs, 2);
    drop(b);
    assert_eq!(a.stats().current, 0);
}

#[test]
fn inner_failure_rolls_back() {
    let a = CountingAlloc::new(FailingAlloc);
    let layout = Layout::from_size_align(64, 8).expect("layout");
    // SAFETY: non-zero-size layout.
    assert!(unsafe { a.alloc(layout) }.is_null());
    // SAFETY: as above.
    assert!(unsafe { a.alloc_zeroed(layout) }.is_null());
    let s = a.stats();
    assert_eq!((s.current, s.allocs), (0, 0));
}

#[test]
fn baseline_and_real_usage() {
    let a = CountingAlloc::new(System);
    let _vm = alloc(&a, 5 * M); // "VM structures" before the request
    assert_eq!(a.usage(), PHP_BASELINE + 5 * M);
    a.set_baseline();
    assert_eq!(a.usage(), PHP_BASELINE);
    assert_eq!(a.peak(), PHP_BASELINE);
    assert_eq!(a.real_usage(), CHUNK_SIZE);
    let b = alloc(&a, 2 * M);
    assert_eq!(a.usage(), PHP_BASELINE + 2 * M);
    assert_eq!(a.real_usage(), 2 * CHUNK_SIZE);
    assert_eq!(a.memory_usage(true), 2 * CHUNK_SIZE);
    drop(b);
    assert_eq!(a.real_usage(), CHUNK_SIZE);
    assert_eq!(a.peak_usage(true), 2 * CHUNK_SIZE);
    assert_eq!(a.peak_usage(false), PHP_BASELINE + 2 * M);
}

#[test]
fn tracked_mode_reports_exact_real_usage() {
    // token_get_all_TOKEN_PARSE_oom_feedback.phpt: under USE_TRACKED_ALLOC
    // `ini_set('memory_limit', memory_get_usage() + 1024)` succeeds and the
    // next allocations exhaust it.
    let a = CountingAlloc::new(System);
    a.set_baseline();
    assert!(!a.is_tracked());
    a.set_tracked(true);
    assert_eq!(a.real_usage(), PHP_BASELINE);
    assert_eq!(a.memory_usage(true), a.usage());
    assert_eq!(a.set_limit(Some(PHP_BASELINE + 1024)), Ok(()));
    let over = alloc(&a, 4096);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: PHP_BASELINE + 1024,
            tried: 4096
        })
    );
    assert_eq!(a.real_peak(), PHP_BASELINE + 4096);
    drop(over);
    a.clear();
    a.set_tracked(false);
    assert_eq!(a.real_usage(), CHUNK_SIZE);
}

#[test]
fn tracked_mode_env_selection() {
    let on = |zend: Option<&str>, tracked: Option<&str>| {
        tracked_from_env(zend.map(str::as_bytes), tracked.map(str::as_bytes))
    };
    assert!(on(Some("0"), Some("1")));
    assert!(on(Some(""), Some(" 2x")));
    assert!(on(Some("off"), Some("-1")));
    assert!(!on(Some("1"), Some("1")));
    assert!(!on(Some("0"), Some("0")));
    assert!(!on(Some("0"), None));
    assert!(!on(None, Some("1")));
    assert_eq!(atol(b"  +42abc"), 42);
    assert_eq!(atol(b"-7"), -7);
    assert_eq!(atol(b"x1"), 0);
}

#[test]
fn usage_saturates_below_baseline() {
    let a = CountingAlloc::new(System);
    let b = alloc(&a, M);
    a.set_baseline();
    drop(b);
    assert_eq!(a.usage(), PHP_BASELINE - M.min(PHP_BASELINE));
    assert_eq!(a.real_usage(), CHUNK_SIZE);
}

#[test]
fn peak_reset_semantics() {
    let a = CountingAlloc::new(System);
    a.set_baseline();
    let big = alloc(&a, 3 * M);
    let p1 = a.peak();
    let small = alloc(&a, 1000);
    assert_eq!(a.peak(), p1 + 1000);
    drop(big);
    assert_eq!(a.peak(), p1 + 1000, "peak survives frees");
    a.reset_peak();
    assert_eq!(a.peak(), a.usage());
    assert!(a.peak() < p1);
    drop(small);
    assert!(a.peak() > a.usage());
}

#[test]
fn set_limit_rules() {
    let a = CountingAlloc::new(System);
    a.set_baseline();
    assert_eq!(a.limit(), None);
    assert_eq!(a.hard_limit(), None);
    // Below real usage (one chunk) fails, like `ini_set('memory_limit','1M')`.
    assert_eq!(
        a.set_limit(Some(M)),
        Err(SetLimitError {
            requested: M,
            current: CHUNK_SIZE
        })
    );
    assert_eq!(a.limit(), None);
    assert_eq!(a.set_limit(Some(3 * M)), Ok(()));
    assert_eq!(a.limit(), Some(3 * M));
    assert_eq!(a.hard_limit(), Some(6 * M));
    assert_eq!(a.set_limit(None), Ok(()));
    assert_eq!(a.limit(), None);
    assert_eq!(a.set_limit(Some(LIMIT_MAX)), Ok(()));
    assert_eq!(a.limit(), None, "LIMIT_MAX means unlimited");
    assert_eq!(a.set_limit(Some(0)).map_err(|e| e.requested), Err(0));
    a.force_limit(Some(0));
    assert_eq!(a.limit(), Some(0));
}

#[test]
fn soft_limit_is_flagged_not_failed() {
    let a = CountingAlloc::new(System);
    let _vm = alloc(&a, 3 * M);
    a.request_start(Some(4 * M)).expect("limit");
    // A large block from the request's chunks fits.
    let ok = alloc(&a, M);
    assert!(!ok.p.is_null());
    assert_eq!(a.check(), None);
    // A huge block needs its own pages: 2M of chunks + 3M > 4M.
    let over = alloc(&a, 3 * M);
    assert!(!over.p.is_null(), "soft-limit allocations succeed");
    assert!(a.is_pending());
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 3 * M
        })
    );
    // First violation wins until cleared.
    let _more = alloc(&a, 77);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 3 * M
        })
    );
    a.clear();
    assert_eq!(a.check(), None);
    // Zend never made the failing allocation: small ones after the fatal
    // (shutdown functions) are served from the existing chunks.
    let _again = alloc(&a, 99);
    assert_eq!(a.check(), None, "the violating block is forgiven");
    // A genuinely new huge block over the limit is flagged again.
    let _big = alloc(&a, 3 * M);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 3 * M
        })
    );
    a.clear();
    drop(over);
    let _fine = alloc(&a, 10);
    assert_eq!(a.check(), None, "back under the limit");
}

/// `zend_mm_check_limit` only runs when Zend needs memory from the OS: a
/// small block that fits in an already counted chunk never fails, even
/// close to the limit; one that needs a new chunk past the limit does.
#[test]
fn small_blocks_fail_only_when_a_new_chunk_passes_the_limit() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    // Fill up to 64 bytes short of 4M with blocks below the huge size.
    let mut fill = Vec::new();
    while 4 * M - a.usage() > M + 64 {
        fill.push(alloc(&a, M));
    }
    fill.push(alloc(&a, 4 * M - a.usage() - 64));
    assert_eq!(a.check(), None);
    let _tiny = alloc(&a, 64);
    assert_eq!(a.check(), None, "still inside the second chunk");
    let _next = alloc(&a, 1);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 1
        }),
        "a third chunk would pass 4M"
    );
    drop(fill);
}

#[test]
fn take_suspends_recording_until_clear() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    assert_eq!(a.take(), None);
    let x = alloc(&a, 5 * M);
    assert!(a.is_pending());
    let over = a.take().expect("pending");
    assert_eq!(
        over,
        OverLimit {
            limit: 4 * M,
            tried: 5 * M
        }
    );
    assert!(!a.is_pending());
    assert!(a.is_handled());
    assert_eq!(a.check(), None);
    assert_eq!(a.take(), None, "taken once");
    let _y = alloc(&a, 100);
    assert_eq!(a.try_reserve(8 * M).map_err(|e| e.tried), Err(8 * M));
    assert!(!a.is_pending(), "nothing recorded while handled");
    a.clear();
    assert!(!a.is_handled());
    let _z = alloc(&a, 100);
    assert_eq!(a.take(), None, "the taken violation's block is forgiven");
    let _w = alloc(&a, 3 * M);
    assert_eq!(
        a.take(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 3 * M
        }),
        "recording is re-armed after clear"
    );
    drop(x);
}

#[test]
fn hard_ceiling_returns_null_without_handler() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    assert_eq!(a.hard_limit(), Some(8 * M));
    let before = a.stats();
    let b = alloc(&a, 9 * M);
    assert!(b.p.is_null());
    assert_eq!(a.stats().current, before.current, "rolled back");
    assert_eq!(
        a.check(),
        None,
        "hard failures do not record a soft violation"
    );
    let mut g = alloc(&a, M);
    grow(&mut g, 9 * M);
    assert_eq!(g.layout.size(), M, "growing realloc past the ceiling fails");
    assert_eq!(a.stats().current, before.current + M);
}

#[test]
fn hard_limit_percent() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(10 * M)).expect("limit");
    assert_eq!(
        a.set_hard_limit_percent(99),
        Err(HardLimitPercentError::Invalid(99))
    );
    assert_eq!(a.set_hard_limit_percent(150), Ok(()));
    assert_eq!(a.hard_limit(), Some(15 * M));
    let b = alloc(&a, 14 * M);
    assert!(!b.p.is_null());
    assert!(a.check().is_some());
    assert_eq!(
        a.set_hard_limit_percent(300),
        Err(HardLimitPercentError::OverSoftLimit(300))
    );
    drop(b);
    assert_eq!(a.set_hard_limit_percent(-1), Ok(()));
    assert_eq!(a.hard_limit(), Some(20 * M));
}

#[test]
fn try_reserve_semantics() {
    let a = CountingAlloc::new(System);
    assert_eq!(a.try_reserve(usize::MAX), Ok(()), "unlimited");
    a.request_start(Some(100 * M)).expect("limit");
    // A huge reservation fits next to the chunks already in use.
    let room = 100 * M - crate::limit::round_to_chunks(a.usage());
    assert!(a.would_fit(room));
    assert_eq!(a.try_reserve(room), Ok(()));
    assert_eq!(a.check(), None);
    let err = a.try_reserve(room + 1).expect_err("over");
    assert_eq!(
        err,
        OverLimit {
            limit: 100 * M,
            tried: room + 1
        }
    );
    assert_eq!(a.check(), Some(err), "reservation failures are recorded");
    a.clear();
    assert_eq!(
        a.try_reserve_n(usize::MAX, 2, 0),
        Err(OverLimit {
            limit: 100 * M,
            tried: usize::MAX
        })
    );
    a.clear();
    assert_eq!(a.try_reserve_n(1024, 1024, 24), Ok(()));
    assert_eq!(a.stats().current, 0, "reservations allocate nothing");
}

#[test]
fn fallible_scope_fails_soft_overruns() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    let b = fallible(|| alloc(&a, 5 * M));
    assert!(
        b.p.is_null(),
        "fallible allocations fail over the soft limit"
    );
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 5 * M
        })
    );
    a.clear();
    let ok = fallible(|| alloc(&a, M));
    assert!(!ok.p.is_null(), "allocations under the limit still succeed");
    let c = alloc(&a, 5 * M);
    assert!(
        !c.p.is_null(),
        "outside the scope the soft limit only flags"
    );
}

#[test]
fn request_lifecycle() {
    let a = CountingAlloc::new(System);
    let _vm = alloc(&a, 3 * M);
    a.request_start(Some(8 * M)).expect("limit");
    let _x = alloc(&a, 9 * M);
    assert!(a.check().is_some());
    a.request_end();
    assert_eq!(a.limit(), None);
    assert_eq!(a.check(), None);
    assert_eq!(a.usage(), PHP_BASELINE + 12 * M, "baseline cleared");
    assert_eq!(a.peak(), a.usage());
    // A failed start leaves the request unlimited.
    let err = a.request_start(Some(M)).expect_err("too small");
    assert_eq!(err.current, CHUNK_SIZE);
    assert_eq!(a.limit(), None);
    assert_eq!(a.usage(), PHP_BASELINE);
}

#[test]
fn memory_limit_ini() {
    let a = CountingAlloc::new(System);
    a.request_start(None).expect("unlimited");
    let none = u64::MAX;
    assert_eq!(
        a.apply_memory_limit_ini(128 << 20, none),
        Ok(MemoryLimitIni::Applied)
    );
    assert_eq!(a.limit(), Some(128 * M));
    assert_eq!(
        a.apply_memory_limit_ini(none, none),
        Ok(MemoryLimitIni::Applied)
    );
    assert_eq!(a.limit(), None);
    // gh17951_runtime_change_3: 1024M over max 512M warns and clamps.
    assert_eq!(
        a.apply_memory_limit_ini(1 << 30, 512 << 20),
        Ok(MemoryLimitIni::ClampedToMax {
            max: 512 << 20,
            warning: Some(clamp_to_max_warning(1 << 30, 512 << 20)),
        })
    );
    assert_eq!(a.limit(), Some(512 * M));
    // gh17951_runtime_change_4: -1 clamps silently.
    assert_eq!(
        a.apply_memory_limit_ini(none, 512 << 20),
        Ok(MemoryLimitIni::ClampedToMax {
            max: 512 << 20,
            warning: None
        })
    );
    assert_eq!(
        a.apply_memory_limit_ini(1 << 20, none),
        Err(SetLimitError {
            requested: M,
            current: CHUNK_SIZE
        })
    );
    assert_eq!(a.limit(), Some(512 * M), "failed set keeps the old limit");
}

#[test]
fn debug_and_gc() {
    let a = CountingAlloc::new(System);
    assert!(format!("{a:?}").starts_with("CountingAlloc"));
    assert_eq!(gc_mem_caches(), 0);
    assert!(!GLOBAL.is_active(), "GLOBAL is not installed in unit tests");
    assert_eq!(usage(), PHP_BASELINE);
}

#[test]
fn cross_thread_accounting_is_exact() {
    let a = CountingAlloc::new(System);
    a.bind_to_current_thread();
    let mine = alloc(&a, 1000);
    let handed_over: Vec<Block<'_>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    for i in 1..2000 {
                        drop(alloc(&a, i));
                    }
                    // Allocated here, freed by the owner thread below.
                    let b = alloc(&a, 500);
                    let b = std::mem::ManuallyDrop::new(b);
                    (b.p.expose_provenance(), b.layout)
                })
            })
            .collect();
        for i in 1..2000 {
            drop(alloc(&a, i));
        }
        workers
            .into_iter()
            .map(|w| {
                let (addr, layout) = w.join().expect("worker");
                let p = ptr::with_exposed_provenance_mut(addr);
                Block { a: &a, p, layout }
            })
            .collect()
    });
    assert_eq!(a.raw_usage(), 1000 + 4 * 500);
    drop(handed_over);
    drop(mine);
    assert_eq!(a.raw_usage(), 0);
}

#[test]
fn uncounted_scope_hides_huge_blocks() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    let base = a.usage();
    let small = uncounted(|| alloc(&a, 1000));
    assert_eq!(a.usage(), base + 1000, "small blocks stay counted");
    let mut big = uncounted(|| alloc(&a, 16 * M));
    assert!(
        !big.p.is_null(),
        "neither limit applies to uncounted blocks"
    );
    assert_eq!(a.usage(), base + 1000);
    assert_eq!(a.uncounted_bytes(), 16 * M);
    assert_eq!(a.check(), None, "no soft-limit violation is recorded");
    // Growing it outside the scope keeps it uncounted, past the hard
    // ceiling (2 x 4 MiB) too.
    grow(&mut big, 32 * M);
    assert_eq!(big.layout.size(), 32 * M);
    assert_eq!(a.usage(), base + 1000);
    assert_eq!(a.uncounted_bytes(), 32 * M);
    assert_eq!(a.check(), None);
    assert!(a.peak() < base + M, "uncounted blocks never reach the peak");
    // Shrinking below the huge threshold makes it an ordinary block.
    grow(&mut big, 1024);
    assert_eq!(big.layout.size(), 1024);
    assert_eq!(a.uncounted_bytes(), 0);
    assert_eq!(a.usage(), base + 1000 + 1024);
    drop(big);
    drop(small);
    assert_eq!(a.usage(), base);
    assert_eq!(a.stats().current, 0);
}

#[test]
fn uncounted_blocks_freed_outside_the_scope() {
    let a = CountingAlloc::new(System);
    let b = uncounted(|| alloc(&a, 8 * M));
    let layout = Layout::from_size_align(4 * M, 8).expect("layout");
    // SAFETY: non-zero size.
    let z = uncounted(|| unsafe { a.alloc_zeroed(layout) });
    assert!(!z.is_null());
    // SAFETY: `z` points to `4 * M` zeroed bytes.
    let (first, last) = unsafe { (*z, *z.add(4 * M - 1)) };
    assert_eq!((first, last), (0, 0));
    assert_eq!(a.stats().current, 0);
    assert_eq!(a.uncounted_bytes(), 12 * M);
    assert_eq!(a.stats().allocs, 2);
    drop(b);
    // SAFETY: `z` came from `a.alloc_zeroed(layout)`.
    unsafe { a.dealloc(z, layout) };
    assert_eq!(a.stats().current, 0, "frees do not underflow the counters");
    assert_eq!(a.uncounted_bytes(), 0);
    assert_eq!(a.stats().frees, 2);
}

#[test]
fn huge_blocks_outside_the_scope_stay_counted() {
    let a = CountingAlloc::new(System);
    let mut b = alloc(&a, 8 * M);
    assert_eq!(a.stats().current, 8 * M);
    grow(&mut b, 16 * M);
    assert_eq!(a.stats().current, 16 * M);
    assert_eq!(a.uncounted_bytes(), 0);
    drop(b);
    assert_eq!(a.stats().current, 0);
}

#[test]
fn counted_block_grown_huge_in_scope_leaves_the_accounting() {
    let a = CountingAlloc::new(System);
    let mut b = alloc(&a, M);
    assert_eq!(a.stats().current, M);
    uncounted(|| grow(&mut b, 8 * M));
    assert_eq!(b.layout.size(), 8 * M);
    assert_eq!(a.stats().current, 0);
    assert_eq!(a.uncounted_bytes(), 8 * M);
    drop(b);
    assert_eq!(a.stats().current, 0);
    assert_eq!(a.uncounted_bytes(), 0);
}

#[test]
fn full_uncounted_registry_falls_back_to_counting() {
    let a = CountingAlloc::new(System);
    let size = 3 * M;
    let blocks: Vec<Block<'_>> = (0..64).map(|_| uncounted(|| alloc(&a, size))).collect();
    assert!(blocks.iter().all(|b| !b.p.is_null()));
    assert_eq!(a.uncounted_bytes(), 64 * size);
    assert_eq!(a.stats().current, 0);
    let extra = uncounted(|| alloc(&a, size));
    assert_eq!(a.stats().current, size, "the 65th huge block is counted");
    drop(blocks);
    assert_eq!(a.uncounted_bytes(), 0);
    drop(extra);
    assert_eq!(a.stats().current, 0);
}
