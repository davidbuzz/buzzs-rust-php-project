// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Method signature compatibility (`zend_do_perform_implementation_check`,
//! `zend_perform_covariant_type_check`) and the "Declaration of ... must be
//! compatible with ..." diagnostics.

use crate::class::{cflags, Class, ClassDecl, ConstExpr};
use crate::func::{flags, Func, Method};
use crate::object::display_name;
use crate::opcode::ParamInfo;
use crate::types::{mask, TypeDecl};
use crate::vm::Vm;
use php_runtime::{Value, ZStr};
use std::rc::Rc;

/// Result of a compatibility check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Error,
    /// Missing return type on a tentative (internal) prototype.
    Warning,
    /// A class needed for the check is not available (yet).
    Unresolved,
}

/// A class-like member of a type: a class name or an intersection group.
enum Member {
    Class(ZStr),
    Group(Vec<ZStr>),
}

/// A type as `zend_perform_covariant_type_check` sees it: `iterable` is
/// `Traversable|array`.
struct Ty {
    mask: u32,
    members: Vec<Member>,
    is_intersection: bool,
}

impl Ty {
    fn of(t: &TypeDecl) -> Self {
        let mut mask = t.mask;
        let mut members = Vec::new();
        if t.pure_intersection {
            if let Some(g) = t.intersections.first() {
                members.extend(g.iter().cloned().map(Member::Class));
            }
            return Ty {
                mask,
                members,
                is_intersection: true,
            };
        }
        members.extend(t.intersections.iter().cloned().map(Member::Group));
        members.extend(t.classes.iter().cloned().map(Member::Class));
        if mask & mask::ITERABLE != 0 {
            mask = (mask & !mask::ITERABLE) | mask::ARRAY;
            members.push(Member::Class(ZStr::from("Traversable")));
        }
        Ty {
            mask,
            members,
            is_intersection: false,
        }
    }

    fn group(g: &[ZStr]) -> Self {
        Ty {
            mask: 0,
            members: g.iter().cloned().map(Member::Class).collect(),
            is_intersection: true,
        }
    }

    fn names(&self) -> impl Iterator<Item = &ZStr> + '_ {
        self.members.iter().flat_map(|m| match m {
            Member::Class(n) => std::slice::from_ref(n).iter(),
            Member::Group(g) => g.iter(),
        })
    }
}

/// A class found by a variance check: linked, or still being linked (only
/// its declaration is known; `ZEND_FETCH_CLASS_ALLOW_UNLINKED`).
pub(crate) enum Found {
    Linked(Rc<Class>),
    Unlinked(Rc<ClassDecl>),
}

impl Found {
    fn name(&self) -> &ZStr {
        match self {
            Found::Linked(c) => &c.name,
            Found::Unlinked(d) => &d.name,
        }
    }
}

/// Class lookups of a variance check. Names that cannot be resolved yet
/// are recorded in order (`register_unresolved_classes`, i.e.
/// `CG(delayed_autoloads)`).
pub(crate) struct Checker<'a> {
    vm: &'a Vm,
    /// The class being linked (not in the class table yet).
    linking: &'a Rc<Class>,
    /// Unresolved class names, in registration order.
    pub(crate) unresolved: Vec<ZStr>,
}

impl<'a> Checker<'a> {
    pub(crate) fn new(vm: &'a Vm, linking: &'a Rc<Class>) -> Self {
        Checker {
            vm,
            linking,
            unresolved: Vec::new(),
        }
    }

    /// `resolve_class_name`: `self` / `parent` in `scope`.
    fn resolve(scope: &Rc<Class>, n: &ZStr) -> ZStr {
        if n.eq_ignore_ascii_case(b"self") {
            scope.name.clone()
        } else if n.eq_ignore_ascii_case(b"parent") {
            scope.parent_name().cloned().unwrap_or_else(|| n.clone())
        } else {
            n.clone()
        }
    }

    /// `lookup_class` (`ZEND_FETCH_CLASS_ALLOW_UNLINKED`, no autoloading;
    /// the class being linked and the scopes involved count as available).
    fn lookup(&self, scope: &Rc<Class>, n: &ZStr) -> Option<Found> {
        for c in [self.linking, scope] {
            if c.name.eq_ignore_ascii_case(n.as_bytes()) {
                return Some(Found::Linked(c.clone()));
            }
        }
        self.lookup_global(n)
    }

    fn lookup_global(&self, n: &ZStr) -> Option<Found> {
        if self.linking.name.eq_ignore_ascii_case(n.as_bytes()) {
            return Some(Found::Linked(self.linking.clone()));
        }
        self.vm
            .find_class(n.as_bytes())
            .map(Found::Linked)
            .or_else(|| crate::classlink::variance_decl(n.as_bytes()).map(Found::Unlinked))
    }

