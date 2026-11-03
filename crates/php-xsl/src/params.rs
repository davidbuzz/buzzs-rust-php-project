//! Stylesheet parameters (`setParameter`/`getParameter`/`removeParameter`).
//!
//! Parameters live in an insertion-ordered table keyed by the parameter
//! name in Clark notation (`{namespace}name`, or `name` without a
//! namespace), mirroring `xsl_object::parameter`. Values are applied with
//! `xsltQuoteOneUserParam` at transformation time, i.e. as plain strings
//! (no XPath evaluation, so any mix of quotes is fine).

use crate::error::{XslError, XslResult};

/// A key of the array form of `setParameter()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKey {
    /// An integer key (rejected).
    Int(i64),
    /// A string key.
    Str(Vec<u8>),
}

/// The ordered parameter table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parameters {
    entries: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Parameters {
    /// `zend_hash_update`: replaces in place or appends.
    pub fn insert(&mut self, key: Vec<u8>, value: Vec<u8>) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// Looks a key up.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_slice())
    }

    /// `zend_hash_del`: whether the key existed.
    pub fn remove(&mut self, key: &[u8]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(k, _)| k != key);
        before != self.entries.len()
    }

    /// The `(key, value)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.entries
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
    }

    /// Number of parameters.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no parameters.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `xsl_create_parameter_key`: the table key for `name` in `namespace`.
///
/// Errors (raised for argument #`arg_num` of `function`) when a namespace
/// is given together with a Clark-notation name or a QName.
pub fn parameter_key(
    function: &'static str,
    arg_num: u32,
    namespace: &[u8],
    name: &[u8],
) -> XslResult<Vec<u8>> {
    if namespace.is_empty() {
        return Ok(name.to_vec());
    }
    if name.first() == Some(&b'{') {
        return Err(XslError::arg_value(
            function,
            arg_num,
            "name",
            "must not use clark notation when argument #1 ($namespace) is not empty",
        ));
    }
    // C checks the NUL-terminated string: only bytes before a NUL count.
    let c_name = name.split(|&b| b == 0).next().unwrap_or_default();
    if name.first() != Some(&b':') && c_name.contains(&b':') {
        return Err(XslError::arg_value(
            function,
            arg_num,
            "name",
            "must not be a QName when argument #1 ($namespace) is not empty",
        ));
    }
    let mut key = Vec::with_capacity(name.len() + namespace.len() + 2);
    key.push(b'{');
    key.extend_from_slice(namespace);
    key.push(b'}');
    key.extend_from_slice(name);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            parameter_key("setParameter", 2, b"", b"foo").unwrap(),
            b"foo"
        );
        assert_eq!(
            parameter_key("setParameter", 2, b"urn:x", b"foo").unwrap(),
            b"{urn:x}foo"
        );
        assert_eq!(
            parameter_key("setParameter", 2, b"urn:x", b":b").unwrap(),
            b"{urn:x}:b"
        );
        assert_eq!(
            parameter_key("setParameter", 2, b"urn:x", b":").unwrap(),
            b"{urn:x}:"
        );
        assert_eq!(
            parameter_key("getParameter", 2, b"urn:x", b"{urn:a}x")
                .unwrap_err()
                .message(),
            "XSLTProcessor::getParameter(): Argument #2 ($name) must not use clark notation when argument #1 ($namespace) is not empty"
        );
        assert_eq!(
            parameter_key("removeParameter", 2, b"urn:x", b"a:b")
                .unwrap_err()
                .message(),
            "XSLTProcessor::removeParameter(): Argument #2 ($name) must not be a QName when argument #1 ($namespace) is not empty"
        );
    }

    #[test]
    fn table_is_ordered() {
        let mut p = Parameters::default();
        p.insert(b"a".to_vec(), b"1".to_vec());
        p.insert(b"b".to_vec(), b"2".to_vec());
        p.insert(b"a".to_vec(), b"3".to_vec());
        let all: Vec<_> = p.iter().collect();
        assert_eq!(all, vec![(&b"a"[..], &b"3"[..]), (&b"b"[..], &b"2"[..])]);
        assert!(p.remove(b"a"));
        assert!(!p.remove(b"a"));
        assert_eq!(p.get(b"b"), Some(&b"2"[..]));
        assert_eq!(p.len(), 1);
    }
}
