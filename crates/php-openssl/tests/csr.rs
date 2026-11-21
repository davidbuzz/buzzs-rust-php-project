// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! CSR functions (expected values from php-src master tests and the
//! `/usr/bin/php8.5` oracle).

#![allow(clippy::too_many_lines)]

mod common;
use common::*;
use php_openssl::csr::CsrNewReturn;
use php_openssl::{CertArg, CsrArg, KeyArg, OpenSsl, PhpArray, PhpValue, Throw};

fn arr(items: &[(&str, &str)]) -> PhpArray {
    let mut a = PhpArray::new();
    for (k, v) in items {
        a.set(*k, PhpValue::str(v));
    }
    a
}

#[test]
fn csr_new_basic_php_test() {
    let mut o = OpenSsl::new();
    let conf = arr(&[("config", &fixture("openssl.cnf"))]);
    // `$a = array()` as the key: ValueError, raised after the CSR was built.
    let bad = KeyArg::Array {
        key: None,
        passphrase: None,
    };
    let r = o.csr_new(
        &PhpArray::new(),
        Some(&bad),
        Some(&conf),
        Some(&PhpArray::new()),
    );
    assert_eq!(
        r.unwrap_err(),
        Throw::ValueError("Key array must be of the form array(0 => key, 1 => phrase)".into())
    );
    assert_eq!(
        warnings(&mut o),
        ["add1_attr_by_txt challengePassword_min -> 4 (failed; check error queue and value of string_mask OpenSSL option if illegal characters are reported)"]
    );
    // array(1, 2) as DN and key: numeric keys ignored, key generated.
    let mut a = PhpArray::new();
    a.push(PhpValue::Int(1));
    a.push(PhpValue::Int(2));
    let b = KeyArg::Array {
        key: Some(Box::new(KeyArg::Other)),
        passphrase: Some(b"2".to_vec()),
    };
    let r = o.csr_new(&a, Some(&b), Some(&conf), None).unwrap();
    assert!(matches!(r.ret, CsrNewReturn::Csr(_)));
    assert!(r.generated_key.is_some());
}

