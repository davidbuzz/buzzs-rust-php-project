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

/// The object stays the same object through its destructor
/// (`zend_objects_store_del` keeps it at refcount 1): `WeakReference::get()`
/// inside `__destruct` is `$this`, a destructor storing `$this` keeps the
/// object (same id, no second destructor call) and its handle is reused
/// only once it is released.
#[test]
fn destructing_object_keeps_its_identity() {
    let out = run(r#"<?php
class D {
    public static $wr; public static $keep;
    function __destruct() { echo "dtor ", spl_object_id($this), " ", var_export(self::$wr->get() === $this, true), "\n"; self::$keep = $this; }
}
$o = new D; D::$wr = WeakReference::create($o); $id = spl_object_id($o);
unset($o);
echo "after unset: ", var_export(D::$wr->get() === D::$keep, true), " ", spl_object_id(D::$keep) === $id ? "same id" : "other id", "\n";
D::$keep = null;
echo "after release: ", var_export(D::$wr->get(), true), "\n";
$n = new stdClass; echo "reused: ", spl_object_id($n) === $id ? "yes" : "no", "\n";
"#);
    assert_eq!(
        out,
        "dtor 1 true\nafter unset: true same id\nafter release: NULL\nreused: yes\n"
    );
}

/// A cycle whose destructor resurrects `$this`: the collection frees
/// nothing, the WeakReference still resolves to the kept object, and once
/// it is released the next collection frees the cycle without a second
/// destructor call.
#[test]
fn resurrected_cycle_is_collected_once_released() {
    let out = run(r#"<?php
class C { public $self; public static $saved; function __destruct() { echo "dtor\n"; self::$saved = $this; } }
$c = new C; $c->self = $c; $w = WeakReference::create($c); unset($c);
var_dump(gc_collect_cycles());
var_dump($w->get() === C::$saved);
C::$saved = null;
var_dump(gc_collect_cycles());
var_dump($w->get());
"#);
    assert_eq!(out, "dtor\nint(0)\nbool(true)\nint(1)\nNULL\n");
}

/// Shutdown destructors (`zend_objects_store_call_destructors`): the object
/// is held across its destructor (`GC_ADDREF`), which may store `$this`;
/// the WeakReference resolves to it.
#[test]
fn shutdown_destructor_may_keep_this() {
    let out = run(r#"<?php
class S { public static $wr; public static $n = 0; function __destruct() { self::$n++; echo "S dtor ", self::$n, " ", var_export(self::$wr->get() === $this, true), "\n"; $GLOBALS['zz'] = $this; } }
$s = new S; S::$wr = WeakReference::create($s);
echo "end of script\n";
"#);
    assert_eq!(out, "end of script\nS dtor 1 true\n");
}

/// A throwing destructor still frees its object: the handle is reused by
/// the next object (the Exception took the handle after it).
#[test]
fn throwing_destructor_frees_the_object() {
    let out = run(r#"<?php
class E { function __destruct() { throw new Exception("boom"); } }
try { $e = new E; unset($e); echo "not reached\n"; } catch (Exception $x) { echo "caught ", $x->getMessage(), "\n"; }
$n = new stdClass; var_dump(spl_object_id($n));
"#);
    assert_eq!(out, "caught boom\nint(1)\n");
}

/// `spl_object_id` and a `WeakMap` entry keyed by the object stay valid
/// inside `__destruct`; the entry goes with the object
/// (`zend_weakrefs_notify` at the free, after the destructor).
#[test]
fn weakmap_entry_lives_through_the_destructor() {
    let out = run(r#"<?php
class I { public static $ids = []; function __destruct() { self::$ids[] = spl_object_id($this); } }
$o = new I; $before = spl_object_id($o); unset($o);
var_dump(I::$ids === [$before]);
class M { public static $map; function __destruct() { var_dump(isset(self::$map[$this]), self::$map->count()); } }
M::$map = new WeakMap; $m = new M; M::$map[$m] = 1; unset($m); var_dump(M::$map->count());
"#);
    assert_eq!(out, "bool(true)\nbool(true)\nint(1)\nint(0)\n");
}
