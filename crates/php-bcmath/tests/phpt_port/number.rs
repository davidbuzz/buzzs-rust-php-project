// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Transcriptions of ext/bcmath/tests/number/**/*.phpt (`BcMath\Number`).

use super::harness::*;
use super::runner::Phpt;
use php_bcmath::{BinaryOp, Number, Operand, RoundingMode};

fn values_mixed() -> Vec<V> {
    vec![
        s("0"),
        s("0.0"),
        s("2"),
        s("1234"),
        s("12.0004"),
        s("0.1230"),
        i(1),
        i(12345),
    ]
}

fn values_signed() -> Vec<V> {
    let mut v = values_mixed();
    v.extend([
        s("-0"),
        s("-0.0"),
        s("-2"),
        s("-1234"),
        s("-12.0004"),
        s("-0.1230"),
        i(-1),
        i(-12345),
    ]);
    v
}

pub fn cast(php: &mut Php, _: &Phpt) {
    for v in values_signed() {
        let n = num(v.clone());
        php.line(format!("========== {} ==========", v.str()));
        let (value, scale) = n.debug_info();
        php.line("array(3) {");
        php.line("  [\"bool\"]=>");
        php.line(format!("  bool({})", n.is_truthy()));
        php.line("  [\"string\"]=>");
        php.line(format!(
            "  string({}) \"{}\"",
            n.to_php_string().len(),
            n.to_php_string()
        ));
        php.line("  [\"array\"]=>");
        php.line("  array(2) {");
        php.line("    [\"value\"]=>");
        php.line(format!("    string({}) \"{value}\"", value.len()));
        php.line("    [\"scale\"]=>");
        php.line(format!("    int({scale})"));
        php.line("  }");
        php.line("}");
        php.line("");
    }
}

pub fn clone(php: &mut Php, _: &Phpt) {
    for v in values_signed() {
        let n = num(v);
        let c = n.clone();
        if n.value() != c.value() || n.scale() != c.scale() {
            php.line("Result is incorrect.");
        }
    }
    php.echo("done!");
}

pub fn construct_64bit(php: &mut Php, _: &Phpt) {
    let values = [
        s("0"),
        s("0.00"),
        s(".1"),
        s("0.1"),
        s("0.20"),
        s("0.00000030"),
        s("1234.0"),
        s("123450"),
        s(""),
        s("-"),
        s("."),
        i(0),
        i(123),
        i(9_223_372_036_854_775_806),
        i(i64::MAX),
        i(-9_223_372_036_854_775_807),
        i(i64::MIN),
    ];
    for v in values {
        let n = num(v);
        php.dump_number(&n);
    }
}

pub fn construct_error(php: &mut Php, _: &Phpt) {
    let r = Number::construct(true, Operand::Int(5));
    php.catch(r);
    let r = Number::construct(false, "a".into());
    if let Some(n) = php.catch(r) {
        php.dump_number(&n);
    }
}

pub fn construct_float(php: &mut Php, _: &Phpt) {
    let r = php
        .zpp_operand("BcMath\\Number::__construct", 1, "num", &V::F(0.1234))
        .expect("ok");
    let n = Number::new(r).expect("ok");
    php.dump_number(&n);
}

pub fn properties_read(php: &mut Php, _: &Phpt) {
    for v in values_mixed() {
        let n = num(v);
        php.line(format!("value: {}", n.value()));
        php.line(format!("scale: {}", n.scale()));
        php.line("");
    }
}

pub fn properties_isset_empty_exists(php: &mut Php, _: &Phpt) {
    let nums = [("zero", num(0)), ("one", num(1)), ("has_frac", num("5.67"))];
    let section = |php: &mut Php, title: &str, f: &dyn Fn(&Number, &str) -> bool| {
        php.line(format!("========== {title} =========="));
        php.line("array(3) {");
        for (name, n) in &nums {
            php.line(format!("  [\"{name}\"]=>"));
            php.line("  array(2) {");
            for prop in ["value", "scale"] {
                php.line(format!("    [\"{prop}\"]=>"));
                php.line(format!("    bool({})", f(n, prop)));
            }
            php.line("  }");
        }
        php.line("}");
    };
    section(php, "isset", &|n, p| {
        n.has_property(p, false).expect("known")
    });
    php.line("");
    section(php, "empty", &|n, p| {
        !n.has_property(p, true).expect("known")
    });
    php.line("");
    // property_exists() sees the declared properties.
    section(php, "property_exists", &|n, p| {
        n.has_property(p, false).is_some()
    });
}

