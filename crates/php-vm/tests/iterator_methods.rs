// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `foreach` over user iterators through the methods resolved once per
//! class (`zend_class_iterator_funcs`, `Zend/zend_interfaces.c`):
//! methods overridden in subclasses, `IteratorAggregate` returning a
//! generator or another aggregate, iterator_to_array and unpacking, an
//! exception from each method, and more classes than the per-thread
//! table remembers at once. Expected output from `/usr/bin/php8.5 -n`.
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
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn iterator_methods_are_the_classes_own() {
    assert_eq!(
        run(r###"<?php
class It implements Iterator { protected $i = 0; function __construct(protected $n) {}
  function current(): mixed { return "c$this->i"; } function key(): mixed { return $this->i; }
  function next(): void { $this->i++; } function valid(): bool { return $this->i < $this->n; } function rewind(): void { $this->i = 0; } }
class Sub extends It { function current(): mixed { return 'sub' . parent::current(); } function key(): mixed { return "k$this->i"; } }
class Sub2 extends Sub { function rewind(): void { $this->i = 1; } }
class Agg implements IteratorAggregate { function getIterator(): Iterator { yield 'a' => 1; yield 'b' => 2; } }
class Agg2 implements IteratorAggregate { function getIterator(): Iterator { return new Sub(2); } }
class Nested implements IteratorAggregate { function getIterator(): Traversable { return new Agg2; } }
class Bad implements IteratorAggregate { function getIterator(): Traversable { return new It(1); } }
function show($t) { $o = []; foreach ($t as $k => $v) { $o[] = "$k=$v"; } return implode(',', $o); }
function run() {
    for ($r = 0; $r < 2; $r++) {
        echo show(new It(2)), ' | ', show(new Sub(2)), ' | ', show(new Sub2(3)), ' | ', show(new Agg), ' | ', show(new Agg2), ' | ', show(new Nested), ' | ', show(new Bad), "\n";
        echo implode(',', iterator_to_array(new Sub(2))), ' ', implode(',', [...new Sub2(3)]), "\n";
    }
}
run();
class Thrower implements Iterator { private $i = 0; function __construct(private $where) {}
  private function t($m) { if ($m === $this->where && $this->i == 1) throw new Exception("in $m"); }
  function current(): mixed { $this->t('current'); return $this->i; } function key(): mixed { $this->t('key'); return $this->i; }
  function next(): void { $this->i++; $this->t('next'); } function valid(): bool { $this->t('valid'); return $this->i < 3; }
  function rewind(): void { if ($this->where === 'rewind') throw new Exception('in rewind'); $this->i = 0; } }
class AggThrow implements IteratorAggregate { function getIterator(): Iterator { throw new Exception('in getIterator'); } }
function thr() {
    foreach (['current', 'key', 'next', 'valid', 'rewind'] as $w) {
        $seen = [];
        try { foreach (new Thrower($w) as $k => $v) { $seen[] = "$k:$v"; } } catch (Exception $e) { echo $e->getMessage(), ' after ', implode(',', $seen), "\n"; }
    }
    try { foreach (new AggThrow as $v) {} } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    try { foreach ((function () { yield 1; throw new Exception('gen'); })() as $v) { echo $v; } } catch (Exception $e) { echo ' ', $e->getMessage(), "\n"; }
}
thr();
function many() {
    for ($j = 0; $j < 24; $j++) { eval("class E$j extends It { function current(): mixed { return '$j' . \$this->i; } }"); }
    $o = [];
    for ($r = 0; $r < 2; $r++) { for ($j = 0; $j < 24; $j++) { $c = "E$j"; $o[] = show(new $c(2)); } }
    echo implode(' ', $o), "\n";
}
many();
"###),
        r###"0=c0,1=c1 | k0=subc0,k1=subc1 | k1=subc1,k2=subc2 | a=1,b=2 | k0=subc0,k1=subc1 | k0=subc0,k1=subc1 | 0=c0
subc0,subc1 subc1,subc2
0=c0,1=c1 | k0=subc0,k1=subc1 | k1=subc1,k2=subc2 | a=1,b=2 | k0=subc0,k1=subc1 | k0=subc0,k1=subc1 | 0=c0
subc0,subc1 subc1,subc2
in current after 0:0
in key after 0:0
in next after 0:0
in valid after 0:0
in rewind after 
in getIterator
1 gen
0=00,1=01 0=10,1=11 0=20,1=21 0=30,1=31 0=40,1=41 0=50,1=51 0=60,1=61 0=70,1=71 0=80,1=81 0=90,1=91 0=100,1=101 0=110,1=111 0=120,1=121 0=130,1=131 0=140,1=141 0=150,1=151 0=160,1=161 0=170,1=171 0=180,1=181 0=190,1=191 0=200,1=201 0=210,1=211 0=220,1=221 0=230,1=231 0=00,1=01 0=10,1=11 0=20,1=21 0=30,1=31 0=40,1=41 0=50,1=51 0=60,1=61 0=70,1=71 0=80,1=81 0=90,1=91 0=100,1=101 0=110,1=111 0=120,1=121 0=130,1=131 0=140,1=141 0=150,1=151 0=160,1=161 0=170,1=171 0=180,1=181 0=190,1=191 0=200,1=201 0=210,1=211 0=220,1=221 0=230,1=231
"###
    );
}
