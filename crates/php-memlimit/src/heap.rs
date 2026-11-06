//! The request thread's small-block heap: a port of the small-size part of
//! `Zend/zend_alloc.c` (`zend_mm`).
//!
//! # Design (as in `zend_mm`)
//!
//! * Memory comes in 2 MiB *chunks* ([`CHUNK_SIZE`], aligned to their own
//!   size), divided into 4 KiB pages.
//! * Requests up to [`MAX_SMALL_SIZE`] (3072 bytes) are rounded up to one
//!   of [`BINS`] size classes (`ZEND_MM_BINS_INFO` in
//!   `Zend/zend_alloc_sizes.h`). Each bin carves *runs* of
//!   `bin_pages` pages out of the current chunk and threads the run's
//!   elements onto the bin's free list (`zend_mm_alloc_small_slow`).
//! * Allocation pops the bin's free list and freeing pushes onto it
//!   (`zend_mm_alloc_small` / `zend_mm_free_small`): a load and a store
//!   each, no locked instruction, because only the thread that owns the
//!   heap (the request thread) touches the lists.
//! * Large (up to `ZEND_MM_MAX_LARGE_SIZE`) and huge requests are left to
//!   the inner allocator. `zend_mm` maps huge blocks on their own, which is
//!   what the inner allocator does for them as well; large runs are rare in
//!   the profiled workloads and are not ported (callgrind on the release
//!   binary, `perf/bench/suite` scaled 50x: `objects` makes 100k small
//!   allocations and no large one, `strings` 10k and 3, `array_rw` 6.2k
//!   and 7, plus 8 large reallocations whose cost is the copy).
//!
//! # Differences forced by Rust
//!
//! * `GlobalAlloc::dealloc` passes the block's `Layout`, so the bin of a
//!   freed block is computed from its size, as `_efree_<size>` does, and
//!   no page map is needed.
//! * Rust code allocates on every thread and may free a block on a thread
//!   other than the one that allocated it. Only the owner thread allocates
//!   from the heap; other threads use the inner allocator. A heap block
//!   freed by another thread is pushed onto the bin's *remote* list (a
//!   lock-free stack), which the owner takes over in one swap when the
//!   bin's own list runs dry.
//! * A freed pointer may belong to the heap or to the inner allocator. The
//!   global chunk map (one bit per 2 MiB of address space, set when a
//!   chunk is obtained) tells them apart. Chunks are never given back, so a
//!   bit is never cleared and an address once tested as "heap" stays so
//!   for as long as the block lives.
//! * Chunks are never returned to the system (`zend_mm` caches its chunks
//!   too, and frees them at `zend_mm_shutdown`, which Rust's object
//!   lifetimes do not allow). Freed small blocks stay on their bin's list.
//!
//! The accounting (`memory_get_usage()`, `memory_limit`) is not affected:
//! [`crate::CountingAlloc`] counts the requested bytes as before.

use std::alloc::{GlobalAlloc, Layout};
use std::ptr;
use std::sync::atomic::{
    AtomicPtr, AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

use crate::limit::{CHUNK_SIZE, PAGE_SIZE};

/// `ZEND_MM_MAX_SMALL_SIZE`: the largest request served from a bin.
pub const MAX_SMALL_SIZE: usize = 3072;

/// `ZEND_MM_BINS`: the number of small size classes.
pub const BINS: usize = 30;

/// The largest alignment a bin element is guaranteed to have for a
/// request rounded up to it: runs start on a page, and every bin size that
/// a multiple of 16 rounds to (16, 32, 48 and all bins from 64 bytes up)
/// is itself a multiple of 16.
pub const MAX_SMALL_ALIGN: usize = 16;

/// `bin_data_size[]`: the element size of each bin.
pub const BIN_SIZE: [u16; BINS] = [
    8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640,
    768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072,
];

/// `bin_elements[]`: elements per run.
pub const BIN_ELEMENTS: [u16; BINS] = [
    512, 256, 170, 128, 102, 85, 73, 64, 51, 42, 36, 32, 25, 21, 18, 16, 64, 32, 9, 8, 32, 16, 9,
    8, 16, 8, 16, 8, 8, 4,
];

/// `bin_pages[]`: pages per run.
pub const BIN_PAGES: [u8; BINS] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 5, 3, 1, 1, 5, 3, 2, 2, 5, 3, 7, 4, 5, 3,
];

