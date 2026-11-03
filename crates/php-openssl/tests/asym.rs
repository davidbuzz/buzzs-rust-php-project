//! RSA primitives, signatures and envelopes (oracle: `/usr/bin/php`).

mod common;
use common::*;
use php_openssl::asym::DigestAlgo;
use php_openssl::{KeyArg, OpenSsl, Throw};

#[test]
fn sign_verify_matches_php() {
    let mut o = OpenSsl::new();
    let priv_url = fixture_url("private_rsa_1024.key");
    let pub_url = fixture_url("public.key");
    let (sk, pk) = (KeyArg::Str(&priv_url), KeyArg::Str(&pub_url));
    let s = o
        .sign(b"data", &sk, DigestAlgo::default(), 0, -2)
        .unwrap()
        .unwrap();
    assert_eq!(
        php_openssl::util::bin2hex(&s[..16]),
        b"4f93d2aac77d9a7cac9d78d795c9e9c1"
    );
    let s = o
        .sign(b"data", &sk, DigestAlgo::Name(b"sha256"), 0, -2)
        .unwrap()
        .unwrap();
    assert_eq!(
        php_openssl::util::bin2hex(&s[..16]),
        b"1e89f2a27963261217b15573b9255348"
    );
    assert_eq!(
        o.verify(b"data", &s, &pk, DigestAlgo::Id(7), 0, -2)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        o.verify(b"datx", &s, &pk, DigestAlgo::Name(b"sha256"), 0, -2)
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        o.sign(b"data", &sk, DigestAlgo::Name(b"nope"), 0, -2)
            .unwrap(),
        None
    );
    assert_eq!(warnings(&mut o), ["Unknown digest algorithm"]);
    assert_eq!(
        o.verify(b"data", b"xx", &pk, DigestAlgo::Name(b"sha256"), 0, -2)
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        errors(&mut o),
        [
            "error:02000077:rsa routines::wrong signature length",
            "error:1C880004:Provider routines::RSA lib"
        ]
    );
    // PSS round trip, and padding rules.
    let s = o
        .sign(b"data", &sk, DigestAlgo::Id(7), 6, -1)
        .unwrap()
        .unwrap();
    assert_eq!(
        o.verify(b"data", &s, &pk, DigestAlgo::Id(7), 6, -1)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        o.sign(b"data", &sk, DigestAlgo::Id(7), 99, -2).unwrap(),
        None
    );
    assert_eq!(warnings(&mut o), ["Unknown padding type"]);
    let ec = fixture_url("private_ec.key");
    assert_eq!(
        o.sign(b"data", &KeyArg::Str(&ec), DigestAlgo::Id(7), 6, -2)
            .unwrap(),
        None
    );
    assert_eq!(
        warnings(&mut o),
        ["Padding parameter is only supported for RSA keys"]
    );
    assert!(matches!(
        o.sign(b"data", &sk, DigestAlgo::Id(7), 0, 1 << 40),
        Err(Throw::ArgumentValueError { arg: 6, .. })
    ));
}

#[test]
fn ed25519_default_digest() {
    let mut o = OpenSsl::new();
    let mut opts = php_openssl::PhpArray::new();
    opts.set("private_key_type", php_openssl::PhpValue::Int(5));
    let k = o.pkey_new(Some(&opts)).unwrap().unwrap();
    let s = o
        .sign(b"msg", &KeyArg::Key(&k), DigestAlgo::Id(0), 0, -2)
        .unwrap()
        .unwrap();
    assert_eq!(s.len(), 64);
    let details = o.pkey_get_details(&k).unwrap();
    let pem = details.get_str("key").unwrap().as_str().unwrap().to_vec();
    assert_eq!(
        o.verify(b"msg", &s, &KeyArg::Str(&pem), DigestAlgo::Id(0), 0, -2)
            .unwrap(),
        Some(1)
    );
}

