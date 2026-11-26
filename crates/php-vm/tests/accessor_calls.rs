// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls of methods whose body is one `return $this->prop;` or
//! `return <literal>;`, which the tape answers without a frame
//! (`calls::accessor_now`) once the site and the method's property cache
//! are warm: every round repeats the calls, so the later rounds take that
//! path. What must stay as a real call does: an uninitialized typed
//! property (its `Error` and trace), a value the return type coerces or
//! refuses, a reference in the slot, `__get()`, a property hook, a
//! dynamic or unset property, a private or static or `#[\Deprecated]`
//! method, a temporary receiver with a destructor, and two classes at one
//! site. Expected output: PHP 8.5.

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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
#[allow(clippy::too_many_lines)] // one script and its expected output
fn accessor_methods_return_what_a_call_returns() {
    let out = run(r#"<?php
class P { private $p = 'parent-private'; protected $q = 'q'; public int $typed; public $u = 1;
  function getP() { return $this->p; } function getQ() { return $this->q; } function getTyped(): int { return $this->typed; }
  function asString(): string { return $this->u; } function asStatic(): static { return $this->o; } public $o;
  function lit() { return 'lit'; } function nul(): ?int { return null; } function arr(): array { return [1, 2]; } function bad(): int { return 'x'; }
  private function priv() { return $this->u; } static function st() { return 1; } function getDyn() { return $this->dyn; }
  #[\Deprecated] function dep() { return $this->u; }
  function viaThis() { return $this->getP() . '/' . $this->lit(); } function callPriv() { return $this->priv(); }
}
class C extends P { private $p = 'child-private'; protected $q = 'child-q'; function getOwnP() { return $this->p; } }
class M { private $data = ['a' => 1]; public $real = 'real'; function __get($n) { return "magic $n"; } function getReal() { return $this->real; } function getMissing() { return $this->missing; } }
class D { public $v = 'd'; function get() { return $this->v; } function __destruct() { echo "destruct D\n"; } }
class H { public string $h { get => "hooked " . $this->h; } function __construct() { $this->h = 'hook'; } function get() { return $this->h; } }
function show($f) { try { var_dump($f()); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), ' @', $e->getLine(), ' trace=', count($e->getTrace()), ' top=', $e->getTrace()[0]['function'] ?? '-', "\n"; } }
$p = new P; $c = new C; $m = new M;
for ($round = 0; $round < 3; $round++) {
  echo "round $round\n";
  show(fn() => $p->getP()); show(fn() => $c->getP()); show(fn() => $c->getOwnP()); show(fn() => $p->getQ()); show(fn() => $c->getQ());
  show(fn() => $p->getTyped()); show(fn() => $p->asString()); show(fn() => $p->asStatic()); show(fn() => $p->lit()); show(fn() => $p->nul()); show(fn() => $p->arr());
  show(fn() => $p->bad()); show(fn() => $p->priv()); show(fn() => $p->st()); show(fn() => $p->getDyn()); show(fn() => $p->dep());
  show(fn() => $p->viaThis()); show(fn() => $p->callPriv()); show(fn() => $m->getReal()); show(fn() => $m->getMissing());
  show(fn() => (new D)->get()); echo "after temp\n"; show(fn() => (new H)->get());
  if ($round === 0) { $p->typed = 3; $p->o = $p; }
  if ($round === 1) { $r = &$p->u; $r = 'via-ref'; unset($p->q); $p->typed = 9; }
}
$x = $p->getTyped(1, 2, 3); var_dump($x);
unset($p->u); show(fn() => $p->asString()); $p->lit();
abstract class A { abstract function g(); } class B extends A { private $v = 'b'; function g() { return $this->v; } } class B2 extends A { private $v = 'b2'; function g() { return $this->v; } }
foreach ([new B, new B2, new B, new B2, new B] as $o) { echo $o->g(), ' '; } echo "\n";
"#);
    assert_eq!(
        out,
        r#"round 0
string(14) "parent-private"
string(14) "parent-private"
string(13) "child-private"
string(1) "q"
string(7) "child-q"
Error: Typed property P::$typed must not be accessed before initialization @3 trace=3 top=getTyped
string(1) "1"
TypeError: P::asStatic(): Return value must be of type P, null returned @4 trace=3 top=asStatic
string(3) "lit"
NULL
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
TypeError: P::bad(): Return value must be of type int, string returned @5 trace=3 top=bad
Error: Call to private method P::priv() from global scope @20 trace=2 top={closure:t.php:20}
int(1)

Warning: Undefined property: P::$dyn in t.php on line 6
NULL

Deprecated: Method P::dep() is deprecated in t.php on line 20
int(1)
string(18) "parent-private/lit"
int(1)
string(4) "real"
string(13) "magic missing"
destruct D
string(1) "d"
after temp
string(11) "hooked hook"
round 1
string(14) "parent-private"
string(14) "parent-private"
string(13) "child-private"
string(1) "q"
string(7) "child-q"
int(3)
string(1) "1"
object(P)#1 (5) {
  ["p":"P":private]=>
  string(14) "parent-private"
  ["q":protected]=>
  string(1) "q"
  ["typed"]=>
  int(3)
  ["u"]=>
  int(1)
  ["o"]=>
  *RECURSION*
}
string(3) "lit"
NULL
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
TypeError: P::bad(): Return value must be of type int, string returned @5 trace=3 top=bad
Error: Call to private method P::priv() from global scope @20 trace=2 top={closure:t.php:20}
int(1)

Warning: Undefined property: P::$dyn in t.php on line 6
NULL

Deprecated: Method P::dep() is deprecated in t.php on line 20
int(1)
string(18) "parent-private/lit"
int(1)
string(4) "real"
string(13) "magic missing"
destruct D
string(1) "d"
after temp
string(11) "hooked hook"

Fatal error: Uncaught Error: Cannot access protected property P::$q in t.php:24
Stack trace:
#0 {main}
  thrown in t.php on line 24
"#
    );
}
