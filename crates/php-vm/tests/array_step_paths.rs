// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The tape's array-element and `foreach` instructions on their own short
//! paths (the reset of a loop over a variable's array, the step at the
//! loop's end, the free of the iterator, element writes and `isset` by a
//! string key, element writes through a property) next to the cases that
//! must keep their general paths: holes, references, symbol tables,
//! copies taken at reset, destructors. Each loop runs more than once so
//! that a later round runs on the tape. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_FE_RESET_R`, `ZEND_FE_FETCH_R`, `ZEND_FE_FREE`, `ZEND_ASSIGN_DIM`,
//! `ZEND_ISSET_ISEMPTY_DIM_OBJ`, `ZEND_ASSIGN_OBJ` of a dimension).

#![allow(clippy::needless_raw_string_hashes)]

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

/// `foreach` by value over empty arrays, hashes and lists with holes at
/// the end, an array grown inside its own loop, a variable behind a
/// reference, nested loops over one array, an iterator holding the last
/// handle of objects with destructors, literals and temporaries, a frame
/// with a symbol table, and loop variables that held other values.
#[test]
fn foreach_reset_end_and_free() {
    assert_eq!(run(FOREACH), FOREACH_OUT);
}

const FOREACH: &str = r#"<?php
class D { public function __construct(public $n) {} public function __destruct() { echo "d{$this->n} "; } }
function f() {
    for ($round = 0; $round < 3; $round++) {
        $empty = [];
        foreach ($empty as $k => $v) { echo "never"; }
        foreach ($empty as $v) { echo "never"; }
        $a = ['a' => 1, 'b' => 'two', 'c' => 3.5];
        foreach ($a as $k => $v) { echo "$k=$v "; }
        foreach ($a as $v) { echo "$v "; }
        // Holes at the end: positions left, but no element.
        $h = ['x' => 1, 'y' => 2, 'z' => 3];
        unset($h['y'], $h['z']);
        foreach ($h as $k => $v) { echo "$k=$v "; }
        $l = [10, 20, 30];
        unset($l[2]);
        foreach ($l as $k => $v) { echo "$k=$v "; }
        // The loop iterates the copy taken at reset.
        $m = [1, 2];
        foreach ($m as $k => $v) { $m[] = $v * 10; echo "$k:$v "; }
        echo count($m), " ";
        // Over a reference.
        $r = [5, 6];
        $alias = &$r;
        foreach ($r as $k => $v) { echo "$k/$v "; }
        unset($alias);
        // Nested over the same array.
        foreach ($a as $k1 => $v1) { foreach ($a as $k2 => $v2) { echo $k1, $k2, ' '; } }
        // The iterator's handle is the last one: freed at the loop's end.
        $objs = [new D($round), new D($round + 10)];
        foreach ($objs as $o) { }
        unset($o);
        echo "| ";
        $objs = null;
        echo "after\n";
        // A literal and a temporary.
        foreach ([1, 2] as $v) { echo $v; }
        foreach (array_reverse($a) as $k => $v) { echo $k; }
        echo "\n";
    }
}
f();
function g() {
    $name = 'arr';
    $$name = ['p' => 1, 'q' => 2];
    foreach ($arr as $k => $v) { echo "$k=$v "; }
    foreach ($arr as $v) { echo "$v "; }
    echo "\n";
}
g(); g();
function h() {
    $a = ['k' => 1];
    $k = 'old';
    $v = [1, 2];
    foreach ($a as $k => $v) { }
    var_dump($k, $v);
    $x = null;
    foreach ($a as $x) {}
    var_dump($x);
}
h();
"#;

const FOREACH_OUT: &str = r#"a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d0 d10 after
12cba
a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d1 d11 after
12cba
a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d2 d12 after
12cba
p=1 q=2 1 2 
p=1 q=2 1 2 
string(1) "k"
int(1)
int(1)
"#;

/// `$a['key'] = value` with string literals for the key: new and existing
/// keys, integers in digits and strings that only look like them, string
/// values over strings, a copy that keeps its values, a list given a
/// string key, a table growing, an element that is a reference or an
/// object with a destructor, an array behind a reference, and containers
/// that are no arrays (`ArrayAccess`, strings, null, undefined, false,
/// an int), with an undefined value variable.
#[test]
fn dim_write_by_literal_key() {
    assert_eq!(run(DIM_WRITE), DIM_WRITE_OUT);
}

