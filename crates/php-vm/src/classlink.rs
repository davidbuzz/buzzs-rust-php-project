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
        let bytes = name.as_bytes().strip_prefix(b"\\").unwrap_or(name.as_bytes());
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
        self.classes.get(&ZStr::from(bytes.to_ascii_lowercase())).cloned()
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
                let n = self.lit_str(*l);
                self.lookup_class_or_error(&n)
            }
            ClassSpec::SelfClass => match self.scope() {
                Some(c) => Ok(c),
                None => Err(self.make_throw("Error", "Cannot use \"self\" when no class scope is active")),
            },
            ClassSpec::Parent => match self.scope() {
                Some(c) => match &c.parent {
                    Some(p) => Ok(p.clone()),
                    None => Err(self.make_throw(
                        "Error",
                        "Cannot use \"parent\" when current class scope has no parent",
                    )),
                },
                None => Err(self.make_throw("Error", "Cannot use \"parent\" when no class scope is active")),
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
                    None => Err(self.make_throw("Error", "Cannot use \"static\" when no class scope is active")),
                }
            }
            ClassSpec::Dyn(op) => {
                let v = self.read(*op)?;
                match v {
                    Value::Object(o) => Ok(object::class_of(&o).clone()),
                    Value::Str(s) => self.lookup_class_or_error(&s),
                    _ => Err(self.make_throw("Error", "Class name must be a valid object or a string")),
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
            return Ok(Value::Str(self.scope().map(|c| c.name.clone()).unwrap_or_default()));
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
                        return Err(self.make_throw("Error", "Class name must be a valid object or a string"))
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
    pub(crate) fn eval_const_expr(&mut self, e: &ConstExpr, scope: Option<&Rc<Class>>) -> VmResult<Value> {
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
    pub(crate) fn op_fetch_class_const(&mut self, spec: &ClassSpec, name: Value) -> VmResult<Value> {
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
        self.class_constant(&cls, &name)
    }

    /// Reads a class constant (evaluating it lazily).
    pub(crate) fn class_constant(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Value> {
        if name.as_bytes() == b"class" {
            return Ok(Value::Str(cls.name.clone()));
        }
        let Some(c) = cls.find_const(name.as_bytes()).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Undefined constant {}::{}",
                    object::display_name(&cls.name),
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
                let vis = if c.flags & flags::PRIVATE != 0 { "private" } else { "protected" };
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Cannot access {vis} constant {}::{}",
                        object::display_name(&cls.name),
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
                    object::display_name(&c.class),
                    name.to_string_lossy()
                ),
            ));
        }
        c.evaluating.set(true);
        let decl = self.find_class(c.class.as_bytes()).unwrap_or_else(|| cls.clone());
        let r = self.eval_const_expr(&c.expr, Some(&decl));
        c.evaluating.set(false);
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
            self.warning(&format!("Constant {} already defined", name.to_string_lossy()))?;
            return Ok(false);
        }
        self.user_constants.push(name.clone());
        self.constants.insert(name, v);
        Ok(true)
    }

    /// Reads a global constant.
    pub(crate) fn fetch_constant(&mut self, name: u32, fallback: Option<u32>) -> VmResult<Value> {
        let n = self.lit_str(name);
        if let Some(v) = self.constants.get(&n) {
            return Ok(v.clone());
        }
        if let Some(v) = self.special_constant(n.as_bytes()) {
            return Ok(v);
        }
        if let Some(fb) = fallback {
            let f = self.lit_str(fb);
            if let Some(v) = self.constants.get(&f) {
                return Ok(v.clone());
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
    pub(crate) fn static_prop_cell(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Rc<RefCell<Value>>> {
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
                let vis = if sp.flags & flags::PRIVATE != 0 { "private" } else { "protected" };
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
                let decl = self.find_class(sp.class.as_bytes()).unwrap_or_else(|| cls.clone());
                let v = self.eval_const_expr(d, Some(&decl))?;
                *sp.cell.borrow_mut() = v;
            }
        }
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
    pub(crate) fn declare_class(&mut self, decl: &Rc<ClassDecl>, is_anon: bool) -> VmResult<Rc<Class>> {
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
        self.classes.insert(lc, cls);
    }

    /// Whether all dependencies of a declaration are available (for early
    /// binding).
    pub(crate) fn can_early_bind(&self, decl: &ClassDecl) -> bool {
        let has = |n: &ZStr| self.find_class(n.as_bytes()).is_some();
        decl.parent.as_ref().is_none_or(has)
            && decl.interfaces.iter().all(has)
            && decl.traits.iter().all(has)
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
        let line = decl.line_start;
        // Parent.
        let parent = match &decl.parent {
            Some(p) => {
                let Some(pc) = self.lookup_class(p)? else {
                    let kind = if decl.kind == ClassKind::Interface { "Interface" } else { "Class" };
                    return Err(self.make_throw(
                        "Error",
                        &format!("{kind} \"{}\" not found", p.to_string_lossy()),
                    ));
                };
                if pc.is_interface() && decl.kind == ClassKind::Class {
                    return Err(self.fatal_at(
                        &format!("Class {display} cannot extend interface {}", pc.name),
                        line,
                    ));
                }
                if pc.is_trait() {
                    return Err(self.fatal_at(
                        &format!("Class {display} cannot extend trait {}", pc.name),
                        line,
                    ));
                }
                if pc.flags & cflags::FINAL != 0 {
                    let what = if pc.is_enum() { "enum" } else { "final class" };
                    return Err(self.fatal_at(
                        &format!("Class {display} cannot extend {what} {}", pc.name),
                        line,
                    ));
                }
                if pc.flags & cflags::READONLY != decl.flags & cflags::READONLY {
                    let (a, b) = if decl.flags & cflags::READONLY != 0 {
                        ("Readonly class", "non-readonly class")
                    } else {
                        ("Non-readonly class", "readonly class")
                    };
                    return Err(self.fatal_at(
                        &format!("{a} {display} cannot extend {b} {}", pc.name),
                        line,
                    ));
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
                        &format!("{display} cannot implement {} - it is not an interface", ic.name),
                        line,
                    ));
                }
                return Err(self.fatal_at(
                    &format!("{display} cannot implement {} - it is not an interface", ic.name),
                    line,
                ));
            }
            if decl.kind == ClassKind::Class
                && (ic.lc_name.as_bytes() == b"unitenum" || ic.lc_name.as_bytes() == b"backedenum")
            {
                return Err(self.fatal_at(
                    &format!("Non-enum class {display} cannot implement interface {}", ic.name),
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
            && interfaces.iter().any(|i| i.lc_name.as_bytes() == b"traversable")
            && !interfaces
                .iter()
                .any(|i| matches!(i.lc_name.as_bytes(), b"iterator" | b"iteratoraggregate"))
            && !parent.as_ref().is_some_and(|p| p.flags & cflags::INTERNAL != 0)
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
                return Err(self.fatal_at(
                    &format!("{display} cannot use {} - it is not a trait", tc.name),
                    line,
                ));
            }
            traits.push(tc);
        }
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
        let interfaces_c = interfaces.clone();
        let cls = Rc::new_cyclic(|weak: &Weak<Class>| {
            build_class(
                &decl_c,
                weak,
                parent_c,
                interfaces_c,
                traits_c,
                cflag,
                is_anon,
                &mut err,
            )
        });
        if let Some(e) = err {
            return Err(self.fatal_at(&e, line));
        }
        // Abstract method check.
        if !cls.is_interface() && !cls.is_trait() && cls.flags & cflags::EXPLICIT_ABSTRACT == 0 {
            let mut missing: Vec<String> = Vec::new();
            for lcname in &cls.method_order {
                if let Some(m) = cls.methods.get(lcname) {
                    if m.is_abstract() {
                        missing.push(format!("{}::{}", object::display_name(&m.scope().name), m.name));
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
                return Err(self.fatal_at(
                    &format!(
                        "{what} {display} contains {n} abstract method{} and must therefore be declared abstract or implement the remaining methods ({list})",
                        if n == 1 { "" } else { "s" }
                    ),
                    line,
                ));
            }
        }
        // Inheritance checks for overridden methods.
        if let Some(e) = check_method_inheritance(&cls) {
            return Err(self.fatal_at(&e.0, e.1));
        }
        Ok(cls)
    }
}

fn is_valid_class_name(n: &[u8]) -> bool {
    !n.is_empty()
        && n.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\\' || c >= 0x80)
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
        let Some(pm) = parent.methods.get(lc) else { continue };
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
                    if pv == flags::PROTECTED { " or weaker" } else { "" }
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
    cflag: u32,
    is_anon: bool,
    err: &mut Option<String>,
) -> Class {
    let name = decl.name.clone();
    let display = object::display_name(&name);
    let mut consts: Vec<Rc<ClassConst>> = Vec::new();
    let mut const_map: HashMap<ZStr, usize> = HashMap::new();
    let mut add_const = |c: Rc<ClassConst>, consts: &mut Vec<Rc<ClassConst>>, map: &mut HashMap<ZStr, usize>| {
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
    for (cname, value, _line) in &decl.cases {
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
                attributes: Vec::new(),
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
    let mut all_props: Vec<(crate::class::PropDecl, ZStr)> = Vec::new();
    if decl.kind == ClassKind::Enum {
        let mk = |n: &str, ty: crate::types::TypeDecl| crate::class::PropDecl {
            name: ZStr::from(n),
            default: None,
            flags: flags::PUBLIC | flags::READONLY,
            ty: Some(ty),
            doc_comment: None,
            attributes: Vec::new(),
            line: decl.line_start,
        };
        all_props.push((mk("name", crate::types::TypeDecl::simple(crate::types::mask::STRING)), name.clone()));
        if let Some(b) = decl.backing {
            all_props.push((mk("value", crate::types::TypeDecl::simple(b)), name.clone()));
        }
    }
    for t in &traits {
        if let Some(td) = &t.decl {
            for p in &td.props {
                all_props.push((p.clone(), name.clone()));
            }
        }
    }
    for p in &decl.props {
        all_props.push((p.clone(), name.clone()));
    }
    for (pd, owner) in all_props {
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
                            if e.visibility() == flags::PROTECTED { " or weaker" } else { "" }
                        ));
                    }
                    if e.flags & flags::READONLY != pd.flags & flags::READONLY && err.is_none() {
                        *err = Some(format!(
                            "Cannot redeclare {}readonly property {}::${} as {}readonly {}::${}",
                            if e.flags & flags::READONLY != 0 { "" } else { "non-" },
                            object::display_name(&e.class),
                            pd.name,
                            if pd.flags & flags::READONLY != 0 { "" } else { "non-" },
                            display,
                            pd.name
                        ));
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
        let pi = Rc::new(PropInfo {
            name: pd.name.clone(),
            slot,
            flags: pd.flags,
            ty: pd.ty.clone(),
            class: owner,
            default: pd.default.clone(),
            doc_comment: pd.doc_comment.clone(),
            attributes: pd.attributes.clone(),
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
    let mut add_method = |lc: ZStr, m: Rc<Method>, methods: &mut HashMap<ZStr, Rc<Method>>, order: &mut Vec<ZStr>| {
        if !methods.contains_key(&lc) {
            order.push(lc.clone());
        }
        methods.insert(lc, m);
    };
    // Own methods first (for get_class_methods order), then inherited.
    let mut own: Vec<(ZStr, Rc<Method>)> = Vec::new();
    for md in &decl.methods {
        let lc = ZStr::from(md.name.to_ascii_lowercase());
        let proto = parent
            .as_ref()
            .and_then(|p| p.methods.get(&lc))
            .filter(|m| m.visibility() != flags::PRIVATE)
            .map_or_else(|| weak.clone(), |m| m.proto_scope.clone());
        own.push((
            lc,
            Rc::new(Method {
                name: md.name.clone(),
                func: Func::User(md.func.clone()),
                flags: md.flags,
                scope: weak.clone(),
                proto_scope: proto,
            }),
        ));
    }
    // Trait methods.
    let mut trait_methods: Vec<(ZStr, Rc<Method>)> = Vec::new();
    if !traits.is_empty() {
        let mut excluded: Vec<(ZStr, ZStr)> = Vec::new(); // (trait lc, method lc)
        for r in &decl.trait_rules {
            if let TraitRule::Precedence {
                method, insteadof, ..
            } = r
            {
                for t in insteadof {
                    excluded.push((
                        ZStr::from(t.to_ascii_lowercase()),
                        ZStr::from(method.to_ascii_lowercase()),
                    ));
                }
            }
        }
        for t in &traits {
            for lcm in &t.method_order {
                let Some(tm) = t.methods.get(lcm) else { continue };
                // Aliases.
                for r in &decl.trait_rules {
                    if let TraitRule::Alias {
                        trait_name,
                        method,
                        flags: f,
                        alias,
                    } = r
                    {
                        if !method.eq_ignore_ascii_case(lcm.as_bytes()) {
                            continue;
                        }
                        if let Some(tn) = trait_name {
                            if !tn.eq_ignore_ascii_case(t.name.as_bytes()) {
                                continue;
                            }
                        }
                        if let Some(a) = alias {
                            let mut fl = tm.flags;
                            if *f & flags::VISIBILITY != 0 {
                                fl = (fl & !flags::VISIBILITY) | (*f & flags::VISIBILITY);
                            }
                            if *f & flags::FINAL != 0 {
                                fl |= flags::FINAL;
                            }
                            trait_methods.push((
                                ZStr::from(a.to_ascii_lowercase()),
                                Rc::new(Method {
                                    name: a.clone(),
                                    func: tm.func.clone(),
                                    flags: fl | flags::FROM_TRAIT,
                                    scope: weak.clone(),
                                    proto_scope: weak.clone(),
                                }),
                            ));
                        }
                    }
                }
                if excluded
                    .iter()
                    .any(|(tl, ml)| tl.as_bytes() == t.lc_name.as_bytes() && ml == lcm)
                {
                    continue;
                }
                let mut fl = tm.flags;
                for r in &decl.trait_rules {
                    if let TraitRule::Alias {
                        trait_name,
                        method,
                        flags: f,
                        alias: None,
                    } = r
                    {
                        if method.eq_ignore_ascii_case(lcm.as_bytes())
                            && trait_name.as_ref().is_none_or(|tn| tn.eq_ignore_ascii_case(t.name.as_bytes()))
                        {
                            if *f & flags::VISIBILITY != 0 {
                                fl = (fl & !flags::VISIBILITY) | (*f & flags::VISIBILITY);
                            }
                            if *f & flags::FINAL != 0 {
                                fl |= flags::FINAL;
                            }
                        }
                    }
                }
                if let Some((_, existing)) = trait_methods.iter().find(|(l, _)| l == lcm) {
                    let ex_trait = existing.func.clone();
                    let same = Func::ptr_eq(&ex_trait, &tm.func);
                    if !same
                        && !own.iter().any(|(l, _)| l == lcm)
                        && !tm.is_abstract()
                        && !existing.is_abstract()
                        && err.is_none()
                    {
                        let other = traits
                            .iter()
                            .find(|tt| tt.methods.get(lcm).is_some_and(|mm| Func::ptr_eq(&mm.func, &ex_trait)))
                            .map(|tt| tt.name.to_string_lossy())
                            .unwrap_or_default();
                        *err = Some(format!(
                            "Trait method {}::{} has not been applied as {}::{}, because of collision with {}::{}",
                            t.name, tm.name, display, tm.name, other, tm.name
                        ));
                    }
                    continue;
                }
                trait_methods.push((
                    lcm.clone(),
                    Rc::new(Method {
                        name: tm.name.clone(),
                        func: tm.func.clone(),
                        flags: fl | flags::FROM_TRAIT,
                        scope: weak.clone(),
                        proto_scope: weak.clone(),
                    }),
                ));
            }
        }
    }
    for (lc, m) in own {
        add_method(lc, m, &mut methods, &mut method_order);
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
            add_method(
                lc,
                Rc::new(Method {
                    name: ZStr::from(s.name.as_str()),
                    func: Func::Builtin(Rc::new(b)),
                    flags: fl,
                    scope: weak.clone(),
                    proto_scope: weak.clone(),
                }),
                &mut methods,
                &mut method_order,
            );
        }
    }
    for (lc, m) in trait_methods {
        if methods.contains_key(&lc) {
            continue;
        }
        add_method(lc, m, &mut methods, &mut method_order);
    }
    if let Some(p) = &parent {
        for lc in &p.method_order {
            if methods.contains_key(lc) {
                continue;
            }
            if let Some(m) = p.methods.get(lc) {
                add_method(lc.clone(), m.clone(), &mut methods, &mut method_order);
            }
        }
    }
    for i in &interfaces {
        for lc in &i.method_order {
            if methods.contains_key(lc) {
                continue;
            }
            if let Some(m) = i.methods.get(lc) {
                add_method(lc.clone(), m.clone(), &mut methods, &mut method_order);
            }
        }
    }
    let mut cflag = cflag;
    if decl.kind == ClassKind::Interface {
        cflag |= cflags::ABSTRACT;
    }
    let magic = build_magic(&methods);
    let internal = if parent.as_ref().is_some_and(|p| p.internal == InternalKind::Throwable) {
        InternalKind::Throwable
    } else {
        InternalKind::None
    };
    let _ = is_anon;
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
    }
}
