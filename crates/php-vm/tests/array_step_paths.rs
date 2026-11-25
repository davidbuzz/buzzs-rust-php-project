// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The tape's array-element and `foreach` instructions on their own short
//! paths (the reset of a loop over a variable's array, the step at the
//! loop's end, the free of the iterator, element writes and `isset` by a
//! string key, element writes through a property) next to the cases that
//! must keep their general paths: holes, references, symbol tables,
//! copies taken at reset, destructors. Each loop runs more than once so
//! that a later round runs on the tape. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_FE_RESET_R`, `ZEND_FE_FETCH_R`, `ZEND_FE_FREE`, `ZEND_ASSIGN_DIM`,
//! `ZEND_ISSET_ISEMPTY_DIM_OBJ`, `ZEND_ASSIGN_OBJ` of a dimension).

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

/// `foreach` by value over empty arrays, hashes and lists with holes at
/// the end, an array grown inside its own loop, a variable behind a
/// reference, nested loops over one array, an iterator holding the last
/// handle of objects with destructors, literals and temporaries, a frame
/// with a symbol table, and loop variables that held other values.
#[test]
fn foreach_reset_end_and_free() {
    assert_eq!(run(FOREACH), FOREACH_OUT);
}

const FOREACH: &str = r#"<?php
class D { public function __construct(public $n) {} public function __destruct() { echo "d{$this->n} "; } }
function f() {
    for ($round = 0; $round < 3; $round++) {
        $empty = [];
        foreach ($empty as $k => $v) { echo "never"; }
        foreach ($empty as $v) { echo "never"; }
        $a = ['a' => 1, 'b' => 'two', 'c' => 3.5];
        foreach ($a as $k => $v) { echo "$k=$v "; }
        foreach ($a as $v) { echo "$v "; }
        // Holes at the end: positions left, but no element.
        $h = ['x' => 1, 'y' => 2, 'z' => 3];
        unset($h['y'], $h['z']);
        foreach ($h as $k => $v) { echo "$k=$v "; }
        $l = [10, 20, 30];
        unset($l[2]);
        foreach ($l as $k => $v) { echo "$k=$v "; }
        // The loop iterates the copy taken at reset.
        $m = [1, 2];
        foreach ($m as $k => $v) { $m[] = $v * 10; echo "$k:$v "; }
        echo count($m), " ";
        // Over a reference.
        $r = [5, 6];
        $alias = &$r;
        foreach ($r as $k => $v) { echo "$k/$v "; }
        unset($alias);
        // Nested over the same array.
        foreach ($a as $k1 => $v1) { foreach ($a as $k2 => $v2) { echo $k1, $k2, ' '; } }
        // The iterator's handle is the last one: freed at the loop's end.
        $objs = [new D($round), new D($round + 10)];
        foreach ($objs as $o) { }
        unset($o);
        echo "| ";
        $objs = null;
        echo "after\n";
        // A literal and a temporary.
        foreach ([1, 2] as $v) { echo $v; }
        foreach (array_reverse($a) as $k => $v) { echo $k; }
        echo "\n";
    }
}
f();
function g() {
    $name = 'arr';
    $$name = ['p' => 1, 'q' => 2];
    foreach ($arr as $k => $v) { echo "$k=$v "; }
    foreach ($arr as $v) { echo "$v "; }
    echo "\n";
}
g(); g();
function h() {
    $a = ['k' => 1];
    $k = 'old';
    $v = [1, 2];
    foreach ($a as $k => $v) { }
    var_dump($k, $v);
    $x = null;
    foreach ($a as $x) {}
    var_dump($x);
}
h();
"#;

const FOREACH_OUT: &str = r#"a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d0 d10 after
12cba
a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d1 d11 after
12cba
a=1 b=two c=3.5 1 two 3.5 x=1 0=10 1=20 0:1 1:2 4 0/5 1/6 aa ab ac ba bb bc ca cb cc | d2 d12 after
12cba
p=1 q=2 1 2 
p=1 q=2 1 2 
string(1) "k"
int(1)
int(1)
"#;
