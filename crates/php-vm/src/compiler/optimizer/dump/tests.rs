//! Tests of the `zend_dump_op_array`-style dump. Expected texts follow
//! php-src's output for the same oplines (`ext/opcache/tests/opt/*.phpt`).

use super::model::{ZLine, Z};
use super::passes;
use super::{dump_unit, fmt_g, main_line_end, write_const};
use crate::compiler::{compile, CompileOptions, CompiledUnit, FunctionInfo};
use php_runtime::{Value, ZStr};
use std::rc::Rc;

fn compile_src(src: &str) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
}

fn dump(src: &str) -> String {
    let unit = compile_src(src);
    let table = |lc: &[u8]| {
        (lc == b"var_dump").then_some(FunctionInfo {
            internal: true,
            required: 1,
            num_params: 2,
            variadic: true,
            by_ref: 0,
        })
    };
    let end = main_line_end(src.as_bytes(), None);
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let level = crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    String::from_utf8(dump_unit(
        &unit,
        &table,
        &flags,
        level,
        end,
        "after optimizer",
    ))
    .expect("utf-8")
}

#[test]
fn floats_print_like_printf_g() {
    assert_eq!(fmt_g(4.0), "4");
    assert_eq!(fmt_g(0.1), "0.1");
    assert_eq!(fmt_g(1.5), "1.5");
    assert_eq!(fmt_g(1e20), "1e+20");
    assert_eq!(fmt_g(1.0e-5), "1e-05");
    assert_eq!(fmt_g(123_456_789.0), "1.23457e+08");
    assert_eq!(fmt_g(-2.5), "-2.5");
    assert_eq!(fmt_g(f64::INFINITY), "inf");
}

#[test]
fn strings_are_escaped_like_smart_str_append_escaped() {
    let mut out = Vec::new();
    write_const(&mut out, &Value::Str(ZStr::from("No match\n\"q\"\\\x01")));
    assert_eq!(out, b"string(\"No match\\n\\\"q\\\"\\\\\\x01\")");
}

#[test]
fn main_line_end_counts_lines() {
    assert_eq!(main_line_end(b"<?php\necho 1;\n?>\n", None), 4);
    assert_eq!(main_line_end(b"<?php echo 1;", None), 1);
}

/// `sccp_016`: `T1 = JMPNZ_EX T1` shares its operand's temporary.
#[test]
fn temporaries_are_compacted_like_php() {
    let mut lines = vec![
        ZLine::new("ISSET_ISEMPTY_CV")
            .t(7)
            .x(" (isset)")
            .a(Z::Cv(0)),
        ZLine::new("JMPNZ_EX").t(7).a(Z::Tmp(7)).b(Z::ZLabel(3)),
        ZLine::new("QM_ASSIGN").t(7).a(Z::Const(Value::Bool(true))),
        ZLine::new("RETURN").a(Z::Tmp(7)),
    ];
    let t = passes::compact_tmps(&mut lines, false);
    assert_eq!(t, 1);
    assert!(lines.iter().all(|l| l.res.tmp().unwrap_or(0) == 0));
    let ranges = passes::live_ranges(&mut lines, t, &|_| true);
    assert!(ranges.is_empty());
}

/// The loop of `array_map_foreach_optimization_001`.
#[test]
fn live_ranges_match_zend_calc_live_ranges() {
    let mut lines = vec![
        ZLine::new("INIT_ARRAY")
            .t(10)
            .x(" 0 (packed)")
            .b(Z::Text("NEXT".into())),
        ZLine::new("FE_RESET_R").v(11).a(Z::Cv(0)).b(Z::ZLabel(7)),
        ZLine::new("FE_FETCH_R")
            .t(13)
            .a(Z::Tmp(11))
            .b(Z::Tmp(12))
            .j(Z::ZLabel(7)),
        ZLine::new("SEND_VAL").a(Z::Tmp(12)).b(Z::Num(1)),
        ZLine::new("DO_UCALL").t(14),
        ZLine::new("ADD_ARRAY_ELEMENT")
            .t(10)
            .a(Z::Tmp(14))
            .b(Z::Tmp(13)),
        ZLine::new("JMP").a(Z::ZLabel(2)),
        ZLine::new("FE_FREE").a(Z::Tmp(11)),
        ZLine::new("ASSIGN").a(Z::Cv(1)).b(Z::Tmp(10)),
    ];
    let t = passes::compact_tmps(&mut lines, false);
    assert_eq!(t, 4);
    let ranges = passes::live_ranges(&mut lines, t, &|_| true);
    let got: Vec<(u32, u32, u32, &str)> = ranges
        .iter()
        .map(|r| (r.var, r.start, r.end, r.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, 1, 8, "tmp/var"),
            (1, 2, 7, "loop"),
            (2, 3, 3, "tmp/var"),
            (3, 3, 5, "tmp/var"),
        ]
        .into_iter()
        .filter(|r| r.1 != r.2)
        .collect::<Vec<_>>()
    );
}

