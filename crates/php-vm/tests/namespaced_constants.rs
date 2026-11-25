// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Global constants in a namespace: php-src keys the constant table by the
//! name with its namespace part lowercased (`zend_register_constant`,
//! Zend/zend_constants.c) and looks a namespaced name up the same way
//! (`zend_get_constant_ex`, `zend_quick_get_constant`), so `\ns\NC`,
//! `defined('ns\NC')` and `constant('nS\NC')` find `NS\NC`, the constant's
//! own name stays case-sensitive, a namespaced constant comes before the
//! global fallback, and the constant keeps its spelling for display.
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

#[test]
fn namespace_part_of_a_constant_name_is_case_insensitive() {
    assert_eq!(
        run(r###"<?php
namespace NS { const NC = 1; const X = 'ns-x'; function f() { return X; } }
namespace Dep { #[\Deprecated] const D = 7; }
namespace {
set_error_handler(function ($no, $msg) { echo "[$msg]\n"; return true; });
const X = 'global-x';
function g() { $o = []; for ($i = 0; $i < 3; $i++) { $o[] = \ns\NC . \Ns\NC . \NS\NC; } return implode(',', $o); }
echo g(), "\n";
echo \defined('ns\NC') ? 'y' : 'n', \defined('\nS\NC') ? 'y' : 'n', \defined('NS\nc') ? 'y' : 'n', \constant('nS\NC'), \constant('\NS\NC'), "\n";
echo NS\f(), NS\f(), "\n";
var_dump(define('ns\NC', 2));
var_dump(define('Ab\Cd\Y', 3), \ab\cd\Y, \constant('AB\CD\Y'));
var_dump(array_keys(get_defined_constants(true)['user']));
try { echo \constant('NS\nc'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { echo \NS\nc; } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo \dep\D, constant('DEP\D'), "\n";
}
"###),
        r###"111,111,111
yyn11
ns-xns-x
[Constant ns\NC already defined, this will be an error in PHP 9]
bool(false)
bool(true)
int(3)
int(3)
array(5) {
  [0]=>
  string(5) "NS\NC"
  [1]=>
  string(4) "NS\X"
  [2]=>
  string(5) "Dep\D"
  [3]=>
  string(1) "X"
  [4]=>
  string(7) "Ab\Cd\Y"
}
Undefined constant "NS\nc"
Undefined constant "NS\nc"
[Constant Dep\D is deprecated]
7[Constant Dep\D is deprecated]
7
"###
    );
}
