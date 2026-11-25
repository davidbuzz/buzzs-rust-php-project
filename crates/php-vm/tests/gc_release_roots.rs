// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! What releasing values records in the cycle collector's root buffer, as
//! `gc_status()` and `gc_collect_cycles()` show it: references released
//! by returning frames (bound by `global`, `static` or `$b = &$a`),
//! references freed while buffered (`GC_REMOVE_FROM_BUFFER`: their slot
//! is reused, so they never move the threshold), objects and arrays in
//! the variables of returning frames, arrays released while held
//! elsewhere (every non-persistent array is collectable: buffered once,
//! out of the buffer when freed) and what they do to the threshold and to
//! when garbage cycles are destructed. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_gc.c,
//! Zend/zend_execute.c `i_free_compiled_variables`).

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

const STATUS: &str = r#"
function st() {
  $s = gc_status();
  return "runs={$s['runs']} collected={$s['collected']} threshold={$s['threshold']} roots={$s['roots']}";
}
"#;

/// A reference made and freed by every call (`$b = &$a` in a function
/// called 24,000 times) is buffered as its first handle goes and freed
/// with its second: its slot is reused by the next one, so the threshold
/// is never reached and never raised (C: no run, threshold 10001).
#[test]
fn references_freed_while_buffered_reuse_their_slot() {
    let src = format!(
        r#"<?php{STATUS}
function locref($i) {{ $a = [$i]; $b = &$a; return 1; }}
function locrefo($i) {{ $o = new stdClass; $b = &$o; return 1; }}
function d($n) {{ for ($i = 0; $i < $n; $i++) {{ locref($i); locrefo($i); }} }}
d(12000);
echo st(), "\n";
d(12000);
echo st(), "\n";
echo gc_collect_cycles(), "\n";
echo st(), "\n";
"#
    );
    assert_eq!(
        run(&src),
        "runs=0 collected=0 threshold=10001 roots=0\n\
         runs=0 collected=0 threshold=10001 roots=0\n\
         0\n\
         runs=0 collected=0 threshold=10001 roots=0\n"
    );
}

/// A cycle through a reference (`$a[] = &$a`) left by each call stays
/// buffered (its reference is alive) and is collected, five arrays.
#[test]
fn reference_cycles_left_by_returning_frames_are_collected() {
    let src = format!(
        r#"<?php{STATUS}
function cycarr($i) {{ $a = [$i]; $a[] = &$a; return 1; }}
for ($i = 0; $i < 5; $i++) {{ cycarr($i); }}
echo gc_collect_cycles(), "\n";
echo st(), "\n";
"#
    );
    assert_eq!(run(&src), "5\nruns=1 collected=5 threshold=10001 roots=0\n");
}

/// Object cycles left by returning frames are collected, their
/// destructors run, and references to arrays and objects bound by
/// `global` and `static` in between change none of it.
#[test]
fn object_cycles_with_global_and_static_binds() {
    let src = format!(
        r#"<?php{STATUS}
class C {{ public $self; public $n;
  function __construct($n) {{ $this->n = $n; }}
  function __destruct() {{ echo "d", $this->n, " "; }} }}
$G = [1, 2];
$GO = new C(0);
function useg() {{ global $G; return count($G); }}
function usego() {{ global $GO; return $GO->n; }}
function cyc($i) {{ $a = new C($i); $a->self = $a; return 1; }}
for ($i = 1; $i <= 3; $i++) {{ cyc($i); useg(); usego(); }}
echo gc_collect_cycles(), "\n";
for ($i = 0; $i < 3000; $i++) {{ useg(); usego(); }}
echo gc_collect_cycles(), "\n";
$s = gc_status(); echo $s['runs'], " ", $s['collected'], " ", $s['threshold'], "\n";
"#
    );
    // php-src destructs in root buffer order: the slots freed and reused
    // by the calls in between put the third object first.
    assert_eq!(run(&src), "d3 d1 d2 3\n0\n3 3 10001\nd0 ");
}

