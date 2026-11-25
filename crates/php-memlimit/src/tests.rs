// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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
        // Charged their size classes: the 1024-byte bin, the 24-byte bin.
        let _b1 = alloc(&a, 1000);
        let _b2 = alloc(&a, 24);
        assert_eq!(a.stats().current, 1048);
        assert_eq!(a.usage(), PHP_BASELINE + 1048);
        assert!(a.is_active());
    }
    let s = a.stats();
    assert_eq!((s.current, s.peak, s.allocs, s.frees), (0, 1048, 2, 2));
    assert_eq!(a.peak(), PHP_BASELINE + 1048);
}

#[test]
fn realloc_accounting() {
    let a = CountingAlloc::new(System);
    let mut b = alloc(&a, 100);
    assert_eq!(a.stats().current, 112, "the 112-byte bin");
    grow(&mut b, 5000);
    assert_eq!(a.stats().current, 8192, "two pages");
    grow(&mut b, 10);
    assert_eq!(a.stats().current, 16, "the 16-byte bin");
    assert_eq!(a.stats().peak, 8192);
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
    assert_eq!(a.peak(), p1 + 1024);
    drop(big);
    assert_eq!(a.peak(), p1 + 1024, "peak survives frees");
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
    // Fill up to 4 KiB short of 4M with blocks of half a megabyte (two
    // 1M blocks never share a chunk: see `big_large_blocks_take_a_chunk_each`)
    // and whole pages, then up to 64 bytes short with small blocks charged
    // their bins (3072 + 896 + 64 = 4096 - 64).
    let mut fill = Vec::new();
    while 4 * M - a.usage() > M / 2 + PAGE_SIZE {
        fill.push(alloc(&a, M / 2));
    }
    fill.push(alloc(&a, 4 * M - a.usage() - PAGE_SIZE));
    for size in [3072, 896, 64] {
        fill.push(alloc(&a, size));
    }
    assert_eq!(a.usage(), 4 * M - 64);
    assert_eq!(a.check(), None);
    let _tiny = alloc(&a, 64);
    assert_eq!(a.check(), None, "still inside the second chunk");
    let _next = alloc(&a, 1);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: CHUNK_SIZE
        }),
        "a third chunk would pass 4M: php-src reports the chunk"
    );
    drop(fill);
}

/// `zend_mm_alloc_pages`: a large block of more than half the pages a
/// chunk hands out shares its chunk with no other such block, so under 8M
/// (four chunks) a fifth one fails although the bytes would fit in three.
#[test]
fn big_large_blocks_take_a_chunk_each() {
    let a = CountingAlloc::new(System);
    a.request_start(Some(8 * M)).expect("limit");
    let size = 1100 * 1024;
    let held: Vec<_> = (0..4).map(|_| alloc(&a, size)).collect();
    assert_eq!(a.check(), None, "four chunks");
    let fifth = alloc(&a, size);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 8 * M,
            tried: CHUNK_SIZE
        }),
        "a fifth chunk would pass 8M"
    );
    a.clear();
    drop(held);
    drop(fifth);
    let _again = alloc(&a, size);
    assert_eq!(a.check(), None, "the freed blocks give their chunks back");
    // Blocks of half a chunk or less are counted by their bytes.
    let halves: Vec<_> = (0..6).map(|_| alloc(&a, 1000 * 1024)).collect();
    assert_eq!(a.check(), None);
    drop(halves);
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
            tried: room + PAGE_SIZE
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
    // fread($fp, 100 << 20) under memory_limit=8M: php8.5 reports
    // "(tried to allocate 104861696 bytes)", the zend_string of
    // 24 + 100 MiB + 1 bytes rounded to pages; a small reservation
    // reports the chunk it would need.
    let b = CountingAlloc::new(System);
    b.request_start(Some(8 * M)).expect("limit");
    assert_eq!(
        b.try_reserve(24 + (100 << 20) + 1).map_err(|e| e.tried),
        Err(104_861_696)
    );
    b.clear();
    b.request_end();
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
    assert_eq!(a.raw_usage(), 1024 + 4 * 512, "bins of 1024 and 512 bytes");
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
    assert_eq!(a.usage(), base + 1024, "small blocks stay counted");
    let mut big = uncounted(|| alloc(&a, 16 * M));
    assert!(
        !big.p.is_null(),
        "neither limit applies to uncounted blocks"
    );
    assert_eq!(a.usage(), base + 1024);
    assert_eq!(a.uncounted_bytes(), 16 * M);
    assert_eq!(a.check(), None, "no soft-limit violation is recorded");
    // Growing it outside the scope keeps it uncounted, past the hard
    // ceiling (2 x 4 MiB) too.
    grow(&mut big, 32 * M);
    assert_eq!(big.layout.size(), 32 * M);
    assert_eq!(a.usage(), base + 1024);
    assert_eq!(a.uncounted_bytes(), 32 * M);
    assert_eq!(a.check(), None);
    assert!(a.peak() < base + M, "uncounted blocks never reach the peak");
    // Shrinking below the huge threshold makes it an ordinary block.
    grow(&mut big, 1024);
    assert_eq!(big.layout.size(), 1024);
    assert_eq!(a.uncounted_bytes(), 0);
    assert_eq!(a.usage(), base + 1024 + 1024);
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

