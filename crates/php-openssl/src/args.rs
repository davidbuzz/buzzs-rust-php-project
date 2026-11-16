// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The extension's object types and the "object or string" parameter
//! conversions (`php_openssl_x509_from_param`, `php_openssl_pkey_from_zval`,
//! …), plus file-path checks and certificate store helpers.

use crate::error::{PhpResult, Throw};
use crate::ffi::{self, Bio, Pkey, X509Cert, X509Req, X509Stack, X509Store};
use crate::util::expand_filepath;
use crate::OpenSsl;

/// `OpenSSLCertificate`.
#[derive(Debug)]
pub struct Certificate {
    pub(crate) x509: X509Cert,
}

/// `OpenSSLCertificateSigningRequest`.
#[derive(Debug)]
pub struct CertificateSigningRequest {
    pub(crate) req: X509Req,
}

/// `OpenSSLAsymmetricKey`.
#[derive(Debug, Clone)]
pub struct AsymmetricKey {
    pub(crate) pkey: Pkey,
    pub(crate) is_private: bool,
}

impl AsymmetricKey {
    /// Whether the object was created as a private key.
    pub fn is_private(&self) -> bool {
        self.is_private
    }
}

/// An `OpenSSLCertificate|string` parameter.
///
/// For parameters declared as plain `zval` (recipient certificates, …)
/// the VM converts non-objects with `zval_try_get_string` first.
#[derive(Debug, Clone, Copy)]
pub enum CertArg<'a> {
    /// A certificate object.
    Object(&'a Certificate),
    /// PEM/DER data, or `file://path`.
    Str(&'a [u8]),
}

/// An `OpenSSLCertificateSigningRequest|string` parameter.
#[derive(Debug, Clone, Copy)]
pub enum CsrArg<'a> {
    /// A CSR object.
    Object(&'a CertificateSigningRequest),
    /// PEM data or `file://path`.
    Str(&'a [u8]),
}

/// A key parameter (`OpenSSLAsymmetricKey|OpenSSLCertificate|array|string`).
#[derive(Debug, Clone)]
pub enum KeyArg<'a> {
    /// A key object.
    Key(&'a AsymmetricKey),
    /// A certificate object.
    Cert(&'a Certificate),
    /// A string (or an object converted with `__toString`): PEM data or
    /// `file://path`.
    Str(&'a [u8]),
    /// `[key, passphrase]`: `key` is element 0 (`None` if missing),
    /// `passphrase` element 1 converted to string (`None` if missing).
    Array {
        /// Element 0 (nested arrays must be passed as [`KeyArg::Other`]).
        key: Option<Box<KeyArg<'a>>>,
        /// Element 1, converted to string.
        passphrase: Option<Vec<u8>>,
    },
    /// Anything else (int, null, nested array, …): silently not a key.
    Other,
}

impl OpenSsl {
    /// `php_openssl_check_path_ex`. `Ok(None)` = rejected with a warning
    /// (or silently for a bare `file://`), `Err` = `ValueError`.
    pub(crate) fn check_path(
        &mut self,
        path: &[u8],
        arg_num: u32,
        contains_file_protocol: bool,
        is_from_array: bool,
        option_name: Option<&str>,
    ) -> PhpResult<Option<Vec<u8>>> {
        if path.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let fs_path = if contains_file_protocol {
            if path.len() <= 7 {
                return Ok(None);
            }
            &path[7..]
        } else {
            path
        };
        let (msg, is_error) = if fs_path.contains(&0) {
            ("must not contain any null bytes", true)
        } else if let Some(real) = expand_filepath(fs_path) {
            return Ok(self.check_open_basedir(&real).then_some(real));
        } else {
            ("must be a valid file path", false)
        };
        if arg_num == 0 {
            let title = option_name.unwrap_or("unknown");
            let label = if is_from_array {
                "array item"
            } else {
                "option"
            };
            self.warn(format!("Path for {title} {label} {msg}"));
            return Ok(None);
        }
        let text = match (is_from_array, option_name) {
            (true, Some(o)) => format!("option {o} array item {msg}"),
            (true, None) => format!("array item {msg}"),
            (false, Some(o)) => format!("option {o} {msg}"),
            (false, None) => msg.to_string(),
        };
        if is_error {
            Err(Throw::ArgumentValueError {
                arg: arg_num,
                message: text,
            })
        } else {
            self.warn(text);
            Ok(None)
        }
    }

    /// `php_check_open_basedir`: `false` (with PHP's warning) when `path`
    /// lies outside every `open_basedir` entry.
    pub(crate) fn check_open_basedir(&mut self, path: &[u8]) -> bool {
        let Some(dirs) = self.open_basedir.clone().filter(|d| !d.is_empty()) else {
            return true;
        };
        let resolve = |p: &[u8]| -> Vec<u8> {
            let s = String::from_utf8_lossy(p).into_owned();
            std::fs::canonicalize(&s).map_or_else(
                |_| expand_filepath(p).unwrap_or_else(|| p.to_vec()),
                |c| c.to_string_lossy().into_owned().into_bytes(),
            )
        };
        let target = resolve(path);
        for dir in dirs.split(|&c| c == b':').filter(|d| !d.is_empty()) {
            let mut base = resolve(dir);
            if dir.ends_with(b"/") && !base.ends_with(b"/") {
                base.push(b'/');
            }
            let mut t = target.clone();
            if base.ends_with(b"/") && !t.ends_with(b"/") && t.len() + 1 == base.len() {
                t.push(b'/');
            }
            if t.starts_with(&base) {
                return true;
            }
        }
        self.warn(format!(
            "open_basedir restriction in effect. File({}) is not within the allowed path(s): ({})",
            String::from_utf8_lossy(path),
            String::from_utf8_lossy(&dirs)
        ));
        false
    }

    /// `php_openssl_check_path` (no `file://` prefix).
    pub(crate) fn check_plain_path(
        &mut self,
        path: &[u8],
        arg_num: u32,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.check_path(path, arg_num, false, false, None)
    }

    /// `php_openssl_bio_new_file`.
    pub(crate) fn bio_new_file(
        &mut self,
        path: &[u8],
        arg_num: u32,
        mode: &str,
    ) -> PhpResult<Option<Bio<'static>>> {
        let Some(real) = self.check_plain_path(path, arg_num)? else {
            return Ok(None);
        };
        let b = Bio::file(&real, mode);
        if b.is_none() {
            self.store_errors();
        }
        Ok(b)
    }

    /// `php_openssl_x509_from_str`.
    pub(crate) fn x509_from_str(
        &mut self,
        s: &[u8],
        arg_num: u32,
        is_from_array: bool,
        option_name: Option<&str>,
    ) -> PhpResult<Option<X509Cert>> {
        let cert = if s.len() > 7 && s.starts_with(b"file://") {
            let Some(path) = self.check_path(s, arg_num, true, is_from_array, option_name)? else {
                return Ok(None);
            };
            let Some(bio) = Bio::file(&path, "r") else {
                self.store_errors();
                return Ok(None);
            };
            X509Cert::read_pem(&bio)
        } else {
            let Some(bio) = Bio::mem_buf(s) else {
                self.store_errors();
                return Ok(None);
            };
            X509Cert::read_pem(&bio)
        };
        if cert.is_none() {
            self.store_errors();
        }
        Ok(cert)
    }

    /// `php_openssl_x509_from_param` / `_from_zval`: returns an owned copy
    /// for strings and a duplicate-free borrow for objects.
    pub(crate) fn cert_from_arg<'c>(
        &mut self,
        arg: CertArg<'c>,
        arg_num: u32,
    ) -> PhpResult<Option<CertRef<'c>>> {
        self.cert_from_arg_ex(arg, arg_num, false, None)
    }

    /// [`Self::cert_from_arg`] with the array/option context used in path
    /// error messages.
    pub(crate) fn cert_from_arg_ex<'c>(
        &mut self,
        arg: CertArg<'c>,
        arg_num: u32,
        is_from_array: bool,
        option_name: Option<&str>,
    ) -> PhpResult<Option<CertRef<'c>>> {
        Ok(match arg {
            CertArg::Object(c) => Some(CertRef::Borrowed(&c.x509)),
            CertArg::Str(s) => self
                .x509_from_str(s, arg_num, is_from_array, option_name)?
                .map(CertRef::Owned),
        })
    }

    /// `php_openssl_csr_from_param`.
    pub(crate) fn csr_from_arg<'c>(
        &mut self,
        arg: CsrArg<'c>,
        arg_num: u32,
    ) -> PhpResult<Option<CsrRef<'c>>> {
        let s = match arg {
            CsrArg::Object(o) => return Ok(Some(CsrRef::Borrowed(&o.req))),
            CsrArg::Str(s) => s,
        };
        let bio = if s.len() > 7 && s.starts_with(b"file://") {
            let Some(path) = self.check_path(s, arg_num, true, false, None)? else {
                return Ok(None);
            };
            Bio::file(&path, "r")
        } else {
            Bio::mem_buf(s)
        };
        let Some(bio) = bio else {
            self.store_errors();
            return Ok(None);
        };
        let csr = X509Req::read_pem(&bio);
        if csr.is_none() {
            self.store_errors();
        }
        Ok(csr.map(CsrRef::Owned))
    }

    /// `php_openssl_pkey_from_zval`.
    pub(crate) fn pkey_from_arg(
        &mut self,
        arg: &KeyArg<'_>,
        public_key: bool,
        passphrase: Option<&[u8]>,
        arg_num: u32,
    ) -> PhpResult<Option<Pkey>> {
        let mut passphrase = passphrase.map(<[u8]>::to_vec);
        let mut val = arg;
        if let KeyArg::Array {
            key,
            passphrase: phrase,
        } = arg
        {
            let Some(phrase) = phrase else {
                return Err(Throw::ValueError(
                    "Key array must be of the form array(0 => key, 1 => phrase)".into(),
                ));
            };
            passphrase = Some(phrase.clone());
            let Some(k) = key else {
                return Err(Throw::ValueError(
                    "Key array must be of the form array(0 => key, 1 => phrase)".into(),
                ));
            };
            val = k;
        }

        let mut key: Option<Pkey> = None;
        let mut cert: Option<CertRef<'_>> = None;
        match val {
            KeyArg::Key(obj) => {
                if !public_key && !obj.is_private {
                    self.warn("Supplied key param is a public key");
                    return Ok(None);
                }
                if public_key && obj.is_private {
                    self.warn("Don't know how to get public key from this private key");
                    return Ok(None);
                }
                return Ok(Some(obj.pkey.clone()));
            }
            KeyArg::Cert(c) => cert = Some(CertRef::Borrowed(&c.x509)),
            KeyArg::Array { .. } | KeyArg::Other => return Ok(None),
            KeyArg::Str(s) => {
                let mut file_path = None;
                if s.len() > 7 && s.starts_with(b"file://") {
                    let Some(p) = self.check_path(s, arg_num, true, false, None)? else {
                        return Ok(None);
                    };
                    file_path = Some(p);
                }
                if public_key {
                    self.errors_set_mark();
                    let c = self.x509_from_str(s, arg_num, false, None)?;
                    if let Some(c) = c {
                        cert = Some(CertRef::Owned(c));
                    } else {
                        self.errors_restore_mark();
                        let bio = match &file_path {
                            Some(p) => Bio::file(p, "r"),
                            None => Bio::mem_buf(s),
                        };
                        let Some(bio) = bio else {
                            self.store_errors();
                            return Ok(None);
                        };
                        key = Pkey::read_public(&bio);
                    }
                } else {
                    let bio = match &file_path {
                        Some(p) => Bio::file(p, "r"),
                        None => Bio::mem_buf(s),
                    };
                    let Some(bio) = bio else { return Ok(None) };
                    key = Pkey::read_private(&bio, passphrase.as_deref());
                }
            }
        }

        if key.is_none() {
            self.store_errors();
            if public_key {
                if let Some(c) = &cert {
                    key = c.get().public_key();
                    if key.is_none() {
                        self.store_errors();
                    }
                }
            }
        }
        Ok(key)
    }

    /// `php_openssl_load_all_certs_from_file`.
    pub(crate) fn load_all_certs_from_file(
        &mut self,
        file: &[u8],
        arg_num: u32,
    ) -> PhpResult<Option<X509Stack>> {
        let Some(path) = self.check_plain_path(file, arg_num)? else {
            return Ok(None);
        };
        let shown = String::from_utf8_lossy(&path).into_owned();
        let Some(bio) = Bio::file(&path, "r") else {
            self.store_errors();
            self.warn(format!("Error opening the file, {shown}"));
            return Ok(None);
        };
        let Some(stack) = ffi::read_x509_infos(&bio) else {
            self.store_errors();
            self.warn(format!("Error reading the file, {shown}"));
            return Ok(None);
        };
        if stack.is_empty() {
            self.warn(format!("No certificates in file, {shown}"));
            return Ok(None);
        }
        Ok(Some(stack))
    }

    /// `php_openssl_setup_verify`: builds a store from CA files and
    /// directories (each item already converted to a string by the VM).
    pub(crate) fn setup_verify(
        &mut self,
        calist: &[Vec<u8>],
        arg_num: u32,
    ) -> PhpResult<Option<X509Store>> {
        let Some(store) = X509Store::new() else {
            self.store_errors();
            return Ok(None);
        };
        let (mut nfiles, mut ndirs) = (0, 0);
        for item in calist {
            let Some(path) = self.check_path(item, arg_num, false, true, None)? else {
                continue;
            };
            let shown = String::from_utf8_lossy(&path).into_owned();
            let Ok(meta) = std::fs::metadata(String::from_utf8_lossy(&path).as_ref()) else {
                self.warn(format!("Unable to stat {shown}"));
                continue;
            };
            if meta.is_file() {
                if store.load_file(Some(&path)) {
                    nfiles += 1;
                } else {
                    self.store_errors();
                    self.warn(format!("Error loading file {shown}"));
                }
            } else if store.add_dir(Some(&path)) {
                ndirs += 1;
            } else {
                self.store_errors();
                self.warn(format!("Error loading directory {shown}"));
            }
        }
        if nfiles == 0 && !store.load_file(None) {
            self.store_errors();
        }
        if ndirs == 0 && !store.add_dir(None) {
            self.store_errors();
        }
        Ok(Some(store))
    }

    /// `php_openssl_array_to_X509_sk`: a stack from a list of certificate
    /// arguments (`is_array` false for a single scalar argument).
    pub(crate) fn certs_to_stack(
        &mut self,
        certs: &[CertArg<'_>],
        is_array: bool,
        arg_num: u32,
        option_name: Option<&str>,
    ) -> PhpResult<Option<X509Stack>> {
        let Some(sk) = X509Stack::new() else {
            return Ok(None);
        };
        for c in certs {
            let Some(cert) = self.cert_from_arg_ex(*c, arg_num, is_array, option_name)? else {
                // PHP returns the partially filled stack here.
                return Ok(Some(sk));
            };
            let Some(owned) = cert.into_owned() else {
                self.store_errors();
                return Ok(Some(sk));
            };
            if !sk.push(owned) {
                self.store_errors();
                return Ok(None);
            }
        }
        Ok(Some(sk))
    }
}

/// A certificate that is either borrowed from an object or freshly parsed.
#[derive(Debug)]
pub(crate) enum CertRef<'a> {
    Borrowed(&'a X509Cert),
    Owned(X509Cert),
}

impl CertRef<'_> {
    pub(crate) fn get(&self) -> &X509Cert {
        match self {
            CertRef::Borrowed(c) => c,
            CertRef::Owned(c) => c,
        }
    }

    /// An owned certificate (duplicating borrowed ones like `X509_dup`).
    pub(crate) fn into_owned(self) -> Option<X509Cert> {
        match self {
            CertRef::Borrowed(c) => c.dup(),
            CertRef::Owned(c) => Some(c),
        }
    }
}

/// A CSR that is either borrowed from an object or freshly parsed.
#[derive(Debug)]
pub(crate) enum CsrRef<'a> {
    Borrowed(&'a X509Req),
    Owned(X509Req),
}

impl CsrRef<'_> {
    pub(crate) fn get(&self) -> &X509Req {
        match self {
            CsrRef::Borrowed(c) => c,
            CsrRef::Owned(c) => c,
        }
    }
}
