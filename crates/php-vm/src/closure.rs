//! Closures (`Zend/zend_closures.c`): creation, binding, calling and the
//! `Closure` class; static variables.

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class, InternalKind};
use crate::func::{flags, Args, Func, UserFunction};
use crate::object;
use crate::vm::{Frame, PendingCall, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpRef, Value, ZArray, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

/// Internal state of a `Closure` object.
#[derive(Debug, Clone)]
pub struct ClosureData {
    /// The function.
    pub func: Func,
    /// Bound `$this`.
    pub this: Option<Obj>,
    /// Class scope.
    pub scope: Option<Rc<Class>>,
    /// Called scope.
    pub called: Option<Rc<Class>>,
    /// Captured `use` variables: (CV index, value; references for by-ref).
    pub vars: Vec<(u32, Value)>,
    /// Static variables of this closure object.
    pub statics: Rc<RefCell<Vec<Option<PhpRef>>>>,
    /// Created by `Closure::fromCallable` / first-class callable syntax.
    pub fake: bool,
    /// `__call` target name for fake closures of magic methods.
    pub magic_name: Option<ZStr>,
}

impl Vm {
    fn new_closure_object(&mut self, data: ClosureData) -> Obj {
        let cls = self.find_class(b"Closure").expect("Closure class");
        let o = object::new_object(cls, Vec::new());
        object::set_internal(&o, data);
        o
    }

    /// Creates a closure for a declared function in the current frame.
    pub(crate) fn make_closure(&mut self, f: &Rc<UserFunction>) -> VmResult<Value> {
        let mut vars = Vec::with_capacity(f.uses.len());
        for u in &f.uses {
            if u.by_ref {
                let fr = self.frame();
                let idx = fr.cv_idx(u.parent_cv);
                let vstore = fr.vars.clone();
                let mut st = vstore.borrow_mut();
                let slot = st.slots[idx].get_or_insert(Value::Null);
                let r = slot.make_ref();
                vars.push((u.cv, Value::Reference(r)));
            } else if u.implicit {
                if let Some(v) = self.cv_get(u.parent_cv) {
                    vars.push((u.cv, v.deref()));
                }
            } else {
                let v = self.read_cv(u.parent_cv)?;
                vars.push((u.cv, v));
            }
        }
        let fr = self.frame();
        let this = if f.flags & flags::STATIC != 0 {
            None
        } else {
            fr.this.clone()
        };
        let data = ClosureData {
            func: Func::User(f.clone()),
            this,
            scope: fr.scope.clone(),
            called: fr.called.clone(),
            vars,
            statics: Rc::new(RefCell::new(Vec::new())),
            fake: false,
            magic_name: None,
        };
        Ok(Value::Object(self.new_closure_object(data)))
    }

    /// First-class callable syntax: wraps a pending call in a closure.
    pub(crate) fn make_closure_from_call(&mut self, call: PendingCall) -> VmResult<Value> {
        if let Some(c) = &call.closure {
            return Ok(Value::Object(c.clone()));
        }
        let data = ClosureData {
            func: call.func,
            this: call.this,
            scope: call.scope,
            called: call.called,
            vars: Vec::new(),
            statics: Rc::new(RefCell::new(Vec::new())),
            fake: true,
            magic_name: call.magic_name,
        };
        Ok(Value::Object(self.new_closure_object(data)))
    }

    /// The closure data of a `Closure` object.
    pub(crate) fn closure_data(&self, o: &Obj) -> Option<ClosureData> {
        object::internal::<ClosureData, _>(o, |d| d.clone())
    }

    /// A pending call for invoking a closure object.
    pub(crate) fn closure_call(&self, o: &Obj) -> Option<PendingCall> {
        let d = self.closure_data(o)?;
        Some(PendingCall {
            this: d.this.clone(),
            scope: d.scope.clone(),
            called: d.called.clone(),
            closure: if d.fake { None } else { Some(o.clone()) },
            magic_name: d.magic_name.clone(),
            ..PendingCall::new(d.func.clone())
        })
    }

    /// `$closure->__invoke(...)`.
    pub(crate) fn closure_method_call(&self, o: &Obj, lc: &ZStr) -> Option<PendingCall> {
        if lc.as_bytes() == b"__invoke" {
            return self.closure_call(o);
        }
        None
    }

    /// Binds captured variables and statics into a new frame.
    pub(crate) fn bind_closure_vars(&self, f: &mut Frame, c: &Obj) {
        if let Some(d) = self.closure_data(c) {
            {
                let mut st = f.vars.borrow_mut();
                for (cv, v) in d.vars {
                    let idx = cv as usize;
                    if idx < st.slots.len() {
                        st.slots[idx] = Some(v);
                    }
                }
            }
            f.statics = Some(d.statics.clone());
        }
    }

    fn statics_store(&mut self) -> Rc<RefCell<Vec<Option<PhpRef>>>> {
        if let Some(s) = &self.frame().statics {
            return s.clone();
        }
        let s = Rc::new(RefCell::new(Vec::new()));
        self.frame_mut().statics = Some(s.clone());
        s
    }

    /// Static variable slot `idx`, if initialized.
    pub(crate) fn static_slot(&mut self, idx: u32) -> Option<PhpRef> {
        if self.frame().statics.is_none() {
            if let Some(uf) = self.frame().ufunc.clone() {
                return uf.statics.borrow().get(idx as usize).cloned().flatten();
            }
        }
        let s = self.statics_store();
        let b = s.borrow();
        b.get(idx as usize).cloned().flatten()
    }

    /// Initializes static variable slot `idx`.
    pub(crate) fn static_init(&mut self, idx: u32, v: Value) -> PhpRef {
        let r = PhpRef::new(v);
        if self.frame().statics.is_none() {
            if let Some(uf) = self.frame().ufunc.clone() {
                let mut b = uf.statics.borrow_mut();
                if b.len() <= idx as usize {
                    b.resize(idx as usize + 1, None);
                }
                b[idx as usize] = Some(r.clone());
                return r;
            }
        }
        let s = self.statics_store();
        let mut b = s.borrow_mut();
        if b.len() <= idx as usize {
            b.resize(idx as usize + 1, None);
        }
        b[idx as usize] = Some(r.clone());
        r
    }

    fn bind_closure(
        &mut self,
        c: &Obj,
        new_this: Option<Value>,
        new_scope: Option<Value>,
        scope_given: bool,
    ) -> VmResult<Value> {
        let Some(mut d) = self.closure_data(c) else {
            return Ok(Value::Null);
        };
        let new_this = new_this.filter(|v| !v.is_null());
        // Resolve the new scope.
        let scope: Option<Rc<Class>> = if scope_given {
            match new_scope {
                Some(Value::Object(o)) => Some(object::class_of(&o).clone()),
                Some(Value::Null) => None,
                Some(Value::Str(s)) if s.as_bytes() == b"static" => d.scope.clone(),
                Some(v) => {
                    let s = self.to_zstr(&v)?;
                    match self.lookup_class(&s)? {
                        Some(c) => Some(c),
                        None => {
                            self.warning(&format!("Class \"{}\" not found", s.to_string_lossy()))?;
                            return Ok(Value::Null);
                        }
                    }
                }
                None => d.scope.clone(),
            }
        } else {
            d.scope.clone()
        };
        let is_static = d.func.flags() & flags::STATIC != 0;
        if let Some(Value::Object(t)) = &new_this {
            if is_static {
                self.warning("Cannot bind an instance to a static closure, this will be an error in PHP 9")?;
                return Ok(Value::Null);
            }
            let _ = t;
        }
        if let Some(s) = &scope {
            if s.flags & cflags::INTERNAL != 0 && s.lc_name.as_bytes() != b"closure"
                && !d.scope.as_ref().is_some_and(|ds| Rc::ptr_eq(ds, s))
            {
                self.warning(&format!("Cannot bind closure to scope of internal class {}", s.name))?;
                return Ok(Value::Null);
            }
        }
        if d.fake {
            // Rebinding a method closure to another scope is not allowed.
            if scope_given && !match (&scope, &d.scope) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            } {
                self.warning("Cannot rebind scope of closure created from method")?;
                return Ok(Value::Null);
            }
        }
        if new_this.is_none() && d.this.is_some() && !is_static {
            if let Func::User(u) = &d.func {
                if u.code.ops.iter().any(|op| matches!(op, crate::opcode::Op::FetchThis { .. })) || d.fake {
                    self.warning("Cannot unbind $this of closure using $this, this will be an error in PHP 9")?;
                    return Ok(Value::Null);
                }
            }
        }
        d.this = match new_this {
            Some(Value::Object(o)) => Some(o),
            _ => None,
        };
        d.called = match &d.this {
            Some(t) => Some(object::class_of(t).clone()),
            None => scope.clone(),
        };
        d.scope = scope;
        // Statics are copied.
        let copied: Vec<Option<PhpRef>> = d
            .statics
            .borrow()
            .iter()
            .map(|s| s.as_ref().map(|r| PhpRef::new(r.get())))
            .collect();
        d.statics = Rc::new(RefCell::new(copied));
        Ok(Value::Object(self.new_closure_object(d)))
    }
}

