// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tests of the JIT: every script runs interpreted and compiled, and the
//! two must print the same bytes (the interpreter's output is what the
//! tape's own tests and the PHPT suite pin down).
#![allow(
    clippy::needless_raw_string_hashes,
    clippy::float_cmp,
    clippy::format_push_string,
    clippy::double_comparisons,
    clippy::single_match_else
)]

use super::{
    float_test, int_cc, leaves, loop_heads, new_record, probe, Compiler, Entry, FloatTest,
    HB_WORDS, REC_ENTRIES,
};
use crate::exec::tape::{lower, TOp, WHY_EXIT, WHY_STEP};
use crate::jit::{configure, options, stats, Options};
use crate::opcode::cmp_outcome;
use crate::{Vm, VmOptions};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
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

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs `src` interpreted, then with every instruction compiled by the
/// first tier alone, then with the second tier as well; checks that
/// something was compiled, that nothing failed to compile and that all
/// three runs print the same. Returns the output.
fn both(src: &str) -> String {
    three(src).0
}

/// [`both`], also returning how many loops the second tier compiled.
fn three(src: &str) -> (String, u64) {
    let saved = options();
    configure(Options {
        enabled: false,
        ..saved
    });
    let interpreted = run(src);
    // A script that dies on a function this bare VM does not have (the
    // extensions' functions: `json_encode`) would compare nothing.
    assert!(
        !interpreted.contains("Call to undefined function") && !interpreted.contains("not found"),
        "{interpreted}"
    );
    let mut typed_loops = 0;
    for registers in [false, true] {
        configure(Options {
            enabled: true,
            everything: true,
            first_tier: true,
            registers,
            hot_loop: 1,
        });
        let before = stats();
        let compiled = run(src);
        let after = stats();
        configure(saved);
        assert_eq!(after.failed, before.failed, "a tape failed to compile");
        // `PHPRUST_JIT` in the environment overrides `configure`.
        if std::env::var_os("PHPRUST_JIT").is_none() {
            assert!(after.compiled > before.compiled, "nothing was compiled");
            assert!(after.code_bytes > before.code_bytes);
            if !registers {
                assert_eq!(after.typed_loops, before.typed_loops);
            }
        }
        assert_eq!(
            interpreted, compiled,
            "registers: {registers}, script:\n{src}"
        );
        typed_loops = after.typed_loops - before.typed_loops;
    }
    (interpreted, typed_loops)
}

#[test]
fn layout_probe_holds() {
    let none = probe().expect("the Value layout the JIT relies on");
    assert!(none > super::TAG_LAST_SCALAR);
}

#[test]
fn compiler_starts_on_this_host() {
    assert!(Compiler::new().is_some());
}

#[test]
fn condition_tables() {
    assert_eq!(int_cc(1), IntCC::SignedLessThan);
    assert_eq!(int_cc(3), IntCC::SignedLessThanOrEqual);
    assert_eq!(int_cc(4), IntCC::SignedGreaterThan);
    assert_eq!(int_cc(6), IntCC::SignedGreaterThanOrEqual);
    assert_eq!(int_cc(2), IntCC::Equal);
    assert_eq!(int_cc(5), IntCC::NotEqual);
    // Every float mask against the outcome it is defined by.
    let pairs = [
        (1.0_f64, 2.0_f64, cmp_outcome::LESS),
        (2.0, 2.0, cmp_outcome::EQUAL),
        (3.0, 2.0, cmp_outcome::GREATER),
        (f64::NAN, 2.0, cmp_outcome::UNORDERED),
    ];
    for mask in 0..16_u8 {
        for (x, y, outcome) in pairs {
            let want = mask & outcome != 0;
            let holds = |cc| match cc {
                FloatCC::LessThan => x < y,
                FloatCC::Equal => x == y,
                FloatCC::LessThanOrEqual => x <= y,
                FloatCC::GreaterThan => x > y,
                FloatCC::GreaterThanOrEqual => x >= y,
                FloatCC::Ordered => !x.is_nan() && !y.is_nan(),
                FloatCC::Unordered => x.is_nan() || y.is_nan(),
                FloatCC::NotEqual => x != y,
                other => panic!("{other:?} is not lowered by every backend"),
            };
            let got = match float_test(mask | cmp_outcome::IDENTITY) {
                FloatTest::Always(always) => always,
                FloatTest::Cc { cc, negate } => holds(cc) != negate,
                FloatTest::Apart { negate } => (x < y || x > y) != negate,
            };
            assert_eq!(got, want, "mask {mask} on {x} ? {y}");
        }
    }
}

/// The compiled code of a tape with nothing specialised stops where the
/// interpreter's loop would, whatever instruction it is entered at.
#[test]
fn compiled_code_reports_where_it_stopped() {
    let src = "<?php echo 'a'; echo 'b';";
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = crate::compiler::CompileOptions {
        filename: "t.php".into(),
        ..crate::compiler::CompileOptions::default()
    };
    let unit = crate::compiler::compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
    let code = &unit.main.code;
    let mut tape = lower(code);
    assert_eq!(
        loop_heads(&tape, &code.ops, &|i| !leaves(&tape, &code.ops, i)),
        Vec::new()
    );
    let original = tape.clone();
    let entries: Vec<Entry> = (0..tape.len())
        .map(|at| Entry {
            at,
            end: 0,
            typed: false,
            native: true,
        })
        .collect();
    let native = new_record(&tape, &entries, 1).expect("record");
    let mut compiler = Compiler::new().expect("compiler");
    // SAFETY: the record was just made for this tape.
    let size = unsafe { compiler.compile_into(code, native.as_ptr()) }.expect("code");
    assert!(size > 0);
    // As `install` does.
    for (i, inst) in tape.iter_mut().enumerate() {
        inst.op = TOp::Native;
        inst.a = u32::try_from(i).expect("index");
        inst.b = u32::try_from(REC_ENTRIES + HB_WORDS * i).expect("block");
        inst.p = native.as_ptr() as usize;
    }
    let mut frame = crate::vm::Frame::new(code.clone(), crate::vm::FrameKind::Main);
    for (inst, was) in tape.iter().zip(&original) {
        // SAFETY: no instruction of this tape touches a slot: each is one
        // `run_core` returns at.
        let out = unsafe { super::enter_out(&raw mut frame, std::ptr::from_ref(inst)) };
        assert!(std::ptr::eq(out.ip, inst));
        assert!(std::ptr::eq(out.at, inst));
        match was.op {
            TOp::Exit => assert_eq!(out.why, WHY_EXIT),
            TOp::Step => assert_eq!(out.why, WHY_STEP),
            _ => {}
        }
    }
}

