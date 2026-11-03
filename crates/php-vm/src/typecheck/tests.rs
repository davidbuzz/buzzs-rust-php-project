//! Script-level tests of type-declaration coercion (expected output from
//! `/usr/bin/php -n`, paths normalised to `t.php`).

#![allow(clippy::needless_raw_string_hashes)]

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn weak_mode_coercions() {
    let src = r#"<?php
function a(int|float $x) { var_dump($x); }
a(true); a("1e2"); a(" 5"); a(false);
function b(int|string $x) { var_dump($x); }
b(1.0); b(INF);
function c(bool $x) { var_dump($x); }
c("0"); c(0.0); c("abc");
function d(string|bool $x) { var_dump($x); }
d(1.5); d(7);
function e(float $x) { var_dump($x); }
e(3); e("4");
function f(int $x) { var_dump($x); }
f(1.5); f("2.5");
try { f("abc"); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
try { f([]); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
class S { function __toString(): string { return "str"; } }
function g(string $x) { var_dump($x); }
g(new S);
function h(?int $x) { var_dump($x); }
h(null);
"#;
    assert_eq!(
        run(src),
        r#"int(1)
float(100)
int(5)
int(0)
int(1)
string(3) "INF"
bool(false)
bool(false)
bool(true)
string(3) "1.5"
string(1) "7"
float(3)
float(4)

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 12
int(1)

Deprecated: Implicit conversion from float-string "2.5" to int loses precision in t.php on line 12
int(2)
f(): Argument #1 ($x) must be of type int, string given, called in t.php on line 14
f(): Argument #1 ($x) must be of type int, array given, called in t.php on line 15
string(3) "str"
NULL
"#
    );
}

#[test]
fn strict_mode_coercions() {
    let src = r#"<?php
declare(strict_types=1);
function e(float $x) { var_dump($x); }
e(3);
function f(int $x) { var_dump($x); }
try { f(1.0); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
try { f("1"); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        r#"float(3)
f(): Argument #1 ($x) must be of type int, float given, called in t.php on line 6
f(): Argument #1 ($x) must be of type int, string given, called in t.php on line 7
"#
    );
}
