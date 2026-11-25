// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$o[$undef] = v` and `$o[k] = $undef` on an object with native
//! dimension handlers while the error handler throws on "Undefined
//! variable": php-src goes on with the opcode, so the handler still
//! writes (`spl_array_write_dimension` appends for a null offset) and an
//! error it throws is chained onto the exception in flight; a user
//! `offsetSet()` is not called (`zend_call_function` returns at once with
//! an exception pending), and diagnostics raised meanwhile are lost.
//! Expected output from `/usr/bin/php8.5 -n`.
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
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// ArrayObject writes and appends; a user ArrayAccess does not.
#[test]
fn native_handlers_write_after_the_warning_threw() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($n, $m) { throw new Exception($m); });
class AA implements ArrayAccess {
  public $d = [];
  function offsetGet($k): mixed { echo "get\n"; return $this->d[$k] ?? null; }
  function offsetSet($k, $v): void { echo "set "; var_dump($k, $v); $this->d[$k] = $v; }
  function offsetExists($k): bool { return isset($this->d[$k]); }
  function offsetUnset($k): void {}
}
function f() {
  $o = new ArrayObject([]);
  try { $o[$undef] = 1; } catch (Exception $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  var_dump(count($o));
  $a = new AA;
  try { $a[$undef] = 1; } catch (Exception $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  var_dump(count($a->d));
  try { $o[$undef] .= 'x'; } catch (Exception $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  var_dump(count($o));
  try { $a['k'] = $undef; } catch (Exception $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  var_dump($a->d);
  try { $o[] = $undef; } catch (Exception $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  var_dump(count($o));
}
f(); f(); f();
"###),
        r###"Exception: Undefined variable $undef
int(1)
Exception: Undefined variable $undef
int(0)
Exception: Undefined variable $undef
int(2)
Exception: Undefined variable $undef
array(0) {
}
Exception: Undefined variable $undef
int(3)
Exception: Undefined variable $undef
int(1)
Exception: Undefined variable $undef
int(0)
Exception: Undefined variable $undef
int(2)
Exception: Undefined variable $undef
array(0) {
}
Exception: Undefined variable $undef
int(3)
Exception: Undefined variable $undef
int(1)
Exception: Undefined variable $undef
int(0)
Exception: Undefined variable $undef
int(2)
Exception: Undefined variable $undef
array(0) {
}
Exception: Undefined variable $undef
int(3)
"###
    );
}

/// The keys ArrayObject uses, SplObjectStorage and SplFixedArray (its
/// TypeError chained onto the warning's exception).
#[test]
fn spl_containers_after_the_warning_threw() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($n, $m) { throw new Exception($m); });
function f() {
  $o = new ArrayObject([]);
  try { $o[$undef] = 1; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $o[$undef] .= 'x'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $o[] = $undef; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $o['k'] = $undef; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $o[$undef]++; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $o[$undef][] = 3; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { unset($o[$undef]); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  var_dump($o->getArrayCopy());
  $s = new SplObjectStorage; $x = new stdClass;
  try { $s[$x] = $undef; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  var_dump(count($s));
  $fa = new SplFixedArray(2);
  try { $fa[0] = $undef; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
  try { $fa[$undef] = 5; } catch (Throwable $e) { echo get_class($e), $e->getMessage(), ' ', $e->getPrevious()?->getMessage(), "\n"; }
  var_dump($fa->toArray());
}
f(); f(); f();
"###),
        r###"Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
array(4) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  NULL
  ["k"]=>
  NULL
}
Undefined variable $undef
int(1)
Undefined variable $undef
TypeErrorCannot access offset of type null on SplFixedArray Undefined variable $undef
array(2) {
  [0]=>
  NULL
  [1]=>
  NULL
}
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
array(4) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  NULL
  ["k"]=>
  NULL
}
Undefined variable $undef
int(1)
Undefined variable $undef
TypeErrorCannot access offset of type null on SplFixedArray Undefined variable $undef
array(2) {
  [0]=>
  NULL
  [1]=>
  NULL
}
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
Undefined variable $undef
array(4) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  NULL
  ["k"]=>
  NULL
}
Undefined variable $undef
int(1)
Undefined variable $undef
TypeErrorCannot access offset of type null on SplFixedArray Undefined variable $undef
array(2) {
  [0]=>
  NULL
  [1]=>
  NULL
}
"###
    );
}
