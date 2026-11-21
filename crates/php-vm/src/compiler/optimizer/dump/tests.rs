// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

/// [`dump`] of `src` compiled at the level it is dumped at, as `include.rs`
/// compiles a script opcache dumps: the optimizer has run on the code.
fn dump_optimized(src: &str) -> String {
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

/// The op array printed for the function whose header starts with `prefix`.
fn section<'a>(out: &'a str, prefix: &str) -> &'a str {
    out.split("\n\n")
        .map(|s| s.trim_start_matches('\n'))
        .find(|s| s.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} in {out}"))
}

const THIS_CALLS: &str = "<?php\nclass A {\n    public function g() { return 1; }\n    public function m() { return $this->g(); }\n    public function n() { return $this?->g(); }\n    public function k() { return function () { return fn() => $this->g(); }; }\n    public static function s() { return function () { return $this->g(); }; }\n    public static function t() { return $this->g(); }\n}\n";

/// `zend_compile_method_call` with master's `this_guaranteed_exists()`
/// (`php8.5 -d opcache.opt_debug_level=0x20000`): `$this` of a non-static
/// method and of closures nested in one is the `THIS` (`IS_UNUSED`)
/// operand of `INIT_METHOD_CALL`, with no `JMP_NULL` for `?->`; a closure
/// in a static method (and a static method) fetches it into a VAR first.
/// A closure's op array has no scope for the called-function lookup.
#[test]
fn this_method_calls_use_the_this_operand_where_this_is_guaranteed() {
    let out = dump(THIS_CALLS);
    for f in ["A::m:", "A::n:"] {
        let s = section(&out, f);
        assert!(
            s.contains("0000 INIT_METHOD_CALL 0 THIS string(\"g\")\n"),
            "{s}"
        );
        assert!(s.contains("DO_UCALL"), "{s}");
        assert!(!s.contains("FETCH_THIS") && !s.contains("JMP_NULL"), "{s}");
    }
    let s = section(&out, "{closure:{closure:A::k()");
    assert!(
        s.contains("0000 INIT_METHOD_CALL 0 THIS string(\"g\")\n"),
        "{s}"
    );
    assert!(s.contains("DO_FCALL") && !s.contains("DO_UCALL"), "{s}");
    for f in ["{closure:A::s()", "A::t:"] {
        let s = section(&out, f);
        assert!(
            s.contains("0000 V0 = FETCH_THIS\n0001 INIT_METHOD_CALL 0 V0 string(\"g\")\n"),
            "{s}"
        );
    }
}

/// The compiled ops behind the dump: no `FetchThis` temporary for a
/// guaranteed `$this`.
#[test]
fn guaranteed_this_is_an_unused_method_call_operand() {
    use crate::opcode::{Op, Operand};
    let unit = compile_src(THIS_CALLS);
    let classes = unit.decls.classes.borrow();
    let ops_of = |name: &str| {
        classes[0]
            .methods
            .iter()
            .find(|m| m.name.as_bytes() == name.as_bytes())
            .map(|m| m.func.code.ops.clone())
            .expect("method")
    };
    for name in ["m", "n"] {
        let ops = ops_of(name);
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::InitMethodCall {
                    obj: Operand::Unused,
                    ..
                }
            )),
            "{name}: {ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::FetchThis { .. } | Op::JmpNull { .. })),
            "{name}: {ops:?}"
        );
    }
    let ops = ops_of("t");
    assert!(
        ops.iter().any(|op| matches!(op, Op::FetchThis { .. })),
        "{ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(
            op,
            Op::InitMethodCall {
                obj: Operand::Unused,
                ..
            }
        )),
        "{ops:?}"
    );
}

