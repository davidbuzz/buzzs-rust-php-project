//! Object handlers of the SPL classes (`zend_object_handlers` overrides),
//! dispatched on the object's [`SplState`].
//!
//! Every function returns `Ok(None)` when the object has no SPL state or its
//! class does not override that handler; the VM then applies its standard
//! behaviour (for `ArrayAccess` objects: calling `offsetGet()` & co., which
//! [`std_read_dimension`] and friends also implement).

use crate::host::{ObjectIterator, SplHost};
use crate::object::{SplObject, SplState};
use crate::util::DimKind;
use php_runtime::{Obj, PropertyPurpose, RtResult, Value, ZArray, ZStr};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Fixed,
    Heap,
    DlList,
    Storage,
    Array,
    Dual,
    Recursive,
    File,
}

fn family(st: &SplObject) -> RtResult<Family> {
    st.with(|s| match s {
        SplState::FixedArray(_) => Family::Fixed,
        SplState::Heap(_) => Family::Heap,
        SplState::DlList(_) => Family::DlList,
        SplState::Storage(_) => Family::Storage,
        SplState::Array(_) => Family::Array,
        SplState::DualIt(_) => Family::Dual,
        SplState::RecursiveIt(_) => Family::Recursive,
        SplState::File(_) => Family::File,
    })
}

fn state(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Option<(Rc<SplObject>, Family)>> {
    match host.spl_state(obj) {
        Some(st) => {
            let f = family(&st)?;
            Ok(Some((st, f)))
        }
        None => Ok(None),
    }
}

/// `zend_std_read_dimension` for `ArrayAccess` objects.
pub fn std_read_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Value> {
    let off = offset.cloned().unwrap_or_default();
    if kind == DimKind::Is {
        let r = host.call_method(obj, "offsetExists", std::slice::from_ref(&off))?;
        if !r.to_bool(host)? {
            return Ok(Value::Null);
        }
    }
    host.call_method(obj, "offsetGet", &[off])
}

/// `zend_std_has_dimension` for `ArrayAccess` objects.
pub fn std_has_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    offset: &Value,
    check_empty: bool,
) -> RtResult<bool> {
    let r = host.call_method(obj, "offsetExists", std::slice::from_ref(offset))?;
    let exists = r.to_bool(host)?;
    if exists && check_empty {
        let v = host.call_method(obj, "offsetGet", std::slice::from_ref(offset))?;
        return v.to_bool(host);
    }
    Ok(exists)
}

/// `read_dimension`.
pub fn read_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Option<Value>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    Ok(Some(match fam {
        Family::Fixed => crate::fixedarray::read_dimension(host, obj, &st, offset, kind)?,
        Family::Storage => crate::observer::read_dimension(host, obj, &st, offset, kind)?,
        Family::Array => crate::array::read_dimension(host, obj, &st, offset, kind)?,
        _ => return Ok(None),
    }))
}

/// `write_dimension` (`offset == None` for `$o[] = $v`). Returns whether
/// the write was handled.
pub fn write_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<bool> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(false);
    };
    match fam {
        Family::Fixed => crate::fixedarray::write_dimension(host, obj, &st, offset, value)?,
        Family::Storage => crate::observer::write_dimension(host, obj, &st, offset, value)?,
        Family::Array => crate::array::write_dimension(host, obj, &st, offset, value)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// `has_dimension` (`isset()` / `empty()` with `check_empty`).
pub fn has_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    offset: &Value,
    check_empty: bool,
) -> RtResult<Option<bool>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    Ok(Some(match fam {
        Family::Fixed => crate::fixedarray::has_dimension(host, obj, &st, offset, check_empty)?,
        Family::Storage => crate::observer::has_dimension(host, obj, &st, offset, check_empty)?,
        Family::Array => crate::array::has_dimension(host, obj, &st, offset, check_empty)?,
        _ => return Ok(None),
    }))
}

