// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/sodium` for the phprust VM: binds the `php-sodium` library (the
//! system libsodium) through the `php_vm::ext` extension API.
//!
//! Every `sodium_*` function, the `SODIUM_*` constants and the
//! `SodiumException` class are registered here; the cryptography, the
//! argument validation and PHP's exact messages live in `php-sodium`.
//!
//! Like php-src, exceptions leaving a sodium function (and every
//! `SodiumException`, wherever it is created) have the arguments scrubbed
//! from all frames of their backtrace, so secrets never show up in traces.
//!
//! The by-reference parameters (`&$state`, `&$string`) are declared
//! untyped, mirroring the C `z` specifier: a non-string variable is reported
//! with sodium's own `SodiumException` instead of a `TypeError`.

// Native handlers share one `fn(&mut Vm, &mut Args) -> VmResult<Value>`
// signature, even those that cannot fail.
#![allow(clippy::unnecessary_wraps)]

use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_sodium::{
    aead, boxes, consts, error, hash, pwhash, ristretto, secretbox, secretstream, sign, util, Error,
};
use php_vm::ext::{
    Args, ClassBuilder, ClassHooks, Extension, InfoItem, ModuleBuilder, Throw, VmResult,
};
use php_vm::Vm;

/// `E_WARNING`.
const E_WARNING: i64 = 2;

/// The `sodium` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sodium;

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// `sodium_remove_param_values_from_backtrace()`: replaces the `args` of
/// every frame in the exception's trace with an empty array.
fn scrub_trace(o: &Obj) {
    let class = php_vm::object::class_of(o);
    let Some(slot) = class
        .props
        .iter()
        .find(|p| p.name.as_bytes() == b"trace")
        .map(|p| p.slot as usize)
    else {
        return;
    };
    let data = php_vm::object::data(o);
    let mut slots = data.slots.borrow_mut();
    let Some(Some(Value::Array(trace))) = slots.get_mut(slot) else {
        return;
    };
    let key = ArrayKey::from("args");
    for (_, frame) in trace.make_mut().iter_mut() {
        if let Value::Array(frame) = frame {
            if frame.contains_key(&key) {
                frame.make_mut().insert("args", Value::Array(ZArray::new()));
            }
        }
    }
}

/// Whether the function scrubs the backtrace when `zend_parse_parameters`
/// fails: every function taking parameters except
/// `sodium_crypto_pwhash_str_needs_rehash` (the parameterless ones use
/// `zend_parse_parameters_none()`, which does not).
fn scrubs_arg_errors(sig: &str) -> bool {
    !sig.contains("()") && !sig.starts_with("sodium_crypto_pwhash_str_needs_rehash(")
}

/// Scrubs the trace of a thrown exception (no-op for exit/fatal unwinds).
fn scrubbed(t: Throw) -> Throw {
    if let Throw::Exception(o) = &t {
        scrub_trace(o);
    }
    t
}

/// The `SodiumException`/`ValueError` for a library error.
fn throw(vm: &mut Vm, e: &Error) -> Throw {
    scrubbed(vm.throw(e.class_name(), e.message()))
}

/// A binary string value.
fn bytes(v: impl Into<ZStr>) -> Value {
    Value::Str(v.into())
}

/// A string result.
fn out(vm: &mut Vm, r: php_sodium::Result<Vec<u8>>) -> VmResult<Value> {
    r.map(bytes).map_err(|e| throw(vm, &e))
}

/// A `string|false` result.
fn out_opt(vm: &mut Vm, r: php_sodium::Result<Option<Vec<u8>>>) -> VmResult<Value> {
    match r {
        Ok(Some(v)) => Ok(bytes(v)),
        Ok(None) => Ok(Value::Bool(false)),
        Err(e) => Err(throw(vm, &e)),
    }
}

