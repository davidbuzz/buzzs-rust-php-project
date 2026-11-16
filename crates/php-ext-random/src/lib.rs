// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/random` for the phprust VM: binds the `php-random` library through
//! the `php_vm::ext` extension API.
//!
//! * Functions: `mt_srand()`, `srand()`, `mt_rand()`, `rand()`,
//!   `mt_getrandmax()`, `getrandmax()`, `lcg_value()`, `random_bytes()`,
//!   `random_int()` and the `MT_RAND_*` constants.
//! * Classes: the `Random\Engine` / `Random\CryptoSafeEngine` interfaces,
//!   the engines `Random\Engine\{Mt19937, PcgOneseq128XslRr64,
//!   Xoshiro256StarStar, Secure}`, `Random\Randomizer`, the
//!   `Random\IntervalBoundary` enum and the `Random\RandomError`,
//!   `Random\BrokenRandomEngineError` and `Random\RandomException`
//!   throwables.
//!
//! # Shared global state
//!
//! The request globals of ext/random (the default `Mt19937` behind
//! `mt_rand()`, the combined LCG, ...) are a [`php_random::RandomGlobals`]
//! stored in the VM (`vm.ext.random`). Other bindings that consume the
//! default engine (`shuffle()`, `str_shuffle()`, `array_rand()`,
//! `uniqid()`, `lcg_value()`) must use the same instance: call
//! [`globals`] to get it.
//!
//! ```ignore
//! let picked = php_ext_random::globals(vm).array_rand(&slots, num);
//! ```

// Native handlers and object hooks must match the VM's fn-pointer
// signatures (`NativeFn`, `ClassHooks`), even when they cannot fail.
#![allow(clippy::unnecessary_wraps)]

mod engines;
mod randomizer;

use php_random::RandomError;
use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, Extension, ModuleBuilder, Throw, VmResult,
};
use php_vm::Vm;

pub use php_random::RandomGlobals;

/// The `random` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Random;

/// `E_DEPRECATED`.
pub(crate) const E_DEPRECATED: i64 = 8192;

/// The request's ext/random globals (`RANDOM_G`), created on first use.
///
/// This is the state behind `mt_srand()`/`mt_rand()`/`rand()`/
/// `lcg_value()`; `shuffle()`, `str_shuffle()`, `array_rand()` and
/// `uniqid()` must draw from it too.
pub fn globals(vm: &mut Vm) -> &mut RandomGlobals {
    vm.ext.random.get_or_insert_with(RandomGlobals::new)
}

/// Converts a library error into the PHP throwable. `pending` carries the
/// exception thrown by a userland `generate()` (for
/// [`RandomError::Propagated`]).
pub(crate) fn throw_random(vm: &mut Vm, e: &RandomError, pending: Option<Throw>) -> Throw {
    match (e, pending) {
        (_, Some(t)) => t,
        (RandomError::Propagated, None) => vm.throw("Random\\RandomError", "Engine failure"),
        (e, None) => vm.throw(e.class_name(), e.message()),
    }
}

/// Element `i` of an `__unserialize()` payload (dereferenced).
pub(crate) fn payload_at(data: &ZArray, i: i64) -> Option<Value> {
    data.get_int(i).map(Value::deref)
}

/// The own properties of an object as an array (`zend_std_get_properties`).
pub(crate) fn own_properties(o: &php_runtime::Obj) -> ZArray {
    let d = php_vm::object::data(o);
    let mut out = ZArray::new();
    {
        let t = out.make_mut();
        let slots = d.slots.borrow();
        for p in &d.class.props {
            if p.flags & acc::STATIC != 0 {
                continue;
            }
            if let Some(Some(v)) = slots.get(p.slot as usize) {
                t.insert(ArrayKey::Str(p.name.clone()), v.clone());
            }
        }
        if let Some(dynamic) = d.dynamic.borrow().as_deref() {
            for (k, v) in dynamic {
                t.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

// ------------------------------------------------------------ functions

fn lcg_value(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(globals(vm).lcg_value()))
}

fn mt_srand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let seed = a.opt(0).map(|_| a.int(0));
    let mode = if a.has(1) {
        a.int(1)
    } else {
        php_random::MT_RAND_MT19937
    };
    if let Some(msg) = globals(vm).mt_srand(seed, mode) {
        vm.raise(E_DEPRECATED, msg)?;
    }
    Ok(Value::Null)
}

/// The `(int $min = UNKNOWN, int $max = UNKNOWN)` pair of `mt_rand()` and
/// `rand()`: `None` without arguments.
fn range_args(vm: &mut Vm, a: &Args) -> VmResult<Option<(i64, i64)>> {
    if !a.has(0) && !a.has(1) {
        return Ok(None);
    }
    if !a.has(1) || !a.has(0) {
        let given = usize::from(a.has(0)) + usize::from(a.has(1));
        return Err(vm.throw(
            "ArgumentCountError",
            &format!(
                "{}() expects exactly 2 arguments, {given} given",
                a.func_name()
            ),
        ));
    }
    Ok(Some((a.int(0), a.int(1))))
}

fn mt_rand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some((min, max)) = range_args(vm, a)? else {
        return Ok(Value::Int(globals(vm).mt_rand()));
    };
    if max < min {
        return Err(a.value_error(vm, 2, "must be greater than or equal to argument #1 ($min)"));
    }
    Ok(Value::Int(globals(vm).mt_rand_common(min, max)))
}

fn rand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some((min, max)) = range_args(vm, a)? else {
        return Ok(Value::Int(globals(vm).mt_rand()));
    };
    Ok(Value::Int(globals(vm).rand_range(min, max)))
}

