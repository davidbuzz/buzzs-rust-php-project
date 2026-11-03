//! Object properties (`zend_std_read_property`, `zend_std_write_property`,
//! `zend_std_has_property`, `zend_std_unset_property`, magic `__get` & co.)
//! and object instantiation.

use crate::class::{cflags, Class, InternalKind, PropInfo};
use crate::exec::scope_display;
use crate::func::flags;
use crate::object::{self, oflags};
use crate::opcode::FetchMode;
use crate::place::PropContainer;
use crate::typecheck::{given_type, TypeCtx};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpRef, Value, ZArray, ZStr};
use std::rc::Rc;

/// Result of a property lookup.
pub(crate) enum PropLookup {
    /// A declared, accessible property.
    Found(Rc<PropInfo>),
    /// A declared property the scope cannot access.
    Inaccessible(Rc<PropInfo>),
    /// No declared property: dynamic.
    NotFound,
}

const G_GET: u8 = 0;
const G_SET: u8 = 1;
const G_ISSET: u8 = 2;
const G_UNSET: u8 = 3;

impl Vm {
    /// Whether an object is an instance of the (lower-case) class name.
    pub(crate) fn instance_of(&self, o: &Obj, lc: &[u8]) -> bool {
        object::class_of(o).instance_of_name(lc)
    }

    fn class_by_name(&self, name: &ZStr) -> Option<Rc<Class>> {
        self.classes.get(&name.to_ascii_lowercase()).cloned()
    }

    /// Property visibility lookup.
    pub(crate) fn lookup_prop(
        &self,
        cls: &Rc<Class>,
        name: &ZStr,
        scope: Option<&Rc<Class>>,
    ) -> PropLookup {
        if let Some(s) = scope {
            if !Rc::ptr_eq(s, cls) {
                if let Some(pi) = s.prop_map.get(name) {
                    if pi.visibility() == flags::PRIVATE
                        && pi.flags & flags::STATIC == 0
                        && pi.class.eq_ignore_ascii_case(s.name.as_bytes())
                        && cls.is_subclass_of(s)
                    {
                        return PropLookup::Found(pi.clone());
                    }
                }
            }
        }
        let Some(pi) = cls.prop_map.get(name) else {
            return PropLookup::NotFound;
        };
        if pi.flags & flags::STATIC != 0 {
            return PropLookup::NotFound;
        }
        match pi.visibility() {
            flags::PUBLIC => PropLookup::Found(pi.clone()),
            flags::PRIVATE => {
                if scope.is_some_and(|s| pi.class.eq_ignore_ascii_case(s.name.as_bytes())) {
                    PropLookup::Found(pi.clone())
                } else {
                    PropLookup::Inaccessible(pi.clone())
                }
            }
            _ => {
                let ok = scope.is_some_and(|s| match self.class_by_name(&pi.class) {
                    Some(decl) => s.is_subclass_of(&decl) || decl.is_subclass_of(s),
                    None => true,
                });
                if ok {
                    PropLookup::Found(pi.clone())
                } else {
                    PropLookup::Inaccessible(pi.clone())
                }
            }
        }
    }

    fn guard_active(&self, o: &Obj, name: &ZStr, kind: u8) -> bool {
        self.prop_guards.contains(&(o.handle(), name.clone(), kind))
    }

    fn with_guard<T>(
        &mut self,
        o: &Obj,
        name: &ZStr,
        kind: u8,
        f: impl FnOnce(&mut Vm) -> VmResult<T>,
    ) -> VmResult<T> {
        let key = (o.handle(), name.clone(), kind);
        self.prop_guards.insert(key.clone());
        let r = f(self);
        self.prop_guards.remove(&key);
        r
    }

    fn call_magic_get(&mut self, o: &Obj, name: &ZStr) -> VmResult<Value> {
        let m = object::class_of(o).magic.get.clone().expect("__get");
        self.with_guard(o, name, G_GET, |vm| {
            vm.call_method(o, &m, vec![Value::Str(name.clone())])
        })
    }

    fn has_magic(&self, o: &Obj, name: &ZStr, kind: u8) -> bool {
        let c = object::class_of(o);
        let m = match kind {
            G_GET => c.magic.get.is_some(),
            G_SET => c.magic.set.is_some(),
            G_ISSET => c.magic.isset.is_some(),
            _ => c.magic.unset.is_some(),
        };
        m && !self.guard_active(o, name, kind)
    }

    fn inaccessible_error(
        &mut self,
        cls: &Rc<Class>,
        pi: &PropInfo,
        verb: &str,
    ) -> crate::vm::Throw {
        let vis = if pi.visibility() == flags::PRIVATE {
            "private"
        } else {
            "protected"
        };
        let _ = cls;
        self.make_throw(
            "Error",
            &format!(
                "Cannot {verb} {vis} property {}::${}",
                object::display_name(&cls.name),
                pi.name
            ),
        )
    }

    fn undefined_property_warning(&mut self, o: &Obj, name: &ZStr) -> VmResult<()> {
        self.warning(&format!(
            "Undefined property: {}::${}",
            object::display_name(&object::class_of(o).name),
            name.to_string_lossy()
        ))
    }

    /// Whether the current code is a hook of property `pi` on `o`.
    fn in_hook(&self, pi: &PropInfo, o: &Obj) -> bool {
        let Some(f) = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
        else {
            return false;
        };
        let Some(uf) = &f.ufunc else { return false };
        match &uf.hook_prop {
            Some(n) => *n == pi.name && f.this.as_ref().is_some_and(|t| Obj::ptr_eq(t, o)),
            None => false,
        }
    }

