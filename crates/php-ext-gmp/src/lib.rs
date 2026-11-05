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

/// Turns the native handler `$f` into one that runs inside
/// [`php_memlimit::uncounted`].
///
/// php-src's `ext/gmp` leaves libgmp on the system allocator (it never
/// calls `mp_set_memory_functions`), so the limbs of a GMP number never
/// count toward `memory_limit` or `memory_get_usage()`; running every
/// handler of the extension in an uncounted scope gives php-gmp's limb
/// buffers the same status.
macro_rules! uncounted {
    ($f:ident) => {{
        fn uncounted_handler(
            vm: &mut php_vm::Vm,
            a: &mut php_vm::ext::Args,
        ) -> php_vm::ext::VmResult<php_runtime::Value> {
            php_memlimit::uncounted(|| $f(vm, a))
        }
        uncounted_handler as php_vm::ext::NativeFn
    }};
}

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

/// libgmp's size limit for an `mpz_t`: `INT_MAX` limbs of 64 bits
/// (`mpz_realloc` aborts with "gmp: overflow in mpz type" beyond it).
const MPZ_MAX_BITS: u128 = 0x7fff_ffff * 64;

/// Refuses to build a number of `bits` bits that libgmp could not
/// represent ([`MPZ_MAX_BITS`]) or that cannot fit in the process's
/// address space (`RLIMIT_AS`, which `php-cli` caps).
///
/// libgmp allocates through the system `malloc`, outside the Zend heap, so
/// `memory_limit` plays no part (see `uncounted!`); past those bounds
/// php-src aborts the process (libgmp's "overflow in mpz type" or "Cannot
/// allocate memory"). We end such requests with PHP's out-of-memory fatal
/// error instead, so `gmp_pow(2, 2**40)` does not take the VM down.
pub(crate) fn check_bits(vm: &mut Vm, bits: u128) -> VmResult<()> {
    let limit = address_space_limit().map_or(MPZ_MAX_BITS / 8, |l| l.min(MPZ_MAX_BITS / 8));
    let bytes = bits / 8 + 32;
    if bytes > limit {
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {bytes} bytes)"
        )));
    }
    Ok(())
}

/// The soft `RLIMIT_AS` in bytes, `None` when unlimited or unknown.
fn address_space_limit() -> Option<u128> {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes into the struct we own.
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &raw mut rl) } != 0 {
        return None;
    }
    (rl.rlim_cur != libc::RLIM_INFINITY).then(|| u128::from(rl.rlim_cur))
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
