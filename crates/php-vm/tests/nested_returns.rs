// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Returns from the first frame of a nested execution (`__toString()`,
//! `__get()`, `offsetGet()`, `getIterator()`, `count()`, `__invoke()`,
//! destructors: user code called from inside the VM, `zend_call_function`)
//! take the direct return path when they can and give the execution its
//! result: declared return types, with and without coercion, arrays,
//! strings, exceptions thrown instead of a return, and recursion through
//! such calls. The expected output is what the generic return printed
//! before (no PHP is available as an oracle where this was written).

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
fn nested_executions_return_their_results() {
    assert_eq!(
        run(r#"<?php
class S { function __construct(public $v) {} function __toString(): string { return (string) $this->v; } }
class G { private $d = ['a' => 1, 'b' => 'two']; function __get($k) { return $this->d[$k] ?? null; } function __isset($k): bool { return isset($this->d[$k]); } }
class A implements ArrayAccess, IteratorAggregate, Countable {
  function offsetExists($o): bool { return $o !== 'no'; }
  function offsetGet($o): mixed { return $o === 'arr' ? [1, 2] : strtoupper_($o); }
  function offsetSet($o, $v): void {}
  function offsetUnset($o): void {}
  function getIterator(): Iterator { yield 'x' => 1; yield 'y' => 2; }
  function count(): int { return "3"; }
}
function strtoupper_($s) { $r = ''; foreach (str_split_($s) as $c) { $r .= $c; } return $r . '!'; }
function str_split_($s) { $o = []; for ($i = 0; $i < strlen($s); $i++) { $o[] = $s[$i]; } return $o; }
class Inv { function __invoke($x): int { return $x * 2; } }
class Thr { function __toString(): string { throw new Exception('no string'); } }
class Coerce { function __toString(): string { return 42 . ''; } }
$s = new S(5); echo "$s", ' ', $s . 'x', "\n";
echo (string) new S(1.5), "\n";
$g = new G; var_dump($g->a, $g->b, $g->c, isset($g->a), isset($g->z));
$a = new A; var_dump($a['k'], $a['arr'], isset($a['no']), isset($a['yes']), count($a));
foreach ($a as $k => $v) { echo "$k=$v\n"; }
$i = new Inv; var_dump($i(21));
try { echo new Thr; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
echo new Coerce, "\n";
class D { function __destruct() { echo "destruct\n"; return 5; } }
function mk() { $d = new D; return 1; }
var_dump(mk());
function nested($n) { return $n > 0 ? (string) new S(nested($n - 1) + 1) : '0'; }
var_dump(nested(30));
echo "done\n";
"#),
        r#"
Deprecated: Returning a value from a destructor is deprecated in t.php on line 25
5 5x
1.5
int(1)
string(3) "two"
NULL
bool(true)
bool(false)
string(2) "k!"
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
bool(false)
bool(true)
int(3)
x=1
y=2
int(42)
no string
42
destruct
int(1)
string(2) "30"
done
"#
    );
}
