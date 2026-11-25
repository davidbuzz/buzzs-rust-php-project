// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The tape's rows for values that are not numbers: the assignment of a
//! string or array literal to a variable, comparisons with a string, null,
//! a bool or the empty array, and the truth tests of such values, next to
//! the cases that must still take the generic path (references, objects
//! whose release runs a destructor, numeric strings, conversions that
//! warn). Each loop runs its body more than once so that a later iteration
//! runs on the tape. The expected output is `/usr/bin/php8.5 -n` on each
//! script (ref: Zend/zend_vm_def.h `ZEND_ASSIGN`, `ZEND_IS_IDENTICAL`,
//! `ZEND_IS_EQUAL`, `ZEND_JMPZ`, `ZEND_BOOL_NOT`).

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

/// `$cv = 'lit'`, `$cv = []`, `$cv = [1, 2]`, `$cv = array('a' => ...)`
/// over every kind of old value: a scalar, a string, an array (unique and
/// holding objects, whose destructors run before the next statement; shared,
/// whose objects live on), an object, a reference (written through), a
/// variable in a cycle. A literal array assigned and then written is
/// separated from the literal.
#[test]
fn literal_assignments() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~{$this->n} "; } }
function f() {
  for ($i = 0; $i < 3; $i++) {
    $s = 'lit';
    $e = [];
    $p = [1, 2];
    $m = array('a' => 1, 'b' => [2]);
    $s = 'other';
    $e[] = $i;
    $p[0] = $i;
    echo $s, count($e), json_encode($p), json_encode($m), ' ';
    $old = [new D($i), new D($i + 10)];
    echo 'a ';
    $old = 'x';
    echo 'b ';
    $sh = [new D($i + 20)]; $keep = $sh;
    $sh = [];
    echo 'c ';
    unset($keep);
    echo 'd ';
    $o = new D($i + 30);
    $o = 'y';
    echo 'e ';
    $r = 5; $ref = &$r; $ref = 'via'; echo $r, ' ';
    $ref = [3]; echo json_encode($r), ' ';
    unset($ref);
    $u = 1.5; $u = 'z'; $t = true; $t = []; $n = null; $n = 'q';
    echo $u, json_encode($t), $n, "\n";
  }
  $cyc = []; $cyc[] = &$cyc; $cyc = 'gone';
  echo $cyc, "\n";
}
f();
"#),
        "other1[0,2]{\"a\":1,\"b\":[2]} a ~0 ~10 b c ~20 d ~30 e via [3] z[]q\nother1[1,2]{\"a\":1,\"b\":[2]} a ~1 ~11 b c ~21 d ~31 e via [3] z[]q\nother1[2,2]{\"a\":1,\"b\":[2]} a ~2 ~12 b c ~22 d ~32 e via [3] z[]q\ngone\n"
    );
}
