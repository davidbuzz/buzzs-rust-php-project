// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ZArray`: PHP's ordered hash table (`HashTable` / `zend_array`).
//!
//! The layout mirrors `Zend/zend_hash.c` closely because several PHP-visible
//! behaviours depend on it:
//!
//! * elements live in insertion order in a vector that may contain holes
//!   (deleted slots); `nNumUsed` is the vector length;
//! * *packed* arrays (keys `0..n` in order, possibly with holes) store bare
//!   values and no hash index (`zval *arPacked`): the key is the position,
//!   and `&ArrayKey` views of it come from a shared immortal table (the
//!   `int_keys` module); inserting out of order converts them to *hash* arrays
//!   of buckets (value + key) whose collision chains live in the index;
//! * `nNextFreeElement` follows Zend's rules exactly (including negative keys
//!   since PHP 8.3 and the packed-array quirk where it is reset to `h + 1`);
//! * positions (`HashPosition`) index into the bucket vector, so iteration by
//!   position keeps working while the table is modified; registered iterators
//!   (`foreach` by reference) are remapped when holes are compacted;
//! * table sizes double like Zend's, so compaction happens at the same time.
//!
//! [`ZArray`] is the refcounted copy-on-write handle; [`HashTable`] is the
//! table itself. Read access goes through `Deref`; mutation through
//! [`ZArray::make_mut`], which separates a shared table first (with
//! `zend_array_dup` semantics: references with refcount 1 are unwrapped).

use crate::numeric::handle_numeric_str;
use crate::sort::{zend_sort, zend_sort_by, SortAccess};
use crate::string::ZStr;
use crate::value::{PhpRef, Value};
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// `HT_MIN_SIZE`.
const HT_MIN_SIZE: u32 = 8;
/// `HT_MAX_SIZE` (64-bit).
const HT_MAX_SIZE: u32 = 0x4000_0000;
/// `HT_INVALID_IDX`.
const INVALID: u32 = u32::MAX;

/// The integer key string key `s` stands for in a symbol table
/// (`ZEND_HANDLE_NUMERIC_STR`): the first byte, tested inline, rules out
/// most strings before [`handle_numeric_str`] parses digits
/// (`_zend_handle_numeric_str` makes the same test first).
#[inline(always)]
#[allow(clippy::inline_always)] // one-byte test on every string-key lookup
fn numeric_key(s: &[u8]) -> Option<i64> {
    match s.first() {
        Some(c) if c.is_ascii_digit() || *c == b'-' => handle_numeric_str(s),
        _ => None,
    }
}

/// An array key: an integer or a (non-canonical-integer) string.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArrayKey {
    /// An integer key.
    Int(i64),
    /// A string key.
    Str(ZStr),
}

impl ArrayKey {
    /// Builds a key from string bytes with symbol-table semantics: canonical
    /// decimal integers (`"12"`, `"-3"`, but not `"012"` or `"1.0"`) become
    /// integer keys (`ZEND_HANDLE_NUMERIC_STR`).
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match handle_numeric_str(bytes) {
            Some(i) => ArrayKey::Int(i),
            None => ArrayKey::Str(ZStr::from(bytes)),
        }
    }

    /// Like [`ArrayKey::from_bytes`] but reuses an existing string.
    #[inline]
    #[must_use]
    pub fn from_zstr(s: &ZStr) -> Self {
        match numeric_key(s.as_bytes()) {
            Some(i) => ArrayKey::Int(i),
            None => ArrayKey::Str(s.clone()),
        }
    }

    /// Whether `bytes` are surely no integer in digits, so a string key as
    /// they are (`ZEND_HANDLE_NUMERIC_STR` leaves them alone), decided
    /// without a call: empty, a first byte that is neither a digit nor
    /// `-`, or a later byte that is no digit. `false` only says that
    /// [`ArrayKey::from_bytes`] must look.
    #[inline(always)]
    #[allow(clippy::inline_always)] // a few byte tests in a lean handler
    #[must_use]
    pub fn surely_str(bytes: &[u8]) -> bool {
        match bytes.split_first() {
            None => true,
            Some((first, rest)) => {
                (!first.is_ascii_digit() && *first != b'-')
                    || rest.iter().any(|b| !b.is_ascii_digit())
            }
        }
    }

    /// The int key a string of up to 18 digits is (`ZEND_HANDLE_NUMERIC_STR`
    /// for those: decimal digits, an optional `-`, no leading zero, not
    /// `-0`), decided without a call. `None` for anything else, including
    /// longer integers in digits, which [`ArrayKey::from_bytes`] converts.
    #[inline(always)]
    #[allow(clippy::inline_always)] // a few byte tests in a lean handler
    #[must_use]
    pub fn small_int(bytes: &[u8]) -> Option<i64> {
        let (neg, digits) = match bytes.split_first()? {
            (b'-', rest) => (true, rest),
            _ => (false, bytes),
        };
        let (&first, _) = digits.split_first()?;
        if digits.len() > 18 || (first == b'0' && (neg || digits.len() > 1)) {
            return None;
        }
        let mut n: i64 = 0;
        for &c in digits {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + i64::from(c - b'0');
        }
        Some(if neg { -n } else { n })
    }

    /// The hash used for bucket placement (`h`): the integer itself or the
    /// string's PHP hash.
    #[inline]
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn hash(&self) -> u64 {
        match self {
            ArrayKey::Int(i) => *i as u64,
            ArrayKey::Str(s) => s.hash_value(),
        }
    }

    /// The key as a PHP value (`int` or `string`).
    #[inline]
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            ArrayKey::Int(i) => Value::Int(*i),
            ArrayKey::Str(s) => Value::Str(s.clone()),
        }
    }

    /// The key as bytes (integers are formatted in decimal).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            ArrayKey::Int(i) => i.to_string().into_bytes(),
            ArrayKey::Str(s) => s.as_bytes().to_vec(),
        }
    }

    /// The key as a string (integers converted).
    #[must_use]
    pub fn to_zstr(&self) -> ZStr {
        match self {
            ArrayKey::Int(i) => ZStr::from(i.to_string()),
            ArrayKey::Str(s) => s.clone(),
        }
    }
}

impl fmt::Debug for ArrayKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArrayKey::Int(i) => write!(f, "{i}"),
            ArrayKey::Str(s) => write!(f, "{s:?}"),
        }
    }
}

impl From<i64> for ArrayKey {
    fn from(i: i64) -> Self {
        ArrayKey::Int(i)
    }
}

impl From<&str> for ArrayKey {
    /// Symbol-table conversion (numeric strings become integer keys).
    fn from(s: &str) -> Self {
        ArrayKey::from_bytes(s.as_bytes())
    }
}

impl From<ZStr> for ArrayKey {
    /// Symbol-table conversion (numeric strings become integer keys).
    fn from(s: ZStr) -> Self {
        ArrayKey::from_zstr(&s)
    }
}

/// A bucket of a hash-mode table (`Bucket`): the value and its key.
///
/// php-src's `Bucket` is a zval, the hash `h` and the key string (32 bytes).
/// Here the key is an [`ArrayKey`] (16 bytes: the integer, or the string,
/// whose header caches its hash, so `h` needs no field of its own), and the
/// collision-chain link (`Z_NEXT(p->val)`, kept in the zval's spare `u2`
/// word in php-src) lives in the bucket (`next: u32`).
#[derive(Clone)]
struct Bucket {
    /// The value; `None` for a deleted slot (`IS_UNDEF`).
    val: Option<Value>,
    /// The key (`Int(0)` in a deleted slot, whose key string is released).
    key: ArrayKey,
    /// Next bucket index in the collision chain (`u32::MAX` / `INVALID` if none).
    next: u32,
}

impl Bucket {
    /// The bucket of a live element.
    #[inline]
    fn new(key: ArrayKey, val: Value, next: u32) -> Self {
        Bucket {
            val: Some(val),
            key,
            next,
        }
    }
}

/// The storage of a [`HashTable`] (its `HT_FLAGS` kind plus `arData`).
enum Store {
    /// `HASH_FLAG_UNINITIALIZED`: nothing allocated yet.
    Uninit,
    /// `HASH_FLAG_PACKED`: bare values whose keys are their positions
    /// (`zval *arPacked`, PHP 8.2+); `None` is a hole (`IS_UNDEF`).
    Packed(Vec<Option<Value>>),
    /// A hash: buckets in insertion order plus the index.
    Hash(HashData),
}

/// The buckets and index of a hash-mode table.
struct HashData {
    /// Buckets in insertion order, holes included (`nNumUsed` entries).
    buckets: Vec<Bucket>,
    /// `nTableSize` hash slots (chain heads, `HT_HASH`).
    index: Box<[u32]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `HASH_FLAG_UNINITIALIZED`.
    Uninit,
    /// `HASH_FLAG_PACKED`.
    Packed,
    /// Regular hash with index.
    Hash,
}

/// Identifier of a registered array iterator (`foreach` by reference).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IterId(u32);

thread_local! {
    static NEXT_ITER_ID: Cell<u32> = const { Cell::new(1) };
    static EMPTY_ARRAY: ZArray = ZArray(Rc::new(HashTable::immutable_empty()));
}

/// The ordered hash table.
pub struct HashTable {
    store: Store,
    num_elements: u32,
    table_size: u32,
    internal_ptr: u32,
    next_free: i64,
    /// Registered iterator positions: `(id, pos)`.
    iterators: IterList,
    /// The cycle collector's mark (php-src's `GC_INFO`, the root buffer
    /// slot in the array's header): 0 while the array is not a possible
    /// root, [`HashTable::GC_IMMUTABLE`] for the shared empty array
    /// (`zend_empty_array`, never buffered), else its slot + 1 in the
    /// array root buffer ([`crate::ref_roots`]). It takes the header's
    /// four bytes of tail padding, so the table stays 72 bytes. A table
    /// cloned or built is unmarked; the mark stays with the allocation
    /// (see [`ZArray::make_mut`]).
    gc_root: Cell<u32>,
}

/// The registered iterators of a table (`HT_ITERATORS_COUNT`): boxed and
/// absent while there are none, which is almost always (8 bytes per table
/// instead of an inline `Vec`'s 24).
#[derive(Clone, Default)]
struct IterList(Option<Box<Vec<(IterId, u32)>>>);

impl IterList {
    fn push(&mut self, it: (IterId, u32)) {
        self.0.get_or_insert_with(Box::default).push(it);
    }

    fn retain(&mut self, f: impl FnMut(&(IterId, u32)) -> bool) {
        if let Some(v) = &mut self.0 {
            v.retain(f);
            if v.is_empty() {
                self.0 = None;
            }
        }
    }

    /// Moves all of `other`'s iterators to the end of this list.
    fn append(&mut self, other: &mut IterList) {
        if let Some(mut o) = other.0.take() {
            match &mut self.0 {
                Some(v) => v.append(&mut o),
                None => self.0 = Some(o),
            }
        }
    }
}

impl std::ops::Deref for IterList {
    type Target = [(IterId, u32)];
    fn deref(&self) -> &[(IterId, u32)] {
        match &self.0 {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }
}

impl std::ops::DerefMut for IterList {
    fn deref_mut(&mut self) -> &mut [(IterId, u32)] {
        match &mut self.0 {
            Some(v) => v.as_mut_slice(),
            None => &mut [],
        }
    }
}

impl<'a> IntoIterator for &'a IterList {
    type Item = &'a (IterId, u32);
    type IntoIter = std::slice::Iter<'a, (IterId, u32)>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut IterList {
    type Item = &'a mut (IterId, u32);
    type IntoIter = std::slice::IterMut<'a, (IterId, u32)>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl Default for HashTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Releases an element moved out of a table: a scalar (or a hole) owns
/// nothing and is forgotten without a call into the drop glue; anything
/// else is dropped.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test per element of a released table
fn release_element(slot: Option<Value>) {
    match slot {
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
            std::mem::forget(slot);
        }
        Some(v) => drop(v),
    }
}

impl Drop for HashTable {
    /// `zend_array_destroy`: a buffered array leaves the root buffer first
    /// (`GC_REMOVE_FROM_BUFFER`, one test of the mark otherwise), then the
    /// elements are released in order, a bucket's value before its key.
    /// Left to the storage's own drop, every element of a list of ints cost
    /// a call into the drop glue of `Value`.
    fn drop(&mut self) {
        let mark = self.gc_root.get();
        if mark != 0 {
            crate::ref_roots::array_freed(self, mark);
        }
        match std::mem::replace(&mut self.store, Store::Uninit) {
            Store::Uninit => {}
            Store::Packed(values) => {
                for slot in values {
                    release_element(slot);
                }
            }
            Store::Hash(data) => {
                for bucket in data.buckets {
                    release_element(bucket.val);
                    if let ArrayKey::Str(key) = bucket.key {
                        drop(key);
                    }
                }
            }
        }
    }
}

/// `zend_hash_check_size`: rounds a size hint up to a power of two >= 8.
fn check_size(n: u32) -> u32 {
    if n <= HT_MIN_SIZE {
        HT_MIN_SIZE
    } else if n >= HT_MAX_SIZE {
        HT_MAX_SIZE
    } else {
        n.next_power_of_two()
    }
}

#[inline]
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A position as an integer key.
#[inline]
fn idx_key(i: usize) -> i64 {
    i64::try_from(i).unwrap_or(i64::MAX)
}

/// The number of hash slots of a table of `table_size` buckets: twice as
/// many, as php-src's `HT_SIZE_TO_MASK(nSize)` (`-(nSize + nSize)`) keeps
/// the chains short (a load factor of at most 1/2).
#[inline]
fn index_len(table_size: u32) -> usize {
    table_size as usize * 2
}

/// A fresh index for `table_size` buckets: [`index_len`] empty hash slots
/// (`HT_INVALID_IDX` / `u32::MAX`).
fn new_index(table_size: u32) -> Box<[u32]> {
    vec![INVALID; index_len(table_size)].into_boxed_slice()
}

/// The hash slot of `h` (`nIndex = h | nTableMask`: `h` modulo the
/// `2 * nTableSize` slots).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn hash_slot(table_size: u32, h: u64) -> usize {
    (h as usize) & (index_len(table_size) - 1)
}

/// Whether two key strings have the same bytes (`zend_string_equal_content`
/// once the hashes matched), without a call: keys of up to 16 bytes, the
/// common case, compare with two overlapping word loads per side, longer
/// ones eight bytes at a time and their last eight. (A `memcmp` call for
/// the long keys made every probe loop that inlines this keep its state in
/// callee-saved registers, pushed and popped around each lookup.)
#[inline]
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    bytes_eq(a, b)
}

/// [`same_bytes`] itself, always in line: the comparison of the lean
/// probes, which must not call (as a call it cost each of them six saved
/// registers and its state kept on the stack).
#[inline(always)]
#[allow(clippy::inline_always)] // see above
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    let n = a.len();
    if n != b.len() {
        return false;
    }
    // The commonest keys first: under 8 bytes, then 8 to 16.
    if n < 8 {
        if n >= 4 {
            return a.first_chunk::<4>() == b.first_chunk::<4>()
                && a.last_chunk::<4>() == b.last_chunk::<4>();
        }
        return a.iter().zip(b).all(|(x, y)| x == y);
    }
    if n <= 16 {
        return a.first_chunk::<8>() == b.first_chunk::<8>()
            && a.last_chunk::<8>() == b.last_chunk::<8>();
    }
    // Over 16 bytes: 8 at a time, then the last 8 (overlapping). A loop of
    // its own: `chunks_exact().zip().all()` compiled to two iterator calls.
    let (mut x, mut y) = (a, b);
    while let (Some((cx, rx)), Some((cy, ry))) =
        (x.split_first_chunk::<8>(), y.split_first_chunk::<8>())
    {
        if cx != cy {
            return false;
        }
        x = rx;
        y = ry;
    }
    a.last_chunk::<8>() == b.last_chunk::<8>()
}

/// `_zend_hash_get_valid_pos` over one storage: the first live slot at or
/// after `pos`, the slot count (`nNumUsed`) when there is none, or `pos`
/// itself when it lies past the end.
#[inline]
fn first_live<T>(slots: &[T], pos: u32, live: impl Fn(&T) -> bool) -> u32 {
    let start = pos as usize;
    match slots.get(start..) {
        Some(rest) => match rest.iter().position(live) {
            Some(i) => to_u32(start + i),
            None => to_u32(slots.len()),
        },
        None => pos,
    }
}

/// The shared key of packed position `pos` through an iterator's cached
/// chunk (`keys`, whose first key is position `keys_start`): one chunk
/// lookup per chunk of positions, then a slice index.
#[inline]
fn packed_key(keys: &mut &'static [ArrayKey], keys_start: &mut u32, pos: u32) -> &'static ArrayKey {
    let cached: &'static [ArrayKey] = *keys;
    if let Some(k) = cached.get(pos.wrapping_sub(*keys_start) as usize) {
        return k;
    }
    let (chunk, start) = int_keys::chunk(pos);
    *keys = chunk;
    *keys_start = start;
    match chunk.get(pos.wrapping_sub(start) as usize) {
        Some(k) => k,
        None => int_keys::get(pos),
    }
}

/// The elements of a table's storage in order, by value.
fn into_entries(store: Store) -> impl Iterator<Item = (ArrayKey, Value)> {
    let (packed, buckets) = match store {
        Store::Uninit => (Vec::new(), Vec::new()),
        Store::Packed(v) => (v, Vec::new()),
        Store::Hash(d) => (Vec::new(), d.buckets),
    };
    packed
        .into_iter()
        .enumerate()
        .filter_map(|(i, slot)| slot.map(|v| (ArrayKey::Int(idx_key(i)), v)))
        .chain(
            buckets
                .into_iter()
                .filter_map(|b| b.val.map(|v| (b.key, v))),
        )
}

/// Shared, immortal `ArrayKey::Int` values for the positions of packed
/// tables.
///
/// A packed table stores bare values, like php-src's `zval *arPacked`: the
/// key of an element is its position. The table API still lends keys out as
/// `&ArrayKey` (`iter()`, `at()`, `keys()`, `first()`, `last()`,
/// `current_key()`, `iter_mut()`), so for packed tables those references
/// point here instead: one process-wide array of `Int(i)` per position,
/// allocated in chunks on first use and never written or freed afterwards.
/// The chunks come from the system allocator directly, outside the counted
/// heap: they are interpreter bookkeeping that php-src does not have (it
/// yields packed keys by value), so `memory_get_usage()` and `memory_limit`
/// must not see them.
///
/// Footprint: 16 bytes per position up to the highest position ever handed
/// out, plus at most one chunk (1 MiB) of slack, for the life of the
/// process. Positions below `2^16` live in chunks of doubling size (4 KiB
/// first, 1 MiB in all); above that every chunk holds `2^16` keys, so the
/// table never overshoots the arrays that drove it by a factor. A script
/// that iterates a packed array of `n` elements therefore pins about `16 n`
/// bytes that `memory_get_usage()` does not report; the arrays themselves
/// take at least `16 n` counted bytes, so the hidden part never exceeds the
/// visible one and stays within `memory_limit`'s order of magnitude. VM
/// code that only needs the key of a position should use
/// [`HashTable::key_at`] or [`HashTable::key_value_at`] (and
/// [`HashTable::fe_fetch`] to step `foreach`), which never touch this
/// table. Iterators cache the chunk they are in (`chunk`), so walking a
/// packed table costs one chunk lookup per chunk of positions, not one per
/// element.
mod int_keys {
    use super::{to_u32, ArrayKey};
    use std::alloc::{handle_alloc_error, GlobalAlloc, Layout, System};
    use std::ptr;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// log2 of the keys in the first chunk.
    const FIRST_BITS: u32 = 8;
    /// log2 of the keys in every chunk above `SMALL`.
    const BIG_BITS: u32 = 16;
    /// Positions below this use the doubling chunks: chunk 0 holds
    /// `0..2^8`, chunk `c` in `1..=8` holds `2^(7+c)..2^(8+c)`.
    const SMALL: u32 = 1 << BIG_BITS;
    /// The number of doubling chunks.
    const SMALL_CHUNKS: usize = (BIG_BITS - FIRST_BITS) as usize + 1;
    /// Chunk `SMALL_CHUNKS - 1 + k` (`k >= 1`) holds the positions
    /// `k << BIG_BITS` up to `(k + 1) << BIG_BITS`; this many chunks cover
    /// every `u32` position.
    const CHUNKS: usize = SMALL_CHUNKS - 1 + (1 << (u32::BITS - BIG_BITS));

    /// The chunks; null until first used.
    static CHUNK: [AtomicPtr<ArrayKey>; CHUNKS] =
        [const { AtomicPtr::new(ptr::null_mut()) }; CHUNKS];

    /// The chunk holding `pos` and the offset of `pos` in it.
    #[inline]
    fn locate(pos: u32) -> (usize, usize) {
        if pos < SMALL {
            let c = (u32::BITS - pos.leading_zeros()).saturating_sub(FIRST_BITS);
            let start = if c == 0 { 0 } else { 1 << (c + FIRST_BITS - 1) };
            (c as usize, (pos - start) as usize)
        } else {
            (
                SMALL_CHUNKS - 1 + (pos >> BIG_BITS) as usize,
                (pos & (SMALL - 1)) as usize,
            )
        }
    }

    /// The first position of chunk `c` and the number of keys it holds.
    fn extent(c: usize) -> (usize, usize) {
        if c == 0 {
            (0, 1 << FIRST_BITS)
        } else if c < SMALL_CHUNKS {
            let len = 1usize << (to_u32(c) + FIRST_BITS - 1);
            (len, len)
        } else {
            ((c - (SMALL_CHUNKS - 1)) << BIG_BITS, 1 << BIG_BITS)
        }
    }

    /// The key `Int(pos)`.
    #[inline]
    pub(super) fn get(pos: u32) -> &'static ArrayKey {
        let (c, off) = locate(pos);
        let mut base = CHUNK[c].load(Ordering::Acquire);
        if base.is_null() {
            base = fill(c);
        }
        // SAFETY: `base` is the published chunk `c`, whose `extent(c).1`
        // keys were all initialised before publication (`fill`) and are
        // never written or freed afterwards, and `locate` keeps
        // `off < extent(c).1`. The keys are integers, which hold no shared
        // mutable state, so lending them to any thread for the rest of the
        // process is sound.
        unsafe { &*base.add(off) }
    }

    /// The chunk holding `pos`, as the slice of its keys and the position
    /// of its first key: iterators fetch it once per chunk and then index
    /// the slice instead of locating every position.
    #[inline]
    pub(super) fn chunk(pos: u32) -> (&'static [ArrayKey], u32) {
        let (c, _) = locate(pos);
        let mut base = CHUNK[c].load(Ordering::Acquire);
        if base.is_null() {
            base = fill(c);
        }
        let (start, len) = extent(c);
        // SAFETY: as in `get`: `base` is the published chunk `c`, whose
        // `len` keys were all initialised before publication (`fill`) and
        // are never written or freed afterwards, so a shared slice over them
        // stays valid for the rest of the process.
        let keys: &'static [ArrayKey] = unsafe { std::slice::from_raw_parts(base, len) };
        (keys, to_u32(start))
    }

    /// The bytes the table has allocated so far (tests and diagnostics).
    #[cfg(test)]
    pub(super) fn allocated_bytes() -> usize {
        (0..CHUNKS)
            .filter(|&c| !CHUNK[c].load(Ordering::Acquire).is_null())
            .map(|c| extent(c).1 * std::mem::size_of::<ArrayKey>())
            .sum()
    }

    /// Allocates, fills and publishes chunk `c`, or returns the chunk
    /// another thread published first.
    #[cold]
    #[inline(never)]
    #[allow(clippy::cast_ptr_alignment)]
    fn fill(c: usize) -> *mut ArrayKey {
        let (start, len) = extent(c);
        let Ok(layout) = Layout::array::<ArrayKey>(len) else {
            std::process::abort()
        };
        // SAFETY: `layout` has a non-zero size (`len >= FIRST`).
        let base = unsafe { System.alloc(layout) }.cast::<ArrayKey>();
        if base.is_null() {
            handle_alloc_error(layout);
        }
        for k in 0..len {
            let key = ArrayKey::Int(i64::try_from(start + k).unwrap_or(i64::MAX));
            // SAFETY: `k < len`, inside the fresh allocation, which
            // `Layout::array` aligned for `ArrayKey`.
            unsafe { base.add(k).write(key) };
        }
        match CHUNK[c].compare_exchange(ptr::null_mut(), base, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => base,
            Err(winner) => {
                // SAFETY: `base` came from `System.alloc(layout)` above and
                // was never published; integer keys need no drop.
                unsafe { System.dealloc(base.cast::<u8>(), layout) };
                winner
            }
        }
    }
}

impl HashData {
    /// Room for `table_size` buckets and their index
    /// (`zend_hash_real_init_mixed_ex` allocates `HT_SIZE_EX(nTableSize)`).
    fn new(table_size: u32) -> Self {
        HashData {
            buckets: Vec::with_capacity(table_size as usize),
            index: new_index(table_size),
        }
    }

    /// Appends a bucket for `key` and links it at the head of its chain
    /// (the tail of `_zend_hash_add_or_update_i`); returns its position.
    /// The caller checked that the key is absent and that the table has
    /// room (`nNumUsed < nTableSize`).
    #[inline]
    fn append(&mut self, ts: u32, key: ArrayKey, val: Value) -> u32 {
        let idx = to_u32(self.buckets.len());
        let s = hash_slot(ts, key.hash());
        let head = match self.index.get_mut(s) {
            Some(slot) => std::mem::replace(slot, idx),
            None => INVALID,
        };
        self.buckets.push(Bucket::new(key, val, head));
        idx
    }

