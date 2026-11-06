//! Object handles around `...$it` argument unpacking, `[...$it]` array
//! spreading and the bare VM's `iterator_to_array()`: php-src's
//! `ZEND_SEND_UNPACK` / `ZEND_ADD_ARRAY_UNPACK` / `spl_iterator_apply` drive
//! a `zend_object_iterator`, which holds an object handle of its own
//! (`zend_iterator_init`) from `get_iterator` to `zend_iterator_dtor`, so
//! objects created while it lives take the next handles. Expected outputs
//! from `/usr/bin/php -n`.
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

fn std_object(handle: u32) -> String {
    format!("object(stdClass)#{handle} (0) {{\n}}\n")
}

fn pair(indent: &str, first: u32, second: u32) -> String {
    format!(
        "array(2) {{\n{indent}[0]=>\n{indent}object(stdClass)#{first} (0) {{\n{indent}}}\n{indent}[1]=>\n{indent}object(stdClass)#{second} (0) {{\n{indent}}}\n}}\n"
    )
}

/// A generator unpacked as arguments or spread into an array: the
/// generator object, then the iterator's handle, then the yielded objects;
/// the iterator's handle is freed before the generator operand
/// (`zend_iterator_dtor`, then `FREE_OP1`).
#[test]
fn generator_unpacking_takes_an_iterator_handle() {
    let src = r#"<?php
function g() { yield new stdClass; yield new stdClass; }
function f(...$a) { var_dump($a); }
f(...g());
$gen = g();
f(...$gen);
unset($gen);
var_dump([...g()]);
var_dump(new stdClass);
"#;
    let expected = format!(
        "{}{}{}{}",
        pair("  ", 3, 4),
        pair("  ", 1, 2),
        pair("  ", 1, 3),
        std_object(3)
    );
    assert_eq!(run(src), expected);
}

/// User iterators: the handle comes after `getIterator()` ran (its objects
/// take theirs first) and before `rewind`; the iterator object returned by
/// `getIterator()` is released before the handle.
#[test]
fn user_iterator_unpacking_takes_an_iterator_handle() {
    let src = r#"<?php
class It implements Iterator {
    private $i = 0;
    public function __construct(private array $a) {}
    public function current(): mixed { return $this->a[$this->i]; }
    public function key(): mixed { return $this->i; }
    public function next(): void { $this->i++; }
    public function rewind(): void { $this->i = 0; }
    public function valid(): bool { return isset($this->a[$this->i]); }
}
class Agg implements IteratorAggregate {
    public function getIterator(): Iterator { return new It([new stdClass, new stdClass]); }
}
class It2 extends It { public function current(): mixed { return new stdClass; } }
function f(...$a) { var_dump($a); }
f(...new It([1, 2]));
var_dump(new stdClass);
var_dump([...new It2([1, 2])]);
var_dump(new stdClass);
f(...new Agg);
var_dump(new stdClass);
"#;
    let expected = format!(
        "array(2) {{\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n}}\n{}{}{}{}{}",
        std_object(1),
        pair("  ", 3, 4),
        std_object(4),
        pair("  ", 1, 2),
        std_object(2)
    );
    assert_eq!(run(src), expected);
}

/// The bare VM's `iterator_to_array()` / `iterator_count()` (php-src:
/// `spl_iterator_apply`) hold the handle the same way, and the iterated
/// object's destructor runs when the unpacking releases it.
#[test]
fn iterator_to_array_and_destructors_follow_the_handle_order() {
    let src = r#"<?php
function g() { yield new stdClass; }
var_dump(iterator_to_array(g()));
var_dump(iterator_count(g()));
var_dump(new stdClass);
class D { public function __construct(public $n) {} public function __destruct() { echo "destruct {$this->n}\n"; } }
function g2() { echo "g2 start\n"; yield new D('a'); echo "g2 end\n"; }
function h(...$a) { echo "h called\n"; }
h(...g2());
echo "after h\n";
var_dump(new stdClass);
"#;
    let expected = format!(
        "array(1) {{\n  [0]=>\n  object(stdClass)#3 (0) {{\n  }}\n}}\nint(1)\n{}g2 start\ng2 end\nh called\ndestruct a\nafter h\n{}",
        std_object(3),
        std_object(1)
    );
    assert_eq!(run(src), expected);
}

/// A closed generator is reported before the by-reference check
/// (`zend_generator_get_iterator`).
#[test]
fn closed_generator_is_reported_before_the_by_reference_check() {
    let src = r#"<?php
function g() { yield 1; }
$g = g();
foreach ($g as $v) {}
try { foreach ($g as &$v) {} } catch (Exception $e) { echo $e::class, ": ", $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "Exception: Cannot traverse an already closed generator\n"
    );
}