/// A `bool` result.
fn out_bool(vm: &mut Vm, r: php_sodium::Result<bool>) -> VmResult<Value> {
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

/// A two-element list `[a, b]`.
fn pair(a: Value, b: Value) -> Value {
    let mut arr = ZArray::new();
    let t = arr.make_mut();
    t.insert(0_i64, a);
    t.insert(1_i64, b);
    Value::Array(arr)
}

/// An `int` parameter whose stub default is a constant expression.
fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// Runs a library call that may warn, then raises the collected
/// `zend_error(E_WARNING, ...)` messages (no prefix) in order.
fn with_warnings<T>(vm: &mut Vm, f: impl FnOnce(&mut dyn FnMut(&str)) -> T) -> VmResult<T> {
    let mut warnings = Vec::new();
    let r = f(&mut |m: &str| warnings.push(m.to_owned()));
    for w in warnings {
        vm.raise(E_WARNING, &w)?;
    }
    Ok(r)
}

/// The string held by the by-reference argument `n`, or `err` (a
/// `SodiumException`) when the variable is not a string.
fn ref_string(vm: &mut Vm, a: &Args, n: usize, err: impl FnOnce() -> Error) -> VmResult<Vec<u8>> {
    match a.at(n) {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(throw(vm, &err())),
    }
}

/// The `$state` reference argument #1 of the stateful APIs.
fn state_arg(vm: &mut Vm, a: &Args) -> VmResult<Vec<u8>> {
    let f = a.func_name();
    ref_string(vm, a, 0, || error::not_a_state_reference(&f))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Handlers whose arguments are all `string`s, mapped to a library
/// function and an output conversion.
macro_rules! strfn {
    ($($name:ident => $lib:path, $conv:ident, [$($n:literal),*];)*) => {
        $(
            fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
                let r = $lib($(a.str($n).as_bytes()),*);
                $conv(vm, r)
            }
        )*
    };
}

/// Handlers returning fresh random key material.
macro_rules! keygen {
    ($($name:ident => $lib:path;)*) => {
        $(
            fn $name(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
                Ok(bytes($lib()))
            }
        )*
    };
}

strfn! {
    aes256gcm_encrypt => aead::aes256gcm_encrypt, out, [0, 1, 2, 3];
    aes256gcm_decrypt => aead::aes256gcm_decrypt, out_opt, [0, 1, 2, 3];
    chacha_encrypt => aead::chacha20poly1305_encrypt, out, [0, 1, 2, 3];
    chacha_decrypt => aead::chacha20poly1305_decrypt, out_opt, [0, 1, 2, 3];
    chacha_ietf_encrypt => aead::chacha20poly1305_ietf_encrypt, out, [0, 1, 2, 3];
    chacha_ietf_decrypt => aead::chacha20poly1305_ietf_decrypt, out_opt, [0, 1, 2, 3];
    xchacha_ietf_encrypt => aead::xchacha20poly1305_ietf_encrypt, out, [0, 1, 2, 3];
    xchacha_ietf_decrypt => aead::xchacha20poly1305_ietf_decrypt, out_opt, [0, 1, 2, 3];
    auth => hash::auth, out, [0, 1];
    auth_verify => hash::auth_verify, out_bool, [0, 1, 2];
    crypto_box => boxes::crypto_box, out, [0, 1, 2];
    box_seed_keypair => boxes::box_seed_keypair, out, [0];
    box_kp_from_sk_pk => boxes::box_keypair_from_secretkey_and_publickey, out, [0, 1];
    box_open => boxes::box_open, out_opt, [0, 1, 2];
    box_publickey => boxes::box_publickey, out, [0];
    box_publickey_from_secretkey => boxes::box_publickey_from_secretkey, out, [0];
    scalarmult_base => boxes::scalarmult_base, out, [0];
    box_seal => boxes::box_seal, out, [0, 1];
    box_seal_open => boxes::box_seal_open, out_opt, [0, 1];
    box_secretkey => boxes::box_secretkey, out, [0];
    rist_add => ristretto::add, out, [0, 1];
    rist_from_hash => ristretto::from_hash, out, [0];
    rist_is_valid_point => ristretto::is_valid_point, out_bool, [0];
    rist_scalar_add => ristretto::scalar_add, out, [0, 1];
    rist_scalar_complement => ristretto::scalar_complement, out, [0];
    rist_scalar_invert => ristretto::scalar_invert, out, [0];
    rist_scalar_mul => ristretto::scalar_mul, out, [0, 1];
    rist_scalar_negate => ristretto::scalar_negate, out, [0];
    rist_scalar_reduce => ristretto::scalar_reduce, out, [0];
    rist_scalar_sub => ristretto::scalar_sub, out, [0, 1];
    rist_sub => ristretto::sub, out, [0, 1];
    rist_scalarmult => ristretto::scalarmult, out, [0, 1];
    rist_scalarmult_base => ristretto::scalarmult_base, out, [0];
    kx_publickey => boxes::kx_publickey, out, [0];
    kx_secretkey => boxes::kx_secretkey, out, [0];
    kx_seed_keypair => boxes::kx_seed_keypair, out, [0];
    scalarmult => boxes::scalarmult, out, [0, 1];
    secretbox => secretbox::secretbox, out, [0, 1, 2];
    secretbox_open => secretbox::secretbox_open, out_opt, [0, 1, 2];
    ss_init_pull => secretstream::init_pull, out, [0, 1];
    shorthash => hash::shorthash, out, [0, 1];
    sign => sign::sign, out, [0, 1];
    sign_detached => sign::sign_detached, out, [0, 1];
    sign_pk_to_curve => sign::sign_ed25519_pk_to_curve25519, out, [0];
    sign_sk_to_curve => sign::sign_ed25519_sk_to_curve25519, out, [0];
    sign_kp_from_sk_pk => sign::sign_keypair_from_secretkey_and_publickey, out, [0, 1];
    sign_open => sign::sign_open, out_opt, [0, 1];
    sign_publickey => sign::sign_publickey, out, [0];
    sign_secretkey => sign::sign_secretkey, out, [0];
    sign_publickey_from_secretkey => sign::sign_publickey_from_secretkey, out, [0];
    sign_seed_keypair => sign::sign_seed_keypair, out, [0];
    sign_verify_detached => sign::sign_verify_detached, out_bool, [0, 1, 2];
    stream_xor => secretbox::stream_xor, out, [0, 1, 2];
    stream_xchacha20_xor => secretbox::stream_xchacha20_xor, out, [0, 1, 2];
    bin2hex => util::bin2hex, out, [0];
    hex2bin => util::hex2bin, out, [0, 1];
}

keygen! {
    aes256gcm_keygen => aead::aes256gcm_keygen;
    chacha_keygen => aead::chacha20poly1305_keygen;
    chacha_ietf_keygen => aead::chacha20poly1305_ietf_keygen;
    xchacha_ietf_keygen => aead::xchacha20poly1305_ietf_keygen;
    auth_keygen => hash::auth_keygen;
    rist_random => ristretto::random;
    rist_scalar_random => ristretto::scalar_random;
    generichash_keygen => hash::generichash_keygen;
    kdf_keygen => hash::kdf_keygen;
    secretbox_keygen => secretbox::secretbox_keygen;
    ss_keygen => secretstream::keygen;
    shorthash_keygen => hash::shorthash_keygen;
    stream_keygen => secretbox::stream_keygen;
    stream_xchacha20_keygen => secretbox::stream_xchacha20_keygen;
}

fn aes256gcm_is_available(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(aead::aes256gcm_is_available()))
}