const DIM_WRITE: &str = r#"<?php
class D { public function __construct(public $n) {} public function __destruct() { echo "d{$this->n} "; } }
class AA implements ArrayAccess {
    public function offsetExists($o): bool { echo "exists($o) "; return true; }
    public function offsetGet($o): mixed { echo "get($o) "; return 1; }
    public function offsetSet($o, $v): void { echo "set($o,$v) "; }
    public function offsetUnset($o): void { echo "unset($o) "; }
}
function f() {
    for ($round = 0; $round < 3; $round++) {
        $a = ['x' => 0];
        $s = "str$round";
        $a['key'] = $round;
        $a['key'] = 1.5;
        $a['name'] = $s;
        $a['name'] = 'lit';
        $a['name'] = $s;
        $a['5'] = 'five';
        $a['-3'] = 'minus';
        $a['05'] = 'zero-five';
        $a[''] = 'empty';
        $a['flag'] = true;
        $a['nul'] = null;
        var_dump($a);
        // Copy on write: the copy keeps its values.
        $b = $a;
        $a['key'] = 'changed';
        echo $b['key'], ' ', $a['key'], "\n";
        // A list gets a string key.
        $l = [1, 2];
        $l['k'] = 3;
        echo json_encode($l), "\n";
        // Growth past the table's room.
        $g = [];
        $g['a'] = 1; $g['b'] = 2; $g['c'] = 3; $g['d'] = 4; $g['e'] = 5; $g['f'] = 6; $g['g'] = 7; $g['h'] = 8; $g['i'] = 9;
        echo implode(',', array_keys($g)), "\n";
        // An element that is a reference is written through.
        $t = 1;
        $r = ['ref' => 0];
        $r['ref'] = &$t;
        $r['ref'] = 42;
        echo $t, "\n";
        // An element that is an object: its destructor runs at the write.
        $o = ['obj' => new D($round)];
        echo "before ";
        $o['obj'] = 7;
        echo "after\n";
        // Through a reference to the array.
        $arr = ['p' => 1];
        $ra = &$arr;
        $arr['p'] = 2;
        $arr['q'] = 3;
        echo json_encode($ra), "\n";
        unset($ra);
        // Containers that are no arrays.
        $obj = new AA;
        $obj['key'] = 5;
        $str = 'abc';
        try { $str['key'] = 'x'; } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        $u = null;
        $u['key'] = 1;
        echo json_encode($u), "\n";
        unset($nv);
        $nv['key'] = 2;
        echo json_encode($nv), "\n";
        $fl = false;
        $fl['key'] = 3;
        echo json_encode($fl), "\n";
        $int = 5;
        try { $int['key'] = 4; } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        // An undefined value variable.
        $w = [];
        $w['k'] = $undefined;
        var_dump($w);
    }
}
f();
"#;

const DIM_WRITE_OUT: &str = r#"array(9) {
  ["x"]=>
  int(0)
  ["key"]=>
  float(1.5)
  ["name"]=>
  string(4) "str0"
  [5]=>
  string(4) "five"
  [-3]=>
  string(5) "minus"
  ["05"]=>
  string(9) "zero-five"
  [""]=>
  string(5) "empty"
  ["flag"]=>
  bool(true)
  ["nul"]=>
  NULL
}
1.5 changed
{"0":1,"1":2,"k":3}
a,b,c,d,e,f,g,h,i
42
before d0 after
{"p":2,"q":3}
set(key,5) TypeError: Cannot access offset of type string on string
{"key":1}
{"key":2}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 67
{"key":3}
Error: Cannot use a scalar value as an array

Warning: Undefined variable $undefined in t.php on line 73
array(1) {
  ["k"]=>
  NULL
}
array(9) {
  ["x"]=>
  int(0)
  ["key"]=>
  float(1.5)
  ["name"]=>
  string(4) "str1"
  [5]=>
  string(4) "five"
  [-3]=>
  string(5) "minus"
  ["05"]=>
  string(9) "zero-five"
  [""]=>
  string(5) "empty"
  ["flag"]=>
  bool(true)
  ["nul"]=>
  NULL
}
1.5 changed
{"0":1,"1":2,"k":3}
a,b,c,d,e,f,g,h,i
42
before d1 after
{"p":2,"q":3}
set(key,5) TypeError: Cannot access offset of type string on string
{"key":1}
{"key":2}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 67
{"key":3}
Error: Cannot use a scalar value as an array

