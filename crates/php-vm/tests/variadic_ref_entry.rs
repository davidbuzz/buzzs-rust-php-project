// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls of user functions with a variadic or by-reference parameter, and
//! the function-entry ops (`RECV_VARIADIC`, `RECV_INIT`, `BIND_GLOBAL`,
//! `BIND_STATIC`), on the plain call paths and the generic one alike:
//! what each prints is compared with the C implementation
//! (`/usr/bin/php8.5 -n`, PHP 8.5.12). The calls run inside a function,
//! in loops, so the tape's prepared calls take them
//! (ref: Zend/zend_vm_def.h `ZEND_RECV_VARIADIC`, `ZEND_SEND_REF`,
//! `ZEND_RECV_INIT`, `ZEND_BIND_GLOBAL`, `ZEND_BIND_STATIC`).

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

/// Variadic functions, methods and closures called with no, one and
/// several extra arguments: the collected array, `func_num_args()`,
/// `func_get_args()` (after the callee changed its variables), backtrace
/// and exception-trace argument lists, recursion, a generator, a typed
/// variadic, named arguments, unpacking, and the too-few-arguments text
/// ("exactly 2": the variadic parameter is not counted).
#[test]
fn variadic_calls() {
    assert_eq!(
        run(r##"<?php
function dispatch($name, $value, ...$args) {
    return $name . ':' . json_encode($value) . ':' . count($args) . ':' . json_encode($args) . ':' . func_num_args() . ':' . json_encode(func_get_args());
}
function only(...$a) { return json_encode($a); }
function trace($a, ...$rest) { return json_encode(array_map(fn($f) => $f['args'] ?? null, debug_backtrace())); }
function thrower($a, ...$rest) { throw new Exception("x"); }
function modify($a, ...$rest) { $rest[] = 'm'; $a = 'z'; return json_encode(func_get_args()) . json_encode($rest); }
class K {
    function m($x, ...$ys) { return $x . '/' . implode(',', $ys); }
    static function s(...$ys) { return array_sum($ys); }
}
function rec($n, ...$acc) { return $n == 0 ? json_encode($acc) : rec($n - 1, ...[...$acc, $n]); }
function rec2($n, ...$acc) { if ($n == 0) return count($acc); return rec2($n - 1, $n, $n); }
function gen(...$xs) { foreach ($xs as $x) yield $x; }
function typed(int ...$n) { return array_sum($n); }
function main() {
    $out = [];
    $k = new K;
    for ($i = 0; $i < 3; $i++) {
        $out[] = dispatch('a', $i);
        $out[] = dispatch('b', $i, 'x');
        $out[] = dispatch('c', [$i], 'x', 2, 3.5, null);
        $out[] = only();
        $out[] = only(1, 2, 3, 4, 5, 6, 7, 8);
        $out[] = trace(1, 2, 3);
        $out[] = modify('a', 'b', 'c');
        $out[] = $k->m('q', 1, 2);
        $out[] = $k->m('q');
        $out[] = K::s(1, 2, 3);
        $out[] = rec(3);
        $out[] = rec2(4);
        $out[] = implode(',', iterator_to_array(gen(1, 2, 3)));
        $out[] = typed(1, 2, '3');
        $out[] = dispatch(name: 'n', value: 1);
        $out[] = dispatch('u', ...[1, 2, 3]);
        $f = function ($a, ...$b) use ($i) { return $i . $a . count($b) . json_encode(func_get_args()); };
        $out[] = $f('p');
        $out[] = $f('p', 'q', 'r');
        $g = fn(...$z) => count($z);
        $out[] = $g(1, 2);
        try { thrower(1, 2, 3); } catch (Exception $e) { $out[] = json_encode($e->getTrace()[0]['args']); $out[] = $e->getTraceAsString(); }
    }
    try { dispatch('only'); } catch (ArgumentCountError $e) { $out[] = $e->getMessage(); }
    try { typed(1, 'x'); } catch (TypeError $e) { $out[] = $e->getMessage(); }
    return $out;
}
foreach (main() as $l) echo $l, "\n";
"##),
        r##"a:0:0:[]:2:["a",0]
b:0:1:["x"]:3:["b",0,"x"]
c:[0]:4:["x",2,3.5,null]:6:["c",[0],"x",2,3.5,null]
[]
[1,2,3,4,5,6,7,8]
[[1,2,3],[]]
["z","b","c"]["b","c","m"]
q/1,2
q/
6
[3,2,1]
2
1,2,3
6
n:1:0:[]:2:["n",1]
u:1:2:[2,3]:4:["u",1,2,3]
0p0["p"]
0p2["p","q","r"]
2
[1,2,3]
#0 t.php(42): thrower(1, 2, 3)
#1 t.php(48): main()
#2 {main}
a:1:0:[]:2:["a",1]
b:1:1:["x"]:3:["b",1,"x"]
c:[1]:4:["x",2,3.5,null]:6:["c",[1],"x",2,3.5,null]
[]
[1,2,3,4,5,6,7,8]
[[1,2,3],[]]
["z","b","c"]["b","c","m"]
q/1,2
q/
6
[3,2,1]
2
1,2,3
6
n:1:0:[]:2:["n",1]
u:1:2:[2,3]:4:["u",1,2,3]
1p0["p"]
1p2["p","q","r"]
2
[1,2,3]
#0 t.php(42): thrower(1, 2, 3)
#1 t.php(48): main()
#2 {main}
a:2:0:[]:2:["a",2]
b:2:1:["x"]:3:["b",2,"x"]
c:[2]:4:["x",2,3.5,null]:6:["c",[2],"x",2,3.5,null]
[]
[1,2,3,4,5,6,7,8]
[[1,2,3],[]]
["z","b","c"]["b","c","m"]
q/1,2
q/
6
[3,2,1]
2
1,2,3
6
n:1:0:[]:2:["n",1]
u:1:2:[2,3]:4:["u",1,2,3]
2p0["p"]
2p2["p","q","r"]
2
[1,2,3]
#0 t.php(42): thrower(1, 2, 3)
#1 t.php(48): main()
#2 {main}
Too few arguments to function dispatch(), 1 passed in t.php on line 44 and exactly 2 expected
typed(): Argument #2 must be of type int, string given, called in t.php on line 45
"##
    );
}

/// Functions with by-reference parameters: not reached by the call (the
/// default binds), passed a plain variable (the caller's variable becomes
/// a reference and keeps the callee's writes), passed a variable that is a
/// reference already, by-reference variadics, methods, closures, and a
/// literal passed by reference.
#[test]
fn by_reference_parameters() {
    assert_eq!(
        run(r##"<?php
function get($key, $group = 'default', $force = false, &$found = null) {
    $found = $key !== 'miss';
    return "$key/$group/" . var_export($force, true) . '/' . func_num_args() . json_encode(func_get_args());
}
function first(&$a, $b = 2) { $a .= '+'; return $b; }
function mid($a, &$b, $c = 3) { $b[] = $a; return $c; }
function refs(&...$all) { foreach ($all as &$x) $x .= '!'; return count($all); }
function defref(&$x = [1]) { $x[] = 2; return json_encode($x); }
class C {
    public $p = 'p';
    function get($k, &$hit = null) { $hit = true; return $k; }
    static function sget($k, &$hit = null) { $hit = 'static'; return $k; }
}
function main() {
    $out = [];
    $c = new C;
    for ($i = 0; $i < 3; $i++) {
        $out[] = get("k$i");
        $out[] = get("k$i", 'g');
        $out[] = get("k$i", 'g', true);
        $out[] = get('miss', 'g', true, $found) . var_export($found, true);
        $v = "v$i";
        $out[] = first($v) . $v;
        $out[] = first($v, 5) . $v;
        $arr = [];
        $out[] = mid(1, $arr) . json_encode($arr);
        $out[] = mid(2, $arr, 9) . json_encode($arr);
        $p = 'a'; $q = 'b';
        $out[] = refs($p, $q) . $p . $q;
        $out[] = refs();
        $out[] = defref();
        $y = [0];
        $out[] = defref($y) . json_encode($y);
        $out[] = $c->get('m') . $c->get('n', $h) . var_export($h, true);
        $out[] = C::sget('s') . C::sget('t', $h2) . $h2;
        $ref = &$v;
        $out[] = first($ref) . $v;
        $f = function ($a, &$b = null) { $b = $a * 2; return $a; };
        $out[] = $f(3) . $f(4, $z) . $z;
        $out[] = json_encode(array_map(fn($fr) => $fr['args'] ?? null, (function ($a, &$b = 5) { return debug_backtrace(); })(1)));
    }
    return $out;
}
foreach (main() as $l) echo $l, "\n";
function lit() { return first(1); }
try { echo lit(), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
"##),
        r##"k0/default/false/1["k0"]
k0/g/false/2["k0","g"]
k0/g/true/3["k0","g",true]
miss/g/true/4["miss","g",true,false]false
2v0+
5v0++
3[1]
9[1,2]
2a!b!
0
[1,2]
[0,2][0,2]
mntrue
ststatic
2v0+++
348
[[1],[]]
k1/default/false/1["k1"]
k1/g/false/2["k1","g"]
k1/g/true/3["k1","g",true]
miss/g/true/4["miss","g",true,false]false
2v1+
5v1++
3[1]
9[1,2]
2a!b!
0
[1,2]
[0,2][0,2]
mntrue
ststatic
2v1+++
348
[[1],[]]
k2/default/false/1["k2"]
k2/g/false/2["k2","g"]
k2/g/true/3["k2","g",true]
miss/g/true/4["miss","g",true,false]false
2v2+
5v2++
3[1]
9[1,2]
2a!b!
0
[1,2]
[0,2][0,2]
mntrue
ststatic
2v2+++
348
[[1],[]]
Error: first(): Argument #1 ($a) could not be passed by reference
"##
    );
}
