//! Symmetric ciphers, digests, PBKDF2, randomness, algorithm listings and
//! `openssl_error_string()`.

use php_openssl::symmetric::{DecryptArgs, EncryptArgs, TagOut};
use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::convert::{bytes, from_php_array, opt_bytes, or_false, string_list};
use crate::state::{run, run_ok};

fn encrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (data, method, pass, iv) = (a.str(0), a.str(1), a.str(2), a.str(4));
    let options = a.int(3);
    let aad = opt_bytes(a, 6);
    let tag_length = a.int(7);
    let tag_requested = a.has(5);
    let r = run(vm, a, |s| {
        s.encrypt(&EncryptArgs {
            data: data.as_bytes(),
            method: method.as_bytes(),
            passphrase: pass.as_bytes(),
            options,
            iv: iv.as_bytes(),
            tag_requested,
            aad: aad.as_deref(),
            tag_length,
        })
    })?;
    let Some(enc) = r else {
        return Ok(Value::Bool(false));
    };
    match enc.tag {
        TagOut::Untouched => {}
        TagOut::Null => a.set_ref(5, Value::Null),
        TagOut::Tag(t) => a.set_ref(5, bytes(t)),
    }
    Ok(bytes(enc.data))
}

fn decrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (data, method, pass, iv) = (a.str(0), a.str(1), a.str(2), a.str(4));
    let options = a.int(3);
    let tag = opt_bytes(a, 5);
    let aad = opt_bytes(a, 6);
    let r = run(vm, a, |s| {
        s.decrypt(&DecryptArgs {
            data: data.as_bytes(),
            method: method.as_bytes(),
            passphrase: pass.as_bytes(),
            options,
            iv: iv.as_bytes(),
            tag: tag.as_deref(),
            aad: aad.as_deref(),
        })
    })?;
    Ok(or_false(r, bytes))
}

fn cipher_iv_length(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = a.str(0);
    let r = run(vm, a, |s| s.cipher_iv_length(m.as_bytes()))?;
    Ok(or_false(r, Value::Int))
}

fn cipher_key_length(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = a.str(0);
    let r = run(vm, a, |s| s.cipher_key_length(m.as_bytes()))?;
    Ok(or_false(r, Value::Int))
}

fn digest(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (data, algo) = (a.str(0), a.str(1));
    let binary = a.bool(2);
    let r = run_ok(vm, a, |s| {
        s.digest(data.as_bytes(), algo.as_bytes(), binary)
    })?;
    Ok(or_false(r, bytes))
}

fn pbkdf2(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (pass, salt, algo) = (a.str(0), a.str(1), a.str(4));
    let (len, iter) = (a.int(2), a.int(3));
    let r = run(vm, a, |s| {
        s.pbkdf2(pass.as_bytes(), salt.as_bytes(), len, iter, algo.as_bytes())
    })?;
    Ok(or_false(r, bytes))
}

fn random_pseudo_bytes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = a.int(0);
    match run(vm, a, |s| s.random_pseudo_bytes(len)) {
        Ok((b, strong)) => {
            if a.has(1) {
                a.set_ref(1, Value::Bool(strong));
            }
            Ok(bytes(b))
        }
        Err(e) => {
            if a.has(1) {
                a.set_ref(1, Value::Bool(false));
            }
            Err(e)
        }
    }
}

fn get_md_methods(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let aliases = a.bool(0);
    Ok(string_list(run_ok(vm, a, |s| s.get_md_methods(aliases))?))
}

fn get_cipher_methods(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let aliases = a.bool(0);
    Ok(string_list(run_ok(vm, a, |s| {
        s.get_cipher_methods(aliases)
    })?))
}

fn get_curve_names(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(string_list(run_ok(
        vm,
        a,
        php_openssl::OpenSsl::get_curve_names,
    )?))
}

fn get_cert_locations(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = run_ok(vm, a, php_openssl::OpenSsl::get_cert_locations)?;
    Ok(Value::Array(from_php_array(arr)))
}

fn error_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = run_ok(vm, a, php_openssl::OpenSsl::error_string)?;
    Ok(or_false(r, Value::string))
}

/// Registers the symmetric/digest/utility functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("openssl_encrypt(string $data, string $cipher_algo, string $passphrase, int $options = 0, string $iv = \"\", &$tag = null, ?string $aad = \"\", int $tag_length = 16): string|false", encrypt);
    m.function("openssl_decrypt(string $data, string $cipher_algo, string $passphrase, int $options = 0, string $iv = \"\", ?string $tag = null, ?string $aad = \"\"): string|false", decrypt);
    m.function(
        "openssl_cipher_iv_length(string $cipher_algo): int|false",
        cipher_iv_length,
    );
    m.function(
        "openssl_cipher_key_length(string $cipher_algo): int|false",
        cipher_key_length,
    );
    m.function(
        "openssl_digest(string $data, string $digest_algo, bool $binary = false): string|false",
        digest,
    );
    m.function("openssl_pbkdf2(string $password, string $salt, int $key_length, int $iterations, string $digest_algo = \"sha1\"): string|false", pbkdf2);
    m.function(
        "openssl_random_pseudo_bytes(int $length, &$strong_result = null): string",
        random_pseudo_bytes,
    );
    m.function(
        "openssl_get_md_methods(bool $aliases = false): array",
        get_md_methods,
    );
    m.function(
        "openssl_get_cipher_methods(bool $aliases = false): array",
        get_cipher_methods,
    );
    m.function("openssl_get_curve_names(): array|false", get_curve_names);
    m.function("openssl_get_cert_locations(): array", get_cert_locations);
    m.function("openssl_error_string(): string|false", error_string);
}
