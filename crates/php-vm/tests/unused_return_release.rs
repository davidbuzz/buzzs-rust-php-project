// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A call whose result is unused: `ZEND_RETURN` releases the returned
//! temporary before `zend_leave_helper` leaves the frame, so a destructor
//! it runs sees the callee in its backtrace, and runs before the callee's
//! variables are released (they are freed after the frame is left). A
//! returned compiled variable is released with the frame's variables.
//! Expected output from `/usr/bin/php8.5 -n` (PHP 8.5.12), plus the
//! compile-time deprecations of php-src master
//! (ref: Zend/zend_vm_def.h `ZEND_RETURN`, `ZEND_RETURN_BY_REF`,
//! `zend_leave_helper`; Zend/zend_compile.c).

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

/// Each call form: a function, a method, a static method, an arrow
/// function, a closure, a returned array holding the object, a call
/// from a callback a builtin calls, and a used result for contrast.
#[test]
fn unused_result_is_released_in_the_callee() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "dtor {$this->n} in: ", implode(",", array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; } }
function make() { return new D('fn'); }
function outer() { make(); echo "after call\n"; }
outer();
class M { function m() { return new D('method'); } static function s() { return new D('static'); } }
function viaMethod() { (new M)->m(); echo "after method\n"; M::s(); echo "after static\n"; $f = fn() => new D('arrow'); $f(); echo "after arrow\n"; $c = function () { return new D('closure'); }; $c(); echo "after closure\n"; }
viaMethod();
function arr() { return [new D('array')]; }
function viaArr() { arr(); echo "after array\n"; }
viaArr();
function nested() { return new D('nested'); }
function viaCallback() { array_map(function () { nested(); echo "in callback\n"; }, [1]); echo "after callback\n"; }
viaCallback();
function used() { return new D('used'); }
function viaUsed() { $x = used(); echo "kept\n"; unset($x); echo "after used\n"; }
viaUsed();
for ($i = 0; $i < 3; $i++) { make(); }
echo "end\n";
"#),
        r#"dtor fn in: __destruct,make,outer
after call
dtor method in: __destruct,m,viaMethod
after method
dtor static in: __destruct,s,viaMethod
after static
dtor arrow in: __destruct,{closure:viaMethod():7},viaMethod
after arrow
dtor closure in: __destruct,{closure:viaMethod():7},viaMethod
after closure
dtor array in: __destruct,arr,viaArr
after array
dtor nested in: __destruct,nested,{closure:viaCallback():13},array_map,viaCallback
in callback
after callback
kept
dtor used in: __destruct,viaUsed
after used
dtor fn in: __destruct,make
dtor fn in: __destruct,make
dtor fn in: __destruct,make
end
"#
    );
}

/// The returned temporary goes first, then the callee's variables in
/// their order, then `$this`; a returned variable goes with the others.
#[test]
fn unused_result_is_released_before_the_callee_variables() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "dtor {$this->n} in: ", implode(",", array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; } }
function cv() { $d = new D('cv'); return $d; }
function local() { $l = new D('local'); return new D('ret'); }
function localcv() { $l = new D('local'); $r = new D('retcv'); return $r; }
function many() { $a = new D('cv-a'); $b = new D('cv-b'); return [new D('ret-0'), new D('ret-1')]; }
class S { function self() { return $this; } function __destruct() { echo "S dtor in: ", implode(",", array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; } }
function outer() { cv(); local(); localcv(); many(); (new S)->self(); echo "after\n"; }
outer();
"#),
        r#"dtor cv in: __destruct,outer
dtor ret in: __destruct,local,outer
dtor local in: __destruct,outer
dtor local in: __destruct,outer
dtor retcv in: __destruct,outer
dtor ret-0 in: __destruct,many,outer
dtor ret-1 in: __destruct,many,outer
dtor cv-a in: __destruct,outer
dtor cv-b in: __destruct,outer
S dtor in: __destruct,outer
after
"#
    );
}

