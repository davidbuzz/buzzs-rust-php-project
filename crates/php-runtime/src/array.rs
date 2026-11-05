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

#[derive(Clone)]
struct Bucket {
    key: ArrayKey,
    val: Value,
    /// Next bucket index in the collision chain (hash mode only).
    next: u32,
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
    data: Vec<Option<Bucket>>,
    /// Hash index (`2 * table_size` slots) in hash mode, empty otherwise.
    index: Vec<u32>,
    num_elements: u32,
    table_size: u32,
    next_free: i64,
    internal_ptr: u32,
    kind: Kind,
    /// Registered iterator positions: `(id, pos)`.
    iterators: Vec<(IterId, u32)>,
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
            data: Vec::new(),
            index: Vec::new(),
            num_elements: 0,
            table_size: check_size(to_u32(n)),
            next_free: i64::MIN,
            internal_ptr: 0,
            kind: Kind::Uninit,
            iterators: Vec::new(),
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
        to_u32(self.data.len())
    }

    /// Whether the table is packed (`HT_IS_PACKED`; uninitialized tables are
    /// reported as packed, like `zend_empty_array`).
    #[must_use]
    pub fn is_packed(&self) -> bool {
        self.kind != Kind::Hash
    }

    /// Whether the table has no holes (`HT_IS_WITHOUT_HOLES`).
    #[must_use]
    pub fn is_without_holes(&self) -> bool {
        self.data.len() == self.num_elements as usize
    }

    /// Whether the keys are exactly `0, 1, 2, ...` in order (`array_is_list`).
    #[must_use]
    pub fn is_list(&self) -> bool {
        if self.kind != Kind::Hash && self.is_without_holes() {
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

    #[inline]
    fn slot(&self, h: u64) -> usize {
        // nIndex = h | nTableMask, i.e. h modulo the index size.
        #[allow(clippy::cast_possible_truncation)]
        let s = (h as usize) & (self.index.len() - 1);
        s
    }

    fn real_init_packed(&mut self) {
        self.kind = Kind::Packed;
    }

    fn real_init_mixed(&mut self) {
        self.kind = Kind::Hash;
        self.index = vec![INVALID; (self.table_size as usize) * 2];
    }

    /// Rebuilds the hash index and compacts holes (`zend_hash_rehash`).
    fn rehash(&mut self) {
        if self.num_elements == 0 {
            if self.kind != Kind::Uninit {
                self.data.clear();
                self.index.fill(INVALID);
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
        self.index.fill(INVALID);
        for i in 0..self.data.len() {
            let Some(b) = &self.data[i] else { continue };
            let s = self.slot(b.key.hash());
            let head = self.index[s];
            if let Some(b) = &mut self.data[i] {
                b.next = head;
            }
            self.index[s] = to_u32(i);
        }
    }

    /// Removes holes, remapping the internal pointer and iterator positions
    /// like `zend_hash_rehash` does: a position pointing at (or into a hole
    /// before) an element moves with that element.
    fn compact(&mut self) {
        let old_used = self.used();
        let old = std::mem::take(&mut self.data);
        // new_pos[i] = new index of the first element at old index >= i.
        let mut new_pos = vec![0u32; old.len() + 1];
        let mut j = to_u32(old.iter().filter(|b| b.is_some()).count());
        new_pos[old.len()] = j;
        for i in (0..old.len()).rev() {
            if old[i].is_some() {
                j -= 1;
            }
            new_pos[i] = if old[i].is_some() { j } else { new_pos[i + 1] };
        }
        let first_hole = to_u32(old.iter().position(Option::is_none).unwrap_or(old.len()));
        let new_used = new_pos[old.len()];
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
        self.data = old.into_iter().flatten().map(Some).collect();
    }

    fn do_resize(&mut self) {
        let n = self.num_elements;
        if self.used() > n + (n >> 5) {
            self.rehash();
        } else if self.table_size < HT_MAX_SIZE {
            self.table_size += self.table_size;
            self.index = vec![INVALID; (self.table_size as usize) * 2];
            self.rehash();
        }
    }

    fn packed_to_hash(&mut self) {
        self.kind = Kind::Hash;
        self.index = vec![INVALID; (self.table_size as usize) * 2];
        self.rehash_without_compaction();
    }

    fn rehash_without_compaction(&mut self) {
        self.index.fill(INVALID);
        for i in 0..self.data.len() {
            let Some(b) = &self.data[i] else { continue };
            let s = self.slot(b.key.hash());
            let head = self.index[s];
            if let Some(b) = &mut self.data[i] {
                b.next = head;
            }
            self.index[s] = to_u32(i);
        }
    }

    fn find_int(&self, h: i64) -> Option<u32> {
        match self.kind {
            Kind::Uninit => None,
            Kind::Packed => {
                let i = usize::try_from(h).ok()?;
                match self.data.get(i) {
                    Some(Some(_)) => Some(to_u32(i)),
                    _ => None,
                }
            }
            Kind::Hash => {
                #[allow(clippy::cast_sign_loss)]
                let mut idx = self.index[self.slot(h as u64)];
                while idx != INVALID {
                    let b = self.data[idx as usize].as_ref()?;
                    if b.key == ArrayKey::Int(h) {
                        return Some(idx);
                    }
                    idx = b.next;
                }
                None
            }
        }
    }

    fn find_str(&self, s: &[u8], h: u64) -> Option<u32> {
        if self.kind != Kind::Hash {
            return None;
        }
        let mut idx = self.index[self.slot(h)];
        while idx != INVALID {
            let b = self.data[idx as usize].as_ref()?;
            if let ArrayKey::Str(k) = &b.key {
                if k.as_bytes() == s {
                    return Some(idx);
                }
            }
            idx = b.next;
        }
        None
    }

    fn find(&self, key: &ArrayKey) -> Option<u32> {
        match key {
            ArrayKey::Int(i) => self.find_int(*i),
            ArrayKey::Str(s) => self.find_str(s.as_bytes(), s.hash_value()),
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
        let idx = self.used();
        let s = self.slot(key.hash());
        let next = self.index[s];
        self.index[s] = idx;
        self.data.push(Some(Bucket { key, val, next }));
        self.num_elements += 1;
        idx
    }

    fn add_to_packed(&mut self, h: i64, val: Value) -> u32 {
        let hu = usize::try_from(h).unwrap_or(0);
        while self.data.len() < hu {
            self.data.push(None);
        }
        self.data.push(Some(Bucket {
            key: ArrayKey::Int(h),
            val,
            next: INVALID,
        }));
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
        match self.kind {
            Kind::Packed => {
                let used = self.data.len();
                if let Some(hu) = usize::try_from(h).ok().filter(|&x| x < used) {
                    if self.data[hu].is_some() {
                        return self.existing(to_u32(hu), val, mode);
                    }
                    // Keep the order: convert to a hash.
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
                if let Some(b) = &mut self.data[idx as usize] {
                    b.val = val;
                }
                Ok((idx, true))
            }
        }
    }

    /// Port of `_zend_hash_add_or_update_i` for string keys (no numeric
    /// normalization).
    fn str_insert(&mut self, key: ZStr, val: Value, mode: Mode) -> Result<(u32, bool), Value> {
        match self.kind {
            Kind::Uninit => self.real_init_mixed(),
            Kind::Packed => {
                self.packed_to_hash();
                if self.used() >= self.table_size {
                    self.do_resize();
                }
            }
            Kind::Hash => {
                if let Some(idx) = self.find_str(key.as_bytes(), key.hash_value()) {
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

    fn val_at_mut(&mut self, idx: u32) -> &mut Value {
        match &mut self.data[idx as usize] {
            Some(b) => &mut b.val,
            None => unreachable!("bucket index returned by insert is always occupied"),
        }
    }

    /// Deletes the bucket at `idx` (`_zend_hash_del_el`), returning its value.
    fn del_at(&mut self, idx: u32) -> Option<Value> {
        let i = idx as usize;
        let bucket = self.data.get(i)?.as_ref()?;
        if self.kind == Kind::Hash {
            let s = self.slot(bucket.key.hash());
            let next = bucket.next;
            if self.index[s] == idx {
                self.index[s] = next;
            } else {
                let mut p = self.index[s];
                while p != INVALID {
                    let pb = self.data[p as usize].as_mut()?;
                    if pb.next == idx {
                        pb.next = next;
                        break;
                    }
                    p = pb.next;
                }
            }
        }
        let b = self.data[i].take()?;
        self.num_elements -= 1;
        if i + 1 == self.data.len() {
            self.data.pop();
            while matches!(self.data.last(), Some(None)) {
                self.data.pop();
            }
            let used = self.used();
            self.internal_ptr = self.internal_ptr.min(used);
            for it in &mut self.iterators {
                it.1 = it.1.min(used);
            }
        }
        Some(b.val)
    }

    // ------------------------------------------------------------------
    // Lookup
    // ------------------------------------------------------------------

    /// Looks up a key.
    #[must_use]
    pub fn get(&self, key: &ArrayKey) -> Option<&Value> {
        let idx = self.find(key)?;
        self.data[idx as usize].as_ref().map(|b| &b.val)
    }

    /// Looks up an integer key.
    #[must_use]
    pub fn get_int(&self, i: i64) -> Option<&Value> {
        let idx = self.find_int(i)?;
        self.data[idx as usize].as_ref().map(|b| &b.val)
    }

    /// Looks up an exact string key (no numeric normalization;
    /// `zend_hash_str_find`).
    #[must_use]
    pub fn get_str(&self, s: &[u8]) -> Option<&Value> {
        let idx = self.find_str(s, crate::string::zend_hash_bytes(s))?;
        self.data[idx as usize].as_ref().map(|b| &b.val)
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
    pub fn get_mut(&mut self, key: &ArrayKey) -> Option<&mut Value> {
        let idx = self.find(key)?;
        self.data[idx as usize].as_mut().map(|b| &mut b.val)
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
        let key = key.into();
        if let Some(idx) = self.find(&key) {
            return Some(std::mem::replace(self.val_at_mut(idx), val));
        }
        let _ = self.key_insert(key, val, Mode::Update);
        None
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
        let h = self.next_free;
        let (idx, _) = self.index_insert(h, val, Mode::Next)?;
        match &self.data[idx as usize] {
            Some(Bucket {
                key: ArrayKey::Int(k),
                ..
            }) => Ok(*k),
            _ => Ok(0),
        }
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
        let key = key.into();
        let idx = match self.find(&key) {
            Some(idx) => return (self.val_at_mut(idx), false),
            None => match self.key_insert(key, Value::Null, Mode::Lookup) {
                Ok((idx, _)) => idx,
                Err(_) => unreachable!("lookup never fails"),
            },
        };
        (self.val_at_mut(idx), true)
    }

    /// Removes a key, returning its value (`zend_hash_del`).
    pub fn remove(&mut self, key: &ArrayKey) -> Option<Value> {
        let idx = self.find(key)?;
        self.del_at(idx)
    }

    /// Removes the element at a position, returning key and value.
    pub fn remove_at(&mut self, pos: u32) -> Option<(ArrayKey, Value)> {
        let key = self.data.get(pos as usize)?.as_ref()?.key.clone();
        self.del_at(pos).map(|v| (key, v))
    }

    /// Removes all elements (`zend_hash_clean`).
    pub fn clear(&mut self) {
        self.data.clear();
        if self.kind == Kind::Hash {
            self.index.fill(INVALID);
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
        Iter {
            inner: self.data.iter(),
            remaining: self.num_elements as usize,
        }
    }

    /// Iterates `(key, &mut value)` pairs in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ArrayKey, &mut Value)> {
        self.data
            .iter_mut()
            .filter_map(|b| b.as_mut().map(|b| (&b.key, &mut b.val)))
    }

    /// Iterates keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &ArrayKey> {
        self.iter().map(|(k, _)| k)
    }

    /// Iterates values in order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, v)| v)
    }

    /// First valid position at or after `pos` (`_zend_hash_get_valid_pos`);
    /// returns [`HashTable::used`] when there is none.
    #[must_use]
    pub fn valid_pos(&self, pos: u32) -> u32 {
        let mut p = pos as usize;
        while p < self.data.len() && self.data[p].is_none() {
            p += 1;
        }
        to_u32(p.max(pos as usize))
    }

    /// The entry at a position (holes and out-of-range give `None`).
    #[must_use]
    pub fn at(&self, pos: u32) -> Option<(&ArrayKey, &Value)> {
        self.data
            .get(pos as usize)?
            .as_ref()
            .map(|b| (&b.key, &b.val))
    }

    /// Mutable access to the value at a position.
    pub fn at_mut(&mut self, pos: u32) -> Option<&mut Value> {
        self.data
            .get_mut(pos as usize)?
            .as_mut()
            .map(|b| &mut b.val)
    }

    /// The first element.
    #[must_use]
    pub fn first(&self) -> Option<(&ArrayKey, &Value)> {
        self.iter().next()
    }

    /// The last element.
    #[must_use]
    pub fn last(&self) -> Option<(&ArrayKey, &Value)> {
        self.data
            .iter()
            .rev()
            .find_map(|b| b.as_ref().map(|b| (&b.key, &b.val)))
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
        self.at(self.current_pos()).map(|(_, v)| v)
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
        let mut idx = self.data.len();
        while idx > 0 {
            idx -= 1;
            if self.data[idx].is_some() {
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
        let mut idx = self.valid_pos(pos) as usize;
        if idx >= self.data.len() {
            return pos;
        }
        loop {
            idx += 1;
            if idx >= self.data.len() {
                return self.used();
            }
            if self.data[idx].is_some() {
                return to_u32(idx);
            }
        }
    }

    /// `zend_hash_move_backwards_ex` on an external position.
    #[must_use]
    pub fn pos_backward(&self, pos: u32) -> u32 {
        let mut idx = pos as usize;
        if idx >= self.data.len() {
            return pos;
        }
        while idx > 0 {
            idx -= 1;
            if self.data[idx].is_some() {
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
            &mut |a: &(Bucket, u32), b: &(Bucket, u32)| {
                let r = cmp(&a.0.key, &a.0.val, &b.0.key, &b.0.val);
                if r != 0 {
                    r
                } else {
                    i32::from(a.1 > b.1) - i32::from(a.1 < b.1)
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
            &mut |a: &(Bucket, u32), b: &(Bucket, u32)| cmp(&a.0.key, &a.0.val, &b.0.key, &b.0.val),
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
    /// tables that need no sorting, then the buckets without holes, each
    /// paired with its original position (`Z_EXTRA`, the stability
    /// tie-break). `None` when there is nothing to sort.
    fn sort_begin(&mut self, renumber: bool) -> Option<Vec<(Bucket, u32)>> {
        if self.num_elements <= 1 {
            if !renumber || self.num_elements == 0 {
                return None;
            }
            if self.kind == Kind::Packed && self.is_without_holes() {
                self.internal_ptr = 0;
                self.next_free = 1;
                return None;
            }
        }
        Some(
            std::mem::take(&mut self.data)
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, b)| (b, to_u32(i)))
                .collect(),
        )
    }

    /// The second half of `zend_hash_sort_internal`: stores the sorted
    /// buckets back, renumbering them or rebuilding the hash index.
    fn sort_finish(&mut self, items: Vec<(Bucket, u32)>, renumber: bool) {
        self.internal_ptr = 0;
        let n = items.len();
        if renumber {
            self.data = items
                .into_iter()
                .enumerate()
                .map(|(i, (mut b, _))| {
                    b.key = ArrayKey::Int(i64::try_from(i).unwrap_or(i64::MAX));
                    Some(b)
                })
                .collect();
            self.next_free = i64::try_from(n).unwrap_or(i64::MAX);
            self.kind = Kind::Packed;
            self.index = Vec::new();
        } else {
            self.data = items.into_iter().map(|(b, _)| Some(b)).collect();
            if self.kind != Kind::Hash {
                self.kind = Kind::Hash;
                self.index = vec![INVALID; (self.table_size as usize) * 2];
            }
            self.rehash_without_compaction();
        }
    }

    /// [`HashTable::sort_by`] for a table that the reference `bound` holds
    /// while it is sorted (`zend_array_sort` on a by-reference argument) and
    /// that the comparisons can reach through `bound`, e.g. because one of
    /// its elements is `bound` itself.
    ///
    /// php-src sorts `arData` in place after `HT_HASH_RESET`
    /// (`zend_hash_sort_internal`, which cites Zend/tests/bug63882_2.phpt),
    /// so whatever reaches the table mid-sort sees its elements in their
    /// current, half-sorted order while every key lookup in it misses. Before
    /// each comparison `bound` is therefore made to hold such a view of the
    /// table: the buckets in their current order (with the renumbering swap,
    /// `zend_hash_bucket_renum_swap`, keys stay in place and only values
    /// move) and an empty hash index. Once PHP code replaces the value of
    /// `bound` (any write separates the shared table), views stop being
    /// published and comparisons see whatever `bound` then holds.
    ///
    /// Returns whether `bound` still holds the table being sorted. When it
    /// does not, php-src drops the sorted table, so the caller should not
    /// store it.
    pub fn sort_by_in_ref<F>(&mut self, bound: &PhpRef, mut cmp: F, renumber: bool) -> bool
    where
        F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32,
    {
        let Some(items) = self.sort_begin(renumber) else {
            return true;
        };
        let n = items.len();
        let slot_keys = if renumber {
            items.iter().map(|(b, _)| b.key.clone()).collect()
        } else {
            Vec::new()
        };
        let mut live = LiveSort {
            items,
            slot_keys,
            table_size: self.table_size,
            next_free: self.next_free,
            bound,
            published: None,
            attached: true,
            cmp: &mut cmp,
        };
        zend_sort_by(&mut live, n);
        let attached = live.attached && live.published.as_ref().is_none_or(|p| holds(bound, p));
        self.sort_finish(live.items, renumber);
        attached
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
        let packed = self.kind != Kind::Hash;
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
            if let Some(b) = self.data[idx as usize].clone() {
                out_insert(&mut out, &b.key, b.val, !packed);
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
            if let Some(b) = self.data.get(idx as usize).cloned().flatten() {
                pos2 += 1;
                if let Some(r) = &mut removed {
                    out_insert(r, &b.key, b.val.clone(), !packed);
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
        let used = to_u32(self.data.len().max(idx as usize));
        while idx < used {
            if let Some(Some(b)) = self.data.get(idx as usize) {
                let b = b.clone();
                out_insert(&mut out, &b.key, b.val, !packed);
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
        if self.kind == Kind::Packed {
            let mut k: u32 = 0;
            let mut iter_pos = self.iterators_lower_pos(0);
            let used = self.used();
            for idx in 0..used {
                if self.data[idx as usize].is_none() {
                    continue;
                }
                if idx != k {
                    let mut b = self.data[idx as usize].take();
                    if let Some(b) = &mut b {
                        b.key = ArrayKey::Int(i64::from(k));
                    }
                    self.data[k as usize] = b;
                    if idx == iter_pos {
                        self.iterators_update(idx, k);
                        iter_pos = self.iterators_lower_pos(iter_pos + 1);
                    }
                }
                k += 1;
            }
            self.data.truncate(k as usize);
            self.next_free = i64::from(k);
        } else {
            let mut k: i64 = 0;
            let mut should_rehash = false;
            for b in self.data.iter_mut().flatten() {
                if let ArrayKey::Int(h) = &mut b.key {
                    if *h != k {
                        *h = k;
                        should_rehash = true;
                    }
                    k += 1;
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
        let key = self.data.get(idx as usize)?.as_ref()?.key.clone();
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
        for b in std::mem::take(&mut self.data).into_iter().flatten() {
            match b.key {
                ArrayKey::Str(s) => {
                    let _ = new.add(ArrayKey::Str(s), b.val);
                }
                ArrayKey::Int(_) => {
                    let _ = new.push(b.val);
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
        if let Some(idx) = self.find_str(key.as_bytes(), key.hash_value()) {
            return Some(std::mem::replace(self.val_at_mut(idx), val));
        }
        let _ = self.str_insert(key, val, Mode::Update);
        None
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

/// Comparator over `(bucket, original position)` pairs used by sorting.
type SortCmp<'a> = dyn FnMut(&(Bucket, u32), &(Bucket, u32)) -> i32 + 'a;

/// Whether `r` holds exactly the table `a`.
fn holds(r: &PhpRef, a: &ZArray) -> bool {
    r.try_borrow()
        .is_some_and(|v| matches!(&*v, Value::Array(b) if ZArray::ptr_eq(a, b)))
}

/// The table of [`HashTable::sort_by_in_ref`] being sorted in place: the
/// comparisons and swaps of `zend_sort` over its buckets, publishing the
/// live view into the bound reference before every comparison.
struct LiveSort<'a, F: ?Sized> {
    /// `(bucket, original position)` in their current order.
    items: Vec<(Bucket, u32)>,
    /// With renumbering, the keys by position (they do not move).
    slot_keys: Vec<ArrayKey>,
    table_size: u32,
    next_free: i64,
    bound: &'a PhpRef,
    /// The view `bound` was last given.
    published: Option<ZArray>,
    /// Whether `bound` still holds the table (no PHP code replaced it).
    attached: bool,
    cmp: &'a mut F,
}

impl<F> LiveSort<'_, F>
where
    F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32 + ?Sized,
{
    /// The table as php-src's `arData` stands mid-sort: buckets in their
    /// current order, no holes, and a reset hash index (every lookup
    /// misses).
    fn view(&self) -> HashTable {
        let data = self
            .items
            .iter()
            .enumerate()
            .map(|(i, (b, _))| {
                Some(Bucket {
                    key: self.slot_keys.get(i).unwrap_or(&b.key).clone(),
                    val: b.val.clone(),
                    next: INVALID,
                })
            })
            .collect();
        HashTable {
            data,
            index: vec![INVALID; (self.table_size as usize) * 2],
            num_elements: to_u32(self.items.len()),
            table_size: self.table_size,
            next_free: self.next_free,
            internal_ptr: 0,
            kind: Kind::Hash,
            iterators: Vec::new(),
        }
    }

    /// Makes `bound` hold the current view, unless PHP code replaced its
    /// value since the last one.
    fn publish(&mut self) {
        if !self.attached {
            return;
        }
        if let Some(prev) = &self.published {
            if !holds(self.bound, prev) {
                self.attached = false;
                self.published = None;
                return;
            }
        }
        let view = ZArray::from_table(self.view());
        let old = self.bound.set(Value::Array(view.clone()));
        self.published = Some(view);
        drop(old);
    }
}

impl<F> SortAccess for LiveSort<'_, F>
where
    F: FnMut(&ArrayKey, &Value, &ArrayKey, &Value) -> i32 + ?Sized,
{
    fn compare(&mut self, a: usize, b: usize) -> i32 {
        self.publish();
        let Self { items, cmp, .. } = self;
        let (x, y) = (&items[a], &items[b]);
        let r = cmp(&x.0.key, &x.0.val, &y.0.key, &y.0.val);
        if r != 0 {
            r
        } else {
            i32::from(x.1 > y.1) - i32::from(x.1 < y.1)
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.items.swap(a, b);
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
            data: Vec::with_capacity(self.data.len()),
            index: Vec::new(),
            num_elements: self.num_elements,
            table_size: self.table_size,
            next_free: self.next_free,
            internal_ptr: if self.internal_ptr < self.used() {
                self.internal_ptr
            } else {
                0
            },
            kind: self.kind,
            iterators: self.iterators.clone(),
        };
        if self.num_elements == 0 {
            out.kind = Kind::Uninit;
            out.table_size = HT_MIN_SIZE;
            out.internal_ptr = 0;
            return out;
        }
        out.data = self
            .data
            .iter()
            .map(|b| {
                b.as_ref().map(|b| Bucket {
                    key: b.key.clone(),
                    val: dup_val(&b.val),
                    next: b.next,
                })
            })
            .collect();
        if out.kind == Kind::Hash {
            out.index = vec![INVALID; (out.table_size as usize) * 2];
            out.rehash();
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
#[derive(Debug)]
pub struct Iter<'a> {
    inner: std::slice::Iter<'a, Option<Bucket>>,
    remaining: usize,
}

impl fmt::Debug for Bucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} => {:?}", self.key, self.val)
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a ArrayKey, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        let b = self.inner.by_ref().flatten().next()?;
        self.remaining -= 1;
        Some((&b.key, &b.val))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(b) = self.inner.next_back() {
            if let Some(b) = b {
                self.remaining -= 1;
                return Some((&b.key, &b.val));
            }
        }
        None
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

    /// Wraps a table.
    #[must_use]
    pub fn from_table(t: HashTable) -> Self {
        ZArray(Rc::new(t))
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

    /// Runs `sort_by_in_ref` on `xyz()` bound to a reference, logging what
    /// the reference holds at every comparison and whether a key lookup in
    /// it succeeds.
    fn live_sort_log(renumber: bool) -> (HashTable, bool, Vec<String>) {
        let mut t = xyz();
        let r = PhpRef::new(Value::Array(ZArray::from_table(t.clone())));
        let mut log = Vec::new();
        let attached = t.sort_by_in_ref(
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
        (t, attached, log)
    }

    #[test]
    fn sort_in_ref_shows_the_live_table_without_lookups() {
        // sort3: compares (3,1), (2,1), swaps x/y, compares (3,2), swaps.
        let (t, attached, log) = live_sort_log(false);
        assert!(attached);
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
        let (t, attached, log) = live_sort_log(true);
        assert!(attached);
        assert_eq!(log[2], "x=1,y=3,z=2 found=false");
        assert_eq!(render(&t), "0=1,1=2,2=3");
    }

    #[test]
    fn sort_in_ref_detaches_when_the_reference_is_replaced() {
        let mut t = xyz();
        let r = PhpRef::new(Value::Array(ZArray::from_table(t.clone())));
        let mut calls = 0;
        let attached = t.sort_by_in_ref(
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
        assert!(!attached);
        assert_eq!(calls, 3);
        // No further views were published over the PHP code's write.
        assert!(matches!(r.get(), Value::Int(7)));
        assert_eq!(render(&t), "y=1,z=2,x=3");
    }
}