Warning: Undefined variable $undefined in t.php on line 73
array(1) {
  ["k"]=>
  NULL
}
array(9) {
  ["x"]=>
  int(0)
  ["key"]=>
  float(1.5)
  ["name"]=>
  string(4) "str2"
  [5]=>
  string(4) "five"
  [-3]=>
  string(5) "minus"
  ["05"]=>
  string(9) "zero-five"
  [""]=>
  string(5) "empty"
  ["flag"]=>
  bool(true)
  ["nul"]=>
  NULL
}
1.5 changed
{"0":1,"1":2,"k":3}
a,b,c,d,e,f,g,h,i
42
before d2 after
{"p":2,"q":3}
set(key,5) TypeError: Cannot access offset of type string on string
{"key":1}
{"key":2}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 67
{"key":3}
Error: Cannot use a scalar value as an array

Warning: Undefined variable $undefined in t.php on line 73
array(1) {
  ["k"]=>
  NULL
}
"#;

/// `$obj->arr['key'] = value` on property cache hits: new and existing
/// string keys, integers in digits, an append, a typed property, a class
/// default that keeps its value, a copy that keeps its values, an element
/// that is a reference, a property that is a reference, an object element
/// released with its destructor, null and string properties, dynamic and
/// magic properties, float, bool and null keys.
#[test]
fn property_element_writes() {
    assert_eq!(run(PROP_DIM_WRITE), PROP_DIM_WRITE_OUT);
}

const PROP_DIM_WRITE: &str = r#"<?php
class D { public function __construct(public $n) {} public function __destruct() { echo "d{$this->n} "; } }
class C {
    public $arr = ['k' => 1];
    public array $typed = [];
    public ?array $maybe = null;
    public $str = 'abc';
    public $ref;
    public function set($i) {
        $this->arr['k'] = $i;
        $this->arr['new'] = "v$i";
        $this->arr['7'] = 'seven';
        $this->arr[''] = 'empty';
        $this->typed['t'] = $i * 2;
        $this->arr[] = 'pushed';
    }
}
#[AllowDynamicProperties]
class Dyn {}
class Magic {
    private $data = [];
    public function __get($n) { echo "get($n) "; return $this->data[$n] ?? []; }
    public function __set($n, $v) { echo "set($n) "; $this->data[$n] = $v; }
}
function f() {
    for ($round = 0; $round < 3; $round++) {
        $o = new C;
        $o->set($round);
        $o->arr['k'] = 'outside';
        var_dump($o->arr, $o->typed);
        // The class default keeps its value.
        $p = new C;
        echo json_encode($p->arr), "\n";
        // A copy of the property's array keeps its values.
        $copy = $o->arr;
        $o->arr['k'] = 'again';
        echo $copy['k'], ' ', $o->arr['k'], "\n";
        // An element that is a reference is written through.
        $t = 0;
        $o->arr['r'] = &$t;
        $o->arr['r'] = 99;
        echo $t, "\n";
        // A property that is a reference to an array.
        $outer = ['x' => 1];
        $o->ref = &$outer;
        $o->ref['x'] = 2;
        $o->ref['y'] = 3;
        echo json_encode($outer), "\n";
        // An object element released by the write: its destructor runs.
        $o->arr['obj'] = new D($round);
        echo "before ";
        $o->arr['obj'] = 1;
        echo "after\n";
        // null becomes an array; a string property throws.
        $o->maybe['m'] = 1;
        echo json_encode($o->maybe), "\n";
        try { $o->str['k'] = 'x'; } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        // Dynamic and magic properties.
        $d = new Dyn;
        $d->list['a'] = 1;
        $d->list['b'] = 2;
        echo json_encode($d), "\n";
        $m = new Magic;
        $m->items['a'] = 1;
        echo "\n";
        // A typed property that would not accept the change.
        try { $o->typed = null; } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
        // A float key, a bool key, a null key.
        $o->arr[1.5] = 'f';
        $o->arr[true] = 'b';
        $o->arr[null] = 'n';
        echo json_encode($o->arr), "\n";
    }
}
f();
"#;