thread_local! {
    /// Events heard by [`record_event`] on this thread.
    static EVENTS: std::cell::RefCell<Vec<HeapEvent>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A heap observer that records what it hears (in the test binary's own
/// allocator, not the instance under test).
fn record_event(e: HeapEvent) {
    EVENTS.with(|v| v.borrow_mut().push(e));
}

fn take_events() -> Vec<HeapEvent> {
    EVENTS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

#[test]
fn heap_observer_hears_the_observed_thread_only() {
    let a = CountingAlloc::new(System);
    a.bind_to_current_thread();
    a.observe(true);
    assert!(
        !a.is_observing(),
        "nothing to report to without an observer"
    );
    assert!(a.set_observer(record_event));
    assert!(!a.set_observer(record_event), "installed once");
    let quiet = alloc(&a, 64);
    a.observe(true);
    assert!(a.is_observing());
    let mut b = alloc(&a, 100);
    let first = b.p.addr();
    grow(&mut b, 200);
    let grown = b.p.addr();
    assert_eq!(a.gc_mem_caches(), 0);
    drop(b);
    std::thread::scope(|s| {
        s.spawn(|| drop(alloc(&a, 10)));
    });
    a.observe(false);
    assert!(!a.is_observing());
    drop(quiet);
    drop(alloc(&a, 32));
    assert_eq!(
        take_events(),
        vec![
            HeapEvent::Alloc {
                size: 100,
                addr: first
            },
            HeapEvent::Realloc {
                size: 200,
                old: first,
                new: grown
            },
            HeapEvent::Gc { freed: 0 },
            HeapEvent::Free { addr: grown },
        ]
    );
    let s = a.stats();
    assert_eq!((s.current, s.allocs, s.frees, s.reallocs), (0, 4, 4, 1));
}

#[test]
fn observing_keeps_the_limit_semantics() {
    let a = CountingAlloc::new(System);
    a.bind_to_current_thread();
    assert!(a.set_observer(record_event));
    a.request_start(Some(128 * M)).expect("limit");
    a.observe(true);
    let small = alloc(&a, 1000);
    assert!(!small.p.is_null());
    assert!(a.check().is_none(), "an allocation under the limit is fine");
    let over = alloc(&a, 200 * M);
    assert!(!over.p.is_null(), "a soft overrun is still granted");
    assert!(a.check().is_some(), "and recorded");
    a.observe(false);
    drop(over);
    drop(small);
    let heard = take_events();
    assert_eq!(heard.len(), 2, "{heard:?}");
    assert!(matches!(heard[0], HeapEvent::Alloc { size: 1000, .. }));
    assert!(matches!(heard[1], HeapEvent::Alloc { size, .. } if size == 200 * M));
    a.request_end();
    assert_eq!(a.stats().current, 0);
}

// ---- the request thread's heap ------------------------------------------

use crate::heap::{is_heap_block, small_size_to_bin, BIN_ELEMENTS, BIN_SIZE};

#[test]
fn owner_small_blocks_come_from_the_heap() {
    let a = CountingAlloc::new(System);
    let before = alloc(&a, 24);
    assert!(!is_heap_block(before.p), "unbound: the inner allocator");
    assert!(a.bind_to_current_thread());
    assert!(a.is_bound_to_current_thread());
    let b = alloc(&a, 24);
    assert!(is_heap_block(b.p));
    assert_eq!(a.heap_chunks(), 1);
    let addr = b.p.addr();
    drop(b);
    let c = alloc(&a, 20);
    assert_eq!(c.p.addr(), addr, "same bin, reused LIFO like zend_mm");
    let large = alloc(&a, MAX_SMALL_SIZE + 1);
    assert!(!is_heap_block(large.p), "large blocks: the inner allocator");
    // SAFETY: non-zero size.
    let wide = unsafe { a.alloc(Layout::from_size_align(64, 64).expect("layout")) };
    assert!(!is_heap_block(wide), "over-aligned: the inner allocator");
    // SAFETY: allocated just above with this layout.
    unsafe { a.dealloc(wide, Layout::from_size_align(64, 64).expect("layout")) };
    // Charged by size class: 20 bytes take the 24-byte bin, 3073 a page.
    assert_eq!(a.stats().current, 24 + 24 + PAGE_SIZE);
    // A block allocated before binding goes back to the inner allocator.
    drop(before);
    assert_eq!(a.stats().current, 24 + PAGE_SIZE);
}

#[test]
fn every_bin_hands_out_disjoint_aligned_blocks() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    for (bin, &size) in BIN_SIZE.iter().enumerate() {
        let size = usize::from(size);
        let align = if size % 16 == 0 { 16 } else { 8 };
        let layout = Layout::from_size_align(size, align).expect("layout");
        // Three runs' worth.
        let n = 2 * usize::from(BIN_ELEMENTS[bin]) + 1;
        let blocks: Vec<*mut u8> = (0..n)
            .map(|i| {
                // SAFETY: non-zero size.
                let p = unsafe { a.alloc(layout) };
                assert!(!p.is_null() && is_heap_block(p), "bin {bin}");
                assert_eq!(p.addr() % align, 0, "bin {bin}");
                let tag = u8::try_from(i % 256).expect("tag");
                // SAFETY: `p` holds `size` bytes.
                unsafe { ptr::write_bytes(p, tag, size) };
                p
            })
            .collect();
        for (i, &p) in blocks.iter().enumerate() {
            let tag = u8::try_from(i % 256).expect("tag");
            // SAFETY: `p` is live and was filled above.
            let bytes = unsafe { std::slice::from_raw_parts(p, size) };
            assert!(bytes.iter().all(|&b| b == tag), "bin {bin}: overlap");
        }
        for p in blocks {
            // SAFETY: allocated above with `layout`, freed once.
            unsafe { a.dealloc(p, layout) };
        }
    }
    assert_eq!(a.stats().current, 0);
    assert_eq!(a.heap_chunks(), 1, "195 pages fit one chunk");
}

#[test]
fn realloc_keeps_contents_across_bins_and_allocators() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let mut b = alloc(&a, 10);
    // SAFETY: `b.p` holds 10 bytes.
    unsafe {
        for i in 0..10u8 {
            b.p.add(usize::from(i)).write(i + 1);
        }
    }
    let check = |b: &Block<'_>| {
        // SAFETY: every size below is at least 10 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(b.p, 10) };
        assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    };
    let first = b.p;
    grow(&mut b, 16);
    assert_eq!(b.p, first, "same bin: in place");
    grow(&mut b, 100);
    assert_ne!(b.p, first, "moved to the 112-byte bin");
    assert!(is_heap_block(b.p));
    check(&b);
    grow(&mut b, 5000);
    assert!(!is_heap_block(b.p), "large: moved to the inner allocator");
    check(&b);
    grow(&mut b, 30);
    assert!(!is_heap_block(b.p), "an inner block stays there");
    check(&b);
    assert_eq!(a.stats().current, 32, "the 32-byte bin");
    assert_eq!(a.stats().reallocs, 4);
    // The block left in the 16-byte bin is handed out again.
    let again = alloc(&a, 12);
    assert_eq!(again.p, first);
}

