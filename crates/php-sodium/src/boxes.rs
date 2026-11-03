//! `sodium_crypto_box*` (X25519-XSalsa20-Poly1305, incl. sealed boxes),
//! `sodium_crypto_kx*` and `sodium_crypto_scalarmult*`.

#![allow(unsafe_code)]

use crate::error::{check_len, internal, overflow, Result};
use crate::{ffi, init, ptr, random, ZSTR_MAX_LEN};

const SK: usize = 32;
const PK: usize = 32;
const KEYPAIR: usize = SK + PK;
const MACBYTES: usize = 16;
const NONCEBYTES: usize = 24;
const SEEDBYTES: usize = 32;
const SEALBYTES: usize = 48;
const SESSIONKEYBYTES: usize = 32;

fn keypair_with(f: impl FnOnce(*mut u8, *mut u8) -> i32) -> Result<Vec<u8>> {
    let mut kp = vec![0u8; KEYPAIR];
    let (sk, pk) = kp.split_at_mut(SK);
    if f(pk.as_mut_ptr(), sk.as_mut_ptr()) != 0 {
        return Err(internal());
    }
    Ok(kp)
}

/// `sodium_crypto_box_keypair()`: secret key ‖ public key.
pub fn box_keypair() -> Result<Vec<u8>> {
    init();
    // SAFETY: both halves are 32-byte buffers.
    keypair_with(|pk, sk| unsafe { ffi::crypto_box_keypair(pk, sk) })
}

/// `sodium_crypto_box_seed_keypair($seed)`.
pub fn box_seed_keypair(seed: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_box_seed_keypair",
        1,
        "seed",
        seed,
        SEEDBYTES,
        "SODIUM_CRYPTO_BOX_SEEDBYTES",
    )?;
    // SAFETY: both halves are 32 bytes; the seed length was checked.
    keypair_with(|pk, sk| unsafe { ffi::crypto_box_seed_keypair(pk, sk, seed.as_ptr()) })
}

/// `sodium_crypto_box_keypair_from_secretkey_and_publickey($secret_key, $public_key)`.
pub fn box_keypair_from_secretkey_and_publickey(sk: &[u8], pk: &[u8]) -> Result<Vec<u8>> {
    let f = "sodium_crypto_box_keypair_from_secretkey_and_publickey";
    check_len(
        f,
        1,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_BOX_SECRETKEYBYTES",
    )?;
    check_len(
        f,
        2,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_BOX_PUBLICKEYBYTES",
    )?;
    Ok([sk, pk].concat())
}

/// `sodium_crypto_box_secretkey($key_pair)`.
pub fn box_secretkey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_box_secretkey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_BOX_KEYPAIRBYTES",
    )?;
    Ok(kp[..SK].to_vec())
}

/// `sodium_crypto_box_publickey($key_pair)`.
pub fn box_publickey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_box_publickey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_BOX_KEYPAIRBYTES",
    )?;
    Ok(kp[SK..].to_vec())
}

fn publickey_from_secretkey(func: &str, sk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        func,
        1,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_BOX_SECRETKEYBYTES",
    )?;
    let mut pk = vec![0u8; PK];
    // SAFETY: 32-byte buffers.  PHP ignores the result (all-zero output is
    // possible only for degenerate keys).
    unsafe { ffi::crypto_scalarmult_base(pk.as_mut_ptr(), sk.as_ptr()) };
    Ok(pk)
}

/// `sodium_crypto_box_publickey_from_secretkey($secret_key)`.
pub fn box_publickey_from_secretkey(sk: &[u8]) -> Result<Vec<u8>> {
    publickey_from_secretkey("sodium_crypto_box_publickey_from_secretkey", sk)
}

/// `sodium_crypto_scalarmult_base($secret_key)` (an alias of
/// [`box_publickey_from_secretkey`] under its own name).
pub fn scalarmult_base(sk: &[u8]) -> Result<Vec<u8>> {
    publickey_from_secretkey("sodium_crypto_scalarmult_base", sk)
}

