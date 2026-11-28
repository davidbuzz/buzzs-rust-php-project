// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Object identity through the object store and the property count of a
//! table whose declared slot is being unset (`Zend/tests/unset`).
//! Expected outputs are `/usr/bin/php8.5 -n` (the same as master here).
mod common;

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

/// Runs `source` as `t.php` (including shutdown) and returns its output.
/// [`run`] with the standard library ([`common::vm_with_stdlib`]).
fn run_std(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `zend_objects_store_call_destructors` passes the object itself: `$this`
/// in a shutdown destructor is identical to the other references.
#[test]
fn shutdown_destructor_this_is_the_same_object() {
    let out = run(r#"<?php
class A {
    public $self;
    function __destruct() { var_dump($this === $this->self, $this === $GLOBALS['a']); }
}
$a = new A;
$a->self = $a;
"#);
    assert_eq!(out, "bool(true)\nbool(true)\n");
}

/// `unset_prop_recursion`: the shutdown destructor's `var_dump($this)`
/// detects the recursion through the children's `parent`, and the
/// children's destructors, run inside `unset($this->children)`, still see
/// the table's old count of 2.
#[test]
fn unset_prop_recursion() {
    let out = run(r#"<?php
class Node {
    public $parent = null;
    public $children = [];
    function insert(Node $node) {
        $node->parent = $this;
        $this->children[] = $node;
    }
    function __destruct() {
        var_dump($this);
        unset($this->children);
    }
}

$a = new Node;
$a->insert(new Node);
$a->insert(new Node);
"#);
    let child = |n: u32| {
        format!(
            "object(Node)#{n} (2) {{\n  [\"parent\"]=>\n  object(Node)#1 (2) {{\n    [\"parent\"]=>\n    NULL\n  }}\n  [\"children\"]=>\n  array(0) {{\n  }}\n}}\n"
        )
    };
    let nested = |n: u32| {
        format!(
            "    [{i}]=>\n    object(Node)#{n} (2) {{\n      [\"parent\"]=>\n      *RECURSION*\n      [\"children\"]=>\n      array(0) {{\n      }}\n    }}\n",
            i = n - 2
        )
    };
    let expected = format!(
        "object(Node)#1 (2) {{\n  [\"parent\"]=>\n  NULL\n  [\"children\"]=>\n  array(2) {{\n{}{}  }}\n}}\n{}{}",
        nested(2),
        nested(3),
        child(2),
        child(3)
    );
    assert_eq!(out, expected);
}

/// The stale count needs a materialized property table without empty
/// slots (`var_dump`/`get_object_vars` build it, a dynamic property too);
/// after the unset the count is exact again.
#[test]
fn unset_counts_old_slot_only_while_its_release_runs() {
    let out = run(r#"<?php
class Y { public $x; function __destruct() { var_dump($this->x); } }
class X { public $a = 1; public $b; }
$x = new X; $y = new Y; $y->x = $x; $x->b = $y; unset($y);
unset($x->b);
echo "--\n";
$x = new X; $y = new Y; $y->x = $x; $x->b = $y; unset($y);
get_object_vars($x);
unset($x->b);
var_dump($x);
echo "--\n";
class P { public $a; public $b; }
$p = new P; unset($p->a); $y = new Y; $y->x = $p; $p->b = $y; unset($y);
get_object_vars($p);
unset($p->b);
"#);
    assert_eq!(
        out,
        "object(X)#1 (1) {\n  [\"a\"]=>\n  int(1)\n}\n--\n\
         object(X)#2 (2) {\n  [\"a\"]=>\n  int(1)\n}\n\
         object(X)#2 (1) {\n  [\"a\"]=>\n  int(1)\n}\n--\n\
         object(P)#1 (0) {\n}\n"
    );
}

/// `clone` of an object without references among its properties copies
/// them straight into the new object (`object::new_object_from_defaults`);
/// one with a reference takes the general copy (a shared reference stays
/// shared, one the object alone held is dereferenced). `__clone()`,
/// dynamic and private properties, readonly properties, destructors of
/// nested objects, an internal class's clone handler. Expected output:
/// php-src's (PHP 8.5).
#[test]
fn clones_copy_properties_as_php_src() {
    assert_eq!(
        run_std(
            r#"<?php
class M { public $a = 1, $b = [1, 2], $c = 'x'; protected $p = null; private $q = ['k' => 'v'];
  public ?M $child = null; public readonly int $ro; function __construct() { $this->ro = 5; }
  function __clone() { $this->c .= '-c'; } function q() { return $this->q; } }
class D { public $o; function __destruct() { echo "~D "; } }
$m = new M; $m->dyn = 'dyn'; $m->b[] = 3; $m->child = new M; $r = &$m->a; $r = 9;
$c = clone $m; $c->b[] = 4; $c->a = 10;
echo json_encode([$m->a, $c->a, $m->b, $c->b, $c->c, $m->c, $c->dyn, $c->q(), $c->child === $m->child, $c->ro]), "\n";
unset($r); $c2 = clone $m; $c2->a = 11; echo $m->a, " ", $c2->a, "\n";
$d = new D; $d->o = new D; $e = clone $d; unset($d); echo "| "; unset($e); echo "\n";
for ($i = 0; $i < 3; $i++) { $x = clone $m; } echo $x->c, " ", count((array)$x), "\n";
$s = new SplStack; $s->push(1); $s2 = clone $s; $s2->push(2); echo count($s), count($s2), "\n";
try { $c->ro = 1; } catch (Error $err) { echo $err->getMessage(), "\n"; }
"#
        ),
        r#"
Deprecated: Creation of dynamic property M::$dyn is deprecated in t.php on line 6
[10,10,[1,2,3],[1,2,3,4],"x-c","x","dyn",{"k":"v"},true,5]
11 11
~D | ~D ~D 
x-c 8
12
Cannot modify readonly property M::$ro
"#
    );
}
