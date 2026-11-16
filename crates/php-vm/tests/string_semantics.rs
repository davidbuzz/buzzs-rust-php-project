// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use php_runtime::{Value, ZStr};
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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn literals_and_folded_initializers_are_interned() {
    assert_eq!(
        run(r#"<?php
$s = 'literal';
debug_zval_dump($s, 'fold' . 'ed', ["nested", ["deep"]]);
const C = 'constant';
class A { public $s = 'property'; }
function f($s = 'default') { debug_zval_dump($s); }
debug_zval_dump(C, (new A)->s);
f();
$s[0] = 'L';
echo $s, "\n";
debug_zval_dump('literal');
"#),
        "string(7) \"literal\" interned\n\
         string(6) \"folded\" interned\n\
         array(2) packed refcount(2){\n  [0]=>\n  string(6) \"nested\" refcount(1)\n  [1]=>\n  array(1) packed refcount(1){\n    [0]=>\n    string(4) \"deep\" refcount(1)\n  }\n}\n\
         string(8) \"constant\" interned\nstring(8) \"property\" interned\n\
         string(7) \"default\" interned\nLiteral\nstring(7) \"literal\" interned\n"
    );
}

#[test]
fn bare_string_adapters_share_unchanged_storage() {
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    for input in [
        ZStr::new(b"binary\0\xff".to_vec()),
        ZStr::new(b"literal".to_vec()),
        ZStr::intern(b"literal"),
    ] {
        let repeated = vm
            .call_function("str_repeat", vec![Value::Str(input.clone()), Value::Int(1)])
            .expect("str_repeat");
        let Value::Str(repeated) = repeated else {
            panic!("string result");
        };
        assert!(ZStr::ptr_eq(&input, &repeated));
        assert_eq!(input.is_interned(), repeated.is_interned());
        let escaped = vm
            .call_function(
                "htmlspecialchars",
                vec![
                    Value::Str(input.clone()),
                    Value::Int(0),
                    Value::Str(ZStr::from("ISO-8859-1")),
                ],
            )
            .expect("htmlspecialchars");
        let Value::Str(escaped) = escaped else {
            panic!("string result");
        };
        assert!(ZStr::ptr_eq(&input, &escaped));
    }
}

#[test]
fn repeat_empty_and_zero_results_are_interned_but_negative_counts_fail() {
    assert_eq!(
        run(r#"<?php
debug_zval_dump(str_repeat('abc', 1), str_repeat('abc', 0), str_repeat('', 2));
try { str_repeat('', -1); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
"#),
        "string(3) \"abc\" interned\nstring(0) \"\" interned\nstring(0) \"\" interned\n\
         str_repeat(): Argument #2 ($times) must be greater than or equal to 0\n"
    );
}

/// Empty and one-byte string literals inside a compile-time array are the
/// interned `ZSTR_EMPTY_ALLOC()`/`ZSTR_CHAR()` strings; longer ones are not
/// (php-src `ext/standard/tests/general_functions/debug_zval_dump_b.phpt`).
#[test]
fn folded_array_short_string_literals_are_interned() {
    let out = run("<?php $a = [[\"\"], ['abc', 'b']]; debug_zval_dump($a);");
    assert!(out.contains("string(0) \"\" interned\n"), "{out}");
    assert!(out.contains("string(1) \"b\" interned\n"), "{out}");
    assert!(out.contains("string(3) \"abc\" refcount(1)\n"), "{out}");
}

/// `zend_assign_to_string_offset`: a non-string value is converted with
/// `zval_try_get_string_func` after the offset checks; `__toString()` may
/// throw (leaving the string unchanged), and only its first byte is
/// written (`Zend/tests/exceptions/exception_from_toString.phpt`).
#[test]
fn string_offset_assignment_converts_objects_and_arrays() {
    assert_eq!(
        run(r#"<?php
class Bad { function __toString(): string { throw new Exception("bad"); } }
class Good { function __construct(public string $v) {} function __toString(): string { return $this->v; } }
$s = "abc";
try { $s[0] = new Bad; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($s[1] = new Good("zq"));
$s[2] = new Good("y");
try { $s[0] = new Good(""); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $s[0] = new stdClass; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$s[4] = [1];
echo $s, "\n";
$o = new stdClass; $o->p = "xyz";
$o->p[1] = new Good("Q");
$r = &$o->p; $r[2] = new Good("R");
echo $o->p, "\n";
"#),
        "bad\n\
         \nWarning: Only the first byte will be assigned to the string offset in t.php on line 6\n\
         string(1) \"z\"\n\
         Cannot assign an empty string to a string offset\n\
         Object of class stdClass could not be converted to string\n\
         \nWarning: Array to string conversion in t.php on line 10\n\
         \nWarning: Only the first byte will be assigned to the string offset in t.php on line 10\n\
         azy A\n\
         xQR\n"
    );
}

/// A constant array operand of `.` is cast to string before the
/// concatenation runs (`zend_compile_binary_op` emits `ZEND_CAST`), so its
/// warning precedes the other operand's conversion; a variable array is
/// converted in operand order (php-src `Zend/tests/operator_unsupported_types.phpt`).
#[test]
fn concat_casts_constant_array_operands_first() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function ($no, $str) { echo "W: $str\n"; });
class T { function __toString(): string { echo "toString\n"; return "t"; } }
try { $r = new stdClass . []; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$a = [];
try { $r = new stdClass . $a; } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo new T . [], "\n";
echo [] . new T, "\n";
"#),
        "W: Array to string conversion\n\
         Object of class stdClass could not be converted to string\n\
         Object of class stdClass could not be converted to string\n\
         W: Array to string conversion\ntoString\ntArray\n\
         W: Array to string conversion\ntoString\nArrayt\n"
    );
}

/// `unserialize()` builds non-empty arrays with `array_finit_size_mixed`
/// (never packed) and `s:` values with `zend_string_finit_fast` (empty and
/// one-byte strings interned), as in ext/mysqli/tests/mysqli_result_references.
#[test]
fn unserialize_arrays_are_mixed_and_short_strings_interned() {
    let out = run(
        r#"<?php debug_zval_dump(unserialize('a:3:{i:0;s:1:"z";i:1;s:0:"";i:2;a:1:{i:0;s:2:"ab";}}'));"#,
    );
    assert!(!out.contains("packed"), "{out}");
    assert!(out.contains("string(1) \"z\" interned\n"), "{out}");
    assert!(out.contains("string(0) \"\" interned\n"), "{out}");
    assert!(out.contains("string(2) \"ab\" refcount("), "{out}");
}

/// `ZEND_CONCAT` of two strings: an empty operand that is not a literal
/// yields the other operand itself (an interned string stays interned),
/// while an empty literal does not short-cut, so `$row['label'] . ''`
/// makes a new string (ext/mysqli/tests/mysqli_result_references.phpt).
/// Expected output from php 8.3.
#[test]
fn concat_with_empty_operand_shares_only_for_non_literals() {
    assert_eq!(
        run(r#"<?php
$r = ['l' => 'a', 'm' => 'abc'];
$x = $r['l'] . ''; debug_zval_dump($x);
$x = '' . $r['l']; debug_zval_dump($x);
$x = $r['m'] . ''; debug_zval_dump($x);
$e = ''; $x = $r['l'] . $e; debug_zval_dump($x);
$x = $e . $r['l']; debug_zval_dump($x);
$c = 'a'; $x = $c . ''; debug_zval_dump($x);
$x = '' . $c; debug_zval_dump($x);
$x = $c . $e; debug_zval_dump($x);
"#),
        "string(1) \"a\" refcount(2)\nstring(1) \"a\" refcount(2)\n\
         string(3) \"abc\" refcount(2)\nstring(1) \"a\" interned\n\
         string(1) \"a\" interned\nstring(1) \"a\" refcount(2)\n\
         string(1) \"a\" refcount(2)\nstring(1) \"a\" interned\n"
    );
}
