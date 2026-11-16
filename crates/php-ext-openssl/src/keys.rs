// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `openssl_pkey_*`, the RSA primitives, `openssl_sign`/`verify`,
//! `openssl_seal`/`open`, `openssl_dh_compute_key`, `openssl_pkey_derive`.

use php_openssl::asym::DigestAlgo;
use php_openssl::constants::{
    OPENSSL_ALGO_SHA1, OPENSSL_PKCS1_PADDING, OPENSSL_RSA_PSS_SALTLEN_AUTO,
};
use php_runtime::{Value, ZArray};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::convert::{
    bytes, from_php_array, int_or, key_hold, opt_array, opt_bytes, or_false, to_php_array,
};
use crate::objects::{key_of, new_key, KeyHold};
use crate::state::{check_paths, deprecated_function, run, run_ok};

/// The `OpenSSLAsymmetricKey` of a typed parameter.
fn key_obj(a: &Args, n: usize) -> Option<std::rc::Rc<php_openssl::AsymmetricKey>> {
    match a.at(n) {
        Value::Object(o) => key_of(&o),
        _ => None,
    }
}

fn pkey_new(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let options = opt_array(a, 0).map(|o| to_php_array(&o));
    match run(vm, a, |s| s.pkey_new(options.as_ref()))? {
        Some(k) => new_key(vm, k),
        None => Ok(Value::Bool(false)),
    }
}

fn pkey_export_to_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[1])?;
    let key = key_hold(vm, &a.at(0))?;
    let file = a.str(1);
    let pass = opt_bytes(a, 2);
    let options = opt_array(a, 3).map(|o| to_php_array(&o));
    let ok = run(vm, a, |s| {
        s.pkey_export_to_file(
            &key.arg(),
            file.as_bytes(),
            pass.as_deref(),
            options.as_ref(),
        )
    })?;
    Ok(Value::Bool(ok))
}

fn pkey_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = key_hold(vm, &a.at(0))?;
    let pass = opt_bytes(a, 2);
    let options = opt_array(a, 3).map(|o| to_php_array(&o));
    let out = run(vm, a, |s| {
        s.pkey_export(&key.arg(), pass.as_deref(), options.as_ref())
    })?;
    Ok(Value::Bool(out.is_some_and(|o| {
        a.set_ref(1, bytes(o));
        true
    })))
}

fn pkey_get_public(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = key_hold(vm, &a.at(0))?;
    match run(vm, a, |s| s.pkey_get_public(&key.arg()))? {
        Some(k) => new_key(vm, k),
        None => Ok(Value::Bool(false)),
    }
}

fn pkey_get_private(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = key_hold(vm, &a.at(0))?;
    let pass = opt_bytes(a, 1);
    match run(vm, a, |s| s.pkey_get_private(&key.arg(), pass.as_deref()))? {
        Some(k) => new_key(vm, k),
        None => Ok(Value::Bool(false)),
    }
}

fn pkey_free(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(
        vm,
        a,
        "as OpenSSLAsymmetricKey objects are freed automatically",
    )?;
    Ok(Value::Null)
}

fn pkey_get_details(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(key) = key_obj(a, 0) else {
        return Ok(Value::Bool(false));
    };
    let r = run_ok(vm, a, |s| s.pkey_get_details(&key))?;
    Ok(or_false(r, |arr| Value::Array(from_php_array(arr))))
}

fn dh_compute_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let public = a.str(0);
    let Some(key) = key_obj(a, 1) else {
        return Ok(Value::Bool(false));
    };
    let r = run_ok(vm, a, |s| s.dh_compute_key(public.as_bytes(), &key))?;
    Ok(or_false(r, bytes))
}

fn pkey_derive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let public = key_hold(vm, &a.at(0))?;
    let private = key_hold(vm, &a.at(1))?;
    let len = a.has(2).then(|| a.int(2));
    let r = run(vm, a, |s| s.pkey_derive(&public.arg(), &private.arg(), len))?;
    Ok(or_false(r, bytes))
}