#[test]
fn zeroed_heap_blocks_are_zero_after_reuse() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let layout = Layout::from_size_align(64, 8).expect("layout");
    // SAFETY: non-zero size; the block is filled, then freed once.
    let p = unsafe {
        let p = a.alloc(layout);
        ptr::write_bytes(p, 0xab, 64);
        a.dealloc(p, layout);
        p
    };
    // SAFETY: non-zero size.
    let q = unsafe { a.alloc_zeroed(layout) };
    assert_eq!(q, p, "the freed block is reused");
    // SAFETY: `q` holds 64 bytes.
    assert!(unsafe { std::slice::from_raw_parts(q, 64) }
        .iter()
        .all(|&b| b == 0));
    // SAFETY: allocated above with `layout`.
    unsafe { a.dealloc(q, layout) };
    assert_eq!(a.stats().current, 0);
}

#[test]
fn heap_ownership_is_exclusive_until_released() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    assert!(a.bind_to_current_thread(), "rebinding the owner is a no-op");
    std::thread::scope(|s| {
        s.spawn(|| {
            assert!(!a.bind_to_current_thread(), "owned by another thread");
            assert!(!a.is_bound_to_current_thread());
            assert!(!is_heap_block(alloc(&a, 24).p));
        });
    });
    let mine = alloc(&a, 24);
    assert!(is_heap_block(mine.p));
    a.unbind_current_thread();
    assert!(!a.is_bound_to_current_thread());
    // Freed after unbinding: onto the remote list.
    drop(mine);
    std::thread::scope(|s| {
        s.spawn(|| {
            assert!(a.bind_to_current_thread(), "free again");
            let b = alloc(&a, 24);
            assert!(is_heap_block(b.p));
            drop(b);
            a.unbind_current_thread();
        });
    });
    // `request_start` binds and `request_end` unbinds.
    a.request_start(None).expect("unlimited");
    assert!(a.is_bound_to_current_thread());
    a.request_end();
    assert!(!a.is_bound_to_current_thread());
    assert_eq!(a.stats().current, 0);
}

#[test]
fn explicit_binding_survives_request_end() {
    // php-cli binds before the request: the teardown that follows
    // `request_end` (TLS destructors, the Vm's drop) stays on the owner
    // path instead of pushing every block onto the remote lists.
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    a.request_start(Some(8 * M)).expect("limit");
    assert!(a.is_bound_to_current_thread());
    let b = alloc(&a, 24);
    assert!(is_heap_block(b.p));
    a.request_end();
    assert!(a.is_bound_to_current_thread(), "bound explicitly: kept");
    let c = alloc(&a, 24);
    assert!(
        is_heap_block(c.p),
        "teardown allocations still use the heap"
    );
    drop(b);
    drop(c);
    a.unbind_current_thread();
    assert!(!a.is_bound_to_current_thread());
    // A thread bound by `request_start` itself (a php -S request thread) is
    // unbound by `request_end`, also after a second `request_start` on it.
    a.request_start(None).expect("unlimited");
    assert!(a.is_bound_to_current_thread());
    a.request_start(None).expect("unlimited");
    a.request_end();
    assert!(!a.is_bound_to_current_thread());
    // Binding explicitly during a request pins the heap too.
    a.request_start(None).expect("unlimited");
    assert!(a.bind_to_current_thread());
    a.request_end();
    assert!(a.is_bound_to_current_thread());
    a.unbind_current_thread();
    assert!(!a.is_bound_to_current_thread());
    // Another thread's `request_end` never touches the owner's binding.
    assert!(a.bind_to_current_thread());
    std::thread::scope(|s| {
        s.spawn(|| {
            assert!(a.request_start(None).is_ok(), "runs on the inner allocator");
            assert!(!a.is_bound_to_current_thread());
            a.request_end();
        });
    });
    assert!(a.is_bound_to_current_thread());
    a.unbind_current_thread();
    assert_eq!(a.stats().current, 0);
}

