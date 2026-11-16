// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `get_class_vars()` (`Zend/zend_builtin_functions.c`: `add_class_vars`
//! over `ce->properties_info`): the order of the entries (a class's own
//! declarations in declaration order, then the parent's table, then the
//! trait properties), the caller's scope deciding which private and
//! protected entries show, and the declared defaults of static properties
//! rather than their current values. Expected output from `/usr/bin/php -n`
//! (8.3 and master share `add_class_vars`).
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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn class_vars_follow_properties_info_and_report_static_defaults() {
    assert_eq!(
        run(r#"<?php
trait T { public $t1 = 't'; public static $ts = 'ts'; }
class A { public $x = 1; public $y = 2; private $px = 'pa'; protected $pr = 'pr'; public static $sa = 'sa'; private static $psa = 'psa';
    static function f($c) { return get_class_vars($c); } }
class B extends A { use T; public $z = 3; public static $sb = 'sb'; public $y = 22; private $px = 'pb';
    function __construct(public $prom = 'p', protected int $pi = 5) {} public $after = 'af'; public int $typed;
    static function g($c) { return get_class_vars($c); } }
class C extends B { public $c1; }
echo serialize(get_class_vars('B')), "\n";
echo serialize(A::f('C')), "\n";
echo serialize(B::g('C')), "\n";
class D { public static $s1 = 1; public $i1 = 1; public static $s2 = 2; public $i2 = 2; }
class E extends D { public $i2 = 'e'; public static $s1 = 'e'; public $i3 = 3; }
echo serialize(get_class_vars('E')), "\n";
class S { public static $a = 1; const K = 3; public static $k = self::K; public static $arr = [self::K]; }
S::$a = 5;
S::$arr[] = 4;
class S2 extends S {}
S2::$a = 6;
echo serialize(get_class_vars('S')), serialize(get_class_vars('S2')), S::$a, S::$arr[1], "\n";
class MyEx extends Exception { public $mine = 1; protected $code = 5;
    static function f($c) { return get_class_vars($c); } }
echo serialize(MyEx::f('MyEx')), "\n";
trait T1 { public $a = 'T1a'; }
trait T2 { use T1; public $b = 'T2b'; }
class UT { use T2; public $own = 1; }
class PP { public $t = 'pp'; }
trait TT { public $t = 'pp'; public $u = 'u'; }
class CC extends PP { use TT; public $c = 1; }
echo serialize(get_class_vars('UT')), serialize(get_class_vars('CC')), "\n";
"#),
        r#"a:10:{s:1:"z";i:3;s:1:"y";i:22;s:4:"prom";N;s:5:"after";s:2:"af";s:5:"typed";N;s:1:"x";i:1;s:2:"t1";s:1:"t";s:2:"sb";s:2:"sb";s:2:"sa";s:2:"sa";s:2:"ts";s:2:"ts";}
a:14:{s:2:"c1";N;s:1:"z";i:3;s:1:"y";i:22;s:4:"prom";N;s:2:"pi";N;s:5:"after";s:2:"af";s:5:"typed";N;s:1:"x";i:1;s:2:"pr";s:2:"pr";s:2:"t1";s:1:"t";s:2:"sb";s:2:"sb";s:2:"sa";s:2:"sa";s:3:"psa";s:3:"psa";s:2:"ts";s:2:"ts";}
a:14:{s:2:"c1";N;s:1:"z";i:3;s:1:"y";i:22;s:2:"px";s:2:"pb";s:4:"prom";N;s:2:"pi";N;s:5:"after";s:2:"af";s:5:"typed";N;s:1:"x";i:1;s:2:"pr";s:2:"pr";s:2:"t1";s:1:"t";s:2:"sb";s:2:"sb";s:2:"sa";s:2:"sa";s:2:"ts";s:2:"ts";}
a:5:{s:2:"i2";s:1:"e";s:2:"i3";i:3;s:2:"i1";i:1;s:2:"s1";s:1:"e";s:2:"s2";i:2;}
a:3:{s:1:"a";i:1;s:1:"k";i:3;s:3:"arr";a:1:{i:0;i:3;}}a:3:{s:1:"a";i:1;s:1:"k";i:3;s:3:"arr";a:1:{i:0;i:3;}}64
a:5:{s:4:"mine";i:1;s:4:"code";i:5;s:7:"message";s:0:"";s:4:"file";s:0:"";s:4:"line";i:0;}
a:3:{s:3:"own";i:1;s:1:"b";s:3:"T2b";s:1:"a";s:3:"T1a";}a:3:{s:1:"c";i:1;s:1:"t";s:2:"pp";s:1:"u";s:1:"u";}
"#
    );
}
