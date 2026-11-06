//! Compiled-variable reads and assignments (`ZEND_ASSIGN` to a CV,
//! `zend_assign_to_variable`): the in-place fast forms must behave exactly
//! like the generic path — references on either side, typed references,
//! old values whose release runs a destructor, undefined sources, chained
//! results, copy-on-write sharing. Expected outputs come from the C
//! implementation.

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
fn assignments_to_compiled_variables_in_the_main_script() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~{$this->n}\n"; } }
$x = 1; $x = 2.5; $x = "s"; $x = null; $x = true; var_dump($x);
$s = "abc"; $t = $s; $s = "z"; var_dump($t, $s);
$a = $b = 5; var_dump($a, $b);
$y = 1; $r = &$y; $x = $y; $y = 2; var_dump($x, $r);
$a = 1; $r2 = &$a; $a = 3; var_dump($r2);
$o = new D(1); $o = 7; echo "after o\n";
$arr = [new D(2)]; $arr = 8; echo "after arr\n";
$x = $undef; var_dump($x);
function &f() { static $st = 1; return $st; }
$z = f(); $z++; var_dump(f());
$x = 4; $x = $x; var_dump($x);
$k = [1, 2]; $m = $k; $m[] = 3; var_dump(count($k), count($m));
$q = $p = "str"; $p = 1; var_dump($q);
$i = 10; $j = $i + 1; $i = $j * 2; var_dump($i, $j);
$f = 1.5; $g = $f + 1; var_dump($g);
$u = $x ?? 9; $v = $nope ?? 9; var_dump($u, $v);
"#),
        "bool(true)\nstring(3) \"abc\"\nstring(1) \"z\"\nint(5)\nint(5)\nint(1)\nint(2)\nint(3)\n~1\nafter o\n~2\nafter arr\n\nWarning: Undefined variable $undef in t.php on line 10\nNULL\nint(1)\nint(4)\nint(2)\nint(3)\nstring(3) \"str\"\nint(22)\nint(11)\nfloat(2.5)\nint(4)\nint(9)\n"
    );
}

#[test]
fn assignments_to_compiled_variables_in_functions_and_through_references() {
    assert_eq!(
        run(r#"<?php
class C { public int $p = 0; }
$c = new C; $r = &$c->p; $r = "5"; var_dump($c->p);
try { $r = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($c->p);
function g() { $a = 1; $b = $a; $a = "x"; return [$a, $b]; }
var_dump(g());
$fn = function () { $l = 3; $m = $l; return $m + $l; }; var_dump($fn());
$x = 1; $c2 = function () use (&$x) { $x = 5; }; $c2(); var_dump($x);
function h() { global $gv; $gv = 11; } h(); var_dump($gv);
function cnt() { static $n = 0; $n = $n + 1; return $n; } cnt(); var_dump(cnt());
$str = "a"; for ($i = 0; $i < 3; $i++) { $str = $str . $i; } var_dump($str);
$e = $w = [1]; $w[0] = 2; var_dump($e[0]);
"#),
        "int(5)\nCannot assign string to reference held by property C::$p of type int\nint(5)\narray(2) {\n  [0]=>\n  string(1) \"x\"\n  [1]=>\n  int(1)\n}\nint(6)\nint(5)\nint(11)\nint(2)\nstring(4) \"a012\"\nint(1)\n"
    );
}