const PROP_DIM_WRITE_OUT: &str = r#"array(5) {
  ["k"]=>
  string(7) "outside"
  ["new"]=>
  string(2) "v0"
  [7]=>
  string(5) "seven"
  [""]=>
  string(5) "empty"
  [8]=>
  string(6) "pushed"
}
array(1) {
  ["t"]=>
  int(0)
}
{"k":1}
outside again
99
{"x":2,"y":3}
before d0 after
{"m":1}
TypeError: Cannot access offset of type string on string
{"list":{"a":1,"b":2}}
get(items) 
Notice: Indirect modification of overloaded property Magic::$items has no effect in t.php on line 64

Cannot assign null to property C::$typed of type array

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 69

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 71
{"k":"again","new":"v0","7":"seven","":"n","8":"pushed","r":99,"obj":1,"1":"b"}
array(5) {
  ["k"]=>
  string(7) "outside"
  ["new"]=>
  string(2) "v1"
  [7]=>
  string(5) "seven"
  [""]=>
  string(5) "empty"
  [8]=>
  string(6) "pushed"
}
array(1) {
  ["t"]=>
  int(2)
}
{"k":1}
outside again
99
{"x":2,"y":3}
before d1 after
{"m":1}
TypeError: Cannot access offset of type string on string
{"list":{"a":1,"b":2}}
get(items) 
Notice: Indirect modification of overloaded property Magic::$items has no effect in t.php on line 64

Cannot assign null to property C::$typed of type array

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 69

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 71
{"k":"again","new":"v1","7":"seven","":"n","8":"pushed","r":99,"obj":1,"1":"b"}
array(5) {
  ["k"]=>
  string(7) "outside"
  ["new"]=>
  string(2) "v2"
  [7]=>
  string(5) "seven"
  [""]=>
  string(5) "empty"
  [8]=>
  string(6) "pushed"
}
array(1) {
  ["t"]=>
  int(4)
}
{"k":1}
outside again
99
{"x":2,"y":3}
before d2 after
{"m":1}
TypeError: Cannot access offset of type string on string
{"list":{"a":1,"b":2}}
get(items) 
Notice: Indirect modification of overloaded property Magic::$items has no effect in t.php on line 64

Cannot assign null to property C::$typed of type array

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 69

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 71
{"k":"again","new":"v2","7":"seven","":"n","8":"pushed","r":99,"obj":1,"1":"b"}
"#;

/// `isset($a['key'])` and `empty($a['key'])` with string literals over
/// every kind of element (null, falsy scalars, arrays, objects, a
/// reference), keys in digits, lists and empty tables, strings, null and
/// undefined containers, `ArrayAccess`, an array behind a reference, and
/// a key deleted and added again.
#[test]
fn isset_by_literal_key() {
    assert_eq!(run(ISSET_LIT), ISSET_LIT_OUT);
}

const ISSET_LIT: &str = r#"<?php
class AA implements ArrayAccess {
    public function offsetExists($o): bool { echo "exists($o) "; return $o === 'key'; }
    public function offsetGet($o): mixed { echo "get($o) "; return 0; }
    public function offsetSet($o, $v): void {}
    public function offsetUnset($o): void {}
}
function f() {
    for ($round = 0; $round < 3; $round++) {
        $a = ['key' => 1, 'nul' => null, 'zero' => 0, 'str0' => '0', 'empty' => '', 'arr' => [], 'full' => [1], 'f' => 0.0, 'obj' => new stdClass, 'no' => false, 5 => 'five'];
        $t = null;
        $a['ref'] = &$t;
        foreach (['key', 'nul', 'zero', 'str0', 'empty', 'arr', 'full', 'f', 'obj', 'no', 'ref', 'missing'] as $k) {}
        var_dump(isset($a['key']), isset($a['nul']), isset($a['zero']), isset($a['missing']), isset($a['ref']), isset($a['obj']));
        var_dump(empty($a['key']), empty($a['nul']), empty($a['zero']), empty($a['str0']), empty($a['empty']), empty($a['arr']), empty($a['full']), empty($a['f']), empty($a['obj']), empty($a['no']), empty($a['ref']), empty($a['missing']));
        $t = 'set';
        var_dump(isset($a['ref']), empty($a['ref']));
        // Digits: the int key.
        var_dump(isset($a['5']), isset($a['05']), isset($a['-0']));
        // A list has no string key.
        $l = [1, 2, 3];
        var_dump(isset($l['key']), empty($l['key']), isset($l['1']));
        $e = [];
        var_dump(isset($e['key']), empty($e['key']));
        // Other containers.
        $s = 'abc';
        var_dump(isset($s['key']), isset($s['1']), empty($s['key']));
        $n = null;
        var_dump(isset($n['key']), empty($n['key']));
        var_dump(isset($undef['key']), empty($undef['key']));
        $o = new AA;
        var_dump(isset($o['key']), empty($o['key']), isset($o['other']));
        // Through a reference.
        $arr = ['key' => 1];
        $r = &$arr;
        var_dump(isset($arr['key']), empty($arr['key']), isset($arr['nope']));
        unset($r);
        // Deleted and added again.
        $d = ['key' => 1, 'x' => 2];
        unset($d['key']);
        var_dump(isset($d['key']));
        $d['key'] = 3;
        var_dump(isset($d['key']), empty($d['key']));
    }
}
f();
"#;

