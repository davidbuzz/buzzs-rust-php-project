// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! What releasing values records in the cycle collector's root buffer, as
//! `gc_status()` and `gc_collect_cycles()` show it: references released
//! by returning frames (bound by `global`, `static` or `$b = &$a`),
//! references freed while buffered (`GC_REMOVE_FROM_BUFFER`: their slot
//! is reused, so they never move the threshold), objects and arrays in
//! the variables of returning frames. The expected output is
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
