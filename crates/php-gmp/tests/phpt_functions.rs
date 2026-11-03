//! Expectations transcribed from `ext/gmp/tests/*.phpt` (php-src master).

mod harness;

use harness::{call, g, Bool, G, I, S, T};
use php_gmp::Mpz;

fn check(func: &str, args: &[harness::V], expected: &str) {
    assert_eq!(call(func, args), expected, "{func}({args:?})");
}

const NOT_INT: &str = "is not an integer string";

#[test]
fn gmp_strval() {
    check(
        "gmp_strval",
        &[S("")],
        &format!("ValueError: gmp_strval(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_strval",
        &[S(""), I(-1)],
        &format!("ValueError: gmp_strval(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_strval", &[T("resource")], "TypeError: gmp_strval(): Argument #1 ($num) must be of type GMP|string|int, resource given");
    let base_err =
        "ValueError: gmp_strval(): Argument #2 ($base) must be between 2 and 62, or -2 and -36";
    check("gmp_strval", &[g("9765456")], "\"9765456\"");
    check("gmp_strval", &[g("9765456"), I(-1)], base_err);
    check("gmp_strval", &[g("9765456"), I(100_000)], base_err);
    check("gmp_strval", &[g("-3373333"), I(10)], "\"-3373333\"");
    check("gmp_strval", &[g("255"), I(-16)], "\"FF\"");
    check("gmp_strval", &[g("255"), I(-37)], base_err);
    check("gmp_strval", &[g("3843"), I(62)], "\"zz\"");
}

#[test]
fn gmp_init() {
    check("gmp_init", &[S("98765678")], "98765678");
    check(
        "gmp_init",
        &[I(1), I(-1)],
        "ValueError: gmp_init(): Argument #2 ($base) must be 0 or between 2 and 62",
    );
    check(
        "gmp_init",
        &[S(""), I(36)],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_init",
        &[S("foo"), I(3)],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_init",
        &[S("993247326237679187178"), I(3)],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "GMP::__construct",
        &[S("x")],
        &format!("ValueError: GMP::__construct(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "GMP::__construct",
        &[I(1), I(63)],
        "ValueError: GMP::__construct(): Argument #2 ($base) must be 0 or between 2 and 62",
    );
    // bug80560: explicit bases with a bare prefix.
    for (s, b) in [("0X", 16), ("0x", 16), ("0B", 2), ("0b", 2)] {
        check(
            "gmp_init",
            &[S(s), I(b)],
            &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
        );
    }
    for f in [
        "gmp_abs",
        "gmp_neg",
        "gmp_sqrt",
        "gmp_com",
        "gmp_sign",
        "gmp_popcount",
    ] {
        for s in ["0B", "0b", "0X", "0x"] {
            assert!(call(f, &[S(s)]).starts_with("ValueError"), "{f}({s})");
        }
    }
}

#[test]
fn gmp_init_integer_notations() {
    let cases = [
        ("0x1A", "26"),
        ("0X1A", "26"),
        ("0b101", "5"),
        ("0B101", "5"),
        ("0o17", "15"),
        ("0O17", "15"),
        ("017", "15"),
        ("  0x1A", "26"),
        ("-0x1A", "-26"),
        ("-017", "-15"),
        ("1 2 3", "123"),
        ("\t\n42", "42"),
    ];
    for (s, v) in cases {
        check("gmp_init", &[S(s)], v);
    }
    check("gmp_init", &[S("0x1A"), I(16)], "26");
    check("gmp_init", &[S("0o17"), I(8)], "15");
    check("gmp_init", &[S("0b11"), I(2)], "3");
    check(
        "gmp_init",
        &[S("0x1A"), I(10)],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_init",
        &[S("-0o17")],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_init",
        &[S("1\x002")],
        &format!("ValueError: gmp_init(): Argument #1 ($num) {NOT_INT}"),
    );
}

#[test]
fn gmp_intval() {
    check("gmp_intval", &[S("-1")], "int(-1)");
    check("gmp_intval", &[I(-2_349_828)], "int(-2349828)");
    check("gmp_intval", &[g("12345678")], "int(12345678)");
    check(
        "gmp_intval",
        &[S("")],
        &format!("ValueError: gmp_intval(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_intval", &[T("stdClass")], "TypeError: gmp_intval(): Argument #1 ($num) must be of type GMP|string|int, stdClass given");
    check(
        "gmp_intval",
        &[S("1.0001")],
        &format!("ValueError: gmp_intval(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_intval", &[S("9223372036854775808")], "int(0)");
    check("gmp_intval", &[S("-9223372036854775809")], "int(-1)");
}

#[test]
fn php_int_max() {
    check(
        "gmp_mul",
        &[I(i64::MAX), I(i64::MAX)],
        "85070591730234615847396907784232501249",
    );
    check(
        "gmp_add",
        &[I(i64::MAX), I(i64::MAX)],
        "18446744073709551614",
    );
    check(
        "gmp_mul",
        &[I(i64::MAX), I(i64::MIN)],
        "-85070591730234615856620279821087277056",
    );
}

#[test]
fn gmp_abs() {
    check(
        "gmp_abs",
        &[S("")],
        &format!("ValueError: gmp_abs(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_abs", &[S("0")], "0");
    check(
        "gmp_abs",
        &[S("-111111111111111111111")],
        "111111111111111111111",
    );
    check("gmp_abs", &[S("0000")], "0");
    check(
        "gmp_abs",
        &[S("09876543")],
        &format!("ValueError: gmp_abs(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_abs",
        &[S("-099987654")],
        &format!("ValueError: gmp_abs(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_abs",
        &[T("float")],
        "TypeError: gmp_abs(): Argument #1 ($num) must be of type GMP|string|int, float given",
    );
}

#[test]
fn bitwise_functions() {
    check("gmp_and", &[S("111111"), S("2222222")], "106502");
    check("gmp_and", &[I(123_123), I(435_234)], "40994");
    check("gmp_and", &[I(555), S("2342341123")], "515");
    check("gmp_and", &[I(-1), I(3333)], "3333");
    check("gmp_and", &[I(4545), I(-20)], "4544");
    check(
        "gmp_and",
        &[S("test"), S("no test")],
        &format!("ValueError: gmp_and(): Argument #1 ($num1) {NOT_INT}"),
    );
    check("gmp_and", &[g("987657876543456"), S("34332")], "1536");
    check(
        "gmp_and",
        &[g("987657876543456"), g("987657878765436543456")],
        "424703623692768",
    );
    check(
        "gmp_and",
        &[I(1), T("array")],
        "TypeError: gmp_and(): Argument #2 ($num2) must be of type GMP|string|int, array given",
    );
    check("gmp_or", &[S("111111"), S("2222222")], "2226831");
    check("gmp_or", &[I(123_123), I(435_234)], "517363");
    check("gmp_or", &[I(555), S("2342341123")], "2342341163");
    check("gmp_or", &[I(-1), I(3333)], "-1");
    check("gmp_or", &[I(4545), I(-20)], "-19");
    check(
        "gmp_or",
        &[g("987657876543456"), S("34332")],
        "987657876576252",
    );
    check(
        "gmp_or",
        &[g("987657876543456"), g("987657878765436543456")],
        "987658441719689394144",
    );
    check("gmp_com", &[I(0)], "-1");
    check("gmp_com", &[S("2394876545678")], "-2394876545679");
    check("gmp_com", &[S("-111")], "110");
    check("gmp_com", &[I(-9876)], "9875");
    check("gmp_com", &[g("98765463337")], "-98765463338");
    check(
        "gmp_com",
        &[S("test")],
        &format!("ValueError: gmp_com(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_hamdist", &[I(1_231_231), I(0)], "int(13)");
    check("gmp_hamdist", &[I(1_231_231), I(-1)], "int(-1)");
    check(
        "gmp_hamdist",
        &[I(1_231_231), S("8765434567897654333334567")],
        "int(36)",
    );
    check(
        "gmp_hamdist",
        &[I(-364_264_234), S("8333765434567897654333334567")],
        "int(-1)",
    );
    check(
        "gmp_hamdist",
        &[g("8765434567"), S("8333765434567897654333334567")],
        "int(43)",
    );
    check("gmp_hamdist", &[g("8765434567"), g("8765434567")], "int(0)");
    check(
        "gmp_hamdist",
        &[g("8765434567"), g("987654445678")],
        "int(26)",
    );
}

#[test]
fn gmp_binomial() {
    check("gmp_binomial", &[I(10), I(5)], "252");
    check("gmp_binomial", &[S("10"), I(5)], "252");
    check("gmp_binomial", &[I(10_000), I(100)], "65208469245472575695415972927215718683781335425416743372210247172869206520770178988927510291340552990847853030615947098118282371982392705479271195296127415562705948429404753632271959046657595132854990606768967505457396473467998111950929802400");
    check("gmp_binomial", &[I(0), I(0)], "1");
    check("gmp_binomial", &[I(0), I(1)], "0");
    check("gmp_binomial", &[I(1), I(0)], "1");
    check("gmp_binomial", &[I(1), I(1)], "1");
    check("gmp_binomial", &[I(-1), I(5)], "-1");
    check("gmp_binomial", &[I(-2), I(6)], "7");
    check(
        "gmp_binomial",
        &[I(5), I(-2)],
        "ValueError: gmp_binomial(): Argument #2 ($k) must be between 0 and 18446744073709551615",
    );
}

#[test]
fn gmp_clrbit_setbit() {
    let idx_err = |f: &str| {
        format!("ValueError: {f}(): Argument #2 ($index) must be between 0 and 2147483647 * 64")
    };
    let mut n = Mpz::from(0);
    assert_eq!(harness::setbit("gmp_clrbit", &mut n, 0, false), "0");
    let mut n = Mpz::from(-1);
    assert_eq!(
        harness::setbit("gmp_clrbit", &mut n, -1, false),
        idx_err("gmp_clrbit")
    );
    assert_eq!(n.to_string(), "-1");
    let mut n = Mpz::from(1_000_000);
    assert_eq!(harness::setbit("gmp_clrbit", &mut n, 3, false), "1000000");
    let mut n = Mpz::parse(b"238462734628347239571823641234", 10).unwrap();
    for i in [3, 5, 20] {
        harness::setbit("gmp_clrbit", &mut n, i, false);
    }
    assert_eq!(n.to_string(), "238462734628347239571822592658");
    // gmp_setbit.phpt
    let mut n = Mpz::from(-1);
    assert_eq!(harness::setbit("gmp_setbit", &mut n, 10, true), "-1");
    let mut n = Mpz::from(5);
    assert_eq!(
        harness::setbit("gmp_setbit", &mut n, -20, false),
        idx_err("gmp_setbit")
    );
    assert_eq!(n.to_string(), "5");
    let mut n = Mpz::from(5);
    assert_eq!(harness::setbit("gmp_setbit", &mut n, 2, false), "1");
    let mut n = Mpz::from(5);
    assert_eq!(harness::setbit("gmp_setbit", &mut n, 1, true), "7");
    let mut n = Mpz::parse(b"100000000000", 10).unwrap();
    assert_eq!(
        harness::setbit("gmp_setbit", &mut n, 23, true),
        "100008388608"
    );
    assert_eq!(
        harness::setbit("gmp_setbit", &mut n, 23, false),
        "100000000000"
    );
    assert_eq!(
        harness::setbit("gmp_setbit", &mut n, 3, true),
        "100000000008"
    );
    // gmp_setbit_long: index validity bound.
    let mut n = Mpz::from(227_200);
    assert_eq!(
        harness::setbit("gmp_setbit", &mut n, 0x3F_FFFF_FFFF, true),
        idx_err("gmp_setbit")
    );
    // A large (but cheap) valid index; render only the bit length.
    assert!(php_gmp::php::gmp_setbit(&mut n, 0xFF_FFFF, true).is_ok());
    assert_eq!(n.bit_len(), 0x100_0000);
}

#[test]
fn gmp_cmp_and_sign() {
    check("gmp_cmp", &[I(123_123), I(-123_123)], "int(1)");
    check(
        "gmp_cmp",
        &[S("12345678900987654321"), S("12345678900987654321")],
        "int(0)",
    );
    check(
        "gmp_cmp",
        &[S("12345678900987654321"), S("123456789009876543211")],
        "int(-1)",
    );
    check("gmp_cmp", &[I(0), I(345_355)], "int(-1)");
    check(
        "gmp_cmp",
        &[T("array"), T("array")],
        "TypeError: gmp_cmp(): Argument #1 ($num1) must be of type GMP|string|int, array given",
    );
    check("gmp_sign", &[I(-1)], "int(-1)");
    check("gmp_sign", &[I(1)], "int(1)");
    check("gmp_sign", &[S("-34535345345")], "int(-1)");
    check(
        "gmp_sign",
        &[S("+34534573457345")],
        &format!("ValueError: gmp_sign(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_sign", &[g("0")], "int(0)");
}

#[test]
fn division_functions() {
    let round_err = |f: &str| {
        format!("ValueError: {f}(): Argument #3 ($rounding_mode) must be one of GMP_ROUND_ZERO, GMP_ROUND_PLUSINF, or GMP_ROUND_MINUSINF")
    };
    check("gmp_div_q", &[I(0), I(1)], "0");
    check(
        "gmp_div_q",
        &[I(1), I(0)],
        "DivisionByZeroError: gmp_div_q(): Argument #2 ($num2) Division by zero",
    );
    check("gmp_div_q", &[I(12653), I(23_482_734)], "0");
    check(
        "gmp_div_q",
        &[I(12653), I(23_482_734), I(10)],
        &round_err("gmp_div_q"),
    );
    check("gmp_div_q", &[I(1_123_123), I(123)], "9131");
    check("gmp_div_q", &[I(1_123_123), I(123), I(1)], "9132");
    check("gmp_div_q", &[I(1_123_123), I(123), I(2)], "9131");
    check("gmp_div", &[I(1_123_123), I(123), I(1)], "9132");
    check("gmp_div_q", &[T("resource"), T("resource")], "TypeError: gmp_div_q(): Argument #1 ($num1) must be of type GMP|string|int, resource given");
    check("gmp_div_qr", &[I(0), I(1)], "[0, 0]");
    check(
        "gmp_div_qr",
        &[g("1"), g("0")],
        "DivisionByZeroError: gmp_div_qr(): Argument #2 ($num2) Division by zero",
    );
    check(
        "gmp_div_qr",
        &[I(1), I(0), I(10)],
        "DivisionByZeroError: gmp_div_qr(): Argument #2 ($num2) Division by zero",
    );
    check("gmp_div_qr", &[I(12653), I(23_482_734)], "[0, 12653]");
    check(
        "gmp_div_qr",
        &[I(12653), I(23_482_734), I(10)],
        &round_err("gmp_div_qr"),
    );
    check("gmp_div_qr", &[I(1_123_123), I(123)], "[9131, 10]");
    check("gmp_div_qr", &[I(1_123_123), I(123), I(1)], "[9132, -113]");
    check("gmp_div_qr", &[I(1_123_123), I(123), I(2)], "[9131, 10]");
    check(
        "gmp_div_r",
        &[I(1), I(0)],
        "DivisionByZeroError: gmp_div_r(): Argument #2 ($num2) Division by zero",
    );
    check("gmp_div_r", &[I(12653), I(23_482_734)], "12653");
    check(
        "gmp_div_r",
        &[I(12653), I(23_482_734), I(10)],
        &round_err("gmp_div_r"),
    );
    check("gmp_div_r", &[I(1_123_123), I(123), I(1)], "-113");
    check("gmp_div_r", &[I(1_123_123), I(123), I(2)], "10");
    check(
        "gmp_divexact",
        &[S("233"), S("23345555555555555555555555")],
        "0",
    );
    check(
        "gmp_divexact",
        &[S("233"), S("0")],
        "DivisionByZeroError: gmp_divexact(): Argument #2 ($num2) Division by zero",
    );
    check("gmp_divexact", &[S("100"), S("10")], "10");
    check("gmp_divexact", &[S("1024"), S("2")], "512");
    check(
        "gmp_divexact",
        &[g("10000000000000000000"), S("2")],
        "5000000000000000000",
    );
    check(
        "gmp_divexact",
        &[g("10000000000000000000"), S("50")],
        "200000000000000000",
    );
    check(
        "gmp_divexact",
        &[g("-100000000000000000000000000"), g("10000000000000000000")],
        "-10000000",
    );
    check(
        "gmp_mod",
        &[S(""), S("")],
        &format!("ValueError: gmp_mod(): Argument #1 ($num1) {NOT_INT}"),
    );
    check("gmp_mod", &[I(0), I(1)], "0");
    check("gmp_mod", &[I(0), I(-1)], "0");
    check(
        "gmp_mod",
        &[I(-1), I(0)],
        "DivisionByZeroError: gmp_mod(): Argument #2 ($num2) Modulo by zero",
    );
    check("gmp_mod", &[g("-100000000"), g("353467")], "31161");
}

#[test]
fn gmp_fact() {
    let range =
        "ValueError: gmp_fact(): Argument #1 ($num) must be between 0 and 18446744073709551615";
    check("gmp_fact", &[I(0)], "1");
    check(
        "gmp_fact",
        &[S("")],
        &format!("ValueError: gmp_fact(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_fact", &[S("-1")], range);
    check("gmp_fact", &[I(-1)], range);
    check("gmp_fact", &[I(20)], "2432902008176640000");
    check(
        "gmp_fact",
        &[S("50")],
        "30414093201713378043612608166064768844377641568960512000000000000",
    );
    check("gmp_fact", &[S("0000")], "1");
    check("gmp_fact", &[g("12")], "479001600");
    check("gmp_fact", &[g("-10")], range);
    check("gmp_fact", &[g("18446744073709551616")], range);
    check("gmp_fact", &[G(Mpz::pow2(100))], range);
}

#[test]
fn gcd_lcm_family() {
    check("gmp_gcd", &[I(234), I(12387)], "3");
    check("gmp_gcd", &[I(0), I(12387)], "12387");
    check("gmp_gcd", &[I(224), I(0)], "224");
    check("gmp_gcd", &[I(-1), I(0)], "1");
    check("gmp_gcd", &[S("12371238123"), S("32618723123")], "1");
    check("gmp_gcd", &[g("8127346234"), S("12372341234")], "2");
    check("gmp_gcd", &[g("8127346234"), I(0)], "8127346234");
    for (a, b, g_) in [
        (I(123), I(45), "3"),
        (I(4341), I(9734), "1"),
        (I(23487), I(333), "3"),
        (I(-234_234), I(-123_123), "3003"),
        (I(-100), I(-2234), "2"),
        (I(345), S("34587345"), "15"),
        (I(345), S("0"), "345"),
        (S("345556456"), I(345_873), "1"),
        (S("34545345556456"), S("323432445873"), "1"),
    ] {
        let out = call("gmp_gcdext", &[a.clone(), b.clone()]);
        assert!(out.starts_with(&format!("g={g_} ")), "{out}");
        let parts: Vec<&str> = out.split(' ').collect();
        let s = Mpz::parse(parts[1][2..].as_bytes(), 10).unwrap();
        let t = Mpz::parse(parts[2][2..].as_bytes(), 10).unwrap();
        let to = |v: &harness::V| match v {
            I(i) => Mpz::from(*i),
            S(s) => Mpz::parse(s.as_bytes(), 10).unwrap(),
            _ => unreachable!(),
        };
        assert_eq!((&(&to(&a) * &s) + &(&to(&b) * &t)).to_string(), g_);
    }
    check("gmp_lcm", &[I(100), I(77)], "7700");
    check("gmp_lcm", &[I(99), I(-77)], "693");
    check("gmp_lcm", &[I(-99), I(-77)], "693");
    check("gmp_lcm", &[I(93), I(0)], "0");
    check("gmp_lcm", &[I(0), I(93)], "0");
    check("gmp_invert", &[I(123_123), I(5_467_624)], "2293131");
    check("gmp_invert", &[S("12312323213123123"), I(7624)], "5827");
    check(
        "gmp_invert",
        &[I(444), I(0)],
        "DivisionByZeroError: Division by zero",
    );
    check(
        "gmp_invert",
        &[I(123_123), S("3333334345467624")],
        "bool(false)",
    );
    check("gmp_invert", &[I(0), I(28347)], "bool(false)");
    check("gmp_invert", &[I(-12), I(456_456)], "bool(false)");
    check("gmp_invert", &[I(234_234), I(-435_345)], "bool(false)");
    check(
        "gmp_invert",
        &[g("349827349623423452345"), g("3498273496234234523451")],
        "3498273496234234523441",
    );
    check(
        "gmp_invert",
        &[g("3498273496234234523451"), g("349827349623423452345")],
        "1",
    );
}

#[test]
fn symbols() {
    let expected = ["0", "-1", "0", "0", "-1", "0", "0", "-1", "0", "0", "-1"];
    for f in ["gmp_jacobi", "gmp_legendre"] {
        for (i, e) in (-1..10).zip(expected) {
            check(f, &[I(i * i - 1), I(3)], &format!("int({e})"));
        }
        check(f, &[I(7), I(23)], "int(-1)");
        check(f, &[S("733535124"), S("1234123423434535623")], "int(0)");
        check(f, &[I(3), S("1234123423434535623")], "int(0)");
        check(f, &[S("123123"), S("1231231")], "int(-1)");
        check(f, &[S("123123"), I(3)], "int(0)");
        check(f, &[I(3), S("1231231")], "int(-1)");
        check(
            f,
            &[I(3), T("array")],
            &format!(
                "TypeError: {f}(): Argument #2 ($num2) must be of type GMP|string|int, array given"
            ),
        );
    }
    check("gmp_kronecker", &[I(23), I(12)], "int(-1)");
    check("gmp_kronecker", &[g("23"), S("12")], "int(-1)");
    check("gmp_kronecker", &[I(23), I(-12)], "int(-1)");
    check("gmp_kronecker", &[I(-23), I(12)], "int(1)");
    check("gmp_kronecker", &[I(-23), I(-12)], "int(-1)");
}

#[test]
fn gmp_neg_nextprime() {
    check("gmp_neg", &[I(0)], "0");
    check("gmp_neg", &[S("-1")], "1");
    check(
        "gmp_neg",
        &[S("")],
        &format!("ValueError: gmp_neg(): Argument #1 ($num) {NOT_INT}"),
    );
    check(
        "gmp_neg",
        &[g("12345678901234567890")],
        "-12345678901234567890",
    );
    check("gmp_nextprime", &[I(-1)], "2");
    check("gmp_nextprime", &[I(0)], "2");
    check("gmp_nextprime", &[I(-1000)], "2");
    check("gmp_nextprime", &[I(1000)], "1009");
    check("gmp_nextprime", &[I(100_000)], "100003");
    check(
        "gmp_nextprime",
        &[S("")],
        &format!("ValueError: gmp_nextprime(): Argument #1 ($num) {NOT_INT}"),
    );
    check("gmp_nextprime", &[T("stdClass")], "TypeError: gmp_nextprime(): Argument #1 ($num) must be of type GMP|string|int, stdClass given");
    let _ = Bool(true);
}
