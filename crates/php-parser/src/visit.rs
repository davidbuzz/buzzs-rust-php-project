//! A read-only AST visitor.
//!
//! Implement [`Visitor`] and override the methods for the nodes of
//! interest; call the matching `walk_*` function to continue into children.

use crate::ast::{
    Arg, ArgKind, ArgList, ArrayItem, AttributeGroup, Callee, ClassDecl, ClassMember,
    ClassMemberKind, ClassRef, Expr, ExprKind, HookBody, InterpPart, MemberName, NewClass, Param,
    PropertyHook, Stmt, StmtKind, TraitAdaptation, VarName,
};

/// Visits AST nodes. Every method defaults to walking the children.
pub trait Visitor {
    /// Called for every statement.
    fn visit_stmt(&mut self, s: &Stmt) {
        walk_stmt(self, s);
    }
    /// Called for every expression.
    fn visit_expr(&mut self, e: &Expr) {
        walk_expr(self, e);
    }
    /// Called for every class-like declaration (named or anonymous).
    fn visit_class(&mut self, c: &ClassDecl) {
        walk_class(self, c);
    }
    /// Called for every parameter.
    fn visit_param(&mut self, p: &Param) {
        walk_param(self, p);
    }
}

/// Visits a list of statements.
pub fn walk_stmts<V: Visitor + ?Sized>(v: &mut V, stmts: &[Stmt]) {
    for s in stmts {
        v.visit_stmt(s);
    }
}

fn walk_attrs<V: Visitor + ?Sized>(v: &mut V, attrs: &[AttributeGroup]) {
    for g in attrs {
        for a in &g.attributes {
            if let Some(args) = &a.args {
                walk_args(v, args);
            }
        }
    }
}

/// Visits the arguments of a call.
pub fn walk_args<V: Visitor + ?Sized>(v: &mut V, args: &ArgList) {
    for a in &args.args {
        walk_arg(v, a);
    }
}

fn walk_arg<V: Visitor + ?Sized>(v: &mut V, a: &Arg) {
    match &a.kind {
        ArgKind::Positional(e) | ArgKind::Unpack(e) | ArgKind::Named { value: e, .. } => {
            v.visit_expr(e);
        }
        ArgKind::Placeholder | ArgKind::VariadicPlaceholder | ArgKind::NamedPlaceholder(_) => {}
    }
}

fn walk_class_ref<V: Visitor + ?Sized>(v: &mut V, c: &ClassRef) {
    if let ClassRef::Expr(e) = c {
        v.visit_expr(e);
    }
}

fn walk_member_name<V: Visitor + ?Sized>(v: &mut V, m: &MemberName) {
    if let MemberName::Expr(e) = m {
        v.visit_expr(e);
    }
}

fn walk_var_name<V: Visitor + ?Sized>(v: &mut V, n: &VarName) {
    if let VarName::Expr(e) = n {
        v.visit_expr(e);
    }
}

fn walk_item<V: Visitor + ?Sized>(v: &mut V, it: &ArrayItem) {
    if let Some(k) = &it.key {
        v.visit_expr(k);
    }
    v.visit_expr(&it.value);
}

fn walk_hooks<V: Visitor + ?Sized>(v: &mut V, hooks: &[PropertyHook]) {
    for h in hooks {
        walk_attrs(v, &h.attributes);
        if let Some(ps) = &h.params {
            for p in ps {
                v.visit_param(p);
            }
        }
        match &h.body {
            HookBody::None => {}
            HookBody::Block(b) => walk_stmts(v, b),
            HookBody::Expr(e) => v.visit_expr(e),
        }
    }
}

/// Visits the children of a parameter.
pub fn walk_param<V: Visitor + ?Sized>(v: &mut V, p: &Param) {
    walk_attrs(v, &p.attributes);
    if let Some(d) = &p.default {
        v.visit_expr(d);
    }
    if let Some(h) = &p.hooks {
        walk_hooks(v, h);
    }
}

/// Visits the members of a class.
pub fn walk_class<V: Visitor + ?Sized>(v: &mut V, c: &ClassDecl) {
    walk_attrs(v, &c.attributes);
    for m in &c.members {
        walk_member(v, m);
    }
}