fn closure_bind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(c) = a.at(0) else {
        return Ok(Value::Null);
    };
    let has_scope = a.has(2);
    vm.bind_closure(&c, Some(a.at(1)), a.get(2), has_scope)
}

fn closure_bind_to(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = a.this_obj();
    let has_scope = a.has(1);
    vm.bind_closure(&c, Some(a.at(0)), a.get(1), has_scope)
}

fn closure_call_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = a.this_obj();
    let Some(mut d) = vm.closure_data(&c) else {
        return Ok(Value::Null);
    };
    let Value::Object(this) = a.at(0) else {
        return Ok(Value::Null);
    };
    if d.func.flags() & flags::STATIC != 0 {
        vm.warning("Cannot bind an instance to a static closure, this will be an error in PHP 9")?;
        return Ok(Value::Null);
    }
    let cls = object::class_of(&this).clone();
    d.scope = Some(cls.clone());
    d.called = Some(cls);
    d.this = Some(this);
    let tmp = vm.new_closure_object(d);
    let args: Vec<Value> = a.values.iter().skip(1).cloned().collect();
    let named = std::mem::take(&mut a.named);
    vm.call_value_named(&Value::Object(tmp), args, named)
}

fn closure_from_callable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    if let Value::Object(o) = &v {
        if vm.closure_data(o).is_some() {
            return Ok(v);
        }
    }
    let call = vm.resolve_callable_for_call(&v)?;
    vm.make_closure_from_call(call)
}

