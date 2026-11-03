//! SPKI and PKCS#12 (oracle: `/usr/bin/php`, php-src tests).

mod common;
use common::*;
use php_openssl::pkcs12::Pkcs12Options;
use php_openssl::{CertArg, KeyArg, OpenSsl, PhpValue};

#[test]
fn spki_roundtrip() {
    let mut o = OpenSsl::new();
    let url = fixture_url("private_rsa_1024.key");
    let k = o
        .pkey_get_private(&KeyArg::Str(&url), Some(b""))
        .unwrap()
        .unwrap();
    let s = o.spki_new(&k, b"chal", 2).unwrap().unwrap();
    assert_eq!(s.len(), 438);
    assert!(s.starts_with(b"SPKAC=MIIBPjCBqDCBnz"));
    // With the "SPKAC=" prefix the base64 decode fails, as in PHP.
    assert!(!o.spki_verify(&s));
    assert_eq!(warnings(&mut o), ["Unable to decode supplied SPKAC"]);
    assert!(o.spki_verify(&s[6..]));
    assert_eq!(o.spki_export_challenge(&s[6..]).unwrap(), b"chal");
    let pem = o.spki_export(&s[6..]).unwrap();
    assert!(pem.starts_with(b"-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDLXp6PkCtbpV+P1gwFQWH6Ez0U\n"));
    assert!(!o.spki_verify(b""));
    assert_eq!(warnings(&mut o), ["Invalid SPKAC"]);
    let _ = errors(&mut o);
    assert!(!o.spki_verify(b"x"));
    assert_eq!(
        errors(&mut o),
        ["error:05800076:x509 certificate routines::base64 decode error"]
    );
    assert_eq!(warnings(&mut o), ["Unable to decode supplied SPKAC"]);
    for ch in [
        &b""[..],
        b"challenge",
        b"\0challenge",
        b"challenge\0",
        b"left\0right",
    ] {
        let s = o.spki_new(&k, ch, 7).unwrap().unwrap();
        assert_eq!(o.spki_export_challenge(&s[6..]).unwrap(), ch);
    }
    assert!(o.spki_new(&k, b"x", 99).unwrap().is_none());
    assert_eq!(warnings(&mut o), ["Unknown digest algorithm"]);
}

#[test]
fn pkcs12_export_read_matches_php_test() {
    let mut o = OpenSsl::new();
    let cert = read("public.crt");
    let key = read("private.crt");
    let extra = read("cert.crt");
    let opts = Pkcs12Options {
        friendly_name: None,
        extracerts: Some((vec![CertArg::Str(&extra)], false)),
    };
    let p12 = o
        .pkcs12_export(CertArg::Str(&cert), &KeyArg::Str(&key), b"qwerty", &opts)
        .unwrap()
        .unwrap();
    assert!(o.pkcs12_read(b"", b"").is_none());
    assert!(o.pkcs12_read(&p12, b"").is_none());
    let _ = errors(&mut o);
    let a = o.pkcs12_read(&p12, b"qwerty").unwrap();
    let keys: Vec<_> = a.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys,
        ["cert", "pkey", "extracerts"].map(php_openssl::Key::from)
    );
    assert!(cert.ends_with(a.get_str("cert").unwrap().as_str().unwrap()));
    assert_eq!(a.get_str("cert").unwrap().as_str().unwrap().len(), 1111);
    assert_eq!(a.get_str("pkey").unwrap().as_str().unwrap().len(), 1704);
    let ex = a
        .get_str("extracerts")
        .and_then(PhpValue::as_array)
        .unwrap();
    assert_eq!(ex.get_index(0).unwrap().as_str().unwrap().len(), 1249);
    // Wrong key, file export.
    let other = fixture_url("private_rsa_1024.key");
    assert!(o
        .pkcs12_export(
            CertArg::Str(&cert),
            &KeyArg::Str(&other),
            b"x",
            &Pkcs12Options::default()
        )
        .unwrap()
        .is_none());
    assert_eq!(
        warnings(&mut o),
        ["Private key does not correspond to cert"]
    );
    let path = tmp("bundle.p12");
    let opts = Pkcs12Options {
        friendly_name: Some(b"friend"),
        extracerts: None,
    };
    assert!(o
        .pkcs12_export_to_file(
            CertArg::Str(&cert),
            path.as_bytes(),
            &KeyArg::Str(&key),
            b"pw",
            &opts
        )
        .unwrap());
    let a = o
        .pkcs12_read(&std::fs::read(&path).unwrap(), b"pw")
        .unwrap();
    assert!(a.get_str("extracerts").is_none());
}

#[test]
fn legacy_pfx_matches_php() {
    let mut o = OpenSsl::new();
    // bug74022.pfx uses RC2-40; PHP 8.3 on OpenSSL 3.0 reads it (the
    // config loads no legacy provider, so this documents the result).
    let r = o.pkcs12_read(&read("bug74022.pfx"), b"csos");
    assert!(r.is_some());
}
