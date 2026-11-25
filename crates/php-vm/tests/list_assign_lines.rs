// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lines of list assignments written over several lines.
//! `zend_compile_list_assign` (`Zend/zend_compile.c`) emits each element's
//! ZEND_FETCH_LIST_R before compiling that element's target, on the line
//! the previous target left (a variable's ZEND_ASSIGN moves to the
//! variable's line) or, for the first element, the line compiling a
//! non-variable source or the `foreach` subject left; a key moves to its
//! own line, a nested list's first element is fetched where its own fetch
//! from the parent is, and a list's line is its first variable's. Expected output from `/usr/bin/php8.5 -n`.
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
fn list_elements_are_fetched_on_php_lines() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function lists($arr) {
  [$a] =
    $arr;
  [
    $b,
    [
      $c,
      $d
    ],
    $e
  ] = $arr;
  $r = [
    $p,
    $q
  ] = [];
  [
    'x' => $g,
    'y' => $h
  ] = $arr;
  foreach ([$arr, [5]] as [
      $m,
      $n]) {}
  $nn = [[]];
  [
   [
    $s,
    $t
   ]
  ] = $nn;
  $y = [1, []];
  [$u,
   [$v,
    $w]] = $y;
  [[
    $s2,
    $t2
  ]] = [[]];
  foreach ([[[]]] as [
      [$a2,
       $b2]]) {}
  return [$s2, $t2, $a, $b, $c, $d, $e, $r, $p, $q, $g, $h, $m, $n, $s, $t, $u, $v, $w];
}
for ($i = 0; $i < 3; $i++) { echo json_encode(lists([])), "\n"; }
"###),
        r###"Undefined array key 0 @4
Undefined array key 0 @7
Undefined array key 1 @7
Undefined array key 2 @10
Undefined array key 0 @17
Undefined array key 1 @15
Undefined array key "x" @19
Undefined array key "y" @20
Undefined array key 0 @22
Undefined array key 1 @23
Undefined array key 1 @23
Undefined array key 0 @28
Undefined array key 1 @28
Undefined array key 0 @33
Undefined array key 1 @34
Undefined array key 0 @39
Undefined array key 1 @37
Undefined array key 0 @40
Undefined array key 1 @41
[null,null,null,null,null,null,null,[],null,null,null,null,5,null,null,null,1,null,null]
Undefined array key 0 @4
Undefined array key 0 @7
Undefined array key 1 @7
Undefined array key 2 @10
Undefined array key 0 @17
Undefined array key 1 @15
Undefined array key "x" @19
Undefined array key "y" @20
Undefined array key 0 @22
Undefined array key 1 @23
Undefined array key 1 @23
Undefined array key 0 @28
Undefined array key 1 @28
Undefined array key 0 @33
Undefined array key 1 @34
Undefined array key 0 @39
Undefined array key 1 @37
Undefined array key 0 @40
Undefined array key 1 @41
[null,null,null,null,null,null,null,[],null,null,null,null,5,null,null,null,1,null,null]
Undefined array key 0 @4
Undefined array key 0 @7
Undefined array key 1 @7
Undefined array key 2 @10
Undefined array key 0 @17
Undefined array key 1 @15
Undefined array key "x" @19
Undefined array key "y" @20
Undefined array key 0 @22
Undefined array key 1 @23
Undefined array key 1 @23
Undefined array key 0 @28
Undefined array key 1 @28
Undefined array key 0 @33
Undefined array key 1 @34
Undefined array key 0 @39
Undefined array key 1 @37
Undefined array key 0 @40
Undefined array key 1 @41
[null,null,null,null,null,null,null,[],null,null,null,null,5,null,null,null,1,null,null]
"###
    );
}