fn closure_invoke(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = a.this_obj();
    let args = std::mem::take(&mut a.values);
    let named = std::mem::take(&mut a.named);
    vm.call_value_named(&Value::Object(c), args, named)
}

/// Debug information of a closure (`var_dump`).
pub(crate) fn closure_debug_array(vm: &mut Vm, o: &Obj) -> Option<ZArray> {
    let d = vm.closure_data(o)?;
    let mut t = HashTable::new();
    let name = match &d.func {
        Func::User(u) if !u.is_closure() => match &d.scope {
            Some(s) => format!("{}::{}", s.name, u.name),
            None => u.name.to_string_lossy(),
        },
        Func::Builtin(b) => match &d.scope {
            Some(s) => format!("{}::{}", s.name, b.name),
            None => b.name.to_string_lossy(),
        },
        Func::User(u) => u.name.to_string_lossy(),
    };
    t.insert(ArrayKey::Str(ZStr::from("name")), Value::Str(ZStr::from(name)));
    if let Func::User(u) = &d.func {
        t.insert(ArrayKey::Str(ZStr::from("file")), Value::Str(u.code.filename.clone()));
        t.insert(ArrayKey::Str(ZStr::from("line")), Value::Int(i64::from(u.line_start)));
        if !d.vars.is_empty() {
            let mut st = HashTable::new();
            for (cv, v) in &d.vars {
                let n = u.code.cv_names.get(*cv as usize).cloned().unwrap_or_default();
                st.insert(ArrayKey::Str(n), v.clone());
            }
            t.insert(ArrayKey::Str(ZStr::from("static")), Value::Array(ZArray::from_table(st)));
        }
    }
    if let Some(this) = &d.this {
        t.insert(ArrayKey::Str(ZStr::from("this")), Value::Object(this.clone()));
    }
    let params = d.func.params().to_vec();
    if !params.is_empty() {
        let mut pt = HashTable::new();
        for p in &params {
            let key = format!("{}${}", if p.by_ref { "&" } else { "" }, p.name);
            let desc = if p.has_default || p.variadic { "<optional>" } else { "<required>" };
            pt.insert(ArrayKey::Str(ZStr::from(key)), Value::Str(ZStr::from(desc)));
        }
        t.insert(ArrayKey::Str(ZStr::from("parameter")), Value::Array(ZArray::from_table(pt)));
    }
    Some(ZArray::from_table(t))
}

/// Registers the `Closure` class.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("Closure")
        .flags(cflags::FINAL | cflags::NO_NEW | cflags::NOT_SERIALIZABLE)
        .internal(InternalKind::Closure)
        .method(
            "bind(Closure $closure, ?object $newThis, object|string|null $newScope = \"static\"): ?Closure",
            flags::PUBLIC | flags::STATIC,
            closure_bind,
        )
        .method(
            "bindTo(?object $newThis, object|string|null $newScope = \"static\"): ?Closure",
            flags::PUBLIC,
            closure_bind_to,
        )
        .method("call(object $newThis, mixed ...$args): mixed", flags::PUBLIC, closure_call_method)
        .method(
            "fromCallable(callable $callback): Closure",
            flags::PUBLIC | flags::STATIC,
            closure_from_callable,
        )
        .method("__invoke(mixed ...$args): mixed", flags::PUBLIC, closure_invoke)
        .register(vm);
}
