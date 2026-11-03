//! Class declaration and linking (`zend_do_inheritance`,
//! `zend_do_bind_traits`, interface implementation), class lookup with
//! autoloading, class constants and static properties.

use crate::class::{
    cflags, Class, ClassConst, ClassDecl, ClassKind, ConstExpr, InternalKind, Magic, PropInfo,
    StaticProp, TraitRule,
};
use crate::func::{flags, Func, Method, UserFunction};
use crate::object;
use crate::opcode::ClassSpec;
use crate::vm::{Frame, FrameKind, Throw, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// Builds magic-method shortcuts from a method table.
pub(crate) fn build_magic(methods: &HashMap<ZStr, Rc<Method>>) -> Magic {
    let g = |n: &str| methods.get(n.as_bytes()).cloned();
    Magic {
        construct: g("__construct"),
        destruct: g("__destruct"),
        get: g("__get"),
        set: g("__set"),
        isset: g("__isset"),
        unset: g("__unset"),
        call: g("__call"),
        call_static: g("__callstatic"),
        to_string: g("__tostring"),
        invoke: g("__invoke"),
        clone: g("__clone"),
        debug_info: g("__debuginfo"),
        serialize: g("__serialize"),
        unserialize: g("__unserialize"),
        sleep: g("__sleep"),
        wakeup: g("__wakeup"),
    }
}

impl Vm {
    /// Looks up a class, autoloading it if needed.
    pub fn lookup_class(&mut self, name: &ZStr) -> VmResult<Option<Rc<Class>>> {
        let bytes = name
            .as_bytes()
            .strip_prefix(b"\\")
            .unwrap_or(name.as_bytes());
        let lc = ZStr::from(bytes.to_ascii_lowercase());
        if let Some(c) = self.classes.get(&lc) {
            return Ok(Some(c.clone()));
        }
        self.autoload(&ZStr::from(bytes), &lc)?;
        Ok(self.classes.get(&lc).cloned())
    }

    /// Looks up a class without autoloading.
    pub fn find_class(&self, name: &[u8]) -> Option<Rc<Class>> {
        let bytes = name.strip_prefix(b"\\").unwrap_or(name);
        self.classes
            .get(&ZStr::from(bytes.to_ascii_lowercase()))
            .cloned()
    }

    fn autoload(&mut self, name: &ZStr, lc: &ZStr) -> VmResult<()> {
        if self.autoloaders.is_empty() || self.autoloading.contains(lc) {
            return Ok(());
        }
        if !is_valid_class_name(name.as_bytes()) {
            return Ok(());
        }
        self.autoloading.insert(lc.clone());
        let loaders = self.autoloaders.clone();
        let mut r = Ok(());
        for l in loaders {
            if let Err(e) = self.call_value(&l, vec![Value::Str(name.clone())]) {
                r = Err(e);
                break;
            }
            if self.classes.contains_key(lc) {
                break;
            }
        }
        self.autoloading.remove(lc);
        r
    }

    /// Looks up a class or throws `Class "X" not found`.
    pub(crate) fn lookup_class_or_error(&mut self, name: &ZStr) -> VmResult<Rc<Class>> {
        match self.lookup_class(name)? {
            Some(c) => Ok(c),
            None => Err(self.make_throw(
                "Error",
                &format!("Class \"{}\" not found", name.to_string_lossy()),
            )),
        }
    }

    /// Resolves a [`ClassSpec`] at runtime.
    pub(crate) fn resolve_class_spec(&mut self, spec: &ClassSpec) -> VmResult<Rc<Class>> {
        match spec {
            ClassSpec::Named(l) => {
                let slot = *l as usize;
                let cached = self
                    .frame()
                    .code
                    .class_cache
                    .borrow()
                    .get(slot)
                    .and_then(Weak::upgrade);
                if let Some(c) = cached {
                    return Ok(c);
                }
                let n = self.lit_str(*l);
                let c = self.lookup_class_or_error(&n)?;
                let mut cache = self.frame().code.class_cache.borrow_mut();
                if cache.len() <= slot {
                    cache.resize(slot + 1, Weak::new());
                }
                cache[slot] = Rc::downgrade(&c);
                Ok(c)
            }
            ClassSpec::SelfClass => {
                match self.scope() {
                    Some(c) => Ok(c),
                    None => Err(self
                        .make_throw("Error", "Cannot use \"self\" when no class scope is active")),
                }
            }
            ClassSpec::Parent => match self.scope() {
                Some(c) => match &c.parent {
                    Some(p) => Ok(p.clone()),
                    None => Err(self.make_throw(
                        "Error",
                        "Cannot use \"parent\" when current class scope has no parent",
                    )),
                },
                None => Err(self.make_throw(
                    "Error",
                    "Cannot use \"parent\" when no class scope is active",
                )),
            },
            ClassSpec::Static => {
                let c = self
                    .frames
                    .iter()
                    .rev()
                    .find(|f| f.kind != FrameKind::Builtin)
                    .and_then(|f| f.called.clone());
                match c {
                    Some(c) => Ok(c),
                    None => Err(self.make_throw(
                        "Error",
                        "Cannot use \"static\" when no class scope is active",
                    )),
                }
            }
            ClassSpec::Dyn(op) => {
                let v = self.read(*op)?;
                match v {
                    Value::Object(o) => Ok(object::class_of(&o).clone()),
                    Value::Str(s) => self.lookup_class_or_error(&s),
                    _ => {
                        Err(self
                            .make_throw("Error", "Class name must be a valid object or a string"))
                    }
                }
            }
        }
    }

    /// `A::class` and friends.
    pub(crate) fn op_fetch_class_name(&mut self, spec: &ClassSpec) -> VmResult<Value> {
        if let ClassSpec::Dyn(op) = spec {
            let v = self.read(*op)?;
            return match v {
                Value::Object(o) => Ok(Value::Str(object::class_of(&o).name.clone())),
                other => Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "Cannot use \"::class\" on {}",
                        crate::typecheck::given_type(&other)
                    ),
                )),
            };
        }
        if let ClassSpec::SelfClass = spec {
            return Ok(Value::Str(
                self.scope().map(|c| c.name.clone()).unwrap_or_default(),
            ));
        }
        let c = self.resolve_class_spec(spec)?;
        Ok(Value::Str(c.name.clone()))
    }

    /// `$a instanceof B`.
    pub(crate) fn op_instanceof(&mut self, v: &Value, spec: &ClassSpec) -> VmResult<bool> {
        let cls = match spec {
            ClassSpec::Named(l) => {
                let n = self.lit_str(*l);
                match self.find_class(n.as_bytes()) {
                    Some(c) => c,
                    None => return Ok(false),
                }
            }
            ClassSpec::Dyn(op) => {
                let c = self.read(*op)?;
                match c {
                    Value::Object(o) => object::class_of(&o).clone(),
                    Value::Str(s) => match self.find_class(s.as_bytes()) {
                        Some(c) => c,
                        None => return Ok(false),
                    },
                    _ => {
                        return Err(self
                            .make_throw("Error", "Class name must be a valid object or a string"))
                    }
                }
            }
            other => self.resolve_class_spec(other)?,
        };
        Ok(match v {
            Value::Object(o) => object::class_of(o).is_subclass_of(&cls),
            _ => false,
        })
    }

    // ------------------------------------------------------------------
    // Constants
    // ------------------------------------------------------------------

    /// Evaluates a constant expression in a class scope.
    pub(crate) fn eval_const_expr(
        &mut self,
        e: &ConstExpr,
        scope: Option<&Rc<Class>>,
    ) -> VmResult<Value> {
        match e {
            ConstExpr::Value(v) => Ok(v.clone()),
            ConstExpr::Code(code) => {
                let mut f = Frame::new(code.clone(), FrameKind::ConstExpr);
                f.scope = scope.cloned();
                f.called = scope.cloned();
                self.frames.push(f);
                let base = self.frames.len() - 1;
                self.execute(base)
            }
        }
    }

    /// `A::NAME`.
    pub(crate) fn op_fetch_class_const(
        &mut self,
        spec: &ClassSpec,
        name: Value,
    ) -> VmResult<Value> {
        let name = match name {
            Value::Str(s) => s,
            other => {
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "Cannot use value of type {} as class constant name",
                        crate::typecheck::given_type(&other)
                    ),
                ))
            }
        };
        let cls = self.resolve_class_spec(spec)?;
        // Inside a constant expression php-src goes through
        // `zend_get_class_constant_ex`, whose errors name the class as
        // written (`self::X`) and which marks the constant while evaluating
        // it; the `FETCH_CLASS_CONSTANT` opcode does neither.
        let in_const_expr = self
            .frames
            .last()
            .is_some_and(|f| f.kind == FrameKind::ConstExpr);
        if in_const_expr {
            let shown = match spec {
                ClassSpec::SelfClass => ZStr::from("self"),
                ClassSpec::Parent => ZStr::from("parent"),
                ClassSpec::Static => ZStr::from("static"),
                ClassSpec::Named(l) => self.lit_str(*l),
                ClassSpec::Dyn(_) => cls.name.clone(),
            };
            self.class_constant_ex(&cls, &name, Some(&shown), true)?;
        } else if cls
            .find_const(name.as_bytes())
            .is_some_and(|c| !c.is_case && c.value.borrow().is_none())
        {
            self.class_constant_ex(&cls, &name, None, false)?;
        }
        self.fetch_class_constant(&cls, &name)
    }

    /// Reads a class constant (evaluating it lazily).
    pub(crate) fn class_constant(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Value> {
        self.class_constant_ex(cls, name, None, true)
    }

    /// [`Self::class_constant`] with the class named `shown` in errors
    /// (instead of its resolved name) and optional recursion marking of an
    /// unevaluated constant (`mark`).
    fn class_constant_ex(
        &mut self,
        cls: &Rc<Class>,
        name: &ZStr,
        shown: Option<&ZStr>,
        mark: bool,
    ) -> VmResult<Value> {
        let shown_name = |c: &ZStr| shown.map_or_else(|| object::display_name(c), Clone::clone);
        if name.as_bytes() == b"class" {
            return Ok(Value::Str(cls.name.clone()));
        }
        let Some(c) = cls.find_const(name.as_bytes()).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Undefined constant {}::{}",
                    shown_name(&cls.name),
                    name.to_string_lossy()
                ),
            ));
        };
        // Visibility.
        if c.flags & flags::VISIBILITY != flags::PUBLIC {
            let scope = self.scope();
            let decl = self.find_class(c.class.as_bytes());
            let ok = match (c.flags & flags::VISIBILITY, &scope, &decl) {
                (flags::PRIVATE, Some(s), Some(d)) => Rc::ptr_eq(s, d),
                (flags::PROTECTED, Some(s), Some(d)) => s.is_subclass_of(d) || d.is_subclass_of(s),
                _ => false,
            };
            if !ok {
                let vis = if c.flags & flags::PRIVATE != 0 {
                    "private"
                } else {
                    "protected"
                };
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Cannot access {vis} constant {}::{}",
                        shown_name(&cls.name),
                        name.to_string_lossy()
                    ),
                ));
            }
        }
        if let Some(v) = c.value.borrow().as_ref() {
            return Ok(v.clone());
        }
        if c.is_case {
            return self.enum_case(cls, name);
        }
        if c.evaluating.get() {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot declare self-referencing constant {}::{}",
                    shown_name(&c.class),
                    name.to_string_lossy()
                ),
            ));
        }
        if mark {
            c.evaluating.set(true);
        }
        let decl = self
            .find_class(c.class.as_bytes())
            .unwrap_or_else(|| cls.clone());
        let r = self.eval_const_expr(&c.expr, Some(&decl));
        if mark {
            c.evaluating.set(false);
        }
        let mut v = r?;
        if let Some(ty) = &c.ty {
            let ctx = crate::typecheck::TypeCtx {
                scope: Some(&decl),
                called: Some(&decl),
                strict: true,
                internal_arg: None,
            };
            let ty = ty.clone();
            if !self.verify_type(&ty, &mut v, &ctx)? {
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "Cannot assign {} to class constant {}::{} of type {}",
                        crate::typecheck::given_type(&v),
                        object::display_name(&c.class),
                        name.to_string_lossy(),
                        ty.to_php_string()
                    ),
                ));
            }
        }
        *c.value.borrow_mut() = Some(v.clone());
        Ok(v)
    }

    /// Defines a global constant (`const X = ...;` / `define`).
    pub(crate) fn declare_constant(&mut self, name: ZStr, v: Value) -> VmResult<bool> {
        if self.constants.contains_key(&name) {
            self.warning(&format!(
                "Constant {} already defined, this will be an error in PHP 9",
                name.to_string_lossy()
            ))?;
            return Ok(false);
        }
        self.user_constants.push(name.clone());
        let (file, _) = self.current_location();
        self.const_files.insert(name.clone(), file);
        self.constants.insert(name, v);
        Ok(true)
    }

    /// Reads a global constant.
    pub(crate) fn fetch_constant(&mut self, name: u32, fallback: Option<u32>) -> VmResult<Value> {
        let n = self.lit_str(name);
        if let Some(v) = self.constants.get(&n).cloned() {
            self.constant_deprecation(&n)?;
            return Ok(v);
        }
        if let Some(v) = self.special_constant(n.as_bytes()) {
            return Ok(v);
        }
        if let Some(fb) = fallback {
            let f = self.lit_str(fb);
            if let Some(v) = self.constants.get(&f).cloned() {
                self.constant_deprecation(&f)?;
                return Ok(v);
            }
            if let Some(v) = self.special_constant(f.as_bytes()) {
                return Ok(v);
            }
        }
        // Namespaced constant names are case-insensitive in the namespace part.
        if let Some(p) = n.as_bytes().iter().rposition(|&c| c == b'\\') {
            let (ns, c) = n.as_bytes().split_at(p);
            let mut k = ns.to_ascii_lowercase();
            k.extend_from_slice(c);
            if let Some(v) = self.constants.get(&ZStr::from(k)) {
                return Ok(v.clone());
            }
        }
        Err(self.make_throw(
            "Error",
            &format!("Undefined constant \"{}\"", n.to_string_lossy()),
        ))
    }

    fn special_constant(&mut self, name: &[u8]) -> Option<Value> {
        match name {
            b"__COMPILER_HALT_OFFSET__" => {
                let file = self.frame().code.filename.clone();
                self.ext.halt_offsets.get(&file).map(|o| Value::Int(*o))
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Static properties
    // ------------------------------------------------------------------

    /// The storage slot of `A::$name`.
    pub(crate) fn static_prop_cell(
        &mut self,
        cls: &Rc<Class>,
        name: &ZStr,
    ) -> VmResult<Rc<RefCell<Value>>> {
        let Some(sp) = cls.find_static(name.as_bytes()).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Access to undeclared static property {}::${}",
                    object::display_name(&cls.name),
                    name.to_string_lossy()
                ),
            ));
        };
        if sp.flags & flags::VISIBILITY != flags::PUBLIC {
            let scope = self.scope();
            let decl = self.find_class(sp.class.as_bytes());
            let ok = match (sp.flags & flags::VISIBILITY, &scope, &decl) {
                (flags::PRIVATE, Some(s), Some(d)) => Rc::ptr_eq(s, d),
                (flags::PROTECTED, Some(s), Some(d)) => s.is_subclass_of(d) || d.is_subclass_of(s),
                _ => false,
            };
            if !ok {
                let vis = if sp.flags & flags::PRIVATE != 0 {
                    "private"
                } else {
                    "protected"
                };
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Cannot access {vis} property {}::${}",
                        object::display_name(&cls.name),
                        name.to_string_lossy()
                    ),
                ));
            }
        }
        if !sp.initialized.get() {
            sp.initialized.set(true);
            if let Some(d) = &sp.default {
                let decl = self
                    .find_class(sp.class.as_bytes())
                    .unwrap_or_else(|| cls.clone());
                let v = self.eval_const_expr(d, Some(&decl))?;
                *sp.cell.borrow_mut() = v;
            }
        }
        crate::typed_refs::note_static_prop(&sp);
        Ok(sp.cell.clone())
    }

    // ------------------------------------------------------------------
    // Functions
    // ------------------------------------------------------------------

    /// Declares a user function.
    pub(crate) fn declare_function(&mut self, f: &Rc<UserFunction>) -> VmResult<()> {
        let lc = ZStr::from(f.name.to_ascii_lowercase());
        if let Some(existing) = self.functions.get(&lc) {
            let msg = match existing {
                Func::User(u) => format!(
                    "Cannot redeclare function {}() (previously declared in {}:{})",
                    f.name, u.code.filename, u.line_start
                ),
                Func::Builtin(_) => format!("Cannot redeclare function {}()", f.name),
            };
            return Err(self.fatal_at(&msg, f.line_start));
        }
        self.functions.insert(lc, Func::User(f.clone()));
        Ok(())
    }

    // ------------------------------------------------------------------
    // Class declaration
    // ------------------------------------------------------------------

    /// Declares (links) a class from its compiled declaration.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn declare_class(
        &mut self,
        decl: &Rc<ClassDecl>,
        is_anon: bool,
    ) -> VmResult<Rc<Class>> {
        let key = Rc::as_ptr(decl) as usize;
        if let Some(c) = self.bound_decls.get(&key) {
            return Ok(c.clone());
        }
        let lc = ZStr::from(decl.name.to_ascii_lowercase());
        if self.classes.contains_key(&lc) {
            let kind = match decl.kind {
                ClassKind::Interface => "interface",
                ClassKind::Trait => "trait",
                ClassKind::Enum => "enum",
                ClassKind::Class => "class",
            };
            let _ = kind;
            return Err(self.fatal_at(
                &format!(
                    "Cannot declare {} {}, because the name is already in use",
                    match decl.kind {
                        ClassKind::Interface => "interface",
                        ClassKind::Trait => "trait",
                        ClassKind::Enum => "enum",
                        ClassKind::Class => "class",
                    },
                    decl.name
                ),
                decl.line_start,
            ));
        }
        let cls = self.link_class(decl, is_anon)?;
        self.bound_decls.insert(key, cls.clone());
        self.register_class(cls.clone());
        Ok(cls)
    }

    /// Registers a linked class.
    pub(crate) fn register_class(&mut self, cls: Rc<Class>) {
        let lc = cls.lc_name.clone();
        self.class_order.push(lc.clone());
        if self.classes.insert(lc.clone(), cls).is_none() {
            crate::reflection::note_new(crate::reflection::Table::Class, lc.as_bytes());
        }
    }

    /// Whether all dependencies of a declaration are available (for early
    /// binding).
    pub(crate) fn can_early_bind(&self, decl: &ClassDecl) -> bool {
        let has = |n: &ZStr| self.find_class(n.as_bytes()).is_some();
        decl.parent.as_ref().is_none_or(has)
            && decl.interfaces.iter().all(has)
            && decl.traits.iter().all(has)
            && !self.has_unresolved_variance(decl)
    }

    /// `zend_try_early_bind` gives up when a method signature check against
    /// the parent is `INHERITANCE_UNRESOLVED`: approximated as an overriding
    /// method whose signature differs from the parent's in a class name
    /// that is not declared yet. The class is then declared when its
    /// statement runs, where the check can report the error.
    fn has_unresolved_variance(&self, decl: &ClassDecl) -> bool {
        fn names(f: &UserFunction, out: &mut Vec<Vec<u8>>) {
            let types = f
                .params
                .iter()
                .filter_map(|p| p.ty.as_ref())
                .chain(f.return_type.as_ref());
            for t in types {
                for n in t.classes.iter().chain(t.intersections.iter().flatten()) {
                    out.push(n.as_bytes().to_ascii_lowercase());
                }
            }
        }
        let Some(parent) = decl
            .parent
            .as_ref()
            .and_then(|p| self.find_class(p.as_bytes()))
        else {
            return false;
        };
        let own = decl.name.as_bytes().to_ascii_lowercase();
        decl.methods.iter().any(|md| {
            let Some(pm) = parent.find_method(&md.name.as_bytes().to_ascii_lowercase()) else {
                return false;
            };
            let Func::User(pf) = &pm.func else {
                return false;
            };
            if pm.visibility() == flags::PRIVATE {
                return false;
            }
            let (mut a, mut b) = (Vec::new(), Vec::new());
            names(&md.func, &mut a);
            names(pf, &mut b);
            a.iter()
                .filter(|n| !b.contains(n))
                .chain(b.iter().filter(|n| !a.contains(n)))
                .any(|n| {
                    !matches!(n.as_slice(), b"self" | b"parent" | b"static")
                        && *n != own.as_slice()
                        && self.find_class(n).is_none()
                })
        })
    }

    /// `zend_traits_init_trait_structures`: resolves the traits named by
    /// `insteadof` and `as` rules (without autoloading) and validates them.
    fn init_trait_structures(
        &self,
        decl: &ClassDecl,
        traits: &[Rc<Class>],
    ) -> Result<TraitRules, String> {
        let find = |n: &ZStr| -> Result<usize, String> {
            let Some(t) = self.find_class(n.as_bytes()).filter(|c| c.linked.get()) else {
                return Err(format!("Could not find trait {n}"));
            };
            // `zend_check_trait_usage`.
            if !t.is_trait() {
                return Err(format!(
                    "Class {} is not a trait, Only traits may be used in 'as' and 'insteadof' statements",
                    t.name
                ));
            }
            traits
                .iter()
                .position(|x| Rc::ptr_eq(x, &t))
                .ok_or_else(|| format!("Required Trait {} wasn't added to {}", t.name, decl.name))
        };
        let mut out = TraitRules {
            excluded: Vec::new(),
            alias_trait: vec![None; decl.trait_rules.len()],
        };
        for r in &decl.trait_rules {
            let TraitRule::Precedence {
                trait_name,
                method,
                insteadof,
            } = r
            else {
                continue;
            };
            let ti = find(trait_name)?;
            let lc = ZStr::from(method.to_ascii_lowercase());
            if !traits[ti].methods.contains_key(&lc) {
                return Err(format!(
                    "A precedence rule was defined for {}::{method} but this method does not exist",
                    traits[ti].name
                ));
            }
            for ex in insteadof {
                let ei = find(ex)?;
                if out.excluded.contains(&(ei, lc.clone())) {
                    return Err(format!(
                        "Failed to evaluate a trait precedence ({method}). Method of trait {} was defined to be excluded multiple times",
                        traits[ei].name
                    ));
                }
                out.excluded.push((ei, lc.clone()));
                if ei == ti {
                    let t = &traits[ti].name;
                    return Err(format!(
                        "Inconsistent insteadof definition. The method {method} is to be used from {t}, but {t} is also on the exclude list"
                    ));
                }
            }
        }
        for (i, r) in decl.trait_rules.iter().enumerate() {
            let TraitRule::Alias {
                trait_name,
                method,
                alias,
                ..
            } = r
            else {
                continue;
            };
            let lc = ZStr::from(method.to_ascii_lowercase());
            if let Some(tn) = trait_name {
                let ti = find(tn)?;
                if !traits[ti].methods.contains_key(&lc) {
                    return Err(format!(
                        "An alias was defined for {}::{method} but this method does not exist",
                        traits[ti].name
                    ));
                }
                out.alias_trait[i] = Some(ti);
                continue;
            }
            let mut found: Option<usize> = None;
            for (j, t) in traits.iter().enumerate() {
                if !t.methods.contains_key(&lc) {
                    continue;
                }
                if let Some(f) = found {
                    let (a, b) = (&traits[f].name, &t.name);
                    return Err(format!(
                        "An alias was defined for method {method}(), which exists in both {a} and {b}. Use {a}::{method} or {b}::{method} to resolve the ambiguity"
                    ));
                }
                found = Some(j);
            }
            if found.is_none() {
                return Err(match alias {
                    Some(a) => format!(
                        "An alias ({a}) was defined for method {method}(), but this method does not exist"
                    ),
                    None => format!(
                        "The modifiers of the trait method {method}() are changed, but this method does not exist. Error"
                    ),
                });
            }
            out.alias_trait[i] = found;
        }
        Ok(out)
    }

    /// `do_inheritance_check_on_method` (with `ZEND_INHERITANCE_CHECK_PROTO`
    /// but no visibility check) of the method satisfying an abstract trait
    /// method; returns the fatal error (message, file, line).
    fn check_trait_abstract(
        &self,
        cls: &Rc<Class>,
        c: &TraitAbstractCheck,
    ) -> Option<(String, ZStr, u32)> {
        let (child, proto) = (&c.child, &c.proto);
        // `fixup_trait_scope`: `self` in trait methods is the using class.
        let child_scope = if c.child_trait.is_some() || Weak::ptr_eq(&child.scope, &Rc::downgrade(cls)) {
            cls.clone()
        } else {
            child.scope()
        };
        let child_name = c
            .child_trait
            .clone()
            .unwrap_or_else(|| object::display_name(&child_scope.name));
        let proto_name = object::display_name(&proto.scope().name);
        let (file, line) = match &child.func {
            Func::User(u) => (u.code.filename.clone(), u.line_start),
            Func::Builtin(_) => (ZStr::from("Unknown"), 0),
        };
        if proto.flags & flags::FINAL != 0 {
            return Some((
                format!("Cannot override final method {proto_name}::{}()", child.name),
                file,
                line,
            ));
        }
        if child.is_static() != proto.is_static() {
            let msg = if child.is_static() {
                format!(
                    "Cannot make non static method {proto_name}::{}() static in class {child_name}",
                    child.name
                )
            } else {
                format!(
                    "Cannot make static method {proto_name}::{}() non static in class {child_name}",
                    child.name
                )
            };
            return Some((msg, file, line));
        }
        let st = crate::variance::implementation_check(
            self,
            &child.func,
            child.flags,
            &child_scope,
            &proto.func,
            proto.flags,
            cls,
        );
        if st == crate::variance::Status::Ok {
            return None;
        }
        Some((
            format!(
                "Declaration of {} must be compatible with {}",
                crate::variance::declaration_as(&child.func, child.flags, &child_name, &child_scope),
                crate::variance::declaration_as(&proto.func, proto.flags, &proto_name, cls)
            ),
            file,
            line,
        ))
    }

    fn fatal_at(&mut self, msg: &str, line: u32) -> Throw {
        let (file, _) = self.current_location();
        self.report_fatal(msg, &file, line);
        Throw::Fatal
    }

    #[allow(clippy::too_many_lines)]
    fn link_class(&mut self, decl: &Rc<ClassDecl>, is_anon: bool) -> VmResult<Rc<Class>> {
        let name = decl.name.clone();
        let display = object::display_name(&name);
        // Messages formatted with `%pS` print the whole name, including the
        // NUL-separated suffix of an anonymous class.
        let full = name.to_string_lossy();
        let line = decl.line_start;
        // Parent.
        let parent = match &decl.parent {
            Some(p) => {
                let Some(pc) = self.lookup_class(p)? else {
                    let kind = if decl.kind == ClassKind::Interface {
                        "Interface"
                    } else {
                        "Class"
                    };
                    return Err(self.make_throw(
                        "Error",
                        &format!("{kind} \"{}\" not found", p.to_string_lossy()),
                    ));
                };
                if pc.is_interface() && decl.kind == ClassKind::Class {
                    return Err(self.fatal_at(
                        &format!("Class {full} cannot extend interface {}", pc.name),
                        line,
                    ));
                }
                if pc.is_trait() {
                    return Err(self.fatal_at(
                        &format!("Class {full} cannot extend trait {}", pc.name),
                        line,
                    ));
                }
                if pc.flags & cflags::FINAL != 0 {
                    let what = if pc.is_enum() { "enum" } else { "final class" };
                    return Err(self.fatal_at(
                        &format!("Class {full} cannot extend {what} {}", pc.name),
                        line,
                    ));
                }
                if pc.flags & cflags::READONLY != decl.flags & cflags::READONLY {
                    let (a, b) = if decl.flags & cflags::READONLY != 0 {
                        ("Readonly class", "non-readonly class")
                    } else {
                        ("Non-readonly class", "readonly class")
                    };
                    return Err(
                        self.fatal_at(&format!("{a} {full} cannot extend {b} {}", pc.name), line)
                    );
                }
                Some(pc)
            }
            None => None,
        };
        // Interfaces (for interfaces: parents).
        let mut interfaces: Vec<Rc<Class>> = Vec::new();
        let mut direct_ifaces: Vec<Rc<Class>> = Vec::new();
        if let Some(p) = &parent {
            for i in &p.interfaces {
                interfaces.push(i.clone());
            }
        }
        let mut iface_names = decl.interfaces.clone();
        if decl
            .methods
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(b"__tostring"))
            && !iface_names
                .iter()
                .any(|i| i.eq_ignore_ascii_case(b"Stringable"))
            && decl.kind != ClassKind::Trait
        {
            iface_names.push(ZStr::from("Stringable"));
        }
        if decl.kind == ClassKind::Enum {
            iface_names.insert(0, ZStr::from("UnitEnum"));
            if decl.backing.is_some() {
                iface_names.insert(1, ZStr::from("BackedEnum"));
            }
        }
        for iname in &iface_names {
            let Some(ic) = self.lookup_class(iname)? else {
                return Err(self.make_throw(
                    "Error",
                    &format!("Interface \"{}\" not found", iname.to_string_lossy()),
                ));
            };
            if !ic.is_interface() {
                if decl.kind == ClassKind::Interface {
                    return Err(self.fatal_at(
                        &format!(
                            "{display} cannot implement {} - it is not an interface",
                            ic.name
                        ),
                        line,
                    ));
                }
                return Err(self.fatal_at(
                    &format!(
                        "{display} cannot implement {} - it is not an interface",
                        ic.name
                    ),
                    line,
                ));
            }
            if decl.kind == ClassKind::Class
                && (ic.lc_name.as_bytes() == b"unitenum" || ic.lc_name.as_bytes() == b"backedenum")
            {
                return Err(self.fatal_at(
                    &format!(
                        "Non-enum class {display} cannot implement interface {}",
                        ic.name
                    ),
                    line,
                ));
            }
            direct_ifaces.push(ic.clone());
            for sub in &ic.interfaces {
                if !interfaces.iter().any(|x| Rc::ptr_eq(x, sub)) {
                    interfaces.push(sub.clone());
                }
            }
            if !interfaces.iter().any(|x| Rc::ptr_eq(x, &ic)) {
                interfaces.push(ic.clone());
            }
        }
        // Traversable must be implemented via Iterator or IteratorAggregate.
        if decl.kind == ClassKind::Class
            && interfaces
                .iter()
                .any(|i| i.lc_name.as_bytes() == b"traversable")
            && !interfaces
                .iter()
                .any(|i| matches!(i.lc_name.as_bytes(), b"iterator" | b"iteratoraggregate"))
            && !parent
                .as_ref()
                .is_some_and(|p| p.flags & cflags::INTERNAL != 0)
        {
            return Err(self.fatal_at(
                &format!(
                    "Class {display} must implement interface Traversable as part of either Iterator or IteratorAggregate"
                ),
                line,
            ));
        }
        // Traits.
        let mut traits = Vec::new();
        for tn in &decl.traits {
            let Some(tc) = self.lookup_class(tn)? else {
                return Err(self.make_throw(
                    "Error",
                    &format!("Trait \"{}\" not found", tn.to_string_lossy()),
                ));
            };
            if !tc.is_trait() {
                return Err(self.make_throw(
                    "Error",
                    &format!("{display} cannot use {} - it is not a trait", tc.name),
                ));
            }
            if tc
                .attributes
                .iter()
                .any(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
            {
                // `zend_use_of_deprecated_trait`.
                let suffix = self.deprecation_suffix(&tc.attributes, Some(&tc))?;
                let code = if tc.flags & cflags::INTERNAL != 0 {
                    crate::errors::level::DEPRECATED
                } else {
                    crate::errors::level::USER_DEPRECATED
                };
                let mut msg =
                    format!("Trait {} used by {display} is deprecated", tc.name).into_bytes();
                msg.extend_from_slice(&suffix);
                self.error_bytes(code, &msg)?;
            }
            // `zend_do_link_class` skips duplicated trait uses.
            if !traits.iter().any(|t: &Rc<Class>| Rc::ptr_eq(t, &tc)) {
                traits.push(tc);
            }
        }
        let trait_rules = if traits.is_empty() {
            TraitRules::default()
        } else {
            match self.init_trait_structures(decl, &traits) {
                Ok(r) => r,
                Err(e) => return Err(self.fatal_at(&e, line)),
            }
        };
        let mut cflag = decl.flags;
        if let Some(p) = &parent {
            if p.flags & cflags::ALLOW_DYNAMIC != 0 {
                cflag |= cflags::ALLOW_DYNAMIC;
            }
        }
        let decl_c = decl.clone();
        let parent_c = parent.clone();
        let traits_c = traits.clone();
        let mut err: Option<String> = None;
        let mut link_out = LinkOut::default();
        let interfaces_c = interfaces.clone();
        let cls = Rc::new_cyclic(|weak: &Weak<Class>| {
            build_class(
                &decl_c,
                weak,
                parent_c,
                interfaces_c,
                traits_c,
                &trait_rules,
                cflag,
                is_anon,
                &mut err,
                &mut link_out,
            )
        });
        if let Some(e) = err {
            return Err(self.fatal_at(&e, line));
        }
        for w in &link_out.warnings {
            self.error_at(crate::errors::level::COMPILE_WARNING, w, &decl.filename, line)?;
        }
        // Inheritance checks for overridden methods.
        if let Some(e) = check_method_inheritance(&cls) {
            return Err(self.fatal_at(&e.0, e.1));
        }
        for c in &link_out.trait_checks {
            if let Some((e, file, l)) = self.check_trait_abstract(&cls, c) {
                self.report_fatal(&e, &file, l);
                return Err(Throw::Fatal);
            }
        }
        let (err, warnings) = crate::variance::check_class(self, &cls);
        if let Some(e) = err {
            return Err(self.fatal_at(&e.0, e.1));
        }
        for (w, l) in warnings {
            let file = decl.filename.clone();
            self.error_at(crate::errors::level::DEPRECATED, &w, &file, l)?;
        }
        // Abstract method check.
        // (`zend_verify_abstract_class`: an explicitly abstract class must
        // still implement its private abstract methods.)
        let explicit_abstract = cls.flags & cflags::EXPLICIT_ABSTRACT != 0;
        if !cls.is_interface() && !cls.is_trait() {
            let mut missing: Vec<String> = Vec::new();
            for lcname in &cls.method_order {
                if let Some(m) = cls.methods.get(lcname) {
                    if m.is_abstract() && (!explicit_abstract || m.visibility() == flags::PRIVATE) {
                        missing.push(format!(
                            "{}::{}",
                            object::display_name(&m.scope().name),
                            m.name
                        ));
                    }
                }
            }
            for pi in &cls.props {
                if explicit_abstract
                    || !cls
                    .prop_map
                    .get(&pi.name)
                    .is_some_and(|x| Rc::ptr_eq(x, pi))
                {
                    continue;
                }
                if let Some(h) = &pi.hooks {
                    let dn = object::display_name(&h.class);
                    if h.abstract_get {
                        missing.push(format!("{dn}::${}::get", pi.name));
                    }
                    if h.abstract_set {
                        missing.push(format!("{dn}::${}::set", pi.name));
                    }
                }
            }
            if !missing.is_empty() {
                let n = missing.len();
                let shown: Vec<String> = missing.iter().take(3).cloned().collect();
                let mut list = shown.join(", ");
                if n > 3 {
                    list.push_str(", ...");
                }
                let what = if cls.is_enum() { "Enum" } else { "Class" };
                let s = if n == 1 { "" } else { "s" };
                let msg = if explicit_abstract
                    || cls.is_enum()
                    || cls.flags & crate::class::cflags::ANON != 0
                {
                    format!("{what} {full} must implement {n} abstract method{s} ({list})")
                } else {
                    format!(
                        "{what} {full} contains {n} abstract method{s} and must therefore be declared abstract or implement the remaining method{s} ({list})"
                    )
                };
                return Err(self.fatal_at(&msg, line));
            }
        }
        if cls.is_enum() {
            if let Some(e) = self.verify_enum(&cls) {
                return Err(self.fatal_at(&e, line));
            }
        }
        if let Some((e, l)) = crate::attrs::check_override(&cls) {
            return Err(self.fatal_at(&e, if l == 0 { line } else { l }));
        }
        for iface in cls.interfaces.clone() {
            if let Some(f) = iface
                .hooks
                .as_ref()
                .and_then(|h| h.interface_gets_implemented)
            {
                f(self, &cls)?;
            }
        }
        // `zend_implement_serializable`.
        if cls.flags & cflags::EXPLICIT_ABSTRACT == 0
            && !cls.is_interface()
            && cls.interfaces.iter().any(|i| i.lc_name.as_bytes() == b"serializable")
            && (cls.find_method(b"__serialize").is_none() || cls.find_method(b"__unserialize").is_none())
        {
            let msg = format!(
                "{} implements the Serializable interface, which is deprecated. Implement __serialize() and __unserialize() instead (or in addition, if support for old PHP versions is necessary)",
                object::display_name(&cls.name)
            );
            self.error_at(crate::errors::level::DEPRECATED, &msg, &decl.filename, line)?;
        }
        Ok(cls)
    }
}