const ISSET_LIT_OUT: &str = r#"bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
exists(key) exists(key) get(key) exists(other) bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
exists(key) exists(key) get(key) exists(other) bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(true)
exists(key) exists(key) get(key) exists(other) bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
"#;

/// `$a[$key] ?? d` with literal, variable and built keys that are there,
/// missing, null, strings, arrays, objects and references, integers in
/// digits, lists, empty tables, null, undefined and string containers,
/// `ArrayAccess`, an array behind a reference, nested offsets, results
/// into temporaries that held strings, and a plain read's warning.
#[test]
fn coalesce_of_an_element() {
    assert_eq!(run(COALESCE), COALESCE_OUT);
}

const COALESCE: &str = r#"<?php
class AA implements ArrayAccess {
    public function offsetExists($o): bool { echo "exists($o) "; return $o === 'key'; }
    public function offsetGet($o): mixed { echo "get($o) "; return 'aa'; }
    public function offsetSet($o, $v): void {}
    public function offsetUnset($o): void {}
}
function f() {
    for ($round = 0; $round < 3; $round++) {
        $a = ['key' => 1, 'nul' => null, 's' => "str$round", 'arr' => [1, 2], 'obj' => new stdClass, 5 => 'five'];
        $t = 'through';
        $a['ref'] = &$t;
        $k = 'key';
        $miss = 'nope';
        echo json_encode([$a['key'] ?? 'd', $a['nope'] ?? 'd', $a['nul'] ?? 'd', $a['s'] ?? 'd', $a['arr'] ?? 'd', $a['ref'] ?? 'd', $a['5'] ?? 'd', $a['05'] ?? 'd']), "\n";
        echo json_encode([$a[$k] ?? 'd', $a[$miss] ?? 'd', $a[$k . ''] ?? 'd', $a['n' . 'ope'] ?? 'd', $a[5] ?? 'd', $a[6] ?? 'd']), "\n";
        echo get_class($a['obj'] ?? null), "\n";
        $l = [1, 2];
        echo json_encode([$l['key'] ?? 'd', $l['1'] ?? 'd', $l[1] ?? 'd']), "\n";
        $e = [];
        echo $e['key'] ?? 'd', "\n";
        $n = null;
        echo $n['key'] ?? 'd', ' ', $undef['key'] ?? 'd', "\n";
        $s = 'abc';
        echo $s['key'] ?? 'd', ' ', $s['1'] ?? 'd', ' ', $s[1] ?? 'd', "\n";
        $o = new AA;
        echo $o['key'] ?? 'd', ' ', $o['other'] ?? 'd', "\n";
        $arr = ['key' => 'byref'];
        $r = &$arr;
        echo $arr['key'] ?? 'd', ' ', $arr['nope'] ?? 'd', "\n";
        unset($r);
        $nested = ['a' => ['b' => 1]];
        echo $nested['a']['b'] ?? 'd', ' ', $nested['a']['x'] ?? 'd', ' ', $nested['x']['b'] ?? 'd', "\n";
        // The result into a temporary that held a string.
        $out = ($a['s'] ?? 'd') . ($a['nope'] ?? 'e') . ($a['s'] ?? 'f');
        echo $out, "\n";
        // A plain read that misses still warns.
        echo $a['nope'], "|\n";
    }
}
f();
"#;

