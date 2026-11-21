// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Unit tests for behaviours the oracle fixture cannot express:
//! references, recursion, the internal pointer, objects, and php-src master
//! semantics that differ from the installed oracle PHP.

use crate::symbols::{SymbolTable, VarSlot};
use crate::*;
use php_runtime::{
    ArrayKey, CollectingContext, HashTable, Obj, PhpObject, PhpRef, RtResult, RuntimeError, Value,
    ZArray, ZStr,
};

type Ctx = CollectingContext;

fn list(vals: impl IntoIterator<Item = Value>) -> ZArray {
    ZArray::from_values(vals)
}

fn ints(v: &[i64]) -> Value {
    Value::Array(list(v.iter().map(|&i| Value::Int(i))))
}

fn arr_of(v: &Value) -> ZArray {
    match v {
        Value::Array(a) => a.clone(),
        Value::Reference(r) => arr_of(&r.get()),
        other => panic!("not an array: {other:?}"),
    }
}

fn int_vals(a: &HashTable) -> Vec<i64> {
    a.values()
        .map(|v| match util::deref(v) {
            Value::Int(i) => i,
            other => panic!("not int: {other:?}"),
        })
        .collect()
}

fn keys(a: &HashTable) -> Vec<String> {
    a.keys()
        .map(|k| match k {
            ArrayKey::Int(i) => i.to_string(),
            ArrayKey::Str(s) => format!("'{}'", s.to_string_lossy()),
        })
        .collect()
}

/// `$r = [..]; $r[] = &$r;` — an array containing a reference to itself.
fn self_ref(vals: Value) -> PhpRef {
    let r = PhpRef::new(vals);
    let me = Value::Reference(r.clone());
    if let Value::Array(a) = &mut *r.borrow_mut() {
        a.make_mut().push(me).unwrap();
    }
    r
}

fn cmp_cb() -> impl FnMut(&mut Ctx, &[Value]) -> RtResult<Value> {
    |c: &mut Ctx, a: &[Value]| php_runtime::ops::spaceship(&a[0], &a[1], c)
}

// ---------------------------------------------------------------------------
// Recursion
// ---------------------------------------------------------------------------

#[test]
fn count_recursive_detects_cycles() {
    let mut ctx = Ctx::new();
    let r = self_ref(Value::Array(list([Value::Int(1), ints(&[2, 3])])));
    let n = basic::count(&mut ctx, "count", &r.get(), COUNT_RECURSIVE).unwrap();
    assert_eq!(n, 5);
    assert_eq!(ctx.messages(), ["Warning: count(): Recursion detected"]);
    let n = basic::count(&mut ctx, "sizeof", &r.get(), COUNT_NORMAL).unwrap();
    assert_eq!(n, 3);
}

#[test]
fn merge_recursive_detects_cycles() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(Value::Array(ZArray::from_pairs([("x", ints(&[1]))])));
    let me = Value::Reference(r.clone());
    if let Value::Array(a) = &mut *r.borrow_mut() {
        a.make_mut().insert("y", me);
    }
    let v = r.get();
    let err = merge::array_merge_recursive(&mut ctx, &[v.clone(), v]).unwrap_err();
    assert_eq!(err.message(), Some("Recursion detected"));
}

#[test]
fn replace_recursive_detects_cycles() {
    let r = PhpRef::new(Value::Array(ZArray::from_pairs([("k", Value::Int(1))])));
    let me = Value::Reference(r.clone());
    if let Value::Array(a) = &mut *r.borrow_mut() {
        a.make_mut().insert("r", me);
    }
    let v = r.get();
    let err = merge::array_replace_recursive(&[v.clone(), v]).unwrap_err();
    assert_eq!(err.message(), Some("Recursion detected"));
}

#[test]
fn walk_recursive_detects_cycles() {
    let mut ctx = Ctx::new();
    let r = self_ref(Value::Array(list([Value::Int(1), ints(&[2])])));
    let mut seen = Vec::new();
    let mut cb = |_: &mut Ctx, a: &[Value]| {
        seen.push(format!("{:?}={:?}", a[1], util::deref(&a[0])));
        Ok(Value::Null)
    };
    let err = callback::array_walk_recursive(&mut ctx, &r, &mut cb, None).unwrap_err();
    assert_eq!(err.message(), Some("Recursion detected"));
    assert_eq!(
        seen,
        [
            "Int(0)=Int(1)",
            "Int(0)=Int(2)",
            "Int(0)=Int(1)",
            "Int(0)=Int(2)"
        ]
    );
}

#[test]
fn compact_detects_cycles() {
    let mut ctx = Ctx::new();
    let r = self_ref(Value::Array(list([Value::from("a")])));
    let mut vars = TestVars::default();
    let err = symbols::compact(&mut ctx, &mut vars, &[r.get()]).unwrap_err();
    assert_eq!(err.message(), Some("Recursion detected"));
    assert_eq!(
        ctx.messages(),
        ["Warning: compact(): Undefined variable $a"]
    );
}

// ---------------------------------------------------------------------------
// Internal pointer
// ---------------------------------------------------------------------------

#[test]
fn internal_pointer_walk() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[1, 2, 3]));
    assert_eq!(pointer::next(&mut ctx, &r).unwrap().as_int(), Some(2));
    assert_eq!(pointer::next(&mut ctx, &r).unwrap().as_int(), Some(3));
    assert!(matches!(
        pointer::next(&mut ctx, &r).unwrap(),
        Value::Bool(false)
    ));
    assert!(matches!(
        pointer::current(&mut ctx, "current", &r.get()).unwrap(),
        Value::Bool(false)
    ));
    assert!(pointer::key(&mut ctx, &r.get()).unwrap().is_null());
    assert!(matches!(
        pointer::prev(&mut ctx, &r).unwrap(),
        Value::Bool(false)
    ));
    assert_eq!(pointer::reset(&mut ctx, &r).unwrap().as_int(), Some(1));
    assert_eq!(pointer::end(&mut ctx, &r).unwrap().as_int(), Some(3));
    assert_eq!(pointer::key(&mut ctx, &r.get()).unwrap().as_int(), Some(2));
    assert_eq!(pointer::prev(&mut ctx, &r).unwrap().as_int(), Some(2));

    let e = PhpRef::new(Value::empty_array());
    assert!(matches!(
        pointer::end(&mut ctx, &e).unwrap(),
        Value::Bool(false)
    ));
    assert!(matches!(
        pointer::current(&mut ctx, "pos", &e.get()).unwrap(),
        Value::Bool(false)
    ));

    // Holes are skipped.
    let mut t = HashTable::new();
    for i in 0..4 {
        t.push(Value::Int(i)).unwrap();
    }
    t.remove(&ArrayKey::Int(1));
    let h = PhpRef::new(Value::Array(ZArray::from_table(t)));
    assert_eq!(pointer::next(&mut ctx, &h).unwrap().as_int(), Some(2));
    assert_eq!(pointer::key(&mut ctx, &h.get()).unwrap().as_int(), Some(2));
    assert!(ctx.messages().is_empty());
}

