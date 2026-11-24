// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

/// `++`/`--` on a dimension reports each offset conversion once
/// (`FETCH_DIM_RW`; the write-back is silent), as in Zend/tests/resource_key.
#[test]
fn incdec_on_dimensions_converts_offsets_once() {
    assert_eq!(
        run(r#"<?php
$a = [1 => 1];
$a[1.5]++;
$b = [[0 => 1]];
--$b[0.5][0.5];
var_dump($a, $b);
"#),
        "\nDeprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 3\n\nDeprecated: Implicit conversion from float 0.5 to int loses precision in t.php on line 5\n\nDeprecated: Implicit conversion from float 0.5 to int loses precision in t.php on line 5\narray(1) {\n  [1]=>\n  int(2)\n}\narray(1) {\n  [0]=>\n  array(1) {\n    [0]=>\n    int(0)\n  }\n}\n"
    );
}

/// `zend_undefined_offset_write` / `zend_fetch_var_address_helper`: a
/// throwing "Undefined ..." warning of a read-modify-write leaves the key
/// or global undefined; `$GLOBALS[...]` bases say "global variable"
/// (Zend/tests/undef_index_to_exception).
#[test]
fn throwing_undefined_warnings_of_read_modify_writes_add_nothing() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function ($_, $msg) { throw new Exception($msg); });
$t = [];
try { $t[0] .= "x"; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { $t["k"]++; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($t);
try { $GLOBALS["u"] .= "x"; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { $GLOBALS["u"]++; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump(isset($u));
restore_error_handler();
$GLOBALS["w"] .= "x";
var_dump($w);
"#),
        "Undefined array key 0\nUndefined array key \"k\"\narray(0) {\n}\nUndefined global variable $u\nUndefined global variable $u\nbool(false)\n\nWarning: Undefined global variable $w in t.php on line 11\nstring(1) \"x\"\n"
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
${'1'} = 5; $t = $GLOBALS; foreach ($t as $kk => $vv) {} var_dump($kk);
unset($b); var_dump(isset($GLOBALS['b']), isset($GLOBALS['later']));
$later = 1;
"#),
        "int(1)\nint(2)\nint(1)\nbool(false)\nbool(false)\n"
    );
}

/// The global scope's compiled variables are one zval with their symbol
/// table slots in php-src (`IS_INDIRECT`): a reference that only the
/// variable holds is copied as its value by a `$GLOBALS` read also from
/// inside a function or into an array, while one bound elsewhere (`&`,
/// `global`) stays shared.
#[test]
fn globals_read_copies_a_variable_held_only_by_the_global_scope() {
    assert_eq!(
        run(r#"<?php
$b = 1; $r = &$b; unset($r); $x = $GLOBALS; $b = 2; var_dump($x['b']);
function f() { $h = $GLOBALS; $GLOBALS['b'] = 3; var_dump($h['b']); }
f();
$d = 1; $r = &$d; unset($r); $arr = [$GLOBALS]; $d = 9; var_dump($arr[0]['d']);
$e = 1; $r = &$e; $y = $GLOBALS; $e = 4; var_dump($y['e']);
function k() { global $e; $z = $GLOBALS; $e = 6; var_dump($z['e']); }
k();
"#),
        "int(1)\nint(2)\nint(1)\nint(4)\nint(6)\n"
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
            "\nFatal error: Cannot append to $GLOBALS in t.php on line 3\nStack trace:\n#0 {main}\n",
            "{stmt}"
        );
    }
}

/// `ZEND_ASSIGN_DIM` reads an undefined final offset only at its
/// container: the warning follows the earlier fetches' diagnostics, is not
/// raised for a scalar, and for a string comes from
/// `zend_check_string_offset` (expected output: `/usr/bin/php8.5 -n`, with
/// the null-offset deprecation of 8.5 and master).
#[test]
fn undefined_final_offsets_warn_at_the_container() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function ($no, $str) { echo "W: $str\n"; });
$i = 5;
try { $i[$u] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$f = false;
$f[$u] = 1;
$a = [1];
$a[1.5][$u] = 2;
$s = "abc";
$s[$u] = "xy";
echo $s, "\n";
class AA implements ArrayAccess {
    function offsetGet($o): mixed { return 1; }
    function offsetSet($o, $v): void { echo "set "; var_dump($o); }
    function offsetExists($o): bool { return true; }
    function offsetUnset($o): void {}
}
$o = new AA;
$o[$u] = 1;
var_dump($f, $a[1]);
"#),
        "Cannot use a scalar value as an array\n\
         W: Automatic conversion of false to array is deprecated\n\
         W: Undefined variable $u\n\
         W: Using null as an array offset is deprecated, use an empty string instead\n\
         W: Implicit conversion from float 1.5 to int loses precision\n\
         W: Undefined variable $u\n\
         W: Using null as an array offset is deprecated, use an empty string instead\n\
         W: Undefined variable $u\n\
         W: String offset cast occurred\n\
         W: Only the first byte will be assigned to the string offset\n\
         xbc\n\
         W: Undefined variable $u\n\
         set NULL\n\
         array(1) {\n  [\"\"]=>\n  int(1)\n}\narray(1) {\n  [\"\"]=>\n  int(2)\n}\n"
    );
}

