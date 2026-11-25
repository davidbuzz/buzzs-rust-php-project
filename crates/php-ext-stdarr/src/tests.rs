// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP snippets run on a VM with this extension.

use super::StdArray;
use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `code` (without the `<?php` tag) and returns its output.
fn run(code: &str) -> String {
    run_with(&[], code)
}

/// [`run`] with `-d` INI settings.
fn run_with(ini: &[(&str, &str)], code: &str) -> String {
    let sink = Sink::default();
    let ext = StdArray;
    let exts: [&dyn Extension; 1] = [&ext];
    let opts = VmOptions {
        ini: ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    let src = format!("<?php\n{code}");
    vm.run_script(src.as_bytes(), "test.php");
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn sorts_through_the_reference() {
    assert_eq!(
        run("$a = [3, 1, 2]; sort($a); echo implode(',', $a);"),
        "1,2,3"
    );
    assert_eq!(
        run("$a = ['b' => 1, 'a' => 2]; ksort($a); echo implode(',', array_keys($a));"),
        "a,b"
    );
}

#[test]
fn user_sort_and_callback_errors() {
    assert_eq!(
        run("$a = [1, 3, 2]; usort($a, fn($x, $y) => $y <=> $x); echo implode(',', $a);"),
        "3,2,1"
    );
    assert_eq!(
        run("$a = [1]; try { usort($a, 'nope'); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "usort(): Argument #2 ($callback) must be a valid callback, function \"nope\" not found or invalid function name"
    );
    assert_eq!(
        run("try { array_map('nope', []); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_map(): Argument #1 ($callback) must be a valid callback or null, function \"nope\" not found or invalid function name"
    );
}

#[test]
fn u_diff_variants_validate_trailing_callbacks() {
    assert_eq!(
        run("try { array_udiff([1]); } catch (ArgumentCountError $e) { echo $e->getMessage(); }"),
        "array_udiff() expects at least 2 arguments, 1 given"
    );
    assert_eq!(
        run("try { array_udiff_uassoc([1], [2], 'strcmp', 'x'); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_udiff_uassoc(): Argument #4 must be a valid callback, function \"x\" not found or invalid function name"
    );
    assert_eq!(
        run("echo implode(',', array_udiff_uassoc([1, 5], [1], fn($a, $b) => $a <=> $b, fn($a, $b) => $a <=> $b));"),
        "5"
    );
    assert_eq!(
        run("try { array_diff([1], 2); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_diff(): Argument #2 must be of type array, int given"
    );
}

#[test]
fn internal_pointer_mutates_the_variable() {
    assert_eq!(
        run("$a = [1, 2, 3]; next($a); next($a); echo current($a), key($a); reset($a); echo current($a);"),
        "321"
    );
    assert_eq!(run("$a = [1, 2, 3]; echo end($a), key($a);"), "32");
}

#[test]
fn extract_and_compact_use_the_callers_scope() {
    assert_eq!(
        run("function f() { $src = ['x' => 1, 'y' => 2]; $n = extract($src); return [$n, $x + $y]; } echo implode(',', f());"),
        "2,3"
    );
    assert_eq!(
        run("function g() { $a = 1; $b = 'z'; return compact('a', ['b']); } echo implode(',', g());"),
        "1,z"
    );
    assert_eq!(
        run("$a = 1; $b = 2; echo implode(',', compact('a', 'b'));"),
        "1,2"
    );
    assert_eq!(
        run("$arr = ['v' => 1]; extract($arr, EXTR_REFS); $v = 5; echo $arr['v'];"),
        "5"
    );
    assert_eq!(
        run("try { call_user_func('extract', ['a' => 1]); } catch (Error $e) { echo $e->getMessage(); }"),
        "Cannot call extract() dynamically"
    );
}

#[test]
fn extract_negative_keys_preserves_the_callers_snapshot() {
    assert_eq!(
        run(
            "$a = [-5 => 'hello', 'world', 2 => 'positive', 'check' => 'extracted'];
            function foo(array $a) {
                extract($a, EXTR_PREFIX_ALL, 'prefix');
                var_dump(get_defined_vars());
            }
            foo($a);"
        ),
        concat!(
            "array(3) {\n",
            "  [\"a\"]=>\n",
            "  array(4) {\n",
            "    [-5]=>\n",
            "    string(5) \"hello\"\n",
            "    [-4]=>\n",
            "    string(5) \"world\"\n",
            "    [2]=>\n",
            "    string(8) \"positive\"\n",
            "    [\"check\"]=>\n",
            "    string(9) \"extracted\"\n",
            "  }\n",
            "  [\"prefix_2\"]=>\n",
            "  string(8) \"positive\"\n",
            "  [\"prefix_check\"]=>\n",
            "  string(9) \"extracted\"\n",
            "}\n"
        )
    );
}

#[test]
fn defined_vars_snapshot_preserves_visibility_and_reference_ownership() {
    assert_eq!(
        run("function snapshot($a) {
                $unset = 1; unset($unset);
                $null = null;
                $shared = [1]; $alias =& $shared;
                $single = [2]; $gone =& $single; unset($gone);
                $dynamic = 'dynamic'; $$dynamic = 3;
                extract($a);
                $vars = get_defined_vars();
                echo implode(',', array_keys($vars)), '|';
                $vars['a']['x'] = 9;
                $vars['single'][0] = 9;
                $vars['shared'][0] = 9;
                $vars['x'] = 9;
                echo $a['x'], ',', $single[0], ',', $shared[0], ',', $alias[0], ',', $x,
                    '|', (int) array_key_exists('null', $vars),
                    '|', (int) array_key_exists('unset', $vars),
                    '|', (int) array_key_exists('vars', $vars);
            }
            snapshot(['x' => 1]);"),
        "a,null,shared,alias,single,dynamic,x|1,2,9,9,1|1|0|0"
    );
}

#[test]
fn rng_shares_mt_srand_state() {
    let a = run("mt_srand(42); $x = range(1, 10); shuffle($x); echo implode(',', $x);");
    let b = run("mt_srand(42); $x = range(1, 10); shuffle($x); echo implode(',', $x);");
    assert_eq!(a, b);
    assert_eq!(run("mt_srand(1); echo strlen(str_shuffle('abcdef'));"), "6");
}

#[test]
fn constants_and_misc() {
    assert_eq!(
        run("echo SORT_STRING, COUNT_RECURSIVE, EXTR_REFS, ARRAY_FILTER_USE_KEY;"),
        "212562"
    );
    assert_eq!(run("echo count([1, [2, 3]], COUNT_RECURSIVE);"), "4");
    assert_eq!(run("echo max(1, 5, 3), min([4, 2]);"), "52");
    assert_eq!(
        run("try { array_key_exists([], []); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "Cannot access offset of type array on array"
    );
    assert_eq!(run("echo implode(',', array_filter([1, 0, 2]));"), "1,2");
}

#[test]
fn clamp_preserves_values_and_validates_bounds() {
    assert_eq!(
        run(
            "echo clamp(2, 1.3, 3.4), '|', clamp(0, 1.3, 3.4), '|', clamp('d', 'c', 'g');
            $f = clamp(...); echo '|', $f(value: 'a', min: 'c', max: 'g');
            echo '|', (int) is_int(clamp(2, 1.3, 3.4)), '|', (int) is_nan(clamp(NAN, 4, 6));"
        ),
        "2|1.3|d|c|1|1"
    );
    assert_eq!(
        run("foreach ([[4, NAN, NAN], [7, 6, NAN], [1, 3, 2]] as $args) {
            try { clamp(...$args); } catch (ValueError $e) { echo $e->getMessage(), '|'; }
        }"),
        "clamp(): Argument #2 ($min) must not be NAN|clamp(): Argument #3 ($max) must not be NAN|clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|"
    );
}

#[test]
fn clamp_source_matrix_matches_direct_and_dynamic_calls() {
    assert_eq!(
        run("declare(strict_types=1);
            $f = clamp(...);
            $cases = [
                [2, 1, 3, 2], [0, 1, 3, 1], [6, 1, 3, 3],
                [2, 1.3, 3.4, 2], [2.5, 1, 3, 2.5],
                [2.5, 1.3, 3.4, 2.5], [0, 1.3, 3.4, 1.3],
                [M_PI, -INF, INF, M_PI], [NAN, 4, 6, NAN],
                ['a', 'c', 'g', 'c'], ['d', 'c', 'g', 'd'],
                ['2025-08-01', '2025-08-15', '2025-09-15', '2025-08-15'],
                ['2025-08-20', '2025-08-15', '2025-09-15', '2025-08-20'],
                [null, -1, 1, -1], [null, 1, 3, 1], [null, -3, -1, -3],
                [-9999, null, 10, -9999], [12, null, 10, 10]
            ];
            foreach ($cases as [$value, $min, $max, $expected]) {
                $direct = clamp($value, $min, $max);
                $dynamic = $f($value, $min, $max);
                if (is_float($expected) && is_nan($expected)) {
                    if (!is_nan($direct) || !is_nan($dynamic)) { echo 'bad NAN'; }
                } elseif ($direct !== $expected || $dynamic !== $expected) {
                    echo 'bad value';
                }
            }
            $a = new InvalidArgumentException('a');
            $b = new RuntimeException('b');
            $c = new LogicException('c');
            foreach ([[$a, $b, $c], [$b, $a, $c], [$c, $a, $b]] as [$value, $min, $max]) {
                if (clamp($value, $min, $max) !== $value || $f($value, $min, $max) !== $value) {
                    echo 'bad object';
                }
            }
            echo 'done';"),
        "done"
    );
}

#[test]
fn clamp_source_errors_match_direct_and_dynamic_calls() {
    assert_eq!(
        run("$f = clamp(...);
            foreach ([[4, NAN, 6], [7, 6, NAN], [1, 3, 2], [-9999, 5, null], [12, -5, null]]
                as [$value, $min, $max]) {
                try { clamp($value, $min, $max); }
                catch (ValueError $e) { echo $e->getMessage(), '|'; }
                try { $f($value, $min, $max); }
                catch (ValueError $e) { echo $e->getMessage(), '|'; }
            }"),
        concat!(
            "clamp(): Argument #2 ($min) must not be NAN|",
            "clamp(): Argument #2 ($min) must not be NAN|",
            "clamp(): Argument #3 ($max) must not be NAN|",
            "clamp(): Argument #3 ($max) must not be NAN|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
            "clamp(): Argument #2 ($min) must be smaller than or equal to argument #3 ($max)|",
        )
    );
}

#[test]
fn globals_prefer_ref_uses_a_snapshot() {
    assert_eq!(
        run("$marker = 1;
            function detached() {
                extract($GLOBALS, EXTR_REFS);
                $marker = 4;
                echo $GLOBALS['marker'], ',', $marker, '|';
            }
            detached();
            $ref = &$marker;
            function shared() {
                extract($GLOBALS, EXTR_REFS);
                $marker = 8;
                echo $GLOBALS['marker'], ',';
                $GLOBALS['marker'] = 9;
                echo $marker;
            }
            shared();"),
        "1,4|8,9"
    );
}

#[test]
fn extract_checks_typed_references_before_later_entries() {
    assert_eq!(
        run("declare(strict_types=1);
            class T { public int $i = 0; public string $s = ''; }
            $o = new T; $i = &$o->i; $s = &$o->s;
            try { extract(['i' => 'foo', 's' => 42]); }
            catch (TypeError $e) { echo $e->getMessage(), '|'; }
            echo $o->i, ',', strlen($o->s), '|';
            echo extract(['i' => '7', 's' => 42]), '|', $o->i, ',', $o->s;"),
        "Cannot assign string to reference held by property T::$i of type int|0,0|2|7,42"
    );
}

#[test]
fn globals_rejects_mutating_array_arguments() {
    assert_eq!(
        run("$marker = 42;
            try { array_splice($GLOBALS, 0, count($GLOBALS)); }
            catch (Error $e) { echo $e->getMessage(), '|'; }
            try { sort($GLOBALS); }
            catch (Error $e) { echo $e->getMessage(), '|'; }
            echo $marker;
            $GLOBALS['items'] = [3, 1, 2];
            sort($GLOBALS['items']); echo '|', implode(',', $items);"),
        "array_splice(): Argument #1 ($array) could not be passed by reference|sort(): Argument #1 ($array) could not be passed by reference|42|1,2,3"
    );
}

#[test]
fn array_map_self_reference_cleanup() {
    assert_eq!(
        run("$a = ['k1' => 'v1', 'k2' => 'v2'];
            $a[] = &$a['k1']; $a[] = &$a;
            $mapped = array_map(fn($v) => [$v], $a);
            $same = array_map(null, $a);
            $zip = array_map(null, $a, $a);
            $a[0] = null; $a[1] = null;
            echo count($mapped), ',', count($same), ',', count($zip), '|', (int) is_null($a), '|done';"),
        "4,4,4|1|done"
    );
}

#[test]
fn merge_recursive_self_reference_cleanup() {
    assert_eq!(
        run("$a = ['key1' => ['key2' => []]];
            $a['key1']['key2']['key3'] = &$a;
            $b = ['key1' => ['key2' => []]];
            $b['key1']['key2']['key3'] = &$b;
            try { array_merge_recursive($a, $b); }
            catch (Error $e) { echo $e->getMessage(), '|'; }
            $a['key1']['key2']['key3'] = null;
            $b['key1']['key2']['key3'] = null;
            echo (int) is_null($a), (int) is_null($b), '|done';"),
        "Recursion detected|11|done"
    );
}

#[test]
fn walk_typed_reference_writeback() {
    assert_eq!(
        run("class T { public int $prop = 42; }
            $o = new T;
            try { @array_walk($o, function (&$v) { $v = []; }); }
            catch (TypeError $e) { echo $e->getMessage(), '|'; }
            echo $o->prop;
            @array_walk($o, function (&$v) { $v = '7'; });
            echo '|', $o->prop, '|', (int) is_int($o->prop);"),
        "Cannot assign array to reference held by property T::$prop of type int|42|7|1"
    );
}

#[test]
fn locale_sort_constant_is_deprecated() {
    let out = run("$a = ['b', 'a']; sort($a, SORT_LOCALE_STRING); echo implode(',', $a);");
    assert!(out.contains(
        "Constant SORT_LOCALE_STRING is deprecated since 8.6, use one of the Collator::*sort*() methods instead"
    ));
    assert!(out.ends_with("a,b"));
}

#[test]
fn object_internal_pointer_persists() {
    let out = run("class T { public int $a; public $b = 1; public $c = 2; }
        $o = new T;
        echo @reset($o), @next($o), @key($o), var_export(@next($o), true), var_export(@key($o), true);");
    // master semantics (GH-16905): uninitialized properties are skipped.
    assert_eq!(out, "12cfalseNULL");
}

#[test]
fn array_splice_detects_destructor_modification() {
    let out = run(
        "class C { function __destruct() { global $arr; $arr[] = 0; } }
        $arr = ['1', new C, '2'];
        try { array_splice($arr, 1, 2); echo 'no'; } catch (Error $e) { echo $e->getMessage(); }",
    );
    assert_eq!(out, "Array was modified during array_splice operation");
    assert_eq!(
        run("$a = [1, 2, 3]; $r = array_splice($a, 1, 1); echo implode(',', $a), '|', implode(',', $r);"),
        "1,3|2"
    );
}

#[test]
fn memory_and_stack_limits() {
    // `memory_limit` is process-global in php-memlimit, so it is exercised
    // by the oom_*.phpt tests rather than by these parallel unit tests.
    assert!(run("array_fill(0, 2147483647, 1);")
        .contains("Possible integer overflow in memory allocation"));
    // Counting (and dropping) a 10000-level array recurses natively: give
    // the script the CLI's large stack.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let deep = "$a = []; for ($i = 0; $i < 10000; $i++) { $a = [$a, []]; }";
            assert_eq!(
                run(&format!("{deep} echo count($a, COUNT_RECURSIVE);")),
                "20000"
            );
            // 128K leaves 80 KiB after the default 48 KiB reservation:
            // enough to compile the script (php-src fails to compile
            // `echo 1;` with 16K or 64K) and far less than 10000 levels of
            // `php_count_recursive`.
            let limited = run_with(
                &[("zend.max_allowed_stack_size", "128K")],
                &format!("{deep} try {{ count($a, COUNT_RECURSIVE); }} catch (Error $e) {{ echo $e->getMessage(); }}"),
            );
            assert_eq!(
                limited,
                "Maximum call stack size of 81920 bytes (zend.max_allowed_stack_size - zend.reserved_stack_size) reached. Infinite recursion?"
            );
        })
        .expect("spawn test thread")
        .join()
        .expect("test thread");
}

/// `Z_PARAM_FUNC`: method callables are resolved in the calling scope,
/// with the `self`/`parent`/`static` deprecations raised once per call of
/// the array function.
#[test]
fn keyword_callables_resolve_in_calling_scope() {
    let out = run(r#"
class A { public function foo($x) { echo static::class, $x, "\n"; return $x; } }
class B extends A {
    function t() {
        array_map("parent::foo", [1, 2]);
        array_map(["self", "foo"], [3]);
        array_map([$this, "A::foo"], [4]);
    }
}
(new B)->t();
"#);
    assert_eq!(
        out,
        "\nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 6\nB1\nB2\n\
         \nDeprecated: Use of \"self\" in callables is deprecated in test.php on line 7\nB3\n\
         \nDeprecated: Callables of the form [\"B\", \"A::foo\"] are deprecated in test.php on line 8\nB4\n"
    );
}

/// By-reference arguments of builtins called from namespaced code, which
/// the tape prepares (`ZEND_SEND_REF` of a variable binds a reference in
/// place): sorts and pointer functions on plain variables, references and
/// globals; a dimension, `array_multisort()`'s prefer-reference
/// parameters, a literal (Error) and a call result (notice) take the
/// generic send. Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn by_reference_arguments_of_prepared_calls() {
    let src = r#"namespace App;
function f() { return [3, 1, 2]; }
function run() {
    for ($i = 0; $i < 2; $i++) {
        $arr = [3, 1, 2]; sort($arr); var_dump($arr);
        $r = [5, 4]; $ref = &$r; rsort($ref); var_dump($r);
        $q = [1, 2, 3]; var_dump(array_shift($q), $q, end($q), key($q), reset($q));
        $data = ['a' => [2, 1]]; sort($data['a']); var_dump($data);
        $ar1 = [3, 1]; $ar2 = ['c', 'a']; array_multisort($ar1, $ar2); var_dump($ar1, $ar2);
        try { sort([3, 2]); } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        $t = end(f()); var_dump($t);
    }
}
run();
$g = [9, 8]; sort($g); var_dump($g, $GLOBALS['g']);
function gl() { global $g; rsort($g); }
gl(); var_dump($g);
"#;
    let expected = r#"array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  [0]=>
  int(5)
  [1]=>
  int(4)
}
int(1)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(3)
}
int(3)
int(1)
int(2)
array(1) {
  ["a"]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(2)
  }
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(3)
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "c"
}
Error: sort(): Argument #1 ($array) could not be passed by reference

Notice: Only variables should be passed by reference in test.php on line 12
int(2)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  [0]=>
  int(5)
  [1]=>
  int(4)
}
int(1)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(3)
}
int(3)
int(1)
int(2)
array(1) {
  ["a"]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(2)
  }
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(3)
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "c"
}
Error: sort(): Argument #1 ($array) could not be passed by reference

Notice: Only variables should be passed by reference in test.php on line 12
int(2)
array(2) {
  [0]=>
  int(8)
  [1]=>
  int(9)
}
array(2) {
  [0]=>
  int(8)
  [1]=>
  int(9)
}
array(2) {
  [0]=>
  int(9)
  [1]=>
  int(8)
}
"#;
    assert_eq!(run(src), expected);
}

/// `in_array()` of a string needle (its own loop: equal bytes, unless both
/// strings may be numeric) and the general comparison: numeric strings,
/// leading whitespace, mixed element types, a reference, arrays, strict and
/// loose, with `array_search()` alongside. Expected output: php-src's
/// (PHP 8.4 CLI).
#[test]
fn in_array_string_needles() {
    let src = r#"$x = "ref";
$hay = ["abc", "10", "1e1", " 10", "0", "", null, 0, 1.5, true, false, [1], "ABC", &$x, "007", "7", "-0", ".5", "0.5"];
$needles = ["abc", "ABC", "10", "1e1", "010", " 10", "10 ", "0", "", "ref", "7", "0x1A", "-0", "0.0", ".5", "0.50", "true", "1.5", "nope", 10, 0, null, true, false, 1.5, [1]];
foreach ($needles as $n) {
    echo var_export($n, true), ': ', var_export(in_array($n, $hay), true), ' ', var_export(in_array($n, $hay, true), true), ' ', var_export(array_search($n, $hay), true), "\n";
}
$strs = ["b", "a", "c"];
var_dump(in_array("a", $strs), in_array("a", ["x" => "a"]), in_array("9", ["9.0"]), in_array("9.0", ["9"]), in_array("abc", ["abc\0"]));
"#;
    let expected = r#"'abc': true true 0
'ABC': true true 9
'10': true true 1
'1e1': true true 1
'010': true false 1
' 10': true true 1
'10 ': true false 1
'0': true true 4
'': true true 5
'ref': true true 9
'7': true true 9
'0x1A': true false 9
'-0': true true 4
'0.0': true false 4
'.5': true true 9
'0.50': true false 9
'true': true false 9
'1.5': true false 8
'nope': true false 9
10: true false 1
0: true true 4
NULL: true true 5
true: true true 0
false: true true 4
1.5: true true 8
array (
  0 => 1,
): true true 9
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
"#;
    assert_eq!(run(src), expected);
}

/// `array_merge()` of lists only appends their values in one go: a list
/// with a hole, a reference shared or not, nested arrays, empty lists, a
/// string key among the lists, appending after the result and a list with
/// sparse integer keys. Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn array_merge_of_lists() {
    let src = r#"$a = [1, 2, 3]; unset($a[1]); $b = [4, [5]]; $x = 9; $r = [&$x, 7]; $e = [];
var_dump(array_merge($a, $b), array_merge($a, $r, $b, $e), array_merge($e, $e), array_merge([1], ['k' => 2], [3]));
$m = array_merge($r, [8]); $m[0] = 'changed'; var_dump($x, $m);
$big = array_merge(range(1, 3), range(4, 6), range(7, 9)); $big[] = 10; var_dump(count($big), array_key_last($big));
$h = [5 => 'a', 9 => 'b']; var_dump(array_merge($h, [1]));
"#;
    let expected = r#"array(4) {
  [0]=>
  int(1)
  [1]=>
  int(3)
  [2]=>
  int(4)
  [3]=>
  array(1) {
    [0]=>
    int(5)
  }
}
array(6) {
  [0]=>
  int(1)
  [1]=>
  int(3)
  [2]=>
  &int(9)
  [3]=>
  int(7)
  [4]=>
  int(4)
  [5]=>
  array(1) {
    [0]=>
    int(5)
  }
}
array(0) {
}
array(3) {
  [0]=>
  int(1)
  ["k"]=>
  int(2)
  [1]=>
  int(3)
}
string(7) "changed"
array(3) {
  [0]=>
  &string(7) "changed"
  [1]=>
  int(7)
  [2]=>
  int(8)
}
int(10)
int(9)
array(3) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  int(1)
}
"#;
    assert_eq!(run(src), expected);
}

/// `array_keys()` without a filter, `array_slice()` of a list and
/// `array_values()` fill their lists in one go: hash and holed lists, a
/// filter loose and strict, slices with and without kept keys, past the
/// end, of references, and appending after the result. Expected output:
/// php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)]
fn lists_filled_in_one_go() {
    let src = r"$h = ['a' => 1, 5 => 2, 'b' => [3], -7 => 4]; $p = [1, 2, 3, 4, 5]; unset($p[2]); $x = 1; $r = [&$x, 2];
var_dump(array_keys($h), array_keys($p), array_keys($h, 2), array_keys($p, '2', true));
var_dump(array_slice($p, 1, 2), array_slice($p, -2), array_slice($p, 0, 10, true), array_slice($p, 1, 2, true), array_slice($h, 1, 2), array_slice($h, 1, 2, true), array_slice($p, 9), array_slice($r, 0, 1));
var_dump(array_values($h), array_values($p), array_values($r)); $v = array_values($r); $v[0] = 9; var_dump($x);
$k = array_keys($p); $k[] = 'n'; var_dump(array_key_last($k));
";
    let expected = r#"array(4) {
  [0]=>
  string(1) "a"
  [1]=>
  int(5)
  [2]=>
  string(1) "b"
  [3]=>
  int(-7)
}
array(4) {
  [0]=>
  int(0)
  [1]=>
  int(1)
  [2]=>
  int(3)
  [3]=>
  int(4)
}
array(1) {
  [0]=>
  int(5)
}
array(0) {
}
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(4)
}
array(2) {
  [0]=>
  int(4)
  [1]=>
  int(5)
}
array(4) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [3]=>
  int(4)
  [4]=>
  int(5)
}
array(2) {
  [1]=>
  int(2)
  [3]=>
  int(4)
}
array(2) {
  [0]=>
  int(2)
  ["b"]=>
  array(1) {
    [0]=>
    int(3)
  }
}
array(2) {
  [5]=>
  int(2)
  ["b"]=>
  array(1) {
    [0]=>
    int(3)
  }
}
array(0) {
}
array(1) {
  [0]=>
  &int(1)
}
array(4) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  array(1) {
    [0]=>
    int(3)
  }
  [3]=>
  int(4)
}
array(4) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(4)
  [3]=>
  int(5)
}
array(2) {
  [0]=>
  &int(1)
  [1]=>
  int(2)
}
int(9)
int(4)
"#;
    assert_eq!(run(src), expected);
}

