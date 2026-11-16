// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Signature creation and verification (`phar_create_signature()`,
//! `phar_verify_signature()` in `util.c`).
//!
//! Hash signatures use `php-hash`; OpenSSL signatures go through the
//! [`OpenSslHook`] the embedder installs (PHP itself calls the userland
//! `openssl_sign()` / `openssl_verify()` functions when phar is built
//! without direct OpenSSL support, which is what this mirrors).

use std::cell::RefCell;
use std::rc::Rc;

use crate::consts::{
    SIG_MD5, SIG_OPENSSL, SIG_OPENSSL_SHA256, SIG_OPENSSL_SHA512, SIG_SHA1, SIG_SHA256, SIG_SHA512,
};
use crate::util::hex_upper;

/// What phar needs from ext/openssl.
pub trait OpenSslHook {
    /// `openssl_sign($data, $sig, $private_key, $algo)`; `None` on failure.
    /// `algo` is `OPENSSL_ALGO_SHA1` (1), `_SHA256` (7) or `_SHA512` (9).
    fn sign(&self, data: &[u8], private_key: &[u8], algo: i64) -> Option<Vec<u8>>;
    /// `openssl_verify($data, $sig, $public_key, $algo) === 1`.
    fn verify(&self, data: &[u8], signature: &[u8], public_key: &[u8], algo: i64) -> bool;
}

thread_local! {
    static OPENSSL: RefCell<Option<Rc<dyn OpenSslHook>>> = const { RefCell::new(None) };
}

/// Installs (or removes) the OpenSSL hook. Without a hook OpenSSL
/// signatures are reported as unavailable ("openssl not loaded").
pub fn set_openssl_hook(hook: Option<Rc<dyn OpenSslHook>>) {
    OPENSSL.with(|h| *h.borrow_mut() = hook);
}

/// Whether OpenSSL signatures are available.
pub fn openssl_available() -> bool {
    OPENSSL.with(|h| h.borrow().is_some())
}

fn openssl() -> Option<Rc<dyn OpenSslHook>> {
    OPENSSL.with(|h| h.borrow().clone())
}

/// The `OPENSSL_ALGO_*` used for a phar OpenSSL signature type.
pub fn openssl_algo(sig_type: u32) -> i64 {
    match sig_type {
        SIG_OPENSSL_SHA512 => 9,
        SIG_OPENSSL_SHA256 => 7,
        _ => 1,
    }
}

/// Whether `sig_type` is one of the OpenSSL signature types.
pub fn is_openssl(sig_type: u32) -> bool {
    matches!(
        sig_type,
        SIG_OPENSSL | SIG_OPENSSL_SHA256 | SIG_OPENSSL_SHA512
    )
}

fn digest(algo: &[u8], data: &[u8]) -> Vec<u8> {
    php_hash::hash(algo, data, &php_hash::HashOptions::default())
        .map(|d| d.bytes)
        .unwrap_or_default()
}

/// The `getSignature()['hash_type']` name.
pub fn type_name(sig_type: u32) -> &'static str {
    match sig_type {
        SIG_MD5 => "MD5",
        SIG_SHA1 => "SHA-1",
        SIG_SHA256 => "SHA-256",
        SIG_SHA512 => "SHA-512",
        SIG_OPENSSL => "OpenSSL",
        SIG_OPENSSL_SHA256 => "OpenSSL_SHA256",
        SIG_OPENSSL_SHA512 => "OpenSSL_SHA512",
        _ => "Unknown",
    }
}

/// `Phar::getSupportedSignatures()`.
pub fn supported_signatures() -> Vec<&'static str> {
    let mut v = vec!["MD5", "SHA-1", "SHA-256", "SHA-512"];
    if openssl_available() {
        v.extend(["OpenSSL", "OpenSSL_SHA256", "OpenSSL_SHA512"]);
    }
    v
}

