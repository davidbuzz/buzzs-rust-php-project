// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use php_runtime::{
    ops, ArrayKey, CastTarget, CollectingContext, ErrorLevel, Obj, PhpRef, PlainObject, Resource,
    RtResult, RuntimeContext, RuntimeError, Value, ZArray,
};

fn array(items: &[(ArrayKey, Value)]) -> Value {
    Value::Array(ZArray::from_pairs(items.iter().cloned()))
}

fn object(class: &str) -> Value {
    Value::Object(PlainObject::create(class, ZArray::new()))
}

fn assert_comparison(a: &Value, b: &Value, expected: i32) {
    let mut ctx = CollectingContext::new();
    assert_eq!(
        ops::compare(a, b, &mut ctx).unwrap(),
        expected,
        "{a:?}, {b:?}"
    );
    assert_eq!(ops::is_equal(a, b, &mut ctx).unwrap(), expected == 0);
    assert_eq!(ops::is_not_equal(a, b, &mut ctx).unwrap(), expected != 0);
    assert_eq!(ops::is_smaller(a, b, &mut ctx).unwrap(), expected < 0);
    assert_eq!(
        ops::is_smaller_or_equal(a, b, &mut ctx).unwrap(),
        expected <= 0
    );
    assert_eq!(
        ops::spaceship(a, b, &mut ctx).unwrap().as_int(),
        Some(i64::from(expected))
    );
    assert!(ctx.diagnostics.is_empty());
}

#[derive(Default)]
struct HookContext {
    diagnostics: CollectingContext,
    casts: Vec<CastTarget>,
    cast: Option<(CastTarget, Value)>,
    comparison: Option<i32>,
    throw_cast: bool,
    throw_diagnostic: bool,
}

impl RuntimeContext for HookContext {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.diagnostics.diagnostic(level, message)?;
        if self.throw_diagnostic {
            return Err(RuntimeError::Error("diagnostic handler threw".into()));
        }
        Ok(())
    }

    fn cast_object(&mut self, _obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        self.casts.push(target);
        if self.throw_cast {
            return Err(RuntimeError::Error("cast handler threw".into()));
        }
        Ok(self
            .cast
            .as_ref()
            .and_then(|(t, v)| (*t == target).then(|| v.clone())))
    }

    fn compare_objects(&mut self, a: &Value, b: &Value) -> RtResult<i32> {
        if let Some(result) = self.comparison {
            return Ok(result);
        }
        ops::std_compare_objects(self, a, b, |_, _, _| Ok(1))
    }
}

#[test]
fn arrays_outrank_numbers_strings_and_resources_without_diagnostics() {
    let scalars = [
        Value::Int(i64::MIN),
        Value::Int(0),
        Value::Int(i64::MAX),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(0.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NAN),
        Value::from(""),
        Value::from("0"),
        Value::from("123"),
        Value::from("1abc"),
        Value::from("string"),
        Value::Resource(Resource::with_handle(3, "stream", None)),
    ];
    for a in [Value::empty_array(), array(&[(0.into(), Value::Int(1))])] {
        for b in &scalars {
            assert_comparison(&a, b, 1);
            assert_comparison(b, &a, -1);
        }
    }
}

#[test]
fn null_and_boolean_comparisons_use_truthiness_not_array_rank() {
    for (a, false_result, true_result) in [
        (Value::empty_array(), 0, -1),
        (array(&[(0.into(), Value::Int(0))]), 1, 0),
        (Value::Int(0), 0, -1),
        (Value::from("0"), 0, -1),
        (Value::Float(f64::NAN), 1, 0),
    ] {
        assert_comparison(&a, &Value::Bool(false), false_result);
        assert_comparison(&Value::Bool(false), &a, -false_result);
        let null_result = if matches!(a, Value::Str(_)) {
            1
        } else {
            false_result
        };
        assert_comparison(&a, &Value::Null, null_result);
        assert_comparison(&Value::Null, &a, -null_result);
        assert_comparison(&a, &Value::Bool(true), true_result);
        assert_comparison(&Value::Bool(true), &a, -true_result);
    }
    assert_comparison(&Value::Null, &Value::from("0"), -1);
    assert_comparison(&Value::from("0"), &Value::Null, 1);
    assert_comparison(&Value::Null, &Value::from(""), 0);
}

#[test]
fn numeric_strings_and_integer_boundaries() {
    for (a, b, expected) in [
        (Value::from(" 2.5 "), Value::Float(2.5), 0),
        (Value::from("1e3"), Value::Int(1000), 0),
        (Value::from("1abc"), Value::Int(1), 1),
        (Value::from("abc"), Value::Int(0), 1),
        (Value::Int(i64::MIN), Value::from("-9223372036854775808"), 0),
        (Value::Int(i64::MAX), Value::from("9223372036854775808"), 0),
        (
            Value::from("9223372036854775808"),
            Value::from("9223372036854775809"),
            -1,
        ),
        (
            Value::from("-9223372036854775809"),
            Value::from("-9223372036854775810"),
            -1,
        ),
        (
            Value::from("9007199254740992"),
            Value::from("9007199254740993"),
            -1,
        ),
        (Value::from("1e400"), Value::from("2e400"), -1),
        (Value::from("1e400"), Value::from("10e399"), 1),
        (Value::Float(f64::INFINITY), Value::from("1e400"), 0),
        (Value::Float(f64::NEG_INFINITY), Value::from("-1e400"), 0),
    ] {
        assert_comparison(&a, &b, expected);
        assert_comparison(&b, &a, -expected);
    }
}

