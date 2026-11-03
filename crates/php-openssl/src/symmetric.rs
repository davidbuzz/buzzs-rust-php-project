//! Symmetric ciphers, digests, PBKDF2, randomness and algorithm listings.

use crate::constants::{OPENSSL_DONT_ZERO_PAD_KEY, OPENSSL_RAW_DATA, OPENSSL_ZERO_PADDING};
use crate::error::{PhpResult, Throw};
use crate::ffi::{self, cipher_mode, Cipher, CipherCtx};
use crate::util::{base64_decode, base64_encode, bin2hex};
use crate::value::{PhpArray, PhpValue};
use crate::OpenSsl;

/// `struct php_openssl_cipher_mode`.
#[allow(clippy::struct_excessive_bools)] // mirrors the C struct
#[derive(Debug, Clone, Copy, Default)]
struct CipherMode {
    is_aead: bool,
    is_single_run_aead: bool,
    set_tag_length_always: bool,
    set_tag_length_when_encrypting: bool,
    aad_supports_vector: bool,
}

impl CipherMode {
    /// `php_openssl_load_cipher_mode`.
    fn load(cipher: &Cipher) -> Self {
        let mode = cipher.mode();
        let mut m = CipherMode::default();
        match mode {
            cipher_mode::GCM | cipher_mode::CCM | cipher_mode::SIV | cipher_mode::OCB => {
                m.set_tag_length_always = mode == cipher_mode::OCB;
                m.is_aead = true;
                m.set_tag_length_when_encrypting = mode == cipher_mode::CCM;
                m.is_single_run_aead = mode == cipher_mode::CCM;
                m.aad_supports_vector = mode == cipher_mode::SIV;
            }
            _ => {
                if cipher.nid() == ffi::NID_CHACHA20_POLY1305 {
                    m.is_aead = true;
                }
            }
        }
        m
    }
}

/// The by-reference `$tag` result of `openssl_encrypt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagOut {
    /// `$tag` untouched (not passed, or the call failed).
    Untouched,
    /// `$tag = null` (non-AEAD cipher).
    Null,
    /// The authentication tag.
    Tag(Vec<u8>),
}

/// Result of [`OpenSsl::encrypt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encrypted {
    /// Ciphertext (base64 unless `OPENSSL_RAW_DATA`).
    pub data: Vec<u8>,
    /// What to assign to `$tag`.
    pub tag: TagOut,
}

/// Arguments of `openssl_encrypt`.
#[derive(Debug, Clone, Copy)]
pub struct EncryptArgs<'a> {
    /// `$data`.
    pub data: &'a [u8],
    /// `$cipher_algo`.
    pub method: &'a [u8],
    /// `$passphrase` (the key).
    pub passphrase: &'a [u8],
    /// `$options`.
    pub options: i64,
    /// `$iv`.
    pub iv: &'a [u8],
    /// Whether `$tag` was passed (argument count ≥ 6).
    pub tag_requested: bool,
    /// `$aad` (`None` = PHP `null`).
    pub aad: Option<&'a [u8]>,
    /// `$tag_length`.
    pub tag_length: i64,
}

/// Arguments of `openssl_decrypt`.
#[derive(Debug, Clone, Copy)]
pub struct DecryptArgs<'a> {
    /// `$data`.
    pub data: &'a [u8],
    /// `$cipher_algo`.
    pub method: &'a [u8],
    /// `$passphrase`.
    pub passphrase: &'a [u8],
    /// `$options`.
    pub options: i64,
    /// `$iv`.
    pub iv: &'a [u8],
    /// `$tag` (`None` = null).
    pub tag: Option<&'a [u8]>,
    /// `$aad` (`None` = null).
    pub aad: Option<&'a [u8]>,
}

