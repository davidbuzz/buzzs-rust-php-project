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
            dormant: false,
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
    let mut most = 0;
    for attempt in 0..40 {
        most = most.max(three(src).1);
        if attempt >= 11 {
            if most > 0 {
                break;
            }
            // Nothing in twelve runs: the flag may be held up for longer
            // than those took.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    most
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
    // (An array of strings is no reason any more: its elements are read
    // by the handler and not looked at.)
    typed_is("<?php $a = ['x', 'y', 'z']; for ($i = 0; $i < 3; $i++) { $t = $a[$i]; $n = $i * 2; } echo $t, $n;", 1);
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

#[test]
fn typed_loops_read_and_write_properties_by_their_short_cuts() {
    three(
        r"<?php
class P { public $x = 0; public $f = 0.5; public int $n = 0; public ?float $q = null; public $s = 'a'; public readonly int $r;
  function __construct() { $this->r = 3; }
  function run($k) { for ($i = 0; $i < $k; $i++) { $this->x += $i; $this->n = $this->x & 7; $this->f = $this->f * 1.5 - $this->r; $this->x--; $this->q = $i; } return [$this->x, $this->n, $this->f, $this->q]; } }
class M { private $data = ['x' => 1]; function __get($n) { return $this->data[$n] ?? 0; } function __set($n, $v) { $this->data[$n] = $v + 1; } }
function f($o, $k) { $t = 0; for ($i = 0; $i < $k; $i++) { $o->x += 2; $o->x++; $t += $o->x; $o->x *= 1; $o->x -= 1; $o->x = $o->x + $i; } return $t; }
function g($o, $k) { for ($i = 0; $i < $k; $i++) { $o->x = $i; if ($i == 11) { $o->x = 'str'; } if ($i == 13) { $o->x = 1.5; } if ($i == 15) { unset($o->x); } if ($i == 17) { $o->x = PHP_INT_MAX; } $o->x++; } return $o->x; }
function h($o, $k) { $t = 0; for ($i = 0; $i < $k; $i++) { try { $o->n += PHP_INT_MAX; } catch (TypeError $e) { $t++; } $o->n = $i; } return [$t, $o->n]; }
$p = new P; var_export([$p->run(40), f(new P, 30), f(new M, 30), g(new P, 30), h(new P, 20)]);
$a = new P; $b = $a; $r = &$a->x; for ($i = 0; $i < 30; $i++) { $a->x += 1; $b->f = $i; $r++; } var_export([$a->x, $b->f, $r]);
$o = new stdClass; $o->d = 1; for ($i = 0; $i < 30; $i++) { $o->d += $i; $o->e = $i; } var_export([$o->d, $o->e]);
",
    );
}

#[test]
fn a_loop_headed_by_an_op_the_interpreter_runs_itself_still_gets_hot() {
    // `$y = abs($x)` and a constant's read have no fast path in
    // `hot::step`: `Vm::run` ran such a loop without the tape ever
    // reaching its head a second time, and it was never compiled.
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let saved = options();
    for src in [
        "<?php $n = 300; $x = -3; $y = 0; for ($i = 0; $i < $n; $i++) { $y = abs($x); } var_dump($y);",
        "<?php const K = 4; $n = 300; $y = 0; for ($i = 0; $i < $n; $i++) { $y = K; $y += $i; } var_dump($y);",
    ] {
        configure(Options {
            enabled: false,
            ..saved
        });
        let interpreted = run(src);
        configure(Options {
            enabled: true,
            everything: false,
            first_tier: false,
            registers: true,
            hot_loop: 5,
        });
        let before = stats();
        let compiled = run(src);
        let typed = stats().typed_loops - before.typed_loops;
        // (Restored before the checks: the options are the process's.)
        configure(saved);
        assert_eq!(interpreted, compiled, "{src}");
        assert!(typed > 0, "no typed loop: {src}");
    }
}

#[test]
fn typed_loops_read_numbers_under_string_keys() {
    typed_at_least(
        "<?php $a = ['alpha' => 1, 'beta' => 2.5, 'gamma' => 3]; $s = 0; $t = 0.0; for ($i = 0; $i < 40; $i++) { $v = $a['gamma']; $s += $v; $w = $a['beta']; $t += $w; } var_dump($s, $t);",
        1,
    );
    three(
        r"<?php
function f($a, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $v = $a['k']; $s += $v; if ($i == 9) { $a['k'] = 2.5; } if ($i == 15) { $a['k'] = 'x'; } if ($i == 17) { unset($a['k']); } if ($i == 19) { $a = 'str'; } if ($i == 21) { $a = ['k' => 7]; } } return $s; }
function g(&$a, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $v = $a['k']; $s += $v; $a['k'] = $v + 1; } return $s; }
try { var_dump(f(['k' => 1, 'j' => 2], 30)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(); }
$b = ['k' => 3]; var_dump(g($b, 30), $b, f(['j' => 1, 'k' => 4], 8), f(['1' => 5, 'k' => 6], 8));
",
    );
}

#[test]
fn typed_loops_read_and_write_static_properties() {
    typed_at_least(
        "<?php class F { static $a = 1; static $f = 0.5; } $s = 0; $t = 0.0; for ($i = 0; $i < 40; $i++) { $x = F::$a; $s += $x; F::$a = $i; $t += F::$f; F::$f = $t * 0.5; } var_dump($s, $t, F::$a, F::$f);",
        1,
    );
    three(
        r"<?php
class S { static $n = 0; static int $typed = 0; static ?int $maybe = null; static $any = 1; protected static $hidden = 2;
  static function run($k) { $t = 0; for ($i = 0; $i < $k; $i++) { $v = self::$n; $t += $v; self::$n = $v + 1; self::$typed = $i; $w = static::$hidden; $t += $w; } return $t; }
  static function turn($k) { $t = 0; for ($i = 0; $i < $k; $i++) { $v = self::$any; if ($i == 9) { self::$any = 2.5; } if ($i == 13) { self::$any = 'str'; } if ($i == 15) { self::$any = [1]; } if ($i == 17) { self::$any = 4; } $t += is_array($v) ? 100 : (int)$v; self::$any = self::$any; } return $t; } }
class T extends S { protected static $hidden = 10; }
function refs($k) { $r = &S::$n; $t = 0; for ($i = 0; $i < $k; $i++) { $v = S::$n; $t += $v; $r++; S::$n = $v + 2; } return [$t, $r]; }
function bad($k) { $t = 0; for ($i = 0; $i < $k; $i++) { try { S::$typed = $i < 12 ? $i : 'x'; } catch (TypeError $e) { $t++; } $v = S::$typed; $t += $v; } return $t; }
var_export([S::run(40), T::run(40), S::turn(30), refs(30), bad(30), S::$n, S::$typed, S::$any]);
",
    );
}

#[test]
fn typed_loops_test_properties_with_isset_and_empty() {
    typed_at_least(
        "<?php class Q { public $b = 0; public $c = 5; static $s = 0; static $t = 3; } $o = new Q; $n = 0; for ($i = 0; $i < 40; $i++) { $x = isset($o->b); $y = empty($o->b); $z = isset(Q::$t); $w = empty(Q::$s); if ($x) { $n++; } if ($y) { $n += 2; } if ($z) { $n += 4; } if ($w) { $n += 8; } if (isset($o->c) && !empty(Q::$t)) { $n += 16; } } var_dump($n, $x, $y, $z, $w);",
        1,
    );
    three(
        r"<?php
class R { public $a = 1; public $n = null; public ?int $t = null; public int $u; static $s = null; static ?int $st = null; private $data = [];
  function __isset($k) { return $k === 'magic'; } function __get($k) { return 7; } }
function f($o, $k) { $c = 0; for ($i = 0; $i < $k; $i++) { if (isset($o->a)) { $c += 1; } if (isset($o->n)) { $c += 2; } if (empty($o->t)) { $c += 4; } if (isset($o->u)) { $c += 8; } if (isset($o->magic)) { $c += 16; } if (isset($o->nope)) { $c += 32; } if (!empty($o->magic)) { $c += 64; }
  if (isset(R::$s)) { $c += 128; } if (empty(R::$st)) { $c += 256; } if ($i == 9) { $o->n = 3; R::$s = 'x'; $o->a = null; } if ($i == 14) { unset($o->a); $o->u = 0; R::$st = 5; } } return $c; }
var_export([f(new R, 30), f(new stdClass, 20)]);
",
    );
}

#[test]
fn typed_loops_run_small_numeric_functions_inline() {
    // A float function, an int one called twice in an expression, a call
    // as an argument of a call.
    typed_at_least(
        "<?php function u_sq($x) { return $x * $x; } function u_add($a, $b) { return $a + $b; } function t($n) { $s = 0; $f = 0.5; for ($i = 0; $i < $n; $i++) { $s += u_sq($i) - u_add($i, 3); $f = u_add($f, 0.25) * 1.5; $s += u_add($i, u_sq($i + 1)); } return [$s, $f]; } var_dump(t(50), t(4));",
        1,
    );
    // Each on its own: what runs inline (branches, locals, bools, four
    // arguments, an unused result), what stops in the middle of a function
    // (an int overflow, a division by zero, whose error names the call),
    // and what is no function to inline (types, defaults, recursion, an
    // undefined variable, a string, no value returned).
    three(
        r#"<?php
function u_sq($x) { return $x * $x; }
function u_add($a, $b) { return $a + $b; }
function u_lim($x) { if ($x > 10) { return 10; } if ($x < -10) { return -10; } return $x; }
function u_big($x) { return $x * 4611686018427387904; }
function u_bump($x) { $x++; $y = $x; $y += 2; return $y; }
function u_undef($x) { return $x + $nope; }
function u_text($x) { return $x . 'a'; }
function u_nothing($x) { $y = $x + 1; }
function u_rec($x) { return $x > 0 ? u_rec($x - 1) + 1 : 0; }
function u_typed(int $x) { return $x + 1; }
function u_ret(int $x): float { return $x; }
function u_dflt($x, $y = 3) { return $x + $y; }
function u_mod($a, $b) { return $a % $b; }
function u_truth($x) { return $x ? 1 : 2; }
function u_both($a, $b) { return $a && $b ? 5 : 6; }
function u_four($a, $b, $c, $d) { return ($a - $b) * ($c - $d); }
function u_half($x) { return $x / 2; }
function u_cmp($a, $b) { return $a < $b; }
function t1($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_sq($i) - u_add($i, 3); } return $s; }
function t2($n) { $f = 0.5; for ($i = 0; $i < $n; $i++) { $f = u_add($f, 0.25) * 1.5 - u_sq($f) * 0.001; } return $f; }
function t3($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_lim($i - 20) + u_lim(u_sq($i)); $s += u_add($i, u_sq($i + 1)); } return $s; }
function t4($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_bump($i) + u_four($i, 1, $s & 3, 2); u_sq($i); } return $s; }
function t5($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_truth($i & 1) + u_truth($i * 0.5) + u_both($i, $i - 5); } return $s; }
function t6($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s = u_big($i + 1) > 1e19 ? $s + 1 : $s + 2; } return $s; }
function t7($n) { $s = 1; for ($i = 0; $i < $n; $i++) { $s = u_sq($s) + 3; } return $s; }
function t8($n, $z) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_mod($i + 7, $i - $z); } return $s; }
function t9($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_dflt($i) + u_dflt($i, 1) + u_typed($i) + u_rec(3); } return $s; }
function t10($n) { $s = 0.0; for ($i = 0; $i < $n; $i++) { $s += u_ret($i) + u_half($i); } return $s; }
function t11($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += @u_undef($i); } return $s; }
function t12($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $v = u_nothing($i); $s += $i; } return [$s, $v]; }
function t13($n) { $s = ''; for ($i = 0; $i < $n; $i++) { $s = u_text($i); } return $s; }
function t14($n, $v) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_sq($v); if ($i == 9) { $v = '7'; } if ($i == 12) { $v = 2.5; } } return $s; }
function t15($n) { $c = 0; for ($i = 0; $i < $n; $i++) { if (u_cmp($i, 7)) { $c++; } $c += u_add($i, $i) > 20 ? 1 : 0; } return $c; }
function t16($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_add($i, u_add(1, u_add($i, 2))); $s -= u_add(u_sq($i), u_sq(2)); } return $s; }
function u_three($a, $b, $c) { return $a * 100 + $b * 10 + $c; }
function t17($n) { $s = 0; $f = 0.5; for ($i = 0; $i < $n; $i++) { $s += u_three($i, u_add($i, u_three(1, 2, u_sq($i))), min($i, u_add(3, $i))) % 977; $f += u_add($f * 0.001, u_sq(u_add($i, 0.5))); $s += max(u_add($i, 1), min(u_sq($i), u_add(40, $i))); } return [$s, $f]; }
function t18($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += u_three(u_add($i, 1), u_add($i, u_add($i, u_add($i, 2))), 5) % 977; } return $s; }
foreach ([40, 6] as $n) {
  var_export([t1($n), t2($n), t3($n), t4($n), t5($n), t6($n), t7($n), t9($n), t10($n), t11($n), t12($n), t13($n), t14($n, 3), t15($n), t16($n), t17($n), t18($n)]);
  foreach ([100, 12] as $z) { try { var_export(t8($n, $z)); } catch (DivisionByZeroError $e) { echo $e->getMessage(), '@', $e->getLine(), ' ', $e->getTraceAsString(), "\n"; } }
}
echo "\n";
"#,
    );
}

