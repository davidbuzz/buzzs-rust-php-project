// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `isset($GLOBALS[$name])` answered from the symbol table (set, null,
//! unset, bound by reference, superglobals, a `global` made in a
//! function, numeric and empty names), and a discarded method result
//! that something else still holds (`$t->me();`, `ZEND_FREE`): released
//! without running or delaying a destructor. Expected output: PHP 8.5.

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
fn isset_on_globals_and_discarded_shared_objects() {
    let out = run(r#"<?php
$a = 1; $n = null; $z = 0; $arr = []; $u = 1; unset($u); $r = &$a;
function f($k) { return isset($GLOBALS[$k]); }
function g() { global $late; $late = 5; $k = 'late'; return isset($GLOBALS[$k]); }
foreach (['a', 'n', 'z', 'arr', 'u', 'r', 'nope', '_SERVER', '_GET', 'GLOBALS', 'argv', '', '7'] as $k) { echo "$k=", var_export(f($k), true), ' '; }
echo "\n"; var_dump(g(), isset($GLOBALS['a']), isset($GLOBALS['n']), f(7), @f(null));
class T { function me() { return $this; } function __destruct() { echo "T gone\n"; } }
function fl() { $t = new T; $t->me(); $t->me()->me(); echo "before end\n"; }
fl(); (new T)->me(); echo "after temp\n";
"#);
    assert_eq!(
        out,
        r#"a=true n=false z=true arr=true u=false r=true nope=false _SERVER=true _GET=true GLOBALS=false argv=true =false 7=false 
bool(true)
bool(true)
bool(false)
bool(false)
bool(false)
before end
T gone
T gone
after temp
"#
    );
}
