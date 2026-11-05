//! Variable access semantics: compound assignments, string offsets,
//! quiet fetches, unset, dynamic calls (`Zend/tests/{offsets,isset,unset,
//! coalesce,dynamic_call,varSyntax,in-de-crement}`). Expected outputs come
//! from the C implementation.

#![allow(clippy::needless_raw_string_hashes)]

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

/// `ZEND_ASSIGN_DIM_OP` converts each offset once; on `ArrayAccess` it is
/// `offsetGet` + `offsetSet`, and a nested one notices the temporary.
#[test]
fn compound_assignments_convert_offsets_once_and_run_handlers_once() {
    assert_eq!(
        run(r#"<?php
$a = [1 => 1];
$a[1.5] += 2;
$b = [[0 => 'a']];
$b[0.5][0.5] .= "x";
var_dump($a, $b);
class AA implements ArrayAccess {
    function offsetGet($k): mixed { echo "get $k\n"; return 5; }
    function offsetSet($k, $v): void { echo "set $k $v\n"; }
    function offsetExists($k): bool { echo "exists $k\n"; return true; }
    function offsetUnset($k): void {}
}
$o = new AA;
var_dump($o['k'] += 1);
try { $o['k']['j'] += 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$s = null;
try { $s->p += 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "\nDeprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 3\n\nDeprecated: Implicit conversion from float 0.5 to int loses precision in t.php on line 5\n\nDeprecated: Implicit conversion from float 0.5 to int loses precision in t.php on line 5\narray(1) {\n  [1]=>\n  int(3)\n}\narray(1) {\n  [0]=>\n  array(1) {\n    [0]=>\n    string(2) \"ax\"\n  }\n}\nget k\nset k 6\nint(6)\nget k\n\nNotice: Indirect modification of overloaded element of AA has no effect in t.php on line 15\nCannot use a scalar value as an array\nAttempt to assign property \"p\" on null\n"
    );
}

/// `zend_check_string_offset` then `zend_wrong_string_offset_error`; quiet
/// reads keep the leading-numeric warning and illegal-offset errors, and
/// only the final isset/empty says "in isset or empty".
#[test]
fn string_containers_check_offsets_before_context_errors() {
    assert_eq!(
        run(r#"<?php
$s = 'abc';
try { $s[1.5] .= 'x'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $s[0]++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $s[][0] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($s['1x'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($s['x'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $s[0]->p = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $r = &$s['1x']; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump($s['1x'] ?? 'd', $s['x'] ?? 'd', isset($s[1.5]), isset($s['1x']));
try { var_dump($s[[]] ?? 'd'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$a = [];
try { var_dump($a[[]] ?? 'd'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(isset($a[[]])); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(isset($a[0][[]])); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$a = [[1]];
try { unset($a[[]][0]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { unset($a[0][[]]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#),
        "\nWarning: String offset cast occurred in t.php on line 3\nCannot use assign-op operators with string offsets\nCannot increment/decrement string offsets\n[] operator not supported for strings\nCannot use string offset as an array\nCannot unset string offsets\nCannot use string offset as an object\n\nWarning: Illegal string offset \"1x\" in t.php on line 9\nCannot create references to/from string offsets\n\nWarning: Illegal string offset \"1x\" in t.php on line 10\n\nDeprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 10\nstring(1) \"b\"\nstring(1) \"d\"\nbool(true)\nbool(false)\nCannot access offset of type array on string\nCannot access offset of type array on array\nCannot access offset of type array in isset or empty\nbool(false)\nCannot access offset of type array on array\nCannot unset offset of type array on array\n"
    );
}

/// `UNSET_DIM/FETCH_DIM_UNSET` warn on undefined CVs and deprecate `false`;
/// `$this` fetches throw outside objects; offsets are read when fetched.
#[test]
fn unset_and_isset_follow_fetch_order_and_this_checks() {
    assert_eq!(
        run(r#"<?php
unset($u[0]);
unset($u->p);
$f = false;
unset($f[0][1]);
var_dump($f);
try { var_dump(isset($this->p)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { var_dump($this[0] ?? 1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(isset($c[$d]));
var_dump($c[$d]);
"#),
        "\nWarning: Undefined variable $u in t.php on line 2\n\nDeprecated: Automatic conversion of false to array is deprecated in t.php on line 5\nbool(false)\nUsing $this when not in object context\nUsing $this when not in object context\n\nWarning: Undefined variable $d in t.php on line 9\nbool(false)\n\nWarning: Undefined variable $c in t.php on line 10\n\nWarning: Undefined variable $d in t.php on line 10\n\nWarning: Trying to access array offset on null in t.php on line 10\nNULL\n"
    );
}

/// `ZEND_CALL_DYNAMIC` (`zend_forbid_dynamic_call`), `zend_init_dynamic_call_*`
/// without `$this`, literal callees, and the class fetched before the
/// method name.
#[test]
fn dynamic_calls_are_flagged_and_never_forward_this() {
    assert_eq!(
        run(r#"<?php
namespace Foo;
function t() {
    foreach (['func_get_args', 'func_num_args', 'get_defined_vars'] as $f) {
        try { $f(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
    }
    try { ((string) 'compact')('x'); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
}
t();
class C {
    function m() {
        foreach ([['Foo\C', 'nope'], 'Foo\C::nope', ['Foo\C', 'inst']] as $cb) {
            try { $cb(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
        }
    }
    function inst() { echo "inst\n"; }
    function __call($n, $a) { echo "__call $n\n"; }
}
(new C)->m();
echo ('\strlen')('abc'), "\n";
try { ('\nope')(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
try { $cb = [1 => 0, 2 => 0]; $cb(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
try { $a::$b(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "Cannot call func_get_args() dynamically\nCannot call func_num_args() dynamically\nCannot call get_defined_vars() dynamically\nCannot call compact() dynamically\nNon-static method Foo\\C::nope() cannot be called statically\nNon-static method Foo\\C::nope() cannot be called statically\nNon-static method Foo\\C::inst() cannot be called statically\n3\nCall to undefined function nope()\nArray callback has to contain indices 0 and 1\n\nWarning: Undefined variable $a in t.php on line 23\nClass name must be a valid object or a string\n"
    );
}

/// `++$this` fails at run time, `??=` frees its memoized offsets on the
/// short-circuit path, and a temporary's dimension cannot go by reference.
#[test]
fn incdec_this_coalesce_temporaries_and_temporary_by_ref() {
    assert_eq!(
        run(r#"<?php
class P {
    function inc() { try { ++$this; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } }
}
(new P)->inc();
class D { function __destruct() { echo "dtor\n"; } }
class AA implements ArrayAccess {
    function offsetGet($k): mixed { return 1; }
    function offsetSet($k, $v): void {}
    function offsetExists($k): bool { return true; }
    function offsetUnset($k): void {}
}
$o = new AA;
$o[new D] ??= 2;
echo "after\n";
$fn = function (&$r) {};
try { $fn([0, 1][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$fn2 = function ($r) { var_dump($r); };
$fn2([0, 1][1]);
"#),
        "Cannot increment P\ndtor\nafter\nCannot use temporary expression in write context\nint(1)\n"
    );
}

/// `zend_separate_if_call_and_write`: `strlen()` compiles to its own opcode.
#[test]
fn writing_through_special_builtin_result_is_a_compile_error() {
    assert_eq!(
        run(r#"<?php
strlen("foo")[0] ??= 123;
"#),
        "\nFatal error: Cannot use result of built-in function in write context in t.php on line 2\nStack trace:\n#0 {main}\n"
    );
}

/// `ZEND_FETCH_GLOBALS` copies the symbol table with
/// `zend_proptable_to_symtable(.., 1)`: a reference held only by the table
/// is copied as a value, a shared one stays shared, numeric names become
/// integer keys, and unset or never-assigned variables are absent.
#[test]
fn globals_read_copies_the_symbol_table_like_zend_array_dup() {
    assert_eq!(
        run(r#"<?php
$a = 1; $r = &$a; unset($r); $g = $GLOBALS; $a = 2; var_dump($g['a']);
$b = 1; $rb = &$b; $g = $GLOBALS; $b = 2; var_dump($g['b']);
${'1'} = 5; var_dump(array_key_last($GLOBALS)); var_dump(array_key_last(get_defined_vars()));
unset($b); var_dump(array_key_exists('b', $GLOBALS), array_key_exists('later', $GLOBALS));
$later = 1;
"#),
        "int(1)\nint(2)\nint(1)\nstring(1) \"1\"\nbool(false)\nbool(false)\n"
    );
}

/// `zend_delayed_compile_dim`: `$GLOBALS[]` in any write context is a
/// compile error (`restrict_globals/invalid_append`).
#[test]
fn appending_to_globals_is_a_compile_error() {
    for stmt in [
        "$GLOBALS[] = 1;",
        "$GLOBALS[][1] = 2;",
        "$GLOBALS[] .= 1;",
        "$GLOBALS[]++;",
        "$x = &$GLOBALS[];",
        "foreach ([1] as $GLOBALS[]) {}",
    ] {
        assert_eq!(
            run(&format!("<?php\necho \"x\";\n{stmt}\n")),
            "\nFatal error: Cannot append to $GLOBALS in t.php on line 3\n",
            "{stmt}"
        );
    }
}