#[test]
fn pointer_on_self_reference_does_not_panic() {
    let mut ctx = Ctx::new();
    let r = self_ref(ints(&[1]));
    pointer::end(&mut ctx, &r).unwrap();
    assert!(matches!(
        pointer::current(&mut ctx, "current", &r.get()).unwrap(),
        Value::Array(_)
    ));
}

#[test]
fn copies_share_internal_pointer_position() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[1, 2, 3]));
    pointer::next(&mut ctx, &r).unwrap();
    let merged = merge::array_merge(&mut ctx, &[r.get(), Value::empty_array()]).unwrap();
    assert_eq!(merged.current().and_then(Value::as_int), Some(2));
}

struct TestObj;

impl PhpObject for TestObj {
    fn handle(&self) -> u32 {
        7
    }
    fn class_name(&self) -> ZStr {
        ZStr::from("Foo")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn object_arguments() {
    let mut ctx = Ctx::new();
    let o = Value::Object(Obj::new(TestObj));
    assert!(matches!(
        pointer::current(&mut ctx, "current", &o).unwrap(),
        Value::Bool(false)
    ));
    let r = PhpRef::new(o.clone());
    let mut cb = |_: &mut Ctx, _: &[Value]| Ok(Value::Null);
    callback::array_walk(&mut ctx, &r, &mut cb, None).unwrap();
    assert_eq!(
        ctx.take().into_iter().map(|d| d.message).collect::<Vec<_>>(),
        [
            "current(): Calling current() on an object is deprecated",
            "array_walk(): Passing an object for argument #1 $array to array_walk() is deprecated, call get_object_vars() first instead",
        ]
    );
    let err = basic::count(&mut ctx, "count", &o, 0).unwrap_err();
    assert_eq!(
        err.message(),
        Some("count(): Argument #1 ($value) must be of type Countable|array, Foo given")
    );
    let err = basic::count(&mut ctx, "sizeof", &Value::Bool(true), 0).unwrap_err();
    assert_eq!(
        err.message(),
        Some("sizeof(): Argument #1 ($value) must be of type Countable|array, true given")
    );
    let err = basic::count(&mut ctx, "count", &ints(&[]), 5).unwrap_err();
    assert_eq!(
        err.message(),
        Some("count(): Argument #2 ($mode) must be either COUNT_NORMAL or COUNT_RECURSIVE")
    );
    let sum = basic::array_sum(&mut ctx, &list([Value::Int(1), o])).unwrap();
    assert_eq!(sum.as_int(), Some(1));
    assert_eq!(
        ctx.messages(),
        ["Warning: array_sum(): Addition is not supported on type Foo"]
    );
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::many_single_char_names)]
fn reference_semantics() {
    let mut ctx = Ctx::new();
    // array_merge keeps shared references.
    let x = PhpRef::new(Value::Int(1));
    let src = Value::Array(ZArray::from_pairs([
        ("a", Value::Reference(x.clone())),
        ("b", Value::Int(2)),
    ]));
    let mut m = merge::array_merge(
        &mut ctx,
        &[
            src,
            Value::Array(ZArray::from_pairs([("c", Value::Int(3))])),
        ],
    )
    .unwrap();
    if let Some(Value::Reference(r)) = m.make_mut().get_mut(&ArrayKey::from("a")) {
        r.set(Value::Int(9));
    } else {
        panic!("reference lost");
    }
    assert_eq!(x.get().as_int(), Some(9));

    // array_pop returns the dereferenced value.
    let p = PhpRef::new(Value::Array(list([Value::Reference(x.clone())])));
    assert_eq!(stack::array_pop(&p).unwrap().as_int(), Some(9));

    // sort() keeps the reference in the array.
    let s = PhpRef::new(Value::Array(list([
        Value::Int(3),
        Value::Reference(x.clone()),
        Value::Int(2),
    ])));
    sort::sort(&mut ctx, &s, SORT_REGULAR).unwrap();
    x.set(Value::Int(100));
    assert_eq!(int_vals(&arr_of(&s.get())), [2, 3, 100]);

    // array_walk modifies through the reference it passes.
    let w = PhpRef::new(ints(&[1, 2]));
    let mut cb = |c: &mut Ctx, a: &[Value]| {
        if let Value::Reference(r) = &a[0] {
            let v = php_runtime::ops::mul(&r.get(), &Value::Int(10), c)?;
            r.set(v);
        }
        Ok(Value::Null)
    };
    callback::array_walk(&mut ctx, &w, &mut cb, None).unwrap();
    assert_eq!(int_vals(&arr_of(&w.get())), [10, 20]);
    // References with refcount 1 are unwrapped when copied.
    let copy = basic::array_slice(&arr_of(&w.get()), 0, None, false);
    assert!(copy.values().all(|v| !v.is_reference()));
    assert!(ctx.messages().is_empty());
}

#[test]
fn walk_sees_appended_elements_and_stops_on_non_array() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[1, 2]));
    let target = r.clone();
    let mut seen = Vec::new();
    let mut cb = |_: &mut Ctx, a: &[Value]| {
        let v = util::deref(&a[0]).as_int().unwrap_or(0);
        seen.push(v);
        if v == 1 {
            if let Value::Array(t) = &mut *target.borrow_mut() {
                t.make_mut().push(Value::Int(3)).unwrap();
            }
        }
        if v == 3 {
            target.set(Value::Int(0));
        }
        Ok(Value::Null)
    };
    let err = callback::array_walk(&mut ctx, &r, &mut cb, None).unwrap_err();
    assert_eq!(seen, [1, 2, 3]);
    assert_eq!(
        err.message(),
        Some("Iterated value is no longer an array or object")
    );
}

#[test]
fn usort_assigns_even_on_exception() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("x", Value::Int(2)),
        ("y", Value::Int(1)),
    ])));
    let mut calls = 0;
    let mut cb = |_: &mut Ctx, _: &[Value]| {
        calls += 1;
        Err(RuntimeError::Error("boom".into()))
    };
    let err = sort::usort(&mut ctx, &r, &mut cb).unwrap_err();
    assert_eq!(err.message(), Some("boom"));
    assert_eq!(calls, 1);
    // usort() renumbers even when the comparison failed.
    assert_eq!(keys(&arr_of(&r.get())), ["0", "1"]);
}

