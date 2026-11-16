// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of the ext/sodium `.phpt` scenarios (vectors, round trips and
//! error texts) onto the Rust API.

use php_sodium::{
    aead, boxes, hash, pwhash, ristretto, secretbox, secretstream, sign, util, Error,
};

fn hex(b: &[u8]) -> String {
    String::from_utf8(util::bin2hex(b).unwrap()).unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    util::hex2bin(s.as_bytes(), b"").unwrap()
}

#[test]
fn crypto_kx() {
    let ckp = boxes::kx_seed_keypair(&unhex(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ))
    .unwrap();
    let skp = boxes::kx_seed_keypair(&unhex(
        "f123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde0",
    ))
    .unwrap();
    assert_eq!(hex(&ckp), "b85c84f9828524519d32b97cd3dda961fdba2dbf407ae4601e2129229aa463c224eaf70f070a925d6d5176f20495d4d90867624d9a10379e2a9aef0955c9bf4e");
    assert_eq!(hex(&skp), "016e814c32b8b66225a403db45bf50fdd1966fb802c3115bf8aa90738c6a02de420ccdb534930fed9aaff12188bedc76e66251f399c404f2e4a15678fd4a484a");
    let c = boxes::kx_client_session_keys(&ckp, &boxes::kx_publickey(&skp).unwrap()).unwrap();
    let s = boxes::kx_server_session_keys(&skp, &boxes::kx_publickey(&ckp).unwrap()).unwrap();
    assert_eq!(
        hex(&c.0),
        "99a430e61d718b71979ebcea6735c4648bc828cfb456890aeda4b628b77d5ac7"
    );
    assert_eq!(c.0, s.1);
    assert_eq!(
        hex(&c.1),
        "876bef865a5ab3f4ae569ea5aaefe5014c3ec22a558c0a2f0274aa9985bd328d"
    );
    assert_eq!(c.1, s.0);
    // Random key pairs agree as well.
    let a = boxes::kx_keypair().unwrap();
    let b = boxes::kx_keypair().unwrap();
    let ca = boxes::kx_client_session_keys(&a, &b[32..]).unwrap();
    let sb = boxes::kx_server_session_keys(&b, &a[32..]).unwrap();
    assert_eq!((ca.0, ca.1), (sb.1, sb.0));
}

#[test]
fn crypto_scalarmult() {
    let n = unhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let p = unhex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    assert_eq!(
        hex(&boxes::scalarmult(&n, &p).unwrap()),
        "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"
    );
    assert!(boxes::scalarmult(&n[1..], &p).is_err());
}

#[test]
fn crypto_generichash() {
    let k16 = b"0123456789abcdef";
    let k32 = b"0123456789abcdef0123456789abcdef";
    let g = |m: &[u8], k: &[u8], l| hex(&hash::generichash(m, k, l).unwrap());
    assert_eq!(
        g(b"msg", b"", 32),
        "96a7ed8861db0abc006f473f9e64687875f3d9df8e723adae9f53a02b2aec378"
    );
    assert_eq!(
        g(b"msg", k16, 32),
        "ba03e32a94ece425a77b350f029e0a3d37e6383158aa7cefa2b1b9470a7fcb7a"
    );
    assert_eq!(g(b"msg", k16, 64), "8ccd640462e7380010c5722d7f3c2354781d1360430197ff233509c27353fd2597c8d689bfe769467056a0655b3faba6af4e4ade248558f7c53538c4d5b94806");
    assert_eq!(g(b"msg", k32, 64), "30f0e5f1e3beb7e0340976ac05a94043cce082d870e28e03c906e8fe9a88786271c6ba141eee2885e7444a870fac498cc78a13b0c53aefaec01bf38ebfe73b3f");
    let mut st = hash::generichash_init(b"", 32).unwrap();
    assert_eq!(
        hex(&hash::generichash_final(&mut st, 32).unwrap()),
        "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"
    );
    let mut st = hash::generichash_init(k16, 64).unwrap();
    hash::generichash_update(&mut st, b"m").unwrap();
    hash::generichash_update(&mut st, b"sg").unwrap();
    let mut st2 = st.clone();
    assert_eq!(
        hex(&hash::generichash_final(&mut st, 64).unwrap()),
        g(b"msg", k16, 64)
    );
    hash::generichash_update(&mut st2, b"2").unwrap();
    assert_eq!(
        hex(&hash::generichash_final(&mut st2, 64).unwrap()),
        "9ef702f51114c9dc2cc7521746e8beebe0a3ca9bb29ec729e16682ca982e7f69ff70235a46659a9a6c28f92fbd990288301b9a6b5517f1f2ba6518074af19a5a"
    );
    assert_eq!(
        hash::generichash(b"test", b"", 128),
        Err(Error::Sodium("unsupported output length".into()))
    );
}

