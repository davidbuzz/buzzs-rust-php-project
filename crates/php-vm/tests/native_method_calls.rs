// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! User methods called from native code (magic property methods,
//! `ArrayAccess`, `Iterator`, `__toString()`): the direct call path and
//! the generic one it leaves the rest to.

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