/// `unset_dimension`. Returns whether it was handled.
pub fn unset_dimension(host: &mut dyn SplHost, obj: &Obj, offset: &Value) -> RtResult<bool> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(false);
    };
    match fam {
        Family::Fixed => crate::fixedarray::unset_dimension(host, obj, &st, offset)?,
        Family::Storage => crate::observer::unset_dimension(host, obj, &st, offset)?,
        Family::Array => crate::array::unset_dimension(host, obj, &st, offset)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// `count_elements` (`count($obj)`).
pub fn count_elements(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Option<i64>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    Ok(Some(match fam {
        Family::Fixed => crate::fixedarray::count_elements(host, obj, &st)?,
        Family::Heap => crate::heap::count_elements(host, obj, &st)?,
        Family::DlList => crate::dllist::count_elements(host, obj, &st)?,
        Family::Array => crate::array::count_elements(host, obj, &st)?,
        _ => return Ok(None),
    }))
}

/// `get_properties_for` / `get_properties`: the property table to use for
/// dumps, `(array)` casts and the like.
pub fn properties_for(
    host: &mut dyn SplHost,
    obj: &Obj,
    purpose: PropertyPurpose,
) -> RtResult<Option<ZArray>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    Ok(Some(match fam {
        Family::Fixed => crate::fixedarray::properties_for(host, obj, &st, purpose)?,
        Family::Array => match crate::array::properties_for(host, obj, &st, purpose)? {
            Some(t) => t,
            None => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

/// `get_debug_info` for classes that override it as a handler rather than
/// a `__debugInfo()` method (`ArrayObject`/`ArrayIterator`).
pub fn debug_info(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Option<ZArray>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    match fam {
        Family::Array => crate::array::debug_info(host, obj, &st).map(Some),
        _ => Ok(None),
    }
}

/// `read_property` (`ArrayObject::ARRAY_AS_PROPS`). `Ok(None)` means: use
/// the standard property lookup.
pub fn read_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    name: &ZStr,
    kind: DimKind,
) -> RtResult<Option<Value>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    match fam {
        Family::Array => crate::array::read_property(host, obj, &st, name, kind),
        _ => Ok(None),
    }
}

/// `write_property`; returns whether SPL handled the write.
pub fn write_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    name: &ZStr,
    value: Value,
) -> RtResult<bool> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(false);
    };
    match fam {
        Family::Array => crate::array::write_property(host, obj, &st, name, value),
        _ => Ok(false),
    }
}

/// `has_property` (`check`: 0 = `isset`, 1 = `empty` semantics (truthy),
/// 2 = exists). `Ok(None)` means: use the standard lookup.
pub fn has_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    name: &ZStr,
    check: i32,
) -> RtResult<Option<bool>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    match fam {
        Family::Array => crate::array::has_property(host, obj, &st, name, check),
        _ => Ok(None),
    }
}

/// `unset_property`; returns whether SPL handled it.
pub fn unset_property(host: &mut dyn SplHost, obj: &Obj, name: &ZStr) -> RtResult<bool> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(false);
    };
    match fam {
        Family::Array => crate::array::unset_property(host, obj, &st, name),
        _ => Ok(false),
    }
}

/// `compare` handler for two objects, at least one of which has SPL state.
pub fn compare(host: &mut dyn SplHost, a: &Obj, b: &Obj) -> RtResult<Option<i32>> {
    let fa = state(host, a)?.map(|s| s.1);
    let fb = state(host, b)?.map(|s| s.1);
    match (fa, fb) {
        (Some(Family::Storage), _) | (_, Some(Family::Storage)) => {
            if fa == fb {
                crate::observer::compare(host, a, b).map(Some)
            } else {
                Ok(Some(1))
            }
        }
        (Some(Family::Array), _) | (_, Some(Family::Array)) => {
            crate::array::compare(host, a, b).map(Some)
        }
        _ => Ok(None),
    }
}

enum Plan {
    Done(Box<SplState>),
    Storage,
    Array,
    Iterators,
    File,
}