fn box_keypair(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    out(vm, boxes::box_keypair())
}

fn kx_keypair(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    out(vm, boxes::kx_keypair())
}

fn sign_keypair(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    out(vm, sign::sign_keypair())
}

/// The library signature of the `kx_*_session_keys` pair.
type SessionKeysFn = fn(&[u8], &[u8]) -> php_sodium::Result<(Vec<u8>, Vec<u8>)>;

fn session_keys(vm: &mut Vm, a: &Args, f: SessionKeysFn) -> VmResult<Value> {
    match f(a.str(0).as_bytes(), a.str(1).as_bytes()) {
        Ok((rx, tx)) => Ok(pair(bytes(rx), bytes(tx))),
        Err(e) => Err(throw(vm, &e)),
    }
}

fn kx_client_session_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    session_keys(vm, a, boxes::kx_client_session_keys)
}

fn kx_server_session_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    session_keys(vm, a, boxes::kx_server_session_keys)
}

/// `SODIUM_CRYPTO_GENERICHASH_BYTES`, the default `$length`.
const GENERICHASH_BYTES: i64 = consts::SODIUM_CRYPTO_GENERICHASH_BYTES;

fn generichash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = hash::generichash(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        int_or(a, 2, GENERICHASH_BYTES),
    );
    out(vm, r)
}

fn generichash_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = hash::generichash_init(a.str(0).as_bytes(), int_or(a, 1, GENERICHASH_BYTES));
    out(vm, r)
}

fn generichash_update(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut state = state_arg(vm, a)?;
    match hash::generichash_update(&mut state, a.str(1).as_bytes()) {
        Ok(()) => {
            a.set_ref(0, bytes(state));
            Ok(Value::Bool(true))
        }
        Err(e) => Err(throw(vm, &e)),
    }
}

fn generichash_final(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut state = state_arg(vm, a)?;
    match hash::generichash_final(&mut state, int_or(a, 1, GENERICHASH_BYTES)) {
        Ok(h) => {
            a.set_ref(0, Value::Null);
            Ok(bytes(h))
        }
        Err(e) => Err(throw(vm, &e)),
    }
}

fn kdf_derive_from_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = hash::kdf_derive_from_key(a.int(0), a.int(1), a.str(2).as_bytes(), a.str(3).as_bytes());
    out(vm, r)
}

