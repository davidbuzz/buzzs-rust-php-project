//! `ZStr`: PHP's refcounted, immutable, binary-safe byte string (`zend_string`).
//!
//! A [`ZStr`] is a cheap-to-clone handle (a single thin pointer) to an immutable
//! byte buffer together with a lazily computed, cached hash (PHP's DJB "times 33"
//! hash, `zend_inline_hash_func`). Strings can be *interned*: interning returns the
//! canonical instance for a given byte sequence so that equality checks between
//! interned strings reduce to a pointer comparison.
//!
//! Strings are immutable from the outside. The only mutation offered is
//! [`ZStr::append`], which extends the buffer in place when the handle is the
//! sole owner (mirroring `zend_string_extend` on a refcount-1 string) and
//! otherwise allocates a new string.
//!
//! # Layout (ref: `Zend/zend_types.h` `struct _zend_string`)
//!
//! Like `zend_string`, a string is ONE allocation: a 40-byte header
//! (refcount, cached hash, data pointer, length, capacity + flags) followed
//! by the bytes, so a short string costs `40 + len` bytes instead of an `Rc`
//! box plus a separate `Vec` buffer (56 + len). Byte vectors longer than
//! [`ADOPT_MIN`] handed to [`ZStr::new`] are adopted instead of copied (the
//! header then points at the vector's buffer), so building a large string
//! never needs two copies of it at once. The data pointer is stored in both
//! cases, so reading the bytes never branches on the representation.

use std::alloc::{self, Layout};
use std::borrow::Borrow;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};

/// `Header::meta` flag: the instance lives in the intern table.
const INTERNED: usize = 1 << (usize::BITS - 1);
/// `Header::meta` flag: the bytes live in an adopted `Vec` buffer (`ptr`,
/// `len`, capacity) rather than inline after the header.
const EXTERNAL: usize = 1 << (usize::BITS - 2);
/// `Header::meta` bits holding the capacity.
const CAP_MASK: usize = EXTERNAL - 1;
/// Byte vectors longer than this are adopted by [`ZStr::new`]; shorter ones
/// are copied into the header block (dropping any spare capacity, like
/// `smart_str_extract`).
const ADOPT_MIN: usize = 1024;

/// The shared header of a [`ZStr`], followed in the same allocation by the
/// inline bytes (`EXTERNAL` clear) or by nothing (`EXTERNAL` set).
#[repr(C)]
struct Header {
    /// Number of live handles (`GC_REFCOUNT`).
    rc: Cell<usize>,
    /// Cached `zend_inline_hash_func` value; `0` means "not computed yet"
    /// (a real hash always has its top bit set, so it is never `0`).
    hash: Cell<u64>,
    /// The first byte: just past the header when inline, the adopted
    /// vector's buffer when external.
    ptr: *mut u8,
    /// Length in bytes.
    len: usize,
    /// Capacity in bytes (inline: of the block after the header; external:
    /// of the vector) in `CAP_MASK`, plus the `INTERNED` / `EXTERNAL` flags.
    meta: usize,
}

/// Size of [`Header`]; the inline bytes start at this offset.
const HEADER: usize = std::mem::size_of::<Header>();

/// A refcounted, immutable, binary-safe PHP string.
///
/// Cloning is O(1). Equality, ordering and hashing are by byte content
/// (`memcmp` semantics), never by identity.
///
/// The handle is a thin pointer (8 bytes, with a niche for `Option`). Like
/// `Rc`, it is neither `Send` nor `Sync`.
pub struct ZStr(NonNull<Header>);

thread_local! {
    static INTERNED_SET: RefCell<HashSet<ZStr>> = RefCell::new(HashSet::new());
    static EMPTY: ZStr = ZStr::intern(b"");
    static CHARS: Vec<ZStr> = (0..=255u8).map(|c| ZStr::intern(&[c])).collect();
}

/// Computes PHP's string hash (`zend_inline_hash_func`, 64-bit variant).
///
/// The result always has its most significant bit set, so it is never zero.
#[must_use]
pub fn zend_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in bytes {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(b));
    }
    hash | 0x8000_0000_0000_0000
}

/// Bytes needed for the decimal form of any `i64` (`MAX_LENGTH_OF_LONG - 1`:
/// 19 digits of `i64::MIN` plus its sign).
pub const LONG_MAX_LEN: usize = 20;

