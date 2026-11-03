//! PHP's `ext/sodium` for phprust, bound to the system libsodium (1.0.18).
//!
//! Every `sodium_*` PHP function maps to a Rust function of the same name
//! without the `sodium_` prefix, grouped by family:
//!
//! * [`util`] — `sodium_memzero/increment/add/memcmp/compare`,
//!   `sodium_bin2hex/hex2bin/bin2base64/base642bin`, `sodium_pad/unpad`.
//! * [`aead`] — `crypto_aead_{aes256gcm,chacha20poly1305,
//!   chacha20poly1305_ietf,xchacha20poly1305_ietf}_*`.
//! * [`hash`] — `crypto_auth*`, `crypto_shorthash*`, `crypto_generichash*`
//!   (incl. the opaque-string streaming state), `crypto_kdf*`.
//! * [`boxes`] — `crypto_box*` (incl. seal), `crypto_kx*`,
//!   `crypto_scalarmult*`.
//! * [`secretbox`] — `crypto_secretbox*`, `crypto_stream*` (incl.
//!   `XChaCha20` and `xor_ic`).
//! * [`sign`] — `crypto_sign*` (incl. detached, seed keypairs and the
//!   Ed25519→Curve25519 conversions).
//! * [`secretstream`] — `crypto_secretstream_xchacha20poly1305_*`.
//! * [`pwhash`] — `crypto_pwhash*` (Argon2i/Argon2id and
//!   scrypt-salsa208-sha256) and the argon2 `password_hash()` backend
//!   ext/sodium registers when the core lacks libargon2.
//! * [`ristretto`] — `crypto_core_ristretto255_*`,
//!   `crypto_scalarmult_ristretto255*`.
//! * [`consts`] — the constants and the library version.
//!
//! # Conventions
//!
//! Strings are bytes in, bytes out.  Failures are [`Error`] values with
//! PHP's complete message (`SodiumException` or `ValueError`); the VM
//! still performs ordinary parameter parsing (types, arity) itself.
//! `string|false` results are `Option`s; array results are tuples.
//! Functions taking a string by reference (`sodium_add`, `sodium_increment`,
//! `sodium_memzero`, the generichash/secretstream `$state`) take
//! `&mut Vec<u8>`/`&mut [u8]`; the VM separates the string first, raises
//! [`error::not_a_state_reference`] / [`error::STRING_REQUIRED`] when the
//! variable is not a string, and, where PHP does, assigns `null` afterwards
//! (`sodium_memzero`, `sodium_crypto_generichash_final`).
//!
//! The few `E_WARNING`s (`"empty password"`, `"wrong size for the hashed
//! password"`) are raised by PHP with `zend_error()`, i.e. *without* a
//! `function(): ` prefix; they are handed to a `warn` callback.
//!
//! # Availability
//!
//! libsodium 1.0.18 has no AEGIS, `IPCrypt`, XOF (SHAKE/`TurboSHAKE`) or
//! ML-KEM, so — exactly like php-src built against it — those functions and
//! constants do not exist.  AES-256-GCM is compiled in (x86-64/aarch64) but
//! only usable when [`aead::aes256gcm_is_available`] says so.

mod ffi;

pub mod aead;
pub mod boxes;
pub mod consts;
pub mod error;
pub mod hash;
pub mod pwhash;
pub mod ristretto;
pub mod secretbox;
pub mod secretstream;
pub mod sign;
pub mod util;

pub use error::{Error, Result};

use std::sync::Once;

/// `sodium_init()` (PHP's `MINIT`), run once before any other call.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: sodium_init() is thread-safe and idempotent; a negative
        // result (no entropy source) makes PHP refuse to start.
        let rc = unsafe { ffi::sodium_init() };
        assert!(rc >= 0, "sodium_init() failed");
        // The opaque state strings mirror these C structs byte for byte; a
        // library with other layouts would make them unsound to use.
        // SAFETY: plain size queries.
        let sizes = unsafe {
            (
                ffi::crypto_generichash_statebytes(),
                ffi::crypto_secretstream_xchacha20poly1305_statebytes(),
            )
        };
        assert_eq!(
            sizes,
            (hash::GENERICHASH_STATE_BYTES, secretstream::STATE_BYTES),
            "unexpected libsodium state sizes"
        );
    });
}

/// `randombytes_buf()` into a fresh buffer of `len` bytes.
pub(crate) fn random(len: usize) -> Vec<u8> {
    init();
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` is valid for `len` writable bytes.
    unsafe { ffi::randombytes_buf(buf.as_mut_ptr(), len) };
    buf
}

/// `ZSTR_MAX_LEN` on 64-bit builds (the `zend_string` header is 24 bytes).
pub(crate) const ZSTR_MAX_LEN: usize = usize::MAX - 24 - 8;

/// A pointer for an optional input buffer: libsodium accepts NULL for
/// zero-length inputs, and `[].as_ptr()` is a dangling (but non-null)
/// pointer which it never dereferences.
pub(crate) fn ptr(buf: &[u8]) -> *const u8 {
    buf.as_ptr()
}

/// A NUL-terminated copy of `bytes`, cut at the first NUL like a PHP
/// string handed to a `char *` C API.
pub(crate) fn c_string(bytes: &[u8]) -> Vec<u8> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let mut v = Vec::with_capacity(end + 1);
    v.extend_from_slice(&bytes[..end]);
    v.push(0);
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn helpers() {
        assert_eq!(super::c_string(b"ab\0cd"), b"ab\0");
        assert_eq!(super::c_string(b""), b"\0");
        assert_eq!(super::random(17).len(), 17);
        assert_ne!(super::random(32), super::random(32));
    }
}
