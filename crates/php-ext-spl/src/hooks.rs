//! The SPL object handlers ([`php_spl::handlers`]) as VM [`ClassHooks`].

use crate::host::spl_state_of;
use crate::iter::{with_host, SplNativeIter};
use php_runtime::numeric::handle_numeric_str;
use php_runtime::{ArrayKey, HashTable, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_spl::handlers as h;
use php_spl::DimKind;
use php_vm::class::Class;
use php_vm::ext::{ClassHooks, NativeIterator, VmResult};
use php_vm::Vm;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The hooks installed on every SPL class (inherited by subclasses).
#[must_use]
pub fn class_hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create),
        clone: Some(clone),
        properties_for: Some(properties_for),
        compare: Some(compare),
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
        unserialize: Some(unserialize),
        ..ClassHooks::default()
    }
}

thread_local! {
    /// The stateful SPL base of each class seen by [`create`], keyed by
    /// the class address (classes live for the whole request).
    static BASES: RefCell<HashMap<usize, Option<&'static str>>> = RefCell::new(HashMap::new());
}

/// The nearest stateful SPL ancestor of `cls` (or `cls` itself).
fn stateful_base(cls: &Rc<Class>) -> Option<&'static str> {
    let key = Rc::as_ptr(cls) as usize;
    if let Some(b) = BASES.with(|m| m.borrow().get(&key).copied()) {
        return b;
    }
    let base = std::iter::successors(Some(cls.clone()), |c| c.parent.clone())
        .find_map(|c| php_spl::object::canonical_class(&c.name.to_string_lossy()));
    BASES.with(|m| m.borrow_mut().insert(key, base));
    base
}

/// `create_object`: attaches the state of the nearest stateful SPL
/// ancestor.
#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::create`
fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    if let Some(st) = stateful_base(php_vm::object::class_of(o)).and_then(php_spl::create_state) {
        php_vm::object::set_internal(o, st);
    }
    Ok(())
}

fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let st = with_host(vm, |host| h::clone_state(host, old, new))?;
    if let Some(st) = st {
        php_vm::object::set_internal(new, st);
    }
    Ok(())
}

fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    if purpose == PropertyPurpose::Debug && php_vm::object::class_of(o).magic.debug_info.is_some() {
        // The VM calls `__debugInfo()` (native or a user override).
        return Ok(None);
    }
    with_host(vm, |host| {
        if purpose == PropertyPurpose::Debug {
            if let Some(t) = h::debug_info(host, o)? {
                return Ok(Some(t));
            }
        }
        h::properties_for(host, o, purpose)
    })
}

fn compare(vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a.deref(), b.deref()) else {
        return Ok(None);
    };
    if spl_state_of(&x).is_none() && spl_state_of(&y).is_none() {
        return Ok(None);
    }
    with_host(vm, |host| h::compare(host, &x, &y))
}

fn count(vm: &mut Vm, o: &Obj) -> VmResult<Option<i64>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    with_host(vm, |host| h::count_elements(host, o))
}

fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    let kind = match crate::dispatch::current_dim_kind(vm) {
        DimKind::Read => DimKind::Read,
        _ => DimKind::Write,
    };
    with_host(vm, |host| h::read_property(host, o, name, kind))
}

fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    if spl_state_of(o).is_none() {
        return Ok(false);
    }
    with_host(vm, |host| h::write_property(host, o, name, v.clone()))
}

fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    with_host(vm, |host| {
        h::has_property(host, o, name, i32::from(check_empty))
    })
}

fn unset_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<bool> {
    if spl_state_of(o).is_none() {
        return Ok(false);
    }
    with_host(vm, |host| h::unset_property(host, o, name))
}

fn read_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<Option<Value>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    let kind = crate::dispatch::current_dim_kind(vm);
    let v = with_host(vm, |host| h::read_dimension(host, o, Some(k), kind))?;
    if let Some(v) = &v {
        if kind != DimKind::Read && !matches!(v, Value::Reference(_) | Value::Object(_)) {
            // `zend_fetch_dimension_address` on an object in a write
            // context.
            vm.raise(
                8,
                &format!(
                    "Indirect modification of overloaded element of {} has no effect",
                    o.class_name().to_string_lossy()
                ),
            )?;
        }
    }
    Ok(v)
}

fn write_dimension(vm: &mut Vm, o: &Obj, k: Option<&Value>, v: &Value) -> VmResult<bool> {
    if spl_state_of(o).is_none() {
        return Ok(false);
    }
    if matches!(v, Value::Reference(_))
        && crate::dispatch::current_dim_kind(vm) == DimKind::Write
        && crate::dispatch::current_op_is_assign_ref(vm)
    {
        // `$obj[$k] = &$v`: FETCH_DIM_W yields a temporary, then
        // ASSIGN_REF fails.
        vm.raise(
            8,
            &format!(
                "Indirect modification of overloaded element of {} has no effect",
                o.class_name().to_string_lossy()
            ),
        )?;
        return Err(vm.throw(
            "Error",
            "Cannot assign by reference to an array dimension of an object",
        ));
    }
    with_host(vm, |host| h::write_dimension(host, o, k, v.clone()))
}

fn has_dimension(vm: &mut Vm, o: &Obj, k: &Value, check_empty: bool) -> VmResult<Option<bool>> {
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    with_host(vm, |host| h::has_dimension(host, o, k, check_empty))
}

fn unset_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<bool> {
    if spl_state_of(o).is_none() {
        return Ok(false);
    }
    with_host(vm, |host| h::unset_dimension(host, o, k))
}

fn get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    let it = with_host(vm, |host| php_spl::iter::get_iterator(host, o, by_ref))?;
    Ok(Box::new(SplNativeIter::new(it)))
}

/// Calls `__unserialize()` with the data array in canonical form: the
/// runtime's object parser keeps integer-like keys as strings, while PHP
/// hands `__unserialize()` an ordinary array (`zend_symtable_update`).
#[allow(clippy::needless_pass_by_value)] // signature fixed by `ClassHooks::unserialize`
fn unserialize(vm: &mut Vm, o: &Obj, data: ZArray) -> VmResult<bool> {
    let cls = php_vm::object::class_of(o).clone();
    if cls.magic.unserialize.is_none() {
        return Ok(false);
    }
    let mut t = HashTable::new();
    for (k, v) in data.iter() {
        let key = match k {
            ArrayKey::Str(s) => match handle_numeric_str(s.as_bytes()) {
                Some(n) => ArrayKey::Int(n),
                None => k.clone(),
            },
            ArrayKey::Int(_) => k.clone(),
        };
        t.insert(key, v.clone());
    }
    vm.call_method_by_name(
        o,
        "__unserialize",
        vec![Value::Array(ZArray::from_table(t))],
    )?;
    Ok(true)
}