#[test]
fn unbinding_while_observed_frees_the_heap() {
    // Observing parks the owner's ownership (`observe(true)` takes it, so
    // the observed thread's frees are reported on the non-owner path) and
    // gives it back when observing stops. A request-bound thread whose
    // request ends in between must not get the heap back.
    let a = CountingAlloc::new(System);
    assert!(a.set_observer(record_event));
    a.request_start(None).expect("unlimited");
    assert!(a.is_bound_to_current_thread());
    a.observe(true);
    assert!(a.is_observing());
    assert!(!a.is_bound_to_current_thread());
    a.request_end();
    a.observe(false);
    assert!(
        !a.is_bound_to_current_thread(),
        "the give-back was cancelled: the heap is free for the next request"
    );
    std::thread::scope(|s| {
        s.spawn(|| {
            assert!(a.bind_to_current_thread(), "free");
            a.unbind_current_thread();
        });
    });
    // An explicitly bound thread keeps the heap across an observed request.
    assert!(a.bind_to_current_thread());
    a.request_start(None).expect("unlimited");
    a.observe(true);
    a.request_end();
    a.observe(false);
    assert!(
        a.is_bound_to_current_thread(),
        "bound explicitly: given back"
    );
    a.unbind_current_thread();
    assert!(!a.is_bound_to_current_thread());
    let _ = take_events();
}

#[test]
fn remote_frees_come_back_to_the_owner() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let layout = Layout::from_size_align(48, 8).expect("layout");
    // SAFETY: non-zero size.
    let handed: Vec<usize> = (0..100)
        .map(|_| unsafe { a.alloc(layout) }.expose_provenance())
        .collect();
    std::thread::scope(|s| {
        s.spawn(|| {
            for &addr in &handed {
                // SAFETY: allocated above with `layout`, freed once, here.
                unsafe { a.dealloc(ptr::with_exposed_provenance_mut(addr), layout) };
            }
        });
    });
    assert_eq!(a.stats().current, 0);
    // Two 85-element runs were carved; 70 blocks are still on the owner's
    // list, then the 100 remote ones are taken over before any new run.
    // SAFETY: non-zero size.
    let again: Vec<*mut u8> = (0..170).map(|_| unsafe { a.alloc(layout) }).collect();
    let got: std::collections::HashSet<usize> = again.iter().map(|p| p.addr()).collect();
    assert!(handed.iter().all(|addr| got.contains(addr)));
    for p in again {
        // SAFETY: allocated above with `layout`, freed once.
        unsafe { a.dealloc(p, layout) };
    }
    assert_eq!(a.heap_chunks(), 1);
}

#[test]
fn heap_survives_cross_thread_churn() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let sizes = [8usize, 24, 56, 100, 256, 640, 3000, 5000];
    std::thread::scope(|s| {
        let mut workers = Vec::new();
        for _ in 0..3 {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<(usize, usize, u8)>>();
            workers.push(tx);
            let a = &a;
            s.spawn(move || {
                for batch in rx {
                    for (addr, size, tag) in batch {
                        let p = ptr::with_exposed_provenance_mut::<u8>(addr);
                        // SAFETY: the owner filled `size` bytes with `tag`
                        // and handed the block over.
                        let bytes = unsafe { std::slice::from_raw_parts(p, size) };
                        assert!(bytes.iter().all(|&b| b == tag), "corrupted");
                        let layout = Layout::from_size_align(size, 8).expect("layout");
                        // SAFETY: allocated by the owner with `layout`.
                        unsafe { a.dealloc(p, layout) };
                    }
                }
            });
        }
        for round in 0..500usize {
            let mut batch = Vec::new();
            for (i, &size) in sizes.iter().enumerate() {
                let tag = u8::try_from((round + i) % 256).expect("tag");
                let layout = Layout::from_size_align(size, 8).expect("layout");
                // SAFETY: non-zero size; filled below.
                let p = unsafe { a.alloc(layout) };
                assert!(!p.is_null());
                // SAFETY: `p` holds `size` bytes.
                unsafe { ptr::write_bytes(p, tag, size) };
                if (round + i) % 2 == 0 {
                    batch.push((p.expose_provenance(), size, tag));
                } else {
                    // SAFETY: `p` is live, filled above, freed once.
                    unsafe {
                        assert!(std::slice::from_raw_parts(p, size)
                            .iter()
                            .all(|&b| b == tag));
                        a.dealloc(p, layout);
                    }
                }
            }
            workers[round % 3].send(batch).expect("worker");
        }
    });
    assert_eq!(a.raw_usage(), 0);
}

#[test]
fn another_thread_reallocates_and_frees_the_owners_heap_block() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let layout = Layout::from_size_align(24, 8).expect("layout");
    // SAFETY: non-zero size; filled below.
    let p = unsafe { a.alloc(layout) };
    assert!(is_heap_block(p));
    // SAFETY: `p` holds 24 bytes.
    unsafe { ptr::write_bytes(p, 0x5a, 24) };
    let addr = p.expose_provenance();
    std::thread::scope(|s| {
        s.spawn(|| {
            let p = ptr::with_exposed_provenance_mut::<u8>(addr);
            // Same bin: resized in place, still the heap's block.
            // SAFETY: allocated by the owner with `layout`; 20 > 0.
            let q = unsafe { a.realloc(p, layout, 20) };
            assert_eq!(q, p);
            let shrunk = Layout::from_size_align(20, 8).expect("layout");
            // Another bin: this thread does not own the heap, so the block
            // moves to the inner allocator and the old one is freed
            // remotely.
            // SAFETY: `q` is live with `shrunk`; 100 > 0.
            let r = unsafe { a.realloc(q, shrunk, 100) };
            assert!(!r.is_null() && !is_heap_block(r));
            // SAFETY: `r` holds 100 bytes, the first 20 copied from `q`.
            let kept = unsafe { std::slice::from_raw_parts(r, 20) };
            assert!(kept.iter().all(|&b| b == 0x5a), "contents moved");
            // SAFETY: allocated just above with this layout.
            unsafe { a.dealloc(r, Layout::from_size_align(100, 8).expect("layout")) };
        });
    });
    assert_eq!(a.stats().current, 0);
    // The owner's list still holds the rest of the run (169 blocks); the
    // 170th request finds the list dry and takes the remote free over.
    let per_run = usize::from(BIN_ELEMENTS[small_size_to_bin(24)]);
    // SAFETY: non-zero size.
    let got: Vec<*mut u8> = (0..per_run).map(|_| unsafe { a.alloc(layout) }).collect();
    assert_eq!(
        got[per_run - 1].addr(),
        addr,
        "remote free reused before a new run"
    );
    for p in got {
        // SAFETY: allocated above with `layout`, freed once.
        unsafe { a.dealloc(p, layout) };
    }
    assert_eq!(a.stats().current, 0);
}