    /// `unlinked_instanceof`.
    fn instance_of(&self, a: &Found, b: &Found, depth: u32) -> bool {
        if a.name().eq_ignore_ascii_case(b.name().as_bytes()) {
            return true;
        }
        match (a, b) {
            (Found::Linked(x), Found::Linked(y)) => {
                if y.is_interface() {
                    x.instance_of_name(y.lc_name.as_bytes())
                } else {
                    x.is_subclass_of(y)
                }
            }

            (Found::Linked(_), Found::Unlinked(_)) => false,
            (Found::Unlinked(d), _) => {
                if depth > 64 {
                    return false;
                }
                d.parent
                    .iter()
                    .chain(d.interfaces.iter())
                    .filter(|n| !n.eq_ignore_ascii_case(d.name.as_bytes()))
                    .any(|n| {
                        self.lookup_global(n)
                            .is_some_and(|p| self.instance_of(&p, b, depth + 1))
                    })
            }
        }
    }

    fn register_unresolved(&mut self, scope: &Rc<Class>, t: &Ty) {
        for n in t.names() {
            let n = Self::resolve(scope, n);
            if self.lookup(scope, &n).is_none()
                && !self
                    .unresolved
                    .iter()
                    .any(|u| u.eq_ignore_ascii_case(n.as_bytes()))
            {
                self.unresolved.push(n);
            }
        }
    }

    /// `zend_type_permits_self`.
    fn permits_self(&self, proto: &Ty, proto_scope: &Rc<Class>, fe_scope: &Rc<Class>) -> bool {
        if proto.mask & mask::OBJECT != 0 {
            return true;
        }
        proto.members.iter().any(|m| match m {
            Member::Class(n) => self
                .lookup(fe_scope, &Self::resolve(proto_scope, n))
                .is_some_and(|c| self.instance_of(&Found::Linked(fe_scope.clone()), &c, 0)),
            Member::Group(_) => false,
        })
    }

    /// `zend_perform_covariant_type_check`: is `fe` a subtype of `proto`?
    pub(crate) fn covariant(
        &mut self,
        fe_scope: &Rc<Class>,
        fe: &TypeDecl,
        proto_scope: &Rc<Class>,
        proto: &TypeDecl,
    ) -> Status {
        let (fe, proto) = (Ty::of(fe), Ty::of(proto));
        // Everything but void is trivially covariant to mixed.
        if proto.mask & mask::ANY == mask::ANY
            && proto.members.is_empty()
            && fe.mask & mask::VOID == 0
        {
            return Status::Ok;
        }
        // Builtin types may be removed, but not added.
        let mut added = fe.mask & !proto.mask;
        if added != 0 {
            if added & mask::STATIC != 0 && self.permits_self(&proto, proto_scope, fe_scope) {
                added &= !mask::STATIC;
            }
            if added == mask::NEVER {
                return Status::Ok;
            }
            if added != 0 {
                return Status::Error;
            }
        }
        let mut have_unresolved = false;
        let early = if fe.is_intersection {
            let early = if proto.is_intersection {
                Status::Error
            } else {
                Status::Ok
            };
            let st = self.intersection_subtype_of_type(fe_scope, &fe, proto_scope, &proto);
            if st == early {
                return st;
            }
            have_unresolved = st == Status::Unresolved;
            early
        } else {
            for m in &fe.members {
                let st = match m {
                    Member::Group(g) => self.intersection_subtype_of_type(
                        fe_scope,
                        &Ty::group(g),
                        proto_scope,
                        &proto,
                    ),
                    Member::Class(n) => {
                        let n = Self::resolve(fe_scope, n);
                        self.class_subtype_of_type(fe_scope, &n, proto_scope, &proto)
                    }
                };
                if st == Status::Error {
                    return st;
                }
                have_unresolved |= st == Status::Unresolved;
            }
            Status::Error
        };
        if !have_unresolved {
            return if early == Status::Error {
                Status::Ok
            } else {
                Status::Error
            };
        }
        self.register_unresolved(fe_scope, &fe);
        self.register_unresolved(proto_scope, &proto);
        Status::Unresolved
    }

