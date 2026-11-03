//! Values produced by the INI parser and the ordered map that
//! `parse_ini_string()` returns.

use std::collections::HashMap;

/// A scalar value as the INI parser hands it to its callback.
#[derive(Debug, Clone, PartialEq)]
pub enum IniScalar {
    /// A string (the only kind produced outside `INI_SCANNER_TYPED`).
    Str(Vec<u8>),
    /// An integer (typed mode numbers and `|`/`&`/`^`/`~`/`!` results).
    Long(i64),
    /// A float (typed mode).
    Double(f64),
    /// `true`/`on`/`yes` and `false`/`off`/`no`/`none` in typed mode.
    Bool(bool),
    /// `null` in typed mode.
    Null,
}

impl IniScalar {
    /// String form as PHP's `zval_get_string()` would produce it (floats
    /// use precision 14, matching the default `precision` INI setting).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Str(s) => s.clone(),
            Self::Long(l) => l.to_string().into_bytes(),
            Self::Double(d) => {
                crate::fmt::format_double(*d, 14, crate::fmt::DoubleMode::Plain).into_bytes()
            }
            Self::Bool(true) => b"1".to_vec(),
            Self::Bool(false) | Self::Null => Vec::new(),
        }
    }
}

/// An array key (`zend_ulong` index or string).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IniKey {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
}

impl IniKey {
    /// Converts a string key the way `zend_symtable_update()` does:
    /// canonical decimal integers become integer keys.
    pub fn symtable(key: &[u8]) -> Self {
        match canonical_int(key) {
            Some(i) => Self::Int(i),
            None => Self::Str(key.to_vec()),
        }
    }
}

/// `ZEND_HANDLE_NUMERIC_STR`: `"123"`, `"-5"` and `"0"` are integers,
/// `"0123"`, `"-0"`, `" 1"`, `"1 "`, `"+1"` and out-of-range values are not.
fn canonical_int(s: &[u8]) -> Option<i64> {
    let digits = s.strip_prefix(b"-").unwrap_or(s);
    if digits.is_empty() || digits.len() > 20 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if digits[0] == b'0' && (digits.len() > 1 || s.len() != digits.len()) {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse::<i64>().ok()
}

/// A value stored in an [`IniMap`].
#[derive(Debug, Clone, PartialEq)]
pub enum IniValue {
    /// A scalar.
    Scalar(IniScalar),
    /// A nested array (sections, `a[] = …` entries).
    Array(IniMap),
}

impl From<IniScalar> for IniValue {
    fn from(s: IniScalar) -> Self {
        Self::Scalar(s)
    }
}

/// An insertion-ordered map with PHP array semantics (enough of them for
/// `parse_ini_string()` results).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IniMap {
    entries: Vec<(IniKey, IniValue)>,
    index: HashMap<IniKey, usize>,
    next_free: Option<i64>,
}

impl IniMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&IniKey, &IniValue)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Looks up a key.
    pub fn get(&self, key: &IniKey) -> Option<&IniValue> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    /// Looks up a string key with symtable semantics.
    pub fn get_str(&self, key: &str) -> Option<&IniValue> {
        self.get(&IniKey::symtable(key.as_bytes()))
    }

    /// Mutable lookup.
    pub fn get_mut(&mut self, key: &IniKey) -> Option<&mut IniValue> {
        self.index.get(key).map(|&i| &mut self.entries[i].1)
    }

    fn bump_next_free(&mut self, key: &IniKey) {
        if let IniKey::Int(h) = *key {
            if self.next_free.is_none_or(|n| h >= n) {
                self.next_free = Some(h.saturating_add(1));
            }
        }
    }

    /// Inserts or updates (`zend_hash_update`); the position of an existing
    /// key is kept.
    pub fn insert(&mut self, key: IniKey, value: IniValue) {
        if let Some(&i) = self.index.get(&key) {
            self.entries[i].1 = value;
            return;
        }
        self.bump_next_free(&key);
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push((key, value));
    }

    /// Appends with the next free integer key (`add_next_index_zval`).
    /// Returns `false` when no key is available.
    pub fn push(&mut self, value: IniValue) -> bool {
        let key = IniKey::Int(self.next_free.unwrap_or(0));
        if self.index.contains_key(&key) {
            return false;
        }
        self.insert(key, value);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symtable_keys() {
        assert_eq!(IniKey::symtable(b"10"), IniKey::Int(10));
        assert_eq!(IniKey::symtable(b"-3"), IniKey::Int(-3));
        assert_eq!(IniKey::symtable(b"0"), IniKey::Int(0));
        assert_eq!(IniKey::symtable(b"-0"), IniKey::Str(b"-0".to_vec()));
        assert_eq!(IniKey::symtable(b"010"), IniKey::Str(b"010".to_vec()));
        assert_eq!(IniKey::symtable(b"1 "), IniKey::Str(b"1 ".to_vec()));
        assert_eq!(IniKey::symtable(b"+1"), IniKey::Str(b"+1".to_vec()));
        assert_eq!(
            IniKey::symtable(b"9223372036854775808"),
            IniKey::Str(b"9223372036854775808".to_vec())
        );
        assert_eq!(
            IniKey::symtable(b"-9223372036854775808"),
            IniKey::Int(i64::MIN)
        );
    }

    #[test]
    fn map_semantics() {
        let mut m = IniMap::new();
        assert!(m.push(IniScalar::Long(1).into()));
        m.insert(IniKey::Int(5), IniScalar::Long(2).into());
        assert!(m.push(IniScalar::Long(3).into()));
        assert_eq!(
            m.get(&IniKey::Int(6)),
            Some(&IniValue::Scalar(IniScalar::Long(3)))
        );
        m.insert(IniKey::Int(0), IniScalar::Null.into());
        assert_eq!(
            m.iter().next().map(|(k, _)| k.clone()),
            Some(IniKey::Int(0))
        );
        assert_eq!(m.len(), 3);
        let mut n = IniMap::new();
        n.insert(IniKey::Int(-5), IniScalar::Null.into());
        n.push(IniScalar::Null.into());
        assert!(n.get(&IniKey::Int(-4)).is_some());
        let mut x = IniMap::new();
        x.insert(IniKey::Int(i64::MAX), IniScalar::Null.into());
        assert!(!x.push(IniScalar::Null.into()));
    }

    #[test]
    fn scalar_bytes() {
        assert_eq!(IniScalar::Double(1.5).to_bytes(), b"1.5");
        assert_eq!(IniScalar::Bool(true).to_bytes(), b"1");
        assert_eq!(IniScalar::Null.to_bytes(), b"");
    }
}
