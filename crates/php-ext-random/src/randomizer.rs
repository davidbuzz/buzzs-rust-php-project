//! `Random\Randomizer`.
//!
//! A randomizer draws from either a native engine (sharing its state, see
//! [`EngineRef`]) or a userland `Random\Engine`, whose `generate()` method
//! is called through the VM (`php_random_algo_user`).

use php_random::{ArraySlots, Engine, IntervalBoundary, RandomError, Randomizer, UserEngine};
use php_runtime::{HashTable, Obj, Value, ZArray, ZStr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, Throw, VmResult,
};
use php_vm::object;
use php_vm::Vm;

use crate::engines::{engine_of, EngineRef, SECURE};
use crate::{own_properties, payload_at, throw_random};

const CLASS: &str = "Random\\Randomizer";

/// Where a randomizer's numbers come from.
#[derive(Debug, Clone)]
enum Source {
    /// A native engine (shared state).
    Native(EngineRef),
    /// A userland engine object.
    User(Obj),
}

/// Native state of a `Random\Randomizer` (`php_random_randomizer`).
#[derive(Debug, Default)]
struct RandomizerObj {
    source: Option<Source>,
}

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, RandomizerObj::default());
    Ok(())
}

/// `randomizer_common_init`.
fn source_for(engine: &Obj) -> Source {
    match engine_of(engine) {
        Some(e) => Source::Native(e),
        None => Source::User(engine.clone()),
    }
}

/// Runs `f` with the engine of `$this`, translating library errors (and
/// exceptions thrown by a userland `generate()`) into PHP throwables.
fn run<R>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut dyn Engine) -> php_random::Result<R>,
) -> VmResult<R> {
    let source =
        object::internal::<RandomizerObj, _>(&a.this_obj(), |r| r.source.clone()).flatten();
    match source {
        None => Err(vm.throw("Error", "Random\\Randomizer is not initialized")),
        Some(Source::Native(e)) => {
            let r = f(&mut *e.borrow_mut());
            r.map_err(|err| throw_random(vm, &err, None))
        }
        Some(Source::User(obj)) => {
            let mut pending: Option<Throw> = None;
            let r = {
                let mut generate = || -> php_random::Result<Vec<u8>> {
                    let v = vm
                        .call_method_by_name(&obj, "generate", Vec::new())
                        .and_then(|v| match v.deref() {
                            Value::Str(s) => Ok(s.as_bytes().to_vec()),
                            other => vm.value_to_string(&other).map(|s| s.as_bytes().to_vec()),
                        });
                    v.map_err(|t| {
                        pending = Some(t);
                        RandomError::Propagated
                    })
                };
                let mut engine = UserEngine(&mut generate);
                f(&mut engine)
            };
            r.map_err(|err| throw_random(vm, &err, pending))
        }
    }
}

fn randomizer(engine: &mut dyn Engine) -> Randomizer<&mut dyn Engine> {
    Randomizer::new(engine)
}

// ---------------------------------------------------------- construction

fn readonly_error(vm: &mut Vm) -> Throw {
    vm.throw(
        "Error",
        "Cannot modify readonly property Random\\Randomizer::$engine",
    )
}

fn engine_initialized(o: &Obj) -> bool {
    let d = object::data(o);
    let slots = d.slots.borrow();
    d.class
        .props
        .iter()
        .find(|p| p.name.as_bytes() == b"engine")
        .and_then(|p| slots.get(p.slot as usize))
        .is_some_and(Option::is_some)
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let engine = if let Some(Value::Object(o)) = a.opt(0) {
        o
    } else {
        if engine_initialized(&this) {
            return Err(readonly_error(vm));
        }
        let Some(cls) = vm.find_class(SECURE.as_bytes()) else {
            return Err(vm.throw("Error", "Class \"Random\\Engine\\Secure\" not found"));
        };
        vm.new_object(&cls)?
    };
    if engine_initialized(&this) {
        return Err(readonly_error(vm));
    }
    vm.set_property_raw(&this, "engine", Value::Object(engine.clone()));
    let source = source_for(&engine);
    object::internal::<RandomizerObj, _>(&this, |r| r.source = Some(source));
    Ok(Value::Null)
}

// --------------------------------------------------------------- methods

fn next_int(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |e| randomizer(e).next_int()).map(Value::Int)
}

fn next_float(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |e| randomizer(e).next_float()).map(Value::Float)
}

fn boundary(vm: &mut Vm, a: &Args) -> VmResult<IntervalBoundary> {
    let Some(Value::Object(o)) = a.opt(2) else {
        return Ok(IntervalBoundary::ClosedOpen);
    };
    let name = vm.get_property(&o, "name")?;
    Ok(match name {
        Value::Str(s) => match s.as_bytes() {
            b"ClosedClosed" => IntervalBoundary::ClosedClosed,
            b"OpenClosed" => IntervalBoundary::OpenClosed,
            b"OpenOpen" => IntervalBoundary::OpenOpen,
            _ => IntervalBoundary::ClosedOpen,
        },
        _ => IntervalBoundary::ClosedOpen,
    })
}

