//! Authentication plugin primitives (ref: `mysqlnd_auth.c`):
//! `mysql_native_password`, `caching_sha2_password` (fast path scramble +
//! RSA full authentication), `sha256_password` and `mysql_clear_password`.

use openssl::pkey::{PKey, Public};
use openssl::rsa::Padding;

use crate::consts::SCRAMBLE_LENGTH;

/// The authentication plugins mysqlnd ships (`auth_plugin_*` registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plugin {
    /// `mysql_native_password`
    Native,
    /// `mysql_clear_password` (the "PAM" plugin)
    ClearPassword,
    /// `caching_sha2_password`
    CachingSha2,
    /// `sha256_password`
    Sha256,
}

impl Plugin {
    /// `fetch_auth_plugin_by_name`
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "mysql_native_password" => Some(Self::Native),
            "mysql_clear_password" => Some(Self::ClearPassword),
            "caching_sha2_password" => Some(Self::CachingSha2),
            "sha256_password" => Some(Self::Sha256),
            _ => None,
        }
    }

    /// The protocol name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "mysql_native_password",
            Self::ClearPassword => "mysql_clear_password",
            Self::CachingSha2 => "caching_sha2_password",
            Self::Sha256 => "sha256_password",
        }
    }
}

fn sha1(d: &[u8]) -> [u8; 20] {
    php_hash::sha1(d)
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let data: Vec<u8> = parts.concat();
    let d = php_hash::hash(b"sha256", &data, &php_hash::HashOptions::default())
        .map(|d| d.bytes)
        .unwrap_or_default();
    let mut out = [0u8; 32];
    if d.len() == 32 {
        out.copy_from_slice(&d);
    }
    out
}

/// `php_mysqlnd_scramble`: `SHA1(pw) XOR SHA1(scramble + SHA1(SHA1(pw)))`.
#[must_use]
pub fn scramble_native(scramble: &[u8], password: &[u8]) -> [u8; 20] {
    let s1 = sha1(password);
    let s2 = sha1(&s1);
    let mut buf = Vec::with_capacity(40);
    buf.extend_from_slice(&scramble[..SCRAMBLE_LENGTH.min(scramble.len())]);
    buf.extend_from_slice(&s2);
    let mut out = sha1(&buf);
    for (o, a) in out.iter_mut().zip(s1.iter()) {
        *o ^= a;
    }
    out
}

/// `php_mysqlnd_scramble_sha2`: `SHA256(pw) XOR SHA256(SHA256(SHA256(pw)) + scramble)`.
#[must_use]
pub fn scramble_sha2(scramble: &[u8], password: &[u8]) -> [u8; 32] {
    let h1 = sha256(&[password]);
    let h2 = sha256(&[&h1]);
    let mut out = sha256(&[&h2, &scramble[..SCRAMBLE_LENGTH.min(scramble.len())]]);
    for (o, a) in out.iter_mut().zip(h1.iter()) {
        *o ^= a;
    }
    out
}

/// `mysqlnd_xor_string` over `password + "\0"` with the 20-byte nonce.
#[must_use]
pub fn xor_password(password: &[u8], scramble: &[u8]) -> Vec<u8> {
    let mut v = password.to_vec();
    v.push(0);
    let n = SCRAMBLE_LENGTH.min(scramble.len()).max(1);
    for (i, b) in v.iter_mut().enumerate() {
        *b ^= scramble.get(i % n).copied().unwrap_or(0);
    }
    v
}

/// Parses a PEM `PUBLIC KEY` (`PEM_read_bio_PUBKEY`).
#[must_use]
pub fn parse_public_key(pem: &[u8]) -> Option<PKey<Public>> {
    PKey::public_key_from_pem(pem).ok()
}

/// Why RSA encryption of the password failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsaError {
    /// The key size is not positive (`"invalid public key size"`).
    InvalidKeySize,
    /// `RSA_PKCS1_OAEP_PADDING` can't fit the password (`"password is too long"`).
    PasswordTooLong,
    /// OpenSSL failed.
    Failed,
}

/// `mysqlnd_sha256_public_encrypt` / `mysqlnd_caching_sha2_public_encrypt`:
/// RSA-OAEP encrypts `xor_password(password, scramble)`.
pub fn rsa_encrypt_password(
    key: &PKey<Public>,
    password: &[u8],
    scramble: &[u8],
) -> Result<Vec<u8>, RsaError> {
    let size = key.size();
    if size == 0 {
        return Err(RsaError::InvalidKeySize);
    }
    if size <= password.len() + 41 {
        return Err(RsaError::PasswordTooLong);
    }
    let rsa = key.rsa().map_err(|_| RsaError::Failed)?;
    let data = xor_password(password, scramble);
    let mut out = vec![0u8; rsa.size() as usize];
    let n = rsa
        .public_encrypt(&data, &mut out, Padding::PKCS1_OAEP)
        .map_err(|_| RsaError::Failed)?;
    out.truncate(n);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        php_hash::bin2hex(b)
    }

    #[test]
    fn native_known_answer() {
        let scr: Vec<u8> = (1..=20).collect();
        assert_eq!(
            hex(&scramble_native(&scr, b"secret")),
            "b32bb3a583e1340c0a1108d58b1be49781ad8c2f"
        );
    }

    #[test]
    fn sha2_known_answer() {
        let scr: Vec<u8> = (1..=20).collect();
        assert_eq!(
            hex(&scramble_sha2(&scr, b"secret")),
            "746ebe205d56a0707acb3e796e834e0dd7b1d61743b26bd5202c7a623230c7c9"
        );
    }

    #[test]
    fn xor_includes_nul() {
        let scr = [1u8; 20];
        assert_eq!(xor_password(b"ab", &scr), vec![b'a' ^ 1, b'b' ^ 1, 1]);
    }

    #[test]
    fn rsa_roundtrip() {
        let rsa = openssl::rsa::Rsa::generate(2048).unwrap();
        let pem = rsa.public_key_to_pem().unwrap();
        let key = parse_public_key(&pem).unwrap();
        let scr: Vec<u8> = (1..=20).collect();
        let enc = rsa_encrypt_password(&key, b"secret", &scr).unwrap();
        assert_eq!(enc.len(), 256);
        let mut dec = vec![0u8; 256];
        let n = rsa
            .private_decrypt(&enc, &mut dec, Padding::PKCS1_OAEP)
            .unwrap();
        assert_eq!(&dec[..n], xor_password(b"secret", &scr).as_slice());
        let long = vec![b'x'; 300];
        assert_eq!(
            rsa_encrypt_password(&key, &long, &scr),
            Err(RsaError::PasswordTooLong)
        );
    }

    #[test]
    fn plugin_names() {
        assert_eq!(
            Plugin::by_name("caching_sha2_password"),
            Some(Plugin::CachingSha2)
        );
        assert_eq!(Plugin::by_name("client_ed25519"), None);
        assert_eq!(Plugin::Native.name(), "mysql_native_password");
    }
}
