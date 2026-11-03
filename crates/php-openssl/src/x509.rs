//! X.509 certificates: `openssl_x509_*`.

use crate::args::{CertArg, Certificate, KeyArg};
use crate::error::PhpResult;
use crate::ffi::{self, Asn1Str, Bio, CheckCert, ExtText, NameRef, X509Cert, X509Stack, X509Store};
use crate::util::{atoi, bin2hex, timegm};
use crate::value::{BoolOrInt, PhpArray, PhpValue};
use crate::OpenSsl;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl OpenSsl {
    /// `php_openssl_add_assoc_name_entry`: subject/issuer as an array,
    /// repeated attributes collected into nested arrays.
    pub(crate) fn name_entries(&mut self, name: NameRef<'_>, short_names: bool) -> PhpArray {
        let mut out = PhpArray::new();
        for e in name.entries() {
            let key = if short_names {
                ffi::nid2sn(e.nid)
            } else {
                ffi::nid2ln(e.nid)
            }
            .unwrap_or_default();
            let Some(value) = e.value else {
                self.store_errors();
                continue;
            };
            let k = crate::value::Key::Str(key.into_bytes());
            match out.get_mut(&k) {
                Some(PhpValue::Array(a)) => a.push(PhpValue::Str(value)),
                Some(slot @ PhpValue::Str(_)) => {
                    let old = std::mem::replace(slot, PhpValue::Null);
                    let mut a = PhpArray::new();
                    a.push(old);
                    a.push(PhpValue::Str(value));
                    *slot = PhpValue::Array(a);
                }
                Some(_) => {}
                None => out.set(k, PhpValue::Str(value)),
            }
        }
        out
    }

    /// `php_openssl_asn1_time_to_time_t`.
    pub(crate) fn asn1_time_to_time_t(&mut self, t: &Asn1Str) -> i64 {
        if t.kind != ffi::V_ASN1_UTCTIME && t.kind != ffi::V_ASN1_GENERALIZEDTIME {
            self.warn("Illegal ASN1 data type for timestamp");
            return -1;
        }
        let b = &t.data;
        if b.contains(&0) {
            self.warn("Illegal length in timestamp");
            return -1;
        }
        let len = b.len();
        if len < 13 || (t.kind == ffi::V_ASN1_GENERALIZEDTIME && len < 15) {
            self.warn(format!(
                "Unable to parse time string {} correctly",
                lossy(b)
            ));
            return -1;
        }
        let sec = atoi(&b[len - 3..]);
        let min = atoi(&b[len - 5..len - 3]);
        let hour = atoi(&b[len - 7..len - 5]);
        let mday = atoi(&b[len - 9..len - 7]);
        let mon = atoi(&b[len - 11..len - 9]) - 1;
        let year = if t.kind == ffi::V_ASN1_UTCTIME {
            let y = atoi(&b[len - 13..len - 11]);
            1900 + if y < 68 { y + 100 } else { y }
        } else {
            atoi(&b[len - 15..len - 11])
        };
        timegm(year, mon, mday, hour, min, sec)
    }

    /// `openssl_x509_read()`.
    pub fn x509_read(&mut self, cert: CertArg<'_>) -> PhpResult<Option<Certificate>> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(None);
        };
        let Some(x509) = c.into_owned() else {
            self.warn("X.509 Certificate could not be duplicated");
            return Ok(None);
        };
        Ok(Some(Certificate { x509 }))
    }

    /// `openssl_x509_free()`: a no-op.
    pub fn x509_free(&mut self, _cert: &Certificate) {}

    /// `openssl_x509_parse()`. `Ok(None)` = `false`.
    pub fn x509_parse(
        &mut self,
        cert: CertArg<'_>,
        short_names: bool,
    ) -> PhpResult<Option<PhpArray>> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            return Ok(None);
        };
        Ok(self.x509_parse_cert(c.get(), short_names))
    }

    fn x509_parse_cert(&mut self, x: &X509Cert, short: bool) -> Option<PhpArray> {
        let mut out = PhpArray::new();
        let Some(name) = x.subject().oneline() else {
            self.store_errors();
            return None;
        };
        out.set("name", PhpValue::Str(name));
        let subject = self.name_entries(x.subject(), short);
        out.set("subject", PhpValue::Array(subject));
        out.set(
            "hash",
            PhpValue::str(format!("{:08x}", x.subject_name_hash())),
        );
        let issuer = self.name_entries(x.issuer(), short);
        out.set("issuer", PhpValue::Array(issuer));
        out.set("version", PhpValue::Int(x.version()));
        let Some((hex, dec)) = x.serial() else {
            self.store_errors();
            return None;
        };
        out.set("serialNumber", PhpValue::Str(dec));
        out.set("serialNumberHex", PhpValue::Str(hex));
        let nb = x.not_before();
        let na = x.not_after();
        out.set("validFrom", PhpValue::Str(nb.data.clone()));
        out.set("validTo", PhpValue::Str(na.data.clone()));
        let t1 = self.asn1_time_to_time_t(&nb);
        out.set("validFrom_time_t", PhpValue::Int(t1));
        let t2 = self.asn1_time_to_time_t(&na);
        out.set("validTo_time_t", PhpValue::Int(t2));
        if let Some(alias) = x.alias() {
            out.set("alias", PhpValue::Str(alias));
        }
        let sig = x.signature_nid();
        out.set(
            "signatureTypeSN",
            PhpValue::str(ffi::nid2sn(sig).unwrap_or_default()),
        );
        out.set(
            "signatureTypeLN",
            PhpValue::str(ffi::nid2ln(sig).unwrap_or_default()),
        );
        out.set("signatureTypeNID", PhpValue::Int(i64::from(sig)));

        let mut purposes = PhpArray::new();
        for p in ffi::purposes() {
            let mut sub = PhpArray::new();
            sub.set(0i64, PhpValue::Bool(x.check_purpose(p.id, 0) != 0));
            sub.set(1i64, PhpValue::Bool(x.check_purpose(p.id, 1) != 0));
            sub.set(2i64, PhpValue::str(if short { p.sname } else { p.name }));
            purposes.set(i64::from(p.id), PhpValue::Array(sub));
        }
        out.set("purposes", PhpValue::Array(purposes));

        let mut exts = PhpArray::new();
        let mut crit = PhpArray::new();
        for (i, e) in x.extensions().into_iter().enumerate() {
            if e.critical {
                crit.push(PhpValue::str(&e.name));
            }
            let text = match x.extension_text(i) {
                ExtText::Text(t) => t,
                ExtText::Unprintable => e.data,
                ExtText::Failed => {
                    self.store_errors();
                    return None;
                }
            };
            exts.set(e.name.as_str(), PhpValue::Str(text));
        }
        out.set("extensions", PhpValue::Array(exts));
        if !crit.is_empty() {
            out.set("criticalExtensions", PhpValue::Array(crit));
        }
        Some(out)
    }

    /// `php_openssl_check_cert`: 0/1 or another verification result.
    pub(crate) fn check_cert(
        &mut self,
        store: &X509Store,
        cert: &X509Cert,
        untrusted: Option<&X509Stack>,
        purpose: i32,
    ) -> i32 {
        match ffi::check_cert(store, cert, untrusted, purpose) {
            CheckCert::NoMemory => {
                self.store_errors();
                self.warn("Memory allocation failure");
                0
            }
            CheckCert::InitFailed => {
                self.store_errors();
                self.warn("Certificate store initialization failed");
                0
            }
            CheckCert::Verified { result, .. } => {
                // Errors of a failed set_purpose and of verification both
                // stay queued; store them in order.
                self.store_errors();
                result
            }
        }
    }

    /// `openssl_x509_checkpurpose()`. `cainfo` items are pre-converted to
    /// strings by the VM.
    pub fn x509_checkpurpose(
        &mut self,
        cert: CertArg<'_>,
        purpose: i64,
        cainfo: &[Vec<u8>],
        untrusted_file: Option<&[u8]>,
    ) -> PhpResult<BoolOrInt> {
        let mut untrusted = None;
        if let Some(f) = untrusted_file {
            match self.load_all_certs_from_file(f, 4)? {
                Some(s) => untrusted = Some(s),
                None => return Ok(BoolOrInt::Int(-1)),
            }
        }
        let Some(store) = self.setup_verify(cainfo, 3)? else {
            return Ok(BoolOrInt::Int(-1));
        };
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            return Ok(BoolOrInt::Int(-1));
        };
        #[allow(clippy::cast_possible_truncation)]
        let ret = self.check_cert(&store, c.get(), untrusted.as_ref(), purpose as i32);
        Ok(if ret == 0 || ret == 1 {
            BoolOrInt::Bool(ret == 1)
        } else {
            BoolOrInt::Int(i64::from(ret))
        })
    }

    /// `openssl_x509_fingerprint()`.
    pub fn x509_fingerprint(
        &mut self,
        cert: CertArg<'_>,
        digest_algo: &[u8],
        binary: bool,
    ) -> PhpResult<Option<Vec<u8>>> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(None);
        };
        Ok(self.x509_fingerprint_raw(c.get(), digest_algo, binary))
    }

    /// `php_openssl_x509_fingerprint`.
    pub(crate) fn x509_fingerprint_raw(
        &mut self,
        x: &X509Cert,
        method: &[u8],
        raw: bool,
    ) -> Option<Vec<u8>> {
        let Some(md) = ffi::md_by_name(method) else {
            self.warn("Unknown digest algorithm");
            return None;
        };
        let Some(d) = x.digest(&md) else {
            self.store_errors();
            self.warn("Could not generate signature");
            return None;
        };
        Some(if raw { d } else { bin2hex(&d) })
    }

    /// `openssl_x509_check_private_key()`.
    pub fn x509_check_private_key(
        &mut self,
        cert: CertArg<'_>,
        private_key: &KeyArg<'_>,
    ) -> PhpResult<bool> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            return Ok(false);
        };
        let Some(key) = self.pkey_from_arg(private_key, false, Some(b""), 2)? else {
            return Ok(false);
        };
        Ok(c.get().check_private_key(&key))
    }

    /// `openssl_x509_verify()`: 1, 0, or -1.
    pub fn x509_verify(&mut self, cert: CertArg<'_>, public_key: &KeyArg<'_>) -> PhpResult<i64> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            return Ok(-1);
        };
        let Some(key) = self.pkey_from_arg(public_key, true, None, 2)? else {
            return Ok(-1);
        };
        let r = c.get().verify(&key);
        if r < 0 {
            self.store_errors();
        }
        Ok(i64::from(r))
    }

    /// `openssl_x509_export()`: the text for `$output`, or `None` (false).
    pub fn x509_export(&mut self, cert: CertArg<'_>, no_text: bool) -> PhpResult<Option<Vec<u8>>> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(None);
        };
        let Some(bio) = Bio::mem() else {
            self.store_errors();
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

    /// `openssl_x509_export_to_file()`.
    pub fn x509_export_to_file(
        &mut self,
        cert: CertArg<'_>,
        output_filename: &[u8],
        no_text: bool,
    ) -> PhpResult<bool> {
        let Some(c) = self.cert_from_arg(cert, 1)? else {
            self.warn("X.509 Certificate cannot be retrieved");
            return Ok(false);
        };
        let Some(path) = self.check_plain_path(output_filename, 2)? else {
            return Ok(false);
        };
        let Some(bio) = Bio::file(&path, "w") else {
            self.store_errors();
            self.warn(format!("Error opening file {}", lossy(&path)));
            self.store_errors();
            return Ok(false);
        };
        if (no_text || c.get().print(&bio)) && c.get().write_pem(&bio) {
            Ok(true)
        } else {
            self.store_errors();
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_conversion() {
        let mut o = OpenSsl::new();
        let t = Asn1Str {
            kind: ffi::V_ASN1_UTCTIME,
            data: b"080515203041Z".to_vec(),
        };
        assert_eq!(o.asn1_time_to_time_t(&t), 1_210_883_441);
        let t = Asn1Str {
            kind: ffi::V_ASN1_GENERALIZEDTIME,
            data: b"20500101000000Z".to_vec(),
        };
        assert_eq!(o.asn1_time_to_time_t(&t), 2_524_608_000);
        let t = Asn1Str {
            kind: ffi::V_ASN1_UTCTIME,
            data: b"0805".to_vec(),
        };
        assert_eq!(o.asn1_time_to_time_t(&t), -1);
        assert_eq!(
            o.take_diagnostics()[0].message,
            "Unable to parse time string 0805 correctly"
        );
    }
}
