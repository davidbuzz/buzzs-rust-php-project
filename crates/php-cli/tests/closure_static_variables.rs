// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ReflectionFunction::getStaticVariables()` copies the function's static
//! variable table with `zval_add_ref` (ext/reflection): a reference only the
//! table holds comes out as its value (a `static $s`), one that something
//! else holds too stays a reference (a variable a closure uses by
//! reference). A debugging library marks the latter with `&`.
//! `getClosureUsedVariables()` keeps every reference. Expected output from
//! `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn static_variables_keep_shared_references() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("closure_static_variables.php");
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
function refs(array $a): string {
    $out = [];
    foreach ($a as $k => $v) {
        $r = ReflectionReference::fromArrayElement($a, $k);
        $out[] = $k . ($r ? ':ref' : ':val') . '=' . json_encode($v);
    }
    return implode(' ', $out);
}
function make() {
    $a = 123; $b = 123; $c = [1];
    $f = function ($x) use ($a, &$b, &$c) { static $s = 5; $b++; return $a + $b + $x; };
    return [$f, &$b];
}
[$f, $ref] = make();
$rf = new ReflectionFunction($f);
echo 'used: ', refs($rf->getClosureUsedVariables()), "\n";
echo 'static: ', refs($rf->getStaticVariables()), "\n";
$f(1);
echo 'used after a call: ', refs($rf->getClosureUsedVariables()), "\n";
echo 'static after a call: ', refs($rf->getStaticVariables()), "\n";
$u = $rf->getClosureUsedVariables();
$u['b'] = 1000; $u['a'] = 2000;
echo 'written through the array: ', refs($rf->getClosureUsedVariables()), ' returns ', $f(0), "\n";
var_dump($rf->getClosureUsedVariables());
$g = fn () => $ref;
echo 'arrow: ', refs((new ReflectionFunction($g))->getClosureUsedVariables()), "\n";
class K { function m() { $v = 1; return function () use (&$v) { return $v; }; } }
$h = (new K)->m();
echo 'method closure: ', refs((new ReflectionFunction($h))->getClosureUsedVariables()), "\n";
function gen() { static $n = 0; static $arr = [1, 2]; $n++; }
gen();
echo 'function statics: ', refs((new ReflectionFunction('gen'))->getStaticVariables()), "\n";
debug_zval_dump((new ReflectionFunction($f))->getStaticVariables());
"###;

const EXPECTED: &str = r###"used: a:val=123 b:ref=123 c:ref=[1]
static: a:val=123 b:val=123 c:val=[1] s:val=5
used after a call: a:val=123 b:ref=124 c:ref=[1]
static after a call: a:val=123 b:val=124 c:val=[1] s:val=5
written through the array: a:val=123 b:ref=1000 c:ref=[1] returns 1124
array(3) {
  ["a"]=>
  int(123)
  ["b"]=>
  &int(1001)
  ["c"]=>
  &array(1) {
    [0]=>
    int(1)
  }
}
arrow: ref:val=123
method closure: v:ref=1
function statics: n:val=1 arr:val=[1,2]
array(4) refcount(1){
  ["a"]=>
  int(123)
  ["b"]=>
  reference refcount(3) {
    int(1001)
  }
  ["c"]=>
  reference refcount(3) {
    array(1) packed refcount(2){
      [0]=>
      int(1)
    }
  }
  ["s"]=>
  int(5)
}
"###;