#[test]
fn crypto_shorthash() {
    let s = |m: &[u8], k: &[u8]| hex(&hash::shorthash(m, k).unwrap());
    assert_eq!(s(b"message", b"0123456789ABCDEF"), "e0ad6fdbf8b9a191");
    assert_eq!(s(b"message", b"0123456789abcdef"), "c667b37af201a2d9");
    assert_eq!(s(b"msg", b"0123456789abcdef"), "d27fa3fc70b45b72");
    assert!(hash::shorthash(b"message", &[0; 32]).is_err());
}

#[test]
fn crypto_kdf() {
    let key = hash::kdf_keygen();
    for (len, id, ctx, k) in [
        (10, 0, &b"context!"[..], &key[..]),
        (100, 0, b"context!", &key),
        (64, 0, b"context", &key),
        (64, -1, b"context!", &key),
        (64, 0, b"context!", b"short key"),
    ] {
        assert!(matches!(
            hash::kdf_derive_from_key(len, id, ctx, k),
            Err(Error::Sodium(_))
        ));
    }
    let d = |id, ctx: &[u8]| hash::kdf_derive_from_key(16, id, ctx, &key).unwrap();
    assert_ne!(d(0, b"context!"), d(1, b"context!"));
    assert_ne!(d(0, b"context!"), d(2, b"context2"));
    assert_eq!(d(0, b"context!"), d(0, b"context!"));
}

#[test]
fn crypto_core_ristretto255() {
    for bad in [
        "00ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "0100000000000000000000000000000000000000000000000000000000000000",
        "26948d35ca62e643e26a83177332e6b6afeb9d08e4268b650f1f5bbd8d81d371",
        "3eb858e78f5a7254d8c9731174a94f76755fd3941c0ac93735c07ba14579630e",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ] {
        assert!(!ristretto::is_valid_point(&unhex(bad)).unwrap(), "{bad}");
    }
    let h = unhex("5d1be09e3d0c82fc538112490e35701979d99e06ca3e2b5b54bffe8b4dc772c14d98b696a1bbfb5ca32c436cc61c16563790306c79eaca7705668b47dffe5bb6");
    assert_eq!(
        hex(&ristretto::from_hash(&h).unwrap()),
        "3066f82a1a747d45120d1740f14358531a8f04bbffe6a819f86dfe50f44a0a46"
    );
    let r = ristretto::scalar_random();
    let s0 = ristretto::scalarmult_base(&r).unwrap();
    assert!(ristretto::is_valid_point(&s0).unwrap());
    let l = b"\xed\xd3\xf5\x5c\x1a\x63\x12\x58\xd6\x9c\xf7\xa2\xde\xf9\xde\x14\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x10";
    let s = ristretto::random();
    assert_eq!(
        ristretto::scalarmult(&s, l),
        Err(Error::Sodium("Result is identity element".into()))
    );
}