/// `"00" "01" … "99"`: two digits per division step.
const DIGIT_PAIRS: &[u8; 200] = b"\
0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Writes the decimal form of `n` at the end of `buf` and returns it
/// (`zend_print_long_to_buf`), two digits per step.
#[inline]
pub fn format_long(n: i64, buf: &mut [u8; LONG_MAX_LEN]) -> &[u8] {
    let mut pos = buf.len();
    let mut m = n.unsigned_abs();
    while m >= 100 {
        #[allow(clippy::cast_possible_truncation)]
        let pair = (m % 100) as usize * 2;
        m /= 100;
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    }
    #[allow(clippy::cast_possible_truncation)]
    let m = m as usize;
    if m >= 10 {
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[m * 2..m * 2 + 2]);
    } else {
        pos -= 1;
        #[allow(clippy::cast_possible_truncation)]
        let digit = m as u8;
        buf[pos] = b'0' + digit;
    }
    if n < 0 {
        pos -= 1;
        buf[pos] = b'-';
    }
    &buf[pos..]
}

/// Length of the decimal form of `n` (what [`format_long`] writes).
#[inline]
#[must_use]
pub fn long_str_len(n: i64) -> usize {
    let mut m = n.unsigned_abs();
    let mut len = usize::from(n < 0) + 1;
    while m >= 10 {
        m /= 10;
        len += 1;
    }
    len
}

#[cold]
#[inline(never)]
fn capacity_overflow() -> ! {
    panic!("string capacity overflow")
}

/// The layout of a header block with `inline_cap` bytes after the header.
#[inline]
fn block_layout(inline_cap: usize) -> Layout {
    let size = HEADER
        .checked_add(inline_cap)
        .unwrap_or_else(|| capacity_overflow());
    Layout::from_size_align(size, std::mem::align_of::<Header>())
        .unwrap_or_else(|_| capacity_overflow())
}

/// Allocates an uninitialised header block with room for `inline_cap`
/// bytes after the header (`zend_string_alloc`).
#[inline]
fn alloc_block(inline_cap: usize) -> NonNull<Header> {
    let layout = block_layout(inline_cap);
    // SAFETY: the layout's size is at least `HEADER` (> 0).
    let raw = unsafe { alloc::alloc(layout) };
    // The allocation is aligned for `Header` (it was requested so).
    #[allow(clippy::cast_ptr_alignment)]
    let raw = raw.cast::<Header>();
    match NonNull::new(raw) {
        Some(block) => block,
        None => alloc::handle_alloc_error(layout),
    }
}

/// Releases a block whose last handle is gone (`zend_string_free`).
///
/// # Safety
/// `block` must be a live header block with no remaining handles.
#[inline(never)]
unsafe fn free_block(block: NonNull<Header>) {
    // SAFETY: the caller guarantees the block is live.
    let (data, len, meta) = unsafe {
        let h = block.as_ref();
        (h.ptr, h.len, h.meta)
    };
    let inline_cap = if meta & EXTERNAL == 0 {
        meta & CAP_MASK
    } else {
        // SAFETY: an external header owns the `Vec` buffer it was built
        // from (see `ZStr::adopt`), with exactly these parts.
        drop(unsafe { Vec::from_raw_parts(data, len, meta & CAP_MASK) });
        0
    };
    // SAFETY: the block was allocated by `alloc_block(inline_cap)` (or
    // reallocated to that layout by `ZStr::extend_unique`).
    unsafe { alloc::dealloc(block.as_ptr().cast::<u8>(), block_layout(inline_cap)) };
}

