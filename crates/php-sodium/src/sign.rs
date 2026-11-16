// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_crypto_sign*` (Ed25519) and the Ed25519→X25519 conversions.

#![allow(unsafe_code)]

use crate::error::{check_len, exc, internal, overflow, Result};
use crate::{ffi, init, ptr, ZSTR_MAX_LEN};

const BYTES: usize = 64;
const SEEDBYTES: usize = 32;
const PK: usize = 32;
const SK: usize = 64;
const KEYPAIR: usize = SK + PK;

fn keypair_with(f: impl FnOnce(*mut u8, *mut u8) -> i32) -> Result<Vec<u8>> {
    let mut kp = vec![0u8; KEYPAIR];
    let (sk, pk) = kp.split_at_mut(SK);
    if f(pk.as_mut_ptr(), sk.as_mut_ptr()) != 0 {
        return Err(internal());
    }
    Ok(kp)
}

/// `sodium_crypto_sign_keypair()`: secret key (64) ‖ public key (32).
pub fn sign_keypair() -> Result<Vec<u8>> {
    init();
    // SAFETY: halves are 64 and 32 bytes.
    keypair_with(|pk, sk| unsafe { ffi::crypto_sign_keypair(pk, sk) })
}

/// `sodium_crypto_sign_seed_keypair($seed)`.
pub fn sign_seed_keypair(seed: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign_seed_keypair",
        1,
        "seed",
        seed,
        SEEDBYTES,
        "SODIUM_CRYPTO_SIGN_SEEDBYTES",
    )?;
    // SAFETY: halves sized per the constants; seed length checked.
    keypair_with(|pk, sk| unsafe { ffi::crypto_sign_seed_keypair(pk, sk, seed.as_ptr()) })
}

/// `sodium_crypto_sign_keypair_from_secretkey_and_publickey($secret_key, $public_key)`.
pub fn sign_keypair_from_secretkey_and_publickey(sk: &[u8], pk: &[u8]) -> Result<Vec<u8>> {
    let f = "sodium_crypto_sign_keypair_from_secretkey_and_publickey";
    check_len(
        f,
        1,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_SIGN_SECRETKEYBYTES",
    )?;
    check_len(
        f,
        2,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES",
    )?;
    Ok([sk, pk].concat())
}

/// `sodium_crypto_sign_publickey_from_secretkey($secret_key)`.
pub fn sign_publickey_from_secretkey(sk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign_publickey_from_secretkey",
        1,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_SIGN_SECRETKEYBYTES",
    )?;
    let mut pk = vec![0u8; PK];
    // SAFETY: sizes checked/allocated.
    if unsafe { ffi::crypto_sign_ed25519_sk_to_pk(pk.as_mut_ptr(), sk.as_ptr()) } != 0 {
        return Err(internal());
    }
    Ok(pk)
}

/// `sodium_crypto_sign_secretkey($key_pair)`.
pub fn sign_secretkey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_sign_secretkey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_SIGN_KEYPAIRBYTES",
    )?;
    Ok(kp[..SK].to_vec())
}

/// `sodium_crypto_sign_publickey($key_pair)`.
pub fn sign_publickey(kp: &[u8]) -> Result<Vec<u8>> {
    check_len(
        "sodium_crypto_sign_publickey",
        1,
        "key_pair",
        kp,
        KEYPAIR,
        "SODIUM_CRYPTO_SIGN_KEYPAIRBYTES",
    )?;
    Ok(kp[SK..].to_vec())
}

