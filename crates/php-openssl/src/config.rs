//! `struct php_x509_request`: the `$options` array of key/CSR functions
//! merged with the OpenSSL configuration file (`php_openssl_parse_config`),
//! and private-key generation (`php_openssl_generate_private_key`).

use crate::constants::{
    OPENSSL_KEYTYPE_DH, OPENSSL_KEYTYPE_DSA, OPENSSL_KEYTYPE_EC, OPENSSL_KEYTYPE_ED25519,
    OPENSSL_KEYTYPE_ED448, OPENSSL_KEYTYPE_RSA, OPENSSL_KEYTYPE_X25519, OPENSSL_KEYTYPE_X448,
};
use crate::error::PhpResult;
use crate::ffi::{self, pkey_id, Bio, Cipher, Conf, ExtTarget, Md, Pkey, PkeyCtx};
use crate::value::{PhpArray, PhpValue};
use crate::OpenSsl;

/// `MIN_KEY_LENGTH`.
pub const MIN_KEY_LENGTH: i64 = 384;

/// The recognised entries of a `$options` array, typed exactly as PHP
/// checks them (`IS_STRING` / `IS_LONG` / presence).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigArgs {
    /// `config` (string).
    pub config: Option<Vec<u8>>,
    /// `config_section_name` (string).
    pub config_section_name: Option<Vec<u8>>,
    /// `digest_alg` (string).
    pub digest_alg: Option<Vec<u8>>,
    /// `x509_extensions` (string).
    pub x509_extensions: Option<Vec<u8>>,
    /// `req_extensions` (string).
    pub req_extensions: Option<Vec<u8>>,
    /// `private_key_bits` (int).
    pub private_key_bits: Option<i64>,
    /// `private_key_type` (int).
    pub private_key_type: Option<i64>,
    /// `encrypt_key`: `Some(value === true)` when present.
    pub encrypt_key: Option<bool>,
    /// `encrypt_key_cipher` (int).
    pub encrypt_key_cipher: Option<i64>,
    /// `curve_name` (string).
    pub curve_name: Option<Vec<u8>>,
}

impl ConfigArgs {
    /// Extracts the options from a PHP array.
    pub fn from_array(a: &PhpArray) -> Self {
        let s = |k: &str| a.get_str(k).and_then(PhpValue::as_str).map(<[u8]>::to_vec);
        let i = |k: &str| a.get_str(k).and_then(PhpValue::as_int);
        ConfigArgs {
            config: s("config"),
            config_section_name: s("config_section_name"),
            digest_alg: s("digest_alg"),
            x509_extensions: s("x509_extensions"),
            req_extensions: s("req_extensions"),
            private_key_bits: i("private_key_bits"),
            private_key_type: i("private_key_type"),
            encrypt_key: a
                .get_str("encrypt_key")
                .map(|v| matches!(v, PhpValue::Bool(true))),
            encrypt_key_cipher: i("encrypt_key_cipher"),
            curve_name: s("curve_name"),
        }
    }
}

/// A parsed request configuration.
#[derive(Debug)]
pub(crate) struct ReqConfig {
    pub global_config: Option<Conf>,
    pub req_config: Conf,
    pub config_filename: Vec<u8>,
    pub section_name: Vec<u8>,
    pub digest: Option<Md>,
    pub extensions_section: Option<Vec<u8>>,
    pub request_extensions_section: Option<Vec<u8>>,
    pub priv_key_bits: i64,
    pub priv_key_type: i64,
    pub priv_key_encrypt: bool,
    pub priv_key_encrypt_cipher: Option<Cipher>,
    pub curve_name: i32,
}

/// `php_openssl_cipher_names` (indexed by `OPENSSL_CIPHER_*`).
const CIPHER_NAMES: [&str; 8] = [
    "RC2-40-CBC",
    "RC2-CBC",
    "RC2-64-CBC",
    "DES-CBC",
    "DES-EDE3-CBC",
    "AES-128-CBC",
    "AES-192-CBC",
    "AES-256-CBC",
];

/// `php_openssl_get_evp_cipher_from_algo`.
pub(crate) fn cipher_from_algo(algo: i64) -> Option<Cipher> {
    let name = CIPHER_NAMES.get(usize::try_from(algo).ok()?)?;
    ffi::cipher_by_name(name.as_bytes())
}