/// `zend_mm_small_size_to_bin()` (the bit-twiddling variant), for
/// `size <= MAX_SMALL_SIZE`.
pub const fn small_size_to_bin(size: usize) -> usize {
    if size == 0 {
        // "we need to support size == 0 ..."
        0
    } else if size <= 64 {
        (size - 1) >> 3
    } else {
        let t1 = size - 1;
        // zend_mm_small_size_to_bit(): 1-based index of the highest set bit.
        let bit = (usize::BITS - t1.leading_zeros()) as usize;
        let t2 = bit - 3;
        (t1 >> t2) + ((t2 - 3) << 2)
    }
}

/// [`small_size_to_bin`] for every multiple of 8 up to [`MAX_SMALL_SIZE`],
/// indexed by `size.div_ceil(8)`: all bin sizes are multiples of 8, so a
/// size and its next multiple of 8 share a bin.
// Bins are below 30: the narrowing cannot truncate.
#[allow(clippy::cast_possible_truncation)]
static SIZE_TO_BIN: [u8; MAX_SMALL_SIZE / 8 + 1] = {
    let mut t = [0u8; MAX_SMALL_SIZE / 8 + 1];
    let mut i = 0;
    while i < t.len() {
        t[i] = small_size_to_bin(i * 8) as u8;
        i += 1;
    }
    t
};

/// The bin serving `layout`, or `None` when the heap does not serve it
/// (larger than [`MAX_SMALL_SIZE`] once rounded up to its alignment, or
/// aligned to more than [`MAX_SMALL_ALIGN`]).
///
/// A pure function of the layout, so the bin found when a block is freed
/// or reallocated is the one it was allocated from.
#[inline]
pub fn small_bin(layout: Layout) -> Option<usize> {
    let align = layout.align();
    // `Layout` guarantees that its size rounded up to its alignment does
    // not overflow `isize`, so this cannot wrap.
    let size = (layout.size() + (align - 1)) & !(align - 1);
    if align <= MAX_SMALL_ALIGN && size <= MAX_SMALL_SIZE {
        Some(usize::from(SIZE_TO_BIN[(size + 7) >> 3]))
    } else {
        None
    }
}

// ---- chunk map --------------------------------------------------------

/// `log2(CHUNK_SIZE)`.
const CHUNK_SHIFT: u32 = CHUNK_SIZE.trailing_zeros();

/// Chunks are tracked for addresses below `2^ADDRESS_BITS` (the user half
/// of a 48-bit address space). A chunk the inner allocator places above
/// that is not used.
const ADDRESS_BITS: u32 = 47;

/// Words of the chunk map (one bit per possible chunk): 8 MiB of zeroed
/// static memory, of which only the pages covering chunks in use are ever
/// touched.
const MAP_WORDS: usize = 1 << (ADDRESS_BITS - CHUNK_SHIFT - 6);

/// One bit per 2 MiB of address space: set for every chunk any heap in
/// the process obtained. Shared by all heaps (a pointer handed to an
/// allocator's `dealloc` was allocated by that allocator, so a set bit
/// means "a block of *this* heap"). Bits are never cleared.
static CHUNK_MAP: [AtomicU64; MAP_WORDS] = [const { AtomicU64::new(0) }; MAP_WORDS];

/// Whether `p` lies in a heap chunk.
#[inline]
pub fn is_heap_block(p: *const u8) -> bool {
    let chunk = p.addr() >> CHUNK_SHIFT;
    CHUNK_MAP
        .get(chunk >> 6)
        .is_some_and(|w| w.load(Relaxed) & (1u64 << (chunk & 63)) != 0)
}