#[test]
fn crypto_scalarmult_ristretto255() {
    let b = unhex("e2f2ae0a6abc4e71a884a961c500515f58e30b6aa582dd8db6a65945e08d2d76");
    let mut n = vec![0u8; 32];
    assert_eq!(
        ristretto::scalarmult_base(&n).unwrap_err().message(),
        "sodium_crypto_scalarmult_ristretto255_base(): Argument #1 ($n) must not be zero"
    );
    assert_eq!(
        ristretto::scalarmult(&n, &b).unwrap_err().message(),
        "Result is identity element"
    );
    let expected = [
        "e2f2ae0a6abc4e71a884a961c500515f58e30b6aa582dd8db6a65945e08d2d76",
        "6a493210f7499cd17fecb510ae0cea23a110e8d5b901f8acadd3095c73a3b919",
        "94741f5d5d52755ece4f23f044ee27d5d1ea1e2bd196b462166b16152a9d0259",
        "da80862773358b466ffadfe0b3293ab3d9fd53c5ea6c955358f568322daf6a57",
        "e882b131016b52c1d3337080187cf768423efccbb517bb495ab812c4160ff44e",
        "f64746d3c92b13050ed8d80236a7f0007c3b3f962f5ba793d19a601ebb1df403",
        "44f53520926ec81fbd5a387845beb7df85a96a24ece18738bdcfa6a7822a176d",
        "903293d8f2287ebe10e2374dc1a53e0bc887e592699f02d077d5263cdd55601c",
        "02622ace8f7303a31cafc63f8fc48fdc16e1c8c8d234b2f0d6685282a9076031",
        "20706fd788b2720a1ed2a5dad4952b01f413bcf0e7564de8cdc816689e2db95f",
        "bce83f8ba5dd2fa572864c24ba1810f9522bc6004afe95877ac73241cafdab42",
        "e4549ee16b9aa03099ca208c67adafcafa4c3f3e4e5303de6026e3ca8ff84460",
        "aa52e000df2e16f55fb1032fc33bc42742dad6bd5a8fc0be0167436c5948501f",
        "46376b80f409b29dc2b5f6f0c52591990896e5716f41477cd30085ab7f10301e",
        "e0c418f7c8d9c4cdd7395b93ea124f3ad99021bb681dfc3302a9d99a2e53e64e",
    ];
    for want in expected {
        util::increment(&mut n);
        let p = ristretto::scalarmult_base(&n).unwrap();
        assert_eq!(hex(&p), want);
        assert_eq!(ristretto::scalarmult(&n, &b).unwrap(), p);
    }
    assert_eq!(
        boxes::scalarmult(&n[1..], &b).unwrap_err().message(),
        "sodium_crypto_scalarmult(): Argument #1 ($n) must be SODIUM_CRYPTO_SCALARMULT_SCALARBYTES bytes long"
    );
}

#[test]
fn crypto_secretstream() {
    let msg1 = b"first message".to_vec();
    let msg2 = vec![7u8; 999];
    let ad = b"associated".to_vec();
    let key = secretstream::keygen();
    let (mut st, header) = secretstream::init_push(&key).unwrap();
    let c1 = secretstream::push(&mut st, &msg1, &ad, 0).unwrap();
    let c2 = secretstream::push(&mut st, &msg2, b"", 3).unwrap();
    let mut st = secretstream::init_pull(&header, &key).unwrap();
    let r1 = secretstream::pull(&mut st, &c1, &ad).unwrap().unwrap();
    let r2 = secretstream::pull(&mut st, &c2, b"").unwrap().unwrap();
    assert_eq!((r1.0.as_slice(), r1.1), (msg1.as_slice(), 0));
    assert_eq!((r2.0.as_slice(), r2.1), (msg2.as_slice(), 3));
    assert_eq!(secretstream::pull(&mut st, &c2, b"").unwrap(), None);
    assert_eq!(secretstream::pull(&mut st, b"x", b"").unwrap(), None);
    let mut st = secretstream::init_pull(&header, &key).unwrap();
    assert_eq!(secretstream::pull(&mut st, &c1, b"").unwrap(), None);
    assert_eq!(
        secretstream::pull(&mut st, &c1, &ad).unwrap().unwrap().0,
        msg1
    );
    let mut st = secretstream::init_pull(&header, &key).unwrap();
    secretstream::rekey(&mut st).unwrap();
    assert_eq!(secretstream::pull(&mut st, &c1, &ad).unwrap(), None);
    assert_eq!(secretstream::pull(&mut st, &c2, b"").unwrap(), None);
}

