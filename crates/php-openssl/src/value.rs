//! A minimal PHP value model used to exchange arrays with the VM.
//!
//! Input arrays (configuration options, distinguished names, key component
//! arrays) are passed in as [`PhpArray`]; structured results such as
//! `openssl_x509_parse()` are returned the same way. String keys must be
//! inserted by the VM with symtable semantics (numeric strings become
//! integer keys), as `add_assoc_*` does in PHP.

/// An array key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
}

impl From<&str> for Key {
    fn from(s: &str) -> Self {
        Key::Str(s.as_bytes().to_vec())
    }
}

impl From<i64> for Key {
    fn from(i: i64) -> Self {
        Key::Int(i)
    }
}

/// A PHP value as far as this extension cares.
#[derive(Debug, Clone, PartialEq)]
pub enum PhpValue {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `float`.
    Float(f64),
    /// Binary string.
    Str(Vec<u8>),
    /// Ordered array.
    Array(PhpArray),
    /// Any object (only its presence/type matters here).
    Object,
}

impl PhpValue {
    /// Convenience constructor for a string value.
    pub fn str(s: impl AsRef<[u8]>) -> Self {
        PhpValue::Str(s.as_ref().to_vec())
    }

    /// Returns the string bytes when this is a string.
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            PhpValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the array when this is an array.
    pub fn as_array(&self) -> Option<&PhpArray> {
        match self {
            PhpValue::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns the integer when this is an int.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            PhpValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// PHP string conversion for scalars (`zval_try_get_string` without
    /// floats/objects, which the VM must convert beforehand). Returns `None`
    /// for arrays and objects.
    pub fn to_php_string(&self) -> Option<Vec<u8>> {
        match self {
            PhpValue::Null | PhpValue::Bool(false) => Some(Vec::new()),
            PhpValue::Bool(true) => Some(b"1".to_vec()),
            PhpValue::Int(i) => Some(i.to_string().into_bytes()),
            PhpValue::Float(f) => Some(format_float(*f).into_bytes()),
            PhpValue::Str(s) => Some(s.clone()),
            PhpValue::Array(_) | PhpValue::Object => None,
        }
    }
}

/// Approximation of PHP's float-to-string conversion (precision 17,
/// shortest round-trip), used only for odd inputs such as float DN values.
fn format_float(f: f64) -> String {
    if f.is_nan() {
        "NAN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "INF" } else { "-INF" }.into()
    } else if f.abs() < 1e15 && f.trunc().to_bits() == f.to_bits() {
        format!("{f:.0}")
    } else {
        format!("{f}")
    }
}

/// A `bool|int` return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOrInt {
    /// `true` / `false`.
    Bool(bool),
    /// An integer (e.g. `-1` for errors).
    Int(i64),
}

/// An ordered PHP array.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhpArray {
    entries: Vec<(Key, PhpValue)>,
}

impl PhpArray {
    /// An empty array.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up a key.
    pub fn get(&self, key: &Key) -> Option<&PhpValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Looks up a string key.
    pub fn get_str(&self, key: &str) -> Option<&PhpValue> {
        self.entries
            .iter()
            .find(|(k, _)| matches!(k, Key::Str(s) if s == key.as_bytes()))
            .map(|(_, v)| v)
    }

    /// Looks up an integer key.
    pub fn get_index(&self, idx: i64) -> Option<&PhpValue> {
        self.get(&Key::Int(idx))
    }

    /// Mutable lookup.
    pub fn get_mut(&mut self, key: &Key) -> Option<&mut PhpValue> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Inserts or replaces (in place, keeping order) like
    /// `zend_hash_update`.
    pub fn set(&mut self, key: impl Into<Key>, value: PhpValue) {
        let key = key.into();
        if let Some(slot) = self.get_mut(&key) {
            *slot = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Appends with the next integer key (`add_next_index_*`).
    pub fn push(&mut self, value: PhpValue) {
        let next = self
            .entries
            .iter()
            .filter_map(|(k, _)| match k {
                Key::Int(i) => Some(*i + 1),
                Key::Str(_) => None,
            })
            .max()
            .unwrap_or(0)
            .max(0);
        self.entries.push((Key::Int(next), value));
    }

    /// Iterates entries in order.
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &PhpValue)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Iterates values in order.
    pub fn values(&self) -> impl Iterator<Item = &PhpValue> {
        self.entries.iter().map(|(_, v)| v)
    }

    /// Consumes into entries.
    pub fn into_entries(self) -> Vec<(Key, PhpValue)> {
        self.entries
    }
}

impl FromIterator<(Key, PhpValue)> for PhpArray {
    fn from_iter<T: IntoIterator<Item = (Key, PhpValue)>>(iter: T) -> Self {
        let mut a = PhpArray::new();
        for (k, v) in iter {
            a.set(k, v);
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_replaces_in_place() {
        let mut a = PhpArray::new();
        a.set("a", PhpValue::Int(1));
        a.set("b", PhpValue::Int(2));
        a.set("a", PhpValue::Int(3));
        let keys: Vec<_> = a.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, vec![Key::from("a"), Key::from("b")]);
        assert_eq!(a.get_str("a"), Some(&PhpValue::Int(3)));
    }

    #[test]
    fn push_uses_next_index() {
        let mut a = PhpArray::new();
        a.push(PhpValue::Null);
        a.set(5i64, PhpValue::Null);
        a.push(PhpValue::Bool(true));
        assert_eq!(a.get_index(6), Some(&PhpValue::Bool(true)));
    }

    #[test]
    fn scalar_to_string() {
        assert_eq!(PhpValue::Int(-3).to_php_string().unwrap(), b"-3");
        assert_eq!(PhpValue::Bool(true).to_php_string().unwrap(), b"1");
        assert_eq!(PhpValue::Null.to_php_string().unwrap(), b"");
        assert!(PhpValue::Array(PhpArray::new()).to_php_string().is_none());
    }
}
