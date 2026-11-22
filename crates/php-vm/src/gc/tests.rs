// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Script-level tests of the cycle collector (expected output from
//! `/usr/bin/php8.5 -n`, or php-src master where the two differ).

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
    let len = super::with_state(|s| s.roots.slots.len()).unwrap_or_default();
    drop(vm);
    assert_eq!(sink.0.borrow().as_slice(), b"ok");
    assert!(len < 16, "{len} buffered entries");
}

/// Destructors collected inside a fiber run in the collector's own fiber,
/// started by the engine (`zend_fiber_start`): their backtraces continue
/// with `gc_collect_cycles()`, without a `Fiber->start()` frame.
#[test]
fn destructor_fiber_backtrace_continues_with_the_collection() {
    let out = run(
        "<?php class C { public $s; function __construct() { $this->s = $this; }\n\
         function __destruct() { echo (new Exception)->getTraceAsString(), \"\\n\"; } }\n\
         $f = new Fiber(function () {\n\
         new C; gc_collect_cycles();\n\
         });\n\
         $f->start();",
    );
    assert_eq!(
        out,
        "#0 [internal function]: C->__destruct()\n\
         #1 [internal function]: gc_destructor_fiber()\n\
         #2 t.php(4): gc_collect_cycles()\n\
         #3 [internal function]: {closure:t.php:3}()\n\
         #4 t.php(6): Fiber->start()\n\
         #5 {main}\n"
    );
}

/// oss-fuzz-471533782: an abandoned destructor fiber (its destructor
/// suspended it) is destroyed before the next one starts; its unwinding
/// has no caller (`prev_execute_data` is `NULL`), and what it throws is
/// chained to the destructors' exceptions (`remember_prev_exception`).
#[test]
fn abandoned_destructor_fiber_is_destroyed_detached() {
    let out = run(
        "<?php class C { public static $pending; public $s; function __construct() { $this->s = $this; }\n\
         function __destruct() {\n\
         if (self::$pending === null) {\n\
         try { Fiber::suspend(); } finally { self::$pending = new RuntimeException('pending'); throw self::$pending; }\n\
         }\n\
         throw new LogicException('thrown', 0, self::$pending); } }\n\
         $f = new Fiber(function () {\n\
         new C; new C;\n\
         try { gc_collect_cycles(); } catch (Throwable $t) {\n\
         echo $t->getPrevious()->getTraceAsString(), \"\\n\";\n\
         for (; $t !== null; $t = $t->getPrevious()) { echo $t->getMessage(), \"\\n\"; } }\n\
         });\n\
         $f->start();",
    );
    assert_eq!(
        out,
        "#0 [internal function]: C->__destruct()\n\
         #1 [internal function]: gc_destructor_fiber()\n\
         #2 {main}\n\
         thrown\npending\n"
    );
}

/// `[A::class, 'm']` is a compile-time literal (`A::class` folds to a
/// string), so the collector counts only the object and the fiber of a
/// cycle through a fiber's callable.
#[test]
fn class_name_callable_array_is_a_literal() {
    let out = run(
        "<?php class A { public $fiber; function m() {}\n\
         function keep() { $this->fiber = new Fiber([A::class, 'm']); }\n\
         function nested() { $x = [self::class, 'm']; $this->fiber = [$x]; $this->self = $this; } }\n\
         $a = new A; $a->keep(); unset($a); var_dump(gc_collect_cycles());\n\
         $a = new A; @$a->nested(); unset($a); var_dump(gc_collect_cycles());",
    );
    assert_eq!(out, "int(2)\nint(2)\n");
}

/// Releasing the last handle of an array releases its elements
/// (`zend_array_destroy`): objects referenced elsewhere become roots, and
/// enough of them trigger a collection right after the releasing op.
#[test]
fn released_array_elements_are_roots_and_fill_the_buffer() {
    let out = run(
        "<?php $o = new stdClass; $a = [$o, [$o]]; $a = null; var_dump(gc_status()['roots']);\n\
         class C { public $s; function __construct() { $this->s = $this; }\n\
         function __destruct() { echo \"C\\n\"; throw new Exception('c'); } }\n\
         $all = []; for ($i = 0; $i < 10001; $i++) $all[] = new stdClass;\n\
         function g() { global $all; $copy = [...$all]; new C;\n\
         try { $copy = null; echo \"not reached\\n\"; } catch (Exception $e) { echo 'caught ', $e->getMessage(), \"\\n\"; } }\n\
         g();",
    );
    assert_eq!(out, "int(1)\nC\ncaught c\n");
}