/// What [`build_class`] leaves for [`Vm::link_class`] to do with VM access.
#[derive(Default)]
struct LinkOut {
    /// Abstract trait methods to check against the method that satisfies
    /// them.
    trait_checks: Vec<TraitAbstractCheck>,
    /// `E_COMPILE_WARNING`s raised while binding traits.
    warnings: Vec<String>,
}

/// An abstract trait method (`proto`, still scoped to its trait) that an
/// existing method (`child`) of the class must be compatible with
/// (`zend_add_trait_method` -> `do_inheritance_check_on_method`).
struct TraitAbstractCheck {
    child: Rc<Method>,
    /// For a child that is itself a not yet fixed up trait method: the
    /// trait it comes from (its scope in messages).
    child_trait: Option<ZStr>,
    proto: Rc<Method>,
}

/// A copy of a trait method for the using class: it gets its own static
/// variables (`zend_add_trait_method` copies the op array).
fn trait_method_copy(f: &Func) -> Func {
    match f {
        Func::User(u) => Func::User(Rc::new(UserFunction {
            name: u.name.clone(),
            params: u.params.clone(),
            required: u.required,
            return_type: u.return_type.clone(),
            flags: u.flags,
            code: u.code.clone(),
            line_start: u.line_start,
            line_end: u.line_end,
            doc_comment: u.doc_comment.clone(),
            statics: RefCell::new(Vec::new()),
            uses: u.uses.clone(),
            class_name: u.class_name.clone(),
            attributes: u.attributes.clone(),
            hook_prop: u.hook_prop.clone(),
        })),
        Func::Builtin(_) => f.clone(),
    }
}