#[test]
fn multisort_writes_back_through_references() {
    let mut ctx = Ctx::new();
    let a = PhpRef::new(ints(&[3, 1, 2]));
    let b = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("x", Value::from("c")),
        ("y", Value::from("a")),
        ("z", Value::from("b")),
    ])));
    sort::array_multisort(
        &mut ctx,
        &[Value::Reference(a.clone()), Value::Reference(b.clone())],
    )
    .unwrap();
    assert_eq!(int_vals(&arr_of(&a.get())), [1, 2, 3]);
    assert_eq!(keys(&arr_of(&b.get())), ["'y'", "'z'", "'x'"]);
    // nNextFreeElement becomes the array size even with string keys.
    assert_eq!(arr_of(&b.get()).next_free_element(), 3);
}

// ---------------------------------------------------------------------------
// extract / compact
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TestVars(Vec<(ZStr, VarSlot)>);

impl SymbolTable for TestVars {
    fn lookup(&mut self, name: &ZStr) -> VarSlot {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map_or(VarSlot::Missing, |(_, v)| v.clone())
    }
    fn assign(&mut self, name: &ZStr, value: Value) -> RtResult<()> {
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some((_, VarSlot::Defined(Value::Reference(r)))) => {
                r.set(value);
            }
            Some((_, slot)) => *slot = VarSlot::Defined(value),
            None => self.0.push((name.clone(), VarSlot::Defined(value))),
        }
        Ok(())
    }
    fn set(&mut self, name: &ZStr, value: Value) {
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some((_, slot)) => *slot = VarSlot::Defined(value),
            None => self.0.push((name.clone(), VarSlot::Defined(value))),
        }
    }
}

impl TestVars {
    fn get(&self, name: &str) -> Option<Value> {
        self.0
            .iter()
            .find(|(n, _)| n.as_bytes() == name.as_bytes())
            .and_then(|(_, v)| match v {
                VarSlot::Defined(v) => Some(v.clone()),
                _ => None,
            })
    }
}

#[test]
fn extract_refs_binds_to_elements() {
    let src = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("a", Value::Int(1)),
        ("b", Value::Int(2)),
    ])));
    let mut vars = TestVars::default();
    let n = symbols::extract(&mut vars, &Value::Reference(src.clone()), EXTR_REFS, None).unwrap();
    assert_eq!(n, 2);
    let Some(Value::Reference(a)) = vars.get("a") else {
        panic!("not bound by reference")
    };
    a.set(Value::Int(42));
    let arr = arr_of(&src.get());
    assert_eq!(
        util::deref(arr.get(&ArrayKey::from("a")).unwrap()).as_int(),
        Some(42)
    );
}

#[test]
fn extract_undefined_compiled_variables() {
    let mut vars = TestVars(vec![(ZStr::from("a"), VarSlot::Undefined)]);
    let src = Value::Array(ZArray::from_pairs([("a", Value::Int(1))]));
    // EXTR_IF_EXISTS skips undefined CVs, EXTR_SKIP fills them.
    assert_eq!(
        symbols::extract(&mut vars, &src, EXTR_IF_EXISTS, None).unwrap(),
        0
    );
    assert_eq!(
        symbols::extract(&mut vars, &src, EXTR_SKIP, None).unwrap(),
        1
    );
    assert_eq!(vars.get("a").and_then(|v| v.as_int()), Some(1));
    let mut vars = TestVars(vec![(ZStr::from("a"), VarSlot::Undefined)]);
    let p = ZStr::from("p");
    assert_eq!(
        symbols::extract(&mut vars, &src, EXTR_PREFIX_SAME, Some(&p)).unwrap(),
        1
    );
    assert!(vars.get("p_a").is_none());
    assert_eq!(vars.get("a").and_then(|v| v.as_int()), Some(1));
}

#[test]
fn extract_argument_errors() {
    let mut vars = TestVars::default();
    let src = ints(&[]);
    let e = symbols::extract(&mut vars, &src, 9, None).unwrap_err();
    assert_eq!(
        e.message(),
        Some("extract(): Argument #2 ($flags) must be a valid extract type")
    );
    let e = symbols::extract(&mut vars, &src, EXTR_PREFIX_ALL, None).unwrap_err();
    assert_eq!(
        e.message(),
        Some("extract(): Argument #3 ($prefix) is required when using this extract type")
    );
    let bad = ZStr::from("1x");
    let e = symbols::extract(&mut vars, &src, EXTR_PREFIX_ALL, Some(&bad)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("extract(): Argument #3 ($prefix) must be a valid identifier")
    );
    assert!(symbols::valid_var_name(b"_a1\x80"));
    assert!(!symbols::valid_var_name(b"\x7f"));
    assert!(!symbols::valid_var_name(b""));
}

// ---------------------------------------------------------------------------
// php-src master semantics (differ from PHP 8.3)
// ---------------------------------------------------------------------------

#[test]
fn bool_comparator_deprecation_and_retry() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[3, 1, 2]));
    let mut cb =
        |c: &mut Ctx, a: &[Value]| Ok(Value::Bool(php_runtime::ops::is_smaller(&a[1], &a[0], c)?));
    sort::usort(&mut ctx, &r, &mut cb).unwrap();
    assert_eq!(int_vals(&arr_of(&r.get())), [1, 2, 3]);
    assert_eq!(
        ctx.messages(),
        ["Deprecated: usort(): Returning bool from comparison function is deprecated, return an integer less than, equal to, or greater than zero"]
    );
}

#[test]
fn master_argument_validation() {
    let mut ctx = Ctx::new();
    let e = basic::array_change_key_case(&arr_of(&ints(&[1])), 2).unwrap_err();
    assert_eq!(
        e.message(),
        Some(
            "array_change_key_case(): Argument #2 ($case) must be either CASE_LOWER or CASE_UPPER"
        )
    );
    let e = callback::array_filter(&mut ctx, &arr_of(&ints(&[1])), None, 7).unwrap_err();
    assert_eq!(e.class_name(), Some("ValueError"));
    let e = random::array_rand(&HashTable::new(), 1, &mut |a, _| Ok(a)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("array_rand(): Argument #1 ($array) must not be empty")
    );
    let e = range::range(
        &mut ctx,
        &Value::Int(1),
        &Value::Int(2),
        Some(&Value::Int(i64::MIN)),
    )
    .unwrap_err();
    assert_eq!(
        e.message(),
        Some("range(): Argument #3 ($step) must be greater than -9223372036854775808")
    );
    let e = range::range(&mut ctx, &Value::Int(0), &Value::Int(i64::MAX), None).unwrap_err();
    assert_eq!(
        e.message(),
        Some("The supplied range exceeds the maximum array size by 9223372035781033984 elements: start=0, end=9223372036854775807, step=1. Calculated size: 9223372036854775807. Maximum size: 1073741824.")
    );
    let e = range::range(
        &mut ctx,
        &Value::Int(0),
        &Value::Float(1e10),
        Some(&Value::Float(0.5)),
    )
    .unwrap_err();
    assert_eq!(
        e.message(),
        Some("The supplied range exceeds the maximum array size by 18926258177.0 elements: start=0.0, end=10000000000.0, step=0.5. Max size: 1073741824")
    );
    assert!(ctx.messages().is_empty());
}

