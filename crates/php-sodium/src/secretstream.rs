//! `sodium_crypto_secretstream_xchacha20poly1305_*`.  The stream state is
//! the raw 52-byte `crypto_secretstream_xchacha20poly1305_state`, carried
//! in a PHP string that the push/pull/rekey functions update in place.

#![allow(unsafe_code)]

use crate::error::{arg, check_len, exc, internal, overflow, Result};
use crate::{ffi, init, ptr, random, ZSTR_MAX_LEN};

/// `sizeof(crypto_secretstream_xchacha20poly1305_state)`.
pub const STATE_BYTES: usize = 52;
const ABYTES: usize = 17;
const HEADERBYTES: usize = 24;
const KEYBYTES: usize = 32;
const MESSAGEBYTES_MAX: u64 = 274_877_906_816;

const F: &str = "sodium_crypto_secretstream_xchacha20poly1305";

/// `..._keygen()`.
pub fn keygen() -> Vec<u8> {
    random(KEYBYTES)
}

/// `..._init_push($key)`: `[state, header]`.
pub fn init_push(key: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    init();
    check_len(
        &format!("{F}_init_push"),
        1,
        "key",
        key,
        KEYBYTES,
        "SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_KEYBYTES",
    )?;
    let mut state = vec![0u8; STATE_BYTES];
    let mut header = vec![0u8; HEADERBYTES];
    // SAFETY: buffers sized per the primitive; key length checked.
    let rc = unsafe {
        ffi::crypto_secretstream_xchacha20poly1305_init_push(
            state.as_mut_ptr(),
            header.as_mut_ptr(),
            key.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok((state, header))
}

/// `..._push(&$state, $message, $additional_data = "", $tag = TAG_MESSAGE)`.
pub fn push(state: &mut [u8], msg: &[u8], ad: &[u8], tag: i64) -> Result<Vec<u8>> {
    init();
    let f = format!("{F}_push");
    if state.len() != STATE_BYTES {
        return Err(arg(&f, 1, "state", "must have a correct length"));
    }
    if msg.len() as u64 > MESSAGEBYTES_MAX || msg.len() > ZSTR_MAX_LEN - ABYTES {
        return Err(arg(
            &f,
            2,
            "message",
            "must be at most SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_MESSAGEBYTES_MAX bytes long",
        ));
    }
    let Ok(tag) = u8::try_from(tag) else {
        return Err(arg(&f, 4, "tag", "must be in the range of 0-255"));
    };
    let mut out = vec![0u8; msg.len() + ABYTES];
    let mut real = 0u64;
    // SAFETY: state length checked; `out` holds message + ABYTES.
    let rc = unsafe {
        ffi::crypto_secretstream_xchacha20poly1305_push(
            state.as_mut_ptr(),
            out.as_mut_ptr(),
            &raw mut real,
            ptr(msg),
            msg.len() as u64,
            ptr(ad),
            ad.len() as u64,
            tag,
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    let real = usize::try_from(real).map_err(|_| overflow())?;
    if real == 0 || real > out.len() {
        return Err(overflow());
    }
    out.truncate(real);
    Ok(out)
}

/// `..._init_pull($header, $key)`: the state.
pub fn init_pull(header: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = format!("{F}_init_pull");
    check_len(
        &f,
        1,
        "header",
        header,
        HEADERBYTES,
        "SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_HEADERBYTES",
    )?;
    check_len(
        &f,
        2,
        "key",
        key,
        KEYBYTES,
        "SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_KEYBYTES",
    )?;
    let mut state = vec![0u8; STATE_BYTES];
    // SAFETY: sizes checked/allocated.
    let rc = unsafe {
        ffi::crypto_secretstream_xchacha20poly1305_init_pull(
            state.as_mut_ptr(),
            header.as_ptr(),
            key.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(state)
}

/// `..._pull(&$state, $ciphertext, $additional_data = "")`:
/// `[message, tag]`, or `None` (PHP's `false`) when authentication fails.
pub fn pull(state: &mut [u8], ct: &[u8], ad: &[u8]) -> Result<Option<(Vec<u8>, i64)>> {
    init();
    if state.len() != STATE_BYTES {
        return Err(exc("incorrect state length"));
    }
    if ct.len() < ABYTES {
        return Ok(None);
    }
    let mut out = vec![0u8; ct.len() - ABYTES];
    let mut real = 0u64;
    let mut tag = 0u8;
    // SAFETY: state length checked; `out` holds the plaintext size.
    let rc = unsafe {
        ffi::crypto_secretstream_xchacha20poly1305_pull(
            state.as_mut_ptr(),
            out.as_mut_ptr(),
            &raw mut real,
            &raw mut tag,
            ct.as_ptr(),
            ct.len() as u64,
            ptr(ad),
            ad.len() as u64,
        )
    };
    if rc != 0 {
        return Ok(None);
    }
    let real = usize::try_from(real).map_err(|_| overflow())?;
    if real > out.len() {
        return Err(overflow());
    }
    out.truncate(real);
    Ok(Some((out, i64::from(tag))))
}

/// `..._rekey(&$state)`.
pub fn rekey(state: &mut [u8]) -> Result<()> {
    init();
    if state.len() != STATE_BYTES {
        return Err(exc("incorrect state length"));
    }
    // SAFETY: state length checked.
    unsafe { ffi::crypto_secretstream_xchacha20poly1305_rekey(state.as_mut_ptr()) };
    Ok(())
}
