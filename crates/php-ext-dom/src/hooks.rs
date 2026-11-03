//! Object handlers of the DOM classes (`dom_object_handlers` and the
//! collection handlers of `php_dom.c`).

use php_dom::classes::{find_class, is_subclass, PropInfo};
use php_dom::dispatch::{self, DomObject, PropRead};
use php_dom::lists::Offset;
use php_dom::{DomValue, Node};
use php_runtime::{ArrayKey, HashTable, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{ClassHooks, NativeIterator, VmResult};
use php_vm::func::flags;
use php_vm::object::{class_of, data, display_name, internal, set_internal};
use php_vm::Vm;

use crate::errors::{check, Ctx};
use crate::run;
use crate::state::{bind_node_object, dom_object, to_dom, to_value, DomIter, DomState};

/// The built-in DOM class an object's class derives from.
pub(crate) fn builtin_of(o: &Obj) -> Option<&'static str> {
    let mut c = Some(class_of(o).clone());
    while let Some(cls) = c {
        let name = display_name(&cls.name).to_string_lossy();
        if let Some(ci) = find_class(&name) {
            if ci.kind == php_dom::classes::ClassKind::Class {
                return Some(ci.name);
            }
        }
        c = cls.parent.clone();
    }
    None
}

/// Whether a built-in class is a node class.
pub(crate) fn is_node_class(b: &str) -> bool {
    b == "DOMNameSpaceNode" || is_subclass(b, "DOMNode") || is_subclass(b, "Dom\\Node")
}

