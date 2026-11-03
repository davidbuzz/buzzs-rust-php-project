//! Statement compilation.

use super::{CResult, Cleanup, Compiler, FnKind, LoopInfo, LoopKind};
use crate::ast::{self, Expr, ExprKind, Stmt, StmtKind, VarName};
use crate::opcode::{ArgSrc, BinKind, ForeachKind, Op, Operand, Place, PlaceBase, TryEntry};
use php_runtime::{Value, ZStr};

impl Compiler {
    /// Compiles the statements of a file.
    pub(crate) fn compile_top_statements(&mut self, stmts: &[Stmt]) -> CResult<()> {
        for (i, s) in stmts.iter().enumerate() {
            if let StmtKind::Declare { directives, .. } = &s.kind {
                for d in directives {
                    if d.name.eq_ignore_case("strict_types") && i != 0 {
                        // Only the very first statement may declare strict_types
                        // (an opening inline HTML is not allowed either).
                        let first_non_decl = stmts[..i].iter().any(|p| {
                            !matches!(p.kind, StmtKind::Declare { .. })
                        });
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
        for s in stmts {
            self.compile_stmt_top(s)?;
            if matches!(s.kind, StmtKind::HaltCompiler { .. }) {
                break;
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
                self.class_uses.clear();
                self.fn_uses.clear();
                self.const_uses.clear();
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
        self.f().line = s.span.line;
        match &s.kind {
            StmtKind::Block(b) => self.compile_stmts(b)?,
            StmtKind::If {
                branches,
                else_branch,
            } => {
                let mut end_jumps = Vec::new();
                for br in branches {
                    self.f().line = br.cond.span.line;
                    let c = self.compile_expr(&br.cond)?;
                    let jz = self.emit(Op::JmpZ(c, 0));
                    self.compile_stmt(&br.body)?;
                    end_jumps.push(self.emit(Op::Jmp(0)));
                    self.patch_here(jz);
                }
                if let Some(e) = else_branch {
                    self.compile_stmt(e)?;
                }
                for j in end_jumps {
                    self.patch_here(j);
                }
            }
            StmtKind::While { cond, body } => {
                let start = self.here();
                self.f().line = cond.span.line;
                let c = self.compile_expr(cond)?;
                let jz = self.emit(Op::JmpZ(c, 0));
                self.push_loop(LoopKind::Loop, Cleanup::None);
                self.compile_stmt(body)?;
                let info = self.pop_loop();
                self.emit(Op::Jmp(start));
                self.patch_here(jz);
                self.finish_loop(info, start);
            }
            StmtKind::DoWhile { body, cond } => {
                let start = self.here();
                self.push_loop(LoopKind::Loop, Cleanup::None);
                self.compile_stmt(body)?;
                let info = self.pop_loop();
                let cont = self.here();
                self.f().line = cond.span.line;
                let c = self.compile_expr(cond)?;
                self.emit(Op::JmpNz(c, start));
                self.finish_loop(info, cont);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                for e in init {
                    self.compile_expr_discard(e)?;
                }
                let start = self.here();
                let mut jz = None;
                for (i, e) in cond.iter().enumerate() {
                    if i + 1 == cond.len() {
                        let c = self.compile_expr(e)?;
                        jz = Some(self.emit(Op::JmpZ(c, 0)));
                    } else {
                        self.compile_expr_discard(e)?;
                    }
                }
                self.push_loop(LoopKind::Loop, Cleanup::None);
                self.compile_stmt(body)?;
                let info = self.pop_loop();
                let cont = self.here();
                for e in step {
                    self.compile_expr_discard(e)?;
                }
                self.emit(Op::Jmp(start));
                if let Some(j) = jz {
                    self.patch_here(j);
                }
                self.finish_loop(info, cont);
            }
            StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body,
            } => self.compile_foreach(subject, key.as_ref(), value, *by_ref, body)?,
            StmtKind::Switch { subject, cases } => self.compile_switch(subject, cases)?,
            StmtKind::Break(n) | StmtKind::Continue(n) => {
                let is_break = matches!(s.kind, StmtKind::Break(_));
                self.compile_break(is_break, n.as_ref(), s.span.line)?;
            }
            StmtKind::Return(e) => self.compile_return(e.as_ref(), s.span.line)?,
            StmtKind::Global(vars) => {
                for v in vars {
                    match &v.kind {
                        ExprKind::Var(VarName::Name(id)) => {
                            if id.name == b"this" {
                                return self.err("Cannot use $this as global variable", v.span.line);
                            }
                            let cv = self.cv(&id.name);
                            let name = self.lit_str(&id.name);
                            self.emit(Op::Global { cv, name });
                        }
                        ExprKind::Var(VarName::Expr(e)) => {
                            // global $$x: bind by runtime name.
                            let n = self.compile_expr(e)?;
                            let t = self.tmp();
                            let gl = self.lit_str(b"GLOBALS");
                            let _ = gl;
                            let place = self.place(Place {
                                base: PlaceBase::VarVar(n),
                                segs: Vec::new(),
                            });
                            let _ = t;
                            // Approximation: make the local a reference to the
                            // global through $GLOBALS semantics.
                            self.emit(Op::Nop);
                            let _ = place;
                        }
                        _ => {}
                    }
                }
            }
            StmtKind::Static(vars) => {
                for sv in &vars[..] {
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
                    self.emit(Op::InitStatic { cv, idx, val });
                    self.patch_here(bind);
                }
            }
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
            StmtKind::Unset(vars) => {
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
            }
            StmtKind::Declare { directives, body } => {
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
                    } else if d.name.eq_ignore_case("ticks") || d.name.eq_ignore_case("encoding") {
                        // Ticks are not supported; encoding is ignored.
                    } else {
                        self.warnings.push((
                            128,
                            format!("Unsupported declare '{}'", d.name.as_str_lossy()),
                            d.span.line,
                        ));
                    }
                }
                if let Some(b) = body {
                    self.compile_stmt(b)?;
                }
            }
            StmtKind::Nop | StmtKind::HaltCompiler { .. } => {}
            StmtKind::Try {
                body,
                catches,
                finally,
            } => self.compile_try(body, catches, finally.as_deref())?,
            StmtKind::Goto(label) => {
                let at = self.emit(Op::Jmp(0));
                let loops = self.f().loop_ids.clone();
                self.f()
                    .gotos
                    .push((at, label.name.clone(), s.span.line, loops));
            }
            StmtKind::Label(label) => {
                let here = self.here();
                let loops = self.f().loop_ids.clone();
                if self.f().labels.contains_key(&label.name) {
                    return self.err(
                        format!("Label '{}' already defined", label.as_str_lossy()),
                        s.span.line,
                    );
                }
                self.f().labels.insert(label.name.clone(), (here, loops));
            }
            StmtKind::Function(fd) => self.compile_function_decl(fd, false)?,
            StmtKind::Class(cd) => self.compile_class_stmt(cd, false)?,
            StmtKind::Const { consts, .. } => {
                for c in consts {
                    self.f().line = c.span.line;
                    let full = self.ns_prefixed_pub(&c.name.name);
                    let name = self.lit_str(&full);
                    let val = self.compile_expr(&c.value)?;
                    self.emit(Op::DeclareConst { name, val });
                }
            }
            StmtKind::Namespace { .. } => {
                // Nested namespaces are rejected by the parser.
                self.compile_stmt_top(s)?;
            }
            StmtKind::Use { kind, uses } => {
                for u in uses {
                    self.add_use(*kind, &u.name.name, u.alias.as_ref().map(|a| a.name.as_slice()), u.span.line)?;
                }
            }
            StmtKind::GroupUse { kind, prefix, uses } => {
                for u in uses {
                    let mut full = prefix.name.clone();
                    full.push(b'\\');
                    full.extend_from_slice(&u.name.name);
                    let k = kind.unwrap_or(u.kind);
                    self.add_use(k, &full, u.alias.as_ref().map(|a| a.name.as_slice()), u.span.line)?;
                }
            }
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

    fn add_use(
        &mut self,
        kind: ast::UseKind,
        full: &[u8],
        alias: Option<&[u8]>,
        line: u32,
    ) -> CResult<()> {
        let short = alias.map_or_else(
            || {
                full.rsplit(|&c| c == b'\\')
                    .next()
                    .unwrap_or(full)
                    .to_vec()
            },
            <[u8]>::to_vec,
        );
        match kind {
            ast::UseKind::Class => {
                let lc = short.to_ascii_lowercase();
                if matches!(lc.as_slice(), b"self" | b"parent" | b"static") {
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
                if let Some(prev) = self.class_uses.get(&lc) {
                    if !prev.eq_ignore_ascii_case(full) {
                        return self.err(
                            format!(
                                "Cannot use {} as {} because the name is already in use",
                                String::from_utf8_lossy(full),
                                String::from_utf8_lossy(&short)
                            ),
                            line,
                        );
                    }
                }
                self.class_uses.insert(lc, full.to_vec());
            }
            ast::UseKind::Function => {
                self.fn_uses.insert(short.to_ascii_lowercase(), full.to_vec());
            }
            ast::UseKind::Const => {
                self.const_uses.insert(short, full.to_vec());
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Loops
    // ------------------------------------------------------------------

    pub(crate) fn push_loop(&mut self, kind: LoopKind, cleanup: Cleanup) {
        let depth = self.fref().finally_stack.len();
        let f = self.f();
        f.loops.push(LoopInfo {
            kind,
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            cleanup,
            finally_depth: depth,
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
            // "continue" targeting switch is equivalent to "break".
            if depth == 1 {
                self.warnings.push((
                    128,
                    "\"continue\" targeting switch is equivalent to \"break\"".to_string(),
                    line,
                ));
            } else {
                self.warnings.push((
                    128,
                    format!("\"continue {depth}\" targeting switch is equivalent to \"break {depth}\""),
                    line,
                ));
            }
            is_break = true;
        }
        // Cleanups of crossed loops.
        let upto = if is_break { target_idx } else { target_idx + 1 };
        let cleanups: Vec<Cleanup> = self.fref().loops[upto..]
            .iter()
            .rev()
            .map(|l| l.cleanup)
            .collect();
        for c in cleanups {
            match c {
                Cleanup::None => {}
                Cleanup::FeFree(i) => {
                    self.emit(Op::FeFree { iter: i });
                }
                Cleanup::FreeTmp(t) => {
                    self.emit(Op::Free(Operand::Tmp(t)));
                }
            }
        }
        // Finally blocks crossed.
        let fdepth = self.fref().loops[target_idx].finally_depth;
        let fins: Vec<u32> = self.fref().finally_stack[fdepth..].iter().rev().copied().collect();
        for fin in fins {
            self.emit_fast_call(fin);
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
                if rt.is_void() && e.is_some() {
                    return self.err(
                        "A void function must not return a value",
                        line,
                    );
                }
                if rt.is_never() {
                    return self.err("A never-returning function must not return", line);
                }
                if e.is_none() && !rt.is_void() && self.fref().has_return_type {
                    let fname = self.fn_display_name();
                    return self.err(
                        format!(
                            "{fname}(): Return value must be of type {}, none returned",
                            rt.to_php_string()
                        ),
                        line,
                    );
                }
            }
        }
        let _ = kind;
        let fins: Vec<u32> = self.fref().finally_stack.iter().rev().copied().collect();
        if returns_ref && !is_gen {
            if let Some(ex) = e {
                if is_ref_expr(ex) {
                    let p = self.compile_place(ex, false)?;
                    let pid = self.place(p);
                    if fins.is_empty() {
                        self.emit(Op::ReturnRef { place: pid });
                        return Ok(());
                    }
                    let t = self.tmp();
                    self.emit(Op::MakeRef { place: pid, dst: t });
                    for fin in fins {
                        self.emit_fast_call(fin);
                    }
                    let tp = self.place(Place {
                        base: PlaceBase::Value(Operand::Tmp(t)),
                        segs: Vec::new(),
                    });
                    self.emit(Op::ReturnRef { place: tp });
                    return Ok(());
                }
            }
        }
        let mut val = match e {
            Some(ex) => self.compile_expr(ex)?,
            None => self.lit(Value::Null),
        };
        if e.is_none() {
            val = Operand::Unused;
        }
        if !fins.is_empty() {
            // Keep the value in a temporary across the finally blocks.
            let t = self.tmp();
            if val != Operand::Unused {
                self.emit(Op::Move { src: val, dst: t });
                val = Operand::Tmp(t);
            }
            for fin in fins {
                self.emit_fast_call(fin);
            }
        }
        self.emit(Op::Return { val });
        Ok(())
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
            let p = if subject.is_variable() && !matches!(subject.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. }) {
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
                self.compile_destructure(items, Operand::Tmp(vt), value.span.line)?;
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
        let (subj, cleanup) = match s {
            Operand::Tmp(t) => (Operand::TmpKeep(t), Cleanup::FreeTmp(t)),
            Operand::Cv(_) => {
                // Copy so that case expressions modifying the variable do
                // not change the subject.
                let t = self.tmp();
                self.emit(Op::Move { src: s, dst: t });
                (Operand::TmpKeep(t), Cleanup::FreeTmp(t))
            }
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
        let try_start = self.here();
        if let Some(fid) = fin_id {
            self.f().finally_stack.push(fid);
            self.f().fast_call_patches_init(fid);
        }
        self.compile_stmts(body)?;
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
                let lc = t.name.to_ascii_lowercase();
                if t.kind == ast::NameKind::Unqualified
                    && matches!(lc.as_slice(), b"self" | b"static" | b"parent")
                {
                    let spec = self.class_spec_name(t)?;
                    let _ = spec;
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
            ExprKind::VoidCast(inner) => self.compile_expr_discard(inner)?,
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
    pub(crate) fn assign_to(&mut self, target: &Expr, val: Operand, dst: Option<u32>) -> CResult<()> {
        if let ExprKind::Var(VarName::Name(id)) = &target.kind {
            if id.name == b"this" {
                return self.err("Cannot re-assign $this", target.span.line);
            }
            let cv = self.cv(&id.name);
            self.emit(Op::AssignCv { cv, val, dst });
            return Ok(());
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
        if is_ref_expr(e) {
            if let ExprKind::Prop { nullsafe: true, .. } = e.kind {
                let op = self.compile_expr(e)?;
                return Ok(ArgSrc::Val(op));
            }
            let p = self.compile_place(e, false)?;
            Ok(ArgSrc::Place(self.place(p)))
        } else if matches!(
            e.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. }
        ) {
            let op = self.compile_expr(e)?;
            Ok(ArgSrc::CallResult(op))
        } else {
            let op = self.compile_expr(e)?;
            Ok(ArgSrc::Val(op))
        }
    }
}

/// Whether an expression denotes a variable that can be referenced.
pub(crate) fn is_ref_expr(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Var(_) | ExprKind::Dim { .. } | ExprKind::Prop { .. } | ExprKind::StaticProp { .. }
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
