// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `switch` with a jump table (`ZEND_SWITCH_STRING` / `ZEND_SWITCH_LONG`,
//! `Op::Match` with a `switch` table) and without: every subject type
//! against string and int cases (a subject of another type falls through
//! to the loose `case` comparisons), numeric-string cases (no table), a
//! default in the middle, repeated cases, fall-through between cases, a
//! reference and an undefined subject (one warning per `case`),
//! temporaries, `continue 2` and literal subjects. Expected output from
//! `/usr/bin/php8.5`.

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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn switch_matches_php_src() {
    assert_eq!(
        run(r#"<?php
class T { function __toString(): string { return "b"; } }
function s($x) {
  switch ($x) {
    case "a": return "A";
    case "b": return "B";
    default: return "D";
    case "c": return "C";
    case "a": return "A2";
    case "": return "EMPTY";
  }
}
function n($x) {
  switch ($x) {
    case 1: return "one"; case 2: return "two"; case 3: return "three";
    case 4: return "four"; case 10: return "ten"; case -1: return "minus";
  }
  return "none";
}
function m($x) { // numeric-string cases: no table, loose compares
  switch ($x) { case "1e1": return "1e1"; case "abc": return "abc"; case " 5": return "sp5"; }
  return "none";
}
function f($x) { $r = ''; switch ($x) { case 'x': $r .= 'x'; case 'y': $r .= 'y'; break; case 'z': $r .= 'z'; } return $r; }
$o = new T;
foreach (["a", "b", "c", "", "B", "d", 0, 1, null, false, true, 0.0, $o, [], "0"] as $v) { echo var_export($v, true) === null ? '' : '', s($v), " "; }
echo "\n";
foreach ([1, 2, 10, -1, "1", "10", "1.0", 1.0, 10.5, true, false, null, "ten", " 4", "4 ", "0x1", 4] as $v) { echo n($v), " "; }
echo "\n";
foreach (["10", 10, 10.0, "abc", "5", 5, "1e1"] as $v) { echo m($v), " "; }
echo "\n";
echo f('x'), f('y'), f('z'), f('w'), "\n";
$a = "b"; $ref = &$a; echo s($ref), s(...[&$a]), "\n";
function u() { switch ($undef) { case "a": echo "A"; break; case "b": echo "B"; break; default: echo "D"; } echo "\n"; }
u();
function tmp() { switch (strtolower("B")) { case "a": return 1; case "b": return 2; } return 0; }
function tmp2() { switch (str_repeat("q", 2)) { case "a": return 1; case "b": return 2; } return 0; }
echo tmp(), tmp2(), "\n";
for ($i = 0; $i < 3; $i++) { switch ("k$i") { case "k0": echo 0; continue 2; case "k1": echo 1; break; default: echo "d"; } echo "|"; }
echo "\n";
$k = "c"; switch ($k) { case "a": case "b": echo "ab"; break; case "c": case "d": echo "cd"; break; } echo "\n";
switch (3) { case 1: echo 1; break; case 2: echo 2; break; case 3: echo 3; break; case 4: echo 4; break; case 5: echo 5; break; } echo "\n";
switch ("lit") { case "x": echo "x"; break; case "lit": echo "lit"; break; } echo "\n";
switch (PHP_INT_MAX) { case 1: case 2: case 3: case 4: case PHP_INT_MAX: echo "max"; } echo "\n";
"#).1,
        r#"A B C EMPTY D D D D EMPTY EMPTY A D B D D 
one two ten minus one ten one one none one none none none four four none four 
1e1 1e1 1e1 abc sp5 sp5 1e1 
xyyz
BB

Warning: Undefined variable $undef in t.php on line 34

Warning: Undefined variable $undef in t.php on line 34
D
20
01|d|
cd
3
lit
max
"#
    );
}
