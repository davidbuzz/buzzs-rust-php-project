// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! An array access on an object without ArrayAccess while an exception is
//! in flight: an "Undefined variable" offset or value whose error handler
//! throws does not stop the opcode, and the "Cannot use object of type
//! ... as array" error (`zend_bad_array_access`,
//! Zend/zend_object_handlers.c) is chained onto the handler's exception;
//! `$o[k] op= v` throws that error twice (`zend_binary_assign_op_obj_dim`,
//! Zend/zend_execute.c). Expected output from `/usr/bin/php8.5 -n`.
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

#[test]
fn object_as_array_error_chains_onto_the_handler_exception() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($no, $msg) { throw new Exception("handler: $msg"); });
function show($t) { echo get_class($t), ': ', $t->getMessage(), ' / previous: ', $t->getPrevious() ? get_class($t->getPrevious()) . ': ' . $t->getPrevious()->getMessage() : 'none', "\n"; }
function f() {
  $e = new stdClass;
  try { $e[$undef] = 1; } catch (Throwable $t) { show($t); }
  try { $e[$undef][1] = 1; } catch (Throwable $t) { show($t); }
  try { $e[$undef] .= 1; } catch (Throwable $t) { show($t); }
  try { $e[$undef] = &$x; } catch (Throwable $t) { show($t); }
  try { $e[$undef]++; } catch (Throwable $t) { show($t); }
  try { unset($e[$undef]); } catch (Throwable $t) { show($t); }
  try { $e[] = $undef; } catch (Throwable $t) { show($t); }
  try { $e[1] = $undef; } catch (Throwable $t) { show($t); }
  $a = [];
  try { $a[$undef] = 1; } catch (Throwable $t) { show($t); }
  var_dump($a);
  $s = 'abc';
  try { $s[$undef] = 'x'; } catch (Throwable $t) { show($t); }
  $i = 5;
  try { $i[$undef] = 1; } catch (Throwable $t) { show($t); }
  try { $this_ = new stdClass; $this_->p[$undef] = 1; } catch (Throwable $t) { show($t); }
  try { $q = new stdClass; $q->p = new stdClass; $q->p[$undef] = 1; } catch (Throwable $t) { show($t); }
}
f();
"###),
        r###"Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Error: Cannot use object of type stdClass as array
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
Exception: handler: Undefined variable $undef / previous: none
array(0) {
}
Exception: handler: Undefined variable $undef / previous: none
Error: Cannot use a scalar value as an array / previous: none
Exception: handler: Undefined variable $undef / previous: none
Error: Cannot use object of type stdClass as array / previous: Exception: handler: Undefined variable $undef
"###
    );
}

#[test]
fn object_as_array_assign_op_throws_twice() {
    assert_eq!(
        run(r###"<?php
function chain($t) { while ($t) { echo get_class($t), ': ', $t->getMessage(), ' <- '; $t = $t->getPrevious(); } echo "\n"; }
function g() {
  $e = new stdClass;
  try { $e[1] .= 1; } catch (Throwable $t) { chain($t); }
  try { $e[1]++; } catch (Throwable $t) { chain($t); }
  set_error_handler(function ($no, $msg) { throw new Exception("handler: $msg"); });
  try { $e[$undef] .= 1; } catch (Throwable $t) { chain($t); }
  try { $e[$undef]++; } catch (Throwable $t) { chain($t); }
  try { $e[$undef] = $undef2; } catch (Throwable $t) { chain($t); }
  try { $e[$undef][$undef2] = 1; } catch (Throwable $t) { chain($t); }
  try { $e[1] .= $undef; } catch (Throwable $t) { chain($t); }
  restore_error_handler();
}
g();
"###),
        r###"Error: Cannot use object of type stdClass as array <- Error: Cannot use object of type stdClass as array <- 
Error: Cannot use object of type stdClass as array <- 
Error: Cannot use object of type stdClass as array <- Error: Cannot use object of type stdClass as array <- Exception: handler: Undefined variable $undef <- 
Error: Cannot use object of type stdClass as array <- Exception: handler: Undefined variable $undef <- 
Error: Cannot use object of type stdClass as array <- Exception: handler: Undefined variable $undef <- 
Error: Cannot use object of type stdClass as array <- Exception: handler: Undefined variable $undef <- 
Error: Cannot use object of type stdClass as array <- Error: Cannot use object of type stdClass as array <- Exception: handler: Undefined variable $undef <- 
"###
    );
}