/// A returning frame's scalar, string and reference-to-scalar variables
/// buffer nothing (`i_free_compiled_variables` reaches
/// `gc_check_possible_root` only for a collectable value); an object that
/// stays referenced is buffered once, and an object freed with the frame
/// is not (C: roots 0, 1, 1, 1).
#[test]
fn returning_frames_buffer_only_shared_objects_among_their_variables() {
    let src = r#"<?php
function st() { $s = gc_status(); return "roots={$s['roots']}"; }
function scal($a, $b) { $c = $a + 1; $d = "s$b"; $e = 1.5; $f = true; $g = null; $h = &$e; return $c; }
function held($x, $n) { $p = $x; $q = $n * 2; $s = "v$n"; return 1; }
for ($i = 0; $i < 1000; $i++) { scal($i, $i); }
echo st(), "\n";
$o = new stdClass; held($o, 1); echo st(), "\n";
held($o, 2); echo st(), "\n";
for ($i = 0; $i < 5; $i++) { held(new stdClass, $i); } echo st(), "\n";
"#;
    assert_eq!(run(src), "roots=0\nroots=1\nroots=1\nroots=1\n");
}

/// Arrays released while another holder remains are possible roots, once
/// each; a literal's array stays held by the literal (no opcache: C's
/// literal arrays are refcounted then); strings never are; a separated
/// copy freed leaves nothing (tools/div/g1.php; C: `php8.5 -n`).
#[test]
fn arrays_released_while_held_are_roots() {
    let src = r#"<?php
function held($a) { return count($a); }
function f() {
    gc_collect_cycles();
    $r = [];
    $a = [1, 2, 3]; $b = $a; unset($b);                 $r[] = gc_status()['roots'];
    $c = [1, [2]]; held($c);                            $r[] = gc_status()['roots'];
    $d = ['x' => new stdClass]; $e = $d; $e = null;     $r[] = gc_status()['roots'];
    $s = str_repeat('a', 3); $t = $s; unset($t);        $r[] = gc_status()['roots'];
    $o = new stdClass; $p = $o; unset($p);              $r[] = gc_status()['roots'];
    $n = gc_collect_cycles();                           $r[] = $n; $r[] = gc_status()['roots'];
    $lit = [1, 2]; $m = $lit; unset($m);                $r[] = gc_status()['roots'];
    $big = range(1, 5); $q = $big; $q[] = 6; unset($q); $r[] = gc_status()['roots'];
    return implode(',', $r);
}
echo f(), "\n", f(), "\n";
function many() { gc_collect_cycles(); $keep = []; for ($i = 0; $i < 12000; $i++) { $x = [$i]; $keep[] = $x; } $s = gc_status(); return $s['roots'] . ' runs=' . $s['runs'] . ' threshold=' . $s['threshold']; }
echo many(), "\n";
class D { public $self; public $id; function __construct($id) { $this->id = $id; $this->self = $this; } function __destruct() { echo "d{$this->id} "; } }
function cycles() { gc_collect_cycles(); for ($i = 0; $i < 3; $i++) { new D($i); } $junk = []; for ($i = 0; $i < 10100; $i++) { $x = [$i, [$i]]; $y = $x; unset($y); } echo "| after loop\n"; }
cycles(); echo "\n";
"#;
    assert_eq!(
        run(src),
        "1,2,3,3,4,0,0,1,1\n\
         1,2,3,3,4,0,0,1,1\n\
         1999 runs=5 threshold=20001\n\
         | after loop\n\
         \n\
         d0 d1 d2 "
    );
}

/// A buffered array that is freed leaves the buffer (the count goes
/// back), one released twice is counted once, and one freed with the
/// object holding it leaves too.
#[test]
fn freed_arrays_leave_the_buffer() {
    let src = r#"<?php
function f() {
  $r = [];
  $a = range(1, 3); $b = $a; unset($b); $r[] = gc_status()['roots'];
  $b = $a; $b = 0; $c = $a; unset($c); $r[] = gc_status()['roots'];
  unset($a); $r[] = gc_status()['roots'];
  $p = range(1, 2); $q = range(1, 2); $p2 = $p; $q2 = $q; unset($p2, $q2); $r[] = gc_status()['roots'];
  $p = null; $r[] = gc_status()['roots'];
  $s = range(1, 2); $t = $s; unset($t); $r[] = gc_status()['roots'];
  $q = null; $s = null; $r[] = gc_status()['roots'];
  $o = new stdClass; $o->list = range(1, 2); $l = $o->list; unset($l); $r[] = gc_status()['roots'];
  $o = null; $r[] = gc_status()['roots'];
  return implode(',', $r);
}
echo f(), "\n", f(), "\n";
"#;
    assert_eq!(run(src), "1,1,0,2,1,2,0,1,0\n1,1,0,2,1,2,0,1,0\n");
}