    fn call_hook(
        &mut self,
        o: &Obj,
        f: &Rc<crate::func::UserFunction>,
        pi: &PropInfo,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        let decl = self
            .class_by_name(f.class_name.as_ref().unwrap_or(&pi.class))
            .unwrap_or_else(|| object::class_of(o).clone());
        let mut call = crate::vm::PendingCall {
            this: Some(o.clone()),
            scope: Some(decl),
            called: Some(object::class_of(o).clone()),
            ..crate::vm::PendingCall::new(crate::func::Func::User(f.clone()))
        };
        call.args = args.into_iter().map(Some).collect();
        self.call_pending(call)
    }

    fn virtual_error(&mut self, o: &Obj, pi: &PropInfo, msg: &str) -> crate::vm::Throw {
        self.make_throw(
            "Error",
            &format!(
                "{msg} {}::${}",
                object::display_name(&object::class_of(o).name),
                pi.name
            ),
        )
    }

    /// Calls a native `read_property` hook with fetch context `ctx`.
    pub(crate) fn native_read_property(
        &mut self,
        o: &Obj,
        name: &ZStr,
        ctx: crate::ext::PropCtx,
    ) -> VmResult<Option<Value>> {
        let Some(f) = object::class_of(o)
            .hooks
            .as_ref()
            .and_then(|h| h.read_property)
        else {
            return Ok(None);
        };
        let saved = std::mem::replace(&mut self.prop_ctx, ctx);
        let r = f(self, o, name);
        self.prop_ctx = saved;
        r
    }