#[test]
fn nan_is_uncomparable_with_numbers_and_strings_in_both_directions() {
    let nan = Value::Float(f64::NAN);
    for other in [
        Value::Int(0),
        Value::Float(0.0),
        nan.clone(),
        Value::Float(f64::INFINITY),
        Value::from(""),
        Value::from("NAN"),
        Value::from("0"),
    ] {
        assert_comparison(&nan, &other, 1);
        assert_comparison(&other, &nan, 1);
        assert!(!ops::is_identical(&nan, &other).unwrap());
    }
}

#[test]
fn arrays_compare_counts_then_keys_then_values() {
    let a = array(&[("a".into(), Value::Int(1)), ("b".into(), Value::Int(2))]);
    let reordered = array(&[("b".into(), Value::Int(2)), ("a".into(), Value::Int(1))]);
    assert_comparison(&a, &reordered, 0);
    assert!(!ops::is_identical(&a, &reordered).unwrap());
    let fewer = array(&[("missing".into(), Value::Int(100))]);
    assert_comparison(&a, &fewer, 1);
    assert_comparison(&fewer, &a, -1);
    let different_keys = array(&[("a".into(), Value::Int(1)), ("c".into(), Value::Int(2))]);
    assert_comparison(&a, &different_keys, 1);
    assert_comparison(&different_keys, &a, 1);
    let larger = array(&[("a".into(), Value::Int(2)), ("b".into(), Value::Int(0))]);
    assert_comparison(&a, &larger, -1);
    assert_comparison(&larger, &a, 1);
    let numeric_string = array(&[("a".into(), Value::from("1")), ("b".into(), Value::Int(2))]);
    assert_comparison(&a, &numeric_string, 0);
    assert!(!ops::is_identical(&a, &numeric_string).unwrap());
    let nested_nan = array(&[(0.into(), Value::Float(f64::NAN))]);
    let nested_number = array(&[(0.into(), Value::Int(0))]);
    assert_comparison(&nested_nan, &nested_number, 1);
    assert_comparison(&nested_number, &nested_nan, 1);
    assert_comparison(&nested_nan, &nested_nan, 0);
}

#[test]
fn default_objects_outrank_arrays_and_resources() {
    for o in [object("stdClass"), object("test")] {
        for other in [
            Value::empty_array(),
            array(&[(0.into(), Value::Int(1))]),
            Value::Resource(Resource::with_handle(1, "stream", None)),
        ] {
            assert_comparison(&o, &other, 1);
            assert_comparison(&other, &o, -1);
        }
        assert_comparison(&o, &Value::Null, 1);
        assert_comparison(&Value::Null, &o, -1);
        assert_comparison(&o, &Value::Bool(false), 1);
        assert_comparison(&Value::Bool(false), &o, -1);
        assert_comparison(&o, &Value::Bool(true), 0);
        assert_comparison(&Value::Bool(true), &o, 0);
        assert_comparison(&o, &o, 0);
        assert!(ops::is_identical(&o, &o).unwrap());
    }
    let (a, b) = (object("stdClass"), object("test"));
    assert_comparison(&a, &b, 1);
    assert_comparison(&b, &a, 1);
    assert!(!ops::is_identical(&a, &b).unwrap());
}

#[test]
fn resource_comparisons_never_request_an_array_cast() {
    let o = object("test");
    let resource = Resource::with_handle(3, "stream", None);
    let mut ctx = HookContext {
        cast: Some((CastTarget::Array, Value::empty_array())),
        ..HookContext::default()
    };
    for closed in [false, true] {
        if closed {
            resource.close();
        }
        let r = Value::Resource(resource.clone());
        assert_eq!(ops::compare(&o, &r, &mut ctx).unwrap(), 1);
        assert_eq!(ops::compare(&r, &o, &mut ctx).unwrap(), -1);
    }
    assert!(ctx.casts.is_empty());
    assert!(ctx.diagnostics.diagnostics.is_empty());
    ctx.comparison = Some(1);
    assert_eq!(
        ops::compare(&Value::Resource(resource), &o, &mut ctx).unwrap(),
        1
    );
}

