// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Transcriptions of ext/bcmath/tests/*.phpt (the procedural functions).

use super::harness::*;
use super::runner::{string_arrays, Phpt};
use php_bcmath::{BcMath, RoundingMode};

/// Scripts built on `run_bcmath_tests_function.inc`: the operand arrays are
/// read from the script itself.
pub fn run_bcmath_tests(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let call = file.find("run_bcmath_tests(").expect("call");
    let args = &file[call + "run_bcmath_tests(".len()..];
    let args: Vec<&str> = args[..args.find(");").expect("end")]
        .split(',')
        .map(str::trim)
        .collect();
    let vars = string_arrays(file);
    let first = &vars[args[0].trim_start_matches('$')];
    let second = &vars[args[1].trim_start_matches('$')];
    let symbol = args[2].trim_matches('"');
    let func = args[3].trim_end_matches("(...)");
    for scale in [0, 10] {
        for a in first {
            php.line(format!("Number \"{a}\" (scale {scale})"));
            for b in second {
                let ret = bc_binary(php.bc, func, a, b, sc(scale)).unwrap_or_else(|e| e.message);
                php.line(format!("{a} {symbol} {} = {ret}", pad_right(b, 30)));
            }
            php.line("");
        }
    }
}

fn two_not_well_formed(php: &mut Php, func: &str) {
    let r = bc_binary(php.bc, func, "a", "1", None);
    php.catch(r);
    let r = bc_binary(php.bc, func, "1", "a", None);
    php.catch(r);
}

pub fn bcadd_error(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcadd");
}
pub fn bcsub_error(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcsub");
}
pub fn bcmul_error(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcmul");
}
pub fn bcdiv_error2(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcdiv");
}
pub fn bcmod_error3(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcmod");
}
pub fn bcpow_error3(php: &mut Php, _: &Phpt) {
    two_not_well_formed(php, "bcpow");
}
pub fn bccomp_error(php: &mut Php, _: &Phpt) {
    let r = php.bc.bccomp(b"a", b"1", None);
    php.catch(r);
    let r = php.bc.bccomp(b"1", b"a", None);
    php.catch(r);
}
pub fn bug80545(php: &mut Php, _: &Phpt) {
    let r = php.bc.bcadd(b"a", b"a", None);
    php.catch(r);
    let r = php.bc.bcadd(b"1", b"a", None);
    php.catch(r);
}

pub fn bcfloor_ceil_error(php: &mut Php, t: &Phpt) {
    let floor = t.file().contains("bcfloor");
    for s in ["hoge", "0.00.1"] {
        let r = if floor {
            php.bc.bcfloor(s.as_bytes())
        } else {
            php.bc.bcceil(s.as_bytes())
        };
        php.catch(r);
    }
}

pub fn bcround_error(php: &mut Php, _: &Phpt) {
    for s in ["hoge", "0.00.1"] {
        let r = php
            .bc
            .bcround(s.as_bytes(), 0, RoundingMode::HalfAwayFromZero);
        php.catch(r);
    }
}

pub fn bcfloor_ceil(php: &mut Php, t: &Phpt) {
    let floor = t.file().contains("bcfloor(");
    let nums = t.file();
    let vars = string_arrays(nums);
    for n in &vars["nums"] {
        let r = if floor {
            php.bc.bcfloor(n.as_bytes())
        } else {
            php.bc.bcceil(n.as_bytes())
        };
        php.line(format!("{} => {}", pad_left(n, 50), r.expect("ok")));
    }
}

pub fn bccomp(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    for (a, b, s) in [
        ("-1", "5", Some(4)),
        (
            "1928372132132819737213",
            "8728932001983192837219398127471",
            None,
        ),
        ("1.00000000000000000001", "1", Some(2)),
        ("97321", "2321", None),
        ("0", "14", None),
        ("1", "0", None),
        ("0.000", "0", Some(3)),
    ] {
        php.line(
            bc.bccomp(a.as_bytes(), b.as_bytes(), s)
                .expect("ok")
                .to_string(),
        );
    }
}