/// An error handler that destroys the string container while its undefined
/// offset is reported abandons the write (`zend_assign_to_string_offset`'s
/// `GC_ADDREF`; php-src `Zend/tests/str_offset_006.phpt`, `str_offset_007.phpt`).
#[test]
fn string_offset_write_is_abandoned_when_the_handler_frees_the_string() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function($code, $msg) {
    echo "Err: $msg\n";
    $GLOBALS['a'] = null;
});
$a[$y] = $a .= ($y);
var_dump($a);
set_error_handler(function($code, $msg) {
    echo "Err: $msg\n";
    $GLOBALS['b'] = '';
});
$b = ['b'];
$b[0][$d] = 'c';
var_dump($b);
$x = ($b[$d] = 'z');
var_dump($x, $b);
"#),
        "Err: Undefined variable $y\nErr: Undefined variable $y\nErr: String offset cast occurred\nNULL\n\
         Err: Undefined variable $d\nErr: String offset cast occurred\nstring(0) \"\"\n\
         Err: Undefined variable $d\nErr: String offset cast occurred\nNULL\nstring(0) \"\"\n"
    );
}

/// `ZEND_ASSIGN_DIM` converts a `false` container to an array before it
/// reads the offset: when the error handler for the "Automatic conversion
/// of false to array" deprecation frees the new array, the write is
/// abandoned and an undefined-variable offset is never read (php-src
/// `Zend/tests/falsetoarray_002.phpt`).
#[test]
fn false_to_array_freed_by_the_handler_skips_the_undefined_offset() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function ($code, $msg) {
    echo "Err: $msg\n";
    $GLOBALS['a'] = '';
});
$a = [!'a'];
$a[0][$d] = 'b';
var_dump($a);
$a = [false];
$r = ($a[0][$d] = 'b');
var_dump($a, $r);
$a = false;
$a[$d] = 'b';
var_dump($a);
"#),
        "Err: Automatic conversion of false to array is deprecated\nstring(0) \"\"\n\
         Err: Automatic conversion of false to array is deprecated\nstring(0) \"\"\nNULL\n\
         Err: Automatic conversion of false to array is deprecated\nstring(0) \"\"\n"
    );
}

/// `$cv[$k] ?? $d` of an offset the array lacks is `null` on the tape
/// (`ZEND_FETCH_DIM_IS`): string, int and integer-string offsets,
/// through a reference, a present `null` element, string offsets and an
/// undefined variable (the generic fetch). Expected output: php-src's
/// (PHP 8.4 CLI).
#[test]
fn coalesce_of_a_missing_element_is_null() {
    let src = r#"<?php
function t() {
    $a = ['k' => 1, 5 => 'five', '7' => 'seven', 'n' => null];
    $r = &$a;
    $s = 'str';
    $keys = ['k', 'z', 5, 6, '5', '7', 7, 'n', '', '08'];
    for ($i = 0; $i < 2; $i++) {
        foreach ($keys as $key) {
            echo var_export($a[$key] ?? 'D', true), ' ', var_export($r[$key] ?? 'D', true), ' ';
            $v = $a[$key] ?? 'D';
            echo var_export($v, true), "\n";
        }
        echo var_export($a['z'] ?? null, true), var_export($a[99] ?? 0, true), var_export($s[1] ?? 'D', true), var_export($s[9] ?? 'D', true), var_export($s['x'] ?? 'D', true), "\n";
        echo var_export($undef['x'] ?? 'U', true), var_export($a[true] ?? 'B', true), "\n";
        $k = 'dyn' . $i;
        echo var_export($a[$k] ?? 'T', true), var_export($a['k' . ''] ?? 'T', true), "\n";
    }
}
t();
"#;
    let expected = r#"1 1 1
'D' 'D' 'D'
'five' 'five' 'five'
'D' 'D' 'D'
'five' 'five' 'five'
'seven' 'seven' 'seven'
'seven' 'seven' 'seven'
'D' 'D' 'D'
'D' 'D' 'D'
'D' 'D' 'D'
NULL0't''D''D'
'U''B'
'T'1
1 1 1
'D' 'D' 'D'
'five' 'five' 'five'
'D' 'D' 'D'
'five' 'five' 'five'
'seven' 'seven' 'seven'
'seven' 'seven' 'seven'
'D' 'D' 'D'
'D' 'D' 'D'
'D' 'D' 'D'
NULL0't''D''D'
'U''B'
'T'1
"#;
    assert_eq!(run(src), expected);
}

/// `$o->p[$k] ?? $d` of an element the array property lacks is `null` on
/// the property cache's path (`ZEND_FETCH_OBJ_IS` + `ZEND_FETCH_DIM_IS`):
/// public, private (from its class and a subclass) and referenced
/// properties, a null or uninitialized typed property, an `ArrayAccess`
/// object, `__get`. Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn coalesce_of_a_missing_property_element_is_null() {
    let src = r#"<?php
