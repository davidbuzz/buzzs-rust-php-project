// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `openssl_pkcs7_*`, `openssl_cms_*` and `openssl_pkcs12_*`.

use php_openssl::cms::CipherAlgo;
use php_openssl::constants::{OPENSSL_CIPHER_AES_128_CBC, OPENSSL_ENCODING_SMIME, PKCS7_DETACHED};
use php_openssl::pkcs12::Pkcs12Options;
use php_openssl::pkcs7::{Header, Recipients};
use php_openssl::BoolOrInt;
use php_runtime::{ArrayKey, Value, ZArray};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::convert::{
    bytes, cert_hold, from_php_array, int_or, key_hold, opt_array, opt_bytes, string_list,
    string_values, to_bytes,
};
use crate::objects::{CertHold, KeyHold};
use crate::state::{check_paths, run, run_cert, run_ok};

/// `?array $headers`: `"name: value"` / `"value"` lines.
fn headers(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<Vec<Header>>> {
    let Some(arr) = opt_array(a, n) else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(arr.len());
    for (k, v) in arr.iter() {
        let name = match k {
            ArrayKey::Str(s) => Some(s.as_bytes().to_vec()),
            ArrayKey::Int(_) => None,
        };
        let value = to_bytes(vm, &v.deref())?;
        out.push(Header { name, value });
    }
    Ok(Some(out))
}

/// Owned recipients (`OpenSSLCertificate|array|string $certificate`).
enum RecipientsHold {
    Many(Vec<CertHold>),
    One(CertHold),
}

impl RecipientsHold {
    fn arg(&self) -> Recipients<'_> {
        match self {
            RecipientsHold::Many(v) => Recipients::Many(v.iter().map(CertHold::arg).collect()),
            RecipientsHold::One(c) => Recipients::One(c.arg()),
        }
    }
}

fn recipients(vm: &mut Vm, v: &Value) -> VmResult<RecipientsHold> {
    if let Value::Array(arr) = v {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr.values() {
            out.push(cert_hold(vm, &item.deref())?);
        }
        Ok(RecipientsHold::Many(out))
    } else {
        Ok(RecipientsHold::One(cert_hold(vm, v)?))
    }
}

/// An optional untyped private key (`$private_key = null`).
fn opt_key(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<KeyHold>> {
    match a.opt(n) {
        Some(v) => Ok(Some(key_hold(vm, &v)?)),
        None => Ok(None),
    }
}

fn bool_or_int(v: BoolOrInt) -> Value {
    match v {
        BoolOrInt::Bool(b) => Value::Bool(b),
        BoolOrInt::Int(i) => Value::Int(i),
    }
}

/// Assigns a list of PEM strings to `$certificates`.
fn set_cert_list(a: &Args, n: usize, list: Option<Vec<Vec<u8>>>) -> Value {
    match list {
        Some(certs) => {
            a.set_ref(n, string_list(certs));
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn pkcs7_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 2, 4, 5, 6])?;
    let input = a.str(0);
    let flags = a.int(1);
    let signers = opt_bytes(a, 2);
    let ca = string_values(vm, opt_array(a, 3).as_ref())?;
    let untrusted = opt_bytes(a, 4);
    let content = opt_bytes(a, 5);
    let output = opt_bytes(a, 6);
    let r = run(vm, a, |s| {
        s.pkcs7_verify(
            input.as_bytes(),
            flags,
            signers.as_deref(),
            &ca,
            untrusted.as_deref(),
            content.as_deref(),
            output.as_deref(),
        )
    })?;
    Ok(bool_or_int(r))
}

fn pkcs7_encrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1])?;
    let (input, output) = (a.str(0), a.str(1));
    let recips = recipients(vm, &a.at(2))?;
    let hdrs = headers(vm, a, 3)?;
    let flags = a.int(4);
    let cipher = int_or(a, 5, OPENSSL_CIPHER_AES_128_CBC);
    let ok = run(vm, a, |s| {
        s.pkcs7_encrypt(
            input.as_bytes(),
            output.as_bytes(),
            &recips.arg(),
            hdrs.as_deref(),
            flags,
            cipher,
        )
    })?;
    Ok(Value::Bool(ok))
}

fn pkcs7_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let r = run_ok(vm, a, |s| s.pkcs7_read(data.as_bytes()))?;
    Ok(set_cert_list(a, 1, r))
}

fn pkcs7_sign(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1, 6])?;
    let (input, output) = (a.str(0), a.str(1));
    let cert = cert_hold(vm, &a.at(2))?;
    let key = key_hold(vm, &a.at(3))?;
    let hdrs = headers(vm, a, 4)?;
    let flags = int_or(a, 5, PKCS7_DETACHED);
    let untrusted = opt_bytes(a, 6);
    let ok = run_cert(vm, a, 3, |s| {
        s.pkcs7_sign(
            input.as_bytes(),
            output.as_bytes(),
            cert.arg(),
            &key.arg(),
            hdrs.as_deref(),
            flags,
            untrusted.as_deref(),
        )
    })?;
    Ok(Value::Bool(ok))
}