/// Records the chunk starting at `chunk`; `false` if its address is beyond
/// the map.
fn mark_chunk(chunk: *const u8) -> bool {
    let c = chunk.addr() >> CHUNK_SHIFT;
    let Some(w) = CHUNK_MAP.get(c >> 6) else {
        return false;
    };
    // Relaxed suffices: every thread that later frees a block of this
    // chunk got the block from the owner through some synchronisation that
    // happens after this store.
    w.fetch_or(1u64 << (c & 63), Relaxed);
    true
}

/// Layout of a chunk obtained from the inner allocator.
const CHUNK_LAYOUT: Layout = match Layout::from_size_align(CHUNK_SIZE, CHUNK_SIZE) {
    Ok(l) => l,
    Err(_) => panic!("chunk layout"),
};

// ---- free-list links ---------------------------------------------------

/// The next-block link stored in the first word of a free block
/// (`zend_mm_free_slot.next_free_slot`).
///
/// # Safety
///
/// `p` is a free heap block the caller has exclusive access to (or one
/// published to it through a remote list's acquire swap): at least 8
/// bytes, 8-aligned (bin sizes are multiples of 8 and runs start on a
/// page), holding a link written by [`write_next`].
#[inline]
#[allow(clippy::cast_ptr_alignment)] // heap blocks are 8-aligned, see above
unsafe fn read_next(p: *mut u8) -> *mut u8 {
    // SAFETY: see the function's contract.
    unsafe { p.cast::<*mut u8>().read() }
}

/// Stores the next-block link in the free block `p`.
///
/// # Safety
///
/// `p` is a heap block that is free (being freed or never handed out) and
/// that the caller has exclusive access to; see [`read_next`].
#[inline]
#[allow(clippy::cast_ptr_alignment)] // heap blocks are 8-aligned, see above
unsafe fn write_next(p: *mut u8, next: *mut u8) {
    // SAFETY: see the function's contract.
    unsafe { p.cast::<*mut u8>().write(next) }
}

// ---- the heap ------------------------------------------------------------

/// The small-block heap (`zend_mm_heap`, small part). See the module docs.
///
/// Every field is an atomic so that the type is `Sync` without `unsafe
/// impl`; the owner-only fields are accessed with relaxed loads and stores,
/// which compile to plain moves. Which thread is the owner is decided by
/// [`crate::CountingAlloc`]; ownership is handed over with release/acquire
/// ordering, so the next owner sees the lists as the previous one left
/// them.
#[derive(Debug)]
pub(crate) struct Heap {
    /// `free_slot[]`: per-bin free lists. Owner only.
    free: [AtomicPtr<u8>; BINS],
    /// Per-bin stacks of blocks freed by other threads (push: any thread,
    /// release CAS; take-all: the owner, acquire swap).
    remote: [AtomicPtr<u8>; BINS],
    /// Next unused page of the current chunk (null before the first
    /// chunk). Owner only.
    bump: AtomicPtr<u8>,
    /// Address one past the end of the current chunk. Owner only.
    bump_end: AtomicUsize,
    /// Chunks obtained so far.
    chunks: AtomicUsize,
}

impl Heap {
    pub(crate) const fn new() -> Self {
        Self {
            free: [const { AtomicPtr::new(ptr::null_mut()) }; BINS],
            remote: [const { AtomicPtr::new(ptr::null_mut()) }; BINS],
            bump: AtomicPtr::new(ptr::null_mut()),
            bump_end: AtomicUsize::new(0),
            chunks: AtomicUsize::new(0),
        }
    }

    /// Chunks obtained so far.
    pub(crate) fn chunks(&self) -> usize {
        self.chunks.load(Relaxed)
    }