/// `destructors_014`: the compiled variables released while an exception
/// unwinds a fiber's frame fill the root buffer; the collection runs before
/// a handler is found, and the destructor's exception replaces the one in
/// flight, with it as its previous one.
#[test]
fn collection_while_unwinding_chains_the_exception_in_flight() {
    let out = run(
        "<?php class C { public $s; function __construct() { $this->s = $this; }\n\
         function __destruct() { echo \"C\\n\"; throw new Exception('c'); } }\n\
         $all = []; for ($i = 0; $i < 10001; $i++) $all[] = new stdClass;\n\
         function f() { global $all; $copy = [...$all]; new C; throw new Exception('f'); }\n\
         $fiber = new Fiber(function () {\n\
         try { f(); } catch (Exception $e) { echo $e->getMessage(), ' ', $e->getPrevious()->getMessage(), \"\\n\"; }\n\
         });\n\
         $fiber->start();",
    );
    assert_eq!(out, "C\nc f\n");
}

/// `destructors_015`: a fiber releases its callable (`zend_fiber_release_callable`)
/// inside the fiber once its function returned; values released with a
/// closure's bound variables can fill the root buffer, and the exception
/// of the collection's destructor is thrown by the fiber.
#[test]
fn fiber_releases_its_callable_when_its_function_returns() {
    let out = run(
        "<?php class C { public $s; function __construct() { $this->s = $this; }\n\
         function __destruct() { echo \"C\\n\"; throw new Exception('c'); } }\n\
         $all = []; for ($i = 0; $i < 10001; $i++) $all[] = new stdClass;\n\
         function make() { global $all; $copy = [...$all];\n\
         return new Fiber(function () use ($copy) { new C; echo \"return\\n\"; }); }\n\
         $fiber = make();\n\
         try { $fiber->start(); echo \"not reached\\n\"; } catch (Exception $e) { echo 'caught ', $e->getMessage(), \"\\n\"; }",
    );
    assert_eq!(out, "return\nC\ncaught c\n");
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

/// `gc_047`: a run finds the cycle alive through the reference the
/// by-reference `foreach` still holds and drops the root; the loop
/// variable is rooted again afterwards (`zend_gc_check_root_tmpvars`), so
/// the cycles are collected once the loop releases it without a GC check.
#[test]
fn live_loop_reference_is_rooted_again_after_a_run() {
    let out = run("<?php $a = [0, 1];\n\
         foreach ($a as &$v) { $a[0] =& $a; $a[1] = array(); $a[1][0] =& $a[1]; $b = 1; $a =& $b;\n\
         var_dump(gc_collect_cycles(), gc_status()['roots']); break; }\n\
         var_dump(gc_collect_cycles());");
    assert_eq!(out, "int(0)\nint(1)\nint(2)\n");
}

/// `gc_023`: the automatic run collects the roots buffered when the
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

/// The cycle class of the root-buffer order tests: destructors print ids.
const CYCLE: &str =
    "class C { public $self; function __construct(public $id) { $this->self = $this; }\n\
     function __destruct() { echo $this->id; } }\n";

/// `destructors_005`: a live root in the first slot leaves the buffer when
/// the collection finds it black, and `gc_compact` moves the last root
/// into the hole: the garbage is destructed in the order 2, 0, 1.
#[test]
fn live_root_hole_takes_the_last_root() {
    let out = run(&format!(
        "<?php {CYCLE}$x = new stdClass; $y = $x; unset($y);\n\
         new C(0); new C(1); new C(2); gc_collect_cycles();"
    ));
    assert_eq!(out, "201");
}

/// `new` of a class with a native constructor buffers the object
/// (`ZEND_CALL_RELEASE_THIS`), as a user constructor does. (`Exception`
/// is the bare VM's class with a native constructor; `ArrayObject` gives
/// the same output under `/usr/bin/php8.5 -n`.)
#[test]
fn native_constructor_buffers_the_new_object() {
    let out = run(&format!(
        "<?php {CYCLE}$y = new Exception('x');\n\
         new C(0); new C(1); new C(2); gc_collect_cycles();"
    ));
    assert_eq!(out, "201");
}

/// A closure call releases the closure (`ZEND_CALL_CLOSURE`): with the
/// object its body created with a native constructor, two live roots come
/// before the cycles, and the last two roots move into their slots.
#[test]
fn closure_call_buffers_the_closure() {
    let out = run(&format!(
        "<?php {CYCLE}function t($mk) {{ $x = $mk(); new C(0); new C(1); new C(2); gc_collect_cycles(); echo \"\\n\"; }}\n\
         t(fn() => new stdClass); gc_collect_cycles(); t(fn() => new Exception('x'));"
    ));
    assert_eq!(out, "201\n210\n");
}

/// `$this->m()` passes `$this` without a reference of its own: returning
/// from it buffers no root.
#[test]
fn this_method_call_buffers_no_root() {
    let out = run(&format!(
        "<?php {CYCLE}class A {{ function run() {{ $this->h(); new C(0); new C(1); new C(2); gc_collect_cycles(); }} function h() {{}} }}\n\
         $a = new A; $a->run();"
    ));
    assert_eq!(out, "012");
}

/// The slot of a freed root is the next one reused (LIFO unused list).
#[test]
fn freed_root_slot_is_reused() {
    let out = run(&format!(
        "<?php {CYCLE}new C(0); $f = new stdClass; $g = $f; unset($g); new C(1);\n\
         unset($f); new C(2); gc_collect_cycles();"
    ));
    assert_eq!(out, "021");
}

/// Garbage that is no root is destructed in `gc_collect_white`'s order:
/// the first child is visited next, the others are stacked (php 1, 3, 2).
#[test]
fn non_root_garbage_follows_the_collect_white_walk() {
    let out = run(
        "<?php class E { public $id; public $back; function __destruct() { echo $this->id; } }\n\
         $r = new stdClass; $r->self = $r;\n\
         $r->a = new E; $r->a->id = 1; $r->a->back = $r;\n\
         $r->b = new E; $r->b->id = 2; $r->c = new E; $r->c->id = 3;\n\
         unset($r); gc_collect_cycles();",
    );
    assert_eq!(out, "132");
}

/// The buffer compaction (`gc_compact`): the first hole gets the last
/// entry, until the used slots are the first `num_roots`.
#[test]
fn compaction_fills_holes_with_the_last_entries() {
    let sink = Sink::default();
    let vm = Vm::new(&VmOptions::default(), Box::new(sink));
    let objs: Vec<php_runtime::Obj> = (0..5)
        .map(|_| {
            crate::object::new_object(vm.find_class(b"stdClass").expect("stdClass"), Vec::new())
        })
        .collect();
    let mut b = super::RootBuffer::default();
    for o in &objs {
        assert!(b.push(o).is_some());
    }
    assert!(b.push(&objs[0]).is_none());
    // Free slots 1 and 2: [A, _, _, D, E] becomes [A, E, D].
    b.release(1);
    b.release(2);
    // The last freed slot is reused first.
    let extra =
        crate::object::new_object(vm.find_class(b"stdClass").expect("stdClass"), Vec::new());
    assert_eq!(b.push(&extra), Some(2));
    b.release(2);
    b.compact();
    let order: Vec<usize> = b
        .roots()
        .iter()
        .map(|(_, o)| {
            objs.iter()
                .position(|x| php_runtime::Obj::ptr_eq(x, o))
                .unwrap_or(99)
        })
        .collect();
    assert_eq!(order, vec![0, 4, 3]);
    assert_eq!((b.num_roots, b.slots.len()), (3, 3));
    drop(vm);
}

/// A registered self-containing array that is still referenced is left
/// alone by a collection.
#[test]
fn live_registered_array_is_kept() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let a = php_runtime::ZArray::from_values([php_runtime::Value::Int(1)]);
    super::self_array_created(&a);
    vm.run_script(b"<?php var_dump(gc_collect_cycles());", "t.php");
    drop(vm);
    assert_eq!(sink.0.borrow().as_slice(), b"int(0)\n");
    assert_eq!(a.len(), 1);
}

