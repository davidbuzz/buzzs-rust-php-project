//! `ZArray`: PHP's ordered hash table (`HashTable` / `zend_array`).
//!
//! The layout mirrors `Zend/zend_hash.c` closely because several PHP-visible
//! behaviours depend on it:
//!
//! * buckets live in insertion order in a vector that may contain holes
//!   (deleted slots); `nNumUsed` is the vector length;
//! * *packed* arrays (keys `0..n` in order, possibly with holes) have no hash
//!   index; inserting out of order converts them to *hash* arrays;
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
    #[must_use]
    pub fn from_zstr(s: &ZStr) -> Self {
        match handle_numeric_str(s.as_bytes()) {
            Some(i) => ArrayKey::Int(i),
            None => ArrayKey::Str(s.clone()),
        }
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
/// word in php-src) lives in the table's index next to the hash slots. A
/// bucket is therefore a value plus 16 bytes, 32 bytes with a 16-byte value.
#[derive(Clone)]
struct Bucket {
    /// The value; `None` for a deleted slot (`IS_UNDEF`).
    val: Option<Value>,
    /// The key (`Int(0)` in a deleted slot, whose key string is released).
    key: ArrayKey,
}

impl Bucket {
    /// The bucket of a live element.
    #[inline]
    fn new(key: ArrayKey, val: Value) -> Self {
        Bucket {
            val: Some(val),
            key,
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
    /// `2 * nTableSize` hash slots (chain heads, `HT_HASH`), then one chain
    /// link per bucket position (`nTableSize` entries, `Z_NEXT`).
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
    static EMPTY_ARRAY: ZArray = ZArray(Rc::new(HashTable::new()));
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

/// A fresh index for `table_size` buckets: `2 * table_size` empty hash
/// slots followed by `table_size` chain links.
fn new_index(table_size: u32) -> Box<[u32]> {
    vec![INVALID; table_size as usize * 3].into_boxed_slice()
}

/// The hash slot of `h` (`nIndex = h | nTableMask`: `h` modulo the
/// `2 * nTableSize` slots).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn hash_slot(table_size: u32, h: u64) -> usize {
    (h as usize) & (table_size as usize * 2 - 1)
}

/// The index entry holding the chain link of the bucket at `idx`.
#[inline]
fn link_slot(table_size: u32, idx: u32) -> usize {
    table_size as usize * 2 + idx as usize
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
/// take `24 n` counted bytes, so the hidden part stays below the visible one
/// and within `memory_limit`'s order of magnitude. VM code that only needs
/// the key of a position should use [`HashTable::key_at`], which never
/// touches this table.
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
    fn new(table_size: u32) -> Self {
        HashData {
            buckets: Vec::new(),
            index: new_index(table_size),
        }
    }

    /// The first bucket of `h`'s chain (`HT_HASH(ht, nIndex)`).
    #[inline]
    fn head(&self, ts: u32, h: u64) -> u32 {
        self.index.get(hash_slot(ts, h)).copied().unwrap_or(INVALID)
    }

    /// The bucket after `idx` in its chain (`Z_NEXT`).
    #[inline]
    fn next_of(&self, ts: u32, idx: u32) -> u32 {
        self.index
            .get(link_slot(ts, idx))
            .copied()
            .unwrap_or(INVALID)
    }

    #[inline]
    fn set_next(&mut self, ts: u32, idx: u32, next: u32) {
        if let Some(n) = self.index.get_mut(link_slot(ts, idx)) {
            *n = next;
        }
    }

    /// `zend_hash_index_find_bucket`.
    #[inline]
    fn find_int(&self, ts: u32, h: i64) -> Option<u32> {
        #[allow(clippy::cast_sign_loss)]
        let mut idx = self.head(ts, h as u64);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if matches!(b.key, ArrayKey::Int(k) if k == h) {
                return Some(idx);
            }
            idx = self.next_of(ts, idx);
        }
        None
    }

    /// `zend_hash_str_find_bucket`: equal hash, then equal bytes.
    fn find_str(&self, ts: u32, s: &[u8], h: u64) -> Option<u32> {
        let mut idx = self.head(ts, h);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if k.hash_value() == h && k.as_bytes() == s {
                    return Some(idx);
                }
            }
            idx = self.next_of(ts, idx);
        }
        None
    }

    /// `zend_hash_find_bucket`: the very same string, or equal hash and
    /// bytes.
    #[inline]
    fn find_zstr(&self, ts: u32, key: &ZStr) -> Option<u32> {
        let h = key.hash_value();
        let mut idx = self.head(ts, h);
        while idx != INVALID {
            let b = self.buckets.get(idx as usize)?;
            if let ArrayKey::Str(k) = &b.key {
                if ZStr::ptr_eq(k, key) || (k.hash_value() == h && k.as_bytes() == key.as_bytes()) {
                    return Some(idx);
                }
            }
            idx = self.next_of(ts, idx);
        }
        None
    }

    /// Rebuilds every chain over the buckets as they stand, holes kept
    /// (`zend_hash_rehash` without compaction).
    #[allow(clippy::cast_possible_truncation)]
    fn relink(&mut self, ts: u32) {
        if self.index.len() != ts as usize * 3 {
            self.index = new_index(ts);
        }
        let (heads, links) = self.index.split_at_mut(ts as usize * 2);
        heads.fill(INVALID);
        let mask = heads.len() - 1;
        for (i, b) in self.buckets.iter().enumerate() {
            if b.val.is_none() {
                continue;
            }
            let s = (b.key.hash() as usize) & mask;
            if let Some(l) = links.get_mut(i) {
                *l = heads[s];
            }
            heads[s] = to_u32(i);
        }
    }

    /// Unlinks the live bucket at `idx` from its chain and empties it
    /// (`_zend_hash_del_el_ex`), returning its value.
    fn unlink_take(&mut self, ts: u32, idx: u32) -> Option<Value> {
        let b = self.buckets.get(idx as usize)?;
        b.val.as_ref()?;
        let s = hash_slot(ts, b.key.hash());
        let next = self.next_of(ts, idx);
        let head = self.index.get(s).copied().unwrap_or(INVALID);
        if head == idx {
            self.index[s] = next;
        } else {
            let mut p = head;
            while p != INVALID {
                let pn = self.next_of(ts, p);
                if pn == idx {
                    self.set_next(ts, p, next);
                    break;
                }
                p = pn;
            }
        }
        let b = self.buckets.get_mut(idx as usize)?;
        let val = b.val.take();
        if matches!(b.key, ArrayKey::Str(_)) {
            b.key = ArrayKey::Int(0);
        }
        val
    }
}

