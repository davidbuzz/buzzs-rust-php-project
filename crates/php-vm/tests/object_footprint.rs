// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Object footprint (`zend_object`, `Zend/zend_objects.c`): the
//! per-object state stays small, and the "explicitly unset" slot flags
//! (`IS_PROP_UNINIT` in php-src's property zval `u2`) kept in a bit set
//! behave like php-src's for classes with more than 63 slots too.
use php_vm::{Vm, VmOptions};
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
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn object_data_stays_small() {
    // handle, class, slots, boxed dynamic table, internal state, flags,
    // unset bits, lazy state.
    assert!(std::mem::size_of::<php_vm::object::ObjectData>() <= 112);
}

#[test]
fn unset_flags_beyond_63_slots() {
    // Expected output from `/usr/bin/php8.5 -n`.
    let out = run(r#"<?php
$decl = '';
for ($i = 0; $i < 70; $i++) { $decl .= "public int \$p$i; "; }
eval("class Big { $decl public function __get(\$n) { return strlen(\$n); } }");
$o = new Big;
try { echo $o->p1, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
unset($o->p1, $o->p66);
echo $o->p1, " ", $o->p66, "\n";
try { echo $o->p67, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$c = clone $o;
echo $c->p1, " ", $c->p66, "\n";
$o->p66 = 5;
echo $o->p66, " ", $c->p66, "\n";
unset($o->p66);
echo $o->p66, "\n";
$o = null;
$n = new Big;
try { echo $n->p66, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(out, "Typed property Big::$p1 must not be accessed before initialization\n2 3\nTyped property Big::$p67 must not be accessed before initialization\n2 3\n5 3\n3\nTyped property Big::$p66 must not be accessed before initialization\n");
}

#[test]
fn dynamic_properties_round_trip() {
    let out = run(r#"<?php
$o = new stdClass;
$o->a = 1;
$o->{'2'} = 'two';
$c = clone $o;
$c->b = [3];
var_dump((array) $o, $c == $o, cnt(get_object_vars($c)));
unset($c->a);
foreach ($c as $k => $v) { echo $k, "\n"; }
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
"#);
    assert_eq!(
        out,
        "array(2) {\n  [\"a\"]=>\n  int(1)\n  [2]=>\n  string(3) \"two\"\n}\nbool(false)\nint(3)\n2\nb\n"
    );
}
