//! Method signature compatibility (`zend_do_perform_implementation_check`,
//! `zend_perform_covariant_type_check`) and the "Declaration of ... must be
//! compatible with ..." diagnostics.

use crate::class::{cflags, Class, ConstExpr};
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
}

fn class_of_name(vm: &Vm, n: &ZStr, scope: &Rc<Class>) -> Option<Rc<Class>> {
    let lc = n.to_ascii_lowercase();
    match lc.as_bytes() {
        b"self" => Some(scope.clone()),
        b"parent" => scope.parent.clone(),
        _ => {
            if scope.lc_name.as_bytes() == lc.as_bytes() {
                return Some(scope.clone());
            }
            vm.find_class(n.as_bytes())
        }
    }
}

fn resolve2(vm: &Vm, n: &ZStr, s1: &Rc<Class>, s2: &Rc<Class>) -> Option<Rc<Class>> {
    class_of_name(vm, n, s1).or_else(|| {
        if s2.lc_name.as_bytes() == n.to_ascii_lowercase().as_bytes() {
            Some(s2.clone())
        } else {
            None
        }
    })
}

/// Whether class name `a` (in `a_scope`) is a subtype of class name `b`.
/// Unresolvable classes are treated as compatible (PHP delays the check).
fn class_subtype(vm: &Vm, a: &ZStr, a_scope: &Rc<Class>, b: &ZStr, b_scope: &Rc<Class>) -> bool {
    if a.eq_ignore_ascii_case(b.as_bytes()) {
        return true;
    }
    let ca = resolve2(vm, a, a_scope, b_scope);
    let cb = resolve2(vm, b, b_scope, a_scope);
    match (ca, cb) {
        (Some(x), Some(y)) => x.is_subclass_of(&y),
        _ => true,
    }
}

/// `zend_perform_covariant_type_check`: is `fe` a subtype of `proto`?
pub(crate) fn covariant(
    vm: &Vm,
    fe_scope: &Rc<Class>,
    fe: &TypeDecl,
    proto_scope: &Rc<Class>,
    proto: &TypeDecl,
) -> bool {
    if proto.is_mixed() && fe.mask & mask::VOID == 0 {
        return true;
    }
    if fe.mask & mask::NEVER != 0 && !fe.has_classes() && fe.mask == mask::NEVER {
        return true;
    }
    let mut fe_mask = fe.mask;
    let pmask = proto.mask;
    // static is a subtype of self/object/static.
    if fe_mask & mask::STATIC != 0 {
        let ok = pmask & (mask::STATIC | mask::OBJECT) != 0
            || proto.classes.iter().any(|c| {
                resolve2(vm, c, proto_scope, fe_scope).is_none_or(|pc| fe_scope.is_subclass_of(&pc))
            });
        if !ok {
            return false;
        }
        fe_mask &= !mask::STATIC;
    }
    // iterable = Traversable|array.
    let p_iter = pmask & mask::ITERABLE != 0;
    if fe_mask & mask::ITERABLE != 0 {
        let ok = p_iter
            || (pmask & mask::ARRAY != 0
                && (pmask & mask::OBJECT != 0
                    || proto
                        .classes
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(b"Traversable"))));
        if !ok {
            return false;
        }
        fe_mask &= !mask::ITERABLE;
    }
    let mut extra_ok = 0;
    if p_iter {
        extra_ok |= mask::ARRAY;
    }
    if fe_mask & !(pmask | extra_ok) & !mask::NULL != 0
        && !(pmask & mask::BOOL == mask::BOOL
            && fe_mask & !(pmask | extra_ok) & !mask::NULL & !mask::BOOL == 0)
    {
        return false;
    }
    if fe_mask & mask::NULL != 0 && pmask & mask::NULL == 0 {
        return false;
    }
    for c in &fe.classes {
        let fc = resolve2(vm, c, fe_scope, proto_scope);
        let ok = pmask & mask::OBJECT != 0
            || (pmask & mask::CALLABLE != 0
                && fc.as_ref().is_some_and(|x| {
                    x.lc_name.as_bytes() == b"closure"
                        || x.methods.contains_key(b"__invoke".as_slice())
                }))
            || (pmask & mask::STATIC != 0
                && fe_scope.flags & cflags::FINAL != 0
                && fc.as_ref().is_some_and(|x| Rc::ptr_eq(x, fe_scope)))
            || (p_iter
                && class_of_name(vm, c, fe_scope)
                    .is_some_and(|x| x.instance_of_name(b"traversable")))
            || proto
                .classes
                .iter()
                .any(|pc| class_subtype(vm, c, fe_scope, pc, proto_scope))
            || proto.intersections.iter().any(|g| {
                g.iter()
                    .all(|pc| class_subtype(vm, c, fe_scope, pc, proto_scope))
            })
            || (pmask & mask::STATIC != 0 && false);
        if !ok {
            return false;
        }
    }
    for g in &fe.intersections {
        let ok = pmask & mask::OBJECT != 0
            || proto.classes.iter().any(|pc| {
                g.iter()
                    .any(|c| class_subtype(vm, c, fe_scope, pc, proto_scope))
            })
            || proto.intersections.iter().any(|pg| {
                pg.iter().all(|pc| {
                    g.iter()
                        .any(|c| class_subtype(vm, c, fe_scope, pc, proto_scope))
                })
            });
        if !ok {
            return false;
        }
    }
    true
}

