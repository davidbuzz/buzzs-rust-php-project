//! `IntlIterator`: wraps a native element source (string enumerations,
//! break iterator boundaries and parts) as a PHP `Iterator`.

use php_runtime::{Obj, Value, ZStr};
use php_vm::ext::{Args, NativeFn, NativeIterator, VmResult};
use php_vm::Vm;

use crate::core;

/// A native element source (`zend_object_iterator` funcs).
pub(crate) trait IterSource {
    /// Moves to the first element (`None` → rewind unsupported).
    fn rewind(&mut self, vm: &mut Vm) -> Option<VmResult<()>>;
    /// Whether there is a current element.
    fn valid(&self) -> bool;
    /// The current element (`null` when invalid).
    fn current(&self) -> Value;
    /// The current key, or `None` for the iteration index.
    fn key(&self) -> Option<Value> {
        None
    }
    /// Advances.
    fn move_forward(&mut self, vm: &mut Vm) -> VmResult<()>;
}

/// The payload of an `IntlIterator`.
pub(crate) struct IterState {
    /// The source.
    pub src: Box<dyn IterSource>,
    /// `zend_object_iterator::index`.
    pub index: i64,
}

/// A string enumeration (a list fetched up front).
pub(crate) struct StringList {
    items: Vec<String>,
    pos: usize,
    started: bool,
}

impl StringList {
    /// Wraps a list.
    pub(crate) fn new(items: Vec<String>) -> Self {
        Self { items, pos: 0, started: false }
    }
}

impl IterSource for StringList {
    fn rewind(&mut self, _vm: &mut Vm) -> Option<VmResult<()>> {
        self.pos = 0;
        self.started = true;
        Some(Ok(()))
    }

    fn valid(&self) -> bool {
        self.started && self.pos < self.items.len()
    }

    fn current(&self) -> Value {
        if self.valid() {
            Value::Str(ZStr::from(self.items[self.pos].as_str()))
        } else {
            Value::Null
        }
    }

    fn move_forward(&mut self, _vm: &mut Vm) -> VmResult<()> {
        if self.started {
            self.pos += 1;
        } else {
            // `next()` before any rewind fetches the first element.
            self.started = true;
        }
        Ok(())
    }
}

/// Creates an `IntlIterator` (or subclass) object around `src`.
pub(crate) fn wrap(vm: &mut Vm, class: &str, src: Box<dyn IterSource>) -> VmResult<Obj> {
    let o = core::instantiate(vm, class)?;
    core::set_data(&o, IterState { src, index: 0 });
    Ok(o)
}

/// Creates an `IntlIterator` over strings (`IntlIterator_from_StringEnumeration`).
pub(crate) fn from_strings(vm: &mut Vm, items: Vec<String>) -> VmResult<Value> {
    Ok(Value::Object(wrap(vm, "IntlIterator", Box::new(StringList::new(items)))?))
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    let o = a.this_obj();
    core::reset_both(&o);
    if !core::is_constructed::<IterState>(&o) {
        return Err(core::unconstructed(vm, "IntlIterator"));
    }
    Ok(o)
}

fn current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    Ok(core::with::<IterState, _>(&o, |s| s.src.current()).unwrap_or_default())
}

fn key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    Ok(core::with::<IterState, _>(&o, |s| s.src.key().unwrap_or(Value::Int(s.index))).unwrap_or_default())
}

fn next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    core::with::<IterState, _>(&o, |s| {
        s.index += 1;
        s.src.move_forward(vm)
    })
    .unwrap_or(Ok(()))?;
    Ok(Value::Null)
}

fn rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let r = core::with::<IterState, _>(&o, |s| s.src.rewind(vm)).flatten();
    match r {
        Some(r) => r?,
        None => core::errors_set(vm, &o, php_intl::UErrorCode::UNSUPPORTED_ERROR, "rewind not supported")?,
    }
    Ok(Value::Null)
}

fn valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    Ok(Value::Bool(core::with::<IterState, _>(&o, |s| s.src.valid()).unwrap_or(false)))
}

/// `foreach` over an `IntlIterator`: drives the source directly.
struct Foreach(Obj);

impl NativeIterator for Foreach {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        core::with::<IterState, _>(&self.0, |s| {
            s.index = 0;
            s.src.rewind(vm).unwrap_or(Ok(()))
        })
        .unwrap_or(Ok(()))
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(core::with::<IterState, _>(&self.0, |s| s.src.valid()).unwrap_or(false))
    }

    fn current(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(core::with::<IterState, _>(&self.0, |s| s.src.current()).unwrap_or_default())
    }

    fn key(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(core::with::<IterState, _>(&self.0, |s| s.src.key().unwrap_or(Value::Int(s.index))).unwrap_or_default())
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        core::with::<IterState, _>(&self.0, |s| {
            s.index += 1;
            s.src.move_forward(vm)
        })
        .unwrap_or(Ok(()))
    }
}

/// The `get_iterator` hook of `IntlIterator`.
pub(crate) fn get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    if by_ref {
        return Err(vm.throw("Exception", "Iteration by reference is not supported"));
    }
    if !core::is_constructed::<IterState>(o) {
        return Err(vm.throw("Exception", "The IntlIterator is not properly constructed"));
    }
    Ok(Box::new(Foreach(o.clone())))
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("IntlIterator::current", current),
        ("IntlIterator::key", key),
        ("IntlIterator::next", next),
        ("IntlIterator::rewind", rewind),
        ("IntlIterator::valid", valid),
    ]
}