    /// The bucket of the string key `key`, whose hash is `h`, found
    /// without a call (a bucket key whose hash is unknown is not looked
    /// at: every key of a table was hashed to be placed).
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of the lean lookups
    fn lean_probe(&self, ts: u32, key: &ZStr, h: u64) -> Option<u32> {
        let mut idx = self.head(ts, h);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if ZStr::ptr_eq(k, key)
                    || (k.known_hash() == Some(h) && bytes_eq(k.as_bytes(), key.as_bytes()))
                {
                    return Some(idx);
                }
            }
            idx = b.next;
        }
        None
    }

    /// [`HashData::find_int`], always inlined: for the lean lookups.
    #[inline(always)]
    #[allow(clippy::inline_always)] // see above
    fn lean_probe_int(&self, ts: u32, h: i64) -> Option<u32> {
        #[allow(clippy::cast_sign_loss)]
        let mut idx = self.head(ts, h as u64);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if matches!(b.key, ArrayKey::Int(k) if k == h) && b.val.is_some() {
                return Some(idx);
            }
            idx = b.next;
        }
        None
    }

    /// The first bucket of `h`'s chain (`HT_HASH(ht, nIndex)`).
    #[inline]
    fn head(&self, ts: u32, h: u64) -> u32 {
        self.index.get(hash_slot(ts, h)).copied().unwrap_or(INVALID)
    }

    /// Whether the index links the buckets as they stand, as far as one
    /// probe tells: the last-placed bucket heads its chain in every linked
    /// table (appends link at the head, unlinks keep the order, re-links
    /// build it that way), and in no table whose index was reset while its
    /// buckets stay ([`HashTable::live_sort_begin`]: a sort with a PHP
    /// comparator, which may copy the table half-sorted). An empty table
    /// is linked.
    fn last_is_linked(&self, ts: u32) -> bool {
        match self.buckets.len().checked_sub(1) {
            None => true,
            Some(last) => self
                .buckets
                .get(last)
                .is_some_and(|b| self.head(ts, b.key.hash()) == to_u32(last)),
        }
    }

    /// The bucket after `idx` in its chain (`Z_NEXT`).
    #[inline]
    #[allow(dead_code)]
    fn next_of(&self, idx: u32) -> u32 {
        self.buckets.get(idx as usize).map_or(INVALID, |b| b.next)
    }

    #[inline]
    #[allow(dead_code)]
    fn set_next(&mut self, idx: u32, next: u32) {
        if let Some(b) = self.buckets.get_mut(idx as usize) {
            b.next = next;
        }
    }

    /// `zend_hash_index_find_bucket`.
    #[inline]
    fn find_int(&self, ts: u32, h: i64) -> Option<u32> {
        #[allow(clippy::cast_sign_loss)]
        let mut idx = self.head(ts, h as u64);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if matches!(b.key, ArrayKey::Int(k) if k == h) && b.val.is_some() {
                return Some(idx);
            }
            idx = b.next;
        }
        None
    }

    /// `zend_hash_str_find_bucket`: equal hash, then equal bytes.
    #[inline]
    fn find_str(&self, ts: u32, s: &[u8], h: u64) -> Option<u32> {
        let mut idx = self.head(ts, h);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if k.hash_value() == h && same_bytes(k.as_bytes(), s) && b.val.is_some() {
                    return Some(idx);
                }
            }
            idx = b.next;
        }
        None
    }

    /// `zend_hash_find_bucket`: the very same string, or equal hash and
    /// bytes.
    #[inline]
    fn find_zstr(&self, ts: u32, key: &ZStr) -> Option<u32> {
        self.probe_zstr(ts, key)
    }

    /// [`HashData::find_zstr`], always inlined: for a caller that is a
    /// string-key path itself (the probe is its body, not a call out of
    /// it).
    #[inline(always)]
    #[allow(clippy::inline_always)] // see above
    fn probe_zstr(&self, ts: u32, key: &ZStr) -> Option<u32> {
        let h = key.hash_value();
        let mut idx = self.head(ts, h);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if (ZStr::ptr_eq(k, key)
                    || (k.hash_value() == h && same_bytes(k.as_bytes(), key.as_bytes())))
                    && b.val.is_some()
                {
                    return Some(idx);
                }
            }
            idx = b.next;
        }
        None
    }

    /// Looks up a string key by identical pointer first, then hash and bytes.
    #[inline]
    #[allow(dead_code)]
    fn get_str_identical_val<'a>(&'a self, ts: u32, key: &ZStr) -> Option<&'a Value> {
        let idx = self.find_zstr(ts, key)?;
        self.buckets.get(idx as usize)?.val.as_ref()
    }

    /// Rebuilds every chain over the buckets as they stand, holes kept
    /// (`zend_hash_rehash` without compaction).
    #[allow(clippy::cast_possible_truncation)]
    fn relink(&mut self, ts: u32) {
        if self.index.len() == index_len(ts) {
            self.index.fill(INVALID);
        } else {
            self.index = new_index(ts);
        }
        let mask = index_len(ts) - 1;
        for (i, b) in self.buckets.iter_mut().enumerate() {
            if b.val.is_none() {
                b.next = INVALID;
                continue;
            }
            let s = (b.key.hash() as usize) & mask;
            b.next = self.index[s];
            self.index[s] = to_u32(i);
        }
    }

    /// Unlinks the live bucket at `idx` from its chain and empties it
    /// (`_zend_hash_del_el_ex`), returning its value.
    fn unlink_take(&mut self, ts: u32, idx: u32) -> Option<Value> {
        let b = self.buckets.get(idx as usize)?;
        b.val.as_ref()?;
        let s = hash_slot(ts, b.key.hash());
        let next = b.next;
        let head = self.index.get(s).copied().unwrap_or(INVALID);
        if head == idx {
            self.index[s] = next;
        } else {
            let mut p = head;
            while p != INVALID {
                let pn = self.buckets.get(p as usize).map_or(INVALID, |b| b.next);
                if pn == idx {
                    if let Some(prev) = self.buckets.get_mut(p as usize) {
                        prev.next = next;
                    }
                    break;
                }
                p = pn;
            }
        }
        let b = self.buckets.get_mut(idx as usize)?;
        b.next = INVALID;
        let val = b.val.take();
        if matches!(b.key, ArrayKey::Str(_)) {
            b.key = ArrayKey::Int(0);
        }
        val
    }
}

impl HashTable {
    /// The byte offset of the element count (`nNumOfElements`, a `u32`)
    /// inside a table: what machine code that tests an array for
    /// emptiness reads (php-vm's trace JIT, whose layout test pins it
    /// together with the count words of the handle).
    pub const NUM_ELEMENTS_OFFSET: usize = std::mem::offset_of!(HashTable, num_elements);

    /// The byte offset, inside a table, of what holds its elements (a
    /// list's bare values or a hash's buckets and index): where machine
    /// code that reads a list's element without a call starts from
    /// (php-vm's trace JIT, whose layout test pins the words it reads
    /// there).
    pub const STORE_OFFSET: usize = std::mem::offset_of!(HashTable, store);

    /// The byte offsets of the table size (`nTableSize`, a `u32`) and of
    /// the next free int key (`nNextFreeElement`, an `i64`) inside a
    /// table: with the element count, what machine code that appends to a
    /// list without a call reads and writes (php-vm's trace JIT, whose
    /// layout test pins them).
    pub const TABLE_SIZE_OFFSET: usize = std::mem::offset_of!(HashTable, table_size);
    /// See [`HashTable::TABLE_SIZE_OFFSET`].
    pub const NEXT_FREE_OFFSET: usize = std::mem::offset_of!(HashTable, next_free);

    /// The byte offset of the root buffer mark (a `u32`,
    /// [`HashTable::gc_root_mark`]; `Cell` is `repr(transparent)`):
    /// machine code that drops one handle of a shared array a variable
    /// held without a call may do so only while the mark is not 0 (the
    /// array is buffered already, or immutable); at 0 the release is a
    /// possible root to buffer (php-vm's trace JIT).
    pub const GC_ROOT_OFFSET: usize = std::mem::offset_of!(HashTable, gc_root);

    /// The room of a list's allocation, in elements; `None` for a table
    /// that is no list. (For the layout test of the code that appends to
    /// a list without a call.)
    #[must_use]
    pub fn packed_capacity(&self) -> Option<usize> {
        match &self.store {
            Store::Packed(v) => Some(v.capacity()),
            Store::Uninit | Store::Hash(_) => None,
        }
    }

    /// Creates an empty table (`zend_new_array(0)`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Creates an empty hash-mode (non-packed) table sized for `n` elements
    /// (`array_init_size` + `zend_hash_real_init_mixed`), as PHP's
    /// unserializer and other internal builders do.
    #[must_use]
    pub fn with_capacity_mixed(n: usize) -> Self {
        let mut t = Self::with_capacity(n);
        t.real_init_mixed();
        t
    }

    /// Creates an empty list sized for `n` elements with its storage in
    /// place (`zend_new_array(n)` + `zend_hash_real_init_packed`, what
    /// `ZEND_INIT_ARRAY` and its first element without a key make): appends
    /// below the table size take [`HashTable::push`]'s short path from the
    /// first one on.
    #[must_use]
    pub fn with_capacity_packed(n: usize) -> Self {
        let mut t = Self::with_capacity(n);
        t.real_init_packed();
        t
    }

    /// A list of `values` (keys `0, 1, 2, ...`) built in one step, sized
    /// for `n` elements: `array_init_size(n)` + `zend_hash_real_init_packed`
    /// + `ZEND_HASH_FILL_PACKED`, what php-src's array builders that know
    /// their result's size do. The elements are written straight into the
    /// list's storage, allocated once for `nTableSize` elements (`n`
    /// rounded up to a power of two, at least 8), with none of
    /// [`HashTable::push`]'s per-element tests; `n` is only a hint (more
    /// or fewer values give the same table, the storage grown as pushes
    /// would grow it). No values leave the table untouched, as
    /// `array_init_size` with nothing added does.
    #[must_use]
    pub fn from_list_sized<I: IntoIterator<Item = Value>>(n: usize, values: I) -> Self {
        let mut t = Self::with_capacity(n);
        let mut v: Vec<Option<Value>> = Vec::with_capacity(t.table_size as usize);
        v.extend(values.into_iter().map(Some));
        let len = v.len();
        if len == 0 {
            return t;
        }
        if len > t.table_size as usize {
            t.table_size = check_size(to_u32(len));
        }
        t.num_elements = to_u32(len);
        t.next_free = idx_key(len);
        t.store = Store::Packed(v);
        t
    }

    /// Whether nothing was ever stored in the table (`zend_new_array`'s
    /// state: `HASH_FLAG_UNINITIALIZED`, no next index taken): the shared
    /// empty table of a literal being built is one.
    #[inline]
    #[must_use]
    pub fn is_untouched(&self) -> bool {
        matches!(self.store, Store::Uninit) && self.next_free == i64::MIN
    }

