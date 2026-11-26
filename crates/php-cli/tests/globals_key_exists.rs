// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `array_key_exists($key, $GLOBALS)` (`ZEND_FETCH_GLOBALS` then
//! `ZEND_ARRAY_KEY_EXISTS`), answered from the symbol table without the
//! copy of it when the key is a string or an int: variables that are set
//! (null included), unset or never defined, numeric names, superglobals,
//! `global` bindings made in a function, and the keys whose diagnostics
//! the op keeps (null, float, bool, array). Expected output: PHP 8.5.

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

#[test]
fn array_key_exists_on_globals_answers_as_the_copy_would() {
    let out = run(r#"<?php
$a = 1; $n = null; $GLOBALS['7'] = 'seven'; $u = 1; unset($u);
function f($k) { return array_key_exists($k, $GLOBALS); }
function g() { global $late; $late = 5; return array_key_exists('late', $GLOBALS); }
foreach (['a', 'n', 'u', 'nope', '7', 7, '07', '_SERVER', 'GLOBALS', 'argv', ''] as $k) { echo var_export($k, true), ' => ', var_export(f($k), true), "\n"; }
var_dump(g(), array_key_exists('a', $GLOBALS), array_key_exists(7, $GLOBALS));
foreach ([null, 1.5, true, false] as $k) { echo var_export(@f($k), true), "\n"; }
try { f([]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$x = $GLOBALS; var_dump(array_key_exists('a', $x), isset($x['n']));
function h() { $v = 'dyn'; $GLOBALS[$v] = 1; return [array_key_exists($v, $GLOBALS), array_key_exists($v . 'x', $GLOBALS)]; } var_dump(h());
"#);
    assert_eq!(
        out,
        r#"'a' => true
'n' => true
'u' => false
'nope' => false
'7' => true
7 => true
'07' => false
'_SERVER' => true
'GLOBALS' => false
'argv' => true
'' => false
bool(true)
bool(true)
bool(true)
false
false
false
false
Cannot access offset of type array on array
bool(true)
bool(false)
array(2) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
}
"#
    );
}