#[test]
fn inlined_functions_may_have_types_defaults_and_bool_or_unused_results() {
    typed_at_least(
        "<?php function v_int(int $x): int { return $x + 1; } function v_d(int $x, int $y = 3, float $z = 0.5): float { return $x * $y + $z; } function v_lt($a, $b) { return $a < $b; } function v_void($x): void { $y = $x + 1; } function t($n) { $s = 0; $f = 0.0; $c = 0; for ($i = 0; $i < $n; $i++) { $s += v_int($i); $f += v_d($i) + v_d($i, 2, 1.5); if (v_lt($i, 9)) { $c++; } v_void($i); } return [$s, $f, $c]; } var_dump(t(50), t(4));",
        1,
    );
    // Types that take the argument as it is, and those that would convert
    // it or refuse it (the interpreter's call, with its errors); defaults
    // of each kind; results that are bools, null, or not used.
    three(
        r#"<?php
function v_int(int $x): int { return $x + 1; }
function v_flt(float $x): float { return $x * 0.5; }
function v_wide(float $x) { return $x + 1; }
function v_ret(int $x): float { return $x; }
function v_num(int|float $x): int|float { return $x * 2; }
function v_opt(?int $x): ?int { return $x; }
function v_dflt($x, $y = 3) { return $x + $y; }
function v_dflt2(int $x, int $y = 3, float $z = 0.5): float { return $x * $y + $z; }
function v_dnull($x, $y = null) { return $y === null ? $x : $x + $y; }
function v_dwide(int $x, float $y = 2) { return $x * $y; }
function v_void($x): void { $y = $x + 1; }
function v_none($x) { $y = $x * 2; return; }
function v_bool($a, $b) { return $a < $b; }
function v_tbool(int $a): bool { return $a > 10; }
function v_even($x) { return $x % 2 == 0; }
function v_true() { return true; }
function v_zero(): int { return 0; }
function v_str(string $x) { return 1; }
function p1($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += v_int($i) + v_num($i) + v_opt($i) + v_zero(); } return $s; }
function p2($n) { $f = 0.5; for ($i = 0; $i < $n; $i++) { $f += v_flt($f) + v_num($f) * 0.01; } return $f; }
function p3($n) { $f = 0.5; for ($i = 0; $i < $n; $i++) { $f += v_wide($i) + v_ret($i) + v_flt($i); } return $f; }
function p4($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += v_dflt($i) + v_dflt($i, 1); } return $s; }
function p5($n) { $f = 0.0; for ($i = 0; $i < $n; $i++) { $f += v_dflt2($i) + v_dflt2($i, 2) + v_dflt2($i, 2, 1.5); } return $f; }
function p6($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += v_dnull($i) + v_dnull($i, 2); } return $s; }
function p7($n) { $f = 0.0; for ($i = 0; $i < $n; $i++) { $f += v_dwide($i) + v_dwide($i, 1.5); } return $f; }
function p8($n) { $s = 0; for ($i = 0; $i < $n; $i++) { v_void($i); v_none($i); $s += $i; } return $s; }
function p9($n) { $c = 0; for ($i = 0; $i < $n; $i++) { if (v_bool($i, 7)) { $c++; } if (v_tbool($i)) { $c += 2; } $b = v_even($i); if ($b) { $c += 4; } if (v_true()) { $c += 8; } } return [$c, $b]; }
function p10($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $v = v_void($i); $w = v_none($i); $s += $i; } return [$s, $v, $w]; }
function p11($n, $x) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += v_int($x); if ($i == 9) { $x = 2.0; } if ($i == 11) { $x = '5'; } if ($i == 13) { $x = 7; } } return $s; }
function p12($n) { $s = 0; for ($i = 0; $i < $n; $i++) { try { $s += v_str($i); } catch (TypeError $e) { $s += 100; } } return $s; }
function p13($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += v_int(PHP_INT_MAX - 20 + $i) > 0 ? 1 : 2; } return $s; }
foreach ([40, 6] as $n) { var_export([p1($n), p2($n), p3($n), p4($n), p5($n), p6($n), p7($n), p8($n), p9($n), p10($n), p11($n, 3), p12($n)]); try { var_export(p13($n)); } catch (TypeError $e) { echo $e->getMessage(), '@', $e->getLine(), "\n"; } }
echo "\n";
"#,
    );
}

