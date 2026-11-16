// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A tiny model of the PHP arrays the `preg_*` functions build.
//!
//! The crate does not depend on the VM's value types, so results are handed
//! back as [`MatchArray`]s: insertion-ordered maps from [`Key`] to [`Value`]
//! that reproduce exactly the key order and overwrite behaviour PHP's
//! `HashTable` shows for these functions (named groups before their index,
//! `zend_hash_update` keeping the original position, `MARK` entries, …).

use std::sync::Arc;

/// An array key: an integer index or a (group / `MARK`) name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Integer key.
    Index(i64),
    /// String key (group names are never numeric strings).
    Name(Arc<[u8]>),
}

impl Key {
    /// Convenience constructor for a string key.
    pub fn name(name: &[u8]) -> Self {
        Self::Name(Arc::from(name))
    }
}

/// A value stored in a [`MatchArray`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A PHP string.
    Str(Vec<u8>),
    /// `null` (unmatched group under `PREG_UNMATCHED_AS_NULL`).
    Null,
    /// A PHP int (offsets in `[match, offset]` pairs).
    Int(i64),
    /// A nested array.
    Array(MatchArray),
}

impl Value {
    /// The bytes of a [`Value::Str`].
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The nested array of a [`Value::Array`].
    pub fn as_array(&self) -> Option<&MatchArray> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The integer of a [`Value::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Builds the `[string|null, offset]` pair used by the `*_OFFSET_CAPTURE` flags.
    pub(crate) fn pair(first: Value, offset: i64) -> Self {
        let mut a = MatchArray::with_capacity(2);
        a.push(first);
        a.push(Value::Int(offset));
        Self::Array(a)
    }
}

/// An insertion-ordered PHP-like array.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchArray {
    entries: Vec<(Key, Value)>,
    next_index: i64,
}

impl MatchArray {
    /// An empty array.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty array with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            entries: Vec::with_capacity(n),
            next_index: 0,
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the array has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends with the next free integer key (`zend_hash_next_index_insert`).
    pub fn push(&mut self, value: Value) {
        let k = self.next_index;
        self.entries.push((Key::Index(k), value));
        self.next_index = k + 1;
    }

    /// Inserts or overwrites in place (`zend_hash_update` / `zend_hash_index_update`).
    pub fn set(&mut self, key: Key, value: Value) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
            return;
        }
        if let Key::Index(i) = key {
            if i >= self.next_index {
                self.next_index = i.saturating_add(1);
            }
        }
        self.entries.push((key, value));
    }

    /// Inserts only when the key is absent (`zend_hash_add`); returns whether it was added.
    pub fn add(&mut self, key: Key, value: Value) -> bool {
        if self.contains_key(&key) {
            return false;
        }
        self.set(key, value);
        true
    }

    /// Whether `key` is present.
    pub fn contains_key(&self, key: &Key) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// Looks a key up.
    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Looks an integer key up.
    pub fn get_index(&self, index: i64) -> Option<&Value> {
        self.get(&Key::Index(index))
    }

    /// Looks a string key up.
    pub fn get_name(&self, name: &[u8]) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| matches!(k, Key::Name(n) if &**n == name))
            .map(|(_, v)| v)
    }

    /// Iterates entries in order.
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// The keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.entries.iter().map(|(k, _)| k)
    }

    /// The values in order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|(_, v)| v)
    }

    /// Removes all entries (and resets the next index).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_index = 0;
    }
}

impl IntoIterator for MatchArray {
    type Item = (Key, Value);
    type IntoIter = std::vec::IntoIter<(Key, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a> IntoIterator for &'a MatchArray {
    type Item = &'a (Key, Value);
    type IntoIter = std::slice::Iter<'a, (Key, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_named_order() {
        let mut a = MatchArray::new();
        a.push(Value::Str(b"x".to_vec()));
        a.set(Key::name(b"n"), Value::Str(b"y".to_vec()));
        a.push(Value::Str(b"y".to_vec()));
        let keys: Vec<_> = a.keys().cloned().collect();
        assert_eq!(keys, vec![Key::Index(0), Key::name(b"n"), Key::Index(1)]);
    }

    #[test]
    fn update_keeps_position_add_does_not_overwrite() {
        let mut a = MatchArray::new();
        a.set(Key::name(b"MARK"), Value::Str(b"a".to_vec()));
        a.push(Value::Null);
        a.set(Key::name(b"MARK"), Value::Str(b"b".to_vec()));
        assert_eq!(a.get_name(b"MARK"), Some(&Value::Str(b"b".to_vec())));
        assert_eq!(a.keys().next(), Some(&Key::name(b"MARK")));
        assert!(!a.add(Key::name(b"MARK"), Value::Null));
        assert_eq!(a.get_name(b"MARK"), Some(&Value::Str(b"b".to_vec())));
    }

    #[test]
    fn sparse_index_then_push() {
        let mut a = MatchArray::new();
        a.set(Key::Index(3), Value::Int(1));
        a.push(Value::Int(2));
        assert_eq!(a.get_index(4), Some(&Value::Int(2)));
        assert_eq!(a.len(), 2);
        a.clear();
        assert!(a.is_empty());
    }
}
