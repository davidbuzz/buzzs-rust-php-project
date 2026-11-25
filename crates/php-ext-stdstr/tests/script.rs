// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `standard`
//! string/math/url binding. Expected outputs come from PHP itself.

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

/// `memory_limit` is process-wide (one counting allocator, as one PHP
/// process): VMs in parallel test threads would reset each other's limit,
/// so the script tests of this binary run one at a time.
static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_with(ini: &[(&str, &str)], code: &str) -> String {
    let _serial = VM_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let ext = php_ext_stdstr::StdStr;
    let exts: [&dyn Extension; 1] = [&ext];
    let opts = VmOptions {
        ini: ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

fn run(code: &str) -> String {
    run_with(&[("display_errors", "1"), ("html_errors", "0")], code)
}

#[test]
fn str_replace_arrays_and_count() {
    let out = run(r#"
        echo str_replace(['a', 'b'], ['b', 'c'], 'ab', $n), " $n\n";
        print_r(str_ireplace('A', 'x', ['k' => 'aA', 5 => 'b'], $n)); echo $n, "\n";
        echo str_replace(['a', 'b'], 'z', 'abc'), "\n";
        try { str_replace('a', ['b'], 'a'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "cc 3\nArray\n(\n    [k] => xx\n    [5] => b\n)\n2\nzzc\n\
         str_replace(): Argument #2 ($replace) must be of type string when argument #1 ($search) is a string\n"
    );
}

/// `explode()` with every piece kept (the list built to size) and with a
/// limit, then appended to: keys, values and the next index as php8.5
/// gives them.
#[test]
fn explode_lists_append_like_php() {
    let out = run(r#"
        function t() {
            foreach ([[',', 'a,b,c', PHP_INT_MAX], [',', 'a,b,c', 3], [',', 'a,b,c', 2],
                      [',', ',a,', 5], [',,', 'a,,,b,,', 9], [',', 'abc', 9], [',', '', 4],
                      [',', 'a,b,c', -1], ['ab', 'xabyabz', 3]] as [$d, $s, $l]) {
                $r = explode($d, $s, $l);
                $r[] = 'n';
                foreach ($r as $k => $v) { echo "$k=$v "; }
                echo "\n";
            }
            $r = explode(',', str_repeat('x,', 20));
            $r[] = 'n';
            echo $r[19], '|', $r[20], '|', $r[21], "\n";
        }
        t();
    "#);
    assert_eq!(
        out,
        "0=a 1=b 2=c 3=n \n0=a 1=b 2=c 3=n \n0=a 1=b,c 2=n \n0= 1=a 2= 3=n \n\
         0=a 1=,b 2= 3=n \n0=abc 1=n \n0= 1=n \n0=a 1=b 2=n \n0=x 1=y 2=z 3=n \nx||n\n"
    );
}

#[test]
fn substr_replace_forms() {
    let out = run(r#"
        echo substr_replace('Hello', 'J', 0, 1), "\n";
        print_r(substr_replace(['abc', 'def'], ['X', 'Y'], [1, 2], [1, 0]));
        try { substr_replace('abc', 'x', [1]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "Jello\nArray\n(\n    [0] => aXc\n    [1] => deYf\n)\n\
         substr_replace(): Argument #3 ($offset) cannot be an array when working on a single string\n"
    );
}

#[test]
fn implode_forms_and_errors() {
    let out = run(r#"
        echo implode(',', [1, 'a', 2.5, true]), "|", implode(['x', 'y']), "|", join('-', [1, 2]), "\n";
        try { implode(',', 'x'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { implode('x'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "1,a,2.5,1|xy|1-2\n\
         implode(): Argument #2 ($array) must be of type ?array, string given\n\
         implode(): If argument #1 ($separator) is of type string, argument #2 ($array) must be of type array, null given\n"
    );
}

#[test]
fn strtr_forms() {
    let out = run(r#"
        echo strtr('Hi all', 'ai', 'eo'), "|", strtr('Hi all', ['Hi' => 'Hello', 'all' => 'world', 1 => 'one']), "|", strtr('a1', [1 => 'one']), "\n";
        try { strtr('a', 'b'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "Ho ell|Hello world|aone\nstrtr(): Argument #2 ($from) must be of type array, string given\n"
    );
}

#[test]
fn strtr_converts_replacements_in_php_order() {
    let out = run(r#"
        class Replacement {
            public $n = 0;
            function __toString(): string {
                echo "convert:", ++$this->n, "\n";
                return (string) $this->n;
            }
        }
        $r = new Replacement;
        echo strtr('aa', ['a' => $r, 'unused' => new stdClass]), "\n";
        echo strtr('x', ['yy' => new stdClass, 'z' => new stdClass]), "\n";
        echo strtr('abab', ['a' => 'b', 'ab' => 'a']), "\n";
        echo strtr('12', [1 => 'one', 2 => 'two']), "\n";
        echo strtr('', ['' => new stdClass, 'a' => new stdClass]), "empty\n";
        set_error_handler(function($n, $s) { echo $s, "\n"; });
        echo strtr('aa', ['' => new stdClass, 'a' => $r]), "\n";
        echo strtr('x', ['missing' => $r]), "\n";
        try { strtr('x', ['' => new stdClass]); }
        catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { strtr('aa', ['a' => new stdClass, 'b' => 'B']); }
        catch (Error $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "convert:1\nconvert:2\n12\nx\naa\nonetwo\nempty\n\
         strtr(): Ignoring replacement of empty string\nconvert:3\nconvert:4\n34\n\
         convert:5\nx\nObject of class stdClass could not be converted to string\n\
         Object of class stdClass could not be converted to string\n"
    );
}

#[test]
fn by_reference_outputs() {
    let out = run(r#"
        echo similar_text('World', 'Word', $p), ' ', round($p, 4), "\n";
        $n = sscanf('age: 25 name: Bob', 'age: %d name: %s', $age, $name);
        var_dump($n, $age, $name);
        var_dump(sscanf('12 apples', '%d %s'));
    "#);
    assert_eq!(
        out,
        "4 88.8889\nint(2)\nint(25)\nstring(3) \"Bob\"\narray(2) {\n  [0]=>\n  int(12)\n  [1]=>\n  string(6) \"apples\"\n}\n"
    );
}

#[test]
fn strtok_keeps_state() {
    let out = run(r#"
        $t = strtok('a b  c', ' ');
        while ($t !== false) { echo "[$t]"; $t = strtok(' '); }
    "#);
    assert_eq!(out, "[a][b][c]");
}

#[test]
fn printf_family_converts_lazily() {
    let out = run(r#"
        echo sprintf('%04d|%s|%5.1f|%x', -12.3456, 'x', 3.14159, 255), "\n";
        echo sprintf('%s', [1]), "\n";
        echo sprintf('%d', [1]), "\n";
        echo vsprintf('%2$s-%1$s', ['a', 'b']), "\n";
        $n = printf("%'*6s\n", 'ab'); echo $n, "\n";
        try { sprintf('%d %d', 1); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "-012|x|  3.1|ff\n\nWarning: Array to string conversion in t.php on line 3\nArray\n1\nb-a\n****ab\n7\n3 arguments are required, 2 given\n"
    );
}

#[test]
fn sprintf_rope_evaluates_arguments_before_converting_in_order() {
    let out = run(r#"
        class S {
            function __construct() { echo "constructed\n"; }
            function __toString(): string { echo "converted\n"; return 'S'; }
        }
        echo sprintf('%s/%s/%d/%%', new S, new S, 12), "\n";
        set_error_handler(function($n, $s) { throw new ErrorException($s); });
        try { sprintf('%s-%s', new stdClass, []); }
        catch (Throwable $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
        try { sprintf('%s-%s', [], new stdClass); }
        catch (Throwable $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "constructed\nconstructed\nconverted\nconverted\nS/S/12/%\n\
         Object of class stdClass could not be converted to string\n#0 {main}\n\
         Array to string conversion\n"
    );
}

#[test]
fn sprintf_rope_values_constants_and_fallbacks() {
    let out = run(r#"
        debug_zval_dump(sprintf('const'), sprintf('%s', 'literal'), sprintf('%%s'), sprintf(''));
        echo sprintf('%s/%s', sprintf('%s:%s', 'foo', 'bar'), sprintf('%s-%s', 'BAZ', 'BAZ')), "\n";
        echo sprintf('%s-%s-%s', true, false, true), "\n";
        echo sprintf("%s\0%s", 'a', 'b'), "\n";
        echo sprintf('%%s', 'unused'), "\n";
        try { sprintf('%s/%s', 'one'); }
        catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
        $f = 'sprintf';
        try { $f('%s', new stdClass); }
        catch (Error $e) { echo str_contains($e->getTraceAsString(), 'sprintf(') ? "dynamic call\n" : "rope\n"; }
        try { sprintf('%02s', new stdClass); }
        catch (Error $e) { echo str_contains($e->getTraceAsString(), 'sprintf(') ? "width call\n" : "rope\n"; }
        try { sprintf('%s', new stdClass, 'extra'); }
        catch (Error $e) { echo str_contains($e->getTraceAsString(), 'sprintf(') ? "extra call\n" : "rope\n"; }
        try { sprintf(format: '%s', values: new stdClass); }
        catch (Error $e) { echo str_contains($e->getTraceAsString(), 'sprintf(') ? "named call\n" : "rope\n"; }
        $g = sprintf(...);
        echo $g('%s/%s', 'a', 'b'), "\n";
        function format_string() { echo "format evaluated\n"; return 'plain'; }
        echo sprintf(format_string()), "\n";
    "#);
    assert_eq!(
        out,
        "string(5) \"const\" interned\nstring(7) \"literal\" interned\n\
         string(2) \"%s\" refcount(1)\nstring(0) \"\" interned\n\
         foo:bar/BAZ-BAZ\n1--1\na\0b\n%s\n3 arguments are required, 2 given\n\
         dynamic call\nwidth call\nextra call\nnamed call\na/b\nformat evaluated\nplain\n"
    );
}

#[test]
fn sprintf_rope_rejects_long_and_malformed_constant_formats() {
    let long = "a".repeat(254) + "%s";
    let out = run(&format!(
        r#"
            try {{ sprintf('{long}', new stdClass); }}
            catch (Error $e) {{ echo str_contains($e->getTraceAsString(), 'sprintf(') ? "long call\n" : "rope\n"; }}
            // With no argument the trailing `%` counts as a missing argument
            // first (ArgumentCountError); with one it is a ValueError.
            try {{ sprintf('%', 'x'); }}
            catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
        "#
    ));
    assert_eq!(
        out,
        "long call\nMissing format specifier at end of string\n"
    );
}

#[test]
fn sprintf_rope_respects_namespaced_and_disabled_functions() {
    assert_eq!(
        run(r#"
            namespace N;
            function sprintf(...$args) { return 'user'; }
            echo sprintf('%s', 'ignored'), "\n", \sprintf('%s', 'global'), "\n";
        "#),
        "user\nglobal\n"
    );
    assert_eq!(
        run_with(
            &[("disable_functions", "sprintf")],
            "function sprintf(...$args) { return 'replacement'; } echo sprintf('%s', 'a');",
        ),
        "replacement"
    );
}

#[test]
fn unchanged_htmlspecialchars_preserves_literal_and_dynamic_identity() {
    let out = run(r#"
        $s = str_repeat('abc', 10);
        $s[0] = 'x';
        $r = htmlspecialchars($s);
        debug_zval_dump($s);
        $r = htmlspecialchars("caf\u{E9}");
        debug_zval_dump($r);
        $s = str_repeat('"it\'s" ', 4);
        $s[0] = '"';
        $r = htmlspecialchars($s, ENT_NOQUOTES);
        debug_zval_dump($s);
    "#);
    assert_eq!(
        out,
        "string(30) \"xbcabcabcabcabcabcabcabcabcabc\" refcount(3)\n\
         string(5) \"café\" interned\n\
         string(28) \"\"it's\" \"it's\" \"it's\" \"it's\" \" refcount(3)\n"
    );
}

#[test]
fn str_repeat_one_preserves_string_storage() {
    use php_runtime::{Value, ZStr};

    let ext = php_ext_stdstr::StdStr;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(std::io::sink()), &exts);
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
    }
    assert_eq!(
        run(r#"
            debug_zval_dump(str_repeat('abc', 1), str_repeat('abc', 0), str_repeat('', 2));
            try { str_repeat('', -1); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        "#),
        "string(3) \"abc\" interned\nstring(0) \"\" interned\nstring(0) \"\" interned\n\
         str_repeat(): Argument #2 ($times) must be greater than or equal to 0\n"
    );
}

#[test]
fn math_functions() {
    let out = run(r#"
        var_dump(round(2.5), round(-2.5), round(1234.5678, -2), round(5, 1), round(2.5, 0, PHP_ROUND_HALF_EVEN));
        var_dump(abs(-5), abs(PHP_INT_MIN), floor(-1.5), ceil(4), intdiv(7, 2), fmod(7.5, 2));
        echo number_format(1234567.891), ' ', number_format(1234567.891, 2, ',', '.'), ' ', number_format(-0.4), "\n";
        echo base_convert('ff', 16, 2), ' ', bindec('111'), ' ', hexdec('ff'), ' ', octdec('777'), ' ', dechex(255), "\n";
        try { intdiv(1, 0); } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; }
        try { round(1, 0, 99); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        try { log(1, 0); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
    "#);
    assert_eq!(
        out,
        "float(3)\nfloat(-3)\nfloat(1200)\nfloat(5)\nfloat(2)\n\
         int(5)\nfloat(9.223372036854776E+18)\nfloat(-2)\nfloat(4)\nint(3)\nfloat(1.5)\n\
         1,234,568 1.234.567,89 0\n11111111 7 255 511 ff\nDivision by zero\n\
         round(): Argument #3 ($mode) must be a valid rounding mode (RoundingMode::*)\n\
         log(): Argument #2 ($base) must be greater than 0\n"
    );
}

#[test]
fn url_and_html() {
    let out = run(r#"
        print_r(parse_url('https://u:p@host:8080/p/a?q=1#f'));
        var_dump(parse_url('//x.com/p', PHP_URL_HOST), parse_url('http:///x'));
        echo urlencode('a b&c'), ' ', rawurlencode('a b'), ' ', urldecode('a+b%21'), "\n";
        echo htmlspecialchars("<a href='x'>&amp;</a>"), ' ', htmlspecialchars('&amp;', double_encode: false), "\n";
        echo htmlentities('café'), ' ', html_entity_decode('&eacute;&lt;&#39;'), "\n";
    "#);
    assert_eq!(
        out,
        "Array\n(\n    [scheme] => https\n    [host] => host\n    [port] => 8080\n    [user] => u\n    [pass] => p\n    [path] => /p/a\n    [query] => q=1\n    [fragment] => f\n)\n\
         string(5) \"x.com\"\nbool(false)\n\
         a+b%26c a%20b a b!\n&lt;a href=&#039;x&#039;&gt;&amp;amp;&lt;/a&gt; &amp;\ncaf&eacute; é<'\n"
    );
}

#[test]
fn pack_unpack_and_misc() {
    let out = run(r#"
        echo bin2hex(pack('nvc*', 0x1234, 0x5678, 65, 66)), "\n";
        print_r(unpack('nfirst/vsecond', "\x12\x34\x78\x56"));
        echo version_compare('5.2', '5.10'), ' ', var_export(version_compare('1.0rc1', '1.0', '<'), true), "\n";
        echo soundex('Robert'), ' ', levenshtein('kitten', 'sitting'), ' ', quoted_printable_encode("a=b\xff"), "\n";
        echo convert_uudecode(convert_uuencode('hello')), "\n";
    "#);
    assert_eq!(
        out,
        "1234785641 42\n"
            .replace(' ', "")
            + "Array\n(\n    [first] => 4660\n    [second] => 22136\n)\n-1 true\nR163 3 a=3Db=FF\nhello\n"
    );
}

#[test]
fn deprecated_functions_and_strings() {
    let out = run(r#"
        echo bin2hex(utf8_encode("\xe9")), "\n";
        echo str_pad('5', 3, '0', STR_PAD_LEFT), ' ', ucwords('hello_world-foo', '_-'), ' ', wordwrap('The quick brown fox', 10, "\n", true), "\n";
        echo strlen(str_shuffle('abcdef')), ' ', count_chars('abca', 3), ' ', str_word_count('Hello fri3nd, you are great'), "\n";
        echo str_increment('Az'), ' ', str_decrement('Ba'), ' ', nl2br("a\nb"), "\n";
    "#);
    assert_eq!(
        out,
        "\nDeprecated: Function utf8_encode() is deprecated since 8.2, visit the php.net documentation for various alternatives in t.php on line 2\nc3a9\n\
         005 Hello_World-Foo The quick\nbrown fox\n6 abc 6\nBa Az a<br />\nb\n"
    );
}

#[test]
fn memory_limit_is_enforced_for_large_results() {
    let out = run_with(
        &[
            ("display_errors", "1"),
            ("memory_limit", "8M"),
            // As run-tests.php runs the .phpt tests: no fatal error backtrace.
            ("fatal_error_backtraces", "0"),
        ],
        "echo strlen(str_repeat('ab', 1000)), \"\\n\"; str_pad('x', 100000000);",
    );
    assert_eq!(
        out,
        "2000\nPHP Fatal error:  Allowed memory size of 8388608 bytes exhausted (tried to allocate 100003840 bytes) in t.php on line 1\n"
            .replace("PHP Fatal error:  ", "\nFatal error: ")
    );
}

/// `strspn()`/`strcspn()` read `$length` in place: `null`, a negative and
/// a positive length, none, and a TypeError (`/usr/bin/php8.5 -n`).
#[test]
fn spn_length_argument() {
    assert_eq!(
        run(
            r#"echo strcspn("abcdef", "e", 1, null), strspn("aab", "a", 0, 1), strcspn("abcdef", "f", 0, -2), strspn("aab", "ab", 1), "|"; try { strspn("a", "a", 0, "x"); } catch (TypeError $e) { echo $e->getMessage(); }"#
        ),
        "3142|strspn(): Argument #4 ($length) must be of type ?int, string given"
    );
}

/// `str_replace()` with arguments read in place and a replacement as long
/// as the search patched into one copy: one byte, several bytes,
/// overlapping candidates, the last byte, no occurrence, a replacement
/// equal to the search, longer and empty replacements, array subjects
/// and searches, numbers, and a reference subject (`/usr/bin/php8.5 -n`).
#[test]
fn str_replace_same_length_and_in_place() {
    let out = run(
        r"echo str_replace('l', 'L', '  Hello World  ', $c), $c, '|';
        echo str_replace('aa', 'bb', 'aaaaa', $c), $c, '|';
        echo str_replace('ab', 'xy', 'abab_ab', $c), $c, '|';
        echo str_replace('z', 'y', 'abc', $c), $c, '|';
        echo str_replace('c', 'c', 'abc', $c), $c, '|';
        echo str_replace('d', 'D', 'abcd', $c), $c, '|';
        echo str_replace('l', 'LL', 'hello', $c), $c, '|';
        echo str_replace('ll', '', 'hello', $c), $c, '|';
        echo implode(',', str_replace('a', 'A', ['x' => 'banana', 'y' => 'cherry', 3])), '|';
        echo str_replace(['a', 'b'], ['b', 'c'], 'ab', $c), $c, '|';
        echo str_replace(1, 2, 1213, $c), $c, '|';
        $s = 'aXa'; $r = &$s; echo str_replace('X', 'Y', $r), $s;",
    );
    assert_eq!(
        out,
        "  HeLLo WorLd  3|bbbba2|xyxy_xy3|abc0|abc1|abcD1|heLLLLo2|heo1|bAnAnA,cherry,3|cc3|22232|aYaaXa"
    );
}

/// `ucfirst()`/`lcfirst()` return the argument itself when the first byte
/// does not change (`zend_string_copy`: one more reference), a new string
/// when it does, and the interned empty string for `''`
/// (`/usr/bin/php8.5 -n`).
#[test]
fn ucfirst_lcfirst_share_unchanged_strings() {
    let out = run(r#"$a = ''; for ($i = 0; $i < 3; $i++) { $a .= 'Ab'; }
        $b = ucfirst($a); debug_zval_dump($b); $c = lcfirst($a); debug_zval_dump($c);
        debug_zval_dump(ucfirst(''), lcfirst(''));
        echo ucfirst('hello'), lcfirst('HELLO'), ucfirst('1x'), ucfirst("\xe9t") === "\xe9t" ? 'same' : 'changed', lcfirst('a'), ucfirst('b');"#);
    assert_eq!(
        out,
        "string(6) \"AbAbAb\" refcount(3)\nstring(6) \"abAbAb\" refcount(2)\nstring(0) \"\" interned\nstring(0) \"\" interned\nHellohELLO1xsameaB"
    );
}

/// The default charset hint of `htmlspecialchars()` follows `ini_set()`
/// of `default_charset` and `internal_encoding` (worked out once per INI
/// generation): UTF-8, ISO-8859-1, `internal_encoding` taking precedence
/// and given up again, an explicit `$encoding` (`/usr/bin/php8.5 -n`).
#[test]
fn htmlspecialchars_default_charset_follows_ini() {
    let out = run(r#"echo bin2hex(htmlspecialchars("\xe9<")), '|';
        ini_set('default_charset', 'ISO-8859-1'); echo bin2hex(htmlspecialchars("\xe9<")), '|';
        ini_set('internal_encoding', 'UTF-8'); echo bin2hex(htmlspecialchars("\xe9<")), '|';
        ini_set('internal_encoding', ''); echo bin2hex(htmlspecialchars("\xe9<")), '|';
        echo bin2hex(htmlspecialchars("\xe9<", ENT_QUOTES, 'UTF-8')), '|';
        ini_set('default_charset', 'UTF-8'); echo bin2hex(htmlspecialchars("\xe9<")), '|', htmlspecialchars('plain');"#);
    assert_eq!(
        out,
        "efbfbd266c743b|e9266c743b|efbfbd266c743b|e9266c743b||efbfbd266c743b|plain"
    );
}

/// `htmlspecialchars()` returns its argument itself when nothing needs
/// encoding (php-src master's `html_verbatim_prefix_len()` fast path:
/// `zend_string_copy`, so refcount(3) here; PHP 8.5 copies and shows 2),
/// the interned empty string for `''`, and encodes after the verbatim
/// prefix as before: quotes by flags, UTF-8 kept or substituted, double
/// encoding off, `ENT_DISALLOWED`, single-byte and partially supported
/// charsets (`/usr/bin/php8.5 -n` for the encoded results).
#[test]
fn htmlspecialchars_verbatim_prefix() {
    let out = run(
        r#"$p = ''; for ($i = 0; $i < 2; $i++) { $p .= 'plain text'; }
        $r = htmlspecialchars($p); debug_zval_dump($r);
        $e = ''; for ($i = 0; $i < 1; $i++) { $e .= ''; } debug_zval_dump(htmlspecialchars($e));
        echo htmlspecialchars("a<b>&c\"d'e"), '|', htmlspecialchars("x'y\"z", ENT_NOQUOTES), '|', htmlspecialchars("x'y", ENT_COMPAT), '|';
        echo htmlspecialchars("caf\xc3\xa9 & t\xc3\xa9"), '|', bin2hex(htmlspecialchars("ok\xff<")), '|', htmlspecialchars("ok\xc3\xa9"), '|';
        echo htmlspecialchars("&amp; &lt; &bogus;", ENT_QUOTES, null, false), '|', bin2hex(htmlspecialchars("abc\x01\x7f", ENT_QUOTES | ENT_DISALLOWED | ENT_HTML5)), '|';
        echo bin2hex(htmlspecialchars("\xe9t\xe9", ENT_QUOTES, 'ISO-8859-1')), '|', bin2hex(htmlspecialchars("\x81\x40<", ENT_QUOTES, 'SJIS')), '|', htmlspecialchars("it's", ENT_QUOTES | ENT_HTML5);"#,
    );
    assert_eq!(
        out,
        "string(20) \"plain textplain text\" refcount(3)\nstring(0) \"\" interned\n\
         a&lt;b&gt;&amp;c&quot;d&#039;e|x'y\"z|x'y|caf\u{e9} &amp; t\u{e9}|6f6befbfbd266c743b|ok\u{e9}|\
         &amp; &lt; &amp;bogus;|616263efbfbdefbfbd|e974e9|8140266c743b|it&apos;s"
    );
}

/// `strtr()` returns its subject itself when nothing is replaced (no pair
/// matches, no pairs, no `$from` byte occurs, an identity translation of
/// several bytes) and a new string when a replacement is made, even an
/// identical one; the array form looks keys up in the pairs array:
/// longest match first, integer keys as their digits, empty keys warned
/// about, keys longer than the subject, replacements converted
/// (`/usr/bin/php8.5 -n`).
#[test]
fn strtr_shares_unchanged_subjects() {
    let out = run(r#"$a = ''; for ($i = 0; $i < 2; $i++) { $a .= 'Hello'; }
        foreach ([['x' => 'y', 'zz' => 'w'], ['x' => 'y'], [], ['l' => 'l'], ['l' => 'l', 'q' => 'r']] as $p) { $r = strtr($a, $p); debug_zval_dump($r); }
        foreach ([['xy', 'ab'], ['l', 'l'], ['lo', 'lo'], ['lo', 'ol'], ['', 'x']] as [$f, $t]) { $r = strtr($a, $f, $t); debug_zval_dump($r); }
        echo strtr("  Hello World  ", ["Hello" => "Bye", "World" => "All"]), '|', strtr("a1b22c-3", [1 => 'one', 22 => 'X', 2 => 'two', -3 => 'm3']), '|', strtr("abc", ['' => 'x', 'b' => 'B']), '|', strtr("abc", ['' => 'x']), '|', strtr("hi all", ["hi" => 1, "all" => 2.5, "h" => true]), '|', strtr("aaa", ["a" => "b", "aa" => "c"]), '|', strtr("ab", ["abc" => "x", "b" => "y"]), '|', strtr("05", ["05" => "five", 5 => "x"]), '|', strtr("abcabc", ["bc" => ""]), '|', strtr("xyz", "xz", "ZXq");"#);
    let unchanged = "string(10) \"HelloHello\" refcount(3)\n";
    let new = "string(10) \"HelloHello\" refcount(2)\n";
    let warning = "\nWarning: strtr(): Ignoring replacement of empty string in t.php on line 4\n";
    assert_eq!(
        out,
        format!(
            "{unchanged}{unchanged}{unchanged}{new}{new}{unchanged}{new}{unchanged}\
             string(10) \"HeoolHeool\" refcount(2)\n{unchanged}  Bye All  |aonebXcm3|{warning}aBc|{warning}abc|1 2.5|cb|ay|five|aa|ZyX"
        )
    );
}
