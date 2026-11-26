// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reads and writes of a declared property whose slot holds a reference
//! (`ZEND_ASSIGN_OBJ` / `ZEND_FETCH_OBJ_R` on `Z_ISREF` slots,
//! `Zend/zend_vm_def.h`), on the property cache's hit paths (the loops run
//! each site several times). A reference nothing else holds (left by
//! `&$this->p`, a by-reference argument such as `array_pop($this->p)`, or
//! `foreach` by reference) is written as the plain value it behaves as;
//! one something else holds is written through, a typed one keeps its
//! type, and the value behind a replaced reference is released at once.
//! `PHP-Parser`'s `$self->semValue` was such a slot: every write of it left
//! the fast paths. Expected output: PHP 8.5.

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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn property_slots_holding_references_read_and_write_as_in_php() {
    let out = run(r#"<?php
class P {
    public $v = [1, 2, 3];
    public int $t = 1;
    public ?P $o = null;
    function unique() { $r = &$this->v; unset($r); foreach ($this->v as &$x) { } unset($x); }
    function writes(int $n) { for ($i = 0; $i < $n; $i++) { $this->v = [$i]; } return $this->v; }
    function reads(int $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $this->v[0]; $x = $this->v; } return $s; }
    function typed(int $n) { for ($i = 0; $i < $n; $i++) { $this->t = $i; } return $this->t; }
    function elem(int $n) { for ($i = 0; $i < $n; $i++) { $this->v[1] = $i; $y = $this->v[1]; } return $y; }
}
$p = new P;
// warm the caches on a plain slot, then leave a reference nobody else holds
var_dump($p->writes(3), $p->reads(3), $p->elem(3));
$p->unique();
var_dump($p->writes(3), $p->reads(3), $p->elem(3));
// a reference something else holds: the write goes through it
$r = &$p->v;
var_dump($p->writes(3), $r, $p->reads(2), $p->elem(3), $r);
$r = 'seen';
var_dump($p->v);
unset($r);
var_dump($p->writes(2), $p->v);
// a typed property: the reference keeps its type
$p->typed(3);
$t = &$p->t;
var_dump($p->typed(3), $t);
try { $t = 'x'; } catch (TypeError $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
unset($t);
var_dump($p->typed(4));
try { $p->t = 'y'; } catch (TypeError $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
// an object behind the reference is released when the slot is overwritten
class D { function __destruct() { echo "destructed\n"; } }
$p->v = new D;
$q = &$p->v; unset($q);
$p->writes(1);
echo "after\n";
// two properties bound to one reference
$a = new P; $b = new P;
$a->v = &$b->v;
$a->writes(2);
var_dump($b->v);
unset($b);
var_dump($a->writes(2));
"#);
    assert_eq!(
        out,
        r#"array(1) {
  [0]=>
  int(2)
}
int(6)
int(2)
array(1) {
  [0]=>
  int(2)
}
int(6)
int(2)
array(1) {
  [0]=>
  int(2)
}
array(1) {
  [0]=>
  int(2)
}
int(4)
int(2)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(2)
}
string(4) "seen"
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
int(2)
int(2)
TypeError: Cannot assign string to reference held by property P::$t of type int
int(3)
TypeError: Cannot assign string to property P::$t of type int
destructed
after
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
"#
    );
}