/// The four RSA primitives share one shape.
#[derive(Clone, Copy)]
enum Rsa {
    PrivateEncrypt,
    PrivateDecrypt,
    PublicEncrypt,
    PublicDecrypt,
}

fn rsa(vm: &mut Vm, a: &Args, op: Rsa) -> VmResult<Value> {
    if matches!(op, Rsa::PrivateDecrypt | Rsa::PublicEncrypt) {
        check_paths(vm, a, &[4])?;
    }
    let data = a.str(0);
    let key = key_hold(vm, &a.at(2))?;
    let padding = int_or(a, 3, OPENSSL_PKCS1_PADDING);
    let digest = opt_bytes(a, 4);
    let r = run(vm, a, |s| {
        let k = key.arg();
        let d = data.as_bytes();
        match op {
            Rsa::PrivateEncrypt => s.private_encrypt(d, &k, padding),
            Rsa::PrivateDecrypt => s.private_decrypt(d, &k, padding, digest.as_deref()),
            Rsa::PublicEncrypt => s.public_encrypt(d, &k, padding, digest.as_deref()),
            Rsa::PublicDecrypt => s.public_decrypt(d, &k, padding),
        }
    })?;
    Ok(Value::Bool(r.is_some_and(|out| {
        a.set_ref(1, bytes(out));
        true
    })))
}

fn private_encrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rsa(vm, a, Rsa::PrivateEncrypt)
}

fn private_decrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rsa(vm, a, Rsa::PrivateDecrypt)
}

fn public_encrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rsa(vm, a, Rsa::PublicEncrypt)
}

fn public_decrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rsa(vm, a, Rsa::PublicDecrypt)
}

/// `string|int $algorithm = OPENSSL_ALGO_SHA1`.
fn algo(a: &Args, n: usize) -> (Option<Vec<u8>>, i64) {
    match a.get(n) {
        Some(Value::Str(s)) => (Some(s.as_bytes().to_vec()), 0),
        Some(Value::Int(i)) => (None, i),
        _ => (None, OPENSSL_ALGO_SHA1),
    }
}

fn digest_algo(name: Option<&[u8]>, id: i64) -> DigestAlgo<'_> {
    name.map_or(DigestAlgo::Id(id), DigestAlgo::Name)
}

fn sign(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let key = key_hold(vm, &a.at(2))?;
    let (name, id) = algo(a, 3);
    let padding = a.int(4);
    let salt = int_or(a, 5, OPENSSL_RSA_PSS_SALTLEN_AUTO);
    let r = run(vm, a, |s| {
        s.sign(
            data.as_bytes(),
            &key.arg(),
            digest_algo(name.as_deref(), id),
            padding,
            salt,
        )
    })?;
    Ok(Value::Bool(r.is_some_and(|sig| {
        a.set_ref(1, bytes(sig));
        true
    })))
}

fn verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let sig = a.str(1);
    let key = key_hold(vm, &a.at(2))?;
    let (name, id) = algo(a, 3);
    let padding = a.int(4);
    let salt = int_or(a, 5, OPENSSL_RSA_PSS_SALTLEN_AUTO);
    let r = run(vm, a, |s| {
        s.verify(
            data.as_bytes(),
            sig.as_bytes(),
            &key.arg(),
            digest_algo(name.as_deref(), id),
            padding,
            salt,
        )
    })?;
    Ok(or_false(r, Value::Int))
}

