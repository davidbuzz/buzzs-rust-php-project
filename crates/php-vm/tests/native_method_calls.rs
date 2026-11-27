// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! User methods called from native code (magic property methods,
//! `ArrayAccess`, `Iterator`, `__toString()`): the direct call path and
//! the generic one it leaves the rest to.

mod common;

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

/// [`run`] with the standard library ([`common::vm_with_stdlib`]).
fn run_std(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// `__get()`/`__set()`/`__isset()`/`__unset()`, `ArrayAccess` (appends,
/// `??`, `empty()`, a throwing `offsetSet()`, a backtrace from
/// `offsetUnset()`, a by-reference `offsetGet()` with a static
/// variable), a `#[\Deprecated]` `__toString()`, `Iterator` and
/// generator `IteratorAggregate` loops, the `__get()` recursion guard, a
/// throwing `__toString()` and `__call()`, under `strict_types`.
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn native_to_user_method_calls() {
    let src = r#"<?php
declare(strict_types=1);
class M {
    private $data = [];
    function __get($n) { echo "get $n\n"; return $this->$n ?? strtoupper($n); }
    function __set($n, $v) { echo "set $n\n"; $this->data[$n] = $v; }
    function __isset($n) { return $n === 'yes'; }
    function __unset($n) { echo "unset $n\n"; }
}
$m = new M; var_dump($m->foo, isset($m->yes), isset($m->no)); $m->bar = 1; unset($m->baz);
class A implements ArrayAccess {
    public $d = [];
    function offsetExists(mixed $o): bool { echo "exists ", var_export($o, true), "\n"; return isset($this->d[$o]); }
    function offsetGet(mixed $o): mixed { return $this->d[$o] ?? 'none'; }
    function offsetSet(mixed $o, mixed $v): void { if ($o === null) { $this->d[] = $v; } else { $this->d[$o] = $v; } }
    function offsetUnset(mixed $o): void { unset($this->d[$o]); }
}
class T implements ArrayAccess {
    function offsetExists($o): bool { return true; }
    function offsetGet($o): mixed { return "got $o"; }
    function offsetSet($o, $v): void { throw new RuntimeException("no set $o"); }
    function offsetUnset($o): void { debug_print_backtrace(); }
}
$a = new A; $a['x'] = 1; $a[] = 2; var_dump($a['x'], $a['y'], isset($a['x']), empty($a['x']), $a[0] ?? 'd'); unset($a['x']); var_dump($a->d);
$t = new T; var_dump($t['5']); try { $t['k'] = 1; } catch (RuntimeException $e) { echo get_class($e), ': ', $e->getMessage(), ' line ', $e->getLine(), "\n"; } unset($t[1]);
try { var_dump($t['abc']); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
class R implements ArrayAccess { function offsetExists($o): bool { return true; } function &offsetGet($o): mixed { static $x = 0; $x++; return $x; } function offsetSet($o, $v): void {} function offsetUnset($o): void {} }
$r = new R; var_dump($r[1], $r[2]);
class D { #[\Deprecated("use other")] function __toString(): string { return 'dep'; } }
echo (string) new D, "\n";
class It implements Iterator { private $i = 0; function __construct(private array $a) {} function current(): mixed { return $this->a[$this->i]; } function key(): mixed { return $this->i; } function next(): void { $this->i++; } function rewind(): void { $this->i = 0; } function valid(): bool { return $this->i < count($this->a); } }
foreach (new It(['a', 'b']) as $k => $v) { echo "$k=$v "; } echo "\n";
class G implements IteratorAggregate { function getIterator(): Iterator { yield 1; yield 2; } } foreach (new G as $v) { echo $v; } echo "\n";
class Rec { function __get($n) { return $this->$n; } } var_dump((new Rec)->loop);
class Str { function __toString(): string { throw new LogicException('ts'); } } try { echo new Str; } catch (LogicException $e) { echo 'caught ', $e->getMessage(), "\n"; }
class V { function __call($n, $a) { return "$n:" . count($a); } } $v = new V; echo $v->anything(1, 2), "\n";
"#;
    let expected = r#"get foo
string(3) "FOO"
bool(true)
bool(false)
set bar
unset baz
exists 'x'
exists 'x'
exists 0
int(1)
string(4) "none"
bool(true)
bool(false)
int(2)
array(1) {
  [0]=>
  int(2)
}
string(5) "got 5"
RuntimeException: no set k line 21
#0 t.php(25): T->offsetUnset(1)
string(7) "got abc"
int(1)
int(2)

Deprecated: Method D::__toString() is deprecated, use other in t.php on line 30
dep
0=a 1=b 
12

Warning: Undefined property: Rec::$loop in t.php on line 34
NULL
caught ts
anything:2
"#;
    assert_eq!(run(src), expected);
}

/// The methods native code calls (`ArrayAccess`, `Countable`,
/// `IteratorAggregate`, `Iterator`, `__toString()`, `__get()`, callbacks)
/// end their nested execution with the plain return on the tape
/// (`Vm::return_plain` at the base): values, destructor order, an
/// exception thrown from one, a return value the declared type coerces.
/// Expected output: php-src's (PHP 8.5).
#[test]
fn methods_called_from_native_code_return_on_the_tape() {
    assert_eq!(
        run_std(
            r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~D{$this->n} "; } }
class AA implements ArrayAccess, IteratorAggregate, Countable {
  public $d = [];
  function offsetSet($k, $v): void { if ($k === 'boom') throw new Exception("boom"); $this->d[$k] = $v; }
  function offsetGet($k): mixed { return $this->d[$k] ?? new D("miss$k"); }
  function offsetExists($k): bool { return isset($this->d[$k]); }
  function offsetUnset($k): void { unset($this->d[$k]); }
  function getIterator(): Iterator { return new ArrayIterator($this->d); }
  function count(): int { return count($this->d) + $this->extra(); }
  function extra() { return 100; }
}
class S { function __construct(public $n) {} function __toString(): string { return $this->n . "/" . strlen($this->n); } }
class Bad { function __toString(): string { return 42; } }
class G { private $x = ['a' => 1]; function __get($n) { return $this->x[$n] ?? "no-$n"; } function __isset($n) { return $n === 'a'; } }
class It implements Iterator { private $i = 0; function current(): mixed { return $this->i * 10; } function key(): mixed { return "k$this->i"; }
  function next(): void { $this->i++; } function rewind(): void { $this->i = 0; } function valid(): bool { return $this->i < 3; } }
$a = new AA;
for ($i = 0; $i < 3; $i++) { $a[$i] = $i * 2; $a["s$i"] = new D("v$i"); }
echo $a[1], " ", $a[2], " ", isset($a[1]) ? "y" : "n", isset($a[9]) ? "y" : "n", " ", count($a), "\n";
$m = $a[7]; echo get_class($m), " "; unset($m); echo "\n";
unset($a["s1"]); echo "\n";
try { $a['boom'] = 1; } catch (Exception $e) { echo "caught ", $e->getMessage(), "\n"; }
foreach ($a as $k => $v) echo $k, "=", is_object($v) ? $v->n : $v, " "; echo "\n";
$s = new S("hey"); for ($i = 0; $i < 3; $i++) echo (string)$s, " ", "x$s", " "; echo "\n";
try { echo (string)new Bad; } catch (TypeError $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$g = new G; echo $g->a, " ", $g->b, " ", isset($g->a) ? 1 : 0, isset($g->q) ? 1 : 0, "\n";
foreach (new It as $k => $v) echo "$k:$v "; echo "\n";
echo implode(",", array_map(fn($x) => $x * 2, [1, 2, 3])), "\n";
$arr = [3, 1, 2]; usort($arr, fn($p, $q) => $p <=> $q); echo implode(",", $arr), "\n";
echo json_encode(array_filter([1, 0, 2, 3], function ($v) { return $v > 1; })), "\n";
$a = null; echo "end\n";
"#
        ),
        r#"2 4 yn 106
D ~Dmiss7 
~Dv1 
caught boom
0=0 s0=v0 1=2 2=4 s2=v2 
hey/3 xhey/3 hey/3 xhey/3 hey/3 xhey/3 
421 no-b 10
k0:0 k1:10 k2:20 
2,4,6
1,2,3
{"2":2,"3":3}
~Dv0 ~Dv2 end
"#
    );
}

/// `(string) $obj` of a user `__toString()` is called at once from the
/// tape (`declined_cast_object`): keys built from objects, a throwing
/// method, an object without one, nested conversions, an exception's own
/// conversion. Expected output: php-src's (PHP 8.5).
#[test]
fn string_casts_of_objects_call_to_string() {
    assert_eq!(
        run_std(
            r#"<?php
class S { function __construct(public $n) {} function __toString(): string { return "<{$this->n}>"; } }
class T { function __toString(): string { throw new RuntimeException("no"); } }
class N {}
class Nest { function __construct(public $in) {} function __toString(): string { return "[" . (string)$this->in . "]"; } }
$s = new S(1); $arr = [];
for ($i = 0; $i < 3; $i++) { $k = new S($i); $arr[(string)$k] = $i; $x = (string)$s; }
echo json_encode($arr), " $x\n";
try { $y = (string)new T; } catch (RuntimeException $e) { echo "caught ", $e->getMessage(), "\n"; }
try { $y = (string)new N; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
echo (string)new Nest(new Nest(new S("z"))), "\n";
$e = new Exception("msg"); $str = (string)$e; echo strlen($str) > 10 ? "exc ok" : "exc bad", "\n";
$t = new S("tmp"); echo strtoupper((string)$t), " ", (string)(new S("direct")), "\n";
"#
        ),
        r#"{"<0>":0,"<1>":1,"<2>":2} <1>
caught no
Error: Object of class N could not be converted to string
[[<z>]]
exc ok
<TMP> <direct>
"#
    );
}