    /// Creates an empty table sized for `n` elements (`zend_new_array(n)`):
    /// nothing is allocated until the first insert, which allocates room for
    /// `nTableSize` (`n` rounded up to a power of two, at least 8) elements
    /// at once, as `zend_hash_real_init_packed` / `_mixed` do; the storage
    /// then doubles with the table size.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        HashTable {
            store: Store::Uninit,
            num_elements: 0,
            table_size: check_size(to_u32(n)),
            internal_ptr: 0,
            next_free: i64::MIN,
            iterators: IterList::default(),
            gc_root: Cell::new(0),
        }
    }

    /// An empty table of `table_size` (already [`check_size`]d) whose
    /// storage is `store` (empty, with room for `table_size` elements):
    /// [`HashTable::with_capacity`]'s fields with the storage set up front.
    fn sized(table_size: u32, store: Store) -> Self {
        HashTable {
            store,
            num_elements: 0,
            table_size,
            internal_ptr: 0,
            next_free: i64::MIN,
            iterators: IterList::default(),
            gc_root: Cell::new(0),
        }
    }

    // ------------------------------------------------------------------
    // Basic properties
    // ------------------------------------------------------------------

    /// Number of elements (`zend_hash_num_elements`).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.num_elements as usize
    }

    /// Whether the table has no elements.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.num_elements == 0
    }

    /// Number of used bucket slots including holes (`nNumUsed`); positions
    /// range over `0..used()`.
    #[inline]
    #[must_use]
    pub fn used(&self) -> u32 {
        match &self.store {
            Store::Uninit => 0,
            Store::Packed(v) => to_u32(v.len()),
            Store::Hash(d) => to_u32(d.buckets.len()),
        }
    }

    #[inline]
    fn kind(&self) -> Kind {
        match self.store {
            Store::Uninit => Kind::Uninit,
            Store::Packed(_) => Kind::Packed,
            Store::Hash(_) => Kind::Hash,
        }
    }

    /// Whether the table is packed (`HT_IS_PACKED`; uninitialized tables are
    /// reported as packed, like `zend_empty_array`).
    #[inline]
    #[must_use]
    pub fn is_packed(&self) -> bool {
        !matches!(self.store, Store::Hash(_))
    }

    /// Whether the table has no holes (`HT_IS_WITHOUT_HOLES`).
    #[inline]
    #[must_use]
    pub fn is_without_holes(&self) -> bool {
        self.used() == self.num_elements
    }

    /// Whether the keys are exactly `0, 1, 2, ...` in order (`array_is_list`).
    #[must_use]
    pub fn is_list(&self) -> bool {
        if self.kind() != Kind::Hash && self.is_without_holes() {
            return true;
        }
        self.iter()
            .enumerate()
            .all(|(i, (k, _))| matches!(k, ArrayKey::Int(n) if usize::try_from(*n) == Ok(i)))
    }

    /// The raw `nNextFreeElement` (`i64::MIN` when no integer key was ever
    /// inserted).
    #[must_use]
    pub fn next_free_element(&self) -> i64 {
        self.next_free
    }

    /// The key that `$a[] = ...` would use.
    #[must_use]
    pub fn next_index(&self) -> i64 {
        if self.next_free == i64::MIN {
            0
        } else {
            self.next_free
        }
    }

    /// Overrides `nNextFreeElement`.
    pub fn set_next_free_element(&mut self, n: i64) {
        self.next_free = n;
    }

    /// The table capacity (`nTableSize`).
    #[must_use]
    pub fn table_size(&self) -> u32 {
        self.table_size
    }

    // ------------------------------------------------------------------
    // Internal machinery
    // ------------------------------------------------------------------

    /// Whether the slot at `pos` holds an element (`Z_TYPE != IS_UNDEF`).
    #[inline]
    fn is_occupied(&self, pos: usize) -> bool {
        match &self.store {
            Store::Uninit => false,
            Store::Packed(v) => matches!(v.get(pos), Some(Some(_))),
            Store::Hash(d) => d.buckets.get(pos).is_some_and(|b| b.val.is_some()),
        }
    }

    /// The value at a position.
    #[inline]
    fn val_ref(&self, pos: u32) -> Option<&Value> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => v.get(pos as usize)?.as_ref(),
            Store::Hash(d) => d.buckets.get(pos as usize)?.val.as_ref(),
        }
    }

    /// The value at a position, mutably.
    #[inline]
    fn val_mut(&mut self, pos: u32) -> Option<&mut Value> {
        match &mut self.store {
            Store::Uninit => None,
            Store::Packed(v) => v.get_mut(pos as usize)?.as_mut(),
            Store::Hash(d) => d.buckets.get_mut(pos as usize)?.val.as_mut(),
        }
    }

    /// `zend_hash_real_init_packed_ex`: room for `nTableSize` values up
    /// front, so appends below the table size never reallocate.
    fn real_init_packed(&mut self) {
        self.store = Store::Packed(Vec::with_capacity(self.table_size as usize));
    }

    /// `zend_hash_real_init_mixed_ex`: room for `nTableSize` buckets and
    /// their index.
    fn real_init_mixed(&mut self) {
        self.store = Store::Hash(HashData::new(self.table_size));
    }

    /// Grows the storage to hold `nTableSize` elements after the table size
    /// changed (the `perealloc` of `arData` in `zend_hash_packed_grow` and
    /// `zend_hash_do_resize`); never shrinks.
    fn reserve_table(&mut self) {
        let ts = self.table_size as usize;
        match &mut self.store {
            Store::Uninit => {}
            Store::Packed(v) => v.reserve_exact(ts.saturating_sub(v.len())),
            Store::Hash(d) => d.buckets.reserve_exact(ts.saturating_sub(d.buckets.len())),
        }
    }

    /// The hash storage, converting an uninitialized or packed table first.
    fn hash_data_mut(&mut self) -> &mut HashData {
        match self.kind() {
            Kind::Hash => {}
            Kind::Packed => self.packed_to_hash(),
            Kind::Uninit => self.real_init_mixed(),
        }
        match &mut self.store {
            Store::Hash(d) => d,
            _ => unreachable!("the table was just converted to a hash"),
        }
    }

    /// Rebuilds the hash index and compacts holes (`zend_hash_rehash`).
    fn rehash(&mut self) {
        if self.num_elements == 0 {
            let cleared = match &mut self.store {
                Store::Uninit => false,
                Store::Packed(v) => {
                    v.clear();
                    true
                }
                Store::Hash(d) => {
                    d.buckets.clear();
                    d.index.fill(INVALID);
                    true
                }
            };
            if cleared {
                self.internal_ptr = 0;
                for it in &mut self.iterators {
                    it.1 = 0;
                }
            }
            return;
        }
        if !self.is_without_holes() {
            self.compact();
        }
        self.rehash_without_compaction();
    }

    /// Removes the holes of a hash, remapping the internal pointer and
    /// iterator positions like `zend_hash_rehash` does: a position pointing
    /// at (or into a hole before) an element moves with that element.
    fn compact(&mut self) {
        let Store::Hash(d) = &mut self.store else {
            return;
        };
        let old_len = d.buckets.len();
        let old_used = to_u32(old_len);
        // new_pos[i] = new index of the first element at old index >= i.
        let mut new_pos = vec![0u32; old_len + 1];
        let mut j = to_u32(d.buckets.iter().filter(|b| b.val.is_some()).count());
        new_pos[old_len] = j;
        for i in (0..old_len).rev() {
            if d.buckets[i].val.is_some() {
                j -= 1;
                new_pos[i] = j;
            } else {
                new_pos[i] = new_pos[i + 1];
            }
        }
        let first_hole = to_u32(
            d.buckets
                .iter()
                .position(|b| b.val.is_none())
                .unwrap_or(old_len),
        );
        let new_used = new_pos[old_len];
        d.buckets.retain(|b| b.val.is_some());
        if self.internal_ptr > first_hole && self.internal_ptr < old_used {
            self.internal_ptr = new_pos[self.internal_ptr as usize];
        }
        for it in &mut self.iterators {
            if it.1 == old_used {
                it.1 = new_used;
            } else if it.1 > first_hole && it.1 < old_used {
                it.1 = new_pos[it.1 as usize];
            }
        }
    }

    /// `zend_hash_do_resize`: compacts a table with enough holes, otherwise
    /// doubles `nTableSize` (buckets and index) and rehashes.
    fn do_resize(&mut self) {
        let n = self.num_elements;
        if self.used() > n + (n >> 5) {
            self.rehash();
        } else if self.table_size < HT_MAX_SIZE {
            self.table_size += self.table_size;
            self.reserve_table();
            // The table is full, so not empty: `rehash` links the chains
            // (`HashData::relink`), which makes the index of the new size
            // (filled once: making it here filled it twice).
            self.rehash();
        }
    }

    /// `zend_hash_packed_to_hash`: the values become buckets keyed by their
    /// positions (in storage for `nTableSize` buckets), then
    /// `zend_hash_rehash` drops the holes (remapping the internal pointer
    /// and the registered iterators) and links the chains.
    fn packed_to_hash(&mut self) {
        let buckets = match std::mem::replace(&mut self.store, Store::Uninit) {
            Store::Uninit => Vec::new(),
            Store::Packed(v) => {
                let mut buckets = Vec::with_capacity((self.table_size as usize).max(v.len()));
                buckets.extend(v.into_iter().enumerate().map(|(i, val)| Bucket {
                    val,
                    key: ArrayKey::Int(idx_key(i)),
                    next: INVALID,
                }));
                buckets
            }
            Store::Hash(d) => {
                self.store = Store::Hash(d);
                return;
            }
        };
        self.store = Store::Hash(HashData {
            buckets,
            index: new_index(self.table_size),
        });
        self.rehash();
    }

    fn rehash_without_compaction(&mut self) {
        let ts = self.table_size;
        if let Store::Hash(d) = &mut self.store {
            d.relink(ts);
        }
    }

    #[inline]
    fn find_int(&self, h: i64) -> Option<u32> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => {
                if h >= 0 {
                    let i = h as usize;
                    match v.get(i) {
                        Some(Some(_)) => Some(to_u32(i)),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            Store::Hash(d) => d.find_int(self.table_size, h),
        }
    }

    #[inline]
    fn find_str(&self, s: &[u8], h: u64) -> Option<u32> {
        match &self.store {
            Store::Hash(d) => d.find_str(self.table_size, s, h),
            _ => None,
        }
    }

    #[inline]
    fn find_zstr(&self, key: &ZStr) -> Option<u32> {
        match &self.store {
            Store::Hash(d) => d.find_zstr(self.table_size, key),
            _ => None,
        }
    }

    #[inline]
    fn find(&self, key: &ArrayKey) -> Option<u32> {
        match key {
            ArrayKey::Int(i) => self.find_int(*i),
            ArrayKey::Str(s) => self.find_zstr(s),
        }
    }

    /// Appends a bucket in hash mode (caller ensured the key is absent and the
    /// table is in hash mode).
    fn hash_append(&mut self, key: ArrayKey, val: Value) -> u32 {
        if let ArrayKey::Int(h) = key {
            self.note_int_key(h);
        }
        let ts = self.table_size;
        let idx = self.hash_data_mut().append(ts, key, val);
        self.num_elements += 1;
        idx
    }

    /// `nNextFreeElement` after adding the integer key `h` to a hash.
    #[inline]
    fn note_int_key(&mut self, h: i64) {
        if h >= self.next_free {
            self.next_free = if h < i64::MAX { h + 1 } else { i64::MAX };
        }
    }

    fn add_to_packed(&mut self, h: i64, val: Value) -> u32 {
        let hu = usize::try_from(h).unwrap_or(0);
        let Store::Packed(v) = &mut self.store else {
            return self.hash_append(ArrayKey::Int(h), val);
        };
        if v.len() < hu {
            v.resize_with(hu, || None);
        }
        v.push(Some(val));
        self.next_free = h + 1;
        self.num_elements += 1;
        to_u32(hu)
    }

    /// Port of `_zend_hash_index_add_or_update_i`. Returns the bucket index
    /// and whether the key already existed (in which case `val` was stored
    /// only for `Mode::Update`).
    ///
    /// The cases that dominate are decided here without a call: an existing
    /// element, an append at `nNumUsed` of a packed table inside
    /// `nTableSize` (`add_to_packed` with no holes to fill), and a new key
    /// in a hash with room. Everything else (initialisation, holes, growth,
    /// conversion) is [`HashTable::index_insert_slow`].
    #[inline]
    fn index_insert(&mut self, h: i64, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        let h = if mode == Mode::Next && h == i64::MIN {
            0
        } else {
            h
        };
        let ts = self.table_size;
        match &mut self.store {
            Store::Packed(v) => {
                if let Ok(i) = usize::try_from(h) {
                    let len = v.len();
                    if i == len && h < i64::from(ts) {
                        v.push(Some(val));
                        self.next_free = h + 1;
                        self.num_elements += 1;
                        return Ok((to_u32(i), false));
                    }
                    let live = matches!(v.get(i), Some(Some(_)));
                    if live {
                        return self.existing(to_u32(i), val, mode);
                    }
                }
            }
            Store::Hash(d) => {
                if let Some(idx) = d.find_int(ts, h) {
                    return self.existing(idx, val, mode);
                }
                if d.buckets.len() < ts as usize {
                    let idx = d.append(ts, ArrayKey::Int(h), val);
                    self.note_int_key(h);
                    self.num_elements += 1;
                    return Ok((idx, false));
                }
            }
            Store::Uninit => {}
        }
        self.index_insert_slow(h, val, mode)
    }

    /// The initialisation, hole, growth and conversion cases of
    /// [`HashTable::index_insert`] (`h` already mapped for `Mode::Next`).
    #[inline(never)]
    fn index_insert_slow(&mut self, h: i64, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match self.kind() {
            Kind::Packed => {
                let used = self.used() as usize;
                if let Some(hu) = usize::try_from(h).ok().filter(|&x| x < used) {
                    if self.is_occupied(hu) {
                        return self.existing(to_u32(hu), val, mode);
                    }
                    // Keep the order: convert to a hash. The conversion
                    // drops the hole at `h`, so the table has room; the
                    // resize check only guards the invariant.
                    self.packed_to_hash();
                    if self.used() >= self.table_size {
                        self.do_resize();
                    }
                } else if (0..i64::from(self.table_size)).contains(&h) {
                    return Ok((self.add_to_packed(h, val), false));
                } else if h >= 0
                    && (h >> 1) < i64::from(self.table_size)
                    && (self.table_size >> 1) < self.num_elements
                {
                    // zend_hash_packed_grow
                    self.table_size += self.table_size;
                    self.reserve_table();
                    return Ok((self.add_to_packed(h, val), false));
                } else {
                    if self.used() >= self.table_size {
                        self.table_size += self.table_size;
                    }
                    self.packed_to_hash();
                }
            }
            Kind::Uninit => {
                if (0..i64::from(self.table_size)).contains(&h) {
                    self.real_init_packed();
                    return Ok((self.add_to_packed(h, val), false));
                }
                self.real_init_mixed();
            }
            Kind::Hash => {
                if let Some(idx) = self.find_int(h) {
                    return self.existing(idx, val, mode);
                }
                if self.used() >= self.table_size {
                    self.do_resize();
                }
            }
        }
        Ok((self.hash_append(ArrayKey::Int(h), val), false))
    }

    /// An insert that found `key` at `idx`: `HASH_LOOKUP` returns it,
    /// `HASH_ADD` gives the value back, `HASH_UPDATE` overwrites.
    #[inline]
    fn existing(&mut self, idx: u32, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match mode {
            Mode::Lookup => Ok((idx, true)),
            Mode::Add | Mode::Next => Err(val),
            Mode::Update => {
                if let Some(slot) = self.val_mut(idx) {
                    *slot = val;
                }
                Ok((idx, true))
            }
        }
    }

    /// Port of `_zend_hash_add_or_update_i` for string keys (no numeric
    /// normalization). An existing key and a new key in a hash with room
    /// are decided here; initialisation, conversion and growth are
    /// [`HashTable::str_insert_slow`].
    #[inline]
    fn str_insert(&mut self, key: ZStr, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        let ts = self.table_size;
        if let Store::Hash(d) = &mut self.store {
            if let Some(idx) = d.find_zstr(ts, &key) {
                return self.existing(idx, val, mode);
            }
            if d.buckets.len() < ts as usize {
                let idx = d.append(ts, ArrayKey::Str(key), val);
                self.num_elements += 1;
                return Ok((idx, false));
            }
        }
        self.str_insert_slow(key, val, mode)
    }

    /// The initialisation, conversion and growth cases of
    /// [`HashTable::str_insert`].
    #[inline(never)]
    fn str_insert_slow(&mut self, key: ZStr, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match self.kind() {
            Kind::Uninit => self.real_init_mixed(),
            Kind::Packed => {
                self.packed_to_hash();
                if self.used() >= self.table_size {
                    self.do_resize();
                }
            }
            Kind::Hash => {
                if let Some(idx) = self.find_zstr(&key) {
                    return self.existing(idx, val, mode);
                }
                if self.used() >= self.table_size {
                    self.do_resize();
                }
            }
        }
        Ok((self.hash_append(ArrayKey::Str(key), val), false))
    }

    #[inline]
    fn key_insert(&mut self, key: ArrayKey, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match key {
            ArrayKey::Int(h) => self.index_insert(h, val, mode),
            ArrayKey::Str(s) => self.str_insert(s, val, mode),
        }
    }

    /// `zend_hash_lookup`: the position of `key` (one probe), inserted with
    /// `null` when absent, and whether it already existed.
    #[inline]
    fn lookup_idx(&mut self, key: ArrayKey) -> (u32, bool) {
        let Ok(r) = self.key_insert(key, Value::Null, Mode::Lookup) else {
            unreachable!("lookup never fails")
        };
        r
    }

    #[inline]
    fn val_at_mut(&mut self, idx: u32) -> &mut Value {
        match self.val_mut(idx) {
            Some(v) => v,
            None => unreachable!("bucket index returned by insert is always occupied"),
        }
    }

    /// Deletes the element at `idx` (`_zend_hash_del_el`), returning its value.
    fn del_at(&mut self, idx: u32) -> Option<Value> {
        let ts = self.table_size;
        let val = match &mut self.store {
            Store::Uninit => None,
            Store::Packed(v) => v.get_mut(idx as usize).and_then(Option::take),
            Store::Hash(d) => d.unlink_take(ts, idx),
        }?;
        self.num_elements -= 1;
        if idx.wrapping_add(1) == self.used() {
            match &mut self.store {
                Store::Uninit => {}
                Store::Packed(v) => {
                    while matches!(v.last(), Some(None)) {
                        v.pop();
                    }
                }
                Store::Hash(d) => {
                    while d.buckets.last().is_some_and(|b| b.val.is_none()) {
                        d.buckets.pop();
                    }
                }
            }
            let used = self.used();
            self.internal_ptr = self.internal_ptr.min(used);
            for it in &mut self.iterators {
                it.1 = it.1.min(used);
            }
        }
        Some(val)
    }

    // ------------------------------------------------------------------
    // Lookup
    // ------------------------------------------------------------------

    /// Looks up a key.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &ArrayKey) -> Option<&Value> {
        match key {
            ArrayKey::Int(i) => self.get_int(*i),
            ArrayKey::Str(s) => self.get_zstr(s),
        }
    }

    /// Looks up an integer key in a packed array (`Store::Packed`).
    /// Zero-overhead read avoiding string conversion and hashing.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the tape's packed `$a[$i]` read
    #[must_use]
    pub fn get_packed(&self, i: i64) -> Option<&Value> {
        if let Store::Packed(v) = &self.store {
            if i >= 0 {
                return v.get(i as usize)?.as_ref();
            }
        }
        None
    }

    /// Looks up an existing integer key mutably in a packed array (`Store::Packed`).
    #[inline(always)]
    #[allow(clippy::inline_always)] // the tape's packed `$a[$i] = ...` write
    pub fn get_packed_mut(&mut self, i: i64) -> Option<&mut Value> {
        if let Store::Packed(v) = &mut self.store {
            if i >= 0 {
                return v.get_mut(i as usize)?.as_mut();
            }
        }
        None
    }

    /// Assigns to an integer key in a packed array, overwriting an existing
    /// plain element or appending when at the boundary within table capacity.
    /// Returns `Ok(Some(old))` if overwritten, `Ok(None)` if appended, or
    /// `Err(val)` if the table is not packed, the key is outside packed range,
    /// points to a hole, or the existing element is not plain.
    #[inline]
    pub fn assign_packed_int(&mut self, h: i64, val: Value) -> Result<Option<Value>, Value> {
        let ts = self.table_size;
        match &mut self.store {
            Store::Packed(v) => {
                if h >= 0 {
                    let idx = h as usize;
                    let len = v.len();
                    if idx < len {
                        if let Some(slot) = &mut v[idx] {
                            if !slot.is_plain() {
                                return Err(val);
                            }
                            return Ok(Some(std::mem::replace(slot, val)));
                        }
                    } else if idx == len && h < i64::from(ts) {
                        v.push(Some(val));
                        self.next_free = h + 1;
                        self.num_elements += 1;
                        return Ok(None);
                    } else if idx == len
                        && (h >> 1) < i64::from(ts)
                        && (ts >> 1) < self.num_elements
                    {
                        self.table_size += self.table_size;
                        self.reserve_table();
                        if let Store::Packed(v) = &mut self.store {
                            v.push(Some(val));
                            self.next_free = h + 1;
                            self.num_elements += 1;
                            return Ok(None);
                        }
                    }
                }
            }
            Store::Uninit => {
                if (0..i64::from(ts)).contains(&h) {
                    self.real_init_packed();
                    let _ = self.add_to_packed(h, val);
                    return Ok(None);
                }
            }
            _ => {}
        }
        Err(val)
    }

    /// Looks up an integer key (`zend_hash_index_find`; a packed table
    /// indexes its values directly).
    #[inline]
    #[must_use]
    pub fn get_int(&self, i: i64) -> Option<&Value> {
        // The packed test first: one compare instead of decoding the
        // storage kind.
        if let Store::Packed(v) = &self.store {
            if i >= 0 {
                return v.get(i as usize)?.as_ref();
            }
            return None;
        }
        let Store::Hash(d) = &self.store else {
            return None;
        };
        let idx = d.find_int(self.table_size, i)?;
        d.buckets.get(idx as usize)?.val.as_ref()
    }

    /// `$a[h] = val` of a plain value (a scalar or a string) over an
    /// element that is absent or plain (`zend_hash_index_update` and the
    /// release of a value that owns nothing more than a string): one probe
    /// of the table, then the store in place, the append to a packed table
    /// or to a hash with room, or the general insert (initialisation, a
    /// hole, growth, conversion). `Err(val)`, nothing changed, when the
    /// element holds anything else: a reference to write through, or an
    /// array or object whose release is the caller's business.
    ///
    /// # Errors
    /// Gives `val` back when the existing element is not plain.
    #[inline]
    pub fn set_int_plain(&mut self, h: i64, val: Value) -> Result<(), Value> {
        let ts = self.table_size;
        match &mut self.store {
            Store::Packed(v) => {
                if let Ok(i) = usize::try_from(h) {
                    let len = v.len();
                    if i < len {
                        if let Some(slot) = &mut v[i] {
                            if !slot.is_plain() {
                                return Err(val);
                            }
                            *slot = val;
                            return Ok(());
                        }
                    } else if i == len && h < i64::from(ts) {
                        v.push(Some(val));
                        self.next_free = h + 1;
                        self.num_elements += 1;
                        return Ok(());
                    }
                }
            }
            Store::Hash(d) => {
                if let Some(idx) = d.find_int(ts, h) {
                    if let Some(slot) = d.buckets.get_mut(idx as usize).and_then(|b| b.val.as_mut())
                    {
                        if !slot.is_plain() {
                            return Err(val);
                        }
                        *slot = val;
                    }
                    return Ok(());
                }
                if d.buckets.len() < ts as usize {
                    d.append(ts, ArrayKey::Int(h), val);
                    self.note_int_key(h);
                    self.num_elements += 1;
                    return Ok(());
                }
            }
            Store::Uninit => {}
        }
        // A new key from here on (an existing one was decided above).
        self.index_insert_slow(h, val, Mode::Update).map(|_| ())
    }

    /// `$a[key] = val` of a plain value (a scalar or a string) for a string
    /// offset with symbol-table semantics (`zend_symtable_update`: `"12"` is
    /// the int key 12): one probe of the table, then the store over a plain
    /// element, the append to a hash with room (under one more handle of
    /// `key`), or the general insert (initialisation, conversion of a list,
    /// growth).
    ///
    /// # Errors
    /// Gives `val` back, nothing changed, when the existing element is not
    /// plain: a reference to write through, or an array or object whose
    /// release is the caller's business.
    #[inline]
    pub fn set_sym_plain(&mut self, key: &ZStr, val: Value) -> Result<(), Value> {
        if let Some(h) = numeric_key(key.as_bytes()) {
            let (slot, added) = self.lookup_ex(ArrayKey::Int(h));
            if !added && !slot.is_plain() {
                return Err(val);
            }
            *slot = val;
            return Ok(());
        }
        let ts = self.table_size;
        if let Store::Hash(d) = &mut self.store {
            if let Some(idx) = d.probe_zstr(ts, key) {
                if let Some(slot) = d.buckets.get_mut(idx as usize).and_then(|b| b.val.as_mut()) {
                    if !slot.is_plain() {
                        return Err(val);
                    }
                    *slot = val;
                }
                return Ok(());
            }
            if d.buckets.len() < ts as usize {
                d.append(ts, ArrayKey::Str(key.clone()), val);
                self.num_elements += 1;
                return Ok(());
            }
        }
        // A new key from here on (an existing one was decided above).
        self.str_insert_slow(key.clone(), val, Mode::Update)
            .map(|_| ())
    }

    /// Mutable lookup of an integer key (one probe, no second storage
    /// match for the slot).
    #[inline]
    pub fn get_int_mut(&mut self, i: i64) -> Option<&mut Value> {
        let ts = self.table_size;
        match &mut self.store {
            Store::Uninit => None,
            Store::Packed(v) => {
                if i >= 0 {
                    v.get_mut(i as usize)?.as_mut()
                } else {
                    None
                }
            }
            Store::Hash(d) => {
                let idx = d.find_int(ts, i)?;
                d.buckets.get_mut(idx as usize)?.val.as_mut()
            }
        }
    }

    /// Looks up an exact string key given as a [`ZStr`] (no numeric
    /// normalization; `zend_hash_find`): the very same string instance
    /// matches without comparing bytes.
    #[inline]
    #[must_use]
    pub fn get_zstr(&self, s: &ZStr) -> Option<&Value> {
        let Store::Hash(d) = &self.store else {
            return None;
        };
        let idx = d.find_zstr(self.table_size, s)?;
        d.buckets.get(idx as usize)?.val.as_ref()
    }

    /// Looks up a string offset with symbol-table semantics
    /// (`zend_symtable_find`: a canonical integer string finds the integer
    /// key), as `get(&ArrayKey::from_zstr(s))` without building the key.
    #[inline]
    #[must_use]
    pub fn get_sym_zstr(&self, s: &ZStr) -> Option<&Value> {
        match numeric_key(s.as_bytes()) {
            Some(i) => self.get_int(i),
            None => self.get_zstr(s),
        }
    }

    /// [`HashTable::get_sym_zstr`] with the probe inlined into the caller:
    /// for an element fast path of the executor, whose body the lookup is.
    #[inline(always)]
    #[allow(clippy::inline_always)] // see above
    #[must_use]
    pub fn sym_zstr_probe(&self, s: &ZStr) -> Option<&Value> {
        if let Some(i) = numeric_key(s.as_bytes()) {
            return self.get_int(i);
        }
        let Store::Hash(d) = &self.store else {
            return None;
        };
        let idx = d.probe_zstr(self.table_size, s)?;
        d.buckets.get(idx as usize)?.val.as_ref()
    }

    /// Mutable [`HashTable::sym_zstr_probe`].
    #[inline(always)]
    #[allow(clippy::inline_always)] // as `sym_zstr_probe`
    pub fn sym_zstr_probe_mut(&mut self, s: &ZStr) -> Option<&mut Value> {
        if let Some(i) = numeric_key(s.as_bytes()) {
            return self.get_int_mut(i);
        }
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return None;
        };
        let idx = d.probe_zstr(ts, s)?;
        d.buckets.get_mut(idx as usize)?.val.as_mut()
    }

    /// The element a string key names in a hash, for a lean caller that
    /// leaves every other case to a general path: `None` not only when
    /// there is no such element, but also for a key whose first byte is a
    /// digit or `-` (it may be an integer in digits), a key whose hash is
    /// not computed yet, and a table that is no hash. It makes no call, so
    /// a caller that makes none either saves no register around it.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_zstr_probe_mut(&mut self, key: &ZStr) -> Option<&mut Value> {
        let first = *key.as_bytes().first()?;
        if first.is_ascii_digit() || first == b'-' {
            return None;
        }
        let h = key.known_hash()?;
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return None;
        };
        let mut idx = d.head(ts, h);
        while idx != INVALID {
            let b = d.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if ZStr::ptr_eq(k, key)
                    || (k.known_hash() == Some(h) && bytes_eq(k.as_bytes(), key.as_bytes()))
                {
                    return d.buckets.get_mut(idx as usize)?.val.as_mut();
                }
            }
            idx = b.next;
        }
        None
    }

    /// The element under the string key `key` (whose hash is `h`, and
    /// which is no integer in digits: [`ArrayKey::surely_str`]) of a hash,
    /// found without a call; `None` also for a table that is no hash.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    #[must_use]
    pub fn lean_find(&self, key: &ZStr, h: u64) -> Option<&Value> {
        let Store::Hash(d) = &self.store else {
            return None;
        };
        let idx = d.lean_probe(self.table_size, key, h)?;
        d.buckets.get(idx as usize)?.val.as_ref()
    }

    /// The element under the int key `i`, found without a call: a list's
    /// slot or a hash's bucket.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    #[must_use]
    pub fn lean_get_int(&self, i: i64) -> Option<&Value> {
        match &self.store {
            Store::Packed(v) => v.get(usize::try_from(i).ok()?)?.as_ref(),
            Store::Hash(d) => {
                let idx = d.lean_probe_int(self.table_size, i)?;
                d.buckets.get(idx as usize)?.val.as_ref()
            }
            Store::Uninit => None,
        }
    }

    /// Mutable [`HashTable::lean_get_int`].
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_get_int_mut(&mut self, i: i64) -> Option<&mut Value> {
        let ts = self.table_size;
        match &mut self.store {
            Store::Packed(v) => v.get_mut(usize::try_from(i).ok()?)?.as_mut(),
            Store::Hash(d) => {
                let idx = d.lean_probe_int(ts, i)?;
                d.buckets.get_mut(idx as usize)?.val.as_mut()
            }
            Store::Uninit => None,
        }
    }

    /// Adds `val` under the int key `i`, which the caller found absent
    /// ([`HashTable::lean_get_int_mut`]), to a hash with room for it,
    /// without a call (and the next free index moves past `i`).
    ///
    /// # Errors
    /// Gives the value back, nothing changed, for a table that is no hash
    /// or is full: the general insert's.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_add_int(&mut self, i: i64, val: Value) -> Result<(), Value> {
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return Err(val);
        };
        let used = d.buckets.len();
        if used >= ts as usize || used == d.buckets.capacity() {
            return Err(val);
        }
        #[allow(clippy::cast_sign_loss)]
        let Some(slot) = d.index.get_mut(hash_slot(ts, i as u64)) else {
            return Err(val);
        };
        let head = std::mem::replace(slot, to_u32(used));
        d.buckets.push(Bucket::new(ArrayKey::Int(i), val, head));
        self.num_elements += 1;
        self.note_int_key(i);
        Ok(())
    }

    /// Appends `val` under the next index to a list with room for it,
    /// without a call: the packed append of `zend_hash_next_index_insert`
    /// (`ZEND_ADD_ARRAY_ELEMENT` with no key) for a table that neither
    /// grows nor changes its kind. The same table as [`HashTable::push`]
    /// makes.
    ///
    /// # Errors
    /// Gives the value back, nothing changed, for a table that is no list,
    /// is full, or whose next index is not its end (a hole before it): the
    /// general [`HashTable::push`]'s.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_push(&mut self, val: Value) -> Result<(), Value> {
        let ts = self.table_size as usize;
        let next = if self.next_free == i64::MIN {
            0
        } else {
            self.next_free
        };
        let Store::Packed(v) = &mut self.store else {
            return Err(val);
        };
        let len = v.len();
        if len >= ts || len == v.capacity() || next != idx_key(len) {
            return Err(val);
        }
        v.push(Some(val));
        // `next < nTableSize`, so no overflow.
        self.next_free = next + 1;
        self.num_elements += 1;
        Ok(())
    }

    /// Mutable [`HashTable::lean_find`].
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_find_mut(&mut self, key: &ZStr, h: u64) -> Option<&mut Value> {
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return None;
        };
        let idx = d.lean_probe(ts, key, h)?;
        d.buckets.get_mut(idx as usize)?.val.as_mut()
    }

    /// Adds the element `val` under the string key `key` (whose hash is
    /// `h`), which the caller found absent ([`HashTable::lean_find`]), to
    /// a hash with room for it, without a call: the tail of
    /// `_zend_hash_add_or_update_i` for a table that neither grows nor
    /// changes its kind.
    ///
    /// # Errors
    /// Gives the key and the value back, nothing changed, for a table that
    /// is no hash or is full: the general insert's.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    pub fn lean_append(&mut self, key: ZStr, h: u64, val: Value) -> Result<(), (ZStr, Value)> {
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return Err((key, val));
        };
        let used = d.buckets.len();
        if used >= ts as usize || used == d.buckets.capacity() {
            return Err((key, val));
        }
        let Some(slot) = d.index.get_mut(hash_slot(ts, h)) else {
            return Err((key, val));
        };
        let head = std::mem::replace(slot, to_u32(used));
        d.buckets.push(Bucket::new(ArrayKey::Str(key), val, head));
        self.num_elements += 1;
        Ok(())
    }

    /// Mutable [`HashTable::get_sym_zstr`].
    #[inline]
    pub fn get_sym_zstr_mut(&mut self, s: &ZStr) -> Option<&mut Value> {
        if let Some(i) = numeric_key(s.as_bytes()) {
            return self.get_int_mut(i);
        }
        let ts = self.table_size;
        let Store::Hash(d) = &mut self.store else {
            return None;
        };
        let idx = d.find_zstr(ts, s)?;
        d.buckets.get_mut(idx as usize)?.val.as_mut()
    }

    /// Looks up a string key with identical pointer check first (`zend_hash_find`).
    #[inline]
    #[must_use]
    pub fn get_str_identical_val(&self, key: &ZStr) -> Option<&Value> {
        self.get_zstr(key)
    }

    /// Looks up an exact string key (no numeric normalization;
    /// `zend_hash_str_find`).
    #[must_use]
    pub fn get_str(&self, s: &[u8]) -> Option<&Value> {
        let idx = self.find_str(s, crate::string::zend_hash_bytes(s))?;
        self.val_ref(idx)
    }

    /// Looks up a string key with symbol-table semantics
    /// (`zend_symtable_find`: `"1"` finds key `1`).
    #[must_use]
    pub fn get_sym(&self, s: &[u8]) -> Option<&Value> {
        match handle_numeric_str(s) {
            Some(i) => self.get_int(i),
            None => self.get_str(s),
        }
    }

    /// Mutable lookup.
    #[inline]
    pub fn get_mut(&mut self, key: &ArrayKey) -> Option<&mut Value> {
        match key {
            ArrayKey::Int(i) => self.get_int_mut(*i),
            ArrayKey::Str(s) => {
                let ts = self.table_size;
                let Store::Hash(d) = &mut self.store else {
                    return None;
                };
                let idx = d.find_zstr(ts, s)?;
                d.buckets.get_mut(idx as usize)?.val.as_mut()
            }
        }
    }

    /// Whether the key exists.
    #[must_use]
    pub fn contains_key(&self, key: &ArrayKey) -> bool {
        self.find(key).is_some()
    }

    /// The position (bucket index) of a key.
    #[must_use]
    pub fn position_of(&self, key: &ArrayKey) -> Option<u32> {
        self.find(key)
    }

    // ------------------------------------------------------------------
    // Insertion / deletion
    // ------------------------------------------------------------------

    /// Inserts or replaces (`zend_hash_update`); returns the old value.
    pub fn insert(&mut self, key: impl Into<ArrayKey>, val: Value) -> Option<Value> {
        let (idx, existed) = self.lookup_idx(key.into());
        let slot = self.val_at_mut(idx);
        if existed {
            Some(std::mem::replace(slot, val))
        } else {
            *slot = val;
            None
        }
    }

    /// Inserts only if absent (`zend_hash_add`); gives the value back if the
    /// key already exists.
    pub fn add(&mut self, key: impl Into<ArrayKey>, val: Value) -> Result<(), Value> {
        self.key_insert(key.into(), val, Mode::Add).map(|_| ())
    }

    /// Appends with the next free integer key (`zend_hash_next_index_insert`,
    /// `$a[] = v`). Returns the key used, or gives the value back when the
    /// next key is already occupied ("Cannot add element to the array as the
    /// next element is already occupied").
    #[inline]
    pub fn push(&mut self, val: Value) -> Result<i64, Value> {
        let h = self.next_index();
        self.index_insert(h, val, Mode::Next)?;
        Ok(h)
    }

    /// Appends every value of `vals` in order, as [`HashTable::push`] of
    /// each would (`ZEND_HASH_FILL_PACKED`): into a list without holes,
    /// as many as it has room for in one `Vec` extension, the rest (and
    /// all of them for any other table) one by one. Gives back the first
    /// value with no free next key (left unpushed, with those after it).
    pub fn push_all<I: IntoIterator<Item = Value>>(&mut self, vals: I) -> Result<(), Value> {
        let mut it = vals.into_iter();
        let ts = self.table_size as usize;
        if let Store::Packed(v) = &mut self.store {
            let len = v.len();
            let next = if self.next_free == i64::MIN {
                0
            } else {
                self.next_free
            };
            if usize::try_from(next).is_ok_and(|x| x == len) && len < ts {
                v.extend(it.by_ref().take(ts - len).map(Some));
                let added = v.len() - len;
                if added > 0 {
                    self.next_free = i64::try_from(v.len()).unwrap_or(i64::MAX);
                    self.num_elements += to_u32(added);
                }
            }
        }
        for val in it {
            self.push(val)?;
        }
        Ok(())
    }

    /// Like [`HashTable::push`] but returns a mutable reference to the new slot.
    pub fn push_mut(&mut self, val: Value) -> Result<&mut Value, Value> {
        let h = self.next_free;
        let (idx, _) = self.index_insert(h, val, Mode::Next)?;
        Ok(self.val_at_mut(idx))
    }

    /// Returns the slot for `key`, inserting `null` when absent
    /// (`zend_hash_lookup`, used for write fetches like `$a[k][] = ...`).
    pub fn lookup(&mut self, key: impl Into<ArrayKey>) -> &mut Value {
        self.lookup_ex(key).0
    }

    /// Like [`HashTable::lookup`] and also reports whether the key was newly
    /// inserted.
    #[inline]
    pub fn lookup_ex(&mut self, key: impl Into<ArrayKey>) -> (&mut Value, bool) {
        let (idx, existed) = self.lookup_idx(key.into());
        (self.val_at_mut(idx), !existed)
    }

    /// Removes a key, returning its value (`zend_hash_del`).
    pub fn remove(&mut self, key: &ArrayKey) -> Option<Value> {
        let idx = self.find(key)?;
        self.del_at(idx)
    }

    /// Removes the element at a position, returning key and value.
    pub fn remove_at(&mut self, pos: u32) -> Option<(ArrayKey, Value)> {
        let key = self.key_at(pos)?;
        self.del_at(pos).map(|v| (key, v))
    }

    /// Removes all elements (`zend_hash_clean`).
    pub fn clear(&mut self) {
        match &mut self.store {
            Store::Uninit => {}
            Store::Packed(v) => v.clear(),
            Store::Hash(d) => {
                d.buckets.clear();
                d.index.fill(INVALID);
            }
        }
        self.num_elements = 0;
        self.next_free = i64::MIN;
        self.internal_ptr = 0;
    }

    // ------------------------------------------------------------------
    // Iteration and positions
    // ------------------------------------------------------------------

    /// Iterates `(key, value)` pairs in order.
    #[must_use]
    pub fn iter(&self) -> Iter<'_> {
        let empty: &[Option<Value>] = &[];
        let inner = match &self.store {
            Store::Uninit => IterInner::Packed {
                slots: empty.iter(),
                front: 0,
                back: 0,
                keys: &[],
                keys_start: 0,
            },
            Store::Packed(v) => IterInner::Packed {
                slots: v.iter(),
                front: 0,
                back: to_u32(v.len()),
                keys: &[],
                keys_start: 0,
            },
            Store::Hash(d) => IterInner::Hash(d.buckets.iter()),
        };
        Iter {
            inner,
            remaining: self.num_elements as usize,
        }
    }

    /// Replaces every string key that is not interned by the interned
    /// instance of its bytes (`zend_new_interned_string` on an array's
    /// keys, as php-src's compiler leaves a constant array's). The keys,
    /// their hashes and their order are the same, so nothing about the
    /// table changes but which string instances it holds: a lookup by an
    /// interned key (every literal) then finds its bucket by pointer, with
    /// no byte comparison.
    pub fn intern_str_keys(&mut self) {
        let Store::Hash(d) = &mut self.store else {
            return;
        };
        for b in &mut d.buckets {
            if b.val.is_none() {
                continue;
            }
            if let ArrayKey::Str(k) = &mut b.key {
                if !k.is_interned() {
                    *k = k.interned();
                }
            }
        }
    }

    /// Iterates `(key, &mut value)` pairs in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ArrayKey, &mut Value)> {
        let (packed, buckets): (&mut [Option<Value>], &mut [Bucket]) = match &mut self.store {
            Store::Uninit => (Default::default(), Default::default()),
            Store::Packed(v) => (v.as_mut_slice(), Default::default()),
            Store::Hash(d) => (Default::default(), d.buckets.as_mut_slice()),
        };
        // The chunk of shared keys last used, as in `Iter`.
        let mut keys: &'static [ArrayKey] = &[];
        let mut keys_start: u32 = 0;
        packed
            .iter_mut()
            .enumerate()
            .filter_map(move |(i, slot)| {
                let v = slot.as_mut()?;
                let key: &ArrayKey = packed_key(&mut keys, &mut keys_start, to_u32(i));
                Some((key, v))
            })
            .chain(buckets.iter_mut().filter_map(|b| {
                let Bucket { val, key, .. } = b;
                let key: &ArrayKey = key;
                val.as_mut().map(|v| (key, v))
            }))
    }

    /// Iterates keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &ArrayKey> {
        self.iter().map(|(k, _)| k)
    }

    /// Calls `f` with every element that is an array, an object or a
    /// reference, in order: the elements whose release is more than
    /// forgetting a scalar or releasing a string (what the collector looks
    /// at when a table is released). A loop over the storage itself: a
    /// table of scalars costs a tag test an element.
    ///
    /// The variants array, object, resource and reference are adjacent, so
    /// the loop tests one range of tags (one compare an element, where the
    /// three-variant pattern took two and a bit test); the rare resource is
    /// sorted out behind it.
    pub fn for_each_counted<'a>(&'a self, mut f: impl FnMut(&'a Value)) {
        let mut visit = |slot: &'a Option<Value>| {
            if let Some(
                v @ (Value::Array(_) | Value::Object(_) | Value::Resource(_) | Value::Reference(_)),
            ) = slot
            {
                if !matches!(v, Value::Resource(_)) {
                    f(v);
                }
            }
        };
        match &self.store {
            Store::Uninit => {}
            Store::Packed(values) => values.iter().for_each(&mut visit),
            Store::Hash(data) => data.buckets.iter().for_each(|b| visit(&b.val)),
        }
    }

    /// Iterates values in order (no keys, so a packed table never touches
    /// the shared key table).
    #[must_use]
    pub fn values(&self) -> Values<'_> {
        let empty: &[Option<Value>] = &[];
        let inner = match &self.store {
            Store::Uninit => ValuesInner::Packed(empty.iter()),
            Store::Packed(v) => ValuesInner::Packed(v.iter()),
            Store::Hash(d) => ValuesInner::Hash(d.buckets.iter()),
        };
        Values {
            inner,
            remaining: self.num_elements as usize,
        }
    }

    /// First valid position at or after `pos` (`_zend_hash_get_valid_pos`);
    /// returns [`HashTable::used`] when there is none, and `pos` itself
    /// when it lies past the end.
    #[inline]
    #[must_use]
    pub fn valid_pos(&self, pos: u32) -> u32 {
        // Packed first: one compare instead of decoding the storage kind.
        if let Store::Packed(v) = &self.store {
            return first_live(v, pos, Option::is_some);
        }
        if let Store::Hash(d) = &self.store {
            first_live(&d.buckets, pos, |b| b.val.is_some())
        } else {
            pos
        }
    }

    /// One `ZEND_FE_FETCH_R` step over the table: the first element at or
    /// after `pos`, as its position and value, or `None` past the last one.
    /// The caller stores `position + 1` as the next `pos`, and takes the key
    /// from [`HashTable::key_value_at`] when the loop has a key variable:
    /// neither touches the shared packed-key table [`HashTable::at`] lends
    /// keys from (php-src's packed `FE_FETCH_R` yields the position as the
    /// key).
    #[inline]
    #[must_use]
    pub fn fe_fetch(&self, pos: u32) -> Option<(u32, &Value)> {
        let start = pos as usize;
        if let Store::Packed(v) = &self.store {
            return v
                .get(start..)?
                .iter()
                .enumerate()
                .find_map(|(i, s)| s.as_ref().map(|x| (to_u32(start + i), x)));
        }
        let Store::Hash(d) = &self.store else {
            return None;
        };
        d.buckets
            .get(start..)?
            .iter()
            .enumerate()
            .find_map(|(i, b)| b.val.as_ref().map(|x| (to_u32(start + i), x)))
    }

    /// [`HashTable::fe_fetch`] that also yields the element's key as a
    /// PHP value (`ZEND_FE_FETCH_R` with a key result): the same slot
    /// serves both, as [`HashTable::key_value_at`] at the returned position
    /// would (a packed table's key is the position, a bucket's key is
    /// cloned once), without a second lookup of the slot.
    #[inline]
    #[must_use]
    pub fn fe_fetch_with_key(&self, pos: u32) -> Option<(u32, &Value, Value)> {
        let start = pos as usize;
        if let Store::Packed(v) = &self.store {
            return v.get(start..)?.iter().enumerate().find_map(|(i, s)| {
                s.as_ref().map(|x| {
                    let p = to_u32(start + i);
                    (p, x, Value::Int(i64::from(p)))
                })
            });
        }
        let Store::Hash(d) = &self.store else {
            return None;
        };
        d.buckets
            .get(start..)?
            .iter()
            .enumerate()
            .find_map(|(i, b)| {
                b.val
                    .as_ref()
                    .map(|x| (to_u32(start + i), x, b.key.to_value()))
            })
    }

    /// The value at exactly position `pos` and its key, both borrowed: the
    /// key of a bucket, or `None` in a list, whose key is the position.
    /// The element a `foreach` step takes when no hole lies before it;
    /// `None` for a hole and at the end ([`HashTable::fe_fetch_with_key`]
    /// has those).
    #[inline(always)]
    #[allow(clippy::inline_always)] // the body of a lean handler
    #[must_use]
    pub fn entry_at(&self, pos: u32) -> Option<(&Value, Option<&ArrayKey>)> {
        match &self.store {
            Store::Hash(d) => {
                let b = d.buckets.get(pos as usize)?;
                Some((b.val.as_ref()?, Some(&b.key)))
            }
            Store::Packed(v) => Some((v.get(pos as usize)?.as_ref()?, None)),
            Store::Uninit => None,
        }
    }

    /// The entry at a position (holes and out-of-range give `None`).
    #[inline]
    #[must_use]
    pub fn at(&self, pos: u32) -> Option<(&ArrayKey, &Value)> {
        if let Store::Packed(v) = &self.store {
            return v
                .get(pos as usize)?
                .as_ref()
                .map(|x| (int_keys::get(pos), x));
        }
        let Store::Hash(d) = &self.store else {
            return None;
        };
        let b = d.buckets.get(pos as usize)?;
        b.val.as_ref().map(|x| (&b.key, x))
    }

    /// The key at a position, by value (holes and out-of-range give
    /// `None`); unlike [`HashTable::at`] it never touches the shared
    /// packed-key table.
    #[inline]
    #[must_use]
    pub fn key_at(&self, pos: u32) -> Option<ArrayKey> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => v
                .get(pos as usize)?
                .as_ref()
                .map(|_| ArrayKey::Int(i64::from(pos))),
            Store::Hash(d) => {
                let b = d.buckets.get(pos as usize)?;
                b.val.as_ref().map(|_| b.key.clone())
            }
        }
    }

    /// The key at a position as a PHP value (`int` or `string`; holes and
    /// out-of-range give `None`), cloned once; like [`HashTable::key_at`]
    /// it never touches the shared packed-key table.
    #[inline]
    #[must_use]
    pub fn key_value_at(&self, pos: u32) -> Option<Value> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => v
                .get(pos as usize)?
                .as_ref()
                .map(|_| Value::Int(i64::from(pos))),
            Store::Hash(d) => {
                let b = d.buckets.get(pos as usize)?;
                b.val.as_ref().map(|_| b.key.to_value())
            }
        }
    }

    /// The value at a position (holes and out-of-range give `None`) without
    /// its key: for `foreach` without a key variable and other positional
    /// walks that need no key, this never touches the shared packed-key
    /// table that [`HashTable::at`] lends keys from.
    #[inline]
    #[must_use]
    pub fn value_at(&self, pos: u32) -> Option<&Value> {
        self.val_ref(pos)
    }

    /// Mutable access to the value at a position.
    #[inline]
    pub fn at_mut(&mut self, pos: u32) -> Option<&mut Value> {
        self.val_mut(pos)
    }

    /// The first element.
    #[must_use]
    pub fn first(&self) -> Option<(&ArrayKey, &Value)> {
        self.iter().next()
    }

    /// The last element.
    #[must_use]
    pub fn last(&self) -> Option<(&ArrayKey, &Value)> {
        self.iter().next_back()
    }

    // ------------------------------------------------------------------
    // Internal pointer (current/next/prev/reset/end/key)
    // ------------------------------------------------------------------

    /// The raw internal pointer (`nInternalPointer`).
    #[must_use]
    pub fn internal_pointer(&self) -> u32 {
        self.internal_ptr
    }

    /// Sets the raw internal pointer.
    pub fn set_internal_pointer(&mut self, pos: u32) {
        self.internal_ptr = pos;
    }

    /// The current valid position of the internal pointer.
    #[must_use]
    pub fn current_pos(&self) -> u32 {
        self.valid_pos(self.internal_ptr)
    }

    /// `current()`: the element at the internal pointer.
    #[must_use]
    pub fn current(&self) -> Option<&Value> {
        self.val_ref(self.current_pos())
    }

    /// `key()`: the key at the internal pointer.
    #[must_use]
    pub fn current_key(&self) -> Option<&ArrayKey> {
        self.at(self.current_pos()).map(|(k, _)| k)
    }

    /// `reset()`: moves the internal pointer to the first element.
    pub fn reset(&mut self) {
        self.internal_ptr = self.valid_pos(0);
    }

    /// `end()`: moves the internal pointer to the last element.
    pub fn end(&mut self) {
        self.internal_ptr = self.end_pos();
    }

    fn end_pos(&self) -> u32 {
        let mut idx = self.used() as usize;
        while idx > 0 {
            idx -= 1;
            if self.is_occupied(idx) {
                return to_u32(idx);
            }
        }
        self.used()
    }

    /// `next()`: advances the internal pointer (`zend_hash_move_forward`).
    pub fn move_next(&mut self) {
        self.internal_ptr = self.pos_forward(self.internal_ptr);
    }

    /// `prev()`: moves the internal pointer back (`zend_hash_move_backwards`).
    pub fn move_prev(&mut self) {
        self.internal_ptr = self.pos_backward(self.internal_ptr);
    }

    /// `zend_hash_move_forward_ex` on an external position.
    #[must_use]
    pub fn pos_forward(&self, pos: u32) -> u32 {
        let used = self.used() as usize;
        let mut idx = self.valid_pos(pos) as usize;
        if idx >= used {
            return pos;
        }
        loop {
            idx += 1;
            if idx >= used {
                return self.used();
            }
            if self.is_occupied(idx) {
                return to_u32(idx);
            }
        }
    }

    /// `zend_hash_move_backwards_ex` on an external position.
    #[must_use]
    pub fn pos_backward(&self, pos: u32) -> u32 {
        let mut idx = pos as usize;
        if idx >= self.used() as usize {
            return pos;
        }
        while idx > 0 {
            idx -= 1;
            if self.is_occupied(idx) {
                return to_u32(idx);
            }
        }
        self.used()
    }

    // ------------------------------------------------------------------
    // Registered iterators (foreach by reference)
    // ------------------------------------------------------------------

    /// Registers an iterator at `pos` (`zend_hash_iterator_add`).
    pub fn iterator_add(&mut self, pos: u32) -> IterId {
        let id = NEXT_ITER_ID.with(|c| {
            let v = c.get();
            c.set(v.wrapping_add(1));
            IterId(v)
        });
        self.iterators.push((id, pos));
        id
    }

    /// The position of an iterator (`zend_hash_iterator_pos`). If this table
    /// does not know the iterator (the variable now holds a different array)
    /// it is attached at the internal pointer's current position.
    pub fn iterator_pos(&mut self, id: IterId) -> u32 {
        if let Some(it) = self.iterators.iter().find(|it| it.0 == id) {
            return it.1;
        }
        let pos = self.current_pos();
        self.iterators.push((id, pos));
        pos
    }

    /// Sets an iterator's position.
    pub fn iterator_set_pos(&mut self, id: IterId, pos: u32) {
        match self.iterators.iter_mut().find(|it| it.0 == id) {
            Some(it) => it.1 = pos,
            None => self.iterators.push((id, pos)),
        }
    }

    /// Unregisters an iterator (`zend_hash_iterator_del`).
    pub fn iterator_del(&mut self, id: IterId) {
        self.iterators.retain(|it| it.0 != id);
    }

    /// Whether any iterator is registered (`HT_HAS_ITERATORS`).
    #[must_use]
    pub fn has_iterators(&self) -> bool {
        !self.iterators.is_empty()
    }

    /// One step of `ZEND_FE_FETCH_RW`: finds the next element at or after the
    /// iterator's position, advances the iterator past it and returns its
    /// position, or `None` at the end.
    pub fn iterator_fetch(&mut self, id: IterId) -> Option<u32> {
        let pos = self.iterator_pos(id);
        let p = self.valid_pos(pos);
        if p >= self.used() {
            return None;
        }
        self.iterator_set_pos(id, p + 1);
        Some(p)
    }

    /// `zend_hash_iterators_lower_pos`: the smallest registered iterator
    /// position `>= start` (or [`HashTable::used`] if none). Use together
    /// with [`HashTable::iterators_update`] to remap iterators while
    /// rebuilding a table, exactly like `php_splice`/`array_shift` do.
    #[must_use]
    pub fn iterators_lower_pos(&self, start: u32) -> u32 {
        let mut res = self.used();
        for it in &self.iterators {
            if it.1 >= start && it.1 < res {
                res = it.1;
            }
        }
        res
    }

    /// `zend_hash_iterators_update`: moves every iterator at `from` to `to`.
    pub fn iterators_update(&mut self, from: u32, to: u32) {
        for it in &mut self.iterators {
            if it.1 == from {
                it.1 = to;
            }
        }
    }

    /// `zend_hash_iterators_advance`: adds `step` to every iterator position
    /// (used by `array_unshift`).
    pub fn iterators_advance(&mut self, step: u32) {
        for it in &mut self.iterators {
            it.1 = it.1.wrapping_add(step);
        }
    }

    /// Registered iterator positions as `(id, pos)` pairs.
    #[must_use]
    pub fn iterator_positions(&self) -> &[(IterId, u32)] {
        &self.iterators
    }

    /// Moves the registered iterators of `other` into this table (used after
    /// building a replacement table, like `HT_SET_ITERATORS_COUNT` swaps).
    pub fn take_iterators_from(&mut self, other: &mut HashTable) {
        self.iterators.append(&mut other.iterators);
    }

    // ------------------------------------------------------------------
    // Bulk operations
    // ------------------------------------------------------------------

    /// `zend_hash_merge`: copies all elements of `source` into `self`.
    /// With `overwrite == false` existing keys are kept (the array `+`
    /// operator); with `true` they are replaced.
    pub fn merge(&mut self, source: &HashTable, overwrite: bool) {
        for (k, v) in source {
            let v = copy_for_insert(v);
            if overwrite {
                self.insert(k.clone(), v);
            } else {
                let _ = self.add(k.clone(), v);
            }
        }
    }

    /// Sorts the table with PHP's `zend_sort`, breaking ties on the original
    /// order (stable, as all PHP 8 sort functions are). With `renumber` the
    /// keys become `0..n` (`sort`, `usort`, ...); otherwise keys are kept
    /// (`asort`, `ksort`, `uasort`, ...). The comparator receives
    /// `(key_a, value_a, key_b, value_b)`; only `> 0` / `== 0` matter.
    pub fn sort_by<F>(&mut self, mut cmp: F, renumber: bool)
    where
        F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32,
    {
        self.sort_impl(
            &mut |a: &SortItem, b: &SortItem| {
                let r = cmp(&a.key, &a.val, &b.key, &b.val);
                if r != 0 {
                    r
                } else {
                    i32::from(a.pos > b.pos) - i32::from(a.pos < b.pos)
                }
            },
            renumber,
        );
    }

    /// `sort()` / `rsort()` of a table whose every value is an int (the
    /// values ordered as `zend_compare` orders two ints, the keys
    /// renumbered `0..n`): what [`HashTable::sort_by`] makes of it with
    /// that comparison, produced by sorting the bare numbers. Equal ints
    /// cannot be told apart, so the stable order needs no tie-break here.
    /// `false`, nothing changed, when some value is not an int (the general
    /// sort compares it) and for a table of fewer than two elements (whose
    /// early exits are the general sort's).
    pub fn sort_ints(&mut self, reverse: bool) -> bool {
        if self.num_elements <= 1 {
            return false;
        }
        let mut nums: Vec<i64> = Vec::with_capacity(self.num_elements as usize);
        match &self.store {
            Store::Uninit => return false,
            Store::Packed(v) => {
                for slot in v {
                    match slot {
                        Some(Value::Int(i)) => nums.push(*i),
                        Some(_) => return false,
                        None => {}
                    }
                }
            }
            Store::Hash(d) => {
                for b in &d.buckets {
                    match &b.val {
                        Some(Value::Int(i)) => nums.push(*i),
                        Some(_) => return false,
                        None => {}
                    }
                }
            }
        }
        nums.sort_unstable();
        if reverse {
            nums.reverse();
        }
        // As `sort_finish` leaves a renumbered table.
        self.internal_ptr = 0;
        let n = nums.len();
        let cap = (self.table_size as usize).max(n);
        let mut vals = Vec::with_capacity(cap);
        vals.extend(nums.into_iter().map(|i| Some(Value::Int(i))));
        self.store = Store::Packed(vals);
        self.next_free = idx_key(n);
        true
    }

    /// Like [`HashTable::sort_by`] without the stability tie-break.
    pub fn sort_unstable_by<F>(&mut self, mut cmp: F, renumber: bool)
    where
        F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32,
    {
        self.sort_impl(
            &mut |a: &SortItem, b: &SortItem| cmp(&a.key, &a.val, &b.key, &b.val),
            renumber,
        );
    }

    /// Port of `zend_hash_sort_internal`.
    fn sort_impl(&mut self, cmp: &mut SortCmp<'_>, renumber: bool) {
        let Some(mut items) = self.sort_begin(renumber) else {
            return;
        };
        zend_sort(&mut items, cmp);
        self.sort_finish(items, renumber);
    }

    /// The first half of `zend_hash_sort_internal`: the early exits for
    /// tables that need no sorting, then the elements without holes, each
    /// paired with its original position (`Z_EXTRA`, the stability
    /// tie-break). `None` when there is nothing to sort.
    fn sort_begin(&mut self, renumber: bool) -> Option<Vec<SortItem>> {
        if self.num_elements <= 1 {
            if !renumber || self.num_elements == 0 {
                return None;
            }
            if self.kind() == Kind::Packed && self.is_without_holes() {
                self.internal_ptr = 0;
                self.next_free = 1;
                return None;
            }
        }
        let store = std::mem::replace(&mut self.store, Store::Uninit);
        Some(
            into_entries(store)
                .enumerate()
                .map(|(i, (key, val))| SortItem {
                    key,
                    val,
                    pos: to_u32(i),
                })
                .collect(),
        )
    }

    /// The second half of `zend_hash_sort_internal`: stores the sorted
    /// elements back, renumbering them (a packed list) or rebuilding the
    /// hash index.
    fn sort_finish(&mut self, items: Vec<SortItem>, renumber: bool) {
        self.internal_ptr = 0;
        let n = items.len();
        // Room for `nTableSize` elements, as php-src sorts `arData` in place
        // (and `zend_hash_to_packed` allocates that many values).
        let cap = (self.table_size as usize).max(n);
        if renumber {
            let mut vals = Vec::with_capacity(cap);
            vals.extend(items.into_iter().map(|it| Some(it.val)));
            self.store = Store::Packed(vals);
            self.next_free = idx_key(n);
        } else {
            let mut buckets = Vec::with_capacity(cap);
            buckets.extend(
                items
                    .into_iter()
                    .map(|it| Bucket::new(it.key, it.val, INVALID)),
            );
            self.store = Store::Hash(HashData {
                buckets,
                index: new_index(self.table_size),
            });
            self.rehash_without_compaction();
        }
    }

    /// Sorts, in place, the array that the reference `bound` holds, the way
    /// `zend_array_sort` sorts a by-reference argument
    /// (`Z_PARAM_ARRAY_HT_EX(array, 0, 1)`, then `zend_array_sort_ex` /
    /// `zend_hash_sort_internal`). Meant for sorts whose comparisons can run
    /// PHP code (`__toString`, error handlers, lazy object initializers) or
    /// reach the array through a reference among its elements
    /// (`Zend/tests/bug63882_2.phpt`).
    ///
    /// php-src separates a shared table, adds a reference to it for the
    /// duration of the sort (`GC_ADDREF`, GH-16648), turns a packed table
    /// into a hash, removes holes, resets the hash index (`HT_HASH_RESET`)
    /// and lets `zend_sort` swap the buckets of the live table. Whatever
    /// reaches the array mid-sort therefore sees its elements in their
    /// current, half-sorted order while every key lookup in it misses; with
    /// renumbering (`zend_hash_bucket_renum_swap`) only the values move and
    /// the keys stay in place. A write separates the shared table (the copy
    /// is rehashed, `zend_array_dup`), and php-src then drops the sorted
    /// table: `bound` keeps whatever PHP code left in it.
    ///
    /// `bound` holds the table being sorted whenever a comparison runs; the
    /// swaps take it back first, so the table is mutated without copying.
    /// The comparisons and swaps are exactly `zend_sort`'s, with ties broken
    /// on the original position. A `bound` that does not hold an array is
    /// left alone.
    pub fn sort_in_ref<F>(bound: &PhpRef, mut cmp: F, renumber: bool)
    where
        F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32,
    {
        let mut table = match bound.set(Value::Null) {
            Value::Array(a) => a,
            other => {
                bound.set(other);
                return;
            }
        };
        // `make_mut` separates a shared table (`zend_array_dup`).
        let begun = table.make_mut().live_sort_begin(renumber);
        let Some(n) = begun else {
            bound.set(Value::Array(table));
            return;
        };
        let mut live = LiveSort {
            bound,
            table,
            order: (0..to_u32(n)).collect(),
            renumber,
            in_bound: false,
            attached: true,
            cmp: &mut cmp,
        };
        zend_sort_by(&mut live, n);
        if let Some(mut table) = live.finish() {
            table.make_mut().live_sort_finish(renumber);
            bound.set(Value::Array(table));
        }
    }

    /// The setup of [`HashTable::sort_in_ref`]: `zend_hash_sort_internal`'s
    /// early exits (`None`: nothing to sort), then, as `zend_array_sort_ex`
    /// and `zend_hash_sort_internal` leave the table for `zend_sort`, a hash
    /// without holes whose index is reset. Returns the element count.
    fn live_sort_begin(&mut self, renumber: bool) -> Option<usize> {
        if self.num_elements <= 1 {
            if !renumber || self.num_elements == 0 {
                return None;
            }
            if self.kind() == Kind::Packed && self.is_without_holes() {
                self.internal_ptr = 0;
                self.next_free = 1;
                return None;
            }
        }
        let store = std::mem::replace(&mut self.store, Store::Uninit);
        let mut buckets: Vec<Bucket> = Vec::with_capacity(self.table_size as usize);
        buckets.extend(into_entries(store).map(|(k, v)| Bucket::new(k, v, INVALID)));
        let n = buckets.len();
        self.store = Store::Hash(HashData {
            buckets,
            index: new_index(self.table_size),
        });
        Some(n)
    }

    /// The end of `zend_hash_sort_internal` on a table sorted in place:
    /// renumbering turns it back into a list, otherwise the hash index is
    /// rebuilt over the sorted buckets.
    fn live_sort_finish(&mut self, renumber: bool) {
        self.internal_ptr = 0;
        if renumber {
            match std::mem::replace(&mut self.store, Store::Uninit) {
                Store::Hash(d) => {
                    let cap = (self.table_size as usize).max(d.buckets.len());
                    let mut vals: Vec<Option<Value>> = Vec::with_capacity(cap);
                    vals.extend(d.buckets.into_iter().filter_map(|b| b.val).map(Some));
                    self.next_free = idx_key(vals.len());
                    self.store = Store::Packed(vals);
                }
                other => self.store = other,
            }
        } else {
            self.rehash_without_compaction();
        }
    }

    /// The key and value at a position, cloned.
    fn entry_cloned(&self, pos: u32) -> Option<(ArrayKey, Value)> {
        self.at(pos).map(|(k, v)| (k.clone(), v.clone()))
    }

    /// `array_splice` core (`php_splice`): removes `length` elements starting
    /// at `offset` (both already in PHP's argument form: negative values count
    /// from the end), inserts `replacement` in their place, renumbers integer
    /// keys and returns the removed elements when `want_removed` is set.
    #[allow(clippy::too_many_lines)]
    pub fn splice(
        &mut self,
        offset: i64,
        length: i64,
        replacement: Option<&HashTable>,
        want_removed: bool,
    ) -> Option<HashTable> {
        let num_in = i64::from(self.num_elements);
        let mut offset = offset;
        let mut length = length;
        if offset > num_in {
            offset = num_in;
        } else if offset < 0 {
            offset += num_in;
            if offset < 0 {
                offset = 0;
            }
        }
        // C compares `(unsigned)offset + (unsigned)length > (unsigned)num_in`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let overflows = (offset as u32).wrapping_add(length as u32) > num_in as u32;
        if length < 0 {
            length += num_in - offset;
        } else if overflows {
            length = num_in - offset;
        }
        let num_out = num_in - length.max(0) + replacement.map_or(0, |r| i64::from(r.num_elements));
        let mut out = HashTable::with_capacity(usize::try_from(num_out).unwrap_or(0));
        let mut removed = if want_removed {
            Some(HashTable::new())
        } else {
            None
        };
        let mut iter_pos = self.iterators_lower_pos(0);
        let packed = self.kind() != Kind::Hash;
        let used = self.used();
        let mut pos: i64 = 0;
        let mut idx: u32 = 0;

        let out_insert = |out: &mut HashTable, key: &ArrayKey, v: Value, keep_str: bool| match key {
            ArrayKey::Str(s) if keep_str => {
                let _ = out.add(ArrayKey::Str(s.clone()), v);
            }
            _ => {
                let _ = out.push(v);
            }
        };

        // Copy entries before the offset.
        while pos < offset && idx < used {
            if let Some((k, v)) = self.entry_cloned(idx) {
                out_insert(&mut out, &k, v, !packed);
                if idx == iter_pos {
                    if i64::from(idx) != pos {
                        self.iterators_update(idx, to_u32(usize::try_from(pos).unwrap_or(0)));
                    }
                    iter_pos = self.iterators_lower_pos(iter_pos + 1);
                }
                pos += 1;
            }
            idx += 1;
        }
        // Removed region.
        let mut pos2 = pos;
        while pos2 - offset < length && idx < used {
            if let Some((k, v)) = self.entry_cloned(idx) {
                pos2 += 1;
                if let Some(r) = &mut removed {
                    out_insert(r, &k, v.clone(), !packed);
                }
                let _ = self.del_at(idx);
                if packed && idx == iter_pos {
                    let to = to_u32(usize::try_from(offset + length).unwrap_or(0));
                    self.iterators_update(idx, to);
                    iter_pos = self.iterators_lower_pos(iter_pos + 1);
                }
            }
            idx += 1;
        }
        if removed.is_some() {
            pos = pos2;
        }
        if !packed {
            iter_pos = self.iterators_lower_pos(iter_pos);
        }
        // Replacement.
        if let Some(r) = replacement {
            for v in r.values() {
                let _ = out.push(copy_for_insert(v));
                pos += 1;
            }
        }
        // Remaining entries.
        let used = self.used().max(idx);
        while idx < used {
            if let Some((k, v)) = self.entry_cloned(idx) {
                out_insert(&mut out, &k, v, !packed);
                if idx == iter_pos {
                    if i64::from(idx) != pos {
                        self.iterators_update(idx, to_u32(usize::try_from(pos).unwrap_or(0)));
                    }
                    iter_pos = self.iterators_lower_pos(iter_pos + 1);
                }
                pos += 1;
            }
            idx += 1;
        }
        out.iterators = std::mem::take(&mut self.iterators);
        out.internal_ptr = 0;
        // Still the same array: its root buffer mark stays (the old
        // table's drop would take it out of the buffer).
        out.gc_root.set(self.gc_root.replace(0));
        *self = out;
        removed
    }

    /// `array_shift` core: removes and returns the first element, renumbers
    /// integer keys from 0 and resets the internal pointer.
    pub fn shift(&mut self) -> Option<Value> {
        if self.num_elements == 0 {
            return None;
        }
        let first = self.valid_pos(0);
        let val = self.del_at(first)?;
        if self.kind() == Kind::Packed {
            let mut k: u32 = 0;
            let mut iter_pos = self.iterators_lower_pos(0);
            let used = self.used();
            for idx in 0..used {
                let Store::Packed(v) = &mut self.store else {
                    break;
                };
                if v[idx as usize].is_none() {
                    continue;
                }
                if idx != k {
                    let moved = v[idx as usize].take();
                    v[k as usize] = moved;
                    if idx == iter_pos {
                        self.iterators_update(idx, k);
                        iter_pos = self.iterators_lower_pos(iter_pos + 1);
                    }
                }
                k += 1;
            }
            if let Store::Packed(v) = &mut self.store {
                v.truncate(k as usize);
            }
            self.next_free = i64::from(k);
        } else {
            let mut k: i64 = 0;
            let mut should_rehash = false;
            if let Store::Hash(d) = &mut self.store {
                for b in d.buckets.iter_mut().filter(|b| b.val.is_some()) {
                    if let ArrayKey::Int(h) = &mut b.key {
                        if *h != k {
                            *h = k;
                            should_rehash = true;
                        }
                        k += 1;
                    }
                }
            }
            self.next_free = k;
            if should_rehash {
                self.rehash();
            }
        }
        self.reset();
        Some(val)
    }

    /// `array_pop` core: removes and returns the last element, adjusting
    /// `nNextFreeElement` and resetting the internal pointer.
    pub fn pop(&mut self) -> Option<Value> {
        if self.num_elements == 0 {
            return None;
        }
        let idx = self.end_pos();
        let key = self.key_at(idx)?;
        if let ArrayKey::Int(h) = key {
            if h == self.next_free.wrapping_sub(1) {
                self.next_free -= 1;
            }
        }
        let v = self.del_at(idx);
        self.reset();
        v
    }

    /// `array_unshift` core: prepends `values`, renumbering integer keys.
    pub fn unshift(&mut self, values: Vec<Value>) {
        let argc = to_u32(values.len());
        let mut new = HashTable::with_capacity(self.len() + values.len());
        for v in values {
            let _ = new.push(v);
        }
        for (k, v) in into_entries(std::mem::replace(&mut self.store, Store::Uninit)) {
            match k {
                ArrayKey::Str(s) => {
                    let _ = new.add(ArrayKey::Str(s), v);
                }
                ArrayKey::Int(_) => {
                    let _ = new.push(v);
                }
            }
        }
        new.iterators = std::mem::take(&mut self.iterators);
        for it in &mut new.iterators {
            it.1 += argc;
        }
        // Still the same array: its root buffer mark stays.
        new.gc_root.set(self.gc_root.replace(0));
        *self = new;
    }

    /// `zend_symtable_to_proptable`: converts integer keys to string keys
    /// (for `(object)` casts).
    #[must_use]
    pub fn to_proptable(&self) -> HashTable {
        let mut out = HashTable::with_capacity(self.len());
        for (k, v) in self {
            let key = match k {
                ArrayKey::Int(i) => ZStr::from(i.to_string()),
                ArrayKey::Str(s) => s.clone(),
            };
            let _ = out.str_insert(key, copy_for_insert(v), Mode::Update);
        }
        out
    }

    /// `zend_proptable_to_symtable`: converts numeric string keys to integer
    /// keys (for `(array)` casts of objects).
    #[must_use]
    pub fn to_symtable(&self) -> HashTable {
        let mut out = HashTable::with_capacity(self.len());
        for (k, v) in self {
            let key = match k {
                ArrayKey::Str(s) => ArrayKey::from_zstr(s),
                ArrayKey::Int(i) => ArrayKey::Int(*i),
            };
            out.insert(key, copy_for_insert(v));
        }
        out
    }

    /// Inserts a string key verbatim, without numeric normalization
    /// (`zend_hash_update` with a string key; used for property tables).
    pub fn insert_str_raw(&mut self, key: ZStr, val: Value) -> Option<Value> {
        let Ok((idx, existed)) = self.str_insert(key, Value::Null, Mode::Lookup) else {
            unreachable!("lookup never fails")
        };
        let slot = self.val_at_mut(idx);
        if existed {
            Some(std::mem::replace(slot, val))
        } else {
            *slot = val;
            None
        }
    }

    /// Builds a list (`zend_array_to_list`) from the values.
    #[must_use]
    pub fn to_list(&self) -> HashTable {
        HashTable::from_list_sized(self.len(), self.values().map(copy_for_insert))
    }
}

