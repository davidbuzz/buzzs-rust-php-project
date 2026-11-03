//! Function, closure and class declarations; parameters and types.

use super::{CResult, ClassCtx, Compiler, FnCtx, FnKind};
use crate::ast::{self, ClassMemberKind, Expr, ExprKind, Stmt, StmtKind, TypeKind};
use crate::class::{
    cflags, AttributeInfo, ClassDecl, ClassKind, ConstDecl, ConstExpr, MethodDecl, PropDecl,
    PropHooks, TraitRule,
};
use crate::func::{flags, ClosureUse, UserFunction};
use crate::opcode::{Op, Operand, ParamInfo, Place, PlaceBase, Seg};
use crate::types::{mask, TypeDecl};
use php_runtime::{Value, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

/// Source text of an expression (used for `assert()` messages).
pub(crate) fn export_expr_text(source: &[u8], e: &Expr) -> String {
    let s = e.span.start.min(source.len());
    let end = e.span.end.min(source.len()).max(s);
    String::from_utf8_lossy(&source[s..end]).into_owned()
}

fn modifiers_to_flags(m: ast::Modifiers) -> u32 {
    let mut f = 0;
    if m.contains(ast::Modifiers::PUBLIC) || m.contains(ast::Modifiers::VAR) {
        f |= flags::PUBLIC;
    }
    if m.contains(ast::Modifiers::PROTECTED) {
        f |= flags::PROTECTED;
    }
    if m.contains(ast::Modifiers::PRIVATE) {
        f |= flags::PRIVATE;
    }
    if m.contains(ast::Modifiers::STATIC) {
        f |= flags::STATIC;
    }
    if m.contains(ast::Modifiers::FINAL) {
        f |= flags::FINAL;
    }
    if m.contains(ast::Modifiers::ABSTRACT) {
        f |= flags::ABSTRACT;
    }
    if m.contains(ast::Modifiers::READONLY) {
        f |= flags::READONLY;
    }
    if m.contains(ast::Modifiers::PUBLIC_SET) {
        f |= flags::SET_PUBLIC;
    }
    if m.contains(ast::Modifiers::PROTECTED_SET) {
        f |= flags::SET_PROTECTED;
    }
    if m.contains(ast::Modifiers::PRIVATE_SET) {
        f |= flags::SET_PRIVATE;
    }
    if f & flags::VISIBILITY == 0 {
        f |= flags::PUBLIC;
    }
    f
}

struct BodySpec<'a> {
    kind: FnKind,
    name: ZStr,
    method_name: ZStr,
    params: &'a [ast::Param],
    return_type: Option<&'a ast::Type>,
    by_ref: bool,
    is_generator: bool,
    is_static: bool,
    line_start: u32,
    line_end: u32,
    doc_comment: Option<&'a Vec<u8>>,
    attributes: &'a ast::Attributes,
    is_ctor: bool,
    uses: Vec<(ZStr, bool, u32, bool)>,
    has_body: bool,
}

impl Compiler {
    pub(crate) fn compile_function_decl(
        &mut self,
        fd: &ast::FunctionDecl,
        top_level: bool,
    ) -> CResult<()> {
        let name = self.ns_prefixed_pub(&fd.name.name);
        let lc = name.to_ascii_lowercase();
        if top_level {
            if let Some(&line) = self.declared_functions.get(&lc) {
                return self.err(
                    format!(
                        "Cannot redeclare function {}() (previously declared in {}:{})",
                        String::from_utf8_lossy(&name),
                        self.filename,
                        line
                    ),
                    fd.span.line,
                );
            }
            self.declared_functions.insert(lc, fd.span.line);
        }
        let spec = BodySpec {
            kind: FnKind::Function,
            name: ZStr::from(name),
            method_name: ZStr::empty(),
            params: &fd.params,
            return_type: fd.return_type.as_ref(),
            by_ref: fd.by_ref,
            is_generator: fd.is_generator,
            is_static: false,
            line_start: fd.span.line,
            line_end: fd.end_line,
            doc_comment: fd.doc_comment.as_ref(),
            attributes: &fd.attributes,
            is_ctor: false,
            uses: Vec::new(),
            has_body: true,
        };
        let uf = self.compile_body(&spec, BodyKind::Stmts(&fd.body))?;
        let idx = self.add_function(uf);
        if top_level {
            self.hoisted_functions.push(idx);
        } else {
            self.f().line = fd.span.line;
            self.emit(Op::DeclareFunction(idx));
        }
        Ok(())
    }

    fn closure_name(&self, line: u32) -> ZStr {
        let f = self.fref();
        let s = match f.kind {
            FnKind::Closure => format!("{{closure:{}:{}}}", f.func_name, line),
            FnKind::Function => format!("{{closure:{}():{}}}", f.func_name, line),
            FnKind::Method => match self.current_class() {
                Some(c) => format!("{{closure:{}::{}():{}}}", c.name, f.method_name, line),
                None => format!("{{closure:{}():{}}}", f.method_name, line),
            },
            FnKind::Main | FnKind::ConstExpr => format!("{{closure:{}:{}}}", self.filename, line),
        };
        ZStr::from(s)
    }

    pub(crate) fn compile_closure(&mut self, c: &ast::Closure) -> CResult<Operand> {
        let mut uses = Vec::new();
        for u in &c.uses {
            if u.name.name == b"this" {
                return self.err("Cannot use $this as lexical variable", u.span.line);
            }
            if is_superglobal_or_globals(&u.name.name) {
                return self.err(
                    format!("Cannot use auto-global as lexical variable"),
                    u.span.line,
                );
            }
            if uses.iter().any(|(n, _, _, _): &(ZStr, bool, u32, bool)| {
                n.as_bytes() == u.name.name.as_slice()
            }) {
                return self.err(
                    format!("Cannot use variable ${} twice", u.name.as_str_lossy()),
                    u.span.line,
                );
            }
            if c.params.iter().any(|p| p.name.name == u.name.name) {
                return self.err(
                    format!(
                        "Cannot use lexical variable ${} as a parameter name",
                        u.name.as_str_lossy()
                    ),
                    u.span.line,
                );
            }
            let pcv = self.cv(&u.name.name);
            uses.push((ZStr::from(u.name.name.as_slice()), u.by_ref, pcv, false));
        }
        let name = self.closure_name(c.span.line);
        let spec = BodySpec {
            kind: FnKind::Closure,
            name,
            method_name: ZStr::empty(),
            params: &c.params,
            return_type: c.return_type.as_ref(),
            by_ref: c.by_ref,
            is_generator: c.is_generator,
            is_static: c.is_static,
            line_start: c.span.line,
            line_end: c.end_line,
            doc_comment: c.doc_comment.as_ref(),
            attributes: &c.attributes,
            is_ctor: false,
            uses,
            has_body: true,
        };
        self.closure_depth += 1;
        let r = self.compile_body(&spec, BodyKind::Stmts(&c.body));
        self.closure_depth -= 1;
        let uf = r?;
        let idx = self.add_function(uf);
        let t = self.tmp();
        self.f().line = c.span.line;
        self.emit(Op::MakeClosure { idx, dst: t });
        Ok(Operand::Tmp(t))
    }

    pub(crate) fn compile_arrow_fn(&mut self, a: &ast::ArrowFn) -> CResult<Operand> {
        let name = self.closure_name(a.span.line);
        let spec = BodySpec {
            kind: FnKind::Closure,
            name,
            method_name: ZStr::empty(),
            params: &a.params,
            return_type: a.return_type.as_ref(),
            by_ref: a.by_ref,
            is_generator: a.is_generator,
            is_static: a.is_static,
            line_start: a.span.line,
            line_end: a.end_line,
            doc_comment: a.doc_comment.as_ref(),
            attributes: &a.attributes,
            is_ctor: false,
            uses: Vec::new(),
            has_body: true,
        };
        self.closure_depth += 1;
        let r = self.compile_body(&spec, BodyKind::Arrow(&a.body));
        self.closure_depth -= 1;
        let mut uf = r?;
        // Implicit by-value capture of every non-parameter variable.
        let nparams = a.params.len();
        let names: Vec<ZStr> = uf.code.cv_names.clone();
        let mut uses = Vec::new();
        for (i, n) in names.iter().enumerate().skip(nparams) {
            let pcv = self.cv(n.as_bytes());
            uses.push(ClosureUse {
                name: n.clone(),
                by_ref: false,
                parent_cv: pcv,
                cv: u32::try_from(i).unwrap_or(0),
                implicit: true,
            });
        }
        uf.uses = uses;
        uf.flags |= flags::ARROW;
        let idx = self.add_function(uf);
        let t = self.tmp();
        self.f().line = a.span.line;
        self.emit(Op::MakeClosure { idx, dst: t });
        Ok(Operand::Tmp(t))
    }