/// [`super::RootBuffer::rehome`] leaves what `insert_new` under the new
/// handle followed by `remove` of the old entry leaves: the slots, the
/// unused list and the index of two buffers with the same history agree.
#[test]
fn rehome_is_insert_then_remove() {
    let sink = Sink::default();
    let vm = Vm::new(&VmOptions::default(), Box::new(sink));
    let std_class = vm.find_class(b"stdClass").expect("stdClass");
    let objs: Vec<php_runtime::Obj> = (0..6)
        .map(|_| crate::object::new_object(std_class.clone(), Vec::new()))
        .collect();
    let handle = |o: &php_runtime::Obj| crate::object::data(o).handle.get();
    let state = |b: &super::RootBuffer| {
        (
            b.slots
                .iter()
                .map(|s| s.as_ref().map(|r| (r.handle, super::weak_addr(&r.weak))))
                .collect::<Vec<_>>(),
            b.unused.clone(),
            b.num_roots,
            b.by_handle.clone(),
        )
    };
    let (mut one, mut two) = (super::RootBuffer::default(), super::RootBuffer::default());
    for b in [&mut one, &mut two] {
        for o in &objs {
            b.push(o);
        }
        // Two freed slots: the last freed is the next one taken.
        assert!(b.remove(objs[1].addr(), handle(&objs[1])));
        assert!(b.remove(objs[4].addr(), handle(&objs[4])));
    }
    // The object in slot 2 goes on under another handle, twice (the
    // second time into the slot it left).
    let o = &objs[2];
    let mut old = handle(o);
    for new in [901u32, 902] {
        one.insert_new(o, new);
        assert!(one.remove(o.addr(), old));
        assert!(two.rehome(o.addr(), old, new));
        assert_eq!(state(&one), state(&two));
        old = new;
    }
    // No freed slot: nothing changes.
    let mut full = super::RootBuffer::default();
    full.push(o);
    let before = state(&full);
    assert!(!full.rehome(o.addr(), handle(o), 903));
    assert_eq!(state(&full), before);
    // Not buffered under that handle.
    assert!(!two.rehome(o.addr(), 5000, 904));
}

