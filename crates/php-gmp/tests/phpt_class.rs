//! `GMP` class behaviour (operators, casts, serialization) and seeded random
//! sequences, transcribed from `ext/gmp/tests/*.phpt`.

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use php_gmp::php::{self, BinaryOp, CountOperand, GmpRandom, Num};
use php_gmp::Mpz;

fn z(v: i64) -> Mpz {
    Mpz::from(v)
}

fn op(o: BinaryOp, a: Num<'_>, b: Num<'_>) -> String {
    let r = (|| {
        if o.takes_count() {
            let count = match b {
                Num::Int(i) => php::shift_amount(o, CountOperand::Int(i))?,
                Num::Gmp(m) => php::shift_amount(o, CountOperand::Gmp(m))?,
                Num::Str(_) => unreachable!(),
            };
            let a = php::operand(a)?;
            return Ok(php::count_op(o, &a, count));
        }
        let a = php::operand(a)?;
        let b = php::operand(b)?;
        php::binary_op(o, &a, &b)
    })();
    match r {
        Ok(v) => v.to_string(),
        Err(e) => format!("{}: {}", e.class.name(), e.message("")),
    }
}

#[test]
fn overloading() {
    let (a, b) = (z(42), z(17));
    let ga = Num::Gmp(&a);
    let gb = Num::Gmp(&b);
    let table = [
        (BinaryOp::Add, "59"),
        (BinaryOp::Sub, "25"),
        (BinaryOp::Mul, "714"),
        (BinaryOp::Div, "2"),
        (BinaryOp::Mod, "8"),
        (BinaryOp::Pow, "3937657486715347520027492352"),
        (BinaryOp::BitOr, "59"),
        (BinaryOp::BitAnd, "0"),
        (BinaryOp::BitXor, "59"),
        (BinaryOp::Shl, "5505024"),
    ];
    for (o, expected) in table {
        assert_eq!(op(o, ga, gb), expected, "{o:?}");
        assert_eq!(op(o, ga, Num::Int(17)), expected, "{o:?}");
        assert_eq!(op(o, Num::Int(42), gb), expected, "{o:?}");
    }
    assert_eq!(
        op(BinaryOp::Div, ga, Num::Int(0)),
        "DivisionByZeroError: Division by zero"
    );
    assert_eq!(
        op(BinaryOp::Mod, ga, Num::Int(0)),
        "DivisionByZeroError: Modulo by zero"
    );
    assert_eq!(op(BinaryOp::Shr, ga, Num::Int(2)), "10");
    let na = -&a;
    assert_eq!(op(BinaryOp::Shr, Num::Gmp(&na), Num::Int(2)), "-11");
    let shift = format!("ValueError: Shift must be between 0 and {}", u64::MAX);
    assert_eq!(op(BinaryOp::Shl, ga, Num::Int(-1)), shift);
    assert_eq!(op(BinaryOp::Shr, ga, Num::Int(-1)), shift);
    assert_eq!(php::bitwise_not(&a).to_string(), "-43");
    // gmp_pow2 / gh22549 / gmp_operator_rhs_gmp_overflow
    let exp = format!("ValueError: Exponent must be between 0 and {}", u64::MAX);
    assert_eq!(op(BinaryOp::Pow, Num::Gmp(&z(2)), Num::Int(10)), "1024");
    assert_eq!(op(BinaryOp::Pow, Num::Gmp(&z(2)), Num::Int(-10)), exp);
    assert_eq!(op(BinaryOp::Pow, Num::Int(2), Num::Gmp(&z(-5))), exp);
    let too_large = Mpz::pow2(64);
    assert_eq!(
        op(BinaryOp::Pow, Num::Gmp(&z(2)), Num::Gmp(&too_large)),
        exp
    );
    assert_eq!(
        op(BinaryOp::Shl, Num::Gmp(&z(2)), Num::Gmp(&too_large)),
        shift
    );
    assert_eq!(
        op(BinaryOp::Shr, Num::Gmp(&z(2)), Num::Gmp(&too_large)),
        shift
    );
    assert_eq!(op(BinaryOp::Shl, Num::Int(1), Num::Gmp(&z(4))), "16");
    // Operator-context string errors.
    assert_eq!(
        op(BinaryOp::Add, ga, Num::Str(b"abc")),
        "ValueError: Number is not an integer string"
    );
    assert_eq!(op(BinaryOp::Add, ga, Num::Str(b"0x10")), "58");
    assert_eq!(
        php::GmpError::operand_type("stdClass").message(""),
        "Number must be of type GMP|string|int, stdClass given"
    );
    assert_eq!(
        php::GmpError::unsupported_operands("GMP", BinaryOp::Shl, "array").message(""),
        "Unsupported operand types: GMP << array"
    );
}

