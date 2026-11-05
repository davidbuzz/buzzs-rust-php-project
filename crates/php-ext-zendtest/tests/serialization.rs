use php_runtime::{ArrayKey, Value};
use php_vm::ext::Extension;
use php_vm::{Throw, Vm, VmOptions};

fn machine() -> Vm {
    let extension = php_ext_zendtest::ZendTest;
    let extensions: [&dyn Extension; 1] = [&extension];
    Vm::with_extensions(&VmOptions::default(), Box::new(std::io::sink()), &extensions)
}

fn field(vm: &Vm, name: &str) -> Value {
    vm.serialization_state()
        .get(&ArrayKey::from(name))
        .cloned()
        .expect("serialization state field")
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
            assert_eq!(field(&vm, "lock"), Value::Int(i64::from(locked)));
            assert_eq!(
                field(&vm, "serialize_level"),
                Value::Int(i64::from(!unserialize))
            );
            assert_eq!(field(&vm, "serialize_data"), Value::Bool(!unserialize));
            assert_eq!(
                field(&vm, "unserialize_level"),
                Value::Int(i64::from(unserialize))
            );
            assert_eq!(field(&vm, "unserialize_data"), Value::Bool(unserialize));
            vm.shutdown(Err(Throw::Fatal));
            assert_eq!(field(&vm, "lock"), Value::Int(i64::from(locked)));
            // The request-start hook clears the real contexts, not a
            // zend_test-only shadow state. Check before any PHP code runs.
            startup(&mut vm);
            assert_eq!(field(&vm, "lock"), Value::Int(0));
            assert_eq!(field(&vm, "serialize_level"), Value::Int(0));
            assert_eq!(field(&vm, "serialize_data"), Value::Bool(false));
            assert_eq!(field(&vm, "unserialize_level"), Value::Int(0));
            assert_eq!(field(&vm, "unserialize_data"), Value::Bool(false));
            let value = vm
                .call_value(
                    &Value::from("unserialize"),
                    vec![Value::from("a:1:{s:6:\"nested\";i:42;}")],
                )
                .expect("unserialize on next request");
            assert!(matches!(value, Value::Array(_)));
            let result = vm.call_value(&Value::from("serialize"), vec![value]);
            assert_eq!(result.unwrap(), Value::from("a:1:{s:6:\"nested\";i:42;}"));
        }
    }
}

#[test]
fn nested_serializable_bailout_does_not_destroy_shared_context() {
    let mut vm = machine();
    vm.run_script(
        br#"<?php
class Payload implements Serializable {
    function serialize(): string { zend_test_serialization_bailout(false, false); }
    function unserialize(string $data): void {}
}
serialize(new Payload);
"#,
        "bailout.php",
    );
    assert_eq!(field(&vm, "serialize_level"), Value::Int(2));
    assert_eq!(field(&vm, "serialize_data"), Value::Bool(true));
    startup(&mut vm);
    assert_eq!(field(&vm, "serialize_level"), Value::Int(0));
    assert_eq!(field(&vm, "serialize_data"), Value::Bool(false));
}

#[test]
fn state_function_reports_the_active_unserializer() {
    let mut vm = machine();
    startup(&mut vm);
    vm.with_unserialize_context(|vm| {
        let value = vm.call_value(&Value::from("zend_test_serialization_state"), vec![])?;
        let Value::Array(state) = value else { panic!("state array") };
        assert_eq!(state.get_str(b"lock"), Some(&Value::Int(0)));
        assert_eq!(state.get_str(b"unserialize_level"), Some(&Value::Int(1)));
        assert_eq!(state.get_str(b"unserialize_data"), Some(&Value::Bool(true)));
        Ok(())
    })
    .expect("unserialize context");
    assert_eq!(field(&vm, "unserialize_level"), Value::Int(0));
    assert_eq!(field(&vm, "unserialize_data"), Value::Bool(false));
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
    assert_eq!(field(&vm, "lock"), Value::Int(1));
    assert_eq!(field(&vm, "unserialize_level"), Value::Int(2));
    assert_eq!(field(&vm, "unserialize_data"), Value::Bool(true));
    // A new VM on this same worker thread must not inherit the abandoned
    // context when it starts the next request.
    drop(vm);
    let mut next = machine();
    startup(&mut next);
    assert_eq!(field(&next, "lock"), Value::Int(0));
    assert_eq!(field(&next, "unserialize_level"), Value::Int(0));
    assert_eq!(field(&next, "unserialize_data"), Value::Bool(false));
}