/// `sodium_crypto_box($message, $nonce, $key_pair)`.
pub fn crypto_box(msg: &[u8], nonce: &[u8], kp: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_box";
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        "SODIUM_CRYPTO_BOX_NONCEBYTES",
    )?;
    check_len(
        f,
        3,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_BOX_KEYPAIRBYTES",
    )?;
    if ZSTR_MAX_LEN - msg.len() <= MACBYTES {
        return Err(overflow());
    }
    let mut out = vec![0u8; msg.len() + MACBYTES];
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_box_easy(
            out.as_mut_ptr(),
            ptr(msg),
            msg.len() as u64,
            nonce.as_ptr(),
            kp[SK..].as_ptr(),
            kp.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_box_open($ciphertext, $nonce, $key_pair)`.
pub fn box_open(ct: &[u8], nonce: &[u8], kp: &[u8]) -> Result<Option<Vec<u8>>> {
    init();
    let f = "sodium_crypto_box_open";
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        "SODIUM_CRYPTO_BOX_NONCEBYTES",
    )?;
    check_len(
        f,
        3,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_BOX_KEYPAIRBYTES",
    )?;
    if ct.len() < MACBYTES {
        return Ok(None);
    }
    let mut out = vec![0u8; ct.len() - MACBYTES];
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_box_open_easy(
            out.as_mut_ptr(),
            ct.as_ptr(),
            ct.len() as u64,
            nonce.as_ptr(),
            kp[SK..].as_ptr(),
            kp.as_ptr(),
        )
    };
    Ok((rc == 0).then_some(out))
}

/// `sodium_crypto_box_seal($message, $public_key)`.
pub fn box_seal(msg: &[u8], pk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_box_seal",
        2,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_BOX_PUBLICKEYBYTES",
    )?;
    if ZSTR_MAX_LEN - msg.len() <= SEALBYTES {
        return Err(overflow());
    }
    let mut out = vec![0u8; msg.len() + SEALBYTES];
    // SAFETY: lengths checked/allocated.
    if unsafe { ffi::crypto_box_seal(out.as_mut_ptr(), ptr(msg), msg.len() as u64, pk.as_ptr()) }
        != 0
    {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_box_seal_open($ciphertext, $key_pair)`.
pub fn box_seal_open(ct: &[u8], kp: &[u8]) -> Result<Option<Vec<u8>>> {
    init();
    check_len(
        "sodium_crypto_box_seal_open",
        2,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_BOX_KEYPAIRBYTES",
    )?;
    if ct.len() < SEALBYTES {
        return Ok(None);
    }
    let mut out = vec![0u8; ct.len() - SEALBYTES];
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_box_seal_open(
            out.as_mut_ptr(),
            ct.as_ptr(),
            ct.len() as u64,
            kp[SK..].as_ptr(),
            kp.as_ptr(),
        )
    };
    Ok((rc == 0).then_some(out))
}

/// `sodium_crypto_scalarmult($n, $p)` (X25519).
pub fn scalarmult(n: &[u8], p: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_scalarmult";
    check_len(f, 1, "n", n, 32, "SODIUM_CRYPTO_SCALARMULT_SCALARBYTES")?;
    check_len(f, 2, "p", p, 32, "SODIUM_CRYPTO_SCALARMULT_BYTES")?;
    let mut q = vec![0u8; 32];
    // SAFETY: 32-byte buffers.
    if unsafe { ffi::crypto_scalarmult(q.as_mut_ptr(), n.as_ptr(), p.as_ptr()) } != 0 {
        return Err(internal());
    }
    Ok(q)
}

fn kx_keypair_from(sk_bytes: Vec<u8>) -> Result<Vec<u8>> {
    let mut kp = sk_bytes;
    kp.resize(KEYPAIR, 0);
    let (sk, pk) = kp.split_at_mut(SK);
    // SAFETY: 32-byte halves.
    if unsafe { ffi::crypto_scalarmult_base(pk.as_mut_ptr(), sk.as_ptr()) } != 0 {
        return Err(internal());
    }
    Ok(kp)
}

/// `sodium_crypto_kx_keypair()`.
pub fn kx_keypair() -> Result<Vec<u8>> {
    kx_keypair_from(random(SK))
}

