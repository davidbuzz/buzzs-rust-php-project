// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_crypto_pwhash*` (Argon2i/Argon2id), the scrypt variants, and
//! the argon2 backend of `password_hash()` that ext/sodium registers when
//! the core was built without libargon2 (`sodium_pwhash.c`).

#![allow(unsafe_code)]

use std::ffi::c_char;

use crate::error::{arg, exc, internal, value_arg, Error, Result};
use crate::{c_string, ffi, init, ZSTR_MAX_LEN};

const SALTBYTES: usize = 16;
const STRBYTES: usize = 128;
const OPSLIMIT_MIN: i64 = 1;
const OPSLIMIT_MAX: i64 = 4_294_967_295;
const MEMLIMIT_MIN: i64 = 8192;
const MEMLIMIT_MAX: i64 = 4_398_046_510_080;
const ALG_ARGON2I13: i64 = 1;
const ALG_ARGON2ID13: i64 = 2;
const SCRYPT_SALTBYTES: usize = 32;
const SCRYPT_STRBYTES: usize = 102;
const SCRYPT_OPSLIMIT_INTERACTIVE: i64 = 524_288;
const SCRYPT_MEMLIMIT_INTERACTIVE: i64 = 16_777_216;

/// The `E_WARNING` for an empty password (raised without a function prefix).
pub const EMPTY_PASSWORD: &str = "empty password";
/// The `E_WARNING` of `..._scryptsalsa208sha256_str_verify()` for a hash
/// of the wrong length.
pub const WRONG_HASH_SIZE: &str = "wrong size for the hashed password";

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn as_usize(v: i64) -> usize {
    v as usize
}

#[allow(clippy::cast_sign_loss)]
fn as_u64(v: i64) -> u64 {
    v as u64
}

