// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Place walks (`$cv[...] = ...`, `$cv op= ...`, `&$cv[...]`, `unset($cv[...])`)
//! on a function's compiled variable reach it through a one-variable
//! stand-in table instead of the frame's whole symbol table: the variable
//! has to come back with what the walk did to it, also when the walk threw
//! or ran user code (`offsetSet()`, closures writing a by-reference
//! variable, nested walks), and walks that read the variable again (an
//! offset that is the variable itself, `$p[] = $p`) as well as the global
//! scope keep the table. The expected output is what the walk through the
//! table printed before (no PHP is available as an oracle where this was
//! written); it agrees with PHP's documented semantics.

#![allow(clippy::needless_raw_string_hashes)]

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
fn walks_on_a_function_variable_keep_it_and_the_others() {
    assert_eq!(
        run(r#"<?php
function j($v) {
  if ($v instanceof stdClass || $v instanceof AA) { $v = (array) $v; $obj = true; } else { $obj = false; }
  if ($v === []) return $obj ? '{}' : '[]';
  if (gettype($v) === 'array') {
    $list = !$obj; $i = 0; foreach ($v as $k => $x) { if ($k !== $i++) { $list = false; } }
    $out = ''; foreach ($v as $k => $x) { $out .= ($out === '' ? '' : ',') . ($list ? '' : '"' . $k . '":') . j($x); }
    return $list ? '[' . $out . ']' : '{' . $out . '}';
  }
  if (gettype($v) === 'string') return '"' . $v . '"';
  return (string) $v;
}
class AA implements ArrayAccess { public $a = [];
 function offsetExists($o): bool { return isset($this->a[$o]); }
 function offsetGet($o): mixed { return $this->a[$o] ?? null; }
 function offsetSet($o, $v): void { if ($o === 'boom') throw new Exception('boom'); $this->a[$o] = $v; }
 function offsetUnset($o): void { unset($this->a[$o]); } }
function f() {
  $x = 1; $aa = new AA; $y = 'y';
  $aa['k'] = 5; $aa['j'] = $x;
  try { $aa['boom'] = 1; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  unset($aa['j']);
  echo get_class($aa), ' ', j($aa->a), ' ', $x, $y, "\n";
  $arr = ['a' => 1]; $copy = $arr;
  $arr += ['b' => 2, 'a' => 9];
  echo j($arr), j($copy), "\n";
  $n = ['a' => ['b' => 1]]; $r = &$n['a']; $r['b'] = 2; echo j($n), "\n";
  $n['c'] = 3; unset($n['c']); echo j($n), "\n";
  $s = 'abc'; $s[1] = 'xyz'; echo $s, "\n";
  $m = ['k' => 'v']; $m[$m['k']] = 1; echo j($m), "\n";
  $p = [1]; $p[] = $p; echo count($p), count($p[1]), "\n";
  $u['x'] = 1; echo j($u), "\n";
  $w .= 'a'; echo $w, "\n";
  $t = [1, 2]; $t[0] += 5; $t[1] .= 'x'; $t[2] ??= 7; echo j($t), "\n";
  $o = new stdClass; $o->list[] = 1; $o->n = 1; $o->n += 2; echo j($o), "\n";
  $q = [3]; $q[0]++; ++$q[0]; echo $q[0], "\n";
  $names = ''; foreach (get_defined_vars() as $k => $_) { $names .= $k . ','; } echo $names, "\n";
}
f();
// Global scope: the table path.
$g = ['a' => 1]; $g += ['b' => 2]; $ga = new AA; $ga['z'] = 1; echo j($g), j($ga->a), "\n";
function h() { global $g; $g['c'] = 3; $g += ['d' => 4]; }
h(); echo j($g), "\n";
// A closure's by-reference variable written during the walk.
function cl() {
  $v = [1]; $aa = new AA;
  $fn = function () use (&$v) { $v[] = 2; };
  $aa2 = new class($fn) implements ArrayAccess { function __construct(public $fn) {}
    function offsetExists($o): bool { return false; } function offsetGet($o): mixed { return null; }
    function offsetSet($o, $val): void { ($this->fn)(); } function offsetUnset($o): void {} };
  $aa2['x'] = 1; $v[] = 3; echo j($v), "\n";
}
cl();
// Recursion: offsetSet runs a function doing walks on its own variables.
class Rec implements ArrayAccess { public $d = 0;
 function offsetExists($o): bool { return false; } function offsetGet($o): mixed { return null; }
 function offsetSet($o, $v): void { if ($this->d++ < 3) { $inner = new Rec; $inner->d = $this->d; $inner[$o] = $v; $arr = []; $arr[$o] = $v; echo $this->d, j($arr); } }
 function offsetUnset($o): void {} }
function rr() { $r = new Rec; $r['q'] = 'v'; echo "\n", get_class($r), "\n"; }
rr();
// Generators.
function gen() { $a = []; $a['k'] = yield 1; $a += ['m' => yield 2]; yield j($a); }
$gn = gen(); $gn->current(); $gn->send('A'); $gn->send('B'); echo $gn->current(), "\n";
function addnine(&$x) { $x[] = 9; }
// By-reference argument into a dim, and reference into a dim of the base.
function refs() { $a = ['x' => [3, 1, 2]]; addnine($a['x']); $b = ['n' => 1]; $rb = &$b['n']; $rb++; $c = $b; $c['n'] = 10; echo j($a), j($b), j($c), "\n"; }
refs();
"#),
        r#"boom
AA {"k":5} 1y
{"a":1,"b":2}{"a":1}
{"a":{"b":2}}
{"a":{"b":2}}

Warning: Only the first byte will be assigned to the string offset in t.php on line 29
axc
{"k":"v","v":1}
21
{"x":1}

Warning: Undefined variable $w in t.php on line 33
a
[6,"2x",7]
{"list":[1],"n":3}
5
x,aa,y,e,arr,copy,n,r,s,m,p,u,w,t,o,q,names,
{"a":1,"b":2}{"z":1}
{"a":1,"b":2,"c":3,"d":4}
[1,2,3]
3{"q":"v"}2{"q":"v"}1{"q":"v"}
Rec
{"k":"A","m":"B"}
{"x":[3,1,2,9]}{"n":10}{"n":10}
"#
    );
}