#[test]
fn comparison_and_casts() {
    let (a, b) = (z(42), z(17));
    assert_eq!(php::compare(&a, &b), 1);
    assert_eq!(php::compare(&b, &a), -1);
    assert_eq!(php::compare(&a, &z(42)), 0);
    let e = php::operand(Num::Str(b"hapfegfbu")).unwrap_err();
    assert_eq!(e.message(""), "Number is not an integer string");
    assert_eq!(php::cast_string(&a), "42");
    assert_eq!(php::cast_int(&a), 42);
    assert_eq!(php::cast_float(&a).to_bits(), 42f64.to_bits());
    assert!(php::cast_bool(&a));
    assert!(!php::cast_bool(&z(0)));
    assert_eq!(
        php::cast_number(&z(i64::MAX - 1)),
        php::Number::Int(i64::MAX - 1)
    );
    assert_eq!(
        php::cast_number(&Mpz::pow2(64)),
        php::Number::Float(18_446_744_073_709_551_616.0)
    );
    assert!(php::cast_float(&Mpz::pow2(2000)).is_infinite());
}

#[test]
fn serialization() {
    assert_eq!(php::serialize_number(&z(42)), "2a");
    assert_eq!(php::serialize_number(&z(13)), "d");
    assert_eq!(php::serialize_number(&z(-255)), "-ff");
    assert_eq!(php::unserialize_number(Some(b"2a")).unwrap(), z(42));
    assert_eq!(php::unserialize_number(Some(b"-ff")).unwrap(), z(-255));
    let err = php::unserialize_number(Some(b"")).unwrap_err();
    assert_eq!(
        format!("{}: {}", err.class.name(), err.message("")),
        "Exception: Could not unserialize number"
    );
    assert!(php::unserialize_number(None).is_err());
    assert_eq!(php::unserialize_legacy_number(Some(b"42")).unwrap(), z(42));
    assert!(php::unserialize_legacy_number(Some(b"2a")).is_err());
    assert_eq!(
        php::unserialize_properties_error().text,
        "Could not unserialize properties"
    );
    assert_eq!(php::serialize_legacy(&z(42), "a:0:{}"), "s:2:\"42\";a:0:{}");
}