/// A destructor of the discarded value that throws: the exception is
/// rethrown at the call site (the callee's own `catch` does not see it),
/// with the callee in its trace.
#[test]
fn destructor_exception_of_unused_result_leaves_the_callee() {
    assert_eq!(
        run(r#"<?php
class T { function __destruct() { throw new Exception("from dtor"); } }
function mt() { return new T; }
function guarded() { try { return new T; } catch (Exception $e) { echo "callee caught\n"; } }
function show($e) { echo "caller caught: ", $e->getMessage(), " line ", $e->getLine(), " trace: ", implode(",", array_map(fn($f) => $f['function'], $e->getTrace())), "\n"; }
function outer() {
    try { mt(); echo "not reached\n"; } catch (Exception $e) { show($e); }
    try { guarded(); echo "not reached\n"; } catch (Exception $e) { show($e); }
    echo "after\n";
}
outer();
"#),
        r#"caller caught: from dtor line 2 trace: __destruct,mt,outer
caller caught: from dtor line 2 trace: __destruct,guarded,outer
after
"#
    );
}

/// A declared return type is verified first; a value it refuses is
/// released while the `TypeError` is thrown from the callee.
#[test]
fn typed_return_is_verified_before_the_release() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "dtor {$this->n} in: ", implode(",", array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; } }
function typed(): D { return new D('typed'); }
function nullable(): ?object { return new D('nullable'); }
function badtyped(): int { return new D('badtyped'); }
function outer() {
    typed(); echo "after typed\n";
    nullable(); echo "after nullable\n";
    try { badtyped(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    echo "after bad\n";
}
outer();
"#),
        r#"dtor typed in: __destruct,typed,outer
after typed
dtor nullable in: __destruct,nullable,outer
after nullable
dtor badtyped in: __destruct,badtyped,outer
badtyped(): Return value must be of type int, D returned
after bad
"#
    );
}

/// A generator's return value (kept by the generator), by-reference
/// returns of a variable and of a temporary, a constructor's return value
/// (`new C`), a return through `finally`, a destructor's return value.
#[test]
fn unused_result_of_other_return_paths() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "dtor {$this->n} in: ", implode(",", array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; } }
function gen() { yield 1; return new D('genret'); }
function viaGen() { $g = gen(); foreach ($g as $_) {} echo "gen done\n"; unset($g); echo "after gen\n"; }
viaGen();
function &byrefcv() { $o = new D('byref-cv'); return $o; }
function &byreftmp() { return new D('byref-tmp'); }
function viaRef() { byrefcv(); echo "after byref cv\n"; byreftmp(); echo "after byref tmp\n"; }
viaRef();
class C { function __construct() { return new D('ctor'); } }
function viaCtor() { new C; echo "after ctor\n"; }
viaCtor();
function fin() { try { return new D('finally'); } finally { echo "in finally\n"; } }
function viaFinally() { fin(); echo "after finally\n"; }
viaFinally();
class X { function __destruct() { echo "X dtor\n"; return new D('from-dtor'); } }
function viaDtor() { $x = new X; unset($x); echo "after x\n"; }
viaDtor();
"#),
        r#"
Deprecated: Returning a value from a constructor is deprecated in t.php on line 10

Deprecated: Returning a value from a destructor is deprecated in t.php on line 16
gen done
dtor genret in: __destruct,viaGen
after gen
dtor byref-cv in: __destruct,viaRef
after byref cv

Notice: Only variable references should be returned by reference in t.php on line 7
dtor byref-tmp in: __destruct,byreftmp,viaRef
after byref tmp
dtor ctor in: __destruct,__construct,viaCtor
after ctor
in finally
dtor finally in: __destruct,fin,viaFinally
after finally
X dtor
dtor from-dtor in: __destruct,viaDtor
after x
"#
    );
}

/// The possible cycle roots a returning frame leaves: its variables and
/// its call's own `$this` or closure, counted by `gc_status()` (objects
/// only), whether the result is used or not, for each kind of frame.
#[test]
fn returning_frames_buffer_the_same_possible_roots() {
    assert_eq!(
        run(r#"<?php
function roots() { return gc_status()['roots']; }
class O { public $p; function keep() { $local = $this; return 1; } function same() { return $this; } }
function holdsObject() { $o = new O; $o2 = $o; return 1; }
function returnsObject() { $o = new O; return $o; }
function withClosure() { $o = new O; $f = function () use ($o) { return $o; }; $f(); return 2; }
function byArg($x) { $y = $x; return 3; }
$o = new O;
$base = roots();
holdsObject(); echo roots() - $base, "\n";
returnsObject(); echo roots() - $base, "\n";
$kept = returnsObject(); echo roots() - $base, "\n";
withClosure(); echo roots() - $base, "\n";
$o->keep(); echo roots() - $base, "\n";
(new O)->keep(); echo roots() - $base, "\n";
$o->same(); echo roots() - $base, "\n";
byArg($o); echo roots() - $base, "\n";
byArg(new O); echo roots() - $base, "\n";
for ($i = 0; $i < 5; $i++) { holdsObject(); returnsObject(); $o->keep(); byArg($o); }
echo roots() - $base, "\n";
"#),
        "0\n0\n1\n1\n2\n2\n2\n2\n2\n2\n"
    );
}