#[test]
fn null_keys_are_deprecated() {
    let mut ctx = Ctx::new();
    let t = ZArray::from_pairs([("", Value::Int(1))]);
    assert!(basic::array_key_exists(&mut ctx, "key_exists", &Value::Null, &t).unwrap());
    let rows = list([Value::Array(ZArray::from_pairs([
        ("id", Value::Null),
        ("v", Value::Int(5)),
    ]))]);
    let col = basic::ColumnKey::Str(ZStr::from("v"));
    let idx = basic::ColumnKey::Str(ZStr::from("id"));
    let out = basic::array_column(&mut ctx, &rows, Some(&col), Some(&idx)).unwrap();
    assert_eq!(keys(&out), ["''"]);
    assert_eq!(
        ctx.messages(),
        [
            "Deprecated: Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead",
            "Deprecated: Using null as an array offset is deprecated, use an empty string instead",
        ]
    );
    let e = basic::array_key_exists(&mut ctx, "key_exists", &ints(&[]), &t).unwrap_err();
    assert_eq!(
        e.message(),
        Some("key_exists(): Argument #1 ($key) must be a valid array offset type")
    );
}

#[test]
fn intersect_hash_path_converts_each_value_once() {
    let mut ctx = Ctx::new();
    let out = setops::array_intersect(
        &mut ctx,
        &[
            Value::Array(list([ints(&[1]), Value::from("Array"), Value::Int(2)])),
            Value::Array(list([Value::from("Array")])),
        ],
    )
    .unwrap();
    assert_eq!(keys(&out), ["0", "1"]);
    assert_eq!(ctx.messages(), ["Warning: Array to string conversion"]);
    // An empty argument keeps nNextFreeElement of the first array.
    let out = setops::array_intersect(&mut ctx, &[ints(&[1, 2, 3]), ints(&[])]).unwrap();
    assert!(out.is_empty());
    assert_eq!(out.next_free_element(), 3);
}

#[test]
fn shuffle_uses_engine_range_and_renumbers() {
    let r = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("a", Value::Int(1)),
        ("b", Value::Int(2)),
        ("c", Value::Int(3)),
    ])));
    let mut calls = Vec::new();
    let mut rng = |a: i64, b: i64| {
        calls.push((a, b));
        Ok(a)
    };
    random::shuffle(&r, &mut rng).unwrap();
    assert_eq!(calls, [(0, 2), (0, 1)]);
    assert_eq!(int_vals(&arr_of(&r.get())), [2, 3, 1]);
    assert_eq!(keys(&arr_of(&r.get())), ["0", "1", "2"]);
    let mut broken = |_: i64, _: i64| Ok(0);
    let e = random::array_rand(&arr_of(&ints(&[1, 2, 3, 4])), 2, &mut broken).unwrap_err();
    assert_eq!(
        e.message(),
        Some("Failed to generate an acceptable random number in 50 attempts")
    );
}

#[test]
fn enum_cases_group_in_array_unique() {
    struct EnumCtx(CollectingContext);
    impl php_runtime::RuntimeContext for EnumCtx {
        fn diagnostic(&mut self, l: php_runtime::ErrorLevel, m: &str) -> RtResult<()> {
            self.0.diagnostic(l, m)
        }
    }
    impl ArrayContext for EnumCtx {
        fn is_enum(&self, _: &Obj) -> bool {
            true
        }
    }
    let a = Obj::new(TestObj);
    let b = Obj::new(TestObj);
    let input = list([
        Value::Object(a.clone()),
        Value::Object(b.clone()),
        Value::Object(a),
        Value::Object(b.clone()),
    ]);
    let mut ctx = EnumCtx(CollectingContext::new());
    let out = setops::array_unique(&mut ctx, &input, SORT_REGULAR).unwrap();
    assert_eq!(keys(&out), ["0", "1"]);
}

#[test]
fn user_compare_cb_type() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[2, 1]));
    let mut cb = cmp_cb();
    sort::uasort(&mut ctx, &r, &mut cb).unwrap();
    assert_eq!(keys(&arr_of(&r.get())), ["1", "0"]);
}

// ---------------------------------------------------------------------------
// php-src regression tests (ext/standard/tests/array)
// ---------------------------------------------------------------------------

/// bug69068: replacing the walked array restarts at its internal pointer.
#[test]
fn walk_follows_replaced_array() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(ints(&[1, 2, 3]));
    let target = r.clone();
    let mut seen = Vec::new();
    let mut cb = |_: &mut Ctx, a: &[Value]| {
        let v = util::deref(&a[0]).as_int().unwrap_or(0);
        seen.push(v);
        if v == 2 {
            target.set(ints(&[4, 5]));
        }
        Ok(Value::Null)
    };
    callback::array_walk(&mut ctx, &r, &mut cb, None).unwrap();
    assert_eq!(seen, [1, 2, 4, 5]);
    assert_eq!(int_vals(&arr_of(&r.get())), [4, 5]);
}

/// bug61967: unsetting elements of the outer array during the walk.
#[test]
fn walk_recursive_with_outer_unset() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(Value::Array(list((0..5).map(|_| ints(&[1, 2, 3, 4, 5])))));
    let target = r.clone();
    let mut seen = Vec::new();
    let mut cb = |_: &mut Ctx, a: &[Value]| {
        let k = a[1].as_int().unwrap_or(-1);
        seen.push(k);
        if let Value::Array(t) = &mut *target.borrow_mut() {
            t.make_mut().remove(&ArrayKey::Int(k));
        }
        Ok(Value::Null)
    };
    callback::array_walk_recursive(&mut ctx, &r, &mut cb, None).unwrap();
    assert_eq!(seen, [0, 1, 2, 3, 4]);
}

/// gh16957 / gh17447: shifting/popping a self-referencing array returns
/// the array as it is after the operation.
#[test]
fn shift_and_pop_self_reference() {
    let r = PhpRef::new(Value::Null);
    r.set(Value::Array(list([
        Value::Reference(r.clone()),
        Value::Int(1),
        Value::from("two"),
    ])));
    let shifted = stack::array_shift(&r).unwrap();
    let after = arr_of(&r.get());
    assert_eq!(keys(&after), ["0", "1"]);
    assert!(php_runtime::ops::is_identical(&shifted, &r.get()).unwrap());

    let p = PhpRef::new(Value::Null);
    p.set(Value::Array(list([Value::Reference(p.clone())])));
    let popped = stack::array_pop(&p).unwrap();
    assert!(arr_of(&popped).is_empty());
    assert!(arr_of(&p.get()).is_empty());
}