    /// Reads `$o->name`.
    pub(crate) fn read_property(
        &mut self,
        o: &Obj,
        name: &ZStr,
        mode: FetchMode,
    ) -> VmResult<Value> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        let ctx = if mode == FetchMode::Is {
            crate::ext::PropCtx::Isset
        } else {
            crate::ext::PropCtx::Read
        };
        if let Some(v) = self.native_read_property(o, name, ctx)? {
            return Ok(v);
        }
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                if let Some(h) = pi.hooks.clone() {
                    if !self.in_hook(&pi, o) {
                        if let Some(g) = &h.get {
                            return Ok(self.call_hook(o, g, &pi, Vec::new())?.deref());
                        }
                        if pi.is_virtual() {
                            return Err(self.virtual_error(
                                o,
                                &pi,
                                "Cannot read from set-only virtual property",
                            ));
                        }
                    } else if pi.is_virtual() {
                        return Err(self.virtual_error(
                            o,
                            &pi,
                            "Must not read from virtual property",
                        ));
                    }
                }
                let v = object::data(o).slots.borrow()[pi.slot as usize].clone();
                if let Some(v) = v {
                    return Ok(v.deref());
                }
                let was_unset = object::data(o)
                    .readonly_init
                    .borrow()
                    .get(pi.slot as usize)
                    .copied()
                    .unwrap_or(false);
                if (pi.ty.is_none() || was_unset) && self.has_magic(o, name, G_GET) {
                    return self.magic_read(o, name, mode);
                }
                if pi.ty.is_some() {
                    if mode == FetchMode::Is {
                        return Ok(Value::Null);
                    }
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Typed property {}::${} must not be accessed before initialization",
                            object::display_name(&pi.class),
                            pi.name
                        ),
                    ));
                }
                if mode == FetchMode::Read {
                    self.undefined_property_warning(o, name)?;
                }
                Ok(Value::Null)
            }
            PropLookup::Inaccessible(pi) => {
                if self.has_magic(o, name, G_GET) {
                    return self.magic_read(o, name, mode);
                }
                if mode == FetchMode::Is {
                    return Ok(Value::Null);
                }
                Err(self.inaccessible_error(&cls, &pi, "access"))
            }
            PropLookup::NotFound => {
                let v = object::data(o)
                    .dynamic
                    .borrow()
                    .as_ref()
                    .and_then(|t| t.get(&ArrayKey::Str(name.clone())).cloned());
                if let Some(v) = v {
                    return Ok(v.deref());
                }
                if self.has_magic(o, name, G_GET) {
                    return self.magic_read(o, name, mode);
                }
                if mode == FetchMode::Read {
                    self.undefined_property_warning(o, name)?;
                }
                Ok(Value::Null)
            }
        }
    }

    fn magic_read(&mut self, o: &Obj, name: &ZStr, mode: FetchMode) -> VmResult<Value> {
        if mode == FetchMode::Is && self.has_magic(o, name, G_ISSET) {
            let m = object::class_of(o).magic.isset.clone().expect("__isset");
            let r = self.with_guard(o, name, G_ISSET, |vm| {
                vm.call_method(o, &m, vec![Value::Str(name.clone())])
            })?;
            if !self.to_bool(&r)? {
                return Ok(Value::Null);
            }
        }
        Ok(self.call_magic_get(o, name)?.deref())
    }

    /// `zend_asymmetric_property_has_set_access`.
    pub(crate) fn has_set_access(&self, pi: &PropInfo) -> bool {
        let scope = self.scope();
        let Some(s) = scope else { return false };
        if s.name.eq_ignore_ascii_case(pi.class.as_bytes()) {
            return true;
        }
        if pi.flags & flags::SET_PROTECTED != 0 {
            if let Some(decl) = self.class_by_name(&pi.class) {
                let mut root = decl.clone();
                while let Some(p) = root.parent.clone() {
                    if p.prop_map
                        .get(&pi.name)
                        .is_some_and(|x| x.visibility() != flags::PRIVATE)
                    {
                        root = p;
                    } else {
                        break;
                    }
                }
                return s.is_subclass_of(&root) || root.is_subclass_of(&s);
            }
        }
        false
    }

    fn asym_error(&mut self, cls_name: &ZStr, pi: &PropInfo, op: &str) -> crate::vm::Throw {
        let vis = if pi.flags & flags::SET_PRIVATE != 0 {
            "private(set)"
        } else if pi.flags & flags::READONLY != 0 {
            "protected(set) readonly"
        } else {
            "protected(set)"
        };
        let _ = cls_name;
        let called = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .and_then(|f| f.called.clone().or_else(|| f.scope.clone()));
        self.make_throw(
            "Error",
            &format!(
                "Cannot {op} {vis} property {}::${} from {}",
                object::display_name(&pi.class),
                pi.name,
                scope_display(called.as_ref())
            ),
        )
    }

    /// Readonly / set-visibility checks for writes (`zend_std_write_property`).
    fn check_write_access(
        &mut self,
        o: &Obj,
        pi: &PropInfo,
        initialized: bool,
        op: &str,
    ) -> VmResult<()> {
        if pi.flags & (flags::READONLY | flags::SET_MASK) == 0 {
            return Ok(());
        }
        let d = object::data(o);
        if pi.flags & flags::READONLY != 0 && initialized && d.flags.get() & oflags::CLONING == 0 {
            let msg = if op == "unset" {
                format!(
                    "Cannot unset readonly property {}::${}",
                    object::display_name(&pi.class),
                    pi.name
                )
            } else {
                format!(
                    "Cannot modify readonly property {}::${}",
                    object::display_name(&pi.class),
                    pi.name
                )
            };
            return Err(self.make_throw("Error", &msg));
        }
        if pi.flags & flags::SET_MASK != 0 && !self.has_set_access(pi) {
            let cn = object::class_of(o).name.clone();
            return Err(self.asym_error(&cn, pi, op));
        }
        Ok(())
    }

    /// Coerces a value for a typed property.
    pub(crate) fn coerce_prop(
        &mut self,
        o: Option<&Obj>,
        pi: &PropInfo,
        mut v: Value,
    ) -> VmResult<Value> {
        let Some(ty) = &pi.ty else {
            return Ok(v);
        };
        let strict = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .is_some_and(|f| f.code.strict_types);
        let decl = self.class_by_name(&pi.class);
        let called = o.map(|o| object::class_of(o).clone());
        let ctx = TypeCtx {
            scope: decl.as_ref(),
            called: called.as_ref(),
            strict,
            internal_arg: None,
        };
        let ty = ty.clone();
        if self.verify_type(&ty, &mut v, &ctx)? {
            return Ok(v);
        }
        let tyname = ty.display(
            decl.as_ref().map(|c| &c.name),
            decl.as_ref().and_then(|c| c.parent_name()),
            None,
        );
        Err(self.make_throw(
            "TypeError",
            &format!(
                "Cannot assign {} to property {}::${} of type {tyname}",
                given_type(&v),
                object::display_name(&pi.class),
                pi.name
            ),
        ))
    }

    /// Writes `$o->name = v`; returns the assigned value.
    pub(crate) fn write_property(&mut self, o: &Obj, name: &ZStr, v: Value) -> VmResult<Value> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.write_property) {
            if f(self, o, name, &v)? {
                return Ok(v);
            }
        }
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                if let Some(h) = pi.hooks.clone() {
                    if !self.in_hook(&pi, o) {
                        if let Some(sh) = &h.set {
                            if pi.flags & flags::SET_MASK != 0 && !self.has_set_access(&pi) {
                                let cn = cls.name.clone();
                                return Err(self.asym_error(&cn, &pi, "modify"));
                            }
                            self.call_hook(o, sh, &pi, vec![v.clone()])?;
                            return Ok(v);
                        }
                        if pi.is_virtual() {
                            return Err(self.virtual_error(
                                o,
                                &pi,
                                "Cannot write to get-only virtual property",
                            ));
                        }
                    } else if pi.is_virtual() {
                        return Err(self.virtual_error(
                            o,
                            &pi,
                            "Must not write to virtual property",
                        ));
                    }
                }
                let idx = pi.slot as usize;
                let cur_defined = object::data(o).slots.borrow()[idx].is_some();
                let magic_ok = !cur_defined
                    && (pi.ty.is_none()
                        || object::data(o)
                            .readonly_init
                            .borrow()
                            .get(idx)
                            .copied()
                            .unwrap_or(false));
                if magic_ok && self.has_magic(o, name, G_SET) {
                    return self.magic_write(o, name, v);
                }
                self.check_write_access(o, &pi, cur_defined, "modify")?;
                let v = self.coerce_prop(Some(o), &pi, v)?;
                if let Some(f) = object::data(o).readonly_init.borrow_mut().get_mut(idx) {
                    *f = false;
                }
                let old = {
                    let d = object::data(o);
                    let mut slots = d.slots.borrow_mut();
                    match &mut slots[idx] {
                        Some(Value::Reference(r)) => Some(r.set(v.clone())),
                        slot => slot.replace(v.clone()),
                    }
                };
                drop(old);
                Ok(v)
            }
            PropLookup::Inaccessible(pi) => {
                if self.has_magic(o, name, G_SET) {
                    return self.magic_write(o, name, v);
                }
                if pi.flags & flags::READONLY != 0 && pi.visibility() == flags::PUBLIC {
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Cannot modify readonly property {}::${}",
                            object::display_name(&cls.name),
                            pi.name
                        ),
                    ));
                }
                Err(self.inaccessible_error(&cls, &pi, "access"))
            }
            PropLookup::NotFound => {
                let key = ArrayKey::Str(name.clone());
                let exists = object::data(o)
                    .dynamic
                    .borrow()
                    .as_ref()
                    .is_some_and(|t| t.contains_key(&key));
                if !exists {
                    if self.has_magic(o, name, G_SET) {
                        return self.magic_write(o, name, v);
                    }
                    self.check_dynamic_creation(o, name)?;
                }
                let old = {
                    let d = object::data(o);
                    let mut dynp = d.dynamic.borrow_mut();
                    let t = dynp.get_or_insert_with(HashTable::new);
                    match t.get_mut(&key) {
                        Some(Value::Reference(r)) => Some(r.set(v.clone())),
                        _ => t.insert(key, v.clone()),
                    }
                };
                drop(old);
                Ok(v)
            }
        }
    }

    fn check_dynamic_creation(&mut self, o: &Obj, name: &ZStr) -> VmResult<()> {
        let cls = object::class_of(o).clone();
        if cls.flags & cflags::READONLY != 0 {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot create dynamic property {}::${}",
                    object::display_name(&cls.name),
                    name.to_string_lossy()
                ),
            ));
        }
        if cls.is_enum() || cls.flags & cflags::NO_DYNAMIC_PROPERTIES != 0 {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot create dynamic property {}::${}",
                    object::display_name(&cls.name),
                    name.to_string_lossy()
                ),
            ));
        }
        if cls.flags & cflags::ALLOW_DYNAMIC == 0 {
            self.deprecated(&format!(
                "Creation of dynamic property {}::${} is deprecated",
                object::display_name(&cls.name),
                name.to_string_lossy()
            ))?;
        }
        Ok(())
    }

    fn magic_write(&mut self, o: &Obj, name: &ZStr, v: Value) -> VmResult<Value> {
        let m = object::class_of(o).magic.set.clone().expect("__set");
        self.with_guard(o, name, G_SET, |vm| {
            vm.call_method(o, &m, vec![Value::Str(name.clone()), v.clone()])
        })?;
        Ok(v)
    }

    /// Writes a dynamic property without checks (casts, internal use).
    pub(crate) fn write_dynamic_prop(&mut self, o: &Obj, name: ZStr, v: Value) {
        let d = object::data(o);
        let mut dynp = d.dynamic.borrow_mut();
        dynp.get_or_insert_with(HashTable::new)
            .insert(ArrayKey::Str(name), v);
    }

    /// Makes `$o->name` a reference and returns it.
    pub(crate) fn property_ref(&mut self, o: &Obj, name: &ZStr) -> VmResult<PhpRef> {
        if let Some(Value::Reference(r)) =
            self.native_read_property(o, name, crate::ext::PropCtx::Ref)?
        {
            return Ok(r);
        }
        match self.property_container(o, name)? {
            PropContainer::Declared(idx) => {
                let d = object::data(o);
                let mut slots = d.slots.borrow_mut();
                let slot = slots[idx].get_or_insert(Value::Null);
                Ok(slot.make_ref())
            }
            PropContainer::Dynamic(key) => {
                let d = object::data(o);
                let mut dynp = d.dynamic.borrow_mut();
                let t = dynp.get_or_insert_with(HashTable::new);
                Ok(t.lookup(key).make_ref())
            }
            PropContainer::Value(v) => match v {
                Value::Reference(r) => Ok(r),
                other => Ok(PhpRef::new(other)),
            },
        }
    }

    /// The storage of a property for nested writes (`$o->a[] = 1`).
    pub(crate) fn property_container(&mut self, o: &Obj, name: &ZStr) -> VmResult<PropContainer> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                if let Some(h) = pi.hooks.clone() {
                    if !self.in_hook(&pi, o) {
                        let v = match &h.get {
                            Some(g) => Some(self.call_hook(o, g, &pi, Vec::new())?),
                            None => None,
                        };
                        let v = match v {
                            Some(v) => Some(v),
                            None if pi.is_virtual() => {
                                return Err(self.virtual_error(
                                    o,
                                    &pi,
                                    "Cannot read from set-only virtual property",
                                ));
                            }
                            None if h.set.is_some() => None,
                            None => None,
                        };
                        match v {
                            Some(Value::Object(x)) => {
                                return Ok(PropContainer::Value(Value::Object(x)))
                            }
                            Some(Value::Reference(r)) => {
                                return Ok(PropContainer::Value(Value::Reference(r)))
                            }
                            Some(_) => {
                                return Err(self.make_throw(
                                    "Error",
                                    &format!(
                                        "Indirect modification of {}::${} is not allowed",
                                        object::display_name(&object::class_of(o).name),
                                        pi.name
                                    ),
                                ));
                            }
                            None => {
                                if h.set.is_some() {
                                    let cur =
                                        object::data(o).slots.borrow()[pi.slot as usize].clone();
                                    if !matches!(
                                        cur.as_ref().map(Value::deref),
                                        Some(Value::Object(_))
                                    ) {
                                        return Err(self.make_throw(
                                            "Error",
                                            &format!(
                                                "Indirect modification of {}::${} is not allowed",
                                                object::display_name(&object::class_of(o).name),
                                                pi.name
                                            ),
                                        ));
                                    }
                                }
                            }
                        }
                    } else if pi.is_virtual() {
                        return Err(self.virtual_error(
                            o,
                            &pi,
                            "Must not read from virtual property",
                        ));
                    }
                }
                let idx = pi.slot as usize;
                let defined = object::data(o).slots.borrow()[idx].is_some();
                if !defined && self.has_magic(o, name, G_GET) {
                    return self.magic_container(o, name);
                }
                if pi.flags & (flags::READONLY | flags::SET_MASK) != 0
                    && (pi.flags & flags::READONLY != 0 || !self.has_set_access(&pi))
                {
                    let is_obj = matches!(
                        object::data(o).slots.borrow()[idx]
                            .as_ref()
                            .map(Value::deref),
                        Some(Value::Object(_))
                    );
                    if !is_obj {
                        if pi.flags & flags::READONLY != 0 {
                            if defined || !self.has_set_access(&pi) {
                                return Err(self.make_throw(
                                    "Error",
                                    &format!(
                                        "Cannot indirectly modify readonly property {}::${}",
                                        object::display_name(&pi.class),
                                        pi.name
                                    ),
                                ));
                            }
                        } else {
                            let cn = cls.name.clone();
                            return Err(self.asym_error(&cn, &pi, "indirectly modify"));
                        }
                    }
                }
                if !defined {
                    if let Some(ty) = &pi.ty {
                        if ty.mask & crate::types::mask::ARRAY == 0 && !ty.is_mixed() {
                            return Err(self.make_throw(
                                "Error",
                                &format!(
                                    "Typed property {}::${} must not be accessed before initialization",
                                    object::display_name(&pi.class),
                                    pi.name
                                ),
                            ));
                        }
                    }
                }
                Ok(PropContainer::Declared(idx))
            }
            PropLookup::Inaccessible(pi) => {
                if self.has_magic(o, name, G_GET) {
                    return self.magic_container(o, name);
                }
                Err(self.inaccessible_error(&cls, &pi, "access"))
            }
            PropLookup::NotFound => {
                let key = ArrayKey::Str(name.clone());
                let exists = object::data(o)
                    .dynamic
                    .borrow()
                    .as_ref()
                    .is_some_and(|t| t.contains_key(&key));
                if !exists {
                    if self.has_magic(o, name, G_GET) {
                        return self.magic_container(o, name);
                    }
                    self.check_dynamic_creation(o, name)?;
                }
                Ok(PropContainer::Dynamic(key))
            }
        }
    }

    fn magic_container(&mut self, o: &Obj, name: &ZStr) -> VmResult<PropContainer> {
        let v = self.call_magic_get(o, name)?;
        match v {
            Value::Reference(r) => Ok(PropContainer::Value(Value::Reference(r))),
            Value::Object(o2) => Ok(PropContainer::Value(Value::Object(o2))),
            other => {
                self.notice(&format!(
                    "Indirect modification of overloaded property {}::${} has no effect",
                    object::display_name(&object::class_of(o).name),
                    name.to_string_lossy()
                ))?;
                Ok(PropContainer::Value(other))
            }
        }
    }

    /// `$o->name =& r`.
    pub(crate) fn assign_property_ref(
        &mut self,
        o: &Obj,
        name: &ZStr,
        r: Value,
    ) -> VmResult<Value> {
        match self.property_container(o, name)? {
            PropContainer::Declared(idx) => {
                let d = object::data(o);
                let old = d.slots.borrow_mut()[idx].replace(r.clone());
                drop(old);
                Ok(r)
            }
            PropContainer::Dynamic(key) => {
                let d = object::data(o);
                let old = d
                    .dynamic
                    .borrow_mut()
                    .get_or_insert_with(HashTable::new)
                    .insert(key, r.clone());
                drop(old);
                Ok(r)
            }
            PropContainer::Value(_) => Ok(r),
        }
    }

    /// `unset($o->name)`.
    pub(crate) fn unset_property(&mut self, o: &Obj, name: &ZStr) -> VmResult<()> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.unset_property) {
            if f(self, o, name)? {
                return Ok(());
            }
        }
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                if pi.hooks.is_some() {
                    return Err(self.virtual_error(o, &pi, "Cannot unset hooked property"));
                }
                let idx = pi.slot as usize;
                let init = object::data(o).slots.borrow()[idx].is_some();
                self.check_write_access(o, &pi, init, "unset")?;
                let defined = object::data(o).slots.borrow()[idx].is_some();
                if !defined && self.has_magic(o, name, G_UNSET) {
                    return self.magic_unset(o, name);
                }
                let old = object::data(o).slots.borrow_mut()[idx].take();
                if let Some(f) = object::data(o).readonly_init.borrow_mut().get_mut(idx) {
                    *f = true;
                }
                drop(old);
                Ok(())
            }
            PropLookup::Inaccessible(pi) => {
                if self.has_magic(o, name, G_UNSET) {
                    return self.magic_unset(o, name);
                }
                Err(self.inaccessible_error(&cls, &pi, "access"))
            }
            PropLookup::NotFound => {
                let key = ArrayKey::Str(name.clone());
                let old = {
                    let d = object::data(o);
                    let mut dynp = d.dynamic.borrow_mut();
                    dynp.as_mut().and_then(|t| t.remove(&key))
                };
                if old.is_none() && self.has_magic(o, name, G_UNSET) {
                    return self.magic_unset(o, name);
                }
                drop(old);
                Ok(())
            }
        }
    }

    fn magic_unset(&mut self, o: &Obj, name: &ZStr) -> VmResult<()> {
        let m = object::class_of(o).magic.unset.clone().expect("__unset");
        self.with_guard(o, name, G_UNSET, |vm| {
            vm.call_method(o, &m, vec![Value::Str(name.clone())])
        })?;
        Ok(())
    }

    /// Value of a property for nested unset (no magic `__get` side effects
    /// beyond PHP's).
    pub(crate) fn read_property_for_unset(
        &mut self,
        o: &Obj,
        name: &ZStr,
    ) -> VmResult<Option<Value>> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                let idx = pi.slot as usize;
                let d = object::data(o);
                let mut slots = d.slots.borrow_mut();
                match &mut slots[idx] {
                    Some(v @ Value::Array(_)) => Ok(Some(Value::Reference(v.make_ref()))),
                    Some(v) => Ok(Some(v.clone())),
                    None => Ok(None),
                }
            }
            PropLookup::Inaccessible(_) => {
                if self.has_magic(o, name, G_GET) {
                    return Ok(Some(self.call_magic_get(o, name)?));
                }
                Ok(None)
            }
            PropLookup::NotFound => {
                let key = ArrayKey::Str(name.clone());
                let d = object::data(o);
                let mut dynp = d.dynamic.borrow_mut();
                match dynp.as_mut().and_then(|t| t.get_mut(&key)) {
                    Some(v @ Value::Array(_)) => Ok(Some(Value::Reference(v.make_ref()))),
                    Some(v) => Ok(Some(v.clone())),
                    None => {
                        drop(dynp);
                        if self.has_magic(o, name, G_GET) {
                            return Ok(Some(self.call_magic_get(o, name)?));
                        }
                        Ok(None)
                    }
                }
            }
        }
    }

    /// `isset($o->name)` / `empty($o->name)`.
    pub(crate) fn has_property(&mut self, o: &Obj, name: &ZStr, empty: bool) -> VmResult<bool> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.has_property) {
            // The hook answers "set / not empty"; callers want "is empty"
            // for `empty()`.
            let saved = std::mem::replace(&mut self.prop_ctx, crate::ext::PropCtx::Isset);
            let r = f(self, o, name, empty);
            self.prop_ctx = saved;
            if let Some(r) = r? {
                return Ok(if empty { !r } else { r });
            }
        }
        let scope = self.scope();
        let direct: Option<Option<Value>> = match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi)
                if pi.hooks.as_ref().is_some_and(|h| h.get.is_some()) && !self.in_hook(&pi, o) =>
            {
                let g = pi
                    .hooks
                    .as_ref()
                    .and_then(|h| h.get.clone())
                    .expect("get hook");
                let v = self.call_hook(o, &g, &pi, Vec::new())?.deref();
                return Ok(if empty {
                    !self.to_bool(&v)?
                } else {
                    !v.is_null()
                });
            }
            PropLookup::Found(pi) => {
                let v = object::data(o).slots.borrow()[pi.slot as usize].clone();
                match v {
                    Some(v) => Some(Some(v.deref())),
                    None => None,
                }
            }
            PropLookup::Inaccessible(_) => None,
            PropLookup::NotFound => object::data(o)
                .dynamic
                .borrow()
                .as_ref()
                .and_then(|t| t.get(&ArrayKey::Str(name.clone())).map(|v| Some(v.deref()))),
        };
        if let Some(Some(v)) = direct {
            return Ok(if empty {
                !self.to_bool(&v)?
            } else {
                !v.is_null()
            });
        }
        if self.has_magic(o, name, G_ISSET) {
            let m = object::class_of(o).magic.isset.clone().expect("__isset");
            let r = self.with_guard(o, name, G_ISSET, |vm| {
                vm.call_method(o, &m, vec![Value::Str(name.clone())])
            })?;
            let set = self.to_bool(&r)?;
            if !empty {
                return Ok(set);
            }
            if !set {
                return Ok(true);
            }
            if self.has_magic(o, name, G_GET) {
                let v = self.call_magic_get(o, name)?;
                return Ok(!self.to_bool(&v.deref())?);
            }
            return Ok(true);
        }
        Ok(empty)
    }

    // ------------------------------------------------------------------
    // Instantiation and conversions
    // ------------------------------------------------------------------

    /// Default slot values of a class (evaluated once).
    pub(crate) fn class_defaults(&mut self, cls: &Rc<Class>) -> VmResult<Vec<Option<Value>>> {
        if let Some(d) = cls.defaults.borrow().as_ref() {
            return Ok(d.clone());
        }
        let mut out = Vec::with_capacity(cls.props.len());
        for pi in &cls.props {
            let v = match &pi.default {
                None => None,
                Some(e) => {
                    let decl = self.class_by_name(&pi.class).unwrap_or_else(|| cls.clone());
                    let v = self.eval_const_expr(e, Some(&decl))?;
                    if pi.ty.is_some() {
                        Some(self.coerce_prop(None, pi, v)?)
                    } else {
                        Some(v)
                    }
                }
            };
            out.push(v);
        }
        *cls.defaults.borrow_mut() = Some(out.clone());
        Ok(out)
    }

    /// `object_init_ex`: creates an instance without calling the constructor.
    pub(crate) fn instantiate(&mut self, cls: &Rc<Class>) -> VmResult<Obj> {
        if cls.is_interface() {
            return Err(self.make_throw(
                "Error",
                &format!("Cannot instantiate interface {}", cls.name),
            ));
        }
        if cls.is_trait() {
            return Err(self.make_throw("Error", &format!("Cannot instantiate trait {}", cls.name)));
        }
        if cls.is_enum() {
            return Err(self.make_throw("Error", &format!("Cannot instantiate enum {}", cls.name)));
        }
        if cls.flags & cflags::ABSTRACT != 0 {
            return Err(self.make_throw(
                "Error",
                &format!("Cannot instantiate abstract class {}", cls.name),
            ));
        }
        if cls.lc_name.as_bytes() == b"generator" {
            // `zend_generator_get_constructor`.
            return Err(self.make_throw(
                "Error",
                "The \"Generator\" class is reserved for internal use and cannot be manually instantiated",
            ));
        }
        if cls.flags & cflags::NO_NEW != 0 {
            return Err(self.make_throw(
                "Error",
                &format!("Instantiation of class {} is not allowed", cls.name),
            ));
        }
        self.instantiate_unchecked(cls)
    }

    /// Creates an instance without the instantiability checks.
    pub(crate) fn instantiate_unchecked(&mut self, cls: &Rc<Class>) -> VmResult<Obj> {
        let slots = self.class_defaults(cls)?;
        let o = object::new_object(cls.clone(), slots);
        if cls.internal == InternalKind::Throwable {
            self.init_throwable(&o)?;
        }
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.create) {
            f(self, &o)?;
        }
        Ok(o)
    }

    /// `(object)` cast of an array.
    pub(crate) fn array_to_object(&mut self, a: &ZArray) -> VmResult<Value> {
        let o = self.create_std_object_vm()?;
        if let Value::Object(obj) = &o {
            let t = a.to_proptable();
            *object::data(obj).dynamic.borrow_mut() = Some(t);
        }
        Ok(o)
    }

    /// A new `stdClass` instance.
    pub(crate) fn create_std_object_vm(&mut self) -> VmResult<Value> {
        let cls = self
            .classes
            .get(&ZStr::from("stdclass"))
            .cloned()
            .expect("stdClass");
        Ok(Value::Object(object::new_object(cls, Vec::new())))
    }

    /// The property table with mangled names (`(array)` cast, var_dump).
    pub(crate) fn object_prop_table(
        &mut self,
        o: &Obj,
        include_uninit: bool,
    ) -> Vec<(ArrayKey, Option<Value>, Option<Rc<PropInfo>>)> {
        let cls = object::class_of(o).clone();
        let d = object::data(o);
        let slots = d.slots.borrow();
        let mut out = Vec::new();
        for pi in &cls.props {
            if pi.is_virtual() {
                continue;
            }
            let v = slots.get(pi.slot as usize).cloned().flatten();
            if v.is_none() && !(include_uninit && pi.ty.is_some()) {
                continue;
            }
            let key = mangle(pi);
            out.push((key, v, Some(pi.clone())));
        }
        drop(slots);
        if let Some(t) = d.dynamic.borrow().as_ref() {
            for (k, v) in t.iter() {
                out.push((k.clone(), Some(v.clone()), None));
            }
        }
        out
    }

    /// `(array) $obj`.
    pub(crate) fn object_to_array(&mut self, o: &Obj) -> VmResult<ZArray> {
        // `convert_to_array`: a Closure becomes `[closure]`.
        if object::class_of(o).lc_name.as_bytes() == b"closure" {
            return Ok(ZArray::from_values([Value::Object(o.clone())]));
        }
        // Native `get_properties_for(ZEND_PROP_PURPOSE_ARRAY_CAST)`.
        let hook = object::class_of(o)
            .hooks
            .as_ref()
            .and_then(|h| h.properties_for);
        if let Some(f) = hook {
            if let Some(t) = f(self, o, php_runtime::PropertyPurpose::ArrayCast)? {
                return Ok(t);
            }
        }
        // `zend_get_properties_no_lazy_init`: no initialization here.
        if let Some(a) = self.internal_object_array(o)? {
            return Ok(a);
        }
        let mut t = HashTable::new();
        for (k, v, _) in self.object_prop_table(o, false) {
            let Some(v) = v else { continue };
            let k = match k {
                ArrayKey::Str(s) => ArrayKey::from_zstr(&s),
                k => k,
            };
            t.insert(k, v);
        }
        Ok(ZArray::from_table(t))
    }

    /// Clones an object (`clone $o`).
    pub(crate) fn clone_value(&mut self, v: Value) -> VmResult<Value> {
        let Value::Object(o) = v else {
            return Err(self.make_throw("Error", "__clone method called on non-object"));
        };
        // Lazy objects initialize first; proxies clone the real instance.
        if let Some(v) = crate::reflection::lazy::clone_lazy(self, &o)? {
            return Ok(v);
        }
        let cls = object::class_of(&o).clone();
        if cls.is_enum() {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Trying to clone an uncloneable object of class {}",
                    cls.name
                ),
            ));
        }
        if cls.flags & cflags::NO_CLONE != 0 {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Trying to clone an uncloneable object of class {}",
                    object::display_name(&cls.name)
                ),
            ));
        }
        if let Some(m) = &cls.magic.clone {
            let scope = self.scope();
            if !self.can_access_method(m, scope.as_ref()) {
                let vis = if m.visibility() == flags::PRIVATE {
                    "private"
                } else {
                    "protected"
                };
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Call to {vis} {}::__clone() from {}",
                        object::display_name(&cls.name),
                        scope_display(scope.as_ref())
                    ),
                ));
            }
        }
        let d = object::data(&o);
        let slots: Vec<Option<Value>> = d
            .slots
            .borrow()
            .iter()
            .map(|s| {
                s.as_ref().map(|v| match v {
                    Value::Reference(r) if r.refcount() == 1 => r.get(),
                    v => v.clone(),
                })
            })
            .collect();
        let n = object::new_object(cls.clone(), slots);
        let nd = object::data(&n);
        *nd.dynamic.borrow_mut() = d.dynamic.borrow().clone();
        self.clone_internal(&o, &n)?;
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.clone) {
            f(self, &o, &n)?;
        }
        if let Some(m) = cls.magic.clone.clone() {
            nd.flags.set(nd.flags.get() | oflags::CLONING);
            let r = self.call_method(&n, &m, Vec::new());
            nd.flags.set(nd.flags.get() & !oflags::CLONING);
            r?;
        }
        Ok(Value::Object(n))
    }
}

