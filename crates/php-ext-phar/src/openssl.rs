//! OpenSSL signatures for phar archives through `php-openssl` (what
//! `phar_create_signature()` / `phar_verify_signature()` do with
//! `openssl_sign()` / `openssl_verify()`).

use php_openssl::asym::DigestAlgo;
use php_openssl::constants::OPENSSL_RSA_PSS_SALTLEN_AUTO;
use php_openssl::{KeyArg, OpenSsl};
use php_phar::signature::OpenSslHook;

/// The [`OpenSslHook`] installed by the extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hook;

impl OpenSslHook for Hook {
    fn sign(&self, data: &[u8], private_key: &[u8], algo: i64) -> Option<Vec<u8>> {
        let mut ossl = OpenSsl::new();
        ossl.sign(
            data,
            &KeyArg::Str(private_key),
            DigestAlgo::Id(algo),
            0,
            OPENSSL_RSA_PSS_SALTLEN_AUTO,
        )
        .ok()
        .flatten()
    }

    fn verify(&self, data: &[u8], signature: &[u8], public_key: &[u8], algo: i64) -> bool {
        let mut ossl = OpenSsl::new();
        matches!(
            ossl.verify(
                data,
                signature,
                &KeyArg::Str(public_key),
                DigestAlgo::Id(algo),
                0,
                OPENSSL_RSA_PSS_SALTLEN_AUTO,
            ),
            Ok(Some(1))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_keys_fail() {
        assert!(Hook.sign(b"data", b"not a key", 1).is_none());
        assert!(!Hook.verify(b"data", b"sig", b"not a key", 1));
    }
}