fn pkcs7_decrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1])?;
    let (input, output) = (a.str(0), a.str(1));
    let cert = cert_hold(vm, &a.at(2))?;
    let key = opt_key(vm, a, 3)?;
    let ok = run_cert(vm, a, 3, |s| {
        let k = key.as_ref().map(KeyHold::arg);
        s.pkcs7_decrypt(input.as_bytes(), output.as_bytes(), cert.arg(), k.as_ref())
    })?;
    Ok(Value::Bool(ok))
}

fn cms_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 2, 4, 5, 6, 7])?;
    let input = a.str(0);
    let flags = a.int(1);
    let certs = opt_bytes(a, 2);
    let ca = string_values(vm, opt_array(a, 3).as_ref())?;
    let untrusted = opt_bytes(a, 4);
    let content = opt_bytes(a, 5);
    let pk7 = opt_bytes(a, 6);
    let sigfile = opt_bytes(a, 7);
    let encoding = int_or(a, 8, OPENSSL_ENCODING_SMIME);
    let ok = run(vm, a, |s| {
        s.cms_verify(
            input.as_bytes(),
            flags,
            certs.as_deref(),
            &ca,
            untrusted.as_deref(),
            content.as_deref(),
            pk7.as_deref(),
            sigfile.as_deref(),
            encoding,
        )
    })?;
    Ok(Value::Bool(ok))
}

fn cms_encrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1])?;
    let (input, output) = (a.str(0), a.str(1));
    let recips = recipients(vm, &a.at(2))?;
    let hdrs = headers(vm, a, 3)?;
    let flags = a.int(4);
    let encoding = int_or(a, 5, OPENSSL_ENCODING_SMIME);
    let (name, id) = match a.get(6) {
        Some(Value::Str(s)) => (Some(s.as_bytes().to_vec()), 0),
        Some(Value::Int(i)) => (None, i),
        _ => (None, OPENSSL_CIPHER_AES_128_CBC),
    };
    let ok = run(vm, a, |s| {
        let cipher = name.as_deref().map_or(CipherAlgo::Id(id), CipherAlgo::Name);
        s.cms_encrypt(
            input.as_bytes(),
            output.as_bytes(),
            &recips.arg(),
            hdrs.as_deref(),
            flags,
            encoding,
            cipher,
        )
    })?;
    Ok(Value::Bool(ok))
}

fn cms_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let r = run_ok(vm, a, |s| s.cms_read(data.as_bytes()))?;
    Ok(set_cert_list(a, 1, r))
}

fn cms_sign(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1, 7])?;
    let (input, output) = (a.str(0), a.str(1));
    let cert = cert_hold(vm, &a.at(2))?;
    let key = key_hold(vm, &a.at(3))?;
    let hdrs = headers(vm, a, 4)?;
    let flags = a.int(5);
    let encoding = int_or(a, 6, OPENSSL_ENCODING_SMIME);
    let untrusted = opt_bytes(a, 7);
    let ok = run_cert(vm, a, 3, |s| {
        s.cms_sign(
            input.as_bytes(),
            output.as_bytes(),
            cert.arg(),
            &key.arg(),
            hdrs.as_deref(),
            flags,
            encoding,
            untrusted.as_deref(),
        )
    })?;
    Ok(Value::Bool(ok))
}

fn cms_decrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1])?;
    let (input, output) = (a.str(0), a.str(1));
    let cert = cert_hold(vm, &a.at(2))?;
    let key = opt_key(vm, a, 3)?;
    let encoding = int_or(a, 4, OPENSSL_ENCODING_SMIME);
    let ok = run_cert(vm, a, 3, |s| {
        let k = key.as_ref().map(KeyHold::arg);
        s.cms_decrypt(
            input.as_bytes(),
            output.as_bytes(),
            cert.arg(),
            k.as_ref(),
            encoding,
        )
    })?;
    Ok(Value::Bool(ok))
}

/// Owned `$options` of the PKCS#12 export functions.
struct Pkcs12Hold {
    friendly_name: Option<Vec<u8>>,
    extracerts: Option<(Vec<CertHold>, bool)>,
}

impl Pkcs12Hold {
    fn arg(&self) -> Pkcs12Options<'_> {
        Pkcs12Options {
            friendly_name: self.friendly_name.as_deref(),
            extracerts: self
                .extracerts
                .as_ref()
                .map(|(v, is_array)| (v.iter().map(CertHold::arg).collect(), *is_array)),
        }
    }
}