pub fn bccomp_variations(php: &mut Php, t: &Phpt) {
    // echo bccomp("a", "b"[, "s"]) . "\n";
    let bc = php.bc;
    for line in t.file().lines().filter(|l| l.starts_with("echo bccomp(")) {
        let args: Vec<String> = line["echo bccomp(".len()..line.find(')').expect("paren")]
            .split(',')
            .map(|a| a.trim().trim_matches('"').to_string())
            .collect();
        let scale = args.get(2).map(|s| s.parse().expect("scale"));
        php.line(
            bc.bccomp(args[0].as_bytes(), args[1].as_bytes(), scale)
                .expect("ok")
                .to_string(),
        );
    }
}

pub fn bcdiv_by_one(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    for scale in 0..=3 {
        php.line(format!("scale: {scale}"));
        for divisor in ["1.010", "1.01", "1.1", "1", "-1.010", "-1.01", "-1.1", "-1"] {
            for dividend in ["100", "-100"] {
                let q = bc
                    .bcdiv(dividend.as_bytes(), divisor.as_bytes(), sc(scale))
                    .expect("ok");
                let w = usize::try_from(scale).unwrap() + 2 + usize::from(scale != 0);
                php.line(format!(
                    "{} / {} = {}",
                    pad_left(dividend, 4),
                    pad_left(divisor, 6),
                    pad_left(&q, w)
                ));
            }
        }
        php.line("");
    }
}

pub fn bcdiv_by_pow_10(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    let dividends = [
        "0.012345",
        "0.12345",
        "1.2345",
        "12.345",
        "123.45",
        "-0.012345",
        "-0.12345",
        "-1.2345",
        "-12.345",
        "-123.45",
    ];
    let divisors = [
        "0.01", "0.1", "1", "10", "100", "-0.01", "-0.1", "-1", "-10", "-100",
    ];
    for scale in [0, 3, 5] {
        php.line(format!("scale: {scale}"));
        for divisor in divisors {
            for dividend in dividends {
                let q = bc
                    .bcdiv(dividend.as_bytes(), divisor.as_bytes(), sc(scale))
                    .expect("ok");
                let w = usize::try_from(scale).unwrap() + 7 + usize::from(scale != 0);
                php.line(format!(
                    "{} / {} = {}",
                    pad_left(dividend, 9),
                    pad_left(divisor, 5),
                    pad_left(&q, w)
                ));
            }
        }
        php.line("");
    }
}

pub fn bcdiv_error1(php: &mut Php, _: &Phpt) {
    for d in ["0", "0.00", "-0.00"] {
        let r = php.bc.bcdiv(b"10.99", d.as_bytes(), None);
        php.catch(r);
    }
}

pub fn bcmod_error2(php: &mut Php, _: &Phpt) {
    for d in ["0", "0.000", "-0.0"] {
        let r = php.bc.bcmod(b"10", d.as_bytes(), None);
        php.catch(r);
    }
}

pub fn bcdivmod_by_zero(php: &mut Php, t: &Phpt) {
    let vars = string_arrays(t.file());
    for a in &vars["dividends"] {
        for b in &vars["divisors"] {
            match php.bc.bcdivmod(a.as_bytes(), b.as_bytes(), None) {
                Ok(_) => php.line("NG"),
                Err(e) => php.line(if e.message == "Division by zero" {
                    "OK"
                } else {
                    "NG"
                }),
            }
        }
    }
}

#[allow(clippy::many_single_char_names)]
pub fn bcdivmod(php: &mut Php, t: &Phpt) {
    let vars = string_arrays(t.file());
    let bc = php.bc;
    for scale in [0, 10] {
        for a in &vars["dividends"] {
            for b in &vars["divisors"] {
                let (q, r) = bc
                    .bcdivmod(a.as_bytes(), b.as_bytes(), sc(scale))
                    .expect("ok");
                let d = bc.bcdiv(a.as_bytes(), b.as_bytes(), sc(0)).expect("ok");
                let m = bc.bcmod(a.as_bytes(), b.as_bytes(), sc(scale)).expect("ok");
                if bc.bccomp(q.as_bytes(), d.as_bytes(), None).expect("ok") != 0 {
                    php.line("Div result is incorrect.");
                }
                if bc.bccomp(r.as_bytes(), m.as_bytes(), None).expect("ok") != 0 {
                    php.line("Mod result is incorrect.");
                }
            }
        }
    }
    php.echo("done!");
}

