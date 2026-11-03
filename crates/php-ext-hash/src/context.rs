//! The `HashContext` class: native state, object handlers and methods.

use php_hash::{HashContext, StateElem, StateValue};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::object;
use php_vm::Vm;

use crate::throw_error;

/// The class name.
pub(crate) const CLASS: &str = "HashContext";

/// Native state of a `HashContext` object (`php_hashcontext_object`).
///
/// `ctx` is `None` for an object that was never initialised (created by
/// `unserialize()` or reflection); a finalized context keeps its algorithm
/// (for `__debugInfo()`) but [`HashContext::is_finalized`] is set.
#[derive(Debug, Default)]
pub(crate) struct HashObj {
    pub(crate) ctx: Option<HashContext>,
}

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, HashObj::default());
    Ok(())
}

/// `php_hashcontext_clone`.
fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let copy = object::internal::<HashObj, _>(old, |h| {
        h.ctx
            .as_ref()
            .filter(|c| !c.is_finalized())
            .and_then(|c| c.try_clone().ok())
    })
    .flatten();
    if let Some(c) = copy {
        object::set_internal(new, HashObj { ctx: Some(c) });
        Ok(())
    } else {
        object::set_internal(new, HashObj::default());
        Err(vm.throw("ValueError", "Cannot clone a finalized HashContext"))
    }
}

/// Wraps a library context in a new `HashContext` object.
pub(crate) fn new_object(vm: &mut Vm, ctx: HashContext) -> VmResult<Value> {
    let Some(cls) = vm.find_class(CLASS.as_bytes()) else {
        return Err(vm.throw("Error", "Class \"HashContext\" not found"));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(&o, HashObj { ctx: Some(ctx) });
    Ok(Value::Object(o))
}

/// Runs `f` on the live (initialised, non-finalized) context of argument
/// `n`, or throws `PHP_HASHCONTEXT_VERIFY`'s `TypeError`.
pub(crate) fn with_live<R>(
    vm: &mut Vm,
    args: &Args,
    index: usize,
    op: impl FnOnce(&mut HashContext) -> R,
) -> VmResult<R> {
    const INVALID: &str = "must be a valid, non-finalized HashContext";
    let Value::Object(obj) = args.at(index) else {
        return Err(args.type_error(vm, index + 1, INVALID));
    };
    let result = object::internal::<HashObj, _>(&obj, |state| match &mut state.ctx {
        Some(ctx) if !ctx.is_finalized() => Some(op(ctx)),
        _ => None,
    })
    .flatten();
    result.ok_or_else(|| args.type_error(vm, index + 1, INVALID))
}

fn ctor(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Illegal call to private/final constructor"))
}

fn state_to_value(v: &StateValue) -> Value {
    match v {
        StateValue::Int(i) => Value::Int(*i),
        StateValue::Bytes(b) => Value::Str(ZStr::from(b.as_slice())),
    }
}

/// The object's own (dynamic) properties, `zend_std_get_properties`.
fn members(o: &Obj) -> ZArray {
    let d = object::data(o);
    let dynamic = d.dynamic.borrow();
    dynamic
        .as_ref()
        .map(|t| ZArray::from_table(t.clone()))
        .unwrap_or_default()
}

/// `HashContext::__serialize()`.
fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let r = object::internal::<HashObj, _>(&o, |h| h.ctx.as_ref().map(HashContext::serialize))
        .flatten();
    let s = match r {
        Some(Ok(s)) => s,
        Some(Err(e)) => return Err(throw_error(vm, "HashContext::__serialize", &e)),
        None => {
            return Err(vm.throw(
                "Exception",
                "HashContext for algorithm \"\" cannot be serialized",
            ))
        }
    };
    let state = ZArray::from_values(s.state.iter().map(state_to_value));
    Ok(Value::Array(ZArray::from_values([
        Value::string(s.algo),
        Value::Int(s.options),
        Value::Array(state),
        Value::Int(s.magic),
        Value::Array(members(&o)),
    ])))
}

fn elem(v: &Value) -> StateElem<'_> {
    match v {
        Value::Int(i) => StateElem::Int(*i),
        Value::Str(s) => StateElem::Bytes(s.as_bytes()),
        _ => StateElem::Other,
    }
}

/// `HashContext::__unserialize(array $data)`.
fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let data = a.array(0);
    let initialised =
        object::internal::<HashObj, _>(&o, |h| h.ctx.as_ref().is_some_and(|c| !c.is_finalized()))
            .unwrap_or(false);
    if initialised {
        return Err(vm.throw(
            "Exception",
            "HashContext::__unserialize called on initialized object",
        ));
    }
    // The VM's unserialize() currently hands `__unserialize()` an array
    // with numeric *string* keys ("0", "1", ...); accept both forms.
    let get = |i: i64| {
        data.get_int(i)
            .or_else(|| data.get_str(i.to_string().as_bytes()))
            .map(Value::deref)
    };
    let (
        Some(Value::Str(algo)),
        Some(Value::Int(options)),
        Some(hash),
        Some(Value::Int(magic)),
        Some(Value::Array(members)),
    ) = (get(0), get(1), get(2), get(3), get(4))
    else {
        return Err(vm.throw("Exception", "Incomplete or ill-formed serialization data"));
    };
    let result = match &hash {
        Value::Array(st) => {
            let lookup = |j: usize| {
                i64::try_from(j)
                    .ok()
                    .and_then(|j| st.get(&ArrayKey::Int(j)))
                    .map(elem)
            };
            HashContext::unserialize(algo.as_bytes(), options, magic, Some(&lookup))
        }
        _ => HashContext::unserialize(algo.as_bytes(), options, magic, None),
    };
    let ctx = match result {
        Ok(c) => c,
        Err(e) => return Err(throw_error(vm, "HashContext::__unserialize", &e)),
    };
    object::set_internal(&o, HashObj { ctx: Some(ctx) });
    for (k, v) in members.iter() {
        vm.set_property_raw(&o, &k.to_zstr().to_string_lossy(), v.deref());
    }
    Ok(Value::Null)
}

/// `HashContext::__debugInfo()`.
fn debug_info(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let mut out = ZArray::new();
    if let Some(name) =
        object::internal::<HashObj, _>(&o, |h| h.ctx.as_ref().map(HashContext::algo_name)).flatten()
    {
        out.make_mut()
            .insert(ZStr::from("algo"), Value::string(name));
    }
    Ok(Value::Array(out))
}

/// Registers the `HashContext` class.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new(CLASS)
        .flags(class_flags::FINAL)
        .method("__construct()", acc::PRIVATE, ctor)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .method("__debugInfo(): array", acc::PUBLIC, debug_info)
        .hooks(ClassHooks {
            create: Some(create),
            clone: Some(clone),
            ..ClassHooks::default()
        })
        .register(m.vm());
}
