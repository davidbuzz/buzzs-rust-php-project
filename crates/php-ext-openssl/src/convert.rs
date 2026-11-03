//! Conversions between VM values and the library's argument and array
//! types (`php_openssl_pkey_from_zval`, `php_openssl_x509_from_zval`, …).

use php_openssl::{Key, PhpArray, PhpValue};
use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::objects::{cert_of, csr_of, key_of, CertHold, CsrHold, KeyHold};

/// A binary string value.
pub(crate) fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

/// `zval_try_get_string`.
pub(crate) fn to_bytes(vm: &mut Vm, v: &Value) -> VmResult<Vec<u8>> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        other => Ok(vm.value_to_string(other)?.as_bytes().to_vec()),
    }
}

/// An `OpenSSLCertificate|string` argument (or an untyped one converted
/// like `php_openssl_x509_from_zval`).
pub(crate) fn cert_hold(vm: &mut Vm, v: &Value) -> VmResult<CertHold> {
    if let Value::Object(o) = v {
        if let Some(c) = cert_of(o) {
            return Ok(CertHold::Obj(c));
        }
    }
    Ok(CertHold::Str(to_bytes(vm, v)?))
}

/// An `OpenSSLCertificateSigningRequest|string` argument.
pub(crate) fn csr_hold(vm: &mut Vm, v: &Value) -> VmResult<CsrHold> {
    if let Value::Object(o) = v {
        if let Some(c) = csr_of(o) {
            return Ok(CsrHold::Obj(c));
        }
    }
    Ok(CsrHold::Str(to_bytes(vm, v)?))
}

/// A key argument, following `php_openssl_pkey_from_zval`'s handling of
/// `[key, passphrase]` arrays, key/certificate objects and strings.
pub(crate) fn key_hold(vm: &mut Vm, v: &Value) -> VmResult<KeyHold> {
    if let Value::Array(arr) = v {
        let passphrase = match arr.get_int(1).map(Value::deref) {
            None => None,
            Some(p) => Some(to_bytes(vm, &p)?),
        };
        let key = match arr.get_int(0).map(Value::deref) {
            None => None,
            Some(Value::Array(_)) => Some(Box::new(KeyHold::Other)),
            Some(k) => Some(Box::new(key_scalar(vm, &k)?)),
        };
        return Ok(KeyHold::Array { key, passphrase });
    }
    key_scalar(vm, v)
}

fn key_scalar(vm: &mut Vm, v: &Value) -> VmResult<KeyHold> {
    match v {
        Value::Object(o) => {
            if let Some(k) = key_of(o) {
                Ok(KeyHold::Key(k))
            } else if let Some(c) = cert_of(o) {
                Ok(KeyHold::Cert(c))
            } else {
                Ok(KeyHold::Str(to_bytes(vm, v)?))
            }
        }
        Value::Str(s) => Ok(KeyHold::Str(s.as_bytes().to_vec())),
        _ => Ok(KeyHold::Other),
    }
}

/// A VM value as a library value (objects and resources are opaque).
pub(crate) fn to_php(v: &Value) -> PhpValue {
    match v {
        Value::Null => PhpValue::Null,
        Value::Bool(b) => PhpValue::Bool(*b),
        Value::Int(i) => PhpValue::Int(*i),
        Value::Float(f) => PhpValue::Float(*f),
        Value::Str(s) => PhpValue::Str(s.as_bytes().to_vec()),
        Value::Array(a) => PhpValue::Array(to_php_array(a)),
        Value::Object(_) | Value::Resource(_) => PhpValue::Object,
        Value::Reference(_) => to_php(&v.deref()),
    }
}

/// A VM array as a library array.
pub(crate) fn to_php_array(a: &ZArray) -> PhpArray {
    a.iter()
        .map(|(k, v)| {
            let key = match k {
                ArrayKey::Int(i) => Key::Int(*i),
                ArrayKey::Str(s) => Key::Str(s.as_bytes().to_vec()),
            };
            (key, to_php(v))
        })
        .collect()
}

/// A library value as a VM value.
pub(crate) fn from_php(v: PhpValue) -> Value {
    match v {
        PhpValue::Null | PhpValue::Object => Value::Null,
        PhpValue::Bool(b) => Value::Bool(b),
        PhpValue::Int(i) => Value::Int(i),
        PhpValue::Float(f) => Value::Float(f),
        PhpValue::Str(s) => bytes(s),
        PhpValue::Array(a) => Value::Array(from_php_array(a)),
    }
}

/// A library array as a VM array (string keys with symbol-table rules).
pub(crate) fn from_php_array(a: PhpArray) -> ZArray {
    let mut out = ZArray::new();
    let t = out.make_mut();
    for (k, v) in a.into_entries() {
        let key = match k {
            Key::Int(i) => ArrayKey::Int(i),
            Key::Str(s) => ArrayKey::from_bytes(&s),
        };
        t.insert(key, from_php(v));
    }
    out
}

/// A list of strings as a VM array.
pub(crate) fn string_list<S: AsRef<[u8]>>(items: impl IntoIterator<Item = S>) -> Value {
    Value::Array(ZArray::from_values(items.into_iter().map(bytes)))
}

/// An optional argument as an array (`?array $x = null`).
pub(crate) fn opt_array(a: &Args, n: usize) -> Option<ZArray> {
    match a.opt(n) {
        Some(Value::Array(arr)) => Some(arr),
        _ => None,
    }
}

/// An optional `?string` argument.
pub(crate) fn opt_bytes(a: &Args, n: usize) -> Option<Vec<u8>> {
    match a.opt(n) {
        Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

/// An `int` argument whose stub default is a constant the signature parser
/// cannot evaluate.
pub(crate) fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    match a.get(n) {
        Some(Value::Int(i)) => i,
        _ => default,
    }
}

/// Converts the string values of an array (`$ca_info`) with
/// `zval_try_get_string`.
pub(crate) fn string_values(vm: &mut Vm, arr: Option<&ZArray>) -> VmResult<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    if let Some(arr) = arr {
        for v in arr.values() {
            out.push(to_bytes(vm, &v.deref())?);
        }
    }
    Ok(out)
}

/// `Ok(Some(x))` → `x`, `Ok(None)` → `false`.
pub(crate) fn or_false<T>(v: Option<T>, f: impl FnOnce(T) -> Value) -> Value {
    v.map_or(Value::Bool(false), f)
}
