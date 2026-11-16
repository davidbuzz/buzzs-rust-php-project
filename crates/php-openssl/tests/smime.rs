// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PKCS#7 and CMS S/MIME functions (php-src test expectations).

#![allow(clippy::too_many_lines)]

mod common;
use common::*;
use php_openssl::cms::CipherAlgo;
use php_openssl::pkcs7::{Header, Recipients};
use php_openssl::{BoolOrInt, CertArg, KeyArg, OpenSsl};

fn hdrs() -> Vec<Header> {
    vec![
        Header {
            name: Some(b"To".to_vec()),
            value: b"test@test".to_vec(),
        },
        Header {
            name: None,
            value: b"Subject: hi".to_vec(),
        },
    ]
}

#[test]
fn pkcs7_verify_signed_eml() {
    let mut o = OpenSsl::new();
    let eml = fixture("signed.eml").into_bytes();
    assert_eq!(
        o.pkcs7_verify(b"wrong", 0, None, &[], None, None, None)
            .unwrap(),
        BoolOrInt::Int(-1)
    );
    assert_eq!(
        o.pkcs7_verify(b"", 0, None, &[], None, None, None).unwrap(),
        BoolOrInt::Int(-1)
    );
    assert_eq!(
        o.pkcs7_verify(&eml, 0, None, &[], None, None, None)
            .unwrap(),
        BoolOrInt::Bool(false)
    );
    assert_eq!(
        o.pkcs7_verify(&eml, 0, Some(b""), &[], None, None, None)
            .unwrap(),
        BoolOrInt::Bool(false)
    );
    let out = tmp("signers.pem");
    let content = tmp("content.txt");
    let p7 = tmp("p7.pem");
    assert_eq!(
        o.pkcs7_verify(&eml, 0x20, Some(out.as_bytes()), &[], None, None, None)
            .unwrap(),
        BoolOrInt::Bool(true)
    );
    assert!(std::fs::read(&out)
        .unwrap()
        .starts_with(b"-----BEGIN CERTIFICATE-----"));
    assert_eq!(
        o.pkcs7_verify(
            &eml,
            0x20,
            Some(out.as_bytes()),
            &[],
            Some(out.as_bytes()),
            Some(content.as_bytes()),
            Some(p7.as_bytes())
        )
        .unwrap(),
        BoolOrInt::Bool(true)
    );
    let pem = std::fs::read(&p7).unwrap();
    assert!(pem.starts_with(b"-----BEGIN PKCS7-----\nMIIFzQYJKoZIhvcNAQcCoIIFvjCCBboCAQExDzANBglghkgBZQMEAgEFADALBgkq\n"));
    assert!(pem.ends_with(b"5Q==\n-----END PKCS7-----\n"));
    assert!(!std::fs::read(&content).unwrap().is_empty());
}

#[test]
fn pkcs7_read() {
    let mut o = OpenSsl::new();
    assert!(o.pkcs7_read(b"").is_none());
    assert!(o.pkcs7_read(&read("cert.crt")).is_none());
    let certs = o.pkcs7_read(&read("cert.p7b")).unwrap();
    assert_eq!(certs.len(), 1);
    assert_eq!(certs[0].len(), 1249);
}

#[test]
fn pkcs7_sign_encrypt_roundtrip() {
    let mut o = OpenSsl::new();
    let plain = fixture("plain.txt").into_bytes();
    let cert = read("cert.crt");
    let key = fixture_url("private_rsa_1024.key");
    let signed = tmp("p7signed.eml");
    assert!(o
        .pkcs7_sign(
            &plain,
            signed.as_bytes(),
            CertArg::Str(&cert),
            &KeyArg::Str(&key),
            Some(&hdrs()),
            0x40,
            None
        )
        .unwrap());
    let text = std::fs::read(&signed).unwrap();
    assert!(text.starts_with(b"To: test@test\nSubject: hi\nMIME-Version: 1.0\n"));
    assert_eq!(
        o.pkcs7_verify(signed.as_bytes(), 0x20, None, &[], None, None, None)
            .unwrap(),
        BoolOrInt::Bool(true)
    );
    let enc = tmp("p7enc.eml");
    assert!(o
        .pkcs7_encrypt(
            &plain,
            enc.as_bytes(),
            &Recipients::One(CertArg::Str(&cert)),
            Some(&hdrs()),
            0,
            5
        )
        .unwrap());
    let dec = tmp("p7dec.txt");
    assert!(o
        .pkcs7_decrypt(
            enc.as_bytes(),
            dec.as_bytes(),
            CertArg::Str(&cert),
            Some(&KeyArg::Str(&key))
        )
        .unwrap());
    assert_eq!(
        std::fs::read(&dec).unwrap(),
        b"Now is the winter of our discontent.\r\n"
    );
    // Errors.
    assert!(!o
        .pkcs7_sign(
            b"/nonexistent",
            signed.as_bytes(),
            CertArg::Str(&cert),
            &KeyArg::Str(&key),
            None,
            0,
            None
        )
        .unwrap());
    assert_eq!(warnings(&mut o), ["Error opening input file /nonexistent!"]);
    assert!(!o
        .pkcs7_encrypt(
            &plain,
            enc.as_bytes(),
            &Recipients::One(CertArg::Str(&cert)),
            None,
            0,
            99
        )
        .unwrap());
    assert_eq!(warnings(&mut o), ["Failed to get cipher"]);
    assert!(!o
        .pkcs7_decrypt(enc.as_bytes(), dec.as_bytes(), CertArg::Str(b"x"), None)
        .unwrap());
    assert_eq!(warnings(&mut o), ["X.509 Certificate cannot be retrieved"]);
}

