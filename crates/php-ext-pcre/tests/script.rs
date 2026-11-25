// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `pcre` extension.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let opts = VmOptions::default();
    let ext = php_ext_pcre::Pcre;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn match_with_named_groups_and_offsets() {
    let out = run(
        r#"var_dump(preg_match('/(?<y>\d+)-(\d+)/', 'x 2024-11', $m, PREG_OFFSET_CAPTURE));
        echo json_encode_like($m);
        function json_encode_like($a) { $s = ''; foreach ($a as $k => $v) { $s .= "$k=" . $v[0] . '@' . $v[1] . ';'; } return $s; }"#,
    );
    assert_eq!(out, "int(1)\n0=2024-11@2;y=2024@2;1=2024@2;2=11@7;");
}

#[test]
fn match_all_orders() {
    let out = run(r"preg_match_all('/(\w)(\d)/', 'a1 b2', $m);
        echo implode(',', $m[1]), '|';
        preg_match_all('/(\w)(\d)/', 'a1 b2', $m, PREG_SET_ORDER);
        echo $m[1][0], '|', preg_match_all('/z/', 'abc');");
    assert_eq!(out, "a,b|b2|0");
}

#[test]
fn invalid_flags_throw_value_error() {
    let out = run(
        r"try { preg_match('/a/', 'a', $m, 3); } catch (ValueError $e) { echo $e->getMessage(); }",
    );
    assert_eq!(
        out,
        "preg_match(): Argument #4 ($flags) must be a PREG_* constant"
    );
}

#[test]
fn replace_variants_and_count() {
    let out = run(
        r"echo preg_replace('/(b)/', '[$1]', 'abcb', -1, $c), $c, '|';
        echo preg_replace('/b/', 'X', 'abcb', 1), '|';
        echo implode(',', preg_replace(['/a/', '/b/'], ['b', 'c'], ['k' => 'ab', 'xy'])), '|';
        var_dump(preg_filter('/a/', 'A', ['a', 'b']));",
    );
    assert_eq!(
        out,
        "a[b]c[b]2|aXcb|cc,xy|array(1) {\n  [0]=>\n  string(1) \"A\"\n}\n"
    );
}

#[test]
fn replacement_array_requires_pattern_array() {
    let out = run(
        r"try { preg_replace('/a/', ['x'], 'a'); } catch (TypeError $e) { echo $e->getMessage(); }",
    );
    assert_eq!(
        out,
        "preg_replace(): Argument #1 ($pattern) must be of type array when argument #2 ($replacement) is an array, string given"
    );
}

#[test]
fn callbacks_reenter_and_count() {
    let out = run(r"echo preg_replace_callback('/\d+/', function ($m) {
            return preg_replace_callback('/./', fn($n) => $n[0] . $n[0], $m[0]);
        }, 'a12b3', -1, $c), $c, '|';
        echo preg_replace_callback_array(['/a/' => fn($m) => 'b', '/b/' => fn($m) => 'c'], 'ab', -1, $n), $n;");
    assert_eq!(out, "a1122b332|cc3");
}

#[test]
fn callback_backtraces_keep_arguments_through_native_reentry_and_unwind() {
    let out = run(r#"foreach ([false, true] as $fail) {
            try {
                $result = preg_replace_callback('/./', function($m) use ($fail) {
                    $trace = debug_backtrace();
                    echo $trace[1]['function'], ':', $trace[0]['args'][0][0], "\n";
                    strlen(123);
                    if ($fail) { throw new Exception('stop'); }
                    return strtoupper($m[0]);
                }, 'abc');
                echo $result, "\n";
            } catch (Exception $e) { echo $e->getMessage(), "\n"; }
        }
        echo preg_replace('/b/', '-', 'abc'), "\n";"#);
    assert_eq!(
        out,
        "preg_replace_callback:a\npreg_replace_callback:b\npreg_replace_callback:c\nABC\npreg_replace_callback:a\nstop\na-c\n"
    );
}

