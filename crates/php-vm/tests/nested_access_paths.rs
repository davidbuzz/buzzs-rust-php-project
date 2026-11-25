// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Nested element and property accesses that take the frame-local fast
//! paths (`unset($a[$k][$j])`, `unset($this->p[$k])`, ...) next to the
//! cases that must still take the generic place walk: auto-vivification,
//! warnings and their order, string offsets, scalars used as arrays,
//! references, copy-on-write separation, `ArrayAccess`, magic methods,
//! readonly and typed properties. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_UNSET_DIM`, `ZEND_FETCH_DIM_UNSET`, `ZEND_FETCH_OBJ_UNSET`).

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

/// unset() of elements of a function variable, a reference and a global.
#[test]
fn unset_cv() {
    assert_eq!(
        run(r#"<?php
class Dtor { public function __destruct() { echo "dtor\n"; } }
function f() {
  $a = ['x' => ['y' => 1, 'z' => 2], 'n' => null, 's' => 'str', 'f' => false, 5 => [1], 'o' => new Dtor];
  $b = $a;
  unset($a['x']['y']); var_dump($a['x'], $b['x']);
  unset($a['missing']['y']); unset($a['n']['y']); unset($a['f']['y']);
  try { unset($a['s']['y']); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  try { unset($a['s'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  unset($b); echo "before\n"; unset($a['o']); echo "after\n";
  unset($a[5][0]); unset($a['5']); var_dump($a);
  unset($u['k']); unset($u['k']['j']);
  $i = 3; unset($a[$i]); $k = 'x'; unset($a[$k]['z']); var_dump($a);
  $r = &$a; unset($r['n']); var_dump(array_keys($a));
  $s = 'abc'; try { unset($s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  $n = 5; try { unset($n[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  $l = [1, 2, 3]; foreach ($l as $kk => &$vv) { if ($kk == 0) unset($l[1]); echo $vv, "\n"; } unset($vv);
  $l = [1, 2, 3]; foreach ($l as $kk => $vv) { if ($kk == 0) unset($l[1]); echo $vv, "\n"; } var_dump($l);
  $d = ['a' => ['b' => ['c' => ['d' => 1, 'e' => 2]]]]; unset($d['a']['b']['c']['d']); var_dump($d);
  $q = ['k' => 1]; unset($q[$undef]); var_dump($q);
  $m = [1 => 'a', '01' => 'b']; unset($m['1']); var_dump($m);
}
f();
$g = ['a' => ['b' => 1, 'c' => 2]];
function gl() { global $g; $x = 'b'; unset($g['a'][$x]); }
gl(); var_dump($g);
unset($g['a']['c']); var_dump($g, $GLOBALS['g']);
"#),
        r#"array(1) {
  ["z"]=>
  int(2)
}
array(2) {
  ["y"]=>
  int(1)
  ["z"]=>
  int(2)
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 7
Cannot unset string offsets
Cannot unset string offsets
before
dtor
after
array(4) {
  ["x"]=>
  array(1) {
    ["z"]=>
    int(2)
  }
  ["n"]=>
  NULL
  ["s"]=>
  string(3) "str"
  ["f"]=>
  bool(false)
}

Warning: Undefined variable $u in t.php on line 12

Warning: Undefined variable $u in t.php on line 12
array(4) {
  ["x"]=>
  array(0) {
  }
  ["n"]=>
  NULL
  ["s"]=>
  string(3) "str"
  ["f"]=>
  bool(false)
}
array(3) {
  [0]=>
  string(1) "x"
  [1]=>
  string(1) "s"
  [2]=>
  string(1) "f"
}
Cannot unset string offsets
Cannot unset offset in a non-array variable
1
3
1
2
3
array(2) {
  [0]=>
  int(1)
  [2]=>
  int(3)
}
array(1) {
  ["a"]=>
  array(1) {
    ["b"]=>
    array(1) {
      ["c"]=>
      array(1) {
        ["e"]=>
        int(2)
      }
    }
  }
}

Warning: Undefined variable $undef in t.php on line 20
array(1) {
  ["k"]=>
  int(1)
}
array(1) {
  ["01"]=>
  string(1) "b"
}
array(1) {
  ["a"]=>
  array(1) {
    ["c"]=>
    int(2)
  }
}
array(1) {
  ["a"]=>
  array(0) {
  }
}
array(1) {
  ["a"]=>
  array(0) {
  }
}
"#
    );
}

/// unset() of elements of array properties.
#[test]
fn unset_prop() {
    assert_eq!(
        run(r#"<?php
class O { public $c = ['g' => ['k' => 1, 'j' => 2]]; public readonly array $ro; public ?array $t = ['x' => 1]; private $p = ['a' => 1]; public $s = 'str';
  public int $i = 1; public $u;
  function __construct() { $this->ro = ['a' => [1]]; unset($this->u); }
  function m() { $g = 'g'; unset($this->c[$g]['k']); unset($this->c['g']['zz']); unset($this->c['nope']['k']); var_dump($this->c);
    try { unset($this->ro['a'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
    unset($this->t['x']); unset($this->p['a']); var_dump($this->t, $this->p);
    try { unset($this->s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { unset($this->i[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }

    $copy = $this->c; unset($this->c[$g]); var_dump($this->c, $copy);
    $r = &$this->c; $r['z'] = [1, 2]; unset($this->c['z'][0]); var_dump($r);
  }
  function __get($n) { echo "__get $n\n"; return null; }
}
(new O)->m();
$o = new O; unset($o->c['g']['j']); var_dump($o->c);
class P { public $list = []; function add($k) { $this->list[$k][] = 1; } function del($k) { unset($this->list[$k][0]); if (!$this->list[$k]) unset($this->list[$k]); } }
$p = new P; for ($i = 0; $i < 3; $i++) { $p->add('a'); $p->add('b'); $p->del('a'); } var_dump($p->list);
$x = new stdClass; $x->d = ['a' => 1]; unset($x->d['a']); var_dump($x);
class AA implements ArrayAccess { public function offsetExists($o): bool { return true; } public function offsetGet($o): mixed { return null; }
  public function offsetSet($o, $v): void {} public function offsetUnset($o): void { echo "offsetUnset $o\n"; } }
class Q { public $aa; function __construct() { $this->aa = ['x' => new AA]; } function m() { unset($this->aa['x']['y']); } }
(new Q)->m();
"#),
        r#"array(1) {
  ["g"]=>
  array(1) {
    ["j"]=>
    int(2)
  }
}
Cannot indirectly modify readonly property O::$ro
array(0) {
}
array(0) {
}
Cannot unset string offsets
Cannot unset offset in a non-array variable
array(0) {
}
array(1) {
  ["g"]=>
  array(1) {
    ["j"]=>
    int(2)
  }
}
array(1) {
  ["z"]=>
  array(1) {
    [1]=>
    int(2)
  }
}
array(1) {
  ["g"]=>
  array(1) {
    ["k"]=>
    int(1)
  }
}
array(1) {
  ["b"]=>
  array(3) {
    [0]=>
    int(1)
    [1]=>
    int(1)
    [2]=>
    int(1)
  }
}
object(stdClass)#3 (1) {
  ["d"]=>
  array(0) {
  }
}
offsetUnset y
"#
    );
}

/// unset() of an offset of a property holding no array.
#[test]
fn unset_prop_scalars() {
    assert_eq!(
        run(r#"<?php
class O { public $s = 'str'; public int $i = 1; public $u; public $n = 5; function __construct() { unset($this->u); } function __get($n) { echo "__get $n\n"; return null; } }
$o = new O;
try { unset($o->s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($o->n[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($o->s[0][1]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$o->a = ['s' => 'x']; try { unset($o->a['s'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo "end\n";
class O2 { public $f = false; public $n = null; public $fl = 1.5; }
$o2 = new O2;
unset($o2->f[0]); var_dump($o2->f);
unset($o2->n[0]); var_dump($o2->n);
try { unset($o2->fl[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$s = new stdClass; $s->x = 'q'; try { unset($s->x[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        r#"Cannot unset string offsets
Cannot unset offset in a non-array variable
Cannot use string offset as an array

Deprecated: Creation of dynamic property O::$a is deprecated in t.php on line 7
Cannot unset string offsets
end

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 11
bool(false)
NULL
Cannot unset offset in a non-array variable
Cannot unset string offsets
"#
    );
}
