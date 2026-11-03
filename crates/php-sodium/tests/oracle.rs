//! Replays `examples/sodium_oracle.php` through this crate and compares
//! every line with the output recorded from `/usr/bin/php` 8.3.6 (bundled
//! with libsodium 1.0.18) in `data/oracle.txt`.  Where php-src master
//! changed behaviour (the pwhash family throws `ValueError` and checks the
//! minimum limits itself), the expectation is adjusted to master.

use php_sodium::{
    aead, boxes, hash, pwhash, ristretto, secretbox, secretstream, sign, util, Error,
};
use std::cell::RefCell;

#[derive(Debug)]
enum V {
    B(Vec<u8>),
    Bool(bool),
    Int(i64),
    Null,
    Arr(Vec<V>),
}

impl V {
    fn show(&self) -> String {
        match self {
            V::B(b) => b.iter().fold(String::new(), |mut s, x| {
                use std::fmt::Write;
                let _ = write!(s, "{x:02x}");
                s
            }),
            V::Bool(b) => b.to_string(),
            V::Int(i) => format!("int({i})"),
            V::Null => "null".into(),
            V::Arr(a) => format!("[{}]", a.iter().map(V::show).collect::<Vec<_>>().join(",")),
        }
    }
}

impl From<Vec<u8>> for V {
    fn from(v: Vec<u8>) -> Self {
        V::B(v)
    }
}
impl From<&[u8]> for V {
    fn from(v: &[u8]) -> Self {
        V::B(v.to_vec())
    }
}
impl From<bool> for V {
    fn from(v: bool) -> Self {
        V::Bool(v)
    }
}
impl From<i64> for V {
    fn from(v: i64) -> Self {
        V::Int(v)
    }
}
impl From<usize> for V {
    fn from(v: usize) -> Self {
        V::Int(i64::try_from(v).unwrap())
    }
}
impl From<Option<Vec<u8>>> for V {
    fn from(v: Option<Vec<u8>>) -> Self {
        v.map_or(V::Bool(false), V::B)
    }
}
impl From<(Vec<u8>, Vec<u8>)> for V {
    fn from((a, b): (Vec<u8>, Vec<u8>)) -> Self {
        V::Arr(vec![V::B(a), V::B(b)])
    }
}
impl From<Vec<V>> for V {
    fn from(v: Vec<V>) -> Self {
        V::Arr(v)
    }
}

