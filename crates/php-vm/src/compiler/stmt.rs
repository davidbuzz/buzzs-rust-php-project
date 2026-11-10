//! Statement compilation.

use super::{CResult, Cleanup, Compiler, FnKind, GotoUnwind, LoopInfo, LoopKind, PendingGoto};
use crate::ast::{self, Expr, ExprKind, Stmt, StmtKind, VarName};
use crate::opcode::{ArgSrc, BinKind, ForeachKind, Op, Operand, Place, PlaceBase, TryEntry};
use php_runtime::{Value, ZStr};

/// `ZEND_SYMBOL_CLASS` (`FC(seen_symbols)` kinds, `Zend/zend_compile.h`).
pub(crate) const SYMBOL_CLASS: u8 = 1 << 0;
/// `ZEND_SYMBOL_FUNCTION`.
pub(crate) const SYMBOL_FUNCTION: u8 = 1 << 1;
/// `ZEND_SYMBOL_CONST`.
pub(crate) const SYMBOL_CONST: u8 = 1 << 2;

impl Compiler<'_> {
    /// Compiles the statements of a file.
    pub(crate) fn compile_top_statements(&mut self, stmts: &[Stmt]) -> CResult<()> {
        self.check_strict_types_position(stmts)?;
        for s in stmts {
            self.compile_stmt_top(s)?;
            if matches!(s.kind, StmtKind::HaltCompiler { .. }) {
                break;
            }
        }
        Ok(())
    }

    /// [`Compiler::compile_top_statements`] over statements it owns: each
    /// one's AST is dropped as soon as it is compiled (see
    /// [`super::compile_program`]).
    pub(crate) fn compile_top_statements_owned(&mut self, stmts: Vec<Stmt>) -> CResult<()> {
        self.check_strict_types_position(&stmts)?;
        for s in stmts {
            self.compile_stmt_top(&s)?;
            if matches!(s.kind, StmtKind::HaltCompiler { .. }) {
                break;
            }
        }
        Ok(())
    }

    /// `declare(strict_types=...)` must be the very first statement.
    fn check_strict_types_position(&self, stmts: &[Stmt]) -> CResult<()> {
        for (i, s) in stmts.iter().enumerate() {
            if let StmtKind::Declare { directives, .. } = &s.kind {
                for d in directives {
                    if d.name.eq_ignore_case("strict_types") && i != 0 {
                        // `zend_is_first_statement(ast, /* allow_nop */ true)`: only
                        // other declares and empty statements (`;`, a bare `?>`) may
                        // precede strict_types (GH-19719); an opening inline HTML
                        // or any other statement may not.
                        let first_non_decl = stmts[..i]
                            .iter()
                            .any(|p| !matches!(p.kind, StmtKind::Declare { .. } | StmtKind::Nop));
                        if first_non_decl {
                            return self.err(
                                "strict_types declaration must be the very first statement in the script",
                                d.span.line,
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_stmt_top(&mut self, s: &Stmt) -> CResult<()> {
        match &s.kind {
            StmtKind::Function(fd) => self.compile_function_decl(fd, true),
            StmtKind::Class(cd) => self.compile_class_stmt(cd, true),
            StmtKind::Namespace { name, body } => {
                self.namespace = name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
                // `zend_reset_import_tables` also forgets the seen symbols.
                self.class_uses.clear();
                self.fn_uses.clear();
                self.const_uses.clear();
                self.seen_symbols.clear();
                if let Some(b) = body {
                    for st in b {
                        self.compile_stmt_top(st)?;
                    }
                }
                Ok(())
            }
            _ => self.compile_stmt(s),
        }
    }

    pub(crate) fn compile_stmts(&mut self, stmts: &[Stmt]) -> CResult<()> {
        for s in stmts {
            self.compile_stmt(s)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn compile_stmt(&mut self, s: &Stmt) -> CResult<()> {
        self.compile_stmt_inner(s)?;
        // `zend_compile_stmt`: ZEND_TICKS after each ticked statement.
        if self.ticks > 0 && !matches!(s.kind, StmtKind::Block(_) | StmtKind::Label(_)) {
            self.f().line = s.span.line;
            let n = self.ticks;
            self.emit(Op::Ticks(n));
        }
        Ok(())
    }

    fn compile_stmt_inner(&mut self, s: &Stmt) -> CResult<()> {
        self.f().line = s.span.line;
        match &s.kind {
            StmtKind::Block(b) => self.compile_stmts(b)?,
            StmtKind::If {
                branches,
                else_branch,
            } => self.compile_if(branches, else_branch.as_deref())?,
            StmtKind::While { cond, body } => self.compile_while(cond, body)?,
            StmtKind::DoWhile { body, cond } => self.compile_do_while(body, cond)?,
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => self.compile_for(init, cond, step, body)?,
            StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body,
            } => self.compile_foreach(subject, key.as_deref(), value, *by_ref, body)?,
            StmtKind::Switch { subject, cases } => self.compile_switch(subject, cases)?,
            StmtKind::Break(n) | StmtKind::Continue(n) => {
                let is_break = matches!(s.kind, StmtKind::Break(_));
                self.compile_break(is_break, n.as_ref(), s.span.line)?;
            }
            StmtKind::Return(e) => self.compile_return(e.as_ref(), s.span.line)?,
            StmtKind::Global(vars) => self.compile_global(vars)?,
            StmtKind::Static(vars) => self.compile_static(vars)?,
            StmtKind::Echo(exprs) => {
                for e in exprs {
                    let op = self.compile_expr(e)?;
                    self.emit(Op::Echo(op));
                }
            }
            StmtKind::InlineHtml(h) => {
                if !h.is_empty() {
                    let l = self.lit(Value::Str(ZStr::from(h.as_slice())));
                    self.emit(Op::Echo(l));
                }
            }
            StmtKind::Expr(e) => self.compile_expr_discard(e)?,
            StmtKind::Unset(vars) => self.compile_unset(vars)?,
            StmtKind::Declare { directives, body } => {
                self.compile_declare(directives, body.as_deref())?;
            }
            StmtKind::Nop | StmtKind::HaltCompiler { .. } => {}
            StmtKind::Try {
                body,
                catches,
                finally,
            } => self.compile_try(body, catches, finally.as_deref())?,
            StmtKind::Goto(label) => self.compile_goto(label, s.span.line),
            StmtKind::Label(label) => self.compile_label(label, s.span.line)?,
            StmtKind::Function(fd) => self.compile_function_decl(fd, false)?,
            StmtKind::Class(cd) => self.compile_class_stmt(cd, false)?,
            StmtKind::Const { consts, attributes } => {
                self.compile_const_stmt(consts, attributes, s.span.line)?;
            }
            StmtKind::Namespace { .. } => {
                // Nested namespaces are rejected by the parser.
                self.compile_stmt_top(s)?;
            }
            StmtKind::Use { kind, uses } => {
                for u in uses {
                    self.add_use(
                        *kind,
                        &u.name.name,
                        u.alias.as_ref().map(|a| a.name.as_slice()),
                        u.span.line,
                    )?;
                }
            }
            StmtKind::GroupUse { kind, prefix, uses } => {
                for u in uses {
                    let mut full = prefix.name.clone();
                    full.push(b'\\');
                    full.extend_from_slice(&u.name.name);
                    let k = kind.unwrap_or(u.kind);
                    self.add_use(
                        k,
                        &full,
                        u.alias.as_ref().map(|a| a.name.as_slice()),
                        u.span.line,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// `unset($a, $b[1], $c->p);` (`zend_compile_unset`): `$this` and a
    /// nullsafe chain cannot be unset.
    fn compile_unset(&mut self, vars: &[Expr]) -> CResult<()> {
        for v in vars {
            self.f().line = v.span.line;
            if let ExprKind::Var(VarName::Name(id)) = &v.kind {
                if id.name == b"this" {
                    return self.err("Cannot unset $this", v.span.line);
                }
            }
            if let ExprKind::Prop { nullsafe: true, .. } = &v.kind {
                return self.err("Can't use nullsafe operator in write context", v.span.line);
            }
            let p = self.compile_place(v, true)?;
            let pid = self.place(p);
            self.emit(Op::Unset { place: pid });
        }
        Ok(())
    }

    /// `goto label;`: a jump patched once the label is known, with the
    /// loops to unwind on the way (`zend_compile_goto`).
    fn compile_goto(&mut self, label: &ast::Ident, line: u32) {
        let unwind = self.emit_goto_unwind();
        let at = self.emit(Op::Jmp(0));
        let loops = self.f().loop_ids.clone();
        self.f().gotos.push(PendingGoto {
            at,
            label: label.name.clone(),
            line,
            loops,
            unwind,
        });
    }

    /// `label:` (`zend_compile_label`): defined once per function.
    fn compile_label(&mut self, label: &ast::Ident, line: u32) -> CResult<()> {
        let here = self.here();
        let loops = self.f().loop_ids.clone();
        if self.f().labels.contains_key(&label.name) {
            return self.err(
                format!("Label '{}' already defined", label.as_str_lossy()),
                line,
            );
        }
        self.f().labels.insert(label.name.clone(), (here, loops));
        Ok(())
    }

    /// `zend_compile_if`: each branch tests its condition and, unless it is
    /// the last one, jumps over the rest when it ran ("Don't generate a
    /// jump for the last branch").
    fn compile_if(
        &mut self,
        branches: &[ast::IfBranch],
        else_branch: Option<&Stmt>,
    ) -> CResult<()> {
        let mut end_jumps = Vec::new();
        for (i, br) in branches.iter().enumerate() {
            self.f().line = br.cond.span.line;
            let c = self.compile_expr(&br.cond)?;
            let jz = self.emit(Op::JmpZ(c, 0));
            self.compile_stmt(&br.body)?;
            if i + 1 < branches.len() || else_branch.is_some() {
                end_jumps.push(self.emit(Op::Jmp(0)));
            }
            self.patch_here(jz);
        }
        if let Some(e) = else_branch {
            self.compile_stmt(e)?;
        }
        for j in end_jumps {
            self.patch_here(j);
        }
        Ok(())
    }

    /// `zend_compile_while`: a jump to the condition, the body, then the
    /// condition and a `JMPNZ` back to the body, so an iteration dispatches
    /// one jump (not a `JMPZ` plus a `JMP` back), and the body is compiled
    /// first (CV order).
    fn compile_while(&mut self, cond: &Expr, body: &Stmt) -> CResult<()> {
        let to_cond = self.emit(Op::Jmp(0));
        self.push_loop(LoopKind::Loop, Cleanup::None);
        let start = self.here();
        self.compile_stmt(body)?;
        let info = self.pop_loop();
        let cond_at = self.here();
        self.patch(to_cond, cond_at);
        self.f().line = cond.span.line;
        let c = self.compile_expr(cond)?;
        self.emit(Op::JmpNz(c, start));
        self.finish_loop(info, cond_at);
        Ok(())
    }

    /// `do { body } while (cond);`: the body, then the condition and a
    /// `JMPNZ` back to it.
    fn compile_do_while(&mut self, body: &Stmt, cond: &Expr) -> CResult<()> {
        let start = self.here();
        self.push_loop(LoopKind::Loop, Cleanup::None);
        self.compile_stmt(body)?;
        let info = self.pop_loop();
        let cont = self.here();
        self.f().line = cond.span.line;
        let c = self.compile_expr(cond)?;
        self.emit(Op::JmpNz(c, start));
        self.finish_loop(info, cont);
        Ok(())
    }

    /// `zend_compile_for`: init, a jump to the condition, the body, the
    /// step, then the condition and a `JMPNZ` back to the body (one jump
    /// per iteration; CVs numbered in that order). Without a condition the
    /// step jumps back unconditionally.
    fn compile_for(
        &mut self,
        init: &[Expr],
        cond: &[Expr],
        step: &[Expr],
        body: &Stmt,
    ) -> CResult<()> {
        for e in init {
            self.compile_expr_discard(e)?;
        }
        let to_cond = self.emit(Op::Jmp(0));
        self.push_loop(LoopKind::Loop, Cleanup::None);
        let start = self.here();
        self.compile_stmt(body)?;
        let info = self.pop_loop();
        let cont = self.here();
        for e in step {
            self.compile_expr_discard(e)?;
        }
        self.patch_here(to_cond);
        let mut c = None;
        for (i, e) in cond.iter().enumerate() {
            if i + 1 == cond.len() {
                self.f().line = e.span.line;
                c = Some(self.compile_expr(e)?);
            } else {
                self.compile_expr_discard(e)?;
            }
        }
        match c {
            Some(c) => self.emit(Op::JmpNz(c, start)),
            None => self.emit(Op::Jmp(start)),
        };
        self.finish_loop(info, cont);
        Ok(())
    }

    /// `global $a, $$name;` (`zend_compile_global_var`).
    fn compile_global(&mut self, vars: &[Expr]) -> CResult<()> {
        for v in vars {
            match &v.kind {
                ExprKind::Var(VarName::Name(id)) => {
                    if id.name == b"this" {
                        return self.err("Cannot use $this as global variable", v.span.line);
                    }
                    // `global $_ENV` names the auto-global too.
                    super::expr::is_superglobal(&id.name);
                    let cv = self.cv(&id.name);
                    let name = self.lit_str(&id.name);
                    self.emit(Op::Global { cv, name });
                }
                ExprKind::Var(VarName::Expr(e)) => {
                    // `zend_compile_global_var` without a CV: the name is
                    // evaluated once, FETCH_W (global) then FETCH_W (local)
                    // and ASSIGN_REF.
                    let n = self.compile_expr(e)?;
                    self.f().line = v.span.line;
                    let global = self.place(Place {
                        base: PlaceBase::Global(Compiler::keep(n)),
                        segs: Vec::new(),
                    });
                    let local = self.place(Place {
                        base: PlaceBase::VarVar(n),
                        segs: Vec::new(),
                    });
                    let t = self.tmp();
                    self.emit(Op::MakeRef {
                        place: global,
                        dst: t,
                    });
                    self.emit(Op::AssignRef {
                        place: local,
                        src: Operand::Tmp(t),
                        dst: None,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `static $a = <const expr>;`: `BindStatic` binds an initialized
    /// static and skips `InitStatic`, which initializes it the first time.
    /// The declaration is also recorded in the function's static variable
    /// table (`op_array->static_variables`, see `zend_compile_static_var`).
    fn compile_static(&mut self, vars: &[ast::StaticVar]) -> CResult<()> {
        for sv in vars {
            self.f().line = sv.span.line;
            if sv.name.name == b"this" {
                return self.err("Cannot use $this as static variable", sv.span.line);
            }
            let cv = self.cv(&sv.name.name);
            let idx = {
                let f = self.f();
                f.num_statics += 1;
                f.num_statics - 1
            };
            let bind = self.emit(Op::BindStatic { cv, idx, skip: 0 });
            let val = match &sv.default {
                Some(e) => self.compile_expr(e)?,
                None => self.lit(Value::Null),
            };
            let init = match val {
                Operand::Lit(l) => self.f().literals.get(l as usize).cloned(),
                _ => None,
            };
            let f = self.f();
            let name = f.cv_names.get(cv as usize).cloned().unwrap_or_default();
            f.static_vars
                .push(crate::opcode::StaticVarDecl { name, init });
            self.emit(Op::InitStatic { cv, idx, val });
            self.patch_here(bind);
        }
        Ok(())
    }

    /// `declare(strict_types=1)`, `declare(ticks=N) { ... }` (block mode
    /// applies the directives to the block only), `declare(encoding=...)`.
    fn compile_declare(
        &mut self,
        directives: &[ast::ConstElem],
        body: Option<&Stmt>,
    ) -> CResult<()> {
        let ticks_before = self.ticks;
        for d in directives {
            if d.name.eq_ignore_case("strict_types") {
                if let ExprKind::Int(v) = d.value.kind {
                    if v != 0 && v != 1 {
                        return self.err(
                            "strict_types declaration must have 0 or 1 as its value",
                            d.span.line,
                        );
                    }
                    if body.is_some() {
                        return self.err(
                            "strict_types declaration must not use block mode",
                            d.span.line,
                        );
                    }
                    self.strict_types = v == 1;
                } else {
                    return self.err(
                        "strict_types declaration must have 0 or 1 as its value",
                        d.span.line,
                    );
                }
            } else if d.name.eq_ignore_case("ticks") {
                let n = match self.try_fold(&d.value) {
                    Some(Value::Int(n)) => n,
                    Some(v) => v.as_int().unwrap_or(0),
                    None => 0,
                };
                self.ticks = u32::try_from(n.max(0)).unwrap_or(u32::MAX);
            }
            // `encoding` is ignored (no zend.multibyte); php-parser's
            // validation pass already reported "Unsupported declare" for
            // any other directive.
        }
        if let Some(b) = body {
            let r = self.compile_stmt(b);
            self.ticks = ticks_before;
            r?;
        }
        Ok(())
    }

    /// `const A = 1, B = 2;` at the top level (`zend_compile_const_decl`);
    /// attributes apply to a single constant. `line` is the statement's.
    fn compile_const_stmt(
        &mut self,
        consts: &[ast::ConstElem],
        attributes: &ast::Attributes,
        line: u32,
    ) -> CResult<()> {
        let mut attrs = u32::MAX;
        if !attributes.is_empty() {
            if consts.len() > 1 {
                let line = consts.first().map_or(line, |c| c.span.line);
                return self.err(
                    "Cannot apply attributes to multiple constants at once",
                    line,
                );
            }
            let list = self.attributes(attributes)?;
            self.validate_attributes(&list, super::attrs::target::CONSTANT, 0, None, false)?;
            // `const` statements are top-level code: the main op array's
            // table, which is the unit's.
            let table = self.current_decls();
            let mut t = table.const_attributes.borrow_mut();
            t.push(std::rc::Rc::new(list));
            attrs = u32::try_from(t.len() - 1).unwrap_or(u32::MAX);
        }
        for c in consts {
            self.f().line = c.span.line;
            let full = self.ns_prefixed_pub(&c.name.name);
            let name = self.lit_str(&full);
            self.check_const_expr(&c.value, true)?;
            self.check_const_decl_name(&c.name.name, &full, c.span.line)?;
            let saved = std::mem::replace(&mut self.in_const_decl, true);
            let depth = std::mem::replace(&mut self.inline_const_depth, self.fns.len());
            let global = std::mem::replace(&mut self.in_global_const, true);
            let val = self.compile_expr(&c.value);
            self.in_global_const = global;
            self.inline_const_depth = depth;
            self.in_const_decl = saved;
            let val = val?;
            self.emit(Op::DeclareConst { name, val, attrs });
        }
        Ok(())
    }

    pub(crate) fn ns_prefixed_pub(&self, name: &[u8]) -> Vec<u8> {
        if self.namespace.is_empty() {
            name.to_vec()
        } else {
            let mut v = self.namespace.clone();
            v.push(b'\\');
            v.extend_from_slice(name);
            v
        }
    }

    /// `zend_compile_const_decl` (`Zend/zend_compile.c)`: `true`/`false`/`null`
    /// cannot be declared, nor a name imported by `use const` for another
    /// constant. Registers the name as seen.
    fn check_const_decl_name(&mut self, unqualified: &[u8], full: &[u8], line: u32) -> CResult<()> {
        if unqualified.eq_ignore_ascii_case(b"null")
            || unqualified.eq_ignore_ascii_case(b"true")
            || unqualified.eq_ignore_ascii_case(b"false")
        {
            return self.err(
                format!(
                    "Cannot redeclare constant '{}'",
                    String::from_utf8_lossy(unqualified)
                ),
                line,
            );
        }
        if self
            .const_uses
            .get(unqualified)
            .is_some_and(|import| import.as_slice() != full)
        {
            return self.err(
                format!(
                    "Cannot declare const {} because the name is already in use",
                    String::from_utf8_lossy(full)
                ),
                line,
            );
        }
        self.register_seen_symbol(full.to_vec(), SYMBOL_CONST);
        Ok(())
    }

    /// `zend_register_seen_symbol`.
    pub(crate) fn register_seen_symbol(&mut self, name: Vec<u8>, kind: u8) {
        *self.seen_symbols.entry(name).or_insert(0) |= kind;
    }

    /// `zend_compile_use` (`Zend/zend_compile.c`) for one imported name.
    #[allow(clippy::too_many_lines)]
    fn add_use(
        &mut self,
        kind: ast::UseKind,
        full: &[u8],
        alias: Option<&[u8]>,
        line: u32,
    ) -> CResult<()> {
        // The grammar drops the leading `\` of `use \A\B`.
        let full = full.strip_prefix(b"\\").unwrap_or(full);
        let short = match alias {
            Some(a) => a.to_vec(),
            None => {
                if let Some(p) = full.iter().rposition(|&c| c == b'\\') {
                    // "use A\B" is equivalent to "use A\B as B".
                    full[p + 1..].to_vec()
                } else {
                    if self.namespace.is_empty() {
                        self.warnings.push((
                            2,
                            format!(
                                "The use statement with non-compound name '{}' has no effect",
                                String::from_utf8_lossy(full)
                            ),
                            line,
                        ));
                    }
                    full.to_vec()
                }
            }
        };
        match kind {
            ast::UseKind::Class => {
                let lc = short.to_ascii_lowercase();
                if super::decl::is_reserved_class_name(&lc) {
                    return self.err(
                        format!(
                            "Cannot use {} as {} because '{}' is a special class name",
                            String::from_utf8_lossy(full),
                            String::from_utf8_lossy(&short),
                            String::from_utf8_lossy(&short)
                        ),
                        line,
                    );
                }
                if matches!(lc.as_slice(), b"_" | b"let" | b"is") {
                    self.warnings.push((
                        8192,
                        format!(
                            "Using \"{}\" as a class name is deprecated",
                            String::from_utf8_lossy(&short)
                        ),
                        line,
                    ));
                }
            }
            ast::UseKind::Function => {
                let lc = short.to_ascii_lowercase();
                if matches!(lc.as_slice(), b"let" | b"is") {
                    self.warnings.push((
                        8192,
                        format!(
                            "Using \"{}\" as a function name is deprecated since 8.6",
                            String::from_utf8_lossy(&short)
                        ),
                        line,
                    ));
                }
            }
            ast::UseKind::Const => {
                if matches!(short.to_ascii_lowercase().as_slice(), b"_" | b"let" | b"is") {
                    self.warnings.push((
                        8192,
                        format!(
                            "Using \"{}\" as a constant name is deprecated since 8.6",
                            String::from_utf8_lossy(&short)
                        ),
                        line,
                    ));
                }
            }
        }
        // Constant imports are case-sensitive.
        let (lookup, symbol, type_str) = match kind {
            ast::UseKind::Class => (short.to_ascii_lowercase(), SYMBOL_CLASS, ""),
            ast::UseKind::Function => (short.to_ascii_lowercase(), SYMBOL_FUNCTION, " function"),
            ast::UseKind::Const => (short.clone(), SYMBOL_CONST, " const"),
        };
        // `zend_check_already_in_use`: a symbol of that name declared
        // earlier in the file (in the current namespace).
        let check = if self.namespace.is_empty() {
            lookup.clone()
        } else {
            let mut v = self.namespace.to_ascii_lowercase();
            v.push(b'\\');
            v.extend_from_slice(&lookup);
            v
        };
        let seen = self
            .seen_symbols
            .get(&check)
            .is_some_and(|k| (k & symbol) != 0)
            && !full.eq_ignore_ascii_case(&check);
        let table = match kind {
            ast::UseKind::Class => &mut self.class_uses,
            ast::UseKind::Function => &mut self.fn_uses,
            ast::UseKind::Const => &mut self.const_uses,
        };
        if seen || table.contains_key(&lookup) {
            return self.err(
                format!(
                    "Cannot use{type_str} {} as {} because the name is already in use",
                    String::from_utf8_lossy(full),
                    String::from_utf8_lossy(&short)
                ),
                line,
            );
        }
        table.insert(lookup, full.to_vec());
        Ok(())
    }

    // ------------------------------------------------------------------
    // Loops
    // ------------------------------------------------------------------

    pub(crate) fn push_loop(&mut self, kind: LoopKind, cleanup: Cleanup) {
        let depth = self.fref().finally_stack.len();
        let f = self.f();
        let in_finally = f.in_finally;
        f.loops.push(LoopInfo {
            kind,
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            cleanup,
            finally_depth: depth,
            in_finally,
        });
        let id = f.next_loop_id;
        f.next_loop_id += 1;
        f.loop_ids.push(id);
    }

    pub(crate) fn pop_loop(&mut self) -> LoopInfo {
        let f = self.f();
        f.loop_ids.pop();
        f.loops.pop().expect("loop info")
    }

    /// Patches breaks to the current position and continues to `cont`.
    pub(crate) fn finish_loop(&mut self, info: LoopInfo, cont: u32) {
        for b in info.break_patches {
            self.patch_here(b);
        }
        for c in info.continue_patches {
            self.patch(c, cont);
        }
    }

    fn compile_break(&mut self, is_break: bool, n: Option<&Expr>, line: u32) -> CResult<()> {
        let word = if is_break { "break" } else { "continue" };
        let depth: usize = match n {
            None => 1,
            Some(e) => match e.kind {
                ExprKind::Int(v) if v >= 1 => usize::try_from(v).unwrap_or(usize::MAX),
                _ => {
                    return self.err(
                        format!("'{word}' operator accepts only positive integers"),
                        line,
                    )
                }
            },
        };
        let nloops = self.fref().loops.len();
        if nloops == 0 {
            return self.err(
                format!("'{word}' not in the 'loop' or 'switch' context"),
                line,
            );
        }
        if depth > nloops {
            return self.err(
                format!(
                    "Cannot '{word}' {depth} level{}",
                    if depth == 1 { "" } else { "s" }
                ),
                line,
            );
        }
        let target_idx = nloops - depth;
        let mut is_break = is_break;
        if !is_break && self.fref().loops[target_idx].kind == LoopKind::Switch {
            // "continue" targeting switch is equivalent to "break"; the
            // warning (`zend_compile_break_continue`) comes from
            // `php_parser::validate`.
            is_break = true;
        }
        // `zend_check_finally_breakout` (pass two): the target loop
        // encloses the finally block the jump is in.
        if self.fref().in_finally > self.fref().loops[target_idx].in_finally {
            return self.err("jump out of a finally block is disallowed", line);
        }
        // `zend_handle_loops_and_finally_ex(depth)`: from the innermost
        // construct out, finally blocks are called and the variables of the
        // crossed loops freed (`ZEND_FREE_ON_RETURN`, so an exception from a
        // destructor is thrown from the loop, not from the try it is in).
        // A break also frees the target loop (it jumps past its FE_FREE).
        let upto = if is_break { target_idx } else { target_idx + 1 };
        let finally_stack = self.fref().finally_stack.clone();
        let mut pending = finally_stack.len();
        for i in (target_idx..nloops).rev() {
            let (depth, cleanup) = {
                let l = &self.fref().loops[i];
                (l.finally_depth, l.cleanup)
            };
            while pending > depth {
                pending -= 1;
                self.emit_fast_call(finally_stack[pending]);
            }
            if i >= upto {
                self.emit_cleanups_of(&[cleanup]);
            }
        }
        let j = self.emit(Op::Jmp(0));
        let l = &mut self.f().loops[target_idx];
        if is_break {
            l.break_patches.push(j);
        } else {
            l.continue_patches.push(j);
        }
        Ok(())
    }

    fn emit_fast_call(&mut self, fin: u32) {
        // Target patched when the finally block is compiled.
        let at = self.emit(Op::FastCall { fin, target: 0 });
        self.f().fast_call_patches_push(fin, at);
    }

    fn compile_return(&mut self, e: Option<&Expr>, line: u32) -> CResult<()> {
        let kind = self.fref().kind;
        let returns_ref = self.fref().returns_ref;
        let is_gen = self.fref().is_generator;
        if let Some(rt) = &self.fref().return_type {
            if !is_gen {
                let what = if self.current_class().is_some() {
                    "method"
                } else {
                    "function"
                };
                if rt.is_void() && e.is_some() {
                    let hint = if e.is_some_and(|e| matches!(self.try_fold(e), Some(Value::Null))) {
                        " (did you mean \"return;\" instead of \"return null;\"?)"
                    } else {
                        ""
                    };
                    return self.err(format!("A void {what} must not return a value{hint}"), line);
                }
                if rt.is_never() {
                    return self.err(format!("A never-returning {what} must not return"), line);
                }
                if e.is_none() && !rt.is_void() && self.fref().has_return_type {
                    let hint = if rt.allows_null() {
                        " (did you mean \"return null;\" instead of \"return;\"?)"
                    } else {
                        ""
                    };
                    return self.err(
                        format!("A {what} with return type must return a value{hint}"),
                        line,
                    );
                }
            }
        }
        let _ = kind;
        let fins: Vec<u32> = self.fref().finally_stack.iter().rev().copied().collect();
        if returns_ref && !is_gen {
            if let Some(ex) = e {
                // zend_compile_return: zend_assert_not_short_circuited.
                if super::expr::contains_nullsafe(ex) {
                    return self.err("Cannot take reference of a nullsafe chain", line);
                }
                if is_ref_expr(ex) {
                    let p = self.compile_place(ex, false)?;
                    let pid = self.place(p);
                    if fins.is_empty() && self.fref().loops.is_empty() {
                        self.emit(Op::ReturnRef { place: pid });
                        return Ok(());
                    }
                    let t = self.tmp();
                    self.emit(Op::MakeRef { place: pid, dst: t });
                    self.emit_return_unwind();
                    let tp = self.place(Place {
                        base: PlaceBase::Value(Operand::Tmp(t)),
                        segs: Vec::new(),
                    });
                    self.emit(Op::ReturnRef { place: tp });
                    return Ok(());
                }
            }
        }
        let by_ref = returns_ref && !is_gen;
        let mut val = match e {
            Some(ex) if by_ref => self.by_ref_return_operand(ex)?,
            Some(ex) => self.compile_expr(ex)?,
            None => self.lit(Value::Null),
        };
        if e.is_none() {
            val = Operand::Unused;
        }
        // `zend_handle_loops_and_finally`: from the innermost construct
        // out, finally blocks are called and loop variables freed.
        if !fins.is_empty() || !self.fref().loops.is_empty() {
            // Keep the value in a temporary across the unwinding. The
            // operand of ZEND_RETURN_BY_REF is a call result or a temporary,
            // which the finally blocks and loop frees cannot touch: php-src
            // copies only a CV (`zend_compile_return`), and a copy would
            // dereference what a by-reference call returned.
            let t = self.tmp();
            let copy = val != Operand::Unused && (!by_ref || matches!(val, Operand::Cv(_)));
            if copy {
                self.emit(Op::Move { src: val, dst: t });
                val = Operand::Tmp(t);
                // zend_compile_return emits VERIFY_RETURN_TYPE before
                // zend_handle_loops_and_finally: the coercion (and its
                // diagnostics) precede the finally blocks (bug72347).
                if !fins.is_empty() && !is_gen && !returns_ref && self.fref().has_return_type {
                    self.emit(Op::VerifyReturn { val });
                }
            }
            self.emit_return_unwind();
        }
        if by_ref {
            // ZEND_RETURN_BY_REF of a value (not a variable).
            self.emit(Op::ReturnByRefValue { val });
        } else {
            self.emit(Op::Return { val });
        }
        Ok(())
    }

    /// The operand of `return expr;` in a by-reference function when `expr`
    /// is not a variable (`zend_compile_return`, also the body of
    /// `fn&() => expr`). A call is fetched for writing
    /// (`zend_is_variable_or_call`), so the reference a by-reference
    /// function returned passes through `ZEND_RETURNS_FUNCTION`. Anything
    /// else is compiled for reading: php-src master guarantees such a TMP
    /// holds no reference (GH-20628 dereferences `$a =& $b` and `@f()` in
    /// read context), so `ZEND_RETURN_BY_REF` raises "Only variable references
    /// should be returned by reference" and returns a new reference to the
    /// value. [`Op::ReturnByRefValue`] tells the two apart by whether the
    /// operand holds a reference, so a temporary is dereferenced here.
    pub(crate) fn by_ref_return_operand(&mut self, ex: &Expr) -> CResult<Operand> {
        let v = self.compile_expr(ex)?;
        if super::expr::is_call_expr(ex) || !matches!(v, Operand::Tmp(_)) {
            return Ok(v);
        }
        let t = self.tmp();
        self.emit(Op::Move { src: v, dst: t });
        Ok(Operand::Tmp(t))
    }

    /// `zend_handle_loops_and_finally` for `return`: innermost first,
    /// finally blocks nested inside a loop run before the loop variable is
    /// freed; the outermost finally blocks run last.
    fn emit_return_unwind(&mut self) {
        let finally_stack: Vec<u32> = self.fref().finally_stack.clone();
        let loops: Vec<(usize, Cleanup)> = self
            .fref()
            .loops
            .iter()
            .rev()
            .map(|l| (l.finally_depth, l.cleanup))
            .collect();
        let mut pending = finally_stack.len();
        for (depth, cleanup) in loops {
            while pending > depth {
                pending -= 1;
                self.emit_fast_call(finally_stack[pending]);
            }
            self.emit_cleanup_on_return(cleanup);
        }
        while pending > 0 {
            pending -= 1;
            self.emit_fast_call(finally_stack[pending]);
        }
    }

    /// `zend_handle_loops_and_finally` for `goto`: every enclosing loop
    /// variable is freed and every enclosing finally block called,
    /// innermost first; `finish_fn` turns the ones the jump does not leave
    /// into NOPs (`zend_resolve_goto_label`).
    fn emit_goto_unwind(&mut self) -> Vec<(usize, GotoUnwind)> {
        let finally_stack: Vec<u32> = self.fref().finally_stack.clone();
        let loops: Vec<(usize, usize, Cleanup)> = self
            .fref()
            .loops
            .iter()
            .zip(&self.fref().loop_ids)
            .rev()
            .map(|(l, &id)| (id, l.finally_depth, l.cleanup))
            .collect();
        let mut unwind = Vec::new();
        let mut pending = finally_stack.len();
        for (id, depth, cleanup) in loops {
            while pending > depth {
                pending -= 1;
                unwind.push((
                    self.fref().ops.len(),
                    GotoUnwind::Finally(finally_stack[pending]),
                ));
                self.emit_fast_call(finally_stack[pending]);
            }
            if cleanup != Cleanup::None {
                unwind.push((self.fref().ops.len(), GotoUnwind::Loop(id)));
                self.emit_cleanups_of(&[cleanup]);
            }
        }
        while pending > 0 {
            pending -= 1;
            unwind.push((
                self.fref().ops.len(),
                GotoUnwind::Finally(finally_stack[pending]),
            ));
            self.emit_fast_call(finally_stack[pending]);
        }
        unwind
    }

    fn emit_cleanup_on_return(&mut self, c: Cleanup) {
        self.emit_cleanups_of(&[c]);
    }

    /// Frees the given loop iterators/temporaries (`return` flavour).
    fn emit_cleanups_of(&mut self, cleanups: &[Cleanup]) {
        for &c in cleanups {
            match c {
                Cleanup::None => {}
                Cleanup::FeFree(i) => {
                    let at = self.fref().iter_starts.get(&i).copied().unwrap_or(u32::MAX);
                    self.emit(Op::FeFreeOnReturn {
                        iter: i,
                        loop_at: at,
                    });
                }
                Cleanup::FreeTmp(t) => {
                    self.emit(Op::Free(Operand::Tmp(t)));
                }
            }
        }
    }

    pub(crate) fn fn_display_name(&self) -> String {
        let f = self.fref();
        if let Some(c) = self.classes.last() {
            if f.kind == FnKind::Method {
                return format!("{}::{}", c.name, f.method_name);
            }
        }
        f.func_name.to_string_lossy()
    }

    fn compile_foreach(
        &mut self,
        subject: &Expr,
        key: Option<&Expr>,
        value: &Expr,
        by_ref: bool,
        body: &Stmt,
    ) -> CResult<()> {
        // zend_compile_foreach: a list() value with references
        // (zend_propagate_list_refs) iterates by reference.
        let list_refs = matches!(&value.kind, ExprKind::Array { items, .. }
            if items.iter().flatten().any(|i| i.by_ref || super::expr::has_nested_ref(i)));
        let by_ref = by_ref || list_refs;
        let iter = {
            let f = self.f();
            f.num_iters += 1;
            f.num_iters - 1
        };
        if let Some(k) = key {
            if matches!(k.kind, ExprKind::Array { .. }) {
                return self.err("Cannot use list as key element", k.span.line);
            }
        }
        if let ExprKind::Var(VarName::Name(id)) = &value.kind {
            if id.name == b"this" {
                return self.err("Cannot re-assign $this", value.span.line);
            }
        }
        let reset = if by_ref {
            // zend_compile_foreach: a nullsafe chain is not a writable
            // variable (zend_can_write_to_variable), so it is iterated as
            // a temporary copy.
            let p = if subject.is_variable()
                && !super::expr::contains_nullsafe(subject)
                && !matches!(
                    subject.kind,
                    ExprKind::Call { .. }
                        | ExprKind::MethodCall { .. }
                        | ExprKind::StaticCall { .. }
                ) {
                self.compile_place(subject, false)?
            } else {
                let op = self.compile_expr(subject)?;
                Place {
                    base: PlaceBase::Value(op),
                    segs: Vec::new(),
                }
            };
            let pid = self.place(p);
            self.emit(Op::FeReset {
                subject: Operand::Unused,
                place: Some(pid),
                kind: ForeachKind::Ref,
                iter,
                end: 0,
            })
        } else {
            let s = self.compile_expr(subject)?;
            self.emit(Op::FeReset {
                subject: s,
                place: None,
                kind: ForeachKind::Value,
                iter,
                end: 0,
            })
        };
        let fetch_at = self.here();
        let reset_at = u32::try_from(reset).unwrap_or(u32::MAX);
        self.f().iter_starts.insert(iter, reset_at);
        let vt = self.tmp();
        let kt = key.map(|_| self.tmp());
        let fetch = self.emit(Op::FeFetch {
            iter,
            end: 0,
            val: vt,
            key: kt,
        });
        // Assign value.
        self.f().line = value.span.line;
        if by_ref {
            if let ExprKind::Array { items, .. } = &value.kind {
                if list_refs {
                    // ZEND_FETCH_LIST_W through the FE_FETCH_RW reference.
                    let src = Place {
                        base: PlaceBase::Value(Operand::TmpKeep(vt)),
                        segs: Vec::new(),
                    };
                    self.compile_destructure_ref(items, &src, value.span.line, false)?;
                    self.emit(Op::Free(Operand::Tmp(vt)));
                } else {
                    self.compile_destructure(items, Operand::Tmp(vt), value.span.line)?;
                }
            } else {
                let p = self.compile_place(value, false)?;
                let pid = self.place(p);
                self.emit(Op::AssignRef {
                    place: pid,
                    src: Operand::Tmp(vt),
                    dst: None,
                });
            }
        } else if let ExprKind::Array { items, .. } = &value.kind {
            self.compile_destructure(items, Operand::Tmp(vt), value.span.line)?;
        } else {
            self.assign_to(value, Operand::Tmp(vt), None)?;
        }
        if let (Some(k), Some(kt)) = (key, kt) {
            self.assign_to(k, Operand::Tmp(kt), None)?;
        }
        self.push_loop(LoopKind::Loop, Cleanup::FeFree(iter));
        self.compile_stmt(body)?;
        let info = self.pop_loop();
        self.emit(Op::Jmp(fetch_at));
        self.patch_here(reset);
        self.patch_here(fetch);
        self.emit(Op::FeFree { iter });
        self.finish_loop(info, fetch_at);
        Ok(())
    }

    fn compile_switch(&mut self, subject: &Expr, cases: &[ast::SwitchCase]) -> CResult<()> {
        let s = self.compile_expr(subject)?;
        // A variable subject is compared as it stands at each case
        // (`ZEND_IS_EQUAL` on the CV, not a copy): each case reads it, so
        // an undefined one warns on every case line, and a case expression
        // that assigns it changes what later cases compare.
        let (subj, cleanup) = match s {
            Operand::Tmp(t) => (Operand::TmpKeep(t), Cleanup::FreeTmp(t)),
            o => (o, Cleanup::None),
        };
        let mut case_jumps = Vec::new();
        let mut default_idx = None;
        let mut seen_default = false;
        for (i, c) in cases.iter().enumerate() {
            match &c.cond {
                Some(e) => {
                    self.f().line = c.span.line;
                    let v = self.compile_expr(e)?;
                    let t = self.tmp();
                    self.emit(Op::Bin {
                        op: BinKind::Equal,
                        a: subj,
                        b: v,
                        dst: t,
                    });
                    case_jumps.push((i, self.emit(Op::JmpNz(Operand::Tmp(t), 0))));
                }
                None => {
                    if seen_default {
                        return self.err(
                            "Switch statements may only contain one default clause",
                            c.span.line,
                        );
                    }
                    seen_default = true;
                    default_idx = Some(i);
                }
            }
        }
        let jmp_default = self.emit(Op::Jmp(0));
        self.push_loop(LoopKind::Switch, cleanup);
        let mut body_starts = Vec::new();
        for c in cases {
            body_starts.push(self.here());
            self.compile_stmts(&c.body)?;
        }
        let info = self.pop_loop();
        let end = self.here();
        for (i, j) in case_jumps {
            self.patch(j, body_starts[i]);
        }
        match default_idx {
            Some(d) => self.patch(jmp_default, body_starts[d]),
            None => self.patch(jmp_default, end),
        }
        if let Cleanup::FreeTmp(t) = cleanup {
            self.emit(Op::Free(Operand::Tmp(t)));
        }
        let after = self.here();
        for b in info.break_patches {
            self.patch(b, after);
        }
        for c in info.continue_patches {
            self.patch(c, after);
        }
        Ok(())
    }

    fn compile_try(
        &mut self,
        body: &[Stmt],
        catches: &[ast::Catch],
        finally: Option<&[Stmt]>,
    ) -> CResult<()> {
        let fin_id = if finally.is_some() {
            let f = self.f();
            f.num_finally += 1;
            Some(f.num_finally - 1)
        } else {
            None
        };
        // zend_compile_try: `label: try { }` must not be equal to
        // `try { label: }`, so a label right before the try gets a NOP of
        // its own (outside the try range).
        let here = self.here();
        if self.fref().labels.values().any(|(l, _)| *l == here) {
            self.emit(Op::Nop);
        }
        let try_start = self.here();
        if let Some(fid) = fin_id {
            self.f().finally_stack.push(fid);
            self.f().fast_call_patches_init(fid);
        }
        let tmps_start = self.f().num_tmps;
        self.compile_stmts(body)?;
        let try_tmps = (tmps_start, self.f().num_tmps);
        if let Some(fid) = fin_id {
            self.emit_fast_call(fid);
        }
        let mut end_jumps = vec![self.emit(Op::Jmp(0))];
        let try_end = self.here();
        let catch_start = if catches.is_empty() {
            None
        } else {
            Some(try_end)
        };
        let mut prev_catch: Option<usize> = None;
        for c in catches {
            if let Some(p) = prev_catch {
                self.patch_here(p);
            }
            self.f().line = c.span.line;
            let mut classes = Vec::new();
            for t in &c.types {
                // `zend_is_const_default_class_ref` in zend_compile_try.
                if t.is_special("self") || t.is_special("static") || t.is_special("parent") {
                    return self.err("Bad class name in the catch statement", c.span.line);
                }
                let n = self.resolve_class_name(t);
                classes.push(self.lit_str(&n));
            }
            let cv = match &c.var {
                Some(v) => {
                    if v.name == b"this" {
                        return self.err("Cannot re-assign $this", c.span.line);
                    }
                    Some(self.cv(&v.name))
                }
                None => None,
            };
            prev_catch = Some(self.emit(Op::Catch {
                classes: classes.into_boxed_slice(),
                cv,
                next: 0,
            }));
            self.compile_stmts(&c.body)?;
            if let Some(fid) = fin_id {
                self.emit_fast_call(fid);
            }
            end_jumps.push(self.emit(Op::Jmp(0)));
        }
        if let Some(p) = prev_catch {
            self.patch_here(p);
            self.emit(Op::Rethrow);
        }
        let catch_end = self.here();
        let mut finally_info = None;
        if let (Some(fid), Some(fbody)) = (fin_id, finally) {
            self.f().finally_stack.pop();
            let fin_start = self.here();
            let patches = self.f().fast_call_patches_take(fid);
            for p in patches {
                self.patch(p, fin_start);
            }
            self.f().in_finally += 1;
            self.compile_stmts(fbody)?;
            self.f().in_finally -= 1;
            self.emit(Op::FastRet { fin: fid });
            let fin_end = self.here();
            finally_info = Some((fid, fin_start, fin_end));
        }
        for j in end_jumps {
            self.patch_here(j);
        }
        let end = self.here();
        self.f().try_table.push((
            TryEntry {
                try_start,
                try_end,
                catch_start,
                catch_end,
                finally: finally_info,
                try_tmps,
            },
            end,
        ));
        Ok(())
    }

    /// Compiles an expression whose value is not used.
    pub(crate) fn compile_expr_discard(&mut self, e: &Expr) -> CResult<()> {
        self.f().line = e.span.line;
        match &e.kind {
            ExprKind::Assign { target, value } => {
                self.compile_assign(target, value, e.span.line, false)?;
            }
            ExprKind::AssignRef { target, value } => {
                self.compile_assign_ref(target, value, e.span.line, false)?;
            }
            ExprKind::AssignOp { .. } | ExprKind::IncDec { .. } => {
                let op = self.compile_expr_nodst(e)?;
                if let Some(op) = op {
                    self.free(op);
                }
            }
            ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::New { .. } => {
                let saved = std::mem::take(&mut self.f().sc_jumps);
                let r = self.compile_call_like(e, false);
                let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
                let op = r?;
                if jumps.is_empty() {
                    if let Some(op) = op {
                        self.free(op);
                    }
                } else {
                    let dst = self.tmp();
                    if let Some(op) = op {
                        self.emit(Op::Move { src: op, dst });
                    }
                    let here = self.here();
                    for j in jumps {
                        if let Op::JmpNull { dst: d, target, .. } = &mut self.f().ops[j] {
                            *d = dst;
                            *target = here;
                        }
                    }
                    self.free(Operand::Tmp(dst));
                }
            }
            ExprKind::VoidCast(inner) => {
                // The value counts as used (#[\NoDiscard]).
                let op = self.compile_expr(inner)?;
                self.free(op);
            }
            _ => {
                let op = self.compile_expr(e)?;
                self.free(op);
            }
        }
        Ok(())
    }

    pub(crate) fn free(&mut self, op: Operand) {
        if let Operand::Tmp(_) = op {
            self.emit(Op::Free(op));
        }
    }

    /// Assigns `val` to the target expression (variable or place).
    pub(crate) fn assign_to(
        &mut self,
        target: &Expr,
        val: Operand,
        dst: Option<u32>,
    ) -> CResult<()> {
        if let ExprKind::Var(VarName::Name(id)) = &target.kind {
            if id.name == b"this" {
                return self.err("Cannot re-assign $this", target.span.line);
            }
            // zend_try_compile_cv fails for auto-globals: they are assigned
            // through a global fetch (the place below), never a local CV.
            if !super::expr::is_superglobal(&id.name) {
                self.note_cv_write(&id.name);
                let cv = self.cv(&id.name);
                self.emit(Op::AssignCv { cv, val, dst });
                return Ok(());
            }
        }
        if let ExprKind::Array { items, .. } = &target.kind {
            self.compile_destructure(items, val, target.span.line)?;
            if let Some(d) = dst {
                let _ = d;
            }
            return Ok(());
        }
        let p = self.compile_place(target, false)?;
        let pid = self.place(p);
        self.emit(Op::Assign {
            place: pid,
            val,
            dst,
        });
        Ok(())
    }

    /// Unused helper kept for `ArgSrc` construction symmetry.
    pub(crate) fn arg_src_for(&mut self, e: &Expr) -> CResult<ArgSrc> {
        if let ExprKind::Assign { target, value } = &e.kind {
            // zend_compile_list_assign with references yields the source
            // VAR (a reference), sent with ZEND_SEND_VAR_NO_REF.
            if let ExprKind::Array { items, .. } = &target.kind {
                if (is_ref_expr(value) || super::expr::is_call_expr(value))
                    && items
                        .iter()
                        .flatten()
                        .any(|i| i.by_ref || super::expr::has_nested_ref(i))
                {
                    let op = self.compile_expr(e)?;
                    return Ok(ArgSrc::CallResult(op));
                }
            }
        }
        if is_ref_expr(e) {
            // zend_compile_args: a short-circuited chain is passed by
            // value, and its jumps end at the argument.
            if super::expr::contains_nullsafe(e) {
                let op = self.compile_expr(e)?;
                return Ok(ArgSrc::Val(op));
            }
            let temp = on_temporary(e);
            let p = self.compile_place(e, false)?;
            self.note_place_read(&p, e.span.line);
            let pid = self.place(p);
            Ok(if temp {
                ArgSrc::TempPlace(pid)
            } else {
                ArgSrc::Place(pid)
            })
        } else if matches!(
            e.kind,
            ExprKind::Call { .. }
                | ExprKind::MethodCall { .. }
                | ExprKind::StaticCall { .. }
                // `ZEND_ASSIGN_REF` yields a VAR holding the reference, sent
                // with SEND_VAR_NO_REF(_EX) like a call result.
                | ExprKind::AssignRef { .. }
        ) {
            let op = self.compile_expr(e)?;
            // zend_compile_args: a function call compiled into a builtin
            // instruction (`strlen()`, `func_get_args()`, ...) yields a
            // CONST/TMP, sent with SEND_VAL_EX ("could not be passed by
            // reference" for a by-reference parameter). A call folded to a
            // constant (`chr(65)`, `strlen("ab")`) emits nothing, so the
            // previous argument's call must not be mistaken for it.
            if matches!(e.kind, ExprKind::Call { .. })
                && (matches!(op, Operand::Lit(_))
                    || !matches!(self.fref().ops.last(), Some(Op::DoCall { .. })))
            {
                return Ok(ArgSrc::Val(op));
            }
            Ok(ArgSrc::CallResult(op))
        } else {
            let op = self.compile_expr(e)?;
            Ok(ArgSrc::Val(op))
        }
    }
}

/// Whether a dimension/property chain starts at a temporary expression
/// (a literal or an operator result, not a variable or a call): such a
/// fetch cannot be sent by reference.
fn on_temporary(e: &Expr) -> bool {
    let mut cur = e;
    let mut through_prop = false;
    loop {
        match &cur.kind {
            ExprKind::Dim { base, .. } => cur = &**base,
            ExprKind::Prop { object, .. } => {
                through_prop = true;
                cur = &**object;
            }
            _ => break,
        }
    }
    if std::ptr::eq(cur, e) {
        return false;
    }
    match &cur.kind {
        // `zend_mark_const_object_fetch`: a constant under a property
        // fetch is copied to a VAR (QM_ASSIGN) in write context (GH-10497).
        ExprKind::Const(_) | ExprKind::ClassConst { .. } => !through_prop,
        // Special-compiled ZEND_FUNC_GET_ARGS / ZEND_FUNC_NUM_ARGS yield a
        // TMP (GH-12102).
        ExprKind::Call { callee, args } => match &**callee {
            ast::Callee::Name(n) => {
                args.args.is_empty()
                    && matches!(
                        n.kind,
                        ast::NameKind::Unqualified | ast::NameKind::FullyQualified
                    )
                    && (n.name.eq_ignore_ascii_case(b"func_get_args")
                        || n.name.eq_ignore_ascii_case(b"func_num_args"))
            }
            ast::Callee::Expr(_) => false,
        },
        kind => matches!(
            kind,
            ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::String(_)
                | ExprKind::Interpolated(_)
                | ExprKind::Array { .. }
                | ExprKind::MagicConst(_)
                | ExprKind::Binary { .. }
                | ExprKind::Unary { .. }
                | ExprKind::Cast { .. }
                | ExprKind::Conditional { .. }
        ),
    }
}

/// Whether an expression denotes a variable that can be referenced.
pub(crate) fn is_ref_expr(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Var(_)
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. }
    )
}

impl super::FnCtx {
    pub(crate) fn fast_call_patches_init(&mut self, fin: u32) {
        let _ = fin;
    }
    pub(crate) fn fast_call_patches_push(&mut self, fin: u32, at: usize) {
        // Recorded as a pending patch keyed by finally id.
        self.pending_fast_calls().push((fin, at));
    }
    pub(crate) fn fast_call_patches_take(&mut self, fin: u32) -> Vec<usize> {
        let all = std::mem::take(self.pending_fast_calls());
        let (mine, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|(f, _)| *f == fin);
        *self.pending_fast_calls() = rest;
        mine.into_iter().map(|(_, a)| a).collect()
    }
}
