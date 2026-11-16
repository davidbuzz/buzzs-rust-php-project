// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Built-in iterator interfaces, `ArrayAccess` and foreach regressions
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

/// `zend_implement_aggregate` / `zend_implement_iterator` (`bug48667_1/2`).
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
/// properties too (`foreach_shadowed_dyn_property`).
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
/// reads their buckets directly (`foreach_018`).
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
/// the `ZEND_RETURN_BY_REF` notice (`oss_fuzz_58181`).
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

/// `list()` over a string warns like any other scalar (`foreach_list_002`).
#[test]
fn list_destructuring_of_string_warns() {
    let out = run("<?php\nforeach (['c'] as list(, $a)) {\n    var_dump($a);\n}\n");
    assert_eq!(
        out,
        "\nWarning: Cannot use string as array in t.php on line 2\nNULL\n"
    );
}

/// `$o[] op= v` on an `ArrayAccess` object reads `offsetGet(null)` and
/// writes `offsetSet(null, ..)` (bug69955); a by-reference fetch whose
/// `offsetGet()` returns an object raises no notice (bug39297).
#[test]
fn array_access_append_assign_op_and_object_ref_fetch() {
    let out = run(r#"<?php
class C implements ArrayAccess {
    public $o;
    function offsetExists($k): bool { return true; }
    function offsetGet($k): mixed { echo "get "; var_dump($k); return $k === 'obj' ? $this->o : 100; }
    function offsetSet($k, $v): void { echo "set "; var_dump($k, $v); }
    function offsetUnset($k): void {}
}
function byRef(&$x) { return $x; }
$c = new C;
$c->o = new stdClass;
var_dump($c[] += 5);
var_dump(byRef($c['obj']) === $c->o);
"#);
    assert_eq!(
        out,
        "get NULL\nset NULL\nint(105)\nint(105)\nget string(3) \"obj\"\nbool(true)\n"
    );
}

/// `ZEND_YIELD` in a by-reference generator: constants, temporaries and
/// non-reference call results notice; variables and by-ref call results
/// do not (`yield_const_by_ref_error`, `yield_by_reference_optimization`,
/// `yield_non_ref_function_call_by_ref_error`).
#[test]
fn by_ref_yield_of_non_variable_notices() {
    let notice = |line: u32| {
        format!(
            "\nNotice: Only variable references should be yielded by reference in t.php on line {line}\n"
        )
    };
    let out = run("<?php\nfunction &gen() {\n    yield \"foo\";\n}\n$gen = gen();\nvar_dump($gen->current());\n");
    assert_eq!(out, format!("{}string(3) \"foo\"\n", notice(3)));
    let out = run(
        "<?php\nfunction &gen() {\n    yield $v = 0;\n    yield $v = 1;\n}\nforeach (gen() as $v) {\n    var_dump($v);\n}\n",
    );
    assert_eq!(out, format!("{}int(0)\n{}int(1)\n", notice(3), notice(4)));
    let out = run(
        "<?php\nfunction foo() { return \"bar\"; }\nfunction &gen() {\n    yield foo();\n}\n$gen = gen();\nvar_dump($gen->current());\n",
    );
    assert_eq!(out, format!("{}string(3) \"bar\"\n", notice(4)));
    // A by-reference call result, a variable and a bare `yield` are silent.
    let out = run(
        "<?php\nfunction &r() { static $a = 1; return $a; }\nfunction &gen() {\n    $x = 2;\n    yield r();\n    yield $x;\n    yield;\n}\nforeach (gen() as $v) { var_dump($v); }\n",
    );
    assert_eq!(out, "int(1)\nint(2)\nNULL\n");
}

/// `zend_compile_yield_from`: `yield from` is a compile error inside a
/// by-reference generator (`yield_from_by_reference`).
#[test]
fn yield_from_in_by_ref_generator_is_fatal() {
    let (status, out) = run_status("<?php\n\nfunction &gen() {\n    yield from [];\n}\n");
    assert_ne!(status, 0, "{out}");
    assert!(
        out.contains(
            "Fatal error: Cannot use \"yield from\" inside a by-reference generator in t.php on line 4"
        ),
        "{out}"
    );
}

/// `foreach` over generators through the one-access resume and fetch
/// paths: plain yields, keys, `yield from` arrays and generators, return
/// values, `break` with `finally`, exceptions and an aggregate returning a
/// generator (expected output from php 8.3).
#[test]
fn generator_foreach_fast_paths_match_general_resume() {
    let src = r#"<?php
function g() { $x = yield 1; echo "got ", var_export($x, true), "\n"; yield 'k' => 2; yield from [10, 20]; yield from h(); return 5; }
function h() { yield 7; yield 8; return 9; }
$gen = g();
foreach ($gen as $k => $v) { echo "$k=$v\n"; }
echo $gen->getReturn(), "\n";
$g2 = (function () { try { yield 1; yield 2; } finally { echo "fin\n"; } })();
foreach ($g2 as $v) { echo $v, "\n"; break; }
unset($g2);
$g3 = (function () { yield 1; throw new Exception("boom"); })();
try { foreach ($g3 as $v) { echo $v, "\n"; } } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { foreach ($g3 as $v) {} } catch (Exception $e) { echo $e->getMessage(), "\n"; }
$s = 0; foreach ((function () { for ($i = 0; $i < 100; $i++) yield $i; })() as $v) $s += $v; echo $s, "\n";
class A implements IteratorAggregate { function getIterator(): Iterator { yield 'a' => 1; yield 'b' => 2; } }
foreach (new A as $k => $v) echo "$k:$v\n";
"#;
    assert_eq!(
        run(src),
        "0=1\ngot NULL\nk=2\n0=10\n1=20\n0=7\n1=8\n5\n1\nfin\n1\nboom\nCannot traverse an already closed generator\n4950\na:1\nb:2\n"
    );
}