#[test]
fn int_loops() {
    let out = both(
        r#"<?php
function sum($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $i; } return $s; }
function down($n) { $c = 0; for ($i = $n; $i > 0; $i--) { $c++; } return $c; }
function mixed($n) { $s = 0; $i = 0; while ($i <= $n) { $s = $s + $i * 2 - 1; $i = $i + 1; } return $s; }
function ne($n) { $c = 0; for ($i = 0; $i != $n; $i++) { $c = $c + 3; } return $c; }
echo sum(1000), " ", down(77), " ", mixed(50), " ", ne(9), "\n";
for ($i = 10; $i >= 0; $i -= 3) { echo $i, ","; }
echo "\n";
"#,
    );
    assert_eq!(out, "499500 77 2499 27\n10,7,4,1,\n");
}

#[test]
fn float_loops_and_widening() {
    both(
        r#"<?php
function f($n) { $x = 1.0; $y = 0; for ($i = 0; $i < $n; $i++) { $x = $x * 1.01 + $i; $y += 0.5; $y -= $i; $y *= 1.5; } return [$x, $y]; }
var_dump(f(40));
$i = 0.5; $n = 0;
while ($i < 10) { $i++; $n++; }
var_dump($i, $n);
$d = 3.0; while ($d > -2) { $d--; }
var_dump($d);
$a = 1; $a += 1.5; var_dump($a);
$b = 2.5; $b *= 2; var_dump($b);
$c = 7; $t = $c - 0.5; var_dump($t); $t = 0.25 + $c; var_dump($t);
"#,
    );
}

#[test]
fn int_overflow_declines_to_the_float_result() {
    let out = both(
        r#"<?php
$m = PHP_INT_MAX - 2;
for ($i = 0; $i < 5; $i++) { $m++; }
var_dump($m);
$m = PHP_INT_MIN + 2;
for ($i = 0; $i < 5; $i++) { $m--; }
var_dump($m);
$a = PHP_INT_MAX; $b = 1;
for ($i = 0; $i < 2; $i++) { $t = $a + $b; var_dump($t); $t = $a * 2; var_dump($t); $t = -$a - 3; var_dump($t); }
$s = PHP_INT_MAX - 1;
for ($i = 0; $i < 4; $i++) { $s += 1; var_dump($s); }
$p = 3037000500;
for ($i = 0; $i < 2; $i++) { $p *= $p; var_dump($p); }
for ($i = PHP_INT_MAX - 2; $i < PHP_INT_MAX; $i++) { echo "i\n"; }
$k = PHP_INT_MAX - 1; $n = 0;
while ($n < 3) { $n++; $j = $k++; var_dump($j); $j = ++$k; var_dump($j); }
"#,
    );
    assert!(out.contains("float(9.223372036854776E+18)"), "{out}");
}

#[test]
fn every_comparison_on_every_pair_of_kinds() {
    // Each comparison as a smart branch on variables, temporaries, small
    // and other literals, over ints, floats (NaN and infinities included)
    // and things that are not numbers (declined to the generic op).
    let mut src = String::from("<?php\nfunction v() { return [0, 1, -1, 5, PHP_INT_MAX, PHP_INT_MIN, 0.0, -0.0, 1.0, 1.5, 5.0, NAN, INF, -INF, 9.2233720368547758E+18, '5', '1.5', 'x', null, true, false, [], [1]]; }\n");
    for (name, op) in [
        ("lt", "<"),
        ("le", "<="),
        ("gt", ">"),
        ("ge", ">="),
        ("eq", "=="),
        ("ne", "!="),
        ("id", "==="),
        ("ni", "!=="),
    ] {
        src.push_str(&format!(
            r#"function {name}($a, $b) {{
    $r = '';
    if ($a {op} $b) {{ $r .= 'a'; }} else {{ $r .= '.'; }}
    if ($a {op} 5) {{ $r .= 'b'; }} else {{ $r .= '.'; }}
    if ($a {op} 1.5) {{ $r .= 'c'; }} else {{ $r .= '.'; }}
    if ($a + 0 {op} $b) {{ $r .= 'd'; }} else {{ $r .= '.'; }}
    if ($a {op} $b + 0) {{ $r .= 'e'; }} else {{ $r .= '.'; }}
    if ($a * 1 {op} $b * 1) {{ $r .= 'f'; }} else {{ $r .= '.'; }}
    if ($a - 0 {op} 5) {{ $r .= 'g'; }} else {{ $r .= '.'; }}
    if ($a - 0 {op} 9000000000) {{ $r .= 'h'; }} else {{ $r .= '.'; }}
    $n = 0;
    while (!($a {op} $b) && $n < 2) {{ $n++; }}
    return $r . $n;
}}
"#
        ));
    }
    src.push_str(
        r#"
foreach (['lt', 'le', 'gt', 'ge', 'eq', 'ne', 'id', 'ni'] as $f) {
    echo $f, ":";
    foreach (v() as $a) {
        foreach (v() as $b) {
            try { echo $f($a, $b), " "; } catch (Throwable $e) { echo get_class($e), " "; }
        }
    }
    echo "\n";
}
"#,
    );
    both(&src);
}

#[test]
fn every_arithmetic_form_on_every_pair_of_kinds() {
    let mut src = String::from("<?php\nfunction v() { return [0, 1, -1, 7, PHP_INT_MAX, PHP_INT_MIN, 4611686018427387904, 0.0, 1.5, -2.25, NAN, INF, 1e308, '5', 'x', null, true, [1]]; }\n");
    for (name, op) in [("add", "+"), ("sub", "-"), ("mul", "*")] {
        src.push_str(&format!(
            r#"function {name}($a, $b) {{
    $o = [];
    $t = $a {op} $b; $o[] = $t;
    $o[] = $a {op} 3;
    $o[] = 3 {op} $a;
    $o[] = $a {op} 2.5;
    $o[] = 9000000000 {op} $a;
    $o[] = ($a {op} 1) {op} $b;
    $o[] = $a {op} ($b {op} 1);
    $o[] = ($a {op} 1) {op} ($b {op} 1);
    $o[] = ($a {op} 1) {op} 3;
    $o[] = 2.5 {op} ($a {op} 1);
    $c = $a; $c {op}= $b; $o[] = $c;
    $c = $a; $c {op}= 3; $o[] = $c;
    $c = $a; $c {op}= 0.5; $o[] = $c;
    $c = $a; $c {op}= $b {op} 1; $o[] = $c;
    $c = $a; $c = $c {op} $b; $o[] = $c;
    $c = 0; $c = $a {op} $b; $o[] = $c;
    $c = 'old'; $c = $a {op} 3; $o[] = $c;
    $c = $a; $o[] = $c++; $o[] = $c; $o[] = ++$c;
    $c = $a; $o[] = $c--; $o[] = $c; $o[] = --$c;
    $c = $a; $c++; $c++; $c--; $o[] = $c;
    return $o;
}}
"#
        ));
    }
    src.push_str(
        r#"
foreach (['add', 'sub', 'mul'] as $f) {
    foreach (v() as $a) {
        foreach (v() as $b) {
            try { echo var_export(array_map(fn($x) => var_export($x, true), $f($a, $b)), true), "\n"; }
            catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
        }
    }
}
"#,
    );
    both(&src);
}