#[test]
fn import_export() {
    let hex = |b: &[u8]| {
        b.iter().fold(String::new(), |mut s, x| {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{x:02x}"));
            s
        })
    };
    let unhex = |s: &str| {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect::<Vec<u8>>()
    };
    let (be, le, lsw) = (
        php::GMP_BIG_ENDIAN,
        php::GMP_LITTLE_ENDIAN,
        php::GMP_LSW_FIRST,
    );
    let cases: [(&str, i64, i64, &str); 21] = [
        ("0", 1, be, ""),
        ("0", 2, be, ""),
        ("0", 3, be, ""),
        ("12345678", 1, be, "12345678"),
        ("12345678", 4, be, "12345678"),
        ("12345678", 4, lsw | be, "12345678"),
        ("12345678", 1, lsw | le, "78563412"),
        ("12345678", 4, le, "78563412"),
        ("12345678", 4, lsw | le, "78563412"),
        ("123456789ABC", 2, be, "123456789abc"),
        ("123456789ABC", 2, lsw | be, "9abc56781234"),
        ("123456789ABC", 2, le, "34127856bc9a"),
        ("123456789ABC", 2, lsw | le, "bc9a78563412"),
        (
            "112233445566778899AABBCC",
            4,
            be,
            "112233445566778899aabbcc",
        ),
        (
            "112233445566778899AABBCC",
            4,
            lsw | be,
            "99aabbcc5566778811223344",
        ),
        (
            "112233445566778899AABBCC",
            4,
            le,
            "4433221188776655ccbbaa99",
        ),
        (
            "112233445566778899AABBCC",
            4,
            lsw | le,
            "ccbbaa998877665544332211",
        ),
        (
            "100120023003400450056006700780089009A00AB00BC00C",
            8,
            be,
            "100120023003400450056006700780089009a00ab00bc00c",
        ),
        (
            "100120023003400450056006700780089009A00AB00BC00C",
            8,
            lsw | be,
            "9009a00ab00bc00c50056006700780081001200230034004",
        ),
        (
            "100120023003400450056006700780089009A00AB00BC00C",
            8,
            le,
            "044003300220011008800770066005500cc00bb00aa00990",
        ),
        (
            "100120023003400450056006700780089009A00AB00BC00C",
            8,
            lsw | le,
            "0cc00bb00aa0099008800770066005500440033002200110",
        ),
    ];
    for (num, size, flags, out) in cases {
        let n = php::gmp_init(php::StrOrInt::Str(num.as_bytes()), 16).unwrap();
        assert_eq!(
            hex(&php::gmp_export(&n, size, flags).unwrap()),
            out,
            "{num} {size} {flags}"
        );
        let back = php::gmp_import(&unhex(out), size, flags).unwrap();
        assert_eq!(php::gmp_strval(&back, 16).unwrap(), num.to_lowercase());
    }
    assert_eq!(
        hex(&php::gmp_export(&z(0xff), 1, php::DEFAULT_IMPORT_EXPORT_FLAGS).unwrap()),
        "ff"
    );
    let msg = |f: &str, e: php::GmpError| format!("{}: {}", e.class.name(), e.message(f));
    for size in [-1, 0] {
        assert_eq!(
            msg("gmp_export", php::gmp_export(&z(123), size, 0).unwrap_err()),
            "ValueError: gmp_export(): Argument #2 ($word_size) must be greater than or equal to 1"
        );
        assert_eq!(
            msg("gmp_import", php::gmp_import(b"a", size, 0).unwrap_err()),
            "ValueError: gmp_import(): Argument #2 ($word_size) must be greater than or equal to 1"
        );
    }
    let both_order = php::GMP_MSW_FIRST | php::GMP_LSW_FIRST;
    assert_eq!(
        msg(
            "gmp_export",
            php::gmp_export(&z(123), 1, both_order).unwrap_err()
        ),
        "ValueError: gmp_export(): Argument #3 ($flags) cannot use multiple word order options"
    );
    assert_eq!(
        msg(
            "gmp_export",
            php::gmp_export(&z(123), 1, be | le).unwrap_err()
        ),
        "ValueError: gmp_export(): Argument #3 ($flags) cannot use multiple endian options"
    );
    assert_eq!(
        msg(
            "gmp_import",
            php::gmp_import(b"a", 1, both_order).unwrap_err()
        ),
        "ValueError: gmp_import(): Argument #3 ($flags) cannot use multiple word order options"
    );
    assert_eq!(
        msg("gmp_import", php::gmp_import(b"a", 1, be | le).unwrap_err()),
        "ValueError: gmp_import(): Argument #3 ($flags) cannot use multiple endian options"
    );
    for (data, size) in [(&b"a"[..], 2), (b"aa", 3), (&[b'a'; 100][..], 64)] {
        assert_eq!(msg("gmp_import", php::gmp_import(data, size, 0).unwrap_err()), "ValueError: gmp_import(): Argument #1 ($data) must be a multiple of argument #2 ($word_size)");
    }
    // gh16411
    let n = php::arg(Num::Str(b"-9223372036854775808"), 1).unwrap();
    assert_eq!(
        msg(
            "gmp_export",
            php::gmp_export(&n, i64::MAX, i64::MIN).unwrap_err()
        ),
        "ValueError: gmp_export(): Argument #2 ($word_size) is too large for argument #1 ($num)"
    );
}

