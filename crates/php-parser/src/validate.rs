//! Compile-time checks that the Zend compiler (`Zend/zend_compile.c`)
//! performs on the AST before generating code, reported with the same
//! messages and line numbers (`Fatal error: ...`).
//!
//! Only checks that need no symbol tables are done here: write contexts,
//! destructuring, argument order, `break`/`continue` depth, parameter
//! lists, closure `use` lists, `declare`/`namespace` placement, class
//! member shapes, and the like. Inheritance and type compatibility checks
//! are left to the compiler proper.

use crate::ast::{
    ArgKind, ArgList, ArrayItem, ArraySyntax, BinaryOp, CastKind, ClassDecl, ClassKind,
    ClassMemberKind, ClassRef, Diagnostic, DiagnosticLevel, Expr, ExprKind, InterpPart,
    InterpSyntax, MemberName, Modifiers, Name, NameKind, NewClass, Param, Program, Stmt, StmtKind,
    TraitAdaptation, Type, TypeKind, VarName,
};
use crate::error::ParseError;
use std::collections::HashSet;

/// Runs the compile-time checks on a parsed program. Returns the first
/// error in compilation order; compile warnings are appended to
/// `program.warnings`.
pub fn validate(program: &mut Program) -> Result<(), ParseError> {
    validate_limited(program, None).map_err(|(e, _)| e)
}

/// Like [`validate`], stopping when the native stack falls below
/// `stack_limit`; the flag of the error tells whether that happened.
pub(crate) fn validate_limited(
    program: &mut Program,
    stack_limit: Option<usize>,
) -> Result<(), (ParseError, bool)> {
    let mut v = Validator {
        foreach_ref_keys: program.foreach_ref_keys.iter().copied().collect(),
        stack_limit,
        ..Validator::default()
    };
    let r = v.top_level(&program.statements);
    if r.is_ok() {
        for i in v.compiler_dups.iter().rev() {
            v.warnings.remove(*i);
        }
    }
    program.warnings.append(&mut v.warnings);
    r.map_err(|e| (e, v.stack_exhausted))
}

type VResult = Result<(), ParseError>;

fn err(msg: impl Into<String>, line: u32) -> VResult {
    Err(ParseError::compile(msg, line))
}

/// Fetch context of `zend_compile_var` (`BP_VAR_*`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Read,
    Write,
    ReadWrite,
    Isset,
    Unset,
    /// `BP_VAR_FUNC_ARG`: write or read, decided at runtime.
    FuncArg,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FnKind {
    /// File-level code.
    Main,
    Function,
    Method,
    Closure,
}

struct FnCtx {
    kind: FnKind,
    /// Returns by reference.
    by_ref: bool,
    /// Loop stack: `true` for `switch`.
    loops: Vec<bool>,
    statics: HashSet<Vec<u8>>,
    /// Lower-cased method name (methods only).
    method: Option<Vec<u8>>,
    /// Nesting depth of `finally` blocks being compiled.
    in_finally: u32,
}

struct ClassCtx {
    kind: ClassKind,
    has_parent: bool,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // mirrors the compiler's file context
struct Validator {
    fns: Vec<FnCtx>,
    classes: Vec<ClassCtx>,
    namespace: Option<Vec<u8>>,
    has_bracketed_namespaces: bool,
    has_unbracketed_namespace: bool,
    in_namespace: bool,
    /// Inside a constant expression (defaults, constant values): class
    /// references are resolved at runtime.
    in_const_expr: bool,
    /// Statement starts of `foreach` loops with a by-reference key.
    foreach_ref_keys: HashSet<usize>,
    /// Native stack address below which recursion must stop.
    stack_limit: Option<usize>,
    /// Set when the check stopped because the native stack ran low.
    stack_exhausted: bool,
    /// `CG(zend_lineno)`: line of the node compiled last.
    cur_line: u32,
    /// Inside a property hook: (property name, hook name).
    hook: Option<(Vec<u8>, Vec<u8>)>,
    warnings: Vec<Diagnostic>,
    /// Indexes into `warnings` of diagnostics the compiler proper also
    /// raises (class-name deprecations of `zend_assert_valid_class_name`).
    /// They are only kept when validation fails, so that a later
    /// compile error is still preceded by them, as in compilation order.
    compiler_dups: Vec<usize>,
}

fn is_this(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Var(VarName::Name(id)) if id.name == b"this")
}

fn is_globals(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Var(VarName::Name(id)) if id.name == b"GLOBALS")
}

/// `$GLOBALS[]`: `is_global_var_fetch` without a dimension.
fn is_globals_append(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Dim { base, index: None } if is_globals(base))
}

const AUTO_GLOBALS: &[&[u8]] = &[
    b"GLOBALS",
    b"_GET",
    b"_POST",
    b"_COOKIE",
    b"_SERVER",
    b"_ENV",
    b"_REQUEST",
    b"_FILES",
    b"_SESSION",
];

/// `zend_ast_is_short_circuited`
fn is_short_circuited(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Dim { base, .. } => is_short_circuited(base),
        ExprKind::Prop {
            object, nullsafe, ..
        }
        | ExprKind::MethodCall {
            object, nullsafe, ..
        } => *nullsafe || is_short_circuited(object),
        ExprKind::StaticProp { class, .. } | ExprKind::StaticCall { class, .. } => {
            match class.as_ref() {
                ClassRef::Expr(x) => is_short_circuited(x),
                ClassRef::Name(_) => false,
            }
        }
        _ => false,
    }
}

/// `zend_is_variable`
fn is_variable(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Var(_)
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. }
    ) || is_call(e)
}

/// `zend_is_call`
fn is_call(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::Pipe { .. }
    )
}

/// Whether the base of a `[...]`/`->` chain is a constant
/// (`zend_mark_const_object_fetch`: such chains are fetched for reading).
fn const_based(e: &Expr) -> (bool, bool) {
    let mut e = e;
    let mut via_prop = false;
    loop {
        match &e.kind {
            ExprKind::Dim { base, .. } => e = base,
            ExprKind::Prop {
                object,
                nullsafe: false,
                ..
            } => {
                via_prop = true;
                e = object;
            }
            _ => break,
        }
    }
    (
        matches!(e.kind, ExprKind::Const(_) | ExprKind::ClassConst { .. }),
        via_prop,
    )
}

/// `zend_can_write_to_variable`
fn can_write_to_variable(e: &Expr) -> bool {
    let (is_const, via_prop) = const_based(e);
    if is_const && via_prop {
        return true;
    }
    let mut base = e;
    while let ExprKind::Dim { base: b, .. }
    | ExprKind::Prop {
        object: b,
        nullsafe: false,
        ..
    } = &base.kind
    {
        base = b;
    }
    is_variable(base) && !is_short_circuited(base)
}

/// The class-fetch type of a name: `self`, `parent` or `static`.
fn special_class(n: &Name) -> Option<&'static str> {
    if n.kind != NameKind::Unqualified {
        return None;
    }
    ["self", "parent", "static"]
        .into_iter()
        .find(|s| n.name.eq_ignore_ascii_case(s.as_bytes()))
}

const RESERVED_CLASS_NAMES: &[&str] = &[
    "null", "true", "false", "int", "float", "string", "bool", "void", "never", "iterable",
    "object", "mixed", "array", "callable", "static", "self", "parent",
];