/// Reads `<fname>.pubkey` for OpenSSL verification.
fn read_pubkey(fname: &[u8]) -> Option<Vec<u8>> {
    let mut p = fname.to_vec();
    p.extend_from_slice(b".pubkey");
    let key = crate::sys::read_file(&p).ok()?;
    (!key.is_empty()).then_some(key)
}

/// `phar_verify_signature()`: checks `sig` over `data` (everything before
/// the signature). Returns the upper-case hex signature on success, or the
/// error text.
pub fn verify(sig_type: u32, data: &[u8], sig: &[u8], fname: &[u8]) -> Result<Vec<u8>, String> {
    let check = |algo: &[u8], len: usize, short: &str, mismatch: &str| {
        if sig.len() < len {
            return Err(short.to_string());
        }
        let d = digest(algo, data);
        if d[..] != sig[..len] {
            return Err(mismatch.to_string());
        }
        Ok(hex_upper(&d))
    };
    match sig_type {
        SIG_OPENSSL | SIG_OPENSSL_SHA256 | SIG_OPENSSL_SHA512 => {
            let Some(hook) = openssl() else {
                return Err("openssl not loaded".into());
            };
            let Some(key) = read_pubkey(fname) else {
                return Err("openssl public key could not be read".into());
            };
            if !hook.verify(data, sig, &key, openssl_algo(sig_type)) {
                return Err("openssl signature could not be verified".into());
            }
            Ok(hex_upper(sig))
        }
        SIG_SHA512 => check(
            b"sha512",
            64,
            "broken openssl signature",
            "broken openssl signature",
        ),
        SIG_SHA256 => check(
            b"sha256",
            32,
            "broken openssl signature",
            "broken signature",
        ),
        SIG_SHA1 => check(b"sha1", 20, "broken signature", "broken signature"),
        SIG_MD5 => check(b"md5", 16, "broken signature", "broken signature"),
        _ => Err("broken or unsupported signature".into()),
    }
}

/// `phar_create_signature()`: signs `data`. Unknown types (including 0)
/// become SHA-256, so `sig_type` is updated in place. Returns the raw
/// signature; `private_key` is only used for OpenSSL types.
pub fn create(
    sig_type: &mut u32,
    data: &[u8],
    private_key: &[u8],
    fname: &[u8],
) -> Result<Vec<u8>, String> {
    match *sig_type {
        SIG_SHA512 => Ok(digest(b"sha512", data)),
        SIG_OPENSSL | SIG_OPENSSL_SHA256 | SIG_OPENSSL_SHA512 => openssl()
            .and_then(|h| h.sign(data, private_key, openssl_algo(*sig_type)))
            .ok_or_else(|| {
                format!(
                    "unable to write phar \"{}\" with requested openssl signature",
                    crate::util::s(fname)
                )
            }),
        SIG_SHA1 => Ok(digest(b"sha1", data)),
        SIG_MD5 => Ok(digest(b"md5", data)),
        _ => {
            *sig_type = SIG_SHA256;
            Ok(digest(b"sha256", data))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_signatures() {
        let mut t = SIG_SHA1;
        let sig = create(&mut t, b"abc", b"", b"x").unwrap();
        assert_eq!(
            verify(SIG_SHA1, b"abc", &sig, b"x").unwrap(),
            b"A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        assert_eq!(
            verify(SIG_SHA1, b"abd", &sig, b"x").unwrap_err(),
            "broken signature"
        );
        let mut t = 0;
        let sig = create(&mut t, b"abc", b"", b"x").unwrap();
        assert_eq!(t, SIG_SHA256);
        assert_eq!(sig.len(), 32);
        assert_eq!(
            verify(SIG_SHA512, b"abc", b"short", b"x").unwrap_err(),
            "broken openssl signature"
        );
        assert_eq!(
            verify(99, b"abc", b"", b"x").unwrap_err(),
            "broken or unsupported signature"
        );
        assert_eq!(
            verify(SIG_OPENSSL, b"abc", b"", b"x").unwrap_err(),
            "openssl not loaded"
        );
    }
}