/// `clone_obj`: the internal state for `new_obj`, a clone of `old`
/// (properties are copied by the VM).
pub fn clone_state(
    host: &mut dyn SplHost,
    old: &Obj,
    new_obj: &Obj,
) -> RtResult<Option<Rc<SplObject>>> {
    let Some(st) = host.spl_state(old) else {
        return Ok(None);
    };
    let base = st.base();
    let fresh = crate::object::create_state(base);
    let Some(fresh) = fresh else {
        return Ok(None);
    };
    let plan = st.with(|s| match s {
        SplState::FixedArray(f) => Plan::Done(Box::new(SplState::FixedArray(f.clone()))),
        SplState::Heap(h) => Plan::Done(Box::new(SplState::Heap(h.clone()))),
        SplState::DlList(d) => Plan::Done(Box::new(SplState::DlList(d.duplicate()))),
        SplState::Storage(_) => Plan::Storage,
        SplState::Array(_) => Plan::Array,
        SplState::DualIt(_) | SplState::RecursiveIt(_) => Plan::Iterators,
        SplState::File(_) => Plan::File,
    })?;
    match plan {
        Plan::Done(s) => {
            fresh.with(|f| *f = *s)?;
        }
        Plan::Storage => crate::observer::clone_into(host, new_obj, &fresh, &st)?,
        Plan::Array => crate::array::clone_into(host, old, new_obj, &fresh, &st)?,
        Plan::Iterators => {
            return Err(php_runtime::RuntimeError::Error(format!(
                "Trying to clone an uncloneable object of class {}",
                old.class_name()
            )))
        }
        Plan::File => crate::directory::clone_into(host, old, &fresh, &st)?,
    }
    Ok(Some(fresh))
}

/// The `get_method` check: must be called before dispatching an instance
/// method call (`$obj->m()`) on an object with SPL state. Fails for
/// `SplFileObject`/`GlobIterator` instances whose constructor did not run
/// ("The parent constructor was not called: the object is in an invalid
/// state").
pub fn before_method_call(host: &mut dyn SplHost, obj: &Obj) -> RtResult<()> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(());
    };
    if fam == Family::File {
        crate::directory::check_method_call(host, obj, &st)?;
    }
    Ok(())
}

/// `get_method` fallback: dual iterators and `RecursiveIteratorIterator`
/// forward calls of methods their class lacks to the inner (current
/// sub-) iterator. Returns the object to call the method on instead.
pub fn method_target(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Option<Obj>> {
    crate::iterators::method_target(host, obj)
}

/// `get_iterator` of the SPL classes that iterate natively (not through
/// their `Iterator` methods). `None` means: use the `Iterator` /
/// `IteratorAggregate` methods.
pub fn native_iterator(
    host: &mut dyn SplHost,
    obj: &Obj,
    by_ref: bool,
) -> RtResult<Option<Box<dyn ObjectIterator>>> {
    let Some((st, fam)) = state(host, obj)? else {
        return Ok(None);
    };
    // User iterator methods and `ArrayObject`'s configured iterator class
    // require method dispatch (`zend_implement_iterator` / aggregate).
    let method_dispatch = match fam {
        Family::Fixed | Family::Storage => host.has_user_method(obj, "getIterator"),
        Family::Array if host.instance_of(obj, "IteratorAggregate") => true,
        _ => ["rewind", "valid", "key", "current", "next"]
            .iter()
            .any(|m| host.has_user_method(obj, m)),
    };
    if method_dispatch {
        return Ok(None);
    }
    let it: Box<dyn ObjectIterator> = match fam {
        Family::Fixed | Family::Heap | Family::DlList | Family::Recursive if by_ref => {
            return Err(php_runtime::RuntimeError::Error(
                "An iterator cannot be used with foreach by reference".to_string(),
            ))
        }
        Family::Fixed => Box::new(crate::fixedarray::FixedArrayIter::new(obj.clone(), st)),
        Family::Heap => Box::new(crate::heap::HeapIter::new(obj.clone(), st)),
        Family::DlList => Box::new(crate::dllist::DlListIter::new(st)?),
        Family::Array => match crate::array::native_iterator(host, obj, &st, by_ref)? {
            Some(it) => it,
            None => return Ok(None),
        },
        Family::Recursive => {
            // RecursiveTreeIterator overrides key()/current() internally, so
            // zend_implement_iterator switched it to method iteration.
            if host.instance_of(obj, "RecursiveTreeIterator") {
                return Ok(None);
            }
            crate::iterators::check_recursive_initialized(&st)?;
            Box::new(crate::iterators::RecursiveItIter::new(obj.clone()))
        }
        Family::File => match crate::directory::native_iterator(host, obj, &st)? {
            Some(it) => it,
            None => return Ok(None),
        },
        Family::Storage | Family::Dual => return Ok(None),
    };
    Ok(Some(it))
}