    #[allow(clippy::too_many_lines)]
    fn compile_body(&mut self, spec: &BodySpec<'_>, body: BodyKind<'_>) -> CResult<UserFunction> {
        // Attributes first (`zend_compile_func_decl`).
        let attributes = self.attributes(spec.attributes)?;
        let tgt = if spec.kind == FnKind::Method {
            super::attrs::target::METHOD
        } else {
            super::attrs::target::FUNCTION
        };
        let in_hook = self.hook_prop.is_some();
        self.validate_attributes(&attributes, tgt, 0, None, in_hook)?;
        let has = |n: &[u8]| attributes.iter().any(|a| a.name.eq_ignore_ascii_case(n));
        let delayed = has(b"DelayedTargetValidation");
        if has(b"NoDiscard") && !in_hook {
            let what = if self.current_class().is_some() {
                "method"
            } else {
                "function"
            };
            if let Some(rt) = spec.return_type {
                let line = spec.line_start;
                if matches!(&rt.kind, ast::TypeKind::Named(n) if n.name.eq_ignore_ascii_case(b"void"))
                {
                    return self.err(
                        format!("A void {what} does not return a value, but #[\\NoDiscard] requires a return value"),
                        line,
                    );
                }
                if matches!(&rt.kind, ast::TypeKind::Named(n) if n.name.eq_ignore_ascii_case(b"never"))
                {
                    return self.err(
                        format!("A never returning {what} does not return a value, but #[\\NoDiscard] requires a return value"),
                        line,
                    );
                }
            }
        }
        let _ = delayed;
        let mut f = FnCtx::new(spec.kind);
        f.line = spec.line_start;
        f.func_name = spec.name.clone();
        f.method_name = spec.method_name.clone();
        f.is_generator = spec.is_generator;
        f.returns_ref = spec.by_ref;
        f.is_static = spec.is_static;
        self.fns.push(f);
        let r = self.compile_body_inner(spec, body);
        let f = self.fns.pop().expect("fn ctx");
        let (params, required, return_type, ctx_uses) = r?;
        let code = self.finish_fn(f)?;
        let mut fl = 0;
        if spec.kind == FnKind::Closure {
            fl |= flags::CLOSURE;
        }
        if spec.is_generator {
            fl |= flags::GENERATOR;
        }
        if spec.by_ref {
            fl |= flags::RETURN_REF;
        }
        if spec.is_static {
            fl |= flags::STATIC;
        }
        if params.last().is_some_and(|p: &ParamInfo| p.variadic) {
            fl |= flags::VARIADIC;
        }
        if !spec.has_body {
            fl |= flags::NO_BODY;
        }
        if attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
        {
            fl |= flags::DEPRECATED;
        }
        if attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(b"NoDiscard"))
            && self.hook_prop.is_none()
        {
            fl |= flags::NODISCARD;
        }
        Ok(UserFunction {
            name: spec.name.clone(),
            params,
            required,
            return_type,
            flags: fl,
            code: Rc::new(code),
            line_start: spec.line_start,
            line_end: spec.line_end,
            doc_comment: spec.doc_comment.map(|d| ZStr::from(d.as_slice())),
            statics: RefCell::new(Vec::new()),
            uses: ctx_uses,
            class_name: self.current_class().map(|c| c.name.clone()),
            attributes,
            hook_prop: None,
        })
    }

    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    fn compile_body_inner(
        &mut self,
        spec: &BodySpec<'_>,
        body: BodyKind<'_>,
    ) -> CResult<(Vec<ParamInfo>, u32, Option<TypeDecl>, Vec<ClosureUse>)> {
        // Parameters occupy the first CVs.
        let mut params = Vec::new();
        for p in spec.params {
            if p.name.name == b"this" {
                return self.err("Cannot use $this as parameter", p.span.line);
            }
            if params
                .iter()
                .any(|q: &ParamInfo| q.name.as_bytes() == p.name.name.as_slice())
            {
                return self.err(
                    format!("Redefinition of parameter ${}", p.name.as_str_lossy()),
                    p.span.line,
                );
            }
            if is_superglobal_or_globals(&p.name.name) && p.name.name != b"GLOBALS" {
                return self.err(
                    format!(
                        "Cannot re-assign auto-global variable ${}",
                        p.name.as_str_lossy()
                    ),
                    p.span.line,
                );
            }
            self.cv(&p.name.name);
            let mut ty = match &p.ty {
                Some(t) => Some(self.resolve_type(t)?),
                None => None,
            };
            if let (Some(t), Some(d)) = (&mut ty, &p.default) {
                if is_null_literal(d) && !t.allows_null() && !t.is_mixed() {
                    t.mask |= mask::NULL;
                    self.warnings.push((
                        8192,
                        format!(
                            "{}(): Implicitly marking parameter ${} as nullable is deprecated, the explicit nullable type must be used instead",
                            self.fn_display_name(),
                            p.name.as_str_lossy()
                        ),
                        p.span.line,
                    ));
                }
            }
            if p.variadic && p.default.is_some() {
                return self.err(
                    "Variadic parameter cannot have a default value",
                    p.span.line,
                );
            }
            let promote = if p.modifiers.is_empty() {
                0
            } else {
                if !spec.is_ctor {
                    return self.err(
                        "Cannot declare promoted property outside a constructor",
                        p.span.line,
                    );
                }
                if p.variadic {
                    return self.err("Cannot declare variadic promoted property", p.span.line);
                }
                modifiers_to_flags(p.modifiers)
            };
            let attrs = self.attributes(&p.attributes)?;
            let promoted_tgt = if promote == 0 {
                0
            } else {
                super::attrs::target::PROPERTY
            };
            self.validate_attributes(
                &attrs,
                super::attrs::target::PARAMETER,
                promoted_tgt,
                None,
                false,
            )?;
            params.push(ParamInfo {
                name: ZStr::from(p.name.name.as_slice()),
                by_ref: p.by_ref,
                variadic: p.variadic,
                ty,
                has_default: p.default.is_some(),
                default_text: p
                    .default
                    .as_ref()
                    .map(|d| export_expr_text(&self.source, d)),
                promote,
                sensitive: attrs
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(b"SensitiveParameter")),
                default_expr: match &p.default {
                    Some(d) => {
                        self.check_const_expr(d, true)?;
                        Some(self.const_expr(d)?)
                    }
                    None => None,
                },
                attributes: attrs,
            });
        }
        // Required count: up to the last required parameter.
        let mut required = 0u32;
        for (i, p) in spec.params.iter().enumerate() {
            if p.default.is_none() && !p.variadic {
                required = u32::try_from(i + 1).unwrap_or(0);
            }
        }
        // Deprecation for optional-before-required.
        for (i, p) in spec.params.iter().enumerate() {
            if let Some(d) = &p.default {
                if (i as u32) < required {
                    let is_nullable_null = is_null_literal(d) && p.ty.is_some();
                    let _ = is_nullable_null;
                    let later = spec.params[i + 1..]
                        .iter()
                        .find(|q| q.default.is_none() && !q.variadic)
                        .map(|q| q.name.as_str_lossy().into_owned())
                        .unwrap_or_default();
                    self.warnings.push((
                        8192,
                        format!(
                            "{}(): Optional parameter ${} declared before required parameter ${} is implicitly treated as a required parameter",
                            self.fn_display_name(),
                            p.name.as_str_lossy(),
                            later
                        ),
                        p.span.line,
                    ));
                }
            }
        }
        // Closure uses after params.
        let mut uses = Vec::new();
        for (name, by_ref, pcv, implicit) in &spec.uses {
            let cv = self.cv(name.as_bytes());
            uses.push(ClosureUse {
                name: name.clone(),
                by_ref: *by_ref,
                parent_cv: *pcv,
                cv,
                implicit: *implicit,
            });
        }
        let return_type = match spec.return_type {
            Some(t) => {
                let rt = self.resolve_type(t)?;
                Some(rt)
            }
            None => None,
        };
        if let Some(rt) = &return_type {
            if spec.is_generator {
                let ok = rt.mask & (mask::OBJECT | mask::ITERABLE) != 0
                    || rt.is_mixed()
                    || rt.classes.iter().any(|c| {
                        let l = c.to_ascii_lowercase();
                        matches!(l.as_bytes(), b"traversable" | b"iterator" | b"generator")
                    });
                if !ok {
                    return self.err(
                        format!(
                            "Generator return type must be a supertype of Generator, {} given",
                            rt.to_php_string()
                        ),
                        spec.return_type.map_or(spec.line_start, |t| t.span.line),
                    );
                }
            }
        }
        {
            let f = self.f();
            f.has_return_type = return_type.is_some();
            f.return_type.clone_from(&return_type);
        }
        // RECV ops.
        for (i, p) in spec.params.iter().enumerate() {
            let n = u32::try_from(i).unwrap_or(0);
            self.f().line = p.span.line;
            let cv = n;
            if p.variadic {
                self.emit(Op::RecvVariadic { n, cv });
            } else if let Some(d) = &p.default {
                let at = self.emit(Op::RecvInit { n, cv, skip: 0 });
                let v = self.compile_expr(d)?;
                self.f().line = p.span.line;
                self.emit(Op::RecvDefault { n, cv, val: v });
                self.patch_here(at);
            } else {
                self.emit(Op::Recv { n, cv });
            }
        }
        // Constructor promotion.
        if spec.is_ctor {
            for (i, p) in spec.params.iter().enumerate() {
                if !p.modifiers.is_empty() {
                    self.f().line = p.span.line;
                    let name = self.lit(Value::Str(ZStr::from(p.name.name.as_slice())));
                    let place = self.place(Place {
                        base: PlaceBase::This,
                        segs: vec![Seg::Prop(name)],
                    });
                    self.emit(Op::Assign {
                        place,
                        val: Operand::Cv(u32::try_from(i).unwrap_or(0)),
                        dst: None,
                    });
                }
            }
        }
        if spec.is_generator {
            self.f().line = spec.line_start;
            self.emit(Op::GeneratorCreate);
        }
        match body {
            BodyKind::Stmts(stmts) => {
                self.compile_stmts(stmts)?;
                self.f().line = spec.line_end;
                self.emit(Op::Return {
                    val: Operand::Unused,
                });
            }
            BodyKind::Arrow(e) => {
                self.f().line = e.span.line;
                if spec.by_ref && super::stmt::is_ref_expr(e) {
                    let p = self.compile_place(e, false)?;
                    let pid = self.place(p);
                    self.emit(Op::ReturnRef { place: pid });
                } else {
                    let v = self.compile_expr(e)?;
                    if return_type.as_ref().is_some_and(TypeDecl::is_void) {
                        self.free(v);
                        self.emit(Op::Return {
                            val: Operand::Unused,
                        });
                    } else {
                        self.emit(Op::Return { val: v });
                    }
                }
            }
            BodyKind::None => {
                self.emit(Op::Return {
                    val: Operand::Unused,
                });
            }
        }
        Ok((params, required, return_type, uses))
    }

    /// Resolves a type declaration.
    pub(crate) fn resolve_type(&mut self, t: &ast::Type) -> CResult<TypeDecl> {
        let mut out = TypeDecl::default();
        self.add_type(&mut out, t, false)?;
        if out.is_mixed() && out.mask & mask::NULL != 0 && matches!(t.kind, TypeKind::Nullable(_)) {
            return self.err(
                "Type mixed cannot be marked as nullable since mixed already includes null",
                t.span.line,
            );
        }
        Ok(out)
    }

    fn add_type(&mut self, out: &mut TypeDecl, t: &ast::Type, in_union: bool) -> CResult<()> {
        match &t.kind {
            TypeKind::Array => self.add_mask(out, mask::ARRAY, "array", t.span.line)?,
            TypeKind::Callable => self.add_mask(out, mask::CALLABLE, "callable", t.span.line)?,
            TypeKind::Static => self.add_mask(out, mask::STATIC, "static", t.span.line)?,
            TypeKind::Nullable(inner) => {
                self.add_type(out, inner, in_union)?;
                if let TypeKind::Named(n) = &inner.kind {
                    let lc = n.name.to_ascii_lowercase();
                    if n.kind == ast::NameKind::Unqualified {
                        if lc.as_slice() == b"null" {
                            return self.err("null cannot be marked as nullable", t.span.line);
                        }
                        if lc.as_slice() == b"void" {
                            return self
                                .err("Void can only be used as a standalone type", t.span.line);
                        }
                        if lc.as_slice() == b"never" {
                            return self
                                .err("never cannot be used as a parameter type", t.span.line);
                        }
                        if lc.as_slice() == b"mixed" {
                            return self.err(
                                "Type mixed cannot be marked as nullable since mixed already includes null",
                                t.span.line,
                            );
                        }
                    }
                }
                out.mask |= mask::NULL;
            }
            TypeKind::Union(ts) => {
                for x in ts {
                    if let TypeKind::Intersection(parts) = &x.kind {
                        let mut g = Vec::new();
                        for p in parts {
                            if let TypeKind::Named(n) = &p.kind {
                                g.push(ZStr::from(self.resolve_type_class(n)));
                            }
                        }
                        out.intersections.push(g);
                    } else {
                        self.add_type(out, x, true)?;
                    }
                }
                if out.mask & mask::VOID != 0 {
                    return self.err("Void can only be used as a standalone type", t.span.line);
                }
                if out.is_mixed() {
                    return self.err(
                        "Type mixed can only be used as a standalone type",
                        t.span.line,
                    );
                }
            }
            TypeKind::Intersection(ts) => {
                let mut g = Vec::new();
                for p in ts {
                    match &p.kind {
                        TypeKind::Named(n) => {
                            let lc = n.name.to_ascii_lowercase();
                            if n.kind == ast::NameKind::Unqualified && is_builtin_type(&lc) {
                                return self.err(
                                    format!(
                                        "Type {} cannot be part of an intersection type",
                                        String::from_utf8_lossy(&lc)
                                    ),
                                    p.span.line,
                                );
                            }
                            g.push(ZStr::from(self.resolve_type_class(n)));
                        }
                        TypeKind::Array => {
                            return self.err(
                                "Type array cannot be part of an intersection type",
                                p.span.line,
                            )
                        }
                        TypeKind::Static => {
                            return self.err(
                                "Type static cannot be part of an intersection type",
                                p.span.line,
                            )
                        }
                        TypeKind::Callable => {
                            return self.err(
                                "Type callable cannot be part of an intersection type",
                                p.span.line,
                            )
                        }
                        _ => {}
                    }
                }
                out.intersections.push(g);
                if !in_union {
                    out.pure_intersection = true;
                }
            }
            TypeKind::Named(n) => {
                let lc = n.name.to_ascii_lowercase();
                if n.kind == ast::NameKind::Unqualified {
                    let m = match lc.as_slice() {
                        b"int" => Some((mask::LONG, "int")),
                        b"float" => Some((mask::DOUBLE, "float")),
                        b"string" => Some((mask::STRING, "string")),
                        b"bool" => Some((mask::BOOL, "bool")),
                        b"false" => Some((mask::FALSE, "false")),
                        b"true" => Some((mask::TRUE, "true")),
                        b"null" => Some((mask::NULL, "null")),
                        b"void" => Some((mask::VOID, "void")),
                        b"never" => Some((mask::NEVER, "never")),
                        b"mixed" => Some((mask::ANY, "mixed")),
                        b"object" => Some((mask::OBJECT, "object")),
                        b"iterable" => Some((mask::ITERABLE, "iterable")),
                        b"array" => Some((mask::ARRAY, "array")),
                        b"callable" => Some((mask::CALLABLE, "callable")),
                        b"static" => Some((mask::STATIC, "static")),
                        _ => None,
                    };
                    if let Some((m, nm)) = m {
                        return self.add_mask(out, m, nm, t.span.line);
                    }
                    if matches!(lc.as_slice(), b"self" | b"parent") {
                        if self.current_class().is_none() && self.closure_depth == 0 {
                            return self.err(
                                format!(
                                    "Cannot use \"{}\" when no class scope is active",
                                    String::from_utf8_lossy(&lc)
                                ),
                                t.span.line,
                            );
                        }
                        let resolved = if lc.as_slice() == b"self" {
                            self.current_class()
                                .filter(|c| !c.is_trait)
                                .map_or_else(|| ZStr::from("self"), |c| c.name.clone())
                        } else {
                            self.current_class()
                                .and_then(|c| c.parent.clone())
                                .unwrap_or_else(|| ZStr::from("parent"))
                        };
                        self.add_class_type(out, resolved, t.span.line)?;
                        return Ok(());
                    }
                    if matches!(
                        lc.as_slice(),
                        b"integer" | b"boolean" | b"double" | b"resource"
                    ) {
                        let correct = match lc.as_slice() {
                            b"integer" => Some("int"),
                            b"boolean" => Some("bool"),
                            b"double" => Some("float"),
                            _ => None,
                        };
                        let name = String::from_utf8_lossy(&n.name).into_owned();
                        let msg = match correct {
                            Some(c) => format!(
                                "\"{name}\" will be interpreted as a class name. Did you mean \"{c}\"? Write \"\\{name}\" to suppress this warning"
                            ),
                            None => format!(
                                "\"{name}\" is not a supported builtin type and will be interpreted as a class name. Write \"\\{name}\" to suppress this warning"
                            ),
                        };
                        self.warnings.push((128, msg, t.span.line));
                    }
                }
                let resolved = ZStr::from(self.resolve_class_name(n));
                self.add_class_type(out, resolved, t.span.line)?;
            }
        }
        Ok(())
    }

    fn add_class_type(&mut self, out: &mut TypeDecl, name: ZStr, line: u32) -> CResult<()> {
        if out
            .classes
            .iter()
            .any(|c| c.eq_ignore_ascii_case(name.as_bytes()))
        {
            return self.err(format!("Duplicate type {name} is redundant"), line);
        }
        out.classes.push(name);
        Ok(())
    }

    fn resolve_type_class(&self, n: &ast::Name) -> Vec<u8> {
        let lc = n.name.to_ascii_lowercase();
        if n.kind == ast::NameKind::Unqualified && lc.as_slice() == b"self" {
            if let Some(c) = self.current_class() {
                if !c.is_trait {
                    return c.name.as_bytes().to_vec();
                }
            }
        }
        self.resolve_class_name(n)
    }

    fn add_mask(&mut self, out: &mut TypeDecl, m: u32, name: &str, line: u32) -> CResult<()> {
        if out.mask & m == m && m != 0 {
            return self.err(format!("Duplicate type {name} is redundant"), line);
        }
        if (m == mask::FALSE || m == mask::TRUE) && out.mask & mask::BOOL == mask::BOOL {
            return self.err(format!("Duplicate type {name} is redundant"), line);
        }
        if m == mask::BOOL && out.mask & (mask::FALSE | mask::TRUE) != 0 {
            let other = if out.mask & mask::FALSE != 0 {
                "false"
            } else {
                "true"
            };
            return self.err(format!("Duplicate type {other} is redundant"), line);
        }
        if (m == mask::TRUE && out.mask & mask::FALSE != 0)
            || (m == mask::FALSE && out.mask & mask::TRUE != 0)
        {
            return self.err(
                "Type contains both true and false, bool must be used instead",
                line,
            );
        }
        out.mask |= m;
        Ok(())
    }

    pub(crate) fn compile_class_stmt(
        &mut self,
        cd: &ast::ClassDecl,
        top_level: bool,
    ) -> CResult<()> {
        let idx = self.compile_class_decl(cd, false)?;
        if top_level {
            self.hoisted_classes.push(idx);
        }
        self.f().line = cd.span.line;
        self.emit(Op::DeclareClass(idx));
        Ok(())
    }

    /// Compiles a class-like declaration; returns its index.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn compile_class_decl(
        &mut self,
        cd: &ast::ClassDecl,
        is_anon: bool,
    ) -> CResult<u32> {
        let kind = match cd.kind {
            ast::ClassKind::Class => ClassKind::Class,
            ast::ClassKind::Interface => ClassKind::Interface,
            ast::ClassKind::Trait => ClassKind::Trait,
            ast::ClassKind::Enum => ClassKind::Enum,
        };
        let parent = cd
            .extends
            .as_ref()
            .map(|n| ZStr::from(self.resolve_class_name(n)));
        let name: ZStr = if is_anon {
            self.anon_counter += 1;
            let prefix = parent.as_ref().map_or_else(
                || {
                    cd.implements.first().map_or_else(
                        || "class".to_string(),
                        |i| String::from_utf8_lossy(&self.resolve_class_name(i)).into_owned(),
                    )
                },
                ZStr::to_string_lossy,
            );
            let mut v = format!("{prefix}@anonymous").into_bytes();
            v.push(0);
            v.extend_from_slice(
                format!(
                    "{}:{}${:x}",
                    self.filename,
                    cd.span.line,
                    self.anon_counter - 1
                )
                .as_bytes(),
            );
            ZStr::from(v)
        } else {
            let n = cd.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
            let lc = n.to_ascii_lowercase();
            if is_reserved_class_name(&lc) {
                return self.err(
                    format!(
                        "Cannot use '{}' as class name as it is reserved",
                        String::from_utf8_lossy(&n)
                    ),
                    cd.span.line,
                );
            }
            ZStr::from(self.ns_prefixed_pub(&n))
        };
        if let Some(p) = &parent {
            let lc = p.to_ascii_lowercase();
            if matches!(lc.as_bytes(), b"self" | b"parent" | b"static") {
                return self.err(
                    format!(
                        "Cannot use '{}' as class name, as it is reserved",
                        p.to_string_lossy()
                    ),
                    cd.span.line,
                );
            }
        }
        let mut cflag = 0u32;
        match kind {
            ClassKind::Interface => cflag |= cflags::INTERFACE,
            ClassKind::Trait => cflag |= cflags::TRAIT,
            ClassKind::Enum => cflag |= cflags::ENUM | cflags::FINAL,
            ClassKind::Class => {}
        }
        if cd.modifiers.contains(ast::Modifiers::ABSTRACT) {
            cflag |= cflags::ABSTRACT | cflags::EXPLICIT_ABSTRACT;
        }
        if cd.modifiers.contains(ast::Modifiers::FINAL) {
            cflag |= cflags::FINAL;
        }
        if cd.modifiers.contains(ast::Modifiers::READONLY) {
            cflag |= cflags::READONLY;
        }
        if is_anon {
            cflag |= cflags::ANON | cflags::FINAL;
        }
        let attributes = self.attributes(&cd.attributes)?;
        self.validate_attributes(
            &attributes,
            super::attrs::target::CLASS,
            0,
            Some(super::attrs::AttrClass {
                name: &name,
                flags: cflag,
            }),
            false,
        )?;
        if attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(b"AllowDynamicProperties"))
        {
            cflag |= cflags::ALLOW_DYNAMIC;
        }
        let interfaces: Vec<ZStr> = cd
            .implements
            .iter()
            .map(|n| ZStr::from(self.resolve_class_name(n)))
            .collect();
        self.classes.push(ClassCtx {
            name: name.clone(),
            parent: parent.clone(),
            is_trait: kind == ClassKind::Trait,
            is_interface: kind == ClassKind::Interface,
            is_anon,
        });
        let saved_closure_depth = self.closure_depth;
        self.closure_depth = 0;
        let r = self.compile_class_members(cd, &name, kind, cflag);
        self.closure_depth = saved_closure_depth;
        self.classes.pop();
        let (consts, props, methods, traits, trait_rules, cases, case_attributes) = r?;
        let backing = match &cd.backing_type {
            Some(t) => {
                let ty = self.resolve_type(t)?;
                if ty.mask == mask::LONG && !ty.has_classes() {
                    Some(mask::LONG)
                } else if ty.mask == mask::STRING && !ty.has_classes() {
                    Some(mask::STRING)
                } else {
                    return self.err(
                        format!(
                            "Enum backing type must be int or string, {} given",
                            ty.to_php_string()
                        ),
                        t.span.line,
                    );
                }
            }
            None => None,
        };
        let decl = ClassDecl {
            name,
            kind,
            flags: cflag,
            parent,
            interfaces,
            traits,
            trait_rules,
            consts,
            props,
            methods,
            cases,
            case_attributes,
            backing,
            attributes,
            doc_comment: cd.doc_comment.as_ref().map(|d| ZStr::from(d.as_slice())),
            filename: self.filename.clone(),
            line_start: cd.span.line,
            line_end: cd.end_line,
        };
        Ok(self.add_class(decl))
    }

    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    fn compile_class_members(
        &mut self,
        cd: &ast::ClassDecl,
        class_name: &ZStr,
        kind: ClassKind,
        cflag: u32,
    ) -> CResult<(
        Vec<ConstDecl>,
        Vec<PropDecl>,
        Vec<MethodDecl>,
        Vec<ZStr>,
        Vec<TraitRule>,
        Vec<(ZStr, Option<ConstExpr>, u32)>,
        Vec<Vec<AttributeInfo>>,
    )> {
        let mut consts = Vec::new();
        let mut props: Vec<PropDecl> = Vec::new();
        let mut methods: Vec<MethodDecl> = Vec::new();
        let mut traits = Vec::new();
        let mut rules = Vec::new();
        let mut cases = Vec::new();
        let mut case_attrs = Vec::new();
        let display = |n: &ZStr| {
            let b = n.as_bytes();
            match b.iter().position(|&c| c == 0) {
                Some(p) => String::from_utf8_lossy(&b[..p]).into_owned(),
                None => n.to_string_lossy(),
            }
        };
        let cname = display(class_name);
        for m in &cd.members {
            match &m.kind {
                ClassMemberKind::Property(pg) => {
                    let fl = modifiers_to_flags(pg.modifiers)
                        | if cflag & cflags::READONLY != 0
                            && pg.modifiers.contains(ast::Modifiers::STATIC) == false
                        {
                            flags::READONLY
                        } else {
                            0
                        };
                    if kind == ClassKind::Interface
                        && pg
                            .modifiers
                            .intersects(ast::Modifiers::PROTECTED | ast::Modifiers::PRIVATE)
                    {
                        return self.err(
                            "Property in interface cannot be protected or private",
                            m.span.line,
                        );
                    }
                    if kind == ClassKind::Interface
                        && pg.modifiers.contains(ast::Modifiers::ABSTRACT)
                    {
                        return self.err("Property in interface cannot be explicitly abstract. All interface members are implicitly abstract", m.span.line);
                    }
                    if kind == ClassKind::Interface && pg.modifiers.contains(ast::Modifiers::FINAL)
                    {
                        return self.err("Property in interface cannot be final", m.span.line);
                    }
                    if pg.modifiers.contains(ast::Modifiers::FINAL)
                        && pg.modifiers.contains(ast::Modifiers::PRIVATE)
                    {
                        return self.err("Property cannot be both final and private", m.span.line);
                    }
                    if kind == ClassKind::Interface && pg.props.iter().all(|p| p.hooks.is_none()) {
                        return self.err("Interfaces may not include properties", m.span.line);
                    }
                    if kind == ClassKind::Enum {
                        return self.err(
                            format!("Enum {cname} cannot include properties"),
                            m.span.line,
                        );
                    }
                    if pg.modifiers.contains(ast::Modifiers::ABSTRACT)
                        && pg.props.iter().all(|p| p.hooks.is_none())
                    {
                        return self.err("Properties cannot be declared abstract", m.span.line);
                    }
                    if pg.modifiers.contains(ast::Modifiers::FINAL)
                        && pg.props.iter().all(|p| p.hooks.is_none())
                    {
                        return self.err("Cannot declare property final, the final modifier is allowed only on methods, classes, and properties with hooks", m.span.line);
                    }
                    let ty = match &pg.ty {
                        Some(t) => {
                            let ty = self.resolve_type(t)?;
                            if ty.mask & mask::VOID != 0 {
                                return self.err(
                                    format!(
                                        "Property {cname}::${} cannot have type void",
                                        pg.props[0].name.as_str_lossy()
                                    ),
                                    t.span.line,
                                );
                            }
                            if ty.mask & mask::NEVER != 0 {
                                return self.err(
                                    format!(
                                        "Property {cname}::${} cannot have type never",
                                        pg.props[0].name.as_str_lossy()
                                    ),
                                    t.span.line,
                                );
                            }
                            if ty.mask & mask::CALLABLE != 0 {
                                return self.err(
                                    format!(
                                        "Property {cname}::${} cannot have type callable",
                                        pg.props[0].name.as_str_lossy()
                                    ),
                                    t.span.line,
                                );
                            }
                            Some(ty)
                        }
                        None => None,
                    };
                    let attrs = self.attributes(&pg.attributes)?;
                    self.validate_attributes(
                        &attrs,
                        super::attrs::target::PROPERTY,
                        0,
                        None,
                        false,
                    )?;
                    for p in &pg.props {
                        self.f().line = p.span.line;
                        if props
                            .iter()
                            .any(|q| q.name.as_bytes() == p.name.name.as_slice())
                        {
                            return self.err(
                                format!("Cannot redeclare {cname}::${}", p.name.as_str_lossy()),
                                p.span.line,
                            );
                        }
                        if fl & flags::READONLY != 0 {
                            if ty.is_none() {
                                return self.err(
                                    format!(
                                        "Readonly property {cname}::${} must have type",
                                        p.name.as_str_lossy()
                                    ),
                                    p.span.line,
                                );
                            }
                            if p.default.is_some() {
                                return self.err(
                                    format!(
                                        "Readonly property {cname}::${} cannot have default value",
                                        p.name.as_str_lossy()
                                    ),
                                    p.span.line,
                                );
                            }
                            if fl & flags::STATIC != 0 {
                                return self.err(
                                    format!(
                                        "Static property {cname}::${} cannot be readonly",
                                        p.name.as_str_lossy()
                                    ),
                                    p.span.line,
                                );
                            }
                        }
                        let default = match &p.default {
                            Some(d) => {
                                self.check_const_expr(d, false)?;
                                Some(self.const_expr(d)?)
                            }
                            None => {
                                if ty.is_none() {
                                    Some(ConstExpr::Value(Value::Null))
                                } else {
                                    None
                                }
                            }
                        };
                        let mut fl = normalize_set_visibility(
                            self,
                            fl,
                            ty.is_some(),
                            &cname,
                            &p.name.as_str_lossy(),
                            p.span.line,
                        )?;
                        let hooks = match &p.hooks {
                            Some(hs) => {
                                let (h, virt) = self.compile_prop_hooks(
                                    &cname,
                                    &p.name.name,
                                    pg.ty.as_ref(),
                                    hs,
                                    kind == ClassKind::Interface,
                                    fl,
                                    p.span.line,
                                )?;
                                if virt {
                                    fl |= flags::VIRTUAL;
                                }
                                Some(Rc::new(h))
                            }
                            None => None,
                        };
                        props.push(PropDecl {
                            name: ZStr::from(p.name.name.as_slice()),
                            default,
                            flags: fl,
                            ty: ty.clone(),
                            doc_comment: p.doc_comment.as_ref().map(|d| ZStr::from(d.as_slice())),
                            attributes: attrs.clone(),
                            line: p.span.line,
                            hooks,
                        });
                    }
                }
                ClassMemberKind::Const(cg) => {
                    let fl = modifiers_to_flags(cg.modifiers);
                    if cg.modifiers.contains(ast::Modifiers::STATIC) {
                        return self.err("Cannot use 'static' as constant modifier", m.span.line);
                    }
                    if cg.modifiers.contains(ast::Modifiers::ABSTRACT) {
                        return self.err("Cannot use 'abstract' as constant modifier", m.span.line);
                    }
                    if cg.modifiers.contains(ast::Modifiers::READONLY) {
                        return self.err("Cannot use 'readonly' as constant modifier", m.span.line);
                    }
                    let ty = match &cg.ty {
                        Some(t) => Some(self.resolve_type(t)?),
                        None => None,
                    };
                    let attrs = self.attributes(&cg.attributes)?;
                    self.validate_attributes(
                        &attrs,
                        super::attrs::target::CLASS_CONST,
                        0,
                        None,
                        false,
                    )?;
                    for c in &cg.consts {
                        if c.name.eq_ignore_case("class") {
                            return self.err(
                                "A class constant must not be called 'class'; it is reserved for class name fetching",
                                c.span.line,
                            );
                        }
                        if consts
                            .iter()
                            .any(|q: &ConstDecl| q.name.as_bytes() == c.name.name.as_slice())
                        {
                            return self.err(
                                format!(
                                    "Cannot redefine class constant {cname}::{}",
                                    c.name.as_str_lossy()
                                ),
                                c.span.line,
                            );
                        }
                        if fl & flags::PRIVATE != 0 && fl & flags::FINAL != 0 {
                            return self.err(
                                format!("Private constant {cname}::{} cannot be final as it is not visible to other classes", c.name.as_str_lossy()),
                                c.span.line,
                            );
                        }
                        self.f().line = c.span.line;
                        self.check_const_expr(&c.value, false)?;
                        let value = self.const_expr(&c.value)?;
                        consts.push(ConstDecl {
                            name: ZStr::from(c.name.name.as_slice()),
                            value,
                            flags: fl,
                            ty: ty.clone(),
                            doc_comment: c.doc_comment.as_ref().map(|d| ZStr::from(d.as_slice())),
                            attributes: attrs.clone(),
                            line: c.span.line,
                        });
                    }
                }
                ClassMemberKind::Method(md) => {
                    let mut fl = modifiers_to_flags(md.modifiers);
                    let mname = md.name.as_str_lossy().into_owned();
                    if methods
                        .iter()
                        .any(|q| q.name.eq_ignore_ascii_case(&md.name.name))
                    {
                        return self
                            .err(format!("Cannot redeclare {cname}::{mname}()"), md.span.line);
                    }
                    if kind == ClassKind::Interface {
                        if md.body.is_some() {
                            return self.err(
                                format!(
                                    "Interface function {cname}::{mname}() cannot contain body"
                                ),
                                md.span.line,
                            );
                        }
                        if fl & (flags::PRIVATE | flags::PROTECTED) != 0 {
                            return self.err(
                                format!("Access type for interface method {cname}::{mname}() must be public"),
                                md.span.line,
                            );
                        }
                        if fl & flags::FINAL != 0 {
                            return self.err(
                                format!("Interface method {cname}::{mname}() must not be final"),
                                md.span.line,
                            );
                        }
                        if fl & flags::ABSTRACT != 0 {
                            return self.err(
                                format!("Interface method {cname}::{mname}() must not be abstract"),
                                md.span.line,
                            );
                        }
                        fl |= flags::ABSTRACT;
                    } else if fl & flags::ABSTRACT != 0 {
                        if md.body.is_some() {
                            return self.err(
                                format!("Abstract function {cname}::{mname}() cannot contain body"),
                                md.span.line,
                            );
                        }
                        if fl & flags::PRIVATE != 0 && kind != ClassKind::Trait {
                            return self.err(
                                format!("Abstract function {cname}::{mname}() cannot be declared private"),
                                md.span.line,
                            );
                        }
                        if fl & flags::FINAL != 0 {
                            return self.err(
                                format!("Cannot use the final modifier on an abstract method {cname}::{mname}()"),
                                md.span.line,
                            );
                        }
                        if kind == ClassKind::Class
                            && cflag & cflags::EXPLICIT_ABSTRACT == 0
                            && cflag & cflags::ANON == 0
                        {
                            return self.err(
                                format!("Class {cname} declares abstract method {mname}() and must therefore be declared abstract"),
                                md.span.line,
                            );
                        }
                    } else if md.body.is_none() {
                        return self.err(
                            format!("Non-abstract method {cname}::{mname}() must contain body"),
                            md.span.line,
                        );
                    }
                    if fl & flags::PRIVATE != 0
                        && fl & flags::FINAL != 0
                        && !md.name.eq_ignore_case("__construct")
                    {
                        self.warnings.push((
                            128,
                            "Private methods cannot be final as they are never overridden by other classes".to_string(),
                            md.span.line,
                        ));
                    }
                    let is_ctor = md.name.eq_ignore_case("__construct");
                    if !is_ctor && md.params.iter().any(|p| !p.modifiers.is_empty()) {
                        return self.err(
                            "Cannot declare promoted property outside a constructor",
                            md.span.line,
                        );
                    }
                    if is_ctor {
                        for p in &md.params {
                            if p.modifiers.is_empty() {
                                continue;
                            }
                            if kind == ClassKind::Interface || fl & flags::ABSTRACT != 0 {
                                return self.err(
                                    "Cannot declare promoted property in an abstract constructor",
                                    p.span.line,
                                );
                            }
                            let pfl = modifiers_to_flags(p.modifiers)
                                | if cflag & cflags::READONLY != 0 {
                                    flags::READONLY
                                } else {
                                    0
                                };
                            let ty = match &p.ty {
                                Some(t) => Some(self.resolve_type(t)?),
                                None => None,
                            };
                            if pfl & flags::READONLY != 0 && ty.is_none() {
                                return self.err(
                                    format!(
                                        "Readonly property {cname}::${} must have type",
                                        p.name.as_str_lossy()
                                    ),
                                    p.span.line,
                                );
                            }
                            if props
                                .iter()
                                .any(|q| q.name.as_bytes() == p.name.name.as_slice())
                            {
                                return self.err(
                                    format!("Cannot redeclare {cname}::${}", p.name.as_str_lossy()),
                                    p.span.line,
                                );
                            }
                            let attrs = self.attributes(&p.attributes)?;
                            self.validate_attributes(
                                &attrs,
                                super::attrs::target::PROPERTY,
                                super::attrs::target::PARAMETER,
                                None,
                                false,
                            )?;
                            let pfl = normalize_set_visibility(
                                self,
                                pfl,
                                ty.is_some(),
                                &cname,
                                &p.name.as_str_lossy(),
                                p.span.line,
                            )?;
                            props.push(PropDecl {
                                name: ZStr::from(p.name.name.as_slice()),
                                default: if ty.is_none() {
                                    Some(ConstExpr::Value(Value::Null))
                                } else {
                                    None
                                },
                                flags: pfl,
                                ty,
                                doc_comment: p
                                    .doc_comment
                                    .as_ref()
                                    .map(|d| ZStr::from(d.as_slice())),
                                attributes: attrs,
                                line: p.span.line,
                                hooks: None,
                            });
                        }
                    }
                    let spec = BodySpec {
                        kind: FnKind::Method,
                        name: ZStr::from(md.name.name.as_slice()),
                        method_name: ZStr::from(md.name.name.as_slice()),
                        params: &md.params,
                        return_type: md.return_type.as_ref(),
                        by_ref: md.by_ref,
                        is_generator: md.is_generator,
                        is_static: fl & flags::STATIC != 0,
                        line_start: md.span.line,
                        line_end: md.end_line,
                        doc_comment: md.doc_comment.as_ref(),
                        attributes: &md.attributes,
                        is_ctor,
                        uses: Vec::new(),
                        has_body: md.body.is_some(),
                    };
                    let body = match &md.body {
                        Some(b) => BodyKind::Stmts(b),
                        None => BodyKind::None,
                    };
                    let mut uf = self.compile_body(&spec, body)?;
                    uf.flags |= fl;
                    self.check_magic_method(&cname, &md.name.as_str_lossy(), &uf, md.span.line)?;
                    if md.name.eq_ignore_case("__tostring") && uf.return_type.is_none() {
                        uf.return_type =
                            Some(crate::types::TypeDecl::simple(crate::types::mask::STRING));
                    }
                    if is_ctor {
                        uf.flags |= flags::CTOR;
                    }
                    methods.push(MethodDecl {
                        name: ZStr::from(md.name.name.as_slice()),
                        func: Rc::new(uf),
                        flags: fl,
                    });
                }
                ClassMemberKind::TraitUse(tu) => {
                    if kind == ClassKind::Interface {
                        return self.err(
                            format!(
                                "Cannot use traits inside of interfaces. {} is used in {cname}",
                                String::from_utf8_lossy(&self.resolve_class_name(&tu.traits[0]))
                            ),
                            m.span.line,
                        );
                    }
                    for t in &tu.traits {
                        traits.push(ZStr::from(self.resolve_class_name(t)));
                    }
                    for a in &tu.adaptations {
                        match a {
                            ast::TraitAdaptation::Precedence {
                                method, insteadof, ..
                            } => {
                                let tn = method
                                    .trait_name
                                    .as_ref()
                                    .map(|n| ZStr::from(self.resolve_class_name(n)))
                                    .unwrap_or_default();
                                rules.push(TraitRule::Precedence {
                                    trait_name: tn,
                                    method: ZStr::from(method.method.name.as_slice()),
                                    insteadof: insteadof
                                        .iter()
                                        .map(|n| ZStr::from(self.resolve_class_name(n)))
                                        .collect(),
                                });
                            }
                            ast::TraitAdaptation::Alias {
                                method,
                                modifiers,
                                alias,
                                ..
                            } => {
                                let f = if modifiers.is_empty() {
                                    0
                                } else {
                                    let mut f = modifiers_to_flags(*modifiers);
                                    if !modifiers.intersects(ast::Modifiers::VISIBILITY) {
                                        f &= !flags::PUBLIC;
                                    }
                                    f
                                };
                                rules.push(TraitRule::Alias {
                                    trait_name: method
                                        .trait_name
                                        .as_ref()
                                        .map(|n| ZStr::from(self.resolve_class_name(n))),
                                    method: ZStr::from(method.method.name.as_slice()),
                                    flags: f,
                                    alias: alias.as_ref().map(|a| ZStr::from(a.name.as_slice())),
                                });
                            }
                        }
                    }
                }
                ClassMemberKind::EnumCase(ec) => {
                    if kind != ClassKind::Enum {
                        return self.err(format!("Case can only be used in enums"), m.span.line);
                    }
                    let v = match &ec.value {
                        Some(e) => {
                            self.check_const_expr(e, false)?;
                            Some(self.const_expr(e)?)
                        }
                        None => None,
                    };
                    match (&ec.value, &cd.backing_type) {
                        (None, Some(_)) => {
                            return self.err(
                                format!(
                                    "Case {} of backed enum {cname} must have a value",
                                    ec.name.as_str_lossy()
                                ),
                                m.span.line,
                            );
                        }
                        (Some(_), None) => {
                            return self.err(
                                format!(
                                    "Case {} of non-backed enum {cname} must not have a value",
                                    ec.name.as_str_lossy()
                                ),
                                m.span.line,
                            );
                        }
                        _ => {}
                    }
                    if cases
                        .iter()
                        .any(|(n, _, _): &(ZStr, Option<ConstExpr>, u32)| {
                            n.as_bytes() == ec.name.name.as_slice()
                        })
                        || consts
                            .iter()
                            .any(|c: &ConstDecl| c.name.as_bytes() == ec.name.name.as_slice())
                    {
                        return self.err(
                            format!(
                                "Cannot redefine class constant {cname}::{}",
                                ec.name.as_str_lossy()
                            ),
                            m.span.line,
                        );
                    }
                    let attrs = self.attributes(&ec.attributes)?;
                    self.validate_attributes(
                        &attrs,
                        super::attrs::target::CLASS_CONST,
                        0,
                        None,
                        false,
                    )?;
                    case_attrs.push(attrs);
                    cases.push((ZStr::from(ec.name.name.as_slice()), v, m.span.line));
                }
            }
        }
        Ok((consts, props, methods, traits, rules, cases, case_attrs))
    }
}