#[test]
fn crypto_stream_xchacha20() {
    let nonce = vec![9u8; 24];
    let key = secretbox::stream_xchacha20_keygen();
    let s = secretbox::stream_xchacha20(100, &nonce, &key).unwrap();
    assert_eq!(s.len(), 100);
    assert_eq!(s, secretbox::stream_xchacha20(100, &nonce, &key).unwrap());
    let x = secretbox::stream_xchacha20_xor(&s, &nonce, &key).unwrap();
    assert_ne!(x, s);
    assert_eq!(
        secretbox::stream_xchacha20_xor(&x, &nonce, &key).unwrap(),
        s
    );
    let (left, right) = (vec![1u8; 64], vec![0xfeu8; 64]);
    let unified =
        secretbox::stream_xchacha20_xor(&[left.clone(), right.clone()].concat(), &nonce, &key)
            .unwrap();
    let l = secretbox::stream_xchacha20_xor_ic(&left, &nonce, 0, &key).unwrap();
    let r = secretbox::stream_xchacha20_xor_ic(&right, &nonce, 1, &key).unwrap();
    assert_eq!([l, r].concat(), unified);
    let msgs: Vec<String> = [
        secretbox::stream_xchacha20(-1, &nonce, &key).unwrap_err(),
        secretbox::stream_xchacha20(100, &nonce[1..], &key).unwrap_err(),
        secretbox::stream_xchacha20(100, &nonce, &key[1..]).unwrap_err(),
        secretbox::stream_xchacha20_xor(&s, &nonce[1..], &key).unwrap_err(),
        secretbox::stream_xchacha20_xor(&s, &nonce, &key[1..]).unwrap_err(),
    ]
    .iter()
    .map(|e| e.message().to_owned())
    .collect();
    assert_eq!(
        msgs,
        [
            "sodium_crypto_stream_xchacha20(): Argument #1 ($length) must be greater than 0",
            "sodium_crypto_stream_xchacha20(): Argument #2 ($nonce) must be SODIUM_CRYPTO_STREAM_XCHACHA20_NONCEBYTES bytes long",
            "sodium_crypto_stream_xchacha20(): Argument #3 ($key) must be SODIUM_CRYPTO_STREAM_XCHACHA20_KEYBYTES bytes long",
            "sodium_crypto_stream_xchacha20_xor(): Argument #2 ($nonce) must be SODIUM_CRYPTO_STREAM_XCHACHA20_NONCEBYTES bytes long",
            "sodium_crypto_stream_xchacha20_xor(): Argument #3 ($key) must be SODIUM_CRYPTO_STREAM_XCHACHA20_KEYBYTES bytes long",
        ]
    );
}