pub fn properties_write_unset(php: &mut Php, t: &Phpt) {
    let unset = t.file().contains("unset(");
    for prop in ["value", "scale"] {
        let e = if unset {
            php_bcmath::BcError::readonly_unset(prop)
        } else {
            php_bcmath::BcError::readonly_modification(prop)
        };
        php.catch::<()>(Err(e));
    }
}

pub fn serialize(php: &mut Php, _: &Phpt) {
    for v in values_mixed() {
        let n = num(v);
        let value = n.serialize();
        php.line(format!(
            "O:13:\"BcMath\\Number\":1:{{s:5:\"value\";s:{}:\"{value}\";}}",
            value.len()
        ));
    }
}

pub fn unserialize(php: &mut Php, _: &Phpt) {
    for v in ["0", "0.0", "2", "1234", "12.0004", "0.1230", "1", "12345"] {
        let n = Number::unserialize(false, Some(v.as_bytes())).expect("ok");
        php.dump_number(&n);
    }
}

pub fn unserialize_error(php: &mut Php, _: &Phpt) {
    let r = Number::unserialize(true, Some(b"5"));
    php.catch(r);
    php.line("");
    // value "a", value "", no value, int value.
    for v in [Some(&b"a"[..]), Some(b""), None, None] {
        let r = Number::unserialize(false, v);
        php.catch(r);
    }
}

pub fn gh15968(php: &mut Php, _: &Phpt) {
    let a = nv("1");
    let r = php.binop(BinaryOp::Add, &a, &V::Obj("MyString"));
    if let Some(n) = php.catch(r) {
        php.dump_number(&n);
    }
}

pub fn gh18641(php: &mut Php, _: &Phpt) {
    // $x = &$fusion->value reads through the property handler.
    let a = num("1");
    php.dump_str(&a.value());
}

// ---------------------------------------------------------------------------
// methods/
// ---------------------------------------------------------------------------

fn values2_basic() -> Vec<(V, &'static str)> {
    vec![
        (i(100), "int"),
        (i(-30), "int"),
        (s("-20"), "string"),
        (s("0.01"), "string"),
        (s("-0.40"), "string"),
        (nv("80.3"), "object"),
        (nv("-50.6"), "object"),
    ]
}

/// add/sub/mul/div/mod: `var_dump($num->$method($value2))`.
pub fn method_basic(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let method = ["add", "sub", "mul", "div", "mod"]
        .into_iter()
        .find(|m| file.contains(&format!("$num->{m}(")))
        .expect("method");
    let symbol = file[file.find("echo \"{$value1} ").expect("echo") + "echo \"{$value1} ".len()..]
        .split(' ')
        .next()
        .expect("symbol")
        .to_string();
    for v1 in ["100.012", "-100.012"] {
        let n = num(v1);
        for (v2, ty) in values2_basic() {
            php.line(format!("{v1} {symbol} {}: {ty}", v2.str()));
            let r = php.method(&n, method, &v2, None).expect("ok");
            php.dump_number(&r);
            php.line("");
        }
    }
}

/// `*_with_scale.phpt`: method vs function at each scale.
pub fn method_with_scale(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let method = ["add", "sub", "mul", "div", "mod"]
        .into_iter()
        .find(|m| file.contains(&format!("$num->{m}(")))
        .expect("method");
    let func = format!("bc{method}");
    let mut values2 = values2_basic();
    if method == "add" || method == "sub" {
        values2.insert(2, (s("100.012"), "string"));
        values2.insert(3, (s("-100.012"), "string"));
    }
    for scale in 0..=4 {
        for v1 in ["100.012", "-100.012"] {
            let n = num(v1);
            for (v2, _) in &values2 {
                let f = bc_binary(php.bc, &func, v1, &v2.str(), sc(scale)).expect("ok");
                let m = php.method(&n, method, v2, Some(&V::I(scale))).expect("ok");
                if m.compare(f.as_str().into(), None).expect("ok") != 0 {
                    php.line("Result is incorrect.");
                }
            }
        }
    }
    php.echo("done!");
}

