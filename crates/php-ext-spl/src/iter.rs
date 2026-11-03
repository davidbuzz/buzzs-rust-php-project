//! Bridges between the two iterator protocols: SPL's
//! [`ObjectIterator`] (driven through an [`SplHost`]) and the VM's
//! [`NativeIterator`] (driven through the [`Vm`]), plus the engine's
//! `InternalIterator` class (`zend_create_internal_iterator_zval`).

use crate::host::{rt_throw, VmHost};
use php_runtime::{RtResult, Value};
use php_spl::ObjectIterator;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, NativeIterator, VmResult};
use php_vm::Vm;

/// Runs `f` with a [`VmHost`] over `vm`, converting the error.
pub fn with_host<T>(
    vm: &mut Vm,
    f: impl FnOnce(&mut VmHost<'_>) -> RtResult<T>,
) -> VmResult<T> {
    let r = {
        let mut host = VmHost::new(vm);
        f(&mut host)
    };
    r.map_err(|e| rt_throw(vm, e))
}

/// An SPL [`ObjectIterator`] seen as a VM [`NativeIterator`] (`foreach`).
pub struct SplNativeIter {
    it: Box<dyn ObjectIterator>,
    index: i64,
}

impl std::fmt::Debug for SplNativeIter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SplNativeIter({})", self.index)
    }
}

impl SplNativeIter {
    /// Wraps an SPL iterator.
    #[must_use]
    pub fn new(it: Box<dyn ObjectIterator>) -> Self {
        SplNativeIter { it, index: 0 }
    }
}

impl NativeIterator for SplNativeIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.index = 0;
        with_host(vm, |h| self.it.rewind(h))
    }
    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool> {
        with_host(vm, |h| self.it.valid(h))
    }
    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        with_host(vm, |h| self.it.current(h)).map(Option::unwrap_or_default)
    }
    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        let k = with_host(vm, |h| self.it.key(h))?;
        Ok(k.unwrap_or(Value::Int(self.index)))
    }
    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.index += 1;
        with_host(vm, |h| self.it.next(h))
    }
}

/// The state of an `InternalIterator` object.
struct InternalIt {
    it: Option<Box<dyn ObjectIterator>>,
    rewound: bool,
}

/// Creates an `InternalIterator` wrapping `it`.
pub fn new_internal_iterator(vm: &mut Vm, it: Box<dyn ObjectIterator>) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"InternalIterator") else {
        return Err(vm.throw("Error", "Class \"InternalIterator\" not found"));
    };
    let o = vm.new_object(&cls)?;
    php_vm::object::set_internal(
        &o,
        InternalIt {
            it: Some(it),
            rewound: false,
        },
    );
    Ok(Value::Object(o))
}

#[derive(Clone, Copy)]
enum Op {
    Current,
    Key,
    Next,
    Valid,
    Rewind,
}

fn internal_op(vm: &mut Vm, a: &Args, op: Op) -> VmResult<Value> {
    let Some(this) = a.this.clone() else {
        return Ok(Value::Null);
    };
    let taken = php_vm::object::internal::<InternalIt, _>(&this, |s| {
        s.it.take().map(|it| (it, s.rewound))
    })
    .flatten();
    let Some((mut it, rewound)) = taken else {
        return Err(vm.throw(
            "Error",
            "The InternalIterator object has not been properly initialized",
        ));
    };
    let r = with_host(vm, |h| run_op(h, it.as_mut(), rewound, op));
    php_vm::object::internal::<InternalIt, _>(&this, |s| {
        s.it = Some(it);
        s.rewound = true;
    });
    r
}

fn run_op(h: &mut VmHost<'_>, it: &mut dyn ObjectIterator, rewound: bool, op: Op) -> RtResult<Value> {
    if !rewound || matches!(op, Op::Rewind) {
        it.rewind(h)?;
    }
    Ok(match op {
        Op::Current => it.current(h)?.unwrap_or_default(),
        Op::Key => it.key(h)?.unwrap_or_default(),
        Op::Next => {
            it.next(h)?;
            Value::Null
        }
        Op::Valid => Value::Bool(it.valid(h)?),
        Op::Rewind => Value::Null,
    })
}

fn ii_current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    internal_op(vm, a, Op::Current)
}
fn ii_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    internal_op(vm, a, Op::Key)
}
fn ii_next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    internal_op(vm, a, Op::Next)
}
fn ii_valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    internal_op(vm, a, Op::Valid)
}
fn ii_rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    internal_op(vm, a, Op::Rewind)
}
fn ii_construct(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// Registers `InternalIterator` unless the VM already provides it.
pub fn register_internal_iterator(vm: &mut Vm) {
    if vm.find_class(b"InternalIterator").is_some() {
        return;
    }
    ClassBuilder::new("InternalIterator")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE)
        .implements(vm, &["Iterator"])
        .method("__construct()", acc::PRIVATE, ii_construct)
        .method("current(): mixed", acc::PUBLIC, ii_current)
        .method("key(): mixed", acc::PUBLIC, ii_key)
        .method("next(): void", acc::PUBLIC, ii_next)
        .method("valid(): bool", acc::PUBLIC, ii_valid)
        .method("rewind(): void", acc::PUBLIC, ii_rewind)
        .register(vm);
}