#[test]
fn random_key_round_trips() {
    // crypto_box.phpt / crypto_sign.phpt / crypto_secretbox.phpt / crypto_aead.phpt
    let msg = b"a secret message";
    let alice = boxes::box_keypair().unwrap();
    let bob = boxes::box_keypair().unwrap();
    let nonce = vec![3u8; 24];
    let a2b = boxes::box_keypair_from_secretkey_and_publickey(&alice[..32], &bob[32..]).unwrap();
    let b2a = boxes::box_keypair_from_secretkey_and_publickey(&bob[..32], &alice[32..]).unwrap();
    let ct = boxes::crypto_box(msg, &nonce, &a2b).unwrap();
    assert_eq!(boxes::box_open(&ct, &nonce, &b2a).unwrap().unwrap(), msg);
    let sealed = boxes::box_seal(msg, &boxes::box_publickey(&bob).unwrap()).unwrap();
    assert_eq!(boxes::box_seal_open(&sealed, &bob).unwrap().unwrap(), msg);
    assert_eq!(boxes::box_seal_open(&sealed, &alice).unwrap(), None);
    assert_eq!(
        boxes::box_publickey_from_secretkey(&boxes::box_secretkey(&alice).unwrap()).unwrap(),
        boxes::box_publickey(&alice).unwrap()
    );

    let kp = sign::sign_keypair().unwrap();
    let (sk, pk) = (
        sign::sign_secretkey(&kp).unwrap(),
        sign::sign_publickey(&kp).unwrap(),
    );
    let signed = sign::sign(msg, &sk).unwrap();
    assert_eq!(sign::sign_open(&signed, &pk).unwrap().unwrap(), msg);
    let sig = sign::sign_detached(msg, &sk).unwrap();
    assert!(sign::sign_verify_detached(&sig, msg, &pk).unwrap());
    assert!(!sign::sign_verify_detached(&sig, b"other", &pk).unwrap());
    assert_eq!(sign::sign_publickey_from_secretkey(&sk).unwrap(), pk);
    // Ed25519 → X25519: the converted keys form a valid box key pair.
    let xsk = sign::sign_ed25519_sk_to_curve25519(&sk).unwrap();
    let xpk = sign::sign_ed25519_pk_to_curve25519(&pk).unwrap();
    assert_eq!(boxes::box_publickey_from_secretkey(&xsk).unwrap(), xpk);
    assert_eq!(
        sign::sign_ed25519_pk_to_curve25519(&[0; 32]),
        Err(Error::Sodium("conversion failed".into()))
    );

    let key = secretbox::secretbox_keygen();
    let ct = secretbox::secretbox(msg, &nonce, &key).unwrap();
    assert_eq!(
        secretbox::secretbox_open(&ct, &nonce, &key)
            .unwrap()
            .unwrap(),
        msg
    );
    let mut bad = ct.clone();
    bad[0] ^= 1;
    assert_eq!(secretbox::secretbox_open(&bad, &nonce, &key).unwrap(), None);

    let key = aead::xchacha20poly1305_ietf_keygen();
    let ct = aead::xchacha20poly1305_ietf_encrypt(msg, b"ad", &nonce, &key).unwrap();
    assert_eq!(ct.len(), msg.len() + 16);
    assert_eq!(
        aead::xchacha20poly1305_ietf_decrypt(&ct, b"ad", &nonce, &key)
            .unwrap()
            .unwrap(),
        msg
    );
    if aead::aes256gcm_is_available() {
        let key = aead::aes256gcm_keygen();
        let ct = aead::aes256gcm_encrypt(msg, b"", &nonce[..12], &key).unwrap();
        assert_eq!(
            aead::aes256gcm_decrypt(&ct, b"", &nonce[..12], &key)
                .unwrap()
                .unwrap(),
            msg
        );
    }
}

#[test]
fn utils_base64_round_trips() {
    // utils.phpt: agrees with base64 for every length, with ignored chars.
    for len in 0..100usize {
        let bin: Vec<u8> = (0..len)
            .map(|i| u8::try_from((i * 37 + 11) % 256).unwrap())
            .collect();
        let b64 = util::bin2base64(&bin, 1).unwrap();
        assert_eq!(util::base642bin(&b64, 1, b"").unwrap(), bin);
        let padded = [&b" "[..], &b64, b"\n"].concat();
        assert_eq!(util::base642bin(&padded, 1, b" \n").unwrap(), bin);
        assert_eq!(
            util::hex2bin(&util::bin2hex(&bin).unwrap(), b"").unwrap(),
            bin
        );
    }
    assert_eq!(util::base642bin(b"YWJjZA", 3, b"").unwrap(), b"abcd");
}