impl ZStr {
    /// Creates a new (non-interned) string from the given bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self::from_vec(bytes.into())
    }

    /// A string holding a copy of `bytes`, in one allocation.
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        Self::from_parts(bytes, &[], 0)
    }

    /// A string holding `a` followed by `b`, in one exactly sized
    /// allocation, with the given `meta` flags.
    fn from_parts(a: &[u8], b: &[u8], flags: usize) -> Self {
        let len = a
            .len()
            .checked_add(b.len())
            .filter(|&n| n <= CAP_MASK)
            .unwrap_or_else(|| capacity_overflow());
        let block = alloc_block(len);
        // SAFETY: `block` is a fresh allocation of `HEADER + len` bytes,
        // aligned for `Header`; the inline area starts right after the
        // header and has room for exactly `len` bytes, and the sources are
        // other allocations.
        unsafe {
            let data = block.as_ptr().cast::<u8>().add(HEADER);
            ptr::copy_nonoverlapping(a.as_ptr(), data, a.len());
            ptr::copy_nonoverlapping(b.as_ptr(), data.add(a.len()), b.len());
            block.as_ptr().write(Header {
                rc: Cell::new(1),
                hash: Cell::new(0),
                ptr: data,
                len,
                meta: len | flags,
            });
        }
        ZStr(block)
    }

    /// An empty, unique string with room for `cap` inline bytes
    /// (`zend_string_alloc(cap)` with the length still 0): the start of a
    /// [`ZStrBuf`].
    fn with_inline_capacity(cap: usize) -> Self {
        if cap > CAP_MASK {
            capacity_overflow();
        }
        let block = alloc_block(cap);
        // SAFETY: `block` is a fresh allocation of `HEADER + cap` bytes,
        // aligned for `Header`; the (uninitialised) inline area starts
        // right after the header and no byte of it is readable yet
        // (`len` is 0).
        unsafe {
            let data = block.as_ptr().cast::<u8>().add(HEADER);
            block.as_ptr().write(Header {
                rc: Cell::new(1),
                hash: Cell::new(0),
                ptr: data,
                len: 0,
                meta: cap,
            });
        }
        ZStr(block)
    }

    /// A string over a byte vector: copied inline when short, adopted
    /// otherwise.
    fn from_vec(v: Vec<u8>) -> Self {
        if v.len() <= ADOPT_MIN {
            return Self::from_slice(&v);
        }
        Self::adopt(v)
    }

    /// A string whose bytes are `v`'s buffer (no copy).
    fn adopt(v: Vec<u8>) -> Self {
        let mut v = ManuallyDrop::new(v);
        let (data, len, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
        if cap > CAP_MASK {
            capacity_overflow();
        }
        let block = alloc_block(0);
        // SAFETY: `block` is a fresh header-sized allocation aligned for
        // `Header`; the header takes ownership of the vector's buffer, which
        // `free_block` / `take_vec` rebuild from exactly these parts.
        unsafe {
            block.as_ptr().write(Header {
                rc: Cell::new(1),
                hash: Cell::new(0),
                ptr: data,
                len,
                meta: cap | EXTERNAL,
            });
        }
        ZStr(block)
    }

    #[inline]
    fn header(&self) -> &Header {
        // SAFETY: a handle keeps its block alive (refcount >= 1).
        unsafe { self.0.as_ref() }
    }

    /// Whether this handle may mutate the string in place: it is the only
    /// handle and the string is not interned (`!ZSTR_IS_INTERNED &&
    /// GC_REFCOUNT == 1`).
    #[inline]
    fn is_unique_mutable(&self) -> bool {
        let h = self.header();
        h.rc.get() == 1 && h.meta & INTERNED == 0
    }

    /// Takes the adopted vector out of a unique external string, leaving
    /// it empty (`len` 0, capacity 0: valid to drop).
    ///
    /// # Safety
    /// `self` must be unique (refcount 1) and external.
    unsafe fn take_vec(&mut self) -> Vec<u8> {
        // SAFETY: unique handle, so no other reference to the header exists.
        let h = unsafe { self.0.as_mut() };
        // SAFETY: an external header owns a `Vec` with exactly these parts.
        let v = unsafe { Vec::from_raw_parts(h.ptr, h.len, h.meta & CAP_MASK) };
        h.ptr = NonNull::<u8>::dangling().as_ptr();
        h.len = 0;
        h.meta = (h.meta & !CAP_MASK) | EXTERNAL;
        v
    }

    /// Hands `v` back to a unique external string emptied by `take_vec`.
    ///
    /// # Safety
    /// As for [`ZStr::take_vec`].
    unsafe fn put_vec(&mut self, v: Vec<u8>) {
        let mut v = ManuallyDrop::new(v);
        if v.capacity() > CAP_MASK {
            capacity_overflow();
        }
        // SAFETY: unique handle, so no other reference to the header exists.
        let h = unsafe { self.0.as_mut() };
        h.ptr = v.as_mut_ptr();
        h.len = v.len();
        h.meta = (h.meta & !CAP_MASK) | v.capacity();
    }

    /// Appends `other` in place to a unique, non-interned string
    /// (`zend_string_extend`): to exactly the new length with `exact`,
    /// else with amortized doubling (repeated `.=`).
    fn extend_unique(&mut self, other: &[u8], exact: bool) {
        debug_assert!(self.is_unique_mutable());
        let (len, meta) = {
            let h = self.header();
            (h.len, h.meta)
        };
        if meta & EXTERNAL != 0 {
            // SAFETY: unique (caller) and external (just checked).
            let mut v = unsafe { self.take_vec() };
            if exact {
                v.reserve_exact(other.len());
            }
            v.extend_from_slice(other);
            // SAFETY: as above; emptied by `take_vec`.
            unsafe { self.put_vec(v) };
        } else {
            let needed = len
                .checked_add(other.len())
                .filter(|&n| n <= CAP_MASK)
                .unwrap_or_else(|| capacity_overflow());
            let cap = meta & CAP_MASK;
            if needed > cap {
                let new_cap = if exact {
                    needed
                } else {
                    let grown = cap.saturating_mul(2).clamp(16, CAP_MASK);
                    needed.max(grown)
                };
                let new_layout = block_layout(new_cap);
                // SAFETY: the block was allocated with `block_layout(cap)`;
                // the new size is non-zero. The handle is unique, so moving
                // the block invalidates no other pointer to it.
                let raw = unsafe {
                    alloc::realloc(
                        self.0.as_ptr().cast::<u8>(),
                        block_layout(cap),
                        new_layout.size(),
                    )
                };
                // `realloc` keeps the layout's (Header) alignment.
                #[allow(clippy::cast_ptr_alignment)]
                let Some(block) = NonNull::new(raw.cast::<Header>()) else {
                    alloc::handle_alloc_error(new_layout)
                };
                self.0 = block;
                // SAFETY: unique handle; the inline area follows the header.
                unsafe {
                    let h = self.0.as_mut();
                    h.ptr = block.as_ptr().cast::<u8>().add(HEADER);
                    h.meta = (meta & !CAP_MASK) | new_cap;
                }
            }
            // SAFETY: unique handle; the inline area has room for `needed`
            // bytes and `other` cannot alias it (it would need a second
            // handle or a shared borrow of `self`).
            unsafe {
                let h = self.0.as_mut();
                ptr::copy_nonoverlapping(other.as_ptr(), h.ptr.add(len), other.len());
                h.len = needed;
            }
        }
        self.header().hash.set(0);
    }

    /// Returns the canonical interned instance for `bytes`, creating it if needed.
    ///
    /// Interned strings live for the remainder of the thread.
    #[must_use]
    pub fn intern(bytes: &[u8]) -> Self {
        INTERNED_SET.with(|set| {
            if let Some(existing) = set.borrow().get(bytes) {
                return existing.clone();
            }
            let s = Self::from_parts(bytes, &[], INTERNED);
            set.borrow_mut().insert(s.clone());
            s
        })
    }

    /// Returns the interned version of this string (itself if already interned).
    #[must_use]
    pub fn interned(&self) -> Self {
        if self.is_interned() {
            self.clone()
        } else {
            Self::intern(self.as_bytes())
        }
    }

    /// The shared empty string (`ZSTR_EMPTY_ALLOC()`).
    #[must_use]
    pub fn empty() -> Self {
        EMPTY.with(Clone::clone)
    }

    /// The shared one-character string for byte `c` (`ZSTR_CHAR(c)`).
    #[must_use]
    pub fn from_byte(c: u8) -> Self {
        CHARS.with(|chars| chars[usize::from(c)].clone())
    }

    /// Returns the bytes of the string.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        let h = self.header();
        // SAFETY: `ptr` is valid for `len` initialised bytes for as long as
        // the block lives, and mutation needs `&mut self` on a unique handle.
        unsafe { std::slice::from_raw_parts(h.ptr, h.len) }
    }

    /// Length in bytes.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.header().len
    }

    /// Whether the string is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.header().len == 0
    }

    /// Whether this instance is interned.
    #[must_use]
    pub fn is_interned(&self) -> bool {
        self.header().meta & INTERNED != 0
    }

    /// Returns the (cached) PHP hash of the string.
    #[inline]
    #[must_use]
    pub fn hash_value(&self) -> u64 {
        let h = self.header().hash.get();
        if h != 0 {
            return h;
        }
        let h = zend_hash_bytes(self.as_bytes());
        self.header().hash.set(h);
        h
    }

    /// Whether two handles point at the very same string instance.
    #[inline]
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.0 == b.0
    }

    /// Number of live handles to this string instance (the "refcount").
    #[must_use]
    pub fn refcount(&self) -> usize {
        self.header().rc.get()
    }

    /// Returns the string as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(self.as_bytes()).into_owned()
    }

    /// Returns the string as `&str` if it is valid UTF-8.
    #[must_use]
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }

    /// Appends `other` to this string.
    ///
    /// When this handle is the only owner (and the string is not interned) the
    /// buffer is extended in place, like `zend_string_extend` on a refcount-1
    /// string, which makes repeated `.=` amortized O(1). Otherwise a new string
    /// is allocated.
    pub fn append(&mut self, other: &[u8]) {
        if other.is_empty() {
            return;
        }
        if self.is_unique_mutable() {
            self.extend_unique(other, false);
            return;
        }
        *self = Self::from_parts(self.as_bytes(), other, 0);
    }

    /// Appends the decimal form of `n` (`concat_function` with an integer
    /// right operand, `smart_str_append_long`), extending in place like
    /// [`ZStr::append`] without materialising the number as a string.
    pub fn append_long(&mut self, n: i64) {
        let mut buf = [0u8; LONG_MAX_LEN];
        self.append(format_long(n, &mut buf));
    }

    /// `ZEND_CONCAT` with a temporary left operand: when this handle is the
    /// only owner (and the string is not interned) the buffer is extended
    /// in place to exactly the new length (`zend_string_extend`), so the
    /// operand and the result never exist side by side; otherwise a new
    /// exactly sized string is built.
    #[must_use]
    pub fn concat_owned(mut self, other: &[u8]) -> Self {
        if self.is_unique_mutable() {
            if !other.is_empty() {
                self.extend_unique(other, true);
            }
            return self;
        }
        Self::concat(self.as_bytes(), other)
    }

    /// Concatenates two byte slices into a new string.
    #[must_use]
    pub fn concat(a: &[u8], b: &[u8]) -> Self {
        Self::from_parts(a, b, 0)
    }

    /// Returns an ASCII-lowercased copy (`zend_string_tolower`); returns a clone
    /// of `self` if nothing changes.
    #[must_use]
    pub fn to_ascii_lowercase(&self) -> Self {
        if self.as_bytes().iter().any(u8::is_ascii_uppercase) {
            ZStr::new(self.as_bytes().to_ascii_lowercase())
        } else {
            self.clone()
        }
    }

    /// Returns an ASCII-uppercased copy (`zend_string_toupper`); returns a clone
    /// of `self` if nothing changes.
    #[must_use]
    pub fn to_ascii_uppercase(&self) -> Self {
        if self.as_bytes().iter().any(u8::is_ascii_lowercase) {
            ZStr::new(self.as_bytes().to_ascii_uppercase())
        } else {
            self.clone()
        }
    }

    /// Case-insensitive (ASCII) equality.
    #[must_use]
    pub fn eq_ignore_ascii_case(&self, other: &[u8]) -> bool {
        self.as_bytes().eq_ignore_ascii_case(other)
    }

    /// Returns the bytes as an owned vector (without copying when this is
    /// the only handle to an adopted vector).
    #[must_use]
    pub fn into_bytes(mut self) -> Vec<u8> {
        if self.is_unique_mutable() && self.header().meta & EXTERNAL != 0 {
            // SAFETY: unique and external (just checked); `self` is then
            // dropped with an empty external header.
            return unsafe { self.take_vec() };
        }
        self.as_bytes().to_vec()
    }
}

