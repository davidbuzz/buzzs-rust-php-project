use php_runtime::{ArrayKey, Value};
use php_vm::ext::Extension;
use php_vm::{Throw, Vm, VmOptions};

fn machine() -> Vm {
    let extension = php_ext_zendtest::ZendTest;
    let extensions: [&dyn Extension; 1] = [&extension];
    Vm::with_extensions(
        &VmOptions::default(),
        Box::new(std::io::sink()),
        &extensions,
    )
}

/// A comparable view of the scalar values these tests inspect (`Value`
/// has no `PartialEq`).
#[derive(Debug, PartialEq)]
enum V {
    Int(i64),
    Bool(bool),
    Str(Vec<u8>),
    Other,
}

fn v(value: &Value) -> V {
    match value {
        Value::Int(i) => V::Int(*i),
        Value::Bool(b) => V::Bool(*b),
        Value::Str(s) => V::Str(s.as_bytes().to_vec()),
        _ => V::Other,
    }
}

fn field(vm: &Vm, name: &str) -> V {
    v(vm.serialization_state()
        .get(&ArrayKey::from(name))
        .expect("serialization state field"))
}

fn startup(vm: &mut Vm) {
    for hook in vm.startup_hooks.clone() {
        hook(vm);
    }
}

#[test]
fn bailout_abandons_actual_context_until_request_startup() {
    let mut vm = machine();
    startup(&mut vm);
    for unserialize in [false, true] {
        for locked in [false, true] {
            let result = vm.call_value(
                &Value::from("zend_test_serialization_bailout"),
                vec![Value::Bool(unserialize), Value::Bool(locked)],
            );
            assert!(matches!(result, Err(Throw::Fatal)));
            assert_eq!(field(&vm, "lock"), V::Int(i64::from(locked)));
            assert_eq!(
                field(&vm, "serialize_level"),
                V::Int(i64::from(!unserialize))
            );
            assert_eq!(field(&vm, "serialize_data"), V::Bool(!unserialize));
            assert_eq!(
                field(&vm, "unserialize_level"),
                V::Int(i64::from(unserialize))
            );
            assert_eq!(field(&vm, "unserialize_data"), V::Bool(unserialize));
            vm.shutdown(Err(Throw::Fatal));
            assert_eq!(field(&vm, "lock"), V::Int(i64::from(locked)));
            // The request-start hook clears the real contexts, not a
            // zend_test-only shadow state. Check before any PHP code runs.
            startup(&mut vm);
            assert_eq!(field(&vm, "lock"), V::Int(0));
            assert_eq!(field(&vm, "serialize_level"), V::Int(0));
            assert_eq!(field(&vm, "serialize_data"), V::Bool(false));
            assert_eq!(field(&vm, "unserialize_level"), V::Int(0));
            assert_eq!(field(&vm, "unserialize_data"), V::Bool(false));
            let value = vm
                .call_value(
                    &Value::from("unserialize"),
                    vec![Value::from("a:1:{s:6:\"nested\";i:42;}")],
                )
                .expect("unserialize on next request");
            assert!(matches!(value, Value::Array(_)));
            let result = vm.call_value(&Value::from("serialize"), vec![value]);
            assert_eq!(
                v(&result.unwrap()),
                V::Str(b"a:1:{s:6:\"nested\";i:42;}".to_vec())
            );
        }
    }
}

#[test]
fn nested_serializable_bailout_does_not_destroy_shared_context() {
    let mut vm = machine();
    vm.run_script(
        br"<?php
class Payload implements Serializable {
    function serialize(): string { zend_test_serialization_bailout(false, false); }
    function unserialize(string $data): void {}
}
serialize(new Payload);
",
        "bailout.php",
    );
    assert_eq!(field(&vm, "serialize_level"), V::Int(2));
    assert_eq!(field(&vm, "serialize_data"), V::Bool(true));
    startup(&mut vm);
    assert_eq!(field(&vm, "serialize_level"), V::Int(0));
    assert_eq!(field(&vm, "serialize_data"), V::Bool(false));
}

#[test]
fn state_function_reports_the_active_unserializer() {
    let mut vm = machine();
    startup(&mut vm);
    vm.with_unserialize_context(|vm| {
        let value = vm.call_value(&Value::from("zend_test_serialization_state"), vec![])?;
        let Value::Array(state) = value else {
            panic!("state array")
        };
        assert_eq!(state.get_str(b"lock").map(v), Some(V::Int(0)));
        assert_eq!(state.get_str(b"unserialize_level").map(v), Some(V::Int(1)));
        assert_eq!(
            state.get_str(b"unserialize_data").map(v),
            Some(V::Bool(true))
        );
        Ok(())
    })
    .expect("unserialize context");
    assert_eq!(field(&vm, "unserialize_level"), V::Int(0));
    assert_eq!(field(&vm, "unserialize_data"), V::Bool(false));
}

#[test]
fn nested_unserializer_bailout_leaves_levels_and_lock_until_next_request() {
    let mut vm = machine();
    startup(&mut vm);
    let result = vm.with_unserialize_context(|vm| {
        vm.call_value(
            &Value::from("zend_test_serialization_bailout"),
            vec![Value::Bool(true), Value::Bool(true)],
        )
    });
    assert!(matches!(result, Err(Throw::Fatal)));
    assert_eq!(field(&vm, "lock"), V::Int(1));
    assert_eq!(field(&vm, "unserialize_level"), V::Int(2));
    assert_eq!(field(&vm, "unserialize_data"), V::Bool(true));
    // A new VM on this same worker thread must not inherit the abandoned
    // context when it starts the next request.
    drop(vm);
    let mut next = machine();
    startup(&mut next);
    assert_eq!(field(&next, "lock"), V::Int(0));
    assert_eq!(field(&next, "unserialize_level"), V::Int(0));
    assert_eq!(field(&next, "unserialize_data"), V::Bool(false));
}