class AA implements ArrayAccess { function offsetExists($o): bool { echo "exists($o) "; return $o === 'k'; } function offsetGet($o): mixed { echo "get($o) "; return 'aa'; } function offsetSet($o, $v): void {} function offsetUnset($o): void {} }
class P {
    public $a = ['k' => 1, 5 => 'five', 'n' => null];
    private $priv = ['k' => 'p'];
    public $nul = null;
    public array $typed;
    public $aa;
    public $ref;
    function __construct() { $this->aa = new AA; $x = ['k' => 'r']; $this->ref = &$x; }
    function t() {
        $keys = ['k', 'z', 5, '5', 'n', 6];
        foreach ($keys as $k) {
            echo var_export($this->a[$k] ?? 'D', true), var_export($this->priv[$k] ?? 'D', true), var_export($this->ref[$k] ?? 'D', true), ' ';
        }
        echo "\n", var_export($this->nul['k'] ?? 'N', true), var_export($this->typed['k'] ?? 'T', true), var_export($this->aa['k'] ?? 'A', true), var_export($this->aa['z'] ?? 'A', true), var_export($this->undef['k'] ?? 'U', true), "\n";
    }
}
class Q extends P { function u() { return [$this->a['z'] ?? 'qd', $this->priv['k'] ?? 'nopriv']; } }
class M { public $a = []; function __get($n) { echo "__get($n) "; return ['k' => 'm']; } }
function run() {
    $p = new P; $q = new Q; $m = new M;
    for ($i = 0; $i < 2; $i++) {
        $p->t(); var_export($q->u()); echo "\n";
        echo var_export($p->a['z'] ?? 'out', true), var_export($m->zz['k'] ?? 'M', true), var_export($m->a['k'] ?? 'MA', true), "\n";
        $p->a['z'] = $i; echo var_export($p->a['z'] ?? 'set', true), "\n"; unset($p->a['z']);
    }
}
run();
"#;
    let expected = r#"1'p''r' 'D''D''D' 'five''D''D' 'five''D''D' 'D''D''D' 'D''D''D' 
'N''T'exists(k) get(k) 'aa'exists(z) 'A''U'
array (
  0 => 'qd',
  1 => 'nopriv',
)
'out'__get(zz) 'm''MA'
0
1'p''r' 'D''D''D' 'five''D''D' 'five''D''D' 'D''D''D' 'D''D''D' 
'N''T'exists(k) get(k) 'aa'exists(z) 'A''U'
array (
  0 => 'qd',
  1 => 'nopriv',
)
'out'__get(zz) 'm''MA'
1
"#;
    assert_eq!(run(src), expected);
}

/// `$cv[$a][$b] ?? $d` of an element missing on the way is `null` on the
/// tape (a chain of `ZEND_FETCH_DIM_IS`): missing outer and inner keys,
/// a `null` element or variable on the way, int and integer-string keys,
/// references, a string on the way (the generic fetch). Expected output:
/// php-src's (PHP 8.4 CLI).
#[test]
fn coalesce_of_a_missing_nested_element_is_null() {
    let src = r#"<?php
function t() {
    $o = new stdClass; $o->p = 1;
    $inner = ['y' => 'iy'];
    $a = ['x' => ['k' => 1, 'n' => null, 's' => 'str', 'o' => $o, 3 => [4 => 'deep']], 'nul' => null, 'r' => &$inner, 'e' => []];
    $r = &$a;
    $paths = [['x','k'], ['x','z'], ['y','z'], ['nul','z'], ['x','n'], ['x', 3], ['x', '3'], ['e', 0]];
    for ($i = 0; $i < 2; $i++) {
        foreach ($paths as [$p, $q]) {
            echo var_export($a[$p][$q] ?? 'D', true), var_export($r[$p][$q] ?? 'D', true), ' ';
        }
        echo "\n", var_export($a['x'][3][4] ?? 'D', true), var_export($a['x'][3][5] ?? 'D', true), var_export($a['x']['n']['q'] ?? 'D', true), var_export($a['x']['s'][0] ?? 'D', true), var_export($a['x']['s']['q'] ?? 'D', true), var_export($a['r']['y'] ?? 'D', true), var_export($a['r']['z'] ?? 'D', true), var_export($undef['a']['b'] ?? 'U', true), "\n";
    }
}
t();
"#;
    let expected = r#"11 'D''D' 'D''D' 'D''D' 'D''D' array (
  4 => 'deep',
)array (
  4 => 'deep',
) array (
  4 => 'deep',
)array (
  4 => 'deep',
) 'D''D' 
'deep''D''D''s''D''iy''D''U'
11 'D''D' 'D''D' 'D''D' 'D''D' array (
  4 => 'deep',
)array (
  4 => 'deep',
) array (
  4 => 'deep',
)array (
  4 => 'deep',
) 'D''D' 
'deep''D''D''s''D''iy''D''U'
"#;
    assert_eq!(run(src), expected);
}
