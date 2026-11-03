//! Cross-checks the engine against results recorded from the system libgmp
//! (`tests/data/gen_oracle.py`, GMP 6.3.0 via ctypes).

use php_gmp::random::MtState;
use php_gmp::{Endian, Mpz, Round, WordOrder};

fn z(s: &str) -> Mpz {
    let (neg, digits) = s.strip_prefix('-').map_or((false, s), |d| (true, d));
    let v = Mpz::parse(digits.as_bytes(), 16).unwrap_or_else(|| panic!("bad hex {s}"));
    if neg {
        -v
    } else {
        v
    }
}

fn h(v: &Mpz) -> String {
    v.to_str_radix(16)
}

fn hu(v: u64) -> String {
    format!("{v:x}")
}

fn hi(v: i64) -> String {
    if v < 0 {
        format!("-{:x}", v.unsigned_abs())
    } else {
        format!("{v:x}")
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn hexbytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn order(s: &str) -> WordOrder {
    if s == "1" {
        WordOrder::MostSignificantFirst
    } else {
        WordOrder::LeastSignificantFirst
    }
}

fn endian(s: &str) -> Endian {
    match s {
        "1" => Endian::Big,
        "-1" => Endian::Little,
        _ => Endian::Native,
    }
}

fn small(s: &str) -> u64 {
    u64::from_str_radix(s, 16).expect("small hex")
}

#[allow(clippy::too_many_lines)]
fn eval(f: &[&str]) -> Vec<String> {
    let a = || z(f[1]);
    let b = || z(f[2]);
    match f[0] {
        "add" => vec![h(&(&a() + &b()))],
        "sub" => vec![h(&(&a() - &b()))],
        "mul" => vec![h(&(&a() * &b()))],
        "tdiv" | "cdiv" | "fdiv" => {
            let round = match f[0] {
                "tdiv" => Round::Zero,
                "cdiv" => Round::PlusInf,
                _ => Round::MinusInf,
            };
            let (q, r) = a().div_qr(&b(), round);
            vec![h(&q), h(&r)]
        }
        "mod" => vec![h(&a().modulo(&b()))],
        "divexact" => vec![h(&a().divexact(&b()))],
        "gcd" => vec![h(&a().gcd(&b()))],
        "lcm" => vec![h(&a().lcm(&b()))],
        "gcdext" => {
            let (g, s, t) = a().gcdext(&b());
            vec![h(&g), h(&s), h(&t)]
        }
        "and" => vec![h(&a().and(&b()))],
        "or" => vec![h(&a().or(&b()))],
        "xor" => vec![h(&a().xor(&b()))],
        "com" => vec![h(&a().com())],
        "hamdist" => vec![hu(a().hamdist(&b()))],
        "popcount" => vec![hu(a().popcount())],
        "invert" => vec![a().invert(&b()).map_or_else(|| "none".into(), |v| h(&v))],
        "jacobi" => vec![hi(a().kronecker(&b()).into())],
        "bits" => {
            let (v, i) = (a(), small(f[2]));
            let mut s = v.clone();
            s.set_bit(i);
            let mut c = v.clone();
            c.clear_bit(i);
            vec![
                hu(u64::from(v.test_bit(i))),
                hu(v.scan0(i)),
                hu(v.scan1(i)),
                h(&s),
                h(&c),
            ]
        }
        "shift" => {
            let (v, i) = (a(), small(f[2]));
            vec![h(&v.shl(i)), h(&v.shr_floor(i))]
        }
        "getsi" => vec![hi(a().get_si())],
        "getd" => vec![hexbytes(&a().to_f64().to_be_bytes())],
        "sqrtrem" => {
            let (r, m) = a().sqrtrem();
            vec![h(&r), h(&m)]
        }
        "rootrem" => {
            let (r, m) = a().rootrem(small(f[2]));
            vec![h(&r), h(&m)]
        }
        "perfect" => {
            let v = a();
            vec![
                hu(u64::from(v.is_perfect_square())),
                hu(u64::from(v.is_perfect_power())),
            ]
        }
        "powm" => vec![h(&a().powm(&b(), &z(f[3])))],
        "pow" => vec![h(&a().pow(small(f[2])))],
        "fact" => vec![h(&Mpz::factorial(small(f[1])))],
        "binomial" => vec![h(&a().binomial(small(f[2])))],
        "prime" => vec![hi(a().probab_prime(0).into())],
        "nextprime" => vec![h(&a().next_prime())],
        "prevprime" => {
            let (p, r) = a().prev_prime().expect("prev prime");
            vec![h(&p), hi(r.into())]
        }
        "getstr" => {
            let base = i32::try_from(z(f[2]).get_si()).expect("base");
            vec![hexbytes(a().to_str_radix(base).as_bytes())]
        }
        "setstr" => {
            let base = u32::try_from(small(f[2])).expect("base");
            vec![Mpz::parse(&unhex(f[1]), base).map_or_else(|| "err".into(), |v| h(&v))]
        }
        "export" => {
            let size = usize::try_from(small(f[2])).expect("size");
            vec![hexbytes(&a().export(size, order(f[3]), endian(f[4])))]
        }
        "import" => {
            let size = usize::try_from(small(f[2])).expect("size");
            vec![h(&Mpz::import(&unhex(f[1]), size, order(f[3]), endian(f[4])))]
        }
        "rand" => {
            let mut st = MtState::new(&a());
            let parts: Vec<String> = f[2]
                .split(',')
                .map(|item| {
                    let (lhs, _) = item.split_once('=').expect("rand item");
                    let v = if let Some(bits) = lhs.strip_prefix('b') {
                        st.urandomb(small(bits))
                    } else {
                        st.urandomm(&z(&lhs[1..]))
                    };
                    format!("{lhs}={}", h(&v))
                })
                .collect();
            return vec![f[0].to_string(), f[1].to_string(), parts.join(",")];
        }
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn matches_libgmp() {
    let data = include_str!("data/gmp_oracle.txt");
    let mut failures = Vec::new();
    let mut count = 0;
    for line in data.lines() {
        let f: Vec<&str> = line.split('|').collect();
        let got = eval(&f);
        let expected: Vec<&str> = if f[0] == "rand" {
            f.clone()
        } else {
            let n = got.len();
            f[f.len() - n..].to_vec()
        };
        count += 1;
        if got.iter().map(String::as_str).ne(expected.iter().copied()) {
            let short: String = line.chars().take(300).collect();
            failures.push(format!("{short}\n   got {:?}", got.iter().map(|s| s.chars().take(120).collect::<String>()).collect::<Vec<_>>()));
        }
    }
    assert!(count > 25_000, "oracle too small: {count}");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
    );
}