fn get_float(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (min, max) = (a.float(0), a.float(1));
    let b = boundary(vm, a)?;
    run(vm, a, |e| randomizer(e).get_float(min, max, b)).map(Value::Float)
}

fn get_int(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (min, max) = (a.int(0), a.int(1));
    run(vm, a, |e| randomizer(e).get_int(min, max)).map(Value::Int)
}

fn get_bytes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    run(vm, a, |e| randomizer(e).get_bytes(n)).map(|b| Value::Str(ZStr::from(b)))
}

fn get_bytes_from_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, n) = (a.str(0), a.int(1));
    run(vm, a, |e| {
        randomizer(e).get_bytes_from_string(s.as_bytes(), n)
    })
    .map(|b| Value::Str(ZStr::from(b)))
}

fn shuffle_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = a.array(0);
    let mut values: Vec<Value> = arr.values().cloned().collect();
    run(vm, a, |e| randomizer(e).shuffle_array(&mut values))?;
    Ok(Value::Array(ZArray::from_values(values)))
}

fn shuffle_bytes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    if s.len() < 2 {
        return Ok(Value::Str(s));
    }
    run(vm, a, |e| randomizer(e).shuffle_bytes(s.as_bytes())).map(|b| Value::Str(ZStr::from(b)))
}

/// The bucket layout of a hash table, for `php_array_pick_keys`.
struct Slots<'a>(&'a HashTable);

impl ArraySlots for Slots<'_> {
    fn num_elements(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap_or(u32::MAX)
    }

    fn num_used(&self) -> u32 {
        self.0.used()
    }

    fn is_slot_used(&self, pos: u32) -> bool {
        self.0.at(pos).is_some()
    }
}

fn pick_array_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = a.array(0);
    let num = a.int(1);
    let picked = run(vm, a, |e| randomizer(e).pick_array_keys(&Slots(&arr), num))?;
    Ok(Value::Array(ZArray::from_values(
        picked
            .into_iter()
            .filter_map(|pos| arr.at(pos).map(|(k, _)| k.to_value())),
    )))
}

// ---------------------------------------------------------- serialization

fn serialize(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::from_values([Value::Array(
        own_properties(&a.this_obj()),
    )])))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let data = a.array(0);
    let invalid = |vm: &mut Vm| {
        vm.throw(
            "Exception",
            "Invalid serialization data for Random\\Randomizer object",
        )
    };
    let members = match payload_at(&data, 0) {
        Some(Value::Array(m)) if data.len() == 1 => m,
        _ => return Err(invalid(vm)),
    };
    // Loading the readonly `engine` property a second time fails.
    if engine_initialized(&this) {
        return Err(invalid(vm));
    }
    let mut engine = None;
    for (k, v) in members.iter() {
        match (k.to_zstr().as_bytes(), v.deref()) {
            (b"engine", Value::Object(o))
                if object::class_of(&o).instance_of_name(b"random\\engine") =>
            {
                engine = Some(o);
            }
            _ => return Err(invalid(vm)),
        }
    }
    let Some(engine) = engine else {
        return Err(invalid(vm));
    };
    vm.set_property_raw(&this, "engine", Value::Object(engine.clone()));
    let source = source_for(&engine);
    object::internal::<RandomizerObj, _>(&this, |r| r.source = Some(source));
    Ok(Value::Null)
}

/// Registers `Random\Randomizer`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new(CLASS)
        .flags(class_flags::FINAL | class_flags::NO_CLONE)
        .prop("engine", acc::PUBLIC | acc::READONLY, Some("Random\\Engine"), None)
        .method("__construct(?Random\\Engine $engine = null)", acc::PUBLIC, construct)
        .method("nextInt(): int", acc::PUBLIC, next_int)
        .method("nextFloat(): float", acc::PUBLIC, next_float)
        .method(
            "getFloat(float $min, float $max, Random\\IntervalBoundary $boundary = Random\\IntervalBoundary::ClosedOpen): float",
            acc::PUBLIC,
            get_float,
        )
        .method("getInt(int $min, int $max): int", acc::PUBLIC, get_int)
        .method("getBytes(int $length): string", acc::PUBLIC, get_bytes)
        .method("getBytesFromString(string $string, int $length): string", acc::PUBLIC, get_bytes_from_string)
        .method("shuffleArray(array $array): array", acc::PUBLIC, shuffle_array)
        .method("shuffleBytes(string $bytes): string", acc::PUBLIC, shuffle_bytes)
        .method("pickArrayKeys(array $array, int $num): array", acc::PUBLIC, pick_array_keys)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .hooks(ClassHooks {
            create: Some(create),
            ..ClassHooks::default()
        })
        .register(m.vm());
}
