//! `openssl_x509_*` and `openssl_csr_*`.

use php_openssl::csr::CsrNewReturn;
use php_openssl::BoolOrInt;
use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::convert::{
    bytes, cert_hold, csr_hold, from_php_array, key_hold, opt_array, opt_bytes, or_false,
    string_values, to_php_array,
};
use crate::objects::{new_cert, new_csr, new_key};
use crate::state::{check_paths, deprecated_function, run, run_cert, run_ok};

fn bool_or_int(v: BoolOrInt) -> Value {
    match v {
        BoolOrInt::Bool(b) => Value::Bool(b),
        BoolOrInt::Int(i) => Value::Int(i),
    }
}

fn x509_export_to_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[1])?;
    let cert = cert_hold(vm, &a.at(0))?;
    let file = a.str(1);
    let no_text = a.bool(2);
    let ok = run_cert(vm, a, 1, |s| {
        s.x509_export_to_file(cert.arg(), file.as_bytes(), no_text)
    })?;
    Ok(Value::Bool(ok))
}

fn x509_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let no_text = a.bool(2);
    let out = run_cert(vm, a, 1, |s| s.x509_export(cert.arg(), no_text))?;
    Ok(Value::Bool(out.is_some_and(|o| {
        a.set_ref(1, bytes(o));
        true
    })))
}

fn x509_fingerprint(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let algo = a.str(1);
    let binary = a.bool(2);
    let r = run_cert(vm, a, 1, |s| {
        s.x509_fingerprint(cert.arg(), algo.as_bytes(), binary)
    })?;
    Ok(or_false(r, bytes))
}

fn x509_check_private_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let key = key_hold(vm, &a.at(1))?;
    let ok = run(vm, a, |s| s.x509_check_private_key(cert.arg(), &key.arg()))?;
    Ok(Value::Bool(ok))
}

fn x509_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let key = key_hold(vm, &a.at(1))?;
    let r = run(vm, a, |s| s.x509_verify(cert.arg(), &key.arg()))?;
    Ok(Value::Int(r))
}

fn x509_parse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let short = a.bool(1);
    let r = run(vm, a, |s| s.x509_parse(cert.arg(), short))?;
    Ok(or_false(r, |arr| Value::Array(from_php_array(arr))))
}

fn x509_checkpurpose(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    let purpose = a.int(1);
    let ca = string_values(vm, opt_array(a, 2).as_ref())?;
    let untrusted = opt_bytes(a, 3);
    let r = run(vm, a, |s| {
        s.x509_checkpurpose(cert.arg(), purpose, &ca, untrusted.as_deref())
    })?;
    Ok(bool_or_int(r))
}

fn x509_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cert = cert_hold(vm, &a.at(0))?;
    match run_cert(vm, a, 1, |s| s.x509_read(cert.arg()))? {
        Some(c) => new_cert(vm, c),
        None => Ok(Value::Bool(false)),
    }
}

fn x509_free(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(
        vm,
        a,
        "as OpenSSLCertificate objects are freed automatically",
    )?;
    Ok(Value::Null)
}

fn csr_export_to_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[1])?;
    let csr = csr_hold(vm, &a.at(0))?;
    let file = a.str(1);
    let no_text = a.bool(2);
    let ok = run(vm, a, |s| {
        s.csr_export_to_file(csr.arg(), file.as_bytes(), no_text)
    })?;
    Ok(Value::Bool(ok))
}

fn csr_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let csr = csr_hold(vm, &a.at(0))?;
    let no_text = a.bool(2);
    let out = run(vm, a, |s| s.csr_export(csr.arg(), no_text))?;
    Ok(Value::Bool(out.is_some_and(|o| {
        a.set_ref(1, bytes(o));
        true
    })))
}

fn csr_sign(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let csr = csr_hold(vm, &a.at(0))?;
    let ca = match a.at(1) {
        Value::Null => None,
        v => Some(cert_hold(vm, &v)?),
    };
    let key = key_hold(vm, &a.at(2))?;
    let days = a.int(3);
    let options = opt_array(a, 4).map(|o| to_php_array(&o));
    let serial = a.int(5);
    let serial_hex = opt_bytes(a, 6);
    let r = run(vm, a, |s| {
        s.csr_sign(
            csr.arg(),
            ca.as_ref().map(super::objects::CertHold::arg),
            &key.arg(),
            days,
            options.as_ref(),
            serial,
            serial_hex.as_deref(),
        )
    })?;
    match r {
        Some(c) => new_cert(vm, c),
        None => Ok(Value::Bool(false)),
    }
}

fn csr_new(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dn = to_php_array(&a.array(0));
    let current = a.at(1);
    let key = match current {
        Value::Null => None,
        v => Some(key_hold(vm, &v)?),
    };
    let options = opt_array(a, 2).map(|o| to_php_array(&o));
    let attribs = opt_array(a, 3).map(|o| to_php_array(&o));
    let r = run(vm, a, |s| {
        let karg = key.as_ref().map(super::objects::KeyHold::arg);
        s.csr_new(&dn, karg.as_ref(), options.as_ref(), attribs.as_ref())
    })?;
    if let Some(k) = r.generated_key {
        let v = new_key(vm, k)?;
        a.set_ref(1, v);
    }
    match r.ret {
        CsrNewReturn::False => Ok(Value::Bool(false)),
        CsrNewReturn::True => Ok(Value::Bool(true)),
        CsrNewReturn::Csr(c) => new_csr(vm, c),
    }
}