pub fn calc_methods_num_arg_error(php: &mut Php, _: &Phpt) {
    let args = [
        (s("a"), "non number str"),
        (V::Arr, "array"),
        (V::Obj("stdClass"), "other object"),
        (V::F(0.1), "float"),
        (V::Null, "null"),
    ];
    let n = num("100.0000");
    for method in ["add", "sub", "mul", "div", "mod", "pow"] {
        php.line(format!("========== {method} =========="));
        for (v, ty) in &args {
            php.line(format!("{ty}:"));
            let r = php.method(&n, method, v, None);
            php.catch(r);
        }
        php.line("");
    }
}

pub fn calc_methods_scale_arg_error(php: &mut Php, _: &Phpt) {
    let args = [
        (V::Arr, "array"),
        (V::Obj("stdClass"), "other object"),
        (V::F(0.1), "float"),
    ];
    let n = num("100.0000");
    for method in ["add", "sub", "mul", "div", "mod", "pow"] {
        php.line(format!("========== {method} =========="));
        for (v, ty) in &args {
            php.line(format!("{ty}:"));
            let r = php.method(&n, method, &i(1), Some(v));
            php.catch(r);
        }
        php.line("");
    }
}

pub fn calc_methods_zero_result_sign(php: &mut Php, _: &Phpt) {
    let cases: [(&str, &str, V, i64); 8] = [
        ("div", "-0.001", s("1"), 0),
        ("div", "0.001", s("-1"), 0),
        ("div", "-0.001", s("10"), 2),
        ("div", "-0.001", s("0.1"), 1),
        ("add", "-0.001", s("-0.001"), 0),
        ("sub", "-0.001", s("0.001"), 0),
        ("mul", "-0.001", s("1"), 0),
        ("pow", "-0.1", i(3), 2),
    ];
    for (method, n, arg, scale) in cases {
        let r = php
            .method(&num(n), method, &arg, Some(&V::I(scale)))
            .expect("ok");
        let c = r.compare(Operand::Int(0), None).expect("ok");
        php.line(format!(
            "{n} {method} {}: {} {c} {}",
            arg.str(),
            r.value(),
            tf(c == 0)
        ));
    }
    let (q, _) = num("-0.001").divmod("1".into(), Some(0)).expect("ok");
    let c = q.compare(Operand::Int(0), None).expect("ok");
    php.line(format!("-0.001 divmod 1: {} {c} {}", q.value(), tf(c == 0)));
}

pub fn floor_ceil(php: &mut Php, t: &Phpt) {
    let floor = t.file().contains("->floor()");
    let vars = super::runner::string_arrays(t.file());
    for n in &vars["nums"] {
        let f = if floor {
            php.bc.bcfloor(n.as_bytes())
        } else {
            php.bc.bcceil(n.as_bytes())
        }
        .expect("ok");
        let x = num(n.as_str());
        let m = if floor { x.floor() } else { x.ceil() };
        if m.compare(f.as_str().into(), Some(0)).expect("ok") != 0 {
            php.line("Result is incorrect.");
        }
    }
    php.echo("done!");
}

pub fn compare_arg_error(php: &mut Php, _: &Phpt) {
    let args = [
        (s("a"), "non number str"),
        (V::Arr, "array"),
        (V::Obj("stdClass"), "other object"),
        (V::F(0.1), "float"),
        (V::Null, "null"),
    ];
    let n = num("100.0000");
    for (v, ty) in &args {
        php.line(format!("{ty}:"));
        let r = php
            .zpp_operand("BcMath\\Number::compare", 1, "num", v)
            .and_then(|op| n.compare(op, None));
        php.catch(r);
        php.line("");
    }
}