#[test]
fn rsa_primitives() {
    let mut o = OpenSsl::new();
    let priv_url = fixture_url("private_rsa_1024.key");
    let pub_url = fixture_url("public.key");
    let (sk, pk) = (KeyArg::Str(&priv_url), KeyArg::Str(&pub_url));
    let c = o.private_encrypt(b"hello", &sk, 1).unwrap().unwrap();
    assert_eq!(
        php_openssl::util::bin2hex(&c[..16]),
        b"0e6af1fed2e04ff3cd420dec3311ad03"
    );
    assert_eq!(o.public_decrypt(&c, &pk, 1).unwrap().unwrap(), b"hello");
    assert_eq!(o.private_encrypt(&[b'x'; 200], &sk, 1).unwrap(), None);
    assert_eq!(
        errors(&mut o),
        [
            "error:0200006E:rsa routines::data too large for key size",
            "error:1C880004:Provider routines::RSA lib"
        ]
    );
    assert_eq!(
        o.public_encrypt(b"x", &KeyArg::Str(b"nope"), 1, None)
            .unwrap(),
        None
    );
    assert_eq!(
        warnings(&mut o),
        ["key parameter is not a valid public key"]
    );
    for (pad, md) in [(4, None), (4, Some(&b"sha256"[..])), (1, None)] {
        let c = o.public_encrypt(b"hello", &pk, pad, md).unwrap().unwrap();
        assert_eq!(
            o.private_decrypt(&c, &sk, pad, md).unwrap().unwrap(),
            b"hello"
        );
    }
    assert_eq!(
        o.public_encrypt(b"hello", &pk, 4, Some(b"nope")).unwrap(),
        None
    );
    assert_eq!(warnings(&mut o), ["Unknown digest algorithm: nope"]);
}

#[test]
fn seal_open() {
    let mut o = OpenSsl::new();
    let priv_url = fixture_url("private_rsa_1024.key");
    let pub_url = fixture_url("public.key");
    let pk = KeyArg::Str(&pub_url);
    assert_eq!(
        o.seal(b"data", std::slice::from_ref(&pk), b"aes-128-cbc", false)
            .unwrap_err(),
        Throw::ArgumentValueError {
            arg: 6,
            message: "cannot be null for the chosen cipher algorithm".into()
        }
    );
    assert_eq!(
        o.seal(b"data", &[], b"aes-128-cbc", true).unwrap_err(),
        Throw::ArgumentMustNotBeEmpty { arg: 4 }
    );
    let s = o
        .seal(b"data", &[pk.clone(), pk.clone()], b"aes-128-cbc", true)
        .unwrap()
        .unwrap();
    assert_eq!(s.length, 16);
    let out = s.outputs.unwrap();
    assert_eq!(out.encrypted_keys.len(), 2);
    assert_eq!(out.encrypted_keys[0].len(), 128);
    let iv = out.iv.unwrap();
    assert_eq!(iv.len(), 16);
    let sk = KeyArg::Str(&priv_url);
    let plain = o
        .open(
            &out.sealed_data,
            &out.encrypted_keys[1],
            &sk,
            b"aes-128-cbc",
            Some(&iv),
        )
        .unwrap();
    assert_eq!(plain.as_deref(), Some(&b"data"[..]));
    assert_eq!(
        o.open(
            &out.sealed_data,
            &out.encrypted_keys[0],
            &sk,
            b"aes-128-cbc",
            Some(b"short")
        )
        .unwrap(),
        None
    );
    assert_eq!(warnings(&mut o), ["IV length is invalid"]);
    assert!(o
        .open(
            &out.sealed_data,
            &out.encrypted_keys[0],
            &sk,
            b"aes-128-cbc",
            None
        )
        .is_err());
    // ECB needs no IV.
    let s = o
        .seal(b"data", std::slice::from_ref(&pk), b"aes-128-ecb", false)
        .unwrap()
        .unwrap();
    let out = s.outputs.unwrap();
    assert!(out.iv.is_none());
    assert_eq!(
        o.open(
            &out.sealed_data,
            &out.encrypted_keys[0],
            &sk,
            b"aes-128-ecb",
            None
        )
        .unwrap()
        .unwrap(),
        b"data"
    );
    assert_eq!(
        o.seal(b"data", &[KeyArg::Str(b"x")], b"aes-128-ecb", false)
            .unwrap(),
        None
    );
    assert_eq!(
        warnings(&mut o),
        ["Not a public key (1th member of pubkeys)"]
    );
}
