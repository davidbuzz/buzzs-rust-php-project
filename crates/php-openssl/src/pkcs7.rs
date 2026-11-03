//! S/MIME PKCS#7: `openssl_pkcs7_*`.

use crate::args::{CertArg, KeyArg};
use crate::config::cipher_from_algo;
use crate::constants::PKCS7_DETACHED;
use crate::error::PhpResult;
use crate::ffi::{Bio, Pkcs7, X509Stack};
use crate::value::BoolOrInt;
use crate::OpenSsl;

/// One `$headers` entry: `"name: value"` for string keys, `"value"` for
/// integer keys (values already converted to strings by the VM).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The string key, if any.
    pub name: Option<Vec<u8>>,
    /// The value.
    pub value: Vec<u8>,
}

/// The `$certificate` argument of the encrypt functions.
#[derive(Debug, Clone)]
pub enum Recipients<'a> {
    /// An array of certificates.
    Many(Vec<CertArg<'a>>),
    /// A single certificate.
    One(CertArg<'a>),
}

/// Truncates at the first NUL, as C's `%s` does.
fn c_str(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `PHP_OPENSSL_BIO_MODE_R(flags)`.
pub(crate) fn mode_r(flags: i64) -> &'static str {
    if flags & crate::constants::PKCS7_BINARY != 0 {
        "rb"
    } else {
        "r"
    }
}

/// `PHP_OPENSSL_BIO_MODE_W(flags)`.
pub(crate) fn mode_w(flags: i64) -> &'static str {
    if flags & crate::constants::PKCS7_BINARY != 0 {
        "wb"
    } else {
        "w"
    }
}

impl OpenSsl {
    /// Writes `$headers` lines (`BIO_printf("%s: %s\n")`).
    pub(crate) fn write_headers(&mut self, out: &Bio<'_>, headers: &[Header]) -> bool {
        for h in headers {
            let mut line = Vec::new();
            if let Some(n) = &h.name {
                line.extend_from_slice(c_str(n));
                line.extend_from_slice(b": ");
            }
            line.extend_from_slice(c_str(&h.value));
            line.push(b'\n');
            if out.write(&line) < 0 {
                self.store_errors();
                return false;
            }
        }
        true
    }

