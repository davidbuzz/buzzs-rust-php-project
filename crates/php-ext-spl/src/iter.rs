//! Bridges between the two iterator protocols: SPL's
//! [`ObjectIterator`] (driven through an [`SplHost`]) and the VM's
//! [`NativeIterator`] (driven through the [`Vm`]), plus the engine's
//! `InternalIterator` class (`zend_create_internal_iterator_zval`).

use crate::host::{rt_throw, VmHost};
use php_runtime::ZStr;
use php_runtime::{RtResult, Value};
use php_spl::ObjectIterator;
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, NativeFn, NativeIterator, VmResult,
};
use php_vm::func::Func;
use php_vm::Vm;
use std::cell::RefCell;
use std::collections::HashMap;

/// Runs `f` with a [`VmHost`] over `vm`, converting the error.
pub fn with_host<T>(vm: &mut Vm, f: impl FnOnce(&mut VmHost<'_>) -> RtResult<T>) -> VmResult<T> {
    let _entry = crate::files::enter();
    let r = {
        let mut host = VmHost::new(vm);
        f(&mut host)
    };
    finish(vm, r)
}

/// Converts a `php-spl` result, surfacing a bailout (`exit`, fatal error)
/// that happened in a callback even when the library carried on.
pub fn finish<T>(vm: &mut Vm, r: RtResult<T>) -> VmResult<T> {
    match r {
        Ok(v) => match vm.pending_throw.take() {
            Some(t) => Err(t),
            None => Ok(v),
        },
        Err(e) => Err(rt_throw(vm, e)),
    }
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

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Current => "current",
            Op::Key => "key",
            Op::Next => "next",
            Op::Valid => "valid",
            Op::Rewind => "rewind",
        }
    }
}

thread_local! {
    /// The VM's own `InternalIterator` method handlers, when the VM
    /// provides the class (its constructor function is crate-private, so
    /// SPL registers a replacement that serves both kinds of objects).
    static VM_HANDLERS: RefCell<HashMap<&'static str, NativeFn>> = RefCell::new(HashMap::new());
}

fn internal_op(vm: &mut Vm, a: &mut Args, op: Op) -> VmResult<Value> {
    let Some(this) = a.this.clone() else {
        return Ok(Value::Null);
    };
    let taken =
        php_vm::object::internal::<InternalIt, _>(&this, |s| s.it.take().map(|it| (it, s.rewound)))
            .flatten();
    let Some((mut it, rewound)) = taken else {
        // An iterator created by the VM itself (`WeakMap::getIterator()`):
        // run the VM's own implementation.
        if let Some(f) = VM_HANDLERS.with(|m| m.borrow().get(op.name()).copied()) {
            return f(vm, a);
        }
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

fn run_op(
    h: &mut VmHost<'_>,
    it: &mut dyn ObjectIterator,
    rewound: bool,
    op: Op,
) -> RtResult<Value> {
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
#[allow(clippy::unnecessary_wraps)] // native method signature
fn ii_construct(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// Registers `InternalIterator` unless the VM already provides it.
pub fn register_internal_iterator(vm: &mut Vm) {
    if let Some(existing) = vm.find_class(b"InternalIterator") {
        VM_HANDLERS.with(|m| {
            let mut m = m.borrow_mut();
            for name in ["current", "key", "next", "valid", "rewind"] {
                if let Some(meth) = existing.methods.get(&ZStr::from(name)) {
                    if let Func::Builtin(b) = &meth.func {
                        m.insert(name, b.handler);
                    }
                }
            }
        });
    }
    ClassBuilder::new("InternalIterator")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE)
        .implements(vm, &["Iterator"])
        .method("__construct()", acc::PRIVATE, ii_construct)
        .method("current(): mixed", acc::PUBLIC, ii_current)
        .method("key(): mixed", acc::PUBLIC, ii_key)
        .method("next(): void", acc::PUBLIC, ii_next)
        .method("valid(): bool", acc::PUBLIC, ii_valid)
        .method("rewind(): void", acc::PUBLIC, ii_rewind)
        // `zend_internal_iterator_free` of the VM's native iterators.
        .hooks(ClassHooks {
            destruct: Some(php_vm::Vm::internal_iterator_destruct),
            ..ClassHooks::default()
        })
        .register(vm);
    // Replacing a class appends its name to the declaration order again.
    let mut seen = std::collections::HashSet::new();
    vm.class_order
        .retain(|n| seen.insert(n.as_bytes().to_vec()));
}
