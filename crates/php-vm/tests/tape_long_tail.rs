// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ops the tape runs as rows of their own instead of leaving its loop for
//! the second-level dispatch or `Vm::run` (`exec/tape.rs`): `$this` as a
//! value (`ZEND_FETCH_THIS`), closure creation
//! (`ZEND_DECLARE_LAMBDA_FUNCTION`), and the other long-tail ops, each
//! shape run several times so that the rows' caches are warm, against
//! the output of `/usr/bin/php8.5 -n` (php-src master where they differ,
//! as noted).
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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = common::vm_with_stdlib(&options, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// `$this` returned, passed, put in a callback array and copied to a
/// variable; closures and arrow functions binding by value, by reference,
/// an undefined variable (the warning), `$this` and nothing; static
/// closures, which php-src master caches per declaration (8.5 makes a new
/// one each time: there `same()` prints "different"); a captured object
/// released with the closure.
#[test]
fn this_values_and_closures() {
    assert_eq!(
        run(r#"<?php
class K {
    public $n = 0;
    function self_() { return $this; }
    function pass() { return spl_object_id($this) === spl_object_id($this->self_()); }
    function cb() { return [$this, 'm']; }
    function m($x) { return "m$x"; }
    function copy() { $x = $this; $x->n++; return $x->n; }
    function closures($k) {
        $a = function ($x) use ($k) { return "$k$x" . $this->n; };
        $b = fn($x) => "$k$x" . $this->n;
        $c = static function ($x) { return "s$x"; };
        $d = function () use (&$k) { $k .= '!'; return $k; };
        $e = function () use ($undef) { return var_export($undef, true); };
        return [$a(1), $b(2), $c(3), $d(), $k, $e()];
    }
    static function st() { try { return isset($this) ? 'set' : 'unset'; } catch (Error $e) { return $e->getMessage(); } }
}
$k = new K;
for ($i = 0; $i < 3; $i++) {
    echo get_class($k->self_()), ' ', var_export($k->pass(), true), ' ', call_user_func($k->cb(), $i), ' ', $k->copy(), "\n";
    echo implode(',', $k->closures("k$i")), "\n";
    echo K::st(), "\n";
}
function plain($v) { $f = function () use ($v) { return $v * 2; }; $g = fn() => $v + 1; $h = static fn() => 7; return $f() + $g() + $h(); }
for ($i = 0; $i < 3; $i++) echo plain($i), "\n";
function same() { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = static fn() => 1; } return $r[0] === $r[2] ? 'same' : 'different'; }
echo same(), "\n";
function nonstatic() { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = fn() => 1; } return $r[0] === $r[2] ? 'same' : 'different'; }
echo nonstatic(), "\n";
class Dd { function __destruct() { echo "destruct\n"; } }
function captures() { $o = new Dd; $f = function () use ($o) { return 1; }; unset($o); echo "unset\n"; $f = null; echo "after\n"; }
captures();
"#),
        r#"K true m0 1

Warning: Undefined variable $undef in t.php on line 14
k011,k021,s3,k0!,k0!,NULL
unset
K true m1 2

Warning: Undefined variable $undef in t.php on line 14
k112,k122,s3,k1!,k1!,NULL
unset
K true m2 3

Warning: Undefined variable $undef in t.php on line 14
k213,k223,s3,k2!,k2!,NULL
unset
8
11
14
same
different
unset
destruct
after
"#
    );
}