/// The dump of op array `name`, up to the blank line that ends it.
fn section_named<'a>(out: &'a str, name: &str) -> &'a str {
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
    let a = section_named(&out, "T::a");
    assert!(a.contains("0000 RETURN int(42)"), "{out}");
    assert!(!a.contains("FETCH_CLASS_CONSTANT"), "{out}");
    let b = section_named(&out, "T::b");
    assert!(
        b.contains("0000 T0 = FETCH_CLASS_CONSTANT (static) (exception) string(\"FOO\")\n0001 VERIFY_RETURN_TYPE T0\n"),
        "{out}"
    );
    assert!(
        section_named(&out, "T::c").contains("0000 RETURN int(42)"),
        "{out}"
    );
    let d = section_named(&out, "T::d");
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
        section_named(&out, "C::a").contains("0000 RETURN int(42)"),
        "{out}"
    );
    assert!(
        section_named(&out, "C::b").contains("0000 RETURN int(42)"),
        "{out}"
    );
    // A private constant of another class is not folded.
    assert!(
        section_named(&out, "C::c")
            .contains("FETCH_CLASS_CONSTANT string(\"P\") string(\"SECRET\")"),
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
    let a = section_named(&out, "T::a");
    assert!(
        a.contains("0000 T0 = FETCH_CLASS_CONSTANT (self) (exception) string(\"A\")\n0001 VERIFY_RETURN_TYPE T0\n"),
        "{out}"
    );
    assert!(
        section_named(&out, "T::b").contains("0000 RETURN int(42)"),
        "{out}"
    );
}

#[test]
fn calls_of_functions_returning_a_literal_are_inlined() {
    // gh11245_1.phpt: `xx()` is replaced by its literal.
    let out = dump("<?php\nfunction xx() { return \"s\"; }\necho xx();\n");
    let main = section_named(&out, "$_main");
    assert!(!main.contains("DO_UCALL"), "{out}");
    assert!(!main.contains("INIT_FCALL"), "{out}");
    assert!(main.contains("0000 ECHO string(\"s\")\n"), "{out}");
    // type_inference_final_class.phpt: a method of a final class; the
    // literal reaches the return check through a TMP.
    let out = dump(
        "<?php\nfinal class T {\n    public function getInt(): int { return 42; }\n    public function get(): int { return $this->getInt(); }\n}\n",
    );
    assert!(
        section_named(&out, "T::get")
            .contains("0000 T0 = VERIFY_RETURN_TYPE int(42)\n0001 RETURN T0"),
        "{out}"
    );
    // A method a child class may override is a prototype: not inlined.
    let out = dump(
        "<?php\nclass T {\n    public function getInt() { return 42; }\n    public function get() { return $this->getInt(); }\n}\n",
    );
    assert!(section_named(&out, "T::get").contains("DO_UCALL"), "{out}");
    // Typed parameters (`ZEND_ACC_HAS_TYPE_HINTS`) keep the call.
    let out = dump("<?php\nfunction f(int $a) { return 1; }\necho f(2);\n");
    assert!(section_named(&out, "$_main").contains("DO_UCALL"), "{out}");
    // Arguments sent to an inlined call are freed.
    let out = dump("<?php\nfunction f($a) { return 1; }\necho f($x);\n");
    let main = section_named(&out, "$_main");
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
    assert!(section_named(&out, "$_main").contains("DO_UCALL"), "{out}");
}

/// `spaceship_range`: the SSA dump after the DFA pass (`opt_debug_level`
/// `0x400000`): RECV of untyped parameters, the `SPACESHIP` range and the
/// `ADD` range and type derived from it, the unused initial CVs (`NOVAL`)
/// and the function's return info.
#[test]
fn ssa_dump_after_dfa_pass_infers_ranges() {
    let src = "<?php\nfunction test($a, $b) {\n    return ($a <=> $b) + 1;\n}\n";
    // Compiled at the level it is dumped at, as `include.rs` compiles a
    // script opcache dumps (the unreachable implicit `RETURN null` after
    // the explicit return is dropped by then).
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        optimization_level: crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL,
        ..CompileOptions::default()
    };
    let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
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

/// `opt/coalesce`: `??` and `??=` of a local nothing has assigned go on to
/// the next op, which is also where the op (kept: its operand is no
/// constant) jumps; the variable's first store goes with its last reader.
#[test]
fn coalesce_of_an_unassigned_local_goes_on() {
    let out = dump_optimized(
        "<?php\nfunction a() {\n    $test = $test ?? true;\n    return $test;\n}\n\
         function b() {\n    $test ??= true;\n    return $test;\n}\n\
         function h($x) {\n    if ($x) { echo 1; }\n    $test = $test ?? true;\n    return $test;\n}\n\
         function p($p) {\n    $x = $p ?? true;\n    return $x;\n}\n\
         function q($c) {\n    $v = $v ?? 1;\n    if ($c) { $v = 2; }\n    return $v;\n}\n",
    );
    let folded = "0000 T1 = COALESCE CV0($test) 0001\n0001 RETURN bool(true)\n";
    assert_eq!(out.matches(folded).count(), 2, "{out}");
    assert!(
        out.contains("0003 T2 = COALESCE CV1($test) 0004\n0004 RETURN bool(true)\n"),
        "{out}"
    );
    // A parameter may hold anything: both paths stay.
    assert!(
        out.contains("0001 T2 = COALESCE CV0($p) 0003\n0002 T2 = QM_ASSIGN bool(true)\n"),
        "{out}"
    );
    // A store that a later op reads stays.
    assert!(out.contains("CV1($v) = QM_ASSIGN int(1)\n"), "{out}");
}

