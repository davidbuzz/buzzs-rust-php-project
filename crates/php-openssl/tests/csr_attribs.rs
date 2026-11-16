// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `openssl_csr_new` with extra attributes (own process: the config's
//! `string_mask` is global OpenSSL state).

mod common;
use common::*;
use php_openssl::csr::CsrNewReturn;
use php_openssl::{CsrArg, KeyArg, OpenSsl, PhpArray, PhpValue};

fn arr(items: &[(&str, &str)]) -> PhpArray {
    let mut a = PhpArray::new();
    for (k, v) in items {
        a.set(*k, PhpValue::str(v));
    }
    a
}

fn uk_dn() -> PhpArray {
    arr(&[
        ("countryName", "UK"),
        ("stateOrProvinceName", "England"),
        ("localityName", "London"),
        ("commonName", "test.php.net"),
        ("emailAddress", "test.php@php.net"),
    ])
}

#[test]
fn csr_with_attributes_matches_master() {
    let mut o = OpenSsl::new();
    let mut args = arr(&[
        ("digest_alg", "sha256"),
        ("config", &fixture("openssl_csr_attribs.cnf")),
    ]);
    args.set("private_key_bits", PhpValue::Int(2048));
    args.set("private_key_type", PhpValue::Int(1));
    args.set("encrypt_key", PhpValue::Bool(true));
    let key = fixture_url("private_rsa_2048.key");
    let attrs = arr(&[
        ("emailAddress", "info@example.com"),
        ("aansluitNummer", "11112222"),
        ("postalCode", "N11"),
    ]);
    let r = o
        .csr_new(
            &uk_dn(),
            Some(&KeyArg::Str(&key)),
            Some(&args),
            Some(&attrs),
        )
        .unwrap();
    assert!(r.generated_key.is_none());
    let CsrNewReturn::Csr(csr) = r.ret else {
        panic!("no csr")
    };
    let subj = o
        .csr_get_subject(CsrArg::Object(&csr), true)
        .unwrap()
        .unwrap();
    assert_eq!(
        String::from_utf8(var_export(&PhpValue::Array(subj))).unwrap(),
        "array (\n  'C' => 'UK',\n  'ST' => 'England',\n  'L' => 'London',\n  'CN' => 'test.php.net',\n  'emailAddress' => 'test.php@php.net',\n)"
    );
    let pem = o.csr_export(CsrArg::Object(&csr), true).unwrap().unwrap();
    let want = std::fs::read(format!(
        "{}/tests/expected/csr_with_attribs.pem",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    assert_eq!(
        String::from_utf8(pem).unwrap(),
        String::from_utf8(want).unwrap()
    );
    assert!(warnings(&mut o).is_empty());

    let r = o
        .csr_new(
            &uk_dn(),
            Some(&KeyArg::Str(&key)),
            Some(&args),
            Some(&arr(&[("wrong", "x")])),
        )
        .unwrap();
    assert!(matches!(r.ret, CsrNewReturn::Csr(_)));
    assert_eq!(
        warnings(&mut o),
        ["attributes: wrong is not a recognized attribute name"]
    );
}
