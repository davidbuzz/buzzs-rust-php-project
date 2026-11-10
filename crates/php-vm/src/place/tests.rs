//! Script-level tests of the place walks: assignments whose value is a
//! reference to a container of the place (`ZEND_ASSIGN_DIM` reads `OP_DATA`
//! after the container fetch), operands read inside the walk, and walks an
//! error handler observes (live walks). Expected output from
//! `/usr/bin/php -n` (8.3); where php-src master raises the null-offset
//! deprecation ("Using null as an array offset is deprecated") after an
//! undefined-variable offset (`slow_index_convert_w`), it is added to 8.3's
//! output.

#![allow(clippy::needless_raw_string_hashes)]

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

/// `$t = &$a; $a[] = $t;` and its variants build arrays that contain
/// themselves; `$a[] = $a` still copies (`zend_is_assign_to_self`).
#[test]
fn reference_to_container_is_read_after_the_fetch() {
    let src = r#"<?php
$arr = [1, 'x'];
$tmp = &$arr;
$arr[] = $tmp;
var_dump($arr);
echo serialize($arr), "\n";
$t =& $u;
$u[] = $t;
var_dump($u);
$x = null; $r =& $x; $x['k'][] = $r; var_dump($x);
$y = []; $ry =& $y; $z = ($y['a'] = $ry); var_dump($z);
$s = [1]; $rs =& $s; $s[] = $s; var_dump($s);
$p = [1]; $q = &$p; $o = []; $o['k'] = $q; var_dump($o);
$b = [1]; $rb =& $b; $b[] = $rb; $b[] = 2; var_dump(cnt($b), cnt($b[1]));
$a = [0]; $a[0] = &$w; $ra =& $a; $a[0] = $ra; var_dump($w);
function g(&$a) { $c = &$a; $a['me'] = $c; }
$h = ['v' => 1]; g($h); var_dump($h);
$f = false; $rf =& $f; $f[] = $rf; var_dump($f);
$m = ['s' => 'abc']; $rm =& $m; $m['s'][1] = $rm; var_dump($m);
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
"#;
    let expected = r#"array(3) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  *RECURSION*
}
a:3:{i:0;i:1;i:1;s:1:"x";i:2;N;}
array(1) {
  [0]=>
  *RECURSION*
}
array(1) {
  ["k"]=>
  array(1) {
    [0]=>
    *RECURSION*
  }
}
array(1) {
  ["a"]=>
  *RECURSION*
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  array(1) {
    [0]=>
    int(1)
  }
}
array(1) {
  ["k"]=>
  array(1) {
    [0]=>
    int(1)
  }
}
int(3)
int(2)
array(1) {
  [0]=>
  *RECURSION*
}
array(2) {
  ["v"]=>
  int(1)
  ["me"]=>
  *RECURSION*
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 18
array(1) {
  [0]=>
  *RECURSION*
}

Warning: Array to string conversion in t.php on line 19

Warning: Only the first byte will be assigned to the string offset in t.php on line 19
array(1) {
  ["s"]=>
  string(3) "aAc"
}
"#;
    assert_eq!(run(src), expected);
}