fn csr_get_subject(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let csr = csr_hold(vm, &a.at(0))?;
    let short = a.bool(1);
    let r = run(vm, a, |s| s.csr_get_subject(csr.arg(), short))?;
    Ok(or_false(r, |arr| Value::Array(from_php_array(arr))))
}

fn csr_get_public_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let csr = csr_hold(vm, &a.at(0))?;
    let short = a.bool(1);
    match run(vm, a, |s| s.csr_get_public_key(csr.arg(), short))? {
        Some(k) => new_key(vm, k),
        None => Ok(Value::Bool(false)),
    }
}

fn spki_new(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Bool(false));
    };
    let Some(key) = crate::objects::key_of(&o) else {
        return Ok(Value::Bool(false));
    };
    let challenge = a.str(1);
    let algo = crate::convert::int_or(a, 2, php_openssl::constants::OPENSSL_ALGO_MD5);
    let r = run(vm, a, |s| s.spki_new(&key, challenge.as_bytes(), algo))?;
    Ok(or_false(r, bytes))
}

fn spki_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let spki = a.str(0);
    Ok(Value::Bool(run_ok(vm, a, |s| {
        s.spki_verify(spki.as_bytes())
    })?))
}

fn spki_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let spki = a.str(0);
    let r = run_ok(vm, a, |s| s.spki_export(spki.as_bytes()))?;
    Ok(or_false(r, bytes))
}

fn spki_export_challenge(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let spki = a.str(0);
    let r = run_ok(vm, a, |s| s.spki_export_challenge(spki.as_bytes()))?;
    Ok(or_false(r, bytes))
}

/// Registers the X.509, CSR and SPKI functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("openssl_x509_export_to_file(OpenSSLCertificate|string $certificate, string $output_filename, bool $no_text = true): bool", x509_export_to_file);
    m.function("openssl_x509_export(OpenSSLCertificate|string $certificate, &$output, bool $no_text = true): bool", x509_export);
    m.function("openssl_x509_fingerprint(OpenSSLCertificate|string $certificate, string $digest_algo = \"sha1\", bool $binary = false): string|false", x509_fingerprint);
    m.function("openssl_x509_check_private_key(OpenSSLCertificate|string $certificate, $private_key): bool", x509_check_private_key);
    m.function(
        "openssl_x509_verify(OpenSSLCertificate|string $certificate, $public_key): int",
        x509_verify,
    );
    m.function("openssl_x509_parse(OpenSSLCertificate|string $certificate, bool $short_names = true): array|false", x509_parse);
    m.function("openssl_x509_checkpurpose(OpenSSLCertificate|string $certificate, int $purpose, array $ca_info = [], ?string $untrusted_certificates_file = null): bool|int", x509_checkpurpose);
    m.function(
        "openssl_x509_read(OpenSSLCertificate|string $certificate): OpenSSLCertificate|false",
        x509_read,
    );
    m.function(
        "openssl_x509_free(OpenSSLCertificate $certificate): void",
        x509_free,
    );
    m.function("openssl_csr_export_to_file(OpenSSLCertificateSigningRequest|string $csr, string $output_filename, bool $no_text = true): bool", csr_export_to_file);
    m.function("openssl_csr_export(OpenSSLCertificateSigningRequest|string $csr, &$output, bool $no_text = true): bool", csr_export);
    m.function("openssl_csr_sign(OpenSSLCertificateSigningRequest|string $csr, OpenSSLCertificate|string|null $ca_certificate, $private_key, int $days, ?array $options = null, int $serial = 0, ?string $serial_hex = null): OpenSSLCertificate|false", csr_sign);
    m.function("openssl_csr_new(array $distinguished_names, &$private_key, ?array $options = null, ?array $extra_attributes = null): OpenSSLCertificateSigningRequest|bool", csr_new);
    m.function("openssl_csr_get_subject(OpenSSLCertificateSigningRequest|string $csr, bool $short_names = true): array|false", csr_get_subject);
    m.function("openssl_csr_get_public_key(OpenSSLCertificateSigningRequest|string $csr, bool $short_names = true): OpenSSLAsymmetricKey|false", csr_get_public_key);
    m.function("openssl_spki_new(OpenSSLAsymmetricKey $private_key, string $challenge, int $digest_algo = OPENSSL_ALGO_MD5): string|false", spki_new);
    m.function("openssl_spki_verify(string $spki): bool", spki_verify);
    m.function(
        "openssl_spki_export(string $spki): string|false",
        spki_export,
    );
    m.function(
        "openssl_spki_export_challenge(string $spki): string|false",
        spki_export_challenge,
    );
}
