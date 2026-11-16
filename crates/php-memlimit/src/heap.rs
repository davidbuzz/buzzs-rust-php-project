// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The request thread's small-block heap: a port of the small-size part of
//! `Zend/zend_alloc.c` (`zend_mm`).
//!
//! # Design (as in `zend_mm`)
//!
//! * Memory comes in 2 MiB *chunks* ([`CHUNK_SIZE`], aligned to their own
//!   size), divided into [`PAGES`] pages of 4 KiB. The first page holds
//!   the chunk's header ([`Chunk`], `zend_mm_chunk`: the page map, the
//!   bitmap of pages in use and the list link), so [`USABLE_PAGES`] pages
//!   are handed out (`ZEND_MM_FIRST_PAGE`).
//! * Requests up to [`MAX_SMALL_SIZE`] (3072 bytes) are rounded up to one
//!   of [`BINS`] size classes (`ZEND_MM_BINS_INFO` in
//!   `Zend/zend_alloc_sizes.h`). Each bin carves *runs* of
//!   `bin_pages` pages out of a chunk and threads the run's
//!   elements onto the bin's free list (`zend_mm_alloc_small_slow`).
//! * Allocation pops the bin's free list and freeing pushes onto it
//!   (`zend_mm_alloc_small` / `zend_mm_free_small`): a load and a store
//!   each, no locked instruction, because only the thread that owns the
//!   heap (the request thread) touches the lists.
//! * The pages of a run come from the chunk that provided the previous
//!   run if it has room, else from the first chunk of the list that has
//!   (`zend_mm_alloc_pages`, which always starts at the first chunk):
//!   the chunk's never-used tail while it has no hole, otherwise the
//!   first fit over its page bitmap (`zend_mm` searches best-fit). A new
//!   chunk is obtained only when no chunk has room.
//! * [`Heap::gc`] is `zend_mm_gc` (`gc_mem_caches()`, `collect_heap()`,
//!   and the end of a request whose thread `request_start` bound, as in
//!   `php -S` and `mod_php`): it counts the free elements of every run
//!   through the page map, unlinks the elements of runs that are entirely
//!   free, returns their pages to the chunk (any bin reuses them) and
//!   gives chunks left without a used page back to the inner allocator,
//!   except the first chunk.
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
//!   freed block is computed from its size, as `_efree_<size>` does; the
//!   page map is consulted only by [`Heap::gc`].
//! * Rust code allocates on every thread and may free a block on a thread
//!   other than the one that allocated it. Only the owner thread allocates
//!   from the heap; other threads use the inner allocator. A heap block
//!   freed by another thread is pushed onto the bin's *remote* list (a
//!   lock-free stack), which the owner takes over in one swap when the
//!   bin's own list runs dry.
//! * A freed pointer may belong to the heap or to the inner allocator. The
//!   global chunk map (one bit per 2 MiB of address space, set when a
//!   chunk is obtained) tells them apart. The bit is cleared when `gc`
//!   gives the chunk back, which it does only once no block of the chunk
//!   is live (every element of every run was on a free list and has been
//!   unlinked), so no free can look the chunk up any more; the inner
//!   allocator may then hand the same addresses out again as its own
//!   blocks, which the clear bit routes back to it. Another thread sees
//!   the cleared bit before it can hold such a block: the block reaches it
//!   through the inner allocator's own synchronisation, which follows the
//!   release-ordered clear.
//! * Freed small blocks stay on their bin's list until `gc` runs. `zend_mm`
//!   keeps them too, and in addition resets the whole heap at request end
//!   (`zend_mm_shutdown`), which Rust's object lifetimes do not allow;
//!   collecting at request end is the closest equivalent. php-cli, whose
//!   process ends with its request, does not collect.
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

/// `ZEND_MM_PAGES`: pages per chunk.
pub const PAGES: usize = CHUNK_SIZE / PAGE_SIZE;

/// `ZEND_MM_FIRST_PAGE`: the first page runs are carved from; page 0
/// holds the chunk's header.
pub const FIRST_PAGE: usize = 1;

/// Pages of a chunk available for runs.
pub const USABLE_PAGES: usize = PAGES - FIRST_PAGE;

/// Words of a chunk's page bitmap (`ZEND_MM_PAGE_MAP_LEN`).
const BITMAP_WORDS: usize = PAGES / 64;

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
/// the process holds. Shared by all heaps (a pointer handed to an
/// allocator's `dealloc` was allocated by that allocator, so a set bit
/// means "a block of *this* heap"). A bit is cleared only when
/// [`Heap::gc`] gives a chunk without live blocks back (module docs).
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