/// `php_openssl_get_evp_md_from_algo`.
pub(crate) fn md_from_algo(algo: i64) -> Option<Md> {
    let name: &str = match algo {
        1 => "SHA1",
        2 => "MD5",
        3 => "MD4",
        6 => "SHA224",
        7 => "SHA256",
        8 => "SHA384",
        9 => "SHA512",
        10 => "RIPEMD160",
        _ => return None,
    };
    ffi::md_by_name(name.as_bytes())
}

/// `php_openssl_conf_get_string`: lookup without leaving errors behind.
pub(crate) fn conf_get_string(conf: &Conf, group: Option<&[u8]>, name: &str) -> Option<Vec<u8>> {
    ffi::err_set_mark();
    let s = conf.get_string(group, name.as_bytes());
    ffi::err_pop_to_mark();
    s
}

fn conf_get_number(conf: &Conf, group: Option<&[u8]>, name: &str) -> i64 {
    ffi::err_set_mark();
    let n = conf.get_number(group, name.as_bytes());
    ffi::err_pop_to_mark();
    n
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl OpenSsl {
    /// `php_openssl_parse_config`; `Ok(None)` = `FAILURE`.
    pub(crate) fn parse_config(
        &mut self,
        args: Option<&ConfigArgs>,
    ) -> PhpResult<Option<ReqConfig>> {
        let default = ConfigArgs::default();
        let a = args.unwrap_or(&default);
        let config_filename = a
            .config
            .clone()
            .unwrap_or_else(|| self.default_conf.clone());
        let section_name = a
            .config_section_name
            .clone()
            .unwrap_or_else(|| b"req".to_vec());

        let global_config = Conf::new();
        if let Some(g) = &global_config {
            if !g.load(&self.default_conf) {
                self.store_errors();
            }
        }
        let Some(req_config) = Conf::new() else {
            return Ok(None);
        };
        if !req_config.load(&config_filename) {
            return Ok(None);
        }

        if let Some(oid_file) = conf_get_string(&req_config, None, "oid_file") {
            if let Some(path) = self.check_path(&oid_file, 0, false, false, Some("oid_file"))? {
                if let Some(bio) = Bio::file(&path, "r") {
                    ffi::obj_create_objects(&bio);
                    drop(bio);
                    self.store_errors();
                }
            }
        }
        if !self.add_oid_section(&req_config) {
            return Ok(None);
        }

        let sec = Some(&section_name[..]);
        let mut digest_name = a
            .digest_alg
            .clone()
            .or_else(|| conf_get_string(&req_config, sec, "default_md"));
        if digest_name.is_none() {
            self.store_errors();
        }
        let extensions_section = a
            .x509_extensions
            .clone()
            .or_else(|| conf_get_string(&req_config, sec, "x509_extensions"));
        if extensions_section.is_none() {
            self.store_errors();
        }
        let request_extensions_section = a
            .req_extensions
            .clone()
            .or_else(|| conf_get_string(&req_config, sec, "req_extensions"));
        if request_extensions_section.is_none() {
            self.store_errors();
        }
        // SET_OPTIONAL_LONG_ARG stores into an `int`.
        #[allow(clippy::cast_possible_truncation)]
        let priv_key_bits = a.private_key_bits.map_or_else(
            || conf_get_number(&req_config, sec, "default_bits") as i32,
            |b| b as i32,
        );
        #[allow(clippy::cast_possible_truncation)]
        let priv_key_type = a
            .private_key_type
            .map_or(OPENSSL_KEYTYPE_RSA, |t| i64::from(t as i32));

        let priv_key_encrypt = if let Some(e) = a.encrypt_key {
            e
        } else {
            let s = conf_get_string(&req_config, sec, "encrypt_rsa_key")
                .or_else(|| conf_get_string(&req_config, sec, "encrypt_key"));
            s.as_deref() != Some(b"no")
        };

        let mut priv_key_encrypt_cipher = None;
        if priv_key_encrypt {
            if let Some(algo) = a.encrypt_key_cipher {
                let Some(c) = cipher_from_algo(algo) else {
                    self.warn("Unknown cipher algorithm for private key");
                    return Ok(None);
                };
                priv_key_encrypt_cipher = Some(c);
            }
        }

        if digest_name.is_none() {
            digest_name = conf_get_string(&req_config, sec, "default_md");
        }
        let mut digest = match &digest_name {
            Some(n) if n == b"null" => ffi::md_null(),
            Some(n) => ffi::md_by_name(n),
            None => None,
        };
        if digest.is_none() {
            digest = ffi::md_by_name(b"sha1");
            self.store_errors();
        }

        if let Some(s) = &extensions_section {
            if !self.config_check_syntax("extensions_section", &config_filename, s, &req_config) {
                return Ok(None);
            }
        }

        let mut curve_name = 0;
        if let Some(c) = &a.curve_name {
            curve_name = ffi::sn2nid(c);
            if curve_name == 0 {
                self.warn(format!("Unknown elliptic curve (short) name {}", lossy(c)));
                return Ok(None);
            }
        }

        if let Some(mask) = conf_get_string(&req_config, sec, "string_mask") {
            if !ffi::set_default_string_mask(&mask) {
                self.warn(format!(
                    "Invalid global string mask setting {}",
                    lossy(&mask)
                ));
                return Ok(None);
            }
        }

        if let Some(s) = &request_extensions_section {
            if !self.config_check_syntax(
                "request_extensions_section",
                &config_filename,
                s,
                &req_config,
            ) {
                return Ok(None);
            }
        }

        Ok(Some(ReqConfig {
            global_config,
            req_config,
            config_filename,
            section_name,
            digest,
            extensions_section,
            request_extensions_section,
            priv_key_bits: i64::from(priv_key_bits),
            priv_key_type,
            priv_key_encrypt,
            priv_key_encrypt_cipher,
            curve_name,
        }))
    }

    /// `php_openssl_config_check_syntax`.
    fn config_check_syntax(
        &mut self,
        label: &str,
        file: &[u8],
        section: &[u8],
        conf: &Conf,
    ) -> bool {
        if conf.add_extensions(section, &ExtTarget::Test) {
            return true;
        }
        self.store_errors();
        self.warn(format!(
            "Error loading {label} section {} of {}",
            lossy(section),
            lossy(file)
        ));
        false
    }

    /// `php_openssl_add_oid_section`.
    fn add_oid_section(&mut self, conf: &Conf) -> bool {
        let Some(sect) = conf_get_string(conf, None, "oid_section") else {
            return true;
        };
        let Some(values) = conf.section(&sect) else {
            self.store_errors();
            self.warn(format!("Problem loading oid section {}", lossy(&sect)));
            return false;
        };
        for v in values {
            if ffi::sn2nid(&v.name) == 0
                && ffi::ln2nid(&v.name) == 0
                && ffi::obj_create(&v.value, &v.name) == 0
            {
                self.store_errors();
                self.warn(format!(
                    "Problem creating object {}={}",
                    lossy(&v.name),
                    lossy(&v.value)
                ));
                return false;
            }
        }
        true
    }

    /// `php_openssl_generate_private_key`.
    pub(crate) fn generate_private_key(&mut self, req: &ReqConfig) -> Option<Pkey> {
        let t = req.priv_key_type;
        if (t == OPENSSL_KEYTYPE_RSA || t == OPENSSL_KEYTYPE_DH || t == OPENSSL_KEYTYPE_DSA)
            && req.priv_key_bits < MIN_KEY_LENGTH
        {
            self.warn(format!(
                "Private key length must be at least {MIN_KEY_LENGTH} bits, configured to {}",
                req.priv_key_bits
            ));
            return None;
        }
        let (id, name) = match t {
            OPENSSL_KEYTYPE_RSA => (pkey_id::RSA, "RSA"),
            OPENSSL_KEYTYPE_DSA => (pkey_id::DSA, "DSA"),
            OPENSSL_KEYTYPE_DH => (pkey_id::DH, "DH"),
            OPENSSL_KEYTYPE_EC => (pkey_id::EC, "EC"),
            OPENSSL_KEYTYPE_X25519 => (pkey_id::X25519, "X25519"),
            OPENSSL_KEYTYPE_ED25519 => (pkey_id::ED25519, "ED25519"),
            OPENSSL_KEYTYPE_X448 => (pkey_id::X448, "X448"),
            OPENSSL_KEYTYPE_ED448 => (pkey_id::ED448, "ED448"),
            _ => {
                self.warn("Unsupported private key type");
                return None;
            }
        };
        let randfile = conf_get_string(&req.req_config, Some(&req.section_name), "RANDFILE");
        let seeded = self.load_rand_file(randfile.as_deref());
        let key = self.generate_key_inner(req, id, name);
        if seeded {
            self.write_rand_file(randfile.as_deref());
        }
        key
    }

    #[allow(clippy::cast_possible_truncation)]
    fn generate_key_inner(&mut self, req: &ReqConfig, id: i32, name: &str) -> Option<Pkey> {
        let bits = req.priv_key_bits as i32;
        let Some(mut ctx) = PkeyCtx::from_name(name) else {
            self.store_errors();
            return None;
        };
        if id != pkey_id::RSA {
            if !ctx.paramgen_init() {
                self.store_errors();
                return None;
            }
            let ok = match id {
                pkey_id::DSA => ctx.set_dsa_paramgen_bits(bits),
                pkey_id::DH => ctx.set_dh_paramgen_prime_len(bits),
                pkey_id::EC => {
                    if req.curve_name == 0 {
                        self.warn("Missing configuration value: \"curve_name\" not set");
                        return None;
                    }
                    ctx.set_ec_curve(req.curve_name)
                }
                _ => true,
            };
            if !ok {
                self.store_errors();
                return None;
            }
            let Some(params) = ctx.paramgen() else {
                self.store_errors();
                return None;
            };
            let Some(c) = PkeyCtx::from_pkey(&params) else {
                self.store_errors();
                return None;
            };
            ctx = c;
        }
        if !ctx.keygen_init() {
            self.store_errors();
            return None;
        }
        if id == pkey_id::RSA && !ctx.set_rsa_keygen_bits(bits) {
            self.store_errors();
            return None;
        }
        let key = ctx.keygen();
        if key.is_none() {
            self.store_errors();
        }
        key
    }

    /// `php_openssl_load_rand_file`; returns `seeded`.
    fn load_rand_file(&mut self, file: Option<&[u8]>) -> bool {
        let Some(file) = file.map(<[u8]>::to_vec).or_else(ffi::rand_file_name) else {
            return false;
        };
        if ffi::rand_load_file(&file) < 0 {
            if ffi::rand_status() == 0 {
                self.store_errors();
                self.warn("Unable to load random state; not enough random data!");
            }
            return false;
        }
        true
    }

    /// `php_openssl_write_rand_file` (only called when seeded).
    fn write_rand_file(&mut self, file: Option<&[u8]>) {
        let file = file.map(<[u8]>::to_vec).or_else(ffi::rand_file_name);
        if file.as_deref().is_none_or(|f| ffi::rand_write_file(f) < 0) {
            self.store_errors();
            self.warn("Unable to write random state");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_typing() {
        let mut a = PhpArray::new();
        a.set("config", PhpValue::Int(1));
        a.set("digest_alg", PhpValue::str("sha256"));
        a.set("private_key_bits", PhpValue::str("2048"));
        a.set("encrypt_key", PhpValue::Int(1));
        let c = ConfigArgs::from_array(&a);
        assert_eq!(c.config, None);
        assert_eq!(c.digest_alg.as_deref(), Some(&b"sha256"[..]));
        assert_eq!(c.private_key_bits, None);
        assert_eq!(c.encrypt_key, Some(false));
    }

    #[test]
    fn algo_tables() {
        assert!(md_from_algo(7).is_some());
        assert!(md_from_algo(5).is_none());
        assert!(md_from_algo(-1).is_none());
        assert!(cipher_from_algo(5).is_some());
        assert!(cipher_from_algo(8).is_none());
    }

    #[test]
    fn parse_default_config() {
        let mut o = OpenSsl::new();
        let r = o.parse_config(None).unwrap().unwrap();
        assert_eq!(r.section_name, b"req");
        assert!(r.digest.is_some());
        assert_eq!(r.priv_key_type, OPENSSL_KEYTYPE_RSA);
    }

    #[test]
    fn missing_config_file_fails() {
        let mut o = OpenSsl::new();
        let args = ConfigArgs {
            config: Some(b"/nonexist".to_vec()),
            ..ConfigArgs::default()
        };
        assert!(o.parse_config(Some(&args)).unwrap().is_none());
        assert_eq!(
            o.error_string().as_deref(),
            Some("error:FFFFFFFF80000002:system library::No such file or directory")
        );
    }
}
