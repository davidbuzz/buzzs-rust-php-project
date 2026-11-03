//! Oracle-checked symmetric crypto (expected values produced by
//! `/usr/bin/php` 8.3 linked against the same OpenSSL 3.0).

use php_openssl::symmetric::{DecryptArgs, EncryptArgs, TagOut};
use php_openssl::{Level, OpenSsl, Throw};

fn enc<'a>(
    data: &'a [u8],
    method: &'a [u8],
    key: &'a [u8],
    options: i64,
    iv: &'a [u8],
) -> EncryptArgs<'a> {
    EncryptArgs {
        data,
        method,
        passphrase: key,
        options,
        iv,
        tag_requested: false,
        aad: Some(b""),
        tag_length: 16,
    }
}

fn warnings(o: &mut OpenSsl) -> Vec<String> {
    o.take_diagnostics()
        .into_iter()
        .filter(|d| d.level == Level::Warning)
        .map(|d| d.message)
        .collect()
}

#[test]
fn cbc_with_zero_iv() {
    let mut o = OpenSsl::new();
    let r = o
        .encrypt(&enc(
            b"openssl_encrypt() tests",
            b"AES-128-CBC",
            b"openssl",
            0,
            &[0; 16],
        ))
        .unwrap()
        .unwrap();
    assert_eq!(r.data, b"iPR4HulskuaP5Z6me5uImk6BqVyJG73+63tkPauVZYk=");
    assert_eq!(r.tag, TagOut::Untouched);
    // Empty IV: same result plus a warning.
    let r2 = o
        .encrypt(&enc(
            b"openssl_encrypt() tests",
            b"AES-128-CBC",
            b"openssl",
            0,
            b"",
        ))
        .unwrap()
        .unwrap();
    assert_eq!(r2.data, r.data);
    assert_eq!(
        warnings(&mut o),
        ["Using an empty Initialization Vector (iv) is potentially insecure and not recommended"]
    );
    let back = o
        .decrypt(&DecryptArgs {
            data: &r.data,
            method: b"AES-128-CBC",
            passphrase: b"openssl",
            options: 0,
            iv: &[0; 16],
            tag: None,
            aad: Some(b""),
        })
        .unwrap()
        .unwrap();
    assert_eq!(back, b"openssl_encrypt() tests");
}

#[test]
fn iv_padding_and_truncation_warnings() {
    let mut o = OpenSsl::new();
    let r = o
        .encrypt(&enc(b"data", b"aes-128-cbc", b"password", 0, b"short"))
        .unwrap()
        .unwrap();
    assert_eq!(r.data, b"83J7bySs0CB8+HLI+YO8hQ==");
    assert_eq!(
        warnings(&mut o),
        ["IV passed is only 5 bytes long, cipher expects an IV of precisely 16 bytes, padding with \\0"]
    );
    let r = o
        .encrypt(&enc(b"data", b"aes-128-cbc", b"password", 0, &[b'y'; 20]))
        .unwrap()
        .unwrap();
    assert_eq!(r.data, b"9njereclYOu6oW5zkqnxtw==");
    assert_eq!(
        warnings(&mut o),
        ["IV passed is 20 bytes long which is longer than the 16 expected by selected cipher, truncating"]
    );
}

type Case<'a> = (&'a [u8], &'a [u8], &'a [u8], i64, &'a str, &'a str);

#[test]
fn aead_modes_match_php() {
    let mut o = OpenSsl::new();
    let cases: &[Case<'_>] = &[
        (
            b"aes-256-gcm",
            &[b'x'; 12],
            b"aad",
            16,
            "i88jKA==",
            "9819579c36747d4bb753172726206d88",
        ),
        (
            b"aes-128-ccm",
            &[b'x'; 10],
            b"",
            14,
            "PIkv6g==",
            "d1ad8fb9be8d06ffa22d472164a6",
        ),
        (
            b"chacha20-poly1305",
            &[b'x'; 12],
            b"",
            16,
            "ZrlTew==",
            "26c2a977ca141ccd0740e0753d54defe",
        ),
        (
            b"aes-128-ocb",
            &[b'x'; 12],
            b"a",
            12,
            "eQjIXQ==",
            "7077100d00eb04220d32c6c8",
        ),
    ];
    for &(method, iv, aad, tag_len, ct, tag_hex) in cases {
        let mut a = enc(b"data", method, b"password", 0, iv);
        a.tag_requested = true;
        a.aad = Some(aad);
        a.tag_length = tag_len;
        let r = o.encrypt(&a).unwrap().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&r.data),
            ct,
            "{}",
            String::from_utf8_lossy(method)
        );
        let TagOut::Tag(tag) = r.tag else {
            panic!("no tag")
        };
        assert_eq!(php_openssl::util::bin2hex(&tag), tag_hex.as_bytes());
        let back = o
            .decrypt(&DecryptArgs {
                data: ct.as_bytes(),
                method,
                passphrase: b"password",
                options: 0,
                iv,
                tag: Some(&tag),
                aad: Some(aad),
            })
            .unwrap();
        assert_eq!(back.as_deref(), Some(&b"data"[..]));
        // Wrong tag fails without a warning.
        let mut bad = tag.clone();
        bad[0] ^= 1;
        let back = o
            .decrypt(&DecryptArgs {
                data: ct.as_bytes(),
                method,
                passphrase: b"password",
                options: 0,
                iv,
                tag: Some(&bad),
                aad: Some(aad),
            })
            .unwrap();
        assert_eq!(back, None);
        assert!(warnings(&mut o).is_empty());
    }
}

