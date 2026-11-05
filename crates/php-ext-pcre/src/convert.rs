//! Conversion of `php-pcre`'s [`MatchArray`] results into PHP arrays.

use php_pcre::{
    Key, MatchArray, MatchRef, Value as PValue, PREG_OFFSET_CAPTURE, PREG_UNMATCHED_AS_NULL,
};
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
        PValue::Str(s) if s.len() <= 1 => Value::Str(str_fast(&s)),
        PValue::Str(s) => Value::Str(ZStr::from(s)),
        PValue::Null => Value::Null,
        PValue::Int(i) => Value::Int(i),
        PValue::Array(a) => Value::Array(to_zarray(a)),
    }
}

/// `zend_string_init_fast()`: empty and one-byte strings are the interned
/// `ZSTR_EMPTY` / `ZSTR_CHAR` instances.
fn str_fast(s: &[u8]) -> ZStr {
    match s {
        [] => ZStr::empty(),
        [c] => ZStr::from_byte(*c),
        _ => ZStr::from(s),
    }
}

/// A byte offset as a PHP int.
fn php_offset(o: usize) -> i64 {
    i64::try_from(o).unwrap_or(i64::MAX)
}

/// `add_offset_pair()`'s `[match, offset]` array.
fn offset_pair(first: Value, offset: i64) -> Value {
    let mut t = HashTable::with_capacity(2);
    t.insert(ArrayKey::Int(0), first);
    t.insert(ArrayKey::Int(1), Value::Int(offset));
    Value::Array(ZArray::from(t))
}

/// `populate_subpat_array()` for a pattern without named groups, built
/// straight from the recorded offsets of `m`.
///
/// This is the `$matches` of the common `preg_match()` call; it yields
/// exactly the table [`to_zarray`] builds from `php-pcre`'s intermediate
/// [`MatchArray`] (same entries, order, capacity and string interning)
/// without materialising that array first. `num_subpats` is the capture
/// count plus one; groups past `m.count()` only appear with
/// `PREG_UNMATCHED_AS_NULL`.
pub(crate) fn unnamed_subpats(m: MatchRef<'_>, num_subpats: usize, flags: i64) -> ZArray {
    let offset_capture = flags & PREG_OFFSET_CAPTURE != 0;
    let unmatched_as_null = flags & PREG_UNMATCHED_AS_NULL != 0;
    let groups = if unmatched_as_null {
        m.count().max(num_subpats)
    } else {
        m.count()
    };
    let mark = m.mark();
    let mut out = HashTable::with_capacity(groups + usize::from(mark.is_some()));
    for i in 0..groups {
        #[allow(clippy::single_match_else)] // the None arm is a multi-line computation
        let val = match (m.span(i), m.group(i)) {
            (Some((start, _)), Some(bytes)) => {
                let s = Value::Str(str_fast(bytes));
                if offset_capture {
                    offset_pair(s, php_offset(start))
                } else {
                    s
                }
            }
            _ => {
                let unset = if unmatched_as_null {
                    Value::Null
                } else {
                    Value::Str(ZStr::empty())
                };
                if offset_capture {
                    offset_pair(unset, -1)
                } else {
                    unset
                }
            }
        };
        out.insert(ArrayKey::Int(php_offset(i)), val);
    }
    if let Some(mark) = mark {
        out.insert(
            ArrayKey::Str(ZStr::from("MARK")),
            Value::Str(str_fast(mark)),
        );
    }
    ZArray::from(out)
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

    /// The direct builder must agree with the general `MatchArray` path for
    /// every flag combination, including unset middle and trailing groups
    /// and `(*MARK)`.
    #[test]
    fn unnamed_subpats_matches_general_path() {
        let re = php_pcre::Regex::compile(b"/(a)(x)?(b)(y)?(*MARK:M)/").unwrap();
        let cfg = php_pcre::MatchConfig::default();
        for flags in [
            0,
            PREG_OFFSET_CAPTURE,
            PREG_UNMATCHED_AS_NULL,
            PREG_OFFSET_CAPTURE | PREG_UNMATCHED_AS_NULL,
        ] {
            for subject in [&b"zzab"[..], &b"zzq"[..]] {
                let spans = php_pcre::match_one_spans(&re, &cfg, subject, true, flags, 0);
                let direct = spans.matches().next().map_or_else(
                    || ZArray::from(HashTable::new()),
                    |m| unnamed_subpats(m, re.capture_count() + 1, flags),
                );
                let general = to_zarray(spans.into_outcome().matches.unwrap());
                assert_eq!(
                    format!("{direct:?}"),
                    format!("{general:?}"),
                    "flags {flags}"
                );
                assert_eq!(direct.table_size(), general.table_size());
            }
        }
    }

    #[test]
    fn short_strings_are_interned() {
        assert!(str_fast(b"").is_interned());
        assert!(str_fast(b"a").is_interned());
        assert!(!str_fast(b"ab").is_interned());
    }
}