/// gh18480: extreme offsets and lengths.
#[test]
fn splice_extreme_arguments() {
    let mut ctx = Ctx::new();
    let mut lens = Vec::new();
    for length in [i64::MIN, i64::MAX] {
        for offset in [i64::MAX, i64::MIN] {
            let r = PhpRef::new(ints(&[i64::MAX]));
            let removed = stack::array_splice(&mut ctx, &r, offset, Some(length), None).unwrap();
            lens.push(removed.len());
        }
    }
    assert_eq!(lens, [0, 0, 0, 1]);
}

/// gh19926: the internal pointer after `array_splice`.
#[test]
fn splice_resets_internal_pointer() {
    let mut ctx = Ctx::new();
    let mut t = HashTable::new();
    for i in 1..=3 {
        t.push(Value::Int(i)).unwrap();
    }
    t.remove(&ArrayKey::Int(0));
    let r = PhpRef::new(Value::Array(ZArray::from_table(t)));
    assert_eq!(pointer::next(&mut ctx, &r).unwrap().as_int(), Some(3));
    let repl = ints(&[999]);
    stack::array_splice(&mut ctx, &r, 0, Some(0), Some(&repl)).unwrap();
    assert_eq!(
        pointer::current(&mut ctx, "current", &r.get())
            .unwrap()
            .as_int(),
        Some(999)
    );
}

/// gh23576: the next free key of `array_keys()` results.
#[test]
fn array_keys_next_index() {
    let mut ctx = Ctx::new();
    let mut t = HashTable::new();
    t.insert(123, Value::Int(123));
    t.remove(&ArrayKey::Int(123));
    for filter in [None, Some(&Value::Int(123))] {
        let mut k = basic::array_keys(&mut ctx, &t, filter, false).unwrap();
        assert_eq!(k.make_mut().push(Value::Int(42)).unwrap(), 0);
    }
}

/// gh9244: multisort followed by `array_shift`.
#[test]
fn multisort_then_shift() {
    let mut ctx = Ctx::new();
    let items = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("foo", Value::Int(1)),
        ("bar", Value::Int(2)),
    ])));
    let order = PhpRef::new(ints(&[4, 3]));
    sort::array_multisort(
        &mut ctx,
        &[Value::Reference(order), Value::Reference(items.clone())],
    )
    .unwrap();
    assert_eq!(stack::array_shift(&items).unwrap().as_int(), Some(2));
}

/// The frameless two-argument `min`/`max` break ties differently from the
/// variadic forms.
#[test]
fn frameless_min_max() {
    let mut ctx = Ctx::new();
    let (i, f) = (Value::Int(1), Value::Float(1.0));
    assert!(matches!(
        basic::min2(&mut ctx, &i, &f).unwrap(),
        Value::Float(_)
    ));
    assert!(matches!(
        basic::min(&mut ctx, &[i.clone(), f.clone()]).unwrap(),
        Value::Int(_)
    ));
    assert!(matches!(
        basic::max2(&mut ctx, &i, &f).unwrap(),
        Value::Int(_)
    ));
    assert!(matches!(
        basic::max2(&mut ctx, &f, &i).unwrap(),
        Value::Float(_)
    ));
    let a = Value::from("a");
    let b = Value::from("b");
    assert_eq!(
        basic::min2(&mut ctx, &b, &a)
            .unwrap()
            .as_str()
            .map(ZStr::as_bytes),
        Some(&b"a"[..])
    );
    assert_eq!(
        basic::max2(&mut ctx, &b, &a)
            .unwrap()
            .as_str()
            .map(ZStr::as_bytes),
        Some(&b"b"[..])
    );
}

#[test]
fn clamp_mixed_values_and_nan_bounds() {
    let mut ctx = Ctx::new();
    let min = Value::Float(1.3);
    let max = Value::Float(3.4);
    assert!(matches!(
        basic::clamp(&mut ctx, &Value::Int(2), &min, &max).unwrap(),
        Value::Int(2)
    ));
    assert!(php_runtime::ops::is_identical(
        &basic::clamp(&mut ctx, &Value::Int(0), &min, &max).unwrap(),
        &min
    )
    .unwrap());
    assert!(matches!(
        basic::clamp(&mut ctx, &Value::Float(f64::NAN), &min, &max).unwrap(),
        Value::Float(f) if f.is_nan()
    ));
    assert_eq!(
        basic::clamp(&mut ctx, &Value::Null, &Value::Int(-1), &Value::Int(1))
            .unwrap()
            .as_int(),
        Some(-1)
    );
    let same = Value::Object(Obj::new(TestObj));
    assert!(php_runtime::ops::is_identical(
        &basic::clamp(&mut ctx, &same, &same, &same).unwrap(),
        &same
    )
    .unwrap());
    let r = PhpRef::new(Value::from("d"));
    assert!(matches!(
        basic::clamp(&mut ctx, &Value::Reference(r), &Value::from("c"), &Value::from("g")).unwrap(),
        Value::Str(s) if s.as_bytes() == b"d"
    ));
    for (min, max, message) in [
        (
            f64::NAN,
            f64::NAN,
            "clamp(): Argument #2 ($min) must not be NAN",
        ),
        (1.0, f64::NAN, "clamp(): Argument #3 ($max) must not be NAN"),
        (
            3.0,
            2.0,
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)",
        ),
    ] {
        let err = basic::clamp(
            &mut ctx,
            &Value::Int(2),
            &Value::Float(min),
            &Value::Float(max),
        )
        .unwrap_err();
        assert_eq!(err.message(), Some(message));
    }
}

/// gh16053: merging into an object entry converts it with `(array)`.
#[test]
fn merge_recursive_into_object_entry() {
    let mut ctx = Ctx::new();
    let a = Value::Array(ZArray::from_pairs([(
        "string",
        Value::Object(Obj::new(TestObj)),
    )]));
    let b = Value::Array(ZArray::from_pairs([("string", Value::from("hello"))]));
    let out = merge::array_merge_recursive(&mut ctx, &[a, b]).unwrap();
    let inner = arr_of(out.get(&ArrayKey::from("string")).unwrap());
    assert_eq!(keys(&inner), ["0"]);
}

