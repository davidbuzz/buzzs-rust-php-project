// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reads and tests of elements, properties and constants that the tape's
//! specialised instructions answer (the `Is`-mode miss of `$a['k'] ?? d`,
//! `isset` / `empty` of a temporary's element, `array_key_exists()`,
//! class constants, `is_*()`, `instanceof`, `foreach` over an array,
//! casts) next to the cases that must still take the generic path:
//! warnings and their order, string offsets, `ArrayAccess`, magic
//! methods, references, float/bool/null keys. Each loop runs its body more
//! than once so that a later iteration runs on the tape. The expected
//! output is `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_FETCH_DIM_IS`, `ZEND_ISSET_ISEMPTY_DIM_OBJ`, `ZEND_FETCH_LIST_R`,
//! `ZEND_ARRAY_KEY_EXISTS`, `ZEND_FETCH_CLASS_CONSTANT`, `ZEND_TYPE_CHECK`,
//! `ZEND_INSTANCEOF`, `ZEND_FE_RESET_R`, `ZEND_CAST`).

#![allow(clippy::needless_raw_string_hashes)]

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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}


/// `$cv[k] ?? d` and `$cv[k][j] ?? d`: hits, misses, undefined and scalar containers, string offsets, odd keys, ArrayAccess.
#[test]
fn coalesce_dim_miss() {
    assert_eq!(
        run(r#"<?php
class AA implements ArrayAccess {
  function offsetExists($o): bool { echo "exists($o) "; return $o === 'y'; }
  function offsetGet($o): mixed { echo "get($o) "; return [$o]; }
  function offsetSet($o, $v): void {}
  function offsetUnset($o): void {}
}
function f($k) {
  $a = ['a' => null, 'b' => [1 => 2, 'c' => ['d' => 'e']], '5' => 7, 'r' => 1]; $r = &$a['r'];
  $i = 5; $b = true; $fl = 1.5; $n = null; $s = "abc"; $o = new AA;
  for ($t = 0; $t < 3; $t++) {
    echo $a['a'] ?? 'd', '|', $a['nope'] ?? 'd', '|', $a[$k] ?? 'd', '|', $a['5'] ?? 'd', '|', $a[5] ?? 'd', '|', $a['r'] ?? 'd', "\n";
    echo $a['b'][1] ?? 'd', '|', $a['b'][2] ?? 'd', '|', $a['b']['c']['d'] ?? 'd', '|', $a['b']['c']['x'] ?? 'd', '|', $a['x']['y']['z'] ?? 'd', '|', $a['a']['x'] ?? 'd', "\n";
    echo $i['a'] ?? 'd', '|', $b[0] ?? 'd', '|', $fl['x']['y'] ?? 'd', '|', $n[1] ?? 'd', '|', $u['a'] ?? 'd', '|', $u['a']['b'] ?? 'd', "\n";
    echo $s[1] ?? 'd', '|', $s['x'] ?? 'd', '|', $s[9] ?? 'd', '|', $s[1][0] ?? 'd', '|', $s[1]['k'] ?? 'd', "\n";
    echo $a[1.5] ?? 'd', '|', $a[true] ?? 'd', '|', $a[null] ?? 'd', '|', $a[$undef] ?? 'd', "\n";
    echo json_encode($o['x'] ?? 'd'), '|', json_encode($o['y'] ?? 'd'), '|', json_encode($o['y'][0] ?? 'd'), "\n";
    $m = $a['nope'] ?? $t; $q = $a['b']['zz'] ?? [$t]; var_dump($m, $q);
  }
}
f('zz');
"#),
        r#"d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(0)
array(1) {
  [0]=>
  int(0)
}
d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(1)
array(1) {
  [0]=>
  int(1)
}
d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(2)
array(1) {
  [0]=>
  int(2)
}
"#
    );
}
