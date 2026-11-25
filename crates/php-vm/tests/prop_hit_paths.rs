// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The interpreter's property and object hit paths (`exec/tape/props.rs`,
//! `exec/spec/props.rs`, `new` in `exec/tape/calls.rs`, `instanceof` and
//! `??`), each run at least three times in a function so that the cache
//! fill, the hit and the declines after it all run. Expected output from
//! `/usr/bin/php8.5 -n`.

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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `$obj->p = v` (`ZEND_ASSIGN_OBJ`): untyped, typed with coercion and a
/// TypeError, nullable, readonly, private from outside and inside, an
/// uninitialized typed slot, a property a reference points into, `__set`
/// for an inaccessible and an undeclared name, dynamic properties, an
/// unset declared slot, and the assignment's own value.
#[test]
fn property_writes_match_php() {
    let out = run(r#"<?php
class P {
    public $u = 1;
    public int $i = 0;
    public ?string $s = null;
    public readonly int $ro;
    private $priv = 0;
    public int $late;
    public $byref = 0;
    function __construct() { $this->ro = 7; }
    function setPriv($v) { $this->priv = $v; return $this->priv; }
}
class M {
    public $declared = 0;
    private $hidden = 0;
    function __set($n, $v) { echo "__set($n)\n"; }
}
#[AllowDynamicProperties] class D {}
function w(P $o, M $m, D $d, $k) {
    $o->u = $k;
    $o->u = "s$k";
    $o->u = [$k];
    $o->i = $k;
    try { $o->i = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $o->i = "4$k";
    $o->i = 2.0;
    $o->s = $k;
    try { $o->ro = $k; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $o->priv = $k; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    echo $o->setPriv($k), "\n";
    $o->late = $k;
    $r = &$o->byref;
    $o->byref = $k * 10;
    echo $r, "\n";
    unset($r);
    $m->declared = $k;
    $m->hidden = $k;
    $m->nope = $k;
    $d->dyn = $k;
    $d->dyn = $k + 1;
    $x = $o->u = $k + 100;
    echo $x, "\n";
}
$o = new P; $m = new M; $d = new D;
for ($k = 0; $k < 3; $k++) { w($o, $m, $d, $k); }
var_dump($o, $m, $d);
$u = new P;
unset($u->u);
for ($k = 0; $k < 3; $k++) { $u->u = $k; }
var_dump($u->u);
"#);
    assert_eq!(
        out,
        r#"Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
0
0
__set(hidden)
__set(nope)
100
Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
1
10
__set(hidden)
__set(nope)
101
Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
2
20
__set(hidden)
__set(nope)
102
object(P)#1 (7) {
  ["u"]=>
  int(102)
  ["i"]=>
  int(2)
  ["s"]=>
  string(1) "2"
  ["ro"]=>
  int(7)
  ["priv":"P":private]=>
  int(2)
  ["late"]=>
  int(2)
  ["byref"]=>
  int(20)
}
object(M)#2 (2) {
  ["declared"]=>
  int(2)
  ["hidden":"M":private]=>
  int(0)
}
object(D)#3 (1) {
  ["dyn"]=>
  int(3)
}
int(2)
"#
    );
}
