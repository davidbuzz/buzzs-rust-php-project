//! Certificate signing requests: `openssl_csr_*`.

use crate::args::{AsymmetricKey, CertArg, Certificate, CertificateSigningRequest, CsrArg, KeyArg};
use crate::config::{conf_get_string, ConfigArgs, ReqConfig};
use crate::error::PhpResult;
use crate::ffi::{self, Bio, ExtTarget, Pkey, X509Cert, X509Name, X509Req};
use crate::value::{Key, PhpArray, PhpValue};
use crate::OpenSsl;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `LONG_MAX / 86400`.
const MAX_DAYS: i64 = i64::MAX / 86400;

/// Return value of `openssl_csr_new`.
#[derive(Debug)]
pub enum CsrNewReturn {
    /// `false`.
    False,
    /// `true` (the request was built but signing it failed).
    True,
    /// The new request object.
    Csr(CertificateSigningRequest),
}

/// Result of [`OpenSsl::csr_new`].
#[derive(Debug)]
pub struct CsrNew {
    /// The PHP return value.
    pub ret: CsrNewReturn,
    /// A freshly generated key to assign to `$private_key` (by reference);
    /// `None` leaves the variable untouched.
    pub generated_key: Option<AsymmetricKey>,
}

impl OpenSsl {
    /// Converts a DN/attribute value like `zval_try_get_string`.
    fn value_string(&mut self, v: &PhpValue) -> Vec<u8> {
        v.to_php_string().unwrap_or_else(|| {
            if matches!(v, PhpValue::Array(_)) {
                self.warn("Array to string conversion");
                b"Array".to_vec()
            } else {
                Vec::new()
            }
        })
    }