    /// The fast half of `zend_mm_alloc_small()`: pops the head of bin
    /// `bin`'s free list, or returns null when the list is empty (the
    /// caller then goes to [`alloc`](Self::alloc), which carves a run).
    /// A load, a dependent load and a store; no call.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap and `bin < BINS`.
    #[inline]
    pub(crate) unsafe fn pop(&self, bin: usize) -> *mut u8 {
        let head = &self.free[bin];
        let p = head.load(Relaxed);
        if !p.is_null() {
            // SAFETY: `p` heads this bin's free list, which only the owner
            // (the caller) touches, so it is a free block holding a link.
            head.store(unsafe { read_next(p) }, Relaxed);
        }
        p
    }

    /// `zend_mm_free_small()` on the owner thread: pushes `p` onto bin
    /// `bin`'s free list. A load and two stores; no call.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `p` is a block of this heap
    /// allocated from bin `bin` (its layout maps to `bin` through
    /// [`small_bin`]) that is not used again.
    #[inline]
    pub(crate) unsafe fn push(&self, p: *mut u8, bin: usize) {
        let head = &self.free[bin];
        // SAFETY: the block is being freed, so the caller has exclusive
        // access to it; the list is the owner's.
        unsafe { write_next(p, head.load(Relaxed)) };
        head.store(p, Relaxed);
    }

    /// `zend_mm_alloc_small()`: a block of bin `bin`, or null when no
    /// chunk could be obtained (the caller then falls back to the inner
    /// allocator).
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap, `bin < BINS`, and `inner` is the
    /// allocator this heap has always obtained its chunks from.
    #[inline]
    pub(crate) unsafe fn alloc<A: GlobalAlloc>(&self, inner: &A, bin: usize) -> *mut u8 {
        // SAFETY: forwarded contract.
        let p = unsafe { self.pop(bin) };
        if p.is_null() {
            // SAFETY: forwarded contract.
            return unsafe { self.alloc_slow(inner, bin) };
        }
        p
    }

    /// `zend_mm_alloc_small_slow()`: takes over the bin's remote frees, or
    /// carves a new run.
    ///
    /// # Safety
    ///
    /// As [`alloc`](Self::alloc), with the bin's free list empty.
    #[cold]
    #[inline(never)]
    unsafe fn alloc_slow<A: GlobalAlloc>(&self, inner: &A, bin: usize) -> *mut u8 {
        let remote = self.remote[bin].swap(ptr::null_mut(), Acquire);
        if !remote.is_null() {
            // SAFETY: the acquire swap made the pushers' links visible and
            // gave the owner exclusive access to the whole stack.
            self.free[bin].store(unsafe { read_next(remote) }, Relaxed);
            return remote;
        }
        let size = usize::from(BIN_SIZE[bin]);
        let count = usize::from(BIN_ELEMENTS[bin]);
        let run_bytes = usize::from(BIN_PAGES[bin]) * PAGE_SIZE;
        // SAFETY: forwarded contract.
        let Some(run) = (unsafe { self.take_pages(inner, run_bytes) }) else {
            return ptr::null_mut();
        };
        // Element 0 is returned; 1..count become the free list, in address
        // order like zend_mm's.
        let mut next = ptr::null_mut();
        for i in (1..count).rev() {
            // SAFETY: `i * size < pages * PAGE_SIZE` (`count * size` fits the
            // run by construction of the tables), so the element lies in
            // the run, which nothing else references yet.
            unsafe {
                let e = run.add(i * size);
                write_next(e, next);
                next = e;
            }
        }
        self.free[bin].store(next, Relaxed);
        run
    }

