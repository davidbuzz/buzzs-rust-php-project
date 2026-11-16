// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

/// Writes `preload` to a file of its own directory, runs it as the startup
/// preload request, then `request` (with `{DIR}` replaced by that
/// directory) in a fresh VM holding the preloaded declarations. Returns
/// both requests' output, with the directory written as `{DIR}`.
fn preload_file_then_run(name: &str, preload: &str, request: &str) -> String {
    let dir = std::env::temp_dir().join(format!("phprust-preload-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let dir = std::fs::canonicalize(&dir).expect("temp dir path");
    let path = dir.join("pre.php");
    std::fs::write(&path, preload).expect("preload file");
    let dir_str = dir.to_string_lossy().into_owned();
    let path_z = ZStr::from(path.to_string_lossy().as_ref());
    let sink = Sink::default();
    let mut startup = new_vm(&sink);
    let result = startup.run_preload(preload.as_bytes(), &path_z);
    let image = startup.finish_preload(result).expect("preload request");
    assert!(!Vm::is_preloading());
    let mut vm = new_vm(&sink);
    vm.install_persistent_declarations(&image)
        .expect("install beside the prelude declarations");
    let request = request.replace("{DIR}", &dir_str);
    let result = vm.run_main(request.as_bytes(), &ZStr::from("request.php"));
    let _ = vm.shutdown(result);
    drop(vm);
    let _ = std::fs::remove_dir_all(&dir);
    let output = sink.0.borrow().clone();
    String::from_utf8(output)
        .expect("UTF-8 output")
        .replace(&dir_str, "{DIR}")
}

/// Including a preloaded script again runs its code without redeclaring
/// what it preloaded (`preload_remove_declares`); a declaration preloading
/// did not reach is declared.
#[test]
fn including_a_preloaded_script_skips_its_preloaded_declarations() {
    assert_eq!(
        preload_file_then_run(
            "include",
            r#"<?php
function f1() {}
if (isset($rt)) { function f2() {} }
interface I {}
class C implements I {}
echo "top\n";
"#,
            r"<?php
var_dump(function_exists('f2'));
$rt = true;
include '{DIR}/pre.php';
var_dump(function_exists('f2'), new C instanceof I);
",
        ),
        "top\nbool(false)\ntop\nbool(true)\nbool(true)\n"
    );
}

/// A function declared inside a function while preloading is preloaded,
/// and its declaration is gone from the enclosing function.
#[test]
fn dynamic_definitions_made_while_preloading_are_removed() {
    assert_eq!(
        preload_file_then_run(
            "dynamic",
            r#"<?php
function outer() {
    function inner() { return "inner"; }
}
outer();
class P { function make() { function made() { return "made"; } } }
"#,
            r#"<?php
include '{DIR}/pre.php';
echo inner(), "\n";
(new P)->make();
echo made(), "\n";
"#,
        ),
        "inner\nmade\n"
    );
}

/// `preload_link()`: the classes the preload script compiled but did not
/// declare are linked after it ran, and each top-level or anonymous one
/// left unlinked warns, in source order; one nested in a block does not
/// (expected output from php-src with opcache).
#[test]
fn unlinked_classes_warn_after_preloading() {
    assert_eq!(
        preload_file_then_run(
            "link",
            r"<?php
function g() { return new class extends Gone {}; }
return;
class A {}
class B extends Missing {}
interface K { function m(); }
class L implements K { function m($x) {} }
class M extends A implements K { function m() { return 1; } }
if ($never) { class N extends Nope {} }
",
            r"<?php
var_dump(class_exists('A', false), class_exists('B', false), class_exists('L', false), (new M)->m());
",
        ),
        "\nWarning: Can't preload unlinked class Gone@anonymous: Unknown parent Gone in {DIR}/pre.php on line 2\n\
         \nWarning: Can't preload unlinked class B: Unknown parent Missing in {DIR}/pre.php on line 5\n\
         \nWarning: Can't preload unlinked class L: Declaration of L::m($x) must be compatible with K::m() in {DIR}/pre.php on line 7\n\
         bool(true)\nbool(false)\nbool(false)\nint(1)\n"
    );
}

/// `opcache_preloading()` holds while the preload script runs, and the
/// request sees what preloading made persistent.
#[test]
fn preloading_state_and_info() {
    let out = preload_file_then_run(
        "info",
        "<?php function pf() {} class PC {} class_alias('Stringable', 'MyStringable');\n",
        "<?php abstract class T implements MyStringable {} var_dump(is_subclass_of('T', 'Stringable'));\n",
    );
    assert_eq!(out, "bool(true)\n");
    let info = Vm::preload_info().expect("installed preload");
    assert!(info.functions.iter().any(|f| f.as_bytes() == b"pf"));
    assert!(info.classes.iter().any(|c| c.as_bytes() == b"PC"));
    assert!(info.scripts[0].as_bytes().ends_with(b"/pre.php"));
}