fn pwhash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let alg = int_or(a, 5, consts::SODIUM_CRYPTO_PWHASH_ALG_DEFAULT);
    let r = with_warnings(vm, |w| {
        pwhash::pwhash(
            a.int(0),
            a.str(1).as_bytes(),
            a.str(2).as_bytes(),
            a.int(3),
            a.int(4),
            alg,
            w,
        )
    })?;
    out(vm, r)
}

fn pwhash_str(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_warnings(vm, |w| {
        pwhash::pwhash_str(a.str(0).as_bytes(), a.int(1), a.int(2), w)
    })?;
    out(vm, r)
}

fn pwhash_str_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_warnings(vm, |w| {
        pwhash::pwhash_str_verify(a.str(0).as_bytes(), a.str(1).as_bytes(), w)
    })?;
    out_bool(vm, r)
}

fn pwhash_str_needs_rehash(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(pwhash::pwhash_str_needs_rehash(
        a.str(0).as_bytes(),
        a.int(1),
        a.int(2),
    )))
}

fn scrypt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_warnings(vm, |w| {
        pwhash::scryptsalsa208sha256(
            a.int(0),
            a.str(1).as_bytes(),
            a.str(2).as_bytes(),
            a.int(3),
            a.int(4),
            w,
        )
    })?;
    out(vm, r)
}

fn scrypt_str(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_warnings(vm, |w| {
        pwhash::scryptsalsa208sha256_str(a.str(0).as_bytes(), a.int(1), a.int(2), w)
    })?;
    out(vm, r)
}

fn scrypt_str_verify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ok = with_warnings(vm, |w| {
        pwhash::scryptsalsa208sha256_str_verify(a.str(0).as_bytes(), a.str(1).as_bytes(), w)
    })?;
    Ok(Value::Bool(ok))
}

fn ss_init_push(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match secretstream::init_push(a.str(0).as_bytes()) {
        Ok((state, header)) => Ok(pair(bytes(state), bytes(header))),
        Err(e) => Err(throw(vm, &e)),
    }
}

fn ss_push(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut state = state_arg(vm, a)?;
    let tag = int_or(
        a,
        3,
        consts::SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_TAG_MESSAGE,
    );
    match secretstream::push(&mut state, a.str(1).as_bytes(), a.str(2).as_bytes(), tag) {
        Ok(c) => {
            a.set_ref(0, bytes(state));
            Ok(bytes(c))
        }
        Err(e) => Err(throw(vm, &e)),
    }
}

fn ss_pull(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut state = state_arg(vm, a)?;
    match secretstream::pull(&mut state, a.str(1).as_bytes(), a.str(2).as_bytes()) {
        Ok(Some((m, tag))) => {
            a.set_ref(0, bytes(state));
            Ok(pair(bytes(m), Value::Int(tag)))
        }
        Ok(None) => Ok(Value::Bool(false)),
        Err(e) => Err(throw(vm, &e)),
    }
}

fn ss_rekey(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut state = state_arg(vm, a)?;
    match secretstream::rekey(&mut state) {
        Ok(()) => {
            a.set_ref(0, bytes(state));
            Ok(Value::Null)
        }
        Err(e) => Err(throw(vm, &e)),
    }
}

fn stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = secretbox::stream(a.int(0), a.str(1).as_bytes(), a.str(2).as_bytes());
    out(vm, r)
}

fn stream_xchacha20(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = secretbox::stream_xchacha20(a.int(0), a.str(1).as_bytes(), a.str(2).as_bytes());
    out(vm, r)
}

fn stream_xchacha20_xor_ic(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = secretbox::stream_xchacha20_xor_ic(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        a.str(3).as_bytes(),
    );
    out(vm, r)
}

fn add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut val = ref_string(vm, a, 0, || Error::Sodium(error::STRINGS_REQUIRED.into()))?;
    match util::add(&mut val, a.str(1).as_bytes()) {
        Ok(()) => {
            a.set_ref(0, bytes(val));
            Ok(Value::Null)
        }
        Err(e) => Err(throw(vm, &e)),
    }
}

fn increment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut val = ref_string(vm, a, 0, || Error::Sodium(error::STRING_REQUIRED.into()))?;
    util::increment(&mut val);
    a.set_ref(0, bytes(val));
    Ok(Value::Null)
}

fn memzero(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut val = ref_string(vm, a, 0, || Error::Sodium(error::STRING_REQUIRED.into()))?;
    util::memzero(&mut val);
    a.set_ref(0, Value::Null);
    Ok(Value::Null)
}

fn compare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::compare(a.str(0).as_bytes(), a.str(1).as_bytes());
    r.map(Value::Int).map_err(|e| throw(vm, &e))
}

