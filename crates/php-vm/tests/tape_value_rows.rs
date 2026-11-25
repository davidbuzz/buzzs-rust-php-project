// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The tape's rows for values that are not numbers: the assignment of a
//! string or array literal to a variable, comparisons with a string, null,
//! a bool or the empty array, and the truth tests of such values, next to
//! the cases that must still take the generic path (references, objects
//! whose release runs a destructor, numeric strings, conversions that
//! warn). Each loop runs its body more than once so that a later iteration
//! runs on the tape. The expected output is `/usr/bin/php8.5 -n` on each
//! script (ref: Zend/zend_vm_def.h `ZEND_ASSIGN`, `ZEND_IS_IDENTICAL`,
//! `ZEND_IS_EQUAL`, `ZEND_JMPZ`, `ZEND_BOOL_NOT`).

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

/// `$cv = 'lit'`, `$cv = []`, `$cv = [1, 2]`, `$cv = array('a' => ...)`
/// over every kind of old value: a scalar, a string, an array (unique and
/// holding objects, whose destructors run before the next statement; shared,
/// whose objects live on), an object, a reference (written through), a
/// variable in a cycle. A literal array assigned and then written is
/// separated from the literal.
#[test]
fn literal_assignments() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~{$this->n} "; } }
function f() {
  for ($i = 0; $i < 3; $i++) {
    $s = 'lit';
    $e = [];
    $p = [1, 2];
    $m = array('a' => 1, 'b' => [2]);
    $s = 'other';
    $e[] = $i;
    $p[0] = $i;
    echo $s, count($e), json_encode($p), json_encode($m), ' ';
    $old = [new D($i), new D($i + 10)];
    echo 'a ';
    $old = 'x';
    echo 'b ';
    $sh = [new D($i + 20)]; $keep = $sh;
    $sh = [];
    echo 'c ';
    unset($keep);
    echo 'd ';
    $o = new D($i + 30);
    $o = 'y';
    echo 'e ';
    $r = 5; $ref = &$r; $ref = 'via'; echo $r, ' ';
    $ref = [3]; echo json_encode($r), ' ';
    unset($ref);
    $u = 1.5; $u = 'z'; $t = true; $t = []; $n = null; $n = 'q';
    echo $u, json_encode($t), $n, "\n";
  }
  $cyc = []; $cyc[] = &$cyc; $cyc = 'gone';
  echo $cyc, "\n";
}
f();
"#),
        "other1[0,2]{\"a\":1,\"b\":[2]} a ~0 ~10 b c ~20 d ~30 e via [3] z[]q\nother1[1,2]{\"a\":1,\"b\":[2]} a ~1 ~11 b c ~21 d ~31 e via [3] z[]q\nother1[2,2]{\"a\":1,\"b\":[2]} a ~2 ~12 b c ~22 d ~32 e via [3] z[]q\ngone\n"
    );
}

