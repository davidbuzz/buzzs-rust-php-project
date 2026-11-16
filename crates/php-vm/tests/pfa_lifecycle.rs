// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PFA lifetime across compilation units within one request, not preloading.
use php_runtime::ZStr;
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_units(declarations: &str, consumer: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_main(declarations.as_bytes(), &ZStr::from("declarations.php"))
        .expect("declaration unit");
    assert!(vm.frames.is_empty());
    vm.run_main(consumer.as_bytes(), &ZStr::from("consumer.php"))
        .expect("consumer unit");
    assert!(vm.frames.is_empty());
    assert_eq!(vm.shutdown(Ok(())), 0);
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn pfa_values_and_factories_survive_the_declaring_unit() {
    assert_eq!(
        run_units(
            r#"<?php
$calls = 0;
function increment($value) {
    global $calls;
    $calls++;
    return $value + 1;
}
function make_increment() {
    return increment(?);
}
$saved = make_increment();
$captured = (static function($value) { return $value + 10; })(?);
var_dump($calls);
"#,
            r#"<?php
var_dump($saved(1));
var_dump(make_increment()(2));
var_dump($captured(3));
var_dump($calls);
"#,
        ),
        "int(0)\nint(2)\nint(3)\nint(13)\nint(2)\n"
    );
}

#[test]
fn pfa_constexpr_values_survive_the_declaring_unit() {
    assert_eq!(
        run_units(
            r#"<?php
#[Attribute]
class CallbackAttribute {
    function __construct(public mixed $callback) {}
}
#[CallbackAttribute(strlen(?))]
class CallbackHolder {
    const CALLBACK = strlen(?);
    #[CallbackAttribute(strlen(?))]
    function callback($value = strlen(?)) {
        return $value;
    }
}
$saved = CallbackHolder::CALLBACK;
"#,
            r#"<?php
var_dump($saved('hello'));
var_dump((CallbackHolder::CALLBACK)('hello'));
$class = new ReflectionClass(CallbackHolder::class);
$attribute = $class->getAttributes(CallbackAttribute::class)[0]->newInstance();
var_dump(($attribute->callback)('hello'));
$method = new ReflectionMethod(CallbackHolder::class, 'callback');
$attribute = $method->getAttributes(CallbackAttribute::class)[0]->newInstance();
var_dump(($attribute->callback)('hello'));
var_dump((new CallbackHolder()->callback())('hello'));
"#,
        ),
        "int(5)\nint(5)\nint(5)\nint(5)\nint(5)\n"
    );
}