const COALESCE_OUT: &str = r#"[1,"d","d","str0",[1,2],"through","five","d"]
[1,"d",1,"d","five","d"]
stdClass
["d",2,2]
d
d d
d b b
exists(key) get(key) aa exists(other) d
byref d
1 d d
str0estr0

Warning: Undefined array key "nope" in t.php on line 38
|
[1,"d","d","str1",[1,2],"through","five","d"]
[1,"d",1,"d","five","d"]
stdClass
["d",2,2]
d
d d
d b b
exists(key) get(key) aa exists(other) d
byref d
1 d d
str1estr1

Warning: Undefined array key "nope" in t.php on line 38
|
[1,"d","d","str2",[1,2],"through","five","d"]
[1,"d",1,"d","five","d"]
stdClass
["d",2,2]
d
d d
d b b
exists(key) get(key) aa exists(other) d
byref d
1 d d
str2estr2

Warning: Undefined array key "nope" in t.php on line 38
|
"#;

/// `isset($a[$k])` and `empty($a[$k])` with keys in variables and
/// temporaries: strings, integers in digits and not, ints (negative, past
/// a list's end), floats, bools and null, over a hash and a list, a list
/// with a hole, an undefined key variable.
#[test]
fn isset_by_variable_and_int_keys() {
    assert_eq!(run(ISSET_VAR), ISSET_VAR_OUT);
}

const ISSET_VAR: &str = r#"<?php
function f() {
    for ($round = 0; $round < 3; $round++) {
        $a = ['key' => 1, 'nul' => null, 'zero' => 0, 7 => 'seven', -2 => 'minus', 9 => null];
        $l = [10, 0, null, ''];
        $keys = ['key', 'nul', 'zero', 'nope', 7, -2, 9, 8, '7', '07', 1.5, true, null, 0, 1, 2, 3, 4, -1];
        foreach ($keys as $k) {
            echo var_export($k, true), ': ', json_encode([isset($a[$k]), empty($a[$k]), isset($l[$k]), empty($l[$k])]), "\n";
        }
        $p = 'ke';
        echo json_encode([isset($a[$p . 'y']), empty($a[$p . 'y']), isset($a[$p . 'x']), isset($l[$round % 2]), isset($a[$round + 7])]), "\n";
        // A key variable that is undefined.
        echo json_encode(isset($a[$undefinedKey])), "\n";
        // Holes in a list.
        $h = [1, 2, 3];
        unset($h[1]);
        $i = 1;
        echo json_encode([isset($h[$i]), isset($h[0]), empty($h[2])]), "\n";
    }
}
f();
"#;

const ISSET_VAR_OUT: &str = r#"'key': [true,false,false,true]
'nul': [false,true,false,true]
'zero': [true,true,false,true]
'nope': [false,true,false,true]
7: [true,false,false,true]
-2: [true,false,false,true]
9: [false,true,false,true]
8: [false,true,false,true]
'7': [true,false,false,true]
'07': [false,true,false,true]
1.5: 
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8
[false,true,true,true]
true: [false,true,true,true]
NULL: 
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8
[false,true,false,true]
0: [false,true,true,false]
1: [false,true,true,true]
2: [false,true,false,true]
3: [false,true,true,true]
4: [false,true,false,true]
-1: [false,true,false,true]
[true,false,false,true,true]

Warning: Undefined variable $undefinedKey in t.php on line 13

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 13
false
[false,true,false]
'key': [true,false,false,true]
'nul': [false,true,false,true]
'zero': [true,true,false,true]
'nope': [false,true,false,true]
7: [true,false,false,true]
-2: [true,false,false,true]
9: [false,true,false,true]
8: [false,true,false,true]
'7': [true,false,false,true]
'07': [false,true,false,true]
1.5: 
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8
[false,true,true,true]
true: [false,true,true,true]
NULL: 
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8
[false,true,false,true]
0: [false,true,true,false]
1: [false,true,true,true]
2: [false,true,false,true]
3: [false,true,true,true]
4: [false,true,false,true]
-1: [false,true,false,true]
[true,false,false,true,false]