thread_local! {
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn warn(msg: &str) {
    WARNINGS.with(|w| w.borrow_mut().push(msg.to_owned()));
}

struct Out(Vec<String>);

impl Out {
    fn t<T: Into<V>>(&mut self, label: &str, f: impl FnOnce() -> Result<T, Error>) {
        WARNINGS.with(|w| w.borrow_mut().clear());
        let mut line = match f() {
            Ok(v) => format!("{label}: {}", v.into().show()),
            Err(e) => format!("{label}: EXC {}: {}", e.class_name(), e.message()),
        };
        WARNINGS.with(|w| {
            for m in w.borrow().iter() {
                line.push_str(" [W: ");
                line.push_str(m);
                line.push(']');
            }
        });
        self.0.push(line);
    }
}

#[allow(clippy::unnecessary_wraps)]
fn ok<T>(v: T) -> Result<T, Error> {
    Ok(v)
}

/// The recorded 8.3 line, adjusted to php-src master where it differs.
fn master(line: &str) -> String {
    let (label, _) = line.split_once(": ").unwrap();
    let overrides: &[(&str, &str)] = &[
        ("pwhash_lenbig", "EXC ValueError: sodium_crypto_pwhash(): Argument #1 ($length) must be less than 4294967295 bytes"),
        ("pwhash_mem_min", "EXC ValueError: sodium_crypto_pwhash(): Argument #5 ($memlimit) must be greater than or equal to 8192"),
        ("pwhash_str_mem_min", "EXC ValueError: sodium_crypto_pwhash_str(): Argument #3 ($memlimit) must be greater than or equal to 8192"),
        ("scrypt_ops_min", "EXC ValueError: sodium_crypto_pwhash_scryptsalsa208sha256(): Argument #4 ($opslimit) must be greater than or equal to 524288"),
        // 8.3 named the wrong constants in these two messages.
        ("kdf_badkey", "EXC SodiumException: sodium_crypto_kdf_derive_from_key(): Argument #4 ($key) must be SODIUM_CRYPTO_KDF_KEYBYTES bytes long"),
        ("scalarmult_badp", "EXC SodiumException: sodium_crypto_scalarmult(): Argument #2 ($p) must be SODIUM_CRYPTO_SCALARMULT_BYTES bytes long"),
        ("scrypt_mem_min", "EXC ValueError: sodium_crypto_pwhash_scryptsalsa208sha256(): Argument #5 ($memlimit) must be greater than or equal to 16777216"),
    ];
    if let Some((_, v)) = overrides.iter().find(|(l, _)| *l == label) {
        return format!("{label}: {v}");
    }
    if (label.starts_with("pwhash") || label.starts_with("scrypt"))
        && line.contains("EXC SodiumException: sodium_crypto_pwhash")
        && line.contains("Argument #")
    {
        return line.replacen("EXC SodiumException", "EXC ValueError", 1);
    }
    // 8.3 misnamed `$string2` in the length-mismatch messages.
    line.replace("($string_2)", "($string2)")
}

#[test]
#[allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::type_complexity,
    clippy::unnecessary_wraps
)]
fn matches_php() {
    let k32: Vec<u8> = (0u8..32).collect();
    let k64: Vec<u8> = k32.iter().chain(k32.iter().rev()).copied().collect();
    let k16 = k32[..16].to_vec();
    let rev32: Vec<u8> = k32.iter().rev().copied().collect();
    let n24 = vec![0x24u8; 24];
    let n12 = vec![0x12u8; 12];
    let n8 = vec![0x08u8; 8];
    let msg: &[u8] = b"The quick brown fox jumps over the lazy dog";
    let ad: &[u8] = b"additional";
    let mut o = Out(Vec::new());

    // utils
    o.t("bin2hex", || util::bin2hex(b"\x00\x01\xfe\xff"));
    o.t("hex2bin", || util::hex2bin(b"00:01:fe:ff", b":"));
    o.t("hex2bin_bad", || util::hex2bin(b"0g", b""));
    o.t("hex2bin_odd", || util::hex2bin(b"abc", b""));
    o.t("hex2bin_ignore_nul", || util::hex2bin(b"ab cd", b" \0x"));
    for v in [1, 3, 5, 7] {
        let long = [&b"\xfb\xff\xfe"[..], msg].concat();
        o.t(&format!("bin2base64_{v}"), || util::bin2base64(&long, v));
        o.t(&format!("bin2base64_short_{v}"), || {
            util::bin2base64(b"\xfb\xff", v)
        });
        let src = [&b"\xfb\xff"[..], msg].concat();
        o.t(&format!("base642bin_{v}"), || {
            util::base642bin(&util::bin2base64(&src, v)?, v, b"")
        });
    }
    o.t("bin2base64_badvariant", || util::bin2base64(b"x", 2));
    o.t("base642bin_badvariant", || util::base642bin(b"x", 0, b""));
    o.t("base642bin_bad", || util::base642bin(b"Zm9v!", 1, b""));
    o.t("base642bin_ignore", || {
        util::base642bin(b" Zm9v\nYmFy ", 1, b" \n")
    });
    o.t("base642bin_nopad_given_pad", || {
        util::base642bin(b"Zm8=", 3, b"")
    });
    o.t("base642bin_pad_missing", || {
        util::base642bin(b"Zm8", 1, b"")
    });
    o.t("pad_xyz_16", || util::pad(b"xyz", 16));
    o.t("pad_empty_1", || util::pad(b"", 1));
    o.t("pad_5_5", || util::pad(b"abcde", 5));
    o.t("pad_7_3", || util::pad(b"abcdefg", 3));
    o.t("pad_bad", || util::pad(b"x", 0));
    o.t("unpad", || util::unpad(&util::pad(b"abcdefg", 3)?, 3));
    o.t("unpad_short", || util::unpad(b"x", 2));
    o.t("unpad_bad", || util::unpad(b"abcd", 4));
    o.t("unpad_neg", || util::unpad(b"abcd", -4));
    o.t("compare", || {
        Ok(vec![
            V::Int(util::compare(b"\x01\x02", b"\x02\x01")?),
            V::Int(util::compare(b"\x02\x01", b"\x01\x02")?),
            V::Int(util::compare(b"ab", b"ab")?),
        ])
    });
    o.t("compare_len", || util::compare(b"a", b"ab"));
    o.t("memcmp", || {
        Ok(vec![
            V::Int(util::memcmp(b"ab", b"ab")?),
            V::Int(util::memcmp(b"ab", b"ac")?),
        ])
    });
    o.t("increment", || {
        let mut s = b"\xff\xfe\x00".to_vec();
        util::increment(&mut s);
        ok(s)
    });
    o.t("add", || {
        let mut s = b"\xff\xff\x01".to_vec();
        util::add(&mut s, b"\x02\x00\x00")?;
        ok(s)
    });
    o.t("add_len", || {
        let mut s = b"ab".to_vec();
        util::add(&mut s, b"a")?;
        ok(s)
    });
    o.t("memzero", || {
        let mut s = b"abc\x01".to_vec();
        util::memzero(&mut s);
        assert_eq!(s, [0; 4]);
        ok(V::Null)
    });

    // aead
    type Enc = fn(&[u8], &[u8], &[u8], &[u8]) -> Result<Vec<u8>, Error>;
    type Dec = fn(&[u8], &[u8], &[u8], &[u8]) -> Result<Option<Vec<u8>>, Error>;
    let algs: [(&str, &[u8], Enc, Dec, fn() -> Vec<u8>); 4] = [
        (
            "chacha20poly1305",
            &n8,
            aead::chacha20poly1305_encrypt,
            aead::chacha20poly1305_decrypt,
            aead::chacha20poly1305_keygen,
        ),
        (
            "chacha20poly1305_ietf",
            &n12,
            aead::chacha20poly1305_ietf_encrypt,
            aead::chacha20poly1305_ietf_decrypt,
            aead::chacha20poly1305_ietf_keygen,
        ),
        (
            "xchacha20poly1305_ietf",
            &n24,
            aead::xchacha20poly1305_ietf_encrypt,
            aead::xchacha20poly1305_ietf_decrypt,
            aead::xchacha20poly1305_ietf_keygen,
        ),
        (
            "aes256gcm",
            &n12,
            aead::aes256gcm_encrypt,
            aead::aes256gcm_decrypt,
            aead::aes256gcm_keygen,
        ),
    ];
    for (alg, nonce, enc, dec, keygen) in algs {
        o.t(&format!("aead_{alg}_enc"), || enc(msg, ad, nonce, &k32));
        o.t(&format!("aead_{alg}_enc_empty"), || {
            enc(b"", b"", nonce, &k32)
        });
        o.t(&format!("aead_{alg}_dec"), || {
            dec(&enc(msg, ad, nonce, &k32)?, ad, nonce, &k32)
        });
        o.t(&format!("aead_{alg}_dec_badad"), || {
            dec(&enc(msg, ad, nonce, &k32)?, b"x", nonce, &k32)
        });
        o.t(&format!("aead_{alg}_dec_short"), || {
            dec(b"short", ad, nonce, &k32)
        });
        o.t(&format!("aead_{alg}_badnonce"), || enc(msg, ad, b"x", &k32));
        o.t(&format!("aead_{alg}_badkey"), || enc(msg, ad, nonce, b"x"));
        o.t(&format!("aead_{alg}_dec_badnonce"), || {
            dec(msg, ad, b"x", &k32)
        });
        o.t(&format!("aead_{alg}_dec_badkey"), || {
            dec(msg, ad, nonce, b"x")
        });
        o.t(&format!("aead_{alg}_keygen_len"), || ok(keygen().len()));
    }
    o.t("aes256gcm_available", || ok(aead::aes256gcm_is_available()));

    // auth / shorthash / generichash / kdf
    o.t("auth", || hash::auth(msg, &k32));
    o.t("auth_badkey", || hash::auth(msg, &k16));
    o.t("auth_verify", || {
        Ok(vec![
            V::Bool(hash::auth_verify(&hash::auth(msg, &k32)?, msg, &k32)?),
            V::Bool(hash::auth_verify(&[0; 32], msg, &k32)?),
        ])
    });
    o.t("auth_verify_badmac", || hash::auth_verify(b"x", msg, &k32));
    o.t("auth_verify_badkey", || hash::auth_verify(b"x", msg, b"y"));
    o.t("shorthash", || hash::shorthash(msg, &k16));
    o.t("shorthash_badkey", || hash::shorthash(msg, &k32));
    o.t("generichash", || hash::generichash(msg, b"", 32));
    o.t("generichash_key", || hash::generichash(msg, &k32, 64));
    o.t("generichash_key16_len16", || {
        hash::generichash(msg, &k16, 16)
    });
    o.t("generichash_badlen", || hash::generichash(msg, b"", 15));
    o.t("generichash_badlen2", || hash::generichash(msg, b"", 65));
    o.t("generichash_badkey", || {
        hash::generichash(msg, b"short", 32)
    });
    o.t("generichash_stream", || {
        let mut st = hash::generichash_init(&k32, 48)?;
        hash::generichash_update(&mut st, &msg[..10])?;
        hash::generichash_update(&mut st, &msg[10..])?;
        let mut copy = st.clone();
        let h = hash::generichash_final(&mut st, 48)?;
        assert!(st.iter().all(|&b| b == 0));
        let h2 = hash::generichash_final(&mut copy, 48)?;
        ok(vec![V::B(h), V::B(h2), V::Null, V::Null])
    });
    o.t("generichash_init_len", || {
        ok(hash::generichash_init(b"", 32)?.len())
    });
    o.t("generichash_update_badstate", || {
        let mut st = b"abc".to_vec();
        hash::generichash_update(&mut st, b"x").map(|()| true)
    });
    o.t("generichash_final_badstate", || {
        hash::generichash_final(&mut b"abc".to_vec(), 32)
    });
    o.t("generichash_final_badlen", || {
        hash::generichash_final(&mut hash::generichash_init(b"", 32)?, 8)
    });
    o.t("generichash_init_badlen", || {
        hash::generichash_init(b"", 100)
    });
    o.t("kdf", || {
        hash::kdf_derive_from_key(32, 7, b"context!", &k32)
    });
    o.t("kdf_64_max_id", || {
        hash::kdf_derive_from_key(64, i64::MAX, b"ABCDEFGH", &k32)
    });
    o.t("kdf_short", || {
        hash::kdf_derive_from_key(15, 1, b"context!", &k32)
    });
    o.t("kdf_long", || {
        hash::kdf_derive_from_key(65, 1, b"context!", &k32)
    });
    o.t("kdf_negid", || {
        hash::kdf_derive_from_key(16, -1, b"context!", &k32)
    });
    o.t("kdf_badctx", || {
        hash::kdf_derive_from_key(16, 1, b"ctx", &k32)
    });
    o.t("kdf_badkey", || {
        hash::kdf_derive_from_key(16, 1, b"context!", &k16)
    });

    // box / kx / scalarmult
    let bkp1 = boxes::box_seed_keypair(&k32).unwrap();
    let bkp2 = boxes::box_seed_keypair(&rev32).unwrap();
    o.t("box_seed_keypair", || ok(bkp1.clone()));
    o.t("box_seed_keypair_bad", || boxes::box_seed_keypair(b"x"));
    o.t("box_publickey_from_secretkey", || {
        boxes::box_publickey_from_secretkey(&boxes::box_secretkey(&bkp1)?)
    });
    o.t("scalarmult_base", || boxes::scalarmult_base(&k32));
    o.t("scalarmult_base_bad", || boxes::scalarmult_base(b"x"));
    o.t("box_publickey_from_secretkey_bad", || {
        boxes::box_publickey_from_secretkey(b"x")
    });
    o.t("box_publickey", || boxes::box_publickey(&bkp1));
    o.t("box_secretkey_bad", || boxes::box_secretkey(b"x"));
    o.t("box_publickey_bad", || boxes::box_publickey(b"x"));
    let a2b = boxes::box_keypair_from_secretkey_and_publickey(&bkp1[..32], &bkp2[32..]).unwrap();
    let b2a = boxes::box_keypair_from_secretkey_and_publickey(&bkp2[..32], &bkp1[32..]).unwrap();
    o.t("box_kp_from_bad1", || {
        boxes::box_keypair_from_secretkey_and_publickey(b"x", &k32)
    });
    o.t("box_kp_from_bad2", || {
        boxes::box_keypair_from_secretkey_and_publickey(&k32, b"x")
    });
    o.t("box", || boxes::crypto_box(msg, &n24, &a2b));
    o.t("box_open", || {
        boxes::box_open(&boxes::crypto_box(msg, &n24, &a2b)?, &n24, &b2a)
    });
    o.t("box_open_fail", || boxes::box_open(&[b'x'; 40], &n24, &b2a));
    o.t("box_open_short", || boxes::box_open(b"x", &n24, &b2a));
    o.t("box_badnonce", || boxes::crypto_box(msg, b"x", &a2b));
    o.t("box_badkp", || boxes::crypto_box(msg, &n24, b"x"));
    o.t("box_open_badnonce", || boxes::box_open(msg, b"x", &a2b));
    o.t("box_seal_len", || {
        ok(boxes::box_seal(msg, &bkp1[32..])?.len())
    });
    o.t("box_seal_roundtrip", || {
        boxes::box_seal_open(&boxes::box_seal(msg, &bkp1[32..])?, &bkp1)
    });
    o.t("box_seal_open_fail", || {
        boxes::box_seal_open(&[b'y'; 60], &bkp1)
    });
    o.t("box_seal_open_short", || boxes::box_seal_open(b"y", &bkp1));
    o.t("box_seal_badpk", || boxes::box_seal(msg, b"x"));
    o.t("box_seal_open_badkp", || boxes::box_seal_open(msg, b"x"));
    o.t("scalarmult", || boxes::scalarmult(&k32, &bkp2[32..]));
    o.t("scalarmult_zero", || boxes::scalarmult(&k32, &[0; 32]));
    o.t("scalarmult_badn", || boxes::scalarmult(b"x", &k32));
    o.t("scalarmult_badp", || boxes::scalarmult(&k32, b"x"));
    let kx1 = boxes::kx_seed_keypair(&k32).unwrap();
    let kx2 = boxes::kx_seed_keypair(&rev32).unwrap();
    o.t("kx_seed_keypair", || ok(kx1.clone()));
    o.t("kx_seed_keypair_bad", || boxes::kx_seed_keypair(b"x"));
    o.t("kx_secretkey", || boxes::kx_secretkey(&kx1));
    o.t("kx_publickey", || boxes::kx_publickey(&kx1));
    o.t("kx_secretkey_bad", || boxes::kx_secretkey(b"x"));
    o.t("kx_publickey_bad", || boxes::kx_publickey(b"x"));
    o.t("kx_client", || {
        boxes::kx_client_session_keys(&kx1, &kx2[32..])
    });
    o.t("kx_server", || {
        boxes::kx_server_session_keys(&kx2, &kx1[32..])
    });
    o.t("kx_client_bad1", || {
        boxes::kx_client_session_keys(b"x", &k32)
    });
    o.t("kx_client_bad2", || {
        boxes::kx_client_session_keys(&kx1, b"x")
    });
    o.t("kx_server_bad1", || {
        boxes::kx_server_session_keys(b"x", &k32)
    });
    o.t("kx_server_bad2", || {
        boxes::kx_server_session_keys(&kx1, b"x")
    });
    o.t("kx_keypair_len", || ok(boxes::kx_keypair()?.len()));

    // secretbox / stream
    o.t("secretbox", || secretbox::secretbox(msg, &n24, &k32));
    o.t("secretbox_open", || {
        secretbox::secretbox_open(&secretbox::secretbox(msg, &n24, &k32)?, &n24, &k32)
    });
    o.t("secretbox_open_fail", || {
        secretbox::secretbox_open(&[b'z'; 20], &n24, &k32)
    });
    o.t("secretbox_open_short", || {
        secretbox::secretbox_open(b"z", &n24, &k32)
    });
    o.t("secretbox_badnonce", || {
        secretbox::secretbox(msg, b"x", &k32)
    });
    o.t("secretbox_badkey", || secretbox::secretbox(msg, &n24, b"x"));
    o.t("secretbox_open_badnonce", || {
        secretbox::secretbox_open(msg, b"x", &k32)
    });
    o.t("secretbox_open_badkey", || {
        secretbox::secretbox_open(msg, &n24, b"x")
    });
    o.t("stream", || secretbox::stream(40, &n24, &k32));
    o.t("stream_zero", || secretbox::stream(0, &n24, &k32));
    o.t("stream_badnonce", || secretbox::stream(4, b"x", &k32));
    o.t("stream_badkey", || secretbox::stream(4, &n24, b"x"));
    o.t("stream_xor", || secretbox::stream_xor(msg, &n24, &k32));
    o.t("stream_xor_badnonce", || {
        secretbox::stream_xor(msg, b"x", &k32)
    });
    o.t("stream_xchacha20", || {
        secretbox::stream_xchacha20(40, &n24, &k32)
    });
    o.t("stream_xchacha20_neg", || {
        secretbox::stream_xchacha20(-1, &n24, &k32)
    });
    o.t("stream_xchacha20_badnonce", || {
        secretbox::stream_xchacha20(1, b"x", &k32)
    });
    o.t("stream_xchacha20_badkey", || {
        secretbox::stream_xchacha20(1, &n24, b"x")
    });
    o.t("stream_xchacha20_xor", || {
        secretbox::stream_xchacha20_xor(msg, &n24, &k32)
    });
    o.t("stream_xchacha20_xor_badkey", || {
        secretbox::stream_xchacha20_xor(msg, &n24, b"x")
    });
    o.t("stream_xchacha20_xor_ic", || {
        secretbox::stream_xchacha20_xor_ic(msg, &n24, 5, &k32)
    });
    o.t("stream_xchacha20_xor_ic_neg", || {
        secretbox::stream_xchacha20_xor_ic(msg, &n24, -1, &k32)
    });
    o.t("stream_xchacha20_xor_ic_badnonce", || {
        secretbox::stream_xchacha20_xor_ic(msg, b"x", 0, &k32)
    });
    o.t("stream_xchacha20_xor_ic_badkey", || {
        secretbox::stream_xchacha20_xor_ic(msg, &n24, 0, b"x")
    });

    // sign
    let skp = sign::sign_seed_keypair(&k32).unwrap();
    let ssk = sign::sign_secretkey(&skp).unwrap();
    let spk = sign::sign_publickey(&skp).unwrap();
    o.t("sign_seed_keypair", || ok(skp.clone()));
    o.t("sign_seed_keypair_bad", || sign::sign_seed_keypair(b"x"));
    o.t("sign_secretkey_bad", || sign::sign_secretkey(b"x"));
    o.t("sign_publickey_bad", || sign::sign_publickey(b"x"));
    o.t("sign_publickey_from_secretkey", || {
        sign::sign_publickey_from_secretkey(&ssk)
    });
    o.t("sign_publickey_from_secretkey_bad", || {
        sign::sign_publickey_from_secretkey(b"x")
    });
    o.t("sign_kp_from", || {
        sign::sign_keypair_from_secretkey_and_publickey(&ssk, &spk)
    });
    o.t("sign_kp_from_bad1", || {
        sign::sign_keypair_from_secretkey_and_publickey(b"x", &spk)
    });
    o.t("sign_kp_from_bad2", || {
        sign::sign_keypair_from_secretkey_and_publickey(&ssk, b"x")
    });
    o.t("sign", || sign::sign(msg, &ssk));
    o.t("sign_badkey", || sign::sign(msg, b"x"));
    o.t("sign_open", || {
        sign::sign_open(&sign::sign(msg, &ssk)?, &spk)
    });
    o.t("sign_open_fail", || sign::sign_open(&[b'q'; 70], &spk));
    o.t("sign_open_short", || sign::sign_open(b"q", &spk));
    o.t("sign_open_badkey", || sign::sign_open(b"q", b"x"));
    o.t("sign_detached", || sign::sign_detached(msg, &ssk));
    o.t("sign_detached_badkey", || sign::sign_detached(msg, b"x"));
    o.t("sign_verify_detached", || {
        Ok(vec![
            V::Bool(sign::sign_verify_detached(
                &sign::sign_detached(msg, &ssk)?,
                msg,
                &spk,
            )?),
            V::Bool(sign::sign_verify_detached(&[1; 64], msg, &spk)?),
        ])
    });
    o.t("sign_verify_detached_badsig", || {
        sign::sign_verify_detached(b"x", msg, &spk)
    });
    o.t("sign_verify_detached_badpk", || {
        sign::sign_verify_detached(&[1; 64], msg, b"x")
    });
    o.t("sign_sk_to_curve", || {
        sign::sign_ed25519_sk_to_curve25519(&ssk)
    });
    o.t("sign_pk_to_curve", || {
        sign::sign_ed25519_pk_to_curve25519(&spk)
    });
    o.t("sign_pk_to_curve_fail", || {
        sign::sign_ed25519_pk_to_curve25519(&[0; 32])
    });
    o.t("sign_pk_to_curve_bad", || {
        sign::sign_ed25519_pk_to_curve25519(b"x")
    });
    o.t("sign_sk_to_curve_bad", || {
        sign::sign_ed25519_sk_to_curve25519(b"x")
    });
    o.t("sign_keypair_len", || ok(sign::sign_keypair()?.len()));

    // secretstream
    o.t("ss_init_push_bad", || secretstream::init_push(b"x"));
    o.t("ss_init_pull_badheader", || {
        secretstream::init_pull(b"x", &k32)
    });
    o.t("ss_init_pull_badkey", || {
        secretstream::init_pull(&n24, b"x")
    });
    o.t("ss_pull_fixed", || {
        let mut st = secretstream::init_pull(&n24, &k32)?;
        let r = secretstream::pull(&mut st, &[0; 20], b"")?;
        assert!(r.is_none());
        ok(vec![
            V::Int(i64::try_from(st.len()).unwrap()),
            V::Bool(false),
        ])
    });
    o.t("ss_push_badstate", || {
        secretstream::push(&mut b"abc".to_vec(), b"x", b"", 0)
    });
    o.t("ss_push_badtag", || {
        secretstream::push(&mut secretstream::init_push(&k32)?.0, b"x", b"", 256)
    });
    o.t("ss_push_negtag", || {
        secretstream::push(&mut secretstream::init_push(&k32)?.0, b"x", b"", -1)
    });
    o.t("ss_pull_badstate", || {
        secretstream::pull(&mut b"abc".to_vec(), b"x", b"").map(|r| r.is_some())
    });
    o.t("ss_pull_short", || {
        let mut st = secretstream::init_pull(&n24, &k32)?;
        secretstream::pull(&mut st, b"x", b"").map(|r| r.is_some())
    });
    o.t("ss_rekey_badstate", || {
        secretstream::rekey(&mut b"abc".to_vec()).map(|()| V::Null)
    });
    o.t("ss_rekey_fixed", || {
        let mut st = secretstream::init_pull(&n24, &k32)?;
        secretstream::rekey(&mut st)?;
        ok(st)
    });

    // pwhash
    let salt16 = &k32[..16];
    let mut w = |m: &str| warn(m);
    o.t("pwhash_i", || {
        pwhash::pwhash(32, b"password", salt16, 3, 8192, 1, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_id", || {
        pwhash::pwhash(16, b"password", salt16, 1, 8192, 2, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_default", || {
        pwhash::pwhash(16, b"password", salt16, 2, 16384, 2, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_i_ops2", || {
        pwhash::pwhash(16, b"password", salt16, 2, 8192, 1, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_empty", || {
        pwhash::pwhash(16, b"", salt16, 1, 8192, 2, &mut w)
    });
    let cases: [(&str, i64, &[u8], &[u8], i64, i64, i64); 8] = [
        ("pwhash_len0", 0, b"p", salt16, 1, 8192, 2),
        ("pwhash_lenbig", 0xffff_ffff, b"p", salt16, 1, 8192, 2),
        ("pwhash_ops0", 16, b"p", salt16, 0, 8192, 2),
        ("pwhash_mem0", 16, b"p", salt16, 1, 0, 2),
        ("pwhash_alg", 16, b"p", salt16, 1, 8192, 3),
        ("pwhash_alg_empty", 16, b"", salt16, 1, 8192, 3),
        ("pwhash_salt", 16, b"", b"salt", 1, 8192, 2),
        ("pwhash_mem_min", 16, b"p", salt16, 1, 8191, 2),
    ];
    for (label, len, pw, salt, ops, mem, alg) in cases {
        let mut w = |m: &str| warn(m);
        o.t(label, || {
            pwhash::pwhash(len, pw, salt, ops, mem, alg, &mut w)
        });
    }
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_prefix", || {
        ok(pwhash::pwhash_str(b"password", 1, 8192, &mut w)?[..27].to_vec())
    });
    o.t("pwhash_str_verify", || {
        let mut w = |m: &str| warn(m);
        let h = pwhash::pwhash_str(b"password", 1, 8192, &mut w)?;
        Ok(vec![
            V::Bool(pwhash::pwhash_str_verify(&h, b"password", &mut w)?),
            V::Bool(pwhash::pwhash_str_verify(&h, b"wrong", &mut w)?),
        ])
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_verify_fixed", || {
        pwhash::pwhash_str_verify(
            b"$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$1Gb4Cb3pPDB9AAJr3Mr1ThGqp4yOYHNGqk7bDyQcGG4",
            b"password",
            &mut w,
        )
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_verify_garbage", || {
        pwhash::pwhash_str_verify(b"nope", b"", &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_ops0", || {
        pwhash::pwhash_str(b"p", 0, 8192, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_mem0", || pwhash::pwhash_str(b"p", 1, 0, &mut w));
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_mem_min", || {
        pwhash::pwhash_str(b"p", 1, 1, &mut w)
    });
    let mut w = |m: &str| warn(m);
    o.t("pwhash_str_empty_len", || {
        ok(pwhash::pwhash_str(b"", 1, 8192, &mut w)?.len())
    });
    o.t("pwhash_needs_rehash", || {
        let h = pwhash::pwhash_str(b"password", 1, 8192, &mut |_| {})?;
        ok(vec![
            V::Bool(pwhash::pwhash_str_needs_rehash(&h, 1, 8192)),
            V::Bool(pwhash::pwhash_str_needs_rehash(&h, 2, 8192)),
            V::Bool(pwhash::pwhash_str_needs_rehash(b"junk", 1, 8192)),
        ])
    });
    let salt32 = &k32[..];
    let cases: [(&str, i64, &[u8], &[u8], i64, i64); 7] = [
        ("scrypt", 32, b"password", salt32, 524_288, 16_777_216),
        ("scrypt_len0", 0, b"p", salt32, 524_288, 16_777_216),
        ("scrypt_ops0", 1, b"p", salt32, 0, 16_777_216),
        ("scrypt_mem0", 1, b"p", salt32, 1, -1),
        ("scrypt_salt", 1, b"", b"x", 1, 1),
        ("scrypt_ops_min", 1, b"p", salt32, 1, 1),
        ("scrypt_mem_min", 1, b"p", salt32, 524_288, 1),
    ];
    for (label, len, pw, salt, ops, mem) in cases {
        let mut w = |m: &str| warn(m);
        o.t(label, || {
            pwhash::scryptsalsa208sha256(len, pw, salt, ops, mem, &mut w)
        });
    }
    o.t("scrypt_str", || {
        let mut w = |m: &str| warn(m);
        let h = pwhash::scryptsalsa208sha256_str(b"password", 524_288, 16_777_216, &mut w)?;
        ok(vec![
            V::Int(i64::try_from(h.len()).unwrap()),
            V::B(h[..3].to_vec()),
            V::Bool(pwhash::scryptsalsa208sha256_str_verify(
                &h,
                b"password",
                &mut w,
            )),
            V::Bool(pwhash::scryptsalsa208sha256_str_verify(&h, b"nope", &mut w)),
        ])
    });
    let cases: [(&str, &[u8], i64, i64); 4] = [
        ("scrypt_str_ops0", b"p", 0, 1),
        ("scrypt_str_mem0", b"p", 1, 0),
        ("scrypt_str_ops_min", b"", 1, 1),
        ("scrypt_str_mem_min", b"p", 524_288, 1),
    ];
    for (label, pw, ops, mem) in cases {
        let mut w = |m: &str| warn(m);
        o.t(label, || {
            pwhash::scryptsalsa208sha256_str(pw, ops, mem, &mut w)
        });
    }
    let mut w = |m: &str| warn(m);
    o.t("scrypt_str_verify_size", || {
        ok(pwhash::scryptsalsa208sha256_str_verify(b"abc", b"", &mut w))
    });

    // ristretto255
    let rev64: Vec<u8> = k64.iter().rev().copied().collect();
    let p1 = ristretto::from_hash(&k64).unwrap();
    let p2 = ristretto::from_hash(&rev64).unwrap();
    let s1 = ristretto::scalar_reduce(&k64).unwrap();
    let s2 = ristretto::scalar_reduce(&rev64).unwrap();
    let ff = [0xffu8; 32];
    o.t("r_from_hash", || ok(p1.clone()));
    o.t("r_from_hash_bad", || ristretto::from_hash(b"x"));
    o.t("r_is_valid", || {
        Ok(vec![
            V::Bool(ristretto::is_valid_point(&p1)?),
            V::Bool(ristretto::is_valid_point(&ff)?),
        ])
    });
    o.t("r_is_valid_bad", || ristretto::is_valid_point(b"x"));
    o.t("r_add", || ristretto::add(&p1, &p2));
    o.t("r_add_invalid", || ristretto::add(&p1, &ff));
    o.t("r_add_bad1", || ristretto::add(b"x", &p2));
    o.t("r_add_bad2", || ristretto::add(&p1, b"x"));
    o.t("r_sub", || ristretto::sub(&p1, &p2));
    o.t("r_sub_bad2", || ristretto::sub(&p1, b"x"));
    o.t("r_scalar_reduce", || ok(s1.clone()));
    o.t("r_scalar_reduce_bad", || ristretto::scalar_reduce(&k32));
    o.t("r_scalar_add", || ristretto::scalar_add(&s1, &s2));
    o.t("r_scalar_add_bad1", || ristretto::scalar_add(b"x", &s2));
    o.t("r_scalar_sub", || ristretto::scalar_sub(&s1, &s2));
    o.t("r_scalar_sub_bad2", || ristretto::scalar_sub(&s1, b"x"));
    o.t("r_scalar_mul", || ristretto::scalar_mul(&s1, &s2));
    o.t("r_scalar_mul_bad2", || ristretto::scalar_mul(&s1, b"x"));
    o.t("r_scalar_invert", || ristretto::scalar_invert(&s1));
    o.t("r_scalar_invert_zero", || {
        ristretto::scalar_invert(&[0; 32])
    });
    o.t("r_scalar_invert_bad", || ristretto::scalar_invert(b"x"));
    o.t("r_scalar_negate", || ristretto::scalar_negate(&s1));
    o.t("r_scalar_negate_bad", || ristretto::scalar_negate(b"x"));
    o.t("r_scalar_complement", || ristretto::scalar_complement(&s1));
    o.t("r_scalar_complement_bad", || {
        ristretto::scalar_complement(b"x")
    });
    o.t("r_random_valid", || {
        ristretto::is_valid_point(&ristretto::random())
    });
    o.t("r_scalar_random_len", || {
        ok(ristretto::scalar_random().len())
    });
    o.t("r_scalarmult", || ristretto::scalarmult(&s1, &p1));
    o.t("r_scalarmult_identity", || {
        ristretto::scalarmult(&[0; 32], &p1)
    });
    o.t("r_scalarmult_bad1", || ristretto::scalarmult(b"x", &p1));
    o.t("r_scalarmult_bad2", || ristretto::scalarmult(&s1, b"x"));
    o.t("r_scalarmult_base", || ristretto::scalarmult_base(&s1));
    o.t("r_scalarmult_base_zero", || {
        ristretto::scalarmult_base(&[0; 32])
    });
    o.t("r_scalarmult_base_bad", || ristretto::scalarmult_base(b"x"));

    // keygens
    let keygens: [(&str, fn() -> Vec<u8>); 8] = [
        ("auth", hash::auth_keygen),
        ("generichash", hash::generichash_keygen),
        ("kdf", hash::kdf_keygen),
        ("secretbox", secretbox::secretbox_keygen),
        ("shorthash", hash::shorthash_keygen),
        ("stream", secretbox::stream_keygen),
        ("stream_xchacha20", secretbox::stream_xchacha20_keygen),
        ("secretstream_xchacha20poly1305", secretstream::keygen),
    ];
    for (name, f) in keygens {
        o.t(&format!("keygen_{name}"), || ok(f().len()));
    }
    let info = |h: &[u8]| {
        let i = pwhash::argon2_get_info(h).unwrap();
        ok(vec![
            V::Int(i.memory_cost),
            V::Int(i.time_cost),
            V::Int(i.threads),
        ])
    };
    o.t("password_get_info", || {
        info(b"$argon2id$v=19$m=1024,t=2,p=3$c2FsdHNhbHQ$abc")
    });
    o.t("password_get_info_partial", || {
        info(b"$argon2i$v=19$m=77$xx")
    });

    let expected: Vec<String> = include_str!("data/oracle.txt")
        .lines()
        .map(master)
        .collect();
    let mut mismatches = Vec::new();
    for (i, (got, want)) in o.0.iter().zip(&expected).enumerate() {
        if got != want {
            mismatches.push(format!("line {}:\n  got  {got}\n  want {want}", i + 1));
        }
    }
    assert_eq!(o.0.len(), expected.len(), "line count");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Every constant `/usr/bin/php` registers for ext/sodium, in order.
#[test]
fn constants_match_php() {
    use php_sodium::consts;
    let mut ours = vec![
        format!("SODIUM_LIBRARY_VERSION={}", consts::library_version()),
        format!(
            "SODIUM_LIBRARY_MAJOR_VERSION={}",
            consts::library_major_version()
        ),
        format!(
            "SODIUM_LIBRARY_MINOR_VERSION={}",
            consts::library_minor_version()
        ),
    ];
    for (name, value) in consts::INT_CONSTANTS {
        ours.push(format!("{name}={value}"));
        // The string constants sit between the pwhash and scrypt groups.
        if *name == "SODIUM_CRYPTO_PWHASH_SALTBYTES" {
            ours.push(format!(
                "SODIUM_CRYPTO_PWHASH_STRPREFIX={}",
                consts::SODIUM_CRYPTO_PWHASH_STRPREFIX
            ));
        }
        if *name == "SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_SALTBYTES" {
            ours.push(format!(
                "SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_STRPREFIX={}",
                consts::SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_STRPREFIX
            ));
        }
    }
    let php: Vec<&str> = include_str!("data/constants.txt").lines().collect();
    assert_eq!(ours, php);
}