/// `opt/dce_001`: concatenations of scalar-typed parameters that nothing
/// reads go, the inner ones with the outer one; an operand that may be an
/// array or an object keeps its concatenation (it may warn or call
/// `__toString()`).
#[test]
fn dead_concatenations_of_scalars_are_dropped() {
    let out = dump_optimized(
        "<?php\nfunction foo(string $s1, string $s2, string $s3, string $s4) {\n\
         $x = ($s1 . $s2) . ($s3 . $s4);\n    $x = 0;\n    return $x;\n}\n",
    );
    assert!(out.contains("(lines=5, args=4, vars=4, tmps=0)"), "{out}");
    assert!(out.contains("0004 RETURN int(0)\n"), "{out}");
    let kept = [
        // No declared type.
        "function foo($s1, string $s2) { $x = $s1 . $s2; $x = 0; return $x; }",
        // A type that takes an array.
        "function foo(array|string $s1, string $s2) { $x = $s1 . $s2; $x = 0; return $x; }",
        // A parameter the function assigns.
        "function foo(string $s1, string $s2) { $s1 = [$s2]; $x = $s1 . $s2; $x = 0; return $x; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("CONCAT"), "{body}: {out}");
    }
}

/// `opt/dce_007`: an array literal that nothing reads goes when building
/// it can raise nothing; a key after which an append fails, a variable
/// that may be unassigned (reading it warns) or a key that is no literal
/// keeps it.
#[test]
fn dead_array_builds_are_dropped() {
    let out = dump_optimized(
        "<?php\nfunction esc($x) {\n    $a = [$x];\n    $b = [$a];\n    return $a;\n}\n",
    );
    assert!(out.contains("(lines=3, args=1, vars=2, tmps=0)"), "{out}");
    assert!(
        out.contains("0001 CV1($a) = INIT_ARRAY 1 (packed) CV0($x) NEXT\n0002 RETURN CV1($a)\n"),
        "{out}"
    );
    let out = dump_optimized(
        "<?php\nfunction f($x) { $b = ['k' => $x, 2 => 1, $x, 'z' => $x]; return $x; }\n",
    );
    assert!(!out.contains("INIT_ARRAY"), "{out}");
    let kept = [
        // The append after the largest int key throws.
        "function f($x) { $b = [9223372036854775807 => $x, $x]; return $x; }",
        // `$u` may be unassigned.
        "function f($x) { if ($x) { $u = 1; } $b = [$u]; return $x; }",
        // The key is a variable.
        "function f($x) { $b = [$x => 1]; return $x; }",
        // `$x` is unset on the way.
        "function f($x) { unset($x); $b = [$x]; return 1; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("INIT_ARRAY"), "{body}: {out}");
    }
}

/// `opt/dce_009`: a `foreach` over a class constant that is an empty array
/// leaves nothing; over one with elements it stays.
#[test]
fn foreach_over_an_empty_class_constant_is_dropped() {
    let out = dump_optimized(
        "<?php\nclass Loop {\n    const VALUES = [];\n    const SOME = [1];\n\
         public static function test2() {\n        foreach (self::VALUES as $x) {\n        }\n    }\n\
         public static function kept() {\n        foreach (self::SOME as $x) {\n        }\n    }\n}\n",
    );
    let test2 = section_named(&out, "Loop::test2");
    assert!(test2.contains("(lines=1, args=0, vars=0, tmps=0)"), "{out}");
    assert!(test2.contains("0000 RETURN null"), "{out}");
    assert!(
        section_named(&out, "Loop::kept").contains("FE_RESET_R"),
        "{out}"
    );
}

