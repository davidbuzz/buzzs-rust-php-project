// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! NaN conversion values and warning multiplicity from php-src master.

use php_runtime::{CollectingContext, ErrorLevel, PhpRef, PlainObject, Value};

#[test]
fn nan_scalar_and_container_conversions_warn_once() {
    for v in [
        Value::Float(f64::NAN),
        Value::Reference(PhpRef::new(Value::Float(f64::NAN))),
    ] {
        let mut ctx = CollectingContext::new();
        assert!(v.to_double(&mut ctx).expect("float conversion").is_nan());
        assert!(ctx.take().is_empty());
        assert!(v.to_bool(&mut ctx).expect("bool conversion"));
        assert_eq!(v.to_zstr(&mut ctx).expect("string conversion"), "NAN");
        assert_eq!(v.to_long(&mut ctx).expect("int conversion"), 0);
        let array = v.to_array(&mut ctx).expect("array conversion");
        assert_eq!(array.len(), 1);
        assert!(array
            .get_int(0)
            .and_then(Value::as_float)
            .expect("array element")
            .is_nan());
        let object = v.to_object(&mut ctx).expect("object conversion");
        let props = object
            .as_object()
            .expect("object")
            .downcast_ref::<PlainObject>()
            .expect("stdClass")
            .props();
        assert_eq!(props.len(), 1);
        assert!(props
            .get_str(b"scalar")
            .and_then(Value::as_float)
            .expect("scalar property")
            .is_nan());
        assert_eq!(
            ctx.messages(),
            [
                "Warning: unexpected NAN value was coerced to bool",
                "Warning: unexpected NAN value was coerced to string",
                "Warning: The float NAN is not representable as an int, cast occurred",
                "Warning: unexpected NAN value was coerced to array",
                "Warning: unexpected NAN value was coerced to object",
            ]
        );
        assert!(ctx
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level == ErrorLevel::Warning));
    }
}

#[test]
fn non_nan_container_conversions_do_not_warn() {
    for f in [0.0, -0.0, 1.5, f64::INFINITY, f64::NEG_INFINITY] {
        let v = Value::Float(f);
        let mut ctx = CollectingContext::new();
        v.to_array(&mut ctx).expect("array conversion");
        v.to_object(&mut ctx).expect("object conversion");
        assert!(ctx.take().is_empty());
    }
}