/// `zend_do_perform_implementation_check`.
pub(crate) fn implementation_check(
    vm: &Vm,
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
    if proto_flags & flags::RETURN_REF != 0 && fe_flags & flags::RETURN_REF == 0 {
        return Status::Error;
    }
    let pv = proto.is_variadic();
    let fv = fe.is_variadic();
    if pv && !fv {
        return Status::Error;
    }
    let pp = proto.params();
    let fp = fe.params();
    let n = pp.len().max(fp.len());
    for i in 0..n {
        let pa = pp.get(i).or_else(|| if pv { pp.last() } else { None });
        let fa = fp.get(i).or_else(|| if fv { fp.last() } else { None });
        let Some(pa) = pa else { continue };
        let Some(fa) = fa else { return Status::Error };
        // Contravariant parameter types.
        if let Some(ft) = &fa.ty {
            if !ft.is_mixed() {
                match &pa.ty {
                    None => return Status::Error,
                    Some(pt) => {
                        if !covariant(vm, proto_scope, pt, fe_scope, ft) {
                            return Status::Error;
                        }
                    }
                }
            }
        }
        if fa.by_ref != pa.by_ref {
            return Status::Error;
        }
    }
    if let Some(prt) = proto.return_type() {
        match fe.return_type() {
            None => {
                if !proto.is_user() {
                    return Status::Warning;
                }
                return Status::Error;
            }
            Some(frt) => {
                if !covariant(vm, fe_scope, frt, proto_scope, prt) {
                    if !proto.is_user() {
                        return Status::Warning;
                    }
                    return Status::Error;
                }
            }
        }
    }
    Status::Ok
}

fn render_default(p: &ParamInfo, f: &Func) -> String {
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
            Value::Float(f) => php_runtime::float::format_double(*f, 17, false),
            _ => "<expression>".into(),
        },
        _ => {
            let t = p.default_text.clone().unwrap_or_default();
            let t = t.trim();
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
pub(crate) fn declaration(f: &Func, fflags: u32, scope: &Rc<Class>) -> String {
    let mut s = String::new();
    if fflags & flags::RETURN_REF != 0 {
        s.push('&');
    }
    s.push_str(&display_name(&scope.name).to_string_lossy());
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
            s.push_str(&render_default(p, f));
        }
    }
    s.push(')');
    if let Some(t) = f.return_type() {
        s.push_str(": ");
        s.push_str(&t.display(Some(&scope.name), parent_name.as_ref(), None));
    }
    s
}

/// Checks the overriding methods of a freshly linked class; returns the
/// fatal error (message, line) or deprecations to emit.
pub(crate) fn check_class(vm: &Vm, cls: &Rc<Class>) -> (Option<(String, u32)>, Vec<(String, u32)>) {
    let mut warnings = Vec::new();
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
            if let Some(im) = i.methods.get(lc) {
                if !protos.iter().any(|(_, p)| Rc::ptr_eq(p, im)) {
                    protos.push((m.clone(), im.clone()));
                }
            }
        }
    }
    for (m, pm) in protos {
        let ps = pm.scope();
        let is_ctor = m.name.eq_ignore_ascii_case(b"__construct");
        if is_ctor && !ps.is_interface() && !pm.is_abstract() {
            continue;
        }
        let st = implementation_check(vm, &m.func, m.flags, &m.scope(), &pm.func, pm.flags, &ps);
        if st == Status::Ok {
            continue;
        }
        let line = match &m.func {
            Func::User(u) => u.line_start,
            Func::Builtin(_) => 0,
        };
        let child = declaration(&m.func, m.flags, &m.scope());
        let parent = declaration(&pm.func, pm.flags, &ps);
        if st == Status::Warning {
            let rtwc = match &m.func {
                Func::User(u) => u
                    .attributes
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(b"ReturnTypeWillChange")),
                Func::Builtin(_) => true,
            };
            if !rtwc {
                warnings.push((
                    format!(
                        "Return type of {child} should either be compatible with {parent}, or the #[\\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice"
                    ),
                    line,
                ));
            }
            continue;
        }
        let _ = cflags::INTERNAL;
        return (
            Some((
                format!("Declaration of {child} must be compatible with {parent}"),
                line,
            )),
            warnings,
        );
    }
    (None, warnings)
}