/// Whether two functions share their code (`existing_fn->op_array.opcodes
/// == fn->op_array.opcodes`).
fn same_code(a: &Func, b: &Func) -> bool {
    match (a, b) {
        (Func::User(x), Func::User(y)) => Rc::ptr_eq(&x.code, &y.code),
        _ => Func::ptr_eq(a, b),
    }
}

/// Applies the modifiers of an `as` rule to a trait method's flags.
fn alias_flags(modifiers: u32, fn_flags: u32) -> u32 {
    if modifiers & flags::VISIBILITY != 0 {
        modifiers | (fn_flags & !flags::VISIBILITY)
    } else {
        modifiers | fn_flags
    }
}

/// `zend_do_traits_method_binding` / `zend_traits_copy_functions` /
/// `zend_add_trait_method` over the method table being built.
struct TraitMethodBinder<'a> {
    class: &'a ZStr,
    weak: &'a Weak<Class>,
    parent: Option<&'a Rc<Class>>,
    traits: &'a [Rc<Class>],
    rules: &'a TraitRules,
    methods: &'a mut HashMap<ZStr, Rc<Method>>,
    order: &'a mut Vec<ZStr>,
    /// Methods copied from a trait whose scope has not been fixed up to
    /// the class yet: lowercase name -> trait index.
    unfixed: HashMap<ZStr, usize>,
    checks: &'a mut Vec<TraitAbstractCheck>,
    warnings: &'a mut Vec<String>,
    err: &'a mut Option<String>,
}