impl Validator {
    fn func(&mut self) -> &mut FnCtx {
        if self.fns.is_empty() {
            self.fns.push(FnCtx {
                kind: FnKind::Main,
                by_ref: false,
                loops: Vec::new(),
                statics: HashSet::new(),
                method: None,
                in_finally: 0,
            });
        }
        let i = self.fns.len() - 1;
        &mut self.fns[i]
    }

    fn warn(&mut self, level: DiagnosticLevel, msg: impl Into<String>, line: u32) {
        self.warnings.push(Diagnostic {
            level,
            message: msg.into(),
            line,
        });
    }

    fn class_display_name(&self, c: &ClassDecl) -> Vec<u8> {
        match &c.name {
            None => b"class@anonymous".to_vec(),
            Some(n) => match &self.namespace {
                Some(ns) if !ns.is_empty() => {
                    let mut v = ns.clone();
                    v.push(b'\\');
                    v.extend_from_slice(&n.name);
                    v
                }
                _ => n.name.clone(),
            },
        }
    }

    /// Stops before the native stack overflows.
    fn check_stack(&mut self, line: u32) -> VResult {
        if crate::stack::exhausted(self.stack_limit) {
            self.stack_exhausted = true;
            return Err(ParseError::parse("memory exhausted", line));
        }
        Ok(())
    }

    // ---- top level -------------------------------------------------------

    fn top_level(&mut self, stmts: &[Stmt]) -> VResult {
        self.fns.push(FnCtx {
            kind: FnKind::Main,
            by_ref: false,
            loops: Vec::new(),
            statics: HashSet::new(),
            method: None,
            in_finally: 0,
        });
        for (i, s) in stmts.iter().enumerate() {
            self.top_stmt(s, stmts, i)?;
        }
        Ok(())
    }

    /// `zend_is_first_statement`
    fn is_first_statement(stmts: &[Stmt], idx: usize, allow_nop: bool) -> bool {
        stmts[..idx].iter().all(|s| match s.kind {
            StmtKind::Nop => allow_nop,
            StmtKind::Declare { .. } => true,
            _ => false,
        })
    }

    fn top_stmt(&mut self, s: &Stmt, file: &[Stmt], idx: usize) -> VResult {
        match &s.kind {
            StmtKind::Nop | StmtKind::HaltCompiler { .. } => return Ok(()),
            StmtKind::Namespace { name, body } => {
                let with_bracket = body.is_some();
                if self.has_bracketed_namespaces {
                    if !with_bracket {
                        return err(
                            "Cannot mix bracketed namespace declarations with unbracketed namespace declarations",
                            s.span.line,
                        );
                    }
                } else if self.has_unbracketed_namespace && with_bracket {
                    return err(
                        "Cannot mix bracketed namespace declarations with unbracketed namespace declarations",
                        s.span.line,
                    );
                }
                if self.has_bracketed_namespaces
                    && with_bracket
                    && (self.in_namespace || self.namespace.is_some())
                {
                    return err("Namespace declarations cannot be nested", s.span.line);
                }
                let is_first_namespace = (!with_bracket && !self.has_unbracketed_namespace)
                    || (with_bracket && !self.has_bracketed_namespaces);
                if is_first_namespace && !Self::is_first_statement(file, idx, true) {
                    return err(
                        "Namespace declaration statement has to be the very first statement or after any declare call in the script",
                        s.span.line,
                    );
                }
                if let Some(n) = name {
                    if n.name.eq_ignore_ascii_case(b"namespace") {
                        return err(
                            format!(
                                "Cannot use '{}' as namespace name",
                                String::from_utf8_lossy(&n.name)
                            ),
                            s.span.line,
                        );
                    }
                }
                self.namespace = name.as_ref().map(|n| n.name.clone());
                self.in_namespace = true;
                if let Some(body) = body {
                    self.has_bracketed_namespaces = true;
                    for (i, st) in body.iter().enumerate() {
                        self.top_stmt(st, body, i)?;
                    }
                    self.in_namespace = false;
                    self.namespace = None;
                } else {
                    self.has_unbracketed_namespace = true;
                }
                return Ok(());
            }
            StmtKind::Declare { .. } => self.declare(s, Some((file, idx)))?,
            _ => self.stmt(s)?,
        }
        if self.has_bracketed_namespaces && !self.in_namespace {
            return err("No code may exist outside of namespace {}", s.span.line);
        }
        Ok(())
    }

    fn declare(&mut self, s: &Stmt, top: Option<(&[Stmt], usize)>) -> VResult {
        let StmtKind::Declare { directives, body } = &s.kind else {
            return Ok(());
        };
        for d in directives {
            let line = s.span.line;
            if !matches!(
                d.value.kind,
                ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
            ) {
                return err(
                    format!(
                        "declare({}) value must be a literal",
                        String::from_utf8_lossy(&d.name.name)
                    ),
                    line,
                );
            }
            let first = |allow_nop: bool| {
                top.is_some_and(|(file, idx)| Self::is_first_statement(file, idx, allow_nop))
            };
            if d.name.eq_ignore_case("ticks") {
            } else if d.name.eq_ignore_case("encoding") {
                if !first(false) {
                    return err(
                        "Encoding declaration pragma must be the very first statement in the script",
                        line,
                    );
                }
            } else if d.name.eq_ignore_case("strict_types") {
                if !first(true) {
                    return err(
                        "strict_types declaration must be the very first statement in the script",
                        line,
                    );
                }
                if body.is_some() {
                    return err("strict_types declaration must not use block mode", line);
                }
                if !matches!(d.value.kind, ExprKind::Int(0 | 1)) {
                    return err(
                        "strict_types declaration must have 0 or 1 as its value",
                        line,
                    );
                }
            } else {
                self.warn(
                    DiagnosticLevel::CompileWarning,
                    format!(
                        "Unsupported declare '{}'",
                        String::from_utf8_lossy(&d.name.name)
                    ),
                    line,
                );
            }
        }
        if let Some(b) = body {
            self.stmt(b)?;
        }
        Ok(())
    }

    // ---- statements ------------------------------------------------------

