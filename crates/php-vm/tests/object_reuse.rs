// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$v = new C(...)` over the last handle of a plain object of `C`: the
//! object is made in the old one's block (`object::reuse_check`), and a
//! script sees what making an object and freeing the old one gives.
//! Expected outputs are `/usr/bin/php -n` (8.3, same as master here). The
//! request allocator is installed: the reuse asks it whether a block taken
//! and freed would leave nothing behind.

use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

php_memlimit::install_global_allocator!();

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

/// One run at a time: the allocator's counts are the process's.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `source` as `t.php` (including shutdown); its output and how many
/// objects were made in a block at hand meanwhile.
fn run(source: &str) -> (String, u64) {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = php_vm::object::reuse_count();
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (
        String::from_utf8(output).expect("UTF-8 output"),
        php_vm::object::reuse_count() - before,
    )
}

/// The handle numbers alternate as the free list hands them out (the new
/// object takes its handle before the old one's goes back), with and
/// without a constructor, and the slots are a new object's.
#[test]
fn handles_alternate_and_slots_are_new() {
    let (out, reused) = run(r#"<?php
class P { public $a = 1; public $b = 2.5; }
class Q { public $x; function __construct($x) { $this->x = $x; } }
function plain($n) { $ids = []; for ($i = 0; $i < $n; $i++) { $o = new P; $ids[] = spl_object_id($o) . ':' . $o->a . ':' . $o->b; $o->a = $i + 5; $o->b = null; } return implode(',', $ids); }
function ctor($n) { $ids = []; for ($i = 0; $i < $n; $i++) { $o = new Q($i); $ids[] = spl_object_id($o) . ':' . $o->x; } return implode(',', $ids); }
echo plain(8), "\n", ctor(8), "\n";
"#);
    assert_eq!(
        out,
        "1:1:2.5,2:1:2.5,1:1:2.5,2:1:2.5,1:1:2.5,2:1:2.5,1:1:2.5,2:1:2.5\n2:0,1:1,2:2,1:3,2:4,1:5,2:6,1:7\n"
    );
    // The first object of each loop is made the plain way (nothing to
    // make it in), and so is the one whose `new` finds out what its
    // constructor does; the others in the block at hand.
    assert!(reused >= 12, "{reused} objects made in a block at hand");
}

/// What keeps the old object from being the new one's block: another
/// handle, a weak reference, a destructor, a dynamic property, an unset
/// slot, a slot holding something to release, an argument read from the
/// variable itself, another class.
#[test]
fn objects_that_are_not_reused() {
    let (out, reused) = run(r#"<?php
class P { public $a = 1; }
class D { function __destruct() { echo "d"; } }
#[AllowDynamicProperties] class Dyn { public $a = 0; }
class Q { public $x; function __construct($x) { $this->x = $x; } }
class Sub extends P {}
function f($n) {
    $keep = [];
    for ($i = 0; $i < $n; $i++) { $o = new P; $keep[] = $o; }
    for ($i = 0; $i < $n; $i++) { $d = new D; }
    for ($i = 0; $i < $n; $i++) { $y = new Dyn; $y->extra = 1; }
    for ($i = 0; $i < $n; $i++) { $u = new P; unset($u->a); }
    for ($i = 0; $i < $n; $i++) { $s = new Q("s$i"); }
    $q = new Q(0);
    for ($i = 0; $i < $n; $i++) { $q = new Q($q); }
    for ($i = 0; $i < $n; $i++) { $m = ($i % 2) ? new Sub : new P; }
    // (Last: once a weak reference exists, no object is made that way.)
    for ($i = 0; $i < $n; $i++) { $v = new P; $w = WeakReference::create($v); }
    for ($i = 0; $i < $n; $i++) { $z = new P; }
    echo "|", count($keep), isset($u->a) ? 'set' : 'unset', $s->x;
}
f(6);
"#);
    assert_eq!(out, "ddddd|6unsets5d");
    assert_eq!(reused, 0);
}

/// The possible roots: an object a constructor ran for is one, in the
/// buffer once however often the variable is assigned; the count follows
/// an object another variable keeps.
#[test]
fn possible_roots_are_php_srcs() {
    let (out, reused) = run(r#"<?php
class Q { public $x; function __construct($x) { $this->x = $x; } }
function f($n) {
    gc_collect_cycles();
    $before = gc_status()['roots'];
    $seen = [];
    for ($i = 0; $i < $n; $i++) { $o = new Q($i); $seen[] = gc_status()['roots'] - $before; if ($i == 3) { $keep = $o; } }
    unset($keep);
    $seen[] = gc_status()['roots'] - $before;
    return implode(',', $seen);
}
echo f(8);
"#);
    assert_eq!(out, "1,1,1,1,2,2,2,2,1");
    assert!(reused >= 4, "{reused} objects made in a block at hand");
}

/// Memory use and its peak do not move once the loop is under way.
#[test]
fn memory_use_and_peak_stay() {
    let (out, reused) = run(r#"<?php
class P { public $a = 1; }
function f($n) {
    $o = new P; $o = new P; $o = new P;
    $m = memory_get_usage(); $pk = memory_get_peak_usage();
    for ($i = 0; $i < $n; $i++) { $o = new P; }
    return (memory_get_usage() - $m) . ',' . (memory_get_peak_usage() - $pk);
}
echo f(50);
"#);
    assert_eq!(out, "0,0");
    assert!(reused >= 40, "{reused} objects made in a block at hand");
}
