//! Built-in iterator interfaces, ArrayAccess and foreach regressions
//! (Zend/tests/{iterators,ArrayAccess,foreach}).
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

/// Runs `source` as `t.php`, returning the exit status and the output.
fn run_status(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

/// Runs `source`, asserting a clean exit, and returns its output.
fn run(source: &str) -> String {
    let (status, output) = run_status(source);
    assert_eq!(status, 0, "{output}");
    output
}

/// `zend_implement_aggregate` / `zend_implement_iterator` (bug48667_1/2).
#[test]
fn class_cannot_be_both_iterator_and_aggregate() {
    for ifaces in ["Iterator, IteratorAggregate", "IteratorAggregate, Iterator"] {
        let src = format!("<?php\n\nabstract class A implements {ifaces} {{ }}\n");
        let (status, out) = run_status(&src);
        assert_ne!(status, 0, "{out}");
        assert!(
            out.contains(
                "Fatal error: Class A cannot implement both Iterator and IteratorAggregate at the same time in t.php on line 3"
            ),
            "{out}"
        );
    }
    // Interfaces may extend both.
    let out = run("<?php\ninterface I extends Iterator, IteratorAggregate {}\necho \"ok\";\n");
    assert_eq!(out, "ok");
}

/// `get_object_vars()` applies `zend_check_property_access()` to dynamic
/// properties too (foreach_shadowed_dyn_property).
#[test]
fn get_object_vars_hides_dynamic_property_shadowed_by_private() {
    let out = run(r#"<?php
class Test {
    private $prop = "Test";
    function run() {
        foreach ($this as $k => $v) { echo "$k => $v\n"; }
        foreach (get_object_vars($this) as $k => $v) { echo "$k: $v\n"; }
    }
}
#[AllowDynamicProperties]
class Test2 extends Test {}
$t = new Test2;
$t->prop = "Test2";
$t->run();
foreach (get_object_vars($t) as $k => $v) { echo "outside $k: $v\n"; }
"#);
    assert_eq!(out, "prop => Test\nprop: Test\noutside prop: Test2\n");
}

/// `zend_fe_fetch_object_helper` unmangles NUL-prefixed dynamic keys and
/// reads their buckets directly (foreach_018).
#[test]
fn foreach_unmangles_nul_prefixed_dynamic_keys() {
    let out = run(r#"<?php
$obj = (object)["\0A\0b" => 42, "\0*\0c" => 24, "d" => 1];
foreach ($obj as $k => $v) { echo strlen($k), ":$k=$v\n"; }
foreach ($obj as $k => &$v) { $v *= 2; }
unset($v);
foreach ($obj as $k => $v) { echo "$k=$v\n"; }
"#);
    assert_eq!(out, "1:b=42\n1:c=24\n1:d=1\nb=84\nc=48\nd=2\n");
}

/// A by-reference arrow function over a non-variable returns by value with
/// the ZEND_RETURN_BY_REF notice (oss_fuzz_58181).
#[test]
fn by_ref_arrow_fn_on_value_notices() {
    let out = run("<?php\n$f = fn&() => true;\n$x = $f();\nvar_dump($x);\n");
    assert_eq!(
        out,
        "\nNotice: Only variable references should be returned by reference in t.php on line 2\nbool(true)\n"
    );
    let out = run("<?php\n$a = 1;\n$f = fn&() => $a;\n$x = $f();\nvar_dump($x);\n");
    assert_eq!(out, "int(1)\n");
}

/// `zend_check_string_offset()` runs before the write-context string
/// offset error (bug73792).
#[test]
fn string_offset_check_precedes_reference_error() {
    let out = run(r#"<?php
$a = 'aaa';
try { foreach ($a['2bbb'] as &$v) {} } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { $a['1x'] .= 'b'; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { $r = &$a['xx']; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "\nWarning: Illegal string offset \"2bbb\" in t.php on line 3\n\
         Error: Cannot create references to/from string offsets\n\
         \nWarning: Illegal string offset \"1x\" in t.php on line 4\n\
         Error: Cannot use assign-op operators with string offsets\n\
         TypeError: Cannot access offset of type string on string\n"
    );
}
