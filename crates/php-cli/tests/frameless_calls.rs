// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frameless calls (`ZEND_FRAMELESS_ICALL_n`) of functions from the
//! extensions, by their global names and from namespaced code
//! (`ZEND_JMP_FRAMELESS`), run by the CLI with every extension loaded.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `source` (read from standard input) with no php.ini, every error
/// shown and none logged.
fn run(source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "log_errors=0",
            "-d",
            "display_errors=1",
            "-d",
            "error_reporting=-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("run the script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A namespaced call of `in_array()` or `str_contains()` runs the global
/// function in place unless the namespace declares one of that name when
/// the call site first runs; a declaration after that leaves the site as
/// it was, while a site that has not run yet calls the new function (both
/// as php-src's `ZEND_JMP_FRAMELESS` decides and caches). Loose and strict
/// `in_array()` over numeric strings, `null`, floats and `__toString()`
/// objects, arguments computed by calls and with side effects, the
/// `str_contains()` family with empty strings, other frameless functions,
/// a type error and a first-class callable. Expected output: php-src's
/// (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn namespaced_frameless_calls() {
    let src = r#"<?php
namespace A {
    function in_array($n, $h) { return "user A in_array"; }
    function probe() { return [in_array('x', ['x']), str_contains('abc', 'b')]; }
}
namespace B {
    function site1() { return in_array('x', ['x']); }
    function site2() { return in_array('x', ['x']); }
    function strs($h, $n) { return [str_contains($h, $n), str_starts_with($h, $n), str_ends_with($h, $n)]; }
}
namespace {
    var_dump(\A\probe());
    var_dump(\B\site1());
    eval('namespace B; function in_array($n, $h) { return "user B in_array"; }');
    var_dump(\B\site1(), \B\site2());
}
namespace C {
    class M { function getName() { return 'n' . ($GLOBALS['k'] ??= 0); } }
    $m = new M; $list = ['n0', 'x'];
    var_dump(in_array($m->getName(), $list), in_array($m->getName(), $list, true), in_array('1', [1, 2]), in_array('1', [1, 2], true), in_array(null, [0]), in_array('abc', [0]), in_array(0, ['a']), in_array('1e3', ['1000']), in_array(1.0, [1]));
    $i = 0; $arr = [1, 2, 3]; var_dump(in_array($i++, $arr), $i, in_array($arr[$i++] ?? 9, $arr), $i);
    var_dump(\B\strs('abc', ''), \B\strs('', ''), \B\strs('abc', 'abcd'), \B\strs('hello', 'lo'), \B\strs('hello', 'he'));
    class S { function __toString(): string { return 'x'; } }
    var_dump(in_array('x', [new S]), in_array(new S, ['x']));
    var_dump(in_array('a', ['a' => 1, 'b' => 'a']), strpos('abc', 'c'), substr('abcdef', 2, 2), max(1, 2), str_replace('a', 'b', 'aaa'), trim(' x '), strtolower('AB'));
    try { var_dump(in_array('x', 'notarray')); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump(str_contains(...)('abc', 'c'));
}
"#;
    let expected = r#"array(2) {
  [0]=>
  string(15) "user A in_array"
  [1]=>
  bool(true)
}
bool(true)
bool(true)
string(15) "user B in_array"
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
int(1)
bool(true)
int(2)
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(false)
  [1]=>
  bool(false)
  [2]=>
  bool(false)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(false)
}
bool(true)
bool(true)
bool(true)
int(2)
string(2) "cd"
int(2)
string(3) "bbb"
string(1) "x"
string(2) "ab"
in_array(): Argument #2 ($haystack) must be of type array, string given
bool(true)
"#;
    assert_eq!(run(src), expected);
}
