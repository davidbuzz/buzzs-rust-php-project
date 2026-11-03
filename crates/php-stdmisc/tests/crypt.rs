//! Oracle tests for `crypt()` and the `password_*` API.

mod common;

use common::{export, fields, fixture_lines, show, unhex};
use php_runtime::{CollectingContext, Value, ZArray};
use php_stdmisc::crypt::argon2::{self, Variant};
use php_stdmisc::crypt::password::{self, AlgoArg};
use php_stdmisc::crypt::{crypt, password::password_verify};

#[test]
fn crypt_matches_oracle() {
    let mut n = 0;
    for line in fixture_lines("crypt.txt") {
        let f = fields(&line);
        let (pw, salt, expect) = (unhex(f[0]), unhex(f[1]), unhex(f[2]));
        let got = crypt(&pw, &salt);
        assert_eq!(
            show(&got),
            show(&expect),
            "crypt({:?}, {:?})",
            show(&pw),
            show(&salt)
        );
        n += 1;
    }
    assert!(n > 500);
}

fn rehash_options(i: usize) -> Option<ZArray> {
    let pairs: Vec<(&str, Value)> = match i {
        0 => return None,
        1 => vec![],
        2 => vec![("cost", Value::Int(10))],
        3 => vec![("cost", Value::Int(12))],
        4 => vec![("cost", Value::from("10"))],
        5 => vec![
            ("memory_cost", Value::Int(65536)),
            ("time_cost", Value::Int(4)),
            ("threads", Value::Int(1)),
        ],
        6 => vec![
            ("memory_cost", Value::Int(1024)),
            ("time_cost", Value::Int(2)),
            ("threads", Value::Int(2)),
        ],
        7 => vec![("threads", Value::Float(1.5))],
        _ => unreachable!(),
    };
    Some(ZArray::from_pairs(pairs))
}

#[test]
fn password_api_matches_oracle() {
    let mut ctx = CollectingContext::new();
    for line in fixture_lines("password.txt") {
        let f = fields(&line);
        match f[0] {
            "info" => {
                let h = unhex(f[1]);
                // master: bcrypt hashes with a cost outside 4..=31 are not
                // identified as bcrypt (8.3 accepted any two digits).
                if h.len() == 60 && h.starts_with(b"$2y$") && password::identify(&h).is_none() {
                    continue;
                }
                let got = export(&Value::Array(password::password_get_info(&h)));
                assert_eq!(got, show(&unhex(f[2])), "password_get_info({:?})", show(&h));
            }
            "rehash" => {
                let h = unhex(f[1]);
                let tag = f[2];
                let sbuf;
                let algo = match tag.as_bytes()[0] {
                    b'n' => AlgoArg::Null,
                    b'i' => AlgoArg::Int(tag[1..].parse().unwrap()),
                    _ => {
                        sbuf = unhex(&tag[1..]);
                        AlgoArg::Str(&sbuf)
                    }
                };
                let oi: usize = f[3].parse().unwrap();
                let opts = rehash_options(oi);
                let mut expect = f[4] == "1";
                // php-src master raised PASSWORD_BCRYPT_DEFAULT_COST from 10 to 12.
                let is_bcrypt = matches!(password::find_algo(algo), Some(password::Algo::Bcrypt));
                if is_bcrypt
                    && password::identify(&h) == Some(password::Algo::Bcrypt)
                    && opts.as_ref().is_none_or(|o| o.get_str(b"cost").is_none())
                {
                    expect = &h[4..6] != b"12";
                }
                let got =
                    password::password_needs_rehash(&h, algo, opts.as_ref(), &mut ctx).unwrap();
                assert_eq!(
                    got,
                    expect,
                    "password_needs_rehash({:?}, {tag}, #{oi})",
                    show(&h)
                );
            }
            "verify" => {
                let (p, h) = (unhex(f[1]), unhex(f[2]));
                assert_eq!(
                    password_verify(&p, &h),
                    f[3] == "1",
                    "password_verify({:?}, {:?})",
                    show(&p),
                    show(&h)
                );
            }
            "argon2" | "bcrypt" => {
                let (p, h) = (unhex(f[1]), unhex(f[2]));
                assert!(password_verify(&p, &h), "verify {:?}", show(&h));
                if f[0] == "argon2" {
                    let variant = if h.starts_with(b"$argon2id") {
                        Variant::Id
                    } else {
                        Variant::I
                    };
                    let d = argon2::decode(variant, &h).unwrap();
                    let again = argon2::hash_encoded(variant, &d.params, &p, &d.salt, d.hash.len())
                        .unwrap();
                    assert_eq!(again.as_bytes(), &h[..]);
                    let mut wrong = p.clone();
                    wrong.push(b'!');
                    assert!(!password_verify(&wrong, &h));
                }
            }
            other => panic!("unknown fixture tag {other}"),
        }
    }
}

#[test]
fn password_hash_shapes() {
    let mut ctx = CollectingContext::new();
    let opts = ZArray::from_pairs([
        ("memory_cost", Value::Int(64)),
        ("time_cost", Value::Int(1)),
    ]);
    for algo in [AlgoArg::Str(b"argon2i"), AlgoArg::Str(b"argon2id")] {
        let h = password::password_hash(b"secret", algo, Some(&opts), &mut ctx).unwrap();
        assert!(h.as_bytes().starts_with(b"$argon2i"));
        assert!(password_verify(b"secret", h.as_bytes()));
        let info = password::password_get_info(h.as_bytes());
        assert!(export(&Value::Array(info)).contains("'memory_cost' => 64"));
    }
    let opts = ZArray::from_pairs([("memory_cost", Value::Int(8)), ("threads", Value::Int(2))]);
    let e =
        password::password_hash(b"x", AlgoArg::Str(b"argon2i"), Some(&opts), &mut ctx).unwrap_err();
    assert_eq!(e.message(), Some("Memory cost is too small"));
    let e = password::password_hash(b"a\0b", AlgoArg::Null, None, &mut ctx).unwrap_err();
    assert_eq!(
        e.message(),
        Some("Bcrypt password must not contain null character")
    );
}
