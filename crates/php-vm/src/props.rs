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
/// Not a recursion guard: marks a readonly property of an object being
/// cloned that was already re-initialized (`IS_PROP_REINITABLE` cleared).
const G_REINIT: u8 = 4;

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
                // `is_protected_compatible_scope(prototype->ce, scope)`.
                let ok = scope.is_some_and(|s| match self.prop_proto_class(pi) {
                    Some(root) => s.is_subclass_of(&root) || root.is_subclass_of(s),
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

    /// `prop_info->prototype->ce`: the class of the topmost non-private
    /// declaration of an instance property.
    fn prop_proto_class(&self, pi: &PropInfo) -> Option<Rc<Class>> {
        let mut root = self.class_by_name(&pi.class)?;
        while let Some(p) = root.parent.clone() {
            if p.prop_map
                .get(&pi.name)
                .is_some_and(|x| x.visibility() != flags::PRIVATE && x.flags & flags::STATIC == 0)
            {
                root = p;
            } else {
                break;
            }
        }
        Some(root)
    }

    fn guard_active(&self, o: &Obj, name: &ZStr, kind: u8) -> bool {
        if self.prop_guards.is_empty() {
            return false;
        }
        self.prop_guards.contains(&(o.handle(), name.clone(), kind))
            || object::data(o).lazy.borrow().as_ref().is_some_and(|info| {
                info.proxy
                    && info.initialized
                    && info.instance.as_ref().is_some_and(|instance| {
                        self.prop_guards
                            .contains(&(instance.handle(), name.clone(), kind))
                    })
            })
            || self
                .frames
                .iter()
                .filter_map(|f| f.this.as_ref())
                .any(|this| {
                    self.prop_guards
                        .contains(&(this.handle(), name.clone(), kind))
                        && object::data(this)
                            .lazy
                            .borrow()
                            .as_ref()
                            .is_some_and(|info| {
                                info.proxy
                                    && info.initialized
                                    && info
                                        .instance
                                        .as_ref()
                                        .is_some_and(|instance| Obj::ptr_eq(instance, o))
                            })
                })
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
            // `zend_should_call_hook`: the hook's `$this`, or the real
            // instance of an initialized lazy proxy.
            Some(n) => {
                *n == pi.name
                    && f.this.as_ref().is_some_and(|t| {
                        Obj::ptr_eq(t, o)
                            || object::data(t).lazy.borrow().as_ref().is_some_and(|i| {
                                i.proxy
                                    && i.initialized
                                    && i.instance.as_ref().is_some_and(|x| Obj::ptr_eq(x, o))
                            })
                    })
            }
            None => false,
        }
    }

    /// The object holding the backing value of hooked property `pi`
    /// (`lazy_object_for` leaves hooked properties alone): a lazy slot
    /// initializes the object first, an initialized proxy forwards to its
    /// real instance.
    fn lazy_backing(&mut self, o: &Obj, pi: &PropInfo) -> VmResult<Obj> {
        let lazy = object::data(o).lazy.borrow().as_ref().is_some_and(|i| {
            if i.initialized {
                i.proxy
            } else {
                !i.initializing && i.lazy_slots.get(pi.slot as usize).copied().unwrap_or(false)
            }
        });
        if lazy {
            self.lazy_object_for(o, None)
        } else {
            Ok(o.clone())
        }
    }

    pub(crate) fn call_hook(
        &mut self,
        o: &Obj,
        f: &Rc<crate::func::UserFunction>,
        pi: &PropInfo,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        // Hooks imported from a trait run in the scope of the using class.
        let decl = self
            .class_by_name(f.class_name.as_ref().unwrap_or(&pi.class))
            .filter(|c| !c.is_trait())
            .or_else(|| self.class_by_name(&pi.class))
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

    /// Initialized plain public slots need no lazy, native or visibility dispatch.
    fn plain_property_slot(o: &Obj, name: &ZStr, write: bool) -> Option<usize> {
        let data = object::data(o);
        let cls = &data.class;
        let pi = cls.prop_map.get(name)?;
        if pi.flags != flags::PUBLIC || pi.hooks.is_some() || (write && pi.ty.is_some()) {
            return None;
        }
        if cls.parent.is_some()
            || cls.hooks.is_some()
            || cls.magic.get.is_some()
            || cls.magic.set.is_some()
            || cls.magic.isset.is_some()
            || cls.magic.unset.is_some()
            || data.lazy.borrow().is_some()
            || data.dynamic.borrow().is_some()
        {
            return None;
        }
        let slot = pi.slot as usize;
        match data.slots.borrow().get(slot) {
            Some(Some(value)) if !matches!(value, Value::Reference(_)) => Some(slot),
            _ => None,
        }
    }

    /// Reads `$o->name`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn read_property(
        &mut self,
        o: &Obj,
        name: &ZStr,
        mode: FetchMode,
    ) -> VmResult<Value> {
        if let Some(slot) = Self::plain_property_slot(o, name, false) {
            return Ok(object::data(o).slots.borrow()[slot]
                .clone()
                .expect("initialized slot"));
        }
        if object::data(o).lazy.borrow().is_some()
            && !object::class_of(o).prop_map.contains_key(name)
            && !object::data(o)
                .dynamic
                .borrow()
                .as_ref()
                .is_some_and(|table| table.contains_key(&ArrayKey::Str(name.clone())))
            && self.has_read_magic(o, name, mode)
        {
            return self.magic_read(o, name, mode);
        }
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
                let backing;
                let o = if pi.hooks.is_some() {
                    backing = self.lazy_backing(o, &pi)?;
                    &backing
                } else {
                    o
                };
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
                if (pi.ty.is_none() || was_unset) && self.has_read_magic(o, name, mode) {
                    let mut v = self.magic_read(o, name, mode)?;
                    if mode != FetchMode::Is {
                        self.verify_magic_get_result(&pi, &mut v)?;
                    }
                    return Ok(v);
                }
                if pi.ty.is_some() {
                    if mode == FetchMode::Is {
                        return Ok(Value::Null);
                    }
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Typed property {}::${} must not be accessed before initialization",
                            pi.class, pi.name
                        ),
                    ));
                }
                if mode == FetchMode::Read {
                    self.undefined_property_warning(o, name)?;
                }
                Ok(Value::Null)
            }
            PropLookup::Inaccessible(pi) => {
                if self.has_read_magic(o, name, mode) {
                    return self.magic_read(o, name, mode);
                }
                if mode == FetchMode::Is {
                    return Ok(Value::Null);
                }
                Err(self.inaccessible_error(&cls, &pi, "access"))
            }
            PropLookup::NotFound => {
                let silent = mode == FetchMode::Is || cls.magic.get.is_some();
                if self.undeclared_prop_check(&cls, name, silent)? {
                    if self.has_read_magic(o, name, mode) {
                        return self.magic_read(o, name, mode);
                    }
                    if mode == FetchMode::Is {
                        return Ok(Value::Null);
                    }
                    self.undeclared_prop_check(&cls, name, false)?;
                    return Ok(Value::Null);
                }
                let v = object::data(o)
                    .dynamic
                    .borrow()
                    .as_ref()
                    .and_then(|t| t.get(&ArrayKey::Str(name.clone())).cloned());
                if let Some(v) = v {
                    return Ok(v.deref());
                }
                if self.has_read_magic(o, name, mode) {
                    return self.magic_read(o, name, mode);
                }
                if mode == FetchMode::Read {
                    self.undefined_property_warning(o, name)?;
                }
                Ok(Value::Null)
            }
        }
    }

    /// Whether a read may go to `__get()` (or, for `isset`-style fetches,
    /// to `__isset()`).
    fn has_read_magic(&self, o: &Obj, name: &ZStr, mode: FetchMode) -> bool {
        self.has_magic(o, name, G_GET)
            || (mode == FetchMode::Is && self.has_magic(o, name, G_ISSET))
    }

    /// `zend_get_property_offset` for a name without a declared instance
    /// property: a mangled name (`"\0..."`) or a static property of that
    /// name. Accessible statics give the "Accessing static property ... as
    /// non static" notice (unless `silent`) and are then treated as
    /// dynamic; returns `true` for a "wrong" offset (magic methods take
    /// over, or the caller reports it by calling again with `silent` off,
    /// which throws).
    fn undeclared_prop_check(
        &mut self,
        cls: &Rc<Class>,
        name: &ZStr,
        silent: bool,
    ) -> VmResult<bool> {
        if name.as_bytes().first() == Some(&0) {
            if silent {
                return Ok(true);
            }
            return Err(self.make_throw("Error", "Cannot access property starting with \"\\0\""));
        }
        let scope = self.scope();
        // `zend_get_parent_private_property`: the scope's own private
        // static wins over what the object's class sees.
        let parent_private = scope.as_ref().and_then(|s| {
            if Rc::ptr_eq(s, cls) || !cls.is_subclass_of(s) {
                return None;
            }
            s.find_static(name.as_bytes())
                .filter(|p| {
                    p.flags & flags::PRIVATE != 0 && p.class.eq_ignore_ascii_case(s.name.as_bytes())
                })
                .map(|p| (p.flags, p.class.clone()))
        });
        let Some((sflags, sclass)) = parent_private.or_else(|| {
            cls.find_static(name.as_bytes())
                .map(|p| (p.flags, p.class.clone()))
        }) else {
            return Ok(false);
        };
        let in_scope = scope
            .as_ref()
            .is_some_and(|s| s.name.eq_ignore_ascii_case(sclass.as_bytes()));
        let accessible = match sflags & flags::VISIBILITY {
            flags::PRIVATE => {
                if !in_scope && !sclass.eq_ignore_ascii_case(cls.name.as_bytes()) {
                    // A parent's private static: plain dynamic property.
                    return Ok(false);
                }
                in_scope
            }
            flags::PROTECTED => {
                in_scope
                    || scope.as_ref().is_some_and(|s| {
                        self.class_by_name(&sclass)
                            .is_some_and(|d| s.is_subclass_of(&d) || d.is_subclass_of(s))
                    })
            }
            _ => true,
        };
        if !accessible {
            if silent {
                return Ok(true);
            }
            let vis = if sflags & flags::PRIVATE != 0 {
                "private"
            } else {
                "protected"
            };
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot access {vis} property {}::${}",
                    object::display_name(&cls.name),
                    name
                ),
            ));
        }
        if !silent {
            self.notice(&format!(
                "Accessing static property {}::${} as non static",
                object::display_name(&cls.name),
                name
            ))?;
        }
        Ok(false)
    }

    /// `__isset()` may have written the property (GH-12695): its current
    /// value, if it now exists.
    fn materialised_prop(&self, o: &Obj, name: &ZStr) -> Option<Value> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) if !pi.is_virtual() => object::data(o).slots.borrow()
                [pi.slot as usize]
                .as_ref()
                .map(Value::deref),
            PropLookup::NotFound => object::data(o)
                .dynamic
                .borrow()
                .as_ref()
                .and_then(|t| t.get(&ArrayKey::Str(name.clone())).map(Value::deref)),
            _ => None,
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
            if let Some(v) = self.materialised_prop(o, name) {
                return Ok(v);
            }
        }
        if !self.has_magic(o, name, G_GET) {
            return Ok(Value::Null);
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
            if let Some(root) = self.prop_proto_class(pi) {
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
                pi.class,
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
        if pi.flags & flags::READONLY != 0 && initialized && self.readonly_locked(o, pi) {
            let msg = if op == "unset" {
                format!("Cannot unset readonly property {}::${}", pi.class, pi.name)
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

    /// Whether an initialized readonly property may not be modified: only
    /// while cloning (`__clone()`, clone-with) is each one reinitializable,
    /// once (`IS_PROP_REINITABLE`).
    pub(crate) fn readonly_locked(&self, o: &Obj, pi: &PropInfo) -> bool {
        object::data(o).flags.get() & oflags::CLONING == 0
            || self
                .prop_guards
                .contains(&(o.handle(), pi.name.clone(), G_REINIT))
    }

    /// Clears `IS_PROP_REINITABLE` after a readonly property was written
    /// during cloning.
    fn consume_reinit(&mut self, o: &Obj, pi: &PropInfo) {
        if pi.flags & flags::READONLY != 0 && object::data(o).flags.get() & oflags::CLONING != 0 {
            self.prop_guards
                .insert((o.handle(), pi.name.clone(), G_REINIT));
        }
    }

    /// Forgets the reinitialization markers of an object.
    fn clear_reinit(&mut self, o: &Obj) {
        if object::class_of(o).props.iter().any(|p| p.is_readonly()) {
            let h = o.handle();
            self.prop_guards.retain(|k| !(k.0 == h && k.2 == G_REINIT));
        }
    }

    /// Coerces a value for a typed property.
    pub(crate) fn coerce_prop(
        &mut self,
        o: Option<&Obj>,
        pi: &PropInfo,
        v: Value,
    ) -> VmResult<Value> {
        if pi.ty.is_none() {
            return Ok(v);
        }
        let strict = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .is_some_and(|f| f.code.strict_types);
        self.coerce_prop_with(o, pi, v, strict)
    }

    /// [`Vm::coerce_prop`] with an explicit `strict_types` mode
    /// (`zend_verify_property_type(.., strict)`).
    fn coerce_prop_with(
        &mut self,
        o: Option<&Obj>,
        pi: &PropInfo,
        mut v: Value,
        strict: bool,
    ) -> VmResult<Value> {
        let Some(ty) = &pi.ty else {
            return Ok(v);
        };
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
        let tyname = ty.display(None, None, None);
        Err(self.make_throw(
            "TypeError",
            &format!(
                "Cannot assign {} to property {}::${} of type {tyname}",
                given_type(&v),
                pi.class,
                pi.name
            ),
        ))
    }

    /// `object_properties_load`: copies the members of an `__unserialize()`
    /// payload into an object (the SPL containers' extra properties).
    /// Declared properties are type-checked strictly; other keys become
    /// dynamic properties, with the "Creation of dynamic property"
    /// deprecation even when the property already exists.
    ///
    /// # Errors
    /// The `TypeError`/`Error` of a rejected member (earlier members stay
    /// loaded), or a throwing deprecation handler.
    pub fn object_properties_load(&mut self, o: &Obj, props: &ZArray) -> VmResult<()> {
        let cls = object::class_of(o).clone();
        for (k, v) in props.iter() {
            let declared = match k {
                ArrayKey::Str(s) => Self::load_target(&cls, s),
                ArrayKey::Int(_) => None,
            };
            if let Some(pi) = declared {
                self.load_declared(o, &cls, &pi, v)?;
                continue;
            }
            let shown = match k {
                ArrayKey::Str(s) => {
                    ZStr::from(php_runtime::dump::unmangle_property_name(s.as_bytes()).1)
                }
                ArrayKey::Int(i) => ZStr::from(i.to_string()),
            };
            if cls.flags & cflags::NO_DYNAMIC_PROPERTIES != 0 {
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Cannot create dynamic property {}::${}",
                        object::display_name(&cls.name),
                        shown.to_string_lossy()
                    ),
                ));
            }
            if cls.flags & cflags::ALLOW_DYNAMIC == 0 {
                self.deprecated(&format!(
                    "Creation of dynamic property {}::${} is deprecated",
                    object::display_name(&cls.name),
                    shown.to_string_lossy()
                ))?;
            }
            let key = match k {
                ArrayKey::Str(s) => s.clone(),
                ArrayKey::Int(i) => ZStr::from(i.to_string()),
            };
            self.write_dynamic_prop(o, key, v.clone());
        }
        Ok(())
    }

    /// The declared, non-static property a (possibly mangled) member name
    /// of [`Vm::object_properties_load`] denotes.
    fn load_target(cls: &Rc<Class>, key: &ZStr) -> Option<Rc<PropInfo>> {
        let (cls_part, unmangled) = php_runtime::dump::unmangle_property_name(key.as_bytes());
        let n = ZStr::from(unmangled);
        match cls_part {
            Some(c) if c != b"*" => cls
                .props
                .iter()
                .find(|p| {
                    p.name == n
                        && p.class.as_bytes().eq_ignore_ascii_case(c)
                        && p.visibility() == flags::PRIVATE
                })
                .cloned(),
            _ => cls
                .prop_map
                .get(&n)
                .filter(|p| p.flags & flags::STATIC == 0)
                .cloned(),
        }
    }

    /// One declared member of [`Vm::object_properties_load`].
    fn load_declared(
        &mut self,
        o: &Obj,
        cls: &Rc<Class>,
        pi: &PropInfo,
        v: &Value,
    ) -> VmResult<()> {
        if pi.is_virtual() {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot unserialize value for virtual property {}::${}",
                    object::display_name(&cls.name),
                    pi.name
                ),
            ));
        }
        let mut val = v.clone();
        if pi.ty.is_some() {
            let is_ref = matches!(v, Value::Reference(_));
            let initialized = object::data(o)
                .slots
                .borrow()
                .get(pi.slot as usize)
                .is_some_and(Option::is_some);
            if pi.is_readonly() && (is_ref || initialized) {
                let msg = if is_ref {
                    "Cannot indirectly modify readonly property"
                } else {
                    "Cannot modify readonly property"
                };
                return Err(self.make_throw("Error", &format!("{msg} {}::${}", pi.class, pi.name)));
            }
            let checked = self.coerce_prop_with(Some(o), pi, v.deref(), true)?;
            if is_ref {
                // `zend_verify_prop_assignable_by_ref` checks the value; the
                // reference itself is stored.
                drop(checked);
            } else {
                val = checked;
            }
        }
        let old = object::data(o)
            .slots
            .borrow_mut()
            .get_mut(pi.slot as usize)
            .map(|slot| slot.replace(val));
        drop(old);
        Ok(())
    }

    /// Writes `$o->name = v`; returns the assigned value.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn write_property(&mut self, o: &Obj, name: &ZStr, v: Value) -> VmResult<Value> {
        if !matches!(v, Value::Reference(_)) {
            if let Some(slot) = Self::plain_property_slot(o, name, true) {
                let old = object::data(o).slots.borrow_mut()[slot].replace(v.clone());
                object::data(o).readonly_init.borrow_mut()[slot] = false;
                drop(old);
                return Ok(v);
            }
        }
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
                let backing;
                let o = if pi.hooks.is_some() {
                    backing = self.lazy_backing(o, &pi)?;
                    &backing
                } else {
                    o
                };
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
                let (typed_ref, v) = self.typed_prop_write(o, &pi, v)?;
                if let Some(f) = object::data(o).readonly_init.borrow_mut().get_mut(idx) {
                    *f = false;
                }
                self.consume_reinit(o, &pi);
                if let Some(r) = typed_ref {
                    return self.assign_ref(&r, v);
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
                if self.undeclared_prop_check(&cls, name, cls.magic.set.is_some())? {
                    if self.has_magic(o, name, G_SET) {
                        return self.magic_write(o, name, v);
                    }
                    self.undeclared_prop_check(&cls, name, false)?;
                    return Ok(v);
                }
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
                if let Some(r) = crate::typed_refs::dynamic_typed_ref(o, &key) {
                    return self.assign_ref(&r, v);
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
                cls.name,
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
        // The slot of the real instance (a lazy proxy forwards).
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        if !Obj::ptr_eq(o, &lazy_target)
            && !self
                .prop_guards
                .contains(&(o.handle(), name.clone(), G_GET))
            && self.guard_active(&lazy_target, name, G_GET)
            && !object::class_of(&lazy_target).prop_map.contains_key(name)
            && !object::data(&lazy_target)
                .dynamic
                .borrow()
                .as_ref()
                .is_some_and(|table| table.contains_key(&ArrayKey::Str(name.clone())))
        {
            return self
                .read_property(&lazy_target, name, FetchMode::Read)
                .map(PhpRef::new);
        }
        let o = &lazy_target;
        if let Some(r) = self.uninit_typed_prop_ref(o, name)? {
            return Ok(r);
        }
        match self.property_container(o, name, false)? {
            PropContainer::Declared(idx) => {
                let r = {
                    let d = object::data(o);
                    let mut slots = d.slots.borrow_mut();
                    let slot = slots[idx].get_or_insert(Value::Null);
                    slot.make_ref()
                };
                self.add_slot_type_source(o, idx, &r);
                Ok(r)
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

    /// The storage of a property for nested writes (`$o->a[] = 1`): the
    /// `BP_VAR_W` (or, with `rw`, `BP_VAR_RW`) fetch of
    /// `zend_fetch_property_address` — `get_property_ptr_ptr`, falling back
    /// to `read_property` for hooks, `__get()` and restricted properties.
    pub(crate) fn property_container(
        &mut self,
        o: &Obj,
        name: &ZStr,
        rw: bool,
    ) -> VmResult<PropContainer> {
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                if let Some(h) = pi.hooks.clone() {
                    if !self.in_hook(&pi, o) {
                        return self.hooked_container(o, &pi, &h);
                    }
                    if pi.is_virtual() {
                        return Err(self.virtual_error(
                            o,
                            &pi,
                            "Must not read from virtual property",
                        ));
                    }
                }
                let idx = pi.slot as usize;
                let defined = object::data(o).slots.borrow()[idx].is_some();
                let uninit = !defined && pi.ty.is_some() && !self.slot_was_unset(o, idx);
                let magic = !defined && !uninit && self.has_magic(o, name, G_GET);
                if !defined && !magic && rw {
                    if pi.ty.is_some() {
                        return Err(self.uninit_prop_error(&pi));
                    }
                    self.undefined_property_warning(o, name)?;
                    let d = object::data(o);
                    let mut slots = d.slots.borrow_mut();
                    if slots[idx].is_none() {
                        slots[idx] = Some(Value::Null);
                    }
                }
                // `read_property` with a write fetch: objects are handed
                // out as a copy (they may be modified), anything else may
                // not be modified indirectly.
                if self.write_restricted(&pi) {
                    let cur = object::data(o).slots.borrow()[idx]
                        .as_ref()
                        .map(Value::deref);
                    if let Some(Value::Object(x)) = cur {
                        return Ok(PropContainer::Value(Value::Object(x)));
                    }
                    return Err(self.indirect_modification_error(&pi));
                }
                if magic {
                    let mut c = self.magic_container(o, name)?;
                    if let PropContainer::Value(v) = &mut c {
                        self.verify_magic_get_result(&pi, v)?;
                    }
                    return Ok(c);
                }
                if !defined {
                    if let Some(ty) = &pi.ty {
                        if ty.mask & crate::types::mask::ARRAY == 0 && !ty.is_mixed() {
                            return Err(self.uninit_prop_error(&pi));
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
                if self.undeclared_prop_check(&cls, name, cls.magic.get.is_some())? {
                    if self.has_magic(o, name, G_GET) {
                        return self.magic_container(o, name);
                    }
                    self.undeclared_prop_check(&cls, name, false)?;
                }
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

    /// Whether the current scope may not modify `pi` in place (readonly,
    /// or set visibility denying access).
    fn write_restricted(&self, pi: &PropInfo) -> bool {
        pi.flags & flags::READONLY != 0
            || (pi.flags & flags::SET_MASK != 0 && !self.has_set_access(pi))
    }

    /// `zend_readonly_property_indirect_modification_error` /
    /// the "indirectly modify" set-visibility error.
    fn indirect_modification_error(&mut self, pi: &PropInfo) -> crate::vm::Throw {
        if pi.flags & flags::READONLY != 0 {
            return self.make_throw(
                "Error",
                &format!(
                    "Cannot indirectly modify readonly property {}::${}",
                    object::display_name(&pi.class),
                    pi.name
                ),
            );
        }
        let cn = pi.class.clone();
        self.asym_error(&cn, pi, "indirectly modify")
    }

    /// "Typed property `C::$p` must not be accessed before initialization".
    fn uninit_prop_error(&mut self, pi: &PropInfo) -> crate::vm::Throw {
        self.make_throw(
            "Error",
            &format!(
                "Typed property {}::${} must not be accessed before initialization",
                pi.class, pi.name
            ),
        )
    }

    /// A write fetch of a hooked property from outside its hooks
    /// (`zend_std_read_property` with `BP_VAR_W`/`BP_VAR_RW`): the `get`
    /// hook's result (or the backing value without one) must be a
    /// reference or an object.
    fn hooked_container(
        &mut self,
        o: &Obj,
        pi: &PropInfo,
        h: &crate::class::PropHooks,
    ) -> VmResult<PropContainer> {
        let v = self.hooked_write_fetch(o, pi, h)?;
        match v {
            Value::Object(_) | Value::Reference(_) => Ok(PropContainer::Value(v)),
            _ => Err(self.make_throw(
                "Error",
                &format!(
                    "Indirect modification of {}::${} is not allowed",
                    object::display_name(&object::class_of(o).name),
                    pi.name
                ),
            )),
        }
    }

    /// The value a write fetch of a hooked property sees: the `get` hook's
    /// result, or the backing value when there is no `get` hook.
    fn hooked_write_fetch(
        &mut self,
        o: &Obj,
        pi: &PropInfo,
        h: &crate::class::PropHooks,
    ) -> VmResult<Value> {
        Ok(match &h.get {
            Some(g) => self.call_hook(o, g, pi, Vec::new())?,
            None if pi.is_virtual() => {
                return Err(self.virtual_error(
                    o,
                    pi,
                    "Cannot read from set-only virtual property",
                ));
            }
            None => {
                let b = self.lazy_backing(o, pi)?;
                let cur = object::data(&b).slots.borrow()[pi.slot as usize].clone();
                match cur {
                    Some(v) => v.deref(),
                    None => return Err(self.uninit_prop_error(pi)),
                }
            }
        })
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

    /// `ZEND_FE_FETCH_RW` over an object: an initialized readonly property
    /// that is not a reference yet cannot be iterated by reference.
    pub(crate) fn foreach_readonly_ref_check(&mut self, o: &Obj, name: &ZStr) -> VmResult<()> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let PropLookup::Found(pi) = self.lookup_prop(&cls, name, scope.as_ref()) else {
            return Ok(());
        };
        if !pi.is_readonly() || pi.hooks.is_some() {
            return Ok(());
        }
        let plain = matches!(
            object::data(o).slots.borrow().get(pi.slot as usize),
            Some(Some(v)) if !matches!(v, Value::Reference(_))
        );
        if !plain {
            return Ok(());
        }
        Err(self.make_throw(
            "Error",
            &format!(
                "Cannot acquire reference to readonly property {}::${}",
                object::display_name(&pi.class),
                pi.name
            ),
        ))
    }

    /// `$o->name =& r`.
    pub(crate) fn assign_property_ref(
        &mut self,
        o: &Obj,
        name: &ZStr,
        r: Value,
    ) -> VmResult<Value> {
        if let Some(f) = object::class_of(o)
            .hooks
            .as_ref()
            .and_then(|h| h.get_property_ptr_ptr)
        {
            f(self, o, name)?;
        }
        let lazy_target = self.lazy_object_for(o, Some(name))?;
        let o = &lazy_target;
        if let Some(r) = self.assign_uninit_typed_prop_ref(o, name, &r)? {
            return Ok(r);
        }
        // A hooked property hands out a value (after running its `get`
        // hook), never its storage.
        if let Some((pi, h)) = self.hooked_outside_hook(o, name) {
            self.hooked_write_fetch(o, &pi, &h)?;
            return Err(self.make_throw("Error", "Cannot assign by reference to overloaded object"));
        }
        if let Some(idx) = self.uninit_backing_slot_in_hook(o, name) {
            self.bind_slot_ref(o, idx, &r)?;
            let old = object::data(o).slots.borrow_mut()[idx].replace(r.clone());
            drop(old);
            return Ok(r);
        }
        match self.property_container(o, name, false)? {
            PropContainer::Declared(idx) => {
                self.bind_slot_ref(o, idx, &r)?;
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
            // Hooks and `__get()` hand out a value, not the storage.
            PropContainer::Value(_) => {
                Err(self.make_throw("Error", "Cannot assign by reference to overloaded object"))
            }
        }
    }

    /// The accessible hooked property `name` when accessed from outside
    /// its hooks.
    fn hooked_outside_hook(
        &self,
        o: &Obj,
        name: &ZStr,
    ) -> Option<(Rc<PropInfo>, Rc<crate::class::PropHooks>)> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let PropLookup::Found(pi) = self.lookup_prop(&cls, name, scope.as_ref()) else {
            return None;
        };
        let h = pi.hooks.clone()?;
        (!self.in_hook(&pi, o)).then_some((pi, h))
    }

    /// The slot of an uninitialized typed backing value accessed from its
    /// own hook (`$this->prop = &$x` inside a hook binds it).
    fn uninit_backing_slot_in_hook(&self, o: &Obj, name: &ZStr) -> Option<usize> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let PropLookup::Found(pi) = self.lookup_prop(&cls, name, scope.as_ref()) else {
            return None;
        };
        if pi.hooks.is_none()
            || pi.is_virtual()
            || pi.ty.is_none()
            || pi.flags & (flags::READONLY | flags::SET_MASK) != 0
            || !self.in_hook(&pi, o)
        {
            return None;
        }
        let idx = pi.slot as usize;
        object::data(o).slots.borrow()[idx].is_none().then_some(idx)
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
                let defined = object::data(o).slots.borrow()[idx].is_some();
                // `IS_PROP_UNINIT`: never initialized nor unset since.
                let uninit = !defined && pi.ty.is_some() && !self.slot_was_unset(o, idx);
                // `zend_std_unset_property`: the readonly / set-visibility
                // checks apply unless `__unset()` takes over.
                if pi.flags & (flags::READONLY | flags::SET_MASK) != 0
                    && (defined || uninit || !self.has_magic(o, name, G_UNSET))
                {
                    self.check_write_access(o, &pi, defined, "unset")?;
                }
                if !defined && !uninit && self.has_magic(o, name, G_UNSET) {
                    return self.magic_unset(o, name);
                }
                // Unsetting clears `IS_PROP_UNINIT` (`__get()` & co. apply
                // from now on).
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
                if self.undeclared_prop_check(&cls, name, cls.magic.unset.is_some())? {
                    if self.has_magic(o, name, G_UNSET) {
                        return self.magic_unset(o, name);
                    }
                    self.undeclared_prop_check(&cls, name, false)?;
                    return Ok(());
                }
                let key = ArrayKey::Str(name.clone());
                let old = {
                    let d = object::data(o);
                    let mut dynp = d.dynamic.borrow_mut();
                    dynp.as_mut().and_then(|t| t.remove(&key))
                };
                if old.is_some() {
                    object::forget_dynamic_prop_order(object::data(o), &key);
                }
                if old.is_none() && self.has_magic(o, name, G_UNSET) {
                    return self.magic_unset(o, name);
                }
                drop(old);
                Ok(())
            }
        }
    }

    /// Whether declared slot `idx` was explicitly unset (the opposite of
    /// `IS_PROP_UNINIT` for an undefined slot).
    #[allow(clippy::unused_self)]
    fn slot_was_unset(&self, o: &Obj, idx: usize) -> bool {
        object::data(o)
            .readonly_init
            .borrow()
            .get(idx)
            .copied()
            .unwrap_or(false)
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
                if let Some(h) = pi.hooks.clone() {
                    if !self.in_hook(&pi, o) {
                        return match self.hooked_container(o, &pi, &h)? {
                            PropContainer::Value(v) => Ok(Some(v)),
                            _ => Ok(None),
                        };
                    }
                    if pi.is_virtual() {
                        return Err(self.virtual_error(
                            o,
                            &pi,
                            "Must not read from virtual property",
                        ));
                    }
                }
                let idx = pi.slot as usize;
                if self.write_restricted(&pi) {
                    let cur = object::data(o).slots.borrow()[idx]
                        .as_ref()
                        .map(Value::deref);
                    return match cur {
                        None => Ok(None),
                        Some(Value::Object(x)) => Ok(Some(Value::Object(x))),
                        Some(_) => Err(self.indirect_modification_error(&pi)),
                    };
                }
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
            PropLookup::Found(pi) => {
                if let Some(h) = pi.hooks.clone() {
                    match &h.get {
                        None if pi.is_virtual() => {
                            return Err(self.virtual_error(
                                o,
                                &pi,
                                "Cannot read from set-only virtual property",
                            ));
                        }
                        Some(g) if !self.in_hook(&pi, o) => {
                            let v = self.call_hook(o, g, &pi, Vec::new())?.deref();
                            return Ok(if empty {
                                !self.to_bool(&v)?
                            } else {
                                !v.is_null()
                            });
                        }
                        Some(_) if pi.is_virtual() => {
                            return Err(self.virtual_error(
                                o,
                                &pi,
                                "Must not read from virtual property",
                            ));
                        }
                        _ => {}
                    }
                }
                let idx = pi.slot as usize;
                let v = object::data(o).slots.borrow()[idx].clone();
                match v {
                    Some(v) => Some(Some(v.deref())),
                    // Uninitialized typed properties skip `__isset()`.
                    None if pi.ty.is_some() && !self.slot_was_unset(o, idx) => {
                        return Ok(empty);
                    }
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
            if let Some(v) = self.materialised_prop(o, name) {
                return Ok(!self.to_bool(&v)?);
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
        for constant in &cls.consts {
            if !constant.is_case && constant.value.borrow().is_none() {
                let scope = self
                    .class_by_name(&constant.class)
                    .unwrap_or_else(|| cls.clone());
                crate::reflection::with_scope(self, Some(scope), |vm| {
                    vm.update_class_constant(cls, &constant.name)
                })?;
            }
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
        // `zend_update_class_constants` then evaluates the static property
        // initializers.
        for sp in &cls.static_props {
            if sp.initialized.get() {
                continue;
            }
            if let Some(d) = &sp.default {
                let decl = self.class_by_name(&sp.class).unwrap_or_else(|| cls.clone());
                let v = self.eval_const_expr(d, Some(&decl))?;
                *sp.cell.borrow_mut() = v;
            }
            sp.initialized.set(true);
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
        if let Some(instance) = crate::reflection::lazy::proxy_instance(o) {
            return self.object_prop_table(&instance, include_uninit);
        }
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
        if let Some(order) = object::prop_order(d) {
            order.sort(&mut out, |(k, _, pi)| (pi.as_ref().map(|p| p.slot), k));
        }
        out
    }

    /// `(array) $obj`.
    pub(crate) fn object_to_array(&mut self, o: &Obj) -> VmResult<ZArray> {
        if let Some(instance) = crate::reflection::lazy::proxy_instance(o) {
            return self.object_to_array(&instance);
        }
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
        let o = match v {
            Value::Object(o) => o,
            Value::Reference(r) => return self.clone_value(r.get()),
            other => {
                // `ZEND_CLONE`, kept in sync with the `clone()` function.
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "clone(): Argument #1 ($object) must be of type object, {} given",
                        other.value_name()
                    ),
                ));
            }
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
                        "Call to {vis} method {}::{}() from {}",
                        object::display_name(&m.scope().name),
                        m.name,
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
        self.add_object_type_sources(&n);
        let nd = object::data(&n);
        *nd.readonly_init.borrow_mut() = d.readonly_init.borrow().clone();
        *nd.dynamic.borrow_mut() = d.dynamic.borrow().clone();
        object::copy_prop_order(d, nd);
        self.clone_internal(&o, &n)?;
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.clone) {
            f(self, &o, &n)?;
        }
        // Markers left by an earlier object with the same handle (or by a
        // clone-with) must not leak into this clone.
        self.clear_reinit(&n);
        if let Some(m) = cls.magic.clone.clone() {
            self.with_reinitable_props(&n, |vm| vm.call_method(&n, &m, Vec::new()))?;
        }
        Ok(Value::Object(n))
    }

    /// Runs `f` with the readonly properties of `o` reinitializable once
    /// each (`zend_object_set_properties_reinitable(o, true)` … `false`), as
    /// around `__clone()` and `__unserialize()`.
    pub(crate) fn with_reinitable_props<T>(
        &mut self,
        o: &Obj,
        f: impl FnOnce(&mut Vm) -> VmResult<T>,
    ) -> VmResult<T> {
        let d = object::data(o);
        let was = d.flags.get() & oflags::CLONING;
        self.clear_reinit(o);
        d.flags.set(d.flags.get() | oflags::CLONING);
        let r = f(self);
        d.flags.set((d.flags.get() & !oflags::CLONING) | was);
        self.clear_reinit(o);
        r
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

/// The property name of a parent hook trampoline (`$prop::get`).
fn trampoline_prop(a: &crate::func::Args) -> ZStr {
    let Some(f) = &a.func else {
        return ZStr::from("");
    };
    let n = f.name.as_bytes();
    let n = n.strip_prefix(b"$").unwrap_or(n);
    let n = n
        .strip_suffix(b"::get")
        .or_else(|| n.strip_suffix(b"::set"))
        .unwrap_or(n);
    ZStr::from(n.to_vec())
}

fn parent_backing_get(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let o = a.this_obj();
    let name = trampoline_prop(a);
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
    let name = trampoline_prop(a);
    let v = a.at(0);
    let cls = object::class_of(&o).clone();
    if let Some(pi) = cls.prop_map.get(&name).cloned() {
        if pi.is_virtual() {
            return Err(vm.virtual_error(&o, &pi, "Must not write to virtual property"));
        }
        let v = vm.coerce_prop(Some(&o), &pi, v)?;
        let old = {
            let d = object::data(&o);
            let mut slots = d.slots.borrow_mut();
            match &mut slots[pi.slot as usize] {
                Some(Value::Reference(r)) => Some(r.set(v.clone())),
                slot => slot.replace(v.clone()),
            }
        };
        drop(old);
        return Ok(v);
    }
    Ok(Value::Null)
}

impl Vm {
    /// `parent::$prop::get()` / `::set()`.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn op_init_parent_hook(&mut self, prop: ZStr, set: bool) -> VmResult<()> {
        let scope = self.scope();
        let this = self.frame().this.clone();
        let Some(parent) = scope.as_ref().and_then(|s| s.parent.clone()) else {
            return Err(self.make_throw(
                "Error",
                "Cannot use \"parent\" when current class scope has no parent",
            ));
        };
        let Some(pi) = parent.prop_map.get(&prop).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Undefined property {}::${}",
                    object::display_name(&parent.name),
                    prop
                ),
            ));
        };
        if pi.visibility() == flags::PRIVATE {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot access private property {}::${}",
                    object::display_name(&parent.name),
                    prop
                ),
            ));
        }
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
                    "set(mixed $value): mixed"
                } else {
                    "get(): mixed"
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
                // `zend_get_property_hook_trampoline`: scoped to the
                // declaring class of the parent property.
                let decl = self.class_by_name(&pi.class).unwrap_or(parent);
                crate::vm::PendingCall {
                    this,
                    scope: Some(decl),
                    ..crate::vm::PendingCall::new(crate::func::Func::Builtin(Rc::new(b)))
                }
            }
        };
        self.frame_mut().calls.push(call);
        Ok(())
    }
}
