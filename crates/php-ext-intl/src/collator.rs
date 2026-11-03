//! `Collator` and the `collator_*()` functions.

use php_intl::collator::{Collator, SortValue};
use php_intl::{Error, UErrorCode};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;

/// The `__construct`/`create` body (`collator_ctor`).
fn ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    let loc = text(&a.str(0));
    if core::is_constructed::<Collator>(o) {
        return Err(vm.throw("Error", "Collator object is already constructed"));
    }
    match Collator::new(&loc) {
        Ok((c, st)) => {
            core::set_data(o, c);
            core::with_global(|g| g.code = st);
            core::with_obj_error(o, |e| e.code = st);
            Ok(true)
        }
        Err(e) => {
            core::fail(vm, a, Some(o), e, 0)?;
            Ok(false)
        }
    }
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::in_ctor(|| ctor(vm, a, &o))?;
    Ok(Value::Null)
}

fn create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = core::instantiate(vm, "Collator")?;
    if ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

/// Fetches the object, resets the errors and checks initialisation
/// (`collator_check_initialized`).
fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<Collator>(&o) {
        core::record(
            vm,
            Some(&o),
            &php_intl::IntlError::new(UErrorCode::ZERO, "Object not initialized")
                .with_scope(php_intl::Scope::ObjectCode),
        )?;
        return Err(vm.throw("Error", "Object not initialized"));
    }
    Ok((o, off))
}

/// Runs `f` on the collator; a library error becomes the intl error and
/// `false`.
fn run<T>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut Collator) -> php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    match core::with::<Collator, _>(&o, f) {
        Some(Ok(v)) => Ok(ok(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn compare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let (x, y) = (a.str(off), a.str(off + 1));
    run(vm, a, |c| c.compare(x.as_bytes(), y.as_bytes()), Value::Int)
}

fn get_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let attr = a.int(off);
    run(vm, a, |c| c.get_attribute(attr), Value::Int)
}

fn set_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let (attr, val) = (a.int(off), a.int(off + 1));
    run(vm, a, |c| c.set_attribute(attr, val), |()| Value::Bool(true))
}

fn get_strength(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |c| Ok(c.strength()), Value::Int)
}

fn set_strength(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let st = a.int(off);
    run(
        vm,
        a,
        |c| {
            c.set_strength(st);
            Ok(())
        },
        |()| Value::Bool(true),
    )
}

fn get_locale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let ty = a.int(off);
    run(vm, a, |c| c.locale(ty), s)
}

fn get_sort_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let x = a.str(off);
    run(vm, a, |c| c.sort_key(x.as_bytes()), |k| k.map_or(Value::Bool(false), |k| Value::Str(ZStr::from(k))))
}

/// A sort operand with owned string data.
enum Owned {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(ZStr),
    Object(Option<ZStr>),
    Array,
}

pub(crate) fn has_to_string(o: &Obj) -> bool {
    php_vm::object::class_of(o).methods.contains_key(&ZStr::from("__tostring"))
}

fn owned(vm: &mut Vm, v: &Value) -> VmResult<Owned> {
    Ok(match v.deref() {
        Value::Null => Owned::Null,
        Value::Bool(b) => Owned::Bool(b),
        Value::Int(i) => Owned::Int(i),
        Value::Float(f) => Owned::Float(f),
        Value::Str(z) => Owned::Str(z),
        Value::Object(o) => {
            if has_to_string(&o) {
                let z = vm.value_to_string(&Value::Object(o))?;
                Owned::Object(Some(z))
            } else {
                Owned::Object(None)
            }
        }
        Value::Array(_) | Value::Resource(_) | Value::Reference(_) => Owned::Array,
    })
}

fn view(o: &Owned) -> SortValue<'_> {
    match o {
        Owned::Null => SortValue::Null,
        Owned::Bool(b) => SortValue::Bool(*b),
        Owned::Int(i) => SortValue::Int(*i),
        Owned::Float(f) => SortValue::Float(*f),
        Owned::Str(z) => SortValue::Str(z.as_bytes()),
        Owned::Object(z) => SortValue::Object { as_string: z.as_ref().map(ZStr::as_bytes) },
        Owned::Array => SortValue::Array,
    }
}

fn sort_impl(vm: &mut Vm, a: &Args, renumber: bool, with_keys: bool) -> VmResult<Value> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<Collator>(&o) {
        if with_keys {
            fetch(vm, a)?;
        }
        return Err(vm.throw("Error", "Object not initialized"));
    }
    let arr = match a.at(off) {
        Value::Array(arr) => arr,
        _ => ZArray::new(),
    };
    let flags = if with_keys { 0 } else { a.int(off + 1) };
    let entries: Vec<(php_runtime::ArrayKey, Value)> = arr.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut ops = Vec::with_capacity(entries.len());
    for (_, v) in &entries {
        ops.push(owned(vm, v)?);
    }
    let views: Vec<SortValue<'_>> = ops.iter().map(view).collect();
    let r = core::with::<Collator, _>(&o, |c| {
        if with_keys {
            c.sort_with_sort_keys_order(&views)
        } else {
            c.sort_order(&views, flags)
        }
    });
    let order = match r {
        Some(Ok(order)) => order,
        Some(Err(e)) => {
            if with_keys {
                return core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false));
            }
            return report_sort_error(vm, a, &o, e, off);
        }
        None => return Ok(Value::Bool(false)),
    };
    let mut out = ZArray::with_capacity(order.len());
    for i in order {
        let (k, v) = &entries[i];
        if renumber {
            let _ = out.make_mut().push(v.clone());
        } else {
            out.make_mut().insert(k.clone(), v.clone());
        }
    }
    a.set_ref(off, Value::Array(out));
    Ok(Value::Bool(true))
}

/// `collator_report_sort_error()`.
fn report_sort_error(vm: &mut Vm, a: &Args, o: &Obj, e: Error, off: usize) -> VmResult<Value> {
    core::fail_with(vm, a, Some(o), e, off, Value::Bool(false))
}

fn sort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sort_impl(vm, a, true, false)
}

fn asort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sort_impl(vm, a, false, false)
}

fn sort_with_sort_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sort_impl(vm, a, true, true)
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("Collator::__construct", construct),
        ("Collator::create", create),
        ("Collator::compare", compare),
        ("Collator::sort", sort),
        ("Collator::sortWithSortKeys", sort_with_sort_keys),
        ("Collator::asort", asort),
        ("Collator::getAttribute", get_attribute),
        ("Collator::setAttribute", set_attribute),
        ("Collator::getStrength", get_strength),
        ("Collator::setStrength", set_strength),
        ("Collator::getLocale", get_locale),
        ("Collator::getErrorCode", core::get_error_code),
        ("Collator::getErrorMessage", core::get_error_message),
        ("Collator::getSortKey", get_sort_key),
    ]
}
