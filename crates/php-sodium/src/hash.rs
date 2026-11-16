// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_crypto_auth*` (HMAC-SHA512-256), `sodium_crypto_shorthash*`
//! (SipHash-2-4), `sodium_crypto_generichash*` (`BLAKE2b`) and
//! `sodium_crypto_kdf*`.

#![allow(unsafe_code)]

use std::ffi::c_char;

use crate::error::{arg, check_len, exc, internal, Result};
use crate::{ffi, init, ptr, random};

const AUTH_BYTES: usize = 32;
const AUTH_KEYBYTES: usize = 32;
const SHORTHASH_BYTES: usize = 8;
const SHORTHASH_KEYBYTES: usize = 16;
const GENERICHASH_BYTES_MIN: i64 = 16;
const GENERICHASH_BYTES_MAX: i64 = 64;
const GENERICHASH_KEYBYTES: usize = 32;
const GENERICHASH_KEYBYTES_MIN: usize = 16;
const GENERICHASH_KEYBYTES_MAX: usize = 64;
const KDF_BYTES_MIN: i64 = 16;
const KDF_BYTES_MAX: i64 = 64;
const KDF_CONTEXTBYTES: usize = 8;
const KDF_KEYBYTES: usize = 32;

/// `sizeof(crypto_generichash_state)`: the length of the opaque state
/// strings `sodium_crypto_generichash_init()` returns.
pub const GENERICHASH_STATE_BYTES: usize = 384;

/// `crypto_generichash_state` with its 64-byte alignment.
#[repr(C, align(64))]
struct State([u8; GENERICHASH_STATE_BYTES]);

impl State {
    fn zeroed() -> Self {
        Self([0; GENERICHASH_STATE_BYTES])
    }
    fn wipe(&mut self) {
        // SAFETY: the array is valid for its size.
        unsafe { ffi::sodium_memzero(self.0.as_mut_ptr(), GENERICHASH_STATE_BYTES) };
    }
}

/// `sodium_crypto_auth($message, $key)`.
pub fn auth(msg: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_auth",
        2,
        "key",
        key,
        AUTH_KEYBYTES,
        "SODIUM_CRYPTO_AUTH_KEYBYTES",
    )?;
    let mut mac = vec![0u8; AUTH_BYTES];
    // SAFETY: sizes checked/allocated per the primitive's constants.
    if unsafe { ffi::crypto_auth(mac.as_mut_ptr(), ptr(msg), msg.len() as u64, key.as_ptr()) } != 0
    {
        return Err(internal());
    }
    Ok(mac)
}

/// `sodium_crypto_auth_verify($mac, $message, $key)`.
pub fn auth_verify(mac: &[u8], msg: &[u8], key: &[u8]) -> Result<bool> {
    init();
    let f = "sodium_crypto_auth_verify";
    check_len(
        f,
        3,
        "key",
        key,
        AUTH_KEYBYTES,
        "SODIUM_CRYPTO_AUTH_KEYBYTES",
    )?;
    check_len(f, 1, "mac", mac, AUTH_BYTES, "SODIUM_CRYPTO_AUTH_BYTES")?;
    // SAFETY: sizes checked.
    Ok(
        unsafe { ffi::crypto_auth_verify(mac.as_ptr(), ptr(msg), msg.len() as u64, key.as_ptr()) }
            == 0,
    )
}

/// `sodium_crypto_auth_keygen()`.
pub fn auth_keygen() -> Vec<u8> {
    random(AUTH_KEYBYTES)
}

