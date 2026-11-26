// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! When a generator's frame is released (`zend_generator_dtor_storage`,
//! `zend_generator_close`, `Zend/zend_generators.c`): unstarted, suspended,
//! finished, suspended inside `try`/`finally` (the `finally` runs),
//! delegating with `yield from`, overwritten in a loop, a temporary a
//! `foreach` breaks out of, held by another generator, and collected in a
//! cycle. A generator with nothing to unwind is closed as its last handle
//! goes, without the destructor call; the order of the destructors of the
//! objects its frame held is what this pins. Expected output: PHP 8.5.

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
fn generator_frames_are_released_when_php_releases_them() {
    let out = run(r#"<?php
class D { function __construct(public string $n) {} function __destruct() { echo "  destruct {$this->n}\n"; } }
function plain($tag) { $d = new D($tag); yield 1; yield 2; echo "  end $tag\n"; }
function fin($tag) { $d = new D($tag); try { yield 1; yield 2; } finally { echo "  finally $tag\n"; } }
function inner() { $d = new D('inner'); yield 1; yield 2; }
function outer() { $d = new D('outer'); yield from inner(); }
function args(D $d) { yield 1; }
echo "unstarted\n"; $g = plain('unstarted'); unset($g); echo "-\n";
echo "unstarted with an argument\n"; $g = args(new D('arg')); unset($g); echo "-\n";
echo "suspended\n"; $g = plain('suspended'); $g->current(); unset($g); echo "-\n";
echo "finished\n"; $g = plain('finished'); foreach ($g as $v) { } echo " after loop\n"; unset($g); echo "-\n";
echo "suspended in try/finally\n"; $g = fin('fin'); $g->current(); unset($g); echo "-\n";
echo "unstarted with finally\n"; $g = fin('fin0'); unset($g); echo "-\n";
echo "delegating\n"; $g = outer(); $g->current(); unset($g); echo "-\n";
echo "overwritten in a loop\n"; for ($i = 0; $i < 3; $i++) { $g = plain("loop$i"); $g->current(); } echo " after\n"; unset($g); echo "-\n";
echo "temporary\n"; foreach (plain('tmp') as $v) { if ($v === 1) { break; } } echo "-\n";
echo "held by another generator\n"; $a = plain('held'); $a->current(); $b = (function () use ($a) { yield $a; })(); $b->current(); unset($a); echo " still held\n"; unset($b); echo "-\n";
echo "collected in a cycle\n"; $g = (function () { $self = yield; $d = new D('cycle'); yield 1; })(); $g->current(); $g->send($g); unset($g); echo " before gc\n"; gc_collect_cycles(); echo "-\n";
echo "done\n";
"#);
    assert_eq!(
        out,
        r"unstarted
-
unstarted with an argument
  destruct arg
-
suspended
  destruct suspended
-
finished
  end finished
  destruct finished
 after loop
-
suspended in try/finally
  finally fin
  destruct fin
-
unstarted with finally
-
delegating
  destruct inner
  destruct outer
-
overwritten in a loop
  destruct loop0
  destruct loop1
 after
  destruct loop2
-
temporary
  destruct tmp
-
held by another generator
 still held
  destruct held
-
collected in a cycle
 before gc
  destruct cycle
-
done
"
    );
}