fn memcmp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::memcmp(a.str(0).as_bytes(), a.str(1).as_bytes());
    r.map(Value::Int).map_err(|e| throw(vm, &e))
}

fn pad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::pad(a.str(0).as_bytes(), a.int(1));
    out(vm, r)
}

fn unpad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::unpad(a.str(0).as_bytes(), a.int(1));
    out(vm, r)
}

fn bin2base64(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::bin2base64(a.str(0).as_bytes(), a.int(1));
    out(vm, r)
}

fn base642bin(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = util::base642bin(a.str(0).as_bytes(), a.int(1), a.str(2).as_bytes());
    out(vm, r)
}

/// The `create` hook of `SodiumException` (`sodium_exception_create_object`):
/// the trace is scrubbed as soon as the object exists.
fn exception_created(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    scrub_trace(o);
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// `(stub signature, handler)` of every function, in php-src's order.
#[rustfmt::skip]
const FUNCTIONS: &[(&str, php_vm::ext::NativeFn)] = &[
    ("sodium_crypto_aead_aes256gcm_is_available(): bool", aes256gcm_is_available),
    ("sodium_crypto_aead_aes256gcm_decrypt(string $ciphertext, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string|false", aes256gcm_decrypt),
    ("sodium_crypto_aead_aes256gcm_encrypt(#[\\SensitiveParameter] string $message, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string", aes256gcm_encrypt),
    ("sodium_crypto_aead_aes256gcm_keygen(): string", aes256gcm_keygen),
    ("sodium_crypto_aead_chacha20poly1305_decrypt(string $ciphertext, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string|false", chacha_decrypt),
    ("sodium_crypto_aead_chacha20poly1305_encrypt(#[\\SensitiveParameter] string $message, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string", chacha_encrypt),
    ("sodium_crypto_aead_chacha20poly1305_keygen(): string", chacha_keygen),
    ("sodium_crypto_aead_chacha20poly1305_ietf_decrypt(string $ciphertext, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string|false", chacha_ietf_decrypt),
    ("sodium_crypto_aead_chacha20poly1305_ietf_encrypt(#[\\SensitiveParameter] string $message, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string", chacha_ietf_encrypt),
    ("sodium_crypto_aead_chacha20poly1305_ietf_keygen(): string", chacha_ietf_keygen),
    ("sodium_crypto_aead_xchacha20poly1305_ietf_decrypt(string $ciphertext, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string|false", xchacha_ietf_decrypt),
    ("sodium_crypto_aead_xchacha20poly1305_ietf_keygen(): string", xchacha_ietf_keygen),
    ("sodium_crypto_aead_xchacha20poly1305_ietf_encrypt(#[\\SensitiveParameter] string $message, string $additional_data, string $nonce, #[\\SensitiveParameter] string $key): string", xchacha_ietf_encrypt),
    ("sodium_crypto_auth(string $message, #[\\SensitiveParameter] string $key): string", auth),
    ("sodium_crypto_auth_keygen(): string", auth_keygen),
    ("sodium_crypto_auth_verify(string $mac, string $message, #[\\SensitiveParameter] string $key): bool", auth_verify),
    ("sodium_crypto_box(#[\\SensitiveParameter] string $message, string $nonce, #[\\SensitiveParameter] string $key_pair): string", crypto_box),
    ("sodium_crypto_box_keypair(): string", box_keypair),
    ("sodium_crypto_box_seed_keypair(#[\\SensitiveParameter] string $seed): string", box_seed_keypair),
    ("sodium_crypto_box_keypair_from_secretkey_and_publickey(#[\\SensitiveParameter] string $secret_key, string $public_key): string", box_kp_from_sk_pk),
    ("sodium_crypto_box_open(string $ciphertext, string $nonce, #[\\SensitiveParameter] string $key_pair): string|false", box_open),
    ("sodium_crypto_box_publickey(#[\\SensitiveParameter] string $key_pair): string", box_publickey),
    ("sodium_crypto_box_publickey_from_secretkey(#[\\SensitiveParameter] string $secret_key): string", box_publickey_from_secretkey),
    ("sodium_crypto_box_seal(#[\\SensitiveParameter] string $message, string $public_key): string", box_seal),
    ("sodium_crypto_box_seal_open(string $ciphertext, #[\\SensitiveParameter] string $key_pair): string|false", box_seal_open),
    ("sodium_crypto_box_secretkey(#[\\SensitiveParameter] string $key_pair): string", box_secretkey),
    ("sodium_crypto_core_ristretto255_add(string $p, string $q): string", rist_add),
    ("sodium_crypto_core_ristretto255_from_hash(string $s): string", rist_from_hash),
    ("sodium_crypto_core_ristretto255_is_valid_point(string $s): bool", rist_is_valid_point),
    ("sodium_crypto_core_ristretto255_random(): string", rist_random),
    ("sodium_crypto_core_ristretto255_scalar_add(string $x, string $y): string", rist_scalar_add),
    ("sodium_crypto_core_ristretto255_scalar_complement(string $s): string", rist_scalar_complement),
    ("sodium_crypto_core_ristretto255_scalar_invert(string $s): string", rist_scalar_invert),
    ("sodium_crypto_core_ristretto255_scalar_mul(string $x, string $y): string", rist_scalar_mul),
    ("sodium_crypto_core_ristretto255_scalar_negate(string $s): string", rist_scalar_negate),
    ("sodium_crypto_core_ristretto255_scalar_random(): string", rist_scalar_random),
    ("sodium_crypto_core_ristretto255_scalar_reduce(string $s): string", rist_scalar_reduce),
    ("sodium_crypto_core_ristretto255_scalar_sub(string $x, string $y): string", rist_scalar_sub),
    ("sodium_crypto_core_ristretto255_sub(string $p, string $q): string", rist_sub),
    ("sodium_crypto_kx_keypair(): string", kx_keypair),
    ("sodium_crypto_kx_publickey(#[\\SensitiveParameter] string $key_pair): string", kx_publickey),
    ("sodium_crypto_kx_secretkey(#[\\SensitiveParameter] string $key_pair): string", kx_secretkey),
    ("sodium_crypto_kx_seed_keypair(#[\\SensitiveParameter] string $seed): string", kx_seed_keypair),
    ("sodium_crypto_kx_client_session_keys(#[\\SensitiveParameter] string $client_key_pair, string $server_key): array", kx_client_session_keys),
    ("sodium_crypto_kx_server_session_keys(#[\\SensitiveParameter] string $server_key_pair, string $client_key): array", kx_server_session_keys),
    ("sodium_crypto_generichash(string $message, #[\\SensitiveParameter] string $key = \"\", int $length = SODIUM_CRYPTO_GENERICHASH_BYTES): string", generichash),
    ("sodium_crypto_generichash_keygen(): string", generichash_keygen),
    ("sodium_crypto_generichash_init(#[\\SensitiveParameter] string $key = \"\", int $length = SODIUM_CRYPTO_GENERICHASH_BYTES): string", generichash_init),
    ("sodium_crypto_generichash_update(&$state, string $message): true", generichash_update),
    ("sodium_crypto_generichash_final(&$state, int $length = SODIUM_CRYPTO_GENERICHASH_BYTES): string", generichash_final),
    ("sodium_crypto_kdf_derive_from_key(int $subkey_length, int $subkey_id, string $context, #[\\SensitiveParameter] string $key): string", kdf_derive_from_key),
    ("sodium_crypto_kdf_keygen(): string", kdf_keygen),
    ("sodium_crypto_pwhash(int $length, #[\\SensitiveParameter] string $password, string $salt, int $opslimit, int $memlimit, int $algo = SODIUM_CRYPTO_PWHASH_ALG_DEFAULT): string", pwhash),
    ("sodium_crypto_pwhash_str(#[\\SensitiveParameter] string $password, int $opslimit, int $memlimit): string", pwhash_str),
    ("sodium_crypto_pwhash_str_verify(string $hash, #[\\SensitiveParameter] string $password): bool", pwhash_str_verify),
    ("sodium_crypto_pwhash_str_needs_rehash(string $password, int $opslimit, int $memlimit): bool", pwhash_str_needs_rehash),
    ("sodium_crypto_pwhash_scryptsalsa208sha256(int $length, #[\\SensitiveParameter] string $password, string $salt, int $opslimit, int $memlimit): string", scrypt),
    ("sodium_crypto_pwhash_scryptsalsa208sha256_str(#[\\SensitiveParameter] string $password, int $opslimit, int $memlimit): string", scrypt_str),
    ("sodium_crypto_pwhash_scryptsalsa208sha256_str_verify(string $hash, #[\\SensitiveParameter] string $password): bool", scrypt_str_verify),
    ("sodium_crypto_scalarmult(#[\\SensitiveParameter] string $n, string $p): string", scalarmult),
    ("sodium_crypto_scalarmult_ristretto255(#[\\SensitiveParameter] string $n, string $p): string", rist_scalarmult),
    ("sodium_crypto_scalarmult_ristretto255_base(#[\\SensitiveParameter] string $n): string", rist_scalarmult_base),
    ("sodium_crypto_secretbox(#[\\SensitiveParameter] string $message, string $nonce, #[\\SensitiveParameter] string $key): string", secretbox),
    ("sodium_crypto_secretbox_keygen(): string", secretbox_keygen),
    ("sodium_crypto_secretbox_open(string $ciphertext, string $nonce, #[\\SensitiveParameter] string $key): string|false", secretbox_open),
    ("sodium_crypto_secretstream_xchacha20poly1305_keygen(): string", ss_keygen),
    ("sodium_crypto_secretstream_xchacha20poly1305_init_push(#[\\SensitiveParameter] string $key): array", ss_init_push),
    ("sodium_crypto_secretstream_xchacha20poly1305_push(&$state, #[\\SensitiveParameter] string $message, string $additional_data = \"\", int $tag = SODIUM_CRYPTO_SECRETSTREAM_XCHACHA20POLY1305_TAG_MESSAGE): string", ss_push),
    ("sodium_crypto_secretstream_xchacha20poly1305_init_pull(string $header, #[\\SensitiveParameter] string $key): string", ss_init_pull),
    ("sodium_crypto_secretstream_xchacha20poly1305_pull(&$state, string $ciphertext, string $additional_data = \"\"): array|false", ss_pull),
    ("sodium_crypto_secretstream_xchacha20poly1305_rekey(&$state): void", ss_rekey),
    ("sodium_crypto_shorthash(string $message, #[\\SensitiveParameter] string $key): string", shorthash),
    ("sodium_crypto_shorthash_keygen(): string", shorthash_keygen),
    ("sodium_crypto_sign(string $message, #[\\SensitiveParameter] string $secret_key): string", sign),
    ("sodium_crypto_sign_detached(string $message, #[\\SensitiveParameter] string $secret_key): string", sign_detached),
    ("sodium_crypto_sign_ed25519_pk_to_curve25519(string $public_key): string", sign_pk_to_curve),
    ("sodium_crypto_sign_ed25519_sk_to_curve25519(#[\\SensitiveParameter] string $secret_key): string", sign_sk_to_curve),
    ("sodium_crypto_sign_keypair(): string", sign_keypair),
    ("sodium_crypto_sign_keypair_from_secretkey_and_publickey(#[\\SensitiveParameter] string $secret_key, string $public_key): string", sign_kp_from_sk_pk),
    ("sodium_crypto_sign_open(string $signed_message, string $public_key): string|false", sign_open),
    ("sodium_crypto_sign_publickey(#[\\SensitiveParameter] string $key_pair): string", sign_publickey),
    ("sodium_crypto_sign_secretkey(#[\\SensitiveParameter] string $key_pair): string", sign_secretkey),
    ("sodium_crypto_sign_publickey_from_secretkey(#[\\SensitiveParameter] string $secret_key): string", sign_publickey_from_secretkey),
    ("sodium_crypto_sign_seed_keypair(#[\\SensitiveParameter] string $seed): string", sign_seed_keypair),
    ("sodium_crypto_sign_verify_detached(string $signature, string $message, string $public_key): bool", sign_verify_detached),
    ("sodium_crypto_stream(int $length, string $nonce, #[\\SensitiveParameter] string $key): string", stream),
    ("sodium_crypto_stream_keygen(): string", stream_keygen),
    ("sodium_crypto_stream_xor(#[\\SensitiveParameter] string $message, string $nonce, #[\\SensitiveParameter] string $key): string", stream_xor),
    ("sodium_crypto_stream_xchacha20(int $length, string $nonce, #[\\SensitiveParameter] string $key): string", stream_xchacha20),
    ("sodium_crypto_stream_xchacha20_keygen(): string", stream_xchacha20_keygen),
    ("sodium_crypto_stream_xchacha20_xor(#[\\SensitiveParameter] string $message, string $nonce, #[\\SensitiveParameter] string $key): string", stream_xchacha20_xor),
    ("sodium_crypto_stream_xchacha20_xor_ic(#[\\SensitiveParameter] string $message, string $nonce, int $counter, #[\\SensitiveParameter] string $key): string", stream_xchacha20_xor_ic),
    ("sodium_add(&$string1, string $string2): void", add),
    ("sodium_compare(#[\\SensitiveParameter] string $string1, #[\\SensitiveParameter] string $string2): int", compare),
    ("sodium_increment(&$string): void", increment),
    ("sodium_memcmp(#[\\SensitiveParameter] string $string1, #[\\SensitiveParameter] string $string2): int", memcmp),
    ("sodium_memzero(#[\\SensitiveParameter] &$string): void", memzero),
    ("sodium_pad(#[\\SensitiveParameter] string $string, int $block_size): string", pad),
    ("sodium_unpad(#[\\SensitiveParameter] string $string, int $block_size): string", unpad),
    ("sodium_bin2hex(#[\\SensitiveParameter] string $string): string", bin2hex),
    ("sodium_hex2bin(#[\\SensitiveParameter] string $string, string $ignore = \"\"): string", hex2bin),
    ("sodium_bin2base64(#[\\SensitiveParameter] string $string, int $id): string", bin2base64),
    ("sodium_base642bin(#[\\SensitiveParameter] string $string, int $id, string $ignore = \"\"): string", base642bin),
    ("sodium_crypto_scalarmult_base(#[\\SensitiveParameter] string $secret_key): string", scalarmult_base),
];

/// Registers the constants in php-src's order: the library version, then
/// the integer constants with the two string prefixes in their places.
fn register_constants(m: &mut ModuleBuilder<'_>) {
    m.constant(
        "SODIUM_LIBRARY_VERSION",
        Value::from(consts::library_version()),
    )
    .constant(
        "SODIUM_LIBRARY_MAJOR_VERSION",
        Value::Int(consts::library_major_version()),
    )
    .constant(
        "SODIUM_LIBRARY_MINOR_VERSION",
        Value::Int(consts::library_minor_version()),
    );
    for &(name, value) in consts::INT_CONSTANTS {
        m.constant(name, Value::Int(value));
        let prefix = match name {
            "SODIUM_CRYPTO_PWHASH_SALTBYTES" => Some((
                "SODIUM_CRYPTO_PWHASH_STRPREFIX",
                consts::SODIUM_CRYPTO_PWHASH_STRPREFIX,
            )),
            "SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_SALTBYTES" => Some((
                "SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_STRPREFIX",
                consts::SODIUM_CRYPTO_PWHASH_SCRYPTSALSA208SHA256_STRPREFIX,
            )),
            _ => None,
        };
        if let Some((n, v)) = prefix {
            m.constant(n, Value::from(v));
        }
    }
}

/// `PHP_MINFO(sodium)`: the `phpinfo()` section.
fn info(_vm: &mut Vm) -> Vec<InfoItem> {
    let version = consts::library_version();
    vec![
        InfoItem::TableStart,
        InfoItem::Row(vec!["sodium support".into(), "enabled".into()]),
        InfoItem::Row(vec!["libsodium headers version".into(), version.clone()]),
        InfoItem::Row(vec!["libsodium library version".into(), version]),
        InfoItem::TableEnd,
    ]
}

impl Extension for Sodium {
    fn name(&self) -> &'static str {
        "sodium"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        php_sodium::init();
        ClassBuilder::new("SodiumException")
            .extends(m.vm(), "Exception")
            .hooks(ClassHooks {
                create: Some(exception_created),
                ..Default::default()
            })
            .register(m.vm());
        for &(sig, f) in FUNCTIONS {
            m.function_static(sig, f);
            if scrubs_arg_errors(sig) {
                m.scrub_arg_error_trace();
            }
        }
        register_constants(m);
        m.info(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_function_is_bound_once() {
        let names: Vec<&str> = FUNCTIONS
            .iter()
            .map(|(sig, _)| sig.split('(').next().unwrap_or_default())
            .collect();
        assert_eq!(names.len(), php_sodium::FUNCTIONS.len());
        for f in php_sodium::FUNCTIONS {
            assert_eq!(names.iter().filter(|n| *n == f).count(), 1, "{f}");
        }
    }

    #[test]
    fn signatures_parse() {
        for (sig, _) in FUNCTIONS {
            let (name, _) = php_vm::ext::check_signature(sig);
            assert!(name.starts_with("sodium_"), "{sig}");
        }
    }

    #[test]
    fn arg_error_scrubbing_follows_php_src() {
        assert!(scrubs_arg_errors(
            "sodium_crypto_shorthash(string $message, #[\\SensitiveParameter] string $key): string"
        ));
        assert!(!scrubs_arg_errors("sodium_crypto_box_keypair(): string"));
        assert!(!scrubs_arg_errors(
            "sodium_crypto_pwhash_str_needs_rehash(string $password, int $opslimit, int $memlimit): bool"
        ));
    }

    #[test]
    fn helpers() {
        let Value::Array(a) = pair(Value::Int(1), Value::Int(2)) else {
            panic!("not an array");
        };
        assert_eq!(a.len(), 2);
        assert!(matches!(bytes(b"ab".to_vec()), Value::Str(s) if s.as_bytes() == b"ab"));
    }
}