/// The value to store when copying an element into another array: references
/// with refcount 1 are unwrapped (like `zend_array_dup`/`zend_array_to_list`),
/// shared references stay shared.
#[must_use]
pub fn copy_for_insert(v: &Value) -> Value {
    match v {
        Value::Reference(r) if r.refcount() == 1 => r.get(),
        other => other.clone(),
    }
}

/// An element being sorted: key, value and original position (`Z_EXTRA`,
/// the stability tie-break).
struct SortItem {
    key: ArrayKey,
    val: Value,
    pos: u32,
}

/// Comparator over sort items.
type SortCmp<'a> = dyn FnMut(&SortItem, &SortItem) -> i32 + 'a;

/// Whether `r` holds exactly the table `a`.
fn holds(r: &PhpRef, a: &ZArray) -> bool {
    r.try_borrow()
        .is_some_and(|v| matches!(&*v, Value::Array(b) if ZArray::ptr_eq(a, b)))
}

/// The table of [`HashTable::sort_in_ref`] being sorted in place: the
/// comparisons and swaps of `zend_sort` over its buckets, with the table
/// held by the bound reference whenever a comparison runs.
struct LiveSort<'a, F: ?Sized> {
    bound: &'a PhpRef,
    /// The table being sorted (the reference `zend_array_sort_ex` adds).
    table: ZArray,
    /// The original position of the element at each position (`Z_EXTRA`,
    /// the stability tie-break).
    order: Vec<u32>,
    /// Whether the swaps move values only (`zend_hash_bucket_renum_swap`).
    renumber: bool,
    /// Whether `bound` was last given `table`.
    in_bound: bool,
    /// Whether `bound` has held `table` at every check so far: once PHP code
    /// writes to it (separating the shared table) or replaces it, php-src
    /// keeps sorting a table nobody can see and drops it.
    attached: bool,
    cmp: &'a mut F,
}