    /// Builds the recipient stack of `openssl_{pkcs7,cms}_encrypt`.
    pub(crate) fn recipient_stack(
        &mut self,
        recipients: &Recipients<'_>,
    ) -> PhpResult<Option<X509Stack>> {
        let Some(sk) = X509Stack::new() else {
            return Ok(None);
        };
        let (list, is_array): (&[CertArg<'_>], bool) = match recipients {
            Recipients::Many(v) => (v, true),
            Recipients::One(c) => (std::slice::from_ref(c), false),
        };
        for c in list {
            let Some(cert) = self.cert_from_arg_ex(*c, 3, is_array, None)? else {
                return Ok(None);
            };
            let Some(owned) = cert.into_owned() else {
                self.store_errors();
                return Ok(None);
            };
            if !sk.push(owned) {
                self.store_errors();
                return Ok(None);
            }
        }
        Ok(Some(sk))
    }

    /// Writes signer certificates (PEM) to `path`; `Some(false)` when a
    /// write failed, `None` when the file could not be opened.
    pub(crate) fn write_signers(
        &mut self,
        path: &[u8],
        signers: Option<Vec<Option<Vec<u8>>>>,
    ) -> PhpResult<Option<bool>> {
        let Some(out) = self.bio_new_file(path, 3, "w")? else {
            self.warn(format!(
                "Signature OK, but cannot open {} for writing",
                lossy(path)
            ));
            return Ok(None);
        };
        let Some(signers) = signers else {
            self.store_errors();
            return Ok(Some(false));
        };
        let mut ok = true;
        for (i, pem) in signers.into_iter().enumerate() {
            if let Some(pem) = pem {
                out.write(&pem);
            } else {
                self.store_errors();
                self.warn(format!("Failed to write signer {i}"));
                ok = false;
            }
        }
        Ok(Some(ok))
    }

    /// `openssl_pkcs7_verify()`: `true`, `false` or `-1`.
    #[allow(clippy::too_many_arguments)]
    pub fn pkcs7_verify(
        &mut self,
        input_filename: &[u8],
        flags: i64,
        signers_certificates_filename: Option<&[u8]>,
        ca_info: &[Vec<u8>],
        untrusted_certificates_filename: Option<&[u8]>,
        content: Option<&[u8]>,
        output_filename: Option<&[u8]>,
    ) -> PhpResult<BoolOrInt> {
        let fail = BoolOrInt::Int(-1);
        let others = match untrusted_certificates_filename {
            Some(f) => match self.load_all_certs_from_file(f, 5)? {
                Some(s) => Some(s),
                None => return Ok(fail),
            },
            None => None,
        };
        let flags = flags & !PKCS7_DETACHED;
        let Some(store) = self.setup_verify(ca_info, 4)? else {
            return Ok(fail);
        };
        let Some(input) = self.bio_new_file(input_filename, 1, mode_r(flags))? else {
            return Ok(fail);
        };
        let Some(p7) = Pkcs7::smime_read(&input) else {
            self.store_errors();
            return Ok(fail);
        };
        let dataout = match content {
            Some(f) => match self.bio_new_file(f, 6, "w")? {
                Some(b) => Some(b),
                None => return Ok(fail),
            },
            None => None,
        };
        let p7bout = match output_filename {
            Some(f) => match self.bio_new_file(f, 7, "w")? {
                Some(b) => Some(b),
                None => return Ok(fail),
            },
            None => None,
        };
        #[allow(clippy::cast_possible_truncation)]
        let iflags = flags as i32;
        if !p7.obj.verify(
            others.as_ref(),
            &store,
            p7.content.as_ref(),
            dataout.as_ref(),
            iflags,
        ) {
            self.store_errors();
            return Ok(BoolOrInt::Bool(false));
        }
        let mut ret = BoolOrInt::Bool(true);
        if let Some(sf) = signers_certificates_filename {
            let signers = p7.obj.signer_pems(others.as_ref(), iflags);
            if self.write_signers(sf, signers)? != Some(true) {
                ret = fail;
            }
            if let Some(out) = &p7bout {
                if !p7.obj.write_pem(out) {
                    self.warn("Failed to write PKCS7 to file");
                    self.store_errors();
                    ret = BoolOrInt::Bool(false);
                }
            }
        }
        Ok(ret)
    }

    /// `openssl_pkcs7_encrypt()`.
    pub fn pkcs7_encrypt(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: &Recipients<'_>,
        headers: Option<&[Header]>,
        flags: i64,
        cipher_algo: i64,
    ) -> PhpResult<bool> {
        let Some(infile) = self.bio_new_file(input_filename, 1, mode_r(flags))? else {
            return Ok(false);
        };
        let Some(outfile) = self.bio_new_file(output_filename, 2, mode_w(flags))? else {
            return Ok(false);
        };
        let Some(certs) = self.recipient_stack(certificate)? else {
            return Ok(false);
        };
        let Some(cipher) = cipher_from_algo(cipher_algo) else {
            self.warn("Failed to get cipher");
            return Ok(false);
        };
        #[allow(clippy::cast_possible_truncation)]
        let iflags = flags as i32;
        let Some(p7) = Pkcs7::encrypt(&certs, &infile, &cipher, iflags) else {
            self.store_errors();
            return Ok(false);
        };
        if let Some(h) = headers {
            if !self.write_headers(&outfile, h) {
                return Ok(false);
            }
        }
        infile.reset();
        if !p7.smime_write(&outfile, &infile, iflags) {
            self.store_errors();
            return Ok(false);
        }
        Ok(true)
    }

    /// `openssl_pkcs7_read()`: PEM certificates then CRLs, or `None`.
    pub fn pkcs7_read(&mut self, data: &[u8]) -> Option<Vec<Vec<u8>>> {
        let (bio, n) = Bio::mem_written(data)?;
        if n <= 0 {
            self.store_errors();
            return None;
        }
        let Some(p7) = Pkcs7::read_pem(&bio) else {
            self.store_errors();
            return None;
        };
        let (mut certs, crls) = p7.certs_and_crls();
        certs.extend(crls);
        Some(certs)
    }

    /// `openssl_pkcs7_sign()`.
    #[allow(clippy::too_many_arguments)]
    pub fn pkcs7_sign(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: CertArg<'_>,
        private_key: &KeyArg<'_>,
        headers: Option<&[Header]>,
        flags: i64,
        untrusted_certificates_filename: Option<&[u8]>,
    ) -> PhpResult<bool> {
        let others = match untrusted_certificates_filename {
            Some(f) => match self.load_all_certs_from_file(f, 7)? {
                Some(s) => Some(s),
                None => return Ok(false),
            },
            None => None,
        };
        let Some(key) = self.pkey_from_arg(private_key, false, Some(b""), 4)? else {
            self.warn("Error getting private key");
            return Ok(false);
        };
        let Some(cert) = self.cert_from_arg(certificate, 3)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(false);
        };
        let Some(infile) = self.bio_new_file(input_filename, 1, mode_r(flags))? else {
            self.warn(format!(
                "Error opening input file {}!",
                lossy(input_filename)
            ));
            return Ok(false);
        };
        let Some(outfile) = self.bio_new_file(output_filename, 2, "w")? else {
            self.warn(format!(
                "Error opening output file {}!",
                lossy(output_filename)
            ));
            return Ok(false);
        };
        #[allow(clippy::cast_possible_truncation)]
        let iflags = flags as i32;
        let Some(p7) = Pkcs7::sign(cert.get(), &key, others.as_ref(), &infile, iflags) else {
            self.store_errors();
            self.warn("Error creating PKCS7 structure!");
            return Ok(false);
        };
        infile.reset();
        if let Some(h) = headers {
            if !self.write_headers(&outfile, h) {
                return Ok(false);
            }
        }
        if !p7.smime_write(&outfile, &infile, iflags) {
            self.store_errors();
            return Ok(false);
        }
        Ok(true)
    }

