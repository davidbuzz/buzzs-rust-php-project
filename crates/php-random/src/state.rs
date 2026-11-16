// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Engine state (de)serialization helpers: the values found in the state
//! array of `__serialize()` / `__unserialize()` and PHP's little-endian hex
//! encoding (`php_random_bin2hex_le` / `php_random_hex2bin_le`).

use std::collections::HashMap;

/// A value of an engine state array, as far as the engines care about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateValue {
    /// `IS_LONG`.
    Int(i64),
    /// `IS_STRING` (the raw bytes).
    Str(Vec<u8>),
    /// Any other type (always rejected).
    Other,
}

impl StateValue {
    /// A string value.
    pub fn str(s: impl Into<Vec<u8>>) -> Self {
        Self::Str(s.into())
    }
}

/// The state array handed to `__unserialize()`: the total element count of
/// the PHP array plus its integer-keyed entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateArray {
    len: usize,
    by_index: HashMap<i64, StateValue>,
}

impl StateArray {
    /// An empty array.
    pub fn new() -> Self {
        Self::default()
    }

    /// A packed list `[0 => v0, 1 => v1, ...]`.
    pub fn from_list(values: impl IntoIterator<Item = StateValue>) -> Self {
        let mut a = Self::new();
        for (i, v) in values.into_iter().enumerate() {
            a.insert_index(i64::try_from(i).unwrap_or(i64::MAX), v);
        }
        a
    }

    /// Adds (or replaces) an integer-keyed element.
    pub fn insert_index(&mut self, key: i64, value: StateValue) {
        if self.by_index.insert(key, value).is_none() {
            self.len += 1;
        }
    }

    /// Accounts for a string-keyed element (never looked up by the engines,
    /// but it counts towards `zend_hash_num_elements`).
    pub fn insert_string_key(&mut self) {
        self.len += 1;
    }

    /// `zend_hash_num_elements`.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the array has no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `zend_hash_index_find`.
    pub fn get(&self, index: i64) -> Option<&StateValue> {
        self.by_index.get(&index)
    }

    /// The string at `index` if it exists and has exactly `len` bytes.
    pub(crate) fn str_of_len(&self, index: i64, len: usize) -> Option<&[u8]> {
        match self.get(index) {
            Some(StateValue::Str(s)) if s.len() == len => Some(s),
            _ => None,
        }
    }

    /// The integer at `index`.
    pub(crate) fn int(&self, index: i64) -> Option<i64> {
        match self.get(index) {
            Some(StateValue::Int(i)) => Some(*i),
            _ => None,
        }
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `php_random_bin2hex_le`: lowercase hex of the little-endian bytes.
pub fn bin2hex_le(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 15)]));
    }
    s
}

/// Hex of a `u32` as stored in memory on a little-endian machine.
pub fn u32_to_hex_le(v: u32) -> String {
    bin2hex_le(&v.to_le_bytes())
}

/// Hex of a `u64` as stored in memory on a little-endian machine.
pub fn u64_to_hex_le(v: u64) -> String {
    bin2hex_le(&v.to_le_bytes())
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `php_random_hex2bin_le`: decodes `hex.len() / 2` bytes (case-insensitive);
/// `None` on any non-hex digit.
pub fn hex2bin_le(hex: &[u8]) -> Option<Vec<u8>> {
    hex.chunks_exact(2)
        .map(|p| Some((hex_digit(p[0])? << 4) | hex_digit(p[1])?))
        .collect()
}

/// Decodes an 8-character hex string into a `u32`.
pub fn hex_le_to_u32(hex: &[u8]) -> Option<u32> {
    let b: [u8; 4] = hex2bin_le(hex)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// Decodes a 16-character hex string into a `u64`.
pub fn hex_le_to_u64(hex: &[u8]) -> Option<u64> {
    let b: [u8; 8] = hex2bin_le(hex)?.try_into().ok()?;
    Some(u64::from_le_bytes(b))
}

/// Little-endian `u64` from 8 bytes (the engines' "endianness safe copy").
pub(crate) fn le_u64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .enumerate()
        .fold(0, |acc, (i, &b)| acc | (u64::from(b) << (8 * i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        assert_eq!(u32_to_hex_le(0x1234_5678), "78563412");
        assert_eq!(u64_to_hex_le(1), "0100000000000000");
        assert_eq!(hex_le_to_u32(b"78563412"), Some(0x1234_5678));
        assert_eq!(hex_le_to_u32(b"7856341G"), None);
        assert_eq!(hex_le_to_u32(b"ABCDEFab"), Some(0xabef_cdab));
        assert_eq!(hex_le_to_u64(b"0100000000000000"), Some(1));
        assert_eq!(hex_le_to_u64(b"01000000"), None);
        assert_eq!(hex2bin_le(b"zz"), None);
        assert_eq!(hex2bin_le(b"0a0"), Some(vec![10]));
    }

    #[test]
    fn state_array() {
        let mut a = StateArray::from_list([StateValue::Int(1), StateValue::str("x")]);
        assert_eq!(a.len(), 2);
        a.insert_string_key();
        assert_eq!(a.len(), 3);
        assert_eq!(a.int(0), Some(1));
        assert_eq!(a.int(1), None);
        assert_eq!(a.str_of_len(1, 1), Some(&b"x"[..]));
        assert_eq!(a.str_of_len(1, 2), None);
        a.insert_index(0, StateValue::Other);
        assert_eq!(a.len(), 3);
        assert!(!a.is_empty());
        assert_eq!(le_u64(&[1, 2]), 0x0201);
    }
}