/// gh15982 / gh20043: `array_find` dereferences; `array_unique` reports
/// conversion errors raised while sorting.
#[test]
fn find_derefs_and_unique_propagates_errors() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(Value::from("hello"));
    let t = list([Value::Reference(r.clone()), Value::Reference(r)]);
    let mut yes = |_: &mut Ctx, _: &[Value]| Ok(Value::Bool(true));
    let v = callback::array_find(&mut ctx, &t, &mut yes).unwrap();
    assert!(matches!(v, Value::Str(_)));
    let objs = list([
        Value::Object(Obj::new(TestObj)),
        Value::Object(Obj::new(TestObj)),
    ]);
    let e = setops::array_unique(&mut ctx, &objs, SORT_STRING | SORT_FLAG_CASE).unwrap_err();
    assert_eq!(
        e.message(),
        Some("Object of class Foo could not be converted to string")
    );
}

// ---------------------------------------------------------------------------
// Sorting keeps the variable bound; callbacks get their arguments by value;
// array_walk() on objects
// ---------------------------------------------------------------------------

/// `natcasesort_variation7`: a self-referencing element still sees the array
/// while it is sorted (it compares as "Array").
#[test]
fn sort_self_referencing_array_in_place() {
    let mut ctx = Ctx::new();
    let r = PhpRef::new(Value::Array(list([
        Value::Int(1),
        Value::Float(3.0),
        Value::from("zero"),
        Value::from("2"),
    ])));
    let me = Value::Reference(r.clone());
    if let Value::Array(a) = &mut *r.borrow_mut() {
        a.make_mut().push(me).unwrap();
    }
    sort::natcasesort(&mut ctx, &r).unwrap();
    let out = arr_of(&r.get());
    assert_eq!(keys(&out), ["0", "3", "1", "4", "2"]);
    assert!(
        matches!(out.get(&ArrayKey::Int(4)), Some(Value::Reference(x)) if PhpRef::ptr_eq(x, &r))
    );
}

/// gh16648: user code writing to the variable during the sort separates it;
/// the sorted table is discarded.
#[test]
fn sort_discards_result_when_variable_is_written() {
    struct Ctx2 {
        inner: CollectingContext,
        target: PhpRef,
    }
    impl php_runtime::RuntimeContext for Ctx2 {
        fn diagnostic(&mut self, l: php_runtime::ErrorLevel, m: &str) -> RtResult<()> {
            self.inner.diagnostic(l, m)
        }
        fn cast_object(&mut self, _: &Obj, _: php_runtime::CastTarget) -> RtResult<Option<Value>> {
            if let Value::Array(a) = &mut *self.target.borrow_mut() {
                for i in 0..4 {
                    a.make_mut().insert(i, Value::Int(i));
                }
            }
            Ok(Some(Value::from("3")))
        }
    }
    impl ArrayContext for Ctx2 {}
    let r = PhpRef::new(Value::Array(ZArray::from_pairs([
        ("a", Value::from("1")),
        ("3", Value::Object(Obj::new(TestObj))),
        ("2", Value::from("2")),
    ])));
    let mut ctx = Ctx2 {
        inner: CollectingContext::new(),
        target: r.clone(),
    };
    sort::asort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(keys(&arr_of(&r.get())), ["'a'", "3", "2", "0", "1"]);
}

/// `array_reduce_accumulator_refcount`: with by-value arguments the moved
/// accumulator has no other holder.
#[test]
fn callbacks_receive_arguments_by_value() {
    let mut ctx = Ctx::new();
    let mut refcounts = Vec::new();
    let mut cb = OwnedArgs(|_: &mut Ctx, mut args: Vec<Value>| {
        let val = args.pop().unwrap_or_default();
        let mut acc = match args.pop() {
            Some(Value::Array(a)) => a,
            _ => ZArray::new(),
        };
        refcounts.push(acc.refcount());
        acc.make_mut().push(val).unwrap();
        Ok(Value::Array(acc))
    });
    let input = arr_of(&ints(&[1, 2, 3]));
    let out = callback::array_reduce(&mut ctx, &input, &mut cb, Some(&ints(&[0]))).unwrap();
    assert_eq!(int_vals(&arr_of(&out)), [0, 1, 2, 3]);
    // The initial value is still held by the caller; later ones are not.
    assert_eq!(refcounts, [2, 1, 1]);
}

/// `array_walk()` on an object goes through the property hooks: bound slots
/// are modified in place, detached ones are written back.
#[test]
fn walk_object_properties() {
    struct ObjCtx {
        inner: CollectingContext,
        props: ZArray,
        writes: Vec<(String, Value)>,
        purposes: Vec<php_runtime::PropertyPurpose>,
    }
    impl php_runtime::RuntimeContext for ObjCtx {
        fn diagnostic(&mut self, l: php_runtime::ErrorLevel, m: &str) -> RtResult<()> {
            self.inner.diagnostic(l, m)
        }
        fn object_properties(
            &mut self,
            _: &Obj,
            purpose: php_runtime::PropertyPurpose,
        ) -> RtResult<Option<ZArray>> {
            self.purposes.push(purpose);
            Ok(Some(self.props.clone()))
        }
        fn property_write(&mut self, _: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
            self.writes.push((name.to_string_lossy(), value.clone()));
            self.props
                .make_mut()
                .insert(ArrayKey::from_zstr(name), value);
            Ok(())
        }
    }
    impl ArrayContext for ObjCtx {}
    let bound = PhpRef::new(Value::Int(1));
    let mut ctx = ObjCtx {
        inner: CollectingContext::new(),
        props: ZArray::from_pairs([
            ("a", Value::Reference(bound.clone())),
            ("\0Foo\0b", Value::Int(2)),
            ("c", Value::Int(3)),
        ]),
        writes: Vec::new(),
        purposes: Vec::new(),
    };
    let obj = PhpRef::new(Value::Object(Obj::new(TestObj)));
    let mut cb = |c: &mut ObjCtx, a: &[Value]| {
        if let Value::Reference(r) = &a[0] {
            if a[1].as_str().map(ZStr::as_bytes) != Some(&b"c"[..]) {
                let v = php_runtime::ops::mul(&r.get(), &Value::Int(10), c)?;
                r.set(v);
            }
        }
        Ok(Value::Null)
    };
    callback::array_walk(&mut ctx, &obj, &mut cb, None).unwrap();
    assert_eq!(bound.get().as_int(), Some(10));
    assert_eq!(ctx.writes.len(), 1);
    assert_eq!(ctx.writes[0].0, "\0Foo\0b");
    assert_eq!(ctx.writes[0].1.as_int(), Some(20));
    assert_eq!(ctx.purposes, [php_runtime::PropertyPurpose::ArrayCast; 7]);
    assert_eq!(
        ctx.inner.messages(),
        ["Deprecated: array_walk(): Passing an object for argument #1 $array to array_walk() is deprecated, call get_object_vars() first instead"]
    );
    for recursive in [false, true] {
        ctx.props = ZArray::from_pairs([("a", Value::Int(1)), ("b", Value::Int(2))]);
        ctx.writes.clear();
        let mut cb = |_: &mut ObjCtx, a: &[Value]| {
            if let Value::Reference(r) = &a[0] {
                r.set(Value::Int(99));
            }
            Err(RuntimeError::Error("callback failed".to_string()))
        };
        let err = if recursive {
            callback::array_walk_recursive(&mut ctx, &obj, &mut cb, None)
        } else {
            callback::array_walk(&mut ctx, &obj, &mut cb, None)
        }
        .unwrap_err();
        assert_eq!(err.message(), Some("callback failed"));
        assert_eq!(ctx.writes.len(), 1);
        assert_eq!(ctx.writes[0].0, "a");
        assert_eq!(ctx.writes[0].1.as_int(), Some(99));
        assert_eq!(
            ctx.props.get(&ArrayKey::from("b")).unwrap().as_int(),
            Some(2)
        );
    }
}