impl OpenSsl {
    /// `php_openssl_validate_iv`; returns the IV to use.
    fn validate_iv(
        &mut self,
        iv: &[u8],
        required: usize,
        ctx: &CipherCtx,
        mode: CipherMode,
    ) -> Option<Vec<u8>> {
        if mode.is_aead {
            if ctx.ctrl(
                ffi::EVP_CTRL_AEAD_SET_IVLEN,
                i32::try_from(iv.len()).unwrap_or(i32::MAX),
            ) <= 0
            {
                self.warn("Setting of IV length for AEAD mode failed");
                return None;
            }
            return Some(iv.to_vec());
        }
        if iv.len() == required {
            return Some(iv.to_vec());
        }
        let mut out = vec![0u8; required];
        if iv.is_empty() {
            return Some(out);
        }
        if iv.len() < required {
            self.warn(format!(
                "IV passed is only {} bytes long, cipher expects an IV of precisely {} bytes, padding with \\0",
                iv.len(),
                required
            ));
            out[..iv.len()].copy_from_slice(iv);
            return Some(out);
        }
        self.warn(format!(
            "IV passed is {} bytes long which is longer than the {} expected by selected cipher, truncating",
            iv.len(),
            required
        ));
        out.copy_from_slice(&iv[..required]);
        Some(out)
    }

    /// `php_openssl_cipher_init`.
    #[allow(clippy::too_many_arguments)]
    fn cipher_init(
        &mut self,
        cipher: &Cipher,
        ctx: &CipherCtx,
        mode: CipherMode,
        password: &[u8],
        iv: &[u8],
        tag: Option<&[u8]>,
        tag_len: i32,
        options: i64,
        enc: bool,
    ) -> bool {
        let max_iv_len = usize::try_from(cipher.iv_length()).unwrap_or(0);
        if enc && iv.is_empty() && max_iv_len > 0 && !mode.is_aead {
            self.warn("Using an empty Initialization Vector (iv) is potentially insecure and not recommended");
        }
        if !ctx.init(Some(cipher), None, None, i32::from(enc)) {
            self.store_errors();
            return false;
        }
        let Some(iv) = self.validate_iv(iv, max_iv_len, ctx, mode) else {
            return false;
        };
        if (mode.set_tag_length_always || (enc && mode.set_tag_length_when_encrypting))
            && ctx.ctrl(ffi::EVP_CTRL_AEAD_SET_TAG, tag_len) <= 0
        {
            self.warn("Setting tag length for AEAD cipher failed");
            return false;
        }
        if !enc {
            if let Some(t) = tag.filter(|t| !t.is_empty()) {
                if !mode.is_aead {
                    self.warn(
                        "The tag cannot be used because the cipher algorithm does not support AEAD",
                    );
                } else if ctx.ctrl_set_tag(ffi::EVP_CTRL_AEAD_SET_TAG, t) <= 0 {
                    self.warn("Setting tag for AEAD cipher decryption failed");
                    return false;
                }
            }
        }
        let key_len = usize::try_from(cipher.key_length()).unwrap_or(0);
        let key: Vec<u8> = if key_len > password.len() {
            if options & OPENSSL_DONT_ZERO_PAD_KEY != 0
                && !ctx.set_key_length(i32::try_from(password.len()).unwrap_or(i32::MAX))
            {
                self.store_errors();
                self.warn("Key length cannot be set for the cipher algorithm");
                return false;
            }
            let mut k = vec![0u8; key_len];
            k[..password.len()].copy_from_slice(password);
            k
        } else {
            if password.len() > key_len
                && !ctx.set_key_length(i32::try_from(password.len()).unwrap_or(i32::MAX))
            {
                self.store_errors();
            }
            password.to_vec()
        };
        // OpenSSL may read up to its (possibly adjusted) key/IV length;
        // pad generously so short inputs never cause out-of-bounds reads.
        let mut key_buf = key;
        key_buf.resize(key_buf.len().max(key_len).max(64), 0);
        let mut iv_buf = iv;
        iv_buf.resize(iv_buf.len().max(64), 0);
        if !ctx.init(None, Some(&key_buf), Some(&iv_buf), i32::from(enc)) {
            self.store_errors();
            return false;
        }
        if options & OPENSSL_ZERO_PADDING != 0 {
            ctx.set_padding(false);
        }
        true
    }

