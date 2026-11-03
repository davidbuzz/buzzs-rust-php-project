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
