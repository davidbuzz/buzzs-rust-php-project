//! `unserialize()` results, error offsets, key semantics and performance,
//! compared with the PHP oracle (`tests/data/gen_unserialize.php` →
//! `tests/data/unserialize_cases.txt`).

use php_runtime::serialize::{unserialize, UnserializeOptions};
use php_runtime::{ArrayKey, CollectingContext, PlainObject, Value};
use std::fmt::Write;
use std::time::{Duration, Instant};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn offsets_match_php() {
    let mut failures = Vec::new();
    for line in include_str!("data/unserialize_cases.txt").lines() {
        let (hex, want) = line.split_once(' ').unwrap();
        let input = unhex(hex);
        let mut ctx = CollectingContext::new();
        let _ = unserialize(&input, &UnserializeOptions::default(), &mut ctx).unwrap();
        let got = ctx
            .diagnostics
            .iter()
            .rev()
            .find(|d| d.message.contains("Error at offset") || d.message.contains("Extra data"))
            .map_or_else(|| "ok".to_string(), |d| d.message.clone());
        if got != want {
            failures.push(format!(
                "{:?}: want {want:?} got {got:?}",
                String::from_utf8_lossy(&input)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn numeric_keys_are_integers() {
    // The table handed to unserialize_object_finish (what __unserialize
    // receives) keeps integer keys.
    struct Ctx(CollectingContext, Vec<ArrayKey>);
    impl php_runtime::RuntimeContext for Ctx {
        fn diagnostic(&mut self, l: php_runtime::ErrorLevel, m: &str) -> php_runtime::RtResult<()> {
            self.0.diagnostic(l, m)
        }
        fn unserialize_object_create(
            &mut self,
            class: &php_runtime::ZStr,
        ) -> php_runtime::RtResult<Value> {
            self.0.unserialize_object_create(class)
        }
        fn unserialize_object_finish(
            &mut self,
            obj: &Value,
            props: php_runtime::ZArray,
        ) -> php_runtime::RtResult<()> {
            self.1 = props.keys().cloned().collect();
            self.0.unserialize_object_finish(obj, props)
        }
        fn property_write(
            &mut self,
            obj: &php_runtime::Obj,
            name: &php_runtime::ZStr,
            value: Value,
        ) -> php_runtime::RtResult<()> {
            self.0.property_write(obj, name, value)
        }
    }
    let mut ctx = CollectingContext::new();
    let opts = UnserializeOptions::default();
    // a:... with "0" string keys -> integer keys.
    let v = unserialize(b"a:2:{s:1:\"0\";i:5;s:2:\"01\";i:6;}", &opts, &mut ctx)
        .unwrap()
        .unwrap();
    let a = v.as_array().unwrap();
    assert_eq!(a.get_int(0).and_then(Value::as_int), Some(5));
    assert_eq!(a.get_str(b"01").and_then(Value::as_int), Some(6));

    let mut c = Ctx(CollectingContext::new(), Vec::new());
    let v = unserialize(b"O:1:\"A\":2:{i:0;s:1:\"x\";i:1;s:1:\"y\";}", &opts, &mut c)
        .unwrap()
        .unwrap();
    assert_eq!(c.1, vec![ArrayKey::Int(0), ArrayKey::Int(1)]);
    // A plain property fill uses string property names.
    let props = v
        .as_object()
        .unwrap()
        .downcast_ref::<PlainObject>()
        .unwrap()
        .props();
    assert!(props.get_str(b"0").is_some());
}

#[test]
fn large_inputs_are_linear() {
    let n = 50_000;
    let mut s = format!("a:{n}:{{");
    for i in 0..n {
        let key = format!("k{i}");
        let _ = write!(
            s,
            "s:{}:\"{key}\";O:8:\"stdClass\":1:{{s:1:\"v\";i:{i};}}",
            key.len()
        );
    }
    s.push('}');
    let mut obj = format!("O:8:\"stdClass\":{n}:{{");
    for i in 0..n {
        let _ = write!(obj, "i:{i};r:1;");
    }
    obj.push('}');
    let start = Instant::now();
    let mut ctx = CollectingContext::new();
    let v = unserialize(s.as_bytes(), &UnserializeOptions::default(), &mut ctx)
        .unwrap()
        .unwrap();
    assert_eq!(v.as_array().unwrap().len(), n);
    let o = unserialize(obj.as_bytes(), &UnserializeOptions::default(), &mut ctx)
        .unwrap()
        .unwrap();
    assert!(ctx.diagnostics.is_empty());
    // Break the self-reference cycle.
    o.as_object()
        .unwrap()
        .downcast_ref::<PlainObject>()
        .unwrap()
        .set_props(php_runtime::ZArray::new());
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "took {:?}",
        start.elapsed()
    );
}