impl Clone for ZStr {
    #[inline]
    fn clone(&self) -> Self {
        let rc = &self.header().rc;
        let n = rc.get().wrapping_add(1);
        if n == 0 {
            // Like `Rc`: an overflowing count would later free a live block.
            std::process::abort();
        }
        rc.set(n);
        ZStr(self.0)
    }
}

impl Drop for ZStr {
    #[inline]
    fn drop(&mut self) {
        let rc = &self.header().rc;
        let n = rc.get() - 1;
        if n != 0 {
            rc.set(n);
            return;
        }
        // SAFETY: this was the last handle to a live block.
        unsafe { free_block(self.0) };
    }
}

/// A string under construction (`smart_str`): bytes are appended into
/// the inline area of one uniquely owned string block, which grows by
/// doubling when a piece does not fit, and [`ZStrBuf::finish`] hands the
/// block over as a [`ZStr`] without copying. Reserving the exact final
/// length up front (`with_capacity`) gives one exactly sized allocation,
/// like php-src's `zend_string_alloc(len)` + `memcpy` in `ZEND_ROPE_END`.
#[derive(Debug)]
pub struct ZStrBuf(ZStr);

impl ZStrBuf {
    /// An empty buffer with room for `cap` bytes.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        ZStrBuf(ZStr::with_inline_capacity(cap))
    }

    /// Appends bytes (`smart_str_appendl`).
    #[inline]
    pub fn push(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.0.extend_unique(bytes, false);
        }
    }

    /// Appends the decimal form of `n` (`smart_str_append_long`).
    #[inline]
    pub fn push_long(&mut self, n: i64) {
        let mut buf = [0u8; LONG_MAX_LEN];
        self.push(format_long(n, &mut buf));
    }

    /// Bytes appended so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing was appended yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The bytes appended so far.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// The finished string (`smart_str_extract`); spare capacity, if any,
    /// stays with it for later in-place appends.
    #[must_use]
    pub fn finish(self) -> ZStr {
        self.0
    }
}

