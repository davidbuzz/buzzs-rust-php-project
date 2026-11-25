// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Conversion of `php-pcre`'s [`MatchArray`] results into PHP arrays.

use php_pcre::{
    Key, MatchArray, MatchRef, Value as PValue, PREG_OFFSET_CAPTURE, PREG_UNMATCHED_AS_NULL,
};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};

/// Converts a [`MatchArray`] into a PHP array, preserving key order.
///
/// `utf` says the strings come from a `/u` match of a subject PCRE2
/// validated (or the caller vouched for): they are flagged valid UTF-8
/// as they are made, so a later `/u` match of a capture skips the check.
pub(crate) fn to_zarray(m: MatchArray, utf: bool) -> ZArray {
    let mut out = HashTable::with_capacity(m.len());
    for (k, v) in m {
        let key = match k {
            Key::Index(i) => ArrayKey::Int(i),
            Key::Name(n) => ArrayKey::Str(str_utf(&n, utf)),
        };
        out.insert(key, to_value(v, utf));
    }
    ZArray::from(out)
}

/// Converts a single [`php_pcre::Value`] (`utf`: see [`to_zarray`]).
pub(crate) fn to_value(v: PValue, utf: bool) -> Value {
    match v {
        PValue::Str(s) if s.len() <= 1 => Value::Str(str_fast(&s)),
        PValue::Str(s) => {
            let z = ZStr::from(s);
            if utf {
                z.mark_valid_utf8();
            }
            Value::Str(z)
        }
        PValue::Null => Value::Null,
        PValue::Int(i) => Value::Int(i),
        PValue::Array(a) => Value::Array(to_zarray(a, utf)),
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

/// [`str_fast`], flagged valid UTF-8 when `utf` (see [`to_zarray`];
/// interned strings keep their own flag).
#[inline]
fn str_utf(s: &[u8], utf: bool) -> ZStr {
    let z = str_fast(s);
    if utf {
        z.mark_valid_utf8();
    }
    z
}

/// A byte offset as a PHP int.
fn php_offset(o: usize) -> i64 {
    i64::try_from(o).unwrap_or(i64::MAX)
}

/// `add_offset_pair()`'s `[match, offset]` array.
fn offset_pair(first: Value, offset: i64) -> Value {
    let mut t = HashTable::with_capacity_packed(2);
    let _ = t.push(first);
    let _ = t.push(Value::Int(offset));
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
/// `PREG_UNMATCHED_AS_NULL`. `utf`: see [`to_zarray`].
///
/// The groups are appended to a list made with its storage in place
/// (`zend_hash_next_index_insert_new` into the packed `subpats` of
/// `populate_subpat_array`); a `MARK` entry turns it into a hash, as in
/// php-src.
pub(crate) fn unnamed_subpats(
    m: MatchRef<'_>,
    num_subpats: usize,
    flags: i64,
    utf: bool,
) -> ZArray {
    let offset_capture = flags & PREG_OFFSET_CAPTURE != 0;
    let unmatched_as_null = flags & PREG_UNMATCHED_AS_NULL != 0;
    let groups = if unmatched_as_null {
        m.count().max(num_subpats)
    } else {
        m.count()
    };
    let mark = m.mark();
    let mut out = HashTable::with_capacity_packed(groups + usize::from(mark.is_some()));
    for i in 0..groups {
        #[allow(clippy::single_match_else)] // the None arm is a multi-line computation
        let val = match (m.span(i), m.group(i)) {
            (Some((start, _)), Some(bytes)) => {
                let s = Value::Str(str_utf(bytes, utf));
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
        // A fresh list always takes the next index (0, 1, ...).
        let _ = out.push(val);
    }
    if let Some(mark) = mark {
        out.insert(
            ArrayKey::Str(ZStr::from("MARK")),
            Value::Str(str_utf(mark, utf)),
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
        let z = to_zarray(m, false);
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
                    |m| unnamed_subpats(m, re.capture_count() + 1, flags, false),
                );
                let general = to_zarray(spans.into_outcome().matches.unwrap(), false);
                assert_eq!(
                    format!("{direct:?}"),
                    format!("{general:?}"),
                    "flags {flags}"
                );
                assert_eq!(direct.table_size(), general.table_size());
            }
        }
    }

    /// Captures of a `/u` match are flagged valid UTF-8 as they are made
    /// (the strings of other matches are not); interned one-byte and empty
    /// strings keep their own flag.
    #[test]
    fn utf_captures_are_flagged() {
        let re = php_pcre::Regex::compile(b"/([^ ]+) ([^ ]+)(*MARK:mk)/u").unwrap();
        let cfg = php_pcre::MatchConfig::default();
        let spans = php_pcre::match_one_spans(&re, &cfg, "h\u{e9}llo w".as_bytes(), true, 0, 0);
        let m = spans.matches().next().unwrap();
        for utf in [false, true] {
            let z = unnamed_subpats(m, re.capture_count() + 1, 0, utf);
            let Some(Value::Str(g1)) = z.get_int(1) else {
                panic!("group 1 missing");
            };
            assert_eq!(g1.as_bytes(), "h\u{e9}llo".as_bytes());
            assert_eq!(g1.is_valid_utf8(), utf);
            let Some(Value::Str(mark)) = z.get_str(b"MARK") else {
                panic!("MARK missing");
            };
            assert_eq!(mark.is_valid_utf8(), utf);
            let Some(Value::Str(g2)) = z.get_int(2) else {
                panic!("group 2 missing");
            };
            assert!(g2.is_interned(), "one byte");
        }
        let mut general = MatchArray::new();
        general.push(PValue::Str(b"abc".to_vec()));
        let z = to_zarray(general.clone(), true);
        assert!(matches!(z.get_int(0), Some(Value::Str(s)) if s.is_valid_utf8()));
        let z = to_zarray(general, false);
        assert!(matches!(z.get_int(0), Some(Value::Str(s)) if !s.is_valid_utf8()));
    }

    /// The `$matches` of a pattern without names is a list (packed), as
    /// php-src's `populate_subpat_array` makes it; `MARK` makes a hash.
    #[test]
    fn unnamed_subpats_is_a_list_without_mark() {
        let re = php_pcre::Regex::compile(b"/(a)(b)/").unwrap();
        let cfg = php_pcre::MatchConfig::default();
        let spans = php_pcre::match_one_spans(&re, &cfg, b"ab", true, 0, 0);
        let z = unnamed_subpats(spans.matches().next().unwrap(), 3, 0, false);
        assert!(z.is_packed());
        assert_eq!(z.len(), 3);
        let spans = php_pcre::match_one_spans(&re, &cfg, b"ab", true, PREG_OFFSET_CAPTURE, 0);
        let z = unnamed_subpats(
            spans.matches().next().unwrap(),
            3,
            PREG_OFFSET_CAPTURE,
            false,
        );
        let Some(Value::Array(pair)) = z.get_int(2) else {
            panic!("pair missing");
        };
        assert!(pair.is_packed());
        assert!(matches!(pair.get_int(1), Some(Value::Int(1))));
    }

    #[test]
    fn short_strings_are_interned() {
        assert!(str_fast(b"").is_interned());
        assert!(str_fast(b"a").is_interned());
        assert!(!str_fast(b"ab").is_interned());
    }
}