    /// `zend_is_class_subtype_of_type`.
    fn class_subtype_of_type(
        &mut self,
        fe_scope: &Rc<Class>,
        fe_name: &ZStr,
        proto_scope: &Rc<Class>,
        proto: &Ty,
    ) -> Status {
        let fe_ce = self.lookup(fe_scope, fe_name);
        let mut have_unresolved = false;
        if proto.mask & mask::OBJECT != 0 {
            if fe_ce.is_some() {
                return Status::Ok;
            }
            have_unresolved = true;
        }
        if proto.mask & mask::CALLABLE != 0 {
            match &fe_ce {
                None => have_unresolved = true,
                Some(c) if c.name().eq_ignore_ascii_case(b"closure") => return Status::Ok,
                Some(_) => {}
            }
        }
        if proto.mask & mask::STATIC != 0 && fe_scope.flags & cflags::FINAL != 0 {
            match &fe_ce {
                None => have_unresolved = true,
                Some(Found::Linked(c)) if Rc::ptr_eq(c, fe_scope) => return Status::Ok,
                Some(_) => {}
            }
        }
        let inter = proto.is_intersection;
        for m in &proto.members {
            match m {
                Member::Group(g) => {
                    match self.class_subtype_of_type(fe_scope, fe_name, proto_scope, &Ty::group(g))
                    {
                        Status::Error if inter => return Status::Error,
                        Status::Unresolved => have_unresolved = true,
                        Status::Ok if !inter => return Status::Ok,
                        _ => {}
                    }
                }
                Member::Class(n) => {
                    let pn = Self::resolve(proto_scope, n);
                    if pn.eq_ignore_ascii_case(fe_name.as_bytes()) {
                        if !inter {
                            return Status::Ok;
                        }
                        continue;
                    }
                    let proto_ce = self.lookup(proto_scope, &pn);
                    let (Some(f), Some(p)) = (&fe_ce, &proto_ce) else {
                        have_unresolved = true;
                        continue;
                    };
                    if self.instance_of(f, p, 0) {
                        if !inter {
                            return Status::Ok;
                        }
                    } else if inter {
                        return Status::Error;
                    }
                }
            }
        }
        if have_unresolved {
            Status::Unresolved
        } else if inter {
            Status::Ok
        } else {
            Status::Error
        }
    }

    /// `zend_is_intersection_subtype_of_class`.
    fn intersection_subtype_of_class(
        &self,
        fe_scope: &Rc<Class>,
        fe: &Ty,
        proto_scope: &Rc<Class>,
        proto_name: &ZStr,
    ) -> Status {
        let mut have_unresolved = false;
        let proto_ce = self.lookup(proto_scope, proto_name);
        for n in fe.names() {
            let n = Self::resolve(fe_scope, n);
            if n.eq_ignore_ascii_case(proto_name.as_bytes()) {
                return Status::Ok;
            }
            let (Some(f), Some(p)) = (self.lookup(fe_scope, &n), &proto_ce) else {
                have_unresolved = true;
                continue;
            };
            if self.instance_of(&f, p, 0) {
                return Status::Ok;
            }
        }
        if have_unresolved {
            Status::Unresolved
        } else {
            Status::Error
        }
    }

    /// `zend_is_intersection_subtype_of_type`.
    fn intersection_subtype_of_type(
        &mut self,
        fe_scope: &Rc<Class>,
        fe: &Ty,
        proto_scope: &Rc<Class>,
        proto: &Ty,
    ) -> Status {
        let mut have_unresolved = false;
        if proto.mask & mask::OBJECT != 0 {
            for n in fe.names() {
                if self.lookup(fe_scope, &Self::resolve(fe_scope, n)).is_some() {
                    return Status::Ok;
                }
                have_unresolved = true;
            }
        }
        let early = if proto.is_intersection {
            Status::Error
        } else {
            Status::Ok
        };
        for m in &proto.members {
            let st = match m {
                Member::Group(g) => {
                    self.intersection_subtype_of_type(fe_scope, fe, proto_scope, &Ty::group(g))
                }
                Member::Class(n) => {
                    let pn = Self::resolve(proto_scope, n);
                    self.intersection_subtype_of_class(fe_scope, fe, proto_scope, &pn)
                }
            };
            if st == early {
                return st;
            }
            have_unresolved |= st == Status::Unresolved;
        }
        if have_unresolved {
            Status::Unresolved
        } else if early == Status::Error {
            Status::Ok
        } else {
            Status::Error
        }
    }

