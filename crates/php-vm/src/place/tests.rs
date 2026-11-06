//! Script-level tests of assignments whose value is a reference to a
//! container of the place (`ZEND_ASSIGN_DIM` reads `OP_DATA` after the
//! container fetch). Expected output from `/usr/bin/php -n`.

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

/// `$t = &$a; $a[] = $t;` and its variants build arrays that contain
/// themselves; `$a[] = $a` still copies (`zend_is_assign_to_self`).
#[test]
fn reference_to_container_is_read_after_the_fetch() {
    let src = r#"<?php
$arr = [1, 'x'];
$tmp = &$arr;
$arr[] = $tmp;
var_dump($arr);
echo serialize($arr), "\n";
$t =& $u;
$u[] = $t;
var_dump($u);
$x = null; $r =& $x; $x['k'][] = $r; var_dump($x);
$y = []; $ry =& $y; $z = ($y['a'] = $ry); var_dump($z);
$s = [1]; $rs =& $s; $s[] = $s; var_dump($s);
$p = [1]; $q = &$p; $o = []; $o['k'] = $q; var_dump($o);
$b = [1]; $rb =& $b; $b[] = $rb; $b[] = 2; var_dump(cnt($b), cnt($b[1]));
$a = [0]; $a[0] = &$w; $ra =& $a; $a[0] = $ra; var_dump($w);
function g(&$a) { $c = &$a; $a['me'] = $c; }
$h = ['v' => 1]; g($h); var_dump($h);
$f = false; $rf =& $f; $f[] = $rf; var_dump($f);
$m = ['s' => 'abc']; $rm =& $m; $m['s'][1] = $rm; var_dump($m);
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
"#;
    let expected = r#"array(3) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  *RECURSION*
}
a:3:{i:0;i:1;i:1;s:1:"x";i:2;N;}
array(1) {
  [0]=>
  *RECURSION*
}
array(1) {
  ["k"]=>
  array(1) {
    [0]=>
    *RECURSION*
  }
}
array(1) {
  ["a"]=>
  *RECURSION*
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  array(1) {
    [0]=>
    int(1)
  }
}
array(1) {
  ["k"]=>
  array(1) {
    [0]=>
    int(1)
  }
}
int(3)
int(2)
array(1) {
  [0]=>
  *RECURSION*
}
array(2) {
  ["v"]=>
  int(1)
  ["me"]=>
  *RECURSION*
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 18
array(1) {
  [0]=>
  *RECURSION*
}

Warning: Array to string conversion in t.php on line 19

Warning: Only the first byte will be assigned to the string offset in t.php on line 19
array(1) {
  ["s"]=>
  string(3) "aAc"
}
"#;
    assert_eq!(run(src), expected);
}