pub fn bcmul_check_overflow(php: &mut Php, _: &Phpt) {
    for k in 1..15 {
        let repeat = 1usize << k;
        let n = "99999999".repeat(repeat);
        let expected = format!(
            "{}8{}1",
            "9".repeat(repeat * 8 - 1),
            "0".repeat(repeat * 8 - 1)
        );
        let actual = php.bc.bcmul(n.as_bytes(), n.as_bytes(), None).expect("ok");
        php.line(format!(
            "{repeat}: {}",
            if actual == expected { "OK" } else { "NG" }
        ));
    }
}

pub fn bcpow_div_by_zero(php: &mut Php, _: &Phpt) {
    for base in ["0", "-0"] {
        for e in ["-15", "-1", "-9"] {
            let r = php.bc.bcpow(base.as_bytes(), e.as_bytes(), None);
            php.echo_or_catch(r);
        }
    }
}

pub fn bcpow_error1(php: &mut Php, _: &Phpt) {
    for e in ["1.1", "0.1"] {
        let r = php.bc.bcpow(b"1", e.as_bytes(), sc(2));
        if let Some(v) = php.catch(r) {
            php.dump_str(&v);
        }
    }
}

pub fn bcpow_error2(php: &mut Php, _: &Phpt) {
    for e in ["9223372036854775808", "-9223372036854775808"] {
        let r = php.bc.bcpow(b"0", e.as_bytes(), sc(2));
        if let Some(v) = php.catch(r) {
            php.dump_str(&v);
        }
    }
}

pub fn bcpowmod_error(php: &mut Php, _: &Phpt) {
    for (a, b, c) in [("a", "1", "1"), ("1", "a", "1"), ("1", "1", "a")] {
        let r = php
            .bc
            .bcpowmod(a.as_bytes(), b.as_bytes(), c.as_bytes(), None);
        php.catch(r);
    }
}

fn powmod_dump(php: &mut Php, a: &str, b: &str, c: &str, scale: Option<i64>) {
    let r = php
        .bc
        .bcpowmod(a.as_bytes(), b.as_bytes(), c.as_bytes(), scale);
    if let Some(v) = php.catch(r) {
        php.dump_str(&v);
    }
}

pub fn bcpowmod_negative_exponent(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "1", "-1", "2", None);
}

pub fn bcpowmod_zero_modulus(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "1", "1", "0", None);
}

pub fn bcpowmod(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "5", "2", "7", None);
    powmod_dump(php, "-2", "5", "7", None);
    powmod_dump(php, "10", "2147483648", "2047", None);
    powmod_dump(php, "10", "0", "2047", None);
}

pub fn bcpowmod_with_mod_1(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "5", "0", "1", None);
    powmod_dump(php, "5", "0", "1", sc(3));
    powmod_dump(php, "5", "0", "-1", None);
    powmod_dump(php, "5", "0", "-1", sc(3));
}

pub fn bug54598(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "5", "0", "1", None);
    powmod_dump(php, "5", "0", "1", sc(3));
}

pub fn bug44995(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "4", "4", "3", sc(1));
    powmod_dump(php, "3234", "32345", "22345", sc(1));
}

pub fn bug72093(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "1", "0", "128", sc(-200));
    // bcpowmod(1, 1.2, 1, 1): the float becomes the string "1.2".
    powmod_dump(php, "1", "1.2", "1", sc(1));
}

pub fn bug75178(php: &mut Php, _: &Phpt) {
    powmod_dump(php, "4.1", "4", "3", sc(3));
    powmod_dump(php, "4", "4", "3.1", sc(3));
}

