//! Script-level tests of the cycle collector (expected output from php
//! 8.3 / master).

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn object_self_cycle() {
    let out = run("<?php $a = new stdClass; $a->a = $a; unset($a); var_dump(gc_collect_cycles());");
    assert_eq!(out, "int(1)\n");
}

#[test]
fn live_cycle_is_kept() {
    let out = run(
        "<?php $a = new stdClass; $a->a = $a; var_dump(gc_collect_cycles()); var_dump($a->a === $a);",
    );
    assert_eq!(out, "int(0)\nbool(true)\n");
}

#[test]
fn object_array_cycle_counts_both() {
    let out = run(
        "<?php $a = new stdClass; $a->x = [1]; $a->x[] = $a; unset($a); var_dump(gc_collect_cycles());",
    );
    assert_eq!(out, "int(2)\n");
}

#[test]
fn two_object_cycle_with_destructors() {
    let out = run(
        "<?php class F { public $o; public $n; function __construct($n) { $this->n = $n; }\n\
         function __destruct() { echo \"d{$this->n}\\n\"; } }\n\
         $a = new F(1); $b = new F(2); $a->o = $b; $b->o = $a; unset($a, $b);\n\
         var_dump(gc_collect_cycles()); echo \"end\\n\";",
    );
    assert_eq!(out, "d1\nd2\nint(2)\nend\n");
}

#[test]
fn destructor_resurrection() {
    let out = run(
        "<?php class R { public $self; function __destruct() { $GLOBALS['keep'] = $this; } }\n\
         $r = new R; $r->self = $r; unset($r);\n\
         var_dump(gc_collect_cycles()); var_dump($keep->self === $keep);",
    );
    assert_eq!(out, "int(0)\nbool(true)\n");
}

#[test]
fn closure_cycle() {
    let out = run(
        "<?php class C { public $f; function __construct() { $this->f = function () { return $this; }; } }\n\
         $c = new C; unset($c); var_dump(gc_collect_cycles());",
    );
    assert_eq!(out, "int(2)\n");
}

#[test]
fn closure_use_by_reference_cycle() {
    let out = run(
        "<?php function f() { $fn = function () use (&$fn) {}; } f(); var_dump(gc_collect_cycles());",
    );
    assert_eq!(out, "int(1)\n");
}

#[test]
fn generator_cycle() {
    let out = run("<?php function g($o) { yield 1; }\n\
         $o = new stdClass; $o->g = g($o); $o->g->current(); unset($o);\n\
         var_dump(gc_collect_cycles());");
    // The generator has a destructor (`dtor_obj`): it is freed by the
    // re-run, the stdClass in the first pass is excluded as nested data.
    assert_eq!(out, "int(1)\n");
}

#[test]
fn weakref_cleared_by_collection() {
    let out = run(
        "<?php $o = new stdClass; $o->o = $o; $w = WeakReference::create($o); unset($o);\n\
         var_dump($w->get() !== null); gc_collect_cycles(); var_dump($w->get());",
    );
    assert_eq!(out, "bool(true)\nNULL\n");
}

#[test]
#[ignore = "needs the WeakMap traversal of gc-proposals (weakref.rs)"]
fn weakmap_entry_does_not_keep_key_alive() {
    let out = run(
        "<?php $m = new WeakMap; $o = new stdClass; $m[$o] = $o; unset($o);\n\
         var_dump($m->count()); var_dump(gc_collect_cycles()); var_dump($m->count());",
    );
    assert_eq!(out, "int(1)\nint(1)\nint(0)\n");
}

#[test]
fn weakmap_value_kept_while_map_and_key_live() {
    let out = run(
        "<?php $m = new WeakMap; $o = new stdClass; $v = new stdClass; $v->v = $v; $m[$o] = $v; unset($v);\n\
         var_dump(gc_collect_cycles()); var_dump($m->count());",
    );
    assert_eq!(out, "int(0)\nint(1)\n");
}

#[test]
fn enable_disable_and_ini() {
    let out = run(
        "<?php gc_disable(); var_dump(gc_enabled(), ini_get('zend.enable_gc'));\n\
         gc_enable(); var_dump(gc_enabled(), ini_get('zend.enable_gc'));\n\
         ini_set('zend.enable_gc', '0'); var_dump(gc_enabled());",
    );
    assert_eq!(
        out,
        "bool(false)\nstring(1) \"0\"\nbool(true)\nstring(1) \"1\"\nbool(false)\n"
    );
}

#[test]
fn status_shape() {
    let out = run(
        "<?php foreach (gc_status() as $k => $_) { echo $k, ','; } echo \"\\n\";\n\
         $o = new stdClass; $o->o = $o; unset($o); gc_collect_cycles(); gc_collect_cycles();\n\
         $s = gc_status(); var_dump($s['runs'], $s['collected'], $s['threshold'], $s['running']);",
    );
    assert_eq!(
        out,
        "running,protected,full,runs,collected,threshold,buffer_size,roots,application_time,collector_time,destructor_time,free_time,\n\
         int(1)\nint(1)\nint(10001)\nbool(false)\n"
    );
}

#[test]
fn nested_collection_returns_zero() {
    let out = run(
        "<?php class N { public $s; function __destruct() { var_dump(gc_status()['running'], gc_collect_cycles()); } }\n\
         $n = new N; $n->s = $n; unset($n); var_dump(gc_collect_cycles());",
    );
    assert_eq!(out, "bool(true)\nint(0)\nint(1)\n");
}