#[test]
fn numeric_object_cast_failure_emits_exact_notice_in_either_direction() {
    let o = object("test");
    for (other, target, expected) in [
        (Value::Int(0), "int", 1),
        (Value::Int(1), "int", 0),
        (Value::Int(2), "int", -1),
        (Value::Float(1.0), "float", 0),
        (Value::Float(2.5), "float", -1),
        (Value::Float(f64::NAN), "float", 1),
    ] {
        for (a, b, result) in [
            (&o, &other, expected),
            (
                &other,
                &o,
                if matches!(&other, Value::Float(f) if f.is_nan()) {
                    1
                } else {
                    -expected
                },
            ),
        ] {
            let mut ctx = CollectingContext::new();
            assert_eq!(ops::compare(a, b, &mut ctx).unwrap(), result);
            assert_eq!(ctx.diagnostics.len(), 1);
            assert_eq!(ctx.diagnostics[0].level, ErrorLevel::Notice);
            assert_eq!(
                ctx.diagnostics[0].message,
                format!("Object of class test could not be converted to {target}")
            );
        }
    }
}

#[test]
fn object_cast_handlers_are_honored_without_fallback_diagnostics() {
    let o = object("test");
    for (target, casted, other, expected) in [
        (CastTarget::Int, Value::Int(5), Value::Int(6), -1),
        (CastTarget::Float, Value::Float(2.5), Value::Float(2.5), 0),
        (CastTarget::String, Value::from("10"), Value::from("9"), 1),
        (CastTarget::Bool, Value::Bool(false), Value::Bool(false), 0),
        (
            CastTarget::Array,
            Value::empty_array(),
            Value::empty_array(),
            0,
        ),
    ] {
        let mut ctx = HookContext {
            cast: Some((target, casted)),
            ..HookContext::default()
        };
        assert_eq!(ops::compare(&o, &other, &mut ctx).unwrap(), expected);
        assert_eq!(ops::compare(&other, &o, &mut ctx).unwrap(), -expected);
        assert_eq!(ctx.casts, [target, target]);
        assert!(ctx.diagnostics.diagnostics.is_empty());
    }
    let mut ctx = HookContext::default();
    assert_eq!(ops::compare(&o, &Value::Bool(true), &mut ctx).unwrap(), 1);
    assert_eq!(ops::compare(&Value::Bool(true), &o, &mut ctx).unwrap(), -1);
    assert_eq!(ctx.casts, [CastTarget::Bool, CastTarget::Bool]);
}

#[test]
fn comparison_propagates_cast_and_diagnostic_exceptions() {
    let o = object("test");
    for other in [Value::Int(1), Value::from("string"), Value::Bool(true)] {
        for (a, b) in [(&o, &other), (&other, &o)] {
            let mut ctx = HookContext {
                throw_cast: true,
                ..HookContext::default()
            };
            let error = ops::compare(a, b, &mut ctx).unwrap_err();
            assert_eq!(error.message(), Some("cast handler threw"));
            assert!(ctx.diagnostics.diagnostics.is_empty());
        }
    }
    let mut ctx = HookContext {
        throw_diagnostic: true,
        ..HookContext::default()
    };
    let error = ops::compare(&o, &Value::Int(1), &mut ctx).unwrap_err();
    assert_eq!(error.message(), Some("diagnostic handler threw"));
    assert_eq!(ctx.diagnostics.diagnostics.len(), 1);
}

#[test]
fn references_preserve_array_object_and_nan_comparisons() {
    for (a, b, expected) in [
        (Value::empty_array(), Value::Int(0), 1),
        (Value::Float(f64::NAN), Value::Float(f64::NAN), 1),
        (object("test"), Value::empty_array(), 1),
        (
            object("test"),
            Value::Resource(Resource::with_handle(3, "stream", None)),
            1,
        ),
    ] {
        let ar = Value::Reference(PhpRef::new(a.clone()));
        let br = Value::Reference(PhpRef::new(b.clone()));
        assert_comparison(&ar, &b, expected);
        assert_comparison(&a, &br, expected);
        assert_comparison(&ar, &br, expected);
    }
}

fn recursive_array() -> Value {
    let reference = PhpRef::new(Value::Null);
    let value = array(&[(0.into(), Value::Reference(reference.clone()))]);
    reference.set(value.clone());
    value
}

#[test]
fn recursive_array_guard_is_released_after_failure() {
    let (a, b) = (recursive_array(), recursive_array());
    let mut ctx = CollectingContext::new();
    for _ in 0..2 {
        let error = ops::compare(&a, &b, &mut ctx).unwrap_err();
        assert_eq!(error.class_name(), Some("Error"));
        assert_eq!(
            error.message(),
            Some("Nesting level too deep - recursive dependency?")
        );
    }
    let non_recursive = array(&[(0.into(), Value::empty_array())]);
    for _ in 0..2 {
        let error = ops::is_identical(&a, &non_recursive).unwrap_err();
        assert_eq!(error.class_name(), Some("Error"));
        assert_eq!(
            error.message(),
            Some("Nesting level too deep - recursive dependency?")
        );
    }
    assert!(ops::is_identical(&a, &a).unwrap());
    assert!(!ops::is_identical(&a, &Value::empty_array()).unwrap());
    assert_comparison(&a, &a, 0);
    assert_comparison(&a, &Value::empty_array(), 1);
    for value in [a, b] {
        if let Value::Array(table) = value {
            if let Some(Value::Reference(reference)) = table.get_int(0) {
                reference.set(Value::Null);
            }
        }
    }
}