#[test]
fn static_methods_and_methods_run_inline_too() {
    typed_at_least(
        "<?php class M { static function sq($x) { return $x * $x; } static function add(int $a, int $b = 1): int { return $a + $b; } function twice($x) { return $x * 2; } function run($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += self::sq($i) + static::add($i) + M::add($i, 2) + $this->twice($i); } return $s; } } function t($o, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $o->twice($i) + M::sq($i); } return $s; } var_dump(t(new M, 50), (new M)->run(50), t(new M, 3));",
        2,
    );
    // Overridden, private and protected methods, `static::` in a
    // subclass, a call site that sees two classes in turn, an object that
    // changes class or becomes null in the loop, `__call`, a method that
    // uses `$this`, calls as arguments of calls.
    three(
        r#"<?php
class M { public $v = 3;
  static function sq($x) { return $x * $x; }
  static function add(int $a, int $b = 1): int { return $a + $b; }
  function twice($x) { return $x * 2; }
  function lt($a, $b) { return $a < $b; }
  function get() { return $this->v; }
  function useThis($x) { return $x + $this->v; }
  private function hidden($x) { return $x + 100; }
  protected static function prot($x) { return $x + 1000; }
  function inner($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $this->twice($i) + $this->hidden($i) + self::sq($i) + static::prot($i) + M::add($i); } return $s; }
  static function sinner($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += M::sq($i) + M::prot($i) + M::add($i, 2); } return $s; }
  function __call($name, $args) { return 7; }
  static function __callStatic($name, $args) { return 9; }
}
class N extends M { function twice($x) { return $x * 3; } static function sq($x) { return $x + 1; } protected static function prot($x) { return $x + 2000; } }
class O { function twice($x) { return 'str'; } }
function q1($n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += M::sq($i) + M::add($i) + M::add($i, 5) + N::sq($i); } return $s; }
function q2($o, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $o->twice($i); if ($o->lt($i, 9)) { $s++; } } return $s; }
function q3($n) { $a = new M; $b = new N; $s = 0; for ($i = 0; $i < $n; $i++) { $o = $i % 3 ? $a : $b; $s += $o->twice($i); } return $s; }
function q4($n) { $a = new M; $s = 0; for ($i = 0; $i < $n; $i++) { $s += $a->twice($i); if ($i == 9) { $a = new N; } if ($i == 14) { $a = null; } if ($i == 15) { $a = new M; } } return $s; }
function q5($o, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $o->get() + $o->useThis($i) + $o->nope($i) + M::nope2($i); } return $s; }
function q6($o, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $o->twice(M::sq($o->twice($i))) % 1000; $s += M::add(M::add($i), $o->twice(2)); } return $s; }
function q7($n) { $s = 0; for ($i = 0; $i < $n; $i++) { try { $s += M::prot($i); } catch (Error $e) { $s += 1; } } return $s; }
function q8($o, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $o->twice($i); } return $s; }
foreach ([40, 6] as $n) {
  var_export([q1($n), q2(new M, $n), q2(new N, $n), q3($n), q5(new M, $n), q6(new M, $n), q6(new N, $n), q7($n), (new M)->inner($n), (new N)->inner($n), M::sinner($n), N::sinner($n)]);
  try { var_export(q4($n)); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), '@', $e->getLine(), "\n"; }
  try { var_export(q8(new O, $n)); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
echo "\n";
"#,
    );
}

