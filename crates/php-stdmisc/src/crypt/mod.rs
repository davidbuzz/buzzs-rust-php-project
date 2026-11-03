//! `crypt()` (`ext/standard/crypt.c` with the bundled `php_crypt_r`
//! implementations) and the password hashing API (`ext/standard/password.c`).
//!
//! All algorithms are pure Rust ports: traditional/extended DES
//! ([`des`]), MD5 (`$1$`), SHA-256/512 (`$5$`/`$6$`, [`shacrypt`]), bcrypt
//! ([`blowfish`]) and Argon2 ([`argon2`]).

pub mod argon2;
mod bf_tables;
pub mod blowfish;
pub mod des;
pub mod password;
pub mod shacrypt;

/// `PHP_MAX_SALT_LEN` (the `CRYPT_SALT_LENGTH` constant).
pub const MAX_SALT_LEN: usize = 123;

/// The `CRYPT_*` constants (all algorithms are always available).
pub const CONSTANTS: &[(&str, i64)] = &[
    ("CRYPT_SALT_LENGTH", 123),
    ("CRYPT_STD_DES", 1),
    ("CRYPT_EXT_DES", 1),
    ("CRYPT_MD5", 1),
    ("CRYPT_BLOWFISH", 1),
    ("CRYPT_SHA256", 1),
    ("CRYPT_SHA512", 1),
];

fn is_valid_salt_character(c: u8) -> bool {
    (b'.'..=b'9').contains(&c) || c.is_ascii_alphabetic()
}

/// Cuts a byte string at its first NUL (C string semantics).
fn c_str(s: &[u8]) -> &[u8] {
    s.split(|&c| c == 0).next().unwrap_or_default()
}

/// `php_crypt()`: hashes `password` with `salt` (both treated as C strings,
/// the salt limited to [`MAX_SALT_LEN`] bytes). `None` means failure.
#[must_use]
pub fn php_crypt(password: &[u8], salt: &[u8]) -> Option<Vec<u8>> {
    let salt = c_str(&salt[..salt.len().min(MAX_SALT_LEN)]);
    let pw = c_str(password);
    let at = |i: usize| salt.get(i).copied().unwrap_or(0);
    if at(0) == b'*' && (at(1) == b'0' || at(1) == b'1') {
        return None;
    }
    let out = if salt.starts_with(b"$1$") {
        Some(shacrypt::md5_crypt(pw, salt))
    } else if salt.starts_with(b"$6$") {
        shacrypt::sha512_crypt(pw, salt)
    } else if salt.starts_with(b"$5$") {
        shacrypt::sha256_crypt(pw, salt)
    } else if at(0) == b'$' && at(1) == b'2' && at(2) != 0 && at(3) == b'$' {
        blowfish::crypt_blowfish(pw, salt)
    } else if at(0) == b'_' || (is_valid_salt_character(at(0)) && is_valid_salt_character(at(1))) {
        des::crypt_extended(pw, salt)
    } else {
        None
    };
    // The output buffer is PHP_MAX_SALT_LEN bytes including the NUL.
    out.filter(|o| o.len() < MAX_SALT_LEN)
}

/// The `crypt($string, $salt)` function: on failure returns `"*0"`, or
/// `"*1"` when the salt itself starts with `"*0"`.
#[must_use]
pub fn crypt(string: &[u8], salt: &[u8]) -> Vec<u8> {
    if let Some(r) = php_crypt(string, salt) {
        return r;
    }
    if salt[..salt.len().min(MAX_SALT_LEN)].starts_with(b"*0") {
        b"*1".to_vec()
    } else {
        b"*0".to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::crypt;

    #[test]
    fn failure_tokens() {
        assert_eq!(crypt(b"x", b"*0"), b"*1");
        assert_eq!(crypt(b"x", b"*1"), b"*0");
        assert_eq!(crypt(b"x", b""), b"*0");
        assert_eq!(crypt(b"x", b"$2y$03$abcdefghijklmnopqrstuu"), b"*0");
        assert_eq!(crypt(b"x", b"\x00a"), b"*0");
    }

    #[test]
    fn php_doc_examples() {
        assert_eq!(crypt(b"rasmuslerdorf", b"rl"), b"rl.3StKT.4T8M");
        assert_eq!(
            crypt(b"rasmuslerdorf", b"_J9..rasm"),
            b"_J9..rasmBYk8r9AiWNc"
        );
        assert_eq!(
            crypt(b"rasmuslerdorf", b"$2a$07$usesomesillystringforsalt$"),
            b"$2a$07$usesomesillystringfore2uDLvp1Ii2e./U9C8sBjqp8I90dH6hi".to_vec()
        );
    }
}