/// The script of [`literal_and_nested_array_roots`].
const LITERALS: &str = r#"<?php
function st() { $s = gc_status(); return "runs={$s['runs']} collected={$s['collected']} threshold={$s['threshold']} roots={$s['roots']}"; }
function f() {
  $r = [];
  $a = [1]; $b = $a; unset($b); $r[] = gc_status()['roots'];
  $c = $a; unset($c); $c = $a; $c = 1; $r[] = gc_status()['roots'];
  unset($a); $r[] = gc_status()['roots'];
  $x = [2]; $y = $x; unset($y); $r[] = gc_status()['roots'];
  $z = [3]; $w = $z; unset($w); $r[] = gc_status()['roots'];
  $x = null; $r[] = gc_status()['roots'];
  $e = []; $f = $e; unset($f); $r[] = gc_status()['roots'];
  $n = [[1], [2]]; $m = $n; unset($n); $r[] = gc_status()['roots'];
  unset($m); $r[] = gc_status()['roots'];
  $in = [5]; $out = [$in, $in]; unset($out); $r[] = gc_status()['roots'];
  return implode(',', $r);
}
echo f(), "\n", st(), "\n";
echo gc_collect_cycles(), " ", st(), "\n";
"#;

/// Literal arrays stay held by their literal, the empty array is never a
/// root, and an array freed holding arrays still held elsewhere buffers
/// them (`zend_array_destroy`); a run empties the buffer.
#[test]
fn literal_and_nested_array_roots() {
    assert_eq!(
        run(LITERALS),
        "1,1,1,2,3,3,3,4,4,5\n\
         runs=0 collected=0 threshold=10001 roots=5\n\
         0 runs=1 collected=0 threshold=10001 roots=0\n"
    );
}

/// No buffered array survives a request: the next request on the same
/// thread starts with an empty buffer and counts the same roots.
#[test]
fn array_roots_do_not_survive_a_request() {
    let first = run(LITERALS);
    assert_eq!(run(LITERALS), first);
    let src = r#"<?php
$keep = [range(1, 2)];
$k2 = $keep[0];
unset($k2);
echo gc_status()['roots'], "\n";
"#;
    assert_eq!(run(src), "1\n");
    assert_eq!(run(src), "1\n");
}

/// Object cycles (each object holds an array holding it) are collected,
/// destructors and all, when the roots buffered by array releases reach
/// the threshold, before the loop ends; the run collected fewer than 100
/// nodes, so the threshold rises.
#[test]
fn array_roots_trigger_a_run_at_the_threshold() {
    let src = r#"<?php
function st() { $s = gc_status(); return "runs={$s['runs']} collected={$s['collected']} threshold={$s['threshold']} roots={$s['roots']}"; }
class D { public $arr; public $id; function __construct($id) { $this->id = $id; $this->arr = [$this]; } function __destruct() { echo "d{$this->id} "; } }
function t() {
  for ($i = 0; $i < 3; $i++) { new D($i); }
  $keep = [];
  for ($i = 0; $i < 10100; $i++) { $x = [$i]; $keep[] = $x; }
  echo "| after loop\n";
  return count($keep);
}
echo t(), "\n", st(), "\n";
"#;
    assert_eq!(
        run(src),
        "d0 d1 d2 | after loop\n\
         10100\n\
         runs=2 collected=6 threshold=20001 roots=0\n"
    );
}

/// `gc_adjust_threshold`: an automatic run over array roots that frees
/// nothing raises the threshold by 10000; a productive one brings it
/// back down to 10001.
#[test]
fn threshold_after_unproductive_and_productive_runs() {
    let src = r#"<?php
function st() { $s = gc_status(); return "runs={$s['runs']} collected={$s['collected']} threshold={$s['threshold']} roots={$s['roots']}"; }
function unprod() { $keep = []; for ($i = 0; $i < 10500; $i++) { $x = [$i]; $keep[] = $x; } return count($keep); }
echo unprod(), " ", st(), "\n";
function prod() { for ($i = 0; $i < 25000; $i++) { $o = new stdClass; $o->a = [$o]; } }
prod();
echo st(), "\n";
echo unprod(), " ", st(), "\n";
"#;
    assert_eq!(
        run(src),
        "10500 runs=1 collected=0 threshold=20001 roots=0\n\
         runs=2 collected=40000 threshold=10001 roots=5000\n\
         10500 runs=3 collected=50000 threshold=10001 roots=0\n"
    );
}
