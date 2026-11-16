// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The native engine classes `Random\Engine\{Mt19937, PcgOneseq128XslRr64,
//! Xoshiro256StarStar, Secure}`.
//!
//! Each engine object owns an [`EngineRef`]: the state shared (like the C
//! `php_random_algo_with_state` pointer) with every `Random\Randomizer`
//! constructed from it, so that both observe the same sequence.

use std::cell::RefCell;
use std::rc::Rc;

use php_random::{
    Engine, Mt19937, Mt19937Mode, NativeEngine, PcgOneseq128XslRr64, RandomError, Secure, Seed,
    StateArray, StateValue, Xoshiro256StarStar,
};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::object;
use php_vm::Vm;

use crate::{own_properties, payload_at, throw_random, E_DEPRECATED};

pub(crate) const MT19937: &str = "Random\\Engine\\Mt19937";
pub(crate) const PCG: &str = "Random\\Engine\\PcgOneseq128XslRr64";
pub(crate) const XOSHIRO: &str = "Random\\Engine\\Xoshiro256StarStar";
pub(crate) const SECURE: &str = "Random\\Engine\\Secure";

/// The shared state of a native engine object.
pub(crate) type EngineRef = Rc<RefCell<NativeEngine>>;

/// Native state of an engine object.
#[derive(Debug)]
pub(crate) struct EngineObj(pub(crate) EngineRef);

/// The engine state of a native engine object, if `o` is one.
pub(crate) fn engine_of(o: &Obj) -> Option<EngineRef> {
    object::internal::<EngineObj, _>(o, |e| e.0.clone())
}

/// The unseeded state `create_object` allocates for a class.
fn initial_state(class: &[u8]) -> NativeEngine {
    match class {
        b"Random\\Engine\\Mt19937" => {
            NativeEngine::Mt19937(Box::new(Mt19937::new(0, Mt19937Mode::Mt19937)))
        }
        b"Random\\Engine\\PcgOneseq128XslRr64" => {
            NativeEngine::Pcg(PcgOneseq128XslRr64::from_seed128(0))
        }
        b"Random\\Engine\\Xoshiro256StarStar" => {
            NativeEngine::Xoshiro(Xoshiro256StarStar::from_seed64(0))
        }
        _ => NativeEngine::Secure(Secure),
    }
}

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let class = object::class_of(o).name.clone();
    object::set_internal(
        o,
        EngineObj(Rc::new(RefCell::new(initial_state(class.as_bytes())))),
    );
    Ok(())
}

/// `php_random_engine_common_clone_object`: a deep copy of the state.
fn clone(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    if let Some(e) = engine_of(old) {
        let copy = e.borrow().clone();
        object::set_internal(new, EngineObj(Rc::new(RefCell::new(copy))));
    }
    Ok(())
}

/// Runs `f` on the engine of `$this`.
fn with_this<R>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut NativeEngine) -> php_random::Result<R>,
) -> VmResult<R> {
    let Some(e) = engine_of(&a.this_obj()) else {
        return Err(vm.throw("Error", "Engine is not initialized"));
    };
    let r = f(&mut e.borrow_mut());
    r.map_err(|err| throw_random(vm, &err, None))
}

/// Replaces the state of `$this` (constructors), keeping it shared.
fn set_state(a: &Args, state: NativeEngine) {
    if let Some(e) = engine_of(&a.this_obj()) {
        *e.borrow_mut() = state;
    }
}

/// The `string|int|null $seed` argument.
fn seed_arg(a: &Args) -> Value {
    a.opt(0).unwrap_or(Value::Null)
}

fn with_seed<R>(v: &Value, f: impl FnOnce(Seed<'_>) -> R) -> R {
    match v {
        Value::Int(i) => f(Seed::Int(*i)),
        Value::Str(s) => f(Seed::Bytes(s.as_bytes())),
        _ => f(Seed::Random),
    }
}

// --------------------------------------------------------- constructors

fn mt_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let seed = a.opt(0).map(|_| a.int(0));
    let mode = if a.has(1) {
        a.int(1)
    } else {
        php_random::MT_RAND_MT19937
    };
    if let Err(e) = Mt19937Mode::from_constructor_arg(mode) {
        return Err(throw_random(vm, &e, None));
    }
    if mode == php_random::MT_RAND_PHP {
        vm.raise(E_DEPRECATED, php_random::engine::MT_RAND_PHP_DEPRECATION)?;
    }
    match Mt19937::construct(seed, mode) {
        Ok(mt) => {
            set_state(a, NativeEngine::Mt19937(Box::new(mt)));
            Ok(Value::Null)
        }
        Err(e) => Err(throw_random(vm, &e, None)),
    }
}

fn pcg_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let seed = seed_arg(a);
    match with_seed(&seed, PcgOneseq128XslRr64::construct) {
        Ok(e) => {
            set_state(a, NativeEngine::Pcg(e));
            Ok(Value::Null)
        }
        Err(e) => Err(throw_random(vm, &e, None)),
    }
}

fn xoshiro_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let seed = seed_arg(a);
    match with_seed(&seed, Xoshiro256StarStar::construct) {
        Ok(e) => {
            set_state(a, NativeEngine::Xoshiro(e));
            Ok(Value::Null)
        }
        Err(e) => Err(throw_random(vm, &e, None)),
    }
}

// --------------------------------------------------------------- methods

fn generate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let bytes = with_this(vm, a, Engine::generate_bytes)?;
    Ok(Value::Str(ZStr::from(bytes)))
}

fn pcg_jump(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let advance = a.int(0);
    with_this(vm, a, |e| match e {
        NativeEngine::Pcg(p) => p.jump(advance),
        _ => Ok(()),
    })?;
    Ok(Value::Null)
}

