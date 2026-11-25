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
