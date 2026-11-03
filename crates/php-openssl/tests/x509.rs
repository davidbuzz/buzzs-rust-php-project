//! X.509 functions checked against `/usr/bin/php` dumps.

mod common;
use common::*;
use php_openssl::{BoolOrInt, CertArg, KeyArg, OpenSsl, PhpValue};

/// Splits an oracle dump into (warnings, `var_export` body).
fn expected(name: &str) -> (Vec<String>, Vec<u8>) {
    let text = std::fs::read(format!(
        "{}/tests/expected/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut warns = vec![];
    let mut body = Vec::new();
    for line in text.split_inclusive(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(&b"PHP Warning:  openssl_x509_parse(): "[..]) {
            let rest = String::from_utf8_lossy(rest);
            warns.push(
                rest.split(" in Command line code")
                    .next()
                    .unwrap()
                    .to_string(),
            );
        } else {
            body.extend_from_slice(line);
        }
    }
    (warns, body)
}

#[test]
fn parse_matches_php() {
    for f in [
        "cert.crt",
        "crit.crt",
        "cve2013_4073.pem",
        "bug28382cert.txt",
        "bug37820cert.pem",
        "cve-2013-6420.crt",
        "bug65698.crt",
        "public.crt",
        "bug64802.pem",
    ] {
        for short in [true, false] {
            let mut o = OpenSsl::new();
            let data = read(f);
            let mut got = o.x509_parse(CertArg::Str(&data), short).unwrap().expect(f);
            let (warns, body) = expected(&format!(
                "parse_{}{f}.txt",
                if short { "" } else { "long_" }
            ));
            // `criticalExtensions` is new in php-src master (the 8.3 oracle
            // lacks it); compare it separately.
            if !body.windows(20).any(|w| w == b"'criticalExtensions'") {
                if let Some(c) = got.get_str("criticalExtensions").cloned() {
                    let mut want = php_openssl::PhpArray::new();
                    match f {
                        "cve2013_4073.pem" => want.push(PhpValue::str("basicConstraints")),
                        "bug64802.pem" => {
                            want.push(PhpValue::str("keyUsage"));
                            want.push(PhpValue::str("basicConstraints"));
                        }
                        "bug65698.crt" => {
                            want.push(PhpValue::str("basicConstraints"));
                            want.push(PhpValue::str("keyUsage"));
                        }
                        _ => panic!("unexpected critical extensions in {f}"),
                    }
                    assert_eq!(c, PhpValue::Array(want));
                    got = got
                        .into_entries()
                        .into_iter()
                        .filter(|(k, _)| *k != php_openssl::Key::from("criticalExtensions"))
                        .collect();
                }
            }
            assert_eq!(
                String::from_utf8_lossy(&var_export(&PhpValue::Array(got))),
                String::from_utf8_lossy(&body),
                "{f} short={short}"
            );
            assert_eq!(warnings(&mut o), warns, "{f}");
        }
    }
}

#[test]
fn read_parse_object_and_failures() {
    let mut o = OpenSsl::new();
    let url = fixture_url("crit.crt");
    let c = o.x509_read(CertArg::Str(&url)).unwrap().unwrap();
    let a = o.x509_parse(CertArg::Object(&c), true).unwrap().unwrap();
    let b = o.x509_parse(CertArg::Str(&url), true).unwrap().unwrap();
    assert_eq!(a, b);
    assert!(o.x509_read(CertArg::Str(b"garbage")).unwrap().is_none());
    assert_eq!(warnings(&mut o), ["X.509 Certificate cannot be retrieved"]);
    assert_eq!(
        errors(&mut o),
        ["error:0480006C:PEM routines::no start line"]
    );
    assert!(o.x509_parse(CertArg::Str(b"x"), true).unwrap().is_none());
    assert!(warnings(&mut o).is_empty());
}

#[test]
fn fingerprint() {
    let mut o = OpenSsl::new();
    let data = read("cert.crt");
    let c = CertArg::Str(&data);
    assert_eq!(
        o.x509_fingerprint(c, b"sha1", false).unwrap().unwrap(),
        b"6e6fd1ea10a5a23071d61c728ee9b40df6dbc33c".to_vec()
    );
    assert_eq!(
        o.x509_fingerprint(c, b"md5", false).unwrap().unwrap(),
        b"ac77008e172897e06c0b065294487a67".to_vec()
    );
    assert_eq!(
        o.x509_fingerprint(c, b"sha1", true).unwrap().unwrap().len(),
        20
    );
    assert!(o.x509_fingerprint(c, b"xx", false).unwrap().is_none());
    assert_eq!(warnings(&mut o), ["Unknown digest algorithm"]);
}

#[test]
fn private_key_check_and_verify() {
    let mut o = OpenSsl::new();
    let cert = read("cert.crt");
    let key = fixture_url("private_rsa_1024.key");
    let other = fixture_url("private_rsa_2048.key");
    assert!(o
        .x509_check_private_key(CertArg::Str(&cert), &KeyArg::Str(&key))
        .unwrap());
    assert!(!o
        .x509_check_private_key(CertArg::Str(&cert), &KeyArg::Str(&other))
        .unwrap());
    let pubk = fixture_url("public.key");
    assert_eq!(
        o.x509_verify(CertArg::Str(&cert), &KeyArg::Str(&pubk))
            .unwrap(),
        1
    );
    let other_pub = fixture_url("public_rsa_2048.key");
    assert_eq!(
        o.x509_verify(CertArg::Str(&cert), &KeyArg::Str(&other_pub))
            .unwrap(),
        0
    );
    assert_eq!(
        o.x509_verify(CertArg::Str(b"x"), &KeyArg::Str(&pubk))
            .unwrap(),
        -1
    );
}

#[test]
fn checkpurpose() {
    let mut o = OpenSsl::new();
    let cert = read("cert.crt");
    let ca = fixture("cert.crt").into_bytes();
    let r = o
        .x509_checkpurpose(CertArg::Str(&cert), 1, std::slice::from_ref(&ca), None)
        .unwrap();
    assert_eq!(r, BoolOrInt::Bool(false));
    let r = o
        .x509_checkpurpose(CertArg::Str(&cert), 5, &[ca], None)
        .unwrap();
    assert_eq!(r, BoolOrInt::Bool(false));
    let r = o
        .x509_checkpurpose(CertArg::Str(&cert), 1, &[], None)
        .unwrap();
    assert_eq!(r, BoolOrInt::Bool(false));
    let r = o
        .x509_checkpurpose(CertArg::Str(b"x"), 1, &[], None)
        .unwrap();
    assert_eq!(r, BoolOrInt::Int(-1));
}

#[test]
fn export() {
    let mut o = OpenSsl::new();
    let data = read("cert.crt");
    let pem = o.x509_export(CertArg::Str(&data), true).unwrap().unwrap();
    assert!(pem.starts_with(b"-----BEGIN CERTIFICATE-----\n"));
    let text = o.x509_export(CertArg::Str(&data), false).unwrap().unwrap();
    assert!(text.starts_with(b"Certificate:\n    Data:\n"));
    assert!(text.ends_with(&pem));
    let path = tmp("x509.pem");
    assert!(o
        .x509_export_to_file(CertArg::Str(&data), path.as_bytes(), true)
        .unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), pem);
    assert!(!o
        .x509_export_to_file(CertArg::Str(&data), b"/nonexistent/dir/x", true)
        .unwrap());
    assert_eq!(warnings(&mut o), ["Error opening file /nonexistent/dir/x"]);
}
