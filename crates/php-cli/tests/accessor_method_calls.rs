// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls of methods whose whole body is one statement the tape runs
//! without a frame (`exec::tape::calls::accessor_now`) and that take
//! arguments or return no property: a setter (plain, fluent, typed), an
//! `isset($this->map[$k])` or `array_key_exists($k, $this->map)` test, the
//! identity, `return $this;` and an empty body. Each round repeats the
//! calls, so the later rounds take that path. What must stay a real call
//! does: an argument the parameter's type coerces or refuses, a value the
//! property's type coerces or refuses, a readonly or hooked or dynamic
//! property, a reference in the slot, an offset that is no int or string
//! (its deprecation or `TypeError`), an undefined variable, too few or too
//! many arguments, by-reference, variadic and defaulted parameters, and an
//! overwritten value whose destructor runs. Expected output: PHP 8.5.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `source` (read from standard input) with no php.ini, every error
/// shown and none logged.
fn run(source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "log_errors=0",
            "-d",
            "display_errors=1",
            "-d",
            "error_reporting=-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("run the script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn frameless_methods_with_arguments_do_what_a_call_does() {
    let got = run(SOURCE);
    assert_eq!(got, EXPECTED);
}

const SOURCE: &str = r##"<?php
class S { private $v = 1; private array $map = ['a' => 1, 'n' => null, 5 => 'five']; public int $typed = 0; public readonly int $ro; public $hooked { set { $this->hooked = strrev($value); } }
  private $ref = 1; public ?S $next = null;
  function __construct() { $this->ro = 1; }
  function set($v) { $this->v = $v; } function fluent($v) { $this->v = $v; return $this; } function fluentStatic($v): static { $this->v = $v; return $this; }
  function setTyped(int $v): void { $this->typed = $v; } function setTypedLoose($v) { $this->typed = $v; } function setRo($v) { $this->ro = $v; } function setHooked($v) { $this->hooked = $v; }
  function setRef($v) { $this->ref = $v; } function &refOf() { return $this->ref; } function setDyn($v) { $this->dyn = $v; } function setNext(S $n): self { $this->next = $n; return $this; }
  function has($k) { return isset($this->map[$k]); } function hasKey($k): bool { return array_key_exists($k, $this->map); } function hasTypedKey(string $k) { return isset($this->map[$k]); }
  function id($a) { return $a; } function idInt(int $a): int { return $a; } function second($a, $b) { return $b; } function me() { return $this; } function meArg($x) { return $this; }
  function nothing() { } function nothingArgs($a, $b) { } function nothingVoid(): void { } function nothingInt(): ?int { } function get() { return $this->v; } function getTyped() { return $this->typed; }
  function byRef(&$x) { } function variadic(...$a) { } function withDefault($a = 1) { return $a; }
}
function show($f) { try { var_dump($f()); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
$s = new S; $t = new S; $und = null; unset($und);
for ($round = 0; $round < 3; $round++) {
  echo "round $round\n";
  show(fn() => $s->set(5)); show(fn() => $s->get()); show(fn() => $s->fluent('x')->get()); show(fn() => $s->fluentStatic([1])->get());
  show(fn() => $s->setTyped(7)); show(fn() => $s->getTyped()); show(fn() => $s->setTyped('8')); show(fn() => $s->getTyped()); show(fn() => $s->setTyped('x'));
  show(fn() => $s->setTypedLoose('9')); show(fn() => $s->getTyped()); show(fn() => $s->setTypedLoose('bad')); show(fn() => $s->setRo(2)); show(fn() => $s->setHooked('abc')); show(fn() => $s->hooked);
  show(fn() => $s->setNext($t)->next === $t); show(fn() => $s->setNext(5));
  show(fn() => $s->has('a')); show(fn() => $s->has('n')); show(fn() => $s->has('zz')); show(fn() => $s->has(5)); show(fn() => $s->has('5')); show(fn() => $s->has(null)); show(fn() => $s->has(1.5)); show(fn() => $s->has([]));
  show(fn() => $s->hasKey('a')); show(fn() => $s->hasKey('n')); show(fn() => $s->hasKey('zz')); show(fn() => $s->hasKey(5)); show(fn() => @$s->hasKey(null)); show(fn() => $s->hasKey([]));
  show(fn() => $s->hasTypedKey('a')); show(fn() => $s->hasTypedKey(5)); show(fn() => $s->hasTypedKey([]));
  show(fn() => $s->id('v')); show(fn() => $s->idInt(3)); show(fn() => $s->idInt('4')); show(fn() => $s->idInt('q')); show(fn() => $s->second(1, 2)); show(fn() => $s->me() === $s); show(fn() => $s->meArg(1) === $s);
  show(fn() => $s->nothing()); show(fn() => $s->nothingArgs(1, 2)); show(fn() => $s->nothingVoid()); show(fn() => $s->nothingInt());
  show(fn() => $s->id($und)); show(fn() => $s->id()); show(fn() => $s->id(1, 2)); show(fn() => $s->withDefault()); show(fn() => $s->variadic(1, 2));
  $x = 1; $s->byRef($x); $r = &$s->refOf(); $r = 'bound'; show(fn() => $s->setRef('new')); show(fn() => $r); unset($r);
  show(fn() => $s->setDyn(1)); show(fn() => $s->dyn ?? 'nodyn');
}
class D2 { public $h; function set($v) { $this->h = $v; } function __destruct() { echo "D2 gone\n"; } }
$h = new S; $d = new D2; $d2 = new D2; $h2 = new class { public $p; function set($v) { $this->p = $v; } };
for ($i = 0; $i < 3; $i++) { $h2->set(new D2); echo "set $i\n"; } $h2->set(null); echo "end\n";
"##;

const EXPECTED: &str = r##"round 0
NULL
int(5)
string(1) "x"
array(1) {
  [0]=>
  int(1)
}
NULL
int(7)
NULL
int(8)
TypeError: S::setTyped(): Argument #1 ($v) must be of type int, string given, called in Standard input code on line 18
NULL
int(9)
TypeError: Cannot assign string to property S::$typed of type int
Error: Cannot modify readonly property S::$ro
NULL
string(3) "cba"
bool(true)
TypeError: S::setNext(): Argument #1 ($n) must be of type S, int given, called in Standard input code on line 20
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)

Deprecated: Using null as an array offset is deprecated, use an empty string instead in Standard input code on line 8
bool(false)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 8
bool(false)
TypeError: Cannot access offset of type array in isset or empty
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
TypeError: Cannot access offset of type array on array
bool(true)
bool(true)
TypeError: S::hasTypedKey(): Argument #1 ($k) must be of type string, array given, called in Standard input code on line 23
string(1) "v"
int(3)
int(4)
TypeError: S::idInt(): Argument #1 ($a) must be of type int, string given, called in Standard input code on line 24
int(2)
bool(true)
bool(true)
NULL
NULL
NULL
TypeError: S::nothingInt(): Return value must be of type ?int, none returned

Warning: Undefined variable $und in Standard input code on line 26
NULL
ArgumentCountError: Too few arguments to function S::id(), 0 passed in Standard input code on line 26 and exactly 1 expected
int(1)
int(1)
NULL
NULL
string(3) "new"

Deprecated: Creation of dynamic property S::$dyn is deprecated in Standard input code on line 7
NULL
int(1)
round 1
NULL
int(5)
string(1) "x"
array(1) {
  [0]=>
  int(1)
}
NULL
int(7)
NULL
int(8)
TypeError: S::setTyped(): Argument #1 ($v) must be of type int, string given, called in Standard input code on line 18
NULL
int(9)
TypeError: Cannot assign string to property S::$typed of type int
Error: Cannot modify readonly property S::$ro
NULL
string(3) "cba"
bool(true)
TypeError: S::setNext(): Argument #1 ($n) must be of type S, int given, called in Standard input code on line 20
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)

Deprecated: Using null as an array offset is deprecated, use an empty string instead in Standard input code on line 8
bool(false)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 8
bool(false)
TypeError: Cannot access offset of type array in isset or empty
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
TypeError: Cannot access offset of type array on array
bool(true)
bool(true)
TypeError: S::hasTypedKey(): Argument #1 ($k) must be of type string, array given, called in Standard input code on line 23
string(1) "v"
int(3)
int(4)
TypeError: S::idInt(): Argument #1 ($a) must be of type int, string given, called in Standard input code on line 24
int(2)
bool(true)
bool(true)
NULL
NULL
NULL
TypeError: S::nothingInt(): Return value must be of type ?int, none returned

Warning: Undefined variable $und in Standard input code on line 26
NULL
ArgumentCountError: Too few arguments to function S::id(), 0 passed in Standard input code on line 26 and exactly 1 expected
int(1)
int(1)
NULL
NULL
string(3) "new"
NULL
int(1)
round 2
NULL
int(5)
string(1) "x"
array(1) {
  [0]=>
  int(1)
}
NULL
int(7)
NULL
int(8)
TypeError: S::setTyped(): Argument #1 ($v) must be of type int, string given, called in Standard input code on line 18
NULL
int(9)
TypeError: Cannot assign string to property S::$typed of type int
Error: Cannot modify readonly property S::$ro
NULL
string(3) "cba"
bool(true)
TypeError: S::setNext(): Argument #1 ($n) must be of type S, int given, called in Standard input code on line 20
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)

Deprecated: Using null as an array offset is deprecated, use an empty string instead in Standard input code on line 8
bool(false)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 8
bool(false)
TypeError: Cannot access offset of type array in isset or empty
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
TypeError: Cannot access offset of type array on array
bool(true)
bool(true)
TypeError: S::hasTypedKey(): Argument #1 ($k) must be of type string, array given, called in Standard input code on line 23
string(1) "v"
int(3)
int(4)
TypeError: S::idInt(): Argument #1 ($a) must be of type int, string given, called in Standard input code on line 24
int(2)
bool(true)
bool(true)
NULL
NULL
NULL
TypeError: S::nothingInt(): Return value must be of type ?int, none returned

Warning: Undefined variable $und in Standard input code on line 26
NULL
ArgumentCountError: Too few arguments to function S::id(), 0 passed in Standard input code on line 26 and exactly 1 expected
int(1)
int(1)
NULL
NULL
string(3) "new"
NULL
int(1)
set 0
D2 gone
set 1
D2 gone
set 2
D2 gone
end
D2 gone
D2 gone
"##;