#[test]
fn main_code_returns_one() {
    let out = dump("<?php\nfunction foo(int $x) {\n    return $x;\n}\n");
    assert!(
        out.starts_with(
            "\n$_main:\n     ; (lines=1, args=0, vars=0, tmps=0)\n     ; (after optimizer)\n     ; t.php:1-5\n0000 RETURN int(1)\n"
        ),
        "{out}"
    );
    assert!(out.contains("\nfoo:\n"), "{out}");
    assert!(
        out.contains("0000 CV0($x) = RECV 1\n0001 RETURN CV0($x)\n"),
        "{out}"
    );
}

#[test]
fn internal_calls_use_init_fcall_and_do_icall() {
    let out = dump("<?php\nvar_dump(1);\n");
    assert!(
        out.contains(
            "0000 INIT_FCALL 1 96 string(\"var_dump\")\n0001 SEND_VAL int(1) 1\n0002 DO_ICALL\n0003 RETURN int(1)\n"
        ),
        "{out}"
    );
}

#[test]
fn assignments_to_scalar_cvs_become_qm_assign() {
    let out = dump("<?php\nfunction f() {\n    $a = 1;\n    return $a;\n}\n");
    assert!(
        out.contains("0000 CV0($a) = QM_ASSIGN int(1)\n0001 RETURN CV0($a)\n"),
        "{out}"
    );
}

#[test]
fn top_level_classes_are_bound_at_compile_time() {
    // dce_006.phpt / type_inference_class_consts2.phpt: no DECLARE_CLASS
    // for a top-level class without interfaces or traits whose parent is
    // already bound.
    let out = dump("<?php\nclass A {}\nclass B extends A {}\n");
    assert!(!out.contains("DECLARE_CLASS"), "{out}");
    // type_inference_class_consts4.phpt: a class implementing an
    // interface is still declared at run time.
    let out = dump("<?php\ninterface I {}\nclass T implements I {}\n");
    assert!(out.contains("0000 DECLARE_CLASS string(\"t\")\n"), "{out}");
    // Inside a conditional block it is not top-level.
    let out = dump("<?php\nif ($x) {\n    class C {}\n}\n");
    assert!(out.contains("DECLARE_CLASS string(\"c\")"), "{out}");
}

#[test]
fn anonymous_class_methods_are_named_up_to_the_nul() {
    let out = dump("<?php\n$o = new class {\n    public function m() {}\n};\n");
    assert!(out.contains("\nclass@anonymous::m:\n"), "{out}");
    assert!(!out.contains("FETCH_CLASS"), "{out}");
}

#[test]
fn typed_returns_get_verify_return_type() {
    // pipe_optimizations.phpt: `int + 1` may overflow to a float, so the
    // check stays.
    let out = dump("<?php\nfunction f(int $a): int {\n    return $a + 1;\n}\n");
    assert!(
        out.contains("0001 T1 = ADD CV0($a) int(1)\n0002 VERIFY_RETURN_TYPE T1\n0003 RETURN T1\n"),
        "{out}"
    );
    // A parameter of the declared type needs no check
    // (`can_elide_return_type_check`).
    // (The unit-test dump runs without unreachable-code removal, so the
    // implicit return's operand-less check may still be printed.)
    let out = dump("<?php\nfunction f(int $a): int {\n    return $a;\n}\n");
    assert!(!out.contains("VERIFY_RETURN_TYPE CV0"), "{out}");
    // A literal of the declared type needs no check; another literal is
    // checked into a TMP (type_inference_final_class.phpt).
    let out = dump("<?php\nfunction f(): int {\n    return 42;\n}\n");
    assert!(!out.contains("VERIFY_RETURN_TYPE int"), "{out}");
    let out = dump("<?php\nfunction f(): float {\n    return 1;\n}\n");
    assert!(
        out.contains("0000 T0 = VERIFY_RETURN_TYPE int(1)\n0001 RETURN T0\n"),
        "{out}"
    );
    // `void` and `mixed` are not checked.
    let out = dump(
        "<?php\nfunction f(): void {\n    return;\n}\nfunction g($x): mixed {\n    return $x;\n}\n",
    );
    assert!(!out.contains("VERIFY_RETURN_TYPE CV0"), "{out}");
}

#[test]
fn dfa_rewrites_arithmetic_with_literals() {
    // pipe_optimizations.phpt: `MUL x, 2` of a number is `ADD x, x`.
    let out = dump("<?php\nfunction f(int $a) {\n    return $a * 2;\n}\n");
    assert!(out.contains("T1 = ADD CV0($a) CV0($a)\n"), "{out}");
    // A long literal against a double becomes a double literal.
    let out = dump("<?php\nfunction f(float $a) {\n    return $a + 1;\n}\n");
    assert!(out.contains("T1 = ADD CV0($a) float(1)\n"), "{out}");
    // Adding zero to a long is a copy.
    let out = dump("<?php\nfunction f(int $a) {\n    return $a + 0;\n}\n");
    assert!(!out.contains(" ADD "), "{out}");
    // Untyped operands are left alone.
    let out = dump("<?php\nfunction f($a) {\n    return $a * 2;\n}\n");
    assert!(out.contains("T1 = MUL CV0($a) int(2)\n"), "{out}");
}