impl HashTable {
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

    /// Creates an empty table sized for `n` elements (`zend_new_array(n)`).
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        HashTable {
            store: Store::Uninit,
            num_elements: 0,
            table_size: check_size(to_u32(n)),
            internal_ptr: 0,
            next_free: i64::MIN,
            iterators: IterList::default(),
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

    fn real_init_packed(&mut self) {
        self.store = Store::Packed(Vec::new());
    }

    fn real_init_mixed(&mut self) {
        self.store = Store::Hash(HashData::new(self.table_size));
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

    fn do_resize(&mut self) {
        let n = self.num_elements;
        if self.used() > n + (n >> 5) {
            self.rehash();
        } else if self.table_size < HT_MAX_SIZE {
            self.table_size += self.table_size;
            if let Store::Hash(d) = &mut self.store {
                d.index = new_index(self.table_size);
            }
            self.rehash();
        }
    }

    /// `zend_hash_packed_to_hash`: the values become buckets keyed by their
    /// positions, then `zend_hash_rehash` drops the holes (remapping the
    /// internal pointer and the registered iterators) and links the chains.
    fn packed_to_hash(&mut self) {
        let buckets = match std::mem::replace(&mut self.store, Store::Uninit) {
            Store::Uninit => Vec::new(),
            Store::Packed(v) => v
                .into_iter()
                .enumerate()
                .map(|(i, val)| Bucket {
                    val,
                    key: ArrayKey::Int(idx_key(i)),
                })
                .collect(),
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
                let i = usize::try_from(h).ok()?;
                match v.get(i) {
                    Some(Some(_)) => Some(to_u32(i)),
                    _ => None,
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
            if h >= self.next_free {
                self.next_free = if h < i64::MAX { h + 1 } else { i64::MAX };
            }
        }
        let ts = self.table_size;
        let d = self.hash_data_mut();
        let idx = to_u32(d.buckets.len());
        let s = hash_slot(ts, key.hash());
        let head = match d.index.get_mut(s) {
            Some(slot) => std::mem::replace(slot, idx),
            None => INVALID,
        };
        d.set_next(ts, idx, head);
        d.buckets.push(Bucket::new(key, val));
        self.num_elements += 1;
        idx
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
    fn index_insert(&mut self, h: i64, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        let mut h = h;
        if mode == Mode::Next && h == i64::MIN {
            h = 0;
        }
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
                    self.table_size += self.table_size;
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
    /// normalization).
    fn str_insert(&mut self, key: ZStr, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
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

    fn key_insert(&mut self, key: ArrayKey, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match key {
            ArrayKey::Int(h) => self.index_insert(h, val, mode),
            ArrayKey::Str(s) => self.str_insert(s, val, mode),
        }
    }

    /// `zend_hash_lookup`: the position of `key` (one probe), inserted with
    /// `null` when absent, and whether it already existed.
    fn lookup_idx(&mut self, key: ArrayKey) -> (u32, bool) {
        let Ok(r) = self.key_insert(key, Value::Null, Mode::Lookup) else {
            unreachable!("lookup never fails")
        };
        r
    }

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

    /// Looks up an integer key (`zend_hash_index_find`; a packed table
    /// indexes its values directly).
    #[inline]
    #[must_use]
    pub fn get_int(&self, i: i64) -> Option<&Value> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => v.get(usize::try_from(i).ok()?)?.as_ref(),
            Store::Hash(d) => {
                let idx = d.find_int(self.table_size, i)?;
                d.buckets.get(idx as usize)?.val.as_ref()
            }
        }
    }

    /// Mutable lookup of an integer key.
    #[inline]
    pub fn get_int_mut(&mut self, i: i64) -> Option<&mut Value> {
        let idx = self.find_int(i)?;
        self.val_mut(idx)
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
        let idx = self.find(key)?;
        self.val_mut(idx)
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
    pub fn push(&mut self, val: Value) -> Result<i64, Value> {
        let h = self.next_index();
        self.index_insert(h, val, Mode::Next)?;
        Ok(h)
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
            },
            Store::Packed(v) => IterInner::Packed {
                slots: v.iter(),
                front: 0,
                back: to_u32(v.len()),
            },
            Store::Hash(d) => IterInner::Hash(d.buckets.iter()),
        };
        Iter {
            inner,
            remaining: self.num_elements as usize,
        }
    }

    /// Iterates `(key, &mut value)` pairs in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ArrayKey, &mut Value)> {
        let (packed, buckets): (&mut [Option<Value>], &mut [Bucket]) = match &mut self.store {
            Store::Uninit => (Default::default(), Default::default()),
            Store::Packed(v) => (v.as_mut_slice(), Default::default()),
            Store::Hash(d) => (Default::default(), d.buckets.as_mut_slice()),
        };
        packed
            .iter_mut()
            .enumerate()
            .filter_map(|(i, slot)| {
                let key: &ArrayKey = int_keys::get(to_u32(i));
                slot.as_mut().map(|v| (key, v))
            })
            .chain(buckets.iter_mut().filter_map(|b| {
                let Bucket { val, key } = b;
                let key: &ArrayKey = key;
                val.as_mut().map(|v| (key, v))
            }))
    }

    /// Iterates keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &ArrayKey> {
        self.iter().map(|(k, _)| k)
    }

    /// Iterates values in order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        let (packed, buckets): (&[Option<Value>], &[Bucket]) = match &self.store {
            Store::Uninit => (Default::default(), Default::default()),
            Store::Packed(v) => (v.as_slice(), Default::default()),
            Store::Hash(d) => (Default::default(), d.buckets.as_slice()),
        };
        packed
            .iter()
            .flatten()
            .chain(buckets.iter().filter_map(|b| b.val.as_ref()))
    }

    /// First valid position at or after `pos` (`_zend_hash_get_valid_pos`);
    /// returns [`HashTable::used`] when there is none.
    #[must_use]
    pub fn valid_pos(&self, pos: u32) -> u32 {
        let used = self.used() as usize;
        let mut p = pos as usize;
        while p < used && !self.is_occupied(p) {
            p += 1;
        }
        to_u32(p.max(pos as usize))
    }

    /// The entry at a position (holes and out-of-range give `None`).
    #[inline]
    #[must_use]
    pub fn at(&self, pos: u32) -> Option<(&ArrayKey, &Value)> {
        match &self.store {
            Store::Uninit => None,
            Store::Packed(v) => v
                .get(pos as usize)?
                .as_ref()
                .map(|x| (int_keys::get(pos), x)),
            Store::Hash(d) => {
                let b = d.buckets.get(pos as usize)?;
                b.val.as_ref().map(|x| (&b.key, x))
            }
        }
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
        if renumber {
            self.store = Store::Packed(items.into_iter().map(|it| Some(it.val)).collect());
            self.next_free = idx_key(n);
        } else {
            let buckets = items
                .into_iter()
                .map(|it| Bucket::new(it.key, it.val))
                .collect();
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
        let buckets: Vec<Bucket> = into_entries(store)
            .map(|(k, v)| Bucket::new(k, v))
            .collect();
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
                    let vals: Vec<Option<Value>> = d
                        .buckets
                        .into_iter()
                        .filter_map(|b| b.val)
                        .map(Some)
                        .collect();
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
        let mut out = HashTable::with_capacity(self.len());
        for v in self.values() {
            let _ = out.push(copy_for_insert(v));
        }
        out
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

impl Clone for HashTable {
    /// `zend_array_dup`: copies the table; references with refcount 1 (other
    /// than a reference to this very array) are unwrapped; holes are removed
    /// from hash tables (packed tables keep them); iterators are duplicated so
    /// that `foreach` by reference can find its position in the copy.
    fn clone(&self) -> Self {
        let self_ptr: *const HashTable = self;
        let dup_val = |v: &Value| -> Value {
            if let Value::Reference(r) = v {
                if r.refcount() == 1 {
                    if let Some(inner) = r.try_borrow() {
                        let is_self = matches!(&*inner, Value::Array(a) if std::ptr::eq(Rc::as_ptr(&a.0), self_ptr));
                        if !is_self {
                            return inner.clone();
                        }
                    }
                }
            }
            v.clone()
        };
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
        };
        if self.num_elements == 0 {
            out.table_size = HT_MIN_SIZE;
            out.internal_ptr = 0;
            return out;
        }
        match &self.store {
            Store::Uninit => {}
            Store::Packed(v) => {
                out.store = Store::Packed(v.iter().map(|s| s.as_ref().map(&dup_val)).collect());
            }
            Store::Hash(d) => {
                let buckets = d
                    .buckets
                    .iter()
                    .map(|b| match &b.val {
                        Some(v) => Bucket::new(b.key.clone(), dup_val(v)),
                        None => Bucket {
                            val: None,
                            key: ArrayKey::Int(0),
                        },
                    })
                    .collect();
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
    /// from either end (the keys).
    Packed {
        slots: std::slice::Iter<'a, Option<Value>>,
        front: u32,
        back: u32,
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
            IterInner::Packed { slots, front, .. } => loop {
                let slot = slots.next()?;
                let pos = *front;
                *front += 1;
                if let Some(v) = slot {
                    break (int_keys::get(pos), v);
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
            IterInner::Packed { slots, back, .. } => loop {
                let slot = slots.next_back()?;
                *back -= 1;
                if let Some(v) = slot {
                    break (int_keys::get(*back), v);
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

impl<'a> IntoIterator for &'a HashTable {
    type Item = (&'a ArrayKey, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
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

    /// Wraps a table (also accepts a boxed table, such as an object's
    /// dynamic property table).
    #[must_use]
    pub fn from_table(t: impl Into<HashTable>) -> Self {
        ZArray(Rc::new(t.into()))
    }

    /// Builds a list from values.
    pub fn from_values<I: IntoIterator<Item = Value>>(values: I) -> Self {
        let iter = values.into_iter();
        let mut t = HashTable::with_capacity(iter.size_hint().0);
        for v in iter {
            let _ = t.push(v);
        }
        ZArray::from_table(t)
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
    pub fn make_mut(&mut self) -> &mut HashTable {
        Rc::make_mut(&mut self.0)
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

    /// Takes the table out if unshared, otherwise clones it.
    #[must_use]
    pub fn into_table(self) -> HashTable {
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
        let _ = Rc::make_mut(&mut self.0);
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
    use super::*;

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
        // bucket a value plus a 16-byte key (`Bucket`: zval + h + key);
        // with a 16-byte value these are php-src's 16 and 32 bytes.
        assert_eq!(
            std::mem::size_of::<Option<Value>>(),
            std::mem::size_of::<Value>()
        );
        assert_eq!(std::mem::size_of::<ArrayKey>(), 16);
        assert_eq!(
            std::mem::size_of::<Bucket>(),
            std::mem::size_of::<Value>() + 16
        );
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
        // 16 bytes per position plus at most one 1 MiB chunk of slack,
        // never a doubling overshoot (which would be 32 MiB here).
        let bound = (top as usize + 2 * (1 << 16)) * std::mem::size_of::<ArrayKey>();
        assert!(int_keys::allocated_bytes() <= bound);
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
        assert_eq!(
            t.get_zstr(&ZStr::from("beta")).and_then(Value::as_int),
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
}
