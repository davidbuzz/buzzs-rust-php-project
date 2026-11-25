// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The call and return protocol of ordinary user calls, and what binds
//! variables at function entry (`global`, `static`): each path a fast
//! path takes is compared with what the C implementation prints
//! (`/usr/bin/php8.5 -n`, PHP 8.5.12), destructor order included
//! (ref: Zend/zend_vm_def.h `ZEND_RETURN`, `zend_leave_helper`,
//! `ZEND_BIND_GLOBAL`, `ZEND_BIND_STATIC`).

#![allow(clippy::needless_raw_string_hashes)]

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

/// `global` and `static` bind the variable to the stored reference; the
/// variable's old value is released after the bind, and rebinding a bound
/// variable leaves the stored value alone.
#[test]
fn global_and_static_binds() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
$g = new D('g1');
function rebind() { global $g; $g = new D('g2'); echo "rebound\n"; }
rebind();
echo "after rebind\n";
function counter() { static $n = 0; static $arr = []; $arr[] = ++$n; return $arr; }
counter(); counter();
var_dump(counter());
function gref() { global $x; $x[] = count($x ?? []); return $x; }
gref(); gref();
var_dump($x);
function twice() { global $y, $y; $y = 'set'; }
twice(); var_dump($y);
$h = [1, 2];
function rebind_ref() { global $h; $local = [9]; $h = &$local; $h[] = 10; return $h; }
var_dump(rebind_ref(), $h);
function st_obj() { static $o; $o ??= new D('static'); return $o->n; }
echo st_obj(), st_obj(), "\n";
class K { static function m() { static $c = 0; return ++$c; } }
class L extends K {}
echo K::m(), L::m(), K::m(), "\n";
$f = function () { static $z = 10; return $z++; };
echo $f(), $f(), "\n";
$f2 = $f; echo $f2(), "\n";
unset($g);
echo "end\n";
"#),
        r#"destruct g1
rebound
after rebind
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  [0]=>
  int(0)
  [1]=>
  int(1)
}
string(3) "set"
array(2) {
  [0]=>
  int(9)
  [1]=>
  int(10)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
staticstatic
123
1011
12
destruct g2
end
destruct static
"#
    );
}
