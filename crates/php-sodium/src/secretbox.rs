// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_crypto_secretbox*` (XSalsa20-Poly1305) and
//! `sodium_crypto_stream*` (`XSalsa20` and `XChaCha20` keystreams).

#![allow(unsafe_code)]

use crate::error::{arg, check_len, internal, overflow, Result};
use crate::{ffi, init, ptr, random, ZSTR_MAX_LEN};

const KEYBYTES: usize = 32;
const NONCEBYTES: usize = 24;
const MACBYTES: usize = 16;

/// `sodium_crypto_secretbox($message, $nonce, $key)`.
pub fn secretbox(msg: &[u8], nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_secretbox";
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        "SODIUM_CRYPTO_SECRETBOX_NONCEBYTES",
    )?;
    check_len(
        f,
        3,
        "key",
        key,
        KEYBYTES,
        "SODIUM_CRYPTO_SECRETBOX_KEYBYTES",
    )?;
    if ZSTR_MAX_LEN - msg.len() <= MACBYTES {
        return Err(overflow());
    }
    let mut out = vec![0u8; msg.len() + MACBYTES];
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_secretbox_easy(
            out.as_mut_ptr(),
            ptr(msg),
            msg.len() as u64,
            nonce.as_ptr(),
            key.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_secretbox_open($ciphertext, $nonce, $key)`.
pub fn secretbox_open(ct: &[u8], nonce: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>> {
    init();
    let f = "sodium_crypto_secretbox_open";
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        "SODIUM_CRYPTO_SECRETBOX_NONCEBYTES",
    )?;
    check_len(
        f,
        3,
        "key",
        key,
        KEYBYTES,
        "SODIUM_CRYPTO_SECRETBOX_KEYBYTES",
    )?;
    if ct.len() < MACBYTES {
        return Ok(None);
    }
    let mut out = vec![0u8; ct.len() - MACBYTES];
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_secretbox_open_easy(
            out.as_mut_ptr(),
            ct.as_ptr(),
            ct.len() as u64,
            nonce.as_ptr(),
            key.as_ptr(),
        )
    };
    Ok((rc == 0).then_some(out))
}

/// `sodium_crypto_secretbox_keygen()`.
pub fn secretbox_keygen() -> Vec<u8> {
    random(KEYBYTES)
}

type StreamFn = unsafe extern "C" fn(*mut u8, u64, *const u8, *const u8) -> i32;
type XorFn = unsafe extern "C" fn(*mut u8, *const u8, u64, *const u8, *const u8) -> i32;

fn keystream(
    f: &str,
    konst: &str,
    length: i64,
    nonce: &[u8],
    key: &[u8],
    gen: StreamFn,
) -> Result<Vec<u8>> {
    init();
    let len = usize::try_from(length)
        .ok()
        .filter(|&l| l > 0 && l < ZSTR_MAX_LEN);
    let Some(len) = len else {
        return Err(arg(f, 1, "length", "must be greater than 0"));
    };
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        &format!("{konst}_NONCEBYTES"),
    )?;
    check_len(f, 3, "key", key, KEYBYTES, &format!("{konst}_KEYBYTES"))?;
    let mut out = vec![0u8; len];
    // SAFETY: lengths checked/allocated.
    if unsafe { gen(out.as_mut_ptr(), len as u64, nonce.as_ptr(), key.as_ptr()) } != 0 {
        return Err(internal());
    }
    Ok(out)
}

fn xor(f: &str, konst: &str, msg: &[u8], nonce: &[u8], key: &[u8], op: XorFn) -> Result<Vec<u8>> {
    init();
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        &format!("{konst}_NONCEBYTES"),
    )?;
    check_len(f, 3, "key", key, KEYBYTES, &format!("{konst}_KEYBYTES"))?;
    let mut out = vec![0u8; msg.len()];
    // SAFETY: lengths checked/allocated.
    if unsafe {
        op(
            out.as_mut_ptr(),
            ptr(msg),
            msg.len() as u64,
            nonce.as_ptr(),
            key.as_ptr(),
        )
    } != 0
    {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_stream($length, $nonce, $key)` (`XSalsa20`).
pub fn stream(length: i64, nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    keystream(
        "sodium_crypto_stream",
        "SODIUM_CRYPTO_STREAM",
        length,
        nonce,
        key,
        ffi::crypto_stream,
    )
}

/// `sodium_crypto_stream_xor($message, $nonce, $key)`.
pub fn stream_xor(msg: &[u8], nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    xor(
        "sodium_crypto_stream_xor",
        "SODIUM_CRYPTO_STREAM",
        msg,
        nonce,
        key,
        ffi::crypto_stream_xor,
    )
}

/// `sodium_crypto_stream_keygen()`.
pub fn stream_keygen() -> Vec<u8> {
    random(KEYBYTES)
}

/// `sodium_crypto_stream_xchacha20($length, $nonce, $key)`.
pub fn stream_xchacha20(length: i64, nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    keystream(
        "sodium_crypto_stream_xchacha20",
        "SODIUM_CRYPTO_STREAM_XCHACHA20",
        length,
        nonce,
        key,
        ffi::crypto_stream_xchacha20,
    )
}

/// `sodium_crypto_stream_xchacha20_xor($message, $nonce, $key)`.
pub fn stream_xchacha20_xor(msg: &[u8], nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    xor(
        "sodium_crypto_stream_xchacha20_xor",
        "SODIUM_CRYPTO_STREAM_XCHACHA20",
        msg,
        nonce,
        key,
        ffi::crypto_stream_xchacha20_xor,
    )
}

/// `sodium_crypto_stream_xchacha20_xor_ic($message, $nonce, $counter, $key)`.
///
/// PHP reports a bad key as argument #3 (`$counter`); that quirk is kept.
pub fn stream_xchacha20_xor_ic(
    msg: &[u8],
    nonce: &[u8],
    counter: i64,
    key: &[u8],
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_stream_xchacha20_xor_ic";
    check_len(
        f,
        2,
        "nonce",
        nonce,
        NONCEBYTES,
        "SODIUM_CRYPTO_STREAM_XCHACHA20_NONCEBYTES",
    )?;
    check_len(
        f,
        3,
        "counter",
        key,
        KEYBYTES,
        "SODIUM_CRYPTO_STREAM_XCHACHA20_KEYBYTES",
    )?;
    let mut out = vec![0u8; msg.len()];
    #[allow(clippy::cast_sign_loss)]
    let ic = counter as u64;
    // SAFETY: lengths checked/allocated.
    let rc = unsafe {
        ffi::crypto_stream_xchacha20_xor_ic(
            out.as_mut_ptr(),
            ptr(msg),
            msg.len() as u64,
            nonce.as_ptr(),
            ic,
            key.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_stream_xchacha20_keygen()`.
pub fn stream_xchacha20_keygen() -> Vec<u8> {
    random(KEYBYTES)
}
