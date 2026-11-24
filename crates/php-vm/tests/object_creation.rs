// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `new` (`ZEND_NEW`, `object_init_ex`, `object_properties_init`,
//! `zend_std_get_constructor`): the instantiability errors, constructor
//! visibility, constructor-less classes with arguments, default property
//! copies and handle reuse with destructor order. Expected output from
//! `/usr/bin/php8.5 -n`.
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

/// Objects of every slot size class (inline up to 64 declared slots, then
/// a buffer of their own): defaults, writes, `clone`, `unset`, the
/// `serialize` round trip, comparison, the property count and destructor
/// order. Expected output from `/usr/bin/php8.5 -n`, also the debug
/// build's of master 4e1c2df8.
#[test]
fn slot_storage_of_every_size_class() {
    let out = run(r#"<?php
// `json_encode()` of the values used here, with the builtins of the bare VM.
function j($v) { return is_array($v) ? '[' . implode(',', $v) . ']' : (string) $v; }
foreach ([1, 2, 9, 48, 64, 65, 70] as $n) {
    $decl = '';
    for ($i = 0; $i < $n; $i++) { $decl .= "public \$p$i = $i; "; }
    eval("class C$n { $decl public int \$t; public function __destruct() { echo 'd', \$this->t, ' '; } }");
    $c = "C$n";
    $o = new $c;
    $last = 'p' . ($n - 1);
    $mid = 'p' . intdiv($n, 2);
    $o->$last = [$n];
    $o->t = -$n;
    $k = clone $o;
    $k->p0 = 'k';
    $k->t = $n;
    if ($n > 1) { unset($o->$mid); }
    $u = unserialize(serialize($o));
    $u->t = 100 + $n;
    echo $n, ": ", count(get_object_vars($o)), " ", j($o->p0), " ", $k->p0, " ", j($u->$last), " ",
        isset($o->$mid) ? "set" : "unset", " ", isset($u->$mid) ? "set" : "unset", " ",
        $o == $k ? "eq" : "ne", " ", count((array) $u), "\n";
    unset($o, $k, $u);
    echo "\n";
}
"#);
    assert_eq!(
        out,
        "1: 2 [1] k [1] set set ne 2\nd-1 d1 d101 \n\
         2: 2 0 k 1 unset set ne 3\nd-2 d2 d102 \n\
         9: 9 0 k [9] unset set ne 10\nd-9 d9 d109 \n\
         48: 48 0 k [48] unset set ne 49\nd-48 d48 d148 \n\
         64: 64 0 k [64] unset set ne 65\nd-64 d64 d164 \n\
         65: 65 0 k [65] unset set ne 66\nd-65 d65 d165 \n\
         70: 70 0 k [70] unset set ne 71\nd-70 d70 d170 \n"
    );
}

/// A lazy ghost whose initializer throws gets its slots back
/// (`zend_lazy_object_revert_init`: the snapshot is written back into the
/// object's own slots), so it is still lazy and marking it initialized
/// gives the defaults, not the initializer's values. PHP 8.4+ semantics
/// (Zend/tests/lazy_objects/init_exception_reverts_initializer_changes);
/// expected output from the debug build of master 4e1c2df8.
#[test]
fn failed_lazy_initialization_reverts_the_slots() {
    let out = run(r#"<?php
// `json_encode()` of a property array, with the builtins of the bare VM.
function j(array $props): string {
    $out = [];
    foreach ($props as $k => $v) { $out[] = "\"$k\":" . (is_array($v) ? '[' . implode(',', $v) . ']' : $v); }
    return '{' . implode(',', $out) . '}';
}
class L { public $a = 1; public int $b = 2; public $c = [3]; }
$r = new ReflectionClass(L::class);
$g = $r->newLazyGhost(function (L $l) { $l->a = 'x'; $l->b = 5; $l->c[] = 4; throw new Exception("init failed"); });
try { $g->a; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
echo (int) $r->isUninitializedLazyObject($g), "\n";
$r->markLazyObjectAsInitialized($g);
echo j(get_object_vars($g)), "\n";
"#);
    assert_eq!(out, "init failed\n1\n{\"a\":1,\"b\":2,\"c\":[3]}\n");
}
