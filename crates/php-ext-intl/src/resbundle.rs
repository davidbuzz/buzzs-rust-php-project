//! `ResourceBundle` and the `resourcebundle_*()` functions.

use php_intl::resourcebundle::{self as lib, Offset, ResourceBundle, ResourceValue};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, NativeIterator, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;

fn ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    if core::is_constructed::<ResourceBundle>(o) {
        return Err(vm.throw("Error", "ResourceBundle object is already constructed"));
    }
    let opt = |n: usize| a.opt(n).map(|v| if let Value::Str(z) = v { text(&z) } else { String::new() });
    let (loc, bundle) = (opt(0), opt(1));
    match ResourceBundle::new(loc.as_deref(), bundle.as_deref(), a.bool(2)) {
        Ok((rb, st)) => {
            core::set_data(o, rb);
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
    let o = core::instantiate(vm, "ResourceBundle")?;
    if ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

/// `resourcebundle_extract_value()`.
fn value(vm: &mut Vm, v: ResourceValue) -> VmResult<Value> {
    Ok(match v {
        ResourceValue::Str(t) => s(t),
        ResourceValue::Binary(b) => Value::Str(ZStr::from(b)),
        ResourceValue::Int(i) => Value::Int(i),
        ResourceValue::IntVector(v) => Value::Array(ZArray::from_values(v.into_iter().map(Value::Int))),
        ResourceValue::Bundle(rb) => {
            let o = core::instantiate(vm, "ResourceBundle")?;
            core::set_data(&o, rb);
            Value::Object(o)
        }
    })
}

/// `resource_bundle_array_fetch()`; `arg` is the offset's argument
/// number (`None` for `$rb[...]`).
fn fetch_elem(vm: &mut Vm, a: Option<&Args>, o: &Obj, off: &Value, fallback: bool, arg: Option<u32>) -> VmResult<Value> {
    core::reset_both(o);
    let key;
    let offset = match off {
        Value::Int(i) => Offset::Index(*i),
        Value::Str(z) => {
            key = z.clone();
            Offset::Key(key.as_bytes())
        }
        _ => Offset::Index(0),
    };
    let Some(r) = core::with::<ResourceBundle, _>(o, |rb| rb.get(offset, fallback, arg)) else {
        return Err(core::unconstructed(vm, "ResourceBundle"));
    };
    match r {
        Ok((v, st)) => {
            core::with_global(|g| g.code = st);
            core::with_obj_error(o, |e| e.code = st);
            value(vm, v)
        }
        Err(php_intl::Error::Intl(ie)) => {
            core::record(vm, Some(o), &ie)?;
            Ok(Value::Null)
        }
        Err(e) => match a {
            Some(a) => Err(core::throw_engine(vm, a, e, 0)),
            None => match e {
                php_intl::Error::Value(m) => Err(vm.throw("ValueError", &m)),
                other => Err(vm.throw("Error", &other.to_string())),
            },
        },
    }
}

fn get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = core::target(a);
    let idx = a.at(off);
    let arg = u32::try_from(off + 1).unwrap_or(1);
    fetch_elem(vm, Some(a), &o, &idx, a.bool(off + 1), Some(arg))
}

fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = core::target(a);
    core::reset_both(&o);
    match core::with::<ResourceBundle, _>(&o, |rb| rb.count()) {
        Some(n) => Ok(Value::Int(n)),
        None => Err(core::unconstructed(vm, "ResourceBundle")),
    }
}

fn get_locales(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::get_locales(&text(&a.str(0))) {
        Ok(v) => Ok(Value::Array(ZArray::from_values(v.into_iter().map(s)))),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

/// `foreach` over a bundle.
struct Iter {
    o: Obj,
    i: i64,
    len: i64,
    current: Option<(Option<String>, Value)>,
}

impl Iter {
    fn read(&mut self, vm: &mut Vm) -> VmResult<()> {
        if self.current.is_some() {
            return Ok(());
        }
        let i = self.i;
        if let Some((k, v)) = core::with::<ResourceBundle, _>(&self.o, |rb| rb.entry(i)).flatten() {
            let v = value(vm, v)?;
            self.current = Some((k, v));
        }
        Ok(())
    }
}

impl NativeIterator for Iter {
    fn rewind(&mut self, _vm: &mut Vm) -> VmResult<()> {
        self.i = 0;
        self.current = None;
        Ok(())
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(self.i < self.len)
    }

    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        self.read(vm)?;
        Ok(self.current.as_ref().map_or(Value::Null, |(_, v)| v.clone()))
    }

    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        self.read(vm)?;
        Ok(match self.current.as_ref() {
            Some((Some(k), _)) => s(k.clone()),
            _ => Value::Int(self.i),
        })
    }

    fn next(&mut self, _vm: &mut Vm) -> VmResult<()> {
        self.i += 1;
        self.current = None;
        Ok(())
    }
}