fn walk_member<V: Visitor + ?Sized>(v: &mut V, m: &ClassMember) {
    match &m.kind {
        ClassMemberKind::Property(g) => {
            walk_attrs(v, &g.attributes);
            for p in &g.props {
                if let Some(d) = &p.default {
                    v.visit_expr(d);
                }
                if let Some(h) = &p.hooks {
                    walk_hooks(v, h);
                }
            }
        }
        ClassMemberKind::Const(g) => {
            walk_attrs(v, &g.attributes);
            for c in &g.consts {
                v.visit_expr(&c.value);
            }
        }
        ClassMemberKind::Method(md) => {
            walk_attrs(v, &md.attributes);
            for p in &md.params {
                v.visit_param(p);
            }
            if let Some(b) = &md.body {
                walk_stmts(v, b);
            }
        }
        ClassMemberKind::TraitUse(tu) => {
            for a in &tu.adaptations {
                match a {
                    TraitAdaptation::Precedence { .. } | TraitAdaptation::Alias { .. } => {}
                }
            }
        }
        ClassMemberKind::EnumCase(c) => {
            walk_attrs(v, &c.attributes);
            if let Some(e) = &c.value {
                v.visit_expr(e);
            }
        }
    }
}

/// Visits the children of a statement.
#[allow(clippy::too_many_lines)]
pub fn walk_stmt<V: Visitor + ?Sized>(v: &mut V, s: &Stmt) {
    match &s.kind {
        StmtKind::Block(b) => walk_stmts(v, b),
        StmtKind::If {
            branches,
            else_branch,
        } => {
            for b in branches {
                v.visit_expr(&b.cond);
                v.visit_stmt(&b.body);
            }
            if let Some(e) = else_branch {
                v.visit_stmt(e);
            }
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            v.visit_expr(cond);
            v.visit_stmt(body);
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            for e in init.iter().chain(cond).chain(step) {
                v.visit_expr(e);
            }
            v.visit_stmt(body);
        }
        StmtKind::Foreach {
            subject,
            key,
            value,
            body,
            ..
        } => {
            v.visit_expr(subject);
            if let Some(k) = key {
                v.visit_expr(k);
            }
            v.visit_expr(value);
            v.visit_stmt(body);
        }
        StmtKind::Switch { subject, cases } => {
            v.visit_expr(subject);
            for c in cases {
                if let Some(e) = &c.cond {
                    v.visit_expr(e);
                }
                walk_stmts(v, &c.body);
            }
        }
        StmtKind::Break(e) | StmtKind::Continue(e) | StmtKind::Return(e) => {
            if let Some(e) = e {
                v.visit_expr(e);
            }
        }
        StmtKind::Global(es) | StmtKind::Echo(es) | StmtKind::Unset(es) => {
            for e in es {
                v.visit_expr(e);
            }
        }
        StmtKind::Static(vars) => {
            for sv in vars {
                if let Some(d) = &sv.default {
                    v.visit_expr(d);
                }
            }
        }
        StmtKind::Expr(e) => v.visit_expr(e),
        StmtKind::Declare { directives, body } => {
            for d in directives {
                v.visit_expr(&d.value);
            }
            if let Some(b) = body {
                v.visit_stmt(b);
            }
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            walk_stmts(v, body);
            for c in catches {
                walk_stmts(v, &c.body);
            }
            if let Some(f) = finally {
                walk_stmts(v, f);
            }
        }
        StmtKind::Function(f) => {
            walk_attrs(v, &f.attributes);
            for p in &f.params {
                v.visit_param(p);
            }
            walk_stmts(v, &f.body);
        }
        StmtKind::Class(c) => v.visit_class(c),
        StmtKind::Const { consts, attributes } => {
            walk_attrs(v, attributes);
            for c in consts {
                v.visit_expr(&c.value);
            }
        }
        StmtKind::Namespace { body, .. } => {
            if let Some(b) = body {
                walk_stmts(v, b);
            }
        }
        StmtKind::InlineHtml(_)
        | StmtKind::Nop
        | StmtKind::Goto(_)
        | StmtKind::Label(_)
        | StmtKind::HaltCompiler { .. }
        | StmtKind::Use { .. }
        | StmtKind::GroupUse { .. } => {}
    }
}