fn compare_values() -> Vec<(V, &'static str)> {
    vec![
        (i(99), "int"),
        (s("99.9999"), "string"),
        (nv("99.9999"), "object"),
        (i(100), "int"),
        (s("100"), "string"),
        (s("100.0000"), "string"),
        (nv(100), "object"),
        (nv("100.0000"), "object"),
        (i(101), "int"),
        (s("100.00001"), "string"),
        (nv("100.00001"), "object"),
    ]
}

pub fn compare(php: &mut Php, _: &Phpt) {
    let v1 = num("100.0000");
    for (v2, ty) in compare_values() {
        php.line(format!("========== with {ty} {} ==========", v2.str()));
        let c = v1.compare(operand(&v2), None).expect("ok");
        php.dump_int(c);
        php.line("");
    }
}

pub fn compare_with_scale(php: &mut Php, _: &Phpt) {
    let values = [
        (i(100), "int"),
        (s("100.0000"), "string"),
        (nv("100.0000"), "object"),
        (s("100.0001"), "string"),
        (nv("100.0001"), "object"),
        (s("100.0010"), "string"),
        (nv("100.0010"), "object"),
        (s("100.0100"), "string"),
        (nv("100.0100"), "object"),
        (s("100.0011"), "string"),
        (nv("100.0011"), "object"),
    ];
    let v1 = num("100.0011");
    for scale in 0..=5 {
        php.line(format!("========== scale is {scale} =========="));
        for (v2, ty) in &values {
            php.line(format!("with {ty} {}:", v2.str()));
            let c = v1.compare(operand(v2), Some(scale)).expect("ok");
            php.dump_int(c);
            php.line("");
        }
    }
}

pub fn compare_near_zero(php: &mut Php, _: &Phpt) {
    let values1 = [num("0.001"), num("-0.001")];
    let values2 = [i(0), i(0), s("0.0011"), s("-0.0011")];
    for scale in [0, 2, 3, 4] {
        php.line(format!("========== scale is {scale} =========="));
        for v1 in &values1 {
            for v2 in &values2 {
                let c = v1.compare(operand(v2), Some(scale)).expect("ok");
                let label = pad_left(&format!("{} <=> {}: ", v1.value(), v2.str()), 20);
                php.line(format!("{label}{}", pad_left(&c.to_string(), 2)));
            }
        }
    }
}

pub fn div_mod_by_zero(php: &mut Php, _: &Phpt) {
    let values2 = [
        (i(0), "int"),
        (i(0), "int"),
        (s("0"), "string"),
        (s("-0"), "string"),
        (s("0.000"), "string"),
        (s("-0.000"), "string"),
        (nv("0"), "object"),
        (nv("-0"), "object"),
        (nv("0.000"), "object"),
        (nv("-0.000"), "object"),
    ];
    for v1 in ["100", "-100"] {
        let n = num(v1);
        for (v2, ty) in &values2 {
            php.line(format!("{v1} and {}: {ty}", v2.str()));
            let check = |r: Result<(), php_bcmath::BcError>, msg: &str| match r {
                Err(e) if e.message == msg => "OK",
                _ => "NG",
            };
            let d = check(n.div(operand(v2), None).map(|_| ()), "Division by zero");
            php.line(format!("div: {d}"));
            let m = check(n.modulo(operand(v2), None).map(|_| ()), "Modulo by zero");
            php.line(format!("mod: {m}"));
            let dm = check(n.divmod(operand(v2), None).map(|_| ()), "Division by zero");
            php.line(format!("divmod: {dm}"));
            php.line("");
        }
    }
}

pub fn divmod(php: &mut Php, _: &Phpt) {
    for v1 in ["100.012", "-100.012"] {
        let n = num(v1);
        for (v2, ty) in values2_basic() {
            php.line(format!("{v1} divmod {}: {ty}", v2.str()));
            let (q, r) = n.divmod(operand(&v2), None).expect("ok");
            php.dump_number(&q);
            php.dump_number(&r);
            php.line("");
        }
    }
}