    /// `zend_do_perform_implementation_check`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn implementation_check(
        &mut self,
        fe: &Func,
        fe_flags: u32,
        fe_scope: &Rc<Class>,
        proto: &Func,
        proto_flags: u32,
        proto_scope: &Rc<Class>,
    ) -> Status {
        if proto.required() < fe.required() {
            return Status::Error;
        }
        if returns_ref(proto, proto_flags) && !returns_ref(fe, fe_flags) {
            return Status::Error;
        }
        let pv = proto.is_variadic();
        let fv = fe.is_variadic();
        if pv && !fv {
            return Status::Error;
        }
        let pp = proto.params();
        let fp = fe.params();
        let mut status = Status::Ok;
        for i in 0..pp.len().max(fp.len()) {
            let pa = pp.get(i).or_else(|| if pv { pp.last() } else { None });
            let fa = fp.get(i).or_else(|| if fv { fp.last() } else { None });
            let Some(pa) = pa else { continue };
            let Some(fa) = fa else { return Status::Error };
            // Contravariant parameter types.
            if let Some(ft) = fa.ty.as_ref().filter(|t| !t.is_mixed()) {
                let Some(pt) = &pa.ty else {
                    return Status::Error;
                };
                match self.covariant(proto_scope, pt, fe_scope, ft) {
                    Status::Error => return Status::Error,
                    Status::Unresolved => status = Status::Unresolved,
                    _ => {}
                }
            }
            if fa.by_ref != pa.by_ref {
                return Status::Error;
            }
        }
        // Return types are covariant; a tentative (internal) prototype
        // return type only warns.
        if let Some(prt) = proto.return_type() {
            let tentative = !proto.is_user();
            let Some(frt) = fe.return_type() else {
                if !tentative {
                    return Status::Error;
                }
                return if status == Status::Ok {
                    Status::Warning
                } else {
                    status
                };
            };
            match self.covariant(fe_scope, frt, proto_scope, prt) {
                Status::Ok => {}
                Status::Error if tentative => return Status::Warning,
                st => return st,
            }
        }
        status
    }
}

/// Whether a method returns by reference (the flag is on the function).
fn returns_ref(f: &Func, fflags: u32) -> bool {
    fflags & flags::RETURN_REF != 0
        || matches!(f, Func::User(u) if u.flags & flags::RETURN_REF != 0)
}

/// The file and first line of a function, for errors about it
/// (`func_filename` / `func_lineno`).
pub(crate) fn func_location(f: &Func) -> (ZStr, u32) {
    match f {
        Func::User(u) => (u.code.filename.clone(), u.line_start),
        Func::Builtin(_) => (ZStr::from("Unknown"), 0),
    }
}

fn render_default(p: &ParamInfo, f: &Func, scope: &ZStr) -> String {
    if !f.is_user() {
        return p
            .default_text
            .clone()
            .unwrap_or_else(|| "<default>".to_string());
    }
    match &p.default_expr {
        Some(ConstExpr::Value(v)) => match v {
            Value::Bool(false) => "false".into(),
            Value::Bool(true) => "true".into(),
            Value::Null => "null".into(),
            Value::Str(s) => {
                let b = s.as_bytes();
                let n = b.len().min(10);
                let mut out = format!("'{}", String::from_utf8_lossy(&b[..n]));
                if b.len() > 10 {
                    out.push_str("...");
                }
                out.push('\'');
                out
            }
            Value::Array(a) => {
                if a.is_empty() {
                    "[]".into()
                } else {
                    "[...]".into()
                }
            }
            Value::Int(i) => i.to_string(),
            // `smart_str_append_double(.., EG(precision))`.
            Value::Float(f) => php_runtime::float::format_double(*f, 14, false),
            _ => "<expression>".into(),
        },
        _ => {
            let t = p.default_text.clone().unwrap_or_default();
            let t = t.trim();
            // `self::class` outside a trait is compiled to the class name.
            if t.eq_ignore_ascii_case("self::class") && !scope.is_empty() {
                return render_default(
                    &ParamInfo {
                        default_expr: Some(ConstExpr::Value(Value::Str(scope.clone()))),
                        ..p.clone()
                    },
                    f,
                    scope,
                );
            }
            let ident = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\\')
            };
            if ident(t) {
                t.trim_start_matches('\\').to_string()
            } else if let Some((a, b)) = t.split_once("::") {
                if ident(a) && ident(b) {
                    format!("{}::{}", a.trim_start_matches('\\'), b)
                } else {
                    "<expression>".into()
                }
            } else {
                "<expression>".into()
            }
        }
    }
}

/// `zend_get_function_declaration`.
#[allow(dead_code)]
pub(crate) fn declaration(f: &Func, fflags: u32, scope: &Rc<Class>) -> String {
    declaration_as(f, fflags, &display_name(&scope.name), scope)
}