/// `sodium_crypto_shorthash($message, $key)`.
pub fn shorthash(msg: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    init();
    check_len(
        "sodium_crypto_shorthash",
        2,
        "key",
        key,
        SHORTHASH_KEYBYTES,
        "SODIUM_CRYPTO_SHORTHASH_KEYBYTES",
    )?;
    let mut out = vec![0u8; SHORTHASH_BYTES];
    // SAFETY: sizes checked/allocated.
    if unsafe { ffi::crypto_shorthash(out.as_mut_ptr(), ptr(msg), msg.len() as u64, key.as_ptr()) }
        != 0
    {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_shorthash_keygen()`.
pub fn shorthash_keygen() -> Vec<u8> {
    random(SHORTHASH_KEYBYTES)
}

fn check_output_length(length: i64) -> Result<usize> {
    if !(GENERICHASH_BYTES_MIN..=GENERICHASH_BYTES_MAX).contains(&length) {
        return Err(exc("unsupported output length"));
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Ok(length as usize)
}

fn check_key(key: &[u8]) -> Result<()> {
    if !key.is_empty()
        && !(GENERICHASH_KEYBYTES_MIN..=GENERICHASH_KEYBYTES_MAX).contains(&key.len())
    {
        return Err(exc("unsupported key length"));
    }
    Ok(())
}

/// `sodium_crypto_generichash($message, $key = "", $length = 32)`.
pub fn generichash(msg: &[u8], key: &[u8], length: i64) -> Result<Vec<u8>> {
    init();
    let len = check_output_length(length)?;
    check_key(key)?;
    let mut out = vec![0u8; len];
    // SAFETY: `out` holds `len` bytes; key length is in range (or 0).
    let rc = unsafe {
        ffi::crypto_generichash(
            out.as_mut_ptr(),
            len,
            ptr(msg),
            msg.len() as u64,
            if key.is_empty() {
                std::ptr::null()
            } else {
                key.as_ptr()
            },
            key.len(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_generichash_keygen()`.
pub fn generichash_keygen() -> Vec<u8> {
    random(GENERICHASH_KEYBYTES)
}

/// `sodium_crypto_generichash_init($key = "", $length = 32)`: the opaque
/// [`GENERICHASH_STATE_BYTES`]-byte state string.
pub fn generichash_init(key: &[u8], length: i64) -> Result<Vec<u8>> {
    init();
    let len = check_output_length(length)?;
    check_key(key)?;
    let mut state = State::zeroed();
    // SAFETY: `state` is a correctly aligned crypto_generichash_state.
    let rc = unsafe {
        ffi::crypto_generichash_init(
            state.0.as_mut_ptr(),
            if key.is_empty() {
                std::ptr::null()
            } else {
                key.as_ptr()
            },
            key.len(),
            len,
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    let out = state.0.to_vec();
    state.wipe();
    Ok(out)
}

fn load_state(bytes: &[u8]) -> Result<State> {
    if bytes.len() != GENERICHASH_STATE_BYTES {
        return Err(exc("incorrect state length"));
    }
    let mut state = State::zeroed();
    state.0.copy_from_slice(bytes);
    Ok(state)
}

/// `sodium_crypto_generichash_update(&$state, $message)`.
pub fn generichash_update(state: &mut [u8], msg: &[u8]) -> Result<()> {
    init();
    let mut tmp = load_state(state)?;
    // SAFETY: `tmp` is an aligned state copy.
    let rc =
        unsafe { ffi::crypto_generichash_update(tmp.0.as_mut_ptr(), ptr(msg), msg.len() as u64) };
    if rc != 0 {
        tmp.wipe();
        return Err(internal());
    }
    state.copy_from_slice(&tmp.0);
    tmp.wipe();
    Ok(())
}

/// `sodium_crypto_generichash_final(&$state, $length = 32)`: on success
/// the state is wiped (PHP then sets the variable to `null`).
pub fn generichash_final(state: &mut [u8], length: i64) -> Result<Vec<u8>> {
    init();
    let mut tmp = load_state(state)?;
    let len = check_output_length(length)?;
    let mut out = vec![0u8; len];
    // SAFETY: `tmp` is an aligned state copy; `out` holds `len` bytes.
    let rc = unsafe { ffi::crypto_generichash_final(tmp.0.as_mut_ptr(), out.as_mut_ptr(), len) };
    tmp.wipe();
    if rc != 0 {
        return Err(internal());
    }
    // SAFETY: `state` is valid for its length.
    unsafe { ffi::sodium_memzero(state.as_mut_ptr(), state.len()) };
    Ok(out)
}

/// `sodium_crypto_kdf_derive_from_key($subkey_length, $subkey_id, $context, $key)`.
pub fn kdf_derive_from_key(
    subkey_length: i64,
    subkey_id: i64,
    context: &[u8],
    key: &[u8],
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_kdf_derive_from_key";
    if subkey_length < KDF_BYTES_MIN {
        return Err(arg(
            f,
            1,
            "subkey_length",
            "must be greater than or equal to SODIUM_CRYPTO_KDF_BYTES_MIN",
        ));
    }
    if subkey_length > KDF_BYTES_MAX {
        return Err(arg(
            f,
            1,
            "subkey_length",
            "must be less than or equal to SODIUM_CRYPTO_KDF_BYTES_MAX",
        ));
    }
    if subkey_id < 0 {
        return Err(arg(f, 2, "subkey_id", "must be greater than or equal to 0"));
    }
    check_len(
        f,
        3,
        "context",
        context,
        KDF_CONTEXTBYTES,
        "SODIUM_CRYPTO_KDF_CONTEXTBYTES",
    )?;
    check_len(f, 4, "key", key, KDF_KEYBYTES, "SODIUM_CRYPTO_KDF_KEYBYTES")?;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let len = subkey_length as usize;
    let mut out = vec![0u8; len];
    // SAFETY: lengths checked; `out` holds `len` bytes.
    unsafe {
        ffi::crypto_kdf_derive_from_key(
            out.as_mut_ptr(),
            len,
            subkey_id.unsigned_abs(),
            context.as_ptr().cast::<c_char>(),
            key.as_ptr(),
        );
    }
    Ok(out)
}

/// `sodium_crypto_kdf_keygen()`.
pub fn kdf_keygen() -> Vec<u8> {
    random(KDF_KEYBYTES)
}
