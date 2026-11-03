//! The incremental contexts: `deflate_init()`/`deflate_add()` and
//! `inflate_init()`/`inflate_add()`/`inflate_get_status()`/
//! `inflate_get_read_len()`, with the final, opaque `DeflateContext` and
//! `InflateContext` classes carrying the native state.

use php_ext_streams::bridge;
use php_runtime::{Obj, Value, ZStr};
use php_streams::value::{Key, Value as SValue};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::{object, Vm};
use php_zlib::context::{
    object_options_deprecation, DEFLATE_CONSTRUCT_ERROR, INFLATE_CONSTRUCT_ERROR,
};
use php_zlib::{DeflateContext, InflateContext};

use crate::codec::int_or;
use crate::{bytes, zlib_error};

/// `ZLIB_SYNC_FLUSH`, the default `$flush_mode`.
const SYNC_FLUSH: i64 = 2;

/// Native state of a `DeflateContext` object.
struct DeflateState(DeflateContext);

/// Native state of an `InflateContext` object.
struct InflateState(InflateContext);

/// Reads `$options` (`array|object`) as a library option table. Objects
/// raise the deprecation first and contribute their visible properties
/// (PHP's `HASH_OF()`).
fn options_arg(vm: &mut Vm, a: &Args, func: &str) -> VmResult<Vec<(Key, SValue)>> {
    let v = match a.opt(1) {
        None => return Ok(Vec::new()),
        Some(Value::Object(o)) => {
            vm.php_deprecated(&object_options_deprecation(func))?;
            vm.call_value(
                &Value::Str(ZStr::from("get_object_vars")),
                vec![Value::Object(o)],
            )?
        }
        Some(v) => v,
    };
    Ok(match bridge::to_lib(&v) {
        SValue::Array(items) => items,
        _ => Vec::new(),
    })
}

/// Instantiates a context class without running its constructor.
fn instantiate(vm: &mut Vm, class: &str) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    vm.new_object(&cls)
}

fn deflate_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let encoding = a.int(0);
    let options = options_arg(vm, a, "deflate_init")?;
    let r = bridge::run(vm, || DeflateContext::init(encoding, &options))?;
    match r {
        Ok(ctx) => {
            let o = instantiate(vm, "DeflateContext")?;
            object::set_internal(&o, DeflateState(ctx));
            Ok(Value::Object(o))
        }
        Err(e) => zlib_error(vm, &e),
    }
}

fn deflate_add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Bool(false));
    };
    let data = a.str(1);
    let flush = int_or(a, 2, SYNC_FLUSH);
    let r = object::internal::<DeflateState, _>(&o, |s| s.0.add(data.as_bytes(), flush));
    match r {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => zlib_error(vm, &e),
        None => Err(vm.throw("Error", "DeflateContext is not initialized")),
    }
}

fn inflate_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let encoding = a.int(0);
    let options = options_arg(vm, a, "inflate_init")?;
    let r = bridge::run(vm, || InflateContext::init(encoding, &options))?;
    match r {
        Ok(ctx) => {
            let o = instantiate(vm, "InflateContext")?;
            object::set_internal(&o, InflateState(ctx));
            Ok(Value::Object(o))
        }
        Err(e) => zlib_error(vm, &e),
    }
}

/// Runs `f` on the native state of the `InflateContext` argument.
fn with_inflate<R>(vm: &mut Vm, a: &Args, f: impl FnOnce(&mut InflateContext) -> R) -> VmResult<R> {
    let r = match a.at(0) {
        Value::Object(o) => object::internal::<InflateState, _>(&o, |s| f(&mut s.0)),
        _ => None,
    };
    r.ok_or_else(|| vm.throw("Error", "InflateContext is not initialized"))
}

fn inflate_add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(1);
    let flush = int_or(a, 2, SYNC_FLUSH);
    let r = with_inflate(vm, a, |c| c.add(data.as_bytes(), flush))?;
    match r {
        Ok(v) => {
            crate::mem::check(vm, crate::mem::zstr(v.len()))?;
            Ok(bytes(v))
        }
        Err(e) => zlib_error(vm, &e),
    }
}

fn inflate_get_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with_inflate(vm, a, |c| c.status())?))
}

fn inflate_get_read_len(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with_inflate(vm, a, |c| c.read_len())?))
}

fn deflate_ctor(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", DEFLATE_CONSTRUCT_ERROR))
}

fn inflate_ctor(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", INFLATE_CONSTRUCT_ERROR))
}

/// Registers the classes and functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let flags = class_flags::FINAL | class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE;
    ClassBuilder::new("InflateContext")
        .flags(flags)
        .method("__construct()", acc::PUBLIC, inflate_ctor)
        .register(m.vm());
    ClassBuilder::new("DeflateContext")
        .flags(flags)
        .method("__construct()", acc::PUBLIC, deflate_ctor)
        .register(m.vm());
    m.function(
        "deflate_init(int $encoding, array|object $options = []): DeflateContext|false",
        deflate_init,
    )
    .function(
        "deflate_add(DeflateContext $context, string $data, int $flush_mode = ZLIB_SYNC_FLUSH): string|false",
        deflate_add,
    )
    .function(
        "inflate_init(int $encoding, array|object $options = []): InflateContext|false",
        inflate_init,
    )
    .function(
        "inflate_add(InflateContext $context, string $data, int $flush_mode = ZLIB_SYNC_FLUSH): string|false",
        inflate_add,
    )
    .function(
        "inflate_get_status(InflateContext $context): int",
        inflate_get_status,
    )
    .function(
        "inflate_get_read_len(InflateContext $context): int",
        inflate_get_read_len,
    );
}