#[test]
fn cms_roundtrips_all_encodings() {
    let mut o = OpenSsl::new();
    let plain = fixture("plain.txt").into_bytes();
    let cert = read("cert.crt");
    let key = fixture_url("private_rsa_1024.key");
    for enc in [0, 1, 2] {
        let out = tmp(&format!("cms_enc_{enc}"));
        assert!(o
            .cms_encrypt(
                &plain,
                out.as_bytes(),
                &Recipients::Many(vec![CertArg::Str(&cert)]),
                Some(&hdrs()),
                0,
                enc,
                CipherAlgo::default()
            )
            .unwrap());
        let dec = tmp(&format!("cms_dec_{enc}"));
        assert!(
            o.cms_decrypt(
                out.as_bytes(),
                dec.as_bytes(),
                CertArg::Str(&cert),
                Some(&KeyArg::Str(&key)),
                enc
            )
            .unwrap(),
            "{enc}"
        );
        // Text mode canonicalises line endings to CRLF (as in PHP).
        assert!(std::fs::read(&dec).unwrap().ends_with(b"discontent.\r\n"));

        let signed = tmp(&format!("cms_signed_{enc}"));
        assert!(o
            .cms_sign(
                &plain,
                signed.as_bytes(),
                CertArg::Str(&cert),
                &KeyArg::Str(&key),
                Some(&hdrs()),
                0,
                enc,
                None
            )
            .unwrap());
        assert!(
            o.cms_verify(
                signed.as_bytes(),
                0x20,
                None,
                &[],
                None,
                None,
                None,
                None,
                enc
            )
            .unwrap(),
            "{enc}"
        );
    }
    // Detached PEM signature verified against the content file.
    let sig = tmp("cms_detached.pem");
    assert!(o
        .cms_sign(
            &plain,
            sig.as_bytes(),
            CertArg::Str(&cert),
            &KeyArg::Str(&key),
            None,
            0x40,
            2,
            None
        )
        .unwrap());
    let signers = tmp("cms_signers.pem");
    assert!(o
        .cms_verify(
            &plain,
            0x60,
            Some(signers.as_bytes()),
            &[],
            None,
            None,
            None,
            Some(sig.as_bytes()),
            2
        )
        .unwrap());
    assert!(std::fs::read(&signers)
        .unwrap()
        .starts_with(b"-----BEGIN CERTIFICATE-----"));
    // Error paths.
    assert!(!o
        .cms_sign(
            &plain,
            sig.as_bytes(),
            CertArg::Str(&cert),
            &KeyArg::Str(&key),
            None,
            0x40,
            1,
            None
        )
        .unwrap());
    assert_eq!(
        warnings(&mut o),
        ["Detached signatures not possible with S/MIME encoding"]
    );
    assert!(o
        .cms_decrypt(
            &plain,
            sig.as_bytes(),
            CertArg::Str(&cert),
            Some(&KeyArg::Str(&key)),
            9
        )
        .is_err());
    let out = tmp("cms_bad");
    assert!(!o
        .cms_encrypt(
            &plain,
            out.as_bytes(),
            &Recipients::One(CertArg::Str(&cert)),
            None,
            0,
            7,
            CipherAlgo::default()
        )
        .unwrap());
    assert_eq!(warnings(&mut o), ["Unknown OPENSSL encoding"]);
    assert!(!o
        .cms_encrypt(
            &plain,
            out.as_bytes(),
            &Recipients::One(CertArg::Str(&cert)),
            None,
            0,
            1,
            CipherAlgo::Name(b"nope")
        )
        .unwrap());
    assert_eq!(warnings(&mut o), ["Failed to get cipher"]);
}

#[test]
fn cms_read_pem() {
    let mut o = OpenSsl::new();
    let plain = fixture("plain.txt").into_bytes();
    let cert = read("cert.crt");
    let key = fixture_url("private_rsa_1024.key");
    let out = tmp("cms_read.pem");
    assert!(o
        .cms_sign(
            &plain,
            out.as_bytes(),
            CertArg::Str(&cert),
            &KeyArg::Str(&key),
            None,
            0,
            2,
            None
        )
        .unwrap());
    let certs = o.cms_read(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(certs.len(), 1);
    assert_eq!(certs[0].len(), 1249);
    assert!(o.cms_read(b"").is_none());
}
