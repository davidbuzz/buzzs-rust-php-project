//! `Transliterator` and the `transliterator_*()` functions.

use php_intl::transliterator::{self as lib, Transliterator};
use php_runtime::{Obj, Value, ZArray};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};

/// Wraps a transliterator in a new object (setting `$id`).
fn object(vm: &mut Vm, t: Transliterator) -> VmResult<Obj> {
    let o = core::instantiate(vm, "Transliterator")?;
    vm.set_property_raw(&o, "id", s(t.id()));
    core::set_data(&o, t);
    Ok(o)
}

fn construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Exception",
        "An object of this type cannot be created with the new operator.",
    ))
}

fn create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match Transliterator::create(a.str(0).as_bytes(), a.int(1)) {
        Ok(t) => Ok(Value::Object(object(vm, t)?)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn create_from_rules(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match Transliterator::create_from_rules(a.str(0).as_bytes(), a.int(1)) {
        Ok(t) => Ok(Value::Object(object(vm, t)?)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn list_ids(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::list_ids() {
        Ok(ids) => Ok(Value::Array(ZArray::from_values(ids.into_iter().map(s)))),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<Transliterator>(&o) {
        return Err(core::unconstructed(vm, "transliterator"));
    }
    Ok((o, off))
}

fn create_inverse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    match core::with::<Transliterator, _>(&o, |t| t.create_inverse()) {
        Some(Ok(t)) => Ok(Value::Object(object(vm, t)?)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Null),
        None => Ok(Value::Null),
    }
}

fn to_rules(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let esc = a.bool(off);
    match core::with::<Transliterator, _>(&o, |t| t.to_rules(esc)) {
        Some(Ok(r)) => Ok(s(r)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn transliterate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let procedural = a.this.is_none();
    let o = if procedural {
        match a.at(0) {
            Value::Object(o) => o,
            v => {
                core::reset_global();
                let id = vm.value_to_string(&v)?;
                match Transliterator::create(id.as_bytes(), lib::FORWARD) {
                    Ok(t) => object(vm, t)?,
                    Err(e) => {
                        if let php_intl::Error::Intl(ie) = &e {
                            core::record(vm, None, ie)?;
                        } else {
                            return Err(core::throw_engine(vm, a, e, 0));
                        }
                        let msg = core::global_error().get_message();
                        vm.php_warning(&format!(
                            "Could not create transliterator with ID \"{}\" ({msg})",
                            id.to_string_lossy()
                        ))?;
                        return Ok(Value::Bool(false));
                    }
                }
            }
        }
    } else {
        a.this_obj()
    };
    let off = usize::from(procedural);
    core::reset_both(&o);
    let st = a.str(off);
    let (start, end) = (a.int(off + 1), a.int(off + 2));
    match core::with::<Transliterator, _>(&o, |t| {
        t.transliterate(st.as_bytes(), start, end, procedural)
    }) {
        Some(Ok(r)) => Ok(s(r)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Err(core::unconstructed(vm, "transliterator")),
    }
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<Transliterator, _>(old, |t| t.try_clone()) {
        Some(Ok(t)) => core::set_data(new, t),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone Transliterator")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized Transliterator")),
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("Transliterator::__construct", construct),
        ("Transliterator::create", create),
        ("Transliterator::createFromRules", create_from_rules),
        ("Transliterator::createInverse", create_inverse),
        ("Transliterator::toRules", to_rules),
        ("Transliterator::listIDs", list_ids),
        ("Transliterator::transliterate", transliterate),
        ("Transliterator::getErrorCode", core::get_error_code),
        ("Transliterator::getErrorMessage", core::get_error_message),
    ]
}