#[test]
fn seeded_random() {
    let big = Mpz::pow2(512);
    let seeds: [(Mpz, [&str; 6]); 8] = [
        (z(0), ["107", "576055025228722307492589900056", "5075491613651149525976453192895895253653438900772590630831858908690082668789318258254821002217677675804439098856210618572534955562143303188483908287009522532300439665975877709754914215718998849272363858786685187951932478210775857465448084868199807983919191214972626993925394176279001074206804955195464", "4098", "866", "-4602"]),
        (z(0), ["107", "576055025228722307492589900056", "5075491613651149525976453192895895253653438900772590630831858908690082668789318258254821002217677675804439098856210618572534955562143303188483908287009522532300439665975877709754914215718998849272363858786685187951932478210775857465448084868199807983919191214972626993925394176279001074206804955195464", "4098", "866", "-4602"]),
        (z(-100), ["800", "136797365759249926716355081555", "983682312243221532860194306859606025979259367996304596374614332718375645173854152266611727577102182844028492473112400528817154210713755887896949560718745264129216953815968005630126359941634684721501777057142617647654380585317016323758806063124938232519551123440573348326061244006512869165793958775168", "1029", "7093", "-9074"]),
        (z(-100), ["800", "136797365759249926716355081555", "983682312243221532860194306859606025979259367996304596374614332718375645173854152266611727577102182844028492473112400528817154210713755887896949560718745264129216953815968005630126359941634684721501777057142617647654380585317016323758806063124938232519551123440573348326061244006512869165793958775168", "1029", "7093", "-9074"]),
        (z(100), ["800", "136797365759249926716355081555", "983682312243221532860194306859606025979259367996304596374614332718375645173854152266611727577102182844028492473112400528817154210713755887896949560718745264129216953815968005630126359941634684721501777057142617647654380585317016323758806063124938232519551123440573348326061244006512869165793958775168", "1029", "7093", "-9074"]),
        (z(100), ["800", "136797365759249926716355081555", "983682312243221532860194306859606025979259367996304596374614332718375645173854152266611727577102182844028492473112400528817154210713755887896949560718745264129216953815968005630126359941634684721501777057142617647654380585317016323758806063124938232519551123440573348326061244006512869165793958775168", "1029", "7093", "-9074"]),
        (-&big, ["762", "822340340897453415684831711085", "7240560133683902061389868703829443708354917824328579773726122219756981024103097560162756171513655189995985599958252688592185764428631571614485572869738344560301294144844739876478557439580966605216861285841689262517286639329902832431755450003123084728943981078635297917573398492558065003906539489023830", "9636", "-9848", "-9648"]),
        (big, ["762", "822340340897453415684831711085", "7240560133683902061389868703829443708354917824328579773726122219756981024103097560162756171513655189995985599958252688592185764428631571614485572869738344560301294144844739876478557439580966605216861285841689262517286639329902832431755450003123084728943981078635297917573398492558065003906539489023830", "9636", "-9848", "-9648"]),
    ];
    let mut rng = GmpRandom::new();
    let entropy = || 12345;
    for (seed, expected) in seeds {
        rng.seed(&seed, entropy);
        let got = [
            rng.bits(10, entropy).unwrap().to_string(),
            rng.bits(100, entropy).unwrap().to_string(),
            rng.bits(1000, entropy).unwrap().to_string(),
            rng.range(&z(0), &z(10000), entropy).unwrap().to_string(),
            rng.range(&z(-10000), &z(10000), entropy)
                .unwrap()
                .to_string(),
            rng.range(&z(-10000), &z(0), entropy).unwrap().to_string(),
        ];
        assert_eq!(got, expected.map(str::to_string), "seed {seed}");
    }
    let e = php::arg(Num::Str(b"not a number"), 1).unwrap_err();
    assert_eq!(
        e.message("gmp_random_seed"),
        "gmp_random_seed(): Argument #1 ($seed) is not an integer string"
    );
    let msg = |f: &str, e: php::GmpError| e.message(f);
    for bits in [0, -1, i64::MAX] {
        assert_eq!(
            msg("gmp_random_bits", rng.bits(bits, entropy).unwrap_err()),
            "gmp_random_bits(): Argument #1 ($bits) must be between 1 and 2147483647"
        );
    }
    for (lo, hi) in [(10, -10), (10, 0), (5, 5)] {
        assert_eq!(
            msg(
                "gmp_random_range",
                rng.range(&z(lo), &z(hi), entropy).unwrap_err()
            ),
            "gmp_random_range(): Argument #1 ($min) must be less than argument #2 ($max)"
        );
    }
    for _ in 0..2000 {
        let r = rng.range(&z(-500), &z(500), entropy).unwrap();
        assert!(r >= z(-500) && r <= z(500));
        let b = rng.bits(7, entropy).unwrap();
        assert!(b < z(128));
    }
    // Lazy initialization uses the supplied entropy exactly once.
    let mut fresh = GmpRandom::new();
    let mut calls = 0;
    let _ = fresh.bits(8, || {
        calls += 1;
        7
    });
    let _ = fresh.bits(8, || panic!("entropy requested twice"));
    assert_eq!(calls, 1);
}
