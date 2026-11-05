//! End-to-end checks of reflection (and closely related engine builtins)
//! against the outputs of the php-src `ext/reflection` `.phpt` tests.

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

/// bug49719.phpt / property_exists.phpt: a parent's private property is
/// not a property of the subclass.
#[test]
fn property_exists_ignores_inherited_private_properties() {
    assert_eq!(
        run(r"<?php
class A { private $a; private $c; }
class B extends A { private $b; private $c; }
class C extends B {}
var_dump(property_exists('B', 'a'), property_exists(new B, 'a'));
var_dump(property_exists('B', 'b'), property_exists('B', 'c'), property_exists('C', 'c'));
"),
        "bool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(false)\n"
    );
}

/// ReflectionClass_setStaticPropertyValue_003.phpt /
/// ReflectionProperty_typed_static.phpt: writes through a static property
/// that holds a typed reference honour the reference's type sources.
#[test]
fn static_setters_check_typed_reference_sources() {
    assert_eq!(
        run(r#"<?php
class Test { public static $x; public static int $y = 2; }
Test::$x =& Test::$y;
$rc = new ReflectionClass('Test');
try { $rc->setStaticPropertyValue("x", "foo"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$rc->setStaticPropertyValue("x", "42");
var_dump(Test::$y);
$rp = new ReflectionProperty('Test', 'x');
try { $rp->setValue(null, "bar"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$rp->setValue(null, "7");
var_dump(Test::$y);
"#),
        "Cannot assign string to reference held by property Test::$y of type int\nint(42)\n\
Cannot assign string to reference held by property Test::$y of type int\nint(7)\n"
    );
}

/// ReflectionProperty_isWritable_readonly.phpt: an initialized readonly
/// property is writable once inside `__clone()`.
#[test]
fn is_writable_sees_reinitable_readonly_properties_while_cloning() {
    assert_eq!(
        run(r"<?php
class A {
    public readonly int $a;
    public function __construct() { $this->a = 42; }
    public function __clone() {
        $rp = new ReflectionProperty($this, 'a');
        var_dump($rp->isWritable(A::class, $this));
        $this->a = 43;
        var_dump($rp->isWritable(A::class, $this));
    }
}
var_dump((new ReflectionProperty(A::class, 'a'))->isWritable(A::class, new A));
clone new A;
"),
        "bool(false)\nbool(true)\nbool(false)\n"
    );
}

/// ReflectionReference_errors.phpt: the private constructor is what
/// rejects `new ReflectionReference()`.
#[test]
fn reflection_reference_constructor_is_private() {
    assert_eq!(
        run(r#"<?php
try { new ReflectionReference(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$a = [1]; $r = &$a[0];
var_dump(ReflectionReference::fromArrayElement($a, 0) instanceof ReflectionReference);
"#),
        "Call to private ReflectionReference::__construct() from global scope\nbool(true)\n"
    );
}

/// bug74949.phpt: `Closure::__invoke` of a closure without parameters or
/// return type has no arginfo, so `__toString()` prints no parameter list.
#[test]
fn closure_invoke_without_arginfo_prints_no_parameters() {
    assert_eq!(
        run(r#"<?php
$f = function () {};
echo new ReflectionMethod($f, "__invoke");
$g = function (): int {};
echo new ReflectionMethod($g, "__invoke");
"#),
        "Method [ <internal> public method __invoke ] {\n}\n\
Method [ <internal> public method __invoke ] {\n\n  - Parameters [0] {\n  }\n  - Return [ int ]\n}\n"
    );
}