pub fn bug78878(php: &mut Php, _: &Phpt) {
    // bcpowmod(2, 65535, -4e-4): the float is passed as "-0.0004"; the
    // outer bcmul is never reached.
    let r = php.bc.bcpowmod(b"2", b"65535", b"-0.0004", None);
    if let Some(v) = php.catch(r) {
        php.echo(v);
    }
}

pub fn bug46781(php: &mut Php, _: &Phpt) {
    let v = php.bc.bcadd(b"-0.0", b"-0.0", sc(1)).expect("ok");
    php.dump_str(&v);
    let c = php.bc.bccomp(b"-0.0", b"0", sc(1)).expect("ok");
    php.dump_int(c);
}

pub fn bug60377(php: &mut Php, _: &Phpt) {
    let r = php.bc.bcscale(Some(634_314_234_334_311));
    php.catch(r);
    php.bc.bcsqrt(b"0", None).expect("ok");
    php.bc.bcadd(b"0", b"-1", sc(10)).expect("ok");
}

pub fn bug66364(php: &mut Php, _: &Phpt) {
    let v = php.bc.bcmul(b"0.3", b"0.2", sc(4)).expect("ok");
    php.dump_str(&v);
}

pub fn bug78238(php: &mut Php, _: &Phpt) {
    let v = php.bc.bcadd(b"0", b"-0.1", None).expect("ok");
    php.dump_str(&v);
    let a = php.bc.bcmul(b"0.9", b"-0.1", sc(1)).expect("ok");
    let b = php.bc.bcmul(b"0.90", b"-0.1", sc(1)).expect("ok");
    php.dump_str(&a);
    php.dump_str(&b);
}

pub fn gh16265(php: &mut Php, _: &Phpt) {
    // -12.3456789000e10 is the float -123456789000.0, converted to string.
    let v = php.bc.bcdiv(b"-0.01", b"-123456789000", sc(9)).expect("ok");
    php.echo(v);
}

pub fn gh16978(php: &mut Php, _: &Phpt) {
    let v = php.bc.bcpow(b"10", b"-112", sc(10)).expect("ok");
    php.line(v);
    let d = format!("1{}", "0".repeat(100));
    let v = php.bc.bcdiv(b"1", d.as_bytes(), sc(1)).expect("ok");
    php.echo(v);
}

pub fn gh17275(php: &mut Php, _: &Phpt) {
    let a = php.bc.bcdiv(b"0.03772321", b"9650.0", sc(8)).expect("ok");
    let b = php.bc.bcdiv(b"0.03772321", b"9650.0", sc(9)).expect("ok");
    php.dump_str(&a);
    php.dump_str(&b);
}

pub fn ghsa_x692(php: &mut Php, _: &Phpt) {
    let n = format!("1.9{}1", "0".repeat(300));
    let c = php.bc.bccomp(n.as_bytes(), b"0", sc(300)).expect("ok");
    php.dump_int(c);
}

pub fn bcround_all(php: &mut Php, _: &Phpt) {
    for mode in modes() {
        for n in [
            "1.0", "-1.0", "1.2", "-1.2", "1.7", "-1.7", "1.5", "-1.5", "2.5", "-2.5",
        ] {
            let r = php.bc.bcround(n.as_bytes(), 0, mode).expect("ok");
            php.line(format!("{:<20}: {n} -> {r}", mode.name()));
        }
    }
}