/// `array_walk()` prepares a lazy object once, then walks only its live
/// `ArrayCast` table, including callback additions and excluding removals.
#[test]
fn walk_object_initializes_before_walking_cast_table() {
    use php_runtime::PropertyPurpose;
    struct LazyCtx {
        inner: CollectingContext,
        preparations: usize,
        slots: Vec<(ArrayKey, PhpRef)>,
        purposes: Vec<PropertyPurpose>,
        bound: Vec<ArrayKey>,
    }
    impl php_runtime::RuntimeContext for LazyCtx {
        fn diagnostic(&mut self, l: php_runtime::ErrorLevel, m: &str) -> RtResult<()> {
            self.inner.diagnostic(l, m)
        }
        fn object_properties(
            &mut self,
            _: &Obj,
            purpose: PropertyPurpose,
        ) -> RtResult<Option<ZArray>> {
            self.purposes.push(purpose);
            assert_eq!(purpose, PropertyPurpose::ArrayCast);
            assert_eq!(self.preparations, 1);
            let mut t = HashTable::new();
            for (k, r) in &self.slots {
                t.insert(k.clone(), r.get());
            }
            Ok(Some(ZArray::from_table(t)))
        }
    }
    impl ArrayContext for LazyCtx {
        fn prepare_object_walk(&mut self, _: &Obj) -> RtResult<()> {
            assert!(self.purposes.is_empty());
            self.preparations += 1;
            Ok(())
        }

        fn property_reference(&mut self, _: &Obj, key: &ArrayKey) -> RtResult<Option<PropertyRef>> {
            self.bound.push(key.clone());
            Ok(self
                .slots
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, r)| PropertyRef::Bound(r.clone())))
        }
    }
    for mutate in [false, true] {
        let a = PhpRef::new(Value::Int(1));
        let b = PhpRef::new(Value::Int(2));
        let removed = PhpRef::new(Value::Int(3));
        let added = PhpRef::new(Value::Int(4));
        let mut ctx = LazyCtx {
            inner: CollectingContext::new(),
            preparations: 0,
            slots: vec![
                (ArrayKey::from("a"), a.clone()),
                (ArrayKey::from("\0C\0b"), b.clone()),
            ],
            purposes: Vec::new(),
            bound: Vec::new(),
        };
        if mutate {
            ctx.slots.push((ArrayKey::from("removed"), removed.clone()));
        }
        let obj = PhpRef::new(Value::Object(Obj::new(TestObj)));
        let mut seen = Vec::new();
        let mut cb = |c: &mut LazyCtx, args: &[Value]| {
            seen.push(args[1].as_str().unwrap().to_string_lossy());
            if mutate && args[1].as_str().map(ZStr::as_bytes) == Some(&b"a"[..]) {
                c.slots.retain(|(k, _)| k != &ArrayKey::from("removed"));
                c.slots.push((ArrayKey::from("added"), added.clone()));
            }
            if let Value::Reference(r) = &args[0] {
                let v = php_runtime::ops::mul(&r.get(), &Value::Int(10), c)?;
                r.set(v);
            }
            Ok(Value::Null)
        };
        callback::array_walk(&mut ctx, &obj, &mut cb, None).unwrap();
        assert_eq!(ctx.preparations, 1);
        assert_eq!(a.get().as_int(), Some(10));
        assert_eq!(b.get().as_int(), Some(20));
        assert_eq!(removed.get().as_int(), Some(3));
        assert_eq!(added.get().as_int(), Some(if mutate { 40 } else { 4 }));
        let mut expected = vec![ArrayKey::from("a"), ArrayKey::from("\0C\0b")];
        if mutate {
            expected.push(ArrayKey::from("added"));
        }
        assert_eq!(ctx.bound, expected);
        assert_eq!(
            seen,
            expected
                .iter()
                .map(ArrayKey::to_zstr)
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ctx.purposes,
            vec![PropertyPurpose::ArrayCast; expected.len() + 1]
        );
    }
}

/// php-src sorts in place with the hash index reset
/// (`zend_hash_sort_internal`): a comparison reaching the array through its
/// own reference sees a table in which every key lookup misses, so
/// `['m' => ['m' => 0, 's' => 0], 's' => &$a]` compares `m` against it as
/// uncomparable (1). Expected orders from php 8.3 (`Zend/tests/bug63882_2.phpt`).
#[test]
fn self_referencing_array_sorts_in_place() {
    for (reverse, expected) in [(false, ["'s'", "'m'"]), (true, ["'m'", "'s'"])] {
        let mut ctx = Ctx::new();
        let r = PhpRef::new(Value::Array(ZArray::from_pairs([(
            "m",
            Value::Array(ZArray::from_pairs([
                ("m", Value::Int(0)),
                ("s", Value::Int(0)),
            ])),
        )])));
        let me = Value::Reference(r.clone());
        if let Value::Array(a) = &mut *r.borrow_mut() {
            a.make_mut().insert("s", me);
        }
        if reverse {
            sort::arsort(&mut ctx, &r, SORT_REGULAR).unwrap();
        } else {
            sort::asort(&mut ctx, &r, SORT_REGULAR).unwrap();
        }
        let out = arr_of(&r.get());
        assert_eq!(keys(&out), expected, "reverse={reverse}");
        // The sorted table is what the reference holds, its element is still
        // that reference, and its index works again.
        assert!(matches!(
            out.get(&ArrayKey::from("s")),
            Some(Value::Reference(x)) if PhpRef::ptr_eq(x, &r)
        ));
        assert!(ctx.messages().is_empty());
    }
}

