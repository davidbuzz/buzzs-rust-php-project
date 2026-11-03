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

/// gh19926: the internal pointer after array_splice.
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

/// gh23576: the next free key of array_keys() results.
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

/// gh9244: multisort followed by array_shift.
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