/// `sodium_crypto_pwhash($length, $password, $salt, $opslimit, $memlimit, $algo)`.
pub fn pwhash(
    length: i64,
    password: &[u8],
    salt: &[u8],
    opslimit: i64,
    memlimit: i64,
    algo: i64,
    warn: &mut dyn FnMut(&str),
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_pwhash";
    if length <= 0 {
        return Err(value_arg(f, 1, "length", "must be greater than 0"));
    }
    if length >= 0xffff_ffff {
        return Err(value_arg(
            f,
            1,
            "length",
            "must be less than 4294967295 bytes",
        ));
    }
    if password.len() >= 0xffff_ffff {
        return Err(value_arg(
            f,
            2,
            "password",
            "must be less than 4294967295 bytes",
        ));
    }
    if opslimit <= 0 {
        return Err(value_arg(f, 4, "opslimit", "must be greater than 0"));
    }
    if memlimit <= 0 {
        return Err(value_arg(f, 5, "memlimit", "must be greater than 0"));
    }
    if algo != ALG_ARGON2I13 && algo != ALG_ARGON2ID13 {
        return Err(exc("unsupported password hashing algorithm"));
    }
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    if salt.len() != SALTBYTES {
        return Err(value_arg(
            f,
            3,
            "salt",
            "must be SODIUM_CRYPTO_PWHASH_SALTBYTES bytes long",
        ));
    }
    if opslimit < OPSLIMIT_MIN {
        return Err(value_arg(
            f,
            4,
            "opslimit",
            &format!("must be greater than or equal to {OPSLIMIT_MIN}"),
        ));
    }
    if memlimit < MEMLIMIT_MIN {
        return Err(value_arg(
            f,
            5,
            "memlimit",
            &format!("must be greater than or equal to {MEMLIMIT_MIN}"),
        ));
    }
    let len = as_usize(length);
    let mut out = vec![0u8; len];
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY: `out` holds `len` bytes; salt length checked; the password is
    // passed with its length.
    let rc = unsafe {
        ffi::crypto_pwhash(
            out.as_mut_ptr(),
            len as u64,
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
            salt.as_ptr(),
            as_u64(opslimit),
            as_usize(memlimit),
            algo as i32,
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// Runs a `*_str()` hasher into a `STRBYTES` buffer and trims at the NUL.
fn str_hash(size: usize, f: impl FnOnce(*mut c_char) -> i32) -> Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    if f(out.as_mut_ptr().cast::<c_char>()) != 0 {
        return Err(internal());
    }
    let end = out.iter().position(|&b| b == 0).unwrap_or(size - 1);
    out.truncate(end.min(size - 1));
    Ok(out)
}

/// `sodium_crypto_pwhash_str($password, $opslimit, $memlimit)`.
pub fn pwhash_str(
    password: &[u8],
    opslimit: i64,
    memlimit: i64,
    warn: &mut dyn FnMut(&str),
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_pwhash_str";
    if opslimit <= 0 {
        return Err(value_arg(f, 2, "opslimit", "must be greater than 0"));
    }
    if memlimit <= 0 {
        return Err(value_arg(f, 3, "memlimit", "must be greater than 0"));
    }
    if password.len() >= 0xffff_ffff {
        return Err(value_arg(
            f,
            1,
            "password",
            "must be less than 4294967295 bytes",
        ));
    }
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    if opslimit < OPSLIMIT_MIN {
        return Err(value_arg(
            f,
            2,
            "opslimit",
            &format!("must be greater than or equal to {OPSLIMIT_MIN}"),
        ));
    }
    if memlimit < MEMLIMIT_MIN {
        return Err(value_arg(
            f,
            3,
            "memlimit",
            &format!("must be greater than or equal to {MEMLIMIT_MIN}"),
        ));
    }
    // SAFETY: STRBYTES output buffer; password passed with its length.
    str_hash(STRBYTES, |out| unsafe {
        ffi::crypto_pwhash_str(
            out,
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
            as_u64(opslimit),
            as_usize(memlimit),
        )
    })
}

/// `sodium_crypto_pwhash_str_needs_rehash($password, $opslimit, $memlimit)`
/// (the first parameter is the *hash*, despite its PHP name).
pub fn pwhash_str_needs_rehash(hash: &[u8], opslimit: i64, memlimit: i64) -> bool {
    init();
    let hash = c_string(hash);
    // SAFETY: `hash` is NUL-terminated.
    unsafe {
        ffi::crypto_pwhash_str_needs_rehash(
            hash.as_ptr().cast::<c_char>(),
            as_u64(opslimit),
            as_usize(memlimit),
        ) != 0
    }
}

/// `sodium_crypto_pwhash_str_verify($hash, $password)`.
pub fn pwhash_str_verify(hash: &[u8], password: &[u8], warn: &mut dyn FnMut(&str)) -> Result<bool> {
    init();
    if password.len() >= 0xffff_ffff {
        return Err(arg(
            "sodium_crypto_pwhash_str_verify",
            2,
            "password",
            "is too long",
        ));
    }
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    let hash = c_string(hash);
    // SAFETY: `hash` is NUL-terminated; the password is passed with its length.
    Ok(unsafe {
        ffi::crypto_pwhash_str_verify(
            hash.as_ptr().cast::<c_char>(),
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
        )
    } == 0)
}

/// `sodium_crypto_pwhash_scryptsalsa208sha256($length, $password, $salt, $opslimit, $memlimit)`.
pub fn scryptsalsa208sha256(
    length: i64,
    password: &[u8],
    salt: &[u8],
    opslimit: i64,
    memlimit: i64,
    warn: &mut dyn FnMut(&str),
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_pwhash_scryptsalsa208sha256";
    if length <= 0 || as_usize(length) >= ZSTR_MAX_LEN || length > 0x1f_ffff_ffe0 {
        return Err(value_arg(f, 1, "length", "must be greater than 0"));
    }
    if opslimit <= 0 {
        return Err(value_arg(f, 4, "opslimit", "must be greater than 0"));
    }
    if memlimit <= 0 {
        return Err(value_arg(f, 5, "memlimit", "must be greater than 0"));
    }
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    if salt.len() != SCRYPT_SALTBYTES {
        return Err(value_arg(
            f,
            3,
            "salt",
            "must be SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_SALTBYTES bytes long",
        ));
    }
    if opslimit < SCRYPT_OPSLIMIT_INTERACTIVE {
        return Err(value_arg(
            f,
            4,
            "opslimit",
            &format!("must be greater than or equal to {SCRYPT_OPSLIMIT_INTERACTIVE}"),
        ));
    }
    if memlimit < SCRYPT_MEMLIMIT_INTERACTIVE {
        return Err(value_arg(
            f,
            5,
            "memlimit",
            &format!("must be greater than or equal to {SCRYPT_MEMLIMIT_INTERACTIVE}"),
        ));
    }
    let len = as_usize(length);
    let mut out = vec![0u8; len];
    // SAFETY: `out` holds `len` bytes; salt length checked.
    let rc = unsafe {
        ffi::crypto_pwhash_scryptsalsa208sha256(
            out.as_mut_ptr(),
            len as u64,
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
            salt.as_ptr(),
            as_u64(opslimit),
            as_usize(memlimit),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    Ok(out)
}

/// `sodium_crypto_pwhash_scryptsalsa208sha256_str($password, $opslimit, $memlimit)`.
pub fn scryptsalsa208sha256_str(
    password: &[u8],
    opslimit: i64,
    memlimit: i64,
    warn: &mut dyn FnMut(&str),
) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_pwhash_scryptsalsa208sha256_str";
    if opslimit <= 0 {
        return Err(value_arg(f, 2, "opslimit", "must be greater than 0"));
    }
    if memlimit <= 0 {
        return Err(value_arg(f, 3, "memlimit", "must be greater than 0"));
    }
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    if opslimit < SCRYPT_OPSLIMIT_INTERACTIVE {
        return Err(value_arg(
            f,
            2,
            "opslimit",
            &format!("must be greater than or equal to {SCRYPT_OPSLIMIT_INTERACTIVE}"),
        ));
    }
    if memlimit < SCRYPT_MEMLIMIT_INTERACTIVE {
        return Err(value_arg(
            f,
            3,
            "memlimit",
            &format!("must be greater than or equal to {SCRYPT_MEMLIMIT_INTERACTIVE}"),
        ));
    }
    // PHP keeps the full STRBYTES - 1 length (the hash is NUL-padded only
    // in theory: scrypt strings always fill it).
    let mut out = vec![0u8; SCRYPT_STRBYTES];
    // SAFETY: STRBYTES output buffer; password passed with its length.
    let rc = unsafe {
        ffi::crypto_pwhash_scryptsalsa208sha256_str(
            out.as_mut_ptr().cast::<c_char>(),
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
            as_u64(opslimit),
            as_usize(memlimit),
        )
    };
    if rc != 0 {
        return Err(internal());
    }
    out.truncate(SCRYPT_STRBYTES - 1);
    Ok(out)
}

/// `sodium_crypto_pwhash_scryptsalsa208sha256_str_verify($hash, $password)`.
pub fn scryptsalsa208sha256_str_verify(
    hash: &[u8],
    password: &[u8],
    warn: &mut dyn FnMut(&str),
) -> bool {
    init();
    if password.is_empty() {
        warn(EMPTY_PASSWORD);
    }
    if hash.len() != SCRYPT_STRBYTES - 1 {
        warn(WRONG_HASH_SIZE);
        return false;
    }
    let hash = c_string(hash);
    // SAFETY: `hash` is NUL-terminated; password passed with its length.
    unsafe {
        ffi::crypto_pwhash_scryptsalsa208sha256_str_verify(
            hash.as_ptr().cast::<c_char>(),
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
        ) == 0
    }
}

/// The `password_hash()` options ext/sodium understands (already
/// converted with `zval_get_long()` by the caller; `None` when absent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Argon2Options {
    /// `memory_cost`, in KiB.
    pub memory_cost: Option<i64>,
    /// `time_cost`.
    pub time_cost: Option<i64>,
    /// `threads`.
    pub threads: Option<i64>,
}

/// The Argon2 variant behind `PASSWORD_ARGON2I` / `PASSWORD_ARGON2ID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argon2 {
    /// `argon2i`.
    I,
    /// `argon2id`.
    Id,
}

/// `get_options()`: (opslimit, memlimit in bytes) or the `ValueError`.
fn argon2_limits(options: &Argon2Options) -> Result<(u64, usize)> {
    let mut opslimit = crate::consts::PASSWORD_ARGON2_DEFAULT_TIME_COST;
    let mut memlimit = crate::consts::PASSWORD_ARGON2_DEFAULT_MEMORY_COST << 10;
    if let Some(m) = options.memory_cost {
        if !(MEMLIMIT_MIN >> 10..=MEMLIMIT_MAX >> 10).contains(&m) {
            return Err(Error::Value(
                "Memory cost is outside of allowed memory range".into(),
            ));
        }
        memlimit = m << 10;
    }
    if let Some(t) = options.time_cost {
        if !(OPSLIMIT_MIN..=OPSLIMIT_MAX).contains(&t) {
            return Err(Error::Value(
                "Time cost is outside of allowed time range".into(),
            ));
        }
        opslimit = t;
    }
    if options.threads.is_some_and(|t| t != 1) {
        return Err(Error::Value(
            "A thread value other than 1 is not supported by this implementation".into(),
        ));
    }
    Ok((as_u64(opslimit), as_usize(memlimit)))
}

/// The `hash` hook of the `argon2i`/`argon2id` password algorithms.
pub fn argon2_hash(password: &[u8], options: &Argon2Options, alg: Argon2) -> Result<Vec<u8>> {
    init();
    if password.len() >= 0xffff_ffff {
        return Err(Error::Value("Password is too long".into()));
    }
    let (opslimit, memlimit) = argon2_limits(options)?;
    let alg = match alg {
        Argon2::I => 1,
        Argon2::Id => 2,
    };
    // SAFETY: STRBYTES output buffer; password passed with its length.
    str_hash(STRBYTES, |out| unsafe {
        ffi::crypto_pwhash_str_alg(
            out,
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
            opslimit,
            memlimit,
            alg,
        )
    })
    .map_err(|_| Error::Value("Unexpected failure hashing password".into()))
}

/// The `verify` hook.
pub fn argon2_verify(password: &[u8], hash: &[u8]) -> bool {
    init();
    if password.len() >= 0xffff_ffff || hash.len() >= 0xffff_ffff {
        return false;
    }
    let hash = c_string(hash);
    // SAFETY: `hash` is NUL-terminated; password passed with its length.
    unsafe {
        ffi::crypto_pwhash_str_verify(
            hash.as_ptr().cast::<c_char>(),
            password.as_ptr().cast::<c_char>(),
            password.len() as u64,
        ) == 0
    }
}

/// The `needs_rehash` hook (true when the options are invalid).
pub fn argon2_needs_rehash(hash: &[u8], options: &Argon2Options) -> bool {
    init();
    let Ok((opslimit, memlimit)) = argon2_limits(options) else {
        return true;
    };
    let hash = c_string(hash);
    // SAFETY: `hash` is NUL-terminated.
    unsafe {
        ffi::crypto_pwhash_str_needs_rehash(hash.as_ptr().cast::<c_char>(), opslimit, memlimit) != 0
    }
}

/// What `password_get_info()` reports in `options` for an argon2 hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Info {
    /// `memory_cost`.
    pub memory_cost: i64,
    /// `time_cost`.
    pub time_cost: i64,
    /// `threads`.
    pub threads: i64,
}

