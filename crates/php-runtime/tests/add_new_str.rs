// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `HashTable::add_new_str` (`zend_hash_add_new`, `Zend/zend_hash.c`): a
//! string key known to be absent is appended without a probe, in order,
//! through the table's initialisation and its growth, and is found again
//! by the ordinary lookups.

use php_runtime::{ArrayKey, HashTable, Value, ZStr};

#[test]
fn appended_keys_are_found_in_order_through_growth() {
    for cap in [0, 1, 8, 100] {
        let mut t = HashTable::with_capacity_mixed(cap);
        for i in 0..50_i64 {
            t.add_new_str(ZStr::from(format!("k{i}")), Value::Int(i));
        }
        assert_eq!(t.len(), 50);
        for i in 0..50_i64 {
            let key = ArrayKey::Str(ZStr::from(format!("k{i}")));
            assert!(matches!(t.get(&key), Some(Value::Int(v)) if *v == i));
        }
        let order: Vec<String> = t
            .iter()
            .map(|(k, _)| match k {
                ArrayKey::Str(s) => s.to_string_lossy(),
                ArrayKey::Int(n) => n.to_string(),
            })
            .collect();
        let want: Vec<String> = (0..50).map(|i| format!("k{i}")).collect();
        assert_eq!(order, want);
        // An ordinary insert of an appended key updates it in place.
        t.insert(ArrayKey::Str(ZStr::from("k3")), Value::Int(-1));
        assert_eq!(t.len(), 50);
    }
}
