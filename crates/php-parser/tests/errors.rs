//! Syntax error messages compared with PHP's.
//!
//! The expected messages are cached in `tests/fixtures/error_messages.tsv`
//! (generated with `php -l` by the ignored `regenerate` test). Snippets only
//! use syntax that the installed PHP (8.3) and php-src master agree on;
//! known grammar differences are listed in `MASTER_OVERRIDES`.

use php_parser::ParseOptions;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const SNIPPETS: &[&str] = &[
    // statements
    "else;",
    "catch",
    "}",
    "echo 1 2;",
    "echo 1",
    "$a = 1 2;",
    "$a = ;",
    "if ($a 2) {}",
    "if (",
    "if 1",
    "if (1): foo(); else foo(); endif;",
    "if (1): else foo(); endif;",
    "if (1): foo(); ) endif;",
    "if (1): foo(); endif",
    "if (1) {} else: foo(); endif;",
    "if (1): foo(); case 1: endif;",
    "while (1 2) {}",
    "while (1): foo(); endfor;",
    "while 1",
    "do {} foo",
    "do {} while (1)",
    "do {} while 1;",
    "for ($i = 0 $i < 1;) {}",
    "for (;$a $b;) {}",
    "for (;; $a $b) {}",
    "for ($a; $b) {}",
    "for (;;): endwhile;",
    "foreach ($a $b) {}",
    "foreach ($a as) {}",
    "foreach ($a as $b 1) {}",
    "foreach ($a as $k => $b 1) {}",
    "foreach ($a as $k => ) {}",
    "foreach ($a as list($x) 1) {}",
    "switch (1) { foo }",
    "switch (1): foo endswitch;",
    "switch(1) x",
    "switch (1) { case 1 2: }",
    "switch (1) { default 1: }",
    "switch (1) { case 1: foo(); else }",
    "switch (1): case 1: foo(); } ",
    "break 1 2;",
    "continue 1 2;",
    "return 1 2;",
    "return",
    "global C;",
    "global $a 1;",
    "global $$;",
    "static $a 1;",
    "static 1;",
    "static $a = 1 2;",
    "unset($a 1);",
    "unset(1);",
    "unset($a, 1);",
    "unset FOO;",
    "unset(FOO);",
    "declare(ticks=1 2);",
    "declare(1);",
    "declare(ticks=1) 1",
    "declare(ticks=1): foo(); endif;",
    "try {} catch (A 1) {}",
    "try {} catch (A $e 1) {}",
    "try {} catch (1) {}",
    "try {} catch {}",
    "try foo();",
    "try {} finally 1",
    "goto 1;",
    "goto a 1;",
    "a: b: 1 2;",
    "__halt_compiler;",
    "__halt_compiler(1);",
    "function f() { __halt_compiler(); }",
    "const A;",
    "const 1 = 2;",
    "const A = 1 2;",
    "const A = 1, ;",
    "function f() { const A = 1; }",
    "function f() { use A; }",
    "function f() { namespace A; }",
    // namespaces and use
    "namespace ;",
    "namespace A",
    "namespace A\\B {",
    "namespace A { use }",
    "use ;",
    "use A\\{};",
    "use function ;",
    "use const 1;",
    "use A as;",
    "use A as 1;",
    "use A B;",
    "use A, ;",
    "use A\\{B, C {};",
    "use A\\{B,,C};",
    "use A\\{,B};",
    "use function A\\{B, function C};",
    "use A\\{function};",
    "use \\A\\{B};",
    // functions
    "function f() ;",
    "function f(1) {}",
    "function 1() {}",
    "function f($a $b) {}",
    "function f($a = 1 2) {}",
    "function f($a,,$b) {}",
    "function f(,) {}",
    "function f(int int $x) {}",
    "function f(int &1) {}",
    "function f(...1) {}",
    "function f(): {}",
    "function f(): ?1 {}",
    "function f(): A| {}",
    "function f(): (A) {}",
    "function f(): (A&B) {}",
    "function f() {",
    "function f() { return; ",
    "function &() {}",
    "function f() use ($a) {}",
    "$f = function() ;",
    "$f = function() use ($b 1) {};",
    "$f = function() use (1) {};",
    "$f = function() use () {};",
    "$f = function() use ($a) ;",
    "$f = fn() ;",
    "$f = fn() => ;",
    "$f = fn($a 1) => 1;",
    "$f = static 1;",
    "$f = static function;",
    "$f = #[A] 1;",
    "$f = #[A] static 1;",
    "#[A 1] function f() {}",
    "#[A(] function f() {}",
    "#[A, ] function f() {}",
    "#[A, 1] function f() {}",
    "#[] function f() {}",
    "#[A] 1;",
    "#[A] echo 1;",
    // classes
    "class { }",
    "class A { foo }",
    "class A { public foo }",
    "class A { public 1 }",
    "class A { public function }",
    "class A { public function 1() {} }",
    "class A { function f() }",
    "class A { function f(); }",
    "class A { function f() {} ",
    "class A { const }",
    "class A { const X }",
    "class A { const X = 1 2; }",
    "class A { const X = 1, }",
    "class A { const int }",
    "class A { use }",
    "class A { use B }",
    "class A { use B { } }",
    "class A { use B { foo } }",
    "class A { use B { foo as } }",
    "class A { use B { foo as 1; } }",
    "class A { use B { foo as bar } }",
    "class A { use B { A::foo } }",
    "class A { use B { A::foo insteadof } }",
    "class A { use B { A::foo insteadof C } }",
    "class A { use B { A:: } }",
    "class A { case }",
    "class A { case A 1 }",
    "class A { case A = 1 2 }",
    "class A { var }",
    "class A { var $a 1 }",
    "class A { public $a }",
    "class A { public $a, }",
    "class A { public $a = 1 2; }",
    "class A { public $a, $b = 1 2; }",
    "class A { public int }",
    "class A { #[A] }",
    "class A { #[A] use B; }",
    "class A extends {}",
    "class A extends B C {}",
    "class A implements {}",
    "class A implements B, {}",
    "class A implements B C {}",
    "class A extends B implements C extends D {}",
    "class A",
    "class A {",
    "interface A extends B C",
    "interface A extends {}",
    "enum A: int B",
    "enum A: {}",
    "enum A: int implements {}",
    "trait A B",
    "trait {}",
    "abstract foo",
    "abstract function f() {}",
    "final 1",
    "abstract final 1",
    "final abstract class X {}",
    "abstract abstract class X {}",
    "final final class X {}",
    "readonly readonly class X {}",
    "readonly function f() {}",
    "new class { foo };",
    "new class extends {};",
    "new class(1 2) {};",
    "new class 1;",
    "new abstract class {};",
    "new final class {};",
    "class A { public public $a; }",
    "class A { public private $a; }",
    "class A { static static $a; }",
    "class A { final abstract function f(); }",
    "class A { abstract $a; }",
    "class A { abstract const X = 1; }",
    "class A { static const X = 1; }",
    "class A { readonly function f() {} }",
    "class A { readonly const X = 1; }",
    "class A { final final function f() {} }",
    "class A { readonly readonly int $a; }",
    "class A { function __construct(static $a) {} }",
    "class A { function __construct(abstract $a) {} }",
    "class A { function __construct(public public $a) {} }",
    "class A { use B { foo as static; } }",
    "class A { use B { foo as abstract bar; } }",
    "class A { use B { foo as readonly; } }",
    // expressions
    "$a->;",
    "$a->1;",
    "$a?->;",
    "$a::;",
    "A::1;",
    "A::$1;",
    "$$;",
    "${;",
    "${1 2};",
    "$a[1 2];",
    "$a[;",
    "$a{1};",
    "$a = [1 2];",
    "$a = [;];",
    "$a = array(;);",
    "$a = array(1 2);",
    "$a = [1, 2;",
    "$a = array;",
    "$a = list;",
    "list($a) 1;",
    "list($a, ) 1;",
    "list(1 2) = $a;",
    "[$a] 1;",
    "$a = &new A;",
    "$a = & 1;",
    "$a =& $b 1;",
    "foo(1 2);",
    "foo(1, ;);",
    "foo(1,,2);",
    "foo(;);",
    "foo(a: 1 2);",
    "foo(a: );",
    "foo(new ;);",
    "foo(static);",
    "foo(print);",
    "foo(array);",
    "foo(list);",
    "foo(isset);",
    "foo(clone);",
    "foo(throw);",
    "foo(match);",
    "foo(include);",
    "foo(readonly);",
    "foo(exit 1);",
    "foo(&$a);",
    "foo(...);",
    "foo(... 1 2);",
    "new;",
    "new 1;",
    "new A 1;",
    "new A(1 2);",
    "new A->b;",
    "new static 1;",
    "new $a->b 1;",
    "new (1 2);",
    "A::b(1 2);",
    "A::{1 2};",
    "A::{1}(1 2);",
    "isset($a 1);",
    "isset($a, ;);",
    "isset();",
    "isset 1;",
    "empty($a 1);",
    "empty();",
    "eval(1 2);",
    "exit 1;",
    "exit(1 2);",
    "die(;);",
    "print;",
    "clone;",
    "yield 1 2;",
    "yield 1 => ;",
    "yield from;",
    "throw;",
    "include;",
    "match;",
    "match 1;",
    "match(1) 1;",
    "match(1) { ; };",
    "match(1) { 1 2 };",
    "match(1) { 1 => 2 3 };",
    "match(1) { 1, ; };",
    "match(1) { default 1 };",
    "match(1) { default => };",
    "match(1 2) {};",
    "1 ? 2;",
    "1 ? 2 : ;",
    "1 ?: ;",
    "1 == 2 == 3;",
    "1 < 2 > 3;",
    "1 === 2 != 3;",
    "1 <= 2 >= 3;",
    "1 <=> 2 == 3;",
    "1 instanceof;",
    "$a instanceof 1;",
    "$a instanceof A 1;",
    "1 = 2;",
    "FOO = 1;",
    "foo() = 1;",
    "($a) = 1;",
    "$a++++;",
    "++1;",
    "++$a++;",
    "--FOO;",
    "(int);",
    "(int) 1 2;",
    "@;",
    "!;",
    "-;",
    "$a + ;",
    "$a . ;",
    "$a ?? ;",
    "$a and ;",
    "$a || ;",
    "$a ** ;",
    "$a = 1 +;",
    "$a ? : ;",
    "(1;",
    "(1 2);",
    "();",
    "[1, 2] = ;",
    "__LINE__::foo();",
    "__FILE__();",
    "FOO::;",
    "static::;",
    "static;",
    "1();",
    "'a' 'b';",
    "\"a\" \"b\";",
    "\"$a[1 2]\";",
    "\"$a[]\";",
    "\"$a[-a]\";",
    "\"${a 1}\";",
    "\"{$a 1}\";",
    "\"{$a->b 1}\";",
    "\"{$f = 'f'}\";",
    "\"{$g{'h'}}\";",
    "\"$a->1\";",
    "\"${a[1 2]}\";",
    "\"$a\" 1;",
    "<<<A\n$a\nA 1;",
    "<<<A\nfoo\nA 1;",
    "`$a` 1;",
    "$a = <<<A\n{$a 1}\nA;\n",
    "$a = 0x;",
    "$a = 1_;",
    "$a = 08;",
    "$a = 1.2.3;",
    "$a = '",
    "$a = \"",
    "$a = \"$a",
    "/* unterminated",
    "$a = (unset) 1;",
    "$a = (real) 1;",
    "$a = (void) 1;",
    "echo <<<A\n  a\n b\n  A;\n",
    "echo \"\\u{zz}\";",
    "$a \x01;",
    "$a = 1 \x7f;",
    "$a \\ $b;",
    "\\;",
    "namespace\\;",
    "A\\;",
    "foo(1)(2) 3;",
    "$a->b(1 2);",
    "$a->b()->c 1;",
    "$a?->b 1;",
    "$a->{1 2};",
    "$a::$b 1;",
    "$a::b 1;",
    "$a::b() 1;",
    "$a = [1, 2][0] 1;",
    "$a = 'abc'[0] 1;",
    "$a = function() {} 1;",
    "$a = fn() => 1 2;",
    "$a = match(1) {} 1;",
    "$a = new class {} 1;",
    "$a = (new A) 1;",
    "list() = $a 1;",
    "[,] = $a;",
    "$a = [,];",
    "$a = [1,,];",
    "[$a,,] = $b;",
    "$a = [...];",
    "$a = [... 1 2];",
    "$a = [1 => &];",
    "$a = [&1];",
    "$a = [&$b 1];",
    "$a = [&$b => 1];",
    "$a = [1 => list($b) 1];",
    "$x = 1 ? 2 : 3 ? 4 : 5;",
    "echo <?php",
    "?><?= 1 2 ?>",
    "?><?= ?>",
    "<?php",
    "$a = <<<'A'\nfoo\n",
];

