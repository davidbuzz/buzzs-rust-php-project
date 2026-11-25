// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$target = &$source` (`zend_compile_assign_ref`, Zend/zend_compile.c):
//! the target is compiled (delayed) before the source, so the target's
//! variables take their slots first, the order `get_defined_vars()` and
//! `compact()` list them in, and the target's offset expressions and
//! calls run before the source's. Expected output from
//! `/usr/bin/php8.5 -n`.
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
fn assign_ref_compiles_the_target_first() {
    assert_eq!(
        run(r###"<?php
function f($x) { echo "f$x "; return $x; }
function o() { echo "o "; return new stdClass; }
function t() { $a = []; $b = []; $a[f(1)] = &$b[f(2)]; o()->p = &$b[f(3)]; $GLOBALS['z'] = &$b[f(4)]; echo "\n"; return array_keys(get_defined_vars()); }
echo implode(',', t()), "\n";
function h() { $q = &$r; $s = &$q; $t[1][2] = &$v; return array_keys(get_defined_vars()); }
echo implode(',', h()), "\n";
function c() { $x = 1; $p = &$m; return compact('x', 'p', 'm'); }
var_dump(array_keys(c()));
function k() { $o = new stdClass; $o->a = &$n; static $st; $st = &$w; return array_keys(get_defined_vars()); }
echo implode(',', k()), "\n";
function f2() { $u = &$undef; $w = 1; return array_keys(get_defined_vars()); }
echo implode(',', f2()), "\n";
function g() { $a = 1; $b = &$c['k']; return array_keys(get_defined_vars()); }
echo implode(',', g()), "\n";
"###),
        r###"f1 f2 o f3 f4 
a,b
q,r,s,t,v
array(3) {
  [0]=>
  string(1) "x"
  [1]=>
  string(1) "p"
  [2]=>
  string(1) "m"
}
o,n,st,w
u,undef,w
a,b,c
"###
    );
}
