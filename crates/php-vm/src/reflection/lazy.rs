//! Lazy objects (`Zend/zend_lazy_objects.c`) as exposed by
//! `ReflectionClass::newLazyGhost()` & co.

use super::rexc;
use crate::func::Args;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, Value};

/// Initializes an uninitialized lazy object; returns the object property
/// operations must act on.
pub(crate) fn force_init(vm: &mut Vm, o: &Obj) -> VmResult<Obj> {
    vm.lazy_object_for(o, None)
}

fn unsupported(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Lazy objects are not supported"))
}

pub(crate) fn rc_new_lazy_ghost(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rc_new_lazy_proxy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rc_reset_as_lazy_ghost(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rc_reset_as_lazy_proxy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rc_initialize_lazy_object(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(a.at(0))
}
pub(crate) fn rc_is_uninitialized_lazy_object(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(false))
}
pub(crate) fn rc_mark_lazy_object_as_initialized(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(a.at(0))
}
pub(crate) fn rc_get_lazy_initializer(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}
pub(crate) fn rp_set_raw_value_without_lazy_initialization(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rp_skip_lazy_initialization(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unsupported(vm, a)
}
pub(crate) fn rp_is_lazy(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(false))
}
