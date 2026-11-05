//! The SPL object handlers ([`php_spl::handlers`]) as VM [`ClassHooks`].

use crate::host::spl_state_of;
use crate::iter::{with_host, SplNativeIter};
use php_runtime::numeric::handle_numeric_str;
use php_runtime::{ArrayKey, HashTable, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_spl::handlers as h;
use php_spl::DimKind;
use php_vm::class::Class;
use php_vm::ext::{ClassHooks, DimensionCtx, NativeIterator, PropCtx, VmResult};
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
        get_method: Some(get_method),
        ..ClassHooks::default()
    }
}

/// The hooks of the classes whose objects own a directory or file stream
/// (`DirectoryIterator`, `SplFileObject` and their subclasses): the SPL
/// handlers plus `dtor_obj`, which closes the stream.
#[must_use]
pub fn filesystem_class_hooks() -> ClassHooks {
    ClassHooks {
        destruct: Some(filesystem_destroy),
        ..class_hooks()
    }
}

/// `spl_filesystem_object_destroy_object`.
fn filesystem_destroy(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    match spl_state_of(o) {
        Some(st) => crate::files::destroy(vm, &st),
        None => Ok(()),
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

/// `get_gc` of the stateful SPL classes (`spl_array_get_gc`,
/// `spl_object_storage_get_gc`, `spl_fixedarray_get_gc`, ...): reports the
/// values the state owns to the cycle collector. State shared with a live
/// native iterator is not reported (the iterator co-owns it).
fn gc_traverse(st: &Rc<php_spl::SplObject>, v: &mut php_vm::gc::GcVisitor<'_>) {
    if Rc::strong_count(st) != 1 {
        return;
    }
    let r = st.with(|s| match s {
        php_spl::SplState::Array(a) => v.value(&a.array),
        php_spl::SplState::Storage(s) => {
            // `spl_object_storage_get_gc` reports each element's object and
            // information directly: the `[object, info]` pair arrays of this
            // representation are not garbage PHP counts (bug #53071). A
            // pair shared with user code stays a node of its own.
            for e in s.storage.values() {
                match e {
                    Value::Array(pair) if pair.refcount() == 1 => v.table(pair),
                    other => v.value(other),
                }
            }
        }
        php_spl::SplState::FixedArray(f) => v.values(&f.elements),
        php_spl::SplState::DualIt(d) => dual_it_gc(d, v),
        _ => {}
    });
    if r.is_err() {
        v.opaque();
    }
}

/// `spl_dual_it_get_gc`: the inner object and its iterator, the current
/// key/value and the variant's own values (the `CallbackFilterIterator`
/// callback, the caching iterator's cache, the append iterator's
/// `ArrayIterator`), so that cycles through a callback's bound `$this` are
/// collectable (GH-7958).
fn dual_it_gc(d: &php_spl::iterators::DualIt, v: &mut php_vm::gc::GcVisitor<'_>) {
    use php_spl::iterators::DualKind;
    if let Some(o) = &d.inner {
        v.obj(o);
    }
    if let Some(it) = &d.inner_it {
        method_iterator_gc(it, v);
    }
    for x in [&d.data, &d.key].into_iter().flatten() {
        v.value(x);
    }
    match &d.kind {
        DualKind::Caching {
            children, cache, ..
        } => {
            if let Some(c) = children {
                v.value(c);
            }
            v.array(cache);
        }
        DualKind::Append { array_it, it } => {
            v.obj(array_it);
            method_iterator_gc(it, v);
        }
        DualKind::CallbackFilter { callback } => v.value(callback),
        _ => {}
    }
}

/// The object a `zend_user_iterator` (`MethodIterator`) owns, when this
/// state holds the only handle to the iterator. Native iterators over SPL
/// state are opaque (omitting them only keeps their targets alive).
fn method_iterator_gc(it: &php_spl::iterators::ItRef, v: &mut php_vm::gc::GcVisitor<'_>) {
    if Rc::strong_count(it) != 1 {
        return;
    }
    if let Ok(b) = it.try_borrow() {
        if let Some(o) = php_spl::ObjectIterator::method_object(&**b) {
            v.obj(o);
        }
    }
}

/// Registers [`gc_traverse`] for the SPL state type.
pub fn register_gc() {
    php_vm::gc::register_traverse_with::<Rc<php_spl::SplObject>>(gc_traverse);
}

/// `create_object`: attaches the state of the nearest stateful SPL
/// ancestor.
#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::create`
fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    if let Some(st) = stateful_base(php_vm::object::class_of(o)).and_then(php_spl::create_state) {
        crate::files::track(&st);
        php_vm::object::set_internal(o, st);
    }
    Ok(())
}

fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let st = with_host(vm, |host| h::clone_state(host, old, new))?;
    if let Some(st) = st {
        crate::files::track(&st);
        php_vm::object::set_internal(new, st);
    }
    Ok(())
}

fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    // php-src: only DatePeriod answers ZEND_PROP_PURPOSE_GET_OBJECT_VARS;
    // everything else falls back to `get_properties`.
    if purpose == PropertyPurpose::GetObjectVars {
        return Ok(None);
    }
    if spl_state_of(o).is_none() {
        return Ok(None);
    }
    if purpose == PropertyPurpose::Debug && php_vm::object::class_of(o).magic.debug_info.is_some() {
        // The VM calls `__debugInfo()` (native or a user override).
        return Ok(None);
    }
    let table = with_host(vm, |host| {
        if purpose == PropertyPurpose::Debug {
            if let Some(t) = h::debug_info(host, o)? {
                return Ok(Some(t));
            }
        }
        h::properties_for(host, o, purpose)
    })?;
    // `(array)` builds a symbol table: integer-like property names become
    // integer keys (and replace elements with the same index).
    Ok(match (purpose, table) {
        (PropertyPurpose::ArrayCast, Some(t)) => Some(canonical_keys(&t)),
        (_, t) => t,
    })
}

/// Rebuilds a table with integer-like string keys turned into integers.
fn canonical_keys(t: &ZArray) -> ZArray {
    if !t
        .keys()
        .any(|k| matches!(k, ArrayKey::Str(s) if handle_numeric_str(s.as_bytes()).is_some()))
    {
        return t.clone();
    }
    let mut out = HashTable::new();
    for (k, v) in t.iter() {
        let key = match k {
            ArrayKey::Str(s) => {
                handle_numeric_str(s.as_bytes()).map_or_else(|| k.clone(), ArrayKey::Int)
            }
            ArrayKey::Int(_) => k.clone(),
        };
        out.insert(key, v.clone());
    }
    ZArray::from_table(out)
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
    let kind = match vm.property_context() {
        PropCtx::Read | PropCtx::Isset | PropCtx::Exists => DimKind::Read,
        PropCtx::Write | PropCtx::Ref => DimKind::Write,
        PropCtx::ReadWrite => DimKind::ReadWrite,
        PropCtx::Unset => DimKind::Unset,
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
    // 0 = isset, 1 = empty (truthy), 2 = exists (`property_exists()`).
    let check = if vm.property_context() == PropCtx::Exists {
        2
    } else {
        i32::from(check_empty)
    };
    with_host(vm, |host| h::has_property(host, o, name, check))
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
    // The VM checks `has_dimension` before an `Is` read itself, so that
    // read is a plain one here (no second `offsetExists()` call).
    let (kind, offset) = match vm.dimension_context() {
        DimensionCtx::Read | DimensionCtx::Is => (DimKind::Read, Some(k)),
        DimensionCtx::Write => (DimKind::Write, Some(k)),
        DimensionCtx::ReadWrite => (DimKind::ReadWrite, Some(k)),
        DimensionCtx::Unset => (DimKind::Unset, Some(k)),
        DimensionCtx::Append => (DimKind::Write, None),
    };
    let v = with_host(vm, |host| h::read_dimension(host, o, offset, kind))?;
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
    // The VM hands a reference to `write_dimension` only for
    // `$obj[$k] = &$v`.
    if let Value::Reference(r) = v {
        // `$obj[$k] = &$v`: `ArrayObject`/`ArrayIterator` fetch the element
        // as a reference and bind it; elsewhere FETCH_DIM_W yields a
        // temporary, then ASSIGN_REF fails.
        if let Some(k) = k {
            if with_host(vm, |host| h::assign_ref_dimension(host, o, k, r))? {
                return Ok(true);
            }
        }
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
    if by_ref && php_vm::object::class_of(o).instance_of_name(b"directoryiterator") {
        // `spl_filesystem_dir_get_iterator` / `spl_filesystem_tree_get_iterator`.
        return Err(vm.throw(
            "Error",
            "An iterator cannot be used with foreach by reference",
        ));
    }
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

/// `get_method` of the SPL classes that refuse calls on objects whose
/// constructor did not run: `spl_filesystem_object_get_method_check`
/// (`SplFileObject`, `SplTempFileObject`, `GlobIterator`: "The parent
/// constructor was not called") and `spl_recursive_it_get_method`
/// (`RecursiveIteratorIterator`: "The %s instance wasn't initialized
/// properly"). Applies to user methods of subclasses too.
fn get_method(vm: &mut Vm, o: &Obj, _name: &ZStr) -> VmResult<()> {
    if spl_state_of(o).is_none() {
        return Ok(());
    }
    let recursive = php_vm::object::class_of(o).instance_of_name(b"recursiveiteratoriterator");
    with_host(vm, |host| {
        h::before_method_call(host, o)?;
        if recursive {
            h::method_target(host, o)?;
        }
        Ok(())
    })
}