/// [`declaration`] of a function shown as a method of `shown` (its
/// `common.scope`) whose `self`/`parent` types resolve in `scope` (which
/// differs for trait methods).
pub(crate) fn declaration_as(f: &Func, fflags: u32, shown: &ZStr, scope: &Rc<Class>) -> String {
    let mut s = String::new();
    if returns_ref(f, fflags) {
        s.push('&');
    }
    s.push_str(&shown.to_string_lossy());
    s.push_str("::");
    s.push_str(&f.name().to_string_lossy());
    s.push('(');
    let req = f.required() as usize;
    let parent_name = scope.parent_name().cloned();
    for (i, p) in f.params().iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if let Some(t) = &p.ty {
            s.push_str(&t.display(Some(&scope.name), parent_name.as_ref(), None));
            s.push(' ');
        }
        if p.by_ref {
            s.push('&');
        }
        if p.variadic {
            s.push_str("...");
        }
        s.push('$');
        s.push_str(&p.name.to_string_lossy());
        if i >= req && !p.variadic {
            s.push_str(" = ");
            let declaring = match f {
                Func::User(u) if !scope.is_trait() => u.class_name.clone().unwrap_or_default(),
                _ => ZStr::default(),
            };
            s.push_str(&render_default(p, f, &declaring));
        }
    }
    s.push(')');
    if let Some(t) = f.return_type() {
        s.push_str(": ");
        s.push_str(&t.display(Some(&scope.name), parent_name.as_ref(), None));
    }
    s
}

/// A method that must be compatible with a prototype, with the scopes
/// its declaration is shown under (`common.scope`) and resolved in
/// (`fixup_trait_scope`).
pub(crate) struct Obligation {
    pub(crate) child: Rc<Method>,
    pub(crate) child_name: ZStr,
    pub(crate) child_scope: Rc<Class>,
    pub(crate) proto: Rc<Method>,
    pub(crate) proto_name: ZStr,
    pub(crate) proto_scope: Rc<Class>,
}

impl Obligation {
    /// Runs the implementation check; unresolved class names are added to
    /// `unresolved`.
    pub(crate) fn check(&self, vm: &Vm, linking: &Rc<Class>, unresolved: &mut Vec<ZStr>) -> Status {
        let mut c = Checker::new(vm, linking);
        let st = c.implementation_check(
            &self.child.func,
            self.child.flags,
            &self.child_scope,
            &self.proto.func,
            self.proto.flags,
            &self.proto_scope,
        );
        for n in c.unresolved {
            if !unresolved
                .iter()
                .any(|u| u.eq_ignore_ascii_case(n.as_bytes()))
            {
                unresolved.push(n);
            }
        }
        st
    }

    /// `emit_incompatible_method_error` for a failed check: the fatal error
    /// (message, file, line), or for `Status::Warning` the deprecation
    /// message (`None` when `#[\ReturnTypeWillChange]` suppresses it).
    pub(crate) fn message(
        &self,
        st: Status,
        unresolved: Option<&ZStr>,
    ) -> Option<(String, ZStr, u32)> {
        let child = declaration_as(
            &self.child.func,
            self.child.flags,
            &self.child_name,
            &self.child_scope,
        );
        let parent = declaration_as(
            &self.proto.func,
            self.proto.flags,
            &self.proto_name,
            &self.proto_scope,
        );
        let (file, line) = func_location(&self.child.func);
        let msg = match st {
            Status::Ok => return None,
            Status::Warning => {
                let rtwc = match &self.child.func {
                    Func::User(u) => u
                        .attributes
                        .iter()
                        .any(|a| a.name.eq_ignore_ascii_case(b"ReturnTypeWillChange")),
                    Func::Builtin(_) => true,
                };
                if rtwc {
                    return None;
                }
                format!(
                    "Return type of {child} should either be compatible with {parent}, or the #[\\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice"
                )
            }
            Status::Unresolved => format!(
                "Could not check compatibility between {child} and {parent}, because class {} is not available",
                unresolved.map(ZStr::to_string_lossy).unwrap_or_default()
            ),
            Status::Error => format!("Declaration of {child} must be compatible with {parent}"),
        };
        Some((msg, file, line))
    }
}

/// A backed property must satisfy its parent type in both directions.
pub(crate) struct PropertyObligation {
    child: Rc<crate::class::PropInfo>,
    child_scope: Rc<Class>,
    proto: Rc<crate::class::PropInfo>,
    proto_scope: Rc<Class>,
    file: ZStr,
    line: u32,
    invariant: bool,
    error: Option<String>,
}