/// Snippets whose message differs between the oracle PHP (8.3) and
/// php-src master, with the master message.
const MASTER_OVERRIDES: &[(&str, &str)] = &[
    (
        "foreach ($a as $b 1) {}",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "foreach ($a as $k => $b 1) {}",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "unset($a 1);",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "unset(FOO);",
        "Parse error: syntax error, unexpected token \")\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "--FOO;",
        "Parse error: syntax error, unexpected token \";\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "\"{$a 1}\";",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "\"{$a->b 1}\";",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    // php-src 780a8280d23 removed `array_object_dereferenceable '{' expr '}'`
    // from the grammar, so "{" left the expected-token list after a variable
    // (Zend/tests/grammar/alternative_offset_syntax_in_encaps_string.phpt,
    // Zend/tests/new_without_parentheses/unset_new.phpt) and `"{$g{'h'}}"`
    // is a syntax error instead of the 8.x compile error.
    (
        "\"{$f = 'f'}\";",
        "Parse error: syntax error, unexpected token \"=\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "\"{$g{'h'}}\";",
        "Parse error: syntax error, unexpected token \"{\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "$a = <<<A\n{$a 1}\nA;\n",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 2",
    ),
    (
        "$a = [&$b 1];",
        "Parse error: syntax error, unexpected integer \"1\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "$a = [&$b => 1];",
        "Parse error: syntax error, unexpected token \"=>\", expecting \"->\" or \"?->\" or \"[\" on line 1",
    ),
    (
        "function f(): (A) {}",
        "Parse error: syntax error, unexpected token \")\", expecting token \"&\" on line 1",
    ),
    (
        "$f = function() use (1) {};",
        "Parse error: syntax error, unexpected integer \"1\", expecting variable or \"&\" or token \"&\" on line 1",
    ),
    (
        "$f = function() use () {};",
        "Parse error: syntax error, unexpected token \")\", expecting variable or \"&\" or token \"&\" on line 1",
    ),
    (
        "class A { abstract $a; }",
        "OK",
    ),
    (
        "$a = &new A;",
        "Parse error: syntax error, unexpected token \";\", expecting \"(\" on line 1",
    ),
    (
        "die(;);",
        "Parse error: syntax error, unexpected token \";\" on line 1",
    ),
    (
        "$a = (void) 1;",
        "Parse error: syntax error, unexpected token \"(void)\" on line 1",
    ),
    (
        "$a{1};",
        "Parse error: syntax error, unexpected token \"{\" on line 1",
    ),
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/error_messages.tsv")
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn ours(snippet: &str) -> String {
    let src = format!("<?php {snippet}");
    match php_parser::parse(src.as_bytes(), &ParseOptions::default()) {
        Ok(_) => "OK".to_owned(),
        Err(e) => format!("{}: {} on line {}", e.label(), e.message, e.line),
    }
}

#[test]
#[ignore = "needs /usr/bin/php; regenerates tests/fixtures/error_messages.tsv"]
fn regenerate() {
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("php-parser-errors-{}.php", std::process::id()));
    let mut out = String::new();
    for s in SNIPPETS {
        std::fs::write(&tmp, format!("<?php {s}")).unwrap();
        let o = Command::new("php")
            .args(["-n", "-l", "-d", "display_errors=1", "-d", "log_errors=0"])
            .arg(&tmp)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&o.stdout);
        let fname = tmp.to_string_lossy().into_owned();
        let mut result = "OK".to_owned();
        for line in stdout.lines() {
            for label in ["Parse error: ", "Fatal error: "] {
                if let Some(rest) = line.strip_prefix(label) {
                    result = format!("{label}{}", rest.replace(&format!(" in {fname}"), ""));
                }
            }
        }
        let _ = writeln!(out, "{}\t{}", escape(s), result);
    }
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(fixture(), out).unwrap();
}