    /// `php_openssl_cipher_update`.
    fn cipher_update(
        &mut self,
        cipher: &Cipher,
        ctx: &CipherCtx,
        mode: CipherMode,
        data: &[u8],
        aad: Option<&[u8]>,
    ) -> Option<Vec<u8>> {
        let aad = if !mode.aad_supports_vector && aad.is_none() {
            Some(&b""[..])
        } else {
            aad
        };
        if mode.is_single_run_aead && !ctx.update_len_only(data.len()) {
            self.store_errors();
            self.warn("Setting of data length failed");
            return None;
        }
        if mode.is_aead {
            if let Some(a) = aad {
                if !ctx.update_aad(a) {
                    self.store_errors();
                    self.warn("Setting of additional application data failed");
                    return None;
                }
            }
        }
        let block = usize::try_from(cipher.block_size()).unwrap_or(0);
        let mut cap = data.len() + block;
        if cipher.mode() == cipher_mode::WRAP {
            cap += block;
        }
        let out = ctx.update(data, cap + 64);
        if out.is_none() {
            self.store_errors();
        }
        out
    }

    /// `openssl_encrypt()`. `Ok(None)` = `false`.
    pub fn encrypt(&mut self, a: &EncryptArgs<'_>) -> PhpResult<Option<Encrypted>> {
        let tag_len = i32::try_from(a.tag_length)
            .map_err(|_| Throw::ValueError("tag_len is too long".into()))?;
        let Some(cipher) = ffi::cipher_by_name(a.method) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        let Some(ctx) = CipherCtx::new() else {
            self.warn("Failed to create cipher context");
            return Ok(None);
        };
        let mode = CipherMode::load(&cipher);
        if !self.cipher_init(
            &cipher,
            &ctx,
            mode,
            a.passphrase,
            a.iv,
            None,
            tag_len,
            a.options,
            true,
        ) {
            return Ok(None);
        }
        let Some(mut out) = self.cipher_update(&cipher, &ctx, mode, a.data, a.aad) else {
            return Ok(None);
        };
        if !ctx.finalize(true, &mut out) {
            self.store_errors();
            return Ok(None);
        }
        let data = if a.options & OPENSSL_RAW_DATA != 0 {
            out
        } else {
            base64_encode(&out)
        };
        let tag = if mode.is_aead && a.tag_requested {
            if let Some(t) = ctx.ctrl_get_tag(ffi::EVP_CTRL_AEAD_GET_TAG, tag_len) {
                TagOut::Tag(t)
            } else {
                self.warn("Retrieving verification tag failed");
                return Ok(None);
            }
        } else if a.tag_requested {
            TagOut::Null
        } else if mode.is_aead {
            self.warn("A tag should be provided when using AEAD mode");
            return Ok(None);
        } else {
            TagOut::Untouched
        };
        Ok(Some(Encrypted { data, tag }))
    }

    /// `openssl_decrypt()`. `Ok(None)` = `false`.
    pub fn decrypt(&mut self, a: &DecryptArgs<'_>) -> PhpResult<Option<Vec<u8>>> {
        if a.method.is_empty() {
            return Err(Throw::ArgumentMustNotBeEmpty { arg: 2 });
        }
        let tag_len = a
            .tag
            .map_or(0, |t| i32::try_from(t.len()).unwrap_or(i32::MAX));
        let Some(cipher) = ffi::cipher_by_name(a.method) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        let Some(ctx) = CipherCtx::new() else {
            self.warn("Failed to create cipher context");
            return Ok(None);
        };
        let mode = CipherMode::load(&cipher);
        let decoded;
        let data = if a.options & OPENSSL_RAW_DATA == 0 {
            decoded = base64_decode(a.data);
            &decoded[..]
        } else {
            a.data
        };
        if !self.cipher_init(
            &cipher,
            &ctx,
            mode,
            a.passphrase,
            a.iv,
            a.tag,
            tag_len,
            a.options,
            false,
        ) {
            return Ok(None);
        }
        let Some(mut out) = self.cipher_update(&cipher, &ctx, mode, data, a.aad) else {
            return Ok(None);
        };
        if mode.is_single_run_aead || ctx.finalize(false, &mut out) {
            Ok(Some(out))
        } else {
            self.store_errors();
            Ok(None)
        }
    }

    /// `openssl_cipher_iv_length()`.
    pub fn cipher_iv_length(&mut self, method: &[u8]) -> PhpResult<Option<i64>> {
        if method.is_empty() {
            return Err(Throw::ArgumentMustNotBeEmpty { arg: 1 });
        }
        let Some(c) = ffi::cipher_by_name(method) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        Ok(Some(i64::from(c.iv_length())))
    }

