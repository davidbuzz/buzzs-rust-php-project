//! RSA encryption primitives, signatures and envelopes:
//! `openssl_{private,public}_{encrypt,decrypt}`, `openssl_sign`,
//! `openssl_verify`, `openssl_seal`, `openssl_open`.

use crate::args::KeyArg;
use crate::config::md_from_algo;
use crate::constants::{
    OPENSSL_PKCS1_OAEP_PADDING, OPENSSL_PKCS1_PADDING, OPENSSL_PKCS1_PSS_PADDING,
    OPENSSL_RSA_PSS_SALTLEN_AUTO,
};
use crate::error::{PhpResult, Throw};
use crate::ffi::{self, pkey_id, CipherCtx, Md, Pkey, PkeyCtx, PkeyCtxRef, PkeyOp, VerifyInit};
use crate::OpenSsl;

/// A `string|int $algorithm` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgo<'a> {
    /// A digest name such as `"sha256"`.
    Name(&'a [u8]),
    /// An `OPENSSL_ALGO_*` constant (0 = key default on OpenSSL 3).
    Id(i64),
}

impl Default for DigestAlgo<'_> {
    fn default() -> Self {
        DigestAlgo::Id(crate::constants::OPENSSL_ALGO_SHA1)
    }
}

/// Outputs of `openssl_seal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Return value (sealed data length).
    pub length: i64,
    /// `$sealed_data`, `$encrypted_keys` and (when `$iv` was passed) `$iv`;
    /// `None` when the length is 0 and nothing is assigned.
    pub outputs: Option<SealOutputs>,
}

/// By-reference results of `openssl_seal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealOutputs {
    /// `$sealed_data`.
    pub sealed_data: Vec<u8>,
    /// `$encrypted_keys` (list).
    pub encrypted_keys: Vec<Vec<u8>>,
    /// `$iv` (only when the argument was passed).
    pub iv: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum RsaOp {
    PrivateEncrypt,
    PrivateDecrypt,
    PublicEncrypt,
    PublicDecrypt,
}

impl OpenSsl {
    /// Shared body of the four RSA primitives.
    fn rsa_op(
        &mut self,
        op: RsaOp,
        data: &[u8],
        key: &KeyArg<'_>,
        padding: i64,
        digest_algo: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        let public = matches!(op, RsaOp::PublicEncrypt | RsaOp::PublicDecrypt);
        let k = if public {
            self.pkey_from_arg(key, true, None, 3)?
        } else {
            self.pkey_from_arg(key, false, Some(b""), 3)?
        };
        let Some(pkey) = k else {
            self.warn(match op {
                RsaOp::PrivateEncrypt => "key param is not a valid private key",
                RsaOp::PrivateDecrypt => "key parameter is not a valid private key",
                RsaOp::PublicEncrypt | RsaOp::PublicDecrypt => {
                    "key parameter is not a valid public key"
                }
            });
            return Ok(None);
        };
        let pop = match op {
            RsaOp::PrivateEncrypt => PkeyOp::Sign,
            RsaOp::PrivateDecrypt => PkeyOp::Decrypt,
            RsaOp::PublicEncrypt => PkeyOp::Encrypt,
            RsaOp::PublicDecrypt => PkeyOp::VerifyRecover,
        };
        #[allow(clippy::cast_possible_truncation)]
        let pad = padding as i32;
        let ctx = PkeyCtx::new(&pkey);
        let ok_init = ctx.as_ref().is_some_and(|c| c.op_init(pop));
        let ok = ok_init
            && ctx.as_ref().is_some_and(|c| match op {
                RsaOp::PrivateDecrypt | RsaOp::PublicEncrypt => {
                    self.set_rsa_padding_and_digest(c, padding, digest_algo)
                }
                _ => c.set_rsa_padding(pad),
            });
        let size = if ok {
            ctx.as_ref().and_then(|c| c.op_size(pop, data))
        } else {
            None
        };
        let (Some(ctx), Some(size)) = (ctx, size) else {
            self.store_errors();
            return Ok(None);
        };
        let out = ctx.op_run(pop, data, size);
        if out.is_none() {
            self.store_errors();
        }
        Ok(out)
    }

    /// `php_openssl_set_rsa_padding_and_digest`.
    fn set_rsa_padding_and_digest(
        &mut self,
        ctx: &PkeyCtx,
        padding: i64,
        digest_algo: Option<&[u8]>,
    ) -> bool {
        #[allow(clippy::cast_possible_truncation)]
        if !ctx.set_rsa_padding(padding as i32) {
            return false;
        }
        if let Some(name) = digest_algo {
            let Some(md) = ffi::md_by_name(name) else {
                self.warn(format!(
                    "Unknown digest algorithm: {}",
                    String::from_utf8_lossy(name)
                ));
                return false;
            };
            if padding == OPENSSL_PKCS1_OAEP_PADDING && !ctx.set_rsa_oaep_md(&md) {
                return false;
            }
        }
        true
    }