fn make_iter(o: &Obj) -> Iter {
    let len = core::with::<ResourceBundle, _>(o, |rb| rb.count()).unwrap_or(0);
    Iter { o: o.clone(), i: 0, len, current: None }
}

/// `get_iterator`.
pub(crate) fn get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    if by_ref {
        return Err(vm.throw("Error", "An iterator cannot be used with foreach by reference"));
    }
    if !core::is_constructed::<ResourceBundle>(o) {
        return Err(vm.throw("Error", "Found unconstructed ResourceBundle"));
    }
    Ok(Box::new(make_iter(o)))
}

/// An `IterSource` adapter so `getIterator()` can return an iterator object.
struct Source(Iter);

impl crate::iter::IterSource for Source {
    fn rewind(&mut self, vm: &mut Vm, _index: &mut i64) -> Option<VmResult<()>> {
        Some(NativeIterator::rewind(&mut self.0, vm))
    }

    fn valid(&self) -> bool {
        self.0.i < self.0.len
    }

    fn current(&self) -> Value {
        self.0.current.as_ref().map_or(Value::Null, |(_, v)| v.clone())
    }

    fn key(&self, _index: i64) -> Value {
        match self.0.current.as_ref() {
            Some((Some(k), _)) => s(k.clone()),
            _ => Value::Int(self.0.i),
        }
    }

    fn move_forward(&mut self, vm: &mut Vm, _index: &mut i64) -> VmResult<()> {
        self.0.i += 1;
        self.0.current = None;
        self.0.read(vm)
    }
}

fn get_iterator_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if !core::is_constructed::<ResourceBundle>(&o) {
        return Err(vm.throw("Error", "Found unconstructed ResourceBundle"));
    }
    let mut it = make_iter(&o);
    it.read(vm)?;
    Ok(Value::Object(crate::iter::wrap(vm, "IntlIterator", Box::new(Source(it)))?))
}

/// `read_dimension`.
pub(crate) fn read_dimension(vm: &mut Vm, o: &Obj, off: &Value) -> VmResult<Option<Value>> {
    match off.deref() {
        v @ (Value::Int(_) | Value::Str(_)) => fetch_elem(vm, None, o, &v, true, None).map(Some),
        other => {
            let t = core::type_name(&other);
            Err(vm.throw("TypeError", &format!("Cannot access offset of type {t} on ResourceBundle")))
        }
    }
}

/// `write_dimension`.
pub(crate) fn write_dimension(vm: &mut Vm, _o: &Obj, _k: Option<&Value>, _v: &Value) -> VmResult<bool> {
    Err(vm.throw("Error", "Cannot modify readonly ResourceBundle"))
}

/// `count_elements`.
pub(crate) fn count_elements(vm: &mut Vm, o: &Obj) -> VmResult<Option<i64>> {
    match core::with::<ResourceBundle, _>(o, |rb| rb.count()) {
        Some(n) => Ok(Some(n)),
        None => {
            core::errors_set(vm, o, php_intl::UErrorCode::ILLEGAL_ARGUMENT_ERROR, "Found unconstructed ResourceBundle")?;
            Ok(Some(0))
        }
    }
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("ResourceBundle::__construct", construct),
        ("ResourceBundle::create", create),
        ("ResourceBundle::get", get),
        ("resourcebundle_get", get),
        ("ResourceBundle::count", count),
        ("ResourceBundle::getLocales", get_locales),
        ("ResourceBundle::getErrorCode", core::get_error_code),
        ("ResourceBundle::getErrorMessage", core::get_error_message),
        ("ResourceBundle::getIterator", get_iterator_method),
    ]
}