impl Compiler {
    /// Compiles the hooks of a property; returns the hooks and whether the
    /// property is virtual.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) fn compile_prop_hooks(
        &mut self,
        cname: &str,
        prop: &[u8],
        prop_ty: Option<&ast::Type>,
        hooks: &[ast::PropertyHook],
        is_interface: bool,
        prop_flags: u32,
        line: u32,
    ) -> CResult<(PropHooks, bool)> {
        let pname = String::from_utf8_lossy(prop).into_owned();
        if prop_flags & flags::READONLY != 0 {
            return self.err("Hooked properties cannot be readonly", line);
        }
        if hooks.is_empty() {
            return self.err("Property hook list must not be empty", line);
        }
        if prop_flags & flags::STATIC != 0 {
            return self.err("Cannot declare hooks for static property", line);
        }
        let mut out = PropHooks {
            class: self
                .current_class()
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            ..PropHooks::default()
        };
        let mut is_virtual = true;
        if is_interface {
            is_virtual = true;
        }
        for h in hooks {
            let hname = h.name.name.to_ascii_lowercase();
            let is_get = match hname.as_slice() {
                b"get" => true,
                b"set" => false,
                _ => {
                    return self.err(
                        format!(
                            "Unknown hook \"{}\" for property {cname}::${pname}, expected \"get\" or \"set\"",
                            h.name.as_str_lossy()
                        ),
                        h.span.line,
                    )
                }
            };
            let hm = modifiers_to_flags(h.modifiers);
            let is_final = h.modifiers.contains(ast::Modifiers::FINAL);
            if is_final && prop_flags & flags::PRIVATE != 0 {
                return self.err(
                    "Property hook cannot be both final and private",
                    h.span.line,
                );
            }
            for (m, n) in [
                (ast::Modifiers::PUBLIC, "public"),
                (ast::Modifiers::PROTECTED, "protected"),
                (ast::Modifiers::PRIVATE, "private"),
                (ast::Modifiers::STATIC, "static"),
                (ast::Modifiers::READONLY, "readonly"),
            ] {
                if h.modifiers.contains(m) {
                    return self.err(
                        format!("Cannot use the {n} modifier on a property hook"),
                        h.span.line,
                    );
                }
            }
            let _ = hm;
            let has_body = !matches!(h.body, ast::HookBody::None);
            let is_abstract = is_interface || (prop_flags & flags::ABSTRACT != 0 && !has_body);
            if is_abstract {
                if has_body {
                    return self.err("Abstract property hook cannot have body", h.span.line);
                }
                if prop_flags & flags::PRIVATE != 0 {
                    return self.err(
                        "Property hook cannot be both abstract and private",
                        h.span.line,
                    );
                }
                if is_final {
                    return self.err(
                        "Property hook cannot be both abstract and final",
                        h.span.line,
                    );
                }
            } else if !has_body {
                return self.err("Non-abstract property hook must have a body", h.span.line);
            }
            if (is_get && out.get.is_some()) || (!is_get && out.set.is_some()) {
                return self.err(
                    format!(
                        "Cannot redeclare property hook \"{}\"",
                        h.name.as_str_lossy()
                    ),
                    h.span.line,
                );
            }
            // Parameters.
            let implicit_param;
            let params: &[ast::Param] = if is_get {
                if h.params.is_some() {
                    return self.err(
                        format!(
                            "get hook of property {cname}::${pname} must not have a parameter list"
                        ),
                        h.span.line,
                    );
                }
                &[]
            } else if let Some(ps) = &h.params {
                if ps.len() != 1 {
                    return self.err(
                        format!("set hook of property {cname}::${pname} must accept exactly one parameters"),
                        h.span.line,
                    );
                }
                let p = &ps[0];
                let n = p.name.as_str_lossy();
                if p.by_ref {
                    return self.err(
                        format!("Parameter ${n} of set hook {cname}::${pname} must not be pass-by-reference"),
                        h.span.line,
                    );
                }
                if p.variadic {
                    return self.err(
                        format!(
                            "Parameter ${n} of set hook {cname}::${pname} must not be variadic"
                        ),
                        h.span.line,
                    );
                }
                if p.default.is_some() {
                    return self.err(
                        format!("Parameter ${n} of set hook {cname}::${pname} must not have a default value"),
                        h.span.line,
                    );
                }
                if prop_ty.is_some() != p.ty.is_some() {
                    return self.err(
                        format!("Type of parameter ${n} of hook {cname}::${pname}::set must be compatible with property type"),
                        h.span.line,
                    );
                }
                ps
            } else {
                implicit_param = vec![ast::Param {
                    attributes: Vec::new(),
                    modifiers: ast::Modifiers::NONE,
                    ty: prop_ty.cloned(),
                    by_ref: false,
                    variadic: false,
                    name: ast::Ident::new(b"value".to_vec(), h.span),
                    default: None,
                    doc_comment: None,
                    hooks: None,
                    span: h.span,
                }];
                &implicit_param
            };
            // Body.
            let body_stmts: Vec<ast::Stmt>;
            let body = match &h.body {
                ast::HookBody::None => BodyKind::None,
                ast::HookBody::Block(b) => BodyKind::Stmts(b),
                ast::HookBody::Expr(e) => {
                    let sp = e.span;
                    let st = if is_get {
                        ast::Stmt {
                            kind: ast::StmtKind::Return(Some((**e).clone())),
                            span: sp,
                        }
                    } else {
                        is_virtual = false;
                        let target = ast::Expr::new(
                            ast::ExprKind::Prop {
                                object: Box::new(ast::Expr::new(
                                    ast::ExprKind::Var(ast::VarName::Name(ast::Ident::new(
                                        b"this".to_vec(),
                                        sp,
                                    ))),
                                    sp,
                                )),
                                name: ast::MemberName::Ident(ast::Ident::new(prop.to_vec(), sp)),
                                nullsafe: false,
                            },
                            sp,
                        );
                        ast::Stmt {
                            kind: ast::StmtKind::Expr(ast::Expr::new(
                                ast::ExprKind::Assign {
                                    target: Box::new(target),
                                    value: e.clone(),
                                },
                                sp,
                            )),
                            span: sp,
                        }
                    };
                    body_stmts = vec![st];
                    BodyKind::Stmts(&body_stmts)
                }
            };
            let void_ty;
            let ret: Option<&ast::Type> = if is_get {
                prop_ty
            } else {
                void_ty = ast::Type {
                    kind: ast::TypeKind::Named(ast::Name {
                        kind: ast::NameKind::Unqualified,
                        name: b"void".to_vec(),
                        span: h.span,
                    }),
                    span: h.span,
                };
                Some(&void_ty)
            };
            let fname = ZStr::from(format!("${pname}::{}", if is_get { "get" } else { "set" }));
            let spec = BodySpec {
                kind: FnKind::Method,
                name: fname.clone(),
                method_name: fname,
                params,
                return_type: ret,
                by_ref: h.by_ref,
                is_generator: h.is_generator,
                is_static: false,
                line_start: h.span.line,
                line_end: h.end_line,
                doc_comment: h.doc_comment.as_ref(),
                attributes: &h.attributes,
                is_ctor: false,
                uses: Vec::new(),
                has_body,
            };
            let saved = self.hook_prop.replace(prop.to_vec());
            let r = self.compile_body(&spec, body);
            self.hook_prop = saved;
            let mut uf = r?;
            uf.hook_prop = Some(ZStr::from(prop));
            let vis = if prop_flags & flags::PRIVATE != 0 {
                flags::PRIVATE
            } else {
                flags::PUBLIC
            };
            uf.flags |= vis
                | if is_final { flags::FINAL } else { 0 }
                | if is_abstract { flags::ABSTRACT } else { 0 };
            // Backing store usage: `$this->prop` inside the hook body.
            if has_body && uses_backing(&uf.code, prop) {
                is_virtual = false;
            }
            let uf = Rc::new(uf);
            if is_get {
                out.get = Some(uf);
                out.final_get = is_final;
                out.abstract_get = is_abstract;
            } else {
                out.set = Some(uf);
                out.final_set = is_final;
                out.abstract_set = is_abstract;
            }
        }
        Ok((out, is_virtual))
    }
}