impl<F: ?Sized> LiveSort<'_, F> {
    /// Takes `table` back from `bound` (so that it is unshared again), or
    /// notes that PHP code replaced it.
    fn reclaim(&mut self) {
        if self.in_bound {
            self.in_bound = false;
            if holds(self.bound, &self.table) {
                drop(self.bound.set(Value::Null));
            } else {
                self.attached = false;
            }
        }
    }

    /// Ends the sort: the table, unless PHP code detached it from `bound`.
    fn finish(mut self) -> Option<ZArray> {
        self.reclaim();
        self.attached.then_some(self.table)
    }
}

impl<F> SortAccess for LiveSort<'_, F>
where
    F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32 + ?Sized,
{
    fn compare(&mut self, a: usize, b: usize) -> i32 {
        if self.attached && !self.in_bound {
            drop(self.bound.set(Value::Array(self.table.clone())));
            self.in_bound = true;
        }
        let Self {
            table, order, cmp, ..
        } = self;
        let (Some((ka, va)), Some((kb, vb))) = (table.at(to_u32(a)), table.at(to_u32(b))) else {
            return 0;
        };
        let r = cmp(ka, va, kb, vb);
        if r != 0 {
            r
        } else {
            i32::from(order[a] > order[b]) - i32::from(order[a] < order[b])
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.reclaim();
        self.order.swap(a, b);
        // PHP code that kept a copy of the array (refcount > 1) shares the
        // live table in php-src; here the swap separates it, and the
        // duplicate keeps the reset index.
        let shared = self.table.refcount() > 1;
        let t = self.table.make_mut();
        let Store::Hash(d) = &mut t.store else {
            return;
        };
        if shared {
            d.index.fill(INVALID);
        }
        if self.renumber {
            // zend_hash_bucket_renum_swap: the values move, the keys stay.
            if a != b {
                let (lo, hi) = (a.min(b), a.max(b));
                let (head, tail) = d.buckets.split_at_mut(hi);
                if let (Some(x), Some(y)) = (head.get_mut(lo), tail.first_mut()) {
                    std::mem::swap(&mut x.val, &mut y.val);
                }
            }
        } else {
            d.buckets.swap(a, b);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `HASH_ADD`
    Add,
    /// `HASH_UPDATE`
    Update,
    /// `HASH_LOOKUP`
    Lookup,
    /// `HASH_ADD | HASH_ADD_NEXT`
    Next,
}

/// `zend_array_dup_value` for one slot of a table being copied: a null,
/// bool, int or float is copied as it stands (`ZVAL_COPY_VALUE`, nothing
/// to count), which is one compare of the tag (those variants come first);
/// a hole or anything else goes to [`dup_other`].
#[inline(always)]
#[allow(clippy::inline_always)] // one tag test per element of a copied table
fn dup_slot(slot: &Option<Value>, table: *const HashTable) -> Option<Value> {
    match slot {
        Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
            // SAFETY: a null, bool, int or float owns no allocation and has
            // no drop glue, so its bits are a second, independent value
            // (php-src copies such a zval with `ZVAL_COPY_VALUE`).
            // `ptr::read` is a typed copy of a valid, initialised
            // `Option<Value>` behind a shared reference. The safe version
            // (`slot.clone()`, a match over every variant that rebuilds the
            // enum) measured 27 instructions an element of a packed list of
            // ints under callgrind; this is one tag test and one 16-byte
            // copy.
            unsafe { std::ptr::read(slot) }
        }
        _ => dup_other(slot, table),
    }
}

/// [`dup_slot`] for a hole or a refcounted element of the table at
/// `table`: a reference held only by the source (refcount 1) is unwrapped
/// into its value, unless it refers to that table itself; anything else is
/// shared (`Z_ADDREF`). Out of line, so the loop over scalars stays tight.
#[inline(never)]
fn dup_other(slot: &Option<Value>, table: *const HashTable) -> Option<Value> {
    let v = slot.as_ref()?;
    if let Value::Reference(r) = v {
        if r.refcount() == 1 {
            if let Some(inner) = r.try_borrow() {
                let is_self =
                    matches!(&*inner, Value::Array(a) if std::ptr::eq(Rc::as_ptr(&a.0), table));
                if !is_self {
                    return Some(inner.clone());
                }
            }
        }
    }
    Some(v.clone())
}

impl Clone for HashTable {
    /// `zend_array_dup`: copies the table; references with refcount 1 (other
    /// than a reference to this very array) are unwrapped; holes are removed
    /// from hash tables (packed tables keep them); iterators are duplicated so
    /// that `foreach` by reference can find its position in the copy.
    fn clone(&self) -> Self {
        let self_ptr: *const HashTable = self;
        let mut out = HashTable {
            store: Store::Uninit,
            num_elements: self.num_elements,
            table_size: self.table_size,
            internal_ptr: if self.internal_ptr < self.used() {
                self.internal_ptr
            } else {
                0
            },
            next_free: self.next_free,
            iterators: self.iterators.clone(),
            gc_root: Cell::new(0),
        };
        if self.num_elements == 0 {
            out.table_size = HT_MIN_SIZE;
            out.internal_ptr = 0;
            return out;
        }
        // The copy has room for `nTableSize` elements, like the source
        // (`zend_array_dup` allocates `HT_SIZE_EX(nTableSize)`).
        let cap = out.table_size as usize;
        match &self.store {
            Store::Uninit => {}
            Store::Packed(v) => {
                let mut vals = Vec::with_capacity(cap.max(v.len()));
                vals.extend(v.iter().map(|s| dup_slot(s, self_ptr)));
                out.store = Store::Packed(vals);
            }
            Store::Hash(d)
                if self.is_without_holes()
                    && d.index.len() == index_len(out.table_size)
                    && d.last_is_linked(out.table_size) =>
            {
                // No holes: every bucket keeps its position in the copy, so
                // the source's chains and index are the copy's as they
                // stand (they are what re-linking the copy would build: a
                // chain lists its live buckets last-placed first, which
                // appends, unlinks and re-links all keep), copied instead
                // of rebuilt bucket by bucket. Not while an in-place sort
                // has the index reset ([`HashData::last_is_linked`]): such
                // a copy is re-linked below, so lookups in it work.
                let mut buckets = Vec::with_capacity(cap.max(d.buckets.len()));
                buckets.extend(d.buckets.iter().map(|b| Bucket {
                    val: dup_slot(&b.val, self_ptr),
                    key: b.key.clone(),
                    next: b.next,
                }));
                out.store = Store::Hash(HashData {
                    buckets,
                    index: d.index.clone(),
                });
            }
            Store::Hash(d) => {
                let mut buckets = Vec::with_capacity(cap.max(d.buckets.len()));
                buckets.extend(d.buckets.iter().map(|b| match &b.val {
                    Some(_) => Bucket {
                        val: dup_slot(&b.val, self_ptr),
                        key: b.key.clone(),
                        next: INVALID,
                    },
                    None => Bucket {
                        val: None,
                        key: ArrayKey::Int(0),
                        next: INVALID,
                    },
                }));
                out.store = Store::Hash(HashData {
                    buckets,
                    index: new_index(out.table_size),
                });
                out.rehash();
            }
        }
        out
    }
}

impl fmt::Debug for HashTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Iterator over the entries of a [`HashTable`].
pub struct Iter<'a> {
    inner: IterInner<'a>,
    remaining: usize,
}

/// The storage walked by an [`Iter`].
enum IterInner<'a> {
    /// Packed values; `front`/`back` are the positions of the next value
    /// from either end (the keys), `keys`/`keys_start` the chunk of shared
    /// keys last used and the position of its first key.
    Packed {
        slots: std::slice::Iter<'a, Option<Value>>,
        front: u32,
        back: u32,
        keys: &'static [ArrayKey],
        keys_start: u32,
    },
    /// Hash buckets.
    Hash(std::slice::Iter<'a, Bucket>),
}

impl fmt::Debug for Iter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Iter")
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a ArrayKey, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let item: (&'a ArrayKey, &'a Value) = match &mut self.inner {
            IterInner::Packed {
                slots,
                front,
                keys,
                keys_start,
                ..
            } => loop {
                let slot = slots.next()?;
                let pos = *front;
                *front += 1;
                if let Some(v) = slot {
                    break (packed_key(keys, keys_start, pos), v);
                }
            },
            IterInner::Hash(buckets) => loop {
                let b = buckets.next()?;
                if let Some(v) = &b.val {
                    break (&b.key, v);
                }
            },
        };
        self.remaining = self.remaining.saturating_sub(1);
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let item: Self::Item = match &mut self.inner {
            IterInner::Packed {
                slots,
                back,
                keys,
                keys_start,
                ..
            } => loop {
                let slot = slots.next_back()?;
                *back -= 1;
                if let Some(v) = slot {
                    break (packed_key(keys, keys_start, *back), v);
                }
            },
            IterInner::Hash(buckets) => loop {
                let b = buckets.next_back()?;
                if let Some(v) = &b.val {
                    break (&b.key, v);
                }
            },
        };
        self.remaining = self.remaining.saturating_sub(1);
        Some(item)
    }
}

impl ExactSizeIterator for Iter<'_> {}

/// Iterator over the values of a [`HashTable`] ([`HashTable::values`]).
pub struct Values<'a> {
    inner: ValuesInner<'a>,
    remaining: usize,
}

/// The storage walked by a [`Values`].
enum ValuesInner<'a> {
    /// Packed values (`None` is a hole).
    Packed(std::slice::Iter<'a, Option<Value>>),
    /// Hash buckets.
    Hash(std::slice::Iter<'a, Bucket>),
}

impl fmt::Debug for Values<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Values")
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
}

impl<'a> Iterator for Values<'a> {
    type Item = &'a Value;

    #[inline]
    fn next(&mut self) -> Option<&'a Value> {
        let v: &'a Value = match &mut self.inner {
            ValuesInner::Packed(slots) => loop {
                if let Some(v) = slots.next()? {
                    break v;
                }
            },
            ValuesInner::Hash(buckets) => loop {
                if let Some(v) = &buckets.next()?.val {
                    break v;
                }
            },
        };
        self.remaining = self.remaining.saturating_sub(1);
        Some(v)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for Values<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let v: Self::Item = match &mut self.inner {
            ValuesInner::Packed(slots) => loop {
                if let Some(v) = slots.next_back()? {
                    break v;
                }
            },
            ValuesInner::Hash(buckets) => loop {
                if let Some(v) = &buckets.next_back()?.val {
                    break v;
                }
            },
        };
        self.remaining = self.remaining.saturating_sub(1);
        Some(v)
    }
}

impl ExactSizeIterator for Values<'_> {}

impl<'a> IntoIterator for &'a HashTable {
    type Item = (&'a ArrayKey, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// The cycle collector's mark in the header (`GC_INFO`, see
/// [`crate::ref_roots`] for the array root buffer).
impl HashTable {
    /// The mark of the shared empty array (`zend_empty_array`, which is
    /// `GC_IMMUTABLE`): never a possible root.
    pub const GC_IMMUTABLE: u32 = u32::MAX;

    /// The shared empty table, marked [`HashTable::GC_IMMUTABLE`].
    fn immutable_empty() -> Self {
        let t = Self::new();
        t.gc_root.set(Self::GC_IMMUTABLE);
        t
    }

    /// The root buffer mark: 0 when the array may be buffered as a
    /// possible root (`GC_MAY_LEAK`); otherwise it is buffered already or
    /// is the immutable empty array. What a release to a non-zero count
    /// tests before anything else.
    #[inline]
    #[must_use]
    pub fn gc_root_mark(&self) -> u32 {
        self.gc_root.get()
    }

    /// Sets the root buffer mark (the buffer's own bookkeeping).
    #[inline]
    pub(crate) fn set_gc_root_mark(&self, mark: u32) {
        self.gc_root.set(mark);
    }

    /// Takes a buffered table out of the root buffer
    /// (`GC_REMOVE_FROM_BUFFER`) and clears its mark, before its contents
    /// leave the allocation the buffer knows.
    fn leave_root_buffer(&self) {
        let mark = self.gc_root.get();
        if mark != 0 {
            crate::ref_roots::array_freed(self, mark);
            self.gc_root.set(0);
        }
    }
}

// ---------------------------------------------------------------------------
// ZArray: the refcounted copy-on-write handle
// ---------------------------------------------------------------------------

/// A refcounted, copy-on-write PHP array.
///
/// `clone()` shares the table (refcount + 1). [`ZArray::make_mut`] returns a
/// mutable table, duplicating it first if it is shared — which is how
/// `$b = $a; $b[] = 1;` separates `$b` from `$a`.
#[derive(Clone)]
pub struct ZArray(Rc<HashTable>);

impl ZArray {
    /// A new empty array (shares a per-thread empty table until written).
    #[must_use]
    pub fn new() -> Self {
        EMPTY_ARRAY.with(Clone::clone)
    }

    /// A new empty array sized for `n` elements.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        ZArray(Rc::new(HashTable::with_capacity(n)))
    }

    /// A new array whose list storage is in place for `n` elements: the
    /// table of an array literal whose elements have no keys
    /// (`zend_new_array(n)` + `zend_hash_real_init_packed`, as
    /// `ZEND_INIT_ARRAY` with a first element and no key makes it). The
    /// same table and allocations, in the same order (the storage, then the
    /// handle), as `ZArray::from_table(HashTable::with_capacity_packed(n))`,
    /// built without the intermediate table's moves and the release of its
    /// placeholder storage.
    #[must_use]
    pub fn packed_sized(n: usize) -> Self {
        let table_size = check_size(to_u32(n));
        let store = Store::Packed(Vec::with_capacity(table_size as usize));
        ZArray(Rc::new(HashTable::sized(table_size, store)))
    }

    /// A new array whose hash storage (buckets and index) is in place for
    /// `n` elements: the table of an array literal with a string key
    /// (`zend_new_array(n)` + `zend_hash_real_init_mixed`, for
    /// `ZEND_INIT_ARRAY` with `ZEND_ARRAY_NOT_PACKED`). As
    /// [`ZArray::packed_sized`], the same as
    /// `ZArray::from_table(HashTable::with_capacity_mixed(n))`.
    #[must_use]
    pub fn mixed_sized(n: usize) -> Self {
        let table_size = check_size(to_u32(n));
        let store = Store::Hash(HashData::new(table_size));
        ZArray(Rc::new(HashTable::sized(table_size, store)))
    }

    /// Wraps a table (also accepts a boxed table, such as an object's
    /// dynamic property table).
    #[must_use]
    pub fn from_table(t: impl Into<HashTable>) -> Self {
        ZArray(Rc::new(t.into()))
    }

    /// Builds a list from values, sized by the iterator's lower size hint
    /// ([`HashTable::from_list_sized`]).
    pub fn from_values<I: IntoIterator<Item = Value>>(values: I) -> Self {
        let iter = values.into_iter();
        ZArray::from_table(HashTable::from_list_sized(iter.size_hint().0, iter))
    }

    /// A list of `values` sized for `n` elements
    /// ([`HashTable::from_list_sized`]).
    pub fn from_list_sized<I: IntoIterator<Item = Value>>(n: usize, values: I) -> Self {
        ZArray::from_table(HashTable::from_list_sized(n, values))
    }

    /// Builds an array from `(key, value)` pairs (later keys overwrite).
    pub fn from_pairs<K: Into<ArrayKey>, I: IntoIterator<Item = (K, Value)>>(pairs: I) -> Self {
        let iter = pairs.into_iter();
        let mut t = HashTable::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            t.insert(k, v);
        }
        ZArray::from_table(t)
    }

    /// Returns a mutable table, separating (copying) it first when shared
    /// (`SEPARATE_ARRAY`).
    ///
    /// The table must never be moved out from behind the returned borrow
    /// (`mem::take`, `mem::swap`, `mem::replace` of the whole table): the
    /// cycle collector's root buffer knows a buffered array by its
    /// address, and a moved table would leave that entry pointing at the
    /// allocation (assigning a new table in place is fine: the old one is
    /// dropped there). [`ZArray::into_table`] is the way to take a table.
    #[inline]
    pub fn make_mut(&mut self) -> &mut HashTable {
        // Only a self-containing array registered with the collector has
        // a weak handle; `Rc::make_mut` moves such a table when it is not
        // shared otherwise.
        if Rc::weak_count(&self.0) != 0 {
            return self.make_mut_with_weak();
        }
        Rc::make_mut(&mut self.0)
    }

    /// [`ZArray::make_mut`] of a table with a weak handle: `Rc::make_mut`
    /// moves it into a new allocation when this is its only strong handle,
    /// and a buffered table's root buffer entry follows it.
    #[cold]
    #[inline(never)]
    fn make_mut_with_weak(&mut self) -> &mut HashTable {
        let old = Rc::as_ptr(&self.0);
        let t = Rc::make_mut(&mut self.0);
        let mark = t.gc_root.get();
        let new: *const HashTable = &*t;
        if mark != 0 && !std::ptr::eq(old, new) {
            crate::ref_roots::array_moved(old, &*t, mark);
        }
        t
    }

    /// The table for writing in place when this handle is its only one
    /// (no other handle and no weak one), so [`ZArray::make_mut`] would
    /// neither copy nor move it; `None` when a write must separate first.
    /// As with [`ZArray::make_mut`], never move the table out of it.
    #[inline]
    pub fn get_mut_unshared(&mut self) -> Option<&mut HashTable> {
        Rc::get_mut(&mut self.0)
    }

    /// Whether two handles share the same table.
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// Number of handles sharing the table (`GC_REFCOUNT`).
    #[must_use]
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    /// An address usable as identity key (recursion detection).
    #[must_use]
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }

    /// The table's address in its allocation: what the array root buffer
    /// holds for a buffered array, without a count ([`crate::ref_roots`]).
    #[inline]
    pub(crate) fn table_ptr(&self) -> *const HashTable {
        Rc::as_ptr(&self.0)
    }

    /// A new handle of the buffered array whose table is at `table`.
    ///
    /// # Safety
    /// `table` must be [`ZArray::table_ptr`] of an array that is still
    /// alive. The root buffer keeps that true for its entries: a table
    /// leaves the buffer when it is dropped (`HashTable::drop`) and before
    /// its contents leave the allocation ([`ZArray::into_table`],
    /// [`ZArray::gc_take_contents`], the move in [`ZArray::make_mut`]).
    pub(crate) unsafe fn from_buffered(table: *const HashTable) -> ZArray {
        // SAFETY: per the contract, `table` is `Rc::as_ptr` of a live
        // `Rc<HashTable>` (strong count >= 1): the count is raised for the
        // handle made here, which then owns it.
        unsafe {
            Rc::increment_strong_count(table);
            ZArray(Rc::from_raw(table))
        }
    }

    /// Takes the table out if unshared, otherwise clones it.
    #[must_use]
    pub fn into_table(self) -> HashTable {
        // A buffered table leaves the root buffer before it leaves its
        // allocation (a shared one is copied, unmarked).
        if self.0.gc_root.get() != 0 && Rc::strong_count(&self.0) == 1 {
            self.0.leave_root_buffer();
        }
        Rc::try_unwrap(self.0).unwrap_or_else(|rc| (*rc).clone())
    }

    /// Stores a handle to this very array inside it, at the end of `path`,
    /// and returns the stored value: the array then contains itself by
    /// value, as `$t = &$a; $a[] = $t;` builds it in php-src, whose
    /// `ZEND_ASSIGN_DIM` fetches (and separates) the container before it
    /// reads `OP_DATA` and adds a reference to the same `zend_array`.
    ///
    /// `path` lists the keys from this table down to the slot; `None`
    /// stands for the last element of that level's table (the one a `[]`
    /// segment just appended). Intermediate elements must be unshared
    /// arrays or references to unshared arrays, which is how a write walk
    /// leaves them, and the slot must exist. Otherwise nothing is stored
    /// and `None` is returned.
    pub fn store_self_at(&mut self, path: &[Option<ArrayKey>]) -> Option<Value> {
        if path.is_empty() {
            return None;
        }
        // Unshared first: the handles below are then the only other ones.
        let _ = self.make_mut();
        let stored = ZArray(Rc::clone(&self.0));
        let result = Value::Array(ZArray(Rc::clone(&self.0)));
        let top = Rc::as_ptr(&self.0).cast_mut();
        // SAFETY: this is `Rc::get_mut_unchecked` (unstable in std). The
        // allocation was unshared after `make_mut`; its only handles now are
        // `self`, which is borrowed mutably for this whole call, and the two
        // created above, neither of which is dereferenced before the last
        // use of `table` (`stored` is moved into the slot, `result` is
        // returned; their counts live in the `Rc` header, not the table).
        // No value reachable from the table holds another handle to it,
        // since any such handle would have made it shared. `Rc::as_ptr`
        // keeps the allocation's provenance, so writing through it is sound
        // while those conditions hold.
        let table = unsafe { &mut *top };
        match store_in(table, path, Value::Array(stored)) {
            Ok(old) => {
                drop(old);
                Some(result)
            }
            Err(unstored) => {
                drop(unstored);
                None
            }
        }
    }
}

/// [`ZArray::store_self_at`]'s walk: stores `v` at `path` below `t`,
/// returning the previous value, or gives `v` back when the path does not
/// lead to an existing slot through unshared arrays.
fn store_in(t: &mut HashTable, path: &[Option<ArrayKey>], v: Value) -> Result<Value, Value> {
    let Some((first, rest)) = path.split_first() else {
        return Err(v);
    };
    let key = match first {
        Some(k) => k.clone(),
        None => match t.last() {
            Some((k, _)) => k.clone(),
            None => return Err(v),
        },
    };
    let Some(slot) = t.get_mut(&key) else {
        return Err(v);
    };
    if rest.is_empty() {
        return Ok(std::mem::replace(slot, v));
    }
    match slot {
        Value::Array(a) if a.refcount() == 1 => store_in(a.make_mut(), rest, v),
        Value::Reference(r) => {
            let Some(mut b) = r.try_borrow_mut() else {
                return Err(v);
            };
            let res = match &mut *b {
                Value::Array(a) if a.refcount() == 1 => store_in(a.make_mut(), rest, v),
                _ => Err(v),
            };
            drop(b);
            res
        }
        _ => Err(v),
    }
}

/// A weak handle of an array's table, for the cycle collector's register
/// of arrays that may contain themselves ([`ZArray::downgrade`]).
#[derive(Clone, Default)]
pub struct WeakZArray(std::rc::Weak<HashTable>);

impl WeakZArray {
    /// The array, unless it was freed.
    #[must_use]
    pub fn upgrade(&self) -> Option<ZArray> {
        self.0.upgrade().map(ZArray)
    }