impl PropertyObligation {
    pub(crate) fn check(&self, vm: &Vm, linking: &Rc<Class>, unresolved: &mut Vec<ZStr>) -> Status {
        let mut checker = Checker::new(vm, linking);
        let child_type = self.child.ty.clone().unwrap_or_else(TypeDecl::mixed);
        let proto_type = self.proto.ty.clone().unwrap_or_else(TypeDecl::mixed);
        let status = if self.invariant {
            match (&self.child.ty, &self.proto.ty) {
                (Some(child), Some(proto)) => {
                    let forward =
                        checker.covariant(&self.child_scope, child, &self.proto_scope, proto);
                    let reverse =
                        checker.covariant(&self.proto_scope, proto, &self.child_scope, child);
                    if forward == Status::Error || reverse == Status::Error {
                        Status::Error
                    } else if forward == Status::Unresolved || reverse == Status::Unresolved {
                        Status::Unresolved
                    } else {
                        Status::Ok
                    }
                }
                (None, None) => Status::Ok,
                _ => Status::Error,
            }
        } else {
            checker.covariant(
                &self.child_scope,
                &child_type,
                &self.proto_scope,
                &proto_type,
            )
        };
        for name in checker.unresolved {
            if !unresolved
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name.as_bytes()))
            {
                unresolved.push(name);
            }
        }
        status
    }

    pub(crate) fn message(&self) -> (String, ZStr, u32) {
        if let Some(error) = &self.error {
            return (error.clone(), self.file.clone(), self.line);
        }
        if self.proto.ty.is_none() {
            return (
                format!(
                    "Type of {}::${} must be omitted to match the parent definition in class {}",
                    display_name(&self.child_scope.name),
                    self.child.name,
                    display_name(&self.proto_scope.name)
                ),
                self.file.clone(),
                self.line,
            );
        }
        let need = self.proto.ty.as_ref().map_or_else(
            || "not be defined".to_string(),
            |ty| format!("be {}", ty.display(None, None, None)),
        );
        (
            format!(
                "Type of {}::${} must {need} (as in class {})",
                display_name(&self.child_scope.name),
                self.child.name,
                display_name(&self.proto_scope.name)
            ),
            self.file.clone(),
            self.line,
        )
    }
}

pub(crate) struct ConstantObligation {
    child: Rc<crate::class::ClassConst>,
    child_scope: Rc<Class>,
    proto: Rc<crate::class::ClassConst>,
    proto_scope: Rc<Class>,
}

impl ConstantObligation {
    pub(crate) fn check(&self, vm: &Vm, linking: &Rc<Class>, unresolved: &mut Vec<ZStr>) -> Status {
        let mut checker = Checker::new(vm, linking);
        let child_type = self.child.ty.clone().unwrap_or_else(TypeDecl::mixed);
        let proto_type = self.proto.ty.clone().unwrap_or_else(TypeDecl::mixed);
        let status = checker.covariant(
            &self.child_scope,
            &child_type,
            &self.proto_scope,
            &proto_type,
        );
        for name in checker.unresolved {
            if !unresolved
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name.as_bytes()))
            {
                unresolved.push(name);
            }
        }
        status
    }

    pub(crate) fn message(&self) -> (String, ZStr, u32) {
        (
            format!(
                "Type of {}::{} must be compatible with {}::{} of type {}",
                self.child_scope.name,
                self.child.name,
                self.proto_scope.name,
                self.proto.name,
                self.proto
                    .ty
                    .as_ref()
                    .map_or_else(|| "mixed".to_string(), TypeDecl::to_php_string)
            ),
            self.child_scope.filename.clone().unwrap_or_default(),
            self.child_scope.line_start,
        )
    }
}

fn check_constant_variance(vm: &Vm, cls: &Rc<Class>, out: &mut ClassCheck) {
    for base in cls.parent.iter().chain(cls.interfaces.iter()) {
        for proto in &base.consts {
            if proto.flags & flags::PRIVATE != 0 || proto.ty.is_none() {
                continue;
            }
            let Some(child) = cls.find_const(proto.name.as_bytes()) else {
                continue;
            };
            if Rc::ptr_eq(child, proto) {
                continue;
            }
            let ob = ConstantObligation {
                child: child.clone(),
                child_scope: vm
                    .find_class(child.class.as_bytes())
                    .unwrap_or_else(|| cls.clone()),
                proto: proto.clone(),
                proto_scope: vm
                    .find_class(proto.class.as_bytes())
                    .unwrap_or_else(|| base.clone()),
            };
            match ob.check(vm, cls, &mut out.unresolved) {
                Status::Error => {
                    out.error = Some(ob.message());
                    return;
                }
                Status::Unresolved => out.pending_consts.push(ob),
                _ => {}
            }
        }
    }
}

/// The outcome of the signature checks of a freshly linked class.
#[derive(Default)]
pub(crate) struct ClassCheck {
    /// The first fatal error (message, file, line).
    pub(crate) error: Option<(String, ZStr, u32)>,
    /// Deprecations (message, line).
    pub(crate) warnings: Vec<(String, u32)>,
    /// Checks that need classes which are not available yet
    /// (`add_compatibility_obligation`).
    pub(crate) pending: Vec<Obligation>,
    /// Backed-property invariance checks awaiting type dependencies.
    pub(crate) pending_props: Vec<PropertyObligation>,
    pub(crate) pending_consts: Vec<ConstantObligation>,
    /// The classes they need, in registration order.
    pub(crate) unresolved: Vec<ZStr>,
}