fn pkcs12_options(vm: &mut Vm, opts: Option<&ZArray>) -> VmResult<Pkcs12Hold> {
    let mut out = Pkcs12Hold {
        friendly_name: None,
        extracerts: None,
    };
    let Some(opts) = opts else {
        return Ok(out);
    };
    if let Some(Value::Str(s)) = opts.get_str(b"friendly_name").map(Value::deref) {
        out.friendly_name = Some(s.as_bytes().to_vec());
    }
    if let Some(v) = opts.get_str(b"extracerts").map(Value::deref) {
        out.extracerts = Some(match &v {
            Value::Array(arr) => {
                let mut list = Vec::with_capacity(arr.len());
                for item in arr.values() {
                    list.push(cert_hold(vm, &item.deref())?);
                }
                (list, true)
            }
            other => (vec![cert_hold(vm, other)?], false),
        });
    }
    Ok(out)
}

fn pkcs12_export_to_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[1])?;
    let cert = cert_hold(vm, &a.at(0))?;
    let file = a.str(1);
    let key = key_hold(vm, &a.at(2))?;
    let pass = a.str(3);
    let opts = pkcs12_options(vm, opt_array(a, 4).as_ref())?;
    let ok = run_cert(vm, a, 1, |s| {
        s.pkcs12_export_to_file(
            cert.arg(),
            file.as_bytes(),
            &key.arg(),
            pass.as_bytes(),
            &opts.arg(),
        )
    })?;
    Ok(Value::Bool(ok))
}

fn pkcs12_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let key = key_hold(vm, &a.at(2))?;
    let pass = a.str(3);
    let opts = pkcs12_options(vm, opt_array(a, 4).as_ref())?;
    let r = run_cert(vm, a, 1, |s| {
        s.pkcs12_export(cert.arg(), &key.arg(), pass.as_bytes(), &opts.arg())
    })?;
    Ok(Value::Bool(r.is_some_and(|out| {
        a.set_ref(1, bytes(out));
        true
    })))
}

fn pkcs12_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (data, pass) = (a.str(0), a.str(2));
    let r = run_ok(vm, a, |s| s.pkcs12_read(data.as_bytes(), pass.as_bytes()))?;
    Ok(Value::Bool(r.is_some_and(|arr| {
        a.set_ref(1, Value::Array(from_php_array(arr)));
        true
    })))
}

/// Registers the S/MIME and PKCS#12 functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("openssl_pkcs7_verify(string $input_filename, int $flags, ?string $signers_certificates_filename = null, array $ca_info = [], ?string $untrusted_certificates_filename = null, ?string $content = null, ?string $output_filename = null): bool|int", pkcs7_verify);
    m.function_static("openssl_pkcs7_encrypt(string $input_filename, string $output_filename, $certificate, ?array $headers, int $flags = 0, int $cipher_algo = OPENSSL_CIPHER_AES_128_CBC): bool", pkcs7_encrypt);
    m.function_static("openssl_pkcs7_sign(string $input_filename, string $output_filename, OpenSSLCertificate|string $certificate, $private_key, ?array $headers, int $flags = PKCS7_DETACHED, ?string $untrusted_certificates_filename = null): bool", pkcs7_sign);
    m.function_static("openssl_pkcs7_decrypt(string $input_filename, string $output_filename, $certificate, $private_key = null): bool", pkcs7_decrypt);
    m.function_static(
        "openssl_pkcs7_read(string $data, &$certificates): bool",
        pkcs7_read,
    );
    m.function_static("openssl_cms_verify(string $input_filename, int $flags = 0, ?string $certificates = null, array $ca_info = [], ?string $untrusted_certificates_filename = null, ?string $content = null, ?string $pk7 = null, ?string $sigfile = null, int $encoding = OPENSSL_ENCODING_SMIME): bool", cms_verify);
    m.function_static("openssl_cms_encrypt(string $input_filename, string $output_filename, $certificate, ?array $headers, int $flags = 0, int $encoding = OPENSSL_ENCODING_SMIME, string|int $cipher_algo = OPENSSL_CIPHER_AES_128_CBC): bool", cms_encrypt);
    m.function_static("openssl_cms_sign(string $input_filename, string $output_filename, OpenSSLCertificate|string $certificate, $private_key, ?array $headers, int $flags = 0, int $encoding = OPENSSL_ENCODING_SMIME, ?string $untrusted_certificates_filename = null): bool", cms_sign);
    m.function_static("openssl_cms_decrypt(string $input_filename, string $output_filename, $certificate, $private_key = null, int $encoding = OPENSSL_ENCODING_SMIME): bool", cms_decrypt);
    m.function_static(
        "openssl_cms_read(string $input_filename, &$certificates): bool",
        cms_read,
    );
    m.function_static("openssl_pkcs12_export_to_file(OpenSSLCertificate|string $certificate, string $output_filename, $private_key, string $passphrase, array $options = []): bool", pkcs12_export_to_file);
    m.function_static("openssl_pkcs12_export(OpenSSLCertificate|string $certificate, &$output, $private_key, string $passphrase, array $options = []): bool", pkcs12_export);
    m.function_static(
        "openssl_pkcs12_read(string $pkcs12, &$certificates, string $passphrase): bool",
        pkcs12_read,
    );
}
