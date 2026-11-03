//! `ZStr`: PHP's refcounted, immutable, binary-safe byte string (`zend_string`).
//!
//! A [`ZStr`] is a cheap-to-clone handle (a single `Rc` pointer) to an immutable
//! byte buffer together with a lazily computed, cached hash (PHP's DJB "times 33"
//! hash, `zend_inline_hash_func`). Strings can be *interned*: interning returns the
//! canonical instance for a given byte sequence so that equality checks between
//! interned strings reduce to a pointer comparison.
//!
//! Strings are immutable from the outside. The only mutation offered is
//! [`ZStr::append`], which extends the buffer in place when the handle is the
//! sole owner (mirroring `zend_string_extend` on a refcount-1 string) and
//! otherwise allocates a new string.

use std::borrow::Borrow;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// The inner, shared part of a [`ZStr`].
struct StrInner {
    /// Cached `zend_inline_hash_func` value; `0` means "not computed yet"
    /// (a real hash always has its top bit set, so it is never `0`).
    hash: Cell<u64>,
    /// Whether this instance lives in the intern table.
    interned: bool,
    /// The bytes. A `Vec` (rather than a boxed slice) so that unique owners can
    /// append in place.
    bytes: Vec<u8>,
}

/// A refcounted, immutable, binary-safe PHP string.
///
/// Cloning is O(1). Equality, ordering and hashing are by byte content
/// (`memcmp` semantics), never by identity.
#[derive(Clone)]
pub struct ZStr(Rc<StrInner>);

thread_local! {
    static INTERNED: RefCell<HashSet<ZStr>> = RefCell::new(HashSet::new());
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

impl ZStr {
    /// Creates a new (non-interned) string from the given bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        ZStr(Rc::new(StrInner {
            hash: Cell::new(0),
            interned: false,
            bytes: bytes.into(),
        }))
    }

    /// Returns the canonical interned instance for `bytes`, creating it if needed.
    ///
    /// Interned strings live for the remainder of the thread.
    #[must_use]
    pub fn intern(bytes: &[u8]) -> Self {
        INTERNED.with(|set| {
            if let Some(existing) = set.borrow().get(bytes) {
                return existing.clone();
            }
            let s = ZStr(Rc::new(StrInner {
                hash: Cell::new(0),
                interned: true,
                bytes: bytes.to_vec(),
            }));
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
        &self.0.bytes
    }

    /// Length in bytes.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }

    /// Whether the string is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }

    /// Whether this instance is interned.
    #[must_use]
    pub fn is_interned(&self) -> bool {
        self.0.interned
    }

    /// Returns the (cached) PHP hash of the string.
    #[inline]
    #[must_use]
    pub fn hash_value(&self) -> u64 {
        let h = self.0.hash.get();
        if h != 0 {
            return h;
        }
        let h = zend_hash_bytes(&self.0.bytes);
        self.0.hash.set(h);
        h
    }

    /// Whether two handles point at the very same string instance.
    #[inline]
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// Number of live handles to this string instance (the "refcount").
    #[must_use]
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    /// Returns the string as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.0.bytes).into_owned()
    }

    /// Returns the string as `&str` if it is valid UTF-8.
    #[must_use]
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0.bytes).ok()
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
        if !self.0.interned {
            if let Some(inner) = Rc::get_mut(&mut self.0) {
                inner.bytes.extend_from_slice(other);
                inner.hash.set(0);
                return;
            }
        }
        let mut v = Vec::with_capacity(self.len() + other.len());
        v.extend_from_slice(self.as_bytes());
        v.extend_from_slice(other);
        *self = ZStr::new(v);
    }

    /// Concatenates two byte slices into a new string.
    #[must_use]
    pub fn concat(a: &[u8], b: &[u8]) -> Self {
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        ZStr::new(v)
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

    /// Returns the bytes as an owned vector.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match Rc::try_unwrap(self.0) {
            Ok(inner) => inner.bytes,
            Err(rc) => rc.bytes.clone(),
        }
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
        if Rc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        if self.0.interned && other.0.interned {
            return false;
        }
        let (h1, h2) = (self.0.hash.get(), other.0.hash.get());
        if h1 != 0 && h2 != 0 && h1 != h2 {
            return false;
        }
        self.0.bytes == other.0.bytes
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
        ZStr::new(s.as_bytes())
    }
}

impl From<String> for ZStr {
    fn from(s: String) -> Self {
        ZStr::new(s.into_bytes())
    }
}

impl From<&[u8]> for ZStr {
    fn from(s: &[u8]) -> Self {
        ZStr::new(s)
    }
}

impl From<Vec<u8>> for ZStr {
    fn from(s: Vec<u8>) -> Self {
        ZStr::new(s)
    }
}

impl<const N: usize> From<&[u8; N]> for ZStr {
    fn from(s: &[u8; N]) -> Self {
        ZStr::new(&s[..])
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
}