impl TraitMethodBinder<'_> {
    /// Binds the non-abstract (`abstract_pass == false`) or the abstract
    /// methods of all used traits.
    fn bind(&mut self, decl: &ClassDecl, abstract_pass: bool) {
        for ti in 0..self.traits.len() {
            let t = self.traits[ti].clone();
            for lcm in &t.method_order {
                let Some(tm) = t.methods.get(lcm) else {
                    continue;
                };
                if tm.is_abstract() != abstract_pass {
                    continue;
                }
                // Aliases with a new name.
                for (ri, r) in decl.trait_rules.iter().enumerate() {
                    let TraitRule::Alias {
                        method,
                        flags: f,
                        alias: Some(a),
                        ..
                    } = r
                    else {
                        continue;
                    };
                    if self.rules.alias_trait.get(ri).copied().flatten() != Some(ti)
                        || !method.eq_ignore_ascii_case(lcm.as_bytes())
                    {
                        continue;
                    }
                    let fl = alias_flags(*f, tm.flags);
                    self.check_private_final(tm.flags, fl, a);
                    self.add(a, ZStr::from(a.to_ascii_lowercase()), tm, fl, ti);
                }
                if self.rules.excluded.iter().any(|(i, ml)| *i == ti && ml == lcm) {
                    continue;
                }
                // Aliases that only change the modifiers.
                let mut fl = tm.flags;
                for (ri, r) in decl.trait_rules.iter().enumerate() {
                    if let TraitRule::Alias {
                        method,
                        flags: f,
                        alias: None,
                        ..
                    } = r
                    {
                        if *f != 0
                            && self.rules.alias_trait.get(ri).copied().flatten() == Some(ti)
                            && method.eq_ignore_ascii_case(lcm.as_bytes())
                        {
                            fl = alias_flags(*f, tm.flags);
                        }
                    }
                }
                self.check_private_final(tm.flags, fl, lcm);
                self.add(&tm.name, lcm.clone(), tm, fl, ti);
            }
        }
    }

    /// `zend_traits_check_private_final_inheritance`.
    fn check_private_final(&mut self, orig: u32, fl: u32, name: &ZStr) {
        if orig & flags::FINAL == 0
            && fl & (flags::PRIVATE | flags::FINAL) == flags::PRIVATE | flags::FINAL
            && !name.eq_ignore_ascii_case(b"__construct")
        {
            self.warnings.push(
                "Private methods cannot be final as they are never overridden by other classes"
                    .to_string(),
            );
        }
    }

    /// `zend_add_trait_method`.
    fn add(&mut self, name: &ZStr, key: ZStr, tm: &Rc<Method>, fl: u32, ti: usize) {
        if let Some(existing) = self.methods.get(&key) {
            let ex_trait = self.unfixed.get(&key).copied();
            if ex_trait.is_some()
                && same_code(&existing.func, &tm.func)
                && existing.visibility() == fl & flags::VISIBILITY
            {
                return;
            }
            if fl & flags::ABSTRACT != 0 {
                // The requirement must be satisfied by the existing method.
                self.checks.push(TraitAbstractCheck {
                    child: existing.clone(),
                    child_trait: ex_trait.map(|i| self.traits[i].name.clone()),
                    proto: tm.clone(),
                });
                return;
            }
            match ex_trait {
                // Members of the class itself override trait methods.
                None if Weak::ptr_eq(&existing.scope, self.weak) => return,
                Some(ei) if !existing.is_abstract() => {
                    if self.err.is_none() {
                        *self.err = Some(format!(
                            "Trait method {}::{} has not been applied as {}::{name}, because of collision with {}::{}",
                            self.traits[ti].name,
                            tm.name,
                            self.class,
                            self.traits[ei].name,
                            existing.name
                        ));
                    }
                    return;
                }
                _ => {}
            }
        }
        let proto = self
            .parent
            .and_then(|p| p.methods.get(&key))
            .filter(|m| m.visibility() != flags::PRIVATE)
            .map_or_else(|| self.weak.clone(), |m| m.proto_scope.clone());
        let m = Rc::new(Method {
            name: name.clone(),
            func: trait_method_copy(&tm.func),
            flags: fl | flags::FROM_TRAIT,
            scope: self.weak.clone(),
            proto_scope: proto,
        });
        if self.methods.insert(key.clone(), m).is_none() {
            self.order.push(key.clone());
        }
        self.unfixed.insert(key, ti);
    }
}

