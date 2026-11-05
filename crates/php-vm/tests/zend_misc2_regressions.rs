//! Regressions from assorted `Zend/tests` subdirectories (concat,
//! dynamic properties, generators, offsets, switch, unset, ...).
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

/// Runs `source` as `t.php` and returns its output.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `ZEND_ASSIGN_DIM_OP` with no offset appends one element and operates
/// on it in place (`bug32833`), also below another append, inside a
/// property and through a reference.
#[test]
fn compound_assignment_to_append_adds_one_element() {
    let out = run(r#"<?php
$t = [];
$t[] .= "ok";
echo count($t), $t[0], "\n";
$u = [];
$u[][] .= "x";
echo count($u), count($u[0]), $u[0][0], "\n";
$o = new stdClass;
$o->a = [5 => 1];
$o->a[] += 2;
echo count($o->a), $o->a[6], "\n";
$r = [];
$b = &$r;
$b[] .= "y";
echo count($r), $r[0], "\n";
"#);
    assert_eq!(out, "1ok\n11x\n22\n1y\n");
}

/// `zend_std_get_property_ptr_ptr` (`BP_VAR_RW`) raises the dynamic
/// property deprecation before the "Undefined property" warning
/// (`dynamic_prop_deprecation`).
#[test]
fn rw_fetch_of_new_dynamic_property_deprecates_before_warning() {
    let out = run(r#"<?php
set_error_handler(function ($no, $msg) { echo $msg, "\n"; });
class T {}
$o = new T;
$o->a += 1;
$o->b++;
$o->c[0] .= "x";
echo $o->a, $o->b, $o->c[0], "\n";
"#);
    assert_eq!(
        out,
        "Creation of dynamic property T::$a is deprecated\n\
         Undefined property: T::$a\n\
         Creation of dynamic property T::$b is deprecated\n\
         Undefined property: T::$b\n\
         Creation of dynamic property T::$c is deprecated\n\
         Undefined property: T::$c\n\
         Undefined array key 0\n\
         11x\n"
    );
}

/// `zend_deprecated_dynamic_property`: an error handler that releases the
/// object makes the creation fail (`dynamic_prop_deprecation_002`).
#[test]
fn dynamic_property_deprecation_handler_releasing_object_throws() {
    let out = run(r#"<?php
set_error_handler(function ($no, $msg) { echo $msg, "\n"; $GLOBALS['r'] = null; });
class T {}
$r = new T;
try { [&$r->y]; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$r = new T;
try { $r->y = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$r = new T;
try { $r->y .= 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$r = new T;
$keep = $r;
[&$r->y];
echo "kept\n";
"#);
    assert_eq!(
        out,
        "Creation of dynamic property T::$y is deprecated\n\
         Cannot create dynamic property T::$y\n\
         Creation of dynamic property T::$y is deprecated\n\
         Cannot create dynamic property T::$y\n\
         Creation of dynamic property T::$y is deprecated\n\
         Cannot create dynamic property T::$y\n\
         Creation of dynamic property T::$y is deprecated\n\
         kept\n"
    );
}

/// The "continue targeting switch" warning is raised once, by
/// `php_parser::validate` (`continue_targeting_switch_warning`).
#[test]
fn continue_targeting_switch_warns_once() {
    let out = run("<?php\nwhile (true) {\n    switch (1) {\n        case 1:\n            continue;\n    }\n    break;\n}\necho \"ok\";\n");
    assert_eq!(
        out,
        "\nWarning: \"continue\" targeting switch is equivalent to \"break\". Did you mean to use \"continue 2\"? in t.php on line 5\nok"
    );
}

/// `$$name` naming `this` goes through `zend_fetch_this_var`: write
/// fetches throw "Cannot re-assign $this", a read outside an object warns
/// (`this_reassign`).
#[test]
fn variable_variable_this_write_throws() {
    let out = run(r#"<?php
#[AllowDynamicProperties]
class C { function m() {
    $a = 'this';
    try { $$a->x = 1; echo "no\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $$a = 1; echo "no\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    echo get_class($$a), "\n";
} }
(new C)->m();
function f() {
    $a = 'this';
    try { $$a .= 1; echo "no\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    var_dump($$a);
}
f();
"#);
    assert_eq!(
        out,
        "Cannot re-assign $this\nCannot re-assign $this\nC\nCannot re-assign $this\n\nWarning: Undefined variable $this in t.php on line 13\nNULL\n"
    );
}
