// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls of user functions whose parameters are typed `callable` or with
//! a `class_alias()` name, repeated so that the call sites take the plain
//! call path after the first, full check: a closure for `callable`, an
//! object of a class reached through an alias (remembered per type), and
//! the values both keep refusing with a `TypeError`. Expected output:
//! PHP 8.5.

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
fn callable_and_aliased_class_parameters_check_as_the_full_check_does() {
    let out = run(r#"<?php
interface I {} class A implements I {} class B extends A {} class Z {} class_alias('A', 'AliasA'); class_alias('I', 'AliasI');
function fa(AliasA $x) { return get_class($x); } function fi(AliasI $x) { return get_class($x); } function fc(callable $c) { return $c(); }
function fu(AliasA|int $x) { return gettype($x); }
for ($i = 0; $i < 4; $i++) {
    echo fa(new A), fa(new B), fi(new B), fu(new A), fu(3), fc(fn() => 'c'), fc('phpversion') !== '' ? 'v' : '', "\n";
    foreach ([new Z, 'A', null, function () {}] as $bad) { try { fa($bad); } catch (TypeError $e) { echo substr($e->getMessage(), 0, 60), "\n"; } }
    foreach ([new Z, 5, 'nope'] as $bad) { try { fc($bad); } catch (TypeError $e) { echo substr($e->getMessage(), 0, 60), "\n"; } }
}
class Inv { function __invoke() { return 'inv'; } } echo fc(new Inv), fc([new Inv, '__invoke']), "\n";
"#);
    assert_eq!(
        out,
        r#"ABBobjectintegercv
fa(): Argument #1 ($x) must be of type AliasA, Z given, call
fa(): Argument #1 ($x) must be of type AliasA, string given,
fa(): Argument #1 ($x) must be of type AliasA, null given, c
fa(): Argument #1 ($x) must be of type AliasA, Closure given
fc(): Argument #1 ($c) must be of type callable, Z given, ca
fc(): Argument #1 ($c) must be of type callable, int given, 
fc(): Argument #1 ($c) must be of type callable, string give
ABBobjectintegercv
fa(): Argument #1 ($x) must be of type AliasA, Z given, call
fa(): Argument #1 ($x) must be of type AliasA, string given,
fa(): Argument #1 ($x) must be of type AliasA, null given, c
fa(): Argument #1 ($x) must be of type AliasA, Closure given
fc(): Argument #1 ($c) must be of type callable, Z given, ca
fc(): Argument #1 ($c) must be of type callable, int given, 
fc(): Argument #1 ($c) must be of type callable, string give
ABBobjectintegercv
fa(): Argument #1 ($x) must be of type AliasA, Z given, call
fa(): Argument #1 ($x) must be of type AliasA, string given,
fa(): Argument #1 ($x) must be of type AliasA, null given, c
fa(): Argument #1 ($x) must be of type AliasA, Closure given
fc(): Argument #1 ($c) must be of type callable, Z given, ca
fc(): Argument #1 ($c) must be of type callable, int given, 
fc(): Argument #1 ($c) must be of type callable, string give
ABBobjectintegercv
fa(): Argument #1 ($x) must be of type AliasA, Z given, call
fa(): Argument #1 ($x) must be of type AliasA, string given,
fa(): Argument #1 ($x) must be of type AliasA, null given, c
fa(): Argument #1 ($x) must be of type AliasA, Closure given
fc(): Argument #1 ($c) must be of type callable, Z given, ca
fc(): Argument #1 ($c) must be of type callable, int given, 
fc(): Argument #1 ($c) must be of type callable, string give
invinv
"#
    );
}