impl ClassCheck {
    /// Records the result of an obligation's first check
    /// (`perform_delayable_implementation_check`); returns whether it
    /// failed fatally.
    pub(crate) fn record(&mut self, vm: &Vm, linking: &Rc<Class>, ob: Obligation) -> bool {
        let st = ob.check(vm, linking, &mut self.unresolved);
        match st {
            Status::Ok => false,
            Status::Unresolved => {
                self.pending.push(ob);
                false
            }
            Status::Warning => {
                if let Some((m, _, l)) = ob.message(st, None) {
                    self.warnings.push((m, l));
                }
                false
            }
            Status::Error => {
                self.error = ob.message(st, None);
                true
            }
        }
    }
}

fn check_property_variance(vm: &Vm, cls: &Rc<Class>, out: &mut ClassCheck) {
    if let Some(parent) = &cls.parent {
        for child in &cls.props {
            if !child.class.eq_ignore_ascii_case(cls.name.as_bytes()) {
                continue;
            }
            let Some(proto) = parent.prop_map.get(&child.name) else {
                continue;
            };
            if proto.visibility() == flags::PRIVATE || proto.is_virtual() {
                continue;
            }
            let proto_scope = vm
                .find_class(proto.class.as_bytes())
                .unwrap_or_else(|| parent.clone());
            let ob = PropertyObligation {
                child: child.clone(),
                child_scope: cls.clone(),
                proto: proto.clone(),
                proto_scope,
                file: cls.filename.clone().unwrap_or_default(),
                line: cls.line_start,
                invariant: true,
                error: None,
            };
            match ob.check(vm, cls, &mut out.unresolved) {
                Status::Error => {
                    out.error = Some(ob.message());
                    return;
                }
                Status::Unresolved => out.pending_props.push(ob),
                _ => {}
            }
        }
    }
    for child in &cls.props {
        if !child.class.eq_ignore_ascii_case(cls.name.as_bytes()) {
            continue;
        }
        if let Some(setter) = child.hooks.as_ref().and_then(|h| h.set.as_ref()) {
            if let Some(param) = setter.params.first() {
                let mut accepted = (**child).clone();
                accepted.ty.clone_from(&param.ty);
                let setter_scope = setter
                    .class_name
                    .as_ref()
                    .and_then(|name| vm.find_class(name.as_bytes()))
                    .filter(|scope| !scope.is_trait())
                    .unwrap_or_else(|| cls.clone());
                let ob = PropertyObligation {
                    child: child.clone(), child_scope: cls.clone(), proto: Rc::new(accepted),
                    proto_scope: setter_scope, file: setter.code.filename.clone(), line: setter.line_start,
                    invariant: false,
                    error: Some(format!(
                        "Type of parameter ${} of hook {}::${}::set must be compatible with property type",
                        param.name, display_name(&cls.name), child.name
                    )),
                };
                match ob.check(vm, cls, &mut out.unresolved) {
                    Status::Error => {
                        out.error = Some(ob.message());
                        return;
                    }
                    Status::Unresolved => out.pending_props.push(ob),
                    _ => {}
                }
            }
        }
    }
}

