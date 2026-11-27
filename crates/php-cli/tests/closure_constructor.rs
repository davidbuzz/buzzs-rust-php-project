// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Closure` declares `private function __construct() {}`
//! (Zend/zend_closures.stub.php). `new Closure` never reaches it
//! (`zend_closure_get_constructor` throws), but reflection shows it: the
//! class is not instantiable, and a tool that builds objects from their
//! constructors (a test framework deriving value generators from parameter
//! types) asks that before it tries `new`. Expected output from
//! `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn closure_has_a_private_constructor() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("closure_constructor.php");
    std::fs::write(&path, SCRIPT).expect("write script");
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .arg("-n")
        .arg(&path)
        .output()
        .expect("spawn php");
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
}

const SCRIPT: &str = r###"<?php
function t(string $what, callable $f) {
    try { $r = $f(); echo "$what: "; var_dump($r); }
    catch (Throwable $e) { echo "$what: ", get_class($e), ": ", $e->getMessage(), "\n"; }
}
$r = new ReflectionClass('Closure');
t('isInstantiable', fn () => $r->isInstantiable());
t('constructor', fn () => implode(' ', Reflection::getModifierNames($r->getConstructor()->getModifiers())) . ' ' . $r->getConstructor()->class . '::' . $r->getConstructor()->name);
t('methods', fn () => implode(',', array_map(fn ($m) => $m->name, $r->getMethods())));
t('method_exists', fn () => method_exists('Closure', '__construct'));
t('is_callable', fn () => is_callable(['Closure', '__construct']));
t('new', fn () => new Closure());
t('newInstance', fn () => $r->newInstance());
t('newInstanceArgs', fn () => $r->newInstanceArgs([]));
t('newInstanceWithoutConstructor', fn () => get_class($r->newInstanceWithoutConstructor()));
t('call the constructor', fn () => (function () {})->__construct());
t('hasMethod', fn () => $r->hasMethod('__construct'));
t('ReflectionMethod', fn () => (string) new ReflectionMethod('Closure', '__construct'));
"###;

const EXPECTED: &str = r###"isInstantiable: bool(false)
constructor: string(28) "private Closure::__construct"
methods: string(61) "__construct,bind,bindTo,call,fromCallable,getCurrent,__invoke"
method_exists: bool(true)
is_callable: bool(false)
new: Error: Instantiation of class Closure is not allowed
newInstance: Error: Instantiation of class Closure is not allowed
newInstanceArgs: Error: Instantiation of class Closure is not allowed
newInstanceWithoutConstructor: ReflectionException: Class Closure is an internal class marked as final that cannot be instantiated without invoking its constructor
call the constructor: Error: Call to private method Closure::__construct() from global scope
hasMethod: bool(true)
ReflectionMethod: string(90) "Method [ <internal:Core, ctor> private method __construct ] {

  - Parameters [0] {
  }
}
"
"###;