#[test]
fn shrinking_realloc_moves_to_the_smaller_bin() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let mut b = alloc(&a, 100);
    let big = b.p;
    // SAFETY: `b.p` holds 100 bytes.
    unsafe {
        for i in 0..100u8 {
            b.p.add(usize::from(i)).write(i);
        }
    }
    grow(&mut b, 10);
    assert_ne!(b.p, big, "a 112-byte block is not kept for 10 bytes");
    assert!(is_heap_block(b.p));
    // SAFETY: `b.p` holds 10 bytes, copied from the old block.
    let bytes = unsafe { std::slice::from_raw_parts(b.p, 10) };
    assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(a.stats().current, 16);
    // The 112-byte block is free again: its bin hands it out next.
    let again = alloc(&a, 112);
    assert_eq!(again.p, big);
    assert_eq!(a.stats().current, 128);
}

#[test]
fn owner_fast_path_accounts_like_the_general_one() {
    // Small heap blocks of the owner take the call-free fast path; its
    // figures must be the ones the general path produces.
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let x = alloc(&a, 24);
    let y = alloc(&a, 1000);
    assert!(is_heap_block(x.p) && is_heap_block(y.p));
    let s = a.stats();
    // 24 bytes and the 1024-byte bin.
    assert_eq!((s.current, s.peak, s.allocs, s.frees), (1048, 1048, 2, 0));
    let y_addr = y.p.addr();
    drop(y);
    let s = a.stats();
    assert_eq!((s.current, s.peak, s.frees), (24, 1048, 1));
    let z = alloc(&a, 500);
    assert_eq!(a.stats().peak, 1048, "below the peak: unchanged");
    let w = alloc(&a, 1000);
    assert_eq!(w.p.addr(), y_addr, "the freed block is reused");
    assert_eq!(a.stats().current, 24 + 512 + 1024);
    assert_eq!(a.stats().peak, 1560, "a new high-water mark");
    // A large block (the inner allocator, through the general path) joins
    // the same counters.
    let big = alloc(&a, MAX_SMALL_SIZE + 1);
    assert!(!is_heap_block(big.p));
    assert_eq!(a.stats().current, 1560 + PAGE_SIZE);
    assert_eq!(a.stats().peak, 1560 + PAGE_SIZE);
    drop(big);
    drop(w);
    drop(z);
    drop(x);
    let s = a.stats();
    assert_eq!((s.current, s.allocs, s.frees), (0, 5, 5));
    a.unbind_current_thread();
}

#[test]
fn fast_path_defers_to_the_limit_check() {
    // A small heap block that would cross the soft threshold leaves the
    // fast path for the general one, which records the violation and
    // grants the block all the same.
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    assert!(a.is_bound_to_current_thread());
    // Tracked mode checks every request, so the threshold is the limit
    // itself. Blocks of 3072 bytes leave 2048 bytes (a bin size) of room:
    // 4M - PHP_BASELINE = 1234 * 3072 + 2048.
    a.set_tracked(true);
    let mut fill = Vec::new();
    while a.usage() + 3072 <= 4 * M {
        fill.push(alloc(&a, 3072));
    }
    assert!(fill.iter().all(|b| is_heap_block(b.p)));
    assert_eq!(a.check(), None);
    let room = 4 * M - a.usage();
    assert_eq!(room, 2048);
    let exact = Some(alloc(&a, room));
    assert_eq!(a.usage(), 4 * M);
    assert_eq!(a.check(), None, "exactly at the limit is fine");
    let over = alloc(&a, 8);
    assert!(!over.p.is_null() && is_heap_block(over.p), "granted");
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 8
        })
    );
    assert_eq!(a.peak(), 4 * M + 8);
    a.clear();
    drop(over);
    drop(exact);
    drop(fill);
    assert_eq!(a.stats().current, 0);
    a.request_end();
    assert!(!a.is_bound_to_current_thread());
}

#[test]
fn observing_takes_the_owner_off_the_heap_and_gives_it_back() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    assert!(a.set_observer(record_event));
    let before = alloc(&a, 24);
    assert!(is_heap_block(before.p));
    a.observe(true);
    assert!(
        !a.is_bound_to_current_thread(),
        "the observed thread gives the heap up"
    );
    let during = alloc(&a, 24);
    assert!(
        !is_heap_block(during.p),
        "observed allocations: the inner allocator"
    );
    let during_addr = during.p.addr();
    drop(during);
    let before_addr = before.p.addr();
    // A heap block freed while observed: remote list, and reported.
    drop(before);
    a.observe(false);
    assert!(a.is_bound_to_current_thread(), "ownership comes back");
    let after = alloc(&a, 24);
    assert!(is_heap_block(after.p));
    assert_eq!(
        take_events(),
        vec![
            HeapEvent::Alloc {
                size: 24,
                addr: during_addr
            },
            HeapEvent::Free { addr: during_addr },
            HeapEvent::Free { addr: before_addr },
        ]
    );
    drop(after);
    assert_eq!(a.stats().current, 0);
}