/// `zend_binary_strcmp`: `memcmp` over the common prefix, then length.
///
/// Returns the raw `memcmp` difference (or the length comparison as -1/0/1),
/// like PHP's `strcmp()`.
#[must_use]
pub fn binary_strcmp(a: &[u8], b: &[u8]) -> i32 {
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            return i32::from(a[i]) - i32::from(b[i]);
        }
    }
    three_way(a.len(), b.len())
}

/// `zend_binary_strncmp`.
#[must_use]
pub fn binary_strncmp(a: &[u8], b: &[u8], length: usize) -> i32 {
    let n = length.min(a.len()).min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            return i32::from(a[i]) - i32::from(b[i]);
        }
    }
    three_way(length.min(a.len()), length.min(b.len()))
}

/// `zend_binary_strcasecmp` (ASCII case folding).
#[must_use]
pub fn binary_strcasecmp(a: &[u8], b: &[u8]) -> i32 {
    let n = a.len().min(b.len());
    for i in 0..n {
        let c1 = a[i].to_ascii_lowercase();
        let c2 = b[i].to_ascii_lowercase();
        if c1 != c2 {
            return i32::from(c1) - i32::from(c2);
        }
    }
    three_way(a.len(), b.len())
}

/// `zend_binary_strncasecmp` (ASCII case folding).
#[must_use]
pub fn binary_strncasecmp(a: &[u8], b: &[u8], length: usize) -> i32 {
    let n = length.min(a.len()).min(b.len());
    for i in 0..n {
        let c1 = a[i].to_ascii_lowercase();
        let c2 = b[i].to_ascii_lowercase();
        if c1 != c2 {
            return i32::from(c1) - i32::from(c2);
        }
    }
    three_way(length.min(a.len()), length.min(b.len()))
}

