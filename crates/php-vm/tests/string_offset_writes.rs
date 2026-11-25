// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$s[$i] = 'x'` (`zend_assign_to_string_offset`, Zend/zend_execute.c): a
//! string the variable alone holds is written in place, any other holder
//! of the same string (another variable, an array key, an argument's
//! source, a literal, a cloned object's property, a copied array's
//! element) keeps its bytes. Expected output from `/usr/bin/php8.5 -n`.
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
fn offset_writes_change_only_the_variable_written() {
    assert_eq!(
        run(r###"<?php
function unshared() { $s = str_repeat('a', 5); $s[0] = 'X'; $s[4] = 'Y'; $s[-2] = 'Z'; return $s; }
function shared() { $s = str_repeat('a', 4); $t = $s; $s[1] = 'X'; return $s . ',' . $t; }
function literal() { $o = ''; for ($i = 0; $i < 3; $i++) { $s = 'abc'; $s[$i] = 'X'; $o .= $s . ','; } return $o; }
function padded() { $s = 'ab'; $s[5] = 'X'; return '[' . $s . ']' . strlen($s); }
function key_then_write() { $s = str_repeat('k', 3); $a = [$s => 1]; $s[0] = 'j'; return $s . ',' . implode('', array_keys($a)) . ',' . (isset($a[$s]) ? 'set' : 'unset'); }
function argument($s) { $s[0] = 'Q'; return $s; }
function by_ref(&$s) { $s[0] = 'R'; }
function loop() { $s = str_repeat('.', 8); for ($i = 0; $i < 8; $i += 2) { $s[$i] = (string) $i; } return $s; }
function result() { $s = str_repeat('a', 3); $r = ($s[1] = 'xyz'); return $s . ',' . $r; }
function prop() { $o = new stdClass; $o->s = str_repeat('p', 3); $o->s[1] = 'Q'; $c = clone $o; $c->s[0] = 'C'; return $o->s . ',' . $c->s; }
function elem() { $a = ['k' => str_repeat('e', 3)]; $b = $a; $a['k'][2] = 'E'; return $a['k'] . ',' . $b['k']; }
for ($n = 0; $n < 2; $n++) {
    $x = str_repeat('m', 3); $y = $x; by_ref($x);
    echo unshared(), ' ', shared(), ' ', literal(), ' ', padded(), ' ', key_then_write(), ' ', argument($y), $y, ' ', $x, ' ', loop(), ' ', @result(), ' ', prop(), ' ', elem(), "\n";
}
"###),
        r###"XaaZY aXaa,aaaa Xbc,aXc,abX, [ab   X]6 jkk,kkk,unset Qmmmmm Rmm 0.2.4.6. axa,x pQp,CQp eeE,eee
XaaZY aXaa,aaaa Xbc,aXc,abX, [ab   X]6 jkk,kkk,unset Qmmmmm Rmm 0.2.4.6. axa,x pQp,CQp eeE,eee
"###
    );
}