/// `bcround_test_helper.inc`.
pub fn run_round_test(php: &mut Php, t: &Phpt) {
    let file = t.file();
    let mode_name = &file[file.find("RoundingMode::").expect("mode") + "RoundingMode::".len()..];
    let mode_name = &mode_name[..mode_name.find(')').expect("paren")];
    let mode = RoundingMode::from_name(mode_name).expect("mode");
    let groups: [(&str, &[(&str, i64)]); 4] = [
        (
            "non-boundary value",
            &[
                ("1.1", 0),
                ("1.2", 0),
                ("1.3", 0),
                ("1.4", 0),
                ("1.6", 0),
                ("1.7", 0),
                ("1.8", 0),
                ("1.9", 0),
                ("-1.1", 0),
                ("-1.2", 0),
                ("-1.3", 0),
                ("-1.4", 0),
                ("-1.6", 0),
                ("-1.7", 0),
                ("-1.8", 0),
                ("-1.9", 0),
            ],
        ),
        (
            "minus precision",
            &[
                ("0", -3),
                ("0.01", -3),
                ("-0.01", -3),
                ("50", -2),
                ("-50", -2),
                ("1230", -1),
                ("1235", -1),
                ("-1230", -1),
                ("-1235", -1),
                ("3400.0000", -2),
                ("3400.0001", -2),
                ("3450.0000", -2),
                ("3450.0001", -2),
                ("-3400.0000", -2),
                ("-3400.0001", -2),
                ("-3450.0000", -2),
                ("-3450.0001", -2),
            ],
        ),
        (
            "zero precision",
            &[
                ("1235", 0),
                ("1235.0", 0),
                ("1235.000001", 0),
                ("1235.5", 0),
                ("1235.500001", 0),
                ("-1235", 0),
                ("-1235.0", 0),
                ("-1235.000001", 0),
                ("-1235.5", 0),
                ("-1235.500001", 0),
                ("0.0001", 0),
                ("0.5", 0),
                ("0.5000", 0),
                ("0.5001", 0),
                ("-0.0001", 0),
                ("-0.5", 0),
                ("-0.5000", 0),
                ("-0.5001", 0),
            ],
        ),
        (
            "plus precision",
            &[
                ("28.40", 1),
                ("28.4000001", 1),
                ("28.45", 1),
                ("28.4500001", 1),
                ("-28.40", 1),
                ("-28.4000001", 1),
                ("-28.45", 1),
                ("-28.4500001", 1),
                ("153.90", 1),
                ("153.9000001", 1),
                ("153.95", 1),
                ("153.9500001", 1),
                ("-153.90", 1),
                ("-153.9000001", 1),
                ("-153.95", 1),
                ("-153.9500001", 1),
                ("0.000001", 3),
                ("0.0005", 3),
                ("0.000500", 3),
                ("0.000501", 3),
                ("-0.000001", 3),
                ("-0.0005", 3),
                ("-0.000500", 3),
                ("-0.000501", 3),
            ],
        ),
    ];
    for (title, cases) in groups {
        php.line(format!("========== {title} =========="));
        for &(n, p) in cases {
            let r = php.bc.bcround(n.as_bytes(), p, mode).expect("ok");
            php.line(format!("{} => {r}", pad_left(&format!("[{n}, {p}]"), 17)));
        }
        php.line("");
    }
}

pub fn bcround_early_return(php: &mut Php, _: &Phpt) {
    let cases: [(&str, i64); 16] = [
        ("123", 1),
        ("123.5", 1),
        ("123.5", 2),
        ("123.0000000000000000000001", 22),
        ("123.0000000000000000000001", 23),
        ("-123", 1),
        ("-123.5", 1),
        ("-123.5", 2),
        ("-123.0000000000000000000001", 22),
        ("-123.0000000000000000000001", 23),
        ("0", 0),
        ("0.0", 0),
        ("0.0000", 0),
        ("-0", 0),
        ("-0.0", 0),
        ("-0.0000", 0),
    ];
    let bc = php.bc;
    let run = |mode| -> Vec<String> {
        cases
            .iter()
            .map(|&(n, p)| {
                format!(
                    "{} => {}\n",
                    pad_left(&format!("[{n}, {p}]"), 33),
                    bc.bcround(n.as_bytes(), p, mode).expect("ok")
                )
            })
            .collect()
    };
    let base = run(RoundingMode::HalfAwayFromZero);
    for r in &base {
        php.echo(r);
    }
    php.line("");
    for mode in modes() {
        let same = run(mode) == base;
        php.line(format!(
            "{}: result is {}",
            pad_left(mode.name(), 24),
            if same {
                "same to HalfAwayFromZero"
            } else {
                "not same to HalfAwayFromZero, failed"
            }
        ));
    }
}