/// `array_unique()` keeps a table of the strings themselves; numeric,
/// empty and case-differing strings, numbers, booleans, null and a
/// reference among them, and the other flags. `array_column()` makes its
/// keys once and reads rows in place: arrays, a reference to one, an
/// object (public, private and magic properties), rows missing the
/// column, float, bool and numeric-string index keys and an array one.
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)]
fn unique_strings_and_columns() {
    let src = r#"$x = 'b'; $u = ['1', '01', '1.0', 1, 1.0, true, null, '', false, 'a', 'A', 'a', &$x, 'b', 0, '0', ' 1', 9 => 'z', 'k' => 'z'];
var_dump(array_unique($u), array_unique($u, SORT_REGULAR), array_unique([3, '3', 3.0]), array_unique(['x' => [1], 'y' => 'Array'], SORT_STRING));
class R { public $id = 7; public $n = 'pub'; private $p = 'priv'; function __get($k) { return "magic $k"; } function __isset($k) { return $k === 'm'; } }
$row = ['id' => 2, 'n' => 'two', 1 => 'one-int', '5' => 'five'];
$rows = [['id' => 1, 'n' => 'one'], &$row, ['n' => 'no-id'], ['id' => 1.9, 'n' => 'float id'], ['id' => true, 'n' => 'bool id'], ['id' => '07', 'n' => 'str id'], new R, 'scalar', ['id' => 3]];
var_dump(array_column($rows, 'n'), array_column($rows, 'n', 'id'), array_column($rows, null, 'id'), array_column($rows, 1), array_column($rows, '5'), array_column($rows, 'p'), array_column($rows, 'm'));
try { array_column([['id' => [1], 'n' => 'x']], 'n', 'id'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    let expected = r#"
Warning: Array to string conversion in test.php on line 3
array(10) {
  [0]=>
  string(1) "1"
  [1]=>
  string(2) "01"
  [2]=>
  string(3) "1.0"
  [6]=>
  NULL
  [9]=>
  string(1) "z"
  [10]=>
  string(1) "A"
  [11]=>
  string(1) "a"
  [12]=>
  &string(1) "b"
  [14]=>
  int(0)
  [16]=>
  string(2) " 1"
}
array(7) {
  [0]=>
  string(1) "1"
  [5]=>
  bool(true)
  [6]=>
  NULL
  [8]=>
  bool(false)
  [10]=>
  string(1) "A"
  [11]=>
  string(1) "a"
  [12]=>
  &string(1) "b"
}
array(1) {
  [0]=>
  int(3)
}
array(1) {
  ["x"]=>
  array(1) {
    [0]=>
    int(1)
  }
}

Deprecated: Implicit conversion from float 1.9 to int loses precision in test.php on line 7

Deprecated: Implicit conversion from float 1.9 to int loses precision in test.php on line 7
array(7) {
  [0]=>
  string(3) "one"
  [1]=>
  string(3) "two"
  [2]=>
  string(5) "no-id"
  [3]=>
  string(8) "float id"
  [4]=>
  string(7) "bool id"
  [5]=>
  string(6) "str id"
  [6]=>
  string(3) "pub"
}
array(5) {
  [1]=>
  string(7) "bool id"
  [2]=>
  string(3) "two"
  [3]=>
  string(5) "no-id"
  ["07"]=>
  string(6) "str id"
  [7]=>
  string(3) "pub"
}
array(6) {
  [1]=>
  array(2) {
    ["id"]=>
    bool(true)
    ["n"]=>
    string(7) "bool id"
  }
  [2]=>
  array(4) {
    ["id"]=>
    int(2)
    ["n"]=>
    string(3) "two"
    [1]=>
    string(7) "one-int"
    [5]=>
    string(4) "five"
  }
  [3]=>
  array(1) {
    ["id"]=>
    int(3)
  }
  ["07"]=>
  array(2) {
    ["id"]=>
    string(2) "07"
    ["n"]=>
    string(6) "str id"
  }
  [7]=>
  object(R)#1 (3) {
    ["id"]=>
    int(7)
    ["n"]=>
    string(3) "pub"
    ["p":"R":private]=>
    string(4) "priv"
  }
  [8]=>
  string(6) "scalar"
}
array(1) {
  [0]=>
  string(7) "one-int"
}
array(1) {
  [0]=>
  string(4) "five"
}
array(0) {
}
array(1) {
  [0]=>
  string(7) "magic m"
}
Cannot access offset of type array on array
"#;
    assert_eq!(run(src), expected);
}
