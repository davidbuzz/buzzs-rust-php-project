// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$x . ''`: which string the result is. `ZEND_CONCAT` (Zend/zend_vm_def.h)
//! returns the other operand itself only for an empty operand that is NOT a
//! literal; with an empty literal on the right, a left string that is shared
//! or interned is copied into a new string (`zend_string_alloc` + `memcpy`),
//! and one nobody else holds is extended by nothing and stays itself. Code
//! separates a value with `$row['k'] . ''` and relies on it
//! (ext/mysqli/tests/mysqli_result_references.phpt). `str_repeat($s, 1)` is
//! a new string too (`zend_string_safe_alloc`), never its argument.
//! `debug_zval_dump()` shows which is which. Run through the `php` binary,
//! in a function (the tape's rows) and at the top level (the frame-level
//! helper). Expected output from `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn an_empty_literal_operand_copies_a_shared_string() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("concat_empty_literal.php");
    std::fs::write(&path, SCRIPT).expect("write script");
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .arg("-n")
        .arg(&path)
        .output()
        .expect("spawn php");
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
}

const SCRIPT: &str = r###"<?php
function sep($row) { $o = []; foreach ($row as $k => $v) { $o[$k] = $row[$k] . ''; } return $o; }
function f() {
    $row = ['a' => str_repeat('x', 1), 'b' => str_repeat('yz', 2), 'c' => 'lit'];
    $copy = sep($row);
    debug_zval_dump($row, $copy);
    $t = str_repeat('q', 3) . '';      // nobody else holds the temporary
    $u = $row['b'] . '' . '';
    $e = '' . $row['a'];
    debug_zval_dump($t, $u, $e);
}
f(); f();
$g = ['k' => str_repeat('g', 1)]; $h = $g['k'] . ''; debug_zval_dump($g, $h);
"###;

const EXPECTED: &str = r###"array(3) refcount(2){
  ["a"]=>
  string(1) "x" refcount(1)
  ["b"]=>
  string(4) "yzyz" refcount(1)
  ["c"]=>
  string(3) "lit" interned
}
array(3) refcount(2){
  ["a"]=>
  string(1) "x" refcount(1)
  ["b"]=>
  string(4) "yzyz" refcount(1)
  ["c"]=>
  string(3) "lit" refcount(1)
}
string(3) "qqq" refcount(2)
string(4) "yzyz" refcount(2)
string(1) "x" refcount(2)
array(3) refcount(2){
  ["a"]=>
  string(1) "x" refcount(1)
  ["b"]=>
  string(4) "yzyz" refcount(1)
  ["c"]=>
  string(3) "lit" interned
}
array(3) refcount(2){
  ["a"]=>
  string(1) "x" refcount(1)
  ["b"]=>
  string(4) "yzyz" refcount(1)
  ["c"]=>
  string(3) "lit" refcount(1)
}
string(3) "qqq" refcount(2)
string(4) "yzyz" refcount(2)
string(1) "x" refcount(2)
array(1) refcount(2){
  ["k"]=>
  string(1) "g" refcount(1)
}
string(1) "g" refcount(2)
"###;
