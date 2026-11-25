// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The guards of `__get`/`__set`/`__isset`/`__unset`
//! (`zend_get_property_guard`, `Zend/zend_object_handlers.c`): inside a
//! magic method the same name on the same object is a plain property
//! access, a different name or another object calls the magic method
//! again, a chain of objects each runs its own; a throw ends the guard;
//! readonly properties are reinitializable once during `__clone()`.
//! Expected output from `/usr/bin/php8.5 -n`.
mod common;

use php_vm::VmOptions;
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
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn magic_guards_cover_one_name_of_one_object() {
    assert_eq!(
        run(r###"<?php
#[AllowDynamicProperties]
class M {
    public $log = [];
    private $data = ['a' => 1];
    function __get($n) {
        $this->log[] = "get $n";
        if ($n === 'self') { return $this->self; }          // same name: no recursion, warning
        if ($n === 'other') { return $this->a . '+' . $this->b; } // different names: each its own __get
        if ($n === 'deep') { return $this->deep2; }
        if ($n === 'deep2') { return $this->deep; }        // deep is guarded: undefined
        return $this->data[$n] ?? "none($n)";
    }
    function __set($n, $v) {
        $this->log[] = "set $n";
        if ($n === 'loop') { $this->loop = $v * 2; return; } // same name inside __set: real property
        $this->data[$n] = $v;
    }
    function __isset($n) { $this->log[] = "isset $n"; return isset($this->$n) || isset($this->data[$n]); }
    function __unset($n) { $this->log[] = "unset $n"; unset($this->$n); unset($this->data[$n]); }
}
function run() {
    $m = new M;
    for ($i = 0; $i < 2; $i++) {
        echo $m->a, ' ', $m->other, ' ', @$m->self, ' ', var_export(@$m->deep, true), "\n";
        $m->b = $i; $m->loop = 3; echo $m->loop, ' ', $m->b, "\n";
        unset($m->loop);
        var_dump(isset($m->b), isset($m->zz), empty($m->a));
        unset($m->b);
    }
    $n = new M; echo $n->other, "\n";       // a different object: its own guards
    echo implode(",", $m->log), "\n"; $m2 = new M; echo $m2->a, "\n";
    echo implode(",", $m2->log), "\n";
}
run();
class Chain { public $next; public $id; function __construct($id, $next) { $this->id = $id; $this->next = $next; }
    function __get($n) { return $this->next ? $this->next->$n . $this->id : "end:$n"; } }
function chain() { $c = new Chain(1, new Chain(2, new Chain(3, null))); for ($i = 0; $i < 2; $i++) { echo $c->x, ' ', $c->y, "\n"; } }
chain();
class T { function __get($n) { throw new Exception("no $n"); } }
function thr() { $t = new T; for ($i = 0; $i < 2; $i++) { try { echo $t->p; } catch (Exception $e) { echo $e->getMessage(), "\n"; } } }
thr();
class R { public readonly int $x; function __construct() { $this->x = 1; } function __clone() { $this->x = 2; } }
function ro() { $r = new R; $c = clone $r; echo $r->x, $c->x, "\n"; try { $c->x = 3; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
ro();
class R2 { public readonly int $x; function __construct() { $this->x = 1; } function __clone() { $this->x = 2; $this->x = 3; } }
function ro2() { try { $c = clone new R2; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
ro2();
"###),
        r###"1 1+none(b)  NULL
6 0
bool(true)
bool(false)
bool(false)
1 1+none(b)  NULL
6 1
bool(true)
bool(false)
bool(false)
1+none(b)
get a,get other,get a,get b,get self,get deep,get deep2,set b,set loop,get b,isset b,isset zz,isset a,get a,unset b,get a,get other,get a,get b,get self,get deep,get deep2,set b,set loop,get b,isset b,isset zz,isset a,get a,unset b
1
get a
end:x21 end:y21
end:x21 end:y21
no p
no p
12
Cannot modify readonly property R::$x
Cannot modify readonly property R2::$x
"###
    );
}

/// An undeclared property read reaching `__get` (and `__isset` first for
/// `??`, `isset`, `empty`) by the short way, and every case that must
/// not take it: a private property seen from inside and outside, a static
/// property of the name, a dynamic property, a parent's private property
/// read from the parent's scope, a mangled and a numeric name.
#[test]
fn undeclared_reads_reach_get_as_the_generic_lookup_does() {
    assert_eq!(
        run(r###"<?php
#[AllowDynamicProperties]
class G { public static $st = 'static'; private $priv = 'p';
    function __get($n) { return "get($n)"; }
    function readIn($n) { return $this->$n; } }
class H extends G { function __isset($n) { echo "isset($n) "; return $n !== 'no'; } }
class Base { private $bp = 'base-private'; function readBp($o) { return $o->bp; } }
class Kid extends Base { function __get($n) { return "kid-get($n)"; } }
function run() {
    for ($i = 0; $i < 2; $i++) {
        $g = new G;
        echo $g->x, ' ', $g->priv, ' ', $g->readIn('priv'), ' ', $g->readIn('y'), "\n";
        echo @$g->st, "\n";
        $g->dyn = 'dynamic'; echo $g->dyn, ' '; unset($g->dyn); echo $g->dyn, "\n";
        $h = new H;
        echo $h->a ?? 'dflt', ' ', $h->no ?? 'dflt', ' ', var_export(isset($h->q), true), ' ', var_export(empty($h->q), true), "\n";
        $k = new Kid; echo $k->bp, ' ', $k->readBp($k), ' ', (new Base)->readBp($k), "\n";
        try { $n = "\0G\0priv"; echo bin2hex($g->$n), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        $n = '123'; echo $g->$n, "\n";
    }
}
run();
"###),
        r###"get(x) get(priv) p get(y)
get(st)
dynamic get(dyn)
isset(a) get(a) isset(no) dflt isset(q) true isset(q) false
kid-get(bp) base-private base-private
676574280047007072697629
get(123)
get(x) get(priv) p get(y)
get(st)
dynamic get(dyn)
isset(a) get(a) isset(no) dflt isset(q) true isset(q) false
kid-get(bp) base-private base-private
676574280047007072697629
get(123)
"###
    );
}

#[test]
fn prop_guards_end_the_innermost_matching_guard() {
    use php_runtime::ZStr;
    use php_vm::vm::PropGuards;
    let mut g = PropGuards::default();
    assert!(g.is_empty());
    let a = ZStr::from("a");
    let b = ZStr::from("b");
    g.insert(1, a.clone(), 0);
    g.insert(1, b.clone(), 0);
    g.insert(2, a.clone(), 0);
    assert!(g.contains(1, &a, 0));
    assert!(!g.contains(1, &a, 1), "another kind");
    assert!(!g.contains(3, &a, 0), "another object");
    // Out of order (a fiber suspended inside the first access).
    g.remove(1, &a, 0);
    assert!(!g.contains(1, &a, 0));
    assert!(g.contains(1, &b, 0) && g.contains(2, &a, 0));
    // A guard not in effect: nothing changes.
    g.remove(1, &a, 0);
    assert_eq!(g.iter().count(), 2);
    g.retain(|k| k.0 != 2);
    assert_eq!((&g).into_iter().count(), 1);
    g.clear();
    assert!(g.is_empty());
}