#[test]
fn sixteen_aligned_requests_of_odd_sizes_are_served_aligned() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let mut live = Vec::new();
    for size in [1usize, 17, 33, 100, 1000, 3057, 3072] {
        let layout = Layout::from_size_align(size, 16).expect("layout");
        // SAFETY: non-zero size.
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null() && is_heap_block(p), "{size}");
        assert_eq!(p.addr() % 16, 0, "{size}");
        // SAFETY: `p` holds `size` bytes.
        unsafe { ptr::write_bytes(p, 0xee, size) };
        live.push((p, layout));
    }
    // 3073 bytes at 16 is past the last bin; 8 bytes at 32 is over-aligned.
    for layout in [
        Layout::from_size_align(3073, 16).expect("layout"),
        Layout::from_size_align(8, 32).expect("layout"),
    ] {
        // SAFETY: non-zero size.
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null() && !is_heap_block(p), "{layout:?}");
        assert_eq!(p.addr() % layout.align(), 0);
        // SAFETY: allocated just above with `layout`.
        unsafe { a.dealloc(p, layout) };
    }
    for (p, layout) in live {
        // SAFETY: `p` holds `layout.size()` bytes filled above.
        let bytes = unsafe { std::slice::from_raw_parts(p, layout.size()) };
        assert!(bytes.iter().all(|&b| b == 0xee), "{layout:?}: overlap");
        // SAFETY: allocated above with `layout`, freed once.
        unsafe { a.dealloc(p, layout) };
    }
    assert_eq!(a.stats().current, 0);
}

// ---- pending hook ---------------------------------------------------------

/// Calls of [`count_pending`] (only `pending_hook_runs_once_per_violation`
/// installs it).
static PENDING_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn count_pending() {
    PENDING_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn pending_hook_runs_once_per_violation() {
    let calls = || PENDING_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    let a = CountingAlloc::new(System);
    assert!(a.set_pending_hook(count_pending));
    assert!(!a.set_pending_hook(count_pending), "installed once");
    a.request_start(Some(4 * M)).expect("limit");
    let _small = alloc(&a, 100);
    assert_eq!(calls(), 0, "no violation, no call");
    let over = alloc(&a, 5 * M);
    assert!(a.is_pending());
    assert_eq!(calls(), 1, "an allocation over the soft limit");
    let _more = alloc(&a, 3 * M);
    assert_eq!(calls(), 1, "the first violation wins: nothing new recorded");
    assert!(a.take().is_some());
    assert!(a.try_reserve(16 * M).is_err());
    assert_eq!(calls(), 1, "nothing is recorded while handled");
    a.clear();
    assert!(a.try_reserve(16 * M).is_err());
    assert_eq!(calls(), 2, "try_reserve records, and calls, too");
    a.clear();
    drop(over);
    a.request_end();
}

// ---- heap collection (`zend_mm_gc`) ----------------------------------------

use crate::heap::{BIN_PAGES, USABLE_PAGES};
use crate::limit::PAGE_SIZE;

/// The 3 KiB bin's geometry: `(blocks per run, pages per run, runs that
/// fill the first chunk and start a second one)`.
fn three_k_runs() -> (usize, usize, usize) {
    let bin = small_size_to_bin(3072);
    let pages = usize::from(BIN_PAGES[bin]);
    (
        usize::from(BIN_ELEMENTS[bin]),
        pages,
        USABLE_PAGES / pages + 1,
    )
}

#[test]
fn gc_mem_caches_gives_empty_runs_and_chunks_back() {
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let (per_run, pages, runs) = three_k_runs();
    let blocks: Vec<Block<'_>> = (0..runs * per_run).map(|_| alloc(&a, 3072)).collect();
    assert!(blocks.iter().all(|b| is_heap_block(b.p)));
    assert_eq!(a.heap_chunks(), 2);
    let live = a.stats();
    assert_eq!(a.gc_mem_caches(), 0, "every block is live");
    assert_eq!(a.stats(), live, "collecting changes no figure");
    drop(blocks);
    let (usage, peak) = (a.usage(), a.peak());
    assert_eq!(a.gc_mem_caches(), runs * pages * PAGE_SIZE);
    assert_eq!(a.heap_chunks(), 1, "the second chunk went back");
    assert_eq!((a.usage(), a.peak()), (usage, peak));
    assert_eq!(a.gc_mem_caches(), 0, "nothing left to give back");
    // The heap serves blocks again, from the first chunk's pages.
    let again = alloc(&a, 3072);
    assert!(is_heap_block(again.p));
    drop(again);
    assert_eq!(a.stats().current, 0);
    a.unbind_current_thread();
}

#[test]
fn request_end_collects_the_heap_of_a_request_bound_thread() {
    // A php -S / mod_php request thread, bound by `request_start`, gives
    // back the runs and chunks its request freed before it gives the heap
    // up (php-src resets its whole heap at every request end).
    let (per_run, _, runs) = three_k_runs();
    let a = CountingAlloc::new(System);
    a.request_start(None).expect("unlimited");
    assert!(a.is_bound_to_current_thread());
    let blocks: Vec<Block<'_>> = (0..runs * per_run).map(|_| alloc(&a, 3072)).collect();
    assert_eq!(a.heap_chunks(), 2);
    drop(blocks);
    a.request_end();
    assert!(!a.is_bound_to_current_thread());
    assert_eq!(a.heap_chunks(), 1, "collected before the heap was given up");
    // php-cli, bound explicitly, leaves its heap as it is: the process ends
    // with its request.
    let b = CountingAlloc::new(System);
    assert!(b.bind_to_current_thread());
    b.request_start(None).expect("unlimited");
    let blocks: Vec<Block<'_>> = (0..runs * per_run).map(|_| alloc(&b, 3072)).collect();
    drop(blocks);
    b.request_end();
    assert!(b.is_bound_to_current_thread());
    assert_eq!(b.heap_chunks(), 2, "not collected");
    b.unbind_current_thread();
    assert_eq!((a.stats().current, b.stats().current), (0, 0));
}

#[test]
fn collect_heap_takes_a_free_heap_for_the_collection() {
    let (per_run, pages, runs) = three_k_runs();
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    let blocks: Vec<Block<'_>> = (0..runs * per_run).map(|_| alloc(&a, 3072)).collect();
    a.unbind_current_thread();
    // Freed after the heap was given up (a request's teardown after
    // `request_end`): onto the remote lists.
    drop(blocks);
    assert_eq!(a.heap_chunks(), 2);
    // While another thread owns the heap, collecting does nothing here.
    // (Results are checked after the barriers, so that a failure cannot
    // leave the other thread waiting.)
    let owned = std::sync::Barrier::new(2);
    let checked = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            let bound = a.bind_to_current_thread();
            owned.wait();
            checked.wait();
            a.unbind_current_thread();
            assert!(bound, "the heap was free");
        });
        owned.wait();
        let collected = a.collect_heap();
        checked.wait();
        assert_eq!(collected, 0, "owned by another thread");
    });
    assert_eq!(a.heap_chunks(), 2);
    // Nobody owns it now: it is taken for the collection, which takes the
    // remote frees over too, and given up again.
    assert_eq!(a.collect_heap(), runs * pages * PAGE_SIZE);
    assert_eq!(a.heap_chunks(), 1);
    assert!(!a.is_bound_to_current_thread());
    assert!(a.bind_to_current_thread(), "free again");
    a.unbind_current_thread();
    assert_eq!(a.stats().current, 0);
}