/// The context for errors raised on `o`.
pub(crate) fn obj_ctx(o: &Obj) -> Ctx {
    Ctx {
        class: display_name(&class_of(o).name).to_string_lossy(),
        ..Ctx::default()
    }
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::create`
fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let Some(b) = builtin_of(o) else {
        return Ok(());
    };
    let obj = if is_node_class(b) {
        let n = Node::new_unbound(display_name(&class_of(o).name).to_string_lossy(), b);
        bind_node_object(&n, o);
        Some(DomObject::Node(n))
    } else {
        dispatch::instantiate(b)
    };
    set_internal(
        o,
        DomState {
            obj,
            keep: Vec::new(),
        },
    );
    Ok(())
}

fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let Some(d) = dom_object(old) else {
        set_internal(new, DomState::default());
        return Ok(());
    };
    let ctx = obj_ctx(old);
    let r = run::lib(vm, || dispatch::clone_object(&d))?;
    let Some(c) = check(vm, r, Some(&ctx))? else {
        return Ok(());
    };
    if let DomObject::Node(n) = &c {
        n.set_class_name(display_name(&class_of(new).name).to_string_lossy());
        bind_node_object(n, new);
    }
    set_internal(
        new,
        DomState {
            obj: Some(c),
            keep: Vec::new(),
        },
    );
    Ok(())
}

fn name_str(name: &ZStr) -> String {
    name.to_string_lossy()
}

fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let Some(d) = dom_object(o) else {
        return Ok(None);
    };
    let n = name_str(name);
    if !dispatch::is_handled_property(&d, &n) {
        return Ok(None);
    }
    let ctx = obj_ctx(o);
    let r = run::lib(vm, || dispatch::read_property(&d, &n))?;
    match check(vm, r, Some(&ctx))? {
        Some(PropRead::Value(v)) => Ok(Some(to_value(vm, v)?)),
        Some(PropRead::Uninit) | None => Ok(Some(Value::Null)),
        Some(PropRead::NotHandled) => Ok(None),
    }
}

/// The declaration of a DOM property (searching the built-in ancestors).
fn find_prop(class: &str, name: &str) -> Option<(&'static str, &'static PropInfo)> {
    let mut cur = find_class(class);
    while let Some(c) = cur {
        if let Some(p) = c.properties.iter().find(|p| p.name == name) {
            return Some((c.name, p));
        }
        cur = c.parent.and_then(find_class);
    }
    None
}

/// `zend_zval_type_name`-style name for messages.
fn type_name(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => display_name(&class_of(o).name).to_string_lossy(),
        Value::Resource(_) => "resource".into(),
        Value::Reference(r) => type_name(&r.get()),
    }
}

/// `zend_verify_property_type` for the DOM property types.
fn coerce(vm: &mut Vm, v: &Value, ty: &str, decl: &str, name: &str) -> VmResult<Value> {
    let v = v.deref();
    let nullable = ty.starts_with('?') || ty.contains("null") || ty == "mixed";
    let base = ty.trim_start_matches('?');
    let fail = |vm: &mut Vm, v: &Value| {
        vm.throw(
            "TypeError",
            &format!(
                "Cannot assign {} to property {decl}::${name} of type {ty}",
                type_name(v)
            ),
        )
    };
    if v.is_null() {
        return if nullable { Ok(Value::Null) } else { Err(fail(vm, &v)) };
    }
    match base {
        "mixed" => Ok(v),
        "string" => match &v {
            Value::Array(_) | Value::Resource(_) => Err(fail(vm, &v)),
            Value::Object(o) => {
                if class_of(o).magic.to_string.is_some() {
                    Ok(Value::Str(vm.value_to_string(&v)?))
                } else {
                    Err(fail(vm, &v))
                }
            }
            _ => Ok(Value::Str(vm.value_to_string(&v)?)),
        },
        "bool" => match &v {
            Value::Array(_) | Value::Object(_) | Value::Resource(_) => Err(fail(vm, &v)),
            _ => Ok(Value::Bool(vm.value_to_bool(&v)?)),
        },
        "int" => match &v {
            Value::Int(_) => Ok(v),
            Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
            Value::Float(_) | Value::Str(_) => match vm.parse_long_weak(&v)? {
                Some(i) => Ok(Value::Int(i)),
                None => Err(fail(vm, &v)),
            },
            _ => Err(fail(vm, &v)),
        },
        cls => match &v {
            Value::Object(o) => {
                let ok = vm
                    .find_class(cls.as_bytes())
                    .is_some_and(|c| class_of(o).is_subclass_of(&c));
                if ok {
                    Ok(v)
                } else {
                    Err(fail(vm, &v))
                }
            }
            _ => Err(fail(vm, &v)),
        },
    }
}

fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    let Some(d) = dom_object(o) else {
        return Ok(false);
    };
    let n = name_str(name);
    if !dispatch::is_handled_property(&d, &n) {
        return Ok(false);
    }
    let builtin = d.builtin_class();
    let value = match find_prop(builtin, &n) {
        Some((_, p)) if p.set_visibility.is_some() || p.readonly => {
            // `private(set)`: the VM raises the visibility error.
            return Ok(false);
        }
        Some((decl, p)) => coerce(vm, v, p.ty.unwrap_or("mixed"), decl, &n)?,
        None => v.deref(),
    };
    let dv = to_dom(vm, &value);
    let ctx = obj_ctx(o);
    let r = run::lib(vm, || dispatch::write_property(&d, &n, &dv))?;
    check(vm, r, Some(&ctx))?;
    Ok(true)
}

fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    let Some(d) = dom_object(o) else {
        return Ok(None);
    };
    let n = name_str(name);
    if !dispatch::is_handled_property(&d, &n) {
        return Ok(None);
    }
    let ctx = obj_ctx(o);
    let r = run::lib(vm, || dispatch::read_property(&d, &n))?;
    let v = match check(vm, r, Some(&ctx))? {
        Some(PropRead::Value(v)) => to_value(vm, v)?,
        Some(PropRead::NotHandled) => return Ok(None),
        _ => {
            return Ok(Some(check_empty));
        }
    };
    if check_empty {
        Ok(Some(!vm.value_to_bool(&v)?))
    } else {
        Ok(Some(!v.is_null()))
    }
}

fn unset_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<bool> {
    let Some(d) = dom_object(o) else {
        return Ok(false);
    };
    let n = name_str(name);
    if !dispatch::is_handled_property(&d, &n) {
        return Ok(false);
    }
    Err(vm.throw(
        "Error",
        &format!(
            "Cannot unset {}::${n}",
            display_name(&class_of(o).name).to_string_lossy()
        ),
    ))
}

/// `"\0Class\0name"` / `"\0*\0name"` / `"name"`.
fn mangle(fl: u32, class: &ZStr, name: &ZStr) -> ZStr {
    let mut v = Vec::new();
    if fl & flags::PRIVATE != 0 {
        v.push(0);
        v.extend_from_slice(class.as_bytes());
        v.push(0);
    } else if fl & flags::PROTECTED != 0 {
        v.extend_from_slice(b"\0*\0");
    } else {
        return name.clone();
    }
    v.extend_from_slice(name.as_bytes());
    ZStr::from(v)
}

/// `zend_std_get_properties`: initialized declared properties followed by
/// dynamic ones.
fn std_properties(o: &Obj) -> HashTable {
    let cls = class_of(o).clone();
    let d = data(o);
    let mut t = HashTable::new();
    {
        let slots = d.slots.borrow();
        for pi in &cls.props {
            if pi.flags & flags::STATIC != 0 || pi.is_virtual() {
                continue;
            }
            if let Some(Some(v)) = slots.get(pi.slot as usize) {
                t.insert(ArrayKey::Str(mangle(pi.flags, &pi.class, &pi.name)), v.clone());
            }
        }
    }
    if let Some(dynp) = d.dynamic.borrow().as_ref() {
        for (k, v) in dynp {
            t.insert(k.clone(), v.clone());
        }
    }
    t
}

/// `dom_get_debug_info`.
fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let Some(d) = dom_object(o) else {
        return Ok(None);
    };
    let mut t = std_properties(o);
    let items = run::lib(vm, || dispatch::debug_info(&d))?;
    for (name, v) in items {
        let v = to_value(vm, v)?;
        t.insert(ArrayKey::Str(ZStr::from(name)), v);
    }
    Ok(Some(ZArray::from_table(t)))
}

fn to_offset(v: &Value) -> Offset {
    match v.deref() {
        Value::Int(i) => Offset::Int(i),
        Value::Float(f) => Offset::Float(f),
        Value::Str(s) => Offset::Str(s.as_bytes().to_vec()),
        Value::Null => Offset::Null,
        Value::Bool(b) => Offset::Bool(b),
        Value::Array(a) => Offset::Array(!a.is_empty()),
        other => Offset::Other(type_name(&other)),
    }
}

fn read_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<Option<Value>> {
    let Some(d) = dom_object(o) else {
        return Ok(None);
    };
    let off = to_offset(k);
    let ctx = obj_ctx(o);
    let r = run::lib(vm, || dispatch::offset_get(&d, &off))?;
    match check(vm, r, Some(&ctx))? {
        Some(v) => Ok(Some(to_value(vm, v)?)),
        None => Ok(Some(Value::Null)),
    }
}

fn has_dimension(vm: &mut Vm, o: &Obj, k: &Value, _check_empty: bool) -> VmResult<Option<bool>> {
    let Some(d) = dom_object(o) else {
        return Ok(None);
    };
    let off = to_offset(k);
    let ctx = obj_ctx(o);
    let r = run::lib(vm, || dispatch::offset_exists(&d, &off))?;
    Ok(Some(check(vm, r, Some(&ctx))?.unwrap_or(false)))
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::count`
fn count(_vm: &mut Vm, o: &Obj) -> VmResult<Option<i64>> {
    Ok(dom_object(o).and_then(|d| dispatch::count(&d)))
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::get_iterator`
fn get_iterator(_vm: &mut Vm, o: &Obj, _by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    Ok(Box::new(DomIter {
        source: dom_object(o),
        handle: None,
        started: false,
    }))
}

/// Handlers of every DOM class.
pub(crate) fn object_hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create),
        clone: Some(clone),
        properties_for: Some(properties_for),
        read_property: Some(read_property),
        write_property: Some(write_property),
        has_property: Some(has_property),
        unset_property: Some(unset_property),
        ..ClassHooks::default()
    }
}

/// Handlers of the collection classes (node lists, maps, token lists).
pub(crate) fn collection_hooks() -> ClassHooks {
    ClassHooks {
        read_dimension: Some(read_dimension),
        has_dimension: Some(has_dimension),
        count: Some(count),
        get_iterator: Some(get_iterator),
        ..object_hooks()
    }
}

/// Unused-value guard for `DomValue` in this module.
#[allow(dead_code)]
fn _unused(_: DomValue) {}

/// Keeps `internal` in use for future handlers.
#[allow(dead_code)]
fn _state(o: &Obj) -> bool {
    internal::<DomState, _>(o, |_| ()).is_some()
}
