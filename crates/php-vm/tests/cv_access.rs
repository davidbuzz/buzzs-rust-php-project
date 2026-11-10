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
$k = [1, 2]; $m = $k; $m[] = 3; var_dump(cnt($k), cnt($m));
$q = $p = "str"; $p = 1; var_dump($q);
$i = 10; $j = $i + 1; $i = $j * 2; var_dump($i, $j);
$f = 1.5; $g = $f + 1; var_dump($g);
$u = $x ?? 9; $v = $nope ?? 9; var_dump($u, $v);
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
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

/// `??`/`?->` on variables holding references, `RECV` stores, repeated
/// `global`/`static` bindings (the old slot value is a reference) and
/// `foreach` value variables: old values are released, and destructed,
/// exactly when the generic path releases them.
#[test]
fn variable_reads_and_rebindings_release_values_at_the_same_points() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~{$this->n}\n"; } }
$v = null; $r = &$v; var_dump($r ?? "dflt"); $v = 0; var_dump($r ?? "dflt");
$o = null; $ro = &$o; var_dump($ro?->p); $o = new D(1); $o->p = 5; var_dump($ro?->p); $o = null; echo "o gone\n";
function take($a, $b = 2) { echo "in take\n"; return $a->n + $b; }
var_dump(take(new D(2))); echo "after take\n";
function gl() { global $g; global $g; $g = new D(3); } gl(); $g = 1; echo "g reset\n";
function st() { static $s = 0; $s++; return $s; } st(); var_dump(st());
foreach ([new D(4), new D(5)] as $it) { echo "it {$it->n}\n"; } echo "loop done\n"; $it = 0;
$list = [1, 2, 3]; $sum = 0; foreach ($list as $k => $e) { $sum = $sum + $e * $k; } var_dump($sum);
$u = "a"; $w = &$u; $copy = $u; $w = "b"; var_dump($copy, $u);
"#),
        "string(4) \"dflt\"\nint(0)\nNULL\n\nDeprecated: Creation of dynamic property D::$p is deprecated in t.php on line 4\nint(5)\n~1\no gone\nin take\n~2\nint(4)\nafter take\n~3\ng reset\nint(2)\nit 4\nit 5\n~4\nloop done\n~5\nint(8)\nstring(1) \"a\"\nstring(1) \"b\"\n"
    );
}

/// `JMPZ`/`JMPNZ` on a compiled variable: bools, null and ints are tested
/// in place; strings, floats behind references, arrays and undefined
/// variables (warning) keep the generic conversion.
#[test]
fn conditions_on_compiled_variables() {
    assert_eq!(
        run(r#"<?php
$f = 0; if ($f) { echo "no\n"; } else { echo "zero\n"; }
$f = 3; while ($f) { echo $f--; } echo "\n";
$b = true; if (!$b) { echo "no\n"; } if ($b) { echo "yes\n"; }
$n = null; echo $n ? "t" : "f", "\n";
$s = "0"; if ($s) { echo "no\n"; } else { echo "str0\n"; }
$x = 1.0; $r = &$x; if ($r) { echo "ref\n"; }
if ($nope) { echo "no\n"; } else { echo "undef\n"; }
$a = []; if ($a) { echo "no\n"; } else { echo "empty\n"; }
"#),
        "zero\n321\nyes\nf\nstr0\nref\n\nWarning: Undefined variable $nope in t.php on line 8\nundef\nempty\n"
    );
}

/// `f($cv)` to a by-reference parameter (`ZEND_SEND_REF`) turns the CV slot
/// into a reference in place, without the symbol table: undefined (a
/// reference to null, no notice), already a reference, copy-on-write shared,
/// static, in the main script (seen through `$GLOBALS`), and builtins'
/// by-reference parameters. Expected output from the C implementation.
#[test]
fn by_reference_arguments_of_compiled_variables() {
    assert_eq!(
        run(r#"<?php
function f(&$a) { $a[] = 1; }
function g() {
    f($u); var_dump($u);
    $v = [5]; $w = &$v; f($w); var_dump(count($v));
    $s = "x"; $t = $s; f2($s); var_dump($s, $t);
    static $st = []; f($st); var_dump(count($st));
    $x = [1]; f($x); var_dump(get_defined_vars()['x']);
    $n = null; settype($n, "array"); var_dump($n);
}
function f2(&$a) { $a .= "y"; }
g(); g();
$m = 1; f2($m); var_dump($GLOBALS['m']);
f($fresh); var_dump($GLOBALS['fresh']);
"#),
        r#"array(1) {
  [0]=>
  int(1)
}
int(2)
string(2) "xy"
string(1) "x"
int(1)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(1)
}
array(0) {
}
array(1) {
  [0]=>
  int(1)
}
int(2)
string(2) "xy"
string(1) "x"
int(2)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(1)
}
array(0) {
}
string(2) "1y"
array(1) {
  [0]=>
  int(1)
}
"#
    );
}