fn three_way(a: usize, b: usize) -> i32 {
    match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

impl PartialEq for ZStr {
    fn eq(&self, other: &Self) -> bool {
        if self.0 == other.0 {
            return true;
        }
        let (a, b) = (self.header(), other.header());
        if a.meta & b.meta & INTERNED != 0 {
            return false;
        }
        let (h1, h2) = (a.hash.get(), b.hash.get());
        if h1 != 0 && h2 != 0 && h1 != h2 {
            return false;
        }
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for ZStr {}

impl PartialEq<[u8]> for ZStr {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_bytes() == other
    }
}

impl PartialEq<&[u8]> for ZStr {
    fn eq(&self, other: &&[u8]) -> bool {
        self.as_bytes() == *other
    }
}

impl PartialEq<str> for ZStr {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for ZStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialOrd for ZStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ZStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl Hash for ZStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Must agree with `<[u8] as Hash>` so that `Borrow<[u8]>` lookups work.
        self.as_bytes().hash(state);
    }
}

impl Borrow<[u8]> for ZStr {
    fn borrow(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl AsRef<[u8]> for ZStr {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl std::ops::Deref for ZStr {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl From<&str> for ZStr {
    fn from(s: &str) -> Self {
        ZStr::from_slice(s.as_bytes())
    }
}

impl From<String> for ZStr {
    fn from(s: String) -> Self {
        ZStr::from_vec(s.into_bytes())
    }
}

impl From<&[u8]> for ZStr {
    fn from(s: &[u8]) -> Self {
        ZStr::from_slice(s)
    }
}

impl From<Vec<u8>> for ZStr {
    fn from(s: Vec<u8>) -> Self {
        ZStr::from_vec(s)
    }
}

impl<const N: usize> From<&[u8; N]> for ZStr {
    fn from(s: &[u8; N]) -> Self {
        ZStr::from_slice(&s[..])
    }
}

impl Default for ZStr {
    fn default() -> Self {
        ZStr::empty()
    }
}

impl fmt::Debug for ZStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_string_lossy())
    }
}

impl fmt::Display for ZStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_properties() {
        let s = ZStr::from("hello");
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        assert_eq!(s.as_bytes(), b"hello");
        assert_eq!(s, ZStr::from(b"hello".to_vec()));
        assert!(ZStr::empty().is_empty());
        assert_eq!(ZStr::from_byte(b'a'), "a");
    }

    #[test]
    fn binary_safe() {
        let s = ZStr::new(vec![0u8, 1, 0, 255]);
        assert_eq!(s.len(), 4);
        assert_eq!(s.as_bytes(), &[0, 1, 0, 255]);
    }

    #[test]
    fn hash_matches_zend() {
        // zend_inline_hash_func("") == 5381 | 1<<63
        assert_eq!(zend_hash_bytes(b""), 0x1505 | 0x8000_0000_0000_0000);
        assert_eq!(
            zend_hash_bytes(b"a"),
            (5381u64 * 33 + u64::from(b'a')) | 0x8000_0000_0000_0000
        );
        let s = ZStr::from("abc");
        assert_eq!(s.hash_value(), zend_hash_bytes(b"abc"));
        // cached
        assert_eq!(s.hash_value(), zend_hash_bytes(b"abc"));
    }

    #[test]
    fn interning() {
        let a = ZStr::intern(b"foo");
        let b = ZStr::intern(b"foo");
        assert!(ZStr::ptr_eq(&a, &b));
        assert!(a.is_interned());
        let c = ZStr::from("foo");
        assert!(!c.is_interned());
        assert_eq!(a, c);
        assert!(ZStr::ptr_eq(&c.interned(), &a));
        assert_ne!(ZStr::intern(b"bar"), a);
    }

    #[test]
    fn append_in_place_and_cow() {
        let mut s = ZStr::from("ab");
        s.append(b"cd");
        assert_eq!(s, "abcd");
        let t = s.clone();
        s.append(b"e");
        assert_eq!(s, "abcde");
        assert_eq!(t, "abcd");
        let mut i = ZStr::intern(b"x");
        i.append(b"y");
        assert_eq!(i, "xy");
        assert_eq!(ZStr::intern(b"x"), "x");
        // hash invalidated after append
        let mut h = ZStr::from("a");
        let _ = h.hash_value();
        h.append(b"b");
        assert_eq!(h.hash_value(), zend_hash_bytes(b"ab"));
    }

    #[test]
    fn thin_handle_layout() {
        assert_eq!(std::mem::size_of::<ZStr>(), 8);
        assert_eq!(std::mem::size_of::<Option<ZStr>>(), 8);
        assert_eq!(HEADER, 40);
    }

    #[test]
    fn refcount_and_drop() {
        let a = ZStr::from("abc");
        assert_eq!(a.refcount(), 1);
        let b = a.clone();
        assert_eq!(a.refcount(), 2);
        assert!(ZStr::ptr_eq(&a, &b));
        drop(b);
        assert_eq!(a.refcount(), 1);
        // A shared string is never extended in place.
        let mut c = a.clone();
        c.append(b"d");
        assert_eq!(a, "abc");
        assert_eq!(c, "abcd");
        assert!(!ZStr::ptr_eq(&a, &c));
    }

    #[test]
    fn inline_append_grows_and_keeps_bytes() {
        let mut s = ZStr::from("x");
        let mut expected = b"x".to_vec();
        for i in 0..2000u32 {
            let piece = i.to_string();
            s.append(piece.as_bytes());
            expected.extend_from_slice(piece.as_bytes());
            assert_eq!(s.len(), expected.len());
        }
        assert_eq!(s.as_bytes(), &expected[..]);
        assert_eq!(s.hash_value(), zend_hash_bytes(&expected));
        assert_eq!(s.clone().into_bytes(), expected);
        assert_eq!(s.into_bytes(), expected);
    }

    #[test]
    fn adopted_vectors() {
        let big = vec![b'z'; ADOPT_MIN + 10];
        let data_ptr = big.as_ptr();
        let mut s = ZStr::new(big);
        assert_eq!(s.len(), ADOPT_MIN + 10);
        assert_eq!(
            s.as_bytes().as_ptr(),
            data_ptr,
            "long vectors are not copied"
        );
        s.append(b"tail");
        assert!(s.as_bytes().ends_with(b"zztail"));
        let t = s.clone().concat_owned(b"!");
        assert!(t.as_bytes().ends_with(b"tail!"));
        assert!(s.as_bytes().ends_with(b"tail"), "shared operand untouched");
        drop(t);
        let u = s.concat_owned(b"?");
        assert_eq!(u.len(), ADOPT_MIN + 15);
        let v = u.into_bytes();
        assert_eq!(v.len(), ADOPT_MIN + 15);
        assert_eq!(v.last(), Some(&b'?'));
    }

    #[test]
    fn concat_owned_in_place_and_empty() {
        let s = ZStr::from("ab");
        let s = s.concat_owned(b"");
        assert_eq!(s, "ab");
        let s = s.concat_owned(b"cd");
        assert_eq!(s, "abcd");
        assert_eq!(ZStr::concat(b"", b""), ZStr::empty());
        assert!(ZStr::from(Vec::new()).is_empty());
        assert_eq!(ZStr::from(String::from("hi")), "hi");
        let short = Vec::with_capacity(500);
        assert!(ZStr::new(short).is_empty());
    }

    #[test]
    fn interned_strings_are_not_mutated() {
        let mut a = ZStr::intern(b"keep");
        let b = a.clone();
        a.append(b"!");
        assert_eq!(b, "keep");
        assert!(ZStr::intern(b"keep").is_interned());
        let c = ZStr::intern(b"keep").concat_owned(b"?");
        assert_eq!(c, "keep?");
        assert!(!c.is_interned());
        assert_eq!(ZStr::intern(b"keep"), "keep");
    }

    #[test]
    fn strcmp_family() {
        assert_eq!(binary_strcmp(b"a", b"b"), -1);
        assert_eq!(binary_strcmp(b"abc", b"ab"), 1);
        assert_eq!(binary_strcmp(b"", b""), 0);
        assert_eq!(binary_strcmp(b"a", b"c"), -2);
        assert_eq!(binary_strcasecmp(b"HELLO", b"hello"), 0);
        assert_eq!(binary_strncmp(b"abcd", b"abcf", 3), 0);
        assert_eq!(binary_strncasecmp(b"ABcd", b"abCF", 3), 0);
        assert_eq!(binary_strncmp(b"ab", b"abc", 5), -1);
    }

    #[test]
    fn case_conversion() {
        let s = ZStr::from("AbC");
        assert_eq!(s.to_ascii_lowercase(), "abc");
        assert_eq!(s.to_ascii_uppercase(), "ABC");
        let l = ZStr::from("abc");
        assert!(ZStr::ptr_eq(&l.to_ascii_lowercase(), &l));
    }

    #[test]
    fn format_long_matches_to_string() {
        let mut cases = vec![
            0,
            1,
            9,
            10,
            99,
            100,
            101,
            999,
            1000,
            -1,
            -9,
            -10,
            -100,
            i64::MAX,
            i64::MIN,
            i64::MIN + 1,
        ];
        let mut p = 1i64;
        while let Some(next) = p.checked_mul(10) {
            cases.extend([p - 1, p, p + 1, -p, -(p - 1)]);
            p = next;
        }
        for n in cases {
            let mut buf = [0u8; LONG_MAX_LEN];
            let text = n.to_string();
            assert_eq!(format_long(n, &mut buf), text.as_bytes(), "{n}");
            assert_eq!(long_str_len(n), text.len(), "{n}");
        }
    }

    #[test]
    fn append_long_in_place() {
        let mut s = ZStr::from("n=");
        s.append_long(-42);
        assert_eq!(s, "n=-42");
        let t = s.clone();
        s.append_long(7);
        assert_eq!(s, "n=-427");
        assert_eq!(t, "n=-42", "a shared string is copied, not extended");
        let mut e = ZStr::empty();
        e.append_long(i64::MIN);
        assert_eq!(e, "-9223372036854775808");
        assert_eq!(ZStr::empty(), "");
    }

    #[test]
    fn zstrbuf_builds_exact_and_grown_strings() {
        let mut b = ZStrBuf::with_capacity(11);
        assert!(b.is_empty());
        b.push(b"item ");
        b.push_long(5);
        b.push(b"");
        b.push(b" of 9");
        assert_eq!(b.len(), 11);
        assert_eq!(b.as_bytes(), b"item 5 of 9");
        let s = b.finish();
        assert_eq!(s, "item 5 of 9");
        assert_eq!(s.refcount(), 1);
        assert!(!s.is_interned());
        assert_eq!(s.hash_value(), zend_hash_bytes(b"item 5 of 9"));

        // Past the reserved capacity it grows like `.=`.
        let mut g = ZStrBuf::with_capacity(0);
        let mut expected = Vec::new();
        for i in 0..500i64 {
            g.push_long(i);
            g.push(b",");
            expected.extend_from_slice(format!("{i},").as_bytes());
        }
        let g = g.finish();
        assert_eq!(g.as_bytes(), &expected[..]);
        let mut g2 = g.clone();
        g2.append(b"!");
        assert_eq!(g.len() + 1, g2.len());
        assert!(ZStrBuf::with_capacity(0).finish().is_empty());
    }
}