#[test]
fn csr_sign_serials() {
    let mut o = OpenSsl::new();
    let conf = arr(&[("config", &fixture("openssl.cnf"))]);
    let mut dn = arr(&[("CN", "x")]);
    let mut orgs = PhpArray::new();
    orgs.push(PhpValue::str("a"));
    orgs.push(PhpValue::str("b"));
    dn.set("O", PhpValue::Array(orgs));
    let key = fixture_url("private_rsa_2048.key");
    let k = KeyArg::Str(&key);
    let r = o.csr_new(&dn, Some(&k), Some(&conf), None).unwrap();
    let CsrNewReturn::Csr(csr) = r.ret else {
        panic!("{:?} {:?}", o.take_diagnostics(), errors(&mut o))
    };
    let subj = o
        .csr_get_subject(CsrArg::Object(&csr), false)
        .unwrap()
        .unwrap();
    assert_eq!(
        String::from_utf8(var_export(&PhpValue::Array(subj))).unwrap(),
        "array (\n  'commonName' => 'x',\n  'organizationName' => \n  array (\n    0 => 'a',\n    1 => 'b',\n  ),\n  'countryName' => 'AU',\n  'stateOrProvinceName' => 'Some-State',\n)"
    );
    let cert = o
        .csr_sign(
            CsrArg::Object(&csr),
            None,
            &k,
            365,
            Some(&conf),
            1_234_567,
            None,
        )
        .unwrap()
        .unwrap();
    let p = o.x509_parse(CertArg::Object(&cert), true).unwrap().unwrap();
    assert_eq!(p.get_str("serialNumber"), Some(&PhpValue::str("1234567")));
    assert_eq!(p.get_str("serialNumberHex"), Some(&PhpValue::str("12D687")));
    assert_eq!(
        p.get_str("signatureTypeSN"),
        Some(&PhpValue::str("RSA-SHA256"))
    );
    let ext = p
        .get_str("extensions")
        .and_then(PhpValue::as_array)
        .unwrap();
    assert_eq!(
        ext.get_str("basicConstraints"),
        Some(&PhpValue::str("CA:TRUE"))
    );
    assert!(ext
        .get_str("authorityKeyIdentifier")
        .unwrap()
        .as_str()
        .unwrap()
        .ends_with(b"\nserial:12:D6:87"));
    let c2 = o
        .csr_sign(
            CsrArg::Object(&csr),
            None,
            &k,
            365,
            Some(&conf),
            0,
            Some(b"DEADBEEF"),
        )
        .unwrap()
        .unwrap();
    let p = o.x509_parse(CertArg::Object(&c2), true).unwrap().unwrap();
    assert_eq!(
        p.get_str("serialNumber"),
        Some(&PhpValue::str("3735928559"))
    );
    let long = b"DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF";
    let c3 = o
        .csr_sign(
            CsrArg::Object(&csr),
            None,
            &k,
            365,
            Some(&conf),
            10,
            Some(long),
        )
        .unwrap()
        .unwrap();
    let p = o.x509_parse(CertArg::Object(&c3), true).unwrap().unwrap();
    assert_eq!(
        p.get_str("serialNumber"),
        Some(&PhpValue::str("0xDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF"))
    );
    for bad in [&b"DEADBEEG"[..], b"0xDEADBEEF"] {
        assert!(o
            .csr_sign(
                CsrArg::Object(&csr),
                None,
                &k,
                365,
                Some(&conf),
                0,
                Some(bad)
            )
            .unwrap()
            .is_none());
        assert_eq!(warnings(&mut o), ["Error parsing serial number"]);
    }
    let huge = vec![b'F'; 1000];
    assert!(o
        .csr_sign(
            CsrArg::Object(&csr),
            None,
            &k,
            365,
            Some(&conf),
            0,
            Some(&huge)
        )
        .unwrap()
        .is_none());
    assert_eq!(
        warnings(&mut o),
        ["Error parsing serial number because it is too long"]
    );
    assert!(o
        .csr_sign(CsrArg::Object(&csr), None, &k, -1, None, 0, None)
        .unwrap()
        .is_none());
    assert_eq!(
        warnings(&mut o),
        ["Days must be between 0 and 106751991167300"]
    );
    // Signed by a CA certificate with a non-matching key.
    let other = fixture_url("private_rsa_1024.key");
    let r = o.csr_sign(
        CsrArg::Object(&csr),
        Some(CertArg::Object(&cert)),
        &KeyArg::Str(&other),
        1,
        None,
        0,
        None,
    );
    assert!(r.unwrap().is_none());
    assert_eq!(
        warnings(&mut o),
        ["Private key does not correspond to signing cert"]
    );
    let signed = o
        .csr_sign(
            CsrArg::Object(&csr),
            Some(CertArg::Object(&cert)),
            &k,
            1,
            None,
            0,
            None,
        )
        .unwrap();
    assert!(signed.is_some());
}

#[test]
fn csr_files_and_public_key() {
    let mut o = OpenSsl::new();
    let csr_url = fixture_url("cert.csr");
    let k = o
        .csr_get_public_key(CsrArg::Str(&csr_url), true)
        .unwrap()
        .unwrap();
    assert!(!k.is_private());
    let d = o.pkey_get_details(&k).unwrap();
    assert_eq!(d.get_str("type"), Some(&PhpValue::Int(0)));
    let text = o.csr_export(CsrArg::Str(&csr_url), false).unwrap().unwrap();
    assert!(text.starts_with(b"Certificate Request:\n"));
    let path = tmp("req.pem");
    assert!(o
        .csr_export_to_file(CsrArg::Str(&csr_url), path.as_bytes(), true)
        .unwrap());
    let written = std::fs::read(&path).unwrap();
    assert!(written.starts_with(b"-----BEGIN CERTIFICATE REQUEST-----\n"));
    assert!(o
        .csr_get_subject(CsrArg::Str(b"nope"), true)
        .unwrap()
        .is_none());
    assert!(o.csr_export(CsrArg::Str(b"nope"), true).unwrap().is_none());
    assert_eq!(
        warnings(&mut o),
        ["X.509 Certificate Signing Request cannot be retrieved"]
    );
}
