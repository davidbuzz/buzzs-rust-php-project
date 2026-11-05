//! Class/function/constant name collision regressions
//! (Zend/tests/{name_collision,class_alias,class_name,namespaces}).
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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

fn assert_contains(source: &str, expected: &str) {
    let out = run(source);
    assert!(out.contains(expected), "{source}\n--- got ---\n{out}");
}

/// `zend_class_redeclaration_error`: the message names the kind and the
/// location of the class already declared under the name.
#[test]
fn class_redeclaration_names_the_previous_declaration() {
    assert_contains(
        "<?php\nclass A {}\ninterface A {}\n",
        "Fatal error: Cannot redeclare class A (previously declared in t.php:2) in t.php on line 3",
    );
    assert_contains(
        "<?php\ntrait A {}\ntrait A {}\n",
        "Fatal error: Cannot redeclare trait A (previously declared in t.php:2) in t.php on line 3",
    );
    assert_contains(
        "<?php\nclass Exception {}\n",
        "Fatal error: Cannot redeclare class Exception in t.php on line 2",
    );
}

/// A class declaration executed twice (in a function called twice) is a
/// redeclaration; an early-bound declaration still runs its statement once.
#[test]
fn class_declaration_statement_runs_once() {
    assert_contains(
        "<?php\nfunction t() {\n    class A {}\n}\nt();\nt();\n",
        "Fatal error: Cannot redeclare class A (previously declared in t.php:3) in t.php on line 3",
    );
    assert_eq!(
        run("<?php\n$a = new A;\necho $a instanceof A ? 'y' : 'n';\nclass A {}\necho 'done';\n"),
        "ydone"
    );
}

/// `class_alias()` warns with the aliased class's kind and location.
#[test]
fn class_alias_collision_warning() {
    assert_contains(
        "<?php\nclass foo {}\ninterface test {}\necho class_alias('foo', 'test') ? 'y' : 'n';\n",
        "Warning: Cannot redeclare class test (previously declared in t.php:2) in t.php on line 4\nn",
    );
    assert_contains(
        "<?php\ninterface a {}\nclass_alias('a', 'b');\nclass b {}\n",
        "Warning: Cannot redeclare interface b (previously declared in t.php:2) in t.php on line 3",
    );
}

/// `zend_do_implement_interfaces`: an interface named twice (via an alias).
#[test]
fn duplicate_interface_through_alias() {
    assert_contains(
        "<?php\ninterface a {}\nclass_alias('a', 'b');\ninterface c extends a, b {}\n",
        "Fatal error: Interface c cannot implement previously implemented interface a in t.php on line 4",
    );
}

/// `zend_compile_use` and the declarations colliding with imports.
#[test]
fn imports_collide_with_declarations() {
    let cases = [
        (
            "<?php\nuse A\\B as Foo;\n\nclass Foo {\n}\n",
            "Fatal error: Cannot redeclare class Foo (previously declared as local import) in t.php on line 4",
        ),
        (
            "<?php\nclass Foo {\n}\n\nuse A\\B as Foo;\n",
            "Fatal error: Cannot use A\\B as Foo because the name is already in use in t.php on line 5",
        ),
        (
            "<?php\nnamespace Bazzle {\n    use function Foo\\bar;\n    function bar() {}\n}\n",
            "Fatal error: Cannot redeclare function Bazzle\\bar() (previously declared as local import) in t.php on line 4",
        ),
        (
            "<?php\nnamespace Bazzle {\n    use const Foo\\BAR;\n    const BAR = 24;\n}\n",
            "Fatal error: Cannot declare const Bazzle\\BAR because the name is already in use in t.php on line 4",
        ),
        (
            "<?php\nuse A\\B;\nuse A\\B;\n",
            "Fatal error: Cannot use A\\B as B because the name is already in use in t.php on line 3",
        ),
        (
            "<?php\nnamespace X;\nuse function A\\b;\nuse function A\\b;\n",
            "Fatal error: Cannot use function A\\b as b because the name is already in use in t.php on line 4",
        ),
        (
            "<?php\nnamespace foo;\nconst NULL = 1;\n",
            "Fatal error: Cannot redeclare constant 'NULL' in t.php on line 3",
        ),
    ];
    for (src, expected) in cases {
        assert_contains(src, expected);
    }
    // Importing the declared class itself is allowed.
    assert_eq!(
        run("<?php\nnamespace N;\nclass Foo {}\nuse N\\Foo;\necho Foo::class;\n"),
        "N\\Foo"
    );
}

/// "use A" outside a namespace has no effect.
#[test]
fn non_compound_use_warns_outside_namespace() {
    assert_eq!(
        run("<?php\nuse A;\nuse \\B;\necho 'ok';\n"),
        "\nWarning: The use statement with non-compound name 'A' has no effect in t.php on line 2\n\
         \nWarning: The use statement with non-compound name 'B' has no effect in t.php on line 3\nok"
    );
    assert_eq!(run("<?php\nnamespace N;\nuse A;\necho 'ok';\n"), "ok");
}