pub fn divmod_with_scale(php: &mut Php, _: &Phpt) {
    for scale in 0..=10 {
        for v1 in ["100.012", "-100.012"] {
            let n = num(v1);
            for (v2, _) in values2_basic() {
                let (fq, fr) = php
                    .bc
                    .bcdivmod(v1.as_bytes(), v2.str().as_bytes(), sc(scale))
                    .expect("ok");
                let (mq, mr) = n.divmod(operand(&v2), Some(scale)).expect("ok");
                if mq.compare(fq.as_str().into(), None).expect("ok") != 0 {
                    php.line("Quot is incorrect.");
                }
                if mr.compare(fr.as_str().into(), None).expect("ok") != 0 {
                    php.line("Rem is incorrect.");
                }
            }
        }
    }
    php.echo("done!");
}

fn pow_exponents() -> Vec<(V, &'static str)> {
    vec![
        (i(2), "int"),
        (i(-3), "int"),
        (s("-2"), "string"),
        (s("0"), "string"),
        (nv("2"), "object"),
        (nv("-2"), "object"),
        (nv("0"), "object"),
    ]
}

pub fn pow(php: &mut Php, _: &Phpt) {
    for v in ["12.5", "-12.5"] {
        let n = num(v);
        for (e, ty) in pow_exponents() {
            php.line(format!("{v} ** {}: {ty}", e.str()));
            let r = n.pow(operand(&e), None).expect("ok");
            php.dump_number(&r);
            php.line("");
        }
    }
}

pub fn pow_with_scale(php: &mut Php, _: &Phpt) {
    for scale in 0..=10 {
        for v in ["12.5", "-12.5"] {
            let n = num(v);
            for (e, _) in pow_exponents() {
                let f = php
                    .bc
                    .bcpow(v.as_bytes(), e.str().as_bytes(), sc(scale))
                    .expect("ok");
                let m = n.pow(operand(&e), Some(scale)).expect("ok");
                if m.compare(f.as_str().into(), None).expect("ok") != 0 {
                    php.line("Result is incorrect.");
                }
            }
        }
    }
    php.echo("done!");
}

/// `methods/pow_div_by_zero.phpt` and `operators/pow_div_by_zero.phpt`.
pub fn pow_div_by_zero(php: &mut Php, t: &Phpt) {
    let operator = t.file().contains("$num ** $exponent");
    let exponents = [(i(-3), "int"), (s("-2"), "string"), (nv("-2"), "object")];
    for v in [i(0), s("0")] {
        let n = num(v.clone());
        for (e, ty) in &exponents {
            php.line(format!("{} ** {}: {ty}", v.str(), e.str()));
            let r = if operator {
                php.binop(BinaryOp::Pow, &V::N(n.clone()), e)
            } else {
                n.pow(operand(e), None)
            };
            php.catch(r);
        }
    }
}