/// `===`, `!==`, `==`, `!=` of a variable, a consumed temporary (an
/// element, a property) and a literal null, bool, string, empty array, as
/// a branch, into a temporary and into a variable, for values of every
/// type; the cases the rows leave to the generic comparison come out as in
/// php-src: a numeric string literal (`'1e3' == 1000`), `INF == 'INF'`, a
/// NaN, an object with `__toString`, an undefined variable (its warning),
/// a reference, a switch on a string and on an int.
#[test]
fn equality_with_literals() {
    assert_eq!(
        run(r#"<?php
class S { function __toString(): string { return "abc"; } }
class P { public $p; function __construct($p) { $this->p = $p; } function t() { return $this->p === 'abc' ? 'P' : 'p'; } }
function f() {
  $vals = [null, true, false, 0, 1, 0.0, 1.5, NAN, INF, '', '0', 'a', 'abc', '1e3', '1000', [], [0], new S, 'x' => 'INF'];
  for ($round = 0; $round < 2; $round++) {
    foreach ($vals as $v) {
      $o = '';
      if ($v === null) $o .= 'A'; else $o .= 'a';
      if ($v !== null) $o .= 'B'; else $o .= 'b';
      if ($v == null) $o .= 'C'; else $o .= 'c';
      if ($v != null) $o .= 'D'; else $o .= 'd';
      if ($v === true) $o .= 'E'; else $o .= 'e';
      if ($v == true) $o .= 'F'; else $o .= 'f';
      if ($v === false) $o .= 'G'; else $o .= 'g';
      if ($v == false) $o .= 'H'; else $o .= 'h';
      if ($v === 'abc') $o .= 'I'; else $o .= 'i';
      if ($v == 'abc') $o .= 'J'; else $o .= 'j';
      if ($v !== '') $o .= 'K'; else $o .= 'k';
      if ($v != '') $o .= 'L'; else $o .= 'l';
      if ($v === []) $o .= 'M'; else $o .= 'm';
      if ($v == []) $o .= 'N'; else $o .= 'n';
      if ($v == '1e3') $o .= 'O'; else $o .= 'o';
      if ('INF' == $v) $o .= 'Q'; else $o .= 'q';
      $r1 = $v === 'abc'; $r2 = $v == null; $r3 = $v !== false; $r4 = $v != [];
      $t = [$v === 'a', $v == 'a', null === $v, $v != true];
      $o .= ' ' . (int)$r1 . (int)$r2 . (int)$r3 . (int)$r4 . ' ' . implode('', array_map('intval', $t));
      $w = ['k' => $v];
      $o .= ($w['k'] === 'abc' ? 'X' : 'x') . ($w['k'] == '' ? 'Y' : 'y') . ((new P($v))->t());
      echo $o, "\n";
    }
  }
  $s = 'ab'; $s .= 'c';
  echo $s === 'abc' ? 'same' : 'diff', ' ', $s == 'abc' ? 'eq' : 'ne', "\n";
  echo @$undef === null ? 'u' : 'U', "\n";
  echo $undef2 == 'x' ? 'v' : 'V', "\n";
  $ref = 'abc'; $rr = &$ref;
  echo $rr === 'abc' ? 'r' : 'R', $rr == null ? 'n' : 'N', "\n";
  $str = 'xyz';
  switch ($str) { case 'abc': echo "1\n"; break; case 'xyz': echo "2\n"; break; default: echo "3\n"; }
  switch (0) { case 'abc': echo "s1\n"; break; case '0': echo "s2\n"; break; }
}
f();
"#),
        "AbCdefgHijKlmNoq 0110 0011xYp\naBcDEFghiJKLmnOQ 0011 0100xyp\naBCdefGHijKlmNoq 0100 0001xYp\naBCdefgHijKLmnoq 0111 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBCdefgHijKLmnoq 0111 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghijKLmnoQ 0011 0000xyp\naBCdefgHijklmnoq 0111 0001xYp\naBcDefgHijKLmnoq 0011 0001xyp\naBcDeFghijKLmnoq 0011 1100xyp\naBcDeFghIJKLmnoq 1011 0000XyP\naBcDeFghijKLmnOq 0011 0000xyp\naBcDeFghijKLmnOq 0011 0000xyp\naBCdefgHijKLMNoq 0110 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghiJKLmnoq 0011 0000xyp\naBcDeFghijKLmnoQ 0011 0000xyp\nAbCdefgHijKlmNoq 0110 0011xYp\naBcDEFghiJKLmnOQ 0011 0100xyp\naBCdefGHijKlmNoq 0100 0001xYp\naBCdefgHijKLmnoq 0111 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBCdefgHijKLmnoq 0111 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghijKLmnoQ 0011 0000xyp\naBCdefgHijklmnoq 0111 0001xYp\naBcDefgHijKLmnoq 0011 0001xyp\naBcDeFghijKLmnoq 0011 1100xyp\naBcDeFghIJKLmnoq 1011 0000XyP\naBcDeFghijKLmnOq 0011 0000xyp\naBcDeFghijKLmnOq 0011 0000xyp\naBCdefgHijKLMNoq 0110 0001xyp\naBcDeFghijKLmnoq 0011 0000xyp\naBcDeFghiJKLmnoq 0011 0000xyp\naBcDeFghijKLmnoQ 0011 0000xyp\nsame eq\nu\n\nWarning: Undefined variable $undef2 in t.php on line 36\nV\nrN\n2\ns2\n"
    );
}

/// The truth of a temporary holding a string, an array or an object (an
/// element, a call's result) in a jump, `!`, `(bool)` and `?:`, and an
/// unused element read (`ZEND_FREE`): a shared or empty array and an
/// object another handle keeps are answered on the tape; an array or
/// object the temporary alone holds is released as in php-src, its
/// destructor running before the branch it decides.
#[test]
fn truth_of_temporaries() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~{$this->n} "; } }
function id($x) { return $x; }
function mk($n) { return new D($n); }
function arr($n) { return $n ? [$n] : []; }
function f() {
  $o = new D('kept');
  $vals = ['k' => [], 'a' => [1], 's' => 'x', 'e' => '', 'z' => '0', 'o' => $o, 'n' => null, 'f' => 0.0];
  for ($round = 0; $round < 3; $round++) {
    $out = '';
    foreach (['k', 'a', 's', 'e', 'z', 'o', 'n', 'f'] as $key) {
      $out .= $vals[$key] ? 'T' : 'f';
      if ($vals[$key]) { $out .= '1'; } else { $out .= '0'; }
      if (!$vals[$key]) { $out .= '!'; }
      $neg = !$vals[$key];
      $out .= $neg ? 'n' : 'p';
      $out .= (bool) id($vals[$key]) ? 'B' : 'b';
      $r = id($vals[$key]) ?: 'dflt';
      $out .= is_string($r) ? $r : gettype($r);
      $vals[$key];
      id($vals[$key]);
      $out .= ' ';
    }
    echo $out, "\n";
    if (arr($round)) { echo 'arr '; }
    if (!arr($round)) { echo 'empty '; }
    if (mk("m$round")) { echo 'obj '; }
    echo "| ";
    $m = mk("j$round") ?: 0;
    echo "kept ";
    $m = null;
    echo "| ";
    $x = arr(0) ?: 'none';
    echo $x, " ";
    $y = !mk("u$round");
    echo var_export($y, true), "\n";
  }
}
f();
echo "end\n";
"#),
        "f0!nbdflt T1pBarray T1pBx f0!nbdflt f0!nbdflt T1pBobject f0!nbdflt f0!nbdflt \nempty ~m0 obj | kept ~j0 | none ~u0 false\nf0!nbdflt T1pBarray T1pBx f0!nbdflt f0!nbdflt T1pBobject f0!nbdflt f0!nbdflt \narr ~m1 obj | kept ~j1 | none ~u1 false\nf0!nbdflt T1pBarray T1pBx f0!nbdflt f0!nbdflt T1pBobject f0!nbdflt f0!nbdflt \narr ~m2 obj | kept ~j2 | none ~u2 false\n~kept end\n"
    );
}