#[test]
fn foreach_over_values_that_are_no_numbers() {
    typed_at_least(
        "<?php class It { public $price; public $qty = 2; function __construct($p) { $this->price = $p; } function total() { return 3; } } function t($items) { $s = 0.0; $c = 0; foreach ($items as $k => $it) { $s += $it->price * $it->qty + $k; $c += $it->total(); } return [$s, $c]; } $a = []; for ($i = 0; $i < 30; $i++) { $a[] = new It($i + 0.5); } var_dump(t($a), t($a), t([]));",
        1,
    );
    // Objects (with a destructor that counts), rows read by a string key,
    // strings, string keys, `continue` and `break`, writes to the
    // objects, values of every kind in one array, a nest, the object kept
    // past the loop.
    three(
        r#"<?php
class It { public $price; public $qty = 2; public $name = 'n'; static $dead = 0;
  function __construct($p) { $this->price = $p; }
  function total() { return 3; }
  static function vat($x) { return $x * 2; }
  function __destruct() { self::$dead++; } }
function mk($n) { $a = []; for ($i = 0; $i < $n; $i++) { $a[] = new It($i + 0.5); } return $a; }
function a1($items) { $s = 0.0; foreach ($items as $it) { $s += $it->price * $it->qty; } return $s; }
function a2($items) { $s = 0.0; $c = 0; foreach ($items as $k => $it) { $s += $it->price + $k; $c += $it->total() + It::vat($k); } return [$s, $c]; }
function a3($rows) { $s = 0; foreach ($rows as $row) { $s += $row['a'] + $row['b']; } return $s; }
function a4($strs) { $n = 0; foreach ($strs as $str) { $n += strlen($str); } return $n; }
function a5($map) { $n = 0; $last = ''; foreach ($map as $key => $v) { $n += $v; $last = $key; } return [$n, $last]; }
function a6($items) { $s = 0.0; foreach ($items as $it) { if ($it->price > 5) { continue; } $s += $it->price; if ($s > 100) { break; } } return $s; }
function a7($items) { $n = 0; foreach ($items as $it) { $it->qty++; $it->price = $it->price + 1; $n += $it->qty; } return $n; }
function a8($mixed) { $n = 0; foreach ($mixed as $v) { $n++; } return [$n, $v]; }
function a9($n) { $c = 0; foreach (mk($n) as $it) { $c += $it->qty; } return [$c, It::$dead]; }
function a10($rows) { $s = 0; foreach ($rows as $i => $row) { foreach ($row as $v) { $s += $v * $i; } } return $s; }
function a11($items) { $s = 0.0; $prev = null; foreach ($items as $it) { $s += $it->price; $prev = $it; } return [$s, $prev->price]; }
$items = mk(40); $rows = []; $strs = []; $map = [];
for ($i = 0; $i < 40; $i++) { $rows[] = ['a' => $i, 'b' => $i * 2]; $strs[] = $i % 3 ? 'xy' : 'abcde'; $map['k' . $i] = $i; }
$mixed = [1, 'two', 3.0, null, [5], new It(6), true];
$nested = [[1, 2, 3], [4, 5], [], [6]];
foreach ([1, 2] as $round) {
  var_export([a1($items), a2($items), a3($rows), a4($strs), a5($map), a6($items), a7($items), a8($mixed), a9(30), a10($nested), a11($items), a1([]), a4(['abc']), It::$dead]);
}
echo "\n";
"#,
    );
}

#[test]
fn the_foreach_short_cut_releases_and_declines_as_the_interpreter_does() {
    // Destructors in their order (the array's own copy keeps the objects
    // to the end; pairs are released as the loop goes), a loop variable
    // that is a reference, an element that is one, `unset` of the
    // variable, string keys.
    three(
        r#"<?php
class D { public $id; static $log = ''; function __construct($i) { $this->id = $i; } function __destruct() { self::$log .= $this->id . ','; } }
function d1($n) { $a = []; for ($i = 0; $i < $n; $i++) { $a[] = new D($i); } $s = 0; foreach ($a as $k => $o) { $a[$k] = null; $s += $o->id; D::$log .= 'v' . $k . ';'; } return $s; }
function d2($n) { $s = 0; $a = []; for ($i = 0; $i < $n; $i++) { $a[] = [$i, new D(100 + $i)]; } foreach ($a as $pair) { $s += $pair[0]; } return $s; }
function d3(&$ref, $list) { $s = 0; foreach ($list as $ref) { $s += $ref->id; } return $s; }
function d4($list) { $s = 0; $x = 5; $y = &$x; foreach ($list as $y) { $s += 1; } return [$s, is_object($x)]; }
function d5($list) { $s = 0; foreach ($list as $i => $v) { $s += $i; unset($v); } return $s; }
function d6($list) { $s = ''; foreach ($list as $k => $v) { $s = $k; } return $s; }
$objs = []; for ($i = 0; $i < 20; $i++) { $objs[] = new D(200 + $i); }
$r = null; $refs = [1, 2, 3]; $q = 7; $refs[1] = &$q;
foreach ([1, 2] as $round) { var_export([d1(20), d2(20), d3($r, $objs), d4($objs), d5($objs), d6(['a' => 1, 'b' => 2, 5 => 3]), d5($refs)]); echo D::$log, "\n"; D::$log = ''; }
"#,
    );
}