fn seal(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let keys_arr = a.array(3);
    let mut holds: Vec<KeyHold> = Vec::with_capacity(keys_arr.len());
    for v in keys_arr.values() {
        holds.push(key_hold(vm, &v.deref())?);
    }
    let method = a.str(4);
    let iv_passed = a.has(5);
    let r = run(vm, a, |s| {
        let args: Vec<_> = holds.iter().map(KeyHold::arg).collect();
        s.seal(data.as_bytes(), &args, method.as_bytes(), iv_passed)
    })?;
    let Some(sealed) = r else {
        return Ok(Value::Bool(false));
    };
    if let Some(out) = sealed.outputs {
        a.set_ref(1, bytes(out.sealed_data));
        a.set_ref(
            2,
            Value::Array(ZArray::from_values(
                out.encrypted_keys.into_iter().map(bytes),
            )),
        );
        if let Some(iv) = out.iv {
            a.set_ref(5, bytes(iv));
        }
    }
    Ok(Value::Int(sealed.length))
}

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let ekey = a.str(2);
    let key = key_hold(vm, &a.at(3))?;
    let method = a.str(4);
    let iv = opt_bytes(a, 5);
    let r = run(vm, a, |s| {
        s.open(
            data.as_bytes(),
            ekey.as_bytes(),
            &key.arg(),
            method.as_bytes(),
            iv.as_deref(),
        )
    })?;
    Ok(Value::Bool(r.is_some_and(|out| {
        a.set_ref(1, bytes(out));
        true
    })))
}

/// Registers the key functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "openssl_pkey_new(?array $options = null): OpenSSLAsymmetricKey|false",
        pkey_new,
    );
    m.function_static("openssl_pkey_export_to_file($key, string $output_filename, ?string $passphrase = null, ?array $options = null): bool", pkey_export_to_file);
    m.function_static("openssl_pkey_export($key, &$output, ?string $passphrase = null, ?array $options = null): bool", pkey_export);
    m.function_static(
        "openssl_pkey_get_public($public_key): OpenSSLAsymmetricKey|false",
        pkey_get_public,
    );
    m.function_static(
        "openssl_get_publickey($public_key): OpenSSLAsymmetricKey|false",
        pkey_get_public,
    );
    m.function_static(
        "openssl_pkey_free(OpenSSLAsymmetricKey $key): void",
        pkey_free,
    );
    m.function_static(
        "openssl_free_key(OpenSSLAsymmetricKey $key): void",
        pkey_free,
    );
    m.function_static("openssl_pkey_get_private($private_key, ?string $passphrase = null): OpenSSLAsymmetricKey|false", pkey_get_private);
    m.function_static("openssl_get_privatekey($private_key, ?string $passphrase = null): OpenSSLAsymmetricKey|false", pkey_get_private);
    m.function_static(
        "openssl_pkey_get_details(OpenSSLAsymmetricKey $key): array|false",
        pkey_get_details,
    );
    m.function_static("openssl_dh_compute_key(string $public_key, OpenSSLAsymmetricKey $private_key): string|false", dh_compute_key);
    m.function_static(
        "openssl_pkey_derive($public_key, $private_key, int $key_length = 0): string|false",
        pkey_derive,
    );
    m.function_static("openssl_private_encrypt(string $data, &$encrypted_data, $private_key, int $padding = OPENSSL_PKCS1_PADDING): bool", private_encrypt);
    m.function_static("openssl_private_decrypt(string $data, &$decrypted_data, $private_key, int $padding = OPENSSL_PKCS1_PADDING, ?string $digest_algo = null): bool", private_decrypt);
    m.function_static("openssl_public_encrypt(string $data, &$encrypted_data, $public_key, int $padding = OPENSSL_PKCS1_PADDING, ?string $digest_algo = null): bool", public_encrypt);
    m.function_static("openssl_public_decrypt(string $data, &$decrypted_data, $public_key, int $padding = OPENSSL_PKCS1_PADDING): bool", public_decrypt);
    m.function_static("openssl_sign(string $data, &$signature, $private_key, string|int $algorithm = OPENSSL_ALGO_SHA1, int $padding = 0, int $salt_length = OPENSSL_RSA_PSS_SALTLEN_AUTO): bool", sign);
    m.function_static("openssl_verify(string $data, string $signature, $public_key, string|int $algorithm = OPENSSL_ALGO_SHA1, int $padding = 0, int $salt_length = OPENSSL_RSA_PSS_SALTLEN_AUTO): int|false", verify);
    m.function_static("openssl_seal(string $data, &$sealed_data, &$encrypted_keys, array $public_key, string $cipher_algo, &$iv = null): int|false", seal);
    m.function_static("openssl_open(string $data, &$output, string $encrypted_key, $private_key, string $cipher_algo, ?string $iv = null): bool", open);
}