/// The resolved trait adaptation rules of a class
/// (`zend_traits_init_trait_structures`).
#[derive(Default)]
struct TraitRules {
    /// (index into the used traits, lowercase method) excluded by
    /// `insteadof`.
    excluded: Vec<(usize, ZStr)>,
    /// For each rule of the declaration: the index of the trait an `as`
    /// rule applies to (`None` for precedence rules).
    alias_trait: Vec<Option<usize>>,
}

fn is_valid_class_name(n: &[u8]) -> bool {
    !n.is_empty()
        && n.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\\' || c >= 0x80)
}

/// `zend_do_traits_property_binding`: the error for a property of trait
/// `traits[ti]` that collides with an own (or earlier trait's) property of
/// `class` in an incompatible way. Traits are bound before inheritance, so
/// inherited properties are not considered here.
#[allow(clippy::mutable_key_type)] // `ZStr` keys hash by content only.
fn trait_property_conflict(
    class: &ZStr,
    traits: &[Rc<Class>],
    ti: usize,
    pd: &crate::class::PropDecl,
    prop_map: &HashMap<ZStr, Rc<PropInfo>>,
    static_map: &HashMap<ZStr, usize>,
    static_props: &[StaticProp],
) -> Option<String> {
    let (cflags_, cty, cdefault, chooked) =
        if let Some(e) = prop_map.get(&pd.name).filter(|e| e.class == *class) {
            (
                e.flags,
                e.ty.as_ref(),
                e.default.as_ref(),
                e.hooks.is_some(),
            )
        } else {
            let sp = static_map
                .get(&pd.name)
                .map(|&i| &static_props[i])
                .filter(|sp| sp.class == *class)?;
            (sp.flags, sp.ty.as_ref(), sp.default.as_ref(), false)
        };
    // `find_first_property_definition`.
    let first = traits[..ti]
        .iter()
        .find(|t| {
            t.decl
                .as_ref()
                .is_some_and(|d| d.props.iter().any(|p| p.name == pd.name))
        })
        .map_or_else(|| class.clone(), |t| t.name.clone());
    let tname = &traits[ti].name;
    if chooked || pd.hooks.is_some() {
        return Some(format!(
            "{first} and {tname} define the same hooked property (${}) in the composition of {class}. Conflict resolution between hooked properties is currently not supported. Class was composed",
            pd.name
        ));
    }
    let mask = flags::VISIBILITY | flags::STATIC | flags::READONLY;
    let compatible = cflags_ & mask == pd.flags & mask
        && cty == pd.ty.as_ref()
        && trait_defaults_identical(cdefault, pd.default.as_ref());
    (!compatible).then(|| {
        format!(
            "{first} and {tname} define the same property (${}) in the composition of {class}. However, the definition differs and is considered incompatible. Class was composed",
            pd.name
        )
    })
}