    /// `openssl_private_encrypt()`: `$encrypted_data` or `None` (false).
    pub fn private_encrypt(
        &mut self,
        data: &[u8],
        private_key: &KeyArg<'_>,
        padding: i64,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.rsa_op(RsaOp::PrivateEncrypt, data, private_key, padding, None)
    }

    /// `openssl_private_decrypt()`.
    pub fn private_decrypt(
        &mut self,
        data: &[u8],
        private_key: &KeyArg<'_>,
        padding: i64,
        digest_algo: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.rsa_op(
            RsaOp::PrivateDecrypt,
            data,
            private_key,
            padding,
            digest_algo,
        )
    }

    /// `openssl_public_encrypt()`.
    pub fn public_encrypt(
        &mut self,
        data: &[u8],
        public_key: &KeyArg<'_>,
        padding: i64,
        digest_algo: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.rsa_op(RsaOp::PublicEncrypt, data, public_key, padding, digest_algo)
    }

    /// `openssl_public_decrypt()`.
    pub fn public_decrypt(
        &mut self,
        data: &[u8],
        public_key: &KeyArg<'_>,
        padding: i64,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.rsa_op(RsaOp::PublicDecrypt, data, public_key, padding, None)
    }

    /// Digest lookup for sign/verify; `Err(())` = "Unknown digest algorithm".
    fn sign_md(algo: DigestAlgo<'_>) -> Result<Option<Md>, ()> {
        let (md, id) = match algo {
            DigestAlgo::Name(n) => (ffi::md_by_name(n), crate::constants::OPENSSL_ALGO_SHA1),
            DigestAlgo::Id(i) => (md_from_algo(i), i),
        };
        if md.is_none() && id != 0 {
            return Err(());
        }
        Ok(md)
    }

    /// `php_openssl_setup_rsa_padding` + `php_openssl_setup_rsa_pss_salt_length`.
    fn setup_rsa(
        &mut self,
        pctx: &PkeyCtxRef,
        pkey: &Pkey,
        padding: i64,
        salt_length: i32,
    ) -> bool {
        if padding != 0 {
            if pkey.type_of_id() != pkey_id::RSA {
                self.warn("Padding parameter is only supported for RSA keys");
                return false;
            }
            if padding == OPENSSL_PKCS1_PSS_PADDING {
                if pctx.set_rsa_padding(6) <= 0 {
                    self.store_errors();
                    return false;
                }
            } else if padding != OPENSSL_PKCS1_PADDING {
                self.warn("Unknown padding type");
                return false;
            }
        }
        if padding != OPENSSL_PKCS1_PSS_PADDING
            || i64::from(salt_length) == OPENSSL_RSA_PSS_SALTLEN_AUTO
        {
            return true;
        }
        if pkey.base_id() != pkey_id::RSA && pkey.base_id() != pkey_id::RSA_PSS {
            return true;
        }
        if pctx.set_rsa_pss_saltlen(salt_length) <= 0 {
            self.store_errors();
            self.warn("Could not set RSA-PSS salt length");
            return false;
        }
        true
    }

    /// `openssl_sign()`: `$signature` or `None` (false).
    pub fn sign(
        &mut self,
        data: &[u8],
        private_key: &KeyArg<'_>,
        algorithm: DigestAlgo<'_>,
        padding: i64,
        salt_length: i64,
    ) -> PhpResult<Option<Vec<u8>>> {
        let salt = i32::try_from(salt_length).map_err(|_| Throw::ArgumentValueError {
            arg: 6,
            message: "salt_length is too long".into(),
        })?;
        let Some(pkey) = self.pkey_from_arg(private_key, false, Some(b""), 3)? else {
            self.warn("Supplied key param cannot be coerced into a private key");
            return Ok(None);
        };
        let Ok(md) = Self::sign_md(algorithm) else {
            self.warn("Unknown digest algorithm");
            return Ok(None);
        };
        let sig = ffi::digest_sign(md.as_ref(), &pkey, data, &mut |pctx| {
            self.setup_rsa(pctx, &pkey, padding, salt)
        });
        if sig.is_none() {
            self.store_errors();
        }
        Ok(sig)
    }

