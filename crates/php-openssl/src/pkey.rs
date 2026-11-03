//! Asymmetric keys: `openssl_pkey_*`, `openssl_dh_compute_key`,
//! `openssl_pkey_derive`.

use std::ffi::CStr;

use crate::args::{AsymmetricKey, KeyArg};
use crate::config::ConfigArgs;
use crate::constants::{
    OPENSSL_KEYTYPE_DH, OPENSSL_KEYTYPE_DSA, OPENSSL_KEYTYPE_EC, OPENSSL_KEYTYPE_ED25519,
    OPENSSL_KEYTYPE_ED448, OPENSSL_KEYTYPE_RSA, OPENSSL_KEYTYPE_X25519, OPENSSL_KEYTYPE_X448,
};
use crate::error::{PhpResult, Throw};
use crate::ffi::{self, pkey_id, Bio, Bn, BnCtx, EcGroup, ParamBld, Pkey, PkeyCtx};
use crate::value::{PhpArray, PhpValue};
use crate::OpenSsl;

/// `OPENSSL_PKEY_SET_BN`: a BIGNUM from a string entry.
fn bn(data: &PhpArray, name: &str) -> Option<Bn> {
    data.get_str(name)
        .and_then(PhpValue::as_str)
        .and_then(Bn::from_bin)
}