/// `check_trait_property_or_constant_value_compatibility` for defaults
/// known at link time; defaults that still need evaluation are assumed
/// compatible.
fn trait_defaults_identical(a: Option<&ConstExpr>, b: Option<&ConstExpr>) -> bool {
    match (a, b) {
        (None, None)
        | (Some(ConstExpr::Code(_)), Some(_))
        | (Some(_), Some(ConstExpr::Code(_))) => true,
        (Some(ConstExpr::Value(x)), Some(ConstExpr::Value(y))) => {
            php_runtime::ops::is_identical(x, y).unwrap_or(false)
        }
        _ => false,
    }
}

fn visibility_name(f: u32) -> &'static str {
    match f & flags::VISIBILITY {
        flags::PRIVATE => "private",
        flags::PROTECTED => "protected",
        _ => "public",
    }
}

/// Checks overriding rules; returns (message, line).
fn check_method_inheritance(cls: &Rc<Class>) -> Option<(String, u32)> {
    let parent = cls.parent.as_ref()?;
    for (lc, m) in &cls.methods {
        let Some(pm) = parent.methods.get(lc) else {
            continue;
        };
        if Rc::ptr_eq(m, pm) {
            continue;
        }
        if !Rc::ptr_eq(&m.scope(), cls) {
            continue;
        }
        let line = match &m.func {
            Func::User(u) => u.line_start,
            Func::Builtin(_) => 0,
        };
        if pm.visibility() == flags::PRIVATE {
            continue;
        }
        if pm.flags & flags::FINAL != 0 {
            return Some((
                format!(
                    "Cannot override final method {}::{}()",
                    object::display_name(&pm.scope().name),
                    pm.name
                ),
                line,
            ));
        }
        if pm.is_static() && !m.is_static() {
            return Some((
                format!(
                    "Cannot make static method {}::{}() non static in class {}",
                    object::display_name(&pm.scope().name),
                    pm.name,
                    object::display_name(&cls.name)
                ),
                line,
            ));
        }
        if !pm.is_static() && m.is_static() {
            return Some((
                format!(
                    "Cannot make non static method {}::{}() static in class {}",
                    object::display_name(&pm.scope().name),
                    pm.name,
                    object::display_name(&cls.name)
                ),
                line,
            ));
        }
        if !pm.is_abstract() && m.is_abstract() {
            return Some((
                format!(
                    "Cannot make non abstract method {}::{}() abstract in class {}",
                    object::display_name(&pm.scope().name),
                    pm.name,
                    object::display_name(&cls.name)
                ),
                line,
            ));
        }
        // Constructors only inherit a prototype (and its visibility) from
        // an abstract or interface declaration.
        if lc.as_bytes() == b"__construct"
            && !pm.is_abstract()
            && !pm.proto_scope.upgrade().is_some_and(|p| p.is_interface())
        {
            continue;
        }
        let pv = pm.visibility();
        let v = m.visibility();
        let weaker = match pv {
            flags::PUBLIC => v != flags::PUBLIC,
            flags::PROTECTED => v == flags::PRIVATE,
            _ => false,
        };
        if weaker {
            return Some((
                format!(
                    "Access level to {}::{}() must be {} (as in class {}){}",
                    object::display_name(&cls.name),
                    m.name,
                    visibility_name(pv),
                    object::display_name(&pm.scope().name),
                    if pv == flags::PROTECTED {
                        " or weaker"
                    } else {
                        ""
                    }
                ),
                line,
            ));
        }
    }
    None
}