/// The root index (by object handle, php-src's `GC_INFO`) finds and
/// forgets every buffered object, and an entry left behind by a freed
/// object never matches the new object that reuses its handle.
#[test]
fn root_index_finds_and_forgets_every_handle() {
    let sink = Sink::default();
    let vm = Vm::new(&VmOptions::default(), Box::new(sink));
    let std_class = vm.find_class(b"stdClass").expect("stdClass");
    let objs: Vec<php_runtime::Obj> = (0..64)
        .map(|_| crate::object::new_object(std_class.clone(), Vec::new()))
        .collect();
    let handle = |o: &php_runtime::Obj| crate::object::data(o).handle.get();
    let mut b = super::RootBuffer::default();
    for (i, o) in objs.iter().enumerate() {
        assert_eq!(b.push(o), Some(i));
        assert!(b.contains(handle(o), o.addr()));
    }
    for o in objs.iter().step_by(2) {
        assert!(b.remove(o.addr(), handle(o)));
        assert!(!b.contains(handle(o), o.addr()));
        assert!(!b.remove(o.addr(), handle(o)));
    }
    assert_eq!(b.num_roots, 32);
    assert!(objs
        .iter()
        .skip(1)
        .step_by(2)
        .all(|o| b.contains(handle(o), o.addr())));
    // A buffered object freed without notice (this buffer is not the
    // collector's): its handle goes to the next object, which is not
    // taken for it, and is buffered in a slot of its own.
    let gone = crate::object::new_object(std_class.clone(), Vec::new());
    let (h, slot) = (handle(&gone), b.push(&gone).expect("buffered"));
    drop(gone);
    let next = crate::object::new_object(std_class.clone(), Vec::new());
    assert_eq!(handle(&next), h);
    assert!(!b.contains(h, next.addr()));
    let mine = b.push(&next).expect("buffered");
    assert_ne!(mine, slot);
    b.release_dead(slot);
    assert!(b.contains(h, next.addr()));
    // Compaction keeps the index on the moved entries.
    b.compact();
    assert_eq!(b.num_roots, 33);
    assert!(b.contains(h, next.addr()));
    assert!(objs
        .iter()
        .skip(1)
        .step_by(2)
        .all(|o| b.contains(handle(o), o.addr())));
    drop(next);
    drop(vm);
}