#[test]
fn methods_that_read_properties_of_this_run_inline() {
    typed_at_least(
        "<?php class G { public $price; protected $qty = 2; function __construct($p) { $this->price = $p; } function getPrice() { return $this->price; } function total() { return $this->price * $this->qty; } function cheap() { return $this->price < 10; } } function t($items) { $s = 0.0; $c = 0; foreach ($items as $it) { $s += $it->getPrice() + $it->total(); if ($it->cheap()) { $c++; } } return [$s, $c]; } $a = []; for ($i = 0; $i < 30; $i++) { $a[] = new G($i + 0.5); } var_dump(t($a), t($a), t([]));",
        1,
    );
    // Protected and private properties (a subclass's own private one of
    // the same name), an overriding getter, a call site that sees two
    // classes one after the other and both in turn, a property that
    // becomes a string or is unset, a typed one that is null, `__get`, a
    // method that writes, one that calls another.
    three(
        r#"<?php
class G { public $price; protected $qty = 2; private $rate = 0.5; public $name = 'n'; public ?int $maybe = null; public $dyn;
  function __construct($p) { $this->price = $p; }
  function getPrice() { return $this->price; }
  function getQty(): int { return $this->qty; }
  function total() { return $this->price * $this->qty; }
  function taxed($extra) { $t = $this->price + $extra; return $t * $this->rate; }
  function cheap() { return $this->price < 10; }
  function getName() { return $this->name; }
  function getMaybe() { return $this->maybe; }
  function bump() { $this->qty++; return $this->qty; }
  function viaThis() { return $this->getPrice(); }
  function __get($n) { return 42; }
  function magic() { return $this->nope; }
}
class H extends G { protected $qty = 5; private $rate = 2.0; function getPrice() { return $this->price + 1000; } function rate() { return $this->rate; } }
function mk($n, $cls) { $a = []; for ($i = 0; $i < $n; $i++) { $a[] = new $cls($i + 0.5); } return $a; }
function g1($items) { $s = 0.0; foreach ($items as $it) { $s += $it->getPrice() + $it->getQty() + $it->total(); } return $s; }
function g2($items) { $s = 0.0; $c = 0; foreach ($items as $k => $it) { $s += $it->taxed($k); if ($it->cheap()) { $c++; } } return [$s, $c]; }
function g3($items) { $n = 0; foreach ($items as $it) { $x = $it->getName(); $n += strlen($x); } return $n; }
function g4($items) { $n = 0; foreach ($items as $i => $it) { if ($i == 7) { $it->maybe = 3; } if ($i == 9) { $it->price = 'str'; } $m = $it->getMaybe(); $n += $m === null ? 1 : 100; } return $n; }
function g5($items) { $n = 0; foreach ($items as $it) { $n += $it->bump() + $it->magic(); } return $n; }
function g6($o, $n) { $s = 0.0; for ($i = 0; $i < $n; $i++) { $s += $o->getPrice() * $i + $o->viaThis(); if ($i == 20) { $o->price = 2; } if ($i == 30) { unset($o->price); $o->price = 1.5; } } return $s; }
function g7($items) { $s = 0.0; foreach ($items as $it) { $s += $it->getPrice() + $it->total(); } return $s; }
$gs = mk(40, 'G'); $hs = mk(40, 'H'); $mixed = []; for ($i = 0; $i < 40; $i++) { $mixed[] = $i % 3 ? $gs[$i] : $hs[$i]; }
foreach ([1, 2] as $round) {
  var_export([g1($gs), g1($hs), g2($gs), g2($hs), g3($gs), g5($gs), g6(new G(3.5), 40), g6(new H(3), 40), g7($mixed), g1(mk(3, 'G'))]);
  try { var_export(g4(mk(12, 'G'))); var_export(g1($mixed)); $z = mk(12, 'G'); $z[5]->price = 'x'; var_export(g7($z)); } catch (TypeError $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
echo "\n";
"#,
    );
}

#[test]
fn inlined_methods_are_resolved_for_the_class_and_the_scope() {
    // A trait's method in two classes, a private property of the same
    // name in a subclass, a method called from inside its class and from
    // a subclass's object, a closure bound to a class scope.
    three(
        r#"<?php
trait T { private $v = 7; function tv() { return $this->v; } function tw() { return 2; } }
class A { use T; public $v2 = 1; }
class B { use T; private $w = 3; }
class C1 { private $p = 1; function get() { return $this->p; } function viaOther(C1 $o) { return $o->get(); } }
class C2 extends C1 { private $p = 50; public $extra = 9; }
function run($list, $n) { $s = 0; for ($r = 0; $r < $n; $r++) { foreach ($list as $o) { $s += $o->tv() + $o->tw(); } } return $s; }
function run2($list) { $s = 0; foreach ($list as $o) { $s += $o->get(); } return $s; }
class K { public $list; function loop() { $s = 0; foreach ($this->list as $o) { $s += $o->get() + $this->own(); } return $s; } private function own() { return 1; } }
class K2 extends K { }
$as = [new A, new A, new A]; $bs = [new B, new B]; $mix = [new A, new B, new A, new B];
$cs = [new C1, new C2, new C1, new C2, new C2];
$k = new K; $k->list = $cs; $k2 = new K2; $k2->list = $cs;
$bound = Closure::bind(function ($list) { $s = 0; foreach ($list as $o) { $s += $o->p; } return $s; }, null, C1::class);
foreach ([1, 2, 3] as $round) { var_export([run($as, 20), run($bs, 20), run($mix, 20), run2($cs), run2([new C1]), $k->loop(), $k2->loop(), $bound([new C1, new C1])]); }
echo "\n";
"#,
    );
}

#[test]
fn fields_of_rows_are_read_by_the_string_key_short_cut() {
    // Rows of one shape; rows whose field is a float, a numeric string,
    // in another order, missing, an array; a row that is a string or
    // null; a key that is an int written as a string.
    three(
        r#"<?php
function r1($rows) { $s = 0; foreach ($rows as $row) { $s += $row['a'] + $row['b'] * 2; } return $s; }
function r2($rows) { $s = 0.0; foreach ($rows as $i => $row) { $s += $row['f'] * $i; if ($row['a'] > 5) { $s += 1; } } return $s; }
function r3($cfg, $n) { $s = 0; for ($i = 0; $i < $n; $i++) { $s += $cfg['step'] + ($cfg['scale'] > 1.0 ? 2 : 3) + $cfg['12'] + $cfg[12]; } return $s; }
function r4($rows) { $s = 0; foreach ($rows as $row) { $s += $row['a'] + ($row['opt'] ?? 7) + (isset($row['b']) ? 1 : 0); } return $s; }
function r5($rows) { $s = 0; foreach ($rows as $k => $row) { try { $s += $row['a']; } catch (TypeError $e) { $s += 1000; } } return $s; }
function r6($rows) { $s = 0; foreach ($rows as $row) { $s += @$row['missing'] + $row['a']; } return $s; }
$rows = []; for ($i = 0; $i < 40; $i++) { $rows[] = ['a' => $i, 'b' => $i * 2, 'f' => $i / 4]; }
$odd = $rows; $odd[7]['a'] = 2.5; $odd[9]['a'] = '11'; $odd[11] = ['b' => 1, 'f' => 0.0, 'a' => 3]; $odd[13]['opt'] = 100; unset($odd[15]['b']);
$bad = $rows; $bad[20]['a'] = [1]; $bad[21] = 'str'; $bad[22] = null;
$cfg = ['step' => 3, 'scale' => 1.5, 12 => 5, 'x' => 'y'];
foreach ([1, 2] as $round) { var_export([r1($rows), r2($rows), r3($cfg, 40), r4($rows), r1($odd), r2($odd), r4($odd), r6($rows), r1([]), r1([['a' => 1, 'b' => 2]])]); try { var_export(r5($bad)); var_export(r1($bad)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
echo "\n";
"#,
    );
}

#[test]
fn methods_that_write_a_property_run_inline_when_nothing_follows_the_write() {
    typed_at_least(
        "<?php class W { public $n = 0; private $hits = 0; function setN($v) { $this->n = $v; } function add($v) { $this->n += $v; } function hit() { $this->hits++; } function hits() { return $this->hits; } } function t($o, $k) { for ($i = 0; $i < $k; $i++) { $o->setN($i); $o->add(2); $o->hit(); } return [$o->n, $o->hits()]; } var_dump(t(new W, 50), t(new W, 4));",
        1,
    );
    // Setters, `+=`, `++`, `*=` on private, typed and readonly
    // properties; a value the type refuses or converts; a property that
    // becomes a string, a float, a reference; an int that overflows; a
    // write followed by more work, two writes, a write on one branch
    // (none of which may run inline); calls as arguments of builtins.
    three(
        r#"<?php
class W { public $n = 0; public $f = 0.5; public int $ti = 0; public float $tf = 0.0; public readonly int $ro; public $s = 'str'; private $hits = 0; public ?W $next = null;
  function __construct() { $this->ro = 1; }
  function setN($v) { $this->n = $v; }
  function setF(float $v): void { $this->f = $v; }
  function add($v) { $this->n += $v; }
  function inc() { $this->n++; }
  function dec() { $this->hits--; return; }
  function scale($k) { $this->f *= $k; }
  function setTi($v) { $this->ti = $v; }
  function setTf($v) { $this->tf = $v; }
  function setRo($v) { $this->ro = $v; }
  function setS($v) { $this->s = $v; }
  function fluent($v) { $this->n = $v; return $this; }
  function setRet($v) { $this->n = $v; return $v; }
  function setThenCalc($v) { $this->n = $v; return $v * 2; }
  function calcThenSet($v) { $t = $v * 2 + 1; $this->n = $t; return $t; }
  function twoSets($v) { $this->n = $v; $this->f = $v * 0.5; }
  function condSet($v) { if ($v > 5) { $this->n = $v; } }
  function hit() { $this->hits++; }
  function hits() { return $this->hits; }
  function addBig() { $this->n += PHP_INT_MAX; }
}
function s1($o, $k) { for ($i = 0; $i < $k; $i++) { $o->setN($i); $o->add(2); $o->inc(); $o->hit(); $o->dec(); $o->hit(); } return [$o->n, $o->hits()]; }
function s2($o, $k) { for ($i = 0; $i < $k; $i++) { $o->setF($i * 0.5); $o->scale(1.5); } return $o->f; }
function s3($o, $k) { $s = 0; for ($i = 0; $i < $k; $i++) { $s += $o->setRet($i) + $o->calcThenSet($i); $o->fluent($i); $o->condSet($i); } return [$s, $o->n]; }
function s4($o, $k) { $s = 0; for ($i = 0; $i < $k; $i++) { $s += $o->setThenCalc($i); $o->twoSets($i); } return [$s, $o->n, $o->f]; }
function s5($o, $k) { for ($i = 0; $i < $k; $i++) { $o->setTi($i); $o->setTf($i * 1.5); } return [$o->ti, $o->tf]; }
function s6($o, $k) { $c = 0; for ($i = 0; $i < $k; $i++) { try { $o->setTi($i < 20 ? $i : 2.5); } catch (TypeError $e) { $c++; } try { $o->setRo($i); } catch (Error $e) { $c += 100; } $o->setTf($i); } return [$c, $o->ti, $o->tf]; }
function s7($o, $k) { for ($i = 0; $i < $k; $i++) { $o->setS($i); $o->setN($i < 10 ? $i : 'x' . $i); } return [$o->s, $o->n]; }
function s8($o, $k) { for ($i = 0; $i < $k; $i++) { $o->inc(); if ($i == 9) { $o->n = 'abc'; } if ($i == 12) { $o->n = 1.5; } if ($i == 20) { $r = &$o->n; $r = 100; } } return $o->n; }
function s9($o, $k) { for ($i = 0; $i < $k; $i++) { $o->add($i); $o->addBig(); $o->add(-1); } return $o->n; }
function s10($list) { foreach ($list as $i => $o) { $o->setN($i); $o->hit(); } $t = 0; foreach ($list as $o) { $t += $o->n + $o->hits(); } return $t; }
function s11($o, $k) { $s = 0; for ($i = 0; $i < $k; $i++) { $s += max($o->setRet($i), 3) + abs($o->calcThenSet(-$i)); } return [$s, $o->n]; }
$list = []; for ($i = 0; $i < 30; $i++) { $list[] = new W; }
foreach ([40, 6] as $k) { var_export([s1(new W, $k), s2(new W, $k), s3(new W, $k), s4(new W, $k), s5(new W, $k), s6(new W, $k), s7(new W, $k), s8(new W, $k), s9(new W, $k), s10($list), s11(new W, $k)]); }
echo "\n";
"#,
    );
}

#[test]
fn loops_that_build_strings_and_read_string_fields_are_compiled() {
    typed_at_least(
        "<?php function t($rows) { $out = ''; $total = 0; foreach ($rows as $i => $row) { $out .= $row['name'] . ','; $line = \"<$i>\" . $row['name']; $total += $row['price'] + strlen($line); } return [$out, $total]; } $rows = []; for ($i = 0; $i < 30; $i++) { $rows[] = ['name' => 'n' . $i, 'price' => $i * 3]; } var_dump(t($rows), t($rows), t([]));",
        1,
    );
    // `.=`, `.`, interpolation, `(string)`, a string that is reset, copied
    // and becomes an int or an array; string, array and object fields of
    // rows and objects read into variables and temporaries; numbers
    // concatenated (a float is formatted by the interpreter).
    three(
        r#"<?php
class Pr { public $name; public $price; public $tags = ['a', 'b']; function __construct($n, $p) { $this->name = $n; $this->price = $p; } function label() { return 1; } }
function c1($rows) { $out = ''; $total = 0; foreach ($rows as $row) { $out .= $row['name'] . ','; $total += $row['price']; } return [$out, $total]; }
function c2($rows) { $out = ''; foreach ($rows as $i => $row) { $out .= "<td>{$row['name']}</td><td>$i</td>"; } return $out; }
function c3($items) { $out = ''; $t = 0.0; foreach ($items as $it) { $line = $it->name . ': ' . $it->price; $out .= $line . "\n"; $t += $it->price * 2; } return [$out, $t]; }
function c4($n) { $s = ''; for ($i = 0; $i < $n; $i++) { $s = $s . $i . '-'; if ($i % 7 == 0) { $s = ''; } } return $s; }
function c5($n) { $a = 'x'; $b = ''; for ($i = 0; $i < $n; $i++) { $b = $a; $a = $b . 'y'; $c = (string)$i; $a .= $c; } return [$a, $b, strlen($a)]; }
function c6($rows) { $n = 0; $last = null; foreach ($rows as $row) { $name = $row['name']; $n += strlen($name); $last = $row; $tags = $row['tags'] ?? null; } return [$n, $last['name'], $tags]; }
function c7($items) { $n = 0; foreach ($items as $it) { $t = $it->tags; $n += count($t) + $it->label(); $nm = $it->name; $n += strlen($nm); } return $n; }
function c8($n) { $s = ''; for ($i = 0; $i < $n; $i++) { $f = $i * 0.5; $s .= $f . ' ' . ($i % 3 == 0 ? 'fizz' : $i) . ';'; } return $s; }
function c9($rows) { $out = []; $k = 0; foreach ($rows as $row) { $key = 'k' . $row['price']; $k += strlen($key); } return $k; }
function c10($n) { $s = 'a'; for ($i = 0; $i < $n; $i++) { $s .= 'b'; $t = $s; $t .= 'c'; if ($i == 5) { $s = 5; } if ($i == 9) { $s = [1]; } if ($i == 10) { $s = 'z'; } } return [$s, $t]; }
$rows = []; $items = [];
for ($i = 0; $i < 40; $i++) { $rows[] = ['name' => 'n' . $i, 'price' => $i * 3, 'tags' => [$i]]; $items[] = new Pr('p' . $i, $i + 0.25); }
foreach ([1, 2] as $round) {
  try { var_export([c1($rows), c2($rows), c3($items), c4(40), c5(40), c6($rows), c7($items), c8(40), c9($rows), c1([]), c3([$items[0]])]); var_export(c10(40)); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
echo "\n";
"#,
    );
}

#[test]
fn a_loop_leaves_for_what_it_cannot_run_and_comes_back() {
    // `echo` and a `new` between inlined calls: the loop is compiled,
    // leaves for those and is entered again after them.
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let src = "<?php class Pt { public $x; function __construct($x) { $this->x = $x; } function getX() { return $this->x; } } function sq($v) { return $v * $v; } function t($items) { $s = 0; foreach ($items as $k => $it) { $s += $it->getX() + sq($k); echo $k, ','; $o = new Pt($k); $s += $o->getX() + sq($s % 7); } return $s; } $a = []; for ($i = 0; $i < 30; $i++) { $a[] = new Pt($i); } var_dump(t($a), t($a), t([]));";
    let saved = options();
    configure(Options {
        enabled: false,
        ..saved
    });
    let interpreted = run(src);
    for hot_loop in [1, 2, 5] {
        configure(Options {
            enabled: true,
            everything: false,
            first_tier: false,
            registers: true,
            hot_loop,
        });
        let before = stats();
        let compiled = run(src);
        let typed = stats().typed_loops - before.typed_loops;
        configure(saved);
        assert_eq!(interpreted, compiled, "hot_loop {hot_loop}");
        assert!(typed > 0, "no typed loop with hot_loop {hot_loop}");
    }
}

#[test]
fn a_loop_compiled_again_is_not_entered_at_its_head_from_a_stale_resume_point() {
    // The second loop is compiled first with the method call left to the
    // interpreter (not called yet: a resume point after it), then again
    // with the call inline (no resume point): the old resume point must
    // not run the new code from the loop's head.
    if std::env::var_os("PHPRUST_JIT").is_some() {
        return;
    }
    let src = r#"<?php
class P { public int $n = 1; static $sf = 0.5; function bump() { $this->n++; } }
function f1($y, $n, $q, $o) { $c = 0; $d = $y; $g = $n; $h = $y; $ss = ["a", "bcd", "", "ef"];
foreach ($ss as $fk => $fs) { $h = strlen($fs) + $g; $a = P::$sf; }
for ($j = 0; $j <= 27; ++$j) { $g = --$a; $o->bump(); if (empty($q[$c % 8])) { $c -= $h; } else { $d -= $c; } }
return [$a, $c, $d, $g, $h, $o->n, $j]; }
for ($round = 0; $round < 2; $round++) {
echo json_encode(f1(0.0, 3, [0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5], new P)), "\n";
echo json_encode(f1(3, 25, [1, 2, 3, 4, 5, 6, 7, 8], new P)), "\n";
}
"#;
    let saved = options();
    configure(Options {
        enabled: false,
        ..saved
    });
    let interpreted = run(src);
    for hot_loop in [1, 2, 3] {
        configure(Options {
            enabled: true,
            everything: false,
            first_tier: false,
            registers: true,
            hot_loop,
        });
        let compiled = run(src);
        configure(saved);
        assert_eq!(interpreted, compiled, "hot_loop {hot_loop}");
    }
}

#[test]
fn inlined_functions_compute_the_math_builtins_they_call() {
    // `sqrt()` of `$this` properties in a method, a builtin of a
    // parameter in a function, and those inside another call's arguments
    // (whose own arguments the inner calls must leave alone).
    typed_is(
        r#"<?php
class P { public $x; public $y; function __construct($x, $y) { $this->x = $x; $this->y = $y; }
    function len() { return sqrt($this->x * $this->x + $this->y * $this->y); } }
function g($v) { return sqrt($v) + abs($v - 3); }
function h($a, $b) { return $a * 10 + $b; }
function f() { $d = 0.0; $p = new P(3, 4);
    for ($i = 0; $i < 20; $i++) { $d += $p->len() + g($i) + h($i, g($i + 1)) + sqrt(g($i)); }
    return $d; }
var_dump(f());
"#,
        1,
    );
    // A `new` each round, left to the interpreter.
    typed_is(
        r#"<?php
class P { public $x; public $y; function __construct($x, $y) { $this->x = $x; $this->y = $y; }
    function len() { return sqrt($this->x * $this->x + $this->y * $this->y); } }
function f() { $d = 0.0; for ($i = 0; $i < 20; $i++) { $p = new P($i, $i + 1); $d += $p->len(); } return $d; }
var_dump(f());
"#,
        1,
    );
}

#[test]
fn typed_loops_divide_ints() {
    // Into a float sum, a math function, `(float)`, `/` by a float: a
    // float either way, exact or not.
    typed_is(
        r#"<?php
function f($n) { $s = 0.5; $t = 0.0;
    for ($i = 1; $i < $n; $i++) { $s += $i / 4; $t = $t * 0.5 + sqrt($i / 3) + (float)($i / 2) + ($i / 6) / 1.5 + PHP_INT_MAX / $i * 1.0; }
    return [$s, $t]; }
var_dump(f(40));
"#,
        1,
    );
    // Into variables: an int when exact, a float when not, as it was the
    // last time (and a loop that keeps being wrong about it is
    // interpreted in the end).
    for src in [
        "<?php function f() { $s = 0; for ($i = 1; $i < 60; $i++) { $q = ($i * 6) / 3; $s += $q; } return $s; } var_dump(f(), f());",
        "<?php function f() { $s = 0.5; for ($i = 1; $i < 60; $i++) { $q = ($i * 4 + 1) / 4; $s += $q; } return $s; } var_dump(f(), f());",
        "<?php function f() { $s = 0; for ($i = 1; $i < 60; $i++) { $q = $i / 3; $s += $q; } return $s; } var_dump(f(), f(), f());",
        "<?php function f() { $s = 0; for ($i = 1; $i < 60; $i++) { $s += (int) ($i / 4); } return $s; } var_dump(f(), f());",
        "<?php function f() { $s = 0; for ($i = 1; $i < 60; $i++) { try { $s += 12 / ($i % 4); } catch (DivisionByZeroError $e) { $s--; } } return $s; } var_dump(f());",
        "<?php function f() { $s = 0; for ($i = 1; $i < 60; $i++) { $s += PHP_INT_MIN / ($i % 3 - 1 ?: 1); } return $s; } var_dump(f());",
        "<?php function f() { $x = 1 << 40; for ($i = 1; $i < 60; $i++) { $x /= 2; } return $x; } var_dump(f());",
    ] {
        both(src);
    }
}

#[test]
fn typed_loops_compare_strings_and_take_any_elements() {
    // Comparisons of strings, null and bools, through the interpreter's
    // semantics (numeric strings, an int with a string), into branches
    // and bools; a `foreach` over numbers under string keys, and over
    // elements of all kinds; `max()` and `min()` of floats.
    let src = r#"<?php
function f($s, $m) {
    $c = 0; $n = strlen($s);
    for ($i = 0; $i < $n; $i++) { $ch = $s[$i]; if ($ch === "o" || $ch == "i") { $c++; } elseif ($ch < "f") { $c += 10; } if ($i == "5") { $c += 100; } }
    $out = "";
    foreach ($m as $k => $v) { $x = 0; if ($v == "1000") { $x |= 1; } if ($v == 0) { $x |= 2; } if ($v == null) { $x |= 4; }
        if ($v < "9") { $x |= 8; } if ($v <= "10") { $x |= 16; } if ($v === "7") { $x |= 32; } if ($v != "abc") { $x |= 64; }
        if ($k < "m") { $x |= 128; } if ($v == 1) { $x |= 256; } if ($v == true) { $x |= 512; } $t = $c % 3 ? "x" : $c; if ($t === "x") { $x |= 1024; }
        $out .= $x . ","; }
    $sum = 0.0; $hi = -INF; $lo = INF;
    foreach (['a' => 1.5, 'b' => -2.0, 'c' => 4.25, 7 => 0.5] as $k => $v) { $sum += $v; $hi = max($hi, $v); $lo = min($lo, $v); }
    return [$c, $out, $sum, $hi, $lo, max(NAN, 1.0), min(1.0, NAN)];
}
for ($r = 0; $r < 3; $r++) {
    var_dump(f('hello world, this is it', ['a' => 1, 'k1' => 2.5, 'z' => null, 'q' => '7', 'e' => '1e3', 'n' => '', 'w' => ' 1', 'x' => 'abc', 'y' => '10', 'b' => true, 'big' => '9223372036854775809']));
    var_dump(f('abc', [1, 2, 'k1' => 3, '9', 0, false]));
}
"#;
    typed_at_least(src, 2);
}

#[test]
fn typed_loops_return_from_a_foreach_and_compare_objects() {
    // A search loop: `return` from inside a `foreach` (the iterator's
    // release before it), `===` of objects, of a key with a string.
    typed_at_least(
        r#"<?php
class It { public $id; public $name; function __construct($i, $n) { $this->id = $i; $this->name = $n; } }
function find($items, $id) { foreach ($items as $item) { if ($item->id === $id) { return $item; } } return null; }
function findk($m, $key) { foreach ($m as $k => $v) { if ($k === $key) { return $v; } } return -1; }
function same($items, $o) { $n = 0; foreach ($items as $item) { if ($item === $o) { $n++; } if ($item !== null && $item->name == "n3") { $n += 10; } } return $n; }
$items = []; for ($i = 0; $i < 50; $i++) { $items[] = new It($i, "n$i"); }
$m = []; for ($i = 0; $i < 50; $i++) { $m["k$i"] = $i * 2; }
$s = 0;
for ($r = 0; $r < 30; $r++) { $s += find($items, $r)->id; $s += findk($m, "k$r"); $s += same($items, $items[$r]); var_dump(find($items, 99)); }
echo $s, "\n";
"#,
        2,
    );
}

#[test]
fn typed_loops_build_array_literals() {
    // Rows built in a loop: string, int and numeric-string keys, the next
    // index, nested literals, values of every kind; an undefined variable
    // is the VM's (its warning), and the loop is interpreted in the end.
    typed_at_least(
        r#"<?php
function f($s) {
    $rows = []; $n = 0;
    for ($i = 0; $i < 30; $i++) {
        $rows[] = ["id" => $i, "v" => $i * 2, "s" => $s, 5 => 1.5, "7" => null, [$i, [$s . $i]], $i => true, $i => false];
        $n += count($rows[$i]);
    }
    return [$n, $rows[29], $rows[3]];
}
function g() { $rows = []; for ($i = 0; $i < 30; $i++) { $rows[] = [$i, $undef]; } return $rows[5]; }
var_dump(f('str'), f('x'), g());
"#,
        1,
    );
}

#[test]
fn typed_loops_test_instanceof_and_leave_for_steps_that_keep_declining() {
    // `instanceof` of classes, a parent, an interface, of values that are
    // no objects; a class that is never loaded (its step declines: the
    // interpreter's op, the loop entered again after it), as is a float
    // compared with a string.
    typed_at_least(
        r#"<?php
interface Shape {} class A implements Shape { public $v = 1; } class B extends A {} class C {}
function f($items) { $n = 0;
    foreach ($items as $it) { if ($it instanceof A) { $n += 1; } if ($it instanceof B) { $n += 10; } if ($it instanceof Shape) { $n += 100; }
        if (!($it instanceof C)) { $n += 1000; } if ($it instanceof Nope) { $n += 5; } if ($it == 2.5) { $n += 7; } if ($it < "y") { $n += 3; } }
    return $n; }
$items = [new A, new B, new C, 1, "x", null, [1], new B, 2.5, "2.5"];
for ($r = 0; $r < 20; $r++) { echo f($items), " "; }
"#,
        1,
    );
}

#[test]
fn typed_loops_test_the_truth_of_anything_and_leave_calls_they_cannot_make() {
    // `if ($v)`, `!$v`, `$v && $k` of strings, arrays, null, objects; a
    // user function called with a string (no number to run it inline
    // with: the interpreter's call, the loop entered again after it).
    let src = r#"<?php
function hallo($a) { } function g($v) { return $v * 2; }
function f($m) { $n = 0;
    foreach ($m as $k => $v) { if ($v) { $n += 1; } if (!$v) { $n += 10; } $b = !$v; if ($b) { $n += 100; } if ($v && $k) { $n += 1000; } if ($v || !$k) { $n += 7; } }
    return $n; }
function h() { $c = 0; for ($i = 0; $i < 30; $i++) { hallo("hallo"); $c += g($i) + $i * 3 - 1; $c ^= 5; $c += $i % 3; } return $c; }
for ($r = 0; $r < 5; $r++) {
    var_dump(f(['a' => 'x', 'b' => '', 'c' => '0', 'd' => [], 'e' => [1], 'f' => null, 'g' => 1.5, 'h' => new stdClass, 'i' => 'abc', 0 => 'z']));
    var_dump(h());
}
"#;
    typed_at_least(src, 1);
}

#[test]
fn typed_loops_run_the_foreach_loops_inside_them() {
    // A `foreach` inside the loop: over a row (string keys), a literal,
    // an empty literal, a temporary; destructors of what the inner
    // arrays held run where they do interpreted.
    typed_at_least(
        r#"<?php
function f($rows) { $s = 0; $t = "";
    foreach ($rows as $k => $row) { foreach ($row as $c => $v) { $s += $v; $t .= $c; } foreach ([1, 2] as $w) { $s += $w; } foreach ([] as $w) { $s += 100; } }
    return [$s, $t]; }
function g($rows) { $n = 0; foreach ($rows as $row) { foreach ($row['items'] as $it) { $n += $it; } } return $n; }
$rows = []; for ($i = 0; $i < 30; $i++) { $rows[] = ['a' => $i, 'b' => $i * 2, 'c' => 1]; }
$r2 = []; for ($i = 0; $i < 30; $i++) { $r2[] = ['items' => [$i, $i + 1, 3]]; }
for ($r = 0; $r < 3; $r++) { var_dump(f($rows), g($r2)); }
"#,
        2,
    );
    // (Once, and only compared: destructors raise the process-wide
    // safe-point flag, which keeps other tests' loops from their code.)
    both(
        r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "d{$this->n} "; } }
function h() { $n = 0; for ($i = 0; $i < 20; $i++) { foreach ([new D($i)] as $o) { $n += $o->n; } } return $n; }
echo h(), "\n";
"#,
    );
}