/// Builds the class entry (inside `Rc::new_cyclic`).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn build_class(
    decl: &Rc<ClassDecl>,
    weak: &Weak<Class>,
    parent: Option<Rc<Class>>,
    interfaces: Vec<Rc<Class>>,
    traits: Vec<Rc<Class>>,
    rules: &TraitRules,
    cflag: u32,
    is_anon: bool,
    err: &mut Option<String>,
    link_out: &mut LinkOut,
) -> Class {
    let name = decl.name.clone();
    let display = object::display_name(&name);
    let mut consts: Vec<Rc<ClassConst>> = Vec::new();
    let mut const_map: HashMap<ZStr, usize> = HashMap::new();
    let mut add_const =
        |c: Rc<ClassConst>, consts: &mut Vec<Rc<ClassConst>>, map: &mut HashMap<ZStr, usize>| {
            if let Some(&i) = map.get(&c.name) {
                consts[i] = c;
            } else {
                map.insert(c.name.clone(), consts.len());
                consts.push(c);
            }
        };
    if let Some(p) = &parent {
        for c in &p.consts {
            if c.flags & flags::PRIVATE == 0 {
                add_const(c.clone(), &mut consts, &mut const_map);
            }
        }
    }
    for i in &interfaces {
        for c in &i.consts {
            if !const_map.contains_key(&c.name) {
                add_const(c.clone(), &mut consts, &mut const_map);
            }
        }
    }
    // Traits' constants.
    for t in &traits {
        for c in &t.consts {
            if !const_map.contains_key(&c.name) {
                let nc = ClassConst {
                    class: name.clone(),
                    value: RefCell::new(None),
                    evaluating: Cell::new(false),
                    ..(**c).clone()
                };
                add_const(Rc::new(nc), &mut consts, &mut const_map);
            }
        }
    }
    for cd in &decl.consts {
        if let Some(&i) = const_map.get(&cd.name) {
            let existing = &consts[i];
            if existing.flags & flags::FINAL != 0 && err.is_none() {
                *err = Some(format!(
                    "{}::{} cannot override final constant {}::{}",
                    display,
                    cd.name,
                    object::display_name(&existing.class),
                    cd.name
                ));
            }
        }
        let value = match &cd.value {
            ConstExpr::Value(v) if cd.ty.is_none() => Some(v.clone()),
            _ => None,
        };
        add_const(
            Rc::new(ClassConst {
                name: cd.name.clone(),
                value: RefCell::new(value),
                expr: cd.value.clone(),
                flags: cd.flags,
                class: name.clone(),
                ty: cd.ty.clone(),
                is_case: false,
                evaluating: Cell::new(false),
                doc_comment: cd.doc_comment.clone(),
                attributes: cd.attributes.clone(),
            }),
            &mut consts,
            &mut const_map,
        );
    }
    for (i, (cname, value, _line)) in decl.cases.iter().enumerate() {
        add_const(
            Rc::new(ClassConst {
                name: cname.clone(),
                value: RefCell::new(None),
                expr: value.clone().unwrap_or(ConstExpr::Value(Value::Null)),
                flags: flags::PUBLIC,
                class: name.clone(),
                ty: None,
                is_case: true,
                evaluating: Cell::new(false),
                doc_comment: None,
                attributes: decl.case_attributes.get(i).cloned().unwrap_or_default(),
            }),
            &mut consts,
            &mut const_map,
        );
    }

    // Properties.
    let mut props: Vec<Rc<PropInfo>> = parent.as_ref().map(|p| p.props.clone()).unwrap_or_default();
    let mut prop_map: HashMap<ZStr, Rc<PropInfo>> = HashMap::new();
    let mut static_props: Vec<StaticProp> = Vec::new();
    let mut static_map: HashMap<ZStr, usize> = HashMap::new();
    if let Some(p) = &parent {
        for (k, pi) in &p.prop_map {
            if pi.visibility() != flags::PRIVATE {
                prop_map.insert(k.clone(), pi.clone());
            }
        }
        for sp in &p.static_props {
            if sp.flags & flags::PRIVATE == 0 {
                static_map.insert(sp.name.clone(), static_props.len());
                static_props.push(sp.clone());
            }
        }
    }
    // (declaration, owner, index of the trait it comes from)
    let mut all_props: Vec<(crate::class::PropDecl, ZStr, Option<usize>)> = Vec::new();
    if decl.kind == ClassKind::Enum {
        let mk = |n: &str, ty: crate::types::TypeDecl| crate::class::PropDecl {
            name: ZStr::from(n),
            default: None,
            flags: flags::PUBLIC | flags::READONLY,
            ty: Some(ty),
            doc_comment: None,
            attributes: Vec::new(),
            line: decl.line_start,
            hooks: None,
        };
        all_props.push((
            mk(
                "name",
                crate::types::TypeDecl::simple(crate::types::mask::STRING),
            ),
            name.clone(),
            None,
        ));
        if let Some(b) = decl.backing {
            all_props.push((
                mk("value", crate::types::TypeDecl::simple(b)),
                name.clone(),
                None,
            ));
        }
    }
    // Own properties first; trait properties are bound after them
    // (`zend_do_traits_property_binding`).
    for p in &decl.props {
        all_props.push((p.clone(), name.clone(), None));
    }
    for (ti, t) in traits.iter().enumerate() {
        if let Some(td) = &t.decl {
            for p in &td.props {
                all_props.push((p.clone(), name.clone(), Some(ti)));
            }
        }
    }
    for (pd, owner, from_trait) in all_props {
        if let Some(ti) = from_trait {
            if let Some(e) = trait_property_conflict(
                &name,
                &traits,
                ti,
                &pd,
                &prop_map,
                &static_map,
                &static_props,
            ) {
                if err.is_none() {
                    *err = Some(e);
                }
                continue;
            }
            let own = |c: &ZStr| *c == name;
            if prop_map.get(&pd.name).is_some_and(|e| own(&e.class))
                || static_map
                    .get(&pd.name)
                    .is_some_and(|&i| own(&static_props[i].class))
            {
                // Compatible with the own declaration: nothing to add.
                continue;
            }
            if cflag & cflags::READONLY != 0 && pd.flags & flags::READONLY == 0 && err.is_none() {
                *err = Some(format!(
                    "Readonly class {} cannot use trait with a non-readonly property {}::${}",
                    name, traits[ti].name, pd.name
                ));
            }
        }
        // `do_inherit_property`: a final parent property cannot be redeclared.
        let inherited_final = prop_map
            .get(&pd.name)
            .filter(|e| e.class != name)
            .map(|e| (e.flags, e.class.clone()))
            .or_else(|| {
                static_map
                    .get(&pd.name)
                    .map(|&i| &static_props[i])
                    .filter(|sp| sp.class != name)
                    .map(|sp| (sp.flags, sp.class.clone()))
            });
        if let Some((f, owner_class)) = inherited_final {
            if f & flags::FINAL != 0 && err.is_none() {
                *err = Some(format!(
                    "Cannot override final property {owner_class}::${}",
                    pd.name
                ));
            }
        }
        if let (Some(ph), Some(ch)) = (
            prop_map
                .get(&pd.name)
                .filter(|e| e.class != name)
                .and_then(|e| e.hooks.as_ref()),
            pd.hooks.as_ref(),
        ) {
            let fin = if ph.final_get && ch.get.is_some() {
                Some("get")
            } else if ph.final_set && ch.set.is_some() {
                Some("set")
            } else {
                None
            };
            if let Some(kind) = fin {
                if err.is_none() {
                    *err = Some(format!(
                        "Cannot override final property hook {}::${}::{kind}()",
                        ph.class, pd.name
                    ));
                }
            }
        }
        if pd.flags & flags::STATIC != 0 {
            let sp = StaticProp {
                name: pd.name.clone(),
                flags: pd.flags,
                ty: pd.ty.clone(),
                class: owner.clone(),
                cell: Rc::new(RefCell::new(Value::Null)),
                default: pd.default.clone(),
                initialized: Rc::new(Cell::new(false)),
                doc_comment: pd.doc_comment.clone(),
            };
            if let Some(&i) = static_map.get(&pd.name) {
                static_props[i] = sp;
            } else {
                static_map.insert(pd.name.clone(), static_props.len());
                static_props.push(sp);
            }
            continue;
        }
        let existing = prop_map.get(&pd.name).cloned();
        let slot = match &existing {
            Some(e) => {
                if e.visibility() != flags::PRIVATE {
                    let weaker = match e.visibility() {
                        flags::PUBLIC => pd.flags & flags::VISIBILITY != flags::PUBLIC,
                        flags::PROTECTED => pd.flags & flags::PRIVATE != 0,
                        _ => false,
                    };
                    if weaker && err.is_none() {
                        *err = Some(format!(
                            "Access level to {}::${} must be {} (as in class {}){}",
                            display,
                            pd.name,
                            visibility_name(e.flags),
                            object::display_name(&e.class),
                            if e.visibility() == flags::PROTECTED {
                                " or weaker"
                            } else {
                                ""
                            }
                        ));
                    }
                    // `do_inherit_property`: an abstract (hooked) parent
                    // property may be implemented by a readonly one.
                    if e.flags & flags::READONLY != pd.flags & flags::READONLY
                        && e.flags & flags::ABSTRACT == 0
                        && err.is_none()
                    {
                        *err =
                            Some(format!(
                            "Cannot redeclare {}readonly property {}::${} as {}readonly {}::${}",
                            if e.flags & flags::READONLY != 0 { "" } else { "non-" },
                            object::display_name(&e.class),
                            pd.name,
                            if pd.flags & flags::READONLY != 0 { "" } else { "non-" },
                            display,
                            pd.name
                        ));
                    }
                    // Set visibility may only be the parent's (or weaker);
                    // get-only virtual parents have none.
                    let parent_get_only = e.is_virtual()
                        && e.hooks
                            .as_ref()
                            .is_some_and(|h| h.set.is_none() && !h.abstract_set);
                    if pd.flags & flags::SET_MASK != 0 && !parent_get_only && err.is_none() {
                        let parent_set = match e.flags & flags::SET_MASK {
                            0 => match e.visibility() {
                                flags::PRIVATE => flags::SET_PRIVATE,
                                flags::PROTECTED => flags::SET_PROTECTED,
                                _ => flags::SET_PUBLIC,
                            },
                            s => s,
                        };
                        if pd.flags & flags::SET_MASK > parent_set {
                            let shown = if e.flags & flags::SET_PRIVATE != 0 {
                                "private(set)"
                            } else if e.flags & flags::SET_PROTECTED != 0 {
                                "protected(set)"
                            } else {
                                "omitted"
                            };
                            *err = Some(format!(
                                "Set access level of {}::${} must be {shown} (as in class {}){}",
                                name,
                                pd.name,
                                e.class,
                                if e.flags & flags::SET_MASK == 0 {
                                    ""
                                } else {
                                    " or weaker"
                                }
                            ));
                        }
                    }
                    e.slot
                } else {
                    u32::try_from(props.len()).unwrap_or(0)
                }
            }
            None => u32::try_from(props.len()).unwrap_or(0),
        };
        if let Some(sp) = static_map.get(&pd.name) {
            let sp = &static_props[*sp];
            if err.is_none() {
                *err = Some(format!(
                    "Cannot redeclare static {}::${} as non static {}::${}",
                    object::display_name(&sp.class),
                    pd.name,
                    display,
                    pd.name
                ));
            }
        }
        let mut pflags = pd.flags;
        if existing
            .as_ref()
            .is_some_and(|e| e.visibility() != flags::PRIVATE && !e.is_virtual())
        {
            pflags &= !flags::VIRTUAL;
        }
        let pi = Rc::new(PropInfo {
            name: pd.name.clone(),
            slot,
            flags: pflags,
            ty: pd.ty.clone(),
            class: owner,
            default: pd.default.clone(),
            doc_comment: pd.doc_comment.clone(),
            attributes: pd.attributes.clone(),
            hooks: pd.hooks.clone(),
        });
        if (slot as usize) < props.len() {
            props[slot as usize] = pi.clone();
        } else {
            props.push(pi.clone());
        }
        prop_map.insert(pd.name.clone(), pi);
    }
    // Private props of this class must be accessible by name too.

    // Methods.
    let mut methods: HashMap<ZStr, Rc<Method>> = HashMap::new();
    let mut method_order: Vec<ZStr> = Vec::new();
    // Own methods first (for get_class_methods order), then trait methods,
    // then inherited ones.
    for md in &decl.methods {
        let lc = ZStr::from(md.name.to_ascii_lowercase());
        let proto = parent
            .as_ref()
            .and_then(|p| p.methods.get(&lc))
            .filter(|m| m.visibility() != flags::PRIVATE)
            .map_or_else(|| weak.clone(), |m| m.proto_scope.clone());
        let m = Rc::new(Method {
            name: md.name.clone(),
            func: Func::User(md.func.clone()),
            flags: md.flags,
            scope: weak.clone(),
            proto_scope: proto,
        });
        if methods.insert(lc.clone(), m).is_none() {
            method_order.push(lc);
        }
    }
    if decl.kind == ClassKind::Enum {
        for (i, (sig, fl, h)) in crate::builtins::enum_methods().into_iter().enumerate() {
            if i > 0 && decl.backing.is_none() {
                break;
            }
            let s = crate::builtins::sig::parse_sig(sig);
            let lc = ZStr::from(s.name.to_ascii_lowercase());
            if methods.contains_key(&lc) {
                continue;
            }
            let b = crate::func::Builtin {
                name: ZStr::from(s.name.as_str()),
                params: s.params,
                defaults: s.defaults,
                required: s.required,
                return_type: s.ret,
                flags: fl,
                handler: h,
            };
            method_order.push(lc.clone());
            methods.insert(
                lc,
                Rc::new(Method {
                    name: ZStr::from(s.name.as_str()),
                    func: Func::Builtin(Rc::new(b)),
                    flags: fl,
                    scope: weak.clone(),
                    proto_scope: weak.clone(),
                }),
            );
        }
    }
    // `zend_do_traits_method_binding`: non-abstract trait methods are bound
    // before inheritance, abstract ones (requirements) after it.
    let mut binder = TraitMethodBinder {
        class: &name,
        weak,
        parent: parent.as_ref(),
        traits: &traits,
        rules,
        methods: &mut methods,
        order: &mut method_order,
        unfixed: HashMap::new(),
        checks: &mut link_out.trait_checks,
        warnings: &mut link_out.warnings,
        err,
    };
    binder.bind(decl, false);
    // `zend_fixup_trait_method`: the copies now belong to the class.
    binder.unfixed.clear();
    if let Some(p) = &parent {
        for lc in &p.method_order {
            if binder.methods.contains_key(lc) {
                continue;
            }
            if let Some(m) = p.methods.get(lc) {
                binder.order.push(lc.clone());
                binder.methods.insert(lc.clone(), m.clone());
            }
        }
    }
    binder.bind(decl, true);
    for i in &interfaces {
        for lc in &i.method_order {
            if methods.contains_key(lc) {
                continue;
            }
            if let Some(m) = i.methods.get(lc) {
                method_order.push(lc.clone());
                methods.insert(lc.clone(), m.clone());
            }
        }
    }
    let mut cflag = cflag;
    if decl.kind == ClassKind::Interface {
        cflag |= cflags::ABSTRACT;
    }
    let magic = build_magic(&methods);
    let internal = if parent
        .as_ref()
        .is_some_and(|p| p.internal == InternalKind::Throwable)
    {
        InternalKind::Throwable
    } else {
        InternalKind::None
    };
    let _ = is_anon;
    let hooks = parent.as_ref().and_then(|p| p.hooks.clone());
    Class {
        lc_name: ZStr::from(name.to_ascii_lowercase()),
        name,
        flags: cflag,
        parent,
        interfaces,
        traits,
        consts,
        const_map,
        props,
        prop_map,
        static_props,
        static_map,
        methods,
        method_order,
        magic,
        defaults: RefCell::new(None),
        backing: decl.backing,
        enum_cases: RefCell::new(HashMap::new()),
        decl: Some(decl.clone()),
        internal,
        attributes: decl.attributes.clone(),
        filename: Some(decl.filename.clone()),
        line_start: decl.line_start,
        line_end: decl.line_end,
        doc_comment: decl.doc_comment.clone(),
        linked: Cell::new(true),
        hooks,
    }
}