#[test]
fn callback_array_keys_must_be_patterns() {
    let out = run(
        r"try { preg_replace_callback_array([fn() => 'x'], 'a'); } catch (TypeError $e) { echo $e->getMessage(); }",
    );
    assert_eq!(
        out,
        "preg_replace_callback_array(): Argument #1 ($pattern) must contain only string patterns as keys"
    );
}

/// `Z_PARAM_FUNC` resolves `$callback` once, in the calling scope, raising
/// the callable deprecations there; `preg_replace_callback_array()`
/// resolves each entry with `zend_is_callable()` (deprecations too). The
/// calls are `ZEND_CALL_DYNAMIC` (`compact()` refuses them).
#[test]
fn callbacks_resolve_in_the_calling_scope_with_deprecations() {
    let out = run(r#"
class foo { public function rc($m) { return strtoupper($m[0]); } }
class bar extends foo {
    function go() {
        echo preg_replace_callback('/a/', 'parent::rc', 'banana'), "\n";
        echo preg_replace_callback('/a/', [$this, 'parent::rc'], 'banana'), "\n";
        echo preg_replace_callback_array(['/n/' => 'parent::rc', '/b/' => 'self::rc'], 'banana'), "\n";
    }
}
(new bar)->go();
try { preg_replace_callback_array(['/a/' => 'parent::rc'], 'a'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { echo preg_replace_callback('/a/', 'compact', 'a'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "\nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 5\n\
         bAnAnA\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::rc\"] are deprecated in test.php on line 6\n\
         bAnAnA\n\
         \nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 7\n\
         \nDeprecated: Use of \"self\" in callables is deprecated in test.php on line 7\n\
         BaNaNa\n\
         preg_replace_callback_array(): Argument #1 ($pattern) must contain only valid callbacks\n\
         Cannot call compact() dynamically\n"
    );
}

#[test]
fn split_grep_quote() {
    let out = run(
        r"echo implode('|', preg_split('/[\s,]+/', 'a, b  c', -1, PREG_SPLIT_NO_EMPTY)), ';';
        foreach (preg_grep('/^\d+$/', ['x' => '1', 'y' => 'a', 'z' => '22']) as $k => $v) { echo $k; }
        foreach (preg_grep('/^\d+$/', ['1', 'a'], PREG_GREP_INVERT) as $k => $v) { echo ';', $k, ';'; }
        echo preg_quote('a.b*c#', '#');",
    );
    assert_eq!(out, "a|b|c;xz;1;a\\.b\\*c\\#");
}

/// `preg_quote()` returns its argument itself when nothing is escaped
/// and the interned empty string for an empty one (`/usr/bin/php8.5 -n`).
#[test]
fn preg_quote_returns_its_argument_when_nothing_is_escaped() {
    let out = run(r"$s = str_repeat('abc', 3);
        $q = preg_quote($s);
        debug_zval_dump($q);
        debug_zval_dump(preg_quote(''));
        debug_zval_dump(preg_quote(str_repeat('a.', 2)));
        debug_zval_dump(preg_quote(str_repeat('a/', 2), '/'));");
    assert_eq!(
        out,
        "string(9) \"abcabcabc\" refcount(3)\nstring(0) \"\" interned\nstring(6) \"a\\.a\\.\" refcount(1)\nstring(6) \"a\\/a\\/\" refcount(1)\n"
    );
}

/// With nothing replaced, `preg_replace()`, `preg_replace_callback()` (also
/// over an array of patterns or of subjects, and with a limit of 0) return
/// the subject string itself, and `preg_split()`'s tail from offset 0 is
/// the subject itself, but not a piece of the same length before an empty
/// match (`/usr/bin/php8.5 -n`'s `debug_zval_dump()`).
#[test]
fn unchanged_subjects_are_returned_themselves() {
    let out = run(r"$s = str_repeat('abc', 3);
$r = preg_replace('/z/', 'y', $s);
debug_zval_dump($r);
$r2 = preg_replace_callback('/z/', fn($m) => 'y', $s);
debug_zval_dump($r2);
$r3 = preg_replace(['/z/', '/q/'], 'y', $s);
debug_zval_dump($r3);
debug_zval_dump(preg_replace('/z/', 'y', [$s]));
debug_zval_dump(preg_replace('/a/', 'y', $s, 0));
debug_zval_dump(preg_replace('/a/', 'y', $s));
$p = preg_split('/z/', $s);
debug_zval_dump($p[0]);
debug_zval_dump(preg_split('/$/', $s, -1, PREG_SPLIT_NO_EMPTY));
");
    assert_eq!(
        out,
        r#"string(9) "abcabcabc" refcount(3)
string(9) "abcabcabc" refcount(4)
string(9) "abcabcabc" refcount(5)
array(1) packed refcount(1){
  [0]=>
  string(9) "abcabcabc" refcount(5)
}
string(9) "abcabcabc" refcount(5)
string(9) "ybcybcybc" refcount(1)
string(9) "abcabcabc" refcount(6)
array(1) packed refcount(1){
  [0]=>
  string(9) "abcabcabc" refcount(1)
}
"#
    );
}

#[test]
fn compile_errors_warn_and_set_last_error() {
    let out = run(r"var_dump(@preg_match('/(/', 'a'));
        echo preg_last_error(), ' ', preg_last_error_msg(), ';';
        preg_match('/a/', 'a');
        echo preg_last_error_msg();");
    assert_eq!(out, "bool(false)\n1 Internal error;No error");
}

#[test]
fn backtrack_limit_from_ini() {
    let out = run(
        r"ini_set('pcre.backtrack_limit', '2'); ini_set('pcre.jit', '0');
        var_dump(preg_match_all('/.*\p{N}/', '0123456789'));
        echo preg_last_error() === PREG_BACKTRACK_LIMIT_ERROR ? 'limit' : 'other';",
    );
    assert_eq!(out, "bool(false)\nlimit");
}

#[test]
fn constants_are_defined() {
    let out = run(
        r"echo PREG_SPLIT_DELIM_CAPTURE, PREG_UNMATCHED_AS_NULL, ' ', PCRE_VERSION_MAJOR, ' ', gettype(PCRE_JIT_SUPPORT);",
    );
    assert_eq!(out, "2512 10 boolean");
}

/// The replacement result made from its parts in one string: matches at
/// the start, the end and everywhere, no match (the subject itself), an
/// empty subject, an empty result, a result longer than 1 KiB, the
/// callback form, `preg_filter()` with nothing replaced, and an invalid
/// pattern (`/usr/bin/php8.5 -n`).
#[test]
fn replace_result_from_parts() {
    let out = run(
        r"echo preg_replace('/o/', '0', '  Hello World  ', -1, $c), $c, '|';
        echo preg_replace('/z/', '0', 'abc', -1, $c), $c, '|';
        echo preg_replace('/c$/', 'XY', 'abc'), '|';
        echo preg_replace('/^a/', 'XY', 'abc'), '|';
        echo preg_replace('/^/', '>', ''), '|';
        echo preg_replace('/a/', '', 'aaa'), '|';
        $big = ''; for ($i = 0; $i < 700; $i++) { $big .= 'ab'; }
        $r = preg_replace('/b/', 'cc', $big); echo strlen($r), $r[0], $r[-1], $r[-3], '|';
        echo preg_replace_callback('/b/', fn($m) => 'Q', 'abcb'), '|';
        echo preg_replace_callback('/z/', fn($m) => 'Q', 'abcb'), '|';
        var_dump(preg_filter('/z/', 'A', 'abc'));
        var_dump(@preg_replace('/(', 'x', 'abc'));",
    );
    assert_eq!(
        out,
        "  Hell0 W0rld  2|abc0|abXY|XYbc|>||2100aca|aQcQ|abcb|NULL\nNULL\n"
    );
}