#[test]
fn pipes_into_method_callables_are_plain_calls() {
    // pipe_optimizations.phpt (`zend_partial_apply` in zend_compile_pipe).
    let out = dump(
        "<?php\nclass O {\n    public function foo($a) { return $a; }\n    public static function bar($a) { return $a; }\n}\n$o = new O;\n$r = 5 |> $o->foo(...) |> O::bar(...);\n",
    );
    assert!(!out.contains("CALLABLE_CONVERT"), "{out}");
    assert!(
        out.contains("INIT_METHOD_CALL 1 CV0($o) string(\"foo\")\n"),
        "{out}"
    );
    assert!(
        out.contains("INIT_STATIC_METHOD_CALL 1 string(\"O\") string(\"bar\")\n"),
        "{out}"
    );
}

/// `list_assign_from_array_literal` (level 0): an `ASSIGN` whose result is
/// unused still took a temporary number in `zend_compile`, so `swap()`
/// has `T2`, `T3` and `tmps=4`.
#[test]
fn unoptimized_temporaries_count_dropped_results() {
    let mut lines = vec![
        ZLine::new("QM_ASSIGN").t(10).a(Z::Cv(1)),
        ZLine::new("QM_ASSIGN").t(11).a(Z::Cv(0)),
        ZLine::new("ASSIGN").a(Z::Cv(0)).b(Z::Tmp(10)),
        ZLine::new("ASSIGN").a(Z::Cv(1)).b(Z::Tmp(11)),
        ZLine::new("PRE_DEC").a(Z::Cv(0)),
        ZLine::new("IS_SMALLER")
            .t(12)
            .a(Z::Const(Value::Int(0)))
            .b(Z::Cv(0)),
        ZLine::new("RETURN").a(Z::Tmp(12)),
    ];
    assert_eq!(passes::number_tmps(&mut lines), 6);
    assert_eq!(lines[0].res.tmp(), Some(0));
    assert_eq!(lines[1].res.tmp(), Some(1));
    assert_eq!(lines[3].op2.tmp(), Some(1));
    assert_eq!(lines[5].res.tmp(), Some(5));
    assert_eq!(lines[6].op1.tmp(), Some(5));
}

/// `spaceship_range`: the SSA dump after the DFA pass (`opt_debug_level`
/// `0x400000`): RECV of untyped parameters, the `SPACESHIP` range and the
/// `ADD` range and type derived from it, the unused initial CVs (`NOVAL`)
/// and the function's return info.
#[test]
fn ssa_dump_after_dfa_pass_infers_ranges() {
    let src = "<?php\nfunction test($a, $b) {\n    return ($a <=> $b) + 1;\n}\n";
    let unit = compile_src(src);
    let table = |_: &[u8]| -> Option<FunctionInfo> { None };
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let out = super::dump_unit_dfa(
        &unit,
        &table,
        &flags,
        crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL,
        main_line_end(src.as_bytes(), None),
        super::DUMP_AFTER_DFA_PASS,
    );
    let expected = "
$_main:
     ; (lines=1, args=0, vars=0, tmps=0, ssa_vars=0, no_loops)
     ; (after dfa pass)
     ; t.php:1-5
     ; return  [long] RANGE[1..1]
BB0:
     ; start exit lines=[0-0]
     ; level=0
0000 RETURN int(1)

test:
     ; (lines=5, args=2, vars=2, tmps=2, ssa_vars=6, no_loops)
     ; (after dfa pass)
     ; t.php:2-4
     ; return  [long] RANGE[0..2]
     ; #0.CV0($a) NOVAL [undef]
     ; #1.CV1($b) NOVAL [undef]
BB0:
     ; start exit lines=[0-4]
     ; level=0
0000 #2.CV0($a) [any] = RECV 1
0001 #3.CV1($b) [any] = RECV 2
0002 #4.T2 [long] RANGE[-1..1] = SPACESHIP #2.CV0($a) [any] #3.CV1($b) [any]
0003 #5.T3 [long] RANGE[0..2] = ADD #4.T2 [long] RANGE[-1..1] int(1)
0004 RETURN #5.T3 [long] RANGE[0..2]
";
    assert_eq!(String::from_utf8(out).expect("utf-8"), expected);
}

/// Without the DFA bits, or without passes 6 and 7, nothing is dumped.
#[test]
fn ssa_dump_needs_the_dfa_pass() {
    let src = "<?php\nfunction f($a) { return $a; }\n";
    let unit = compile_src(src);
    let table = |_: &[u8]| -> Option<FunctionInfo> { None };
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let end = main_line_end(src.as_bytes(), None);
    let level = crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    assert!(super::dump_unit_dfa(&unit, &table, &flags, level, end, 0x2_0000).is_empty());
    assert!(super::dump_unit_dfa(
        &unit,
        &table,
        &flags,
        crate::compiler::optimizer::PASS_6,
        end,
        super::DUMP_BEFORE_DFA_PASS
    )
    .is_empty());
}