    /// `bytes` (a multiple of the page size, at most a chunk) of fresh
    /// pages from the current chunk, or from a new one.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `inner` as in [`alloc`](Self::alloc).
    unsafe fn take_pages<A: GlobalAlloc>(&self, inner: &A, bytes: usize) -> Option<*mut u8> {
        let cur = self.bump.load(Relaxed);
        if !cur.is_null() && self.bump_end.load(Relaxed) - cur.addr() >= bytes {
            // SAFETY: `cur + bytes` stays within (or one past) the chunk.
            self.bump.store(unsafe { cur.add(bytes) }, Relaxed);
            return Some(cur);
        }
        // The rest of the current chunk (fewer pages than the longest run)
        // is left unused, like the tail of a full zend_mm chunk.
        // SAFETY: `CHUNK_LAYOUT` has a non-zero size.
        let chunk = unsafe { inner.alloc(CHUNK_LAYOUT) };
        if chunk.is_null() {
            return None;
        }
        if !mark_chunk(chunk) {
            // SAFETY: allocated just above with this layout.
            unsafe { inner.dealloc(chunk, CHUNK_LAYOUT) };
            return None;
        }
        self.chunks.fetch_add(1, Relaxed);
        // SAFETY: `bytes <= CHUNK_SIZE`.
        self.bump.store(unsafe { chunk.add(bytes) }, Relaxed);
        self.bump_end.store(chunk.addr() + CHUNK_SIZE, Relaxed);
        Some(chunk)
    }

    /// `zend_mm_free_small()`: puts the block `p` of bin `bin` back. On
    /// the owner thread it goes onto the bin's free list; on any other
    /// thread onto the bin's remote list.
    ///
    /// # Safety
    ///
    /// `p` is a live block of this heap allocated from bin `bin` (its
    /// layout maps to `bin` through [`small_bin`]) that is not used again,
    /// and `owner` is true only on the thread owning the heap.
    #[inline]
    pub(crate) unsafe fn free(&self, p: *mut u8, bin: usize, owner: bool) {
        if owner {
            // SAFETY: forwarded contract.
            unsafe { self.push(p, bin) };
        } else {
            // SAFETY: forwarded contract.
            unsafe { self.free_remote(p, bin) };
        }
    }

    /// Pushes `p` onto bin `bin`'s remote list (a Treiber stack: pushes
    /// from any thread, the owner only ever takes the whole stack, so there
    /// is no ABA problem).
    ///
    /// # Safety
    ///
    /// As [`free`](Self::free).
    #[cold]
    #[inline(never)]
    unsafe fn free_remote(&self, p: *mut u8, bin: usize) {
        let head = &self.remote[bin];
        let mut cur = head.load(Relaxed);
        loop {
            // SAFETY: `p` is being freed: nobody else accesses it until the
            // CAS below publishes it.
            unsafe { write_next(p, cur) };
            match head.compare_exchange_weak(cur, p, Release, Relaxed) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).expect("layout")
    }

    #[test]
    fn bin_tables_match_zend_alloc_sizes() {
        for (b, &size) in BIN_SIZE.iter().enumerate() {
            let size = usize::from(size);
            let pages = usize::from(BIN_PAGES[b]);
            assert_eq!(
                usize::from(BIN_ELEMENTS[b]),
                pages * PAGE_SIZE / size,
                "bin {b}: elements per run"
            );
            assert_eq!(size % 8, 0, "bin {b}");
            assert_eq!(small_size_to_bin(size), b, "bin {b}");
            if b > 0 {
                assert!(BIN_SIZE[b - 1] < BIN_SIZE[b], "bin {b}");
            }
        }
        assert_eq!(usize::from(BIN_SIZE[BINS - 1]), MAX_SMALL_SIZE);
    }

    #[test]
    fn every_small_size_maps_to_the_smallest_fitting_bin() {
        for size in 0..=MAX_SMALL_SIZE {
            let b = small_size_to_bin(size);
            assert!(usize::from(BIN_SIZE[b]) >= size, "{size}");
            assert!(b == 0 || usize::from(BIN_SIZE[b - 1]) < size, "{size}");
            assert_eq!(small_bin(layout(size, 1)), Some(b), "{size}");
        }
        assert_eq!(small_bin(layout(MAX_SMALL_SIZE + 1, 1)), None);
        assert_eq!(small_bin(layout(1 << 20, 8)), None);
    }

    #[test]
    fn aligned_requests_get_bins_of_aligned_elements() {
        for align in [1, 2, 4, 8, 16] {
            for size in 1..=MAX_SMALL_SIZE {
                let b = small_bin(layout(size, align)).expect("served");
                let elem = usize::from(BIN_SIZE[b]);
                assert!(elem >= size, "{size}/{align}");
                // Runs start on a page, so element `i` sits at `i * elem`.
                assert_eq!(elem % align, 0, "{size}/{align}");
            }
        }
        assert_eq!(small_bin(layout(8, 32)), None, "over-aligned");
        assert_eq!(small_bin(layout(64, 4096)), None, "page-aligned");
    }