/// `sodium_crypto_sign($message, $secret_key)`: signature ‖ message.
pub fn sign(msg: &[u8], sk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign",
        2,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_SIGN_SECRETKEYBYTES",
    )?;
    if ZSTR_MAX_LEN - msg.len() <= BYTES {
        return Err(overflow());
    }
    let mut out = vec![0u8; msg.len() + BYTES];
    let mut real = 0u64;
    // SAFETY: sizes checked/allocated.
    let rc = unsafe {
        ffi::crypto_sign(
            out.as_mut_ptr(),
            &raw mut real,
            ptr(msg),
            msg.len() as u64,
            sk.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    let real = usize::try_from(real).map_err(|_| overflow())?;
    if real > out.len() {
        return Err(overflow());
    }
    out.truncate(real);
    Ok(out)
}

/// `sodium_crypto_sign_open($signed_message, $public_key)`.
pub fn sign_open(signed: &[u8], pk: &[u8]) -> Result<Option<Vec<u8>>> {
    init();
    check_len(
        "sodium_crypto_sign_open",
        2,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES",
    )?;
    if signed.len() >= ZSTR_MAX_LEN {
        return Err(overflow());
    }
    let mut out = vec![0u8; signed.len()];
    let mut real = 0u64;
    // SAFETY: the output buffer is as long as the signed message.
    let rc = unsafe {
        ffi::crypto_sign_open(
            out.as_mut_ptr(),
            &raw mut real,
            ptr(signed),
            signed.len() as u64,
            pk.as_ptr(),
        )
    };
    if rc != 0 {
        return Ok(None);
    }
    let real = usize::try_from(real).map_err(|_| overflow())?;
    if real > signed.len() {
        return Err(overflow());
    }
    out.truncate(real);
    Ok(Some(out))
}

/// `sodium_crypto_sign_detached($message, $secret_key)`.
pub fn sign_detached(msg: &[u8], sk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign_detached",
        2,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_SIGN_SECRETKEYBYTES",
    )?;
    let mut sig = vec![0u8; BYTES];
    let mut real = 0u64;
    // SAFETY: sizes checked/allocated.
    let rc = unsafe {
        ffi::crypto_sign_detached(
            sig.as_mut_ptr(),
            &raw mut real,
            ptr(msg),
            msg.len() as u64,
            sk.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(exc("signature creation failed"));
    }
    if real == 0 || real > BYTES as u64 {
        return Err(exc("signature has a bogus size"));
    }
    #[allow(clippy::cast_possible_truncation)]
    sig.truncate(real as usize);
    Ok(sig)
}

/// `sodium_crypto_sign_verify_detached($signature, $message, $public_key)`.
pub fn sign_verify_detached(sig: &[u8], msg: &[u8], pk: &[u8]) -> Result<bool> {
    init();
    let f = "sodium_crypto_sign_verify_detached";
    check_len(f, 1, "signature", sig, BYTES, "SODIUM_CRYPTO_SIGN_BYTES")?;
    check_len(
        f,
        3,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES",
    )?;
    // SAFETY: sizes checked.
    Ok(unsafe {
        ffi::crypto_sign_verify_detached(sig.as_ptr(), ptr(msg), msg.len() as u64, pk.as_ptr())
    } == 0)
}

/// `sodium_crypto_sign_ed25519_sk_to_curve25519($secret_key)`.
pub fn sign_ed25519_sk_to_curve25519(sk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign_ed25519_sk_to_curve25519",
        1,
        "secret_key",
        sk,
        SK,
        "SODIUM_CRYPTO_SIGN_SECRETKEYBYTES",
    )?;
    let mut out = vec![0u8; 32];
    // SAFETY: sizes checked/allocated.
    if unsafe { ffi::crypto_sign_ed25519_sk_to_curve25519(out.as_mut_ptr(), sk.as_ptr()) } != 0 {
        return Err(exc("conversion failed"));
    }
    Ok(out)
}

/// `sodium_crypto_sign_ed25519_pk_to_curve25519($public_key)`.
pub fn sign_ed25519_pk_to_curve25519(pk: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_sign_ed25519_pk_to_curve25519",
        1,
        "public_key",
        pk,
        PK,
        "SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES",
    )?;
    let mut out = vec![0u8; 32];
    // SAFETY: sizes checked/allocated.
    if unsafe { ffi::crypto_sign_ed25519_pk_to_curve25519(out.as_mut_ptr(), pk.as_ptr()) } != 0 {
        return Err(exc("conversion failed"));
    }
    Ok(out)
}