type Labeled = Vec<(V, &'static str)>;

fn powmod_cases() -> (Labeled, Labeled) {
    (
        vec![
            (i(2), "int"),
            (s("0"), "string"),
            (s("3"), "string"),
            (nv("2"), "object"),
            (nv("0"), "object"),
        ],
        vec![(i(2), "int"), (s("3"), "string"), (nv("2"), "object")],
    )
}

pub fn powmod(php: &mut Php, _: &Phpt) {
    let (exps, mods) = powmod_cases();
    for v in ["12", "-12"] {
        let n = num(v);
        for (e, te) in &exps {
            for (m, tm) in &mods {
                php.line(format!(
                    "{v}, expo is {}({te}), mod is {}({tm}):",
                    e.str(),
                    m.str()
                ));
                let r = n.powmod(operand(e), operand(m), None).expect("ok");
                php.dump_number(&r);
                php.line("");
            }
        }
    }
}

pub fn powmod_with_scale(php: &mut Php, _: &Phpt) {
    let (exps, mods) = powmod_cases();
    for scale in [0, 10] {
        for v in ["12", "-12"] {
            let n = num(v);
            for (e, _) in &exps {
                for (m, _) in &mods {
                    let f = php
                        .bc
                        .bcpowmod(
                            v.as_bytes(),
                            e.str().as_bytes(),
                            m.str().as_bytes(),
                            sc(scale),
                        )
                        .expect("ok");
                    let r = n.powmod(operand(e), operand(m), Some(scale)).expect("ok");
                    if r.compare(f.as_str().into(), None).expect("ok") != 0 {
                        php.line("Result is incorrect.");
                    }
                }
            }
        }
    }
    php.echo("done!");
}

pub fn powmod_arg_error(php: &mut Php, _: &Phpt) {
    let args = [
        (s("a"), "non number str"),
        (V::Arr, "array"),
        (V::Obj("stdClass"), "other object"),
        (V::F(0.1), "float"),
        (V::Null, "null"),
    ];
    let n = num("100.0000");
    let f = "BcMath\\Number::powmod";
    php.line("========== check 1st arg ==========");
    for (v, ty) in &args {
        php.line(format!("{ty}:"));
        let r = php
            .zpp_operand(f, 1, "exponent", v)
            .and_then(|e| n.powmod(e, Operand::Int(1), None));
        php.catch(r);
    }
    php.line("");
    php.line("========== check 2nd arg ==========");
    for (v, ty) in &args {
        php.line(format!("{ty}:"));
        let r = php
            .zpp_operand(f, 2, "modulus", v)
            .and_then(|m| n.powmod(Operand::Int(1), m, None));
        php.catch(r);
    }
}

pub fn round_has_trailing_zeros(php: &mut Php, _: &Phpt) {
    for mode in RoundingMode::ALL {
        php.line(format!("========== {} ==========", mode.name()));
        for (v, p) in [("0.00", 1), ("0.1", 5), ("-0.300", 5), ("1.995", 2)] {
            let r = num(v).round(p, mode).expect("ok");
            php.line(format!(
                "value is {}, precision is {p}: {}",
                pad_right(v, 6),
                r.to_php_string()
            ));
        }
        php.line("");
    }
}

pub fn round(php: &mut Php, _: &Phpt) {
    let check = |php: &mut Php, n: &str, p: i64, mode| {
        let f = php.bc.bcround(n.as_bytes(), p, mode).expect("ok");
        let m = num(n).round(p, mode).expect("ok");
        if m.compare(f.as_str().into(), None).expect("ok") != 0 {
            php.line("Result is incorrect.");
        }
    };
    for mode in RoundingMode::ALL {
        for n in [
            "0", "0.1", "-0.1", "1.0", "-1.0", "1.2", "-1.2", "1.7", "-1.7", "1.5", "-1.5", "2.5",
            "-2.5",
        ] {
            for p in [0, 5, -5] {
                check(php, n, p, mode);
            }
        }
    }
    for mode in RoundingMode::ALL {
        for n in ["0", "1.2345678", "-1.2345678"] {
            check(php, n, 5, mode);
        }
    }
    php.echo("done!");
}

pub fn sqrt(php: &mut Php, _: &Phpt) {
    for r in [
        "15151324141414.412312232141241",
        "141241241241241248267654747412",
        "0.1322135476547459213732911312",
    ] {
        let m = num(r).sqrt(None).expect("ok");
        php.dump_number(&m);
    }
}

pub fn sqrt_with_scale(php: &mut Php, t: &Phpt) {
    let vars = super::runner::string_arrays(t.file());
    for scale in [0, 10] {
        for r in &vars["radicants"] {
            let f = php.bc.bcsqrt(r.as_bytes(), sc(scale)).expect("ok");
            let m = num(r.as_str()).sqrt(Some(scale)).expect("ok");
            if m.compare(f.as_str().into(), None).expect("ok") != 0 {
                php.line("Result is incorrect.");
            }
        }
    }
    php.echo("done!");
}

// ---------------------------------------------------------------------------
// operators/
// ---------------------------------------------------------------------------

fn op_from_symbol(sym: &str) -> BinaryOp {
    match sym {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "**" => BinaryOp::Pow,
        _ => panic!("op {sym}"),
    }
}

/// Parses `$values = [...]` with int and string items.
fn mixed_values(file: &str, var: &str) -> Vec<V> {
    let start = file.find(&format!("${var} = [")).expect("values") + var.len() + 5;
    let body = &file[start..start + file[start..].find("];").expect("end")];
    body.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| {
            if x.starts_with('\'') || x.starts_with('"') {
                s(x.trim_matches(['\'', '"']))
            } else {
                i(x.parse().expect("int"))
            }
        })
        .collect()
}