    /// Whether the array is still alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}

impl fmt::Debug for WeakZArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WeakZArray({})", self.0.strong_count())
    }
}

/// Cycle-collector support (`Zend/zend_gc.c`): an array that contains
/// itself by value ([`ZArray::store_self_at`] on master) is a cycle of
/// arrays alone, which only the collector can free.
impl ZArray {
    /// A weak handle of this array's table.
    #[must_use]
    pub fn downgrade(&self) -> WeakZArray {
        WeakZArray(Rc::downgrade(&self.0))
    }

    /// Empties this array in place although other handles share its table,
    /// returning the former contents: `zend_gc_collect_cycles` destroys
    /// garbage arrays whatever their count (`HT_ALLOW_COW_VIOLATION`),
    /// which breaks a cycle of arrays.
    ///
    /// # Safety
    /// Every handle sharing the table must be owned by garbage that nothing
    /// outside the collector can reach, and no reference into the table
    /// (`&HashTable`, an element borrow, an iterator) may be alive for the
    /// duration of the call. The collector guarantees both: it calls this
    /// for arrays its analysis found unreachable, from its free phase, where
    /// no PHP code runs and the graph holds handles only.
    #[must_use]
    pub unsafe fn gc_take_contents(&self) -> HashTable {
        // The contents leave the allocation: a buffered table leaves the
        // root buffer first.
        self.0.leave_root_buffer();
        let table = Rc::as_ptr(&self.0).cast_mut();
        // SAFETY: per this function's contract no other reference into the
        // table exists while it is replaced; `Rc::as_ptr` keeps the
        // allocation's provenance (this is `Rc::get_mut_unchecked`).
        unsafe { std::mem::take(&mut *table) }
    }
}

impl Default for ZArray {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for ZArray {
    type Target = HashTable;
    fn deref(&self) -> &HashTable {
        &self.0
    }
}

impl fmt::Debug for ZArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

/// Unboxes a table (objects keep their dynamic property table boxed).
impl From<Box<HashTable>> for HashTable {
    fn from(t: Box<HashTable>) -> Self {
        *t
    }
}

impl From<Box<HashTable>> for ZArray {
    fn from(t: Box<HashTable>) -> Self {
        ZArray(Rc::new(*t))
    }
}

impl From<HashTable> for ZArray {
    fn from(t: HashTable) -> Self {
        ZArray::from_table(t)
    }
}

impl FromIterator<Value> for ZArray {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        ZArray::from_values(iter)
    }
}

#[cfg(test)]
mod tests {
    /// `push_all` appends as `push` of each value would: into a list
    /// with room, a list with a hole (pushed one by one), a hash and a
    /// table whose next key is taken.
    #[test]
    fn push_all_appends_like_push() {
        use super::{ArrayKey, HashTable};
        use crate::value::Value;
        let vals = || (1..=3).map(Value::Int).collect::<Vec<_>>();
        let mut each = HashTable::with_capacity_packed(8);
        let mut all = HashTable::with_capacity_packed(8);
        for v in vals() {
            each.push(v).unwrap();
        }
        all.push_all(vals()).unwrap();
        all.push_all(Vec::new()).unwrap();
        assert!(all.is_packed());
        assert_eq!(all.len(), 3);
        assert_eq!(all.next_index(), 3);
        assert_eq!(format!("{all:?}"), format!("{each:?}"));
        let mut holed = HashTable::new();
        holed.push_all(vals()).unwrap();
        holed.remove(&ArrayKey::Int(2));
        holed.push_all(vals()).unwrap();
        assert_eq!(holed.len(), 5);
        assert_eq!(holed.next_index(), 6);
        let mut hash = HashTable::new();
        hash.insert(ArrayKey::Int(10), Value::Null);
        hash.push_all(vals()).unwrap();
        assert_eq!(hash.next_index(), 14);
        let mut full = HashTable::new();
        full.insert(ArrayKey::Int(i64::MAX), Value::Null);
        assert!(full.push_all(vals()).is_err());
    }

    /// A list built to size is the list pushes build: same keys, length,
    /// next free index, packed storage and table size, whatever the hint;
    /// an empty one is untouched.
    #[test]
    fn from_list_sized_matches_pushes() {
        for (hint, count) in [
            (0_usize, 0_i64),
            (5, 0),
            (0, 3),
            (5, 5),
            (3, 9),
            (20, 2),
            (8, 8),
            (9, 9),
        ] {
            let size = usize::try_from(count).unwrap_or(0);
            let built = HashTable::from_list_sized(size.max(hint), (0..count).map(Value::Int));
            let built_hinted = HashTable::from_list_sized(hint, (0..count).map(Value::Int));
            let mut pushed = HashTable::with_capacity(hint);
            for i in 0..count {
                assert!(pushed.push(Value::Int(i)).is_ok());
            }
            for t in [&built, &built_hinted] {
                assert_eq!(t.len(), pushed.len(), "{hint} {count}");
                assert_eq!(t.next_free_element(), pushed.next_free_element());
                assert_eq!(t.next_index(), pushed.next_index());
                assert_eq!(t.is_packed(), pushed.is_packed());
                assert_eq!(t.is_untouched(), pushed.is_untouched());
                assert_eq!(t.used(), pushed.used());
                assert!(t.is_list());
                let keys: Vec<_> = t.iter().map(|(k, _)| k.clone()).collect();
                let want: Vec<_> = pushed.iter().map(|(k, _)| k.clone()).collect();
                assert_eq!(keys, want);
                for i in 0..count {
                    assert!(matches!(t.get_int(i), Some(Value::Int(v)) if *v == i));
                }
            }
            assert_eq!(
                built_hinted.table_size(),
                pushed.table_size(),
                "{hint} {count}"
            );
            assert_eq!(built_hinted.packed_capacity().is_some(), count > 0);
        }
        // Appending after it continues the list.
        let mut t = HashTable::from_list_sized(2, [Value::Int(7), Value::Int(8)]);
        assert!(matches!(t.push(Value::Int(9)), Ok(2)));
        assert_eq!(t.len(), 3);
        let z = ZArray::from_values(Vec::<Value>::new());
        assert!(z.is_untouched());
        assert_eq!(ZArray::from_list_sized(1, [Value::Null]).len(), 1);
    }

    /// The lean probe finds what the general one finds for the keys it
    /// takes, and nothing for the keys it leaves to it; `entry_at` gives
    /// the element at a position with its key, and nothing at a hole.
    #[test]
    fn lean_probe_and_entry_at() {
        let mut t = HashTable::new();
        for (i, name) in (0i64..).zip(["foo_0", "x", "a-long-key-of-twenty-six-bytes", "12", "-3x"])
        {
            t.insert(ArrayKey::Str(ZStr::from(name)), Value::Int(i));
        }
        // Another string with the same bytes; its hash is not known yet.
        let fresh = |s: &str| ZStr::from(s.to_owned().into_bytes());
        let key = fresh("foo_0");
        assert!(t.lean_zstr_probe_mut(&key).is_none());
        let _ = key.hash_value();
        assert!(matches!(t.lean_zstr_probe_mut(&key), Some(Value::Int(0))));
        for (name, found) in [
            ("x", Some(1)),
            ("a-long-key-of-twenty-six-bytes", Some(2)),
            ("a-long-key-of-twenty-six-bytez", None),
            ("12", None),
            ("-3x", None),
            ("", None),
            ("nope", None),
        ] {
            let key = fresh(name);
            let _ = key.hash_value();
            let got = match t.lean_zstr_probe_mut(&key) {
                Some(Value::Int(i)) => Some(*i),
                _ => None,
            };
            assert_eq!(got, found, "{name}");
        }
        // The general probe finds the ones left to it.
        assert!(matches!(
            t.get_sym_zstr_mut(&fresh("-3x")),
            Some(Value::Int(4))
        ));
        // A list: no probe; its entries have no key of their own.
        let mut list = HashTable::new();
        let _ = list.push(Value::Int(7));
        let _ = list.push(Value::Int(8));
        assert!(list.lean_zstr_probe_mut(&key).is_none());
        assert!(matches!(list.entry_at(1), Some((Value::Int(8), None))));
        assert!(list.entry_at(2).is_none());
        // A hash: the key of the bucket; nothing at a hole or at the end.
        assert!(matches!(
            t.entry_at(1),
            Some((Value::Int(1), Some(ArrayKey::Str(k)))) if k.as_bytes() == b"x"
        ));
        t.remove(&ArrayKey::Str(ZStr::from("x")));
        assert!(t.entry_at(1).is_none());
        assert!(t.entry_at(5).is_none());
        assert!(HashTable::new().entry_at(0).is_none());
    }