/// `sscanf()`'s `%ld`: optional whitespace and sign, then digits.
fn scan_long(s: &[u8]) -> Option<(i64, &[u8])> {
    let s = s.trim_ascii_start();
    let (neg, rest) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut v: i64 = 0;
    for &d in &rest[..digits] {
        v = v.saturating_mul(10).saturating_add(i64::from(d - b'0'));
    }
    Some((if neg { v.saturating_neg() } else { v }, &rest[digits..]))
}

/// The `get_info` hook: `None` when the hash is not an argon2 one.
pub fn argon2_get_info(hash: &[u8]) -> Option<Argon2Info> {
    if hash.len() < "$argon2id$".len() + 1 {
        return None;
    }
    let mut p = if let Some(rest) = hash.strip_prefix(b"$argon2i$") {
        rest
    } else {
        hash.strip_prefix(b"$argon2id$")?
    };
    let mut info = Argon2Info {
        memory_cost: crate::consts::PASSWORD_ARGON2_DEFAULT_MEMORY_COST,
        time_cost: crate::consts::PASSWORD_ARGON2_DEFAULT_TIME_COST,
        threads: 1,
    };
    // "v=%ld$m=%ld,t=%ld,p=%ld", stopping at the first mismatch.
    let mut fields = [None::<i64>; 4];
    for (i, lit) in [&b"v="[..], b"$m=", b",t=", b",p="].into_iter().enumerate() {
        let Some(rest) = p.strip_prefix(lit) else {
            break;
        };
        let Some((v, rest)) = scan_long(rest) else {
            break;
        };
        fields[i] = Some(v);
        p = rest;
    }
    if let Some(m) = fields[1] {
        info.memory_cost = m;
    }
    if let Some(t) = fields[2] {
        info.time_cost = t;
    }
    if let Some(t) = fields[3] {
        info.threads = t;
    }
    Some(info)
}