fn xoshiro_jump(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_this(vm, a, |e| {
        if let NativeEngine::Xoshiro(x) = e {
            x.jump();
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn xoshiro_jump_long(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_this(vm, a, |e| {
        if let NativeEngine::Xoshiro(x) = e {
            x.jump_long();
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn state_to_value(v: &StateValue) -> Value {
    match v {
        StateValue::Int(i) => Value::Int(*i),
        StateValue::Str(s) => Value::Str(ZStr::from(s.as_slice())),
        StateValue::Other => Value::Null,
    }
}

fn value_to_state(v: &Value) -> StateValue {
    match v.deref() {
        Value::Int(i) => StateValue::Int(i),
        Value::Str(s) => StateValue::Str(s.as_bytes().to_vec()),
        _ => StateValue::Other,
    }
}

/// Converts a PHP array into the library's [`StateArray`].
pub(crate) fn state_array(arr: &ZArray) -> StateArray {
    let mut out = StateArray::new();
    for (k, v) in arr.iter() {
        match k {
            php_runtime::ArrayKey::Int(i) => out.insert_index(*i, value_to_state(v)),
            php_runtime::ArrayKey::Str(_) => out.insert_string_key(),
        }
    }
    out
}

fn state_list(e: &NativeEngine) -> Option<ZArray> {
    e.serialize_state()
        .map(|s| ZArray::from_values(s.iter().map(state_to_value)))
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let Some(state) = engine_of(&o).and_then(|e| state_list(&e.borrow())) else {
        return Err(vm.throw("Exception", "Engine serialize failed"));
    };
    Ok(Value::Array(ZArray::from_values([
        Value::Array(own_properties(&o)),
        Value::Array(state),
    ])))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let data = a.array(0);
    let class = object::class_of(&o).name.to_string_lossy();
    let invalid = |vm: &mut Vm| throw_random(vm, &RandomError::invalid_serialization(&class), None);
    let Some(e) = engine_of(&o) else {
        return Err(invalid(vm));
    };
    if data.len() != 2 {
        return Err(invalid(vm));
    }
    let Some(Value::Array(members)) = payload_at(&data, 0) else {
        return Err(invalid(vm));
    };
    // Engines do not allow dynamic properties, so any member fails to
    // load.
    if !members.is_empty() {
        return Err(invalid(vm));
    }
    let Some(Value::Array(state)) = payload_at(&data, 1) else {
        return Err(invalid(vm));
    };
    let ok = e.borrow_mut().unserialize_state(&state_array(&state));
    if !ok {
        return Err(invalid(vm));
    }
    Ok(Value::Null)
}

fn debug_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let mut out = own_properties(&o);
    if let Some(e) = engine_of(&o) {
        let e = e.borrow();
        if !matches!(*e, NativeEngine::Secure(_)) {
            let Some(state) = state_list(&e) else {
                return Err(vm.throw("Exception", "Engine serialize failed"));
            };
            out.make_mut()
                .insert(ZStr::from("__states"), Value::Array(state));
        }
    }
    Ok(Value::Array(out))
}

fn hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create),
        clone: Some(clone),
        ..ClassHooks::default()
    }
}

/// Registers the engine classes (after the `Random\Engine` interfaces);
/// all are `@strict-properties` (`ZEND_ACC_NO_DYNAMIC_PROPERTIES`).
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new(MT19937)
        .flags(class_flags::FINAL | class_flags::NO_DYNAMIC_PROPERTIES)
        .implements(m.vm(), &["Random\\Engine"])
        .method(
            "__construct(int|null $seed = null, int $mode = MT_RAND_MT19937)",
            acc::PUBLIC,
            mt_construct,
        )
        .method("generate(): string", acc::PUBLIC, generate)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .method("__debugInfo(): array", acc::PUBLIC, debug_info)
        .hooks(hooks())
        .register(m.vm());
    ClassBuilder::new(PCG)
        .flags(class_flags::FINAL | class_flags::NO_DYNAMIC_PROPERTIES)
        .implements(m.vm(), &["Random\\Engine"])
        .method(
            "__construct(string|int|null $seed = null)",
            acc::PUBLIC,
            pcg_construct,
        )
        .method("generate(): string", acc::PUBLIC, generate)
        .method("jump(int $advance): void", acc::PUBLIC, pcg_jump)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .method("__debugInfo(): array", acc::PUBLIC, debug_info)
        .hooks(hooks())
        .register(m.vm());
    ClassBuilder::new(XOSHIRO)
        .flags(class_flags::FINAL | class_flags::NO_DYNAMIC_PROPERTIES)
        .implements(m.vm(), &["Random\\Engine"])
        .method(
            "__construct(string|int|null $seed = null)",
            acc::PUBLIC,
            xoshiro_construct,
        )
        .method("generate(): string", acc::PUBLIC, generate)
        .method("jump(): void", acc::PUBLIC, xoshiro_jump)
        .method("jumpLong(): void", acc::PUBLIC, xoshiro_jump_long)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .method("__debugInfo(): array", acc::PUBLIC, debug_info)
        .hooks(hooks())
        .register(m.vm());
    ClassBuilder::new(SECURE)
        .flags(
            class_flags::FINAL
                | class_flags::NOT_SERIALIZABLE
                | class_flags::NO_CLONE
                | class_flags::NO_DYNAMIC_PROPERTIES,
        )
        .implements(m.vm(), &["Random\\CryptoSafeEngine"])
        .method("generate(): string", acc::PUBLIC, generate)
        .hooks(hooks())
        .register(m.vm());
}