/// `sodium_crypto_kx_seed_keypair($seed)`: the secret key is
/// BLAKE2b-256(seed), as PHP computes it (not libsodium's own
/// `crypto_kx_seed_keypair`, which uses BLAKE2b-512).
pub fn kx_seed_keypair(seed: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_kx_seed_keypair",
        1,
        "seed",
        seed,
        SEEDBYTES,
        "SODIUM_CRYPTO_KX_SEEDBYTES",
    )?;
    let mut sk = vec![0u8; SK];
    // SAFETY: 32-byte output, 32-byte input, no key.
    unsafe {
        ffi::crypto_generichash(
            sk.as_mut_ptr(),
            SK,
            seed.as_ptr(),
            SEEDBYTES as u64,
            std::ptr::null(),
            0,
        );
    }
    kx_keypair_from(sk)
}

/// `sodium_crypto_kx_secretkey($key_pair)`.
pub fn kx_secretkey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_kx_secretkey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_KX_KEYPAIRBYTES",
    )?;
    Ok(kp[..SK].to_vec())
}

/// `sodium_crypto_kx_publickey($key_pair)`.
pub fn kx_publickey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_kx_publickey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_KX_KEYPAIRBYTES",
    )?;
    Ok(kp[SK..].to_vec())
}

/// BLAKE2b-512(`q ‖ client_pk ‖ server_pk`), split in two.
fn session_keys(
    f: &str,
    kp: &[u8],
    other_pk: &[u8],
    (kp_name, other_name): (&str, &str),
    client: bool,
) -> Result<(Vec<u8>, Vec<u8>)> {
    init();
    check_len(f, 1, kp_name, kp, KEYPAIR, "SODIUM_CRYPTO_KX_KEYPAIRBYTES")?;
    check_len(
        f,
        2,
        other_name,
        other_pk,
        PK,
        "SODIUM_CRYPTO_KX_PUBLICKEYBYTES",
    )?;
    let (sk, pk) = kp.split_at(SK);
    let mut q = [0u8; 32];
    // SAFETY: 32-byte buffers.
    if unsafe { ffi::crypto_scalarmult(q.as_mut_ptr(), sk.as_ptr(), other_pk.as_ptr()) } != 0 {
        return Err(internal());
    }
    let (client_pk, server_pk) = if client {
        (pk, other_pk)
    } else {
        (other_pk, pk)
    };
    let mut input = Vec::with_capacity(96);
    input.extend_from_slice(&q);
    input.extend_from_slice(client_pk);
    input.extend_from_slice(server_pk);
    let mut keys = [0u8; 2 * SESSIONKEYBYTES];
    // SAFETY: 64-byte output (BLAKE2b's maximum), unkeyed.
    unsafe {
        ffi::crypto_generichash(
            keys.as_mut_ptr(),
            keys.len(),
            input.as_ptr(),
            input.len() as u64,
            std::ptr::null(),
            0,
        );
        ffi::sodium_memzero(q.as_mut_ptr(), q.len());
        ffi::sodium_memzero(input.as_mut_ptr(), input.len());
    }
    let (a, b) = keys.split_at(SESSIONKEYBYTES);
    let result = if client {
        (a.to_vec(), b.to_vec())
    } else {
        (b.to_vec(), a.to_vec())
    };
    // SAFETY: local buffer.
    unsafe { ffi::sodium_memzero(keys.as_mut_ptr(), keys.len()) };
    Ok(result)
}

/// `sodium_crypto_kx_client_session_keys($client_key_pair, $server_key)`:
/// `[rx, tx]`.
pub fn kx_client_session_keys(kp: &[u8], server_pk: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    session_keys(
        "sodium_crypto_kx_client_session_keys",
        kp,
        server_pk,
        ("client_key_pair", "server_key"),
        true,
    )
}

/// `sodium_crypto_kx_server_session_keys($server_key_pair, $client_key)`:
/// `[rx, tx]`.
pub fn kx_server_session_keys(kp: &[u8], client_pk: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    session_keys(
        "sodium_crypto_kx_server_session_keys",
        kp,
        client_pk,
        ("server_key_pair", "client_key"),
        false,
    )
}
