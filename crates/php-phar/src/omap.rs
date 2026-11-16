// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! An insertion-ordered byte-string map with `zend_hash` semantics: new keys
//! append, updates keep their position, deletions leave the order of the
//! remaining keys intact and keys can be renamed in place.

use std::collections::HashMap;

/// Insertion-ordered map keyed by byte strings.
#[derive(Debug, Clone)]
pub struct OrderedMap<V> {
    slots: Vec<Option<(Vec<u8>, V)>>,
    index: HashMap<Vec<u8>, usize>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            slots: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live elements.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Whether `key` exists.
    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.index.contains_key(key)
    }

    /// Looks a key up.
    pub fn get(&self, key: &[u8]) -> Option<&V> {
        let i = *self.index.get(key)?;
        self.slots[i].as_ref().map(|(_, v)| v)
    }

    /// Looks a key up mutably.
    pub fn get_mut(&mut self, key: &[u8]) -> Option<&mut V> {
        let i = *self.index.get(key)?;
        self.slots[i].as_mut().map(|(_, v)| v)
    }

    /// `zend_hash_add`: inserts only when absent; returns false otherwise.
    pub fn add(&mut self, key: Vec<u8>, value: V) -> bool {
        if self.index.contains_key(&key) {
            return false;
        }
        self.index.insert(key.clone(), self.slots.len());
        self.slots.push(Some((key, value)));
        true
    }

    /// `zend_hash_update`: replaces in place or appends.
    pub fn update(&mut self, key: Vec<u8>, value: V) {
        if let Some(&i) = self.index.get(&key) {
            self.slots[i] = Some((key, value));
        } else {
            self.add(key, value);
        }
    }

    /// Removes a key, returning its value.
    pub fn remove(&mut self, key: &[u8]) -> Option<V> {
        let i = self.index.remove(key)?;
        let v = self.slots[i].take().map(|(_, v)| v);
        if self.slots.len() > 32 && self.index.len() * 2 < self.slots.len() {
            self.compact();
        }
        v
    }

    fn compact(&mut self) {
        self.slots.retain(Option::is_some);
        self.index.clear();
        for (i, (k, _)) in self.slots.iter().flatten().enumerate() {
            self.index.insert(k.clone(), i);
        }
    }

    /// Renames a key in place (keeps its position). Fails when `to` exists
    /// or `from` does not.
    pub fn rename(&mut self, from: &[u8], to: Vec<u8>) -> bool {
        if self.index.contains_key(&to) {
            return false;
        }
        let Some(i) = self.index.remove(from) else {
            return false;
        };
        if let Some((k, _)) = self.slots[i].as_mut() {
            k.clone_from(&to);
        }
        self.index.insert(to, i);
        true
    }

    /// Iterates in order.
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &V)> {
        self.slots.iter().flatten().map(|(k, v)| (k, v))
    }

    /// Iterates mutably in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Vec<u8>, &mut V)> {
        self.slots.iter_mut().flatten().map(|(k, v)| (&*k, v))
    }

    /// The keys in order.
    pub fn keys(&self) -> Vec<Vec<u8>> {
        self.iter().map(|(k, _)| k.clone()).collect()
    }

    /// Removes every element for which `keep` returns false.
    pub fn retain(&mut self, mut keep: impl FnMut(&[u8], &mut V) -> bool) {
        for slot in &mut self.slots {
            if let Some((k, v)) = slot {
                if !keep(k, v) {
                    self.index.remove(k.as_slice());
                    *slot = None;
                }
            }
        }
        self.compact();
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.index.clear();
    }

    /// Sorts by key (`zend_hash_sort` with a binary comparison).
    pub fn sort_keys(&mut self) {
        self.slots.retain(Option::is_some);
        self.slots.sort_by(|a, b| match (a, b) {
            (Some((ka, _)), Some((kb, _))) => ka.cmp(kb),
            _ => std::cmp::Ordering::Equal,
        });
        self.compact();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_semantics() {
        let mut m = OrderedMap::new();
        assert!(m.add(b"b".to_vec(), 1));
        assert!(m.add(b"a".to_vec(), 2));
        assert!(!m.add(b"a".to_vec(), 3));
        m.update(b"b".to_vec(), 4);
        m.update(b"c".to_vec(), 5);
        assert_eq!(m.keys(), vec![b"b".to_vec(), b"a".to_vec(), b"c".to_vec()]);
        assert_eq!(m.get(b"b"), Some(&4));
        assert!(m.rename(b"a", b"z".to_vec()));
        assert_eq!(m.keys(), vec![b"b".to_vec(), b"z".to_vec(), b"c".to_vec()]);
        assert_eq!(m.remove(b"z"), Some(2));
        assert_eq!(m.len(), 2);
        m.sort_keys();
        assert_eq!(m.keys(), vec![b"b".to_vec(), b"c".to_vec()]);
        m.retain(|k, _| k == b"c");
        assert_eq!(m.keys(), vec![b"c".to_vec()]);
    }
}