    /// `small_int` gives the int key `ArrayKey::from_bytes` makes, for every
    /// string it takes, and takes every such key of up to 18 digits.
    #[test]
    fn small_int_is_the_numeric_key() {
        let mut cases: Vec<String> = [
            "",
            "-",
            "0",
            "-0",
            "00",
            "01",
            "-01",
            "1",
            "-1",
            "9",
            "10",
            "1999",
            "c350",
            "1f",
            "1e5",
            " 1",
            "1 ",
            "+1",
            "1.0",
            "999999999999999999",
            "-999999999999999999",
            "1000000000000000000",
            "9223372036854775807",
            "-9223372036854775808",
            "9223372036854775808",
            "0x1",
            "١",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        cases.extend((0..2000).map(|i| format!("{i:x}")));
        cases.extend((0..300).map(|i| format!("-{i}")));
        for s in &cases {
            let general = match ArrayKey::from_bytes(s.as_bytes()) {
                ArrayKey::Int(i) => Some(i),
                ArrayKey::Str(_) => None,
            };
            match ArrayKey::small_int(s.as_bytes()) {
                Some(i) => assert_eq!(Some(i), general, "{s:?}"),
                None => assert!(
                    general.is_none() || s.trim_start_matches('-').len() > 18,
                    "{s:?}"
                ),
            }
        }
        // The lean int functions on a hash and on a list.
        let mut h = HashTable::with_capacity_mixed(8);
        assert!(h.lean_add_int(10, Value::Int(1)).is_ok());
        assert!(h.lean_add_int(-3, Value::Int(2)).is_ok());
        assert!(matches!(h.lean_get_int(10), Some(Value::Int(1))));
        assert!(matches!(h.get_int(-3), Some(Value::Int(2))));
        assert!(h.lean_get_int(11).is_none());
        if let Some(v) = h.lean_get_int_mut(10) {
            *v = Value::Int(5);
        }
        assert!(matches!(h.get_int(10), Some(Value::Int(5))));
        assert!(matches!(h.push(Value::Null), Ok(11)));
        let mut l = HashTable::new();
        let _ = l.push(Value::Int(7));
        assert!(matches!(l.lean_get_int(0), Some(Value::Int(7))));
        assert!(l.lean_get_int(-1).is_none());
        assert!(l.lean_add_int(1, Value::Null).is_err());
    }

    /// The lean lookups and the lean append agree with the general table
    /// functions, and `surely_str` with the key conversion.
    #[test]
    fn lean_find_and_append() {
        let fresh = |s: &str| ZStr::from(s.to_owned().into_bytes());
        let mut lean = HashTable::with_capacity_mixed(8);
        let mut general = HashTable::with_capacity_mixed(8);
        let names = [
            "a",
            "c350",
            "1f",
            "x-y",
            "",
            "a-rather-long-key-of-30-bytes!",
            "1e5",
        ];
        for (i, name) in (0i64..).zip(names) {
            let key = fresh(name);
            let h = key.hash_now();
            assert_eq!(h, zend_hash_bytes_of(name));
            assert!(lean.lean_find(&key, h).is_none(), "{name}");
            assert!(lean.lean_append(key, h, Value::Int(i)).is_ok(), "{name}");
            general.insert(ArrayKey::Str(fresh(name)), Value::Int(i));
        }
        // The eighth fills the table; the ninth is the general insert's.
        let key = fresh("eighth");
        let h = key.hash_now();
        assert!(lean.lean_append(key, h, Value::Null).is_ok());
        let key = fresh("ninth");
        let h = key.hash_now();
        assert!(lean.lean_append(key, h, Value::Null).is_err());
        assert_eq!(lean.len(), 8);
        for (i, name) in (0i64..).zip(names) {
            let key = fresh(name);
            let h = key.hash_now();
            assert!(matches!(lean.lean_find(&key, h), Some(Value::Int(n)) if *n == i));
            assert!(matches!(lean.get_zstr(&key), Some(Value::Int(n)) if *n == i));
            assert!(matches!(general.lean_find(&key, h), Some(Value::Int(n)) if *n == i));
            if let Some(v) = lean.lean_find_mut(&key, h) {
                *v = Value::Int(-1);
            }
            assert!(matches!(lean.get_zstr(&key), Some(Value::Int(-1))));
        }
        let missing = fresh("missing");
        assert!(lean.lean_find(&missing, missing.hash_now()).is_none());
        // A list and an empty table: not the lean functions'.
        let mut list = HashTable::new();
        let _ = list.push(Value::Int(1));
        let key = fresh("k");
        let h = key.hash_now();
        assert!(list.lean_find(&key, h).is_none());
        assert!(list.lean_append(key, h, Value::Null).is_err());
        assert!(HashTable::new()
            .lean_find(&missing, missing.hash_now())
            .is_none());
        // `surely_str` never claims a key the conversion makes an int.
        for name in [
            "",
            "a",
            "-",
            "-a",
            "1f",
            "f1",
            "12",
            "-12",
            "012",
            "0",
            "-0",
            "1e5",
            "12 ",
            " 12",
            "9223372036854775808",
            "1.5",
            "+1",
        ] {
            let is_str = matches!(ArrayKey::from_bytes(name.as_bytes()), ArrayKey::Str(_));
            if ArrayKey::surely_str(name.as_bytes()) {
                assert!(is_str, "{name:?}");
            }
        }
        assert!(ArrayKey::surely_str(b"c350") && ArrayKey::surely_str(b"1f"));
        assert!(!ArrayKey::surely_str(b"12") && !ArrayKey::surely_str(b"-"));
    }

    /// The hash of a string's bytes, for the test above.
    fn zend_hash_bytes_of(s: &str) -> u64 {
        crate::string::zend_hash_bytes(s.as_bytes())
    }

    /// Keys compare by their bytes at every length around the size classes
    /// of `same_bytes`, and differ when any one byte does.
    #[test]
    fn same_bytes_at_every_length() {
        let base: Vec<u8> = (0u8..80)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        for n in 0..=base.len() {
            let a = &base[..n];
            // (Another allocation with the same bytes.)
            let copy = a.to_vec();
            assert!(super::same_bytes(a, &copy), "length {n}");
            assert!(!super::same_bytes(a, &base[..n.saturating_sub(1)]) || n == 0);
            for at in 0..n {
                let mut b = a.to_vec();
                b[at] ^= 0x40;
                assert!(!super::same_bytes(a, &b), "length {n}, byte {at}");
            }
        }
    }

    use super::*;

    /// `sort_ints` gives what `sort_by` gives with the int comparison, for
    /// lists, tables with holes and tables with string keys, ascending and
    /// descending; and leaves a table with any other value alone.
    #[test]
    fn sort_ints_is_the_general_sort_of_ints() {
        let lists: [Vec<i64>; 5] = [
            vec![3, 1, 2],
            vec![5, 5, -1, i64::MAX, i64::MIN, 0, 5],
            (0..200).map(|i| (i * 7919) % 101 - 50).collect(),
            (0..50).rev().collect(),
            vec![1, 1],
        ];
        let dump = |t: &HashTable| {
            (
                t.iter()
                    .map(|(k, v)| format!("{k:?}={v:?}"))
                    .collect::<Vec<_>>(),
                t.len(),
                t.next_index(),
            )
        };
        for list in &lists {
            for reverse in [false, true] {
                for shape in 0..3 {
                    let build = || {
                        let mut t = HashTable::new();
                        for (n, &v) in list.iter().enumerate() {
                            match shape {
                                0 => {
                                    let _ = t.push(Value::Int(v));
                                }
                                1 => {
                                    let k = i64::try_from(n).unwrap_or(0) * 3 + 7;
                                    t.insert(ArrayKey::Int(k), Value::Int(v));
                                }
                                _ => {
                                    let k = ZStr::from(format!("k{n}").as_str());
                                    t.insert(ArrayKey::Str(k), Value::Int(v));
                                }
                            }
                        }
                        t
                    };
                    let mut fast = build();
                    assert!(fast.sort_ints(reverse), "{list:?}");
                    let mut general = build();
                    general.sort_by(
                        |_, a, _, b| {
                            let (Value::Int(x), Value::Int(y)) = (a, b) else {
                                return 0;
                            };
                            let r = i32::from(x > y) - i32::from(x < y);
                            if reverse {
                                -r
                            } else {
                                r
                            }
                        },
                        true,
                    );
                    assert_eq!(dump(&fast), dump(&general), "{list:?} {reverse} {shape}");
                }
            }
        }
        let mut mixed = HashTable::new();
        let _ = mixed.push(Value::Int(2));
        let _ = mixed.push(Value::Float(1.5));
        let _ = mixed.push(Value::Int(1));
        assert!(!mixed.sort_ints(false));
        assert!(matches!(mixed.get_int(0), Some(Value::Int(2))));
        let mut one = HashTable::new();
        let _ = one.push(Value::Int(2));
        assert!(!one.sort_ints(false));
        assert!(!HashTable::new().sort_ints(false));
    }

    /// `set_int_plain` leaves a table exactly as `insert` leaves it, key
    /// by key, for the orders that take its different paths (a packed
    /// append, a descending fill that starts a hash, holes, overwrites),
    /// and declines an element that is not plain.
    #[test]
    fn set_int_plain_is_insert_for_plain_elements() {
        let orders: [Vec<i64>; 6] = [
            (0..40).collect(),
            (0..40).rev().collect(),
            vec![0, 1, 2, 5, 3, 4, 100, 6, -1, 7, 2, 5],
            vec![7, 7, 7, 3, 3, 0],
            vec![i64::MAX, i64::MIN, 0, 1, i64::MAX - 1],
            (0..300).map(|i| (i * 7919) % 101).collect(),
        ];
        for order in &orders {
            let mut a = HashTable::new();
            let mut b = HashTable::new();
            for (n, &k) in order.iter().enumerate() {
                let v = i64::try_from(n).unwrap_or(0);
                assert!(
                    a.set_int_plain(k, Value::Int(v)).is_ok(),
                    "{order:?} at {k}"
                );
                b.insert(ArrayKey::Int(k), Value::Int(v));
                assert_eq!(a.len(), b.len(), "{order:?} at {k}");
                assert_eq!(a.next_index(), b.next_index(), "{order:?} at {k}");
            }
            let pairs = |t: &HashTable| {
                t.iter()
                    .map(|(k, v)| format!("{k:?}={v:?}"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(pairs(&a), pairs(&b), "{order:?}");
        }
        // A string over an int and an int over a string are plain stores.
        let mut t = HashTable::new();
        assert!(t.set_int_plain(3, Value::Str(ZStr::from("s"))).is_ok());
        assert!(t.set_int_plain(3, Value::Int(1)).is_ok());
        assert!(matches!(t.get_int(3), Some(Value::Int(1))));
        // An array element is not overwritten: the value comes back.
        t.insert(ArrayKey::Int(4), Value::Array(ZArray::new()));
        assert!(matches!(
            t.set_int_plain(4, Value::Int(9)),
            Err(Value::Int(9))
        ));
        assert!(matches!(t.get_int(4), Some(Value::Array(_))));
        let mut packed = HashTable::new();
        for i in 0..3 {
            packed.insert(ArrayKey::Int(i), Value::Array(ZArray::new()));
        }
        assert!(packed.set_int_plain(1, Value::Int(9)).is_err());
        assert!(packed.set_int_plain(3, Value::Int(9)).is_ok());
        assert_eq!(packed.len(), 4);
    }

    #[test]
    fn sym_zstr_lookups_match_the_built_key() {
        let mut t = HashTable::new();
        t.insert(ArrayKey::Int(12), Value::Int(1));
        t.insert(ArrayKey::Int(-3), Value::Int(2));
        t.insert(ArrayKey::from_bytes(b"012"), Value::Int(3));
        t.insert(ArrayKey::from_bytes(b"beta"), Value::Int(4));
        for k in ["12", "-3", "012", "beta", "", "-", "-0", "7", "gamma"] {
            let z = ZStr::from(k);
            let want = t.get(&ArrayKey::from_zstr(&z)).cloned();
            let got = t.get_sym_zstr(&z).cloned();
            assert_eq!(format!("{want:?}"), format!("{got:?}"), "{k}");
        }
        if let Some(v) = t.get_sym_zstr_mut(&ZStr::from("12")) {
            *v = Value::Int(10);
        }
        if let Some(v) = t.get_sym_zstr_mut(&ZStr::from("beta")) {
            *v = Value::Int(40);
        }
        assert!(t.get_sym_zstr_mut(&ZStr::from("nope")).is_none());
        assert!(matches!(t.get_int(12), Some(Value::Int(10))));
        assert!(matches!(t.get_str(b"beta"), Some(Value::Int(40))));
        // A packed table has no string keys.
        let mut p = HashTable::new();
        assert!(p.push(Value::Int(5)).is_ok());
        assert!(matches!(
            p.get_sym_zstr(&ZStr::from("0")),
            Some(Value::Int(5))
        ));
        assert!(p.get_sym_zstr(&ZStr::from("x")).is_none());
        assert!(p.get_sym_zstr_mut(&ZStr::from("x")).is_none());
    }

    #[test]
    fn get_mut_unshared_only_for_a_sole_handle() {
        let mut a = ZArray::from_values([Value::Int(1)]);
        assert!(a.get_mut_unshared().is_some());
        let b = a.clone();
        assert!(a.get_mut_unshared().is_none());
        drop(b);
        let w = a.downgrade();
        assert!(a.get_mut_unshared().is_none());
        drop(w);
        assert!(a.get_mut_unshared().is_some());
    }

    fn keys(t: &HashTable) -> Vec<String> {
        t.keys()
            .map(|k| match k {
                ArrayKey::Int(i) => i.to_string(),
                ArrayKey::Str(s) => format!("{s:?}"),
            })
            .collect()
    }

    fn ints(t: &HashTable) -> Vec<i64> {
        t.values().map(|v| v.as_int().unwrap_or(-999)).collect()
    }

    #[test]
    fn table_header_stays_small() {
        // data, index, boxed iterator list, counters (zend_array: 56).
        assert!(std::mem::size_of::<HashTable>() <= 80);
        // The root buffer mark lives in what was tail padding.
        assert_eq!(std::mem::size_of::<HashTable>(), 72);
        assert!(HashTable::GC_ROOT_OFFSET + 4 <= std::mem::size_of::<HashTable>());
        assert_eq!(std::mem::size_of::<IterList>(), 8);
        let mut t = HashTable::new();
        let _ = t.push(Value::Int(1));
        let a = t.iterator_add(0);
        let b = t.iterator_add(0);
        assert!(t.has_iterators());
        t.iterator_del(a);
        assert_eq!(t.iterator_positions().len(), 1);
        t.iterator_del(b);
        assert!(!t.has_iterators());
        assert!(t.iterator_positions().is_empty());
    }

    #[test]
    fn element_layout_matches_zend() {
        // A packed element is a bare value (`zval *arPacked`), a hash
        // bucket a value plus a 16-byte key and 4-byte collision link (`Bucket`).
        assert_eq!(
            std::mem::size_of::<Option<Value>>(),
            std::mem::size_of::<Value>()
        );
        assert_eq!(std::mem::size_of::<ArrayKey>(), 16);
        assert_eq!(
            std::mem::size_of::<Bucket>(),
            std::mem::size_of::<Value>() + 24
        );
    }

    #[test]
    fn hash_index_has_two_four_byte_slots_per_bucket() {
        let t = HashTable::with_capacity_mixed(16);
        let Store::Hash(d) = &t.store else { panic!() };
        assert_eq!(d.index.len(), 2 * t.table_size() as usize);
        assert_eq!(std::mem::size_of_val(&d.index[0]), 4);
        assert!(d.index.iter().all(|&x| x == u32::MAX));
    }

    #[test]
    fn shared_packed_keys_cover_chunk_boundaries() {
        let top = 1u32 << 18;
        for pos in [
            0u32, 1, 255, 256, 257, 511, 512, 1023, 1024, 32_767, 32_768, 65_535, 65_536, 65_537,
            131_071, 131_072, 200_000, top,
        ] {
            assert_eq!(int_keys::get(pos), &ArrayKey::Int(i64::from(pos)));
            assert!(std::ptr::eq(int_keys::get(pos), int_keys::get(pos)));
        }
        // Neighbouring positions share a chunk or sit at the start of the
        // next one: the keys are contiguous within a chunk.
        for pos in [100u32, 300, 40_000, 70_000, 150_000] {
            let a: *const ArrayKey = int_keys::get(pos);
            let b: *const ArrayKey = int_keys::get(pos + 1);
            assert!(std::ptr::eq(a.wrapping_add(1), b));
        }
        // A chunk is the contiguous run of keys holding its position.
        for pos in [0u32, 255, 256, 600, 32_768, 65_535, 65_536, 131_071, top] {
            let (keys, start) = int_keys::chunk(pos);
            assert!(start <= pos);
            let k = keys.get((pos - start) as usize);
            assert!(k.is_some_and(|k| std::ptr::eq(k, int_keys::get(pos))));
            assert_eq!(keys.first(), Some(&ArrayKey::Int(i64::from(start))));
        }
        // 16 bytes per position plus at most one 1 MiB chunk of slack,
        // never a doubling overshoot (which would be 32 MiB here).
        let bound = (top as usize + 2 * (1 << 16)) * std::mem::size_of::<ArrayKey>();
        assert!(int_keys::allocated_bytes() <= bound);
    }

    #[test]
    fn packed_iteration_caches_key_chunks() {
        // 70 000 positions span the doubling chunks and two fixed-size ones.
        let n: i64 = 70_000;
        let mut t = HashTable::new();
        for i in 0..n {
            t.push(Value::Int(i)).unwrap();
        }
        for i in (0..n).step_by(7) {
            t.remove(&ArrayKey::Int(i));
        }
        let want: Vec<i64> = (0..n).filter(|i| i % 7 != 0).collect();
        let key_of = |k: &ArrayKey| match k {
            ArrayKey::Int(i) => *i,
            ArrayKey::Str(_) => -1,
        };
        let fwd: Vec<i64> = t.iter().map(|(k, _)| key_of(k)).collect();
        assert_eq!(fwd, want);
        let mut back: Vec<i64> = t.iter().rev().map(|(k, _)| key_of(k)).collect();
        back.reverse();
        assert_eq!(back, want);
        assert!(t.iter().all(|(k, v)| Some(key_of(k)) == v.as_int()));
        // Alternating ends share one cached chunk without mixing keys up.
        let mut it = t.iter();
        let (mut lo, mut hi) = (Vec::new(), Vec::new());
        for step in 0..want.len() {
            if step % 2 == 0 {
                if let Some((k, _)) = it.next() {
                    lo.push(key_of(k));
                }
            } else if let Some((k, _)) = it.next_back() {
                hi.push(key_of(k));
            }
        }
        assert!(it.next().is_none());
        hi.reverse();
        lo.extend(hi);
        assert_eq!(lo, want);
        // iter_mut() and keys() lend the same keys; values() is exact.
        let via_mut: Vec<i64> = t.iter_mut().map(|(k, _)| key_of(k)).collect();
        assert_eq!(via_mut, want);
        assert_eq!(t.keys().map(key_of).collect::<Vec<_>>(), want);
        assert_eq!(t.values().len(), want.len());
        assert_eq!(t.values().size_hint(), (want.len(), Some(want.len())));
        assert_eq!(
            t.values().next_back().and_then(Value::as_int),
            want.last().copied()
        );
        assert_eq!(
            t.values().filter_map(Value::as_int).collect::<Vec<_>>(),
            want
        );
    }

    #[test]
    fn insert_fast_paths_keep_zend_semantics() {
        let mut t = HashTable::new();
        for i in 0..4 {
            assert_eq!(t.push(Value::Int(i)).ok(), Some(i));
        }
        // Existing elements: update in place, `add` refuses, lookups find.
        assert_eq!(
            t.insert(2, Value::Int(20)).and_then(|v| v.as_int()),
            Some(2)
        );
        assert!(t.add(1, Value::Int(9)).is_err());
        let (slot, added) = t.lookup_ex(3);
        assert!(!added);
        *slot = Value::Int(30);
        // An append at nNumUsed through `insert` stays packed.
        assert!(t.insert(4, Value::Int(40)).is_none());
        assert!(t.is_packed());
        assert_eq!(render(&t), "0=0,1=1,2=20,3=30,4=40");
        assert_eq!(t.next_index(), 5);
        // Deleting the last element trims nNumUsed but not
        // nNextFreeElement: the next push leaves a hole behind.
        t.remove(&ArrayKey::Int(4));
        assert_eq!(t.used(), 4);
        assert_eq!(t.push(Value::Int(50)).ok(), Some(5));
        assert!(t.is_packed());
        assert_eq!(t.used(), 6);
        assert_eq!(render(&t), "0=0,1=1,2=20,3=30,5=50");
        // Writing into the hole keeps the order: the table becomes a hash.
        assert!(t.insert(4, Value::Int(44)).is_none());
        assert!(!t.is_packed());
        assert_eq!(render(&t), "0=0,1=1,2=20,3=30,5=50,4=44");
        // Hash: updates, lookups and appends with room, then growth.
        assert_eq!(
            t.insert(5, Value::Int(55)).and_then(|v| v.as_int()),
            Some(50)
        );
        let (_, added) = t.lookup_ex(100);
        assert!(added);
        assert_eq!(t.next_index(), 101);
        for k in 200..240 {
            assert!(t.insert(k, Value::Int(k)).is_none());
        }
        assert!(t.table_size() >= 64);
        for k in 200..240 {
            assert_eq!(t.get_int(k).and_then(Value::as_int), Some(k));
        }
        assert!(t
            .get(&ArrayKey::Int(100))
            .is_some_and(|v| matches!(v, Value::Null)));
        // String keys go through the same fast path of str_insert.
        let mut s = HashTable::new();
        for i in 0..20 {
            assert!(s
                .insert_str_raw(ZStr::from(format!("k{i}")), Value::Int(i))
                .is_none());
        }
        assert_eq!(
            s.insert_str_raw(ZStr::from("k3"), Value::Int(33))
                .and_then(|v| v.as_int()),
            Some(3)
        );
        assert!(s.add(ArrayKey::from("k4"), Value::Null).is_err());
        assert_eq!(s.len(), 20);
        assert!(s.table_size() >= 32);
        assert_eq!(s.get_str(b"k3").and_then(Value::as_int), Some(33));
        assert_eq!(s.get_str(b"k19").and_then(Value::as_int), Some(19));
        // Mutable lookups in either storage.
        if let Some(v) = t.get_int_mut(5) {
            *v = Value::Int(56);
        }
        assert_eq!(t.get_int(5).and_then(Value::as_int), Some(56));
        assert!(t.get_int_mut(-1).is_none());
        if let Some(v) = s.get_mut(&ArrayKey::from("k7")) {
            *v = Value::Int(77);
        }
        assert_eq!(s.get_str(b"k7").and_then(Value::as_int), Some(77));
        assert!(s.get_mut(&ArrayKey::Int(7)).is_none());
        assert!(s.get_mut(&ArrayKey::from("nope")).is_none());
        // `$a[] = v` when nNextFreeElement points at an existing element.
        let mut n = HashTable::new();
        n.push(Value::Int(0)).unwrap();
        n.push(Value::Int(1)).unwrap();
        n.set_next_free_element(1);
        assert!(n.push(Value::Int(2)).is_err());
        assert_eq!(n.len(), 2);
        assert!(n.get_mut(&ArrayKey::from("x")).is_none());
        assert!(n.get_mut(&ArrayKey::Int(1)).is_some());
        assert!(n.get_int_mut(2).is_none());
        assert!(HashTable::new().get_int_mut(0).is_none());
    }

    #[test]
    fn storage_is_presized_to_the_table_size() {
        fn capacity(t: &HashTable) -> usize {
            match &t.store {
                Store::Uninit => 0,
                Store::Packed(v) => v.capacity(),
                Store::Hash(d) => d.buckets.capacity(),
            }
        }
        fn data_ptr(t: &HashTable) -> usize {
            match &t.store {
                Store::Uninit => 0,
                Store::Packed(v) => v.as_ptr() as usize,
                Store::Hash(d) => d.buckets.as_ptr() as usize,
            }
        }
        // zend_hash_real_init_packed: the first insert allocates nTableSize
        // values, so appends up to it never move the storage.
        let mut t = HashTable::new();
        assert_eq!(capacity(&t), 0);
        t.push(Value::Int(0)).unwrap();
        assert_eq!(t.table_size(), 8);
        assert!(capacity(&t) >= 8);
        let first = data_ptr(&t);
        for i in 1..8 {
            t.push(Value::Int(i)).unwrap();
        }
        assert_eq!(data_ptr(&t), first);
        // zend_hash_packed_grow doubles the table size and the storage.
        t.push(Value::Int(8)).unwrap();
        assert_eq!(t.table_size(), 16);
        assert!(capacity(&t) >= 16);
        // A size hint is rounded up to a power of two, allocated at once.
        let mut h = HashTable::with_capacity(100);
        assert_eq!(capacity(&h), 0);
        h.push(Value::Null).unwrap();
        assert_eq!(h.table_size(), 128);
        assert!(capacity(&h) >= 128);
        // Hashes: zend_hash_real_init_mixed, then zend_hash_do_resize.
        let mut m = HashTable::with_capacity_mixed(20);
        assert!(capacity(&m) >= 32);
        let first = data_ptr(&m);
        for i in 0..32 {
            m.insert(format!("k{i}").as_str(), Value::Int(i));
        }
        assert_eq!(data_ptr(&m), first);
        m.insert("more", Value::Null);
        assert_eq!(m.table_size(), 64);
        assert!(capacity(&m) >= 64);
        // Conversions, copies (zend_array_dup) and sorts keep room for
        // nTableSize elements.
        t.insert("s", Value::Null);
        assert!(!t.is_packed());
        assert!(capacity(&t) >= t.table_size() as usize);
        for c in [t.clone(), m.clone(), h.clone()] {
            assert!(capacity(&c) >= c.table_size() as usize);
        }
        let mut sorted = t.clone();
        sorted.sort_by(|_, a, _, b| int_cmp(a, b), true);
        assert!(sorted.is_packed());
        assert!(capacity(&sorted) >= sorted.table_size() as usize);
    }

    #[test]
    fn positional_reads_skip_holes_without_shared_keys() {
        let mut t = HashTable::new();
        assert_eq!(t.valid_pos(0), 0);
        assert!(t.fe_fetch(0).is_none());
        for i in 0..6 {
            t.push(Value::Int(i * 10)).unwrap();
        }
        t.remove(&ArrayKey::Int(1));
        t.remove(&ArrayKey::Int(2));
        // Deleting the last element trims the trailing hole.
        t.remove(&ArrayKey::Int(5));
        assert_eq!(t.used(), 5);
        assert_eq!(t.valid_pos(0), 0);
        assert_eq!(t.valid_pos(1), 3);
        assert_eq!(t.valid_pos(5), 5);
        assert_eq!(t.valid_pos(9), 9);
        let mut seen = Vec::new();
        let mut pos = 0;
        while let Some((p, v)) = t.fe_fetch(pos) {
            let k = t.key_value_at(p);
            seen.push((p, k.as_ref().and_then(Value::as_int), v.as_int()));
            pos = p + 1;
        }
        assert_eq!(
            seen,
            [
                (0, Some(0), Some(0)),
                (3, Some(3), Some(30)),
                (4, Some(4), Some(40))
            ]
        );
        assert!(t.key_value_at(1).is_none());
        assert!(t.key_value_at(7).is_none());
        assert!(t.fe_fetch(5).is_none());
        assert!(t.fe_fetch(9).is_none());
        // A hash: the conversion compacts, string keys come back as strings.
        t.insert("s", Value::Int(7));
        assert!(!t.is_packed());
        assert_eq!(
            t.fe_fetch(1).map(|(p, v)| (p, v.as_int())),
            Some((1, Some(30)))
        );
        assert!(matches!(t.key_value_at(3), Some(Value::Str(s)) if s.as_bytes() == b"s"));
        t.remove(&ArrayKey::Int(3));
        assert_eq!(t.valid_pos(1), 2);
        assert_eq!(
            t.fe_fetch(1).map(|(p, v)| (p, v.as_int())),
            Some((2, Some(40)))
        );
    }

    #[test]
    fn fe_fetch_with_key_matches_fe_fetch_then_key_value_at() {
        fn key_repr(k: &Value) -> String {
            match k {
                Value::Int(i) => format!("i{i}"),
                Value::Str(s) => format!("s{}", String::from_utf8_lossy(s.as_bytes())),
                _ => "?".to_owned(),
            }
        }
        type Walk = Vec<(u32, Option<i64>, String)>;
        fn walk_both(t: &HashTable) -> (Walk, Walk) {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            let mut pos = 0;
            while let Some((p, v)) = t.fe_fetch(pos) {
                let k = t.key_value_at(p).map_or_else(String::new, |k| key_repr(&k));
                a.push((p, v.as_int(), k));
                pos = p + 1;
            }
            let mut pos = 0;
            while let Some((p, v, k)) = t.fe_fetch_with_key(pos) {
                b.push((p, v.as_int(), key_repr(&k)));
                pos = p + 1;
            }
            // Past the end.
            assert!(t.fe_fetch_with_key(t.used()).is_none());
            assert!(t.fe_fetch_with_key(t.used() + 3).is_none());
            (a, b)
        }
        // Packed with holes: the key is the position.
        let mut t = HashTable::new();
        assert!(t.fe_fetch_with_key(0).is_none());
        for i in 0..6 {
            t.push(Value::Int(i * 10)).unwrap();
        }
        t.remove(&ArrayKey::Int(0));
        t.remove(&ArrayKey::Int(3));
        assert!(t.is_packed());
        let (a, b) = walk_both(&t);
        assert_eq!(a, b);
        assert_eq!(
            b.iter().map(|e| e.2.as_str()).collect::<Vec<_>>(),
            ["i1", "i2", "i4", "i5"]
        );
        // A hash of string and integer keys (numeric strings normalised:
        // "08" and "-0" stay strings, "8" becomes 8) with a hole.
        let mut t = HashTable::new();
        t.insert("foo_1", Value::Int(1));
        t.insert("08", Value::Int(2));
        t.insert("8", Value::Int(3));
        t.insert("-0", Value::Int(4));
        t.insert(ArrayKey::Int(-5), Value::Int(5));
        t.remove(&ArrayKey::from("08"));
        let (a, b) = walk_both(&t);
        assert_eq!(a, b);
        assert_eq!(
            b,
            [
                (0, Some(1), "sfoo_1".to_owned()),
                (2, Some(3), "i8".to_owned()),
                (3, Some(4), "s-0".to_owned()),
                (4, Some(5), "i-5".to_owned()),
            ]
        );
    }

    #[test]
    fn packed_to_hash_compacts_like_zend_hash_rehash() {
        // zend_hash_fconvert_packed_to_hash ends with zend_hash_rehash,
        // which drops the holes and remaps the internal pointer and the
        // iterators: a position at (or in a hole before) an element follows
        // that element, a past-the-end position moves to the new end.
        let mut t = HashTable::new();
        for i in 0..6 {
            t.push(Value::Int(i)).unwrap();
        }
        t.remove(&ArrayKey::Int(1));
        t.remove(&ArrayKey::Int(3));
        t.set_internal_pointer(3);
        let at_hole = t.iterator_add(1);
        let at_elem = t.iterator_add(4);
        let past_end = t.iterator_add(6);
        t.insert("s", Value::Int(9));
        assert!(!t.is_packed());
        assert_eq!(t.used(), 5);
        assert_eq!(render(&t), "0=0,2=2,4=4,5=5,s=9");
        assert_eq!(t.current().and_then(Value::as_int), Some(4));
        assert_eq!(t.iterator_pos(at_hole), 1);
        assert_eq!(t.iterator_pos(at_elem), 2);
        // The past-the-end iterator picks up the element appended by the
        // converting insert.
        assert_eq!(t.iterator_pos(past_end), 4);
        for k in [0, 2, 4, 5] {
            assert_eq!(t.get_int(k).and_then(Value::as_int), Some(k));
        }
        assert_eq!(t.get_str(b"s").and_then(Value::as_int), Some(9));
        // The order-keeping conversion ($a = [0, 1, 2]; unset($a[1]);
        // $a[1] = 1) compacts too and appends the key at the end.
        let mut t = HashTable::new();
        for i in 0..3 {
            t.push(Value::Int(i)).unwrap();
        }
        t.remove(&ArrayKey::Int(1));
        t.insert(1, Value::Int(1));
        assert_eq!(render(&t), "0=0,2=2,1=1");
        assert_eq!(t.used(), 3);
        assert_eq!(t.position_of(&ArrayKey::Int(1)), Some(2));
    }

    #[test]
    fn packed_storage_iterates_with_position_keys() {
        let mut t = HashTable::new();
        for i in 0..5 {
            t.push(Value::Int(i * 10)).unwrap();
        }
        t.remove(&ArrayKey::Int(1));
        t.remove(&ArrayKey::Int(3));
        assert!(t.is_packed());
        assert!(!t.is_without_holes());
        assert!(!t.is_list());
        assert_eq!(keys(&t), ["0", "2", "4"]);
        let back: Vec<i64> = t
            .iter()
            .rev()
            .map(|(k, _)| match k {
                ArrayKey::Int(i) => *i,
                ArrayKey::Str(_) => -1,
            })
            .collect();
        assert_eq!(back, [4, 2, 0]);
        assert_eq!(t.iter().len(), 3);
        assert_eq!(
            t.at(2).map(|(k, v)| (k.clone(), v.as_int())),
            Some((ArrayKey::Int(2), Some(20)))
        );
        assert!(t.at(1).is_none());
        assert_eq!(t.key_at(4), Some(ArrayKey::Int(4)));
        assert_eq!(t.key_at(3), None);
        for (k, v) in t.iter_mut() {
            if let ArrayKey::Int(i) = k {
                *v = Value::Int(*i);
            }
        }
        assert_eq!(ints(&t), [0, 2, 4]);
        assert_eq!(t.values().count(), 3);
        if let Some(v) = t.get_int_mut(4) {
            *v = Value::Int(44);
        }
        assert_eq!(t.last().map(|(_, v)| v.as_int()), Some(Some(44)));
        assert_eq!(t.first().map(|(k, _)| k.clone()), Some(ArrayKey::Int(0)));
        // Deleting the last element trims trailing holes (`nNumUsed`).
        t.remove(&ArrayKey::Int(4));
        assert_eq!(t.used(), 3);
        // Converting to a hash keeps the order and the keys and drops the
        // holes (`zend_hash_packed_to_hash` rehashes).
        t.insert("s", Value::Int(7));
        assert!(!t.is_packed());
        assert_eq!(keys(&t), ["0", "2", "\"s\""]);
        assert_eq!(t.get_int(2).and_then(Value::as_int), Some(2));
        assert_eq!(t.position_of(&ArrayKey::from("s")), Some(2));
        assert_eq!(t.used(), 3);
    }

    #[test]
    fn hash_collisions_unlink_and_relink() {
        // Keys 0, 16, 32, 48 share hash slot 0 of an 8-bucket table (16
        // hash slots).
        let mut t = HashTable::with_capacity_mixed(8);
        for k in [0, 16, 32, 48] {
            t.insert(k, Value::Int(k));
        }
        assert!(!t.is_packed());
        t.remove(&ArrayKey::Int(16));
        assert!(t.get_int(16).is_none());
        for k in [0, 32, 48] {
            assert_eq!(t.get_int(k).and_then(Value::as_int), Some(k));
        }
        t.remove(&ArrayKey::Int(48));
        t.remove(&ArrayKey::Int(0));
        assert_eq!(t.get_int(32).and_then(Value::as_int), Some(32));
        assert_eq!(keys(&t), ["32"]);
        // Growing rehashes (compacting holes); every key stays reachable,
        // in the copy too.
        for k in 100..140 {
            t.insert(k, Value::Int(k));
        }
        assert_eq!(t.len(), 41);
        assert!(t.table_size() >= 64);
        let c = t.clone();
        for k in (100..140).chain([32]) {
            assert_eq!(t.get_int(k).and_then(Value::as_int), Some(k));
            assert_eq!(c.get_int(k).and_then(Value::as_int), Some(k));
        }
        assert_eq!(keys(&c), keys(&t));
    }

    #[test]
    fn string_keys_match_by_identity_or_content() {
        let mut t = HashTable::new();
        let k = ZStr::from("beta");
        t.insert(ArrayKey::Str(k.clone()), Value::Int(2));
        t.insert("alpha", Value::Int(1));
        assert_eq!(t.get_zstr(&k).and_then(Value::as_int), Some(2));
        assert_eq!(t.get_str_identical_val(&k).and_then(Value::as_int), Some(2));
        assert_eq!(
            t.get_zstr(&ZStr::from("beta")).and_then(Value::as_int),
            Some(2)
        );
        assert_eq!(
            t.get_str_identical_val(&ZStr::from("beta"))
                .and_then(Value::as_int),
            Some(2)
        );
        assert!(t.get_zstr(&ZStr::from("gamma")).is_none());
        assert_eq!(t.get_str(b"alpha").and_then(Value::as_int), Some(1));
        assert_eq!(
            t.insert_str_raw(ZStr::from("alpha"), Value::Int(9))
                .and_then(|v| v.as_int()),
            Some(1)
        );
        assert_eq!(t.remove_at(0).map(|(k, _)| k), Some(ArrayKey::Str(k)));
        assert!(t.get_str(b"beta").is_none());
        assert_eq!(keys(&t), ["\"alpha\""]);
        assert_eq!(ints(&t), [9]);
    }

    #[test]
    fn short_key_bytes_compare_like_slices() {
        let base: Vec<u8> = (0u8..40).map(|i| b'a' + i % 26).collect();
        let mut t = HashTable::new();
        for n in 0..=40 {
            let a = &base[..n];
            assert!(same_bytes(a, a));
            // Equal bytes in another allocation.
            let copy = Vec::from(a);
            assert!(same_bytes(a, &copy));
            if n > 0 {
                assert!(!same_bytes(a, &a[..n - 1]));
                assert!(!same_bytes(&a[..n - 1], a));
            }
            // Every byte takes part, whichever word load covers it.
            for i in 0..n {
                let mut b = a.to_vec();
                if let Some(c) = b.get_mut(i) {
                    *c ^= 1;
                }
                assert!(!same_bytes(a, &b), "n={n} i={i}");
            }
            t.insert(ArrayKey::Str(ZStr::from(a)), Value::Int(idx_key(n)));
        }
        // Lookups by another instance of the same bytes compare contents.
        for n in 0..=40 {
            let fresh = ZStr::from(&base[..n]);
            assert_eq!(t.get_zstr(&fresh).and_then(Value::as_int), Some(idx_key(n)));
            assert_eq!(
                t.get_str(&base[..n]).and_then(Value::as_int),
                Some(idx_key(n))
            );
        }
        assert!(t.get_str(b"abcdefghijklmnoq").is_none());
        assert!(t.get_zstr(&ZStr::from("abcdeg")).is_none());
    }

    #[test]
    fn sorting_packed_tables_with_holes() {
        let mut t = HashTable::new();
        for v in [5, 3, 9, 1] {
            t.push(Value::Int(v)).unwrap();
        }
        t.remove(&ArrayKey::Int(2));
        let cmp = |_: &ArrayKey, a: &Value, _: &ArrayKey, b: &Value| int_cmp(a, b);
        let mut kept = t.clone();
        kept.sort_by(cmp, false);
        assert_eq!(render(&kept), "3=1,1=3,0=5");
        assert!(!kept.is_packed());
        assert_eq!(kept.get_int(0).and_then(Value::as_int), Some(5));
        t.sort_by(cmp, true);
        assert_eq!(render(&t), "0=1,1=3,2=5");
        assert!(t.is_packed());
        assert_eq!(t.next_index(), 3);
    }

    #[test]
    fn packed_shift_moves_values_down() {
        let mut t = HashTable::new();
        for v in 0..4 {
            t.push(Value::Int(v)).unwrap();
        }
        t.remove(&ArrayKey::Int(2));
        let id = t.iterator_add(3);
        assert_eq!(t.shift().and_then(|v| v.as_int()), Some(0));
        assert!(t.is_packed());
        assert_eq!(render(&t), "0=1,1=3");
        assert_eq!(t.iterator_pos(id), 1);
        assert_eq!(t.next_index(), 2);
        assert_eq!(t.pop().and_then(|v| v.as_int()), Some(3));
        assert_eq!(t.next_index(), 1);
    }

    #[test]
    fn weak_handle_and_gc_take_contents() {
        let mut a = ZArray::new();
        a.make_mut().push(Value::Int(1)).unwrap();
        let w = a.downgrade();
        assert!(w.is_alive());
        let shared = a.clone();
        // SAFETY: no borrow into the table is alive; both handles are ours.
        let old = unsafe { shared.gc_take_contents() };
        assert_eq!(old.len(), 1);
        assert_eq!(a.len(), 0);
        assert!(w.upgrade().is_some_and(|x| ZArray::ptr_eq(&x, &a)));
        drop((a, shared));
        assert!(!w.is_alive());
        assert!(w.upgrade().is_none());
    }

    #[test]
    fn insertion_order_and_normalization() {
        let mut t = HashTable::new();
        t.insert("b", Value::Int(1));
        t.insert("10", Value::Int(2));
        t.insert("a", Value::Int(3));
        t.insert("010", Value::Int(4));
        t.insert(ArrayKey::from_bytes(b"-5"), Value::Int(5));
        assert_eq!(keys(&t), ["\"b\"", "10", "\"a\"", "\"010\"", "-5"]);
        assert_eq!(t.get_sym(b"10").and_then(Value::as_int), Some(2));
        assert_eq!(t.get_int(10).and_then(Value::as_int), Some(2));
        assert!(t.get_str(b"10").is_none());
        assert_eq!(t.len(), 5);
        // update keeps position
        t.insert("b", Value::Int(9));
        assert_eq!(ints(&t), [9, 2, 3, 4, 5]);
        assert_eq!(t.next_index(), 11);
    }

    #[test]
    fn next_free_negative_and_max() {
        let mut t = HashTable::new();
        t.insert(-5, Value::Int(1));
        assert_eq!(t.push(Value::Int(2)).ok(), Some(-4));
        let mut t = HashTable::new();
        t.insert(i64::MAX, Value::Int(1));
        assert!(t.push(Value::Int(2)).is_err());
        let mut t = HashTable::new();
        assert_eq!(t.push(Value::Null).ok(), Some(0));
        assert_eq!(t.push(Value::Null).ok(), Some(1));
    }

    #[test]
    fn packed_quirks_match_php() {
        // $a=range(0,9); unset 5..9; $a[6]="x"; $a[]="y" -> keys 0..4,6,7
        let mut t = HashTable::with_capacity(10);
        for i in 0..10 {
            t.push(Value::Int(i)).unwrap();
        }
        for i in 5..10 {
            t.remove(&ArrayKey::Int(i));
        }
        t.insert(6, Value::Int(6));
        t.push(Value::Int(7)).unwrap();
        assert_eq!(keys(&t), ["0", "1", "2", "3", "4", "6", "7"]);
        // $b=[0,1,2]; unset($b[1]); $b[1]="z" -> 0,2,1
        let mut t = HashTable::new();
        for i in 0..3 {
            t.push(Value::Int(i)).unwrap();
        }
        assert!(t.is_packed());
        t.remove(&ArrayKey::Int(1));
        t.insert(1, Value::Int(1));
        assert_eq!(keys(&t), ["0", "2", "1"]);
        assert!(!t.is_packed());
        // array_pop then push reuses the key
        let mut t = HashTable::new();
        for i in 0..3 {
            t.push(Value::Int(i)).unwrap();
        }
        assert_eq!(t.pop().and_then(|v| v.as_int()), Some(2));
        assert_eq!(t.push(Value::Int(9)).ok(), Some(2));
    }

    #[test]
    fn store_self_at_makes_an_array_contain_itself() {
        // $t = &$a; $a[] = $t;  (the test leaks the cycle on purpose)
        let mut a = ZArray::new();
        a.make_mut().push(Value::Int(1)).unwrap();
        a.make_mut().push(Value::Null).unwrap();
        let v = a.store_self_at(&[None]).expect("stored");
        assert!(matches!(&v, Value::Array(h) if ZArray::ptr_eq(h, &a)));
        assert!(matches!(a.get_int(1), Some(Value::Array(x)) if ZArray::ptr_eq(x, &a)));
        assert_eq!(a.refcount(), 3);
        drop(v);
        assert_eq!(a.refcount(), 2);
        // A later write separates: the copy holds the old table.
        let old = a.clone();
        a.make_mut().push(Value::Int(5)).unwrap();
        assert!(!ZArray::ptr_eq(&a, &old));
        assert!(matches!(a.get_int(1), Some(Value::Array(x)) if ZArray::ptr_eq(x, &old)));
    }

    #[test]
    fn store_self_at_walks_nested_tables_and_references() {
        // $t = &$a; $a['k']['r'][] = $t; with $a['k']['r'] a reference
        let inner = PhpRef::new(Value::Array(ZArray::from_values([Value::Null])));
        let mut k = HashTable::new();
        k.insert("r", Value::Reference(inner.clone()));
        let mut a = ZArray::new();
        a.make_mut()
            .insert("k", Value::Array(ZArray::from_table(k)));
        let path = [Some(ArrayKey::from("k")), Some(ArrayKey::from("r")), None];
        assert!(a.store_self_at(&path).is_some());
        let got = inner.get();
        let Value::Array(r) = &got else {
            panic!("array expected");
        };
        assert!(matches!(r.get_int(0), Some(Value::Array(x)) if ZArray::ptr_eq(x, &a)));
        // Missing slots and shared intermediates store nothing.
        let mut b = ZArray::from_values([Value::Int(1)]);
        assert!(b.store_self_at(&[Some(ArrayKey::Int(7))]).is_none());
        assert!(b.store_self_at(&[]).is_none());
        let shared = ZArray::from_values([Value::Null]);
        let _keep = shared.clone();
        let mut c = ZArray::from_values([Value::Array(shared)]);
        assert!(c
            .store_self_at(&[Some(ArrayKey::Int(0)), Some(ArrayKey::Int(0))])
            .is_none());
        assert_eq!(b.refcount(), 1);
        assert_eq!(c.refcount(), 1);
    }

    #[test]
    fn copy_on_write() {
        let mut a = ZArray::new();
        a.make_mut().push(Value::Int(1)).unwrap();
        let mut b = a.clone();
        assert!(ZArray::ptr_eq(&a, &b));
        b.make_mut().push(Value::Int(2)).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 2);
        assert!(!ZArray::ptr_eq(&a, &b));
    }

    #[test]
    fn dup_unwraps_rc1_references() {
        let mut a = ZArray::new();
        let r = crate::PhpRef::new(Value::Int(1));
        a.make_mut().push(Value::Reference(r.clone())).unwrap();
        // refcount 2: stays a reference in the copy
        let mut shared = a.clone();
        shared.make_mut();
        assert!(shared.get_int(0).is_some_and(Value::is_reference));
        drop(r);
        drop(shared);
        let mut copy = a.clone();
        copy.make_mut();
        assert!(!copy.get_int(0).is_some_and(Value::is_reference));
    }

    /// `zend_array_dup` of a packed table with holes and every kind of
    /// element: scalars and holes are copied as they stand, a string and
    /// a nested array gain a handle, a reference held only by the source
    /// is unwrapped, a shared one stays shared; the count, the next free
    /// index and the internal pointer carry over, and dropping the copy
    /// gives the handles back.
    #[test]
    fn clone_of_a_packed_table_copies_each_kind() {
        let s = ZStr::from(String::from("heap string"));
        let inner = {
            let mut a = ZArray::new();
            a.make_mut().push(Value::Int(7)).unwrap();
            a
        };
        let shared = crate::PhpRef::new(Value::Int(5));
        let mut t = HashTable::new();
        t.push(Value::Null).unwrap();
        t.push(Value::Bool(true)).unwrap();
        t.push(Value::Int(-3)).unwrap();
        t.push(Value::Float(1.5)).unwrap();
        t.push(Value::Str(s.clone())).unwrap();
        t.push(Value::Array(inner.clone())).unwrap();
        t.push(Value::Reference(crate::PhpRef::new(Value::Int(9))))
            .unwrap();
        t.push(Value::Reference(shared.clone())).unwrap();
        t.push(Value::Int(100)).unwrap();
        t.remove(&ArrayKey::Int(8));
        t.remove(&ArrayKey::Int(1));
        t.move_next();
        t.move_next();
        assert!(matches!(t.store, Store::Packed(_)));
        let (s_rc, inner_rc, shared_rc) = (s.refcount(), inner.refcount(), shared.refcount());
        let copy = t.clone();
        assert!(matches!(copy.store, Store::Packed(_)));
        assert_eq!(copy.len(), 7);
        assert_eq!(copy.next_free, 9);
        assert_eq!(copy.current_key(), t.current_key());
        assert!(matches!(copy.get_int(0), Some(Value::Null)));
        assert!(copy.get_int(1).is_none());
        assert!(matches!(copy.get_int(2), Some(Value::Int(-3))));
        assert!(
            matches!(copy.get_int(3), Some(Value::Float(f)) if f.to_bits() == 1.5f64.to_bits())
        );
        assert!(matches!(copy.get_int(4), Some(Value::Str(c)) if ZStr::ptr_eq(c, &s)));
        assert!(matches!(copy.get_int(5), Some(Value::Array(c)) if ZArray::ptr_eq(c, &inner)));
        assert!(matches!(copy.get_int(6), Some(Value::Int(9))));
        assert!(
            matches!(copy.get_int(7), Some(Value::Reference(r)) if r.refcount() == shared_rc + 1)
        );
        assert!(copy.get_int(8).is_none());
        assert_eq!(s.refcount(), s_rc + 1);
        assert_eq!(inner.refcount(), inner_rc + 1);
        assert!(t.get_int(6).is_some_and(Value::is_reference));
        drop(copy);
        assert_eq!(s.refcount(), s_rc);
        assert_eq!(inner.refcount(), inner_rc);
        assert_eq!(shared.refcount(), shared_rc);
    }

    /// `for_each_counted` visits the arrays and references of a packed
    /// table and of a hash, in order, and skips scalars, strings, holes
    /// and resources.
    #[test]
    fn for_each_counted_visits_arrays_objects_and_references() {
        let fill = |t: &mut HashTable| {
            t.push(Value::Int(1)).unwrap();
            t.push(Value::Array(ZArray::new())).unwrap();
            t.push(Value::Str(ZStr::from("s"))).unwrap();
            t.push(Value::Resource(crate::value::Resource::new("stream", None)))
                .unwrap();
            t.push(Value::Reference(crate::PhpRef::new(Value::Null)))
                .unwrap();
            t.push(Value::Float(2.0)).unwrap();
            t.push(Value::Array(ZArray::new())).unwrap();
            t.remove(&ArrayKey::Int(5));
        };
        let mut packed = HashTable::new();
        fill(&mut packed);
        let mut hash = HashTable::with_capacity_mixed(0);
        hash.insert("k", Value::Null);
        fill(&mut hash);
        assert!(matches!(packed.store, Store::Packed(_)));
        assert!(matches!(hash.store, Store::Hash(_)));
        for t in [&packed, &hash] {
            let mut seen = Vec::new();
            t.for_each_counted(|v| {
                seen.push(match v {
                    Value::Array(_) => 'a',
                    Value::Reference(_) => 'r',
                    _ => '?',
                });
            });
            assert_eq!(seen, ['a', 'r', 'a']);
        }
    }

    #[test]
    fn internal_pointer() {
        let mut t = HashTable::new();
        for i in 0..3 {
            t.push(Value::Int(i * 10)).unwrap();
        }
        assert_eq!(t.current().and_then(Value::as_int), Some(0));
        t.move_next();
        assert_eq!(t.current().and_then(Value::as_int), Some(10));
        t.end();
        assert_eq!(t.current_key(), Some(&ArrayKey::Int(2)));
        t.move_next();
        assert!(t.current().is_none());
        t.move_prev(); // prev() past the end stays past the end
        assert!(t.current().is_none());
        t.reset();
        t.move_prev();
        assert!(t.current().is_none());
        t.reset();
        t.remove(&ArrayKey::Int(0));
        assert_eq!(t.current().and_then(Value::as_int), Some(10));
    }

    #[test]
    fn foreach_by_ref_sees_appends_and_survives_compaction() {
        let mut a = ZArray::new();
        for i in 0..4 {
            a.make_mut().push(Value::Int(i)).unwrap();
        }
        let id = a.make_mut().iterator_add(0);
        let mut seen = Vec::new();
        while let Some(pos) = a.make_mut().iterator_fetch(id) {
            let v = a.at(pos).and_then(|(_, v)| v.as_int()).unwrap_or(-1);
            seen.push(v);
            if v == 1 {
                a.make_mut().push(Value::Int(100)).unwrap();
                a.make_mut().remove(&ArrayKey::Int(0));
            }
            if v == 2 {
                // copy-on-write separation keeps the iterator position
                let _keep = a.clone();
                a.make_mut().insert("k", Value::Int(200));
            }
        }
        a.make_mut().iterator_del(id);
        assert_eq!(seen, [0, 1, 2, 3, 100, 200]);
        assert!(!a.has_iterators());
    }

    #[test]
    fn iterator_compaction_remaps_positions() {
        let mut t = HashTable::new();
        t.insert("x", Value::Int(0));
        for i in 1..8 {
            t.insert(format!("k{i}").as_str(), Value::Int(i));
        }
        let id = t.iterator_add(0);
        for _ in 0..6 {
            t.iterator_fetch(id);
        }
        for i in 1..5 {
            t.remove(&ArrayKey::from(format!("k{i}").as_str()));
        }
        // table full -> next insert compacts
        t.insert("new", Value::Int(99));
        let mut rest = Vec::new();
        while let Some(p) = t.iterator_fetch(id) {
            rest.push(t.at(p).and_then(|(_, v)| v.as_int()).unwrap_or(-1));
        }
        assert_eq!(rest, [6, 7, 99]);
    }

    #[test]
    fn stable_sort_and_renumber() {
        let mut t = HashTable::new();
        for (k, v) in [("a", 3), ("b", 1), ("c", 3), ("d", 2)] {
            t.insert(k, Value::Int(v));
        }
        let cmp = |_: &ArrayKey, a: &Value, _: &ArrayKey, b: &Value| {
            let (a, b) = (a.as_int().unwrap_or(0), b.as_int().unwrap_or(0));
            i32::from(a > b) - i32::from(a < b)
        };
        let mut asorted = t.clone();
        asorted.sort_by(cmp, false);
        assert_eq!(keys(&asorted), ["\"b\"", "\"d\"", "\"a\"", "\"c\""]);
        t.sort_by(cmp, true);
        assert_eq!(keys(&t), ["0", "1", "2", "3"]);
        assert_eq!(ints(&t), [1, 2, 3, 3]);
        assert!(t.is_packed());
        assert_eq!(t.next_index(), 4);
    }

    #[test]
    fn splice_shift_unshift() {
        let mut t = HashTable::new();
        for i in 0..5 {
            t.push(Value::Int(i)).unwrap();
        }
        let mut rep = HashTable::new();
        rep.push(Value::Int(10)).unwrap();
        rep.push(Value::Int(11)).unwrap();
        let removed = t.splice(1, 2, Some(&rep), true).unwrap();
        assert_eq!(ints(&t), [0, 10, 11, 3, 4]);
        assert_eq!(keys(&t), ["0", "1", "2", "3", "4"]);
        assert_eq!(ints(&removed), [1, 2]);
        t.splice(-1, 0, None, false);
        assert_eq!(t.len(), 5);
        t.splice(-2, -1, None, false);
        assert_eq!(ints(&t), [0, 10, 11, 4]);

        let mut h = HashTable::new();
        h.insert(5, Value::Int(1));
        h.insert("s", Value::Int(2));
        h.insert(9, Value::Int(3));
        assert_eq!(h.shift().and_then(|v| v.as_int()), Some(1));
        assert_eq!(keys(&h), ["\"s\"", "0"]);
        assert_eq!(h.next_index(), 1);
        h.unshift(vec![Value::Int(7)]);
        assert_eq!(keys(&h), ["0", "\"s\"", "1"]);
    }

    #[test]
    fn public_iterator_remapping() {
        let mut t = HashTable::new();
        for i in 0..4 {
            t.push(Value::Int(i)).unwrap();
        }
        let id = t.iterator_add(2);
        assert_eq!(t.iterators_lower_pos(0), 2);
        t.iterators_update(2, 0);
        assert_eq!(t.iterator_positions(), &[(id, 0)]);
        t.iterators_advance(3);
        assert_eq!(t.iterator_pos(id), 3);
        let mut rebuilt = HashTable::with_capacity_mixed(4);
        assert!(!rebuilt.is_packed());
        rebuilt.take_iterators_from(&mut t);
        assert!(!t.has_iterators());
        assert_eq!(rebuilt.iterator_pos(id), 3);
        let single = Value::Reference(crate::PhpRef::new(Value::Int(1)));
        assert!(!copy_for_insert(&single).is_reference());
        let shared = single.clone();
        assert!(copy_for_insert(&shared).is_reference());
    }

    #[test]
    fn proptable_conversion() {
        let mut t = HashTable::new();
        t.insert(1, Value::Int(1));
        t.insert("a", Value::Int(2));
        let p = t.to_proptable();
        assert!(p.get_str(b"1").is_some());
        assert!(p.get_int(1).is_none());
        let s = p.to_symtable();
        assert!(s.get_int(1).is_some());
    }

    /// Property test: random operations against a simple reference model.
    #[test]
    fn random_ops_match_model() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _round in 0..200 {
            let mut t = HashTable::new();
            let mut model: Vec<(ArrayKey, i64)> = Vec::new();
            let mut next_free: i64 = i64::MIN;
            for step in 0..300 {
                let op = rnd() % 10;
                #[allow(clippy::cast_possible_wrap)]
                let kint = (rnd() % 40) as i64 - 5;
                let key = if rnd() % 3 == 0 {
                    ArrayKey::Str(ZStr::from(format!("s{}", rnd() % 20)))
                } else {
                    ArrayKey::Int(kint)
                };
                match op {
                    0..=3 => {
                        t.insert(key.clone(), Value::Int(step));
                        if let Some(e) = model.iter_mut().find(|e| e.0 == key) {
                            e.1 = step;
                        } else {
                            model.push((key.clone(), step));
                        }
                        if let ArrayKey::Int(h) = key {
                            // the packed quirk can lower next_free; mirror the table
                            if h >= next_free {
                                next_free = h + 1;
                            }
                        }
                    }
                    4..=6 => {
                        let removed = t.remove(&key).and_then(|v| v.as_int());
                        let pos = model.iter().position(|e| e.0 == key);
                        assert_eq!(removed, pos.map(|p| model.remove(p).1));
                    }
                    7 => {
                        let k = t.next_index();
                        if t.push(Value::Int(step)).is_ok() {
                            model.push((ArrayKey::Int(k), step));
                        }
                    }
                    8 => {
                        let mut c = t.clone();
                        c.insert("tmp", Value::Null);
                        let _ = c;
                    }
                    _ => {
                        let got = t.get(&key).and_then(Value::as_int);
                        let want = model.iter().find(|e| e.0 == key).map(|e| e.1);
                        assert_eq!(got, want);
                    }
                }
                assert_eq!(t.len(), model.len());
                let got: Vec<(ArrayKey, i64)> = t
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_int().unwrap_or(-1)))
                    .collect();
                assert_eq!(got, model);
                let _ = next_free;
            }
        }
    }