/// `opt/dce_008`: a store of a locally built array of scalars into an
/// array nothing reads goes; one of a value that may hold an object, or of
/// a variable that may be unassigned, stays.
#[test]
fn dead_store_of_an_array_of_scalars_is_dropped() {
    let out = dump_optimized(
        "<?php\nfunction esc(int $x) {\n    $a[0] = $x;\n    $b[0] = $a;\n    return $a;\n}\n",
    );
    assert!(out.contains("(lines=4, args=1, vars=2, tmps=0)"), "{out}");
    assert!(
        out.contains("0001 ASSIGN_DIM CV1($a) int(0)\n0002 OP_DATA CV0($x)\n0003 RETURN CV1($a)\n"),
        "{out}"
    );
    let kept = [
        // An untyped parameter may hold an object.
        "function esc($x) { $a[0] = $x; $b[0] = $a; return $a; }",
        // `$a` may be unassigned where it is read.
        "function esc(int $x) { if ($x) { $a[0] = $x; } $b[0] = $a; return $x; }",
        // `$a` is assigned something else as well.
        "function esc(int $x) { $a[0] = $x; $a = new stdClass; $b[0] = $a; return $x; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("ASSIGN_DIM CV2($b)"), "{body}: {out}");
    }
}

/// `opt/sccp_005`, `opt/sccp_002`: an array literal with an element that
/// is no constant is known in its other elements, and so is what a store
/// under a literal key puts there; with every read a literal, the array
/// goes. An element under a key the build does not fix, or one a later
/// element replaces, is not known.
#[test]
fn array_literal_with_a_variable_is_partly_known() {
    let out =
        dump_optimized("<?php\nfunction foo(int $x) {\n    $a = [1,2,$x];\n    echo $a[1];\n}\n");
    assert!(out.contains("(lines=3, args=1, vars=1, tmps=0)"), "{out}");
    assert!(
        out.contains("0001 ECHO string(\"2\")\n0002 RETURN null\n"),
        "{out}"
    );
    let out = dump_optimized(
        "<?php\nfunction foo(int $x) {\n    $a = [$x];\n    $i = 1;\n    $c = $i < 2;\n    if ($c) {\n        $k = 2 * $i;\n        $a[$k] = $i;\n        echo $a[$k];\n    }\n    echo $a[2];\n}\n",
    );
    assert!(out.contains("(lines=4, args=1, vars=1, tmps=0)"), "{out}");
    assert!(
        out.contains("0001 ECHO string(\"1\")\n0002 ECHO string(\"1\")\n0003 RETURN null\n"),
        "{out}"
    );
    let kept = [
        // The element read is the one that is not known.
        "function foo(int $x) { $a = [1, 2, $x]; echo $a[2]; }",
        // A key that is no literal may be any key.
        "function foo(int $x) { $a = [1, $x => 5, 3]; echo $a[0]; }",
        // The known element is replaced by one that is not.
        "function foo(int $x) { $a = [1 => 7, 1 => $x]; echo $a[1]; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("FETCH_DIM_R"), "{body}: {out}");
    }
    // A later literal under the key of an unknown element is known again.
    let out =
        dump_optimized("<?php\nfunction foo(int $x) { $a = [1 => $x, 1 => 7]; echo $a[1]; }\n");
    assert!(out.contains("ECHO string(\"7\")"), "{out}");
}

/// What goes with an array nothing reads: an append only with a constant
/// value and room for it. php-src keeps `ASSIGN_DIM ... NEXT` of a value it
/// does not know, and the append after the largest int key throws.
#[test]
fn unread_append_stays_unless_constant_and_safe() {
    for (body, kept) in [
        ("$a = [$x]; $a[] = 1;", false),
        ("$a = [1, 2]; $a[] = $x;", true),
        ("$a = [9223372036854775807 => 5]; $a[] = 1;", true),
        ("$a[5] = $x; $a[] = 1;", false),
        ("$a = []; $a[] = $x;", true),
        ("$a = [1, 2]; $a[] = 3;", false),
        ("$a = [9223372036854775807 => $x]; $a[] = 1;", true),
        ("$a = [1]; $a[9223372036854775807] = $x; $a[] = 1;", true),
        ("$a = [1]; $a['9223372036854775807'] = $x; $a[] = 1;", true),
    ] {
        let out = dump_optimized(&format!(
            "<?php
function f(int $x) {{ {body} }}
"
        ));
        assert_eq!(out.contains("ASSIGN_DIM"), kept, "{body}: {out}");
    }
}

