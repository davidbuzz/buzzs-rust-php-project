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