/// `$this` and closures of calls returning in a loop are buffered once
/// (`LAST_ROOT` answers the later returns), and a collection that empties
/// the buffer makes the next return buffer the object again (php-src
/// 8.5: 2, 0, 0, 1, 1, 2).
#[test]
fn returning_this_is_buffered_once_and_again_after_a_collection() {
    let out = run("<?php class A { public $s; function m() { return 1; } }\n\
         $a = new A; $b = new A;\n\
         for ($i = 0; $i < 3; $i++) { $a->m(); $b->m(); }\n\
         var_dump(gc_status()['roots']); var_dump(gc_collect_cycles());\n\
         var_dump(gc_status()['roots']); $a->m(); var_dump(gc_status()['roots']);\n\
         $a->s = $a; $a->m(); unset($a); var_dump(gc_collect_cycles());\n\
         $c = function () { return 2; };\n\
         for ($i = 0; $i < 3; $i++) { $c(); $b->m(); }\n\
         var_dump(gc_status()['roots']);");
    assert_eq!(out, "int(2)\nint(0)\nint(0)\nint(1)\nint(1)\nint(2)\n");
}

/// The last root is recognised by its address alone: once it is freed (its
/// buffer entry removed), an object allocated in its place is buffered
/// when a shared handle of it is released (php-src 8.5: 1, 0, 1,
/// 1, 2, 2, 0, 0, 2).
#[test]
fn a_freed_last_root_does_not_hide_an_object_at_its_address() {
    let out = run("<?php class A { public $s; function m() { return 1; } }\n\
         $a = new A; $a->m(); $x = $a; $a->m(); var_dump(gc_status()['roots']);\n\
         unset($a, $x); var_dump(gc_status()['roots']);\n\
         $b = new A; $b->m(); var_dump(gc_status()['roots']);\n\
         $y = $b; unset($y); var_dump(gc_status()['roots']);\n\
         $c = new A; $d = $c; unset($d); var_dump(gc_status()['roots']);\n\
         $c->m(); $c->m(); var_dump(gc_status()['roots']);\n\
         var_dump(gc_collect_cycles(), gc_status()['roots']);\n\
         $c->m(); $b->m(); var_dump(gc_status()['roots']);");
    assert_eq!(
        out,
        "int(1)\nint(0)\nint(1)\nint(1)\nint(2)\nint(2)\nint(0)\nint(0)\nint(2)\n"
    );
}

/// Freeing a buffered object forgets `LAST_ROOT` only when it records that
/// object: `$o = new C; $o->m();` in a loop frees the previous object
/// between buffering the new one (its constructor's return) and the
/// method's return, which then finds the new one recorded. The counts are
/// php-src 8.5's.
#[test]
fn freeing_another_root_keeps_the_last_root() {
    let out = run(
        "<?php class C { public $self; function __construct() {} function m() { return 1; } }\n\
         for ($i = 0; $i < 3; $i++) { $o = new C; $o->m(); }\n\
         var_dump(gc_status()['roots']);\n\
         $o->self = $o; $o->m(); unset($o);\n\
         var_dump(gc_collect_cycles()); var_dump(gc_status()['roots']);\n\
         $p = new C; $p->m(); var_dump(gc_status()['roots']);\n\
         $q = new C; $p->m(); $q->m(); unset($p); $q->m(); var_dump(gc_status()['roots']);",
    );
    assert_eq!(out, "int(1)\nint(1)\nint(0)\nint(1)\nint(1)\n");

    let sink = Sink::default();
    let vm = Vm::new(&VmOptions::default(), Box::new(sink));
    let std_class = vm.find_class(b"stdClass").expect("stdClass");
    let a = crate::object::new_object(std_class.clone(), Vec::new());
    let b = crate::object::new_object(std_class, Vec::new());
    let handle = |o: &php_runtime::Obj| crate::object::data(o).handle.get();
    let mut buf = super::RootBuffer::default();
    assert_eq!(buf.push(&a), Some(0));
    assert_eq!(buf.push(&b), Some(1));
    super::remember_root(&b);
    assert!(buf.remove(a.addr(), handle(&a)));
    assert!(super::is_last_root(&b));
    // The freed slot is the next one taken.
    assert_eq!(buf.push(&a), Some(0));
    assert!(buf.remove(b.addr(), handle(&b)));
    assert!(!super::is_last_root(&b));
    assert_eq!((buf.num_roots, buf.unused.as_slice()), (1, &[1][..]));
    drop(buf);
    drop((a, b));
    drop(vm);
}
