// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A small insertion-ordered map with integer or string keys, mirroring the
//! parts of Zend's `HashTable` semantics the SDL model relies on
//! (`zend_hash_add` fails on duplicates, `zend_hash_next_index_insert`
//! appends with the next free integer key, deletion keeps the order of the
//! remaining entries).

use std::collections::HashMap;

/// A key of a [`ZHash`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ZKey {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(String),
}

impl ZKey {
    /// The string key, if any.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ZKey::Str(s) => Some(s),
            ZKey::Int(_) => None,
        }
    }
}

/// Insertion-ordered map with Zend-like semantics.
#[derive(Debug, Clone)]
pub struct ZHash<V> {
    entries: Vec<Option<(ZKey, V)>>,
    index: HashMap<ZKey, usize>,
    next_free: i64,
    len: usize,
}

impl<V> Default for ZHash<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            next_free: 0,
            len: 0,
        }
    }
}

impl<V> ZHash<V> {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `zend_hash_str_add`: inserts unless the key exists; returns whether
    /// the value was inserted.
    pub fn add(&mut self, key: &str, value: V) -> bool {
        self.add_key(ZKey::Str(key.to_owned()), value)
    }

    /// `zend_hash_add` with any key.
    pub fn add_key(&mut self, key: ZKey, value: V) -> bool {
        if self.index.contains_key(&key) {
            return false;
        }
        self.push_entry(key, value);
        true
    }

    /// `zend_hash_update`: inserts or replaces (keeping the position).
    pub fn update(&mut self, key: &str, value: V) {
        let key = ZKey::Str(key.to_owned());
        if let Some(&pos) = self.index.get(&key) {
            if let Some(e) = self.entries[pos].as_mut() {
                e.1 = value;
            }
        } else {
            self.push_entry(key, value);
        }
    }

    /// `zend_hash_next_index_insert`.
    pub fn push(&mut self, value: V) {
        let key = ZKey::Int(self.next_free);
        self.push_entry(key, value);
    }

    fn push_entry(&mut self, key: ZKey, value: V) {
        if let ZKey::Int(i) = key {
            if i >= self.next_free {
                self.next_free = i.saturating_add(1);
            }
        }
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push(Some((key, value)));
        self.len += 1;
    }

    /// Looks a string key up.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        // Avoid allocating for the common case by scanning the index with a
        // borrowed key.
        self.index
            .get(&ZKey::Str(key.to_owned()))
            .and_then(|&p| self.entries[p].as_ref().map(|e| &e.1))
    }

    /// Mutable lookup of a string key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let pos = *self.index.get(&ZKey::Str(key.to_owned()))?;
        self.entries[pos].as_mut().map(|e| &mut e.1)
    }

    /// Looks an integer key up.
    #[must_use]
    pub fn get_index(&self, key: i64) -> Option<&V> {
        self.index
            .get(&ZKey::Int(key))
            .and_then(|&p| self.entries[p].as_ref().map(|e| &e.1))
    }

    /// Whether a string key exists.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.index.contains_key(&ZKey::Str(key.to_owned()))
    }

    /// Removes a key.
    pub fn remove(&mut self, key: &ZKey) -> Option<V> {
        let pos = self.index.remove(key)?;
        let (_, v) = self.entries[pos].take()?;
        self.len -= 1;
        Some(v)
    }

    /// Iterates `(key, value)` in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&ZKey, &V)> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().map(|(k, v)| (k, v)))
    }

    /// Iterates values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }

    /// Iterates values mutably in insertion order.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries
            .iter_mut()
            .filter_map(|e| e.as_mut().map(|(_, v)| v))
    }

    /// The first value (`zend_hash_internal_pointer_reset` + current).
    #[must_use]
    pub fn first(&self) -> Option<&V> {
        self.values().next()
    }

    /// Raw slot count (positions are stable while iterating by index).
    #[must_use]
    pub fn slots(&self) -> usize {
        self.entries.len()
    }

    /// The entry at a raw slot.
    #[must_use]
    pub fn slot(&self, pos: usize) -> Option<(&ZKey, &V)> {
        self.entries
            .get(pos)
            .and_then(|e| e.as_ref().map(|(k, v)| (k, v)))
    }

    /// Mutable entry at a raw slot.
    pub fn slot_mut(&mut self, pos: usize) -> Option<(&ZKey, &mut V)> {
        self.entries
            .get_mut(pos)
            .and_then(|e| e.as_mut().map(|(k, v)| (&*k, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_update_push_remove() {
        let mut h = ZHash::new();
        assert!(h.add("a", 1));
        assert!(!h.add("a", 2));
        h.push(3);
        h.update("a", 4);
        h.push(5);
        let v: Vec<_> = h.values().copied().collect();
        assert_eq!(v, [4, 3, 5]);
        assert_eq!(h.get_index(1), Some(&5));
        assert_eq!(h.remove(&ZKey::Int(0)), Some(3));
        assert_eq!(h.len(), 2);
        h.push(6);
        assert_eq!(h.get_index(2), Some(&6));
        let keys: Vec<_> = h.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, [ZKey::Str("a".into()), ZKey::Int(1), ZKey::Int(2)]);
    }
}