fn uses_backing(code: &crate::opcode::OpArray, prop: &[u8]) -> bool {
    code.places.iter().any(|p| {
        matches!(p.base, crate::opcode::PlaceBase::This)
            && matches!(p.segs.first(), Some(crate::opcode::Seg::Prop(crate::opcode::Operand::Lit(i)))
                if matches!(&code.literals[*i as usize], php_runtime::Value::Str(s) if s.as_bytes() == prop))
    })
}

impl Compiler {
    /// `zend_check_magic_method_implementation`.
    #[allow(clippy::too_many_lines)]
    fn check_magic_method(
        &mut self,
        cname: &str,
        name: &str,
        uf: &UserFunction,
        line: u32,
    ) -> CResult<()> {
        let lc = name.to_ascii_lowercase();
        if !lc.starts_with("__") {
            return Ok(());
        }
        let nargs = uf.params.iter().filter(|p| !p.variadic).count();
        let is_static = uf.flags & flags::STATIC != 0;
        let has_ret = uf.return_type.is_some();
        let args = |c: &Compiler, n: usize| -> CResult<()> {
            if nargs != n {
                let msg = match n {
                    0 => format!("Method {cname}::{name}() cannot take arguments"),
                    1 => format!("Method {cname}::{name}() must take exactly 1 argument"),
                    _ => format!("Method {cname}::{name}() must take exactly {n} arguments"),
                };
                return c.err(msg, line);
            }
            if uf.params.iter().take(n).any(|p| p.by_ref) {
                return c.err(
                    format!("Method {cname}::{name}() cannot take arguments by reference"),
                    line,
                );
            }
            Ok(())
        };
        let non_static = |c: &Compiler| -> CResult<()> {
            if is_static {
                return c.err(format!("Method {cname}::{name}() cannot be static"), line);
            }
            Ok(())
        };
        let must_static = |c: &Compiler| -> CResult<()> {
            if !is_static {
                return c.err(format!("Method {cname}::{name}() must be static"), line);
            }
            Ok(())
        };
        let arg_type = |c: &Compiler, i: usize, m: u32, tn: &str| -> CResult<()> {
            if let Some(p) = uf.params.get(i) {
                if let Some(t) = &p.ty {
                    if t.mask & m == 0 && !t.is_mixed() {
                        return c.err(
                            format!("{cname}::{name}(): Parameter #{} (${}) must be of type {tn} when declared", i + 1, p.name),
                            line,
                        );
                    }
                }
            }
            Ok(())
        };
        let ret_type = |c: &Compiler, m: u32, tn: &str| -> CResult<()> {
            let Some(t) = &uf.return_type else {
                return Ok(());
            };
            if t.mask & crate::types::mask::NEVER != 0 {
                return Ok(());
            }
            let mut extra = t.mask & !m & !crate::types::mask::ITERABLE;
            if t.mask & crate::types::mask::ITERABLE != 0 && m & crate::types::mask::ARRAY == 0 {
                extra |= crate::types::mask::ARRAY;
            }
            let complex = t.has_classes() || extra & crate::types::mask::STATIC != 0;
            extra &= !crate::types::mask::STATIC;
            if extra != 0 || (complex && m != crate::types::mask::OBJECT) {
                return c.err(
                    format!("{cname}::{name}(): Return type must be {tn} when declared"),
                    line,
                );
            }
            Ok(())
        };
        let public = |c: &mut Compiler| {
            if uf.flags & flags::PUBLIC == 0 {
                c.warnings.push((
                    2,
                    format!("The magic method {cname}::{name}() must have public visibility"),
                    line,
                ));
            }
        };
        let no_ret = |c: &Compiler| -> CResult<()> {
            if has_ret {
                return c.err(
                    format!("Method {cname}::{name}() cannot declare a return type"),
                    line,
                );
            }
            Ok(())
        };
        use crate::types::mask as m;
        match lc.as_str() {
            "__construct" => {
                non_static(self)?;
                no_ret(self)?;
            }
            "__destruct" => {
                args(self, 0)?;
                non_static(self)?;
                no_ret(self)?;
            }
            "__clone" => {
                args(self, 0)?;
                non_static(self)?;
                ret_type(self, m::VOID, "void")?;
            }
            "__get" => {
                args(self, 1)?;
                non_static(self)?;
                public(self);
                arg_type(self, 0, m::STRING, "string")?;
            }
            "__set" => {
                args(self, 2)?;
                non_static(self)?;
                public(self);
                arg_type(self, 0, m::STRING, "string")?;
                ret_type(self, m::VOID, "void")?;
            }
            "__unset" => {
                args(self, 1)?;
                non_static(self)?;
                public(self);
                arg_type(self, 0, m::STRING, "string")?;
                ret_type(self, m::VOID, "void")?;
            }
            "__isset" => {
                args(self, 1)?;
                non_static(self)?;
                public(self);
                arg_type(self, 0, m::STRING, "string")?;
                ret_type(self, m::BOOL, "bool")?;
            }
            "__call" | "__callstatic" => {
                args(self, 2)?;
                if lc == "__call" {
                    non_static(self)?;
                } else {
                    must_static(self)?;
                }
                public(self);
                arg_type(self, 0, m::STRING, "string")?;
                arg_type(self, 1, m::ARRAY, "array")?;
            }
            "__tostring" => {
                args(self, 0)?;
                non_static(self)?;
                public(self);
                ret_type(self, m::STRING, "string")?;
            }
            "__debuginfo" => {
                args(self, 0)?;
                non_static(self)?;
                public(self);
                ret_type(self, m::ARRAY | m::NULL, "?array")?;
            }
            "__serialize" | "__sleep" => {
                args(self, 0)?;
                non_static(self)?;
                public(self);
                ret_type(self, m::ARRAY, "array")?;
            }
            "__unserialize" => {
                args(self, 1)?;
                non_static(self)?;
                public(self);
                arg_type(self, 0, m::ARRAY, "array")?;
                ret_type(self, m::VOID, "void")?;
            }
            "__set_state" => {
                args(self, 1)?;
                must_static(self)?;
                public(self);
                arg_type(self, 0, m::ARRAY, "array")?;
                ret_type(self, m::OBJECT, "object")?;
            }
            "__invoke" => {
                non_static(self)?;
                public(self);
            }
            "__wakeup" => {
                args(self, 0)?;
                non_static(self)?;
                public(self);
                ret_type(self, m::VOID, "void")?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// `zend_declare_typed_property` set-visibility normalization.
fn normalize_set_visibility(
    c: &Compiler,
    fl: u32,
    has_type: bool,
    cname: &str,
    pname: &str,
    line: u32,
) -> CResult<u32> {
    let mut fl = fl;
    if fl & flags::PUBLIC != 0 && fl & flags::READONLY != 0 && fl & flags::SET_MASK == 0 {
        fl |= flags::SET_PROTECTED;
    } else if fl & flags::SET_MASK != 0 {
        if !has_type {
            return c.err(
                format!("Property with asymmetric visibility {cname}::${pname} must have type"),
                line,
            );
        }
        let rank = |v: u32| {
            if v & (flags::PUBLIC | flags::SET_PUBLIC) != 0 {
                0
            } else if v & (flags::PROTECTED | flags::SET_PROTECTED) != 0 {
                1
            } else {
                2
            }
        };
        if rank(fl & flags::VISIBILITY) > rank(fl & flags::SET_MASK) {
            return c.err(
                format!("Visibility of property {cname}::${pname} must not be weaker than set visibility"),
                line,
            );
        }
        if (fl & flags::PUBLIC != 0 && fl & flags::SET_PUBLIC != 0)
            || (fl & flags::PROTECTED != 0 && fl & flags::SET_PROTECTED != 0)
            || (fl & flags::PRIVATE != 0 && fl & flags::SET_PRIVATE != 0)
        {
            fl &= !flags::SET_MASK;
        }
        if fl & flags::SET_PRIVATE != 0 {
            fl |= flags::FINAL;
        }
    }
    Ok(fl)
}

enum BodyKind<'a> {
    Stmts(&'a [Stmt]),
    Arrow(&'a Expr),
    None,
}

fn is_null_literal(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Const(n) if n.kind != ast::NameKind::Qualified && n.name.eq_ignore_ascii_case(b"null"))
}

fn is_superglobal_or_globals(n: &[u8]) -> bool {
    super::expr::is_superglobal(n) || n == b"GLOBALS"
}

fn is_builtin_type(lc: &[u8]) -> bool {
    matches!(
        lc,
        b"int"
            | b"float"
            | b"string"
            | b"bool"
            | b"false"
            | b"true"
            | b"null"
            | b"void"
            | b"never"
            | b"mixed"
            | b"object"
            | b"iterable"
            | b"array"
            | b"callable"
            | b"static"
    )
}

fn is_reserved_class_name(lc: &[u8]) -> bool {
    matches!(
        lc,
        b"self"
            | b"parent"
            | b"static"
            | b"int"
            | b"float"
            | b"bool"
            | b"string"
            | b"true"
            | b"false"
            | b"null"
            | b"void"
            | b"never"
            | b"iterable"
            | b"object"
            | b"mixed"
            | b"array"
            | b"callable"
    )
}

#[allow(dead_code)]
fn unused(_: StmtKind) {}