#[test]
fn messages_match_php() {
    let Ok(data) = std::fs::read_to_string(fixture()) else {
        eprintln!("no fixture; skipping");
        return;
    };
    let mut failures = String::new();
    let mut n = 0;
    for line in data.lines() {
        let Some((snip, expected)) = line.split_once('\t') else {
            continue;
        };
        let Some(snippet) = SNIPPETS.iter().find(|s| escape(s) == snip) else {
            continue;
        };
        n += 1;
        let expected = MASTER_OVERRIDES
            .iter()
            .find(|(s, _)| s == snippet)
            .map_or(expected, |(_, m)| m);
        let got = ours(snippet);
        if got != expected {
            let _ = writeln!(failures, "{snip}\n    php:  {expected}\n    ours: {got}");
        }
    }
    assert!(n > 0);
    assert!(failures.is_empty(), "mismatches:\n{failures}");
}

/// Compile-time checks around an empty dimension (`zend_compile.c`):
///
/// * `zend_delayed_compile_dim()` rejects `$GLOBALS[]` with "Cannot append to
///   $GLOBALS" before compiling the base, in every fetch mode (read, write,
///   isset, unset, function argument), and the fetch type reaches the base
///   unchanged through `zend_delayed_compile_var()`, so an `[]` inside an
///   `unset()` chain is "Cannot use [] for unsetting".
/// * `zend_compile_isset_or_empty()` and `zend_compile_unset()` test
///   `is_global_var_fetch()` first, so the direct `isset($GLOBALS[])`,
///   `empty($GLOBALS[])` and `unset($GLOBALS[])` keep the generic `[]`
///   messages, and `isset()` of a call fails before its operand is compiled
///   (`zend_is_variable()` excludes calls).
/// * A constant-object chain (`ZEND_CONST_OBJECT_FETCH`) compiles in any
///   fetch type, so the same `[]` rules apply inside it.
///
/// Messages and lines verified with php 8.3.6; the two `A::B->c[]` cases
/// follow master, where 8.3 still rejects the whole chain.
#[test]
fn empty_dimension_messages() {
    const APPEND: &str = "Fatal error: Cannot append to $GLOBALS on line 1";
    const READING: &str = "Fatal error: Cannot use [] for reading on line 1";
    const UNSETTING: &str = "Fatal error: Cannot use [] for unsetting on line 1";
    let cases: &[(&str, &str)] = &[
        ("$x = $GLOBALS[];", APPEND),
        ("echo $GLOBALS[];", APPEND),
        ("$GLOBALS[];", APPEND),
        ("$x = $GLOBALS[] ?? 1;", APPEND),
        ("$GLOBALS[] ??= 1;", APPEND),
        ("$x = $GLOBALS[][\"a\"];", APPEND),
        ("$x = $GLOBALS[]->a;", APPEND),
        ("$x = $GLOBALS[]::$y;", APPEND),
        ("$x = ${$GLOBALS[]};", APPEND),
        ("$x = fn() => $GLOBALS[];", APPEND),
        ("f(...$GLOBALS[]);", APPEND),
        ("f(1, $GLOBALS[]);", APPEND),
        ("isset($GLOBALS[][\"a\"]);", APPEND),
        ("isset($GLOBALS[]->a);", APPEND),
        ("empty($GLOBALS[]->a);", APPEND),
        ("empty($GLOBALS[]());", APPEND),
        ("unset($GLOBALS[][\"a\"]);", APPEND),
        ("unset(${$GLOBALS[]});", APPEND),
        ("$GLOBALS[] = 1;", APPEND),
        ("$GLOBALS[]++;", APPEND),
        ("$x = &$GLOBALS[];", APPEND),
        ("[$GLOBALS[]] = [1];", APPEND),
        ("foreach ([1] as $GLOBALS[]) {}", APPEND),
        // The direct forms of isset/empty/unset.
        ("isset($GLOBALS[]);", READING),
        ("isset($x, $GLOBALS[]);", READING),
        ("empty($GLOBALS[]);", READING),
        ("unset($GLOBALS[]);", UNSETTING),
        ("unset($x, $GLOBALS[]);", UNSETTING),
        (
            "isset($GLOBALS[]());",
            "Fatal error: Cannot use isset() on the result of an expression (you can use \"null !== expression\" instead) on line 1",
        ),
        // Not a `$GLOBALS` fetch: `$$GLOBALS[]` is `${$GLOBALS}[]`.
        ("$x = $$GLOBALS[];", READING),
        ("$x = $a[];", READING),
        ("$x = $a[]->b;", READING),
        ("isset($a[]);", READING),
        ("isset($a[][\"b\"]);", READING),
        ("unset($a[]::$b);", READING),
        // The unset fetch type reaches the base of the chain.
        ("unset($a[]);", UNSETTING),
        ("unset($a[][\"b\"]);", UNSETTING),
        ("unset($a[][\"b\"][\"c\"]);", UNSETTING),
        ("unset($a[]->b);", UNSETTING),
        ("unset($a->b[]->c);", UNSETTING),
        ("unset($this[]->a);", UNSETTING),
        ("unset(A::$b[][0]);", UNSETTING),
        ("unset($GLOBALS[\"x\"][]);", UNSETTING),
        ("class A { const B = 1; } unset(A::B->c[][0]);", UNSETTING),
        // Appending inside a write chain is fine.
        ("$a[][\"b\"] = 1;", "OK"),
        ("$a[][\"b\"] .= 1;", "OK"),
        ("$x = &$a[][\"b\"];", "OK"),
        ("f($a[][\"b\"]);", "OK"),
        ("class A { const B = 1; } A::B->c[][0] = 1;", "OK"),
        // Lines: the error is reported at the line of the dimension's base.
        (
            "$x =\n  $GLOBALS\n  [];",
            "Fatal error: Cannot append to $GLOBALS on line 2",
        ),
        (
            "isset(\n  $GLOBALS\n  []\n);",
            "Fatal error: Cannot use [] for reading on line 2",
        ),
        (
            "isset(\n  $GLOBALS\n  []\n  [\"a\"]\n);",
            "Fatal error: Cannot append to $GLOBALS on line 2",
        ),
        (
            "unset(\n  $a\n  []\n  [\"b\"]\n);",
            "Fatal error: Cannot use [] for unsetting on line 2",
        ),
    ];
    let mut failures = String::new();
    for (snippet, expected) in cases {
        let got = ours(snippet);
        if got != *expected {
            let _ = writeln!(failures, "{snippet}\n    php:  {expected}\n    ours: {got}");
        }
    }
    assert!(failures.is_empty(), "mismatches:\n{failures}");
}

