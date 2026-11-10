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

/// The dump of op array `name`, up to the blank line that ends it.
fn section<'a>(out: &'a str, name: &str) -> &'a str {
    let head = format!("\n{name}:\n");
    let start = out.find(&head).map_or(out.len(), |p| p + head.len());
    let rest = &out[start..];
    &rest[..rest.find("\n\n").unwrap_or(rest.len())]
}

#[test]
fn class_constants_fold_where_no_child_can_override_them() {
    // type_inference_class_consts1.phpt: `self::` folds; `static::` only
    // when the constant is final, but a typed one still elides the return
    // check.
    let out = dump(
        "<?php\nclass T {\n    public const FOO = 42;\n    public final const BAR = 42;\n    public const int BAZ = 42;\n    public function a(): int { return self::FOO; }\n    public function b(): int { return static::FOO; }\n    public function c(): int { return static::BAR; }\n    public function d(): int { return static::BAZ; }\n}\n",
    );
    let a = section(&out, "T::a");
    assert!(a.contains("0000 RETURN int(42)"), "{out}");
    assert!(!a.contains("FETCH_CLASS_CONSTANT"), "{out}");
    let b = section(&out, "T::b");
    assert!(
        b.contains("0000 T0 = FETCH_CLASS_CONSTANT (static) (exception) string(\"FOO\")\n0001 VERIFY_RETURN_TYPE T0\n"),
        "{out}"
    );
    assert!(
        section(&out, "T::c").contains("0000 RETURN int(42)"),
        "{out}"
    );
    let d = section(&out, "T::d");
    assert!(
        d.contains(
            "0000 T0 = FETCH_CLASS_CONSTANT (static) (exception) string(\"BAZ\")\n0001 RETURN T0"
        ),
        "{out}"
    );
}

#[test]
fn class_constants_of_bound_classes_fold_by_name_and_parent() {
    // type_inference_class_consts2.phpt / 3.phpt.
    let out = dump(
        "<?php\nclass P {\n    public const FOO = 42;\n    private const SECRET = 1;\n}\nfinal class C extends P {\n    public function a(): int { return parent::FOO; }\n    public function b(): int { return P::FOO; }\n    public function c(): int { return P::SECRET; }\n}\n",
    );
    assert!(
        section(&out, "C::a").contains("0000 RETURN int(42)"),
        "{out}"
    );
    assert!(
        section(&out, "C::b").contains("0000 RETURN int(42)"),
        "{out}"
    );
    // A private constant of another class is not folded.
    assert!(
        section(&out, "C::c").contains("FETCH_CLASS_CONSTANT string(\"P\") string(\"SECRET\")"),
        "{out}"
    );
}

#[test]
fn class_constants_of_classes_bound_at_run_time_are_not_inherited() {
    // type_inference_class_consts4.phpt: the class implementing the
    // interface is linked at run time, so `self::A` is not found; the
    // interface itself is bound at compile time.
    let out = dump(
        "<?php\ninterface I {\n    public const int A = 42;\n}\nclass T implements I {\n    public function a(): int { return self::A; }\n    public function b(): int { return I::A; }\n}\n",
    );
    let a = section(&out, "T::a");
    assert!(
        a.contains("0000 T0 = FETCH_CLASS_CONSTANT (self) (exception) string(\"A\")\n0001 VERIFY_RETURN_TYPE T0\n"),
        "{out}"
    );
    assert!(
        section(&out, "T::b").contains("0000 RETURN int(42)"),
        "{out}"
    );
}

#[test]
fn calls_of_functions_returning_a_literal_are_inlined() {
    // gh11245_1.phpt: `xx()` is replaced by its literal.
    let out = dump("<?php\nfunction xx() { return \"s\"; }\necho xx();\n");
    let main = section(&out, "$_main");
    assert!(!main.contains("DO_UCALL"), "{out}");
    assert!(!main.contains("INIT_FCALL"), "{out}");
    assert!(main.contains("0000 ECHO string(\"s\")\n"), "{out}");
    // type_inference_final_class.phpt: a method of a final class; the
    // literal reaches the return check through a TMP.
    let out = dump(
        "<?php\nfinal class T {\n    public function getInt(): int { return 42; }\n    public function get(): int { return $this->getInt(); }\n}\n",
    );
    assert!(
        section(&out, "T::get").contains("0000 T0 = VERIFY_RETURN_TYPE int(42)\n0001 RETURN T0"),
        "{out}"
    );
    // A method a child class may override is a prototype: not inlined.
    let out = dump(
        "<?php\nclass T {\n    public function getInt() { return 42; }\n    public function get() { return $this->getInt(); }\n}\n",
    );
    assert!(section(&out, "T::get").contains("DO_UCALL"), "{out}");
    // Typed parameters (`ZEND_ACC_HAS_TYPE_HINTS`) keep the call.
    let out = dump("<?php\nfunction f(int $a) { return 1; }\necho f(2);\n");
    assert!(section(&out, "$_main").contains("DO_UCALL"), "{out}");
    // Arguments sent to an inlined call are freed.
    let out = dump("<?php\nfunction f($a) { return 1; }\necho f($x);\n");
    let main = section(&out, "$_main");
    assert!(main.contains("CHECK_VAR CV0($x)"), "{out}");
    // The literal echoed in place of a temporary is a string.
    assert!(main.contains("ECHO string(\"1\")"), "{out}");
}

#[test]
fn functions_the_vm_optimizer_reduced_to_a_literal_are_not_inlined() {
    // sccp_042.phpt / ct_eval_md5_sha1.phpt: php-src inlines a call only
    // when the callee's compiled code is a literal return; SCCP's result
    // comes too late.
    let src =
        "<?php\nfunction t() {\n    $a = 1;\n    $b = 2;\n    return [$a <=> $b];\n}\necho t();\n";
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let level = crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    let opts = CompileOptions {
        filename: "t.php".into(),
        optimization_level: level,
        ..CompileOptions::default()
    };
    let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
    let table = |_: &[u8]| -> Option<FunctionInfo> { None };
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let end = main_line_end(src.as_bytes(), None);
    let out = String::from_utf8(dump_unit(
        &unit,
        &table,
        &flags,
        level,
        end,
        "after optimizer",
    ))
    .expect("utf-8");
    assert!(section(&out, "$_main").contains("DO_UCALL"), "{out}");
}
