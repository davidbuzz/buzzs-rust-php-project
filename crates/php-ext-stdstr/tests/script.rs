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

fn run_with(ini: &[(&str, &str)], code: &str) -> String {
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
        &[("display_errors", "1"), ("memory_limit", "8M")],
        "echo strlen(str_repeat('ab', 1000)), \"\\n\"; str_pad('x', 100000000);",
    );
    assert_eq!(
        out,
        "2000\nPHP Fatal error:  Allowed memory size of 8388608 bytes exhausted (tried to allocate 100000032 bytes) in t.php on line 1\n"
            .replace("PHP Fatal error:  ", "\nFatal error: ")
    );
}
