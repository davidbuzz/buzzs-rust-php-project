// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sort`/`rsort`/`asort`/`ksort` orderings (default flags) built from
//! `HashTable::sort_by` + `ops::compare`, compared with the PHP oracle:
//!
//! ```text
//! $a = [3, "10", "9", "abc", 1.5, true, null, "1e1", [], "", "a", 0, -1, "  5"];
//! ```

use php_runtime::{ops, ArrayKey, CollectingContext, HashTable, Value, ZArray};

fn json(t: &HashTable) -> String {
    let item = |v: &Value| match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => format!("\"{s}\""),
        Value::Array(_) => "[]".to_string(),
        _ => "?".to_string(),
    };
    if t.is_list() {
        let v: Vec<String> = t.values().map(item).collect();
        format!("[{}]", v.join(","))
    } else {
        let v: Vec<String> = t
            .iter()
            .map(|(k, v)| format!("\"{}\":{}", String::from_utf8_lossy(&k.to_bytes()), item(v)))
            .collect();
        format!("{{{}}}", v.join(","))
    }
}

fn mixed() -> HashTable {
    let mut t = HashTable::new();
    for v in [
        Value::Int(3),
        Value::from("10"),
        Value::from("9"),
        Value::from("abc"),
        Value::Float(1.5),
        Value::Bool(true),
        Value::Null,
        Value::from("1e1"),
        Value::Array(ZArray::new()),
        Value::from(""),
        Value::from("a"),
        Value::Int(0),
        Value::Int(-1),
        Value::from("  5"),
    ] {
        t.push(v).unwrap();
    }
    t
}

#[test]
fn sort_orders_match_php() {
    let mut ctx = CollectingContext::new();
    let mut a = mixed();
    a.sort_by(|_, x, _, y| ops::compare(x, y, &mut ctx).unwrap_or(0), true);
    assert_eq!(
        json(&a),
        r#"[null,"",-1,0,1.5,3,"  5","9","10","abc",true,"1e1","a",[]]"#
    );
    let mut b = a.clone();
    b.sort_by(
        |_, x, _, y| -ops::compare(x, y, &mut ctx).unwrap_or(0),
        true,
    );
    assert_eq!(
        json(&b),
        r#"["abc","a","10","1e1","9","  5",3,1.5,-1,true,null,[],0,""]"#
    );

    let mut c = HashTable::new();
    for (k, v) in [("b", 1), ("a", 1), ("c", 0), ("10", 1), ("9", 0)] {
        c.insert(ArrayKey::from(k), Value::Int(v));
    }
    c.sort_by(
        |_, x, _, y| ops::compare(x, y, &mut ctx).unwrap_or(0),
        false,
    );
    assert_eq!(json(&c), r#"{"c":0,"9":0,"b":1,"a":1,"10":1}"#);
    c.sort_by(
        |k1, _, k2, _| ops::compare_keys(k1, k2, &mut ctx).unwrap_or(0),
        false,
    );
    assert_eq!(json(&c), r#"{"9":0,"10":1,"a":1,"b":1,"c":0}"#);
}