#[test]
fn aead_without_tag_and_bad_iv() {
    let mut o = OpenSsl::new();
    assert!(o
        .encrypt(&enc(b"data", b"aes-128-gcm", b"password", 0, &[b'x'; 12]))
        .unwrap()
        .is_none());
    assert_eq!(
        warnings(&mut o),
        ["A tag should be provided when using AEAD mode"]
    );
    let mut a = enc(b"data", b"aes-256-ccm", b"password", 0, b"");
    a.tag_requested = true;
    assert!(o.encrypt(&a).unwrap().is_none());
    assert_eq!(
        warnings(&mut o),
        ["Setting of IV length for AEAD mode failed"]
    );
    let mut a = enc(b"data", b"aes-256-ccm", b"password", 0, &[b'x'; 16]);
    a.tag_requested = true;
    a.tag_length = 1024;
    assert!(o.encrypt(&a).unwrap().is_none());
    assert_eq!(
        warnings(&mut o),
        ["Setting of IV length for AEAD mode failed"]
    );
}

#[test]
fn unknown_cipher_and_key_length_errors() {
    let mut o = OpenSsl::new();
    assert!(o
        .encrypt(&enc(b"x", b"wrong", b"k", 0, b""))
        .unwrap()
        .is_none());
    assert_eq!(warnings(&mut o), ["Unknown cipher algorithm"]);
    let r = o
        .encrypt(&enc(
            b"openssl_encrypt() tests",
            b"AES-128-CBC",
            b"openssl",
            4,
            &[0; 16],
        ))
        .unwrap();
    assert!(r.is_none());
    assert_eq!(
        warnings(&mut o),
        ["Key length cannot be set for the cipher algorithm"]
    );
    assert_eq!(
        o.decrypt(&DecryptArgs {
            data: b"x",
            method: b"",
            passphrase: b"",
            options: 0,
            iv: b"",
            tag: None,
            aad: None
        }),
        Err(Throw::ArgumentMustNotBeEmpty { arg: 2 })
    );
}

#[test]
fn decrypt_failure_fills_error_queue() {
    let mut o = OpenSsl::new();
    let r = o
        .decrypt(&DecryptArgs {
            data: b"garbage",
            method: b"aes-128-cbc",
            passphrase: b"password",
            options: 0,
            iv: &[b'y'; 16],
            tag: None,
            aad: Some(b""),
        })
        .unwrap();
    assert_eq!(r, None);
    assert_eq!(
        o.error_string().as_deref(),
        Some("error:1C80006B:Provider routines::wrong final block length")
    );
    assert_eq!(o.error_string(), None);
}

#[test]
fn legacy_cipher_is_unsupported() {
    let mut o = OpenSsl::new();
    let r = o
        .encrypt(&enc(b"data", b"bf-cbc", b"password", 0, &[b'y'; 8]))
        .unwrap();
    assert_eq!(r, None);
    assert_eq!(
        o.error_string().as_deref(),
        Some("error:0308010C:digital envelope routines::unsupported")
    );
}

#[test]
fn lengths() {
    let mut o = OpenSsl::new();
    assert_eq!(o.cipher_iv_length(b"aes-128-gcm").unwrap(), Some(12));
    assert_eq!(o.cipher_key_length(b"chacha20-poly1305").unwrap(), Some(32));
    assert_eq!(o.cipher_iv_length(b"nope").unwrap(), None);
    assert_eq!(warnings(&mut o), ["Unknown cipher algorithm"]);
    assert_eq!(
        o.cipher_iv_length(b""),
        Err(Throw::ArgumentMustNotBeEmpty { arg: 1 })
    );
}

#[test]
fn pbkdf2_and_digest() {
    let mut o = OpenSsl::new();
    let k = o
        .pbkdf2(b"password", b"salt", 20, 1000, b"sha256")
        .unwrap()
        .unwrap();
    assert_eq!(
        php_openssl::util::bin2hex(&k),
        b"632c2812e46d4604102ba7618e9d6d7d2f8128f6"
    );
    assert!(matches!(
        o.pbkdf2(b"p", b"s", 0, 1, b""),
        Err(Throw::ArgumentValueError { arg: 3, .. })
    ));
    assert_eq!(o.pbkdf2(b"p", b"s", 1, 1, b"nope").unwrap(), None);
    assert_eq!(warnings(&mut o), ["Unknown digest algorithm"]);
    assert_eq!(
        o.digest(b"abc", b"sha256", false).unwrap(),
        b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(o.digest(b"abc", b"md5", true).unwrap().len(), 16);
    assert_eq!(o.digest(b"abc", b"whirlpool", false), None);
}

#[test]
fn random_bytes() {
    let mut o = OpenSsl::new();
    let (b, strong) = o.random_pseudo_bytes(10).unwrap();
    assert_eq!(b.len(), 10);
    assert!(strong);
    assert_eq!(
        o.random_pseudo_bytes(0),
        Err(Throw::ArgumentValueError {
            arg: 1,
            message: "must be greater than 0".into()
        })
    );
}

#[test]
fn listings() {
    let mut o = OpenSsl::new();
    let c = o.get_cipher_methods(false);
    assert!(c.contains(&"aes-128-cbc".to_string()));
    assert!(c.windows(2).all(|w| w[0] <= w[1]));
    assert!(o.get_cipher_methods(true).len() >= c.len());
    let m = o.get_md_methods(false);
    assert!(m.iter().any(|x| x == "sha256"));
    assert!(o.get_md_methods(true).len() > m.len());
    assert!(o.get_curve_names().contains(&"prime256v1".to_string()));
    let loc = o.get_cert_locations();
    assert_eq!(loc.len(), 8);
}