pub fn bcround_precision_bounds(php: &mut Php, _: &Phpt) {
    let r = php
        .bc
        .bcround(b"1", i64::MAX, RoundingMode::HalfAwayFromZero);
    php.catch(r);
    let one = num("1");
    let r = one.round(i64::MAX, RoundingMode::HalfAwayFromZero);
    php.catch(r);
    let r = one.round(2_147_483_648, RoundingMode::HalfAwayFromZero);
    php.catch(r);
}

pub fn bcscale(php: &mut Php, _: &Phpt) {
    php.line(php.bc.bcadd(b"1", b"2", None).expect("ok"));
    for s in [2, 10, 0] {
        php.bc.bcscale(Some(s)).expect("ok");
        php.line(php.bc.bcadd(b"1", b"2", None).expect("ok"));
    }
}

pub fn bcscale_variation001(php: &mut Php, _: &Phpt) {
    php.line(php.bc.bcdiv(b"20.56", b"4", None).expect("ok"));
    let r = php.bc.bcscale(Some(-4));
    php.catch(r);
}

pub fn bcscale_variation002(php: &mut Php, _: &Phpt) {
    // INI bcmath.scale=-2 is rejected by OnUpdateScale (handled by the runner).
    php.echo(php.bc.bcadd(b"-4.27", b"7.3", None).expect("ok"));
}

pub fn bcscale_variation003(php: &mut Php, _: &Phpt) {
    // ini_get('bcmath.scale') mirrors bcscale(); the VM keeps them in sync.
    let ini = php.bc.default_scale();
    php.dump_int(ini);
    for s in [Some(1), Some(4), None] {
        let old = php.bc.bcscale(s).expect("ok");
        php.dump_int(old);
        let ini = php.bc.default_scale();
        php.dump_int(ini);
    }
    let old = php.bc.bcscale(None).expect("ok");
    php.dump_int(old);
}

pub fn bcsqrt(php: &mut Php, t: &Phpt) {
    let vars = string_arrays(t.file());
    for scale in [0, 10] {
        for r in &vars["radicants"] {
            php.line(php.bc.bcsqrt(r.as_bytes(), sc(scale)).expect("ok"));
        }
    }
}

pub fn bcsqrt_error1(php: &mut Php, _: &Phpt) {
    let r = php.bc.bcsqrt(b"-9", None);
    php.catch(r);
}

pub fn bcsqrt_error2(php: &mut Php, _: &Phpt) {
    let r = php.bc.bcsqrt(b"a", None);
    php.catch(r);
}

/// `scale.phpt` (explicit 5) and `scale_ini.phpt` (`bcmath.scale = 5`).
pub fn scale(php: &mut Php, t: &Phpt) {
    let s = if t.file().contains("'1', 5)") {
        sc(5)
    } else {
        None
    };
    let bc = php.bc;
    let lines = [
        ("bcadd:    ", bc.bcadd(b"2", b"1", s)),
        ("bcdiv:    ", bc.bcdiv(b"2", b"1", s)),
        ("bcmul:    ", bc.bcmul(b"2", b"1", s)),
        ("bcpow:    ", bc.bcpow(b"2", b"1", s)),
        ("bcpowmod: ", bc.bcpowmod(b"2", b"1", b"3", s)),
        ("bcsqrt:   ", bc.bcsqrt(b"4", s)),
        ("bcsub:    ", bc.bcsub(b"2", b"1", s)),
    ];
    for (label, v) in lines {
        php.line(format!("{label}{}", v.expect("ok")));
    }
}