/// `{add,sub,mul,div,mod}_{int,string,object}.phpt`.
pub fn operator_values(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let values = mixed_values(file, "values");
    let sym_start = file.find("echo \"{$value1} ").expect("echo") + "echo \"{$value1} ".len();
    let sym = file[sym_start..].split(' ').next().expect("sym");
    let op = op_from_symbol(sym);
    let kind = if file.contains("((int) $value2)") {
        "int"
    } else if file.contains("((string) $value2)") {
        "string"
    } else {
        "object"
    };
    for v1 in &values {
        let n1 = num(v1.clone());
        for v2 in &values {
            php.line(format!("{} {sym} {}", v1.str(), v2.str()));
            let (ret, ret2) = match kind {
                "int" => (
                    php.binop(op, &V::N(n1.clone()), &i(v2.int())),
                    Some(php.binop(op, &i(v1.int()), &nv(v2.clone()))),
                ),
                "string" => (
                    php.binop(op, &V::N(n1.clone()), &s(&v2.str())),
                    Some(php.binop(op, &s(&v1.str()), &nv(v2.clone()))),
                ),
                _ => (php.binop(op, &V::N(n1.clone()), &nv(v2.clone())), None),
            };
            let ret = ret.expect("ok");
            if let Some(ret2) = ret2 {
                if ret.compare((&ret2.expect("ok")).into(), None).expect("ok") != 0 {
                    php.line("Result is incorrect.");
                }
            }
            php.dump_number(&ret);
            php.line("");
        }
    }
}

/// `pow_{int,string,object}.phpt`.
pub fn operator_pow(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let values = mixed_values(file, "values");
    let exponents = mixed_values(file, "exponents");
    let kind = if file.contains("((int) $exponent)") {
        "int"
    } else if file.contains("((string) $exponent)") {
        "string"
    } else {
        "object"
    };
    for v in &values {
        let n1 = num(v.clone());
        for e in &exponents {
            php.line(format!("{} ** {}", v.str(), e.str()));
            let (ret, ret2) = match kind {
                "int" => (
                    php.binop(BinaryOp::Pow, &V::N(n1.clone()), &i(e.int())),
                    Some(php.binop(BinaryOp::Pow, &i(v.int()), &nv(e.clone()))),
                ),
                "string" => (
                    php.binop(BinaryOp::Pow, &V::N(n1.clone()), &s(&e.str())),
                    Some(php.binop(BinaryOp::Pow, &s(&v.str()), &nv(e.clone()))),
                ),
                _ => (
                    php.binop(BinaryOp::Pow, &V::N(n1.clone()), &nv(e.clone())),
                    None,
                ),
            };
            let ret = ret.expect("ok");
            if let Some(ret2) = ret2 {
                if ret.compare((&ret2.expect("ok")).into(), None).expect("ok") != 0 {
                    php.line("Result is incorrect.");
                }
            }
            php.dump_number(&ret);
            php.line("");
        }
    }
}

/// `calc_{array,null,undef,other_class,non_numeric_string,float}.phpt`.
pub fn operator_bad_operand(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let (rhs, undef) = if file.contains("$array") {
        (V::Arr, false)
    } else if file.contains("$undef") {
        (V::Null, true)
    } else if file.contains("$other") {
        (V::Obj("stdClass"), false)
    } else if file.contains("'a'") {
        (s("a"), false)
    } else if file.contains("1.01") {
        (V::F(1.01), false)
    } else {
        (V::Null, false)
    };
    let n = nv(100);
    for op in [
        BinaryOp::Add,
        BinaryOp::Sub,
        BinaryOp::Mul,
        BinaryOp::Div,
        BinaryOp::Mod,
        BinaryOp::Pow,
    ] {
        if undef {
            php.warning("Undefined variable $undef");
        }
        let r = php.binop(op, &n, &rhs);
        php.catch(r);
    }
}

