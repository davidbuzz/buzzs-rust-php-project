//! Object identity through the object store and the property count of a
//! table whose declared slot is being unset (`Zend/tests/unset`).
//! Expected outputs are `/usr/bin/php -n` (8.3, same as master here).
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