Warning: Undefined variable $undefinedKey in t.php on line 13

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 13
false
[false,true,false]
'key': [true,false,false,true]
'nul': [false,true,false,true]
'zero': [true,true,false,true]
'nope': [false,true,false,true]
7: [true,false,false,true]
-2: [true,false,false,true]
9: [false,true,false,true]
8: [false,true,false,true]
'7': [true,false,false,true]
'07': [false,true,false,true]
1.5: 
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8
[false,true,true,true]
true: [false,true,true,true]
NULL: 
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 8
[false,true,false,true]
0: [false,true,true,false]
1: [false,true,true,true]
2: [false,true,false,true]
3: [false,true,true,true]
4: [false,true,false,true]
-1: [false,true,false,true]
[true,false,false,true,false]

Warning: Undefined variable $undefinedKey in t.php on line 13

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 13
false
[false,true,false]
"#;

/// `$v = $a[$k]` with keys in variables and temporaries: strings,
/// integers in digits and not, elements of every kind (a reference, an
/// array, an object), missing keys and their warnings, destinations that
/// held an array, an object with a destructor, a reference, the key's or
/// the array's own variable, a list, an array behind a reference and a
/// string container.
#[test]
fn element_read_into_a_variable() {
    assert_eq!(run(FOLDED_READ), FOLDED_READ_OUT);
}

const FOLDED_READ: &str = r#"<?php
class D { public function __construct(public $n) {} public function __destruct() { echo "d{$this->n} "; } }
function f() {
    for ($round = 0; $round < 3; $round++) {
        $a = ['key' => 1, 's' => "str$round", 'arr' => [1, 2], 'nul' => null, 7 => 'seven', '07' => 'oh-seven', 'obj' => new stdClass];
        $t = 'through';
        $a['ref'] = &$t;
        $out = [];
        foreach (['key', 's', 'arr', 'nul', '7', '07', 'ref', 'obj'] as $k) {
            $v = $a[$k];
            $out[] = is_object($v) ? 'object' : $v;
        }
        echo json_encode($out), "\n";
        $p = 'ke';
        $v = $a[$p . 'y'];
        echo $v, "\n";
        // Missing keys warn.
        $k = 'nope';
        $v = $a[$k];
        var_dump($v);
        $k = '8';
        $v = $a[$k];
        var_dump($v);
        // The destination held an array, an object, a reference.
        $dst = [1];
        $k = 's';
        $dst = $a[$k];
        echo $dst, "\n";
        $dst = new D($round);
        $dst = $a[$k];
        echo " ", $dst, "\n";
        $x = 1;
        $refd = &$x;
        $refd = $a[$k];
        echo $x, "\n";
        unset($refd);
        // The key's own variable as the destination.
        $k = 'key';
        $k = $a[$k];
        var_dump($k);
        // The array's own variable as the destination.
        $b = ['inner' => ['x' => 1]];
        $k = 'inner';
        $b = $b[$k];
        echo json_encode($b), "\n";
        // A list, a reference to the array, a string container.
        $l = [5, 6];
        $k = '1';
        $v = $l[$k];
        echo $v, "\n";
        $arr = ['key' => 'r'];
        $ra = &$arr;
        $k = 'key';
        $v = $arr[$k];
        echo $v, "\n";
        unset($ra);
        $s = 'abc';
        $k = '1';
        $v = $s[$k];
        echo $v, "\n";
        $k = 'x';
        try { $v = $s[$k]; } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        $v = $a[$k] ?? 'dflt';
        echo $v, "\n";
    }
}
f();
"#;

const FOLDED_READ_OUT: &str = r#"[1,"str0",[1,2],null,"seven","oh-seven","through","object"]
1

Warning: Undefined array key "nope" in t.php on line 19
NULL

Warning: Undefined array key 8 in t.php on line 22
NULL
str0
d0  str0
str0
int(1)
{"x":1}
6
r
b
TypeError: Cannot access offset of type string on string
dflt
[1,"str1",[1,2],null,"seven","oh-seven","through","object"]
1

Warning: Undefined array key "nope" in t.php on line 19
NULL

Warning: Undefined array key 8 in t.php on line 22
NULL
str1
d1  str1
str1
int(1)
{"x":1}
6
r
b
TypeError: Cannot access offset of type string on string
dflt
[1,"str2",[1,2],null,"seven","oh-seven","through","object"]
1

Warning: Undefined array key "nope" in t.php on line 19
NULL

Warning: Undefined array key 8 in t.php on line 22
NULL
str2
d2  str2
str2
int(1)
{"x":1}
6
r
b
TypeError: Cannot access offset of type string on string
dflt
"#;