#[test]
fn scalar_assignments_and_truth_jumps() {
    both(
        r#"<?php
function g($n) {
    $o = [];
    for ($i = 0; $i < $n; $i++) {
        $a = $i; $b = $a; $c = 1.5; $d = $c; $e = null; $f = $e; $g = true; $h = $g;
        $t = $i < 2; if ($t) { $o[] = 'lt'; }
        $u = !($i == 1); if (!$u) { $o[] = 'one'; }
        $k = 9000000000; $l = -7; $s = 'str'; $s = $i; $arr = [1]; $arr = $c;
        $o[] = [$a, $b, $c, $d, $f, $h, $k, $l, $s, $arr];
    }
    return $o;
}
var_dump(g(3));
$x = 0; $y = $x; $x = 5; var_dump($y, $x);
"#,
    );
}

#[test]
fn undefined_and_non_numeric_operands_decline() {
    let out = both(
        r#"<?php
function h() {
    $r = [];
    for ($i = 0; $i < 2; $i++) {
        $r[] = $u + 1;
        $s = "3"; $s++; $r[] = $s;
        $z = "abc"; $z++; $r[] = $z;
        $n = null; $n++; $r[] = $n;
        $m = null; $m--; $r[] = $m;
        $w += 2; $r[] = $w;
        if ($q < 3) { $r[] = 'q'; }
        $ref = 1; $p = &$ref; $ref++; $ref += 2; $r[] = $p;
    }
    return $r;
}
var_dump(h());
"#,
    );
    assert!(out.contains("Undefined variable $u"));
}

#[test]
fn family_instructions_and_calls() {
    both(
        r#"<?php
class P { public $x = 1; public static $n = 0; const K = 4; function inc() { $this->x += 2; return $this; } }
function fill($n) { $a = []; for ($i = 0; $i < $n; $i++) { $a[$i] = $i * $i; $a['k' . $i] = strlen("x$i"); } return $a; }
function total($a) { $s = 0; foreach ($a as $k => $v) { if (is_int($k)) { $s += $v; } elseif (isset($a[$k])) { $s -= 1; } } return $s; }
function idiv($a, $b) { return [$a % $b, intdiv($a, $b), $a / $b, $a & $b, $a | $b, $a ^ $b, $a << 2, $a >> 1]; }
$a = fill(20);
echo count($a), " ", total($a), "\n";
$p = new P;
for ($i = 0; $i < 10; $i++) { $p->inc(); P::$n += P::K; $p->x++; }
var_dump($p->x, P::$n, idiv(17, 5));
function fib($n) { return $n < 2 ? $n : fib($n - 1) + fib($n - 2); }
echo fib(15), "\n";
$s = ''; for ($i = 0; $i < 5; $i++) { $s .= $i . ','; }
echo $s, strlen($s), "\n";
$m = [[1, 2], [3, 4]]; $t = 0;
for ($i = 0; $i < 2; $i++) { for ($j = 0; $j < 2; $j++) { $t += $m[$i][$j]; $m[$i][$j]++; } }
var_dump($t, $m, $q ?? 'unset', isset($m[1][1]), empty($m[5]));
"#,
    );
}

#[test]
fn exceptions_generators_and_destructors_pass_through_compiled_code() {
    both(
        r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "d{$this->n} "; } }
function gen($n) { for ($i = 0; $i < $n; $i++) { $x = yield $i * 2; if ($x) { echo "got $x "; } } return 7; }
function thrower($i) { if ($i == 3) { throw new RuntimeException("at $i"); } return $i + 1; }
$g = gen(4);
foreach ($g as $v) { echo $v, " "; }
echo $g->getReturn(), "\n";
for ($i = 0; $i < 6; $i++) {
    $d = new D($i);
    try { $i = thrower($i); } catch (RuntimeException $e) { echo $e->getMessage(), " line ", $e->getLine(), " "; } finally { echo "f "; }
}
echo "\n";
function deep($n) { $s = 0; for ($i = 0; $i < 3; $i++) { $s += $n > 0 ? deep($n - 1) : 1; } return $s; }
echo deep(5), "\n";
echo intdiv(1, 0);
"#,
    );
}

/// The op arrays of `src` (main code first) and how many loops of each
/// are worth compiling.
fn worth(src: &str) -> Vec<usize> {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = crate::compiler::CompileOptions {
        filename: "t.php".into(),
        strlen_builtin: true,
        special_builtins: u32::MAX,
        ..crate::compiler::CompileOptions::default()
    };
    let unit = crate::compiler::compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
    let mut all = vec![unit.main.code.clone()];
    all.extend(unit.decls.functions.borrow().iter().map(|f| f.code.clone()));
    all.iter()
        .map(|code| {
            let tape = lower(code);
            loop_heads(&tape, &code.ops, &|i| !leaves(&tape, &code.ops, i)).len()
        })
        .collect()
}

#[test]
fn only_loops_that_stay_in_machine_code_are_worth_compiling() {
    // Main code: a loop around a call. `sum`: a numeric loop. `plus`: no
    // loop. `outer`: a loop around a call, and a numeric loop inside it.
    let worth = worth(
        r#"<?php
function sum($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $i; } return $s; }
function plus($a) { return $a + 1; }
function outer($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += plus($i); for ($j = 0; $j < 3; $j++) { $s += $j; } } return $s; }
for ($i = 0; $i < 3; $i++) { echo plus($i); }
"#,
    );
    assert_eq!(worth, [0, 1, 0, 1]);
}

#[test]
fn a_branching_instruction_without_a_target_is_not_a_loop() {
    // The fetch of `$h[$k] ?? 0` can branch but has no target of its own
    // here; the loop around it leaves at the `??` op on every round.
    let worth = worth(
        r#"<?php
$h = ['k' => 1]; $k = 'k';
for ($i = 0; $i < 5; $i++) { $x = $h[$k] ?? 0; }
"#,
    );
    assert_eq!(worth, [0]);
}

#[test]
fn default_selection_compiles_the_numeric_loop_only() {
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let saved = options();
    configure(Options {
        enabled: true,
        everything: false,
        first_tier: true,
        registers: false,
        hot_loop: 1,
    });
    let before = stats();
    let out = run(r#"<?php
function sum($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $i; } return $s; }
function plus($a) { return $a + 1; }
for ($i = 0; $i < 3; $i++) { echo plus($i), sum(10), " "; }
"#);
    let after = stats();
    configure(saved);
    assert_eq!(out, "145 245 345 ");
    assert_eq!(after.compiled, before.compiled + 1);
    assert_eq!(after.failed, before.failed);
}

/// Checks that the second tier compiles exactly `n` loops of `src` (and
/// that all three runs print the same). With `PHPRUST_JIT` in the
/// environment the options are not this test's to set: only the output is
/// compared.
fn typed_is(src: &str, n: u64) {
    let typed = typed(src);
    if std::env::var_os("PHPRUST_JIT").is_none() {
        assert_eq!(typed, n, "script:\n{src}");
    }
}