    fn stmts(&mut self, stmts: &[Stmt]) -> VResult {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn loop_body(&mut self, body: &Stmt, is_switch: bool) -> VResult {
        self.func().loops.push(is_switch);
        let r = self.stmt(body);
        self.func().loops.pop();
        r
    }

    #[allow(clippy::too_many_lines)]
    fn stmt(&mut self, s: &Stmt) -> VResult {
        self.check_stack(s.span.line)?;
        let line = s.span.line;
        match &s.kind {
            StmtKind::Block(b) => self.stmts(b),
            StmtKind::If {
                branches,
                else_branch,
            } => {
                for b in branches {
                    self.expr(&b.cond)?;
                    self.stmt(&b.body)?;
                }
                if let Some(e) = else_branch {
                    self.stmt(e)?;
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                self.expr(cond)?;
                self.loop_body(body, false)
            }
            StmtKind::DoWhile { body, cond } => {
                self.loop_body(body, false)?;
                self.expr(cond)
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                for e in init.iter().chain(cond) {
                    self.expr(e)?;
                }
                self.loop_body(body, false)?;
                for e in step {
                    self.expr(e)?;
                }
                Ok(())
            }
            StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body,
            } => {
                if let Some(k) = key {
                    // CG(zend_lineno) is the statement's line: its first child.
                    let line = subject.span.line;
                    if self.foreach_ref_keys.contains(&s.span.start) {
                        return err("Key element cannot be a reference", line);
                    }
                    if matches!(k.kind, ExprKind::Array { .. }) {
                        return err("Cannot use list as key element", line);
                    }
                }
                if *by_ref && is_variable(subject) && can_write_to_variable(subject) {
                    self.var(subject, Mode::Write)?;
                } else {
                    self.expr(subject)?;
                }
                self.foreach_target(value, line)?;
                if let Some(k) = key {
                    self.foreach_target(k, line)?;
                }
                self.loop_body(body, false)
            }
            StmtKind::Switch { subject, cases } => {
                self.expr(subject)?;
                self.func().loops.push(true);
                let r = (|| {
                    for c in cases {
                        if c.semicolon {
                            self.warn(
                                DiagnosticLevel::Deprecated,
                                "Case statements followed by a semicolon (;) are deprecated, use a colon (:) instead",
                                c.cond.as_ref().map_or(c.span.line, |e| e.span.line),
                            );
                        }
                        if let Some(e) = &c.cond {
                            self.expr(e)?;
                        }
                        self.stmts(&c.body)?;
                    }
                    Ok(())
                })();
                self.func().loops.pop();
                r
            }
            StmtKind::Break(depth) | StmtKind::Continue(depth) => {
                let is_break = matches!(s.kind, StmtKind::Break(_));
                self.break_continue(is_break, depth.as_ref(), line)
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) if self.func().by_ref && is_variable(e) && !is_call(e) => {
                        self.var(e, Mode::Write)?;
                    }
                    Some(e) => self.expr(e)?,
                    None => {}
                }
                let line = if e.is_some() { self.cur_line } else { line };
                if e.is_some() && !self.classes.is_empty() {
                    match self.func().method.as_deref() {
                        Some(b"__construct") => self.warn(
                            DiagnosticLevel::Deprecated,
                            "Returning a value from a constructor is deprecated",
                            line,
                        ),
                        Some(b"__destruct") => self.warn(
                            DiagnosticLevel::Deprecated,
                            "Returning a value from a destructor is deprecated",
                            line,
                        ),
                        _ => {}
                    }
                }
                if self.func().in_finally > 0 {
                    self.warn(
                        DiagnosticLevel::Deprecated,
                        "Returning from a finally block is deprecated",
                        line,
                    );
                }
                Ok(())
            }
            StmtKind::Global(vars) => {
                for v in vars {
                    if is_this(v) {
                        return err("Cannot use $this as global variable", v.span.line);
                    }
                    if let ExprKind::Var(VarName::Expr(x)) = &v.kind {
                        self.expr(x)?;
                    }
                }
                Ok(())
            }
            StmtKind::Static(vars) => {
                for v in vars {
                    if v.name.name == b"this" {
                        return err("Cannot use $this as static variable", v.span.line);
                    }
                    if let Some(d) = &v.default {
                        self.expr(d)?;
                    }
                    if !self.func().statics.insert(v.name.name.clone()) {
                        return err(
                            format!(
                                "Duplicate declaration of static variable ${}",
                                String::from_utf8_lossy(&v.name.name)
                            ),
                            v.span.line,
                        );
                    }
                }
                Ok(())
            }
            StmtKind::Echo(es) => {
                for e in es {
                    self.expr(e)?;
                }
                Ok(())
            }
            StmtKind::Unset(vars) => {
                for v in vars {
                    self.unset(v)?;
                }
                Ok(())
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Declare { .. } => self.declare(s, None),
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                if catches.is_empty() && finally.is_none() {
                    return err("Cannot use try without catch or finally", line);
                }
                self.stmts(body)?;
                for c in catches {
                    if let Some(v) = &c.var {
                        if v.name == b"this" {
                            return err("Cannot re-assign $this", c.span.line);
                        }
                    }
                    self.stmts(&c.body)?;
                }
                if let Some(f) = finally {
                    self.func().in_finally += 1;
                    let r = self.stmts(f);
                    self.func().in_finally -= 1;
                    r?;
                }
                Ok(())
            }
            StmtKind::Function(f) => {
                self.fns.push(FnCtx {
                    kind: FnKind::Function,
                    by_ref: f.by_ref,
                    loops: Vec::new(),
                    statics: HashSet::new(),
                    method: None,
                    in_finally: 0,
                });
                let r = (|| {
                    self.params(&f.params, None)?;
                    if let Some(t) = &f.return_type {
                        self.ty(t)?;
                    }
                    self.stmts(&f.body)
                })();
                self.fns.pop();
                r
            }
            StmtKind::Class(c) => self.class(c),
            StmtKind::Const { consts, .. } => {
                for c in consts {
                    self.const_expr(&c.value)?;
                }
                Ok(())
            }
            StmtKind::Namespace { .. }
            | StmtKind::InlineHtml(_)
            | StmtKind::Nop
            | StmtKind::Goto(_)
            | StmtKind::Label(_)
            | StmtKind::HaltCompiler { .. }
            | StmtKind::Use { .. }
            | StmtKind::GroupUse { .. } => Ok(()),
        }
    }

    fn foreach_target(&mut self, target: &Expr, line: u32) -> VResult {
        if is_this(target) {
            return err("Cannot re-assign $this", line);
        }
        if let ExprKind::Array { items, syntax } = &target.kind {
            return self.list_assign(items, *syntax, target.span.line);
        }
        self.ensure_writable(target)?;
        self.var(target, Mode::Write)
    }

    fn break_continue(&mut self, is_break: bool, depth: Option<&Expr>, line: u32) -> VResult {
        let name = if is_break { "break" } else { "continue" };
        let depth = match depth {
            None => 1,
            Some(d) => match d.kind {
                ExprKind::Int(n) if n >= 1 => n,
                ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_) => {
                    return err(
                        format!("'{name}' operator accepts only positive integers"),
                        line,
                    )
                }
                _ => {
                    return err(
                        format!(
                            "'{name}' operator with non-integer operand is no longer supported"
                        ),
                        line,
                    )
                }
            },
        };
        let loops = self.func().loops.clone();
        if loops.is_empty() {
            return err(
                format!("'{name}' not in the 'loop' or 'switch' context"),
                line,
            );
        }
        let Ok(d) = usize::try_from(depth) else {
            return err(format!("Cannot '{name}' {depth} levels"), line);
        };
        if d > loops.len() {
            return err(
                format!(
                    "Cannot '{name}' {depth} level{}",
                    if depth == 1 { "" } else { "s" }
                ),
                line,
            );
        }
        if !is_break {
            let target = loops.len() - d;
            if loops[target] {
                let outermost = target == 0;
                let msg = match (depth == 1, outermost) {
                    (true, true) => "\"continue\" targeting switch is equivalent to \"break\"".to_owned(),
                    (true, false) => format!(
                        "\"continue\" targeting switch is equivalent to \"break\". Did you mean to use \"continue {}\"?",
                        depth + 1
                    ),
                    (false, true) => format!(
                        "\"continue {depth}\" targeting switch is equivalent to \"break {depth}\""
                    ),
                    (false, false) => format!(
                        "\"continue {depth}\" targeting switch is equivalent to \"break {depth}\". Did you mean to use \"continue {}\"?",
                        depth + 1
                    ),
                };
                self.warn(DiagnosticLevel::Warning, msg, line);
            }
        }
        Ok(())
    }

    fn unset(&mut self, v: &Expr) -> VResult {
        self.ensure_writable(v)?;
        if is_this(v) {
            return err("Cannot unset $this", v.span.line);
        }
        // `zend_compile_unset` tests `is_global_var_fetch` before it compiles
        // the variable, so a direct `unset($GLOBALS[])` keeps the `[]` message.
        if is_globals_append(v) {
            return err("Cannot use [] for unsetting", v.span.line);
        }
        self.var(v, Mode::Unset)
    }

    // ---- functions -------------------------------------------------------

    /// Parameter list checks (`zend_compile_params`). `ctor` describes the
    /// method when compiling a method: (is constructor, is abstract or in
    /// an interface).
    fn params(&mut self, params: &[Param], ctor: Option<(bool, bool)>) -> VResult {
        let mut seen: HashSet<&[u8]> = HashSet::new();
        let mut variadic = false;
        for p in params {
            let line = p.span.line;
            if AUTO_GLOBALS.contains(&p.name.name.as_slice()) {
                return err(
                    format!(
                        "Cannot re-assign auto-global variable {}",
                        String::from_utf8_lossy(&p.name.name)
                    ),
                    line,
                );
            }
            if !seen.insert(&p.name.name) {
                return err(
                    format!(
                        "Redefinition of parameter ${}",
                        String::from_utf8_lossy(&p.name.name)
                    ),
                    line,
                );
            }
            if p.name.name == b"this" {
                return err("Cannot use $this as parameter", line);
            }
            if variadic {
                return err("Only the last parameter can be variadic", line);
            }
            if p.variadic {
                variadic = true;
                if p.default.is_some() {
                    return err("Variadic parameter cannot have a default value", line);
                }
            } else if let Some(d) = &p.default {
                self.const_expr(d)?;
            }
            if let Some(t) = &p.ty {
                self.ty(t)?;
            }
            let promoted = !p.modifiers.is_empty() || p.hooks.is_some();
            if promoted {
                match ctor {
                    Some((true, abstract_)) => {
                        if abstract_ {
                            return err(
                                "Cannot declare promoted property in an abstract constructor",
                                line,
                            );
                        }
                        if p.variadic {
                            return err("Cannot declare variadic promoted property", line);
                        }
                    }
                    _ => {
                        return err(
                            "Cannot declare promoted property outside a constructor",
                            line,
                        )
                    }
                }
            }
        }
        Ok(())
    }

    /// Type checks that need no class information.
    #[allow(clippy::unused_self)] // kept a method like the other checks
    fn ty(&mut self, t: &Type) -> VResult {
        match &t.kind {
            TypeKind::Nullable(inner) => {
                if let TypeKind::Named(n) = &inner.kind {
                    if n.is_special("mixed") {
                        return err(
                            "Type mixed cannot be marked as nullable since mixed already includes null",
                            t.span.line,
                        );
                    }
                    if n.is_special("null") {
                        return err("null cannot be marked as nullable", t.span.line);
                    }
                    if n.is_special("void") {
                        return err("Void can only be used as a standalone type", t.span.line);
                    }
                }
                Ok(())
            }
            TypeKind::Union(ts) => {
                for x in ts {
                    if let TypeKind::Named(n) = &x.kind {
                        if n.is_special("void") {
                            return err("Void can only be used as a standalone type", t.span.line);
                        }
                        if n.is_special("mixed") {
                            return err(
                                "Type mixed can only be used as a standalone type",
                                t.span.line,
                            );
                        }
                        if n.is_special("never") {
                            return err("never can only be used as a standalone type", t.span.line);
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn closure_like(
        &mut self,
        params: &[Param],
        by_ref: bool,
        f: impl FnOnce(&mut Self) -> VResult,
    ) -> VResult {
        self.fns.push(FnCtx {
            kind: FnKind::Closure,
            by_ref,
            loops: Vec::new(),
            statics: HashSet::new(),
            method: None,
            in_finally: 0,
        });
        let r = self.params(params, None).and_then(|()| f(self));
        self.fns.pop();
        r
    }

    // ---- classes ---------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn class(&mut self, c: &ClassDecl) -> VResult {
        let line = c.span.line;
        if let Some(n) = &c.name {
            if !self.classes.is_empty() {
                return err("Class declarations may not be nested", line);
            }
            let ty = match c.kind {
                ClassKind::Enum => "an enum name",
                ClassKind::Interface => "an interface name",
                ClassKind::Trait => "a trait name",
                ClassKind::Class => "a class name",
            };
            if RESERVED_CLASS_NAMES
                .iter()
                .any(|r| n.name.eq_ignore_ascii_case(r.as_bytes()))
            {
                return err(
                    format!(
                        "Cannot use \"{}\" as {ty} as it is reserved",
                        String::from_utf8_lossy(&n.name)
                    ),
                    line,
                );
            }
            // `zend_assert_valid_class_name()` identifier deprecations; the
            // compiler raises them again when validation succeeds.
            let dep = if n.name.as_slice() == b"_" {
                Some("Using \"_\" as {ty} is deprecated since 8.4")
            } else if n.name.eq_ignore_ascii_case(b"let") {
                Some("Using \"let\" as {ty} is deprecated since 8.6")
            } else if n.name.eq_ignore_ascii_case(b"is") {
                Some("Using \"is\" as {ty} is deprecated since 8.6")
            } else {
                None
            };
            if let Some(dep) = dep {
                self.compiler_dups.push(self.warnings.len());
                self.warn(DiagnosticLevel::Deprecated, dep.replace("{ty}", ty), line);
            }
        }
        let class_name = self.class_display_name(c);
        self.classes.push(ClassCtx {
            kind: c.kind,
            has_parent: c.extends.is_some(),
        });
        let saved_fns = std::mem::take(&mut self.fns);
        self.fns.push(FnCtx {
            kind: FnKind::Main,
            by_ref: false,
            loops: Vec::new(),
            statics: HashSet::new(),
            method: None,
            in_finally: 0,
        });
        let r = self.class_members(c, &class_name);
        self.fns = saved_fns;
        self.classes.pop();
        r
    }

    #[allow(clippy::too_many_lines)]
    fn class_members(&mut self, c: &ClassDecl, class_name: &[u8]) -> VResult {
        let cname = String::from_utf8_lossy(class_name).into_owned();
        let mut methods: HashSet<Vec<u8>> = HashSet::new();
        let mut props: HashSet<Vec<u8>> = HashSet::new();
        let mut consts: HashSet<Vec<u8>> = HashSet::new();
        let in_interface = c.kind == ClassKind::Interface;
        for m in &c.members {
            let line = m.span.line;
            match &m.kind {
                ClassMemberKind::Property(g) => {
                    if c.kind == ClassKind::Enum {
                        return err(format!("Enum {cname} cannot include properties"), line);
                    }
                    if g.modifiers.contains(Modifiers::FINAL)
                        && g.modifiers.contains(Modifiers::PRIVATE)
                    {
                        return err("Property cannot be both final and private", line);
                    }
                    for p in &g.props {
                        let pline = p.span.line;
                        if !props.insert(p.name.name.clone()) {
                            return err(
                                format!(
                                    "Cannot redeclare {cname}::${}",
                                    String::from_utf8_lossy(&p.name.name)
                                ),
                                pline,
                            );
                        }
                        if let Some(d) = &p.default {
                            self.const_expr(d)?;
                        }
                        if let Some(h) = &p.hooks {
                            self.hooks(&p.name.name, h)?;
                        }
                    }
                }
                ClassMemberKind::Const(g) => {
                    for k in &g.consts {
                        if !consts.insert(k.name.name.clone()) {
                            return err(
                                format!(
                                    "Cannot redefine class constant {cname}::{}",
                                    String::from_utf8_lossy(&k.name.name)
                                ),
                                k.span.line,
                            );
                        }
                        self.const_expr(&k.value)?;
                    }
                }
                ClassMemberKind::Method(md) => {
                    let name = String::from_utf8_lossy(&md.name.name).into_owned();
                    let mut flags = md.modifiers;
                    if in_interface {
                        if flags.intersects(Modifiers::PROTECTED | Modifiers::PRIVATE) {
                            return err(
                                format!(
                                    "Access type for interface method {cname}::{name}() must be public"
                                ),
                                md.span.line,
                            );
                        }
                        if flags.contains(Modifiers::FINAL) {
                            return err(
                                format!("Interface method {cname}::{name}() must not be final"),
                                md.span.line,
                            );
                        }
                    }
                    if flags.contains(Modifiers::ABSTRACT)
                        && !c.modifiers.contains(Modifiers::ABSTRACT)
                        && c.kind != ClassKind::Trait
                    {
                        let msg = if c.name.is_none() {
                            format!("Anonymous class method {name}() must not be abstract")
                        } else if c.kind == ClassKind::Enum {
                            format!("Enum method {cname}::{name}() must not be abstract")
                        } else if in_interface {
                            format!("Interface method {cname}::{name}() must not be abstract")
                        } else {
                            format!(
                                "Class {cname} declares abstract method {name}() and must therefore be declared abstract"
                            )
                        };
                        return err(msg, md.span.line);
                    }
                    if in_interface {
                        flags |= Modifiers::ABSTRACT;
                    }
                    if flags.contains(Modifiers::PRIVATE)
                        && flags.contains(Modifiers::FINAL)
                        && !md.name.eq_ignore_case("__construct")
                    {
                        self.warn(
                            DiagnosticLevel::CompileWarning,
                            "Private methods cannot be final as they are never overridden by other classes",
                            md.span.line,
                        );
                    }
                    let has_body = md.body.is_some();
                    if flags.contains(Modifiers::ABSTRACT) {
                        let what = if in_interface {
                            "Interface"
                        } else {
                            "Abstract"
                        };
                        if flags.contains(Modifiers::PRIVATE) && c.kind != ClassKind::Trait {
                            return err(
                                format!(
                                    "{what} function {cname}::{name}() cannot be declared private"
                                ),
                                md.span.line,
                            );
                        }
                        if has_body {
                            return err(
                                format!("{what} function {cname}::{name}() cannot contain body"),
                                md.span.line,
                            );
                        }
                    } else if !has_body {
                        return err(
                            format!("Non-abstract method {cname}::{name}() must contain body"),
                            md.span.line,
                        );
                    }
                    if !methods.insert(md.name.name.to_ascii_lowercase()) {
                        return err(format!("Cannot redeclare {cname}::{name}()"), md.span.line);
                    }
                    let is_ctor = md.name.eq_ignore_case("__construct");
                    let abstract_ctor = flags.contains(Modifiers::ABSTRACT);
                    self.fns.push(FnCtx {
                        kind: FnKind::Method,
                        by_ref: md.by_ref,
                        loops: Vec::new(),
                        statics: HashSet::new(),
                        method: Some(md.name.name.to_ascii_lowercase()),
                        in_finally: 0,
                    });
                    let r = (|| {
                        self.params(&md.params, Some((is_ctor, abstract_ctor)))?;
                        if is_ctor {
                            for p in &md.params {
                                if (!p.modifiers.is_empty() || p.hooks.is_some())
                                    && !props.insert(p.name.name.clone())
                                {
                                    return err(
                                        format!(
                                            "Cannot redeclare {cname}::${}",
                                            String::from_utf8_lossy(&p.name.name)
                                        ),
                                        p.span.line,
                                    );
                                }
                            }
                        }
                        for p in &md.params {
                            if let Some(h) = &p.hooks {
                                self.hooks(&p.name.name, h)?;
                            }
                        }
                        if let Some(t) = &md.return_type {
                            self.ty(t)?;
                        }
                        // `zend_compile_func_decl`, right after the params
                        // (CG(zend_lineno) is the last parameter's line).
                        if md.is_generator {
                            let msg = if is_ctor {
                                Some("Making a constructor a Generator is deprecated")
                            } else if md.name.eq_ignore_case("__destruct") {
                                Some("Making a destructor a Generator is deprecated")
                            } else {
                                None
                            };
                            if let Some(msg) = msg {
                                let line = md.params.last().map_or(md.span.line, |p| p.span.line);
                                self.warn(DiagnosticLevel::Deprecated, msg, line);
                            }
                        }
                        if let Some(b) = &md.body {
                            self.stmts(b)?;
                        }
                        Ok(())
                    })();
                    self.fns.pop();
                    r?;
                }
                ClassMemberKind::TraitUse(tu) => {
                    for a in &tu.adaptations {
                        if let TraitAdaptation::Alias { modifiers, .. } = a {
                            if modifiers.contains(Modifiers::STATIC) {
                                return err(
                                    "Cannot use \"static\" as method modifier in trait alias",
                                    line,
                                );
                            }
                            if modifiers.contains(Modifiers::ABSTRACT) {
                                return err(
                                    "Cannot use \"abstract\" as method modifier in trait alias",
                                    line,
                                );
                            }
                        }
                    }
                }
                ClassMemberKind::EnumCase(ec) => {
                    let case = String::from_utf8_lossy(&ec.name.name);
                    if c.kind != ClassKind::Enum {
                        return err("Case can only be used in enums", line);
                    }
                    if c.backing_type.is_some() && ec.value.is_none() {
                        return err(
                            format!("Case {case} of backed enum {cname} must have a value"),
                            line,
                        );
                    }
                    if c.backing_type.is_none() && ec.value.is_some() {
                        return err(
                            format!("Case {case} of non-backed enum {cname} must not have a value"),
                            line,
                        );
                    }
                    if let Some(v) = &ec.value {
                        self.const_expr(v)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `zend_compile_parent_property_hook_call` checks.
    fn parent_hook_call(
        &mut self,
        class: &ClassRef,
        name: &MemberName,
        args: &ArgList,
        line: u32,
    ) -> VResult {
        if self.classes.is_empty() {
            return err("Cannot use \"parent\" when no class scope is active", line);
        }
        if args.is_partial() {
            return err("Cannot create Closure for parent property hook call", line);
        }
        let (ClassRef::Expr(e), MemberName::Ident(hook)) = (class, name) else {
            return Ok(());
        };
        let ExprKind::StaticProp {
            name: VarName::Name(prop),
            ..
        } = &e.kind
        else {
            return Ok(());
        };
        let p = String::from_utf8_lossy(&prop.name).into_owned();
        let h = String::from_utf8_lossy(&hook.name).into_owned();
        match &self.hook {
            None => err(
                format!("Must not use parent::${p}::{h}() outside a property hook"),
                line,
            ),
            Some((active_prop, _)) if *active_prop != prop.name => err(
                format!(
                    "Must not use parent::${p}::{h}() in a different property (${})",
                    String::from_utf8_lossy(active_prop)
                ),
                line,
            ),
            Some((_, active_hook)) if !active_hook.eq_ignore_ascii_case(&hook.name) => err(
                format!(
                    "Must not use parent::${p}::{h}() in a different property hook ({})",
                    String::from_utf8_lossy(active_hook).to_ascii_lowercase()
                ),
                line,
            ),
            Some(_) => Ok(()),
        }
    }

    /// Property hook bodies.
    fn hooks(&mut self, prop: &[u8], hooks: &[crate::ast::PropertyHook]) -> VResult {
        for h in hooks {
            let saved = self.hook.replace((prop.to_vec(), h.name.name.clone()));
            self.fns.push(FnCtx {
                kind: FnKind::Method,
                by_ref: h.by_ref,
                loops: Vec::new(),
                statics: HashSet::new(),
                method: None,
                in_finally: 0,
            });
            let r = (|| {
                if let Some(ps) = &h.params {
                    self.params(ps, None)?;
                }
                match &h.body {
                    crate::ast::HookBody::None => Ok(()),
                    crate::ast::HookBody::Block(b) => self.stmts(b),
                    crate::ast::HookBody::Expr(e) => self.expr(e),
                }
            })();
            self.fns.pop();
            self.hook = saved;
            r?;
        }
        Ok(())
    }

    /// `zend_ensure_valid_class_fetch_type`
    fn class_fetch(&mut self, class: &ClassRef, line: u32) -> VResult {
        match class {
            ClassRef::Expr(e) => self.expr(e),
            ClassRef::Name(n) => {
                let Some(kind) = special_class(n) else {
                    return Ok(());
                };
                if self.in_const_expr {
                    return Ok(());
                }
                let scope_known = match self.fns.last().map(|f| f.kind) {
                    Some(FnKind::Closure) | None => false,
                    Some(FnKind::Main) => !self.classes.is_empty(),
                    Some(FnKind::Function | FnKind::Method) => true,
                } && self
                    .classes
                    .last()
                    .is_none_or(|c| c.kind != ClassKind::Trait);
                if !scope_known {
                    return Ok(());
                }
                match self.classes.last() {
                    None => err(
                        format!("Cannot use \"{kind}\" when no class scope is active"),
                        line,
                    ),
                    Some(c) if kind == "parent" && !c.has_parent => err(
                        "Cannot use \"parent\" when current class scope has no parent",
                        line,
                    ),
                    Some(_) => Ok(()),
                }
            }
        }
    }

    // ---- expressions -----------------------------------------------------

    /// Checks a constant expression (default values, constant values).
    fn const_expr(&mut self, e: &Expr) -> VResult {
        let saved = std::mem::replace(&mut self.in_const_expr, true);
        let r = self.expr(e);
        self.in_const_expr = saved;
        r
    }

    /// `zend_ensure_writable_variable`
    #[allow(clippy::unused_self)] // kept a method like the other checks
    fn ensure_writable(&mut self, e: &Expr) -> VResult {
        let line = e.span.line;
        if matches!(e.kind, ExprKind::Call { .. } | ExprKind::Pipe { .. }) {
            return err("Can't use function return value in write context", line);
        }
        if matches!(
            e.kind,
            ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. }
        ) {
            return err("Can't use method return value in write context", line);
        }
        if is_short_circuited(e) {
            return err("Can't use nullsafe operator in write context", line);
        }
        if is_globals(e) {
            return err(
                "$GLOBALS can only be modified using the $GLOBALS[$name] = $value syntax",
                line,
            );
        }
        Ok(())
    }

    fn args(&mut self, args: &ArgList) -> VResult {
        let mut uses_named = false;
        let mut uses_unpack = false;
        let mut uses_variadic_placeholder = false;
        for a in &args.args {
            let line = a.span.line;
            match &a.kind {
                ArgKind::Unpack(e) => {
                    if uses_named {
                        return err("Cannot use argument unpacking after named arguments", line);
                    }
                    if args.is_partial() {
                        return err("Cannot combine partial application and unpacking", line);
                    }
                    uses_unpack = true;
                    self.expr(e)?;
                }
                ArgKind::Named { value, .. } => {
                    uses_named = true;
                    self.arg_value(value)?;
                }
                ArgKind::NamedPlaceholder(_) => uses_named = true,
                ArgKind::Positional(_) | ArgKind::Placeholder | ArgKind::VariadicPlaceholder => {
                    if uses_unpack {
                        return err(
                            "Cannot use positional argument after argument unpacking",
                            line,
                        );
                    }
                    let is_vp = a.kind == ArgKind::VariadicPlaceholder;
                    if uses_named && !is_vp {
                        return err("Cannot use positional argument after named argument", line);
                    }
                    if uses_variadic_placeholder {
                        return err(
                            if is_vp {
                                "Variadic placeholder may only appear once"
                            } else {
                                "Variadic placeholder must be last"
                            },
                            line,
                        );
                    }
                    if is_vp {
                        uses_variadic_placeholder = true;
                    }
                    if let ArgKind::Positional(e) = &a.kind {
                        self.arg_value(e)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// An argument may be passed by reference: variables are compiled in
    /// `FUNC_ARG` mode (no `[]`-for-reading error).
    fn arg_value(&mut self, e: &Expr) -> VResult {
        if is_variable(e) && !is_call(e) {
            self.var(e, Mode::FuncArg)
        } else {
            self.expr(e)
        }
    }

    fn items(&mut self, items: &[Option<ArrayItem>], line: u32) -> VResult {
        let mut last_line = None;
        for it in items {
            match it {
                None => {
                    return err(
                        "Cannot use empty array elements in arrays",
                        last_line.unwrap_or(line),
                    )
                }
                Some(it) => last_line = Some(it.value.span.line),
            }
        }
        for it in items.iter().flatten() {
            if let Some(k) = &it.key {
                self.expr(k)?;
            }
            if it.by_ref {
                self.var(&it.value, Mode::Write)?;
            } else {
                self.expr(&it.value)?;
            }
        }
        Ok(())
    }

    /// `zend_compile_list_assign` (structure checks).
    fn list_assign(
        &mut self,
        items: &[Option<ArrayItem>],
        style: ArraySyntax,
        _line: u32,
    ) -> VResult {
        let keyed = items
            .iter()
            .flatten()
            .next()
            .is_some_and(|i| i.key.is_some());
        let mut has_elems = false;
        for it in items {
            let Some(it) = it else {
                if keyed {
                    return err(
                        "Cannot use empty array entries in keyed array assignment",
                        self.cur_line,
                    );
                }
                continue;
            };
            if it.unpack {
                return err(
                    "Spread operator is not supported in assignments",
                    self.cur_line,
                );
            }
            has_elems = true;
            if keyed != it.key.is_some() {
                return err(
                    "Cannot mix keyed and unkeyed array entries in assignments",
                    self.cur_line,
                );
            }
            if let Some(k) = &it.key {
                self.expr(k)?;
            }
            if let ExprKind::Array {
                items: sub,
                syntax: sub_style,
            } = &it.value.kind
            {
                if *sub_style == ArraySyntax::Long {
                    return err("Cannot assign to array(), use [] instead", self.cur_line);
                }
                if *sub_style != style {
                    return err("Cannot mix [] and list()", self.cur_line);
                }
                self.list_assign(sub, style, it.value.span.line)?;
            } else {
                if !can_write_to_variable(&it.value) {
                    return err(
                        "Assignments can only happen to writable values",
                        self.cur_line,
                    );
                }
                if is_this(&it.value) {
                    return err("Cannot re-assign $this", it.value.span.line);
                }
                self.ensure_writable(&it.value)?;
                self.var(&it.value, Mode::Write)?;
            }
        }
        if !has_elems {
            return err("Cannot use empty list", self.cur_line);
        }
        Ok(())
    }

    /// `zend_compile_var` in the given fetch mode.
    fn var(&mut self, e: &Expr, mode: Mode) -> VResult {
        let line = e.span.line;
        self.cur_line = line;
        match &e.kind {
            ExprKind::Var(VarName::Expr(x)) => self.expr(x),
            ExprKind::Var(VarName::Name(_)) => Ok(()),
            ExprKind::Dim { base, index } => {
                // `zend_delayed_compile_dim`: `$GLOBALS[]` is rejected before
                // the base is compiled and before the `[]` checks below, in
                // every fetch mode.
                if index.is_none() && is_globals(base) {
                    return err("Cannot append to $GLOBALS", line);
                }
                // The fetch type reaches the base unchanged
                // (`zend_delayed_compile_var(var_ast, type)`): `[]` inside an
                // `unset()` chain is an `[]` for unsetting, and `$a[]['k'] = 1`
                // appends.
                self.var(base, mode)?;
                match index {
                    None => match mode {
                        Mode::Read | Mode::Isset => err("Cannot use [] for reading", line),
                        Mode::Unset => err("Cannot use [] for unsetting", line),
                        Mode::Write | Mode::ReadWrite | Mode::FuncArg => Ok(()),
                    },
                    Some(i) => self.expr(i),
                }
            }
            ExprKind::Prop { object, name, .. } => {
                if is_this(object) {
                    // `$this->x`: no write check on $this itself
                } else if matches!(mode, Mode::Write | Mode::ReadWrite | Mode::Unset)
                    && is_variable(object)
                    && !const_based(object).0
                {
                    // `zend_delayed_compile_prop` fetches the object with the
                    // same type (`unset($a[]->b)` is an `[]` for unsetting).
                    self.var(object, mode)?;
                } else if is_variable(object) {
                    self.var(object, Mode::Read)?;
                } else {
                    self.expr(object)?;
                }
                self.member_name(name)
            }
            ExprKind::StaticProp { class, name, .. } => {
                self.class_fetch(class, line)?;
                if let VarName::Expr(x) = name {
                    self.expr(x)?;
                }
                Ok(())
            }
            ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::Assign { .. }
            | ExprKind::AssignRef { .. }
            | ExprKind::Pipe { .. } => self.expr(e),
            _ => {
                if matches!(mode, Mode::Write | Mode::ReadWrite | Mode::Unset) {
                    return err("Cannot use temporary expression in write context", line);
                }
                self.expr(e)
            }
        }
    }

    fn member_name(&mut self, m: &MemberName) -> VResult {
        match m {
            MemberName::Ident(_) => Ok(()),
            MemberName::Expr(x) => self.expr(x),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, e: &Expr) -> VResult {
        self.check_stack(e.span.line)?;
        let line = e.span.line;
        self.cur_line = line;
        match &e.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::String(_)
            | ExprKind::Const(_)
            | ExprKind::MagicConst(_) => Ok(()),
            ExprKind::Interpolated(parts) | ExprKind::ShellExec(parts) => {
                if matches!(e.kind, ExprKind::ShellExec(_)) {
                    self.warn(
                        DiagnosticLevel::Deprecated,
                        "The backtick (`) operator is deprecated, use shell_exec() instead",
                        line,
                    );
                }
                for p in parts {
                    match p {
                        InterpPart::Literal(_, span) => self.cur_line = span.line,
                        InterpPart::Expr { expr, syntax } => {
                            match syntax {
                                InterpSyntax::DollarCurly => self.warn(
                                    DiagnosticLevel::Deprecated,
                                    "Using ${var} in strings is deprecated, use {$var} instead",
                                    self.cur_line,
                                ),
                                InterpSyntax::DollarCurlyExpr => self.warn(
                                    DiagnosticLevel::Deprecated,
                                    "Using ${expr} (variable variables) in strings is deprecated, use {${expr}} instead",
                                    self.cur_line,
                                ),
                                InterpSyntax::Simple | InterpSyntax::Braced => {}
                            }
                            self.expr(expr)?;
                        }
                    }
                }
                Ok(())
            }
            ExprKind::Array { items, syntax } => {
                if *syntax == ArraySyntax::List {
                    return err("Cannot use list() as standalone expression", line);
                }
                self.items(items, line)
            }
            ExprKind::ClassConst { class, name } => {
                self.class_fetch(class, line)?;
                self.member_name(name)
            }
            ExprKind::ClassName(class) => self.class_fetch(class, line),
            ExprKind::Var(_)
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. } => self.var(e, Mode::Read),
            ExprKind::Call { callee, args } => {
                if let crate::ast::Callee::Expr(c) = callee.as_ref() {
                    self.expr(c)?;
                }
                self.args(args)
            }
            ExprKind::MethodCall {
                object, name, args, ..
            } => {
                self.expr(object)?;
                self.member_name(name)?;
                self.args(args)
            }
            ExprKind::StaticCall { class, name, args } => {
                if class.is_parent_hook_call(name) {
                    self.parent_hook_call(class, name, args, line)?;
                } else {
                    self.class_fetch(class, line)?;
                    self.member_name(name)?;
                }
                self.args(args)
            }
            ExprKind::New { class, args } => {
                match class.as_ref() {
                    NewClass::Name(n) => {
                        self.class_fetch(&ClassRef::Name(n.clone()), line)?;
                    }
                    NewClass::Expr(x) => self.expr(x)?,
                    NewClass::Anonymous(c) => {
                        self.args(args)?;
                        let saved_classes = std::mem::take(&mut self.classes);
                        let r = self.class(c);
                        self.classes = saved_classes;
                        return r;
                    }
                }
                self.args(args)
            }
            ExprKind::Assign { target, value } => {
                if is_this(target) {
                    return err("Cannot re-assign $this", line);
                }
                self.ensure_writable(target)?;
                if let ExprKind::Array { items, syntax } = &target.kind {
                    if items_have_refs(items) && !is_variable(value) {
                        return err("Cannot assign reference to non referenceable value", line);
                    }
                    self.expr(value)?;
                    return self.list_assign(items, *syntax, target.span.line);
                }
                self.var(target, Mode::Write)?;
                self.expr(value)
            }
            ExprKind::AssignRef { target, value } => {
                if is_this(target) {
                    return err("Cannot re-assign $this", line);
                }
                self.ensure_writable(target)?;
                if is_short_circuited(value) {
                    return err("Cannot take reference of a nullsafe chain", line);
                }
                if is_globals(value) {
                    return err("Cannot acquire reference to $GLOBALS", line);
                }
                self.var(target, Mode::Write)?;
                if is_call(value) {
                    self.expr(value)
                } else {
                    self.var(value, Mode::Write)
                }
            }
            ExprKind::AssignOp { target, value, op } => {
                self.ensure_writable(target)?;
                if *op == BinaryOp::Coalesce {
                    self.var(target, Mode::Isset)?;
                } else {
                    self.var(target, Mode::ReadWrite)?;
                }
                self.expr(value)
            }
            ExprKind::Binary { left, right, op } => {
                if *op == BinaryOp::Coalesce {
                    self.coalesce_operand(left)?;
                } else {
                    self.expr(left)?;
                }
                // `zend_compile_short_circuiting`: a constant left operand
                // that decides the result means the right one is never
                // compiled.
                if let Some(t) = const_truthiness(left) {
                    if (*op == BinaryOp::BoolAnd && !t) || (*op == BinaryOp::BoolOr && t) {
                        return Ok(());
                    }
                }
                self.expr(right)
            }
            ExprKind::Pipe { left, right } => {
                // `zend_compile_pipe`
                if matches!(&right.kind, ExprKind::ArrowFn(f) if !f.parenthesized) {
                    return err(
                        "Arrow functions on the right hand side of |> must be parenthesized",
                        line,
                    );
                }
                self.expr(left)?;
                self.expr(right)
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Silence(expr)
            | ExprKind::Print(expr)
            | ExprKind::Throw(expr)
            | ExprKind::VoidCast(expr)
            | ExprKind::YieldFrom(expr)
            | ExprKind::Include { expr, .. } => self.expr(expr),
            ExprKind::Cast { kind, expr } => {
                self.expr(expr)?;
                if *kind == CastKind::Unset {
                    return err("The (unset) cast is no longer supported", line);
                }
                Ok(())
            }
            ExprKind::IncDec { var, .. } => {
                self.ensure_writable(var)?;
                self.var(var, Mode::ReadWrite)
            }
            ExprKind::Isset(vars) => {
                for v in vars {
                    // `zend_is_variable()`: a call is not a variable here, and
                    // the check precedes the compilation of the operand.
                    if !is_variable(v) || is_call(v) {
                        return err(
                            "Cannot use isset() on the result of an expression (you can use \"null !== expression\" instead)",
                            v.span.line,
                        );
                    }
                    // `zend_compile_isset_or_empty` tests `is_global_var_fetch`
                    // before it compiles the variable.
                    if is_globals_append(v) {
                        return err("Cannot use [] for reading", v.span.line);
                    }
                    self.var(v, Mode::Isset)?;
                }
                Ok(())
            }
            ExprKind::Empty(x) => {
                if is_variable(x) && !is_call(x) {
                    if is_globals_append(x) {
                        return err("Cannot use [] for reading", x.span.line);
                    }
                    self.var(x, Mode::Isset)
                } else {
                    self.expr(x)
                }
            }
            ExprKind::Instanceof { expr, class } => {
                self.expr(expr)?;
                self.class_fetch(class, line)
            }
            ExprKind::Conditional {
                cond, then, else_, ..
            } => {
                if let ExprKind::Conditional {
                    then: inner_then,
                    parenthesized: false,
                    ..
                } = &cond.kind
                {
                    match (inner_then.is_some(), then.is_some()) {
                        (true, true) => return err("Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`", line),
                        (true, false) => return err("Unparenthesized `a ? b : c ?: d` is not supported. Use either `(a ? b : c) ?: d` or `a ? b : (c ?: d)`", line),
                        (false, true) => return err("Unparenthesized `a ?: b ? c : d` is not supported. Use either `(a ?: b) ? c : d` or `a ?: (b ? c : d)`", line),
                        (false, false) => {}
                    }
                }
                self.expr(cond)?;
                if let Some(t) = then {
                    self.expr(t)?;
                }
                self.expr(else_)
            }
            ExprKind::Yield { key, value } => {
                if let Some(k) = key {
                    self.expr(k)?;
                }
                if let Some(v) = value {
                    if is_variable(v) && !is_call(v) {
                        // yields by reference in by-ref generators
                        self.var(v, Mode::Read)?;
                    } else {
                        self.expr(v)?;
                    }
                }
                Ok(())
            }
            ExprKind::Closure(c) => {
                let mut seen: HashSet<&[u8]> = HashSet::new();
                for u in &c.uses {
                    if u.name.name == b"this" {
                        return err("Cannot use $this as lexical variable", u.span.line);
                    }
                    if AUTO_GLOBALS.contains(&u.name.name.as_slice()) {
                        return err("Cannot use auto-global as lexical variable", u.span.line);
                    }
                    if !seen.insert(&u.name.name) {
                        return err(
                            format!(
                                "Cannot use variable ${} twice",
                                String::from_utf8_lossy(&u.name.name)
                            ),
                            u.span.line,
                        );
                    }
                }
                for u in &c.uses {
                    if c.params.iter().any(|p| p.name.name == u.name.name) {
                        return err(
                            format!(
                                "Cannot use lexical variable ${} as a parameter name",
                                String::from_utf8_lossy(&u.name.name)
                            ),
                            u.span.line,
                        );
                    }
                }
                self.closure_like(&c.params, c.by_ref, |v| {
                    if let Some(t) = &c.return_type {
                        v.ty(t)?;
                    }
                    // `zend_compile_closure_uses()` stores the lexical
                    // variables in the closure's static_variables table, so a
                    // later `static $x` of the same name is a duplicate.
                    v.func()
                        .statics
                        .extend(c.uses.iter().map(|u| u.name.name.clone()));
                    v.stmts(&c.body)
                })
            }
            ExprKind::ArrowFn(f) => self.closure_like(&f.params, f.by_ref, |v| {
                if let Some(t) = &f.return_type {
                    v.ty(t)?;
                }
                v.expr(&f.body)
            }),
            ExprKind::Match { subject, arms } => {
                self.expr(subject)?;
                for a in arms {
                    if let Some(c) = &a.conds {
                        for x in c {
                            self.expr(x)?;
                        }
                    }
                    self.expr(&a.body)?;
                }
                Ok(())
            }
        }
    }

    /// The left operand of `??` is fetched in isset mode.
    fn coalesce_operand(&mut self, e: &Expr) -> VResult {
        if is_variable(e) && !is_call(e) {
            self.var(e, Mode::Isset)
        } else {
            self.expr(e)
        }
    }
}

/// The truthiness of an operand the compiler evaluates to a constant.
fn const_truthiness(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Int(i) => Some(*i != 0),
        ExprKind::Float(f) => Some(*f != 0.0),
        ExprKind::String(s) => Some(!(s.is_empty() || s == b"0")),
        ExprKind::Const(n)
            if matches!(n.kind, NameKind::Unqualified | NameKind::FullyQualified) =>
        {
            if n.name.eq_ignore_ascii_case(b"true") {
                Some(true)
            } else if n.name.eq_ignore_ascii_case(b"false") || n.name.eq_ignore_ascii_case(b"null")
            {
                Some(false)
            } else {
                None
            }
        }
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Not,
            expr,
        } => const_truthiness(expr).map(|b| !b),
        _ => None,
    }
}

/// `zend_propagate_list_refs`
fn items_have_refs(items: &[Option<ArrayItem>]) -> bool {
    items.iter().flatten().any(|i| {
        i.by_ref || matches!(&i.value.kind, ExprKind::Array { items, .. } if items_have_refs(items))
    })
}
