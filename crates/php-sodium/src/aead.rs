// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_crypto_aead_*`: AES-256-GCM, ChaCha20-Poly1305 (original and
//! IETF) and XChaCha20-Poly1305-IETF.

#![allow(unsafe_code)]

use std::ffi::c_int;

use crate::error::{arg, check_len, exc, internal, overflow, Result};
use crate::{ffi, init, ptr, random, ZSTR_MAX_LEN};

type EncryptFn = unsafe extern "C" fn(
    *mut u8,
    *mut u64,
    *const u8,
    u64,
    *const u8,
    u64,
    *const u8,
    *const u8,
    *const u8,
) -> c_int;
type DecryptFn = unsafe extern "C" fn(
    *mut u8,
    *mut u64,
    *mut u8,
    *const u8,
    u64,
    *const u8,
    u64,
    *const u8,
    *const u8,
) -> c_int;

/// How a construction limits message sizes.
#[derive(Clone, Copy)]
enum Limit {
    /// No per-key limit beyond `ZSTR_MAX_LEN`.
    None,
    /// AES-256-GCM: 16·(2^32−2) bytes; decrypt reports "is too long".
    Gcm,
    /// ChaCha20-Poly1305-IETF: 64·2^32−64 bytes.
    Ietf,
    /// XChaCha20-Poly1305-IETF: as IETF, but decrypt reports an overflow.
    XIetf,
}

struct Aead {
    /// The PHP function prefix, e.g. `sodium_crypto_aead_aes256gcm`.
    func: &'static str,
    /// The constant prefix, e.g. `SODIUM_CRYPTO_AEAD_AES256GCM`.
    konst: &'static str,
    npub: usize,
    key: usize,
    abytes: usize,
    limit: Limit,
    enc: EncryptFn,
    dec: DecryptFn,
}

const GCM_MAX: u64 = 16 * ((1 << 32) - 2);
const IETF_MAX: u64 = 64 * (1 << 32) - 64;

const AES256GCM: Aead = Aead {
    func: "sodium_crypto_aead_aes256gcm",
    konst: "SODIUM_CRYPTO_AEAD_AES256GCM",
    npub: 12,
    key: 32,
    abytes: 16,
    limit: Limit::Gcm,
    enc: ffi::crypto_aead_aes256gcm_encrypt,
    dec: ffi::crypto_aead_aes256gcm_decrypt,
};
const CHACHA20POLY1305: Aead = Aead {
    func: "sodium_crypto_aead_chacha20poly1305",
    konst: "SODIUM_CRYPTO_AEAD_CHACHA20POLY1305",
    npub: 8,
    key: 32,
    abytes: 16,
    limit: Limit::None,
    enc: ffi::crypto_aead_chacha20poly1305_encrypt,
    dec: ffi::crypto_aead_chacha20poly1305_decrypt,
};
const CHACHA20POLY1305_IETF: Aead = Aead {
    func: "sodium_crypto_aead_chacha20poly1305_ietf",
    konst: "SODIUM_CRYPTO_AEAD_CHACHA20POLY1305_IETF",
    npub: 12,
    key: 32,
    abytes: 16,
    limit: Limit::Ietf,
    enc: ffi::crypto_aead_chacha20poly1305_ietf_encrypt,
    dec: ffi::crypto_aead_chacha20poly1305_ietf_decrypt,
};
const XCHACHA20POLY1305_IETF: Aead = Aead {
    func: "sodium_crypto_aead_xchacha20poly1305_ietf",
    konst: "SODIUM_CRYPTO_AEAD_XCHACHA20POLY1305_IETF",
    npub: 24,
    key: 32,
    abytes: 16,
    limit: Limit::XIetf,
    enc: ffi::crypto_aead_xchacha20poly1305_ietf_encrypt,
    dec: ffi::crypto_aead_xchacha20poly1305_ietf_decrypt,
};

impl Aead {
    fn check(&self, func: &str, nonce: &[u8], key: &[u8]) -> Result<()> {
        let k = self.konst;
        check_len(
            func,
            3,
            "nonce",
            nonce,
            self.npub,
            &format!("{k}_NPUBBYTES"),
        )?;
        check_len(func, 4, "key", key, self.key, &format!("{k}_KEYBYTES"))
    }