fn mt_getrandmax(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(RandomGlobals::mt_getrandmax()))
}

fn random_bytes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    if n < 1 {
        return Err(a.value_error(vm, 1, "must be greater than 0"));
    }
    match php_random::csprng::random_bytes(n) {
        Ok(b) => Ok(Value::Str(ZStr::from(b))),
        Err(e) => Err(throw_random(vm, &e, None)),
    }
}

fn random_int(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (min, max) = (a.int(0), a.int(1));
    if min > max {
        return Err(a.value_error(vm, 1, "must be less than or equal to argument #2 ($max)"));
    }
    match php_random::csprng::random_int(min, max) {
        Ok(v) => Ok(Value::Int(v)),
        Err(e) => Err(throw_random(vm, &e, None)),
    }
}

// --------------------------------------------------------- registration

/// Declares the `Random\IntervalBoundary` unit enum.
fn register_interval_boundary(m: &mut ModuleBuilder<'_>) {
    let mut b = ClassBuilder::new("Random\\IntervalBoundary").unit_enum();
    for case in ["ClosedOpen", "ClosedClosed", "OpenClosed", "OpenOpen"] {
        b = b.case(case, None);
    }
    b.register(m.vm());
}

fn register_classes(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new("Random\\RandomError")
        .extends(m.vm(), "Error")
        .register(m.vm());
    ClassBuilder::new("Random\\BrokenRandomEngineError")
        .extends(m.vm(), "Random\\RandomError")
        .register(m.vm());
    ClassBuilder::new("Random\\RandomException")
        .extends(m.vm(), "Exception")
        .register(m.vm());
    ClassBuilder::new("Random\\Engine")
        .flags(class_flags::INTERFACE)
        .abstract_method("generate(): string", acc::PUBLIC)
        .register(m.vm());
    ClassBuilder::new("Random\\CryptoSafeEngine")
        .flags(class_flags::INTERFACE)
        .implements(m.vm(), &["Random\\Engine"])
        .register(m.vm());
    register_interval_boundary(m);
    engines::register(m);
    randomizer::register(m);
}

impl Extension for Random {
    fn name(&self) -> &'static str {
        "random"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.constant("MT_RAND_MT19937", Value::Int(php_random::MT_RAND_MT19937))
            .constant_deprecated(
                "MT_RAND_PHP",
                Value::Int(php_random::MT_RAND_PHP),
                "8.3",
                "as it uses a biased non-standard variant of Mt19937",
            );
        m.function_static("lcg_value(): float", lcg_value)
            .deprecated("8.4", "use \\Random\\Randomizer::getFloat() instead")
            .function_static(
                "mt_srand(?int $seed = null, int $mode = MT_RAND_MT19937): void",
                mt_srand,
            )
            .function_static(
                "srand(?int $seed = null, int $mode = MT_RAND_MT19937): void",
                mt_srand,
            )
            .function_static("rand(int $min = UNKNOWN, int $max = UNKNOWN): int", rand)
            .function_static(
                "mt_rand(int $min = UNKNOWN, int $max = UNKNOWN): int",
                mt_rand,
            )
            .function_static("mt_getrandmax(): int", mt_getrandmax)
            .function_static("getrandmax(): int", mt_getrandmax)
            .function_static("random_bytes(int $length): string", random_bytes)
            .function_static("random_int(int $min, int $max): int", random_int);
        register_classes(m);
    }
}
