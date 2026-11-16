// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PKCS#12: `openssl_pkcs12_export`, `_export_to_file`, `_read`.

use crate::args::{CertArg, KeyArg};
use crate::error::PhpResult;
use crate::ffi::{self, Bio, X509Stack};
use crate::value::{PhpArray, PhpValue};
use crate::OpenSsl;

/// The recognised `$options` of `openssl_pkcs12_export*`.
#[derive(Debug, Clone, Default)]
pub struct Pkcs12Options<'a> {
    /// `friendly_name` (only when it is a string).
    pub friendly_name: Option<&'a [u8]>,
    /// `extracerts`: the certificates and whether the option was an array
    /// (a scalar is treated as a single certificate).
    pub extracerts: Option<(Vec<CertArg<'a>>, bool)>,
}

impl OpenSsl {
    /// Shared body; `out_file` selects `_to_file`.
    fn pkcs12_export_impl(
        &mut self,
        cert: CertArg<'_>,
        private_key: &KeyArg<'_>,
        passphrase: &[u8],
        options: &Pkcs12Options<'_>,
        out_file: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(None);
        };
        let Some(key) = self.pkey_from_arg(private_key, false, Some(b""), 3)? else {
            self.warn("Cannot get private key from parameter 3");
            return Ok(None);
        };
        if !c.get().check_private_key(&key) {
            if out_file.is_some() {
                self.store_errors();
            }
            self.warn("Private key does not correspond to cert");
            return Ok(None);
        }
        let path = match out_file {
            Some(f) => match self.check_plain_path(f, 2)? {
                Some(p) => Some(p),
                None => return Ok(None),
            },
            None => None,
        };
        let ca: Option<X509Stack> = match &options.extracerts {
            Some((certs, is_array)) => {
                match self.certs_to_stack(certs, *is_array, 5, Some("extracerts"))? {
                    Some(s) => Some(s),
                    None => return Ok(None),
                }
            }
            None => None,
        };
        let shown = path
            .as_deref()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .unwrap_or_default();
        // PKCS12_create first, then open the output (as PHP does).
        let Some(mem) = Bio::mem() else {
            return Ok(None);
        };
        match ffi::pkcs12_create_write(
            &mem,
            passphrase,
            options.friendly_name,
            &key,
            c.get(),
            ca.as_ref(),
        ) {
            Ok(()) => {}
            Err(false) => {
                self.store_errors();
                return Ok(None);
            }
            Err(true) => {
                self.store_errors();
                if path.is_some() {
                    self.warn(format!("Error writing to file {shown}"));
                }
                return Ok(None);
            }
        }
        let der = mem.contents();
        if let Some(p) = &path {
            let Some(bio) = Bio::file(p, "w") else {
                self.store_errors();
                self.warn(format!("Error opening file {shown}"));
                return Ok(None);
            };
            if usize::try_from(bio.write(&der)).ok() != Some(der.len()) {
                self.store_errors();
                self.warn(format!("Error writing to file {shown}"));
                return Ok(None);
            }
        }
        Ok(Some(der))
    }

    /// `openssl_pkcs12_export()`: the DER bundle for `$output` or `None`.
    pub fn pkcs12_export(
        &mut self,
        cert: CertArg<'_>,
        private_key: &KeyArg<'_>,
        passphrase: &[u8],
        options: &Pkcs12Options<'_>,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.pkcs12_export_impl(cert, private_key, passphrase, options, None)
    }

    /// `openssl_pkcs12_export_to_file()`.
    pub fn pkcs12_export_to_file(
        &mut self,
        cert: CertArg<'_>,
        output_filename: &[u8],
        private_key: &KeyArg<'_>,
        passphrase: &[u8],
        options: &Pkcs12Options<'_>,
    ) -> PhpResult<bool> {
        Ok(self
            .pkcs12_export_impl(
                cert,
                private_key,
                passphrase,
                options,
                Some(output_filename),
            )?
            .is_some())
    }

    /// `openssl_pkcs12_read()`: the array for `$certificates` (keys
    /// `cert`, `pkey`, `extracerts`) or `None` (false).
    pub fn pkcs12_read(&mut self, pkcs12: &[u8], passphrase: &[u8]) -> Option<PhpArray> {
        let (bio, n) = Bio::mem_written(pkcs12)?;
        if n <= 0 {
            self.store_errors();
            return None;
        }
        let Some(p) = ffi::pkcs12_parse(&bio, passphrase) else {
            self.store_errors();
            return None;
        };
        let mut out = PhpArray::new();
        if let Some(cert) = &p.cert {
            match cert.to_pem() {
                Some(pem) => out.set("cert", PhpValue::Str(pem)),
                None => self.store_errors(),
            }
        }
        if let Some(key) = &p.pkey {
            let mem = Bio::mem()?;
            if key.write_private(&mem, None, None) {
                out.set("pkey", PhpValue::Str(mem.contents()));
            } else {
                self.store_errors();
            }
        }
        if let Some(ca) = p.ca.as_ref().filter(|c| !c.is_empty()) {
            // PHP pops from the end of the stack.
            let mut extra = PhpArray::new();
            for (i, pem) in ca.pems().into_iter().rev().enumerate() {
                extra.set(i64::try_from(i).unwrap_or(0), PhpValue::Str(pem));
            }
            out.set("extracerts", PhpValue::Array(extra));
        }
        Some(out)
    }
}
