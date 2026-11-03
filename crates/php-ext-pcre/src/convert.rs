//! Conversion of `php-pcre`'s [`MatchArray`] results into PHP arrays.

use php_pcre::{Key, MatchArray, Value as PValue};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};

/// Converts a [`MatchArray`] into a PHP array, preserving key order.
pub(crate) fn to_zarray(m: MatchArray) -> ZArray {
    let mut out = HashTable::with_capacity(m.len());
    for (k, v) in m {
        let key = match k {
            Key::Index(i) => ArrayKey::Int(i),
            Key::Name(n) => ArrayKey::Str(ZStr::from(&*n)),
        };
        out.insert(key, to_value(v));
    }
    ZArray::from(out)
}

/// Converts a single [`php_pcre::Value`].
pub(crate) fn to_value(v: PValue) -> Value {
    match v {
        PValue::Str(s) => Value::Str(ZStr::from(s)),
        PValue::Null => Value::Null,
        PValue::Int(i) => Value::Int(i),
        PValue::Array(a) => Value::Array(to_zarray(a)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_types() {
        let mut m = MatchArray::new();
        m.set(Key::name(b"name"), PValue::Str(b"x".to_vec()));
        m.push(PValue::Str(b"x".to_vec()));
        m.push(PValue::Null);
        let mut inner = MatchArray::new();
        inner.push(PValue::Str(b"y".to_vec()));
        inner.push(PValue::Int(3));
        m.push(PValue::Array(inner));
        let z = to_zarray(m);
        let keys: Vec<ArrayKey> = z.keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                ArrayKey::Str(ZStr::from("name")),
                ArrayKey::Int(0),
                ArrayKey::Int(1),
                ArrayKey::Int(2)
            ]
        );
        assert!(matches!(z.get_int(1), Some(Value::Null)));
        match z.get_int(2) {
            Some(Value::Array(a)) => assert!(matches!(a.get_int(1), Some(Value::Int(3)))),
            other => panic!("unexpected {other:?}"),
        }
    }
}
