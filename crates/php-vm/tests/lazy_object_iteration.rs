// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `foreach` and `==` on objects through their properties table
//! (`zend_object_handlers.c`: `zend_std_compare_objects`,
//! `zend_vm_def.h`: `ZEND_FE_RESET_R` / `ZEND_FE_FETCH_R` on plain objects),
//! including lazy objects (`zend_lazy_objects.c`) and a ghost's table of
//! frozen declared-property copies. The lazy-object expectations are derived
//! from php-src master (PHP 8.3 has no lazy objects); the others from
//! `/usr/bin/php8.5 -n`.
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

/// Runs `source` as `t.php` and returns its output.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// A ghost whose properties table exists before its initialization: the
/// initializer's dynamic write (`$this->a = 1`) separates the table,
/// freezing `b` at its default (1) while the slot then gets 3.
const FROZEN_GHOST: &str = r"<?php
#[AllowDynamicProperties]
class C {
    public int $b = 1;
    public function __construct() {
        $this->a = 1;
        $this->b = 3;
    }
}
$r = new ReflectionClass(C::class);
$o = $r->newLazyGhost(function ($o) { $o->__construct(); });
ob_start(); var_dump($o); ob_end_clean();
$r->initializeLazyObject($o);
";

/// `zend_std_compare_objects` compares the classes before initializing
/// anything: a proxy of `C` whose factory would return a `D` is
/// uncomparable with a `D` without the factory running. Two objects of one
/// class with one of them lazy go through their properties tables, which
/// initializes the lazy one. The factory's `D` is a valid real instance:
/// `zend_lazy_object_compatible` wants the proxy's class to be an
/// `instanceof` the real one's (`C extends D`, no added properties); the
/// other way round the initialization is a `TypeError` ("## C vs D" in
/// `Zend/tests/lazy_objects/initializer_must_return_the_right_type.phpt`).
#[test]
fn comparison_checks_the_classes_before_initializing() {
    let src = r#"<?php
class D { public int $a = 1; }
class C extends D {}
$r = new ReflectionClass(C::class);
$ghost = $r->newLazyGhost(function ($o) { echo "init ghost\n"; });
$proxy = $r->newLazyProxy(function ($o) { echo "init proxy\n"; return new D(); });
$d = new D();
var_dump($ghost == $d);
var_dump($proxy == $d);
$r->initializeLazyObject($ghost);
var_dump($ghost == $proxy);
"#;
    assert_eq!(
        run(src),
        "bool(false)\nbool(false)\ninit ghost\ninit proxy\nbool(true)\n"
    );
}

/// `ZEND_FE_RESET_R` walks `zobj->properties`: over a table of frozen
/// copies the loop sees the copy of `b` (1), not its slot (3), and the
/// dynamic property after it. A by-reference loop binds the table's
/// entries (`ZEND_FE_FETCH_RW` makes them references): writes land in the
/// frozen copy and in the dynamic property, never in the slot.
#[test]
fn foreach_over_a_frozen_table_reads_and_writes_the_copies() {
    let src = format!(
        "{FROZEN_GHOST}{}",
        r#"
foreach ($o as $k => $v) { echo "$k=$v\n"; }
echo $o->b, "\n";
foreach ($o as $k => &$v) { $v = $v + 10; }
unset($v);
echo $o->b, "\n";
foreach ($o as $k => $v) { echo "$k=$v\n"; }
"#
    );
    assert_eq!(run(&src), "b=1\na=1\n3\n3\nb=11\na=11\n");
}

/// `==` on an object with a properties table compares the tables
/// (`zend_compare_symbol_tables`): the frozen copy of `b` counts, not the
/// slot.
#[test]
fn comparison_uses_the_frozen_table() {
    let src = format!(
        "{FROZEN_GHOST}{}",
        r"
$p = new C();
$p->b = 1;
$q = new C();
var_dump($o == $p, $o == $q, $o->b == $q->b);
"
    );
    assert_eq!(run(&src), "bool(true)\nbool(false)\nbool(true)\n");
}

/// Without properties tables the declared slots are compared and a defined
/// slot against an undefined one is uncomparable (1) either way; once a
/// table exists (`var_dump()` built it) `zend_hash_compare` orders the
/// undefined `IS_INDIRECT` entry below the defined one (-1).
#[test]
fn undefined_slots_order_below_defined_ones_through_a_table() {
    let src = r"<?php
class U { public $a; public int $t; }
$x = new U; $y = new U; $y->t = 5;
var_dump($x <=> $y, $y <=> $x);
ob_start(); var_dump($x); ob_end_clean();
var_dump($x <=> $y, $y <=> $x);
$x->t = 5;
var_dump($x == $y);
";
    assert_eq!(run(src), "int(1)\nint(1)\nint(-1)\nint(1)\nbool(true)\n");
}

/// Dynamic properties make the element counts differ: the object with more
/// entries orders above (`zend_hash_compare` compares the counts first).
#[test]
fn dynamic_properties_count_in_table_comparisons() {
    let src = r"<?php
#[AllowDynamicProperties]
class C { public $a; }
$x = new C; $y = new C;
$x->p = 1; $y->p = 1; $y->q = 2;
var_dump($x <=> $y, $y <=> $x);
$x->q = 2;
var_dump($x == $y);
$x->q = 3;
var_dump($x <=> $y);
";
    assert_eq!(run(src), "int(-1)\nint(1)\nbool(true)\nint(1)\n");
}