/// The reference may sit deeper than the top level: comparing
/// `['k' => ['m' => 0, 'n' => 0]]` with `['k' => &$a]` looks `m` up in the
/// live table being sorted, misses, and finds the arrays uncomparable (1).
/// Expected orders from php 8.3.
#[test]
fn nested_reference_sees_the_live_table() {
    let cases = [
        (false, false, ["'n'", "'m'"]),
        (false, true, ["'m'", "'n'"]),
        (true, false, ["'m'", "'n'"]),
        (true, true, ["'n'", "'m'"]),
    ];
    for (reverse, n_first, expected) in cases {
        let mut ctx = Ctx::new();
        let r = PhpRef::new(Value::Array(ZArray::new()));
        let m = Value::Array(ZArray::from_pairs([("k", Value::Reference(r.clone()))]));
        let inner = ZArray::from_pairs([("m", Value::Int(0)), ("n", Value::Int(0))]);
        let n = Value::Array(ZArray::from_pairs([("k", Value::Array(inner))]));
        let pairs = if n_first {
            [("n", n), ("m", m)]
        } else {
            [("m", m), ("n", n)]
        };
        r.set(Value::Array(ZArray::from_pairs(pairs)));
        if reverse {
            sort::arsort(&mut ctx, &r, SORT_REGULAR).unwrap();
        } else {
            sort::asort(&mut ctx, &r, SORT_REGULAR).unwrap();
        }
        let out = arr_of(&r.get());
        assert_eq!(keys(&out), expected, "reverse={reverse} n_first={n_first}");
    }
}

/// Without objects, arrays or references nothing can observe the sort; a
/// shared array is still separated (`Z_PARAM_ARRAY_HT_EX(array, 0, 1)`).
#[test]
fn scalar_sort_separates_a_shared_array() {
    let mut ctx = Ctx::new();
    let shared = list([Value::Int(3), Value::Int(1), Value::Int(2)]);
    let r = PhpRef::new(Value::Array(shared.clone()));
    sort::sort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(int_vals(&arr_of(&r.get())), [1, 2, 3]);
    assert_eq!(int_vals(&shared), [3, 1, 2]);
    let r = PhpRef::new(Value::Int(1));
    assert!(sort::asort(&mut ctx, &r, SORT_REGULAR).is_err());
}

/// Int pairs compare inline in `sort_table` (`zend_compare`'s long/long
/// case); ties keep their original order, `arsort`/`rsort` reverse the
/// comparison but not the tie-break, and mixed values take the generic
/// comparison. Expected orders from `/usr/bin/php8.5 -n`.
#[test]
fn int_sorts_compare_inline_and_stay_stable() {
    let mut ctx = Ctx::new();
    let v = [
        3,
        10,
        -7,
        3,
        0,
        9,
        3,
        1,
        -7,
        i64::MAX,
        i64::MIN,
        2,
        3,
        8,
        6,
        5,
        4,
        11,
        12,
        3,
    ];
    let key_list = |s: &str| s.split(',').map(str::to_owned).collect::<Vec<_>>();
    let r = PhpRef::new(ints(&v));
    sort::asort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(
        keys(&arr_of(&r.get())),
        key_list("10,2,8,4,7,11,0,3,6,12,19,16,15,14,13,5,1,17,18,9")
    );
    let r = PhpRef::new(ints(&v));
    sort::arsort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(
        keys(&arr_of(&r.get())),
        key_list("9,18,17,1,5,13,14,15,16,0,3,6,12,19,11,7,4,2,8,10")
    );
    let r = PhpRef::new(ints(&v));
    sort::rsort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(
        int_vals(&arr_of(&r.get())),
        [
            i64::MAX,
            12,
            11,
            10,
            9,
            8,
            6,
            5,
            4,
            3,
            3,
            3,
            3,
            3,
            2,
            1,
            0,
            -7,
            -7,
            i64::MIN
        ]
    );
    let mixed = Value::Array(list([
        Value::Int(3),
        Value::string("10"),
        Value::Float(2.5),
        Value::Int(1),
        Value::Int(3),
        Value::string("abc"),
        Value::Int(-7),
        Value::Int(0),
    ]));
    let r = PhpRef::new(mixed);
    sort::asort(&mut ctx, &r, SORT_REGULAR).unwrap();
    assert_eq!(keys(&arr_of(&r.get())), key_list("6,7,3,2,0,4,1,5"));
}

/// The array functions call [`ArrayCallback::call_slice`] (no per-call
/// argument vector) and hand the arguments over by value: an
/// implementation can move them out, leaving `null`.
#[test]
fn callbacks_use_call_slice() {
    struct SliceCb {
        arities: Vec<usize>,
    }
    impl ArrayCallback<Ctx> for SliceCb {
        fn call(&mut self, _: &mut Ctx, _: Vec<Value>) -> RtResult<Value> {
            panic!("the array functions call call_slice");
        }
        fn call_slice(&mut self, _: &mut Ctx, args: &mut [Value]) -> RtResult<Value> {
            self.arities.push(args.len());
            let first = std::mem::take(&mut args[0]);
            let second = args.get_mut(1).map(std::mem::take);
            Ok(match (first, second) {
                (Value::Int(a), Some(Value::Int(b))) => Value::Int(a - b),
                (Value::Int(a), _) => Value::Int(a * 10),
                (_, Some(Value::Int(b))) => Value::Int(-b),
                _ => Value::Null,
            })
        }
    }
    let mut ctx = Ctx::new();
    let mut cb = SliceCb {
        arities: Vec::new(),
    };
    // One array: one argument per call.
    let out = callback::array_map(&mut ctx, Some(&mut cb), &[ints(&[1, 2])]).unwrap();
    assert_eq!(int_vals(&out), [10, 20]);
    // Two arrays, the shorter padded with null; the reused buffer is
    // refilled for every call.
    let out =
        callback::array_map(&mut ctx, Some(&mut cb), &[ints(&[5, 7, 9]), ints(&[1, 2])]).unwrap();
    assert_eq!(int_vals(&out), [4, 5, 90]);
    // Without a callback, the same buffer builds the zipped rows.
    let out = callback::array_map::<Ctx>(&mut ctx, None, &[ints(&[1, 2]), ints(&[3])]).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(arr_of(out.get(&ArrayKey::Int(1)).unwrap()).len(), 2);
    // array_filter (USE_BOTH) and the user comparator take two arguments.
    let input = arr_of(&ints(&[3, 1, 5]));
    let kept =
        callback::array_filter(&mut ctx, &input, Some(&mut cb), ARRAY_FILTER_USE_BOTH).unwrap();
    assert_eq!(int_vals(&kept), [3, 5]);
    let r = PhpRef::new(ints(&[3, 1, 2]));
    sort::usort(&mut ctx, &r, &mut cb).unwrap();
    assert_eq!(int_vals(&arr_of(&r.get())), [1, 2, 3]);
    assert!(cb.arities[..2].iter().all(|&n| n == 1));
    assert!(cb.arities[2..].iter().all(|&n| n == 2));
}