/// Visits the children of an expression.
#[allow(clippy::too_many_lines)]
pub fn walk_expr<V: Visitor + ?Sized>(v: &mut V, e: &Expr) {
    match &e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::String(_)
        | ExprKind::Const(_)
        | ExprKind::MagicConst(_) => {}
        ExprKind::Interpolated(parts) | ExprKind::ShellExec(parts) => {
            for p in parts {
                if let InterpPart::Expr { expr, .. } = p {
                    v.visit_expr(expr);
                }
            }
        }
        ExprKind::Array { items, .. } => {
            for it in items.iter().flatten() {
                walk_item(v, it);
            }
        }
        ExprKind::ClassConst { class, name } => {
            walk_class_ref(v, class);
            walk_member_name(v, name);
        }
        ExprKind::ClassName(class) => walk_class_ref(v, class),
        ExprKind::Var(n) => walk_var_name(v, n),
        ExprKind::Dim { base, index } => {
            v.visit_expr(base);
            if let Some(i) = index {
                v.visit_expr(i);
            }
        }
        ExprKind::Prop { object, name, .. } => {
            v.visit_expr(object);
            walk_member_name(v, name);
        }
        ExprKind::StaticProp { class, name, .. } => {
            walk_class_ref(v, class);
            walk_var_name(v, name);
        }
        ExprKind::Call { callee, args } => {
            if let Callee::Expr(c) = callee.as_ref() {
                v.visit_expr(c);
            }
            walk_args(v, args);
        }
        ExprKind::MethodCall {
            object, name, args, ..
        } => {
            v.visit_expr(object);
            walk_member_name(v, name);
            walk_args(v, args);
        }
        ExprKind::StaticCall { class, name, args } => {
            walk_class_ref(v, class);
            walk_member_name(v, name);
            walk_args(v, args);
        }
        ExprKind::New { class, args } => {
            match class.as_ref() {
                NewClass::Name(_) => {}
                NewClass::Expr(x) => v.visit_expr(x),
                NewClass::Anonymous(c) => {
                    walk_args(v, args);
                    v.visit_class(c);
                    return;
                }
            }
            walk_args(v, args);
        }
        ExprKind::Assign { target, value }
        | ExprKind::AssignRef { target, value }
        | ExprKind::AssignOp { target, value, .. } => {
            v.visit_expr(target);
            v.visit_expr(value);
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
            v.visit_expr(left);
            v.visit_expr(right);
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Include { expr, .. }
        | ExprKind::VoidCast(expr)
        | ExprKind::Empty(expr)
        | ExprKind::Silence(expr)
        | ExprKind::Print(expr)
        | ExprKind::YieldFrom(expr)
        | ExprKind::Throw(expr) => v.visit_expr(expr),
        ExprKind::IncDec { var, .. } => v.visit_expr(var),
        ExprKind::Isset(vars) => {
            for x in vars {
                v.visit_expr(x);
            }
        }
        ExprKind::Instanceof { expr, class } => {
            v.visit_expr(expr);
            walk_class_ref(v, class);
        }
        ExprKind::Conditional {
            cond, then, else_, ..
        } => {
            v.visit_expr(cond);
            if let Some(t) = then {
                v.visit_expr(t);
            }
            v.visit_expr(else_);
        }
        ExprKind::Yield { key, value } => {
            if let Some(k) = key {
                v.visit_expr(k);
            }
            if let Some(x) = value {
                v.visit_expr(x);
            }
        }
        ExprKind::Closure(c) => {
            walk_attrs(v, &c.attributes);
            for p in &c.params {
                v.visit_param(p);
            }
            walk_stmts(v, &c.body);
        }
        ExprKind::ArrowFn(f) => {
            walk_attrs(v, &f.attributes);
            for p in &f.params {
                v.visit_param(p);
            }
            v.visit_expr(&f.body);
        }
        ExprKind::Match { subject, arms } => {
            v.visit_expr(subject);
            for a in arms {
                if let Some(c) = &a.conds {
                    for x in c {
                        v.visit_expr(x);
                    }
                }
                v.visit_expr(&a.body);
            }
        }
    }
}