/// `div_by_zero.phpt` / `mod_by_zero.phpt`.
pub fn operator_by_zero(php: &mut Php, t: &Phpt) {
    let op = if t.file().contains("$num / 0") {
        BinaryOp::Div
    } else {
        BinaryOp::Mod
    };
    let n = nv(100);
    let zero = nv(0);
    for (a, b) in [
        (&n, &i(0)),
        (&n, &s("0")),
        (&n, &nv(0)),
        (&i(100), &zero),
        (&s("100"), &zero),
        (&n, &zero),
    ] {
        let r = php.binop(op, a, b);
        php.catch(r);
    }
}

pub fn compound_assignment(php: &mut Php, _: &Phpt) {
    let groups = [
        ("add", BinaryOp::Add, 100, [i(100), s("50"), nv(30)]),
        ("sub", BinaryOp::Sub, 100, [i(100), s("50"), nv(30)]),
        ("mul", BinaryOp::Mul, 100, [i(100), s("50"), nv(30)]),
        ("div", BinaryOp::Div, 100, [i(100), s("50"), nv(30)]),
        ("mod", BinaryOp::Mod, 1000, [i(90), s("30"), nv(6)]),
        ("pow", BinaryOp::Pow, 10, [i(2), s("3"), nv(0)]),
    ];
    for (name, op, start, rhs) in groups {
        php.line(format!("========== {name} =========="));
        let old = nv(start);
        let mut cur = old.clone();
        for r in &rhs {
            cur = V::N(php.binop(op, &cur, r).expect("ok"));
            php.line(cur.str());
        }
        php.line(format!("old: {}", old.str()));
        php.line("");
    }
}

/// `increment.phpt` / `decrement.phpt`.
pub fn inc_dec(php: &mut Php, t: &Phpt) {
    let inc = t.file().contains("$num++");
    for v in mixed_values(t.file(), "values") {
        php.line(format!(
            "========== {}{} ==========",
            v.str(),
            if inc { "++" } else { "--" }
        ));
        let old = num(v);
        let op = if inc { BinaryOp::Add } else { BinaryOp::Sub };
        let new = php.binop(op, &V::N(old.clone()), &i(1)).expect("ok");
        php.line("$num:");
        php.dump_number(&new);
        php.line("$num_old:");
        php.dump_number(&old);
        php.line("");
    }
}

fn compare_block(php: &mut Php, v1: &V, l1: &str, v2: &V, l2: &str) {
    let c12 = Php::cmp(v1, v2);
    let c21 = Php::cmp(v2, v1);
    // $a > $b is evaluated as $b < $a (and >= as <=) with swapped operands.
    php.line(format!("{l1} >  {l2}: {}", tf(lt(c21))));
    php.line(format!("{l1} >= {l2}: {}", tf(le(c21))));
    php.line(format!("{l1} == {l2}: {}", tf(eq(c12))));
    php.line(format!("{l1} <= {l2}: {}", tf(le(c12))));
    php.line(format!("{l1} <  {l2}: {}", tf(lt(c12))));
    php.line("");
    php.line("inversion");
    php.line(format!("{l2} >  {l1}: {}", tf(lt(c12))));
    php.line(format!("{l2} >= {l1}: {}", tf(le(c12))));
    php.line(format!("{l2} == {l1}: {}", tf(eq(c21))));
    php.line(format!("{l2} <= {l1}: {}", tf(le(c21))));
    php.line(format!("{l2} <  {l1}: {}", tf(lt(c21))));
    php.line("");
}

pub fn operator_compare(php: &mut Php, _: &Phpt) {
    let v1 = nv("100.0000");
    for (v2, ty) in compare_values() {
        php.line(format!("========== with {ty} {} ==========", v2.str()));
        compare_block(php, &v1, &v1.str(), &v2, &v2.str());
    }
}

pub fn operator_compare_invalid(php: &mut Php, _: &Phpt) {
    let v1 = nv("100.0000");
    for (v2, ty) in [
        (V::Null, "null"),
        (s("string"), "string"),
        (V::Obj("stdClass"), "object"),
        (V::Arr, "array"),
        (V::Obj("resource"), "resource"),
    ] {
        php.line(format!("========== with {ty} =========="));
        compare_block(php, &v1, &v1.str(), &v2, ty);
    }
}