/// A non-empty string entry.
fn nonempty_str<'a>(data: &'a PhpArray, name: &str) -> Option<&'a [u8]> {
    data.get_str(name)
        .and_then(PhpValue::as_str)
        .filter(|s| !s.is_empty())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl OpenSsl {
    /// `openssl_pkey_new()`. `Ok(None)` = `false`.
    pub fn pkey_new(&mut self, options: Option<&PhpArray>) -> PhpResult<Option<AsymmetricKey>> {
        if let Some(opts) = options {
            let sub = |k: &str| opts.get_str(k).and_then(PhpValue::as_array);
            if let Some(d) = sub("rsa") {
                return Ok(self.pkey_init_rsa(d).map(|pkey| AsymmetricKey {
                    pkey,
                    is_private: true,
                }));
            } else if let Some(d) = sub("dsa") {
                return Ok(self.pkey_init_ffc(d, false));
            } else if let Some(d) = sub("dh") {
                return Ok(self.pkey_init_ffc(d, true));
            } else if let Some(d) = sub("ec") {
                return Ok(self.pkey_init_ec(d));
            } else if let Some(d) = sub("x25519") {
                return Ok(self.pkey_init_curve_25519_448("X25519", d));
            } else if let Some(d) = sub("ed25519") {
                return Ok(self.pkey_init_curve_25519_448("ED25519", d));
            } else if let Some(d) = sub("x448") {
                return Ok(self.pkey_init_curve_25519_448("X448", d));
            } else if let Some(d) = sub("ed448") {
                return Ok(self.pkey_init_curve_25519_448("ED448", d));
            }
        }
        let args = options.map(ConfigArgs::from_array);
        let Some(req) = self.parse_config(args.as_ref())? else {
            return Ok(None);
        };
        Ok(self.generate_private_key(&req).map(|pkey| AsymmetricKey {
            pkey,
            is_private: true,
        }))
    }

    /// `php_openssl_pkey_init_rsa`.
    fn pkey_init_rsa(&mut self, data: &PhpArray) -> Option<Pkey> {
        let key = (|| {
            let ctx = PkeyCtx::from_name("RSA");
            let mut bld = ParamBld::new();
            let n = bn(data, "n");
            let e = bn(data, "e");
            let d = bn(data, "d");
            let p = bn(data, "p");
            let q = bn(data, "q");
            let dmp1 = bn(data, "dmp1");
            let dmq1 = bn(data, "dmq1");
            let iqmp = bn(data, "iqmp");
            let (Some(ctx), Some(bld), Some(n), Some(d)) = (ctx, bld.as_mut(), n, d) else {
                return None;
            };
            bld.push_bn(c"n", n);
            bld.push_bn(c"d", d);
            for (name, v) in [
                (c"e", e),
                (c"rsa-factor1", p),
                (c"rsa-factor2", q),
                (c"rsa-exponent1", dmp1),
                (c"rsa-exponent2", dmq1),
                (c"rsa-coefficient1", iqmp),
            ] {
                if let Some(v) = v {
                    bld.push_bn(name, v);
                }
            }
            let params = bld.to_param()?;
            ctx.fromdata(&params)
        })();
        self.store_errors();
        key
    }

    /// `php_openssl_pkey_init_dsa` / `php_openssl_pkey_init_dh`.
    fn pkey_init_ffc(&mut self, data: &PhpArray, is_dh: bool) -> Option<AsymmetricKey> {
        let key = (|| {
            let ctx = PkeyCtx::from_name(if is_dh { "DH" } else { "DSA" });
            let mut bld = ParamBld::new();
            let p = bn(data, "p");
            let q = bn(data, "q");
            let g = bn(data, "g");
            let priv_key = bn(data, "priv_key");
            let mut pub_key = bn(data, "pub_key");
            let (Some(ctx), Some(bld), Some(p), Some(g)) = (ctx, bld.as_mut(), p, g) else {
                return None;
            };
            if !is_dh && q.is_none() {
                return None;
            }
            let has_priv = priv_key.is_some();
            let mut has_pub = pub_key.is_some();
            if is_dh {
                if let Some(priv_key) = &priv_key {
                    if pub_key.is_none() {
                        pub_key = Some(ffi::dh_pub_from_priv(priv_key, &g, &p)?);
                    }
                }
                bld.push_bn(c"p", p);
                bld.push_bn(c"g", g);
                if let Some(q) = q {
                    bld.push_bn(c"q", q);
                }
                if let Some(k) = priv_key {
                    bld.push_bn(c"priv", k);
                }
                if let Some(k) = pub_key {
                    bld.push_bn(c"pub", k);
                    has_pub = true;
                }
            } else {
                bld.push_bn(c"p", p);
                bld.push_bn(c"q", q?);
                bld.push_bn(c"g", g);
                // PHP silently ignores priv_key without pub_key for DSA.
                if let Some(k) = pub_key {
                    bld.push_bn(c"pub", k);
                    if let Some(k) = priv_key {
                        bld.push_bn(c"priv", k);
                    }
                }
            }
            let params = bld.to_param()?;
            let param_key = ctx.fromdata(&params)?;
            let explicit = if is_dh { has_pub || has_priv } else { has_pub };
            if explicit {
                Some(AsymmetricKey {
                    pkey: param_key,
                    is_private: has_priv,
                })
            } else {
                let kctx = PkeyCtx::from_pkey(&param_key)?;
                if !kctx.keygen_init() {
                    return None;
                }
                kctx.keygen().map(|pkey| AsymmetricKey {
                    pkey,
                    is_private: true,
                })
            }
        })();
        self.store_errors();
        key
    }

    /// `php_openssl_pkey_init_ec`.
    fn pkey_init_ec(&mut self, data: &PhpArray) -> Option<AsymmetricKey> {
        let r = self.pkey_init_ec_inner(data);
        self.store_errors();
        r
    }

    fn pkey_init_ec_inner(&mut self, data: &PhpArray) -> Option<AsymmetricKey> {
        let ctx = PkeyCtx::from_name("EC");
        let bctx = BnCtx::new();
        let mut bld = ParamBld::new();
        let (Some(mut ctx), Some(bctx), Some(bld)) = (ctx, bctx, bld.as_mut()) else {
            return None;
        };
        let nid;
        let group;
        if let Some(curve) = nonempty_str(data, "curve_name") {
            nid = ffi::sn2nid(curve);
            if nid == 0 {
                self.warn(format!(
                    "Unknown elliptic curve (short) name {}",
                    lossy(curve)
                ));
                return None;
            }
            group = EcGroup::by_curve_name(nid)?;
            if !bld.push_utf8(c"group", curve) {
                return None;
            }
        } else {
            let p = bn(data, "p");
            let a = bn(data, "a");
            let b = bn(data, "b");
            let order = bn(data, "order");
            let (Some(p), Some(a), Some(b), Some(order)) = (p, a, b, order) else {
                let none = ["p", "a", "b", "order"]
                    .iter()
                    .all(|k| bn(data, k).is_none());
                self.warn(if none {
                    "Missing params: curve_name"
                } else {
                    "Missing params: curve_name or p, a, b, order"
                });
                return None;
            };
            group = EcGroup::curve_gfp(&p, &a, &b, &bctx)?;
            let order_copy = Bn::from_bin(&order.to_vec())?;
            if !bld.push_bn(c"p", p)
                || !bld.push_bn(c"a", a)
                || !bld.push_bn(c"b", b)
                || !bld.push_bn(c"order", order)
                || !bld.push_utf8(c"field-type", b"prime-field")
            {
                return None;
            }
            let point_g = group.new_point()?;
            if let Some(gen) = nonempty_str(data, "generator") {
                if !group.oct2point(&point_g, gen, &bctx) || !bld.push_octet(c"generator", gen) {
                    return None;
                }
            } else {
                let (Some(gx), Some(gy)) = (bn(data, "g_x"), bn(data, "g_y")) else {
                    self.warn("Missing params: generator or g_x and g_y");
                    return None;
                };
                if !group.set_affine(&point_g, &gx, &gy, &bctx) {
                    return None;
                }
                let buf = group.point2buf_compressed(&point_g, &bctx)?;
                if !bld.push_octet(c"generator", &buf) {
                    return None;
                }
            }
            if let Some(seed) = nonempty_str(data, "seed") {
                if !group.set_seed(seed) || !bld.push_octet(c"seed", seed) {
                    return None;
                }
            }
            let cofactor = bn(data, "cofactor");
            let cof_copy = cofactor.as_ref().and_then(|c| Bn::from_bin(&c.to_vec()));
            if let Some(c) = cofactor {
                if !bld.push_bn(c"cofactor", c) {
                    return None;
                }
            }
            if !group.set_generator(&point_g, &order_copy, cof_copy.as_ref()) {
                return None;
            }
            nid = group.check_named_curve(&bctx);
        }

        let mut d_present = false;
        let mut xy_present = false;
        if nid != ffi::NID_SM2 {
            let d = bn(data, "d");
            let x = bn(data, "x");
            let y = bn(data, "y");
            let mut point_q = None;
            if let Some(d) = d {
                d_present = true;
                let q = group.new_point()?;
                if !group.mul_generator(&q, &d, &bctx) || !bld.push_bn(c"priv", d) {
                    return None;
                }
                point_q = Some(q);
            } else if let (Some(x), Some(y)) = (&x, &y) {
                xy_present = true;
                let q = group.new_point()?;
                if !group.set_affine(&q, x, y, &bctx) {
                    return None;
                }
                point_q = Some(q);
            }
            if let Some(q) = point_q {
                let buf = group.point2buf_compressed(&q, &bctx)?;
                if !bld.push_octet(c"pub", &buf) {
                    return None;
                }
            }
        }
        let params = bld.to_param()?;
        let mut param_key = None;
        if d_present || xy_present {
            let k = ctx.fromdata(&params)?;
            ctx = PkeyCtx::new(&k)?;
            param_key = Some(k);
        }
        if ctx.check_or_public_check_quick() {
            // PHP up-refs `param_key` (NULL when no key data was given).
            return param_key.map(|pkey| AsymmetricKey {
                pkey,
                is_private: d_present,
            });
        }
        if !ctx.keygen_init() || !ctx.set_params(&params) {
            return None;
        }
        ctx.generate().map(|pkey| AsymmetricKey {
            pkey,
            is_private: true,
        })
    }

    /// `php_openssl_pkey_object_curve_25519_448`.
    fn pkey_init_curve_25519_448(&mut self, name: &str, data: &PhpArray) -> Option<AsymmetricKey> {
        let r = (|| {
            let mut bld = ParamBld::new()?;
            if let Some(k) = nonempty_str(data, "priv_key") {
                if !bld.push_octet(c"priv", k) {
                    return None;
                }
            }
            if let Some(k) = nonempty_str(data, "pub_key") {
                if !bld.push_octet(c"pub", k) {
                    return None;
                }
            }
            let params = bld.to_param();
            let ctx = PkeyCtx::from_name(name);
            let (Some(params), Some(ctx)) = (params, ctx) else {
                return None;
            };
            let has_priv = data.get_str("priv_key").is_some();
            let has_pub = data.get_str("pub_key").is_some();
            if has_priv || has_pub {
                ctx.fromdata(&params).map(|pkey| AsymmetricKey {
                    pkey,
                    is_private: has_priv,
                })
            } else {
                if !ctx.keygen_init() {
                    return None;
                }
                ctx.keygen().map(|pkey| AsymmetricKey {
                    pkey,
                    is_private: true,
                })
            }
        })();
        self.store_errors();
        r
    }

    /// Shared body of `openssl_pkey_export` / `_to_file`: the PEM text.
    fn pkey_export_pem(
        &mut self,
        key: &KeyArg<'_>,
        passphrase: Option<&[u8]>,
        options: Option<&PhpArray>,
        out_path: Option<&[u8]>,
    ) -> PhpResult<Option<Vec<u8>>> {
        let Some(pkey) = self.pkey_from_arg(key, false, passphrase, 1)? else {
            self.warn("Cannot get key from parameter 1");
            return Ok(None);
        };
        let file_path = match out_path {
            Some(p) => match self.check_plain_path(p, 2)? {
                Some(fp) => Some(fp),
                None => return Ok(None),
            },
            None => None,
        };
        let args = options.map(ConfigArgs::from_array);
        let Some(req) = self.parse_config(args.as_ref())? else {
            return Ok(None);
        };
        let bio = match &file_path {
            Some(fp) => Bio::file(fp, "w"),
            None => Bio::mem(),
        };
        let Some(bio) = bio else {
            self.store_errors();
            return Ok(None);
        };
        let default_cipher;
        let cipher = if passphrase.is_some() && req.priv_key_encrypt {
            if let Some(c) = &req.priv_key_encrypt_cipher {
                Some(c)
            } else {
                default_cipher = ffi::cipher_by_name(b"DES-EDE3-CBC");
                default_cipher.as_ref()
            }
        } else {
            None
        };
        if pkey.write_private(&bio, cipher, passphrase) {
            Ok(Some(bio.contents()))
        } else {
            self.store_errors();
            Ok(None)
        }
    }

    /// `openssl_pkey_export()`: the PEM to assign to `$output`, or `None`
    /// for `false`.
    pub fn pkey_export(
        &mut self,
        key: &KeyArg<'_>,
        passphrase: Option<&[u8]>,
        options: Option<&PhpArray>,
    ) -> PhpResult<Option<Vec<u8>>> {
        self.pkey_export_pem(key, passphrase, options, None)
    }

    /// `openssl_pkey_export_to_file()`.
    pub fn pkey_export_to_file(
        &mut self,
        key: &KeyArg<'_>,
        output_filename: &[u8],
        passphrase: Option<&[u8]>,
        options: Option<&PhpArray>,
    ) -> PhpResult<bool> {
        Ok(self
            .pkey_export_pem(key, passphrase, options, Some(output_filename))?
            .is_some())
    }

    /// `openssl_pkey_get_public()` / `openssl_get_publickey()`.
    pub fn pkey_get_public(&mut self, key: &KeyArg<'_>) -> PhpResult<Option<AsymmetricKey>> {
        Ok(self
            .pkey_from_arg(key, true, None, 1)?
            .map(|pkey| AsymmetricKey {
                pkey,
                is_private: false,
            }))
    }

    /// `openssl_pkey_get_private()` / `openssl_get_privatekey()`.
    /// `passphrase` defaults to `Some(b"")` in PHP; `None` is `null`.
    pub fn pkey_get_private(
        &mut self,
        key: &KeyArg<'_>,
        passphrase: Option<&[u8]>,
    ) -> PhpResult<Option<AsymmetricKey>> {
        Ok(self
            .pkey_from_arg(key, false, passphrase, 1)?
            .map(|pkey| AsymmetricKey {
                pkey,
                is_private: true,
            }))
    }

    /// `openssl_pkey_get_details()`.
    pub fn pkey_get_details(&mut self, key: &AsymmetricKey) -> Option<PhpArray> {
        let pkey = &key.pkey;
        let Some(pem) = pkey.public_pem() else {
            self.store_errors();
            return None;
        };
        let mut out = PhpArray::new();
        out.set("bits", PhpValue::Int(i64::from(pkey.bits())));
        out.set("key", PhpValue::Str(pem));
        let bn_params = |names: &[(&CStr, &str)]| {
            let mut a = PhpArray::new();
            for (param, name) in names {
                if let Some(v) = pkey.bn_param(param) {
                    a.set(*name, PhpValue::Str(v));
                }
            }
            a
        };
        let curve = |a: &mut PhpArray| {
            if let Some(v) = pkey.octet_param(c"priv") {
                a.set("priv_key", PhpValue::Str(v));
            }
            if let Some(v) = pkey.octet_param(c"pub") {
                a.set("pub_key", PhpValue::Str(v));
            }
        };
        let ktype = match pkey.details_base_id() {
            pkey_id::RSA => {
                let a = bn_params(&[
                    (c"n", "n"),
                    (c"e", "e"),
                    (c"d", "d"),
                    (c"rsa-factor1", "p"),
                    (c"rsa-factor2", "q"),
                    (c"rsa-exponent1", "dmp1"),
                    (c"rsa-exponent2", "dmq1"),
                    (c"rsa-coefficient1", "iqmp"),
                ]);
                out.set("rsa", PhpValue::Array(a));
                OPENSSL_KEYTYPE_RSA
            }
            pkey_id::DSA => {
                let a = bn_params(&[
                    (c"p", "p"),
                    (c"q", "q"),
                    (c"g", "g"),
                    (c"priv", "priv_key"),
                    (c"pub", "pub_key"),
                ]);
                out.set("dsa", PhpValue::Array(a));
                OPENSSL_KEYTYPE_DSA
            }
            pkey_id::DH => {
                let a = bn_params(&[
                    (c"p", "p"),
                    (c"g", "g"),
                    (c"priv", "priv_key"),
                    (c"pub", "pub_key"),
                ]);
                out.set("dh", PhpValue::Array(a));
                OPENSSL_KEYTYPE_DH
            }
            pkey_id::EC => {
                let mut a = PhpArray::new();
                if let Some(name) = pkey.utf8_param(c"group") {
                    let nid = ffi::sn2nid(&name);
                    a.set("curve_name", PhpValue::Str(name));
                    if nid != 0 {
                        if let Some(oid) = ffi::nid2oid(nid) {
                            a.set("curve_oid", PhpValue::str(oid));
                        }
                    }
                }
                for (param, name) in [(c"qx", "x"), (c"qy", "y"), (c"priv", "d")] {
                    if let Some(v) = pkey.bn_param(param) {
                        a.set(name, PhpValue::Str(v));
                    }
                }
                out.set("ec", PhpValue::Array(a));
                OPENSSL_KEYTYPE_EC
            }
            id @ (pkey_id::X25519 | pkey_id::ED25519 | pkey_id::X448 | pkey_id::ED448) => {
                let (name, t) = match id {
                    pkey_id::X25519 => ("x25519", OPENSSL_KEYTYPE_X25519),
                    pkey_id::ED25519 => ("ed25519", OPENSSL_KEYTYPE_ED25519),
                    pkey_id::X448 => ("x448", OPENSSL_KEYTYPE_X448),
                    _ => ("ed448", OPENSSL_KEYTYPE_ED448),
                };
                let mut a = PhpArray::new();
                curve(&mut a);
                out.set(name, PhpValue::Array(a));
                t
            }
            _ => -1,
        };
        out.set("type", PhpValue::Int(ktype));
        Some(out)
    }

    /// `php_openssl_pkey_derive`.
    fn derive_shared(&mut self, key: &Pkey, peer: &Pkey, requested: usize) -> Option<Vec<u8>> {
        let ctx = PkeyCtx::new(key)?;
        let mut size = requested;
        if !ctx.derive_init(peer) {
            self.store_errors();
            return None;
        }
        if size == 0 {
            let Some(s) = ctx.derive_size() else {
                self.store_errors();
                return None;
            };
            size = s;
        }
        let r = ctx.derive(size);
        if r.is_none() {
            self.store_errors();
        }
        r
    }

    /// `openssl_dh_compute_key()`.
    pub fn dh_compute_key(
        &mut self,
        public_key: &[u8],
        private_key: &AsymmetricKey,
    ) -> Option<Vec<u8>> {
        let pkey = &private_key.pkey;
        if pkey.base_id() != pkey_id::DH {
            return None;
        }
        let peer = Pkey::empty();
        let Some(peer) =
            peer.filter(|p| p.copy_parameters(pkey) && p.set_encoded_public_key(public_key))
        else {
            self.store_errors();
            return None;
        };
        self.derive_shared(pkey, &peer, 0)
    }

    /// `openssl_pkey_derive()`; `key_length` is `Some` when the deprecated
    /// third argument was passed.
    pub fn pkey_derive(
        &mut self,
        public_key: &KeyArg<'_>,
        private_key: &KeyArg<'_>,
        key_length: Option<i64>,
    ) -> PhpResult<Option<Vec<u8>>> {
        if key_length.is_some() {
            self.deprecated("the $key_length parameter is deprecated as it is either ignored or truncates the key");
        }
        let key_len = key_length.unwrap_or(0);
        if key_len < 0 {
            return Err(Throw::ArgumentValueError {
                arg: 3,
                message: "must be greater than or equal to 0".into(),
            });
        }
        let Some(pkey) = self.pkey_from_arg(private_key, false, Some(b""), 2)? else {
            return Ok(None);
        };
        let Some(peer) = self.pkey_from_arg(public_key, true, None, 1)? else {
            return Ok(None);
        };
        Ok(self.derive_shared(&pkey, &peer, usize::try_from(key_len).unwrap_or(0)))
    }
}