/// [`typed_is`] for at least `n` loops.
fn typed_at_least(src: &str, n: u64) {
    let typed = typed(src);
    if std::env::var_os("PHPRUST_JIT").is_none() {
        assert!(typed >= n, "{typed} typed loops, script:\n{src}");
    }
}

/// How many loops of `src` the second tier compiles, the output being the
/// same in all three runs.
///
/// The most of a few runs: the safe-point flag is process-wide, and while
/// a test on another thread keeps it raised every op of this one returns
/// to `Vm::run`, which then runs the loop's first op itself and never
/// reaches the entry instruction (nothing is compiled, and nothing is
/// wrong).
fn typed(src: &str) -> u64 {
    (0..12).map(|_| three(src).1).max().unwrap_or(0)
}

#[test]
fn numeric_loops_get_typed_code() {
    typed_is("<?php function f($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $i; } return $s; } echo f(100), f(5);", 1);
    // Floats, an int widened on every round, a literal of each kind.
    typed_is("<?php $x = 1.5; $k = 3; for ($i = 0; $i < 50; $i++) { $x = $x * 1.01 + $k; $x -= 2; } var_dump($x, $i);", 1);
    // A nest: the inner loop, then the outer one around it.
    typed_at_least("<?php $t = 0; for ($i = 0; $i < 30; $i++) { for ($j = 0; $j < 30; $j++) { $t = $t + $i * $j; } } echo $t;", 1);
    // `while` with a condition on temporaries and a countdown.
    typed_is("<?php $a = 0; $b = 1; $n = 40; while ($n-- > 0) { $c = $a + $b; $a = $b; $b = $c; } var_dump($a, $b, $n);", 1);
}

#[test]
fn loops_the_second_tier_leaves_alone() {
    // An array of strings: its elements are not numbers.
    typed_is("<?php $a = ['x', 'y', 'z']; for ($i = 0; $i < 3; $i++) { $t = $a[$i]; $n = $i * 2; } echo $t, $n;", 0);
    // A string operand when the loop is first reached.
    typed_is(
        "<?php $u = '5'; $s = 0; for ($i = 0; $i < 9; $i++) { $s = $s + $u; } var_dump($s);",
        0,
    );
    // A string on every round, whenever the loop is looked at.
    typed_is("<?php $u = '2'; $x = 1; for ($i = 0; $i < 40; $i++) { $x = $u * 3; $x -= $i; } var_dump($x);", 0);
}

#[test]
fn typed_loops_leave_exactly_where_the_types_stop_holding() {
    // An overflow in the middle of a round, at each kind of instruction:
    // the frame is exact at that instruction and the first tier redoes it.
    let (out, loops) = three(
        r#"<?php
function add($n) { $s = PHP_INT_MAX - 5; $c = 0; for ($i = 0; $i < $n; $i++) { $c = $c + 2; $s += $i; $c++; } return [$s, $c, $i]; }
function mul($n) { $p = 3; $c = 0; for ($i = 0; $i < $n; $i++) { $c -= 1; $p = $p * 7; } return [$p, $c, $i]; }
function inc() { $c = 0; $m = PHP_INT_MAX; for ($i = $m - 3; $i != 0; $i++) { $c++; if ($c > 6) { break; } } return [$i, $c]; }
function step() { $k = PHP_INT_MIN + 2; $c = 0; for ($i = 0; $i < 6; $i++) { $k--; $t = $k--; $c += 1; } return [$k, $t, $c]; }
var_dump(add(8), mul(40), inc(), step());
"#,
    );
    assert!(out.contains("float("), "{out}");
    if std::env::var_os("PHPRUST_JIT").is_none() {
        // Each loop once, and again for the floats an overflow left.
        assert!(loops >= 4, "{loops}");
    }
    // Types that change between two visits of the loop: the guards send
    // the later visits to the first tier.
    both(
        r#"<?php
function f($a, $n) { $s = $a; for ($i = 0; $i < $n; $i++) { $s = $s + $i; $s -= 1; } return $s; }
var_dump(f(1, 5), f(1.5, 5), f('7', 5), f(null, 3), f(true, 3), f(2, 5), f(2.5, 0));
function g($n) { for ($i = 0; $i < $n; $i++) { $u = $i * 2; } return $u ?? 'none'; }
var_dump(g(3), g(0), g(2.5));
"#,
    );
}

#[test]
fn typed_loops_agree_on_every_numeric_instruction() {
    // The loops of `every_arithmetic_form...` are straight-line code; here
    // each form sits in a loop of its own, for int and for float inputs,
    // with comparisons of every kind deciding the rounds.
    let mut src = String::from("<?php\n");
    for (name, op) in [("add", "+"), ("sub", "-"), ("mul", "*")] {
        src.push_str(&format!(
            r#"function {name}($a, $b, $n) {{
    $o = [];
    for ($i = 0; $i < $n; $i++) {{ $t = $a {op} $b; $a = $t {op} 1; $b {op}= 2; $u = 3 {op} $b; $v = $b {op} 2.5; $w = 9000000000 {op} $i; }}
    $o[] = [$a, $b, $t ?? 0, $u ?? 0, $v ?? 0, $w ?? 0];
    for ($i = $n; $i > 0; $i--) {{ $a {op}= $i; $c = $a; $d = $c++; $e = --$c; $f = $c--; $g = ++$c; }}
    $o[] = [$a, $c ?? 0, $d ?? 0, $e ?? 0, $f ?? 0, $g ?? 0];
    $i = 0; while ($i <= $n) {{ $i = $i + 1; if ($i == 2) {{ continue; }} if ($a != $b) {{ $b {op}= 0.5; }} if ($i >= 4) {{ break; }} }}
    $o[] = [$a, $b, $i];
    $k = 0; $j = $n; do {{ $k++; $j -= 1; }} while ($j > 0 && $k !== $j && $k < 3.5);
    $o[] = [$k, $j];
    return $o;
}}
"#
        ));
    }
    src.push_str(
        r#"
foreach (['add', 'sub', 'mul'] as $f) {
    foreach ([0, 1, -3, 7, PHP_INT_MAX, PHP_INT_MIN, 0.5, -2.25, NAN, INF, 1e308] as $a) {
        foreach ([2, -1, 1.5, PHP_INT_MAX] as $b) {
            foreach ([0, 1, 6] as $n) {
                echo var_export(array_map(fn($r) => array_map(fn($x) => var_export($x, true), $r), $f($a, $b, $n)), true), "\n";
            }
        }
    }
}
"#,
    );
    both(&src);
}

#[test]
fn a_loop_that_keeps_leaving_goes_back_to_the_interpreter() {
    // A string operand on every round: after `BAD_LIMIT` leaves the entry
    // interprets, and the result is the same either way.
    let out = both(
        r#"<?php
function f($n) { $s = 0; $u = "5"; for ($i = 0; $i < $n; $i++) { $s = $s + $u; $s -= 1; } return $s; }
var_dump(f(500), f(3));
"#,
    );
    assert_eq!(out, "int(2000)\nint(12)\n");
}

