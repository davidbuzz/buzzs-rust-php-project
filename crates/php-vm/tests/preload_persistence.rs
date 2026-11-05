//! opcache.preload: declarations promoted from a startup request into the
//! fresh VM of each later request (`Vm::finish_preload`,
//! `Vm::install_persistent_declarations`).
use php_runtime::ZStr;
use php_vm::ext::{Extension, ModuleBuilder};
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

/// Like ext/bcmath's `enum RoundingMode`: declarations an extension
/// compiles from PHP source while every VM is constructed.
struct Prelude;

impl Extension for Prelude {
    fn name(&self) -> &'static str {
        "preload_prelude"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        let _ = m.vm().run_main(
            b"<?php enum PreludeMode { case Up; } function prelude_helper() { return 40; }",
            &ZStr::from("[prelude]"),
        );
    }
}

fn new_vm(sink: &Sink) -> Vm {
    Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &[&Prelude])
}

/// Runs `preload` as the startup request, then `request` in a fresh VM
/// holding the preloaded declarations; returns both requests' output.
fn preload_then_run(preload: &str, request: &str) -> String {
    let sink = Sink::default();
    let mut startup = new_vm(&sink);
    let result = startup.run_main(preload.as_bytes(), &ZStr::from("preload.php"));
    let image = startup.finish_preload(result).expect("preload request");
    let mut vm = new_vm(&sink);
    vm.install_persistent_declarations(&image)
        .expect("install beside the prelude declarations");
    let result = vm.run_main(request.as_bytes(), &ZStr::from("request.php"));
    assert_eq!(vm.shutdown(result), 0);
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn preloaded_pfa_factories_run_in_the_request() {
    assert_eq!(
        preload_then_run(
            r"<?php
function f($abc) {
    return $abc + 1;
}
function test() {
    return f(?);
}
var_dump(test()(1));
",
            r"<?php
var_dump(test()(1));
var_dump(prelude_helper() + test()(1));
var_dump(PreludeMode::Up->name);
",
        ),
        "int(2)\nint(2)\nint(42)\nstring(2) \"Up\"\n"
    );
}

#[test]
fn preloaded_constexpr_pfas_evaluate_in_the_request() {
    assert_eq!(
        preload_then_run(
            r"<?php
#[Attribute]
class A {
    function __construct(public mixed $fn) {}
}
#[A(strlen(?))]
class C {
    const CC = strlen(?);
    #[A(strlen(?))]
    function f($arg = strlen(?)) {
        return $arg;
    }
}
",
            r"<?php
var_dump((C::CC)('hello'));
var_dump((new ReflectionClass(C::class)->getAttributes(A::class)[0]->newInstance()->fn)('hello'));
var_dump((new ReflectionMethod(C::class, 'f')->getAttributes(A::class)[0]->newInstance()->fn)('hello'));
var_dump((new C()->f())('hello'));
",
        ),
        "int(5)\nint(5)\nint(5)\nint(5)\n"
    );
}

#[test]
fn destructors_run_in_the_request_after_preloading() {
    assert_eq!(
        preload_then_run(
            r#"<?php
class D {
    function __destruct() { echo "destructed\n"; }
}
echo "preloaded\n";
"#,
            r#"<?php
new D();
echo "done\n";
"#,
        ),
        "preloaded\ndestructed\ndone\n"
    );
}
