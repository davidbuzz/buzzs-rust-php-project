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