    #[test]
    fn chunk_map_tracks_marked_chunks() {
        assert!(!is_heap_block(ptr::null()));
        let beyond = ptr::without_provenance::<u8>(1 << ADDRESS_BITS);
        assert!(!is_heap_block(beyond));
        assert!(!mark_chunk(beyond), "beyond the map");
        // A real chunk, so that no other allocation can live there; it is
        // leaked on purpose (map bits are never cleared).
        // SAFETY: non-zero size.
        let chunk = unsafe { System.alloc(CHUNK_LAYOUT) };
        assert!(!chunk.is_null());
        assert_eq!(chunk.addr() % CHUNK_SIZE, 0);
        assert!(!is_heap_block(chunk));
        assert!(mark_chunk(chunk));
        assert!(is_heap_block(chunk));
        assert!(is_heap_block(chunk.wrapping_add(CHUNK_SIZE / 2)));
        assert!(is_heap_block(chunk.wrapping_add(CHUNK_SIZE - 1)));
    }

    #[test]
    fn heap_carves_runs_and_reuses_freed_blocks() {
        let heap = Heap::new();
        let bin = small_size_to_bin(40);
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread is the only user of `heap`; `bin < BINS`.
        let blocks: Vec<*mut u8> = (0..=per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        assert!(blocks.iter().all(|p| !p.is_null() && is_heap_block(*p)));
        assert_eq!(heap.chunks(), 1);
        // The first run is handed out in address order, 40 bytes apart.
        for w in blocks[..per_run].windows(2) {
            assert_eq!(w[1].addr() - w[0].addr(), 40);
        }
        // The extra block starts the next run, one page further.
        assert_eq!(blocks[per_run].addr() - blocks[0].addr(), PAGE_SIZE);
        for &p in &blocks {
            // SAFETY: allocated above from `bin`, freed once, by the owner.
            unsafe { heap.free(p, bin, true) };
        }
        // LIFO: the last block freed is the next one handed out.
        // SAFETY: as above.
        let again = unsafe { heap.alloc(&System, bin) };
        assert_eq!(again, blocks[per_run]);
        // SAFETY: as above.
        unsafe { heap.free(again, bin, true) };
    }

    #[test]
    fn remote_frees_are_taken_over_when_the_list_runs_dry() {
        let heap = Heap::new();
        let bin = small_size_to_bin(3072);
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let run: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        // Freed "by another thread": onto the remote list.
        for &p in &run {
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, false) };
        }
        // The owner's list is empty, so the next allocation takes the
        // remote list over instead of carving a new run.
        // SAFETY: as above.
        let reused: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        let mut a: Vec<usize> = run.iter().map(|p| p.addr()).collect();
        let mut b: Vec<usize> = reused.iter().map(|p| p.addr()).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
        assert_eq!(heap.chunks(), 1);
        for &p in &reused {
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, true) };
        }
    }

    #[test]
    fn heap_takes_a_new_chunk_when_the_current_one_is_full() {
        let heap = Heap::new();
        let bin = small_size_to_bin(2048);
        // 512 pages per chunk, 4 pages (8 blocks) per run: 1024 blocks
        // fill a chunk, one more needs a second one.
        let n =
            CHUNK_SIZE / PAGE_SIZE / usize::from(BIN_PAGES[bin]) * usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let blocks: Vec<*mut u8> = (0..=n)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        assert!(blocks.iter().all(|p| !p.is_null() && is_heap_block(*p)));
        assert_eq!(heap.chunks(), 2);
        assert_eq!(blocks[n - 1].addr() - blocks[0].addr(), CHUNK_SIZE - 2048);
        for &p in &blocks {
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, true) };
        }
    }
}