#[test]
fn gc_mem_caches_while_observed_collects_and_reports() {
    // `zend_mm_custom_handlers_gc_01`: the observed heap's gc handler runs
    // `zend_mm_gc` on the original heap and reports what it freed. The
    // observed thread's ownership is parked, so the heap is taken for the
    // collection and parked again.
    let (per_run, pages, _) = three_k_runs();
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    assert!(a.set_observer(record_event));
    let _ = take_events();
    let run: Vec<Block<'_>> = (0..per_run).map(|_| alloc(&a, 3072)).collect();
    a.observe(true);
    // Observed frees: the remote lists, and reported.
    drop(run);
    assert_eq!(a.gc_mem_caches(), pages * PAGE_SIZE);
    assert!(!a.is_bound_to_current_thread(), "still parked");
    a.observe(false);
    assert!(a.is_bound_to_current_thread(), "ownership comes back");
    let heard = take_events();
    assert_eq!(heard.len(), per_run + 1, "{heard:?}");
    assert!(heard[..per_run]
        .iter()
        .all(|e| matches!(e, HeapEvent::Free { .. })));
    assert_eq!(
        heard[per_run],
        HeapEvent::Gc {
            freed: pages * PAGE_SIZE
        }
    );
    a.unbind_current_thread();
    assert_eq!(a.stats().current, 0);
}

#[test]
fn persistent_growth_and_release_leave_usage_and_peak_alone() {
    let a = CountingAlloc::new(System);
    a.set_baseline();
    let _warm = alloc(&a, 64);
    let start = a.usage();
    let peak = a.peak();
    // Growth inside the scope does not show.
    let mut b = a.persistent(|| alloc(&a, 4096));
    assert_eq!(a.usage(), start);
    assert_eq!(a.peak(), peak);
    // Neither does a reallocation...
    a.persistent(|| grow(&mut b, 8192));
    assert_eq!(a.usage(), start);
    // ...nor the release.
    a.persistent(|| drop(b));
    assert_eq!(a.usage(), start);
    assert_eq!(a.peak(), peak);
    // Ordinary blocks still count.
    let c = alloc(&a, 100);
    assert_eq!(a.usage(), start + 112);
    drop(c);
    assert_eq!(a.usage(), start);
}

#[test]
fn persistent_block_freed_outside_the_scope_shows_as_a_release() {
    let a = CountingAlloc::new(System);
    a.set_baseline();
    let start = a.usage();
    let b = a.persistent(|| alloc(&a, 1000));
    assert_eq!(a.usage(), start);
    drop(b);
    assert_eq!(a.usage(), start - 1024);
}

#[test]
fn persistent_scope_moves_the_limit_threshold_with_the_baseline() {
    let a = CountingAlloc::new(System);
    a.set_baseline();
    a.set_tracked(true);
    a.set_limit(Some(PHP_BASELINE + 10_000)).expect("limit");
    let _p = a.persistent(|| alloc(&a, 8_000));
    // 8,000 persistent bytes (charged 8,192) do not count: 8,000 more
    // still fit.
    let _b = alloc(&a, 8_000);
    assert!(a.check().is_none());
}

// ---- php-src's size classes ---------------------------------------------

/// The element sizes of `ZEND_MM_BINS_INFO` (`Zend/zend_alloc_sizes.h`),
/// copied from the C macro.
const C_BINS: [usize; 30] = [
    8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640,
    768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072,
];