/// The mangled property-table key of a declared property.
pub(crate) fn mangle(pi: &PropInfo) -> ArrayKey {
    match pi.visibility() {
        flags::PRIVATE => {
            let mut v = vec![0u8];
            v.extend_from_slice(pi.class.as_bytes());
            v.push(0);
            v.extend_from_slice(pi.name.as_bytes());
            ArrayKey::Str(ZStr::from(v))
        }
        flags::PROTECTED => {
            let mut v = b"\0*\0".to_vec();
            v.extend_from_slice(pi.name.as_bytes());
            ArrayKey::Str(ZStr::from(v))
        }
        _ => ArrayKey::Str(pi.name.clone()),
    }
}

fn parent_backing_get(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let o = a.this_obj();
    let name = crate::builtins::util::s(a, 0);
    let cls = object::class_of(&o).clone();
    if let Some(pi) = cls.prop_map.get(&name).cloned() {
        if pi.is_virtual() {
            return Err(vm.virtual_error(&o, &pi, "Must not read from virtual property"));
        }
        let v = object::data(&o).slots.borrow()[pi.slot as usize].clone();
        return match v {
            Some(v) => Ok(v.deref()),
            None => Err(vm.make_throw(
                "Error",
                &format!(
                    "Typed property {}::${} must not be accessed before initialization",
                    pi.class, pi.name
                ),
            )),
        };
    }
    Ok(Value::Null)
}

