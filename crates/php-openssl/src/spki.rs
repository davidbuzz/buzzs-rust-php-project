//! Netscape SPKI (`<keygen>`): `openssl_spki_*`.

use crate::args::{AsymmetricKey, KeyArg};
use crate::config::md_from_algo;
use crate::error::PhpResult;
use crate::ffi::{Bio, Spki};
use crate::OpenSsl;

/// `php_openssl_spki_cleanup`: drops CR/LF (the C loop stops at NUL).
/// Returns the cleaned bytes and the length PHP computes
/// (`input length - removed`).
fn spki_cleanup(s: &[u8]) -> (Vec<u8>, usize) {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    let cleaned: Vec<u8> = s[..end]
        .iter()
        .copied()
        .filter(|&b| b != b'\n' && b != b'\r')
        .collect();
    let removed = end - cleaned.len();
    (cleaned, s.len() - removed)
}

impl OpenSsl {
    /// Decodes an SPKAC for verify/export; `None` after warning.
    fn spki_decode(&mut self, spkac: &[u8], decode_msg: &str) -> Option<Spki> {
        let (cleaned, len) = spki_cleanup(spkac);
        if len == 0 {
            self.warn("Invalid SPKAC");
            return None;
        }
        let spki = Spki::decode(&cleaned);
        if spki.is_none() {
            self.store_errors();
            self.warn(decode_msg);
        }
        spki
    }

    /// `openssl_spki_new()`: `"SPKAC=..."` or `None` (false).
    pub fn spki_new(
        &mut self,
        private_key: &AsymmetricKey,
        challenge: &[u8],
        digest_algo: i64,
    ) -> PhpResult<Option<Vec<u8>>> {
        // PHP passes the challenge as the (unused) passphrase.
        let Some(pkey) =
            self.pkey_from_arg(&KeyArg::Key(private_key), false, Some(challenge), 1)?
        else {
            self.warn("Unable to use supplied private key");
            return Ok(None);
        };
        let Some(md) = md_from_algo(digest_algo) else {
            self.warn("Unknown digest algorithm");
            return Ok(None);
        };
        let Some(spki) = Spki::new() else {
            self.store_errors();
            self.warn("Unable to create new SPKAC");
            return Ok(None);
        };
        let steps: [(&dyn Fn() -> bool, &str); 3] = [
            (
                &|| spki.set_challenge(challenge),
                "Unable to set challenge data",
            ),
            (&|| spki.set_pubkey(&pkey), "Unable to embed public key"),
            (
                &|| spki.sign(&pkey, &md),
                "Unable to sign with specified digest algorithm",
            ),
        ];
        for (step, msg) in steps {
            if !step() {
                self.store_errors();
                self.warn(msg);
                return Ok(None);
            }
        }
        let Some(b64) = spki.encode() else {
            self.store_errors();
            self.warn("Unable to encode SPKAC");
            return Ok(None);
        };
        let mut out = b"SPKAC=".to_vec();
        out.extend_from_slice(&b64);
        Ok(Some(out))
    }

    /// `openssl_spki_verify()`.
    pub fn spki_verify(&mut self, spkac: &[u8]) -> bool {
        let mut i = 0;
        if let Some(spki) = self.spki_decode(spkac, "Unable to decode supplied SPKAC") {
            if let Some(k) = spki.public_key() {
                i = spki.verify(&k);
            } else {
                self.store_errors();
                self.warn("Unable to acquire signed public key");
            }
        }
        if i > 0 {
            true
        } else {
            self.store_errors();
            false
        }
    }

    /// `openssl_spki_export()`: the public key PEM or `None` (false).
    pub fn spki_export(&mut self, spkac: &[u8]) -> Option<Vec<u8>> {
        let spki = self.spki_decode(spkac, "Unable to decode supplied SPKAC")?;
        let Some(k) = spki.public_key() else {
            self.store_errors();
            self.warn("Unable to acquire signed public key");
            return None;
        };
        let pem = Bio::mem()
            .filter(|b| k.write_public(b))
            .map(|b| b.contents());
        if pem.is_none() {
            self.store_errors();
        }
        pem
    }

    /// `openssl_spki_export_challenge()`.
    pub fn spki_export_challenge(&mut self, spkac: &[u8]) -> Option<Vec<u8>> {
        let spki = self.spki_decode(spkac, "Unable to decode SPKAC")?;
        Some(spki.challenge())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup() {
        assert_eq!(spki_cleanup(b"ab\r\ncd"), (b"abcd".to_vec(), 4));
        assert_eq!(spki_cleanup(b"\n"), (Vec::new(), 0));
        assert_eq!(spki_cleanup(b"a\0\nb"), (b"a".to_vec(), 4));
    }
}