fn check_hook_variance(vm: &Vm, cls: &Rc<Class>, out: &mut ClassCheck) {
    for source in cls.parent.iter().chain(cls.interfaces.iter()) {
        for proto in &source.props {
            if proto.visibility() == flags::PRIVATE {
                continue;
            }
            let Some(child) = cls.prop_map.get(&proto.name) else {
                continue;
            };
            if source.is_interface() && !child.is_virtual() {
                if let Some(proto_set) = proto.hooks.as_ref().and_then(|h| h.set.as_ref()) {
                    // `do_inherit_property` compares set visibility only:
                    // a `public(set) readonly` property passes here and
                    // leaves the set hook abstract instead.
                    if child.flags & (flags::SET_PROTECTED | flags::SET_PRIVATE) != 0 {
                        out.error = Some((
                            format!(
                                "Set access level of {}::${} must be omitted (as in class {})",
                                display_name(&cls.name),
                                child.name,
                                display_name(&source.name)
                            ),
                            cls.filename.clone().unwrap_or_default(),
                            cls.line_start,
                        ));
                        return;
                    }
                    if child.hooks.as_ref().is_none_or(|h| h.set.is_none()) {
                        if let Some(param) = proto_set.params.first() {
                            let mut accepted = (**proto).clone();
                            accepted.ty.clone_from(&param.ty);
                            let need = param.ty.as_ref().map_or_else(
                                || "mixed".to_string(),
                                |ty| ty.display(None, None, None),
                            );
                            let ob = PropertyObligation {
                                child: Rc::new(accepted), child_scope: source.clone(),
                                proto: child.clone(), proto_scope: cls.clone(),
                                file: cls.filename.clone().unwrap_or_default(), line: cls.line_start,
                                invariant: false,
                                error: Some(format!("Set type of {}::${} must be supertype of {need} (as in interface {})",
                                    display_name(&cls.name), child.name, display_name(&source.name))),
                            };
                            match ob.check(vm, cls, &mut out.unresolved) {
                                Status::Error => {
                                    out.error = Some(ob.message());
                                    return;
                                }
                                Status::Unresolved => out.pending_props.push(ob),
                                _ => {}
                            }
                        }
                    }
                }
            }
            let (Some(child_hooks), Some(proto_hooks)) = (&child.hooks, &proto.hooks) else {
                continue;
            };
            for (child_hook, proto_hook) in [
                (&child_hooks.get, &proto_hooks.get),
                (&child_hooks.set, &proto_hooks.set),
            ] {
                let (Some(child_hook), Some(proto_hook)) = (child_hook, proto_hook) else {
                    continue;
                };
                if Rc::ptr_eq(child_hook, proto_hook) {
                    continue;
                }
                let method = |function: &Rc<crate::func::UserFunction>, scope: &Rc<Class>| {
                    Rc::new(Method {
                        name: function.name.clone(),
                        func: Func::User(function.clone()),
                        flags: function.flags,
                        scope: Rc::downgrade(scope),
                        proto_scope: Rc::downgrade(scope),
                    })
                };
                let ob = Obligation {
                    child: method(child_hook, cls),
                    child_name: display_name(&cls.name),
                    child_scope: cls.clone(),
                    proto: method(proto_hook, source),
                    proto_name: display_name(&source.name),
                    proto_scope: source.clone(),
                };
                if out.record(vm, cls, ob) {
                    return;
                }
            }
        }
    }
}

/// Checks the overriding methods of a freshly linked class against their
/// parent and interface prototypes.
pub(crate) fn check_class(vm: &Vm, cls: &Rc<Class>, out: &mut ClassCheck) {
    check_constant_variance(vm, cls, out);
    if out.error.is_some() {
        return;
    }
    check_property_variance(vm, cls, out);
    if out.error.is_some() {
        return;
    }
    check_hook_variance(vm, cls, out);
    if out.error.is_some() {
        return;
    }
    let mut protos: Vec<(Rc<Method>, Rc<Method>)> = Vec::new();
    for lc in &cls.method_order {
        let m = &cls.methods[lc];
        if !Rc::ptr_eq(&m.scope(), cls) {
            continue;
        }
        if let Some(p) = &cls.parent {
            if let Some(pm) = p.methods.get(lc) {
                if !Rc::ptr_eq(pm, m) && (pm.visibility() != flags::PRIVATE || pm.is_abstract()) {
                    protos.push((m.clone(), pm.clone()));
                }
            }
        }
        for i in &cls.interfaces {
            // `zend_do_inherit_interfaces`: an interface the parent already
            // implements is not checked again (the parent's methods were).
            // Constructors are the exception: a concrete parent constructor
            // imposes no signature, so `do_inheritance_check_on_method`
            // checks the child against the parent method's prototype, the
            // interface's `__construct` (ctor_in_interface_04).
            if lc.as_bytes() != b"__construct"
                && cls
                    .parent
                    .as_ref()
                    .is_some_and(|p| p.interfaces.iter().any(|pi| Rc::ptr_eq(pi, i)))
            {
                continue;
            }
            if let Some(im) = i.methods.get(lc) {
                if !protos.iter().any(|(_, p)| Rc::ptr_eq(p, im)) {
                    protos.push((m.clone(), im.clone()));
                }
            }
        }
    }
    for (m, mut pm) in protos {
        let mut ps = pm.scope();
        let is_ctor = m.name.eq_ignore_ascii_case(b"__construct");
        if is_ctor && !ps.is_interface() && !pm.is_abstract() {
            // A constructor is only checked against an abstract (or
            // interface) prototype, which may be further up.
            let proto = pm
                .proto_scope
                .upgrade()
                .and_then(|c| c.methods.get(b"__construct".as_slice()).cloned())
                .filter(|p| p.is_abstract() || p.scope().is_interface());
            match proto {
                Some(p) => {
                    ps = p.scope();
                    pm = p;
                }
                None => continue,
            }
        }
        let ob = Obligation {
            child_name: display_name(&cls.name),
            child_scope: cls.clone(),
            child: m,
            proto_name: display_name(&ps.name),
            proto_scope: ps,
            proto: pm,
        };
        if out.record(vm, cls, ob) {
            break;
        }
    }
}