fn parent_backing_set(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let o = a.this_obj();
    let name = crate::builtins::util::s(a, 0);
    let v = a.at(1);
    let cls = object::class_of(&o).clone();
    if let Some(pi) = cls.prop_map.get(&name).cloned() {
        if pi.is_virtual() {
            return Err(vm.virtual_error(&o, &pi, "Must not write to virtual property"));
        }
        let v = vm.coerce_prop(Some(&o), &pi, v)?;
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(v.clone());
        return Ok(v);
    }
    Ok(Value::Null)
}

impl Vm {
    /// `parent::$prop::get()` / `::set()`.
    pub(crate) fn op_init_parent_hook(&mut self, prop: ZStr, set: bool) -> VmResult<()> {
        let scope = self.scope();
        let this = self.frame().this.clone();
        let parent = scope.as_ref().and_then(|s| s.parent.clone());
        let pi = parent.as_ref().and_then(|p| p.prop_map.get(&prop).cloned());
        let Some(pi) = pi else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Undefined property {}::${}",
                    parent.map(|p| p.name.to_string_lossy()).unwrap_or_default(),
                    prop
                ),
            ));
        };
        let hook = pi
            .hooks
            .as_ref()
            .and_then(|h| if set { h.set.clone() } else { h.get.clone() });
        let call = match hook {
            Some(f) => {
                let decl = self.class_by_name(f.class_name.as_ref().unwrap_or(&pi.class));
                crate::vm::PendingCall {
                    this,
                    scope: decl,
                    called: self.frame().called.clone(),
                    ..crate::vm::PendingCall::new(crate::func::Func::User(f))
                }
            }
            None => {
                let sig = if set {
                    "set(string $name, mixed $value): mixed"
                } else {
                    "get(string $name): mixed"
                };
                let s = crate::builtins::sig::parse_sig(sig);
                let b = crate::func::Builtin {
                    name: ZStr::from(format!("${prop}::{}", if set { "set" } else { "get" })),
                    params: s.params,
                    defaults: s.defaults,
                    required: s.required,
                    return_type: None,
                    flags: 0,
                    handler: if set {
                        parent_backing_set
                    } else {
                        parent_backing_get
                    },
                };
                let mut c = crate::vm::PendingCall {
                    this,
                    scope: scope.clone(),
                    ..crate::vm::PendingCall::new(crate::func::Func::Builtin(Rc::new(b)))
                };
                c.args.push(Some(Value::Str(prop)));
                c
            }
        };
        self.frame_mut().calls.push(call);
        Ok(())
    }
}
