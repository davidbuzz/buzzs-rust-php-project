// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `defined('NAME')` (`ZEND_DEFINED`, `Zend/zend_vm_def.h`) through its
//! run-time cache slot: a found constant stays found, a missing one is
//! missing only until the next `define()`; namespaced names match with
//! the namespace part in any case (`zend_quick_check_constant`), class
//! constant names (`K::C`) and number literals are looked up as strings.
//! Each call runs several times, so the cached answer is used after the
//! first. Expected output from `/usr/bin/php8.5 -n`.
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
fn defined_answers_follow_define() {
    assert_eq!(
        run(r###"<?php
namespace NS { const NC = 1; }
namespace {
class K { const C = 1; }
function d() {
    $o = [];
    for ($i = 0; $i < 3; $i++) {
        $o[] = var_export(defined('LATER'), true);
        $o[] = var_export(defined('PHP_VERSION'), true);
        $o[] = var_export(defined('NS\NC'), true) . var_export(defined('\NS\NC'), true) . var_export(defined('ns\NC'), true) . var_export(defined('NS\nc'), true);
        $o[] = var_export(defined('K::C'), true) . var_export(defined('K::D'), true);
        $o[] = var_export(defined('later'), true);
        if ($i == 0) { define('LATER', 5); }
        if ($i == 1) { define('OTHER' . $i, 1); }
        $o[] = var_export(defined('OTHER1'), true);
        if (!defined('ONCE')) { define('ONCE', $i); }
        $o[] = ONCE;
    }
    return implode(' ', $o);
}
echo d(), "\n";
function e() { $r = []; foreach (['A1', 'A2'] as $n) { for ($i = 0; $i < 2; $i++) { $r[] = (int) defined('A10') . (int) defined('A11') . (int) defined('A20'); define($n . $i, 1); } } return implode(',', $r); }
echo e(), "\n";
}
"###),
        r###"false true truetruetruefalse truefalse false false 0 true true truetruetruefalse truefalse false true 0 true true truetruetruefalse truefalse false true 0
000,100,110,111
"###
    );
}

#[test]
fn defined_matches_namespaces_in_any_case() {
    assert_eq!(
        run(r###"<?php
namespace Low { const LC = 1; }
namespace {
define('Ab\Cd\X', 2);
function f() { $o = ''; for ($i = 0; $i < 2; $i++) { $o .= var_export(defined('ab\CD\X'), true) . var_export(defined('Ab\Cd\x'), true) . var_export(defined('LOW\LC'), true) . var_export(defined('low\lc'), true) . ' '; } return $o; }
echo f(), "\n";
function g() { $o = ''; for ($i = 0; $i < 3; $i++) { $o .= (int) defined(1.5); if ($i == 1) define('1.5', 'x'); } return $o; }
echo g(), "\n";
}
"###),
        r###"truefalsetruefalse truefalsetruefalse 
001
"###
    );
}
