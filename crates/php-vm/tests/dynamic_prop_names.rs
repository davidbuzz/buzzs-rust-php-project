// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$this->$name` and `$cv->$name` (a property named by a variable),
//! read and assigned, which take a direct look into the class's property
//! table for a declared property (`zend_get_property_offset`,
//! `Zend/zend_object_handlers.c`) and the generic path otherwise: numeric
//! and dynamic names, a parent's private property seen from the parent's
//! and the child's scope, protected ones, typed, readonly and hooked
//! properties, mangled names, `__get`/`__set` for inaccessible and
//! missing names, an unset property assigned again, a property bound to
//! a reference. Expected output from `/usr/bin/php8.5 -n`.
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
fn properties_named_by_variables_match_the_generic_lookup() {
    assert_eq!(
        run(r###"<?php
class Base { public $pub = 'bp'; protected $prot = 'bprot'; private $priv = 'bpriv';
    function readB($n) { return $this->$n; } function writeB($n, $v) { $this->$n = $v; } }
#[AllowDynamicProperties]
class Row extends Base { public $a; public $b; public int $typed = 0; public readonly string $ro; private $priv = 'rpriv';
    public string $hooked { get => 'hooked:' . $this->a; set => $this->a = $value; }
    function __construct(array $row) { foreach ($row as $k => $v) { $this->$k = $v; } }
    function read($n) { return $this->$n; } function write($n, $v) { $this->$n = $v; } function initRo() { $n = 'ro'; $this->$n = 'r'; } }
class Magic { public $d = 'decl'; private $hidden = 'h';
    function __get($n) { return "get($n)"; } function __set($n, $v) { echo "set($n)\n"; } }
function err($f) { try { $f(); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
function run() {
    for ($i = 0; $i < 2; $i++) {
        $r = new Row(['a' => 1, 'b' => [2], '0' => 'zero', '1x' => 'onex', 'dyn' => 'd', 'pub' => 'P']);
        echo $r->read('a'), json_encode($r->read('b')), $r->read('0'), $r->read('1x'), $r->read('dyn'), $r->read('pub'), "\n";
        echo $r->read('priv'), ' ', $r->readB('priv'), ' ', $r->readB('prot'), ' ', $r->read('prot'), "\n";
        $r->writeB('priv', 'B!'); $r->write('priv', 'R!'); echo $r->readB('priv'), $r->read('priv'), "\n";
        $r->write('hooked', 'H'); echo $r->read('hooked'), ' ', $r->a, "\n";
        $r->write('typed', '5'); var_dump($r->read('typed'));
        err(fn() => $r->write('typed', 'x'));
        $r->initRo(); echo $r->read('ro'), "\n"; err(fn() => $r->write('ro', 'again'));
        $n = 'a'; $x = $r; echo $x->$n, "\n"; $x->$n = 'viacv'; echo $r->a, "\n";
        err(function () use ($r) { $n = 'prot'; echo $r->$n; });
        err(function () use ($r) { $n = "\0Row\0priv"; echo $r->$n; });
        err(function () use ($r) { $n = ''; $r->$n = 1; });
        $m = new Magic; $n = 'd'; echo $m->$n; $n = 'hidden'; echo ' ', $m->$n; $n = 'nope'; echo ' ', $m->$n, "\n"; $m->$n = 1; $n = 'hidden'; $m->$n = 2;
        $n = 'd'; $m->$n = 'D'; echo $m->d, "\n";
        unset($r->a); $r->write('a', 'back'); echo $r->read('a'), "\n";
        $o = new stdClass; $o->k = 1; $n = 'k'; echo $o->$n; $o->$n = 2; echo $o->k, "\n";
        $ref = &$r->b; $r->write('b', 'through-ref'); echo $ref, "\n"; unset($ref);
        echo @$r->read('missing') === null ? 'null' : 'x', "\n";
    }
}
run();
"###),
        r###"1[2]zeroonexdP
rpriv bpriv bprot bprot
B!R!
hooked:H H
int(5)
TypeError: Cannot assign string to property Row::$typed of type int
r
Error: Cannot modify readonly property Row::$ro
H
viacv
Error: Cannot access protected property Row::$prot
Error: Cannot access property starting with "\0"
decl get(hidden) get(nope)
set(nope)
set(hidden)
D
back
12
through-ref
null
1[2]zeroonexdP
rpriv bpriv bprot bprot
B!R!
hooked:H H
int(5)
TypeError: Cannot assign string to property Row::$typed of type int
r
Error: Cannot modify readonly property Row::$ro
H
viacv
Error: Cannot access protected property Row::$prot
Error: Cannot access property starting with "\0"
decl get(hidden) get(nope)
set(nope)
set(hidden)
D
back
12
through-ref
null
"###
    );
}