    /// `openssl_pkcs7_decrypt()`. When `private_key` is `None` the
    /// certificate argument is also used as the key (`recipient_as_key`).
    pub fn pkcs7_decrypt(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: CertArg<'_>,
        private_key: Option<&KeyArg<'_>>,
    ) -> PhpResult<bool> {
        let Some(cert) = self.cert_from_arg(certificate, 3)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(false);
        };
        let as_key = cert_as_key(certificate);
        let Some(key) = self.pkey_from_arg(private_key.unwrap_or(&as_key), false, Some(b""), 4)?
        else {
            self.warn("Unable to get private key");
            return Ok(false);
        };
        let Some(input) = self.bio_new_file(input_filename, 1, "rb")? else {
            return Ok(false);
        };
        let Some(out) = self.bio_new_file(output_filename, 2, "wb")? else {
            return Ok(false);
        };
        let Some(p7) = Pkcs7::smime_read(&input) else {
            self.store_errors();
            return Ok(false);
        };
        #[allow(clippy::cast_possible_truncation)]
        if p7
            .obj
            .decrypt(&key, cert.get(), &out, PKCS7_DETACHED as i32)
        {
            Ok(true)
        } else {
            self.store_errors();
            Ok(false)
        }
    }
}

/// The certificate zval reinterpreted as a key argument.
pub(crate) fn cert_as_key(c: CertArg<'_>) -> KeyArg<'_> {
    match c {
        CertArg::Object(o) => KeyArg::Cert(o),
        CertArg::Str(s) => KeyArg::Str(s),
    }
}