#[test]
fn typed_loops_take_the_scalar_family() {
    // `%`, shifts, bit operations, `/` with a float and truth jumps.
    typed_is("<?php $s = 0; for ($i = 0; $i < 200; $i++) { $s += $i * 2 % 7; $s = ($s ^ $i) & 1023; $s |= $i >> 1; $s = $s << 1; $s %= 100003; } var_dump($s);", 1);
    typed_is(
        "<?php $x = 100.0; $n = 9; while ($n) { $x = $x / 3 + $n / 2.0; $n--; } var_dump($x, $n);",
        1,
    );
    // What the fast path declines, met in the middle of a typed loop: the
    // generic op answers, warns or throws, from the exact frame.
    both(
        r#"<?php
function m($a, $d, $n) { $r = []; for ($i = 0; $i < $n; $i++) { try { $r[] = $a % $d; $d--; $a += 3; } catch (Throwable $e) { $r[] = get_class($e) . ':' . $e->getMessage(); $d--; } } return $r; }
function sh($a, $k, $n) { $r = []; for ($i = 0; $i < $n; $i++) { try { $r[] = $a << $k; $r[] = $a >> $k; $k += 31; } catch (Throwable $e) { $r[] = get_class($e); $k = -$k + 1; } } return $r; }
function dv($a, $d, $n) { $r = []; for ($i = 0; $i < $n; $i++) { try { $a = $a / $d; $d -= 0.5; $r[] = $a; } catch (Throwable $e) { $r[] = get_class($e); $d = 4.0; } } return $r; }
function tr($x, $n) { $c = 0; for ($i = 0; $i < $n; $i++) { if ($x) { $c++; } $x = $x - 1; if (!$x) { $c += 10; } } return [$c, $x]; }
function pure($a, $d) { $s = 0; for ($i = 0; $i < 6; $i++) { $s += $a % $d; $d -= 1; $s ^= $i; } return $s; }
echo var_export([m(17, 2, 6), m(PHP_INT_MIN, 1, 4), sh(5, 1, 6), sh(-9, 62, 4), dv(100.0, 1.0, 5), dv(7, 2.5, 4)], true), "\n";
echo var_export([tr(3, 6), tr(2.0, 5), tr(1.5, 4)], true), "\n";
var_dump(tr(NAN, 2));
foreach ([[17, 9], [17, 3], [PHP_INT_MIN, 4], [5, 2.5]] as [$a, $d]) {
    try { var_dump(pure($a, $d)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
"#,
    );
}

#[test]
fn a_loop_is_typed_for_the_types_it_settles_on() {
    // An int at first and a float after one round: no round can follow
    // another with the first types, so the loop is looked at again on its
    // second round.
    typed_is(
        "<?php $x = 1; for ($i = 0; $i < 9; $i++) { $x = $x * 1.5; } var_dump($x);",
        1,
    );
    // Typed for ints; the sum overflows into a float half way: compiled
    // once more, for the float.
    typed_is("<?php $s = PHP_INT_MAX - 40; for ($i = 0; $i < 60; $i++) { $s += $i; $s -= 1; } var_dump($s);", 2);
    // Called with ints, then floats, then ints again: two versions, then
    // the first tier.
    both(
        r#"<?php
function f($a, $n) { for ($i = 0; $i < $n; $i++) { $a = $a + $i; $a -= 1; } return $a; }
for ($r = 0; $r < 30; $r++) { $v = $r % 3 == 0 ? 1.5 : $r; echo f($v, 4), ' '; }
"#,
    );
}

#[test]
fn typed_loops_read_and_write_arrays_through_their_handlers() {
    typed_is("<?php $a = [1, 2, 3, 4]; $s = 0; for ($i = 0; $i < 4; $i++) { $s += $a[$i]; $v = $a[$i]; $s += $v * 2; } echo $s;", 1);
    // A sieve: writes, appends, `count()`, a truth test of an element.
    typed_at_least(
        r#"<?php
function sieve($n) {
    $f = [];
    for ($i = 0; $i <= $n; $i++) { $f[] = 1; }
    $c = 0;
    for ($i = 2; $i < count($f); $i++) {
        if ($f[$i]) { for ($k = $i + $i; $k <= $n; $k += $i) { $f[$k] = 0; } $c++; }
    }
    return $c;
}
echo sieve(200), sieve(30);
"#,
        2,
    );
    // Floats.
    typed_is("<?php $a = [0.5, 1.5, 2.5]; $s = 0.0; for ($r = 0; $r < 9; $r++) { $i = $r % 3; $x = $a[$i]; $a[$i] = $x * 1.5; $s += $x; } var_dump($s, $a);", 1);
    // What the handlers decline or answer differently, in the middle of a
    // typed loop: an offset outside the array, an element of another
    // type, a string offset, a shared array (copied on the write), an
    // array that is replaced by a number, a reference.
    both(
        r#"<?php
function rd($a, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $v = $a[$i]; $s += $i; if ($v) { $s += 1; } } return [$s, $v]; }
function wr($a, $n) { $b = $a; for ($i = 0; $i < $n; $i++) { $a[$i] = $i * 3; $a[] = $i; } return [$a, $b]; }
function mix($n) { $a = [1, 2, 3]; for ($i = 0; $i < $n; $i++) { $t = $a[$i % 3]; if ($i == 4) { $a = 7; } $u = $t + 1; } return [$a, $t, $u]; }
function key($a) { $s = 0; for ($i = 0; $i < 3; $i++) { $k = $i * 2; $s += $a[$k]; } return $s; }
function refs($n) { $a = [5, 6, 7]; $r = &$a[1]; for ($i = 0; $i < $n; $i++) { $v = $a[$i % 3]; $a[$i % 3] = $v + 1; } return $a; }
function len($s, $n) { $c = 0; for ($i = 0; $i < $n; $i++) { $c += strlen($s) + count([$i, $i]); } return $c; }
echo var_export([rd([3, 0, 5], 3), rd([1, 2, 'x', 4], 4), rd([1.5, 2, null, [1]], 4), wr([9, 9], 4), wr([], 3)], true), "\n";
echo var_export([key([0 => 1, 2 => 2, 4 => 3]), key([1, 2, 3, 4, 5]), refs(7), len('abc', 5)], true), "\n";
try { var_dump(mix(8)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
var_dump(rd([1, 2], 4));
"#,
    );
}

#[test]
fn array_short_cuts_fall_back_to_the_handlers() {
    // The helpers take an int offset into a packed array of numbers the
    // variable holds alone; everything else is the handler's.
    both(
        r#"<?php
function over($a, $n) { for ($i = 0; $i < $n; $i++) { $a[$i] = $i + 0.5; $v = $a[$i]; $a[$i] = $i; } return [$a, $v]; }
function neg($a) { $r = []; for ($i = -2; $i < 2; $i++) { $a[$i] = $i * 2; $x = $a[$i]; $r[] = $x; } return [$a, $r]; }
function grow($n) { $a = [0]; for ($i = 0; $i < $n; $i++) { $a[$i + 1] = $a[$i] + 2; } return $a; }
function shared($a, $n) { $b = $a; $c = [$a]; for ($i = 0; $i < $n; $i++) { $t = $a[$i]; $a[$i] = $t * 2; } return [$a, $b, $c]; }
function mixed($n) { $a = [1, 2.5, 'three', 4, null, true, [5]]; $s = 0; for ($i = 0; $i < $n; $i++) { $a[$i] = $i; $x = $a[($i + 1) % 7]; if (is_int($x)) { $s += $x; } } return [$a, $s]; }
function keyed($n) { $a = [3 => 1, 1 => 2, 'k' => 3]; $s = 0; for ($i = 1; $i < $n; $i += 2) { $s += $a[$i]; $a[$i] = $s; } return [$a, $s]; }
function swap($n) { $a = [5, 3, 9, 1, 7]; for ($p = 0; $p < $n; $p++) { for ($i = 0; $i < 4; $i++) { $j = $i + 1; $x = $a[$i]; $y = $a[$j]; if ($x > $y) { $a[$i] = $y; $a[$j] = $x; } } } return $a; }
echo var_export([over([1, 2, 3], 3), over(['a', 'b'], 2), neg([7, 8]), grow(5), shared([1, 2, 3], 3)], true), "\n";
echo var_export([mixed(7), keyed(4), swap(5)], true), "\n";
"#,
    );
    // The bubble sort's inner loop reads and writes through the helpers.
    typed_at_least("<?php $a = [5, 3, 9, 1, 7, 2, 8]; for ($p = 0; $p < 7; $p++) { for ($i = 0; $i < 6; $i++) { $j = $i + 1; $x = $a[$i]; $y = $a[$j]; if ($x > $y) { $a[$i] = $y; $a[$j] = $x; } } } echo var_export($a, true);", 1);
}

#[test]
fn typed_loops_take_nested_arrays_and_compound_updates() {
    // A matrix product: `$m[$i][$j]` reads and a write through the 2-D
    // helpers, `+=` on an element through its handler.
    typed_at_least(
        r#"<?php
function mul($a, $b, $n) {
    $c = [];
    for ($i = 0; $i < $n; $i++) { $row = []; for ($j = 0; $j < $n; $j++) { $row[] = 0; } $c[] = $row; }
    for ($i = 0; $i < $n; $i++) {
        for ($j = 0; $j < $n; $j++) {
            $s = 0;
            for ($k = 0; $k < $n; $k++) { $s += $a[$i][$k] * $b[$k][$j]; }
            $c[$i][$j] = $s;
        }
    }
    return $c;
}
var_dump(mul([[1, 2], [3, 4]], [[5, 6], [7, 8]], 2));
var_dump(mul([[1.5, 2.0], [3.0, 4.5]], [[0.5, 6.0], [7.0, 8.5]], 2));
"#,
        2,
    );
    // A histogram and running sums: `$a[$i] += x`, `$a[$i]++`, with and
    // without a used result.
    typed_at_least("<?php $h = [0, 0, 0, 0]; $t = 0; for ($i = 0; $i < 40; $i++) { $k = $i % 4; $h[$k] += $i; $h[$k]++; $t += $h[$k]--; $u = ++$h[3 - $k]; } var_dump($h, $t, $u);", 1);
    // What the helpers and handlers do not take, mid-loop: rows of
    // different kinds, a shared row, a missing row, a string element, an
    // element that overflows into a float, nested appends.
    both(
        r#"<?php
function rd($m, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { for ($j = 0; $j < $n; $j++) { $v = $m[$i][$j]; $s += $v; } } return $s; }
function wr($m, $n) { $row = $m[0]; $keep = $m; for ($i = 0; $i < $n; $i++) { for ($j = 0; $j < $n; $j++) { $m[$i][$j] = $i * $n + $j; $m[$i][$j] += 1; } } return [$m, $row, $keep]; }
function ops($a, $n) { for ($i = 0; $i < $n; $i++) { $a[$i] += PHP_INT_MAX - 2; $a[$i] *= 2; $a[$i] -= $i; $a[$i]++; $a[$i] %= 1000; } return $a; }
function inc($a, $n) { $r = []; for ($i = 0; $i < $n; $i++) { $x = $a[$i]++; $y = --$a[$i]; $r[] = $x; $r[] = $y; } return [$a, $r]; }
function deep($n) { $m = [[[1, 2], [3, 4]], [[5, 6], [7, 8]]]; $s = 0; for ($i = 0; $i < $n; $i++) { $s += $m[$i % 2][($i + 1) % 2][$i % 2]; $m[$i % 2][0][1] = $s; $m[1][1][] = $i; } return [$m, $s]; }
foreach ([[[1, 2], [3, 4]], [[1.5, 2], [3, 'x']], [[1, 2], 7], [[1, 2]], [[PHP_INT_MAX, 1], [1, 1]]] as $m) {
    try { var_dump(rd($m, 2)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    try { var_dump(wr($m, 2)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
var_dump(ops([1, 2.5, 3], 3), ops([1, 'x'], 1), inc([1, 2.5, PHP_INT_MAX], 3), deep(5));
try { var_dump(ops([1, 2, 'x'], 3)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
"#,
    );
}

#[test]
fn foreach_over_arrays_is_compiled() {
    // The first tier runs the step through its fast path; the second
    // keeps the value and the key in registers.
    typed_is("<?php function sum($a) { $s = 0; foreach ($a as $v) { $s += $v; } return $s; } echo sum([1, 2, 3, 4]), sum([5, 6]);", 1);
    typed_is("<?php $a = [1.5, 2.5, 4.0]; $s = 0.0; $w = 0; foreach ($a as $k => $v) { $s += $v * $k; $w += $k; } var_dump($s, $w);", 1);
    // Nested, with a numeric loop inside and an array write.
    typed_at_least("<?php $m = [[1, 2, 3], [4, 5, 6]]; $t = 0; $o = [0, 0, 0]; foreach ($m as $row) { foreach ($row as $j => $v) { $t += $v; $o[$j] += $v; for ($k = 0; $k < $v; $k++) { $t++; } } } var_dump($t, $o);", 1);
    // Everything a `foreach` can be given, and every way out of one.
    both(
        r#"<?php
function gen($n) { for ($i = 0; $i < $n; $i++) { yield $i => $i * 2; } }
class Bag { public $a = 1; public $b = 2.5; private $c = 3; }
function each($x) { $s = 0; $c = 0; foreach ($x as $k => $v) { if (is_int($v) || is_float($v)) { $s += $v; } $c++; if ($c > 6) { break; } if ($v === 'skip') { continue; } } return [$s, $c, $k ?? null, $v ?? null]; }
function byref($a) { foreach ($a as &$v) { $v = $v * 2; } unset($v); foreach ($a as $k => $v) { $a[$k] = $v + 1; } return $a; }
function grow($a) { $n = 0; foreach ($a as $v) { $a[] = $v; $n += $v; if ($n > 100) { break; } } return [$a, $n]; }
function twice($a) { $s = 0; foreach ($a as $x) { foreach ($a as $y) { $s += $x * $y; } } return $s; }
function keys($a) { $o = []; foreach ($a as $k => $v) { $o[] = $k; $o[] = $v; } return $o; }
function first($a) { foreach ($a as $v) { return $v; } return 'empty'; }
foreach ([[1, 2, 3], [1.5, 2.5], [], [1, 'skip', 3.5, null, [4]], ['a' => 1, 'b' => 2], [3 => 1, 1 => 2], [1, 2, 3, 4, 5, 6, 7, 8, 9], gen(4), new Bag] as $x) {
    var_dump(each($x));
}
var_dump(byref([1, 2, 3]), grow([1, 2, 3]), twice([1, 2, 3]), twice([0.5, 2]), keys([5, 6]), keys(['x' => 1.5, 3 => 2]), first([9]), first([]));
try { foreach (5 as $v) { echo $v; } } catch (Throwable $e) { echo get_class($e); }
foreach ([PHP_INT_MAX, 1, 2] as $v) { $big = ($big ?? 0) + $v; }
var_dump($big);
"#,
    );
}

#[test]
fn typed_loops_take_casts_and_ternaries() {
    // Ops with no tape instruction: such a loop is compiled for the
    // second tier's sake alone.
    typed_is("<?php $s = 0.0; $c = 0; for ($i = 1; $i < 60; $i++) { $f = (float)$i; $n = (int)($f * 1.7); $s += $f / 3; $c += $n; $t = $i > 30 ? 2 : 3; $c += $t; $u = $i % 2 ? $f : 1.5; $s += $u; } var_dump($s, $c);", 1);
    both(
        r#"<?php
function conv($x, $n) { $r = []; for ($i = 0; $i < $n; $i++) { $a = (int)$x; $b = (float)$x; $c = (int)($x * 1e18); $d = $i ? $x : -$x; $x = $x * 3 + 0.5; $r[] = $a; $r[] = $b; $r[] = $c; $r[] = $d; } return $r; }
var_dump(conv(1.5, 5), conv(7, 4), conv(-2.75, 3), conv(1e300, 2), conv(NAN, 2), conv(INF, 2), conv('12abc', 2), conv(null, 2), conv(true, 2));
"#,
    );
}

#[test]
fn a_loop_is_typed_after_it_has_gone_round() {
    // With the usual selection the loop is compiled, and then typed, in
    // the middle of its run: its temporaries still hold the last round's
    // numbers, which must not become types the next round has to meet.
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let saved = options();
    for hot_loop in [1, 5, 40] {
        let mut typed = 0;
        // (A few tries: see `typed`.)
        for _ in 0..5 {
            configure(Options {
                enabled: true,
                everything: false,
                first_tier: true,
                registers: true,
                hot_loop,
            });
            let before = stats();
            let out = run("<?php $n = 300; $a = [0, 1, 2, 3]; $s = 0; $t = 0; for ($i = 0; $i < $n; $i++) { $s = $a[$i & 3]; $t += $s * 2 - ($i % 3); } echo $s, ' ', $t;");
            typed = typed.max(stats().typed_loops - before.typed_loops);
            assert_eq!(out, "3 600");
        }
        assert_eq!(typed, 1, "hot_loop {hot_loop}");
    }
    configure(saved);
}

#[test]
fn typed_loops_read_and_write_properties() {
    typed_is("<?php class P { public $x = 1; public $f = 0.5; } $o = new P; $s = 0; for ($i = 0; $i < 30; $i++) { $o->x += $i; $v = $o->x; $s += $v + $o->x; $o->f = $o->f * 1.5; $o->x++; } var_dump($o->x, $o->f, $s);", 1);
    both(
        r#"<?php
class P { public $x = 1; public int $n = 0; public ?float $f = null; public $s = 'a'; private $h = 5;
  function run($k) { for ($i = 0; $i < $k; $i++) { $this->x = $this->x + $this->h; $this->n += $i; $this->h--; } return $this; } }
class M { private $d = ['x' => 3]; function __get($n) { return $this->d[$n] ?? 0; } function __set($n, $v) { $this->d[$n] = $v * 2; } }
function use_it($o, $k) { $t = 0; for ($i = 0; $i < $k; $i++) { $o->x += 2; $t += $o->x; $o->x--; $v = $o->x; $t -= $v; } return $t; }
function mixed($o, $k) { $r = []; for ($i = 0; $i < $k; $i++) { try { $o->n = $o->n * 3 + 1; $o->f = $i / 2; $o->s = $i; $o->dyn = $i; $r[] = $o->n; if ($i == 3) { $o->x = 'str'; } $w = $o->x; } catch (Throwable $e) { $r[] = get_class($e); } } return $r; }
var_dump((array)(new P)->run(6), use_it(new P, 5), use_it(new M, 5), mixed(new P, 6));
$o = new P; $o->x = PHP_INT_MAX - 3; var_dump(use_it($o, 4), $o->x, $o->n);
$r = 5; $o = new P; $o->x = &$r; var_dump(use_it($o, 3), $r);
"#,
    );
}

#[test]
fn typed_loops_take_bools_and_loop_exits() {
    // `a && b` as a loop condition, `!`, `(bool)`, `while (1)` with a
    // `return`, an assignment used as an expression.
    typed_at_least(
        r#"<?php
function mandel($w, $h) { $o = 0;
  for ($y = 0; $y <= $h; $y = $y + 1) { $imc = 0.1 * ($y - 5);
    for ($x = 0; $x <= $w; $x = $x + 1) { $rec = 0.05 * ($x - 20) - 0.45; $re = $rec; $im = $imc; $color = 50; $re2 = $re * $re; $im2 = $im * $im;
      while ((($re2 + $im2) < 1000000) && $color > 0) { $im = $re * $im * 2 + $imc; $re = $re2 - $im2 + $rec; $re2 = $re * $re; $im2 = $im * $im; $color = $color - 1; }
      if ($color == 0) { $o++; } else { $o += 2; } } }
  return $o; }
echo mandel(30, 10);
"#,
        1,
    );
    both(
        r#"<?php
function heap($n, $ra) { $l = ($n >> 1) + 1; $ir = $n;
  while (1) {
    if ($l > 1) { $rra = $ra[--$l]; } else { $rra = $ra[$ir]; $ra[$ir] = $ra[1]; if (--$ir == 1) { $ra[1] = $rra; return $ra; } }
    $i = $l; $j = $l << 1;
    while ($j <= $ir) { if (($j < $ir) && ($ra[$j] < $ra[$j + 1])) { $j++; } if ($rra < $ra[$j]) { $ra[$i] = $ra[$j]; $j += ($i = $j); } else { $j = $ir + 1; } }
    $ra[$i] = $rra; } }
function flags($a, $b, $n) { $r = []; for ($i = 0; $i < $n; $i++) { $p = $a < $i && $b > $i; $q = $a == $i || !$p; $t = (bool)($a - $i); $u = !$b; $v = $a === $i; $w = $a !== $b; $x = $a <= $b; $y = $a != $i;
    if ($p) { $r[] = 1; } if ($q && $t) { $r[] = 2; } if ($u || $v) { $r[] = 3; } if (!$w) { $r[] = 4; } if ($x) { $r[] = 5; } while ($y) { $r[] = 6; $y = false; } $a = $a + 0.5; } return $r; }
function first($a, $x) { $i = 0; while (true) { if ($a[$i] == $x) { return $i; } $i++; if ($i > 5) { return -1; } } }
var_dump(heap(9, [0, 5.5, 3.25, 9.0, 1.5, 7.75, 2.0, 8.5, 4.25, 6.0]), heap(6, [0, 6, 5, 4, 3, 2, 1]));
var_dump(flags(2, 4, 6), flags(2.5, 0, 4), flags(NAN, 1, 3), flags(0, NAN, 3), flags('x', 2, 2), flags(null, true, 2));
var_dump(first([3, 1, 4, 1, 5, 9], 4), first([3.5, 1.5], 9), first([1, 2, 3, 4, 5, 6, 7], 7));
"#,
    );
}

#[test]
fn typed_loops_take_isset_type_checks_coalesce_and_append() {
    // (Compiled again when an offset first misses: the read then meets
    // null, not the number the loop was typed for.)
    typed_at_least(
        "<?php $h = [1, 2, 3]; $s = ''; $c = 0; for ($i = 0; $i < 30; $i++) { $k = $i % 5; if (isset($h[$k])) { $c += $h[$k] ?? 0; } $x = $h[$k] ?? 7; $c += $x; $s .= 'ab'; $s .= $i; if (is_int($x)) { $c++; } if (empty($h[$k])) { $c--; } $b = isset($h[$k]); if ($b) { $c += 2; } } var_dump($c, $s);",
        1,
    );
    both(
        r#"<?php
function f($h, $n, $u) { $c = 0; $s = 'x'; $r = [];
  for ($i = 0; $i < $n; $i++) { $k = $i % 4;
    $v = $h[$k] ?? -1; $w = $u ?? $i; $c += is_int($v) ? $v : 100; $s .= $k; $s .= '-';
    if (isset($h[$k]) && !empty($h[$k])) { $c += 3; } if (is_float($v) || is_null($w)) { $c += 5; } if (isset($u)) { $c -= 1; } if (empty($u)) { $c += 2; }
    $r[] = isset($h[$i]); $r[] = is_string($v) || is_bool($v); }
  return [$c, $s, $r, $v, $w]; }
var_dump(f([1, 2, 3], 9, null), f([0, 2.5, null, 'x'], 9, 4), f(['a' => 1, 2 => 2], 6, 0), f([], 3, 'u'), f('str', 5, 1.5), f(7, 4, false));
"#,
    );
}

#[test]
fn typed_loops_read_constants_as_their_values() {
    typed_at_least(
        "<?php const STEP = 3; const SCALE = 1.5; const ON = true; class K { const N = 4; const F = 0.25; } $s = 0; $x = 0.0; $c = 0; for ($i = 0; $i < 40; $i += STEP) { $s += K::N * $i; $x = $x * SCALE + K::F; $n = PHP_INT_MAX; if (ON) { $c++; } if ($i < $n) { $c += 2; } } var_dump($s, $x, $c);",
        1,
    );
    both(
        r#"<?php
define('LIMIT', 7); define('RATE', 0.5); define('NAME', 'n'); define('FLAG', false);
class C { const A = 2; const B = self::A * 3; const S = 's'; static function run($n) { $t = 0; for ($i = 0; $i < $n; $i++) { $t += self::B + static::A; if ($i == LIMIT) { $t *= 2; } } return $t; } }
class D extends C { const A = 10; }
function f($n) { $r = []; for ($i = 0; $i < $n; $i++) { $v = LIMIT - $i; $w = $v * RATE; $z = NAME; $u = FLAG || $i > LIMIT; $r[] = $v; $r[] = $w; $r[] = $z; $r[] = $u; } return $r; }
var_dump(f(9), C::run(10), D::run(10));
for ($i = 0; $i < 6; $i++) { try { $q = $i + UNDEFINED_CONSTANT; } catch (Error $e) { echo get_class($e), ' '; } }
"#,
    );
}

#[test]
fn an_entry_neither_tier_runs_is_taken_back_out_of_the_tape() {
    // With the second tier alone, a loop it wanted but cannot type (an
    // array as a value, a string that turns up) loses its entry
    // instruction: the op array goes back to the tape it had.
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let src = r#"<?php
function pick($n) { $val = [0, 1, 2]; $f = false; $c = 0; for ($i = 0; $i < $n; ++$i) { $x = $f ? null : $val; $c += $i; } return [$x, $c]; }
function turn($n) { $s = 1; for ($i = 0; $i < $n; $i++) { $s = $s + $i; if ($i == 20) { $s = 'x' . $s; } if ($i == 22) { $s = 5; } } return $s; }
function flag($n) { $f = false; $j = 0; $x = 0; for ($i = 0; $i < $n; ++$i) { $x = $f ? $f : $j + 1; } return $x; }
var_dump(pick(60), turn(60), flag(60), pick(5), turn(30));
"#;
    let saved = options();
    configure(Options {
        enabled: false,
        ..saved
    });
    let interpreted = run(src);
    for hot_loop in [1, 3, 64] {
        configure(Options {
            enabled: true,
            everything: false,
            first_tier: false,
            registers: true,
            hot_loop,
        });
        let before = stats();
        let compiled = run(src);
        assert_eq!(interpreted, compiled, "hot_loop {hot_loop}");
        // The first tier compiled nothing.
        assert_eq!(stats().compiled, before.compiled);
    }
    configure(saved);
}

#[test]
fn a_slot_written_on_one_way_into_a_join_keeps_no_stale_type() {
    // Found by `scripts/jit_fuzz.py` (seed 91): `$c` comes in an int, is
    // assigned a float on one way, and was then read as the int.
    three(
        r"<?php
function g($x, $y) { $a = $y; $c = $x; $d = 1; $w = 0; while ($w++ < 8) { if ($a < 18) { $c = $a; } $d = $c % 8; } return [$c, $d]; }
function k($x, $y) { $a = $y; $c = $x; $d = 1; $w = 0; while ($w++ < 8) { if ($a < 18) { $c = $a; } $d = $c | 8; } return [$c, $d]; }
function m($x, $y) { $c = $x; $d = 0; for ($i = 0; $i < 9; $i++) { if ($i > 4) { $c = $y; } else { $d += 2; } $d += $c; } return [$c, $d]; }
function n($x) { $c = $x; $d = 0; for ($i = 0; $i < 9; $i++) { if ($i > 4) { $c = 5; } $d += $c * 2; } return [$c, $d]; }
var_export([g(1, 3.0), k(1, 3.0), g(1, 30.0), m(1, 2.5), m(1.5, 2), m(1, 2), n(1), n(1.5), n(7)]);
",
    );
}