/// A closure's `use` variables live in its static-variables table
/// (`zend_compile_closure_uses()`), so redeclaring one as `static` is a
/// duplicate (`Zend/tests/static_variables/static_variables_closure_bind.phpt`).
#[test]
fn static_redeclaring_closure_use_is_duplicate() {
    let src = b"<?php\n$a = null;\nfunction () use (&$a) {\n    static $a;\n};\n";
    let e = php_parser::parse(src, &ParseOptions::default()).expect_err("duplicate static");
    assert_eq!(e.message, "Duplicate declaration of static variable $a");
    assert_eq!(e.line, 4);
    // A use variable and a different static are fine.
    let ok = b"<?php\nfunction () use ($a) { static $b; };\n";
    assert!(php_parser::parse(ok, &ParseOptions::default()).is_ok());
}

#[test]
fn parser_action_compile_errors_are_thrown() {
    // zend_throw_exception(zend_ce_compile_error, ...) in the parser
    // actions; post-parse compile checks stay fatal.
    for src in [
        &b"<?php class C { final final function foo() {} }"[..],
        b"<?php class C { private protected $x; }",
        b"<?php if (true) { __HALT_COMPILER(); }",
        b"<?php declare(encoding=[]);",
    ] {
        let e = php_parser::parse(src, &ParseOptions::default()).expect_err("compile error");
        assert_eq!(e.kind, php_parser::ErrorKind::Compile);
        assert!(e.thrown, "{}", e.message);
    }
    let e = php_parser::parse(
        b"<?php $a = null;\nfunction () use (&$a) {\n    static $a;\n};\n",
        &ParseOptions::default(),
    )
    .expect_err("duplicate static");
    assert!(!e.thrown);
}
