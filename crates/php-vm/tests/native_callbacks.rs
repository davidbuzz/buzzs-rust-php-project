// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! User code the engine calls from native code: the magic property
//! methods, `ArrayAccess`, `Countable`, `Iterator`/`IteratorAggregate`,
//! `__toString`, `__invoke`, and the callbacks of the array and PCRE
//! builtins. Each script's output is what the C implementation prints
//! (`/usr/bin/php8.5 -n`, PHP 8.5.12): exceptions thrown inside, the
//! `__get` guard, backtraces taken inside, argument coercion and
//! by-reference parameters, generators and fibers inside a callback
//! (ref: Zend/zend_execute_API.c `zend_call_function`,
//! `zend_call_known_function`; Zend/zend_object_handlers.c).

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

/// `__get`/`__set`/`__isset`/`__unset`, the guard (inside `__get` the same name is read
/// directly), a typed `__get`, a throw, `&__get`, a backtrace inside.
#[test]
fn magic_property_methods() {
    assert_eq!(
        run(r##"<?php
class M {
    private array $data = ['a' => 1];
    public function __get($n) { echo "get $n\n"; if ($n === 'self') { return $this->self; } return $this->data[$n] ?? null; }
    public function __set($n, $v) { echo "set $n\n"; $this->data[$n] = $v; }
    public function __isset($n) { echo "isset $n\n"; return isset($this->data[$n]); }
    public function __unset($n) { echo "unset $n\n"; unset($this->data[$n]); }
}
function t() {
    $m = new M;
    var_dump($m->a, $m->b);
    $m->b = 'x';

    var_dump(isset($m->b), isset($m->zz), empty($m->a));
    unset($m->b);
    var_dump($m->b);
    var_dump($m->self);
    $name = 'dyn' . 1;
    $m->$name = [1, 2];
    var_dump($m->$name);
}
t();
class T { public function __get(string $n): int { return strlen($n); } }
function t2() { $t = new T; $s = 0; for ($i = 0; $i < 3; $i++) { $s += $t->{"p$i"}; } var_dump($s, $t->{123}); }
t2();
class E { public function __get($n) { throw new RuntimeException("no $n"); } }
function t3() { $e = new E; try { $x = $e->foo; } catch (RuntimeException $x) { echo get_class($x), ': ', $x->getMessage(), ' line ', $x->getLine(), "\n"; } }
t3();
class R { private $v = 1; public function &__get($n) { return $this->v; } }
function t4() { $r = new R; $x = &$r->q; $x = 9; var_dump($r->q); }
t4();
class B { public function __get($n) { foreach (debug_backtrace() as $f) { echo ($f['class'] ?? ''), ($f['type'] ?? ''), $f['function'], '(', count($f['args'] ?? []), ")\n"; } return 0; } }
function t5() { (new B)->zap; }
t5();
"##),
        r##"get a
get b
int(1)
NULL
set b
isset b
isset zz
isset a
get a
bool(true)
bool(false)
bool(false)
unset b
get b
NULL
get self

Warning: Undefined property: M::$self in t.php on line 4
NULL
set dyn1
get dyn1
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
int(6)
int(3)
RuntimeException: no foo line 26
int(9)
B->__get(1)
t5(0)
"##
    );
}

/// `offsetGet`/`offsetSet`/`offsetExists`/`offsetUnset`, a throw inside `offsetGet`,
/// `count()` and `getIterator()`, the `Iterator` methods in their order.
#[test]
fn array_access_countable_iterator() {
    assert_eq!(
        run(r##"<?php
class A implements ArrayAccess {
    public array $d = [1, 2, 3];
    public function offsetGet($o): mixed { echo "get ", var_export($o, true), "\n"; return $this->d[$o] ?? null; }
    public function offsetSet($o, $v): void { echo "set ", var_export($o, true), "\n"; if ($o === null) { $this->d[] = $v; } else { $this->d[$o] = $v; } }
    public function offsetExists($o): bool { echo "exists ", var_export($o, true), "\n"; return isset($this->d[$o]); }
    public function offsetUnset($o): void { echo "unset ", var_export($o, true), "\n"; unset($this->d[$o]); }
}
function t() {
    $a = new A;
    var_dump($a[1], $a['2']);
    $a[5] = 'five';
    $a[] = 'pushed';
    var_dump(isset($a[0]), isset($a[9]), empty($a[1]));
    unset($a[0]);
    $k = 1; $s = 0;
    for ($i = 0; $i < 3; $i++) { $s += $a[$k]; }
    var_dump($s, count($a->d));
}
t();
class I implements ArrayAccess {
    public function offsetGet(mixed $o): mixed { if ($o === 'bad') { throw new LogicException('bad offset'); } return $o * 2; }
    public function offsetSet(mixed $o, mixed $v): void {}
    public function offsetExists(mixed $o): bool { return true; }
    public function offsetUnset(mixed $o): void {}
}
function t2() { $i = new I; var_dump($i[21]); try { $i['bad']; } catch (LogicException $e) { echo $e->getMessage(), "\n"; } }
t2();
class C implements Countable, IteratorAggregate { public function count(): int { return 7; } public function getIterator(): Iterator { return new ArrayIterator(['x' => 1, 'y' => 2]); } }
function t3() { $c = new C; var_dump(count($c)); foreach ($c as $k => $v) { echo "$k=$v\n"; } }
t3();
class It implements Iterator { private $i = 0; public function current(): mixed { echo "current\n"; return $this->i * 10; } public function key(): mixed { echo "key\n"; return $this->i; } public function next(): void { echo "next\n"; $this->i++; } public function rewind(): void { echo "rewind\n"; $this->i = 0; } public function valid(): bool { echo "valid\n"; return $this->i < 2; } }
function t4() { foreach (new It as $k => $v) { echo "$k => $v\n"; } }
t4();
"##),
        r##"get 1
get '2'
int(2)
int(3)
set 5
set NULL
exists 0
exists 9
exists 1
get 1
bool(true)
bool(false)
bool(false)
unset 0
get 1
get 1
get 1
int(6)
int(4)
int(42)
bad offset
int(7)
x=1
y=2
rewind
valid
current
key
0 => 0
next
valid
current
key
1 => 10
next
valid
"##
    );
}

/// `__toString` in a concatenation, a conversion and a comparison, a throw inside it,
/// and `__invoke` called directly and as a callback.
#[test]
fn to_string_and_invoke() {
    assert_eq!(
        run(r##"<?php
class S { public function __construct(private string $s) {} public function __toString(): string { return $this->s; } }
function t() { $a = new S('a'); $s = ''; for ($i = 0; $i < 3; $i++) { $s .= $a; } echo $s, "\n"; echo strlen(new S('four')), "\n"; var_dump((string) new S('x'), "pre" . new S('y') . "post", new S('z') == 'z'); }
t();
class X { public function __toString(): string { throw new DomainException('nope'); } }
function t2() { try { echo new X; } catch (DomainException $e) { echo "caught ", $e->getMessage(), "\n"; } }
t2();
class Inv { public function __invoke($a, $b = 2) { return $a + $b; } }
function t3() { $f = new Inv; var_dump($f(1), array_map($f, [1, 2]), call_user_func($f, 5, 5)); }
t3();
"##),
        r##"aaa
4
string(1) "x"
string(8) "preypost"
bool(true)
caught nope
int(3)
array(2) {
  [0]=>
  int(3)
  [1]=>
  int(4)
}
int(10)
"##
    );
}

/// The callback builtins: `array_filter` in its three modes, `array_reduce`, `array_walk`
/// by reference, the sorts, `array_find*`/`array_any`/`array_all`,
/// `preg_replace_callback`; throws and argument errors, coercion, by-reference
/// callbacks, method callables, a backtrace, a generator and a fiber inside a callback.
#[test]
fn callback_builtins() {
    assert_eq!(
        run(r##"<?php
function t() {
    $a = ['a' => 1, 'b' => 2, 'c' => 3, 'd' => 4];
    var_dump(array_filter($a, fn($x) => $x % 2 == 0));
    var_dump(array_filter($a, fn($k) => $k != 'b', ARRAY_FILTER_USE_KEY));
    var_dump(array_filter($a, fn($v, $k) => $v > 1 && $k != 'd', ARRAY_FILTER_USE_BOTH));
    var_dump(array_reduce($a, fn($c, $x) => $c + $x, 10));
    array_walk($a, function (&$v, $k, $p) { $v = "$p$k$v"; }, '#');
    var_dump($a);
    $n = [5, 3, 9, 1];
    usort($n, fn($x, $y) => $x <=> $y); var_dump($n);
    $m = ['x' => 3, 'y' => 1, 'z' => 2];
    uasort($m, fn($x, $y) => $y <=> $x); var_dump($m);
    uksort($m, 'strcmp'); var_dump($m);
    var_dump(array_find([1, 2, 3], fn($v) => $v > 1), array_find_key([1, 2, 3], fn($v) => $v > 1), array_any([1, 2], fn($v) => $v > 1), array_all([1, 2], fn($v) => $v > 1));
    var_dump(preg_replace_callback('/\d+/', fn($m) => $m[0] * 2, 'a1 b22 c3'));
    $calls = 0;
    var_dump(array_map(function ($x) use (&$calls) { $calls++; return $x . '!'; }, ['p', 'q']), $calls);
}
t();
function t2() {
    try { usort($GLOBALS['dummy'], fn() => 0); } catch (Throwable $e) { echo get_class($e), "\n"; }
    $n = [3, 1, 2];
    try { usort($n, function ($x, $y) { if ($x == 2 || $y == 2) { throw new Exception("cmp $x $y"); } return $x <=> $y; }); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    var_dump(count($n));
    try { array_filter([1, 2], fn($a, $b) => true); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
    var_dump(array_map(fn(int $i) => $i + 1, ['5', 6.0]));
    try { array_map(fn(int $i) => $i, ['x']); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $r = 1;
    call_user_func(function (&$x) { $x++; }, $r);
    var_dump($r);
    call_user_func_array(function (&$x) { $x++; }, [&$r]);
    var_dump($r);
}
t2();
class K { public static function cmp($a, $b) { return $b <=> $a; } public function keep($v) { return $v !== 2; } }
function t3() {
    $a = [1, 3, 2];
    usort($a, 'K::cmp'); var_dump($a);
    usort($a, ['K', 'cmp']); var_dump($a);
    var_dump(array_values(array_filter($a, [new K, 'keep'])));
    array_filter([1], function ($x) { foreach (debug_backtrace() as $f) { echo $f['function'], '(', count($f['args'] ?? []), ")\n"; } return true; });
}
t3();
function gen() { yield 1; yield 2; }
function t4() { var_dump(array_map(fn($g) => iterator_to_array($g), [gen()])); $f = new Fiber(function () { return array_map(function ($x) { return Fiber::suspend($x) + 1; }, [10, 20]); }); $v = $f->start(); while (!$f->isTerminated()) { $v = $f->resume($v * 2); } var_dump($f->getReturn()); }
t4();
"##),
        r##"array(2) {
  ["b"]=>
  int(2)
  ["d"]=>
  int(4)
}
array(3) {
  ["a"]=>
  int(1)
  ["c"]=>
  int(3)
  ["d"]=>
  int(4)
}
array(2) {
  ["b"]=>
  int(2)
  ["c"]=>
  int(3)
}
int(20)
array(4) {
  ["a"]=>
  string(3) "#a1"
  ["b"]=>
  string(3) "#b2"
  ["c"]=>
  string(3) "#c3"
  ["d"]=>
  string(3) "#d4"
}
array(4) {
  [0]=>
  int(1)
  [1]=>
  int(3)
  [2]=>
  int(5)
  [3]=>
  int(9)
}
array(3) {
  ["x"]=>
  int(3)
  ["z"]=>
  int(2)
  ["y"]=>
  int(1)
}
array(3) {
  ["x"]=>
  int(3)
  ["y"]=>
  int(1)
  ["z"]=>
  int(2)
}
int(2)
int(1)
bool(true)
bool(false)
string(9) "a2 b44 c6"
array(2) {
  [0]=>
  string(2) "p!"
  [1]=>
  string(2) "q!"
}
int(2)
TypeError
cmp 2 1
int(3)
Too few arguments to function {closure:t2():26}(), 1 passed and exactly 2 expected
array(2) {
  [0]=>
  int(6)
  [1]=>
  int(7)
}
{closure:t2():28}(): Argument #1 ($i) must be of type int, string given

Warning: {closure:t2():30}(): Argument #1 ($x) must be passed by reference, value given in t.php on line 30
int(1)
int(2)
array(3) {
  [0]=>
  int(3)
  [1]=>
  int(2)
  [2]=>
  int(1)
}
array(3) {
  [0]=>
  int(3)
  [1]=>
  int(2)
  [2]=>
  int(1)
}
array(2) {
  [0]=>
  int(3)
  [1]=>
  int(1)
}
{closure:t3():42}(1)
array_filter(2)
t3(0)
array(1) {
  [0]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(2)
  }
}
array(2) {
  [0]=>
  int(21)
  [1]=>
  int(41)
}
"##
    );
}

/// The edges of the direct entries: a typed `__get`, an `offsetGet()` with a default
/// parameter and a static variable, an `offsetSet()` with `finally`, a deprecated
/// `__toString`, a generator `getIterator()`, a reference passed to a callback, a
/// typed callback.
#[test]
fn direct_entry_edges() {
    assert_eq!(
        run(r##"<?php
class G { public function __get(string $n): int { return (int) $n * 2; } }
class D implements ArrayAccess {
    public function offsetGet($o, $extra = 5): mixed { static $calls = 0; $calls++; return "$o/$extra/$calls"; }
    public function offsetSet($o, $v): void { try { echo "set $o\n"; return; } finally { echo "finally $o\n"; } }
    public function offsetExists($o): bool { return $o !== 'no'; }
    public function offsetUnset($o): void {}
}
class Dep { #[\Deprecated("use other")] public function __toString(): string { return "dep"; } }
class Gen implements IteratorAggregate { public function getIterator(): Generator { yield 'k' => 'v'; } }
function t() {
    $g = new G;
    var_dump($g->{'21'});
    try { var_dump($g->abc); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $d = new D;
    var_dump($d[1], $d['x']);
    $d['y'] = 1;
    var_dump(isset($d['no']), isset($d['yes']));
    echo new Dep, "\n";
    foreach (new Gen as $k => $v) { echo "$k=$v\n"; }
    $ref = 'r';
    $arr = [&$ref];
    var_dump(array_map(fn($x) => $x . '!', $arr));
    var_dump(array_filter([1, 0, 2], fn(int $x): bool => (bool) $x));
}
t();
"##),
        r##"int(42)
int(0)
string(5) "1/5/1"
string(5) "x/5/2"
set y
finally y
bool(false)
bool(true)

Deprecated: Method Dep::__toString() is deprecated, use other in t.php on line 19
dep
k=v
array(1) {
  [0]=>
  string(2) "r!"
}
array(2) {
  [0]=>
  int(1)
  [2]=>
  int(2)
}
"##
    );
}

/// `$obj[$k] = $v`, `$obj[$k]` and `$obj[$k] ?? d` of user `ArrayAccess`
/// objects (a subclass too) called from the tape, an exception out of
/// `offsetSet()` (its line and trace), an object without `ArrayAccess`, an
/// `ArrayObject`; and the user methods native code calls (`__toString()`,
/// `__get()`/`__set()`/`__isset()`/`__unset()`, `getIterator()`,
/// `jsonSerialize()`, `count()` with a default) entered on a direct frame:
/// `func_get_args()`, `func_num_args()`, `debug_backtrace()`, a throw, a
/// recursive `__get()`. Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn array_access_and_native_method_calls() {
    let out = run(r#"<?php
namespace App;
class OA implements \ArrayAccess {
    public array $a = [];
    public function offsetExists(mixed $o): bool { echo "exists($o) "; return isset($this->a[$o]); }
    public function offsetGet(mixed $o): mixed { echo "get($o) "; return $this->a[$o] ?? 'none'; }
    public function offsetSet(mixed $o, mixed $v): void {
        if ($o === 'bad') { throw new \RuntimeException("bad key"); }
        echo "set($o) "; $this->a[$o ?? 'null'] = $v;
    }
    public function offsetUnset(mixed $o): void { unset($this->a[$o]); }
}
class Sub extends OA {}
class Plain {}
function trace(\Throwable $e) { return $e->getMessage() . ' @' . $e->getLine() . ' ' . count($e->getTrace()) . ' ' . ($e->getTrace()[0]['function'] ?? '-') . ':' . ($e->getTrace()[0]['line'] ?? '-'); }
function run() {
    $c = new OA; $s = new Sub; $p = new Plain; $ao = new \ArrayObject([]);
    for ($i = 0; $i < 3; $i++) {
        $c['k'] = $i; $c[$i] = 'v' . $i; $s['x'] = $i;
        $v = $c['k']; echo $v, ' ';
        $w = $c['missing'] ?? 'dflt'; echo $w, ' ';
        $z = $s['x'] ?? 'no'; echo $z, ' ';
        $ao['q'] = $i; echo $ao['q'], ' ';
        try { $c['bad'] = 1; } catch (\RuntimeException $e) { echo trace($e), ' '; }
        try { $p['x'] = 1; } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), ' '; }
        try { $q = $p['x']; } catch (\Error $e) { echo $e->getMessage(), ' '; }
        $r = $c; $r['shared'] = $i; echo count($c->a), "\n";
    }
}
run();
"#);
    assert_eq!(
        out,
        r#"set(k) set(0) set(x) get(k) 0 exists(missing) dflt exists(x) get(x) 0 0 bad key @8 2 offsetSet:24 Error: Cannot use object of type App\Plain as array Cannot use object of type App\Plain as array set(shared) 3
set(k) set(1) set(x) get(k) 1 exists(missing) dflt exists(x) get(x) 1 1 bad key @8 2 offsetSet:24 Error: Cannot use object of type App\Plain as array Cannot use object of type App\Plain as array set(shared) 4
set(k) set(2) set(x) get(k) 2 exists(missing) dflt exists(x) get(x) 2 2 bad key @8 2 offsetSet:24 Error: Cannot use object of type App\Plain as array Cannot use object of type App\Plain as array set(shared) 5
"#
    );
    let out = run(r#"<?php
namespace App;
class M {
    public $log = [];
    public function __toString(): string { return 'M' . count(func_get_args()) . func_num_args(); }
    public function __get($n) { $this->log[] = "get:$n"; return strtoupper($n); }
    public function __set($n, $v) { $this->log[] = "set:$n=" . (is_object($v) ? 'obj' : $v); }
    public function __isset($n) { return $n === 'yes'; }
    public function __unset($n) { $this->log[] = "unset:$n"; }
}
class T {
    public function __toString(): string { $bt = debug_backtrace(); return count($bt) . ':' . ($bt[1]['function'] ?? '-'); }
}
class Ex { public function __toString(): string { throw new \LogicException('nope'); } }
class Rec { public $d = 0; public function __get($n) { return $this->d < 3 ? ($this->d++) . ',' . $this->x : 'end'; } }
class It implements \IteratorAggregate { public function getIterator(): \Iterator { return new \ArrayIterator([1, 2]); } }
class J implements \JsonSerializable { public function __construct(private $v = 7) {} public function jsonSerialize(): mixed { return ['v' => $this->v]; } }
class C implements \Countable { public function count(int $mode = 5): int { return $mode + func_num_args(); } }
function run() {
    $m = new M; $t = new T; $r = new Rec; $c = new C;
    for ($i = 0; $i < 4; $i++) {
        echo $m, ' ', (string)$m, ' ', "x{$m}y", ' ', strlen($m), ' ';
        echo $m->foo, $m->bar, ' ';
        $m->baz = $i; $m->obj = $m; unset($m->gone);
        echo isset($m->yes) ? 'Y' : 'N', isset($m->no) ? 'Y' : 'N', empty($m->yes) ? 'E' : 'F', ' ';
        echo $t, ' ', implode(',', [$t, $m]), ' ';
        try { echo new Ex; } catch (\LogicException $e) { echo $e->getMessage(), '@', $e->getLine(), ' '; }
        $r->d = 0; echo $r->x, ' ';
        foreach (new It as $v) { echo $v; }
        echo ' ', json_encode(new J), ' ', count($c), ' ', str_repeat('=', 1), "\n";
    }
    echo implode('|', $m->log), "\n";
}
run();
"#);
    assert_eq!(
        out,
        r#"M00 M00 xM00y 3 FOOBAR YNF 2:App\run 3:implode,M00 nope@14 
Warning: Undefined property: App\Rec::$x in t.php on line 15
0, 12 {"v":7} 5 =
M00 M00 xM00y 3 FOOBAR YNF 2:App\run 3:implode,M00 nope@14 
Warning: Undefined property: App\Rec::$x in t.php on line 15
0, 12 {"v":7} 5 =
M00 M00 xM00y 3 FOOBAR YNF 2:App\run 3:implode,M00 nope@14 
Warning: Undefined property: App\Rec::$x in t.php on line 15
0, 12 {"v":7} 5 =
M00 M00 xM00y 3 FOOBAR YNF 2:App\run 3:implode,M00 nope@14 
Warning: Undefined property: App\Rec::$x in t.php on line 15
0, 12 {"v":7} 5 =
get:foo|get:bar|set:baz=0|set:obj=obj|unset:gone|get:yes|get:foo|get:bar|set:baz=1|set:obj=obj|unset:gone|get:yes|get:foo|get:bar|set:baz=2|set:obj=obj|unset:gone|get:yes|get:foo|get:bar|set:baz=3|set:obj=obj|unset:gone|get:yes
"#
    );
}

/// `$obj->$name()` with a name that changes from call to call (Doctrine's
/// `Column::setOptions()`), whose site cache misses are looked up on the
/// tape: public, private, protected, static, case-insensitive names,
/// `__call()` for inaccessible and missing ones, undefined methods,
/// non-objects, non-string names, `__invoke`; and `$a += $b` of arrays on
/// the tape: shared and own arrays, a used result, `$a += $a`, lists,
/// non-array operands, destructors, a reference, recursion. Expected
/// output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn dynamic_method_names_and_array_union() {
    assert_eq!(
        run(r#"<?php
namespace App;
class P { protected function prot($v) { return "prot$v"; } }
class Col extends P {
  private $log = [];
  public function setA($v) { $this->log[] = "A$v"; return $this; }
  public function setB($v, $w = 'd') { $this->log[] = "B$v$w"; return $this; }
  private function priv($v) { return "priv$v"; }
  public static function stat($v) { return "stat$v"; }
  public function __call($n, $a) { return "__call($n," . implode(',', $a) . ")"; }
  public function run($names) { $out = []; foreach ($names as $n) { $out[] = is_string($r = $this->$n(1)) ? $r : 'obj'; } return implode(' ', $out); }
  public function log() { return implode(',', $this->log); }
}
class NoCall { public function m($x) { return "m$x"; } }
function bench() {
  $c = new Col; $n = new NoCall;
  $names = ['setA', 'setB', 'SETA', 'priv', 'stat', 'prot', 'missing', 'setB'];
  for ($i = 0; $i < 3; $i++) {
    foreach ($names as $m) { $r = $c->$m($i); echo is_string($r) ? $r : 'obj', ' '; }
    echo $c->run($names), ' ';
    foreach (['m', 'M', 'nope'] as $m) { try { echo $n->$m($i), ' '; } catch (\Error $e) { echo $e->getMessage(), ' '; } }
    $m = 'priv'; try { echo $n->$m(); } catch (\Error $e) { echo get_class($e), ' '; }
    $o = null; try { $o->$m(); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $k = 5; try { $c->$k(); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $f = function ($x) { return "cl$x"; }; $inv = '__invoke'; echo $f->$inv($i), "\n";
  }
  echo $c->log(), "\n";
}
bench();
"#),
        r#"obj obj obj __call(priv,0) stat0 __call(prot,0) __call(missing,0) obj obj obj obj priv1 stat1 prot1 __call(missing,1) obj m0 m0 Call to undefined method App\NoCall::nope() Error Call to a member function priv() on null Method name must be a string cl0
obj obj obj __call(priv,1) stat1 __call(prot,1) __call(missing,1) obj obj obj obj priv1 stat1 prot1 __call(missing,1) obj m1 m1 Call to undefined method App\NoCall::nope() Error Call to a member function priv() on null Method name must be a string cl1
obj obj obj __call(priv,2) stat2 __call(prot,2) __call(missing,2) obj obj obj obj priv1 stat1 prot1 __call(missing,1) obj m2 m2 Call to undefined method App\NoCall::nope() Error Call to a member function priv() on null Method name must be a string cl2
A0,B0d,A0,B0d,A1,B1d,A1,B1d,A1,B1d,A1,B1d,A1,B1d,A1,B1d,A2,B2d,A2,B2d,A1,B1d,A1,B1d
"#
    );
    assert_eq!(
        run(r#"<?php
namespace App;
class D { function __destruct() { echo "D "; } }
function tr($t) { $r = ['x' => $t]; if ($t < 3) { $r += tr($t + 1); } return $r; }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $a = ['a' => 1, 'b' => 2]; $b = ['b' => 9, 'c' => 3];
    $a += $b; echo json_encode($a), ' ';
    $c = $a; $c += ['z' => 0]; echo count($a), count($c), ' ';
    $r = ($a += [5 => 5]); echo count($r), ' ';
    $a += $a; echo count($a), ' ';
    $l = [1, 2]; $l += [5, 6, 7]; echo json_encode($l), ' ';
    $s = 'str'; try { $s += [1]; } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    $n = null; try { $n += [1]; } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    $o = ['d' => new D]; $o += ['e' => new D]; unset($o); echo '| ';
    $ref = [1]; $rr = &$ref; $ref += [1 => 2]; echo count($rr), ' ';
    echo json_encode(tr(0)), "\n";
  }
}
run();
"#),
        r#"{"a":1,"b":2,"c":3} 34 4 4 [1,2,7] Unsupported operand types: string + array Unsupported operand types: null + array D D | 2 {"x":0}
{"a":1,"b":2,"c":3} 34 4 4 [1,2,7] Unsupported operand types: string + array Unsupported operand types: null + array D D | 2 {"x":0}
{"a":1,"b":2,"c":3} 34 4 4 [1,2,7] Unsupported operand types: string + array Unsupported operand types: null + array D D | 2 {"x":0}
"#
    );
}

/// Argument unpacking (`...$args`): lists go into the pending call's
/// inline slots (`Facade::__callStatic()`'s `$instance->$method(...$args)`),
/// with references in the array, by-reference parameters, string keys,
/// unpacking after positional arguments, too many arguments, variadics,
/// `Traversable`s, builtins, `__call()`/`__callStatic()`; and a later
/// unpack's positional elements after an earlier one's named ones, which
/// php-src accepts (its check covers one unpack's elements). Expected
/// output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn argument_unpacking() {
    assert_eq!(
        run(r#"<?php
namespace App;
class Svc {
  public function get($a, $b = 'B') { return "$a|$b|" . func_num_args(); }
  public function many(...$xs) { return implode(',', $xs) . '#' . count(func_get_args()); }
  public function typed(int $i, string $s = 'x') { return "$i$s"; }
  public function byref(&$r) { $r = 'changed'; return 'r'; }
  public function __call($n, $a) { return "__call($n:" . implode(',', $a) . ')'; }
  public static function __callStatic($n, $a) { return "static($n:" . count($a) . ')'; }
}
function f($a, $b, $c = 'C') { return "$a$b$c" . func_num_args(); }
function run() {
  $s = new Svc; $m = 'get';
  for ($i = 0; $i < 3; $i++) {
    $args = ['k', $i];
    echo $s->$m(...$args), ' ', $s->get(...['only']), ' ', $s->get('p', ...[]), ' ', $s->get(...[1, 2, 3, 4]), ' ';
    echo $s->many(...[1, 2, 3]), ' ', $s->many(0, ...[1, 2], ...[3]), ' ', f(...['a', 'b']), f('x', ...['y', 'z']), ' ';
    $v = 'orig'; $ra = [&$v]; echo $s->get(...$ra), ' '; $ra2 = ['val']; echo $s->byref(...$ra2), $ra2[0], ' ';
    echo $s->get(...['b' => 'named', 'a' => 'A']), ' ';
    try { echo $s->typed(...['5', 'q']), ' ', $s->typed(...['nope']); } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    echo max(...[3, 9, 2]), ' ', count(array_merge(...[[1], [2, 3]])), ' ';
    echo $s->undefined(...[1, 2]), ' ', Svc::stat(...[1]), ' ';
    echo $s->get(...new \ArrayIterator(['it', 'er'])), ' ';
    $t = function (...$q) { return count($q); }; echo $t(...range(1, 5)), ' ';
    try { $s->get(...['a' => 1], ...[2]); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { $s->get(...7); } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    echo f(...[$i => 'z', 'q', 'r']), "\n";
  }
}
run();
"#),
        r#"k|0|2 only|B|1 p|B|1 1|2|4 1,2,3#3 0,1,2,3#4 abC2xyz3 orig|B|1 rchanged A|named|2 5q App\Svc::typed(): Argument #1 ($i) must be of type int, string given, called in t.php on line 20 9 3 __call(undefined:1,2) static(stat:1) it|er|2 5 Only arrays and Traversables can be unpacked, int given zqr3
k|1|2 only|B|1 p|B|1 1|2|4 1,2,3#3 0,1,2,3#4 abC2xyz3 orig|B|1 rchanged A|named|2 5q App\Svc::typed(): Argument #1 ($i) must be of type int, string given, called in t.php on line 20 9 3 __call(undefined:1,2) static(stat:1) it|er|2 5 Only arrays and Traversables can be unpacked, int given zqr3
k|2|2 only|B|1 p|B|1 1|2|4 1,2,3#3 0,1,2,3#4 abC2xyz3 orig|B|1 rchanged A|named|2 5q App\Svc::typed(): Argument #1 ($i) must be of type int, string given, called in t.php on line 20 9 3 __call(undefined:1,2) static(stat:1) it|er|2 5 Only arrays and Traversables can be unpacked, int given zqr3
"#
    );
    assert_eq!(
        run(r#"<?php
function g($a, $b = 'B') { return "$a|$b"; }
try { echo g(...['a' => 1], ...[2]), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
try { echo g(...['a' => 1], ...[2, 3]), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
$x = [2]; try { echo g(...['a' => 1], ...$x), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
try { echo g(...['b' => 1], ...[2]), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
try { echo g(...['b' => 1, 'a' => 0], ...[2, 3]), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
function h(...$r) { return json_encode($r); }
echo h(...['x' => 1], ...[2, 3]), "\n";
echo h(...[1], ...['k' => 2], ...[3]), "\n";
"#),
        r#"1|2
1|2
1|2
ArgumentCountError: g(): Argument #1 ($a) not passed
0|1
{"0":2,"1":3,"x":1}
{"0":1,"1":3,"k":2}
"#
    );
}

/// `clone` (Lodata's `getModel()` clones an Eloquent model per call):
/// references kept in slots and a typed property's reference, dynamic and
/// unset properties, readonly properties reinitialized by `__clone()` once,
/// a private `__clone()`, a throwing one, an enum; and `$obj::class` of a
/// variable on the tape (Doctrine's `static fn (Type $type): string =>
/// $type::class`) with its errors for non-objects. Expected output:
/// `/usr/bin/php8.5` (8.5.13).
#[test]
fn clone_and_object_class_name() {
    assert_eq!(
        run(r#"<?php
namespace App;
class P { public $a = 1; public int $t = 5; protected $arr = [1]; private $p = 'p'; public $o;
  public function show() { return json_encode([$this->a, $this->t, $this->arr, $this->p, isset($this->dyn) ? $this->dyn : '-', isset($this->gone) ? 'g' : 'nog']); } }
final class R { public function __construct(public readonly array $items, public readonly int $n = 0) {}
  public function with($n) { $c = clone $this; return $c; }
  public function __clone() { $this->n = $this->n + 1; } }
final class RR { public function __construct(public readonly int $n = 0) {} public function __clone() { $this->n = 1; $this->n = 2; } }
class Priv { private function __clone() {} public function dup() { return clone $this; } }
class Thr { public function __clone() { throw new \LogicException('no clone'); } }
enum E { case A; }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $p = new P; $p->dyn = 'd'; unset($p->a); $x = 7; $p->o = &$x; $p->t = $i;
    $q = clone $p; $x = 8; echo $q->show(), ' ', $q->o, ' ', $p->show(), ' ';
    $ti = 1; $tp = new P; $tp->t = &$ti; $tc = clone $tp; try { $ti = 'str'; } catch (\TypeError $e) { echo 'TE:', $e->getMessage(), ' '; } echo $tc->t, ' ';
    $r = new R([1, 2], $i); $r2 = $r->with(9); echo $r2->n, count($r2->items), ' ';
    try { clone new RR; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { clone new Priv; } catch (\Error $e) { echo $e->getMessage(), ' '; } echo get_class((new Priv)->dup()), ' ';
    try { clone new Thr; } catch (\LogicException $e) { echo $e->getMessage(), ' '; }
    try { clone E::A; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $n = clone (clone $q); echo $n->show(), "\n";
  }
}
run();
"#),
        r#"
Deprecated: Creation of dynamic property App\P::$dyn is deprecated in t.php on line 14

Warning: Undefined property: App\P::$a in t.php on line 4
[null,0,[1],"p","d","nog"] 8 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,0,[1],"p","d","nog"] TE:Cannot assign string to reference held by property App\P::$t of type int 1 12 Cannot modify readonly property App\RR::$n Call to private method App\Priv::__clone() from global scope App\Priv no clone Trying to clone an uncloneable object of class App\E 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,0,[1],"p","d","nog"]

Deprecated: Creation of dynamic property App\P::$dyn is deprecated in t.php on line 14

Warning: Undefined property: App\P::$a in t.php on line 4
[null,1,[1],"p","d","nog"] 8 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,1,[1],"p","d","nog"] TE:Cannot assign string to reference held by property App\P::$t of type int 1 22 Cannot modify readonly property App\RR::$n Call to private method App\Priv::__clone() from global scope App\Priv no clone Trying to clone an uncloneable object of class App\E 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,1,[1],"p","d","nog"]

Deprecated: Creation of dynamic property App\P::$dyn is deprecated in t.php on line 14

Warning: Undefined property: App\P::$a in t.php on line 4
[null,2,[1],"p","d","nog"] 8 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,2,[1],"p","d","nog"] TE:Cannot assign string to reference held by property App\P::$t of type int 1 32 Cannot modify readonly property App\RR::$n Call to private method App\Priv::__clone() from global scope App\Priv no clone Trying to clone an uncloneable object of class App\E 
Warning: Undefined property: App\P::$a in t.php on line 4
[null,2,[1],"p","d","nog"]
"#
    );
    assert_eq!(
        run(r#"<?php
namespace App;
class T {} class U extends T {}
function run() {
  $objs = [new T, new U, new \ArrayObject([]), function () {}];
  for ($i = 0; $i < 3; $i++) {
    echo implode(',', array_map(static fn ($o) => $o::class, $objs)), ' ';
    foreach ($objs as $o) { echo $o::class, ' '; }
    $s = 'App\T'; try { echo $s::class; } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    $n = null; try { echo $n::class; } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    try { echo $undef::class; } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    $r = new U; $ref = &$r; echo $ref::class, ' ', (new T)::class, "\n";
  }
}
run();
"#),
        r#"App\T,App\U,ArrayObject,Closure App\T App\U ArrayObject Closure Cannot use "::class" on string Cannot use "::class" on null 
Warning: Undefined variable $undef in t.php on line 11
Cannot use "::class" on null App\U App\T
App\T,App\U,ArrayObject,Closure App\T App\U ArrayObject Closure Cannot use "::class" on string Cannot use "::class" on null 
Warning: Undefined variable $undef in t.php on line 11
Cannot use "::class" on null App\U App\T
App\T,App\U,ArrayObject,Closure App\T App\U ArrayObject Closure Cannot use "::class" on string Cannot use "::class" on null 
Warning: Undefined variable $undef in t.php on line 11
Cannot use "::class" on null App\U App\T
"#
    );
}

/// `(string) $obj` on the tape (Lodata's `$this->array[(string) $key] =
/// $value`): `__toString()` called from the tape's decline step, its
/// exception and its line, an object without one, nested casts, and
/// `__toString()` bodies that are one property read and their `return`
/// entering the tape. Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn object_string_casts() {
    assert_eq!(
        run(r#"<?php
namespace App;
class N { public function __construct(private $n) {} public function __toString(): string { return $this->n; } }
class Bad { public function __toString(): string { throw new \RuntimeException('bad toString'); } }
class Plain {}
class Inner { public function __toString(): string { return 'in' . (string) new N('ner'); } }
function run() {
  $arr = [];
  foreach ([new N('a'), new N('b'), 'str', 5, 1.5, true, null, new Inner] as $k) { $arr[(string) $k] = 1; }
  echo implode(',', array_keys($arr)), "\n";
  for ($i = 0; $i < 3; $i++) {
    $x = new N("v$i"); $s = (string) $x; echo $s, ' ', strlen((string) $x), ' ';
    try { $y = (string) new Bad; } catch (\RuntimeException $e) { echo $e->getMessage(), '@', $e->getLine(), ' '; }
    try { $y = (string) new Plain; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $o = new N('t'); $t = (string) $o . '!'; echo $t, "\n";
  }
}
run();
"#),
        r#"a,b,str,5,1.5,1,,inner
v0 2 bad toString@4 Object of class App\Plain could not be converted to string t!
v1 2 bad toString@4 Object of class App\Plain could not be converted to string t!
v2 2 bad toString@4 Object of class App\Plain could not be converted to string t!
"#
    );
}

/// Property sites that see objects of several classes (a base class's
/// method running for its subclasses): the property cache keeps the entry
/// it replaces as an alternate and swaps back to it, and a declined read
/// refills on the tape; subclasses redeclaring the property, a get hook, an
/// unset property taken by `__get()`, dynamic properties, writes, `isset()`
/// and `unset()`. Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn polymorphic_property_sites() {
    assert_eq!(
        run(r#"<?php
namespace App;
#[\AllowDynamicProperties]
class B { public $types = []; protected $x = 'B'; public function t() { return ($this->types ? 'T' : '-') . $this->x; }
  public function w($v) { $this->x = $v; return $this->x; } public function i() { return isset($this->x) ? 'i' : 'n'; } }
class C1 extends B {}
class C2 extends B { protected $x = 'C2'; }
class C3 extends B { public $types = [1]; }
class Hk extends B { public $types { get => ['h']; } }
class Mg extends B { public function __construct() { unset($this->x); } public function __get($n) { return "magic:$n"; } }
class Dy extends B { public function __construct() { $this->extra = 'e'; } }
function read($o) { return $o->types ? count($o->types) : 0; }
function run() {
  $os = [new C1, new C2, new C3, new Hk, new Mg, new Dy, new B];
  for ($i = 0; $i < 3; $i++) {
    foreach ($os as $k => $o) { echo $o->t(), read($o), $o->i(), ' '; }
    foreach ($os as $k => $o) { echo $o->w("w$i$k"), ' '; }
    foreach ([$os[0], $os[1], $os[0], $os[2], $os[0], $os[1]] as $o) { echo $o->t(); }
    unset($os[0]->types); echo isset($os[0]->types) ? 'set' : 'unset', ' '; $os[0]->types = [];
    echo "\n";
  }
}
run();
"#),
        r#"-B0i -C20i TB1i TB1i -magic:x0n -B0i -B0i w00 w01 w02 w03 w04 w05 w06 -w00-w01-w00Tw02-w00-w01unset 
-w000i -w010i Tw021i Tw031i -w040i -w050i -w060i w10 w11 w12 w13 w14 w15 w16 -w10-w11-w10Tw12-w10-w11unset 
-w100i -w110i Tw121i Tw131i -w140i -w150i -w160i w20 w21 w22 w23 w24 w25 w26 -w20-w21-w20Tw22-w20-w21unset 
"#
    );
}

/// Writes that initialize an uninitialized property on the cache hit
/// (`PC_INIT`): readonly properties from their declaring class (promoted
/// constructor parameters), the second write's error, a child class's and
/// the global scope's (the generic op's errors), typed properties without
/// a default with coercion and its `TypeError`, a class with `__set()`
/// before and after `unset()`, `__clone()` reinitializing a readonly
/// property. Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn uninitialized_property_initialization() {
    assert_eq!(
        run(r#"<?php
namespace App;
final class Id { public function __construct(private readonly string $value, private readonly bool $quoted) {}
  public function v() { return $this->value . ($this->quoted ? 'q' : 'u'); }
  public function again() { $this->value = 'x'; } }
class P { public readonly int $n; public function init($v) { $this->n = $v; return $this->n; } }
class Ch extends P { public function initChild($v) { $this->n = $v; } }
class T { public int $i; public ?string $s; private array $a; public function set($i, $s) { $this->i = $i; $this->s = $s; $this->a = [$i]; return $this->i . $this->s . count($this->a); } }
class M { public int $x; public function __set($n, $v) { echo "__set($n) "; } public function w($v) { $this->x = $v; return $this->x; } public function u() { unset($this->x); } }
class Cl { public function __construct(public readonly array $items) {} public function __clone() { $this->items = [...$this->items, 'c']; } }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $id = new Id("v$i", (bool)($i % 2)); echo $id->v(), ' ';
    try { $id->again(); } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), ' '; }
    $p = new P; echo $p->init($i), ' ';
    try { $p->init(9); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { (new Ch)->initChild(1); echo 'child-ok '; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { $q = new P; $q->n = 3; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $t = new T; echo $t->set($i, "s$i"), ' ', $t->set('7', null), ' ';
    try { $t->set('nope', 's'); } catch (\TypeError $e) { echo $e->getMessage(), ' '; }
    $m = new M; echo $m->w(5), ' '; $m->u(); try { $m->w(6); } catch (\Error $e) { echo $e->getMessage(), " "; } echo isset($m->x) ? 'set' : 'unset', ' ';
    $c = new Cl([1]); $d = clone $c; echo count($d->items), count($c->items), "\n";
  }
}
run();
"#),
        r#"v0u Error: Cannot modify readonly property App\Id::$value 0 Cannot modify readonly property App\P::$n child-ok Cannot modify protected(set) readonly property App\P::$n from global scope 0s01 71 Cannot assign string to property App\T::$i of type int 5 __set(x) Typed property App\M::$x must not be accessed before initialization unset 21
v1q Error: Cannot modify readonly property App\Id::$value 1 Cannot modify readonly property App\P::$n child-ok Cannot modify protected(set) readonly property App\P::$n from global scope 1s11 71 Cannot assign string to property App\T::$i of type int 5 __set(x) Typed property App\M::$x must not be accessed before initialization unset 21
v2u Error: Cannot modify readonly property App\Id::$value 2 Cannot modify readonly property App\P::$n child-ok Cannot modify protected(set) readonly property App\P::$n from global scope 2s21 71 Cannot assign string to property App\T::$i of type int 5 __set(x) Typed property App\M::$x must not be accessed before initialization unset 21
"#
    );
}

/// Class-typed property writes on the cache hit: the class the entry
/// accepts is learned by the frame-local write too, so readonly and
/// uninitialized class-typed properties (promoted constructor parameters
/// of objects) are initialized there; subclasses, interfaces, nullable and
/// union types, wrong classes and their `TypeError`s, a readonly property's
/// second write. Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn class_typed_property_writes() {
    assert_eq!(
        run(r#"<?php
namespace App;
interface I {} class A implements I {} class B extends A {} class C {}
class H { public A $a; public I $i; public ?A $n = null; public readonly A $ro; public A|C $u;
  function set($x) { $this->a = $x; return get_class($this->a); } function seti($x) { $this->i = $x; return get_class($this->i); }
  function setn($x) { $this->n = $x; return $this->n ? get_class($this->n) : 'null'; } function setro($x) { $this->ro = $x; return get_class($this->ro); }
  function setu($x) { $this->u = $x; return get_class($this->u); } }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $h = new H;
    foreach ([new A, new B, new C, new A] as $x) { try { echo $h->set($x), ' '; } catch (\TypeError $e) { echo 'TE ', substr($e->getMessage(), 0, 40), ' '; } }
    foreach ([new A, new B, new C] as $x) { try { echo $h->seti($x), ' '; } catch (\TypeError $e) { echo 'TEi '; } }
    foreach ([new B, null, new C] as $x) { try { echo $h->setn($x), ' '; } catch (\TypeError $e) { echo 'TEn '; } }
    try { echo $h->setro(new B), ' '; echo $h->setro(new A); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { $h2 = new H; echo $h2->setro(new C); } catch (\TypeError $e) { echo 'TEro '; }
    foreach ([new A, new C, new B, 5] as $x) { try { echo $h->setu($x), ' '; } catch (\TypeError $e) { echo 'TEu '; } }
    echo "\n";
  }
}
run();
"#),
        r#"App\A App\B TE Cannot assign App\C to property App\H::$ App\A App\A App\B TEi App\B null TEn App\B Cannot modify readonly property App\H::$ro TEro App\A App\C App\B TEu 
App\A App\B TE Cannot assign App\C to property App\H::$ App\A App\A App\B TEi App\B null TEn App\B Cannot modify readonly property App\H::$ro TEro App\A App\C App\B TEu 
App\A App\B TE Cannot assign App\C to property App\H::$ App\A App\A App\B TEi App\B null TEn App\B Cannot modify readonly property App\H::$ro TEro App\A App\C App\B TEu 
"#
    );
    assert_eq!(
        run(r#"<?php
namespace App;
final class Ident { public function __construct(public readonly string $v) {} }
final class GenericName { private readonly array $ids; public function __construct(private readonly Ident $first, Ident ...$rest) { $this->ids = [$first, ...$rest]; }
  public function n() { return count($this->ids) . $this->first->v; } public function again(Ident $x) { $this->first = $x; } }
class Holder { public function __construct(private readonly \stdClass $o, private readonly ?Ident $id = null) {} public function o() { return get_class($this->o) . ($this->id?->v ?? '-'); } }
class Cl { public function __construct(public readonly Ident $id) {} public function __clone() { $this->id = new Ident('cloned'); } }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $g = new GenericName(new Ident("a$i"), new Ident('b')); echo $g->n(), ' ';
    try { $g->again(new Ident('x')); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { new GenericName(new \stdClass); } catch (\TypeError $e) { echo substr($e->getMessage(), 0, 60), ' '; }
    echo (new Holder(new \stdClass))->o(), (new Holder(new \stdClass, new Ident('i')))->o(), ' ';
    try { new Holder(new Ident('z')); } catch (\TypeError $e) { echo 'TE ', substr($e->getMessage(), 0, 50), ' '; }
    $c = new Cl(new Ident('orig')); $d = clone $c; echo $c->id->v, $d->id->v, "\n";
  }
}
run();
"#),
        r#"2a0 Cannot modify readonly property App\GenericName::$first App\GenericName::__construct(): Argument #1 ($first) must be stdClass-stdClassi TE App\Holder::__construct(): Argument #1 ($o) must b origcloned
2a1 Cannot modify readonly property App\GenericName::$first App\GenericName::__construct(): Argument #1 ($first) must be stdClass-stdClassi TE App\Holder::__construct(): Argument #1 ($o) must b origcloned
2a2 Cannot modify readonly property App\GenericName::$first App\GenericName::__construct(): Argument #1 ($first) must be stdClass-stdClassi TE App\Holder::__construct(): Argument #1 ($o) must b origcloned
"#
    );
}

/// `new $this->factory(...)`, `new $class`, `new $object` and their errors
/// from the tape's decline step (`Vm::op_new`: autoloading, an unknown and
/// an abstract class, a throwing constructor, spread arguments); method
/// call sites that see several classes start their call there too.
/// Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn dynamic_new_on_the_tape() {
    assert_eq!(
        run(r#"<?php
namespace App;
spl_autoload_register(function ($c) { echo "autoload($c) "; if ($c === 'App\Lazy') { eval('namespace App; class Lazy { function __construct(public $v = 1) {} }'); } });
class P { function __construct(public $v = null) {} }
class Q extends P {}
abstract class Ab {}
class Thr { function __construct() { throw new \LogicException('ctor'); } }
class F { public function __construct(private string $factory) {} public function make($v) { return new $this->factory($v); } }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $f = new F(P::class); $g = new F(Q::class); echo get_class($f->make(1)), $f->make(2)->v, get_class($g->make(3)), ' ';
    $c = 'App\Lazy'; $o = new $c(5); echo $o->v, ' ';
    $c = 'App\Nope'; try { new $c; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $c = Ab::class; try { new $c; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    $c = new Q(1); $d = new $c(2); echo get_class($d), $d->v, ' ';
    $c = Thr::class; try { new $c; } catch (\LogicException $e) { echo $e->getMessage(), '@', $e->getLine(), ' '; }
    $c = 'App\P'; $e = new $c(...[7]); echo $e->v, "\n";
  }
}
run();
"#),
        r#"App\P2App\Q autoload(App\Lazy) 5 autoload(App\Nope) Class "App\Nope" not found Cannot instantiate abstract class App\Ab App\Q2 ctor@7 7
App\P2App\Q 5 autoload(App\Nope) Class "App\Nope" not found Cannot instantiate abstract class App\Ab App\Q2 ctor@7 7
App\P2App\Q 5 autoload(App\Nope) Class "App\Nope" not found Cannot instantiate abstract class App\Ab App\Q2 ctor@7 7
"#
    );
}

/// Static calls of one method from several sites (each site keeps its
/// own resolution, also when another site resolved the method first):
/// late static binding, protected and private methods from inside and
/// outside their class, a trait's method. Expected output:
/// `/usr/bin/php8.5` (8.5.13).
#[test]
fn static_calls_from_several_sites() {
    assert_eq!(
        run(r#"<?php
namespace V;
class P { public static function who() { return static::class; } protected static function prot() { return 'prot'; } private static function priv() { return 'priv'; }
  public static function viaSelf() { return self::prot() . self::priv(); } }
class Q extends P { public static function call() { return P::prot() . P::who() . Q::who() . P::who() . static::who(); } }
trait T { public static function t() { return static::class; } }
class U { use T; }
function f() { return P::who() . Q::who() . P::who() . U::t() . P::who(); }
for ($i = 0; $i < 300; $i++) { $o = f() . Q::call() . P::viaSelf() . Q::call() . P::viaSelf(); }
echo $o, "\n";
try { P::prot(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
function g() { try { return P::prot(); } catch (\Error $e) { return $e->getMessage(); } }
function h() { return Q::call() . g() . g(); }
for ($i = 0; $i < 300; $i++) { $o = h(); } echo $o, "\n";
"#),
        r#"V\PV\QV\PV\UV\PprotV\PV\QV\PV\QprotprivprotV\PV\QV\PV\Qprotpriv
Call to protected method V\P::prot() from global scope
protV\PV\QV\PV\QCall to protected method V\P::prot() from global scopeCall to protected method V\P::prot() from global scope
"#
    );
}

/// Static call starts the tape cannot prepare (an argument is a call or
/// a superglobal) run from its decline step: autoloading, unknown classes
/// and methods, `__callStatic()`, `parent::` forwarding `$this`, a throw
/// while the arguments are evaluated. Expected output: `/usr/bin/php8.5`
/// (8.5.13).
#[test]
fn unprepared_static_calls() {
    assert_eq!(
        run(r#"<?php
namespace W;
spl_autoload_register(function ($c) { echo "load($c) "; if ($c === 'W\Late') { eval('namespace W; class Late { static function m($x) { return "late$x"; } }'); } });
class Str { static function studly($s) { return ucfirst($s); } static function wrap(...$a) { return '[' . implode(',', $a) . ']'; } }
class Base { public $n = 'b'; function who($x) { return static::class . $this->n . $x; } static function __callStatic($m, $a) { return "cs:$m:" . count($a); } }
class Kid extends Base { function who($x) { return 'kid>' . parent::who(self::id(strtoupper($x))) . static::magic(strlen($x)); } static function id($v) { return $v; } }
function boom() { throw new \RuntimeException('boom'); }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $k = new Kid;
    echo Str::studly(strtolower('ABC')), Str::wrap(count([1, 2]), Str::studly('x'), $_GET['n'] ?? 1), ' ', $k->who('q'), ' ';
    echo Late::m(strlen('ab')), ' ';
    try { echo Nope::m(strlen('a')); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { echo Str::missing(strlen('a')); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { echo Str::wrap(boom()); } catch (\RuntimeException $e) { echo $e->getMessage(), '@', $e->getLine(), ' '; }
    echo Base::other(strlen('abc'), 2), "\n";
  }
}
run();
"#),
        r#"Abc[2,X,1] kid>W\KidbQcs:magic:1 load(W\Late) late2 load(W\Nope) Class "W\Nope" not found Call to undefined method W\Str::missing() boom@7 cs:other:2
Abc[2,X,1] kid>W\KidbQcs:magic:1 late2 load(W\Nope) Class "W\Nope" not found Call to undefined method W\Str::missing() boom@7 cs:other:2
Abc[2,X,1] kid>W\KidbQcs:magic:1 late2 load(W\Nope) Class "W\Nope" not found Call to undefined method W\Str::missing() boom@7 cs:other:2
"#
    );
}

/// `getAttributes()` filters: by name (a leading `\` matches nothing, as
/// `zend_get_attribute_str`), by `IS_INSTANCEOF`, none matching (the
/// shared empty array, still a value of its own), repeated attributes,
/// and the flag and class errors. Expected output: `/usr/bin/php8.5`
/// (8.5.13).
#[test]
fn attribute_filters() {
    assert_eq!(
        run(r#"<?php
#[Attribute(Attribute::TARGET_ALL | Attribute::IS_REPEATABLE)] class Boot { function __construct(public $v = 0) {} }
#[Attribute] class Init {} class Sub extends Boot {}
#[Boot(1)] #[Boot(2)] #[Init] class M { function a() {} #[Boot(3)] function c() {} #[Sub] function d() {} }
$r = new ReflectionClass('M');
foreach ($r->getMethods() as $m) {
  $all = $m->getAttributes(); $b = $m->getAttributes(Boot::class); $io = $m->getAttributes(Boot::class, ReflectionAttribute::IS_INSTANCEOF);
  echo $m->getName(), ' ', count($all), count($b), count($io), ' ', var_export($b === [], true), ' ', implode(',', array_map(fn($a) => $a->getName(), $io)), "\n";
  $b[] = 'x'; echo count($b), ' ', count($m->getAttributes(Boot::class)), "\n";
}
foreach ($r->getAttributes() as $a) echo $a->getName(), $a->isRepeated() ? 'R' : '', ' ', json_encode($a->getArguments()), "\n";
echo count($r->getAttributes('nope')), count($r->getAttributes('\Boot')), "\n";
try { $r->getAttributes(Boot::class, 5); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
try { $r->getAttributes('Missing', ReflectionAttribute::IS_INSTANCEOF); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        r#"a 000 true 
1 0
c 111 false Boot
2 1
d 101 true Sub
1 0
BootR [1]
BootR [2]
Init []
00
ReflectionClass::getAttributes(): Argument #2 ($flags) must be a valid attribute filter flag
Class "Missing" not found
"#
    );
}

/// Promoted `public readonly` properties (implicitly `protected(set)`)
/// initialized on the cache hit from their declaring class; a second
/// write, a write from outside, a subclass's constructor, `__clone()`;
/// and a base class's property written for many subclasses (refilled on
/// the tape's decline step), whose replaced values run destructors.
/// Expected output: `/usr/bin/php8.5` (8.5.13).
#[test]
fn readonly_promotion_and_polymorphic_writes() {
    assert_eq!(
        run(r#"<?php
namespace S;
class Box { function __construct(public $v = 0) {} }
class R { public function __construct(public readonly Box $p, public readonly string $s, protected readonly int $n = 3, public private(set) readonly array $a = []) {}
  function again() { $this->p = new Box(9); } function n() { return $this->n; } }
class K extends R { public function __construct(Box $b) { parent::__construct($b, 'k'); } }
class C2 extends R { public readonly int $extra; public function __construct() { $this->extra = 5; parent::__construct(new Box(2), 'c'); } }
class Cl { public function __construct(public readonly Box $b) {} public function __clone() { $this->b = new Box(42); } }
abstract class Asset { protected $_name = ''; public $d; function setName($n) { $this->_name = $n; return $this; } function name() { return $this->_name; } function __destruct() {} }
class T1 extends Asset {} class T2 extends Asset {} class T3 extends Asset {} class T4 extends Asset { function __destruct() { echo "d4 "; } }
function run() {
  for ($i = 0; $i < 3; $i++) {
    $r = new R(new Box($i), "s$i"); echo $r->p->v, $r->s, $r->n(), count($r->a), ' ';
    try { $r->again(); } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { $r->p = new Box; } catch (\Error $e) { echo $e->getMessage(), ' '; }
    try { new R('x', 's'); } catch (\TypeError $e) { echo 'TE ', substr($e->getMessage(), 0, 45), ' '; }
    $k = new K(new Box(7)); echo $k->p->v, $k->s, ' ';
    $c = new C2; echo $c->extra, $c->p->v, ' ';
    $o = new Cl(new Box(1)); $o2 = clone $o; echo $o->b->v, $o2->b->v, ' ';
    foreach ([new T1, new T2, new T3, new T4, new T1] as $a) { echo $a->setName("n$i")->name(); $a->d = new T4; $a->d = null; }
    echo "\n";
  }
}
run();
"#),
        r#"0s030 Cannot modify readonly property S\R::$p Cannot modify readonly property S\R::$p TE S\R::__construct(): Argument #1 ($p) must be  7k 52 142 n0d4 n0d4 n0d4 n0d4 n0d4 d4 
1s130 Cannot modify readonly property S\R::$p Cannot modify readonly property S\R::$p TE S\R::__construct(): Argument #1 ($p) must be  7k 52 142 n1d4 n1d4 n1d4 n1d4 n1d4 d4 
2s230 Cannot modify readonly property S\R::$p Cannot modify readonly property S\R::$p TE S\R::__construct(): Argument #1 ($p) must be  7k 52 142 n2d4 n2d4 n2d4 n2d4 n2d4 d4 
"#
    );
}
