//! `new` (`ZEND_NEW`, `object_init_ex`, `object_properties_init`,
//! `zend_std_get_constructor`): the instantiability errors, constructor
//! visibility, constructor-less classes with arguments, default property
//! copies and handle reuse with destructor order. Expected output from
//! PHP 8.3 (`php -n`).
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

#[test]
fn instantiability_errors_keep_their_order_and_text() {
    let out = run(r#"<?php
interface I {} trait T {} enum E { case A; } abstract class A {}
foreach (['I', 'T', 'E', 'A', 'Generator', 'Closure', 'WeakReference'] as $c) {
    try { new $c; } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
"#);
    assert_eq!(
        out,
        "Cannot instantiate interface I\n\
         Cannot instantiate trait T\n\
         Cannot instantiate enum E\n\
         Cannot instantiate abstract class A\n\
         The \"Generator\" class is reserved for internal use and cannot be manually instantiated\n\
         Instantiation of class Closure is not allowed\n\
         Direct instantiation of WeakReference is not allowed, use WeakReference::create instead\n"
    );
}

#[test]
fn constructor_visibility_and_constructorless_arguments() {
    let out = run(r#"<?php
class P { private function __construct() { echo "P\n"; } static function make() { return new P; } }
class Q { protected function __construct() { echo "Q\n"; } }
class R extends Q { static function make() { return new R; } }
try { new P; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { new Q; } catch (Error $e) { echo $e->getMessage(), "\n"; }
P::make(); R::make();
function f() { echo "f\n"; return 1; }
class N { public $a = [1, 2]; public $b = 'x'; public int $c = 3; }
$x = new N(f(), f());
try { $y = new N(a: 1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$x->a[] = 3; $x->b .= 'y'; $x->c++;
$z = new N;
echo serialize([$x, $z]), " ", spl_object_id($x), " ", spl_object_id($z), "\n";
"#);
    assert_eq!(
        out,
        "Call to private P::__construct() from global scope\n\
         Call to protected Q::__construct() from global scope\n\
         P\nQ\nf\nf\n\
         Unknown named parameter $a\n\
         a:2:{i:0;O:1:\"N\":3:{s:1:\"a\";a:3:{i:0;i:1;i:1;i:2;i:2;i:3;}s:1:\"b\";s:2:\"xy\";s:1:\"c\";i:4;}\
         i:1;O:1:\"N\":3:{s:1:\"a\";a:2:{i:0;i:1;i:1;i:2;}s:1:\"b\";s:1:\"x\";s:1:\"c\";i:3;}} 2 3\n"
    );
}

#[test]
fn handles_are_reused_and_destructors_run_in_order() {
    let out = run(r#"<?php
class D { public $id; function __construct($id) { $this->id = $id; } function __destruct() { echo "d", $this->id, " "; } }
for ($i = 0; $i < 4; $i++) { $o = new D($i); echo spl_object_id($o), " "; }
$o = null;
echo "\n";
$keep = [];
for ($i = 0; $i < 3; $i++) { $keep[] = new D($i + 10); }
unset($keep[1]);
$n = new stdClass; echo spl_object_id($n), "\n";
"#);
    assert_eq!(out, "1 d0 2 d1 1 d2 2 d3 \nd11 1\nd10 d12 ");
}
