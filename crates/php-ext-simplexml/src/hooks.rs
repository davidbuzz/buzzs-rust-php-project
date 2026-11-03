//! The `SimpleXMLElement` object handlers as VM [`ClassHooks`].
//!
//! The VM's property/dimension hooks do not say which fetch they serve
//! (`BP_VAR_R`/`W`/`RW`/`IS`); [`fetch_mode`] recovers it from the
//! instruction being executed.

use php_runtime::{CastTarget, Obj, PropertyPurpose, RtResult, Value, ZArray, ZStr};
use php_simplexml::{CheckKind, FetchMode, PropertyPtr, Sxe, SxeRef};
use php_vm::ext::{ClassHooks, NativeIterator, VmResult};
use php_vm::func::Func;
use php_vm::opcode::{FetchMode as OpFetch, Op};
use php_vm::vm::FrameKind;
use php_vm::Vm;

use crate::host::{rt_throw, sxe_of, VmHost};

/// The hooks of `SimpleXMLElement` (inherited by subclasses).
pub fn class_hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create),
        clone: Some(clone),
        properties_for: Some(properties_for),
        compare: Some(compare),
        cast: Some(cast),
        count: Some(count),
        read_property: Some(read_property),
        write_property: Some(write_property),
        has_property: Some(has_property),
        unset_property: Some(unset_property),
        read_dimension: Some(read_dimension),
        write_dimension: Some(write_dimension),
        has_dimension: Some(has_dimension),
        unset_dimension: Some(unset_dimension),
        get_iterator: Some(get_iterator),
        ..ClassHooks::default()
    }
}

