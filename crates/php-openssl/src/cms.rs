//! CMS (RFC 5652) S/MIME: `openssl_cms_*`.

use crate::args::{CertArg, KeyArg};
use crate::config::cipher_from_algo;
use crate::constants::{
    CMS_PARTIAL, CMS_STREAM, OPENSSL_CMS_BINARY, OPENSSL_CMS_DETACHED, OPENSSL_ENCODING_DER,
    OPENSSL_ENCODING_PEM, OPENSSL_ENCODING_SMIME,
};
use crate::error::{PhpResult, Throw};
use crate::ffi::{self, Bio, Cms};
use crate::pkcs7::{cert_as_key, mode_r, mode_w, Header, Recipients};
use crate::OpenSsl;

/// A `string|int $cipher_algo` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherAlgo<'a> {
    /// A cipher name.
    Name(&'a [u8]),
    /// An `OPENSSL_CIPHER_*` constant.
    Id(i64),
}

impl Default for CipherAlgo<'_> {
    fn default() -> Self {
        CipherAlgo::Id(crate::constants::OPENSSL_CIPHER_AES_128_CBC)
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn uflags(f: i64) -> u32 {
    f as u32
}

#[allow(clippy::cast_possible_truncation)]
fn iflags(f: i64) -> i32 {
    f as i32
}

impl OpenSsl {
    /// Writes a CMS structure in the requested encoding (shared by
    /// encrypt and sign).
    fn cms_write(
        &mut self,
        cms: &Cms,
        out: &Bio<'_>,
        infile: &Bio<'_>,
        flags: i64,
        encoding: i64,
    ) -> bool {
        let need_final = flags & CMS_PARTIAL != 0 && flags & CMS_STREAM == 0;
        let ok = match encoding {
            OPENSSL_ENCODING_SMIME => cms.smime_write(out, infile, iflags(flags)),
            OPENSSL_ENCODING_DER => {
                if need_final && !cms.finalize(infile, uflags(flags)) {
                    self.store_errors();
                    return false;
                }
                cms.write_der(out)
            }
            OPENSSL_ENCODING_PEM => {
                if need_final && !cms.finalize(infile, uflags(flags)) {
                    self.store_errors();
                    return false;
                }
                if flags & CMS_STREAM != 0 {
                    cms.write_pem_stream(out, infile, iflags(flags))
                } else {
                    cms.write_pem(out)
                }
            }
            _ => {
                self.warn("Unknown OPENSSL encoding");
                return false;
            }
        };
        if !ok {
            self.store_errors();
        }
        ok
    }

    /// `openssl_cms_verify()`.
    #[allow(clippy::too_many_arguments)]
    pub fn cms_verify(
        &mut self,
        input_filename: &[u8],
        flags: i64,
        certificates: Option<&[u8]>,
        ca_info: &[Vec<u8>],
        untrusted_certificates_filename: Option<&[u8]>,
        content: Option<&[u8]>,
        pk7: Option<&[u8]>,
        sigfile: Option<&[u8]>,
        encoding: i64,
    ) -> PhpResult<bool> {
        let Some(input) = self.bio_new_file(input_filename, 1, mode_r(flags))? else {
            return Ok(false);
        };
        let detached = flags & OPENSSL_CMS_DETACHED != 0;
        let sigbio_owned = match sigfile {
            Some(sf) if detached => {
                if encoding == OPENSSL_ENCODING_SMIME {
                    self.warn("Detached signatures not possible with S/MIME encoding");
                    return Ok(false);
                }
                match self.bio_new_file(sf, 1, mode_r(flags))? {
                    Some(b) => Some(b),
                    None => return Ok(false),
                }
            }
            _ => None,
        };
        let sigbio = sigbio_owned.as_ref().unwrap_or(&input);
        let (cms, smime_content) = match encoding {
            OPENSSL_ENCODING_PEM => (Cms::read_pem(sigbio), None),
            OPENSSL_ENCODING_DER => (Cms::read_der(sigbio), None),
            OPENSSL_ENCODING_SMIME => match Cms::smime_read(sigbio) {
                Some(r) => (Some(r.obj), r.content),
                None => (None, None),
            },
            _ => {
                self.warn("Unknown encoding");
                return Ok(false);
            }
        };
        let Some(cms) = cms else {
            self.store_errors();
            return Ok(false);
        };
        let datain: Option<&Bio<'_>> = if encoding == OPENSSL_ENCODING_SMIME {
            smime_content.as_ref()
        } else if detached {
            Some(&input)
        } else {
            None
        };
        let others = match untrusted_certificates_filename {
            Some(f) => match self.load_all_certs_from_file(f, 5)? {
                Some(s) => Some(s),
                None => return Ok(false),
            },
            None => None,
        };
        let Some(store) = self.setup_verify(ca_info, 4)? else {
            return Ok(false);
        };
        let dataout = match content {
            Some(f) => match self.bio_new_file(f, 6, "wb")? {
                Some(b) => Some(b),
                None => return Ok(false),
            },
            None => None,
        };
        let p7bout = match pk7 {
            Some(f) => match self.bio_new_file(f, 7, "wb")? {
                Some(b) => Some(b),
                None => return Ok(false),
            },
            None => None,
        };
        if !cms.verify(
            others.as_ref(),
            &store,
            datain,
            dataout.as_ref(),
            uflags(flags),
        ) {
            self.store_errors();
            return Ok(false);
        }
        let mut ret = true;
        if let Some(sf) = certificates {
            let signers = cms.signer_pems();
            if self.write_signers(sf, signers)? != Some(true) {
                ret = false;
            }
            if let Some(out) = &p7bout {
                if !cms.write_pem(out) {
                    self.warn("Failed to write CMS to file");
                    self.store_errors();
                    ret = false;
                }
            }
        }
        Ok(ret)
    }

    /// `openssl_cms_encrypt()`.
    #[allow(clippy::too_many_arguments)]
    pub fn cms_encrypt(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: &Recipients<'_>,
        headers: Option<&[Header]>,
        flags: i64,
        encoding: i64,
        cipher_algo: CipherAlgo<'_>,
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
        let cipher = match cipher_algo {
            CipherAlgo::Name(n) => ffi::cipher_by_name(n),
            CipherAlgo::Id(i) => cipher_from_algo(i),
        };
        let Some(cipher) = cipher else {
            self.warn("Failed to get cipher");
            return Ok(false);
        };
        let Some(cms) = Cms::encrypt(&certs, &infile, &cipher, uflags(flags)) else {
            self.store_errors();
            return Ok(false);
        };
        if encoding == OPENSSL_ENCODING_SMIME {
            if let Some(h) = headers {
                if !self.write_headers(&outfile, h) {
                    return Ok(false);
                }
            }
        }
        infile.reset();
        Ok(self.cms_write(&cms, &outfile, &infile, flags, encoding))
    }

    /// `openssl_cms_read()`: PEM certificates then CRLs, or `None`.
    pub fn cms_read(&mut self, data: &[u8]) -> Option<Vec<Vec<u8>>> {
        let (bio, n) = Bio::mem_written(data)?;
        if n <= 0 {
            self.store_errors();
            return None;
        }
        let Some(cms) = Cms::read_pem(&bio) else {
            self.store_errors();
            return None;
        };
        let (mut certs, crls) = cms.certs_and_crls();
        certs.extend(crls);
        Some(certs)
    }

    /// `openssl_cms_sign()`.
    #[allow(clippy::too_many_arguments)]
    pub fn cms_sign(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: CertArg<'_>,
        private_key: &KeyArg<'_>,
        headers: Option<&[Header]>,
        flags: i64,
        encoding: i64,
        untrusted_certificates_filename: Option<&[u8]>,
    ) -> PhpResult<bool> {
        let others = match untrusted_certificates_filename {
            Some(f) => match self.load_all_certs_from_file(f, 8)? {
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
        if encoding & OPENSSL_ENCODING_SMIME != 0 && flags & OPENSSL_CMS_DETACHED != 0 {
            self.warn("Detached signatures not possible with S/MIME encoding");
            return Ok(false);
        }
        let Some(infile) = self.bio_new_file(input_filename, 1, mode_r(flags))? else {
            self.warn(format!(
                "Error opening input file {}!",
                lossy(input_filename)
            ));
            return Ok(false);
        };
        let Some(outfile) = self.bio_new_file(output_filename, 2, mode_w(OPENSSL_CMS_BINARY))?
        else {
            self.warn(format!(
                "Error opening output file {}!",
                lossy(output_filename)
            ));
            return Ok(false);
        };
        let Some(cms) = Cms::sign(cert.get(), &key, others.as_ref(), &infile, uflags(flags)) else {
            self.store_errors();
            self.warn("Error creating CMS structure!");
            return Ok(false);
        };
        if infile.reset() != 0 {
            self.store_errors();
            return Ok(false);
        }
        if encoding == OPENSSL_ENCODING_SMIME {
            if let Some(h) = headers {
                if !self.write_headers(&outfile, h) {
                    return Ok(false);
                }
            }
        }
        Ok(self.cms_write(&cms, &outfile, &infile, flags, encoding))
    }

    /// `openssl_cms_decrypt()`. When `private_key` is `None` the
    /// certificate argument doubles as the key.
    pub fn cms_decrypt(
        &mut self,
        input_filename: &[u8],
        output_filename: &[u8],
        certificate: CertArg<'_>,
        private_key: Option<&KeyArg<'_>>,
        encoding: i64,
    ) -> PhpResult<bool> {
        let Some(cert) = self.cert_from_arg(certificate, 3)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(false);
        };
        let as_key = cert_as_key(certificate);
        let key_arg = private_key.unwrap_or(&as_key);
        let Some(key) = self.pkey_from_arg(
            key_arg,
            false,
            Some(b""),
            if private_key.is_some() { 4 } else { 3 },
        )?
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
        let cms = match encoding {
            OPENSSL_ENCODING_DER => Cms::read_der(&input),
            OPENSSL_ENCODING_PEM => Cms::read_pem(&input),
            OPENSSL_ENCODING_SMIME => Cms::smime_read(&input).map(|r| r.obj),
            _ => {
                return Err(Throw::ArgumentValueError {
                    arg: 5,
                    message: "must be an OPENSSL_ENCODING_* constant".into(),
                })
            }
        };
        let Some(cms) = cms else {
            self.store_errors();
            return Ok(false);
        };
        if cms.decrypt(&key, cert.get(), &out) {
            Ok(true)
        } else {
            self.store_errors();
            Ok(false)
        }
    }
}