    /// `openssl_cipher_key_length()`.
    pub fn cipher_key_length(&mut self, method: &[u8]) -> PhpResult<Option<i64>> {
        if method.is_empty() {
            return Err(Throw::ArgumentMustNotBeEmpty { arg: 1 });
        }
        let Some(c) = ffi::cipher_by_name(method) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        Ok(Some(i64::from(c.key_length())))
    }

    /// `openssl_random_pseudo_bytes()`; the bool is `$strong_result`.
    pub fn random_pseudo_bytes(&mut self, length: i64) -> PhpResult<(Vec<u8>, bool)> {
        if length <= 0 {
            return Err(Throw::ArgumentValueError {
                arg: 1,
                message: "must be greater than 0".into(),
            });
        }
        if length > i64::from(i32::MAX) {
            return Err(Throw::ArgumentValueError {
                arg: 1,
                message: format!("must be less than or equal to {}", i32::MAX),
            });
        }
        let len = usize::try_from(length).unwrap_or(0);
        if let Some(b) = ffi::rand_bytes(len) {
            Ok((b, true))
        } else {
            self.store_errors();
            Err(Throw::Exception("Error reading from source device".into()))
        }
    }

    /// `openssl_pbkdf2()`. `Ok(None)` = `false`.
    pub fn pbkdf2(
        &mut self,
        password: &[u8],
        salt: &[u8],
        key_length: i64,
        iterations: i64,
        digest_algo: &[u8],
    ) -> PhpResult<Option<Vec<u8>>> {
        let too_long = |arg: u32, name: &str| Throw::ArgumentValueError {
            arg,
            message: format!("{name} is too long"),
        };
        let key_length_i = i32::try_from(key_length).map_err(|_| too_long(3, "key"))?;
        let iterations = i32::try_from(iterations).map_err(|_| too_long(4, "iterations"))?;
        if key_length_i <= 0 {
            return Err(Throw::ArgumentValueError {
                arg: 3,
                message: "must be greater than 0".into(),
            });
        }
        let md = if digest_algo.is_empty() {
            ffi::md_by_name(b"SHA1")
        } else {
            ffi::md_by_name(digest_algo)
        };
        let Some(md) = md else {
            self.warn("Unknown digest algorithm");
            return Ok(None);
        };
        let out = ffi::pbkdf2(
            password,
            salt,
            iterations,
            &md,
            usize::try_from(key_length_i).unwrap_or(0),
        );
        if out.is_none() {
            self.store_errors();
        }
        Ok(out)
    }

    /// `openssl_digest()`. `Ok(None)` = `false`.
    pub fn digest(&mut self, data: &[u8], digest_algo: &[u8], binary: bool) -> Option<Vec<u8>> {
        let Some(md) = ffi::md_by_name(digest_algo) else {
            self.warn("Unknown digest algorithm");
            return None;
        };
        if let Some(d) = ffi::digest(&md, data) {
            Some(if binary { d } else { bin2hex(&d) })
        } else {
            self.store_errors();
            None
        }
    }

    /// `openssl_get_md_methods()`.
    pub fn get_md_methods(&mut self, aliases: bool) -> Vec<String> {
        ffi::md_names(aliases)
    }

    /// `openssl_get_cipher_methods()`: provided ciphers, lower-cased and
    /// sorted (`zend_hash_sort` with `string_compare_function`).
    pub fn get_cipher_methods(&mut self, aliases: bool) -> Vec<String> {
        let mut v = ffi::provided_cipher_names(aliases);
        v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        v
    }

    /// `openssl_get_curve_names()`.
    pub fn get_curve_names(&mut self) -> Vec<String> {
        ffi::builtin_curve_names()
    }

    /// `openssl_get_cert_locations()`.
    pub fn get_cert_locations(&mut self) -> PhpArray {
        let mut a = PhpArray::new();
        for (k, v) in ffi::default_cert_locations() {
            a.set(k, PhpValue::str(v));
        }
        a.set("ini_cafile", PhpValue::Str(self.ini_cafile.clone()));
        a.set("ini_capath", PhpValue::Str(self.ini_capath.clone()));
        a
    }
}