/// Runs a library handler with the VM as host, converting its error.
pub fn direct<R>(vm: &mut Vm, f: impl FnOnce(&mut VmHost<'_>) -> RtResult<R>) -> VmResult<R> {
    let r = f(&mut VmHost::new(vm));
    r.map_err(|e| rt_throw(vm, e))
}

/// The fetch type of the property/dimension access being executed.
pub fn fetch_mode(vm: &Vm) -> FetchMode {
    let Some(f) = vm.frames.iter().rev().find(|f| f.kind != FrameKind::Builtin) else {
        return FetchMode::Read;
    };
    match f.ip.checked_sub(1).and_then(|i| f.code.ops.get(i)) {
        Some(Op::Assign { .. } | Op::AssignRef { .. } | Op::MakeRef { .. }) => FetchMode::Write,
        Some(Op::FeReset { place: Some(_), .. }) => FetchMode::Write,
        Some(Op::AssignOp { .. } | Op::IncDec { .. }) => FetchMode::ReadWrite,
        Some(
            Op::Isset { .. }
            | Op::Empty { .. }
            | Op::Fetch {
                mode: OpFetch::Is, ..
            },
        ) => FetchMode::IsSet,
        _ => FetchMode::Read,
    }
}

/// Whether `method` of the object's class is implemented in PHP code.
pub fn is_user_method(o: &Obj, method: &str) -> bool {
    php_vm::object::class_of(o)
        .find_method(method.as_bytes())
        .is_some_and(|m| matches!(m.func, Func::User(_)))
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::create`
fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let class = php_vm::object::class_of(o).name.clone();
    php_vm::object::set_internal::<SxeRef>(o, Sxe::new(class));
    Ok(())
}

fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let Some(st) = sxe_of(old) else {
        return Ok(());
    };
    let copy = direct(vm, |_| Sxe::clone_object(&st))?;
    php_vm::object::set_internal::<SxeRef>(new, copy);
    Ok(())
}

fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    if purpose == PropertyPurpose::Debug && php_vm::object::class_of(o).magic.debug_info.is_some() {
        // The VM calls `__debugInfo()` (native or a user override).
        return Ok(None);
    }
    let is_debug = purpose == PropertyPurpose::Debug;
    direct(vm, |h| Sxe::properties(&st, h, is_debug)).map(Some)
}

fn compare(vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a.deref(), b.deref()) else {
        return Ok(None);
    };
    let (Some(sx), Some(sy)) = (sxe_of(&x), sxe_of(&y)) else {
        return Ok(None);
    };
    direct(vm, |_| Sxe::compare(&sx, &sy)).map(Some)
}

fn cast(vm: &mut Vm, o: &Obj, target: CastTarget) -> VmResult<Option<Value>> {
    if matches!(target, CastTarget::String | CastTarget::Array) {
        // Strings go through `__toString()` (native or a user override,
        // `zend_std_cast_object_tostring`); arrays through the property
        // table.
        return Ok(None);
    }
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    direct(vm, |h| Sxe::cast(&st, h, target))
}

fn count(vm: &mut Vm, o: &Obj) -> VmResult<Option<i64>> {
    if is_user_method(o, "count") {
        // `intern->fptr_count`: the VM calls the user's `count()`.
        return Ok(None);
    }
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    direct(vm, |_| Sxe::count(&st)).map(Some)
}

fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    let mode = fetch_mode(vm);
    direct(vm, |h| Sxe::read_property(&st, h, name.as_bytes(), mode)).map(Some)
}

/// The `__get` fallback the VM uses for intermediate property fetches in
/// write context (`$sxe->a->b = 1`, `$sxe->a[] = 1`): PHP's
/// `get_property_ptr_ptr` (creating a missing element) followed by
/// `read_property(BP_VAR_W)`.
pub fn property_for_write(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Value> {
    let Some(st) = sxe_of(o) else {
        return Ok(Value::Null);
    };
    let trampoline = |vm: &mut Vm, f: &dyn Fn(&mut VmHost<'_>) -> RtResult<Value>| {
        let mut h = VmHost::new(vm);
        h.skip_native = true;
        let r = f(&mut h);
        r.map_err(|e| rt_throw(vm, e))
    };
    let ptr = {
        let mut h = VmHost::new(vm);
        h.skip_native = true;
        let r = Sxe::property_ptr(&st, &mut h, name.as_bytes());
        r.map_err(|e| rt_throw(vm, e))?
    };
    match ptr {
        PropertyPtr::Value(v) => Ok(v),
        PropertyPtr::Error => Ok(Value::Null),
        PropertyPtr::Fallback => trampoline(vm, &|h| {
            Sxe::read_property(&st, h, name.as_bytes(), FetchMode::Write)
        }),
    }
}

fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    let Some(st) = sxe_of(o) else {
        return Ok(false);
    };
    direct(vm, |h| Sxe::write_property(&st, h, name.as_bytes(), &v.deref()))?;
    Ok(true)
}

fn check_kind(check_empty: bool) -> CheckKind {
    if check_empty {
        CheckKind::NotEmpty
    } else {
        CheckKind::Isset
    }
}

fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    let kind = check_kind(check_empty);
    let has = direct(vm, |h| Sxe::has_property(&st, h, name.as_bytes(), kind))?;
    // The VM expects "is empty" (not PHP's "is not empty") back for
    // `empty()` on properties (unlike `has_dimension`).
    Ok(Some(if check_empty { !has } else { has }))
}

fn unset_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<bool> {
    let Some(st) = sxe_of(o) else {
        return Ok(false);
    };
    direct(vm, |h| Sxe::unset_property(&st, h, name.as_bytes()))?;
    Ok(true)
}

fn read_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<Option<Value>> {
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    // Compound assignments on a dimension (`$sxe['a'] += 1`) read it with
    // `BP_VAR_R` before writing (`zend_binary_assign_op_obj_dim`).
    let mode = match fetch_mode(vm) {
        FetchMode::ReadWrite => FetchMode::Read,
        m => m,
    };
    let key = k.deref();
    let v = direct(vm, |h| Sxe::read_dimension(&st, h, Some(&key), mode))?;
    if mode == FetchMode::Write && !matches!(v, Value::Object(_) | Value::Reference(_)) {
        // `zend_fetch_dimension_address` on an object in a write context.
        vm.raise(
            8,
            &format!(
                "Indirect modification of overloaded element of {} has no effect",
                php_vm::object::display_name(&o.class_name())
            ),
        )?;
    }
    Ok(Some(v))
}

fn write_dimension(vm: &mut Vm, o: &Obj, k: Option<&Value>, v: &Value) -> VmResult<bool> {
    let Some(st) = sxe_of(o) else {
        return Ok(false);
    };
    let key = k.map(Value::deref);
    direct(vm, |h| Sxe::write_dimension(&st, h, key.as_ref(), &v.deref()))?;
    Ok(true)
}

fn has_dimension(vm: &mut Vm, o: &Obj, k: &Value, check_empty: bool) -> VmResult<Option<bool>> {
    let Some(st) = sxe_of(o) else {
        return Ok(None);
    };
    let kind = check_kind(check_empty);
    let key = k.deref();
    direct(vm, |h| Sxe::has_dimension(&st, h, &key, kind)).map(Some)
}

fn unset_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<bool> {
    let Some(st) = sxe_of(o) else {
        return Ok(false);
    };
    let key = k.deref();
    direct(vm, |h| Sxe::unset_dimension(&st, h, &key))?;
    Ok(true)
}

/// `foreach` over a `SimpleXMLElement` (`php_sxe_iterator`).
struct SxeIter {
    sxe: SxeRef,
}

impl NativeIterator for SxeIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        direct(vm, |h| Sxe::rewind(&self.sxe, h))
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(Sxe::valid(&self.sxe))
    }

    fn current(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(Sxe::iter_current(&self.sxe).unwrap_or(Value::Null))
    }

    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        Ok(Sxe::iter_key(&self.sxe, &mut VmHost::new(vm)))
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        direct(vm, |h| Sxe::next(&self.sxe, h))
    }
}

fn get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    direct(vm, |_| Sxe::get_iterator(by_ref))?;
    let Some(sxe) = sxe_of(o) else {
        return Err(vm.throw("Error", "SimpleXMLElement is not properly initialized"));
    };
    Ok(Box::new(SxeIter { sxe }))
}
