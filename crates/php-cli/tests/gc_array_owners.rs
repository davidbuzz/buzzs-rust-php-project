// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! What `gc_collect_cycles()` counts and frees when the garbage holds
//! arrays with one owner (a property's or an element's own array, which
//! the collector's graph takes without indexing it by address), arrays
//! two objects share, references, deeply nested arrays, and garbage next
//! to data that stays alive. Expected output: PHP 8.5.

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
fn cycles_through_owned_and_shared_arrays_are_counted_as_php_counts_them() {
    let out = run(r#"<?php
class N { public $a; public $b; public $c; }
function one() { $x = new N; $x->a = [$x, [1, [$x]]]; }
function shared() { $x = new N; $s = [new N, 'k' => [1, 2]]; $x->a = $s; $y = new N; $y->a = $s; $y->b = [$y, $x]; $x->b = [$y]; }
function refs() { $x = new N; $v = [1]; $x->a = &$v; $x->b = [$x]; $w = 'w'; $x->c = [&$w, &$w]; }
function nested() { $x = new N; $a = []; for ($i = 0; $i < 50; $i++) { $a = [$a, $i]; } $x->a = [$a, $x]; }
function live(N $keep) { $x = new N; $x->a = [$x]; $x->b = $keep->a; $keep->b = [[$x->b]]; }
one(); var_dump(gc_collect_cycles());
shared(); var_dump(gc_collect_cycles());
refs(); var_dump(gc_collect_cycles());
nested(); var_dump(gc_collect_cycles());
$keep = new N; $keep->a = [new N, [3]];
live($keep); var_dump(gc_collect_cycles());
var_dump(count($keep->a), count($keep->b[0][0]));
one(); shared(); refs(); nested(); live($keep);
var_dump(gc_collect_cycles());
$o = new N; $id0 = spl_object_id($o);
var_dump(spl_object_id(new N) > 0, gc_status()['runs'], gc_status()['collected']);
"#);
    assert_eq!(
        out,
        r#"int(4)
int(6)
int(3)
int(52)
int(2)
int(2)
int(2)
int(67)
bool(true)
int(6)
int(134)
"#
    );
}