    /// `"k=v,..."` for an array of integers.
    fn render(t: &HashTable) -> String {
        t.iter()
            .map(|(k, v)| {
                let k = String::from_utf8_lossy(&k.to_bytes()).into_owned();
                match v {
                    Value::Int(i) => format!("{k}={i}"),
                    _ => format!("{k}=?"),
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    fn int_cmp(a: &Value, b: &Value) -> i32 {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => i32::from(x > y) - i32::from(x < y),
            _ => 0,
        }
    }

    fn xyz() -> HashTable {
        let mut t = HashTable::new();
        t.insert("x", Value::Int(3));
        t.insert("y", Value::Int(1));
        t.insert("z", Value::Int(2));
        t
    }

    /// The table a reference holds (empty when it holds no array).
    fn table_of(r: &PhpRef) -> HashTable {
        match r.get() {
            Value::Array(a) => (*a).clone(),
            _ => HashTable::new(),
        }
    }

    /// Runs `sort_in_ref` on `xyz()` bound to a reference, logging what
    /// the reference holds at every comparison and whether a key lookup in
    /// it succeeds; returns the table it holds afterwards.
    fn live_sort_log(renumber: bool) -> (HashTable, Vec<String>) {
        let r = PhpRef::new(Value::Array(ZArray::from_table(xyz())));
        let mut log = Vec::new();
        HashTable::sort_in_ref(
            &r,
            |_, a, _, b| {
                if let Value::Array(live) = r.get() {
                    let found = live.get(&ArrayKey::from("x")).is_some();
                    log.push(format!("{} found={found}", render(&live)));
                }
                int_cmp(a, b)
            },
            renumber,
        );
        (table_of(&r), log)
    }

    #[test]
    fn sort_in_ref_shows_the_live_table_without_lookups() {
        // zend_sort_3: compares (3,1) and (2,1), swaps x/y, compares
        // (3,2), swaps.
        let (t, log) = live_sort_log(false);
        assert_eq!(
            log,
            [
                "x=3,y=1,z=2 found=false",
                "x=3,y=1,z=2 found=false",
                "y=1,x=3,z=2 found=false",
            ]
        );
        assert_eq!(render(&t), "y=1,z=2,x=3");
        assert!(matches!(t.get(&ArrayKey::from("x")), Some(Value::Int(3))));
    }

    #[test]
    fn sort_in_ref_renumber_keeps_keys_in_place() {
        // zend_hash_bucket_renum_swap moves values only.
        let (t, log) = live_sort_log(true);
        assert_eq!(log[2], "x=1,y=3,z=2 found=false");
        assert_eq!(render(&t), "0=1,1=2,2=3");
        assert!(t.is_packed());
        assert_eq!(t.next_index(), 3);
    }

    #[test]
    fn sort_in_ref_matches_sort_by() {
        // 40 elements: the quicksort part of zend_sort, with ties.
        let mut plain = HashTable::new();
        for i in 0..40_i64 {
            plain.insert(format!("k{i}").as_str(), Value::Int((i * 7) % 11));
        }
        plain.remove(&ArrayKey::from("k5"));
        for renumber in [false, true] {
            let r = PhpRef::new(Value::Array(ZArray::from_table(plain.clone())));
            HashTable::sort_in_ref(&r, |_, a, _, b| int_cmp(a, b), renumber);
            let mut want = plain.clone();
            want.sort_by(|_, a, _, b| int_cmp(a, b), renumber);
            assert_eq!(render(&table_of(&r)), render(&want), "renumber={renumber}");
        }
    }

    #[test]
    fn sort_in_ref_separates_a_shared_table() {
        let keep = ZArray::from_table(xyz());
        let r = PhpRef::new(Value::Array(keep.clone()));
        HashTable::sort_in_ref(&r, |_, a, _, b| int_cmp(a, b), false);
        assert_eq!(render(&keep), "x=3,y=1,z=2");
        assert_eq!(render(&table_of(&r)), "y=1,z=2,x=3");
    }

    #[test]
    fn sort_in_ref_detaches_when_the_reference_is_replaced() {
        let r = PhpRef::new(Value::Array(ZArray::from_table(xyz())));
        let mut calls = 0;
        HashTable::sort_in_ref(
            &r,
            |_, a, _, b| {
                calls += 1;
                if calls == 1 {
                    r.set(Value::Int(7));
                }
                int_cmp(a, b)
            },
            false,
        );
        assert_eq!(calls, 3);
        // The sorted table was dropped; the PHP code's value stays.
        assert!(matches!(r.get(), Value::Int(7)));
    }

    #[test]
    fn sort_in_ref_write_keeps_the_separated_copy() {
        // `$c['new'] = 9` from a comparison separates the half-sorted live
        // table (rehashed, so lookups work) and php-src drops the sorted one.
        let r = PhpRef::new(Value::Array(ZArray::from_table(xyz())));
        let mut calls = 0;
        HashTable::sort_in_ref(
            &r,
            |_, a, _, b| {
                calls += 1;
                if calls == 3 {
                    if let Value::Array(arr) = &mut *r.borrow_mut() {
                        arr.make_mut().insert("new", Value::Int(9));
                    }
                }
                int_cmp(a, b)
            },
            false,
        );
        assert_eq!(calls, 3);
        let t = table_of(&r);
        assert_eq!(render(&t), "y=1,x=3,z=2,new=9");
        assert!(matches!(t.get(&ArrayKey::from("x")), Some(Value::Int(3))));
    }

    /// The literal tables built in one step are the tables the general
    /// constructors make: kind, table size, room, next index, and the same
    /// tables after the same appends (sizes 0, 1, 8 and 9: below, at and
    /// past the smallest table size).
    #[test]
    fn sized_literal_tables_match_the_general_ones() {
        for n in [0usize, 1, 8, 9] {
            let packed = ZArray::packed_sized(n);
            let general = HashTable::with_capacity_packed(n);
            assert_eq!(packed.kind(), Kind::Packed, "{n}");
            assert_eq!(packed.table_size(), general.table_size(), "{n}");
            assert_eq!(packed.packed_capacity(), general.packed_capacity(), "{n}");
            assert_eq!(packed.next_free_element(), i64::MIN, "{n}");
            assert!(packed.is_empty() && !packed.is_untouched(), "{n}");
            let mixed = ZArray::mixed_sized(n);
            let general_mixed = HashTable::with_capacity_mixed(n);
            assert_eq!(mixed.kind(), Kind::Hash, "{n}");
            assert_eq!(mixed.table_size(), general_mixed.table_size(), "{n}");
            match (&mixed.store, &general_mixed.store) {
                (Store::Hash(a), Store::Hash(b)) => {
                    assert_eq!(a.index.len(), b.index.len(), "{n}");
                    assert_eq!(a.buckets.capacity(), b.buckets.capacity(), "{n}");
                    assert!(a.index.iter().all(|&i| i == INVALID), "{n}");
                }
                _ => panic!("not hashes: {n}"),
            }
            assert_eq!(mixed.next_free_element(), i64::MIN, "{n}");
            // The same appends make the same tables.
            let (mut a, mut b) = (packed, ZArray::from_table(general));
            let (mut c, mut d) = (mixed, ZArray::from_table(general_mixed));
            for i in 0..n {
                let i = i64::try_from(i).unwrap_or(0);
                let _ = a.make_mut().push(Value::Int(i));
                let _ = b.make_mut().push(Value::Int(i));
                c.make_mut().insert(format!("k{i}").as_str(), Value::Int(i));
                d.make_mut().insert(format!("k{i}").as_str(), Value::Int(i));
            }
            assert_eq!(render(&a), render(&b), "{n}");
            assert_eq!(a.table_size(), b.table_size(), "{n}");
            assert_eq!(a.next_index(), b.next_index(), "{n}");
            assert_eq!(render(&c), render(&d), "{n}");
            assert_eq!(c.table_size(), d.table_size(), "{n}");
            assert_eq!(c.next_index(), d.next_index(), "{n}");
        }
    }

    /// `lean_push` makes the table `push` makes while the list has room,
    /// with counted (strings, arrays) and plain elements, and gives the
    /// value back for a full list, a hash, an empty table with no storage
    /// and a list whose next index is past its end.
    #[test]
    fn lean_push_agrees_with_push() {
        for n in [0usize, 1, 8, 9] {
            let mut lean = HashTable::with_capacity_packed(n);
            let mut general = HashTable::with_capacity_packed(n);
            let ts = lean.table_size() as usize;
            for i in 0..ts + 2 {
                let k = i64::try_from(i).unwrap_or(0);
                let v = match i % 3 {
                    0 => Value::Int(k),
                    1 => Value::Str(ZStr::from(format!("s{i}"))),
                    _ => Value::Array(ZArray::from_values([Value::Int(k)])),
                };
                let _ = general.push(v.clone());
                // The lean push takes the value while the list has room,
                // also the room a general push made by growing it.
                let room = lean.len() < lean.table_size() as usize;
                match lean.lean_push(v) {
                    Ok(()) => assert!(room, "{n}: {i}"),
                    Err(v) => {
                        assert!(!room, "{n}: {i}");
                        let _ = lean.push(v);
                    }
                }
            }
            assert_eq!(lean.len(), general.len(), "{n}");
            assert_eq!(lean.table_size(), general.table_size(), "{n}");
            assert_eq!(lean.next_index(), general.next_index(), "{n}");
            assert_eq!(
                lean.keys().cloned().collect::<Vec<_>>(),
                general.keys().cloned().collect::<Vec<_>>(),
                "{n}"
            );
            for (a, b) in lean.values().zip(general.values()) {
                assert_eq!(std::mem::discriminant(a), std::mem::discriminant(b), "{n}");
            }
        }
        // Not a list with room at its end: the general push's.
        let mut hash = HashTable::with_capacity_mixed(8);
        assert!(hash.lean_push(Value::Int(1)).is_err());
        assert!(hash.is_empty());
        let mut uninit = HashTable::new();
        assert!(uninit.lean_push(Value::Int(1)).is_err());
        assert!(uninit.is_untouched());
        let mut gap = HashTable::with_capacity_packed(8);
        assert!(gap.lean_push(Value::Int(0)).is_ok());
        gap.set_next_free_element(5);
        assert!(gap.lean_push(Value::Int(5)).is_err());
        assert_eq!(gap.len(), 1);
        assert!(matches!(gap.push(Value::Int(5)), Ok(5)));
        assert_eq!(render(&gap), "0=0,5=5");
    }

    /// A copy of a hash without holes takes the source's chains and index
    /// as they stand: they equal what re-linking the copy builds, every
    /// key is found, and the copy grows and takes new keys as a table of
    /// its own. A hash with holes is still compacted (sizes 1, 8, 9 and
    /// 40; int and string keys, counted and plain values).
    #[test]
    fn clone_of_a_hash_keeps_its_chains() {
        for n in [1i64, 8, 9, 40] {
            let mut t = HashTable::with_capacity_mixed(0);
            for i in 0..n {
                if i % 2 == 0 {
                    t.insert(ArrayKey::Int(i * 7 - 9), Value::Int(i));
                } else {
                    t.insert(
                        format!("k{i}").as_str(),
                        Value::Str(ZStr::from(format!("v{i}"))),
                    );
                }
            }
            let copy = t.clone();
            let (Store::Hash(a), Store::Hash(b)) = (&t.store, &copy.store) else {
                panic!("not hashes: {n}");
            };
            assert_eq!(a.index, b.index, "{n}");
            let nexts = |d: &HashData| d.buckets.iter().map(|b| b.next).collect::<Vec<_>>();
            assert_eq!(nexts(a), nexts(b), "{n}");
            let mut relinked = copy.clone();
            relinked.rehash_without_compaction();
            let Store::Hash(r) = &relinked.store else {
                panic!("not a hash: {n}");
            };
            assert_eq!(r.index, b.index, "{n}");
            assert_eq!(nexts(r), nexts(b), "{n}");
            for (k, _) in t.iter() {
                assert!(copy.contains_key(k), "{n}: {k:?}");
            }
            assert_eq!(copy.len(), t.len(), "{n}");
            assert_eq!(copy.next_index(), t.next_index(), "{n}");
            let mut grown = copy;
            for i in 0..20 {
                grown.insert(format!("new{i}").as_str(), Value::Int(i));
            }
            assert_eq!(grown.len(), t.len() + 20, "{n}");
            for (k, _) in t.iter() {
                assert!(grown.contains_key(k), "{n}: {k:?}");
            }
            assert!(matches!(
                grown.get(&ArrayKey::from("new19")),
                Some(Value::Int(19))
            ));
            // With a hole: the copy is compacted and every key found.
            let first = t.first().map(|(k, _)| k.clone());
            if let Some(k) = first {
                drop(t.remove(&k));
            }
            let compacted = t.clone();
            assert_eq!(compacted.used() as usize, compacted.len(), "{n}");
            for (k, _) in t.iter() {
                assert!(compacted.contains_key(k), "{n}: {k:?}");
            }
            // Half-way through an in-place sort (the index reset, the
            // buckets kept): the copy is re-linked, every key found.
            if t.len() > 1 {
                let keys: Vec<ArrayKey> = t.keys().cloned().collect();
                assert!(t.live_sort_begin(false).is_some(), "{n}");
                let mid = t.clone();
                for k in &keys {
                    assert!(mid.contains_key(k), "{n}: {k:?}");
                }
                t.live_sort_finish(false);
                for k in &keys {
                    assert!(t.contains_key(k), "{n}: {k:?}");
                }
            }
        }
    }
}