    fn encrypt(&self, msg: &[u8], ad: &[u8], nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        init();
        let func = format!("{}_encrypt", self.func);
        self.check(&func, nonce, key)?;
        if ZSTR_MAX_LEN - msg.len() <= self.abytes {
            return Err(overflow());
        }
        let mlen = msg.len() as u64;
        let too_long = match self.limit {
            Limit::Gcm => mlen > GCM_MAX - self.abytes as u64,
            Limit::Ietf => mlen > IETF_MAX,
            Limit::None | Limit::XIetf => false,
        };
        if too_long {
            return Err(exc("message too long for a single key"));
        }
        let mut out = vec![0u8; msg.len() + self.abytes];
        let mut real = 0u64;
        // SAFETY: `out` has room for the message plus the tag; nonce and key
        // lengths were checked; `nsec` is unused (NULL).
        let rc = unsafe {
            (self.enc)(
                out.as_mut_ptr(),
                &raw mut real,
                ptr(msg),
                mlen,
                ptr(ad),
                ad.len() as u64,
                std::ptr::null(),
                nonce.as_ptr(),
                key.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(internal());
        }
        let real = usize::try_from(real).map_err(|_| overflow())?;
        if real == 0 || real >= ZSTR_MAX_LEN || real > out.len() {
            return Err(overflow());
        }
        out.truncate(real);
        Ok(out)
    }

    fn decrypt(&self, ct: &[u8], ad: &[u8], nonce: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>> {
        init();
        let func = format!("{}_decrypt", self.func);
        self.check(&func, nonce, key)?;
        if ct.len() < self.abytes {
            return Ok(None);
        }
        let body = (ct.len() - self.abytes) as u64;
        match self.limit {
            Limit::Gcm if body > GCM_MAX => return Err(arg(&func, 1, "ciphertext", "is too long")),
            Limit::Ietf if body > IETF_MAX => return Err(exc("message too long for a single key")),
            Limit::XIetf if body > IETF_MAX => return Err(overflow()),
            _ => {}
        }
        if ct.len() >= ZSTR_MAX_LEN {
            return Err(overflow());
        }
        let mut out = vec![0u8; ct.len()];
        let mut real = 0u64;
        // SAFETY: `out` is at least as long as the plaintext can be.
        let rc = unsafe {
            (self.dec)(
                out.as_mut_ptr(),
                &raw mut real,
                std::ptr::null_mut(),
                ct.as_ptr(),
                ct.len() as u64,
                ptr(ad),
                ad.len() as u64,
                nonce.as_ptr(),
                key.as_ptr(),
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
        Ok(Some(out))
    }
}

/// `sodium_crypto_aead_aes256gcm_is_available()`: whether the CPU has the
/// AES-NI/PCLMUL (or `ARMv8` crypto) support libsodium requires.
pub fn aes256gcm_is_available() -> bool {
    init();
    // SAFETY: no preconditions after sodium_init().
    unsafe { ffi::crypto_aead_aes256gcm_is_available() == 1 }
}

/// `sodium_crypto_aead_aes256gcm_encrypt($message, $additional_data, $nonce, $key)`.
pub fn aes256gcm_encrypt(msg: &[u8], ad: &[u8], nonce: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    AES256GCM.encrypt(msg, ad, nonce, key)
}

/// `sodium_crypto_aead_aes256gcm_decrypt()`; `None` is PHP's `false`.
pub fn aes256gcm_decrypt(
    ct: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    AES256GCM.decrypt(ct, ad, nonce, key)
}

/// `sodium_crypto_aead_aes256gcm_keygen()`.
pub fn aes256gcm_keygen() -> Vec<u8> {
    random(AES256GCM.key)
}

/// `sodium_crypto_aead_chacha20poly1305_encrypt()`.
pub fn chacha20poly1305_encrypt(
    msg: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Vec<u8>> {
    CHACHA20POLY1305.encrypt(msg, ad, nonce, key)
}

/// `sodium_crypto_aead_chacha20poly1305_decrypt()`.
pub fn chacha20poly1305_decrypt(
    ct: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    CHACHA20POLY1305.decrypt(ct, ad, nonce, key)
}

/// `sodium_crypto_aead_chacha20poly1305_keygen()`.
pub fn chacha20poly1305_keygen() -> Vec<u8> {
    random(CHACHA20POLY1305.key)
}

/// `sodium_crypto_aead_chacha20poly1305_ietf_encrypt()`.
pub fn chacha20poly1305_ietf_encrypt(
    msg: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Vec<u8>> {
    CHACHA20POLY1305_IETF.encrypt(msg, ad, nonce, key)
}

/// `sodium_crypto_aead_chacha20poly1305_ietf_decrypt()`.
pub fn chacha20poly1305_ietf_decrypt(
    ct: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    CHACHA20POLY1305_IETF.decrypt(ct, ad, nonce, key)
}

/// `sodium_crypto_aead_chacha20poly1305_ietf_keygen()`.
pub fn chacha20poly1305_ietf_keygen() -> Vec<u8> {
    random(CHACHA20POLY1305_IETF.key)
}

/// `sodium_crypto_aead_xchacha20poly1305_ietf_encrypt()`.
pub fn xchacha20poly1305_ietf_encrypt(
    msg: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Vec<u8>> {
    XCHACHA20POLY1305_IETF.encrypt(msg, ad, nonce, key)
}

/// `sodium_crypto_aead_xchacha20poly1305_ietf_decrypt()`.
pub fn xchacha20poly1305_ietf_decrypt(
    ct: &[u8],
    ad: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    XCHACHA20POLY1305_IETF.decrypt(ct, ad, nonce, key)
}

/// `sodium_crypto_aead_xchacha20poly1305_ietf_keygen()`.
pub fn xchacha20poly1305_ietf_keygen() -> Vec<u8> {
    random(XCHACHA20POLY1305_IETF.key)
}