    /// `openssl_verify()`: 1, 0, -1, or `None` for `false`.
    pub fn verify(
        &mut self,
        data: &[u8],
        signature: &[u8],
        public_key: &KeyArg<'_>,
        algorithm: DigestAlgo<'_>,
        padding: i64,
        salt_length: i64,
    ) -> PhpResult<Option<i64>> {
        let salt = i32::try_from(salt_length).map_err(|_| Throw::ArgumentValueError {
            arg: 6,
            message: "salt_length is too long".into(),
        })?;
        let Ok(md) = Self::sign_md(algorithm) else {
            self.warn("Unknown digest algorithm");
            return Ok(None);
        };
        let Some(pkey) = self.pkey_from_arg(public_key, true, None, 3)? else {
            self.warn("Supplied key param cannot be coerced into a public key");
            return Ok(None);
        };
        let r = ffi::digest_verify(md.as_ref(), &pkey, signature, data, &mut |pctx| {
            self.setup_rsa(pctx, &pkey, padding, salt)
        });
        Ok(Some(match r {
            VerifyInit::NoCtx | VerifyInit::InitFailed => {
                self.store_errors();
                -1
            }
            VerifyInit::Result(e) => {
                if e < 0 {
                    self.store_errors();
                }
                i64::from(e)
            }
        }))
    }

    /// `openssl_seal()`. `iv_passed` tells whether the `$iv` argument was
    /// given. `Ok(None)` = `false`.
    pub fn seal(
        &mut self,
        data: &[u8],
        public_keys: &[KeyArg<'_>],
        cipher_algo: &[u8],
        iv_passed: bool,
    ) -> PhpResult<Option<Sealed>> {
        if public_keys.is_empty() {
            return Err(Throw::ArgumentMustNotBeEmpty { arg: 4 });
        }
        let Some(cipher) = ffi::cipher_by_name(cipher_algo) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        let iv_len = usize::try_from(cipher.iv_length()).unwrap_or(0);
        if !iv_passed && iv_len > 0 {
            return Err(Throw::ArgumentValueError {
                arg: 6,
                message: "cannot be null for the chosen cipher algorithm".into(),
            });
        }
        let mut keys = Vec::with_capacity(public_keys.len());
        for (i, k) in public_keys.iter().enumerate() {
            let Some(pk) = self.pkey_from_arg(k, true, None, 4)? else {
                self.warn(format!("Not a public key ({}th member of pubkeys)", i + 1));
                return Ok(None);
            };
            keys.push(pk);
        }
        let Some(ctx) = CipherCtx::new().filter(|c| c.encrypt_init_cipher(&cipher)) else {
            self.store_errors();
            return Ok(None);
        };
        ctx.reset();
        let Some(s) = ffi::seal(&ctx, &cipher, &keys, data) else {
            self.store_errors();
            return Ok(None);
        };
        let length = i64::try_from(s.data.len()).unwrap_or(i64::MAX);
        let outputs = (!s.data.is_empty()).then(|| SealOutputs {
            sealed_data: s.data,
            encrypted_keys: s.keys,
            iv: iv_passed.then(|| s.iv[..iv_len.min(s.iv.len())].to_vec()),
        });
        Ok(Some(Sealed { length, outputs }))
    }

    /// `openssl_open()`: `$output` or `None` (false).
    pub fn open(
        &mut self,
        data: &[u8],
        encrypted_key: &[u8],
        private_key: &KeyArg<'_>,
        cipher_algo: &[u8],
        iv: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        let Some(pkey) = self.pkey_from_arg(private_key, false, Some(b""), 4)? else {
            self.warn("Unable to coerce parameter 4 into a private key");
            return Ok(None);
        };
        let Some(cipher) = ffi::cipher_by_name(cipher_algo) else {
            self.warn("Unknown cipher algorithm");
            return Ok(None);
        };
        let iv_len = usize::try_from(cipher.iv_length()).unwrap_or(0);
        let iv_buf = if iv_len > 0 {
            let Some(iv) = iv else {
                return Err(Throw::ArgumentValueError {
                    arg: 6,
                    message: "cannot be null for the chosen cipher algorithm".into(),
                });
            };
            if iv.len() != iv_len {
                self.warn("IV length is invalid");
                return Ok(None);
            }
            Some(iv)
        } else {
            None
        };
        let out = CipherCtx::new()
            .and_then(|ctx| ffi::open(&ctx, &cipher, encrypted_key, iv_buf, &pkey, data));
        match out {
            Some(o) if !o.is_empty() => Ok(Some(o)),
            _ => {
                self.store_errors();
                Ok(None)
            }
        }
    }
}
