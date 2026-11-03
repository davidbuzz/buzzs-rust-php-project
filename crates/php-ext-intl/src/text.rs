//! `grapheme_*()`, `idn_to_*()` and `Normalizer`.

use php_intl::{grapheme, idn, normalizer};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::path;

fn bytes(v: Vec<u8>) -> Value {
    Value::Str(ZStr::from(v))
}

fn finish<T>(
    vm: &mut Vm,
    a: &Args,
    r: php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
    fail: Value,
) -> VmResult<Value> {
    match r {
        Ok(v) => Ok(ok(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, fail),
    }
}

fn pos_value(p: Option<i64>) -> Value {
    p.map_or(Value::Bool(false), Value::Int)
}

fn strlen(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = grapheme::strlen(a.str(0).as_bytes());
    finish(vm, a, r, Value::Int, Value::Null)
}

type PosFn = fn(&[u8], &[u8], i64, &str) -> php_intl::Result<Option<i64>>;

fn pos_common(vm: &mut Vm, a: &Args, f: PosFn) -> VmResult<Value> {
    core::reset_global();
    let loc = path(vm, a, 3)?;
    let r = f(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2), &loc);
    finish(vm, a, r, pos_value, Value::Bool(false))
}

fn strpos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    pos_common(vm, a, grapheme::strpos)
}

fn stripos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    pos_common(vm, a, grapheme::stripos)
}

fn strrpos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    pos_common(vm, a, grapheme::strrpos)
}

fn strripos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    pos_common(vm, a, grapheme::strripos)
}

fn substr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let _loc = path(vm, a, 3)?;
    let len = a.opt(2).map(|v| if let Value::Int(i) = v { i } else { 0 });
    let r = grapheme::substr(a.str(0).as_bytes(), a.int(1), len);
    finish(vm, a, r, bytes, Value::Bool(false))
}

fn strstr_common(vm: &mut Vm, a: &Args, ci: bool) -> VmResult<Value> {
    core::reset_global();
    let loc = path(vm, a, 3)?;
    let r = grapheme::strstr(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.bool(2),
        ci,
        &loc,
    );
    finish(
        vm,
        a,
        r,
        |v| v.map_or(Value::Bool(false), bytes),
        Value::Bool(false),
    )
}

fn strstr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, false)
}

fn stristr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, true)
}

fn str_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = grapheme::str_split(a.str(0).as_bytes(), a.int(1));
    // One string (header + data) and one array slot per element.
    if let Ok(parts) = &r {
        let bytes: usize = parts.iter().map(|p| 56 + p.len()).sum();
        if !core::reserve(bytes) {
            return Ok(Value::Bool(false));
        }
    }
    finish(
        vm,
        a,
        r,
        |v| Value::Array(ZArray::from_values(v.into_iter().map(bytes))),
        Value::Bool(false),
    )
}

fn levenshtein(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let loc = path(vm, a, 5)?;
    // UTF-16 copies of both strings and two `zend_long` rows.
    let (l1, l2) = (a.str(0).len(), a.str(1).len());
    if !core::reserve(2 * (l1 + l2) + 16 * (l2 + 1)) {
        return Ok(Value::Bool(false));
    }
    let r = grapheme::levenshtein(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        a.int(3),
        a.int(4),
        &loc,
    );
    finish(vm, a, r, Value::Int, Value::Bool(false))
}

fn strrev(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = grapheme::strrev(a.str(0).as_bytes());
    finish(vm, a, r, bytes, Value::Bool(false))
}

fn extract(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let (r, next) = grapheme::extract(a.str(0).as_bytes(), a.int(1), a.int(2), a.int(3));
    if a.has(4) {
        if let Some(n) = next {
            a.set_ref(4, Value::Int(n));
        }
    }
    finish(vm, a, r, bytes, Value::Bool(false))
}

fn idn_common(vm: &mut Vm, a: &Args, to_ascii: bool) -> VmResult<Value> {
    core::reset_global();
    let d = a.str(0);
    let r = if to_ascii {
        idn::to_ascii(d.as_bytes(), a.int(1), a.int(2))
    } else {
        idn::to_utf8(d.as_bytes(), a.int(1), a.int(2))
    };
    // `zend_try_array_init($idna_info)` happens once the arguments are valid.
    if a.has(3)
        && !matches!(
            r,
            Err(php_intl::Error::ArgumentValue { .. } | php_intl::Error::ArgumentType { .. })
        )
    {
        a.set_ref(3, Value::Array(ZArray::new()));
    }
    match r {
        Ok(out) => {
            if a.has(3) {
                if let Some(info) = out.info {
                    let mut arr = ZArray::new();
                    let h = arr.make_mut();
                    h.insert("result", bytes(info.result));
                    h.insert(
                        "isTransitionalDifferent",
                        Value::Bool(info.is_transitional_different),
                    );
                    h.insert("errors", Value::Int(i64::from(info.errors)));
                    a.set_ref(3, Value::Array(arr));
                }
            }
            Ok(out.value.map_or(Value::Bool(false), bytes))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn idn_to_ascii(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    idn_common(vm, a, true)
}

fn idn_to_utf8(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    idn_common(vm, a, false)
}

fn normalize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    // UTF-16 input and output buffers.
    if !core::reserve(4 * a.str(0).len()) {
        return Ok(Value::Bool(false));
    }
    let r = normalizer::normalize(a.str(0).as_bytes(), a.int(1));
    finish(vm, a, r, s, Value::Bool(false))
}

fn is_normalized(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = normalizer::is_normalized(a.str(0).as_bytes(), a.int(1));
    finish(vm, a, r, Value::Bool, Value::Bool(false))
}

fn raw_decomposition(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = normalizer::raw_decomposition(a.str(0).as_bytes(), a.int(1));
    finish(vm, a, r, |v| v.map_or(Value::Null, s), Value::Null)
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("grapheme_strlen", strlen),
        ("grapheme_strpos", strpos),
        ("grapheme_stripos", stripos),
        ("grapheme_strrpos", strrpos),
        ("grapheme_strripos", strripos),
        ("grapheme_substr", substr),
        ("grapheme_strstr", strstr),
        ("grapheme_stristr", stristr),
        ("grapheme_str_split", str_split),
        ("grapheme_levenshtein", levenshtein),
        ("grapheme_strrev", strrev),
        ("grapheme_extract", extract),
        ("idn_to_ascii", idn_to_ascii),
        ("idn_to_utf8", idn_to_utf8),
        ("Normalizer::normalize", normalize),
        ("Normalizer::isNormalized", is_normalized),
        ("Normalizer::getRawDecomposition", raw_decomposition),
    ]
}
