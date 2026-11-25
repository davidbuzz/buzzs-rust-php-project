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
    // A callback named by a variable is a dynamic call (php8.5 throws for
    // each form); a literal name is bound at compile time
    // (`zend_compile_func_cuf`), so that call is extract()'s own.
    assert_eq!(
        run("try { $f = 'extract'; call_user_func($f, ['a' => 1]); } catch (Error $e) { echo $e->getMessage(); }"),
        "Cannot call extract() dynamically"
    );
    assert_eq!(
        run("try { $f = 'extract'; $f(['a' => 1]); } catch (Error $e) { echo $e->getMessage(); }"),
        "Cannot call extract() dynamically"
    );
    assert_eq!(
        run("function h() { call_user_func('extract', ['a' => 1]); return isset($a) ? 'set' : 'unset'; } echo h();"),
        "set"
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

/// `extract()` meeting a name the function does not compile (most of
/// `$GLOBALS`' names; `not_compiled` below) attaches the function's
/// variable table in the middle of the walk: the variables written
/// afterwards still reach the function (ext/standard/tests/array/bug25708.phpt).
#[test]
fn extract_keeps_writing_after_a_name_the_function_does_not_compile() {
    assert_eq!(
        run("$a = 1; $b = 2;
            function by_ref() { $a = null; $b = null; extract($GLOBALS, EXTR_REFS); echo $a, $b, '|'; }
            function fresh() { extract($GLOBALS, EXTR_REFS); echo $a, $b, '|'; }
            function by_value() { $a = null; extract($GLOBALS); echo $a, $b, '|'; }
            function mixed() { $x = 0; extract(['not_compiled' => 9, 'x' => 5, 'y' => 6]); echo $x, $y, '|'; }
            function later() {
                $x = 0;
                extract(['x' => 5, 'not_compiled' => 9, 'y' => 6], EXTR_PREFIX_ALL, 'p');
                echo $x, isset($p_x) ? $p_x : '-', $p_y;
            }
            by_ref(); fresh(); by_value(); mixed(); later();"),
        "12|12|12|56|056"
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

/// `compact()` and `extract()` in a function read and write its variables
/// in their slots, a variable `extract()` defines (`$made`) included, and
/// `get_defined_vars()` sees them all (`/usr/bin/php8.5 -n`:
/// `31,2,m!,a,b,i,c,made,`).
#[test]
fn compact_and_extract_use_the_functions_slots() {
    assert_eq!(
        run("function f() { $a = 1; $b = 2; for ($i = 0; $i < 3; $i++) { $c = compact('a', 'b'); $c['a'] += 10; extract($c); } extract(['made' => 'm']); $made .= '!'; echo $a, ',', $b, ',', $made, ','; foreach (array_keys(get_defined_vars()) as $k) { echo $k, ','; } } f();"),
        "31,2,m!,a,b,i,c,made,"
    );
}

/// `array_keys()` reads its table and filter in place: a reference inside
/// the array and a reference passed as the filter compare by their values,
/// `null` is a filter, and a missing filter with `strict` throws
/// (`/usr/bin/php8.5 -n`).
#[test]
fn array_keys_reads_table_and_filter_in_place() {
    let pre = "$x = 1; $a = ['a' => &$x, '5' => 1, 2 => '1', 'z' => null];";
    let cases = [
        ("echo implode(',', array_keys($a));", "a,5,2,z"),
        ("echo implode(',', array_keys($a, 1));", "a,5,2"),
        ("echo implode(',', array_keys($a, '1', true));", "2"),
        ("echo implode(',', array_keys($a, null));", "z"),
        ("$f = 1; $r = &$f; echo implode(',', array_keys($a, $r, true));", "a,5"),
        ("echo count(array_keys([])), count($a);", "04"),
        (
            "try { array_keys($a, strict: true); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(); }",
            "ArgumentCountError: array_keys(): Argument #2 ($filter_value) must be passed explicitly, because the default value is not known",
        ),
    ];
    for (code, want) in cases {
        assert_eq!(run(&format!("{pre} {code}")), want, "{code}");
    }
}

/// The internal-pointer functions move the array in place and copy out
/// the element once: a reference element reads through, a copy of the
/// array is separated and keeps its own pointer, an empty array gives
/// `false`, an array holding a reference to itself, an object, and a
/// TypeError (`/usr/bin/php8.5 -n`).
#[test]
fn pointer_functions_in_place() {
    let out = run(r#"$x = 'X'; $a = [1, &$x, 'c'];
        echo next($a), current($a), key($a), end($a), prev($a), reset($a), '|';
        $b = $a; next($b); next($b); echo current($a), current($b), '|';
        $e = []; var_dump(next($e), end($e), current($e), key($e));
        $self = [1]; $self[1] = &$self; next($self); echo gettype(current($self)), '|';
        $o = new stdClass; $o->p = 1; $o->q = 2;
        echo @end($o), @current($o), '|';
        try { next($y); } catch (TypeError $t) { echo $t->getMessage(), '|'; }
        echo end($a);"#);
    assert_eq!(
        out,
        "XX1cX1|1c|bool(false)\nbool(false)\nbool(false)\nNULL\narray|22|next(): Argument #1 ($array) must be of type array, null given|c"
    );
}

/// `array_merge()` of hashes: string keys appended by their stored hash
/// and updated in place, integer keys renumbered, a list first or a hash
/// first, an emptied hash first, a shared reference kept and a table that
/// grows past its first size (`/usr/bin/php8.5 -n`).
#[test]
fn array_merge_hashes() {
    let out = run(
        r#"function show($a) { $s = ''; foreach ($a as $k => $v) { $s .= var_export($k, true) . '=' . (is_array($v) ? 'A' : var_export($v, true)) . ','; } return $s . '|'; }
        $x = 1; $r = ['a' => &$x, 'b' => 2];
        echo show(array_merge(['key' => 1, 'b' => 2, 'c' => 3], ['a' => 1, 'b' => 20, 'c' => 30, 5 => 'five', 'd' => 4]));
        echo show(array_merge([1, 2], ['a' => 1, '7' => 'x'], ['a' => 'A', 3]));
        echo show(array_merge(['a' => 1, 9 => 'n'], [10, 11], ['b' => 2]));
        $m = array_merge($r, ['c' => 3]); $x = 99; echo show($m);
        $t = ['a' => 1]; unset($t['a']); echo show(array_merge($t, [1, 2], ['z' => 0]));
        echo show(array_merge(['a' => 1], [])), show(array_merge([], ['b' => 2, 3]));
        $big = []; for ($i = 0; $i < 20; $i++) { $big["k$i"] = $i; }
        echo count(array_merge($big, ['k3' => 'x', 'new' => 1])), array_merge($big, ['k3' => 'x'])['k3'], '|';
        $o = ['o' => new stdClass]; echo get_class(array_merge($o, ['p' => 1])['o']);"#,
    );
    assert_eq!(
        out,
        "'key'=1,'b'=20,'c'=30,'a'=1,0='five','d'=4,|0=1,1=2,'a'='A',2='x',3=3,|'a'=1,0='n',1=10,2=11,'b'=2,|'a'=99,'b'=2,'c'=3,|0=1,1=2,'z'=0,|'a'=1,|'b'=2,0=3,|21x|stdClass"
    );
}

/// `array_pop()`, `array_shift()`, `array_push()` and `array_unshift()`
/// work on the array in place: a shared reference element read after the
/// call, a copy of the array kept apart, the next free index and the
/// internal pointer after the call, empty arrays, an array holding a
/// reference to itself, a non-array, and pushing the array onto itself
/// (`/usr/bin/php8.5 -n`).
#[test]
fn stack_functions_in_place() {
    let out = run(r#"$x = 'X'; $a = [1, 'k' => 2, &$x]; $b = $a;
        echo array_pop($a), count($a), count($b), '|';
        $x = 'Y'; echo array_pop($b), '|';
        $a = [5 => 'a', 6 => 'b']; array_pop($a); $a[] = 'c'; echo implode(',', array_keys($a)), '|';
        $a = [1, 2, 3]; next($a); next($a); array_pop($a); echo current($a), '|';
        $a = ['x' => 1, 3 => 2, 9 => 3]; echo array_shift($a), implode(',', array_keys($a)), '|';
        $a = []; var_dump(array_pop($a), array_shift($a));
        $a = [1]; echo array_push($a, 2, [3]), count($a[2]), '|';
        $a = ['k' => 1, 5 => 2]; echo array_unshift($a, 'z', 'y'), implode(',', array_keys($a)), implode(',', array_map('strval', array_slice($a, 0, 3))), '|';
        $a = [1, 2]; next($a); array_unshift($a, 0); echo current($a), '|';
        $s = [1]; $s[] = &$s; echo gettype(array_pop($s)), count($s), '|';
        $n = 5; try { array_pop($n); } catch (TypeError $e) { echo $e->getMessage(), '|'; }
        try { array_push($n, 1); } catch (TypeError $e) { echo $e->getMessage(), '|'; }
        $a = [1, 2]; $c = $a; array_push($a, $a); echo count($a), count($a[2]), count($c);"#);
    assert_eq!(
        out,
        "X23|Y|5,6|1|10,1|NULL\nNULL\n31|40,1,k,2z,y,1|0|array1|array_pop(): Argument #1 ($array) must be of type array, int given|array_push(): Argument #1 ($array) must be of type array, int given|322"
    );
}

/// `array_unique()` and `array_diff()` over a string set placed by the
/// strings' stored hashes: strings, numbers and their string forms,
/// `null`/`''`/`false`, a reference element, numeric-looking strings kept
/// verbatim, arrays (`"Array"`), one element, and an empty exclusion
/// (`/usr/bin/php8.5 -n`).
#[test]
fn array_unique_and_diff_string_sets() {
    let out = run(
        r#"function show($a) { $s = ''; foreach ($a as $k => $v) { $s .= var_export($k, true) . '=' . (is_array($v) ? 'A' : var_export($v, true)) . ','; } return $s . '|'; }
        echo show(array_unique(['a', 'b', 'c', 'd', 'e']));
        echo show(array_unique([1, '1', 1.0, '1.0', true, 'x' => 'a', 'y' => 'a', 3 => null, 4 => '', 5 => false]));
        $x = 'b'; echo show(array_unique(['k' => 'b', 'r' => &$x, 'z' => 'c']));
        echo show(array_unique(['12', 12, '012', 'a' => 'A', 'b' => 'a']));
        echo show(@array_unique([[1], [2], 'Array']));
        echo show(array_unique([5 => 'q']));
        echo show(array_diff(['a', 'b', 1, '2', 'c' => 'c'], ['b', 2], ['c']));
        echo show(array_diff([1, 2, 3], []));"#,
    );
    assert_eq!(
        out,
        "0='a',1='b',2='c',3='d',4='e',|0=1,3=NULL,'x'='a',|'k'='b','z'='c',|0='12',2='012','a'='A','b'='a',|0=A,|5='q',|0='a',2=1,|0=1,1=2,2=3,|"
    );
}

/// `array_search()` / `in_array()` walk positions and compare strings with
/// `zend_fast_equal_strings()`'s first-byte test: numeric strings,
/// leading whitespace, case, holes in a list and in a hash, a reference
/// element, `null` and `0` against `''` (`/usr/bin/php8.5 -n`).
#[test]
fn array_search_positions_and_strings() {
    let out = run(r#"$small = ['a', 'b', 'c', 'd', 'e'];
        var_export([array_search('d', $small), array_search('z', $small), array_search('10', ['1e1']), array_search('1e1', [5, '10']), array_search(' 1', ['1']), array_search('abc', ['ABC', 'abc'])]);
        $h = [3 => 'x', 'k' => 'y', 9 => 'z']; unset($h[3]); var_export([array_search('z', $h), array_search('y', $h), in_array('x', $h)]);
        $l = [1, 2, 3, 4]; unset($l[1]); var_export([array_search(3, $l), array_search('4', $l), array_search('4', $l, true), in_array(2, $l)]);
        $x = 'q'; $r = ['a', &$x]; var_export([array_search('q', $r), array_search('q', $r, true), in_array(null, ['', 0]), array_search(0, ['a', 0]), array_search('', [null])]);"#);
    assert_eq!(
        out.replace(|c: char| c == '\n' || c == ' ', ""),
        "array(0=>3,1=>false,2=>0,3=>1,4=>0,5=>1,)array(0=>9,1=>'k',2=>false,)array(0=>2,1=>3,2=>false,3=>false,)array(0=>1,1=>1,2=>true,3=>1,4=>0,)"
    );
}

/// `array_filter()` without a callback decides the common scalars inline
/// and appends kept list elements in place: every falsy and truthy kind,
/// keys kept with holes, the next index after the call, a reference
/// element, keys out of order, the callback forms, and NaN
/// (`/usr/bin/php8.5 -n`).
#[test]
fn array_filter_truthiness_and_keys() {
    let out = run(
        r#"function show($a) { $s = ''; foreach ($a as $k => $v) { $s .= var_export($k, true) . '=' . (is_array($v) ? 'A' : var_export($v, true)) . ','; } return $s . '|'; }
        echo show(array_filter(['a', '', 'b', '0', 'c', 0, 1, null, false, true, [], [0], 0.0, 'x' => 'X', -1, '00', ' ']));
        echo show(array_filter([0, 0, 'a', 'b'])), show(array_filter([])), show(array_filter([0, '']));
        $l = [1, 2, 3]; unset($l[1]); echo show(array_filter($l)); $r = array_filter($l); $r[] = 9; echo show($r);
        $x = 1; echo show(array_filter([&$x, 0]));
        echo show(array_filter([5 => 1, 2 => 2, 0 => 3]));
        echo show(array_filter([1, 2, 3, 4], fn($v) => $v % 2)), show(array_filter(['a' => 1, 'b' => 2], fn($k) => $k == 'b', ARRAY_FILTER_USE_KEY));
        echo show(@array_filter([NAN, 0.0, 1.5]));"#,
    );
    assert_eq!(
        out,
        "0='a',2='b',4='c',6=1,9=true,11=A,'x'='X',13=-1,14='00',15=' ',|2='a',3='b',|||0=1,2=3,|0=1,2=3,3=9,|0=1,|5=1,2=2,0=3,|0=1,2=3,|'b'=2,|0=NAN,2=1.5,|"
    );
}