#[test]
fn zend_size_matches_the_c_size_classes() {
    use crate::limit::MAX_LARGE_SIZE;
    // Small: the first bin that holds the request (`zend_mm_small_size_to_bin`
    // picks the smallest), for every size.
    for size in 1..=MAX_SMALL_SIZE {
        let want = C_BINS
            .iter()
            .copied()
            .find(|&b| b >= size)
            .expect("a bin holds every small size");
        assert_eq!(zend_size(size), want, "{size}");
        assert_eq!(
            usize::from(BIN_SIZE[small_size_to_bin(size)]),
            want,
            "{size}"
        );
    }
    // Every boundary: a bin's size stays in it, one byte more is the next.
    for w in C_BINS.windows(2) {
        assert_eq!(zend_size(w[0]), w[0]);
        assert_eq!(zend_size(w[0] + 1), w[1]);
    }
    assert_eq!(zend_size(0), 8, "size 0 takes the first bin");
    // Large: whole pages (`ZEND_MM_SIZE_TO_NUM(size, ZEND_MM_PAGE_SIZE)`);
    // huge: `ZEND_MM_ALIGNED_SIZE_EX(size, REAL_PAGE_SIZE)`.
    for (size, want) in [
        (MAX_SMALL_SIZE + 1, PAGE_SIZE),
        (PAGE_SIZE, PAGE_SIZE),
        (PAGE_SIZE + 1, 2 * PAGE_SIZE),
        (255 * PAGE_SIZE, 255 * PAGE_SIZE),
        (255 * PAGE_SIZE + 1, 256 * PAGE_SIZE),
        (MAX_LARGE_SIZE - 1, MAX_LARGE_SIZE),
        (MAX_LARGE_SIZE, MAX_LARGE_SIZE),
        (MAX_LARGE_SIZE + 1, CHUNK_SIZE),
        (CHUNK_SIZE, CHUNK_SIZE),
        (CHUNK_SIZE + 1, CHUNK_SIZE + PAGE_SIZE),
        // str_repeat('a', 5 MiB): a 24-byte zend_string header + 5 MiB + 1.
        (5 * M + 25, 5_246_976),
    ] {
        assert_eq!(zend_size(size), want, "{size}");
    }
    // The thresholds the accounting tests sizes against are fixed points,
    // so a size and its class fall on the same side of each.
    for t in [MAX_SMALL_SIZE, MAX_LARGE_SIZE, 255 * PAGE_SIZE] {
        assert_eq!(zend_size(t), t);
    }
}

#[test]
fn usage_moves_only_when_a_block_changes_size_class() {
    // memory_get_usage() with a string of n bytes alive (header + n + 1
    // bytes): php8.5 steps 256 256 ... 256 264 at n = 8 -> 9 for a block
    // crossing from the 32-byte into the 40-byte bin, never by one byte.
    let a = CountingAlloc::new(System);
    assert!(a.bind_to_current_thread());
    a.set_baseline();
    let base = a.usage();
    let seen: Vec<usize> = (1..=40)
        .map(|n| {
            let _live = alloc(&a, 24 + n + 1);
            a.usage() - base
        })
        .collect();
    let mut want = vec![32; 7];
    want.extend([40; 8]);
    want.extend([48; 8]);
    want.extend([56; 8]);
    want.extend([64; 8]);
    want.extend([80; 1]);
    assert_eq!(seen, want);
    // A realloc within a class moves nothing, across classes the difference.
    let mut b = alloc(&a, 25);
    let in_class = a.usage();
    grow(&mut b, 32);
    assert_eq!(a.usage(), in_class);
    grow(&mut b, 33);
    assert_eq!(a.usage(), in_class + 8);
    grow(&mut b, 3000);
    assert_eq!(a.usage(), in_class - 32 + 3072);
    grow(&mut b, 3100);
    assert_eq!(a.usage(), in_class - 32 + PAGE_SIZE);
    grow(&mut b, 4000);
    assert_eq!(a.usage(), in_class - 32 + PAGE_SIZE, "the same page");
    drop(b);
    assert_eq!(a.usage(), base);
    assert_eq!(a.peak(), base + 4096);
    a.unbind_current_thread();
}

#[test]
fn limit_errors_report_what_php_src_tried_to_allocate() {
    // `php -d memory_limit=4M -r '$s = str_repeat("a", 5 * 1024 * 1024);'`:
    // "Allowed memory size of 4194304 bytes exhausted (tried to allocate
    // 5246976 bytes)", the huge block rounded to pages.
    let a = CountingAlloc::new(System);
    a.request_start(Some(4 * M)).expect("limit");
    let over = alloc(&a, 5 * M + 25);
    assert_eq!(
        a.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: 5_246_976
        })
    );
    assert_eq!(
        a.check().expect("pending").to_string(),
        "Allowed memory size of 4194304 bytes exhausted (tried to allocate 5246976 bytes)"
    );
    a.clear();
    drop(over);
    a.request_end();
    // A loop filling an array with small strings: "(tried to allocate
    // 2097152 bytes)", the chunk the next small block would need.
    let b = CountingAlloc::new(System);
    b.request_start(Some(4 * M)).expect("limit");
    let mut fill = Vec::new();
    while b.check().is_none() {
        fill.push(alloc(&b, 24 + 100 + 1));
    }
    assert_eq!(
        b.check(),
        Some(OverLimit {
            limit: 4 * M,
            tried: CHUNK_SIZE
        })
    );
    b.clear();
    drop(fill);
    b.request_end();
}