#[test]
#[allow(clippy::many_single_char_names)]
fn pwhash_scenarios() {
    // pwhash_argon2i.phpt / pwhash_scrypt.phpt essentials, with small limits.
    let mut warnings = Vec::new();
    let mut w = |m: &str| warnings.push(m.to_owned());
    let salt = [5u8; 16];
    let a = pwhash::pwhash(32, b"pw", &salt, 3, 8192, 1, &mut w).unwrap();
    let b = pwhash::pwhash(32, b"pw", &salt, 3, 8192, 1, &mut w).unwrap();
    assert_eq!(a, b);
    let h = pwhash::pwhash_str(b"pw", 2, 8192, &mut w).unwrap();
    assert!(h.starts_with(b"$argon2id$v=19$m=8,t=2,p=1$"));
    assert!(pwhash::pwhash_str_verify(&h, b"pw", &mut w).unwrap());
    assert!(!pwhash::pwhash_str_needs_rehash(&h, 2, 8192));
    assert!(pwhash::pwhash_str_needs_rehash(&h, 3, 8192));
    assert!(warnings.is_empty());
    // pwhash_memlimit_below_min.phpt (master: ValueError)
    let e = pwhash::pwhash(32, b"password", &salt, 2, 1, 2, &mut |_| {}).unwrap_err();
    assert_eq!(e.class_name(), "ValueError");
    assert_eq!(
        e.message(),
        "sodium_crypto_pwhash(): Argument #5 ($memlimit) must be greater than or equal to 8192"
    );
    let e = pwhash::pwhash_str(b"password", 2, 1, &mut |_| {}).unwrap_err();
    assert_eq!(
        e.message(),
        "sodium_crypto_pwhash_str(): Argument #3 ($memlimit) must be greater than or equal to 8192"
    );
}

#[test]
fn password_hash_backend() {
    // php_password_hash_argon2i.phpt / bug78516.phpt, with modest costs.
    let opts = pwhash::Argon2Options {
        memory_cost: Some(8191),
        time_cost: Some(3),
        threads: Some(1),
    };
    let h = pwhash::argon2_hash(b"secret", &opts, pwhash::Argon2::I).unwrap();
    assert!(
        h.starts_with(b"$argon2i$v=19$m=8191,t=3,p=1$"),
        "{}",
        String::from_utf8_lossy(&h)
    );
    assert!(pwhash::argon2_verify(b"secret", &h));
    assert!(!pwhash::argon2_verify(b"Secret", &h));
    assert!(!pwhash::argon2_needs_rehash(&h, &opts));
    assert!(pwhash::argon2_needs_rehash(
        &h,
        &pwhash::Argon2Options::default()
    ));
    let info = pwhash::argon2_get_info(&h).unwrap();
    assert_eq!(
        (info.memory_cost, info.time_cost, info.threads),
        (8191, 3, 1)
    );
    let h = pwhash::argon2_hash(b"secret", &opts, pwhash::Argon2::Id).unwrap();
    assert!(h.starts_with(b"$argon2id$v=19$m=8191,t=3,p=1$"));
    let err =
        |o: pwhash::Argon2Options| pwhash::argon2_hash(b"x", &o, pwhash::Argon2::Id).unwrap_err();
    assert_eq!(
        err(pwhash::Argon2Options {
            memory_cost: Some(7),
            ..Default::default()
        }),
        Error::Value("Memory cost is outside of allowed memory range".into())
    );
    assert_eq!(
        err(pwhash::Argon2Options {
            time_cost: Some(0),
            ..Default::default()
        }),
        Error::Value("Time cost is outside of allowed time range".into())
    );
    assert_eq!(
        err(pwhash::Argon2Options {
            threads: Some(2),
            ..Default::default()
        }),
        Error::Value("A thread value other than 1 is not supported by this implementation".into())
    );
    assert_eq!(pwhash::argon2_get_info(b"$2y$10$abc"), None);
    assert_eq!(pwhash::argon2_get_info(b"$argon2i$"), None);
}
