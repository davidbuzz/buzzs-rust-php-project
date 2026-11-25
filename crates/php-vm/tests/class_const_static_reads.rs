// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Class-constant and static-property reads inside functions, where they
//! run as the tape's own rows on a hit of the op's run-time cache
//! (php-src's `ZEND_FETCH_CLASS_CONSTANT` and `ZEND_FETCH_STATIC_PROP_R`
//! with `CACHED_PTR`, `Zend/zend_vm_def.h`) and decline to the generic op
//! otherwise: inheritance, `self::`/`parent::`/`static::` late binding,
//! enum and interface constants, `final` constants, visibility errors, a
//! constant whose initialiser throws on every use, static properties
//! redeclared in subclasses or shared with them, references to them,
//! typed and undeclared ones. Each read runs several times, so the cached
//! row is taken after the first. Expected output from `/usr/bin/php8.5 -n`.
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
fn class_constants_resolve_through_scope_and_late_binding() {
    assert_eq!(
        run(r###"<?php
interface I { const IC = 'i'; }
class A implements I { const X = 1; const Y = 'a'; public const Z = [1, 2];
    protected const P = 'pa'; private const Q = 'qa';
    static function sx() { return self::X; } static function lx() { return static::X; }
    function q() { return self::Q . static::P; } }
class B extends A { const X = 2; protected const P = 'pb';
    static function px() { return parent::X; } }
enum E: int { case One = 1; const D = self::One; const N = 7; static function n() { return self::N; } }
final class F { final public const K = 'k'; }
class Lazy { const T = UNDEF_CONST_XYZ; const OK = 3; }
function loop() {
    $out = [];
    for ($i = 0; $i < 3; $i++) {
        $out[] = A::X . B::X . A::sx() . B::sx() . A::lx() . B::lx() . B::px() . A::IC . B::Y;
        $v = A::Z; $out[] = count($v);
        $out[] = E::N . E::n() . E::D->value . E::One->name . F::K . (new B)->q() . (new A)->q();
    }
    return implode(',', $out);
}
echo loop(), "\n";
function priv() { for ($i = 0; $i < 2; $i++) { try { echo A::Q; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } } }
priv();
function prot() { for ($i = 0; $i < 2; $i++) { try { echo B::P; } catch (Error $e) { echo $e->getMessage(), "\n"; } } }
prot();
function lazy() { for ($i = 0; $i < 3; $i++) { try { echo Lazy::OK; echo Lazy::T; } catch (Error $e) { echo ' ', $e->getMessage(), "\n"; } } }
lazy();
function undef() { for ($i = 0; $i < 2; $i++) { try { echo A::NOPE; } catch (Error $e) { echo $e->getMessage(), "\n"; } } }
undef();
"###),
        r###"1211121ia,2,771Onekqapbqapa,1211121ia,2,771Onekqapbqapa,1211121ia,2,771Onekqapbqapa
Error: Cannot access private constant A::Q
Error: Cannot access private constant A::Q
Cannot access protected constant B::P
Cannot access protected constant B::P
3 Undefined constant "UNDEF_CONST_XYZ"
3 Undefined constant "UNDEF_CONST_XYZ"
3 Undefined constant "UNDEF_CONST_XYZ"
Undefined constant A::NOPE
Undefined constant A::NOPE
"###
    );
}

#[test]
fn static_property_reads_follow_redeclaration_and_references() {
    assert_eq!(
        run(r###"<?php
class A { public static $n = 1; public static $s = 'a'; protected static $p = 'pa'; public static ?int $t = null; public static int $u;
    static function get() { return self::$n . static::$n . static::$s; } }
class B extends A { public static $n = 2; }
class C extends A {}
function loop() {
    $o = [];
    for ($i = 0; $i < 3; $i++) {
        $o[] = A::$n . B::$n . C::$n . A::get() . B::get() . C::get();
        $x = A::$s; $o[] = $x;
        $o[] = var_export(A::$t, true) . var_export(isset(A::$t), true) . (A::$t ?? 'dflt');
        C::$n++;
    }
    return implode(',', $o);
}
echo loop(), "\n";
function refs() {
    $r = &B::$n; $r = 10;
    $a = &A::$s;
    for ($i = 0; $i < 3; $i++) { echo B::$n, A::$s, ' '; $r++; $a .= 'x'; $v = B::$n; echo $v, ' '; }
    echo "\n";
}
refs();
function errs() {
    for ($i = 0; $i < 2; $i++) {
        try { echo A::$u; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { echo A::$p; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { echo A::$nope; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        echo A::$nope ?? 'none', "\n";
    }
}
errs();
function arr() { A::$s = [1, 2]; for ($i = 0; $i < 2; $i++) { $x = A::$s; $x[] = $i; echo count($x), count(A::$s); } echo "\n"; }
arr();
"###),
        r###"12111a12a11a,a,NULLfalsedflt,22222a22a22a,a,NULLfalsedflt,32333a32a33a,a,NULLfalsedflt
10a 11 11ax 12 12axx 13 
Typed static property A::$u must not be accessed before initialization
Cannot access protected property A::$p
Access to undeclared static property A::$nope
none
Typed static property A::$u must not be accessed before initialization
Cannot access protected property A::$p
Access to undeclared static property A::$nope
none
3232
"###
    );
}