/// Undefined variables used as offsets are read when their fetch runs
/// (after the earlier fetches' diagnostics, with the null-offset
/// deprecation right after them, and not for a scalar container), and an
/// undefined assigned value once the slot or the object is there
/// (`ZEND_ASSIGN_DIM`, `ZEND_ASSIGN_OBJ` read `OP_DATA` last).
#[test]
fn undefined_offsets_and_values_are_read_inside_the_walk() {
    let src = r#"<?php
$a = [];
$a[1.5][$u][2.5] = 1;
$b[$u] = $v;
$c = [];
$c[$u]['k'] = $v;
$d = [];
$d['x'][$u] .= 'y';
$e = 'str';
try { $e[$u] = $v; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
$p = new stdClass;
$p->q[$u] = $v;
$s = 5;
try { $s[$u] = $v; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
try { $s[$u][1] = $v; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
$n = null;
try { $n->p = $v; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
try { $n[0]->p = $v; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
echo serialize([$a, $b, $c, $d, $e, $p, $n]), "\n";
"#;
    let null_offset =
        "Using null as an array offset is deprecated, use an empty string instead in t.php on line";
    let expected = format!(
        "\nDeprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 3\n\
         \nWarning: Undefined variable $u in t.php on line 3\n\
         \nDeprecated: {null_offset} 3\n\
         \nDeprecated: Implicit conversion from float 2.5 to int loses precision in t.php on line 3\n\
         \nWarning: Undefined variable $u in t.php on line 4\n\
         \nDeprecated: {null_offset} 4\n\
         \nWarning: Undefined variable $v in t.php on line 4\n\
         \nWarning: Undefined variable $u in t.php on line 6\n\
         \nDeprecated: {null_offset} 6\n\
         \nWarning: Undefined variable $v in t.php on line 6\n\
         \nWarning: Undefined array key \"x\" in t.php on line 8\n\
         \nWarning: Undefined variable $u in t.php on line 8\n\
         \nDeprecated: {null_offset} 8\n\
         \nWarning: Undefined array key \"\" in t.php on line 8\n\
         \nWarning: Undefined variable $u in t.php on line 10\n\
         \nWarning: String offset cast occurred in t.php on line 10\n\
         \nWarning: Undefined variable $v in t.php on line 10\n\
         Cannot assign an empty string to a string offset\n\
         \nWarning: Undefined variable $u in t.php on line 12\n\
         \nDeprecated: {null_offset} 12\n\
         \nWarning: Undefined variable $v in t.php on line 12\n\
         Cannot use a scalar value as an array\n\
         Cannot use a scalar value as an array\n\
         \nWarning: Undefined variable $v in t.php on line 17\n\
         Attempt to assign property \"p\" on null\n\
         \nWarning: Undefined variable $v in t.php on line 18\n\
         Attempt to assign property \"p\" on null\n\
         a:7:{{i:0;a:1:{{i:1;a:1:{{s:0:\"\";a:1:{{i:2;i:1;}}}}}}i:1;a:1:{{s:0:\"\";N;}}\
         i:2;a:1:{{s:0:\"\";a:1:{{s:1:\"k\";N;}}}}i:3;a:1:{{s:1:\"x\";a:1:{{s:0:\"\";s:1:\"y\";}}}}\
         i:4;s:3:\"str\";i:5;O:8:\"stdClass\":1:{{s:1:\"q\";a:1:{{s:0:\"\";N;}}}}i:6;a:1:{{i:0;N;}}}}\n"
    );
    assert_eq!(run(src), expected);
}

/// With an error handler set, `$a[$u] = $v` raises the offset's warning,
/// its null-offset deprecation and then the value's warning; a typed
/// reference that cannot hold an array rejects the write before either
/// operand is read.
#[test]
fn live_walk_reads_the_offset_before_the_value() {
    let src = r#"<?php
set_error_handler(function ($no, $msg) { echo "Err: $msg\n"; });
$a = [];
$a[$u] = $v;
class O { public ?int $p = null; }
$o = new O;
$r = &$o->p;
try { $r[$x] = $y; } catch (TypeError $e) { echo "TE: ", $e->getMessage(), "\n"; }
try { $r[$x][1] = $y; } catch (TypeError $e) { echo "TE: ", $e->getMessage(), "\n"; }
var_dump($a, $o->p);
"#;
    let expected = "Err: Undefined variable $u
Err: Using null as an array offset is deprecated, use an empty string instead
Err: Undefined variable $v
TE: Cannot auto-initialize an array inside a reference held by property O::$p of type ?int
TE: Cannot auto-initialize an array inside a reference held by property O::$p of type ?int
array(1) {
  [\"\"]=>
  NULL
}
NULL
";
    assert_eq!(run(src), expected);
}

/// An error handler that throws stops the operation where php-src's
/// opcodes stop: after a `false` container's conversion the interrupted
/// fetch still completes (an intermediate one adds its key, a final
/// assignment stores, a read-modify-write gives up at the missing key);
/// after an offset conversion or a missing read-modify-write key nothing is
/// added; after the assigned value's warning the null is stored.
#[test]
fn throwing_handler_stops_the_walk_where_php_src_does() {
    let src = r#"<?php
set_error_handler(function ($no, $msg) { throw new Exception($msg); });
$a = false; try { $a[1][2] = 3; } catch (Exception $e) { echo "1: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = [false]; try { $a[0][1][2] = 3; } catch (Exception $e) { echo "2: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = []; try { $a[1.5][2] = 3; } catch (Exception $e) { echo "3: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = []; try { $a[1][2.5][3] = 3; } catch (Exception $e) { echo "4: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = [1 => false]; try { $a[1][2] = 'x'; } catch (Exception $e) { echo "5: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = []; try { $a[1]['k'] .= 'x'; } catch (Exception $e) { echo "6: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = [false]; try { $a[0][1] .= 'x'; } catch (Exception $e) { echo "7: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = [false]; try { $a[0][1]++; } catch (Exception $e) { echo "8: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = [1 => 'one']; try { $a[2.5] .= 'x'; } catch (Exception $e) { echo "9: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = false; try { $r = &$a[1][2]; } catch (Exception $e) { echo "10: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
$a = []; try { $a[1][2] = $u; } catch (Exception $e) { echo "11: ", $e->getMessage(), "\n"; } echo serialize($a), "\n";
"#;
    let expected = r#"1: Automatic conversion of false to array is deprecated
a:1:{i:1;N;}
2: Automatic conversion of false to array is deprecated
a:1:{i:0;a:1:{i:1;N;}}
3: Implicit conversion from float 1.5 to int loses precision
a:0:{}
4: Implicit conversion from float 2.5 to int loses precision
a:1:{i:1;a:0:{}}
5: Automatic conversion of false to array is deprecated
a:1:{i:1;a:1:{i:2;s:1:"x";}}
6: Undefined array key 1
a:0:{}
7: Automatic conversion of false to array is deprecated
a:1:{i:0;a:0:{}}
8: Automatic conversion of false to array is deprecated
a:1:{i:0;a:0:{}}
9: Implicit conversion from float 2.5 to int loses precision
a:1:{i:1;s:3:"one";}
10: Automatic conversion of false to array is deprecated
a:1:{i:1;N;}
11: Undefined variable $u
a:1:{i:1;a:1:{i:2;N;}}
"#;
    assert_eq!(run(src), expected);
}

/// An error handler that frees the container during a fetch's diagnostic
/// gives that fetch up: the rest of the place works on a temporary (the
/// assignment's result is still the value), a final write or
/// read-modify-write does not happen, and a `false` container's fetch still
/// reads its undefined-variable offset (`return_null`).
#[test]
fn freeing_handler_gives_the_fetch_up() {
    let src = r#"<?php
set_error_handler(function ($no, $msg) { echo "Err: $msg\n"; $GLOBALS['a'] = ''; });
$a = [[['x']]]; $r = ($a[0][$d][1] = 'b'); var_dump($a, $r);
$a = [1]; $a[$g]++; var_dump($a);
$a = [false]; $a[0][$h] .= 'x'; var_dump($a);
$a = [!'a']; $a[0][$i][1] = 'b'; var_dump($a);
$a = [false]; $a[0][1] .= 'b'; var_dump($a);
$a = [false]; $a[0][1]++; var_dump($a);
$a = [[1]]; $a[0][1.5][2] = 'c'; var_dump($a);
"#;
    let expected = r#"Err: Undefined variable $d
string(0) ""
string(1) "b"
Err: Undefined variable $g
string(0) ""
Err: Automatic conversion of false to array is deprecated
string(0) ""
Err: Automatic conversion of false to array is deprecated
Err: Undefined variable $i
string(0) ""
Err: Automatic conversion of false to array is deprecated
string(0) ""
Err: Automatic conversion of false to array is deprecated
string(0) ""
Err: Implicit conversion from float 1.5 to int loses precision
string(0) ""
"#;
    assert_eq!(run(src), expected);
}

/// `zend_std_get_property_ptr_ptr` adds a missing dynamic property as null
/// before its "Undefined property" warning: when the error handler throws
/// there, the property stays, `++` and arithmetic compound assignments
/// still change it, and `.=` leaves it null; a cached array property's
/// missing key or converted offset is not added.
#[test]
fn missing_properties_and_keys_with_a_throwing_handler() {
    let src = r#"<?php
function thrower($no, $msg) { throw new Exception("handler: $msg"); }
set_error_handler('thrower');
$o = new stdClass;
try { $o->undef .= 'x'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { $o->n++; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { $o->m += 5; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($o);
class W { public $arr = ['k' => 'v']; function run() {
    try { $this->arr['zz'] .= 'q'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    try { $this->arr[2.5] .= 'q'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    var_dump($this->arr);
} }
(new W)->run();
"#;
    let expected = r#"handler: Undefined property: stdClass::$undef
handler: Undefined property: stdClass::$n
handler: Undefined property: stdClass::$m
object(stdClass)#1 (3) {
  ["undef"]=>
  NULL
  ["n"]=>
  int(1)
  ["m"]=>
  int(5)
}
handler: Undefined array key "zz"
handler: Implicit conversion from float 2.5 to int loses precision
array(1) {
  ["k"]=>
  string(1) "v"
}
"#;
    assert_eq!(run(src), expected);
}

/// `zend_fetch_dimension_address` (FETCH_DIM_W, _RW, _UNSET) makes an
/// array of a referenced `false` silently; `ZEND_ASSIGN_DIM` and
/// `ZEND_ASSIGN_DIM_OP` raise the deprecation. `ZEND_ASSIGN_DIM_OP` does not
/// check a typed reference (FETCH_DIM_RW does), and the write-back of `++`
/// through a property reports the offset conversion once.
#[test]
fn reference_containers_and_incdec_write_backs() {
    let src = r#"<?php
$a = false; $r = &$a; $a[1][2] = 3;
$b = false; $r = &$b; $b[1] = 3;
$c = false; $r = &$c; $c[1] .= 'x';
$d = false; $r = &$d; $d[1]['x'] .= 'x';
$e = false; $r = &$e; $e[1]++;
$f = false; $r = &$f; $q = &$f[1];
$g = [false]; $r = &$g[0]; $g[0][1][2] = 3;
$h = false; $r = &$h; unset($h[1][2]);
$i = false; $r = &$i; $i[] = 1;
$j = false; $r = &$j; $j[][1] = 1;
echo serialize([$a, $b, $c, $d, $e, $f, $g, $h, $i, $j]), "\n";
class O { public ?int $p = null; }
$o = new O; $rp = &$o->p;
try { $rp['k'] .= 'x'; } catch (TypeError $ex) { echo $ex->getMessage(), "\n"; }
var_dump($o->p);
$o2 = new O; $rq = &$o2->p;
try { $rq['j']++; } catch (TypeError $ex) { echo $ex->getMessage(), "\n"; }
var_dump($o2->p);
class C { public $a = [1 => 1]; function f() { $this->a[1.5]++; var_dump($this->a); } }
(new C)->f();
$o = new stdClass; $o->a = [1 => 1];
$o->a[1.5]++;
var_dump($o->a);
"#;
    let expected = r#"
Deprecated: Automatic conversion of false to array is deprecated in t.php on line 3

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 4

Warning: Undefined array key 1 in t.php on line 4

Warning: Undefined array key 1 in t.php on line 5

Warning: Undefined array key "x" in t.php on line 5

Warning: Undefined array key 1 in t.php on line 6

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 10
a:10:{i:0;a:1:{i:1;a:1:{i:2;i:3;}}i:1;a:1:{i:1;i:3;}i:2;a:1:{i:1;s:1:"x";}i:3;a:1:{i:1;a:1:{s:1:"x";s:1:"x";}}i:4;a:1:{i:1;i:1;}i:5;a:1:{i:1;N;}i:6;a:1:{i:0;a:1:{i:1;a:1:{i:2;i:3;}}}i:7;b:0;i:8;a:1:{i:0;i:1;}i:9;a:1:{i:0;a:1:{i:1;i:1;}}}

Warning: Undefined array key "k" in t.php on line 15
array(1) {
  ["k"]=>
  string(1) "x"
}
Cannot auto-initialize an array inside a reference held by property O::$p of type ?int
NULL

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 20
array(1) {
  [1]=>
  int(2)
}

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 23
array(1) {
  [1]=>
  int(2)
}
"#;
    assert_eq!(run(src), expected);
}

/// bug79793: an error handler that attaches or syncs the globals symbol
/// table (`global $x`, `$GLOBALS` writes, a by-reference capture of a
/// global) during the "Undefined array key" warning of a global-scope
/// read-modify-write neither drops the write nor overwrites the array with
/// the frame's stale copy; a handler that really frees or captures the
/// array still abandons it.
#[test]
fn symtab_sync_in_handler_keeps_global_rw_write() {
    let src = r#"<?php
$key = "foo";
$key .= "bar";
set_error_handler(function ($_, $m) use (&$key) { echo "$m\n"; $key .= "baz"; });
$ary = [];
$ary[$key]++;
$ary[$key] += 1;
foreach ($ary as $k => $v) { echo "$k=$v;"; } echo "\n";
set_error_handler(function () { global $z; $z = 1; });
$a = []; $a["k"]++; $a["j"] .= "x";
foreach ($a as $k => $v) { echo "$k=$v;"; } echo "\n";
set_error_handler(function () { $GLOBALS['b'] = $GLOBALS['a']; });
$a = []; $a["k"]++;
foreach ([$a, $b] as $v) { echo "[", isset($v["k"]) ? "k" : "", "]"; } echo "\n";
set_error_handler(function () { global $a; $a = 1; });
$a = []; $a["k"]++;
var_dump($a);
"#;
    let expected = "Undefined array key \"foobar\"\n\
Undefined array key \"foobarbaz\"\n\
foobar=1;foobarbaz=1;\n\
k=1;j=x;\n\
[][]\n\
int(1)\n";
    assert_eq!(run(src), expected);
}

/// `place op= $undef` (`ZEND_ASSIGN_DIM_OP`) reads `OP_DATA` once it has
/// the slot: after the container's, the offsets' and the missing key's
/// diagnostics; an object reads it before `offsetGet`; a scalar or string
/// container throws without reading it (opcache/tests/jit/assign_dim_010).
#[test]
fn assign_dim_op_reads_undefined_value_at_the_slot() {
    let src = r#"<?php
set_error_handler(function ($n, $m) { echo "$m\n"; });
class AO implements ArrayAccess { public $a = []; function offsetExists($k): bool { return true; } function offsetGet($k): mixed { echo "get $k\n"; return $this->a[$k] ?? null; } function offsetSet($k, $v): void { echo "set $k\n"; $this->a[$k] = $v; } function offsetUnset($k): void {} }
function t() { $a = []; $a['x'] += $y; var_dump($a); }
t();
function u() { $a[1][2] .= $y; var_dump($a); }
u();
function v() { for ($i = 0; $i < 2; $i++) { $a[] &= $y; $a = null; } var_dump($a); }
v();
function w() { $o = new AO; $o['k'] -= $y; var_dump($o['k']); }
w();
function x() { $s = 5; try { $s['q'] += $y; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
x();
function z() { $s = "ab"; try { $s[] .= $y; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
z();
"#;
    let expected = "Undefined array key \"x\"\n\
Undefined variable $y\n\
array(1) {\n\
  [\"x\"]=>\n\
  int(0)\n\
}\n\
Undefined variable $a\n\
Undefined array key 1\n\
Undefined array key 2\n\
Undefined variable $y\n\
array(1) {\n\
  [1]=>\n\
  array(1) {\n\
    [2]=>\n\
    string(0) \"\"\n\
  }\n\
}\n\
Undefined variable $a\n\
Undefined variable $y\n\
Undefined variable $y\n\
NULL\n\
Undefined variable $y\n\
get k\n\
set k\n\
get k\n\
int(0)\n\
Cannot use a scalar value as an array\n\
[] operator not supported for strings\n\
";
    assert_eq!(run(src), expected);
}

/// An error handler that throws at the undefined container of a
/// read-modify-write leaves an empty array, and an illegal offset still
/// throws, chained after the handler's exception (opcache/tests/jit/
/// fetch_dim_rw_004). The handler, called from user code, reports its
/// argument errors with "called in"; called from a builtin, without.
#[test]
fn rw_fetch_after_a_throwing_handler_and_handler_call_site() {
    let src = r#"<?php
set_error_handler(function (int $n, string $m) { echo "$m\n"; if ($m[0] === 'U') throw new Exception("h"); });
function a() { try { $y['k']++; } catch (Throwable $e) { echo get_class($e), ':', $e->getMessage(), ':', $e->getPrevious()?->getMessage(), "\n"; } var_dump($y); }
a();
function b() { $k = []; try { $y[$k]++; } catch (Throwable $e) { echo get_class($e), ':', $e->getMessage(), ':', $e->getPrevious()?->getMessage(), "\n"; } var_dump($y); }
b();
function c() { try { $y[[]] .= 'a'; } catch (Throwable $e) { echo get_class($e), ':', $e->getMessage(), ':', $e->getPrevious()?->getMessage(), "\n"; } var_dump($y); }
c();
set_error_handler(function (stdClass $n) {});
function d() { try { echo $u; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } }
d();
try { trigger_error("x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    let expected = "Undefined variable $y\n\
Exception:h:\n\
array(0) {\n\
}\n\
Undefined variable $y\n\
TypeError:Cannot access offset of type array on array:h\n\
array(0) {\n\
}\n\
Undefined variable $y\n\
TypeError:Cannot access offset of type array on array:h\n\
array(0) {\n\
}\n\
{closure:t.php:9}(): Argument #1 ($n) must be of type stdClass, int given, called in t.php on line 10\n\
{closure:t.php:9}(): Argument #1 ($n) must be of type stdClass, int given\n\
";
    assert_eq!(run(src), expected);
}