    /// `php_openssl_csr_add_subj_entry`.
    fn csr_add_subj_entry(&mut self, item: &PhpValue, subj: &X509Name, nid: i32) -> bool {
        let s = self.value_string(item);
        if subj.add_by_nid(nid, &s) {
            return true;
        }
        self.store_errors();
        let shown = lossy(&s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]);
        self.warn(format!(
            "dn: add_entry_by_NID {nid} -> {shown} (failed; check error queue and value of string_mask OpenSSL option if illegal characters are reported)"
        ));
        false
    }

    /// `php_openssl_csr_make`.
    fn csr_make(
        &mut self,
        req: &ReqConfig,
        key: &Pkey,
        csr: &X509Req,
        dn: &PhpArray,
        attribs: Option<&PhpArray>,
    ) -> bool {
        let conf = &req.req_config;
        let sec = Some(&req.section_name[..]);
        let Some(dn_sect) = conf.get_string(sec, b"distinguished_name") else {
            self.store_errors();
            return false;
        };
        let Some(dn_sk) = conf.section(&dn_sect) else {
            self.store_errors();
            return false;
        };
        let attr_sk = match conf_get_string(conf, sec, "attributes") {
            None => None,
            Some(s) => {
                let Some(sk) = conf.section(&s) else {
                    self.store_errors();
                    return false;
                };
                Some(sk)
            }
        };
        if !csr.set_version(0) {
            self.store_errors();
            return false;
        }
        let Some(subj) = X509Name::new() else {
            self.store_errors();
            return false;
        };
        for (k, item) in dn.iter() {
            let Key::Str(name) = k else { continue };
            let nid = ffi::txt2nid(name);
            if nid == 0 {
                self.warn(format!("dn: {} is not a recognized name", lossy(name)));
                continue;
            }
            if let PhpValue::Array(items) = item {
                for sub in items.values() {
                    if !self.csr_add_subj_entry(sub, &subj, nid) {
                        return false;
                    }
                }
            } else if !self.csr_add_subj_entry(item, &subj, nid) {
                return false;
            }
        }
        for v in &dn_sk {
            let name = &v.name;
            if name.len() < 9 || !name.ends_with(b"_default") {
                continue;
            }
            let mut ty = &name[..(name.len() - 8).min(200)];
            if let Some(p) = ty.iter().position(|&c| c == b':' || c == b',' || c == b'.') {
                if p + 1 < ty.len() {
                    ty = &ty[p + 1..];
                }
            }
            let nid = ffi::txt2nid(ty);
            if subj.index_by_nid(nid) >= 0 {
                continue;
            }
            if !subj.add_by_txt(ty, &v.value) {
                self.store_errors();
                self.warn(format!(
                    "add_entry_by_txt {} -> {} (failed)",
                    lossy(ty),
                    lossy(&v.value)
                ));
                return false;
            }
            if subj.entry_count() == 0 {
                self.warn("No objects specified in config file");
                return false;
            }
        }
        if !csr.set_subject(&subj) {
            self.store_errors();
            return false;
        }
        if let Some(attribs) = attribs {
            for (k, item) in attribs.iter() {
                let Key::Str(name) = k else {
                    self.warn("attributes: numeric fild names are not supported");
                    continue;
                };
                let nid = ffi::txt2nid(name);
                if nid == 0 {
                    self.warn(format!(
                        "attributes: {} is not a recognized attribute name",
                        lossy(name)
                    ));
                    continue;
                }
                let s = self.value_string(item);
                if !csr.add_attr_by_nid(nid, &s) {
                    self.store_errors();
                    self.warn(format!(
                        "attributes: add_attr_by_NID {nid} -> {} (failed)",
                        lossy(&s)
                    ));
                    return false;
                }
            }
            for v in attr_sk.iter().flatten() {
                let nid = ffi::txt2nid(&v.name);
                if csr.attr_index(nid) >= 0 {
                    continue;
                }
                if !csr.add_attr_by_txt(&v.name, &v.value) {
                    self.store_errors();
                    self.warn(format!(
                        "add1_attr_by_txt {} -> {} (failed; check error queue and value of string_mask OpenSSL option if illegal characters are reported)",
                        lossy(&v.name),
                        lossy(&v.value)
                    ));
                    return false;
                }
            }
        }
        if !csr.set_pubkey(key) {
            self.store_errors();
        }
        true
    }

    /// `openssl_csr_new()`. `private_key` is the current value of the
    /// by-reference `$private_key` (`None` when it is `null`).
    pub fn csr_new(
        &mut self,
        dn: &PhpArray,
        private_key: Option<&KeyArg<'_>>,
        options: Option<&PhpArray>,
        extra_attributes: Option<&PhpArray>,
    ) -> PhpResult<CsrNew> {
        let mut pending = None;
        let r = self.csr_new_inner(dn, private_key, options, extra_attributes, &mut pending);
        match pending {
            Some(e) => Err(e),
            None => r,
        }
    }

    fn csr_new_inner(
        &mut self,
        dn: &PhpArray,
        private_key: Option<&KeyArg<'_>>,
        options: Option<&PhpArray>,
        extra_attributes: Option<&PhpArray>,
        pending: &mut Option<crate::Throw>,
    ) -> PhpResult<CsrNew> {
        let mut result = CsrNew {
            ret: CsrNewReturn::False,
            generated_key: None,
        };
        let args = options.map(ConfigArgs::from_array);
        let Some(req) = self.parse_config(args.as_ref())? else {
            return Ok(result);
        };
        // PHP keeps going after a ValueError from the key conversion and
        // only surfaces the exception when the function returns.
        let mut key = match private_key {
            Some(k) => self.pkey_from_arg(k, false, None, 2).unwrap_or_else(|e| {
                *pending = Some(e);
                None
            }),
            None => None,
        };
        let mut we_made_the_key = false;
        if key.is_none() {
            key = self.generate_private_key(&req);
            we_made_the_key = true;
        }
        let Some(key) = key else {
            self.warn("Unable to generate a private key");
            return Ok(result);
        };
        let Some(csr) = X509Req::new() else {
            self.store_errors();
            return Ok(result);
        };
        if !self.csr_make(&req, &key, &csr, dn, extra_attributes) {
            return Ok(result);
        }
        if let Some(sect) = &req.request_extensions_section {
            if !req.req_config.add_extensions(sect, &ExtTarget::Req(&csr)) {
                self.store_errors();
                self.warn(format!("Error loading extension section {}", lossy(sect)));
                return Ok(result);
            }
        }
        result.ret = CsrNewReturn::True;
        if csr.sign(&key, req.digest.as_ref()) {
            result.ret = CsrNewReturn::Csr(CertificateSigningRequest { req: csr });
        } else {
            self.store_errors();
            self.warn("Error signing request");
        }
        if we_made_the_key {
            result.generated_key = Some(AsymmetricKey {
                pkey: key,
                is_private: true,
            });
        }
        Ok(result)
    }

    /// `openssl_csr_sign()`.
    #[allow(clippy::too_many_arguments)]
    pub fn csr_sign(
        &mut self,
        csr: CsrArg<'_>,
        ca_certificate: Option<CertArg<'_>>,
        private_key: &KeyArg<'_>,
        days: i64,
        options: Option<&PhpArray>,
        serial: i64,
        serial_hex: Option<&[u8]>,
    ) -> PhpResult<Option<Certificate>> {
        let Some(csr) = self.csr_from_arg(csr, 1)? else {
            self.warn("X.509 Certificate Signing Request cannot be retrieved");
            return Ok(None);
        };
        let csr = csr.get();
        let ca = match ca_certificate {
            Some(c) => {
                let Some(c) = self.cert_from_arg(c, 2)? else {
                    self.warn("X.509 Certificate cannot be retrieved");
                    return Ok(None);
                };
                Some(c)
            }
            None => None,
        };
        let Some(priv_key) = self.pkey_from_arg(private_key, false, Some(b""), 3)? else {
            self.warn("Cannot get private key from parameter 3");
            return Ok(None);
        };
        if let Some(c) = &ca {
            if !c.get().check_private_key(&priv_key) {
                self.store_errors();
                self.warn("Private key does not correspond to signing cert");
                return Ok(None);
            }
        }
        if !(0..=MAX_DAYS).contains(&days) {
            self.warn(format!("Days must be between 0 and {MAX_DAYS}"));
            return Ok(None);
        }
        let args = options.map(ConfigArgs::from_array);
        let Some(req) = self.parse_config(args.as_ref())? else {
            return Ok(None);
        };
        let Some(key) = csr.public_key() else {
            self.store_errors();
            self.warn("Error unpacking public key");
            return Ok(None);
        };
        match csr.verify(&key) {
            i if i < 0 => {
                self.store_errors();
                self.warn("Signature verification problems");
                return Ok(None);
            }
            0 => {
                self.warn("Signature did not match the certificate request");
                return Ok(None);
            }
            _ => {}
        }
        let Some(new_cert) = X509Cert::new() else {
            self.store_errors();
            self.warn("No memory");
            return Ok(None);
        };
        if !new_cert.set_version(2) {
            return Ok(None);
        }
        if let Some(hex) = serial_hex {
            if hex.len() > 200 {
                self.warn("Error parsing serial number because it is too long");
                return Ok(None);
            }
            if !new_cert.set_serial_hex(hex) {
                self.store_errors();
                self.warn("Error parsing serial number");
                return Ok(None);
            }
        } else if !new_cert.set_serial(serial) {
            self.store_errors();
            self.warn("Error setting serial number");
            return Ok(None);
        }
        if !new_cert.set_subject(csr.subject()) {
            self.store_errors();
            return Ok(None);
        }
        let issuer_cert = ca.as_ref().map(crate::args::CertRef::get);
        let issuer_name = issuer_cert.unwrap_or(&new_cert).subject();
        if !new_cert.set_issuer(issuer_name) {
            self.store_errors();
            return Ok(None);
        }
        if !new_cert.set_validity(60 * 60 * 24 * days) {
            self.store_errors();
            return Ok(None);
        }
        if !new_cert.set_pubkey(&key) {
            self.store_errors();
            return Ok(None);
        }
        if let Some(sect) = &req.extensions_section {
            let target = ExtTarget::Cert {
                issuer: issuer_cert,
                subject: &new_cert,
                req: csr,
            };
            if !req.req_config.add_extensions(sect, &target) {
                self.store_errors();
                return Ok(None);
            }
        }
        if !new_cert.sign(&priv_key, req.digest.as_ref()) {
            self.store_errors();
            self.warn("Failed to sign it");
            return Ok(None);
        }
        Ok(Some(Certificate { x509: new_cert }))
    }

    /// `openssl_csr_get_subject()`.
    pub fn csr_get_subject(
        &mut self,
        csr: CsrArg<'_>,
        short_names: bool,
    ) -> PhpResult<Option<PhpArray>> {
        let Some(c) = self.csr_from_arg(csr, 1)? else {
            return Ok(None);
        };
        Ok(Some(self.name_entries(c.get().subject(), short_names)))
    }

    /// `openssl_csr_get_public_key()`.
    pub fn csr_get_public_key(
        &mut self,
        csr: CsrArg<'_>,
        _short_names: bool,
    ) -> PhpResult<Option<AsymmetricKey>> {
        let Some(c) = self.csr_from_arg(csr, 1)? else {
            return Ok(None);
        };
        let pubkey = c.get().public_key().and_then(|k| {
            let pem = k.public_pem()?;
            let bio = Bio::mem_buf(&pem)?;
            Pkey::read_public(&bio)
        });
        let Some(pkey) = pubkey else {
            self.store_errors();
            return Ok(None);
        };
        Ok(Some(AsymmetricKey {
            pkey,
            is_private: false,
        }))
    }

    /// `openssl_csr_export()`: the text for `$output` or `None` (false).
    pub fn csr_export(&mut self, csr: CsrArg<'_>, no_text: bool) -> PhpResult<Option<Vec<u8>>> {
        let Some(c) = self.csr_from_arg(csr, 1)? else {
            self.warn("X.509 Certificate Signing Request cannot be retrieved");
            return Ok(None);
        };
        let Some(bio) = Bio::mem() else {
            return Ok(None);
        };
        if !no_text && !c.get().print(&bio) {
            self.store_errors();
            return Ok(None);
        }
        if c.get().write_pem(&bio) {
            Ok(Some(bio.contents()))
        } else {
            self.store_errors();
            Ok(None)
        }
    }

    /// `openssl_csr_export_to_file()`.
    pub fn csr_export_to_file(
        &mut self,
        csr: CsrArg<'_>,
        output_filename: &[u8],
        no_text: bool,
    ) -> PhpResult<bool> {
        let Some(c) = self.csr_from_arg(csr, 1)? else {
            self.warn("X.509 Certificate Signing Request cannot be retrieved");
            return Ok(false);
        };
        let Some(path) = self.check_plain_path(output_filename, 2)? else {
            return Ok(false);
        };
        let Some(bio) = Bio::file(&path, "w") else {
            self.store_errors();
            self.warn(format!("Error opening file {}", lossy(&path)));
            return Ok(false);
        };
        if !no_text && !c.get().print(&bio) {
            self.store_errors();
            Ok(false)
        } else if !c.get().write_pem(&bio) {
            self.warn(format!("Error writing PEM to file {}", lossy(&path)));
            self.store_errors();
            Ok(false)
        } else {
            Ok(true)
        }
    }
}