#[test]
fn exceptions_from_destructors_are_chained() {
    let out = run(
        "<?php class E { public $o; function __destruct() { throw new Exception('x'); } }\n\
         $a = new E; $b = new E; $a->o = $b; $b->o = $a; unset($a, $b);\n\
         try { gc_collect_cycles(); } catch (Exception $e) { var_dump($e->getPrevious() instanceof Exception); }",
    );
    assert_eq!(out, "bool(true)\n");
}

#[test]
#[ignore = "needs the php-runtime reference roots of gc-proposals"]
fn reference_array_cycle() {
    let out = run("<?php $a = []; $a[] = &$a; unset($a); var_dump(gc_collect_cycles());");
    assert_eq!(out, "int(1)\n");
}

#[test]
fn destructor_objects_freed_by_rerun() {
    // php-src's gc_017: destructors unlink the nodes; only the root
    // object is left for the re-run.
    let out = run(
        "<?php class N { public $name; public $children; public $parent;\n\
         function __construct($n) { $this->name = $n; }\n\
         function insert($c) { $c->parent = $this; $this->children[] = $c; }\n\
         function __destruct() { echo $this->name; unset($this->name, $this->children, $this->parent); } }\n\
         $a = new N('A'); $b = new N('B'); $c = new N('C'); $a->insert($b); $a->insert($c);\n\
         unset($a, $b, $c); echo ' ', gc_collect_cycles(), \"\\n\";",
    );
    assert_eq!(out, " ABC1\n");
}

#[test]
fn nested_data_of_destructor_object_survives_pass() {
    // php-src's bug69446: the destructor resurrects its object; the cycle
    // holding it is freed, the object and its data are kept.
    let out = run(
        "<?php class Bad { public $x; function __destruct() { $GLOBALS['bar'] = $this; } }\n\
         $foo = new stdClass; $foo->foo = $foo; $foo->bad = new Bad; $foo->bad->x = new stdClass;\n\
         unset($foo); var_dump(gc_collect_cycles()); var_dump($bar->x instanceof stdClass);",
    );
    assert_eq!(out, "int(1)\nbool(true)\n");
}

#[test]
#[ignore = "needs the Fiber traversal of gc-proposals (fiber.rs)"]
fn fiber_callback_cycle() {
    // php-src's fibers/gc-cycle-callback.
    let out = run(
        "<?php $ref = new class { public $fiber; function __destruct() { echo \"DTOR\\n\"; } };\n\
         $fiber = new Fiber(function () use ($ref) {}); $ref->fiber = $fiber;\n\
         $fiber = null; $ref = null; echo \"COLLECT\\n\"; gc_collect_cycles(); echo \"DONE\\n\";",
    );
    assert_eq!(out, "COLLECT\nDTOR\nDONE\n");
}

#[test]
#[ignore = "needs the safe-point and release-site hooks of gc-proposals (exec.rs)"]
fn threshold_triggers_automatic_collection() {
    let out = run(
        "<?php for ($i = 0; $i < 25000; $i++) { $o = new stdClass; $o->self = $o; }\n\
         $s = gc_status(); var_dump($s['runs'] >= 2, $s['collected'] >= 20000);",
    );
    assert_eq!(out, "bool(true)\nbool(true)\n");
}

/// Bug #43450: a buffered root whose handle is freed leaves the buffer
/// (`GC_REMOVE_FROM_BUFFER`), so short-lived objects do not grow it.
#[test]
fn freed_objects_leave_the_root_buffer() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(
        b"<?php class Q { function m() {} }\nfor ($i = 0; $i < 1000; $i++) { $q = new Q; $q->m(); }\necho 'ok';",
        "t.php",
    );
    let len = super::with_state(|s| s.roots.entries.len()).unwrap_or_default();
    drop(vm);
    assert_eq!(sink.0.borrow().as_slice(), b"ok");
    assert!(len < 16, "{len} buffered entries");
}

/// Binding a reference (`$a[$i][0] = &$a[$i]`, `$y = &$x`) releases no
/// handle in php-src, so it buffers no possible root.
#[test]
fn binding_references_buffers_no_roots() {
    let out = run(
        "<?php $a = []; for ($i = 0; $i < 3; $i++) { $a[$i] = [[]]; $a[$i][0] = &$a[$i]; }\n\
         $x = [1]; $y = &$x; $z = &$x;\n\
         var_dump(gc_status()['roots']);",
    );
    assert_eq!(out, "int(0)\n");
}

/// gc_023: the automatic run collects the roots buffered when the
/// threshold was reached, not the root that crossed it
/// (`gc_possible_root_when_full`).
#[test]
fn threshold_run_leaves_the_crossing_root() {
    let out = run("<?php $a = [];\n\
         for ($i = 0; $i < 10001; $i++) { $a[$i] = [[]]; $a[$i][0] = &$a[$i]; }\n\
         unset($a); $s = gc_status();\n\
         var_dump($s['runs'], $s['collected'], $s['roots'], gc_collect_cycles());");
    assert_eq!(out, "int(1)\nint(10000)\nint(1)\nint(1)\n");
}

/// A cycle spanning several node chunks and trash batches of the
/// collector (`graph::Nodes`, `TRASH_BATCH`).
#[test]
fn large_cycle_is_collected() {
    let out = run("<?php class N { public $next; public $a; }\n\
         $first = new N; $cur = $first;\n\
         for ($i = 1; $i < 10000; $i++) { $n = new N; $cur->next = $n; $cur = $n; }\n\
         $cur->next = $first; unset($first, $cur, $n);\n\
         var_dump(gc_collect_cycles());\n\
         $o = new N; var_dump(spl_object_id($o) <= 10000);");
    assert_eq!(out, "int(10000)\nbool(true)\n");
}
