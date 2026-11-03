//! `ext/gmp` for the phprust VM: binds the `php-gmp` library (the `gmp_*`
//! functions and the `GMP` class with operator overloading, comparison,
//! casts and serialization) through the `php_vm::ext` extension API.
//!
//! `GMP|int|string` parameters are declared with their stub types, so the
//! VM performs ZPP (weak-mode scalar coercion, the null deprecation and the
//! `must be of type GMP|string|int` `TypeError`); the handlers then convert
//! each value to an [`Mpz`] in argument order.

// Native handlers share one signature even when they cannot fail.
#![allow(clippy::unnecessary_wraps)]
// Handlers follow the usual short names (a = args, o = object, m = number).
#![allow(clippy::many_single_char_names)]

mod class;
mod funcs;

use php_gmp::{php as gmp, GmpError, Mpz};
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, Extension, ModuleBuilder, Throw, VmResult};
use php_vm::object::{has_internal, internal};
use php_vm::Vm;

/// The `gmp` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gmp;

/// Internal state of a `GMP` object.
#[derive(Debug, Clone, Default)]
pub(crate) struct GmpState(pub(crate) Mpz);

/// Throws `e` as raised by `func`.
pub(crate) fn throw(vm: &mut Vm, func: &str, e: &GmpError) -> Throw {
    vm.throw(e.class.name(), &e.message(func))
}

/// Whether `o` is a `GMP` instance.
pub(crate) fn is_gmp(o: &Obj) -> bool {
    has_internal::<GmpState>(o)
}

/// The number held by a `GMP` object.
pub(crate) fn mpz_of(o: &Obj) -> Option<Mpz> {
    internal::<GmpState, _>(o, |s| s.0.clone())
}

/// Wraps `m` in a new `GMP` object.
pub(crate) fn new_gmp(vm: &mut Vm, m: Mpz) -> VmResult<Value> {
    let Some(class) = vm.find_class(b"GMP") else {
        return Err(vm.throw("Error", "Class \"GMP\" not found"));
    };
    let o = vm.new_object(&class)?;
    php_vm::object::set_internal(&o, GmpState(m));
    Ok(Value::Object(o))
}

/// Whether the code calling the running native function (or, for
/// operator handlers, the running code) uses `strict_types`
/// (`ZEND_ARG_USES_STRICT_TYPES()`).
pub(crate) fn caller_strict(vm: &Vm) -> bool {
    let mut frames = vm.frames.iter().rev();
    let top = frames.next();
    match top {
        Some(f) if matches!(f.kind, php_vm::vm::FrameKind::Builtin) => {
            frames.next().is_some_and(|f| f.code.strict_types)
        }
        Some(f) => f.code.strict_types,
        None => false,
    }
}

/// `zend_parse_arg_long_weak` for a float: `None` for NaN/out-of-range,
/// with the "loses precision" deprecation for fractional values.
pub(crate) fn float_to_long(vm: &mut Vm, d: f64) -> VmResult<Option<i64>> {
    if d.is_nan() || !php_runtime::convert::double_fits_long(d) {
        return Ok(None);
    }
    #[allow(clippy::cast_possible_truncation)]
    let l = d as i64;
    if !php_runtime::convert::is_long_compatible(d, l) {
        vm.raise(
            E_DEPRECATED,
            &format!(
                "Implicit conversion from float {} to int loses precision",
                php_runtime::float::format_double_h(d, -1)
            ),
        )?;
    }
    Ok(Some(l))
}

/// Refuses to build a number of `bits` bits when it would not fit in
/// `memory_limit` (4 GiB when unlimited).
///
/// libgmp ignores PHP's memory limit (and aborts the process past 2^37
/// bits); the VM process must not be taken down by `gmp_pow(2, 2**40)`,
/// so such requests end with PHP's out-of-memory fatal error instead.
pub(crate) fn check_bits(vm: &mut Vm, bits: u128) -> VmResult<()> {
    let limit = vm
        .ini_string("memory_limit")
        .map(|raw| php_support_quantity(&raw))
        .filter(|&l| l > 0)
        .map_or(1u128 << 32, |l| u128::from(l.unsigned_abs()));
    let bytes = bits / 8 + 32;
    if bytes > limit {
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {bytes} bytes)"
        )));
    }
    Ok(())
}

/// `zend_ini_parse_quantity` for `memory_limit` (`128M`, `1G`, `-1`).
fn php_support_quantity(raw: &[u8]) -> i64 {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    let (num, mul) = match s.as_bytes().last().map(u8::to_ascii_lowercase) {
        Some(b'k') => (&s[..s.len() - 1], 1i64 << 10),
        Some(b'm') => (&s[..s.len() - 1], 1i64 << 20),
        Some(b'g') => (&s[..s.len() - 1], 1i64 << 30),
        _ => (s, 1),
    };
    num.trim()
        .parse::<i64>()
        .map_or(0, |n| n.saturating_mul(mul))
}

/// `E_DEPRECATED`.
pub(crate) const E_DEPRECATED: i64 = 8192;

/// Converts argument `n`, declared `GMP|int|string` (stub) but registered
/// as `mixed` so that the exact `gmp_zend_parse_arg_into_mpz` semantics
/// apply: GMP objects, ints and integer strings are taken as is; in weak
/// mode `null`, bools and integral floats go through
/// `zend_parse_arg_long_weak`; anything else is a `TypeError`.
pub(crate) fn num_arg(vm: &mut Vm, a: &Args, n: usize, func: &str) -> VmResult<Mpz> {
    let pos = u32::try_from(n + 1).unwrap_or(u32::MAX);
    let v = a.at(n);
    let weak = |vm: &mut Vm, v: &Value| -> VmResult<Option<i64>> {
        if caller_strict(vm) {
            return Ok(None);
        }
        Ok(match v {
            Value::Null => {
                vm.raise(
                    E_DEPRECATED,
                    &format!(
                        "{func}(): Passing null to parameter #{pos} (${}) of type GMP|string|int is deprecated",
                        gmp::param_name(func, pos)
                    ),
                )?;
                Some(0)
            }
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Float(d) => float_to_long(vm, *d)?,
            _ => None,
        })
    };
    let r = match &v {
        Value::Object(o) if is_gmp(o) => return Ok(mpz_of(o).unwrap_or_default()),
        Value::Int(i) => return Ok(Mpz::from(*i)),
        Value::Str(s) => gmp::parse_str(s.as_bytes(), 0, pos),
        Value::Object(_) | Value::Array(_) | Value::Resource(_) => {
            Err(GmpError::arg_type(pos, &v.value_name()))
        }
        other => match weak(vm, other)? {
            Some(i) => return Ok(Mpz::from(i)),
            None => Err(GmpError::arg_type(pos, &v.value_name())),
        },
    };
    r.map_err(|e| throw(vm, func, &e))
}

impl Extension for Gmp {
    fn name(&self) -> &'static str {
        "gmp"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, value) in gmp::INT_CONSTANTS {
            m.constant(name, Value::Int(value));
        }
        m.constant("GMP_VERSION", Value::from(gmp::GMP_VERSION));
        class::register(m);
        funcs::register(m);
        m.on_request_shutdown(funcs::reset_random);
    }
}