/// Forgets the chunk starting at `chunk` (marked before), as it goes back
/// to the inner allocator.
fn unmark_chunk(chunk: *const u8) {
    let c = chunk.addr() >> CHUNK_SHIFT;
    if let Some(w) = CHUNK_MAP.get(c >> 6) {
        // Release: ordered before the inner allocator can hand the memory
        // out again (module docs).
        w.fetch_and(!(1u64 << (c & 63)), Release);
    }
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

// ---- chunk header and page map -------------------------------------------

/// The chunk header (`zend_mm_chunk`), at the start of every chunk's first
/// page. Owner only, reached through raw pointers into the chunk's own
/// memory (never two references at a time).
#[repr(C)]
struct Chunk {
    /// The next chunk of the heap's list, null for the last.
    next: *mut Chunk,
    /// Pages not in use (`free_pages`).
    free_pages: u32,
    /// Every page from this index on is free (`free_tail`: the never-used
    /// tail, lowered when the run ending there is given back). Exact: the
    /// page below it is in use, unless it is [`FIRST_PAGE`].
    free_tail: u32,
    /// One bit per page, set while the page is in use (`free_map`).
    free_map: [u64; BITMAP_WORDS],
    /// What each page holds (`map`); see [`srun`], [`nrun`], [`lrun`].
    map: [u32; PAGES],
}

const _: () = assert!(std::mem::size_of::<Chunk>() <= PAGE_SIZE);

/// Page map entries (`zend_mm_page_info`). The first page of a small run
/// holds [`srun`]`(bin, count)`, where `count` is the number of its free
/// elements while [`Heap::gc`] runs (0 otherwise); the run's other pages
/// hold [`nrun`]`(bin, offset)` with their distance from the first page;
/// the header page holds [`lrun`]`(1)`; a free page holds 0.
const IS_SRUN: u32 = 0x8000_0000;
const IS_LRUN: u32 = 0x4000_0000;
const INFO_BIN_MASK: u32 = 0x1f;
const INFO_COUNT_SHIFT: u32 = 16;
const INFO_COUNT_MASK: u32 = 0x3ff;
const INFO_PAGES_MASK: u32 = 0x3ff;

/// `ZEND_MM_SRUN_EX`: the first page of a small run of `bin` with `count`
/// free elements counted.
// `bin < BINS` and `count <= 512`: nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
const fn srun(bin: usize, count: usize) -> u32 {
    IS_SRUN | bin as u32 | ((count as u32) << INFO_COUNT_SHIFT)
}

/// `ZEND_MM_NRUN`: page `offset` of a small run of `bin`.
// `bin < BINS` and `offset < 8`: nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
const fn nrun(bin: usize, offset: usize) -> u32 {
    IS_SRUN | IS_LRUN | bin as u32 | ((offset as u32) << INFO_COUNT_SHIFT)
}

/// `ZEND_MM_LRUN`: the first page of a run of `pages` pages that is not a
/// small run (today only the header page).
// `pages <= PAGES`: nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
const fn lrun(pages: usize) -> u32 {
    IS_LRUN | pages as u32
}

/// `ZEND_MM_SRUN_BIN_NUM`.
const fn info_bin(info: u32) -> usize {
    (info & INFO_BIN_MASK) as usize
}

/// `ZEND_MM_SRUN_FREE_COUNTER` and `ZEND_MM_NRUN_OFFSET` (the same bits).
const fn info_count(info: u32) -> usize {
    ((info >> INFO_COUNT_SHIFT) & INFO_COUNT_MASK) as usize
}

/// `ZEND_MM_LRUN_PAGES`.
const fn info_pages(info: u32) -> usize {
    (info & INFO_PAGES_MASK) as usize
}

/// `zend_mm_panic("zend_mm_heap corrupted")`: a free list or page map that
/// cannot be right (a block freed twice, a write past a block's end into a
/// header). Nothing sensible can follow, and this runs inside the
/// allocator, where unwinding is not an option.
#[cold]
#[inline(never)]
fn heap_corrupted() -> ! {
    std::process::abort()
}

/// Whether page `page` of `map` is in use.
fn bitmap_is_set(map: &[u64; BITMAP_WORDS], page: usize) -> bool {
    map[page / 64] & (1u64 << (page % 64)) != 0
}

/// `zend_mm_bitset_set_range`: marks `n` pages from `page` in use.
fn bitmap_set(map: &mut [u64; BITMAP_WORDS], page: usize, n: usize) {
    for p in page..page + n {
        map[p / 64] |= 1u64 << (p % 64);
    }
}

/// `zend_mm_bitset_reset_range`: marks `n` pages from `page` free.
fn bitmap_clear(map: &mut [u64; BITMAP_WORDS], page: usize, n: usize) {
    for p in page..page + n {
        map[p / 64] &= !(1u64 << (p % 64));
    }
}

/// The first page of `n` consecutive free pages in `map`, or `None`
/// (`zend_mm_alloc_pages`'s search, first fit; runs may span words).
fn find_free_run(map: &[u64; BITMAP_WORDS], n: usize) -> Option<usize> {
    // Free pages immediately before `pos`.
    let mut len = 0;
    let mut pos = 0;
    for &word in map {
        let mut used = word;
        let mut left = 64;
        while left > 0 {
            // Free pages at `pos`.
            let free = (used.trailing_zeros() as usize).min(left);
            if free > 0 {
                len += free;
                if len >= n {
                    return Some(pos + free - len);
                }
                pos += free;
                left -= free;
                used = used
                    .checked_shr(u32::try_from(free).unwrap_or(u32::MAX))
                    .unwrap_or(0);
            }
            // Pages in use at `pos`.
            let busy = (used.trailing_ones() as usize).min(left);
            if busy > 0 {
                len = 0;
                pos += busy;
                left -= busy;
                used = used
                    .checked_shr(u32::try_from(busy).unwrap_or(u32::MAX))
                    .unwrap_or(0);
            }
        }
    }
    None
}

/// `zend_mm_alloc_pages` within one chunk: marks `n` consecutive free
/// pages in use and returns the first, preferring a hole left by
/// [`Heap::gc`] (first fit) to the never-used tail.
///
/// # Safety
///
/// `chunk` is a live chunk of the heap the calling thread owns, with no
/// reference to its header outstanding; `1 <= n <= USABLE_PAGES`.
// Page counts are at most `PAGES`: nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
unsafe fn chunk_take(chunk: *mut Chunk, n: usize) -> Option<usize> {
    // SAFETY: see the function's contract.
    let c = unsafe { &mut *chunk };
    let free_pages = c.free_pages as usize;
    if free_pages < n {
        return None;
    }
    let tail = c.free_tail as usize;
    // Free pages below the tail: holes `gc` left (every page from the tail
    // on is free, so this cannot underflow).
    let holes = free_pages - (PAGES - tail);
    let page = if holes == 0 {
        if PAGES - tail < n {
            return None;
        }
        tail
    } else {
        find_free_run(&c.free_map, n)?
    };
    bitmap_set(&mut c.free_map, page, n);
    c.free_pages = (free_pages - n) as u32;
    if page + n > tail {
        c.free_tail = (page + n) as u32;
    }
    Some(page)
}

/// `zend_mm_free_pages_ex`: the run of `n` pages at `page` becomes free.
///
/// # Safety
///
/// As [`chunk_take`]; the pages are a run of this chunk that is in use.
// Page counts are at most `PAGES`: nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
unsafe fn chunk_release(chunk: *mut Chunk, page: usize, n: usize) {
    // SAFETY: see the function's contract.
    let c = unsafe { &mut *chunk };
    bitmap_clear(&mut c.free_map, page, n);
    c.map[page..page + n].fill(0);
    c.free_pages += n as u32;
    if c.free_tail as usize == page + n {
        // Everything from `page` on is free now, and so are the free pages
        // right below it: the tail is kept exact (`zend_mm_alloc_pages`
        // corrects zend_mm's "may be not accurate" tail while searching),
        // so a chunk whose holes all join the tail is carved from the tail
        // again, without a bitmap search.
        let mut tail = page;
        while tail > FIRST_PAGE && !bitmap_is_set(&c.free_map, tail - 1) {
            tail -= 1;
        }
        c.free_tail = tail as u32;
    }
}

/// The chunk holding the heap block `p` and the first page of its run.
///
/// # Safety
///
/// `p` is a block of a live chunk of the heap the calling thread owns.
// The chunk base is 2 MiB-aligned, far more than the header needs.
#[allow(clippy::cast_ptr_alignment)]
unsafe fn run_of(p: *mut u8) -> (*mut Chunk, usize) {
    // Chunks are aligned to their size: the header is at the aligned base
    // (`ZEND_MM_ALIGNED_BASE`), inside the same allocation as `p`.
    let chunk = p.map_addr(|a| a & !(CHUNK_SIZE - 1)).cast::<Chunk>();
    let mut page = (p.addr() & (CHUNK_SIZE - 1)) / PAGE_SIZE;
    // SAFETY: `chunk` is the live chunk holding `p`, see above.
    let info = unsafe { (*chunk).map[page] };
    if info & IS_LRUN != 0 {
        // Not the run's first page: its offset leads there.
        let offset = info_count(info);
        if offset > page {
            heap_corrupted();
        }
        page -= offset;
    }
    (chunk, page)
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
    /// The first chunk (`main_chunk`), head of the chunk list; null before
    /// the first chunk is obtained. Never given back. Owner only.
    first: AtomicPtr<Chunk>,
    /// The last chunk of the list, where new chunks are appended. Owner
    /// only.
    last: AtomicPtr<Chunk>,
    /// The chunk that most recently provided pages: tried first. Owner
    /// only.
    current: AtomicPtr<Chunk>,
    /// Chunks held.
    chunks: AtomicUsize,
}

impl Heap {
    pub(crate) const fn new() -> Self {
        Self {
            free: [const { AtomicPtr::new(ptr::null_mut()) }; BINS],
            remote: [const { AtomicPtr::new(ptr::null_mut()) }; BINS],
            first: AtomicPtr::new(ptr::null_mut()),
            last: AtomicPtr::new(ptr::null_mut()),
            current: AtomicPtr::new(ptr::null_mut()),
            chunks: AtomicUsize::new(0),
        }
    }

    /// Chunks held.
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
        // SAFETY: forwarded contract.
        let Some(run) = (unsafe { self.take_run(inner, bin) }) else {
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

    /// The pages of a new run of bin `bin` (`zend_mm_alloc_pages`),
    /// recorded in the page map, or `None` when no chunk could be
    /// obtained.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `inner` as in [`alloc`](Self::alloc).
    unsafe fn take_run<A: GlobalAlloc>(&self, inner: &A, bin: usize) -> Option<*mut u8> {
        let pages = usize::from(BIN_PAGES[bin]);
        // SAFETY: forwarded contract.
        let (chunk, page) = match unsafe { self.find_pages(pages) } {
            Some(found) => found,
            None => {
                // SAFETY: forwarded contract.
                let chunk = unsafe { self.new_chunk(inner) };
                if chunk.is_null() {
                    return None;
                }
                // A fresh chunk has every usable page free, more than any
                // run needs.
                // SAFETY: a live chunk of this heap, no header reference
                // outstanding.
                let page = unsafe { chunk_take(chunk, pages) }?;
                (chunk, page)
            }
        };
        self.current.store(chunk, Relaxed);
        // SAFETY: `chunk` is a live chunk of this heap and `page..page +
        // pages` are its pages, just marked in use, below `PAGES`.
        unsafe {
            (*chunk).map[page] = srun(bin, 0);
            for i in 1..pages {
                (*chunk).map[page + i] = nrun(bin, i);
            }
            Some(chunk.cast::<u8>().add(page * PAGE_SIZE))
        }
    }

    /// `pages` consecutive free pages from the chunk that last provided
    /// some, else from the first chunk that has them.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap.
    unsafe fn find_pages(&self, pages: usize) -> Option<(*mut Chunk, usize)> {
        let current = self.current.load(Relaxed);
        if !current.is_null() {
            // SAFETY: `current` is a live chunk of this heap.
            if let Some(page) = unsafe { chunk_take(current, pages) } {
                return Some((current, page));
            }
        }
        let mut chunk = self.first.load(Relaxed);
        while !chunk.is_null() {
            if chunk != current {
                // SAFETY: a live chunk of this heap's list.
                if let Some(page) = unsafe { chunk_take(chunk, pages) } {
                    return Some((chunk, page));
                }
            }
            // SAFETY: as above.
            chunk = unsafe { (*chunk).next };
        }
        None
    }

    /// A new chunk from `inner`, initialised (`zend_mm_chunk_init`) and
    /// appended to the list; null when none could be obtained.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `inner` as in [`alloc`](Self::alloc).
    // `USABLE_PAGES` and `FIRST_PAGE` fit a `u32`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_ptr_alignment)]
    unsafe fn new_chunk<A: GlobalAlloc>(&self, inner: &A) -> *mut Chunk {
        // SAFETY: `CHUNK_LAYOUT` has a non-zero size.
        let raw = unsafe { inner.alloc(CHUNK_LAYOUT) };
        if raw.is_null() {
            return ptr::null_mut();
        }
        if !mark_chunk(raw) {
            // SAFETY: allocated just above with this layout.
            unsafe { inner.dealloc(raw, CHUNK_LAYOUT) };
            return ptr::null_mut();
        }
        // The chunk is aligned to 2 MiB, far more than the header needs.
        let chunk = raw.cast::<Chunk>();
        // SAFETY: `chunk` is a fresh 2 MiB block that holds the header, and
        // the list's chunks are live.
        unsafe {
            chunk.write(Chunk {
                next: ptr::null_mut(),
                free_pages: USABLE_PAGES as u32,
                free_tail: FIRST_PAGE as u32,
                free_map: [0; BITMAP_WORDS],
                map: [0; PAGES],
            });
            // The header page is in use.
            (*chunk).free_map[0] = (1u64 << FIRST_PAGE) - 1;
            (*chunk).map[0] = lrun(FIRST_PAGE);
            let last = self.last.load(Relaxed);
            if last.is_null() {
                self.first.store(chunk, Relaxed);
            } else {
                (*last).next = chunk;
            }
        }
        self.last.store(chunk, Relaxed);
        self.chunks.fetch_add(1, Relaxed);
        chunk
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

    /// `zend_mm_gc()`: gives the pages of runs without a live element back
    /// to their chunks, and chunks left without a used page (except the
    /// first) back to `inner`. Returns the bytes released (the freed
    /// runs' pages).
    ///
    /// Blocks on the remote lists are free too: they are taken over
    /// first. Nothing a live block refers to is touched: a run is released
    /// only when every one of its elements was found on a free list.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `inner` as in [`alloc`](Self::alloc).
    pub(crate) unsafe fn gc<A: GlobalAlloc>(&self, inner: &A) -> usize {
        // SAFETY: forwarded contract.
        unsafe {
            self.gc_take_remote();
            self.gc_count_and_unlink();
            self.gc_release(inner)
        }
    }

    /// The first step of [`gc`](Self::gc): splices each remote list onto
    /// its bin's free list.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap.
    unsafe fn gc_take_remote(&self) {
        for (bin, remote) in self.remote.iter().enumerate() {
            let head = remote.swap(ptr::null_mut(), Acquire);
            if head.is_null() {
                continue;
            }
            let mut tail = head;
            // SAFETY: the acquire swap gave the owner the whole stack.
            unsafe {
                loop {
                    let next = read_next(tail);
                    if next.is_null() {
                        break;
                    }
                    tail = next;
                }
                write_next(tail, self.free[bin].load(Relaxed));
            }
            self.free[bin].store(head, Relaxed);
        }
    }

    /// The second step of [`gc`](Self::gc): counts the free elements of
    /// every run in its first page's map entry, then unlinks the elements
    /// of runs that are entirely free.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap.
    unsafe fn gc_count_and_unlink(&self) {
        for (bin, free) in self.free.iter().enumerate() {
            let elements = usize::from(BIN_ELEMENTS[bin]);
            let mut has_empty = false;
            let mut p = free.load(Relaxed);
            while !p.is_null() {
                // SAFETY: `p` is a free block of this heap on the owner's
                // list; its chunk is live.
                let (chunk, page) = unsafe { run_of(p) };
                // SAFETY: `page` is a page of the live chunk.
                let info = unsafe { (*chunk).map[page] };
                let count = info_count(info) + 1;
                // The run's first page must say "small run of this bin". A
                // block is on its free list once, so more free elements
                // than the run holds means a corrupted list (most likely a
                // cycle, which would loop here forever).
                if info & (IS_SRUN | IS_LRUN) != IS_SRUN
                    || info_bin(info) != bin
                    || count > elements
                {
                    heap_corrupted();
                }
                has_empty |= count == elements;
                // SAFETY: as above.
                unsafe {
                    (*chunk).map[page] = srun(bin, count);
                    p = read_next(p);
                }
            }
            if !has_empty {
                continue;
            }
            let mut prev: *mut u8 = ptr::null_mut();
            let mut p = free.load(Relaxed);
            while !p.is_null() {
                // SAFETY: as in the counting pass.
                let next = unsafe { read_next(p) };
                // SAFETY: as in the counting pass.
                let (chunk, page) = unsafe { run_of(p) };
                // SAFETY: as in the counting pass.
                if info_count(unsafe { (*chunk).map[page] }) == elements {
                    if prev.is_null() {
                        free.store(next, Relaxed);
                    } else {
                        // SAFETY: `prev` is a free block on this list.
                        unsafe { write_next(prev, next) };
                    }
                } else {
                    prev = p;
                }
                p = next;
            }
        }
    }

    /// The last step of [`gc`](Self::gc): frees the pages of the empty
    /// runs, resets the counters, and gives chunks left without a used
    /// page back to `inner` (except the first). Returns the bytes
    /// released.
    ///
    /// # Safety
    ///
    /// The calling thread owns the heap; `inner` as in [`alloc`](Self::alloc).
    unsafe fn gc_release<A: GlobalAlloc>(&self, inner: &A) -> usize {
        let mut collected = 0;
        let mut prev: *mut Chunk = ptr::null_mut();
        let mut chunk = self.first.load(Relaxed);
        while !chunk.is_null() {
            // SAFETY: `chunk` is a live chunk of this heap's list; the
            // header is reached through the raw pointer, one access at a
            // time.
            let next = unsafe { (*chunk).next };
            let mut i = FIRST_PAGE;
            loop {
                // Re-read: releasing the run that ends at the tail lowers
                // it.
                // SAFETY: as above.
                let free_tail = unsafe { (*chunk).free_tail } as usize;
                if i >= free_tail {
                    break;
                }
                // SAFETY: as above.
                if !bitmap_is_set(unsafe { &(*chunk).free_map }, i) {
                    i += 1;
                    continue;
                }
                // SAFETY: as above.
                let info = unsafe { (*chunk).map[i] };
                if info & IS_SRUN == 0 {
                    // A large run (`ZEND_MM_LRUN`; this heap's only one is
                    // the header page, below `FIRST_PAGE`).
                    i += info_pages(info).max(1);
                    continue;
                }
                if info & IS_LRUN != 0 {
                    // The walk goes from run to run, so this is a run's
                    // first page: it cannot hold an `nrun` offset.
                    heap_corrupted();
                }
                let bin = info_bin(info);
                let pages = usize::from(BIN_PAGES[bin]);
                if info_count(info) == usize::from(BIN_ELEMENTS[bin]) {
                    // All elements are free (and were unlinked).
                    // SAFETY: a run in use of this live chunk.
                    unsafe { chunk_release(chunk, i, pages) };
                    collected += pages;
                } else {
                    // Reset the counter.
                    // SAFETY: as above.
                    unsafe { (*chunk).map[i] = srun(bin, 0) };
                }
                i += pages;
            }
            // SAFETY: as above.
            let free_pages = unsafe { (*chunk).free_pages } as usize;
            if free_pages == USABLE_PAGES && !prev.is_null() {
                // SAFETY: `prev` is the live predecessor of `chunk`.
                unsafe { (*prev).next = next };
                if self.last.load(Relaxed) == chunk {
                    self.last.store(prev, Relaxed);
                }
                if self.current.load(Relaxed) == chunk {
                    self.current.store(self.first.load(Relaxed), Relaxed);
                }
                self.chunks.fetch_sub(1, Relaxed);
                let raw = chunk.cast::<u8>();
                unmark_chunk(raw);
                // SAFETY: obtained from `inner` with this layout; no block
                // of it is live.
                unsafe { inner.dealloc(raw, CHUNK_LAYOUT) };
            } else {
                prev = chunk;
            }
            chunk = next;
        }
        collected * PAGE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).expect("layout")
    }

    /// The owner's free list of `bin`, head first.
    fn free_list(heap: &Heap, bin: usize) -> Vec<*mut u8> {
        let mut list = Vec::new();
        let mut p = heap.free[bin].load(Relaxed);
        while !p.is_null() {
            list.push(p);
            // SAFETY: a free block on the owner's list (this thread's).
            p = unsafe { read_next(p) };
        }
        list
    }

    /// `System`, logging the chunks given back to it: the address, and
    /// whether the chunk map still marked it. The map is read before the
    /// chunk is freed, while no other heap can have obtained the address
    /// (tests run in parallel, and `System` reuses freed chunks at once).
    #[derive(Debug, Default)]
    struct ChunkLog {
        given_back: std::sync::Mutex<Vec<(usize, bool)>>,
    }

    impl ChunkLog {
        /// The chunks given back so far, sorted.
        fn chunks_given_back(&self) -> Vec<(usize, bool)> {
            let mut chunks = self.given_back.lock().expect("log").clone();
            chunks.sort_unstable();
            chunks
        }
    }

    // SAFETY: every call is forwarded to `System` unchanged; logging only
    // reads the chunk map and allocates through the test binary's own
    // allocator.
    unsafe impl GlobalAlloc for ChunkLog {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller upholds `alloc`'s contract.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
            if layout == CHUNK_LAYOUT {
                let marked = is_heap_block(p);
                self.given_back
                    .lock()
                    .expect("log")
                    .push((p.addr(), marked));
            }
            // SAFETY: the caller upholds `dealloc`'s contract.
            unsafe { System.dealloc(p, layout) }
        }
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
        assert_eq!(PAGES, 512);
        assert_eq!(USABLE_PAGES, 511);
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
        unmark_chunk(beyond);
        // A real chunk, so that no other allocation can live there; it is
        // leaked on purpose, marked.
        // SAFETY: non-zero size.
        let chunk = unsafe { System.alloc(CHUNK_LAYOUT) };
        assert!(!chunk.is_null());
        assert_eq!(chunk.addr() % CHUNK_SIZE, 0);
        assert!(!is_heap_block(chunk));
        assert!(mark_chunk(chunk));
        assert!(is_heap_block(chunk));
        assert!(is_heap_block(chunk.wrapping_add(CHUNK_SIZE / 2)));
        assert!(is_heap_block(chunk.wrapping_add(CHUNK_SIZE - 1)));
        unmark_chunk(chunk);
        assert!(!is_heap_block(chunk), "given back: no longer a heap block");
        assert!(!is_heap_block(chunk.wrapping_add(CHUNK_SIZE - 1)));
        assert!(mark_chunk(chunk));
        assert!(is_heap_block(chunk));
    }

    #[test]
    fn page_info_encoding_round_trips() {
        for bin in 0..BINS {
            let elements = usize::from(BIN_ELEMENTS[bin]);
            let info = srun(bin, elements);
            assert!(info & IS_SRUN != 0 && info & IS_LRUN == 0);
            assert_eq!((info_bin(info), info_count(info)), (bin, elements));
            assert_eq!(info_count(srun(bin, 0)), 0);
            for offset in 1..usize::from(BIN_PAGES[bin]) {
                let n = nrun(bin, offset);
                assert!(n & IS_SRUN != 0 && n & IS_LRUN != 0);
                assert_eq!((info_bin(n), info_count(n)), (bin, offset));
            }
        }
        let header = lrun(FIRST_PAGE);
        assert!(header & IS_SRUN == 0 && header & IS_LRUN != 0);
        assert_eq!(info_pages(header), FIRST_PAGE);
        assert!(std::mem::size_of::<Chunk>() <= PAGE_SIZE);
    }

    #[test]
    fn find_free_run_scans_across_words() {
        let mut map = [0u64; BITMAP_WORDS];
        map[0] = 1; // the header page
        assert_eq!(find_free_run(&map, 1), Some(1));
        assert_eq!(find_free_run(&map, USABLE_PAGES), Some(1));
        assert_eq!(find_free_run(&map, PAGES), None);
        // Pages 0..=61 in use: a 2-page hole at 62, then word 1 is free.
        map[0] = (1u64 << 62) - 1;
        assert_eq!(find_free_run(&map, 2), Some(62));
        assert_eq!(find_free_run(&map, 3), Some(62), "spans the word boundary");
        // Page 64 in use: 3 pages first fit at 65.
        map[1] = 1;
        assert_eq!(find_free_run(&map, 2), Some(62));
        assert_eq!(find_free_run(&map, 3), Some(65));
        // Everything in use but the last page.
        let mut full = [u64::MAX; BITMAP_WORDS];
        full[BITMAP_WORDS - 1] = u64::MAX >> 1;
        assert_eq!(find_free_run(&full, 1), Some(PAGES - 1));
        assert_eq!(find_free_run(&full, 2), None);
        assert_eq!(find_free_run(&[u64::MAX; BITMAP_WORDS], 1), None);
        // The bitmap helpers agree with the search.
        let mut m = [0u64; BITMAP_WORDS];
        bitmap_set(&mut m, 0, 70);
        assert!(bitmap_is_set(&m, 69) && !bitmap_is_set(&m, 70));
        assert_eq!(find_free_run(&m, 1), Some(70));
        bitmap_clear(&mut m, 60, 5);
        assert!(!bitmap_is_set(&m, 60) && !bitmap_is_set(&m, 64) && bitmap_is_set(&m, 65));
        assert_eq!(find_free_run(&m, 5), Some(60));
        assert_eq!(find_free_run(&m, 6), Some(70));
    }

    #[test]
    fn chunk_pages_keep_an_exact_tail() {
        let mut free_map = [0u64; BITMAP_WORDS];
        free_map[0] = 1; // the header page
        let mut map = [0u32; PAGES];
        map[0] = lrun(FIRST_PAGE);
        let c = Box::into_raw(Box::new(Chunk {
            next: ptr::null_mut(),
            free_pages: u32::try_from(USABLE_PAGES).expect("pages"),
            free_tail: u32::try_from(FIRST_PAGE).expect("page"),
            free_map,
            map,
        }));
        // SAFETY: `c` is a live header owned by this test; the helpers keep
        // no reference to it beyond their call.
        unsafe {
            // Runs A = 1-2, B = 3-5 and C = 6, from the tail.
            assert_eq!(chunk_take(c, 2), Some(1));
            assert_eq!(chunk_take(c, 3), Some(3));
            assert_eq!(chunk_take(c, 1), Some(6));
            assert_eq!(((*c).free_tail, (*c).free_pages), (7, 505));
            // B leaves a hole; releasing C, which ends at the tail, lowers
            // the tail over the hole down to A.
            chunk_release(c, 3, 3);
            assert_eq!((*c).free_tail, 7);
            chunk_release(c, 6, 1);
            assert_eq!(((*c).free_tail, (*c).free_pages), (3, 509));
            // No hole is left: carved from the tail again.
            assert_eq!(chunk_take(c, 4), Some(3));
            // A's two pages become a hole: a 3-page run goes past it, a
            // 1-page run fills it, first fit.
            chunk_release(c, 1, 2);
            assert_eq!(chunk_take(c, 3), Some(7));
            assert_eq!(chunk_take(c, 1), Some(1));
            assert_eq!((*c).free_tail, 10);
            // Back down, one exact step at a time.
            chunk_release(c, 7, 3);
            assert_eq!((*c).free_tail, 7, "page 6 is in use");
            chunk_release(c, 3, 4);
            assert_eq!((*c).free_tail, 2, "over A's free second page");
            assert!((&(*c).map)[3..7].iter().all(|&info| info == 0));
            chunk_release(c, 1, 1);
            assert_eq!(((*c).free_tail, (*c).free_pages), (1, 511));
            assert_eq!(chunk_take(c, USABLE_PAGES), Some(1));
            assert_eq!(chunk_take(c, 1), None, "full");
            drop(Box::from_raw(c));
        }
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
        // The first run starts right after the header page and is handed
        // out in address order, 40 bytes apart.
        assert_eq!(blocks[0].addr() % CHUNK_SIZE, FIRST_PAGE * PAGE_SIZE);
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
        let pages = usize::from(BIN_PAGES[bin]);
        let elements = usize::from(BIN_ELEMENTS[bin]);
        // 511 usable pages per chunk, 4 pages (8 blocks) per run: 127 runs
        // fill a chunk (3 pages stay unused), one more run needs a second.
        let runs = USABLE_PAGES / pages;
        let n = runs * elements;
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let blocks: Vec<*mut u8> = (0..=n)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        assert!(blocks.iter().all(|p| !p.is_null() && is_heap_block(*p)));
        assert_eq!(heap.chunks(), 2);
        assert_eq!(
            blocks[n - 1].addr() - blocks[0].addr(),
            (runs - 1) * pages * PAGE_SIZE + (elements - 1) * 2048
        );
        assert_eq!(blocks[n].addr() % CHUNK_SIZE, FIRST_PAGE * PAGE_SIZE);
        assert_ne!(blocks[n].addr() / CHUNK_SIZE, blocks[0].addr() / CHUNK_SIZE);
        for &p in &blocks {
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, true) };
        }
    }

    #[test]
    fn gc_frees_empty_runs_and_returns_empty_chunks() {
        let heap = Heap::new();
        let log = ChunkLog::default();
        let bin = small_size_to_bin(3072); // 4 elements per 3-page run
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        let pages = usize::from(BIN_PAGES[bin]);
        let runs_per_chunk = USABLE_PAGES / pages;
        // Two chunks' worth of runs plus one run in a third chunk.
        let runs = 2 * runs_per_chunk + 1;
        // SAFETY: this thread owns `heap`, whose chunks all come from
        // `log`; `bin < BINS`.
        let blocks: Vec<*mut u8> = (0..runs * per_run)
            .map(|_| unsafe { heap.alloc(&log, bin) })
            .collect();
        assert!(blocks.iter().all(|p| !p.is_null()));
        assert_eq!(heap.chunks(), 3);
        // The chunk holding run `r`.
        let chunk_of = |r: usize| blocks[r * per_run].addr() & !(CHUNK_SIZE - 1);
        let (first, second, third) = (
            chunk_of(0),
            chunk_of(runs_per_chunk),
            chunk_of(2 * runs_per_chunk),
        );
        assert!(first != second && second != third && first != third);
        // SAFETY: as above. Nothing is free yet, so nothing is collected.
        assert_eq!(unsafe { heap.gc(&log) }, 0);
        assert!(log.chunks_given_back().is_empty());
        // The middle chunk's blocks are freed: its runs come back and the
        // chunk goes back; the list now runs from the first to the third.
        let middle = runs_per_chunk * per_run..2 * runs_per_chunk * per_run;
        for &p in &blocks[middle.clone()] {
            // SAFETY: allocated above, freed once, by the owner.
            unsafe { heap.free(p, bin, true) };
        }
        // SAFETY: as above.
        let freed = unsafe { heap.gc(&log) };
        assert_eq!(freed, runs_per_chunk * pages * PAGE_SIZE);
        assert_eq!(heap.chunks(), 2);
        assert_eq!(
            log.chunks_given_back(),
            vec![(second, false)],
            "unmarked before it went back"
        );
        assert!(free_list(&heap, bin).is_empty());
        // Then everything else: the third chunk goes too, the first stays.
        for (i, &p) in blocks.iter().enumerate() {
            if !middle.contains(&i) {
                // SAFETY: as above.
                unsafe { heap.free(p, bin, true) };
            }
        }
        let rest = runs - runs_per_chunk;
        assert_eq!(free_list(&heap, bin).len(), rest * per_run);
        // SAFETY: as above.
        let freed = unsafe { heap.gc(&log) };
        assert_eq!(freed, rest * pages * PAGE_SIZE);
        assert_eq!(heap.chunks(), 1);
        assert!(free_list(&heap, bin).is_empty(), "no free element left");
        let mut both = vec![(second, false), (third, false)];
        both.sort_unstable();
        assert_eq!(log.chunks_given_back(), both);
        assert!(is_heap_block(blocks[0]), "the first chunk is kept");
        // The first chunk's pages are reused from the start (its tail came
        // down to the first page).
        // SAFETY: as above.
        let again = unsafe { heap.alloc(&log, bin) };
        assert_eq!(again, blocks[0]);
        assert_eq!(free_list(&heap, bin).len(), per_run - 1);
        // SAFETY: as above.
        assert_eq!(unsafe { heap.gc(&log) }, 0, "a run in use is kept");
        assert_eq!(free_list(&heap, bin).len(), per_run - 1);
        // Once the first chunk is full, a new chunk is appended to it (the
        // list's end moved back to the first chunk).
        // SAFETY: as above.
        let more: Vec<*mut u8> = (0..runs_per_chunk * per_run)
            .map(|_| unsafe { heap.alloc(&log, bin) })
            .collect();
        assert_eq!(heap.chunks(), 2);
        for &p in more.iter().chain([again].iter()) {
            // SAFETY: allocated above, freed once, by the owner.
            unsafe { heap.free(p, bin, true) };
        }
        // SAFETY: as above.
        let freed = unsafe { heap.gc(&log) };
        assert_eq!(freed, (runs_per_chunk + 1) * pages * PAGE_SIZE);
        assert_eq!(heap.chunks(), 1, "the first chunk is never given back");
        assert_eq!(log.chunks_given_back().len(), 3);
    }

    #[test]
    fn gc_keeps_runs_with_live_blocks() {
        let heap = Heap::new();
        let bin = small_size_to_bin(1024); // 8 elements per 2-page run
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let first: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        // SAFETY: as above.
        let second: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        // All of the second run and all but one block of the first are
        // freed.
        for &p in second.iter().chain(&first[1..]) {
            // SAFETY: allocated above, freed once, by the owner.
            unsafe { heap.free(p, bin, true) };
        }
        // SAFETY: this thread owns `heap`.
        assert_eq!(unsafe { heap.gc(&System) }, 2 * PAGE_SIZE);
        // The first run's free blocks are still on the list, LIFO.
        let list = free_list(&heap, bin);
        assert_eq!(list.len(), per_run - 1);
        assert!(list.iter().all(|p| first[1..].contains(p)));
        assert_eq!(list[0], first[per_run - 1]);
        // The second run's pages are free again: the next 2-page run of a
        // different bin lands there, before the never-used tail.
        let other = small_size_to_bin(896);
        assert_eq!(BIN_PAGES[other], 2);
        // SAFETY: as above.
        let q = unsafe { heap.alloc(&System, other) };
        assert_eq!(q, second[0]);
        assert_eq!(heap.chunks(), 1);
        // SAFETY: allocated above, freed once.
        unsafe {
            heap.free(q, other, true);
            heap.free(first[0], bin, true);
        }
        // SAFETY: as above.
        assert_eq!(unsafe { heap.gc(&System) }, 4 * PAGE_SIZE);
        assert!(free_list(&heap, bin).is_empty());
        assert!(free_list(&heap, other).is_empty());
    }

    #[test]
    fn freed_pages_are_reused_first_fit_by_any_bin() {
        let heap = Heap::new();
        let bin = small_size_to_bin(1024); // 2-page runs
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let runs: Vec<Vec<*mut u8>> = (0..3)
            .map(|_| {
                (0..per_run)
                    .map(|_| unsafe { heap.alloc(&System, bin) })
                    .collect()
            })
            .collect();
        for &p in &runs[1] {
            // SAFETY: allocated above, freed once, by the owner.
            unsafe { heap.free(p, bin, true) };
        }
        // SAFETY: this thread owns `heap`.
        assert_eq!(unsafe { heap.gc(&System) }, 2 * PAGE_SIZE);
        // A 1-page run of another bin takes the hole's first page; a
        // 2-page run does not fit the page left and goes to the tail; the
        // next 1-page run takes the hole's second page.
        let one = small_size_to_bin(40);
        assert_eq!(BIN_PAGES[one], 1);
        // SAFETY: as above.
        let a = unsafe { heap.alloc(&System, one) };
        assert_eq!(a, runs[1][0]);
        // SAFETY: as above (the bin's list is empty: its runs are in use
        // or were unlinked by gc).
        let b = unsafe { heap.alloc(&System, bin) };
        assert_eq!(b.addr(), runs[2][0].addr() + 2 * PAGE_SIZE);
        let other_one = small_size_to_bin(56);
        assert_eq!(BIN_PAGES[other_one], 1);
        // SAFETY: as above.
        let c = unsafe { heap.alloc(&System, other_one) };
        assert_eq!(c.addr(), runs[1][0].addr() + PAGE_SIZE);
        assert_eq!(heap.chunks(), 1);
    }

    #[test]
    fn gc_counts_remote_frees() {
        let heap = Heap::new();
        let bin = small_size_to_bin(512); // 8 elements per page
        let per_run = usize::from(BIN_ELEMENTS[bin]);
        // SAFETY: this thread owns `heap`; `bin < BINS`.
        let run: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        for (i, &p) in run.iter().enumerate() {
            // Half "by another thread": onto the remote list.
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, i % 2 == 0) };
        }
        assert_eq!(free_list(&heap, bin).len(), per_run / 2);
        // SAFETY: this thread owns `heap`.
        assert_eq!(unsafe { heap.gc(&System) }, PAGE_SIZE);
        assert!(free_list(&heap, bin).is_empty());
        assert!(heap.remote[bin].load(Relaxed).is_null());
        // Remote frees of a run that stays in use join the owner's list.
        // SAFETY: as above.
        let run: Vec<*mut u8> = (0..per_run)
            .map(|_| unsafe { heap.alloc(&System, bin) })
            .collect();
        for &p in &run[1..] {
            // SAFETY: allocated above, freed once.
            unsafe { heap.free(p, bin, false) };
        }
        // SAFETY: as above.
        assert_eq!(unsafe { heap.gc(&System) }, 0);
        let list = free_list(&heap, bin);
        assert_eq!(list.len(), per_run - 1);
        assert!(list.iter().all(|p| run[1..].contains(p)));
        assert!(heap.remote[bin].load(Relaxed).is_null());
        // SAFETY: as above.
        let again = unsafe { heap.alloc(&System, bin) };
        assert!(run[1..].contains(&again), "taken from the spliced list");
    }
}