pub fn negative_scale(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    let results = [
        bc.bcadd(b"1", b"2", sc(-1)),
        bc.bcsub(b"1", b"2", sc(-1)),
        bc.bcmul(b"1", b"2", sc(-1)),
        bc.bcdiv(b"1", b"2", sc(-1)),
        bc.bcmod(b"1", b"2", sc(-1)),
        bc.bcpowmod(b"1", b"2", b"3", sc(-9)),
        bc.bcpow(b"1", b"2", sc(-1)),
        bc.bcsqrt(b"9", sc(-1)),
    ];
    for r in results {
        php.catch(r);
    }
    let r = bc.bccomp(b"1", b"2", sc(-1));
    php.catch(r);
    let r = php.bc.bcscale(Some(-1));
    php.catch(r);
}

pub fn sec_embedded_null_truncation(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    for s in [&b"100\x005"[..], b"1.2\x003", b"\x00123"] {
        let results: [Result<String, php_bcmath::BcError>; 8] = [
            bc.bcadd(s, b"0", None),
            bc.bcsub(s, b"0", None),
            bc.bcmul(s, b"1", None),
            bc.bcdiv(s, b"1", None),
            bc.bcmod(s, b"7", None),
            bc.bcpow(s, b"1", None),
            bc.bccomp(s, b"0", None).map(|c| c.to_string()),
            bc.bcsqrt(s, None),
        ];
        for r in results {
            if php.catch(r).is_some() {
                php.line("FAIL: accepted");
            }
        }
    }
}

pub fn str2num_formatting(php: &mut Php, _: &Phpt) {
    let bc = php.bc;
    let ok_cases: [(&str, Option<i64>); 5] = [
        ("1", None),
        ("1.1", sc(2)),
        ("", sc(2)),
        ("+0", None),
        ("-0", None),
    ];
    let bad = [" 0", "1e1", "1,1", "Hello", "1 1", "1.a"];
    for (a, s) in ok_cases {
        php.line(bc.bcadd(a.as_bytes(), b"2", s).expect("ok"));
    }
    php.line("");
    for a in bad {
        let r = bc.bcadd(a.as_bytes(), b"2", None);
        if let Some(v) = php.catch(r) {
            php.echo(v);
        }
    }
    php.line("");
    for (a, s) in ok_cases {
        php.line(bc.bccomp(a.as_bytes(), b"2", s).expect("ok").to_string());
    }
    php.line("");
    for a in &bad[..5] {
        let r = bc.bccomp(a.as_bytes(), b"2", None);
        if let Some(v) = php.catch(r) {
            php.echo(v.to_string());
        }
    }
}

/// gh16262: `bcdiv()` vs `Number::div()` at several scales.
pub fn gh16262(php: &mut Php, _: &Phpt) {
    let one = num("1");
    for scale in [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)] {
        php.line(format!(
            "========== scale: {} ==========",
            scale.map_or_else(|| "null".to_string(), |s: i64| s.to_string())
        ));
        for d in ["1000", "2000"] {
            php.line(format!("1 / {d}:"));
            let f = php.bc.bcdiv(b"1", d.as_bytes(), scale).expect("ok");
            php.dump_str(&f);
            let m = one.div(d.into(), scale).expect("ok");
            php.dump_number(&m);
        }
        php.line("");
    }
}

pub fn gh20006(php: &mut Php, _: &Phpt) {
    // pow($n, 2) on a Number goes through the ** operator handler.
    let n = num("0");
    let r = Number::do_operation(php_bcmath::BinaryOp::Pow, (&n).into(), 2.into()).expect("ok");
    php.dump_number(&r);
}

pub fn bc_is_zero_for_scale_clamp(php: &mut Php, _: &Phpt) {
    let short_zero = num("1.0").sub("1.0".into(), None).expect("ok");
    let long_negative = num(format!("-0.{}1", "0".repeat(64)).as_str());
    let c = short_zero
        .compare((&long_negative).into(), Some(64))
        .expect("ok");
    php.dump_int(c);
}

use php_bcmath::Number;

/// Default state for a script (bcmath.scale from the INI section).
pub fn state_for(t: &Phpt) -> BcMath {
    let mut bc = BcMath::new();
    if let Some(s) = t.ini_scale() {
        bc.set_ini_scale(s);
    }
    bc
}