/// GH-24063: `0.0` and `-0.0` are identical (`===`) but two constants: a
/// join of the two knows no element, whichever way the arrays were made.
#[test]
fn signed_zeros_are_two_constants() {
    for body in [
        "function f(float $n) { if ($n < 0) { $a['k'] = -0.0; } else { $a['k'] = 0.0; } return $a['k']; }",
        "function f(float $n) { if ($n < 0) { $a = [1, -0.0]; } else { $a = [1, 0.0]; } return $a[1]; }",
        "function f(float $n, $x) { $a = ['x' => $x]; if ($n < 0) { $a['k'] = -0.0; } else { $a['k'] = 0.0; } return $a['k']; }",
        "function f(float $n) { $a = [5]; if ($n < 0) { $a[1] = [-0.0, 2]; } else { $a[1] = [0.0, 2]; } return $a[1][0]; }",
    ] {
        let out = dump_optimized(&format!("<?php
{body}
"));
        assert!(out.contains("FETCH_DIM_R"), "{body}: {out}");
    }
    // The same zero on both paths is one constant.
    let out = dump_optimized(
        "<?php
function f(float $n) { if ($n < 0) { $a = [1, -0.0]; } else { $a = [2, -0.0]; } return $a[1]; }
",
    );
    assert!(out.contains("RETURN float(-0)"), "{out}");
}

/// `opt/dce_004`: an array a variable holds only until the next assignment
/// replaces it goes, with the element stores into it; one that is read
/// first, holds a value that may be an object, or is replaced on one path
/// only, stays.
#[test]
fn array_replaced_before_it_is_read_is_dropped() {
    let out = dump_optimized(
        "<?php\nfunction foo(int $x, int $y) {\n    $a = [$x];\n    $a[1] = $y;\n    $a = $y;\n    return $a;\n}\n",
    );
    assert!(out.contains("(lines=4, args=2, vars=3, tmps=0)"), "{out}");
    assert!(
        out.contains("0002 CV2($a) = QM_ASSIGN CV1($y)\n0003 RETURN CV2($a)\n"),
        "{out}"
    );
    let kept = [
        // Read before it is replaced.
        "function foo(int $x, int $y) { $a = [$x]; $a[1] = $y; $b = count($a); $a = $y; return $a + $b; }",
        // An untyped parameter may hold an object.
        "function foo($x, int $y) { $a = [$x]; $a[1] = $y; $a = $y; return $a; }",
        // Replaced on one path only.
        "function foo(int $x, int $y) { $a = [$x]; $a[1] = $y; if ($x) { $a = $y; } return $a; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("INIT_ARRAY"), "{body}: {out}");
    }
}

/// `opt/sccp_017`, `opt/sccp_019`: once the read of a known element is a
/// literal, the copy of the locally built array and both arrays' element
/// stores go; a copy of a variable that may be unassigned (the read warns)
/// or may hold an object stays.
#[test]
fn dead_copies_of_an_array_of_scalars_are_dropped() {
    for body in [
        "$a[0] = 5; $a[1] = $x; $b = $a; return $b[0];",
        "$a[0] = 5; $a[1] = $x; $b = $a; $b[0] = 42; return $a[0];",
    ] {
        let out = dump_optimized(&format!("<?php\nfunction foo(int $x) {{ {body} }}\n"));
        assert!(
            out.contains("(lines=2, args=1, vars=1, tmps=0)"),
            "{body}: {out}"
        );
        assert!(out.contains("0001 RETURN int(5)\n"), "{body}: {out}");
    }
    let kept = [
        // `$a` may be unassigned where it is copied.
        "function foo(int $x) { if ($x) { $a[0] = 5; } $b = $a; return $x; }",
        // An untyped parameter may hold an object.
        "function foo($x) { $a[0] = $x; $b = $a; return 1; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("CV2($b)"), "{body}: {out}");
    }
}

/// An array literal that may be the last holder of an object keeps its
/// store, even into a variable nothing reads (the object lives to the end
/// of the function in PHP); one built from variables that keep their
/// values to the end does not.
#[test]
fn array_literal_holding_an_object_keeps_its_store() {
    let kept = [
        // The array is the object's only holder.
        "function f() { $a = [new stdClass]; echo 1; }",
        // `$o` gives its handle up before the end.
        "function f($o) { $b = [$o]; $o = null; echo 1; }",
        "function f() { $o = new stdClass; $b = ['k' => $o]; unset($o); echo 1; }",
        // An array of such an array.
        "function f() { $a = [new stdClass]; $b = [$a]; $a = null; echo 1; }",
    ];
    for body in kept {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(out.contains("INIT_ARRAY"), "{body}: {out}");
        assert!(!out.contains("FREE"), "{body}: {out}");
    }
    let dropped = [
        // A parameter the function never writes keeps its value.
        "function f($x) { $b = [$x]; echo 1; }",
        // So does a local assigned once, before.
        "function f($x) { $a = [$x]; $b = [$a]; return $a; }",
    ];
    for body in dropped {
        let out = dump_optimized(&format!("<?php\n{body}\n"));
        assert!(!out.contains("($b)"), "{body}: {out}");
    }
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

/// `zend_emit_jmp_null()` on the variable itself and
/// `zend_short_circuiting_commit()`: every `JMP_NULL` of a chain writes the
/// result of its last instruction, which under `isset()` is the
/// `ISSET_ISEMPTY_PROP_OBJ` (and the jump gives `false`).
#[test]
fn nullsafe_jumps_write_the_chain_result() {
    let out = dump("<?php\nfunction f($o) { return isset($o?->next?->foo); }\n");
    let s = section(&out, "f:");
    assert!(
        s.contains(
            "0001 T1 = JMP_NULL CV0($o) 0005\n\
             0002 T2 = FETCH_OBJ_IS CV0($o) string(\"next\")\n\
             0003 T1 = JMP_NULL T2 0005\n\
             0004 T1 = ISSET_ISEMPTY_PROP_OBJ (isset) T2 string(\"foo\")\n\
             0005 RETURN T1\n"
        ),
        "{s}"
    );
    let out = dump("<?php\nfunction f($o) { return $o?->foo; }\n");
    let s = section(&out, "f:");
    assert!(
        s.contains(
            "0001 T1 = JMP_NULL CV0($o) 0003\n\
             0002 T1 = FETCH_OBJ_R CV0($o) string(\"foo\")\n\
             0003 RETURN T1\n"
        ),
        "{s}"
    );
}

/// A chain on a variable known to be null is skipped: the use reads the
/// value it gives (`sccp.c`'s `ZEND_JMP_NULL` result and edge).
#[test]
fn known_null_chains_fold() {
    let out = dump_optimized(
        "<?php\nfunction f() { $n = null; return $n?->m()->foo; }\n\
         function g() { $n = null; return empty($n?->foo); }\n",
    );
    let has = |f: &str, line: &str| section(&out, f).lines().any(|l| l == line);
    assert!(has("f:", "0000 RETURN null"), "{out}");
    assert!(has("g:", "0000 RETURN bool(true)"), "{out}");
}

/// `place_essa_pis()`: the fall-through edge of a `JMP_NULL` of a CV
/// narrows the variable to non-null, so the fetch reads an object.
#[test]
fn jmp_null_places_a_pi_on_the_fall_through_edge() {
    let src = "<?php\nclass T { public int $p; }\nfunction f(?T $t) { var_dump($t?->p); }\n";
    let unit = compile_src(src);
    let table = |_: &[u8]| -> Option<FunctionInfo> { None };
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let end = main_line_end(src.as_bytes(), None);
    let level = crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    let out = String::from_utf8(super::dump_unit_dfa(
        &unit,
        &table,
        &flags,
        level,
        end,
        super::DUMP_BEFORE_DFA_PASS,
    ))
    .expect("utf-8");
    assert!(
        out.contains(
            " = Pi<BB0>(#1.CV0($t) [null, object (instanceof T)] & TYPE [ref, bool, long, \
             double, string, array of [any, ref], object, resource])\n"
        ),
        "{out}"
    );
    assert!(out.contains("[null] = JMP_NULL"), "{out}");
}

/// `zend_compile_args_ex` with a known function: named arguments in order
/// are positional; one out of order stays named (with the parameter's
/// send mode), the `INIT_*` count is the positional ones, and
/// `CHECK_UNDEF_ARGS` precedes the call.
#[test]
fn named_arguments_resolve_against_a_known_function() {
    let out = dump(
        "<?php\nfunction f($a, $b = 2, &$c = null) { return $a; }\n\
         function t1() { return f(a: 1); }\n\
         function t2() { return f(b: 5, a: 1); }\n\
         function t3($v) { return f(1, c: $v); }\n\
         function t4() { return unknown_fn(a: 1); }\n",
    );
    let s = section(&out, "t1:");
    assert!(
        s.contains("0000 INIT_FCALL 1 ") && s.contains("0001 SEND_VAL int(1) 1\n"),
        "{s}"
    );
    let s = section(&out, "t2:");
    assert!(
        s.contains("0000 INIT_FCALL 0 ")
            && s.contains(
                "0001 SEND_VAL int(5) string(\"b\")\n\
                 0002 SEND_VAL int(1) string(\"a\")\n\
                 0003 CHECK_UNDEF_ARGS\n"
            ),
        "{s}"
    );
    let s = section(&out, "t3:");
    assert!(
        s.contains("0001 INIT_FCALL 1 ")
            && s.contains("0003 SEND_REF CV0($v) string(\"c\")\n0004 CHECK_UNDEF_ARGS\n"),
        "{s}"
    );
    let s = section(&out, "t4:");
    assert!(
        s.contains(
            "0000 INIT_FCALL_BY_NAME 0 string(\"unknown_fn\")\n\
             0001 SEND_VAL_EX int(1) string(\"a\")\n\
             0002 CHECK_UNDEF_ARGS\n"
        ),
        "{s}"
    );
}

/// `zend_ssa_escape_analysis`: an array only a local variable holds does
/// not escape (`NOESC`, with the types `assign_dim_array_result_type`
/// gives it); a returned one does.
#[test]
fn local_arrays_do_not_escape() {
    let src = "<?php\nfunction f(int $p, int $k) { $a = [$p]; $r = ($a[$k] += 1); return $r; }\n\
               function g(int $p) { $a = [$p]; return $a; }\n";
    let unit = compile_src(src);
    let table = |_: &[u8]| -> Option<FunctionInfo> { None };
    let flags = |_: &[u8]| -> (u32, Option<crate::types::TypeDecl>) { (0, None) };
    let end = main_line_end(src.as_bytes(), None);
    let level = crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    let out = String::from_utf8(super::dump_unit_dfa(
        &unit,
        &table,
        &flags,
        level,
        end,
        super::DUMP_AFTER_DFA_PASS,
    ))
    .expect("utf-8");
    let f = section(&out, "f:");
    assert!(
        f.contains("NOESC [[packed, hash] array [long] of [long]] = INIT_ARRAY 1 (packed)"),
        "{f}"
    );
    assert!(
        f.contains("NOVAL NOESC [[packed, hash] array [long] of [long, double]]"),
        "{f}"
    );
    assert!(f.contains("; return  [long, double]"), "{f}");
    let g = section(&out, "g:");
    assert!(g.contains("INIT_ARRAY") && !g.contains("NOESC"), "{g}");
}

/// The block pass assembles the live blocks only (`zend_optimize_cfg`,
/// `zend_mark_reachable_blocks`): after an endless loop in a `try` the
/// `FAST_CALL` and the return go, while the `finally` block and its
/// `FAST_RET` stay live through the exception path. Without pass 9 the
/// temporaries keep `zend_compile`'s numbers: the `BOOL_NOT` folded into
/// the jump and the `finally` variable allocated where the `try` starts.
#[test]
fn block_pass_keeps_only_live_blocks() {
    let src = "<?php\nif (!isset($badvar)) {\n    throw new Exception(\"a\");\n}\n\
               try {\n    while (true) { }\n} finally {\n    throw new Exception(\"b\");\n}\n";
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let level = crate::compiler::optimizer::PASS_5;
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
    assert!(out.contains("0001 JMPNZ T1 0006\n0002 T3 = NEW 1"), "{out}");
    assert!(out.contains("0006 JMP 0006\n0007 T6 = NEW 1"), "{out}");
    assert!(out.contains("0011 FAST_RET T5\nEXCEPTION TABLE:"), "{out}");
    assert!(!out.contains("RETURN"), "{out}");
}
