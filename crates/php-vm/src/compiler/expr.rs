// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Expression compilation.

use super::decl::is_superglobal_or_globals;
use super::stack;
use super::stmt::is_ref_expr;
use super::{CResult, Compiler, FnKind};
use crate::ast::{
    self, ArgKind, ArgList, ArrayItem, BinaryOp as AB, Callee, Expr, ExprKind, InterpPart,
    MagicConst, MemberName, NewClass, UnaryOp, VarName,
};
use crate::func::flags;
use crate::opcode::{
    type_mask, ArgSrc, BinKind, CastKind, ClassSpec, FetchMode, IncDecKind, IncludeKind, Label,
    MatchKey, MatchTable, NullChain, Op, Operand, Place, PlaceBase, Seg, UnKind,
};
use php_runtime::{BinaryOp, CollectingContext, Value, ZArray, ZStr};

/// `zend_is_auto_global()` at compile time: whether `name` is a
/// superglobal (always global). Like php-src, naming one arms its JIT
/// creation: the VM creates `$_ENV`/`$_REQUEST` (and a JIT `$_SERVER`)
/// once the compilation finishes (see `crate::auto_globals`).
pub(crate) fn is_superglobal(name: &[u8]) -> bool {
    name.first() == Some(&b'_') && crate::auto_globals::note_compiled(name)
}

/// Whether an interpolated part is a plain variable that compiles to a
/// bare CV operand without emitting code (`$name`; not `$this`,
/// `$GLOBALS` or a possible superglobal).
fn is_plain_cv_part(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Var(VarName::Name(id))
        if id.name != b"this" && id.name != b"GLOBALS" && id.name.first() != Some(&b'_'))
}

pub(crate) fn map_binop(op: AB) -> Option<BinaryOp> {
    Some(match op {
        AB::Add => BinaryOp::Add,
        AB::Sub => BinaryOp::Sub,
        AB::Mul => BinaryOp::Mul,
        AB::Div => BinaryOp::Div,
        AB::Mod => BinaryOp::Mod,
        AB::Pow => BinaryOp::Pow,
        AB::Concat => BinaryOp::Concat,
        AB::ShiftLeft => BinaryOp::ShiftLeft,
        AB::ShiftRight => BinaryOp::ShiftRight,
        AB::BitOr => BinaryOp::BitOr,
        AB::BitAnd => BinaryOp::BitAnd,
        AB::BitXor => BinaryOp::BitXor,
        AB::BoolXor => BinaryOp::BoolXor,
        _ => return None,
    })
}

/// The `op_type` order php-src's `zend_vm_set_opcode_handler` compares to
/// put the operands of a `SPEC(COMMUTATIVE)` opcode (`ZEND_MUL`,
/// `ZEND_BW_OR`/`AND`/`XOR`) in canonical order: `IS_CONST` <
/// `IS_TMP_VAR` < `IS_CV` (`IS_VAR` results are temporaries here). A
/// swapped pair converts — and reports unsupported operand types — in
/// swapped order (`operator_unsupported_types.phpt`).
fn operand_rank(op: Operand) -> u8 {
    match op {
        Operand::Unused => 0,
        Operand::Lit(_) => 1,
        Operand::Tmp(_) | Operand::TmpKeep(_) => 2,
        Operand::Cv(_) => 8,
    }
}

impl Compiler<'_> {
    fn parent_hook_property(&self, class: &ast::ClassRef, name: &MemberName) -> Option<ZStr> {
        let ast::ClassRef::Expr(expr) = class else {
            return None;
        };
        let ExprKind::StaticProp {
            class,
            name: prop,
            parenthesized: false,
        } = &expr.kind
        else {
            return None;
        };
        if !matches!(class.as_ref(), ast::ClassRef::Name(parent) if parent.is_special("parent"))
            || !matches!(name, MemberName::Ident(kind) if kind.eq_ignore_case("get") || kind.eq_ignore_case("set"))
        {
            return None;
        }
        match prop {
            VarName::Name(prop) => Some(ZStr::from(prop.name.as_slice())),
            VarName::Expr(expr) => {
                if !matches!(
                    expr.kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
                ) {
                    return None;
                }
                self.try_fold(expr)?
                    .to_zstr(&mut CollectingContext::new())
                    .ok()
            }
        }
    }

    /// Compiles an expression and returns the operand holding its value.
    pub(crate) fn compile_expr(&mut self, e: &Expr) -> CResult<Operand> {
        let saved = std::mem::take(&mut self.f().sc_jumps);
        let saved_quiet = std::mem::replace(&mut self.f().sc_quiet, false);
        let r = self.compile_expr_inner(e);
        self.f().sc_quiet = saved_quiet;
        let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
        let op = r?;
        if jumps.is_empty() {
            return Ok(op);
        }
        // `zend_short_circuiting_commit()`: the jumps write the result of
        // the chain's last instruction.
        if let Some(t) = self.last_result(op) {
            self.finish_short_circuit(jumps, t);
            return Ok(op);
        }
        let dst = self.tmp();
        self.emit(Op::Move { src: op, dst });
        self.finish_short_circuit(jumps, dst);
        Ok(Operand::Tmp(dst))
    }

    /// The temporary `op` names when the last instruction emitted defines
    /// it.
    pub(crate) fn last_result(&mut self, op: Operand) -> Option<u32> {
        let Operand::Tmp(t) = op else {
            return None;
        };
        let last = self.f().ops.last()?;
        (super::optimizer::def_tmp(last) == Some(t)).then_some(t)
    }

    pub(crate) fn finish_short_circuit(&mut self, jumps: Vec<usize>, dst: u32) {
        self.commit_short_circuit(jumps, dst, None);
    }

    /// Points the chain's jumps past its last instruction, writing `dst`,
    /// as `isset()` / `empty()` when `chain` says so.
    fn commit_short_circuit(&mut self, jumps: Vec<usize>, dst: u32, chain: Option<NullChain>) {
        let here = self.here();
        for j in jumps {
            if let Op::JmpNull {
                dst: d,
                target,
                chain: c,
                ..
            } = &mut self.f().ops[j]
            {
                *d = dst;
                *target = here;
                if let Some(chain) = chain {
                    *c = chain;
                }
            }
        }
    }

    /// The short-circuit kind of a `JMP_NULL` emitted now
    /// (`ZEND_JMP_NULL_BP_VAR_IS` in a quiet fetch).
    fn null_chain(&mut self) -> NullChain {
        if self.f().sc_quiet {
            NullChain::Quiet
        } else {
            NullChain::Expr
        }
    }

    /// Compiles a chain element, sharing the current nullsafe jump list.
    fn compile_chain(&mut self, e: &Expr) -> CResult<Operand> {
        match e.kind {
            ExprKind::Prop { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::Call { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::Dim { .. }
            | ExprKind::StaticProp { .. } => self.compile_expr_inner(e),
            _ => self.compile_expr(e),
        }
    }

    /// The class of `X::$p` / `X::m()`: a dynamic class expression belongs
    /// to the enclosing nullsafe chain (`zend_ast_is_short_circuited`
    /// follows `child[0]` of `ZEND_AST_STATIC_PROP` / `ZEND_AST_STATIC_CALL`).
    fn chain_class_spec(&mut self, cref: &ast::ClassRef) -> CResult<ClassSpec> {
        match cref {
            ast::ClassRef::Expr(e) => {
                // `zend_compile_class_ref`, as in `Compiler::class_spec`.
                if !matches!(self.try_fold(e), None | Some(Value::Str(_))) {
                    return self.err("Illegal class name", e.span.line);
                }
                Ok(ClassSpec::Dyn(self.compile_chain(e)?))
            }
            ast::ClassRef::Name(_) => self.class_spec(cref),
        }
    }

    /// The place of an `isset()` / `empty()` / `??` operand with the
    /// short-circuit jumps of its own nullsafe chain. Nullsafe bases are
    /// fetched quietly (`zend_emit_jmp_null` after a `BP_VAR_IS` fetch).
    fn compile_is_place(&mut self, e: &Expr) -> CResult<(Place, Vec<usize>)> {
        let saved = std::mem::take(&mut self.f().sc_jumps);
        let saved_quiet = std::mem::replace(&mut self.f().sc_quiet, true);
        let p = self.compile_place(e, false);
        self.f().sc_quiet = saved_quiet;
        let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
        Ok((p?, jumps))
    }

    /// Compiles an assignment-like expression without a result.
    pub(crate) fn compile_expr_nodst(&mut self, e: &Expr) -> CResult<Option<Operand>> {
        match &e.kind {
            ExprKind::AssignOp { op, target, value } => {
                self.compile_assign_op(*op, target, value, false, e.span.line)
            }
            ExprKind::IncDec { op, var } => {
                let p = self.compile_incdec_place(var)?;
                let pid = self.place(p);
                self.emit(Op::IncDec {
                    place: pid,
                    kind: map_incdec(*op),
                    dst: None,
                });
                Ok(None)
            }
            _ => self.compile_expr(e).map(Some),
        }
    }

    /// One `zend_compile_expr()` level: its `zend_check_stack_limit()`,
    /// then the expression.
    fn compile_expr_inner(&mut self, e: &Expr) -> CResult<Operand> {
        let bytes = stack::expr_frame_bytes(&e.kind);
        if !self.stack.enter(bytes) {
            return self.stack_limit_error(e.span.line);
        }
        let r = self.compile_expr_body(e);
        self.stack.leave(bytes);
        r
    }

    #[allow(clippy::too_many_lines)]
    fn compile_expr_body(&mut self, e: &Expr) -> CResult<Operand> {
        let line = e.span.line;
        self.f().line = line;
        if let Some(v) = self.try_fold(e) {
            return Ok(self.lit(v));
        }
        if self.stack.overflowed() {
            // `zend_eval_const_expr()` ran out of stack.
            return self.stack_limit_error(line);
        }
        Ok(match &e.kind {
            ExprKind::Int(i) => self.lit(Value::Int(*i)),
            ExprKind::Float(f) => self.lit(Value::Float(*f)),
            ExprKind::String(s) => self.lit(Value::Str(ZStr::from(s.as_slice()))),
            ExprKind::Interpolated(parts) => self.compile_rope(parts)?,
            ExprKind::ShellExec(parts) => {
                let r = self.compile_rope(parts)?;
                let name = self.lit_str(b"shell_exec");
                self.emit(Op::InitFCall {
                    name,
                    fallback: None,
                    display: name,
                });
                self.emit(Op::Send {
                    src: ArgSrc::Val(r),
                });
                let t = self.tmp();
                self.emit(Op::DoCall { dst: Some(t) });
                Operand::Tmp(t)
            }
            ExprKind::Array { items, syntax } => {
                if *syntax == ast::ArraySyntax::List {
                    return self.err("Cannot use list() outside of assignment context", line);
                }
                self.compile_array(items, line)?
            }
            ExprKind::Const(name) => {
                let (n, fb) = self.resolve_const_name(name);
                let lc = n.to_ascii_lowercase();
                if name.kind != ast::NameKind::Qualified && !n.contains(&b'\\') || fb.is_some() {
                    let last = fb.as_ref().unwrap_or(&n).to_ascii_lowercase();
                    match last.as_slice() {
                        b"true" => return Ok(self.lit(Value::Bool(true))),
                        b"false" => return Ok(self.lit(Value::Bool(false))),
                        b"null" => return Ok(self.lit(Value::Null)),
                        _ => {}
                    }
                }
                // `zend_try_ct_eval_const()`: persistent internal constants
                // are substituted at compile time.
                if fb.is_none() {
                    if let Some(v) = self.ct_eval_const(&n) {
                        return Ok(self.lit(v));
                    }
                }
                if lc.as_slice() == b"__compiler_halt_offset__" {
                    let n = self.lit_str(b"__COMPILER_HALT_OFFSET__");
                    let t = self.tmp();
                    self.emit(Op::FetchConst {
                        name: n,
                        fallback: None,
                        dst: t,
                    });
                    return Ok(Operand::Tmp(t));
                }
                let name_l = self.lit_str(&n);
                let fb_l = fb.map(|f| self.lit_str(&f));
                let t = self.tmp();
                self.emit(Op::FetchConst {
                    name: name_l,
                    fallback: fb_l,
                    dst: t,
                });
                Operand::Tmp(t)
            }
            ExprKind::MagicConst(m) => self.compile_magic(*m, line),
            ExprKind::ClassConst { class, name } => {
                let spec = self.class_spec(class)?;
                let n = self.member_name(name)?;
                let t = self.tmp();
                self.emit(Op::FetchClassConst {
                    class: spec,
                    name: n,
                    dst: t,
                });
                Operand::Tmp(t)
            }
            ExprKind::ClassName(class) => {
                if let ast::ClassRef::Name(n) = &**class {
                    let lc = n.name.to_ascii_lowercase();
                    if n.kind == ast::NameKind::Unqualified
                        && !matches!(lc.as_slice(), b"self" | b"static" | b"parent")
                        || n.kind != ast::NameKind::Unqualified
                    {
                        let r = self.resolve_class_name(n);
                        return Ok(self.lit(Value::Str(ZStr::from(r))));
                    }
                    // `zend_try_compile_const_expr_resolve_class_name()`
                    // checks `X::class` even in constant expressions.
                    if self.in_const_expr() {
                        let what = String::from_utf8_lossy(&lc).into_owned();
                        self.ensure_valid_class_fetch(&what, n.span.line)?;
                    }
                    if lc.as_slice() == b"self" {
                        if let Some(c) = self.current_class() {
                            if !c.is_trait && !c.is_anon && self.closure_depth == 0 {
                                let name = c.name.clone();
                                return Ok(self.lit(Value::Str(name)));
                            }
                        }
                    }
                }
                if let ast::ClassRef::Expr(inner) = &**class {
                    self.check_class_name_operand(inner)?;
                }
                let spec = self.class_spec(class)?;
                let t = self.tmp();
                let const_expr = self.in_evaluated_const_expr();
                self.emit(Op::FetchClassName {
                    class: spec,
                    dst: t,
                    const_expr,
                });
                Operand::Tmp(t)
            }
            ExprKind::Var(VarName::Name(id)) => {
                if id.name == b"this" {
                    let t = self.tmp();
                    self.emit(Op::FetchThis { dst: t });
                    Operand::Tmp(t)
                } else if is_superglobal(&id.name) {
                    let n = self.lit(Value::Str(ZStr::from(id.name.as_slice())));
                    self.fetch_place(
                        Place {
                            base: PlaceBase::Global(n),
                            segs: Vec::new(),
                        },
                        FetchMode::Read,
                    )
                } else if id.name == b"GLOBALS" {
                    self.fetch_place(
                        Place {
                            base: PlaceBase::GlobalsArray,
                            segs: Vec::new(),
                        },
                        FetchMode::Read,
                    )
                } else {
                    self.note_cv_read(&id.name, line);
                    Operand::Cv(self.cv(&id.name))
                }
            }
            ExprKind::Var(VarName::Expr(_))
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. } => {
                if let ExprKind::Dim { base, index: None } = &e.kind {
                    // zend_delayed_compile_dim checks `$GLOBALS[]` first.
                    if matches!(&base.kind, ExprKind::Var(VarName::Name(id)) if id.name == b"GLOBALS")
                    {
                        return self.err("Cannot append to $GLOBALS", line);
                    }
                    return self.err("Cannot use [] for reading", line);
                }
                let p = self.compile_place(e, false)?;
                self.note_place_read(&p, line);
                self.fetch_place(p, FetchMode::Read)
            }
            ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::New { .. } => self.compile_call_like(e, true)?.unwrap_or(Operand::Unused),
            ExprKind::Assign { target, value } => self
                .compile_assign(target, value, line, true)?
                .unwrap_or(Operand::Unused),
            ExprKind::AssignRef { target, value } => {
                self.compile_assign_ref(target, value, line, true)?
            }
            ExprKind::AssignOp { op, target, value } => self
                .compile_assign_op(*op, target, value, true, line)?
                .unwrap_or(Operand::Unused),
            ExprKind::Binary { op, left, right } => self.compile_binary(*op, left, right)?,
            ExprKind::Unary { op, expr } => {
                // `zend_ast_create_1()` gives a prefix operator's node the
                // line of its operand, and the opcode is emitted after the
                // operand is compiled: keep the line that left.
                let a = self.compile_expr(expr)?;
                let t = self.tmp();
                match op {
                    UnaryOp::Minus => {
                        let m1 = self.lit(Value::Int(-1));
                        self.emit(Op::Bin {
                            op: BinKind::Arith(BinaryOp::Mul),
                            a,
                            b: m1,
                            dst: t,
                        });
                    }
                    UnaryOp::Plus => {
                        let one = self.lit(Value::Int(1));
                        self.emit(Op::Bin {
                            op: BinKind::Arith(BinaryOp::Mul),
                            a,
                            b: one,
                            dst: t,
                        });
                    }
                    UnaryOp::Not => {
                        self.emit(Op::Unary {
                            op: UnKind::Not,
                            a,
                            dst: t,
                        });
                    }
                    UnaryOp::BitNot => {
                        self.emit(Op::Unary {
                            op: UnKind::BitNot,
                            a,
                            dst: t,
                        });
                    }
                }
                Operand::Tmp(t)
            }
            ExprKind::IncDec { op, var } => {
                let p = self.compile_incdec_place(var)?;
                let pid = self.place(p);
                let t = self.tmp();
                self.emit(Op::IncDec {
                    place: pid,
                    kind: map_incdec(*op),
                    dst: Some(t),
                });
                Operand::Tmp(t)
            }
            ExprKind::Cast { kind, expr } => {
                // `zend_ast_create_1()` gives a prefix operator's node the
                // line of its operand, and the opcode is emitted after the
                // operand is compiled: keep the line that left.
                let a = self.compile_expr(expr)?;
                let t = self.tmp();
                let k = match kind {
                    ast::CastKind::Int => CastKind::Int,
                    ast::CastKind::Float => CastKind::Float,
                    ast::CastKind::String => CastKind::String,
                    ast::CastKind::Array => CastKind::Array,
                    ast::CastKind::Object => CastKind::Object,
                    ast::CastKind::Bool => CastKind::Bool,
                    ast::CastKind::Unset => {
                        return self.err("The (unset) cast is no longer supported", line);
                    }
                };
                self.emit(Op::Cast { kind: k, a, dst: t });
                Operand::Tmp(t)
            }
            ExprKind::VoidCast(inner) => {
                // ZEND_AST_CAST_VOID: the value counts as used (#[\NoDiscard]).
                let op = self.compile_expr(inner)?;
                self.free(op);
                self.lit(Value::Null)
            }
            // The parser makes `isset($a, $b, $c)` the `ZEND_AST_AND` chain
            // `(isset($a) && isset($b)) && isset($c)`, compiled by
            // `zend_compile_short_circuiting`; one variable is its
            // `ZEND_ISSET_ISEMPTY_*` alone.
            ExprKind::Isset(vars) => {
                let Some((first, rest)) = vars.split_first() else {
                    return Ok(self.lit(Value::Bool(true)));
                };
                let mut acc = self.compile_isset_one(first)?;
                for v in rest {
                    let t = self.tmp();
                    let j = self.emit(Op::JmpZEx {
                        a: acc,
                        dst: t,
                        target: 0,
                    });
                    let r = self.compile_isset_one(v)?;
                    self.emit(Op::Unary {
                        op: UnKind::Bool,
                        a: r,
                        dst: t,
                    });
                    self.patch_here(j);
                    acc = Operand::Tmp(t);
                }
                acc
            }
            ExprKind::Empty(inner) => {
                let t = self.tmp();
                if is_ref_expr(inner)
                    && !matches!(inner.kind, ExprKind::Var(VarName::Name(ref id)) if id.name == b"this")
                {
                    let (p, jumps) = self.compile_is_place(inner)?;
                    let pid = self.place(p);
                    self.emit(Op::Empty { place: pid, dst: t });
                    self.commit_short_circuit(jumps, t, Some(NullChain::Empty));
                } else {
                    let a = self.compile_expr(inner)?;
                    self.emit(Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t,
                    });
                }
                Operand::Tmp(t)
            }
            ExprKind::Silence(inner) => {
                let saved = self.tmp();
                self.user_opcode_hook("ZEND_BEGIN_SILENCE");
                self.emit(Op::BeginSilence { dst: saved });
                let v = self.compile_expr(inner)?;
                let v = match v {
                    Operand::Cv(_) => {
                        let t = self.tmp();
                        self.emit(Op::Move { src: v, dst: t });
                        Operand::Tmp(t)
                    }
                    o => o,
                };
                self.user_opcode_hook("ZEND_END_SILENCE");
                self.emit(Op::EndSilence {
                    saved: Operand::Tmp(saved),
                });
                v
            }
            ExprKind::Print(inner) => {
                let v = self.compile_expr(inner)?;
                self.emit(Op::Echo(v));
                self.lit(Value::Int(1))
            }
            ExprKind::Include { kind, expr } => {
                let a = self.compile_expr(expr)?;
                // `zend_ast_create_1()` takes the operand's line, so a
                // multi-line `eval(\n$code)` reports the operand's line.
                self.f().line = expr.span.line;
                let t = self.tmp();
                let k = match kind {
                    ast::IncludeKind::Include => IncludeKind::Include,
                    ast::IncludeKind::IncludeOnce => IncludeKind::IncludeOnce,
                    ast::IncludeKind::Require => IncludeKind::Require,
                    ast::IncludeKind::RequireOnce => IncludeKind::RequireOnce,
                    ast::IncludeKind::Eval => IncludeKind::Eval,
                };
                self.emit(Op::Include { kind: k, a, dst: t });
                Operand::Tmp(t)
            }
            ExprKind::Instanceof { expr, class } => {
                // `zend_compile_instanceof`: a constant operand is false.
                if self.try_fold(expr).is_some() {
                    return Ok(self.lit(Value::Bool(false)));
                }
                let a = self.compile_expr(expr)?;
                let spec = self.class_spec(class)?;
                let t = self.tmp();
                self.emit(Op::InstanceOf {
                    a,
                    class: spec,
                    dst: t,
                });
                Operand::Tmp(t)
            }
            ExprKind::Conditional {
                cond, then, else_, ..
            } => {
                let c = self.compile_expr(cond)?;
                let t = self.tmp();
                if let Some(th) = then {
                    let jz = self.emit(Op::JmpZ(c, 0));
                    let v = self.compile_expr(th)?;
                    self.emit(Op::Move { src: v, dst: t });
                    let j = self.emit(Op::Jmp(0));
                    self.patch_here(jz);
                    let v2 = self.compile_expr(else_)?;
                    self.emit(Op::Move { src: v2, dst: t });
                    self.patch_here(j);
                } else {
                    let js = self.emit(Op::JmpSet {
                        a: c,
                        dst: t,
                        target: 0,
                    });
                    let v2 = self.compile_expr(else_)?;
                    self.emit(Op::Move { src: v2, dst: t });
                    self.patch_here(js);
                }
                Operand::Tmp(t)
            }
            ExprKind::Yield { key, value } => {
                self.check_yield(line)?;
                let k = match key {
                    Some(k) => Some(self.compile_expr(k)?),
                    None => None,
                };
                let t = self.tmp();
                if self.fref().returns_ref {
                    if let Some(v) = value {
                        // zend_compile_yield: zend_assert_not_short_circuited.
                        if contains_nullsafe(v) {
                            return self.err("Cannot take reference of a nullsafe chain", line);
                        }
                        if is_ref_expr(v) {
                            let p = self.compile_place(v, false)?;
                            let pid = self.place(p);
                            self.emit(Op::YieldRef {
                                key: k,
                                place: pid,
                                dst: Some(t),
                            });
                            return Ok(Operand::Tmp(t));
                        }
                        // A constant, a temporary or a call result: ZEND_YIELD
                        // notices unless a call (ZEND_RETURNS_FUNCTION)
                        // returned a reference.
                        let call = is_call_expr(v);
                        // ZEND_YIELD is emitted on the line compiling the
                        // value left (the node has the operand's line).
                        let val = self.compile_expr(v)?;
                        self.emit(Op::YieldByRefValue {
                            key: k,
                            val,
                            call,
                            dst: Some(t),
                        });
                        return Ok(Operand::Tmp(t));
                    }
                }
                // With a value, ZEND_YIELD is emitted on the line compiling
                // it left; a bare `yield` is on its own line.
                let v = match value {
                    Some(v) => Some(self.compile_expr(v)?),
                    None => {
                        self.f().line = line;
                        None
                    }
                };
                self.emit(Op::Yield {
                    key: k,
                    val: v,
                    dst: Some(t),
                });
                Operand::Tmp(t)
            }
            ExprKind::YieldFrom(inner) => {
                self.check_yield(line)?;
                if self.fref().returns_ref {
                    // zend_compile_yield_from.
                    return self.err(
                        "Cannot use \"yield from\" inside a by-reference generator",
                        line,
                    );
                }
                // On the operand's line, as a prefix operator.
                let v = self.compile_expr(inner)?;
                let t = self.tmp();
                self.emit(Op::YieldFrom {
                    src: v,
                    dst: Some(t),
                });
                Operand::Tmp(t)
            }
            ExprKind::Throw(inner) => {
                // On the operand's line, as a prefix operator.
                let v = self.compile_expr(inner)?;
                self.emit(Op::Throw(v));
                self.lit(Value::Null)
            }
            ExprKind::Closure(c) => self.compile_closure(c)?,
            ExprKind::ArrowFn(a) => self.compile_arrow_fn(a)?,
            ExprKind::Match { subject, arms } => self.compile_match(subject, arms)?,
            ExprKind::Pipe { left, right } => self
                .compile_pipe(left, right, line, true)?
                .unwrap_or(Operand::Unused),
        })
    }

    fn check_yield(&mut self, line: u32) -> CResult<()> {
        let k = self.fref().kind;
        if matches!(k, FnKind::Main | FnKind::ConstExpr) {
            return self.err(
                "The \"yield\" expression can only be used inside a function",
                line,
            );
        }
        Ok(())
    }

    pub(crate) fn fetch_place(&mut self, p: Place, mode: FetchMode) -> Operand {
        let lines = self.f().place_lines.take();
        let p = match (mode, lines) {
            (FetchMode::Read, Some((q, lines))) if q == p && lines.len() == p.segs.len() => {
                self.split_read_by_line(p, &lines)
            }
            _ => p,
        };
        let pid = self.place(p);
        let t = self.tmp();
        self.emit(Op::Fetch {
            place: pid,
            mode,
            dst: t,
        });
        Operand::Tmp(t)
    }

    /// A read of a chain whose segments lie on different lines
    /// (`$o->a\n->b`, `$a['x']\n['y']`): php-src emits one FETCH opline
    /// per segment, each on the line the compiler was on after the
    /// segment's offset or name (`zend_delayed_compile_dim()` /
    /// `zend_delayed_compile_prop()`), so a warning or error of a segment
    /// names that segment's line. The segments before each line change
    /// are fetched into a temporary on their own line, and the rest of
    /// the chain reads on from it, the last piece on its own line; a
    /// chain on one line stays one place. Returns the last piece.
    fn split_read_by_line(&mut self, p: Place, lines: &[u32]) -> Place {
        let known = |l: &u32| *l != 0;
        if !lines.iter().all(known) || lines.windows(2).all(|w| w[0] == w[1]) {
            return p;
        }
        let Place { mut base, segs } = p;
        let mut start = 0;
        for (k, w) in lines.windows(2).enumerate() {
            if w[0] == w[1] {
                continue;
            }
            let end = k + 1;
            self.f().line = w[0];
            let piece = Place {
                base,
                segs: segs[start..end].to_vec(),
            };
            let pid = self.place(piece);
            let t = self.tmp();
            self.emit(Op::Fetch {
                place: pid,
                mode: FetchMode::Read,
                dst: t,
            });
            base = PlaceBase::Value(Operand::Tmp(t));
            start = end;
        }
        if let Some(&last) = lines.last() {
            self.f().line = last;
        }
        Place {
            base,
            segs: segs[start..].to_vec(),
        }
    }

    /// `zend_compile_encaps_list` (`Zend/zend_compile.c`). php-src converts
    /// a part to a string as soon as it has been evaluated, in its
    /// `ZEND_ROPE_INIT` / `ZEND_ROPE_ADD` opcode on the line the compiler
    /// is on after compiling it (`CG(zend_lineno)`); only the last part is
    /// converted by `ZEND_ROPE_END`. Our `Rope` op reads and converts its
    /// operands in order when it runs, so a part can be left to it when no
    /// later part runs code before the rope (nothing can observe the delay)
    /// and the rope's line is the line the part would have been converted
    /// on. Every other non-final part is converted right after its code by
    /// a `Cast`, which keeps "Array to string conversion", `__toString`
    /// calls and their exceptions in php-src's order and on php-src's line.
    /// A one-part rope is that cast (`zend_compile_rope_finalize`) and an
    /// all-literal one is its constant.
    fn compile_rope(&mut self, parts: &[InterpPart]) -> CResult<Operand> {
        let last_expr = parts
            .iter()
            .rposition(|p| matches!(p, InterpPart::Expr { .. }));
        // The last part whose code runs before the rope (reading a plain CV
        // emits none).
        let last_code = parts
            .iter()
            .rposition(|p| matches!(p, InterpPart::Expr { expr, .. } if !is_plain_cv_part(expr)));
        // The line the rope is emitted on when its last part is a plain CV:
        // the line that CV is read on. A code-emitting last part is final,
        // so only it is left to the rope.
        let rope_line = last_expr.and_then(|i| match &parts[i] {
            InterpPart::Expr { expr, .. } if is_plain_cv_part(expr) => Some(expr.span.line),
            _ => None,
        });
        let mut ops = Vec::with_capacity(parts.len());
        for (i, p) in parts.iter().enumerate() {
            match p {
                InterpPart::Literal(s, _) => {
                    ops.push(self.lit(Value::Str(ZStr::from(s.as_slice()))));
                }
                InterpPart::Expr { expr, .. } => {
                    let v = self.compile_expr(expr)?;
                    // `self.f().line` is now where php-src would convert it.
                    let deferred = Some(i) == last_expr
                        || (last_code.is_none_or(|l| i >= l) && Some(self.f().line) == rope_line);
                    ops.push(if deferred { v } else { self.cast_to_string(v) });
                }
            }
        }
        if ops.len() <= 1 {
            return Ok(match ops.first().copied() {
                None => self.lit(Value::Str(ZStr::empty())),
                Some(part @ Operand::Lit(_)) => part,
                Some(part) => self.cast_to_string(part),
            });
        }
        let t = self.tmp();
        self.emit(Op::Rope {
            parts: ops.into_boxed_slice(),
            dst: t,
        });
        Ok(Operand::Tmp(t))
    }

    /// `ZEND_CAST` to string of `v`, on the current line.
    fn cast_to_string(&mut self, v: Operand) -> Operand {
        let dst = self.tmp();
        self.emit(Op::Cast {
            kind: CastKind::String,
            a: v,
            dst,
        });
        Operand::Tmp(dst)
    }

    fn compile_array(&mut self, items: &[Option<ArrayItem>], line: u32) -> CResult<Operand> {
        if items.iter().flatten().any(|it| it.unpack) {
            self.check_const_array_unpack(items, line)?;
        }
        if items.iter().flatten().any(|it| {
            matches!(
                it.key.as_ref().map(|k| &k.kind),
                Some(ExprKind::Array { .. })
            )
        }) {
            self.check_ct_array_keys(items, line)?;
        }
        let t = self.tmp();
        self.emit(Op::InitArray { dst: t });
        for it in items {
            let Some(it) = it else {
                return self.err("Cannot use empty array elements in arrays", line);
            };
            self.f().line = it.span.line;
            if it.unpack {
                if it.by_ref {
                    return self.err("Cannot assign reference to non referenceable value", line);
                }
                let v = self.compile_expr(&it.value)?;
                if self.in_const_expr() || self.in_const_decl {
                    self.emit(Op::ConstArrayUnpack { arr: t, src: v });
                } else {
                    self.emit(Op::ArrayUnpack { arr: t, src: v });
                }
                continue;
            }
            let key = match &it.key {
                Some(k) => Some(self.compile_expr(k)?),
                None => None,
            };
            if it.by_ref {
                if !is_ref_expr(&it.value) {
                    return self.err(
                        "Cannot create references to/from string offsets nor overloaded objects",
                        line,
                    );
                }
                let p = self.compile_place(&it.value, false)?;
                let pid = self.place(p);
                self.emit(Op::ArrayAddRef {
                    arr: t,
                    key,
                    place: pid,
                });
            } else {
                let v = self.compile_expr(&it.value)?;
                self.emit(Op::ArrayAdd {
                    arr: t,
                    key,
                    val: v,
                });
            }
        }
        Ok(Operand::Tmp(t))
    }

    /// `zend_try_ct_eval_array`: an array literal whose elements are all
    /// compile-time constants is evaluated while compiling, so unpacking a
    /// constant non-array (`[...42]`) is a compile error rather than the
    /// runtime `Error` of `ZEND_ADD_ARRAY_UNPACK`.
    #[cold]
    fn check_const_array_unpack(&self, items: &[Option<ArrayItem>], line: u32) -> CResult<()> {
        let mut bad = None;
        for it in items {
            // A hole is reported by `compile_array` itself.
            let Some(it) = it else { return Ok(()) };
            if it.unpack {
                let Some(v) = self.try_fold(&it.value) else {
                    return Ok(());
                };
                if bad.is_none() && !matches!(v, Value::Array(_)) {
                    bad = Some(v);
                }
            } else if it.by_ref
                || self.try_fold(&it.value).is_none()
                || it.key.as_ref().is_some_and(|k| self.try_fold(k).is_none())
            {
                return Ok(());
            }
        }
        match bad {
            Some(v) => self.err(
                format!(
                    "Only arrays and Traversables can be unpacked, {} given",
                    crate::typecheck::given_type(&v)
                ),
                line,
            ),
            None => Ok(()),
        }
    }

    fn compile_magic(&mut self, m: MagicConst, line: u32) -> Operand {
        let s: Vec<u8> = match m {
            MagicConst::Line => return self.lit(Value::Int(i64::from(line))),
            MagicConst::File => self.filename.as_bytes().to_vec(),
            MagicConst::Dir => {
                let f = self.filename.as_bytes();
                dirname(f)
            }
            MagicConst::Namespace => self.namespace.clone(),
            MagicConst::Class => {
                if let Some(c) = self.current_class() {
                    if c.is_trait {
                        let t = self.tmp();
                        self.emit(Op::FetchClassName {
                            class: ClassSpec::SelfClass,
                            dst: t,
                            const_expr: false,
                        });
                        return Operand::Tmp(t);
                    }
                    c.name.as_bytes().to_vec()
                } else {
                    Vec::new()
                }
            }
            MagicConst::Trait => match self.current_class() {
                Some(c) if c.is_trait => c.name.as_bytes().to_vec(),
                _ => Vec::new(),
            },
            MagicConst::Function => {
                let f = self.innermost_named_fn();
                f.0
            }
            MagicConst::Method => {
                let (fname, is_method) = self.innermost_named_fn();
                if is_method {
                    if let Some(c) = self.current_class() {
                        let mut v = c.name.as_bytes().to_vec();
                        v.extend_from_slice(b"::");
                        v.extend_from_slice(&fname);
                        v
                    } else {
                        fname
                    }
                } else {
                    fname
                }
            }
            MagicConst::Property => self.hook_prop.clone().unwrap_or_default(),
        };
        self.lit(Value::Str(ZStr::from(s)))
    }

    fn innermost_named_fn(&self) -> (Vec<u8>, bool) {
        let f = self.fref();
        match f.kind {
            FnKind::Main | FnKind::ConstExpr => (Vec::new(), false),
            FnKind::Closure | FnKind::Function => (f.func_name.as_bytes().to_vec(), false),
            FnKind::Method => (f.method_name.as_bytes().to_vec(), true),
        }
    }

    /// A property, method or constant name operand. php-src compiles the
    /// name with `zend_compile_expr()`, which moves `CG(zend_lineno)` to the
    /// line of the name, so the fetch, call init or assignment emitted next
    /// reports that line (`$o\n->p` on the line of `p`).
    pub(crate) fn member_name(&mut self, n: &MemberName) -> CResult<Operand> {
        Ok(match n {
            MemberName::Ident(id) => {
                self.f().line = id.span.line;
                self.lit(Value::Str(ZStr::from(id.name.as_slice())))
            }
            MemberName::Expr(e) => self.compile_expr(e)?,
        })
    }

    /// `zend_try_ct_eval_array`: an all-constant array literal with an
    /// array key is a compile error.
    #[cold]
    fn check_ct_array_keys(&self, items: &[Option<ArrayItem>], line: u32) -> CResult<()> {
        let mut keys = Vec::new();
        for it in items {
            let Some(it) = it else { return Ok(()) };
            if it.by_ref || it.unpack || self.try_fold(&it.value).is_none() {
                return Ok(());
            }
            if let Some(k) = &it.key {
                match self.try_fold(k) {
                    Some(v) => keys.push(v),
                    None => return Ok(()),
                }
            }
        }
        if keys.iter().any(|k| matches!(k, Value::Array(_))) {
            return self.err("Illegal offset type", line);
        }
        Ok(())
    }

    /// A property fetch in a constant expression (`ZEND_AST_PROP` of
    /// `zend_ast_evaluate_ex`): the object is fetched, then the name is
    /// evaluated, then non-enum objects are rejected.
    #[cold]
    fn const_prop_place(&mut self, p: Place, name: &MemberName) -> CResult<Place> {
        let direct = match &p.base {
            PlaceBase::Value(op) if p.segs.is_empty() => Some(*op),
            _ => None,
        };
        let v = match direct {
            Some(op) => op,
            None => self.fetch_place(p, FetchMode::Read),
        };
        let n = self.member_name(name)?;
        let dst = self.tmp();
        self.emit(Op::ConstPropCheck {
            obj: Compiler::keep(v),
            name: n,
            dst,
        });
        Ok(Place {
            base: PlaceBase::Value(v),
            segs: vec![Seg::Prop(Operand::Tmp(dst))],
        })
    }

    /// The object operand of `$this->m()` (`zend_compile_method_call`):
    /// [`Operand::Unused`] (`IS_UNUSED` op1 of `ZEND_INIT_METHOD_CALL`, the
    /// frame's `$this` read in place) where `this_guaranteed_exists()`, a
    /// `FETCH_THIS` temporary otherwise. `object` is the `$this` variable.
    fn method_call_this(&mut self, object: &Expr) -> CResult<Operand> {
        if self.this_guaranteed_exists() {
            Ok(Operand::Unused)
        } else {
            self.compile_chain(object)
        }
    }

    /// `this_guaranteed_exists()` (`Zend/zend_compile.c`): whether the
    /// function being compiled always runs with a `$this`: a non-static
    /// method, or a non-static closure nested only in non-static closures
    /// up to one. A closure has no class scope at compile time there, so one
    /// in a static method, a function or the main script is not; neither is
    /// a static closure.
    pub(crate) fn this_guaranteed_exists(&self) -> bool {
        for f in self.fns.iter().rev() {
            if f.is_static {
                return false;
            }
            match f.kind {
                FnKind::Method => return true,
                FnKind::Closure => {}
                FnKind::Main | FnKind::Function | FnKind::ConstExpr => return false,
            }
        }
        false
    }

    /// A method name operand: `zend_compile_method_call` /
    /// `zend_compile_static_call` reject a constant that is not a string.
    fn method_member_name(&mut self, n: &MemberName, line: u32) -> CResult<Operand> {
        if let MemberName::Expr(e) = n {
            if !matches!(self.try_fold(e), None | Some(Value::Str(_))) {
                return self.err("Method name must be a string", line);
            }
        }
        self.member_name(n)
    }

    /// Compiles a writable expression into a place (offsets evaluated now),
    /// after `zend_delayed_compile_var()`'s `zend_check_stack_limit()`.
    pub(crate) fn compile_place(&mut self, e: &Expr, for_unset: bool) -> CResult<Place> {
        if !self.stack.enter(stack::VAR_FRAME_BYTES) {
            return self.stack_limit_error(e.span.line);
        }
        let r = self.compile_place_body(e, for_unset);
        self.stack.leave(stack::VAR_FRAME_BYTES);
        r
    }

    fn compile_place_body(&mut self, e: &Expr, _for_unset: bool) -> CResult<Place> {
        self.f().line = e.span.line;
        Ok(match &e.kind {
            ExprKind::Var(VarName::Name(id)) => {
                if id.name == b"this" {
                    Place {
                        base: PlaceBase::This,
                        segs: Vec::new(),
                    }
                } else if is_superglobal(&id.name) {
                    let n = self.lit(Value::Str(ZStr::from(id.name.as_slice())));
                    Place {
                        base: PlaceBase::Global(n),
                        segs: Vec::new(),
                    }
                } else if id.name == b"GLOBALS" {
                    Place {
                        base: PlaceBase::GlobalsArray,
                        segs: Vec::new(),
                    }
                } else {
                    Place {
                        base: PlaceBase::Cv(self.cv(&id.name)),
                        segs: Vec::new(),
                    }
                }
            }
            ExprKind::Var(VarName::Expr(ne)) => {
                // zend_compile_simple_var_no_cv: a constant name that is an
                // auto-global (`${'_SERVER'}`) is a global fetch.
                let global = match self.try_fold(ne) {
                    Some(Value::Str(s)) if is_superglobal(s.as_bytes()) => Some(s),
                    _ => None,
                };
                if let Some(s) = global {
                    let n = self.lit(Value::Str(s));
                    Place {
                        base: PlaceBase::Global(n),
                        segs: Vec::new(),
                    }
                } else {
                    let n = self.compile_expr(ne)?;
                    Place {
                        base: PlaceBase::VarVar(n),
                        segs: Vec::new(),
                    }
                }
            }
            ExprKind::Dim { base, index } => {
                if let ExprKind::Var(VarName::Name(id)) = &base.kind {
                    if id.name == b"GLOBALS" {
                        // zend_delayed_compile_dim: `$GLOBALS[$name]` is a
                        // global variable fetch; `$GLOBALS[]` never compiles.
                        let Some(idx) = index else {
                            return self.err("Cannot append to $GLOBALS", e.span.line);
                        };
                        let n = self.compile_expr(idx)?;
                        return Ok(Place {
                            base: PlaceBase::Global(n),
                            segs: Vec::new(),
                        });
                    }
                }
                let mut p = self.compile_place_base(base)?;
                let mut lines = self.take_place_lines(&p);
                // `zend_ast_evaluate_ex` rejects an object container of a
                // constant-expression dimension fetch (it could run
                // arbitrary code through `offsetGet()`) before evaluating
                // the offset. Only a container computed into a temporary
                // can be an object (nested fetches are not checked).
                if let (Some(_), PlaceBase::Value(v @ Operand::Tmp(_))) = (index, &p.base) {
                    if p.segs.is_empty() && self.in_evaluated_const_expr() {
                        self.emit(Op::ConstDimCheck {
                            obj: Compiler::keep(*v),
                        });
                    }
                }
                let seg = match index {
                    Some(i) => Seg::Dim(self.compile_expr(i)?),
                    None => Seg::Append,
                };
                p.segs.push(seg);
                lines.push(self.f().line);
                self.f().place_lines = Some((p.clone(), lines));
                p
            }
            ExprKind::Prop {
                object,
                name,
                nullsafe,
            } => {
                let mut p = self.compile_place_base(object)?;
                let mut lines = self.take_place_lines(&p);
                if *nullsafe {
                    let chain = self.null_chain();
                    let mode = if chain == NullChain::Quiet {
                        FetchMode::Is
                    } else {
                        FetchMode::Read
                    };
                    // `zend_emit_jmp_null()` on the variable itself, which
                    // the property fetch then reads again.
                    let (v, base) = match p.base {
                        PlaceBase::Value(op) if p.segs.is_empty() => (op, PlaceBase::Value(op)),
                        PlaceBase::Cv(cv) if p.segs.is_empty() => {
                            (Operand::Cv(cv), PlaceBase::Cv(cv))
                        }
                        _ => {
                            let v = self.fetch_place(p, mode);
                            (v, PlaceBase::Value(v))
                        }
                    };
                    let j = self.emit(Op::JmpNull {
                        a: Compiler::keep(v),
                        dst: 0,
                        target: 0,
                        chain,
                    });
                    self.f().sc_jumps.push(j);
                    p = Place {
                        base,
                        segs: Vec::new(),
                    };
                    lines.clear();
                }
                if self.in_evaluated_const_expr() {
                    return self.const_prop_place(p, name);
                }
                let n = match name {
                    // zend_delayed_compile_prop converts a constant name to
                    // a string at compile time.
                    MemberName::Expr(e) if matches!(self.try_fold(e), Some(Value::Array(_))) => {
                        self.warnings.push((
                            2,
                            "Array to string conversion".to_string(),
                            e.span.line,
                        ));
                        self.lit(Value::Str(ZStr::from("Array")))
                    }
                    _ => self.member_name(name)?,
                };
                p.segs.push(Seg::Prop(n));
                lines.push(match name {
                    MemberName::Ident(id) => id.span.line,
                    MemberName::Expr(_) => self.f().line,
                });
                self.f().place_lines = Some((p.clone(), lines));
                p
            }
            ExprKind::StaticProp { class, name, .. } => {
                let spec = self.chain_class_spec(class)?;
                let n = match name {
                    VarName::Name(id) => self.lit(Value::Str(ZStr::from(id.name.as_slice()))),
                    VarName::Expr(ex) => self.compile_expr(ex)?,
                };
                Place {
                    base: PlaceBase::StaticProp {
                        class: spec,
                        name: n,
                    },
                    segs: Vec::new(),
                }
            }
            _ => {
                let v = self.compile_chain(e)?;
                Place {
                    base: PlaceBase::Value(v),
                    segs: Vec::new(),
                }
            }
        })
    }

    /// The segment lines [`Compiler::compile_place`] recorded for `p` (see
    /// `FnCtx::place_lines`), or 0, unknown, for each segment of a place
    /// built otherwise.
    fn take_place_lines(&mut self, p: &Place) -> Vec<u32> {
        match self.f().place_lines.take() {
            Some((q, lines)) if q == *p && lines.len() == p.segs.len() => lines,
            _ => vec![0; p.segs.len()],
        }
    }

    fn compile_place_base(&mut self, base: &Expr) -> CResult<Place> {
        match base.kind {
            ExprKind::Var(_)
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. } => self.compile_place(base, false),
            _ => {
                let v = self.compile_chain(base)?;
                Ok(Place {
                    base: PlaceBase::Value(v),
                    segs: Vec::new(),
                })
            }
        }
    }

    /// The operand of `++`/`--`: `zend_compile_pre_incdec` fetches `$this`
    /// with `FETCH_THIS` (a temporary copy, so `++$this` compiles and fails
    /// at run time); everything else is a write place.
    fn compile_incdec_place(&mut self, var: &Expr) -> CResult<Place> {
        if let ExprKind::Var(VarName::Name(id)) = &var.kind {
            if id.name == b"this" {
                self.f().line = var.span.line;
                return Ok(Place {
                    base: PlaceBase::This,
                    segs: Vec::new(),
                });
            }
        }
        self.compile_write_place(var)
    }

    /// A place used for writing: rejects nullsafe chains and temporaries.
    pub(crate) fn compile_write_place(&mut self, e: &Expr) -> CResult<Place> {
        if contains_nullsafe(e) {
            return self.err("Can't use nullsafe operator in write context", e.span.line);
        }
        match &e.kind {
            ExprKind::Var(_)
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. } => {}
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. } => {
                return self.err(
                    "Can't use function return value in write context",
                    e.span.line,
                );
            }
            _ => {
                return self.err(
                    "Cannot use temporary expression in write context",
                    e.span.line,
                );
            }
        }
        if let ExprKind::Var(VarName::Name(id)) = &e.kind {
            if id.name == b"this" {
                return self.err("Cannot re-assign $this", e.span.line);
            }
            if id.name == b"GLOBALS" {
                return self.err(
                    "$GLOBALS can only be modified using the $GLOBALS[$name] = $value syntax",
                    e.span.line,
                );
            }
        }
        let mut inner = e;
        while let ExprKind::Dim { base: next, .. } | ExprKind::Prop { object: next, .. } =
            &inner.kind
        {
            inner = &**next;
        }
        if let ExprKind::Var(VarName::Name(id)) = &inner.kind {
            self.note_cv_write(&id.name);
        }
        if !std::ptr::eq(inner, e) && self.is_opcode_builtin_call(inner) {
            return self.err(
                "Cannot use result of built-in function in write context",
                e.span.line,
            );
        }
        self.compile_place(e, false)
    }

    /// Whether `e` calls a builtin that `zend_try_compile_special_func_ex`
    /// (`Zend/zend_compile.c`) compiles to an opcode with a TMP result or
    /// to a constant, which `zend_separate_if_call_and_write` and
    /// `zend_compile_assign_ref` reject in write context. Only the forms
    /// that depend on the argument count alone are recognised, plus
    /// `defined()` and `in_array()`, which depend on their literal
    /// arguments ([`Compiler::compile_defined`],
    /// [`Compiler::in_array_set`]). `chr()` and `ord()` are only folded in
    /// read context, so they never qualify.
    fn is_opcode_builtin_call(&self, e: &Expr) -> bool {
        let ExprKind::Call { callee, args } = &e.kind else {
            return false;
        };
        let Callee::Name(n) = &**callee else {
            return false;
        };
        let positional: Vec<&Expr> = args
            .args
            .iter()
            .filter_map(|a| match &a.kind {
                ArgKind::Positional(e) => Some(e),
                _ => None,
            })
            .collect();
        if positional.len() != args.args.len() {
            return false;
        }
        let (resolved, fallback) = self.resolve_function_name(n);
        if fallback.is_some() {
            // Runtime resolution (`ZEND_INIT_NS_FCALL_BY_NAME`).
            return false;
        }
        let argc = args.args.len();
        match resolved.to_ascii_lowercase().as_slice() {
            b"strlen" | b"is_null" | b"is_bool" | b"is_long" | b"is_int" | b"is_integer"
            | b"is_float" | b"is_double" | b"is_string" | b"is_array" | b"is_object"
            | b"is_resource" | b"is_scalar" | b"boolval" | b"intval" | b"floatval"
            | b"doubleval" | b"strval" | b"gettype" | b"count" | b"sizeof" => argc == 1,
            b"get_class" => argc <= 1,
            b"get_called_class" => argc == 0,
            b"array_key_exists" => argc == 2,
            b"defined" => {
                argc == 1
                    && self.special_builtin_enabled(b"defined")
                    && defined_literal_name(positional[0])
            }
            b"in_array" => self.in_array_set(&positional).is_some(),
            _ => false,
        }
    }

    fn compile_isset_one(&mut self, v: &Expr) -> CResult<Operand> {
        if !is_ref_expr(v) {
            return self.err(
                "Cannot use isset() on the result of an expression (you can use \"null !== expression\" instead)",
                v.span.line,
            );
        }
        let t = self.tmp();
        if let ExprKind::Var(VarName::Name(id)) = &v.kind {
            if id.name == b"this" {
                let pid = self.place(Place {
                    base: PlaceBase::This,
                    segs: Vec::new(),
                });
                self.emit(Op::Isset { place: pid, dst: t });
                return Ok(Operand::Tmp(t));
            }
        }
        let (p, jumps) = self.compile_is_place(v)?;
        let pid = self.place(p);
        self.emit(Op::Isset { place: pid, dst: t });
        self.commit_short_circuit(jumps, t, Some(NullChain::Isset));
        Ok(Operand::Tmp(t))
    }

    /// `zend_compile_binary_op` (`ZEND_CONCAT`): a constant array operand
    /// is cast to string by a `ZEND_CAST` emitted after both operands and
    /// before the concatenation, so its "Array to string conversion"
    /// warning precedes the other operand's conversion error
    /// (`operator_unsupported_types.phpt`: `new stdClass . []`).
    fn concat_cast_array_literal(&mut self, operand: Operand) -> Operand {
        let Operand::Lit(i) = operand else {
            return operand;
        };
        if !matches!(self.fref().literals.get(i as usize), Some(Value::Array(_))) {
            return operand;
        }
        let dst = self.tmp();
        self.emit(Op::Cast {
            kind: CastKind::String,
            a: operand,
            dst,
        });
        Operand::Tmp(dst)
    }

    /// `zend_try_ct_eval_binary_op()` in `zend_compile_binary_op()`: two
    /// literal operands that [`Compiler::try_fold`] could not see (magic
    /// constants, substituted constants) fold unless evaluating them would
    /// diagnose or throw.
    fn fold_lit_binary(&self, op: AB, l: Operand, r: Operand) -> Option<Value> {
        let (Operand::Lit(a), Operand::Lit(b)) = (l, r) else {
            return None;
        };
        let bop = map_binop(op)?;
        let lits = &self.fref().literals;
        let (x, y) = (lits.get(a as usize)?, lits.get(b as usize)?);
        if matches!(x, Value::Array(_)) || matches!(y, Value::Array(_)) {
            return None;
        }
        let mut ctx = CollectingContext::new();
        let v = php_runtime::ops::binary_op(bop, x, y, &mut ctx).ok()?;
        ctx.diagnostics.is_empty().then_some(v)
    }

    fn compile_binary(&mut self, op: AB, left: &Expr, right: &Expr) -> CResult<Operand> {
        let t = self.tmp();
        match op {
            AB::BoolAnd | AB::BoolOr => {
                let l = self.compile_expr(left)?;
                // `zend_compile_short_circuiting`: a constant left side
                // that decides the result means the right side is never
                // compiled.
                if let Operand::Lit(i) = l {
                    let lv = self.f().literals.get(i as usize).cloned();
                    if let Some(truthy) = lv.as_ref().and_then(const_truthiness) {
                        if (op == AB::BoolAnd) != truthy {
                            return Ok(self.lit(Value::Bool(truthy)));
                        }
                    }
                }
                let j = if op == AB::BoolAnd {
                    self.emit(Op::JmpZEx {
                        a: l,
                        dst: t,
                        target: 0,
                    })
                } else {
                    self.emit(Op::JmpNzEx {
                        a: l,
                        dst: t,
                        target: 0,
                    })
                };
                let r = self.compile_expr(right)?;
                self.emit(Op::Unary {
                    op: UnKind::Bool,
                    a: r,
                    dst: t,
                });
                self.patch_here(j);
            }
            AB::Coalesce => {
                let l = if is_ref_expr(left) {
                    if let ExprKind::Var(VarName::Name(id)) = &left.kind {
                        if id.name == b"this" {
                            self.compile_expr(left)?
                        } else if !is_superglobal_or_globals(&id.name) {
                            // `ZEND_COALESCE` of the CV itself, read
                            // without the undefined-variable warning (the
                            // handlers' `read_cv_defined` / `coalesce_in`),
                            // not a copy into a temporary first.
                            Operand::Cv(self.cv(&id.name))
                        } else {
                            let (p, jumps) = self.compile_is_place(left)?;
                            let r = self.fetch_place(p, FetchMode::Is);
                            if jumps.is_empty() {
                                r
                            } else {
                                let d = match r {
                                    Operand::Tmp(d) => d,
                                    _ => unreachable!("fetch_place returns a tmp"),
                                };
                                self.finish_short_circuit(jumps, d);
                                r
                            }
                        }
                    } else {
                        let (p, jumps) = self.compile_is_place(left)?;
                        let r = self.fetch_place(p, FetchMode::Is);
                        if !jumps.is_empty() {
                            let d = match r {
                                Operand::Tmp(d) => d,
                                _ => unreachable!("fetch_place returns a tmp"),
                            };
                            self.finish_short_circuit(jumps, d);
                        }
                        r
                    }
                } else {
                    self.compile_expr(left)?
                };
                let j = self.emit(Op::Coalesce {
                    a: l,
                    dst: t,
                    target: 0,
                });
                let r = self.compile_expr(right)?;
                self.emit(Op::Move { src: r, dst: t });
                self.patch_here(j);
            }
            _ => {
                let l = self.compile_expr(left)?;
                let r = self.compile_expr(right)?;
                if let Some(v) = self.fold_lit_binary(op, l, r) {
                    return Ok(self.lit(v));
                }
                // The operator (and a cast of a constant array operand)
                // takes `CG(zend_lineno)` as the operands left it: the line
                // of the last compiled operand (`zend_compile_expr_inner`).
                let (l, r) = if op == AB::Concat {
                    let l = self.concat_cast_array_literal(l);
                    (l, self.concat_cast_array_literal(r))
                } else {
                    (l, r)
                };
                let (l, r) = if (matches!(op, AB::Identical | AB::NotIdentical)
                    && matches!(l, Operand::Lit(_))
                    && !matches!(r, Operand::Lit(_)))
                    || (matches!(op, AB::Mul | AB::BitOr | AB::BitAnd | AB::BitXor)
                        && operand_rank(l) < operand_rank(r))
                {
                    (r, l)
                } else {
                    (l, r)
                };
                let (kind, a, b) = match op {
                    AB::Identical => (BinKind::Identical, l, r),
                    AB::NotIdentical => (BinKind::NotIdentical, l, r),
                    AB::Equal => (BinKind::Equal, l, r),
                    AB::NotEqual => (BinKind::NotEqual, l, r),
                    AB::Less => (BinKind::Less, l, r),
                    AB::LessEqual => (BinKind::LessEqual, l, r),
                    AB::Greater => (BinKind::Less, r, l),
                    AB::GreaterEqual => (BinKind::LessEqual, r, l),
                    AB::Spaceship => (BinKind::Spaceship, l, r),
                    other => (
                        BinKind::Arith(map_binop(other).unwrap_or(BinaryOp::Add)),
                        l,
                        r,
                    ),
                };
                if matches!(kind, BinKind::Arith(BinaryOp::Add)) {
                    self.user_opcode_hook("ZEND_ADD");
                }
                self.emit(Op::Bin {
                    op: kind,
                    a,
                    b,
                    dst: t,
                });
            }
        }
        Ok(Operand::Tmp(t))
    }

    pub(crate) fn compile_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        line: u32,
        need: bool,
    ) -> CResult<Option<Operand>> {
        match &target.kind {
            ExprKind::Var(VarName::Name(id))
                if id.name != b"GLOBALS" && !is_superglobal(&id.name) =>
            {
                if id.name == b"this" {
                    return self.err("Cannot re-assign $this", line);
                }
                self.note_cv_write(&id.name);
                let cv = self.cv(&id.name);
                let v = self.compile_expr(value)?;
                self.f().line = line;
                let dst = need.then(|| self.tmp());
                self.emit(Op::AssignCv { cv, val: v, dst });
                Ok(dst.map(Operand::Tmp))
            }
            ExprKind::Array { items, .. } => {
                let has_ref = items
                    .iter()
                    .flatten()
                    .any(|i| i.by_ref || has_nested_ref(i));
                if has_ref && contains_nullsafe(value) {
                    // zend_compile_assign: zend_assert_not_short_circuited.
                    return self.err("Cannot take reference of a nullsafe chain", line);
                }
                if has_ref && is_ref_expr(value) {
                    // zend_compile_assign: the source is fetched for
                    // writing and made a reference first, so the elements
                    // are taken from it even if a target rebinds the
                    // source variable (`[&$a, &$b] = $a`).
                    let src = self.compile_place(value, false)?;
                    let pid = self.place(src);
                    let t = self.tmp();
                    self.emit(Op::MakeRef { place: pid, dst: t });
                    let src = Place {
                        base: PlaceBase::Value(Operand::TmpKeep(t)),
                        segs: Vec::new(),
                    };
                    self.compile_destructure_ref(items, &src, line, false)?;
                    if need {
                        return Ok(Some(Operand::Tmp(t)));
                    }
                    self.free(Operand::Tmp(t));
                    return Ok(None);
                }
                if !need && !has_ref {
                    if let Some(values) = list_from_array_values(items, value) {
                        self.compile_list_from_array(items, values)?;
                        return Ok(None);
                    }
                }
                let v = self.compile_expr(value)?;
                if has_ref && is_call_expr(value) {
                    // zend_compile_list_assign: the call result is a VAR
                    // that ZEND_FETCH_LIST_W writes through only when the
                    // function returned a reference.
                    if let Operand::Tmp(t) = v {
                        let src = Place {
                            base: PlaceBase::Value(Operand::TmpKeep(t)),
                            segs: Vec::new(),
                        };
                        self.f().line = line;
                        self.compile_destructure_ref(items, &src, line, true)?;
                        if need {
                            return Ok(Some(v));
                        }
                        self.free(v);
                        return Ok(None);
                    }
                }
                // A variable source is copied (ZEND_QM_ASSIGN) without moving
                // the line; any other source leaves the line where compiling
                // it ended, and the first element is fetched there.
                let rhs_cv = matches!(v, Operand::Cv(_));
                let v = match v {
                    Operand::Cv(_) => {
                        let t = self.tmp();
                        self.emit(Op::Move { src: v, dst: t });
                        Operand::Tmp(t)
                    }
                    o => o,
                };
                if rhs_cv {
                    self.f().line = line;
                }
                self.compile_destructure_keep(items, v, line, !rhs_cv)?;
                if need {
                    Ok(Some(v))
                } else {
                    self.free(v);
                    Ok(None)
                }
            }
            _ => {
                let p = self.compile_write_place(target)?;
                // An element or property assignment is the delayed opline
                // `zend_delayed_compile_dim`/`_prop` created after compiling
                // the last offset or name: it keeps that line, not the line
                // the value ends on (`$o\n->p\n= f();` assigns on the line
                // of `p`).
                let at = match target.kind {
                    ExprKind::Dim { .. } | ExprKind::Prop { .. } | ExprKind::StaticProp { .. } => {
                        self.f().line
                    }
                    _ => line,
                };
                let v = self.compile_expr(value)?;
                self.f().line = at;
                let pid = self.place(p);
                let dst = need.then(|| self.tmp());
                self.emit(Op::Assign {
                    place: pid,
                    val: v,
                    dst,
                });
                Ok(dst.map(Operand::Tmp))
            }
        }
    }

    /// Destructures `src` (read without consuming); the caller frees it.
    /// `after_rhs`: the first element is fetched on the line compiling the
    /// source left (see [`Self::destructure_items`]).
    fn compile_destructure_keep(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
        after_rhs: bool,
    ) -> CResult<()> {
        let keep = Compiler::keep(src);
        self.destructure_items(items, keep, line, after_rhs)
    }

    /// Destructures `src` and consumes it: a `foreach` target or a nested
    /// list, whose first element php-src fetches on the line the
    /// `FE_FETCH` or the enclosing element's fetch is on.
    pub(crate) fn compile_destructure(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
    ) -> CResult<()> {
        let keep = Compiler::keep(src);
        self.destructure_items(items, keep, line, true)?;
        self.free(src);
        Ok(())
    }

    /// The elements of a list assignment from `src`. The first element is
    /// fetched on the line of the list (`zend_ast_create_list` takes the
    /// line of its first child, down to the first variable), or with
    /// `after_rhs` on the line compiling the source left.
    fn destructure_items(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
        after_rhs: bool,
    ) -> CResult<()> {
        if items.iter().all(Option::is_none) {
            return self.err("Cannot use empty list", line);
        }
        let keyed = items.iter().flatten().any(|i| i.key.is_some());
        let unkeyed = items.iter().flatten().any(|i| i.key.is_none());
        if keyed && unkeyed {
            return self.err(
                "Cannot mix keyed and unkeyed array entries in assignments",
                line,
            );
        }
        let mut idx = 0i64;
        let mut first = true;
        for it in items {
            let Some(it) = it else {
                idx += 1;
                continue;
            };
            if it.unpack {
                return self.err("Spread operator is not supported in assignments", line);
            }
            // `zend_compile_list_assign` emits each ZEND_FETCH_LIST_R before
            // compiling that element's target, so an unkeyed element after
            // the first is fetched on the line the previous target left
            // (`[\n$a,\n$b\n] = [];` reports key 1 on the line of `$a`); a
            // key moves the line to its own when it is compiled.
            if first {
                if !after_rhs {
                    self.f().line = first_leaf_line(it);
                }
                first = false;
            }
            let key = match &it.key {
                Some(k) => self.compile_expr(k)?,
                None => {
                    let k = self.lit(Value::Int(idx));
                    idx += 1;
                    k
                }
            };
            let elem = self.fetch_place(
                Place {
                    base: PlaceBase::Value(src),
                    segs: vec![Seg::Dim(key)],
                },
                FetchMode::List,
            );
            if let ExprKind::Array { items: sub, .. } = &it.value.kind {
                // A nested list's first element is fetched on the line its
                // own fetch from the parent is on.
                self.compile_destructure(sub, elem, it.span.line)?;
            } else {
                self.assign_to(&it.value, elem, None)?;
            }
        }
        Ok(())
    }

    /// `zend_try_compile_list_assign_from_array`: `[$a, $b] = [$x, $y];` as
    /// a statement compiles the values in order (copying variables), then
    /// assigns each target its value directly, without building the
    /// array. A pending value is released if an exception interrupts the
    /// assignments (see [`crate::opcode::LiveTmp`]). Each target is
    /// assigned on the line the values (then the previous target) left, as
    /// `zend_emit_assign_znode` does: a nested list's first element is
    /// fetched there.
    fn compile_list_from_array(
        &mut self,
        targets: &[Option<ArrayItem>],
        values: &[Option<ArrayItem>],
    ) -> CResult<()> {
        let mut ops = Vec::with_capacity(values.len());
        for v in values.iter().flatten() {
            let op = self.compile_expr(&v.value)?;
            let op = if let Operand::Cv(_) = op {
                let t = self.tmp();
                self.emit(Op::Move { src: op, dst: t });
                Operand::Tmp(t)
            } else {
                op
            };
            ops.push((op, self.here()));
        }
        let mut live = Vec::new();
        for (t, (op, start)) in targets.iter().flatten().zip(ops) {
            self.assign_to(&t.value, op, None)?;
            if let Operand::Tmp(tmp) = op {
                let end = self.here();
                live.push(crate::opcode::LiveTmp { start, end, tmp });
            }
        }
        self.f().live_tmps.extend(live);
        Ok(())
    }

    /// Destructuring with references from the place `src`; `call_base`:
    /// `src` is a call result (see [`Op::ListFetchRef`]).
    pub(crate) fn compile_destructure_ref(
        &mut self,
        items: &[Option<ArrayItem>],
        src: &Place,
        line: u32,
        call_base: bool,
    ) -> CResult<()> {
        let mut idx = 0i64;
        for it in items {
            let Some(it) = it else {
                idx += 1;
                continue;
            };
            let key = match &it.key {
                Some(k) => self.compile_expr(k)?,
                None => {
                    let k = self.lit(Value::Int(idx));
                    idx += 1;
                    k
                }
            };
            let mut p = src.clone();
            // Offsets of the source place must not be consumed twice.
            keep_place(&mut p);
            p.segs.push(Seg::Dim(key));
            if let ExprKind::Array { items: sub, .. } = &it.value.kind {
                if call_base && has_nested_ref(it) {
                    let mut q = p.clone();
                    keep_place(&mut q);
                    let pid = self.place(q);
                    self.emit(Op::ListFetchRef {
                        place: pid,
                        dst: None,
                    });
                }
                self.compile_destructure_ref(sub, &p, line, call_base)?;
            } else if it.by_ref {
                let pid = self.place(p);
                let t = self.tmp();
                if call_base {
                    self.emit(Op::ListFetchRef {
                        place: pid,
                        dst: Some(t),
                    });
                } else {
                    self.emit(Op::MakeRef { place: pid, dst: t });
                }
                let tp = self.compile_write_place(&it.value)?;
                let tpid = self.place(tp);
                self.emit(Op::AssignRef {
                    place: tpid,
                    src: Operand::Tmp(t),
                    dst: None,
                });
            } else {
                let v = self.fetch_place(p, FetchMode::List);
                self.assign_to(&it.value, v, None)?;
            }
        }
        Ok(())
    }

    /// `ZEND_ASSIGN_REF`; `used` is whether the result is wanted. An
    /// unused result has no destination: php-src frees TMP/VAR operands
    /// without a GC check (`zval_ptr_dtor_nogc`), so a result handle this
    /// VM dropped later would buffer the reference as a spurious possible
    /// root (`gc::release_duplicate` drops it quietly in the handler).
    pub(crate) fn compile_assign_ref(
        &mut self,
        target: &Expr,
        value: &Expr,
        line: u32,
        used: bool,
    ) -> CResult<Operand> {
        if contains_nullsafe(value) {
            return self.err("Cannot take reference of a nullsafe chain", line);
        }
        if let ExprKind::Array { .. } = target.kind {
            return self.err("Cannot assign reference to array destructuring", line);
        }
        if self.is_opcode_builtin_call(value) {
            return self.err(
                "Cannot use result of built-in function in write context",
                line,
            );
        }
        // `zend_compile_assign_ref` compiles the target (delayed) before
        // the source: the target's variables get their slots first, which
        // is the order `get_defined_vars()` and `compact()` see, and its
        // offset expressions and calls run before the source's.
        let target_place = self.compile_write_place(target)?;
        let src = if is_ref_expr(value) {
            let p = self.compile_place(value, false)?;
            let pid = self.place(p);
            let t = self.tmp();
            self.emit(Op::MakeRef { place: pid, dst: t });
            Operand::Tmp(t)
        } else {
            self.compile_expr(value)?
        };
        self.f().line = line;
        let pid = self.place(target_place);
        let dst = used.then(|| self.tmp());
        self.emit(Op::AssignRef {
            place: pid,
            src,
            dst,
        });
        Ok(dst.map_or(Operand::Unused, Operand::Tmp))
    }

    fn compile_assign_op(
        &mut self,
        op: AB,
        target: &Expr,
        value: &Expr,
        need: bool,
        line: u32,
    ) -> CResult<Option<Operand>> {
        if op == AB::Coalesce {
            // A plain local is read as `??` reads it, `ZEND_COALESCE` of
            // the CV itself (`zend_compile_assign_coalesce` compiles the
            // variable for `BP_VAR_IS`, which is the CV operand), and
            // written as `=` writes it.
            if let ExprKind::Var(VarName::Name(id)) = &target.kind {
                if id.name != b"this" && id.name != b"GLOBALS" && !is_superglobal(&id.name) {
                    self.note_cv_write(&id.name);
                    let cv = self.cv(&id.name);
                    let t = self.tmp();
                    let j = self.emit(Op::Coalesce {
                        a: Operand::Cv(cv),
                        dst: t,
                        target: 0,
                    });
                    let v = self.compile_expr(value)?;
                    self.f().line = line;
                    self.emit(Op::AssignCv {
                        cv,
                        val: v,
                        dst: Some(t),
                    });
                    self.patch_here(j);
                    if need {
                        return Ok(Some(Operand::Tmp(t)));
                    }
                    self.free(Operand::Tmp(t));
                    return Ok(None);
                }
            }
            let p = self.compile_write_place(target)?;
            let temps = place_tmps(&p);
            let mut pk = p.clone();
            keep_place(&mut pk);
            let t = self.tmp();
            let cur = self.fetch_place(pk, FetchMode::Is);
            let j = self.emit(Op::Coalesce {
                a: cur,
                dst: t,
                target: 0,
            });
            let v = self.compile_expr(value)?;
            self.f().line = line;
            let pid = self.place(p);
            self.emit(Op::Assign {
                place: pid,
                val: v,
                dst: Some(t),
            });
            self.patch_here(j);
            // The memoized offsets are freed on both paths (the assignment
            // consumed them; a short-circuit did not).
            for tmp in temps {
                self.emit(Op::Free(Operand::Tmp(tmp)));
            }
            if need {
                return Ok(Some(Operand::Tmp(t)));
            }
            self.free(Operand::Tmp(t));
            return Ok(None);
        }
        let p = self.compile_write_place(target)?;
        let v = self.compile_expr(value)?;
        self.f().line = line;
        let pid = self.place(p);
        let dst = need.then(|| self.tmp());
        self.emit(Op::AssignOp {
            place: pid,
            op: map_binop(op).unwrap_or(BinaryOp::Add),
            val: v,
            dst,
        });
        Ok(dst.map(Operand::Tmp))
    }

    /// `zend_compile_match`: the subject is compiled once and read by
    /// every test without a copy (a CV is re-read, so a condition that
    /// assigns to it is seen by the later tests). When every condition is
    /// an integer or string literal and there are at least two, one
    /// [`Op::Match`] jumps through a table; otherwise each condition is
    /// tested with `===` in order. Without a default arm the
    /// [`Op::MatchError`] comes before the arm bodies.
    fn compile_match(&mut self, subject: &Expr, arms: &[ast::MatchArm]) -> CResult<Operand> {
        let s = self.compile_expr(subject)?;
        let sk = Compiler::keep(s);
        let result = self.tmp();
        let mut default_arm = None;
        for (i, arm) in arms.iter().enumerate() {
            if arm.conds.is_none() {
                if default_arm.is_some() {
                    return self.err(
                        "Match expressions may only contain one default arm",
                        arm.span.line,
                    );
                }
                default_arm = Some(i);
            }
        }
        let keys = match_jumptable_keys(self, arms);
        // Ops take `CG(zend_lineno)`: `MATCH` and its `MATCH_ERROR` the
        // subject's line, a `MATCH_ERROR` after `===` tests the line the
        // last condition left.
        let mut table = None;
        let mut arm_jumps: Vec<(usize, usize)> = Vec::new();
        let mut no_match = None;
        if keys.is_some() {
            self.f().line = subject.span.line;
            table = Some(self.emit(Op::Match {
                subject: sk,
                table: Box::new(MatchTable::new(0)),
            }));
        } else {
            // `can_match_use_jumptable` folded the conditions up to the
            // first one that is not an int or string literal; a folded
            // condition that was not a literal already became a literal
            // of the line then current (the subject's), which its `===`
            // test gets (an undefined subject warns on that line).
            let mut prefolded = match_prefolded_conds(self, arms);
            for (i, arm) in arms.iter().enumerate() {
                for c in arm.conds.iter().flatten() {
                    let is_literal = matches!(
                        c.kind,
                        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
                    );
                    let line = if prefolded > 0 && !is_literal {
                        subject.span.line
                    } else {
                        c.span.line
                    };
                    self.f().line = line;
                    prefolded = prefolded.saturating_sub(1);
                    let v = self.compile_expr(c)?;
                    // Compiling the condition may move the line to its own;
                    // the `===` test keeps the line chosen above.
                    self.f().line = line;
                    let t = self.tmp();
                    self.emit(Op::Bin {
                        op: BinKind::Identical,
                        a: sk,
                        b: v,
                        dst: t,
                    });
                    arm_jumps.push((i, self.emit(Op::JmpNz(Operand::Tmp(t), 0))));
                }
            }
            no_match = Some(self.emit(Op::Jmp(0)));
        }
        if default_arm.is_none() {
            let here = self.here();
            if let Some(j) = no_match {
                self.patch(j, here);
            }
            self.set_match_default(table, here);
            self.emit(Op::MatchError { subject: sk });
        }
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for (i, arm) in arms.iter().enumerate() {
            let here = self.here();
            starts.push(here);
            if default_arm == Some(i) {
                if let Some(j) = no_match {
                    self.patch(j, here);
                }
                self.set_match_default(table, here);
            }
            self.f().line = arm.body.span.line;
            let v = self.compile_expr(&arm.body)?;
            self.emit(Op::Move {
                src: v,
                dst: result,
            });
            ends.push(self.emit(Op::Jmp(0)));
        }
        for (i, j) in arm_jumps {
            self.patch(j, starts[i]);
        }
        if let (Some(at), Some(keys)) = (table, keys) {
            if let Op::Match { table, .. } = &mut self.f().ops[at] {
                for (arm, key) in keys {
                    table.insert(key, starts[arm]);
                }
            }
        }
        for j in ends {
            self.patch_here(j);
        }
        self.free(s);
        Ok(Operand::Tmp(result))
    }

    /// Sets the default target of the [`Op::Match`] at `at`, if any.
    fn set_match_default(&mut self, at: Option<usize>, target: Label) {
        if let Some(at) = at {
            if let Op::Match { table, .. } = &mut self.f().ops[at] {
                table.default = target;
            }
        }
    }

    pub(crate) fn emit_init_fcall(&mut self, n: &ast::Name) {
        let (name, fb) = self.resolve_function_name(n);
        let display = self.lit_str(&name);
        let lname = self.lit_str(&name.to_ascii_lowercase());
        let fbl = fb.map(|f| self.lit_str(&f.to_ascii_lowercase()));
        self.emit(Op::InitFCall {
            name: lname,
            fallback: fbl,
            display,
        });
    }

    /// Compiles calls, `new` and the pipe operator; returns the result
    /// operand (`None` when `need` is false and the call's result is
    /// unused).
    pub(crate) fn compile_call_like(&mut self, e: &Expr, need: bool) -> CResult<Option<Operand>> {
        if let ExprKind::Pipe { left, right } = &e.kind {
            return self.compile_pipe(left, right, e.span.line, need);
        }
        self.compile_call_like_ex(e, need, None)
    }

    /// `zend_compile_pipe()`: the left operand is evaluated first, then
    /// sent to the right-hand side. A right-hand side that
    /// `zend_partial_apply()` accepts (see [`pipe_partial_applies`]) is
    /// compiled as the plain call with the operand where the placeholder
    /// stands (`$x |> foo(1, ?)` is `foo(1, $x)`, after `$x`): no Closure
    /// is created. Anything else is `($rhs)($x)`, the right-hand side
    /// compiled after the operand and called dynamically.
    fn compile_pipe(
        &mut self,
        left: &Expr,
        right: &Expr,
        line: u32,
        need: bool,
    ) -> CResult<Option<Operand>> {
        let l = self.compile_expr(left)?;
        // A variable is copied into a temporary (`ZEND_QM_ASSIGN`) so that
        // a by-reference parameter never binds to it, and so that the
        // value sent is the one read before the other arguments run.
        let piped = match l {
            Operand::Cv(_) | Operand::TmpKeep(_) => {
                let t = self.tmp();
                self.emit(Op::Move { src: l, dst: t });
                Operand::Tmp(t)
            }
            other => other,
        };
        if pipe_partial_applies(right) {
            return self.compile_call_like_ex(right, need, Some(piped));
        }
        let c = self.compile_expr(right)?;
        self.emit(Op::InitDynCall { callee: c });
        self.emit(Op::Send {
            src: ArgSrc::Val(piped),
        });
        self.f().line = line;
        let dst = need.then(|| self.tmp());
        self.emit(Op::DoCall { dst });
        Ok(dst.map(Operand::Tmp))
    }

    /// [`Self::compile_call_like`] for a call or `new`; `pipe` is the
    /// operand of a pipe whose right-hand side is this call, sent where
    /// the call's only placeholder stands ([`Self::compile_pipe`]).
    #[allow(clippy::too_many_lines)]
    fn compile_call_like_ex(
        &mut self,
        e: &Expr,
        need: bool,
        pipe: Option<Operand>,
    ) -> CResult<Option<Operand>> {
        let line = e.span.line;
        let partial = pipe.is_none() && args_of(e).is_some_and(ArgList::is_partial);
        match &e.kind {
            ExprKind::Call { callee, args } => {
                // `zend_compile_call`: a string literal callee (`'f'()`,
                // `('\f')()`) is a fully qualified static call.
                let literal = match &**callee {
                    Callee::Expr(ex) => literal_function_name(ex),
                    Callee::Name(_) => None,
                };
                let name = match &**callee {
                    Callee::Name(n) => Some(n),
                    Callee::Expr(_) => literal.as_ref(),
                };
                let mut fbc = None;
                let mut frameless = false;
                if let Some(n) = name {
                    if !partial {
                        // (A piped call is never a special function: their
                        // compilers read the arguments from the syntax tree,
                        // where the piped operand has no node. It is the
                        // regular call by name.)
                        if pipe.is_none() {
                            if let Some(r) = self.compile_special_call(n, args, line, need)? {
                                return Ok(Some(r));
                            }
                        }
                        fbc = self.known_fbc(n);
                        frameless = fbc.is_none() && self.frameless_ns_call(n, args);
                    }
                    self.emit_init_fcall(n);
                } else if let Callee::Expr(ex) = &**callee {
                    let c = self.compile_chain(ex)?;
                    self.f().line = line;
                    self.emit(Op::InitDynCall { callee: c });
                }
                if !frameless {
                    return self.finish_call_at(args, line, line, need, fbc, pipe);
                }
                // `zend_compile_ns_call` with a frameless branch: the
                // arguments are compiled for the namespaced call (calls in
                // them get no frameless branch of their own), then again,
                // for reading, for `ZEND_FRAMELESS_ICALL_n`.
                self.f().in_frameless_branch = true;
                let r = self.finish_call_at(args, line, line, need, None, pipe);
                self.f().in_frameless_branch = false;
                let r = r?;
                self.check_frameless_args(args)?;
                Ok(r)
            }
            ExprKind::MethodCall {
                object,
                name,
                args,
                nullsafe,
            } => {
                let checkpoint = self.f().sc_jumps.len();
                let o = if is_this_var(object) {
                    // Throws without `$this`: `?->` needs no JMP_NULL.
                    self.method_call_this(object)?
                } else {
                    let o = self.compile_chain(object)?;
                    if *nullsafe {
                        let chain = self.null_chain();
                        let j = self.emit(Op::JmpNull {
                            a: Compiler::keep(o),
                            dst: 0,
                            target: 0,
                            chain,
                        });
                        self.f().sc_jumps.push(j);
                    }
                    o
                };
                // zend_compile_method_call(): a callable conversion inside a
                // short-circuiting chain.
                if partial && self.f().sc_jumps.len() != checkpoint {
                    return self.err(
                        "Cannot combine nullsafe operator with Closure creation",
                        line,
                    );
                }
                // INIT_METHOD_CALL is emitted on the line the method name
                // left (`zend_compile_method_call`): "Call to undefined
                // method" in a chain split over lines names that line.
                let n = self.method_member_name(name, line)?;
                self.emit(Op::InitMethodCall { obj: o, name: n });
                self.finish_call_at(args, line, member_name_line(name), need, None, pipe)
            }
            ExprKind::StaticCall { class, name, args }
                if self.parent_hook_property(class, name).is_some() =>
            {
                let prop = self
                    .parent_hook_property(class, name)
                    .expect("parent hook call");
                let MemberName::Ident(kind) = name else {
                    unreachable!("parent hook call")
                };
                let is_set = kind.eq_ignore_case("set");
                let kname = if is_set { "set" } else { "get" };
                match self.hook_prop.clone() {
                    None => {
                        return self.err(
                            format!(
                                "Must not use parent::${}::{kname}() outside a property hook",
                                prop.to_string_lossy()
                            ),
                            line,
                        )
                    }
                    Some(hp) if hp != prop.as_bytes() => {
                        return self.err(
                            format!(
                                "Must not use parent::${}::{kname}() in a different property (${})",
                                prop.to_string_lossy(),
                                String::from_utf8_lossy(&hp)
                            ),
                            line,
                        )
                    }
                    Some(_) => {
                        let cur = self.fref().method_name.to_string_lossy();
                        if !cur.ends_with(&format!("::{kname}")) {
                            let other = if is_set { "get" } else { "set" };
                            return self.err(
                                format!(
                                    "Must not use parent::${}::{kname}() in a different property hook ({other})",
                                    prop.to_string_lossy()
                                ),
                                line,
                            );
                        }
                    }
                }
                let p = self.lit_str(prop.as_bytes());
                self.f().line = line;
                self.emit(Op::InitParentHook {
                    prop: p,
                    set: is_set,
                });
                self.finish_call_at(args, line, line, need, None, pipe)
            }
            ExprKind::StaticCall { class, name, args } => {
                let spec = self.chain_class_spec(class)?;
                let n = self.method_member_name(name, line)?;
                self.emit(Op::InitStaticCall {
                    class: spec,
                    name: n,
                });
                self.finish_call_at(args, line, member_name_line(name), need, None, pipe)
            }
            ExprKind::New { class, args } => {
                // `ZEND_AST_NEW` takes the line of its class child, which
                // NEW and the constructor's DO_FCALL report.
                let line = match &**class {
                    NewClass::Name(n) => n.span.line,
                    NewClass::Expr(ex) => ex.span.line,
                    NewClass::Anonymous(_) => line,
                };
                let spec = match &**class {
                    NewClass::Name(n) => self.class_spec_name(n)?,
                    NewClass::Expr(ex) => {
                        let v = self.compile_expr(ex)?;
                        ClassSpec::Dyn(v)
                    }
                    NewClass::Anonymous(decl) => {
                        let idx = self.compile_class_decl(decl, true)?;
                        let t = self.tmp();
                        self.f().line = line;
                        self.emit(Op::DeclareAnonClass { idx, dst: t });
                        ClassSpec::Dyn(Operand::Tmp(t))
                    }
                };
                if args.is_partial() {
                    return self.err("Cannot create Closure for new expression", line);
                }
                self.f().line = line;
                let r = self.tmp();
                if args.is_empty() {
                    let at = self.emit(Op::New {
                        class: spec,
                        dst: r,
                        skip: 0,
                    });
                    self.f().line = line;
                    self.emit(Op::DoCall { dst: None });
                    self.patch_here(at);
                } else {
                    self.emit(Op::New {
                        class: spec,
                        dst: r,
                        skip: 0,
                    });
                    self.compile_args(args, None)?;
                    self.f().line = line;
                    self.emit(Op::DoCall { dst: None });
                }
                Ok(Some(Operand::Tmp(r)))
            }
            _ => Ok(Some(self.compile_expr(e)?)),
        }
    }

    /// The arguments and the `DoCall` of a call (`zend_compile_call_common`)
    /// whose `DoCall` reports `call_line` (the call site backtraces show):
    /// method and static calls use the line of the method name
    /// (`zend_compile_call_common(..., method_ast lineno)`). `fbc` is the
    /// called function when the compiler knows it ([`Self::known_fbc`]).
    /// `pipe` is a piped operand that takes the place of the only
    /// placeholder ([`Self::compile_pipe`]), making the call a plain one.
    fn finish_call_at(
        &mut self,
        args: &ArgList,
        line: u32,
        call_line: u32,
        need: bool,
        fbc: Option<super::FunctionInfo>,
        pipe: Option<Operand>,
    ) -> CResult<Option<Operand>> {
        if pipe.is_some() {
            self.compile_args_piped(args, fbc.as_ref(), pipe)?;
            return Ok(self.emit_do_call(call_line, need));
        }
        if args.is_first_class_callable() {
            let t = self.tmp();
            self.f().line = line;
            let const_expr =
                self.in_const_expr() || (self.in_global_const && self.fref().kind == FnKind::Main);
            self.emit(Op::CallableConvert { dst: t, const_expr });
            return Ok(Some(Operand::Tmp(t)));
        }
        if args.is_partial() {
            let (variadic, const_args) = self.compile_partial_args(args)?;
            let pfa = self.closure_name(line);
            let pfa = ZStr::from(pfa.as_bytes().strip_prefix(b"{closure:").map_or_else(
                || pfa.as_bytes().to_vec(),
                |r| [b"{closure:pfa:".as_slice(), r].concat(),
            ));
            let name = self.lit_idx(Value::Str(pfa));
            let t = self.tmp();
            self.f().line = line;
            self.emit(Op::CallablePartial {
                dst: t,
                variadic,
                const_args,
                name,
            });
            return Ok(Some(Operand::Tmp(t)));
        }
        self.compile_args(args, fbc.as_ref())?;
        Ok(self.emit_do_call(call_line, need))
    }

    /// The `DoCall` of [`Self::finish_call_at`], after the arguments.
    fn emit_do_call(&mut self, call_line: u32, need: bool) -> Option<Operand> {
        // `zend_compile_call_common` gives the call opcode the call's own
        // line (`opline->lineno = lineno`) but leaves `CG(zend_lineno)`
        // where compiling the last argument put it, so the opcodes that
        // consume the result (ECHO, ROPE_ADD, CAST, VERIFY_RETURN_TYPE, ...)
        // report that line.
        let after_args = self.f().line;
        self.f().line = call_line;
        let dst = need.then(|| self.tmp());
        self.emit(Op::DoCall { dst });
        self.f().line = after_args;
        dst.map(Operand::Tmp)
    }

    /// Arguments of a partial application (`zend_compile_args_ex` with
    /// `is_call_partial`): returns (uses `...`, literal-argument mask).
    fn compile_partial_args(&mut self, args: &ArgList) -> CResult<(bool, u32)> {
        let mut uses_named = false;
        let mut variadic = false;
        let mut const_args = 0u32;
        let mut pos = 0u32;
        for a in &args.args {
            self.f().line = a.span.line;
            let line = a.span.line;
            match &a.kind {
                ArgKind::Unpack(_) => {
                    if uses_named {
                        return self
                            .err("Cannot use argument unpacking after named arguments", line);
                    }
                    return self.err("Cannot combine partial application and unpacking", line);
                }
                ArgKind::Named { .. } | ArgKind::NamedPlaceholder(_) => {
                    uses_named = true;
                    if variadic {
                        return self.err("Variadic placeholder must be last", line);
                    }
                }
                ArgKind::Positional(_) | ArgKind::Placeholder | ArgKind::VariadicPlaceholder => {
                    let is_var = matches!(a.kind, ArgKind::VariadicPlaceholder);
                    if uses_named && !is_var {
                        return self
                            .err("Cannot use positional argument after named argument", line);
                    }
                    if variadic {
                        return self.err(
                            if is_var {
                                "Variadic placeholder may only appear once"
                            } else {
                                "Variadic placeholder must be last"
                            },
                            line,
                        );
                    }
                }
            }
            match &a.kind {
                ArgKind::Positional(ex) => {
                    let constant_return = if let ExprKind::Call { callee, args } = &ex.kind {
                        if let Callee::Name(name) = &**callee {
                            let (resolved, _) = self.resolve_function_name(name);
                            let functions = self.decls.functions.borrow();
                            self.hoisted_functions.iter().any(|index| {
                                let function = &functions[*index as usize];
                                args.args.is_empty()
                                    && function.name.eq_ignore_ascii_case(&resolved)
                                    && function.params.is_empty()
                                    && function.return_type.is_none()
                                    && function.flags & flags::RETURN_REF == 0
                                    && matches!(
                                        function.code.ops.first(),
                                        Some(Op::Return {
                                            val: Operand::Lit(_)
                                        })
                                    )
                            })
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if pos < 32
                        && (constant_return
                            || (!matches!(ex.kind, ExprKind::Unary { .. })
                                && self.try_fold(ex).is_some()))
                    {
                        const_args |= 1 << pos;
                    }
                    let src = self.arg_src_for(ex)?;
                    self.emit(Op::Send { src });
                    pos += 1;
                }
                ArgKind::Named { name, value } => {
                    let n = self.lit_str(&name.name);
                    let src = self.arg_src_for(value)?;
                    self.emit(Op::SendNamed { name: n, src });
                }
                ArgKind::Placeholder => {
                    self.emit(Op::SendPlaceholder { name: None });
                    pos += 1;
                }
                ArgKind::NamedPlaceholder(id) => {
                    let n = self.lit_str(&id.name);
                    self.emit(Op::SendPlaceholder { name: Some(n) });
                }
                ArgKind::VariadicPlaceholder => variadic = true,
                ArgKind::Unpack(_) => {}
            }
        }
        Ok((variadic, const_args))
    }

    /// `zend_compile_args`' `fbc` for a call by name (`zend_compile_call`):
    /// known unless the name is resolved at run time (unqualified in a
    /// namespace), when `CG(function_table)` holds it: a finalized
    /// top-level function of this file, or a function declared when the
    /// compilation started.
    pub(crate) fn known_fbc(&self, n: &ast::Name) -> Option<super::FunctionInfo> {
        let (name, fallback) = self.resolve_function_name(n);
        if fallback.is_some() {
            return None;
        }
        let lc = name.to_ascii_lowercase();
        if let Some(info) = self.known_functions.get(&lc) {
            return Some(info.clone());
        }
        self.functions?.find(&lc)
    }

    /// php-src master's frameless branch of a namespaced call
    /// (`zend_compile_ns_call`): the global function the name falls back to
    /// is an internal function with a frameless handler for this many
    /// arguments (`find_frameless_function_info`), there is no unpacking
    /// or named argument, and the call is not itself an argument of a call
    /// with such a branch (`CG(context).in_jmp_frameless_branch`).
    fn frameless_ns_call(&self, n: &ast::Name, args: &ArgList) -> bool {
        if self.fref().in_frameless_branch || args.args.len() > 3 {
            return false;
        }
        if args
            .args
            .iter()
            .any(|a| !matches!(a.kind, ArgKind::Positional(_)))
        {
            return false;
        }
        let (_, Some(fallback)) = self.resolve_function_name(n) else {
            return false;
        };
        let lc = fallback.to_ascii_lowercase();
        let Some(arities) = super::frameless_arities(&lc) else {
            return false;
        };
        let Some(fbc) = self.functions.and_then(|t| t.find(&lc)) else {
            return false;
        };
        let count = u32::try_from(args.args.len()).unwrap_or(u32::MAX);
        fbc.internal
            && arities.iter().any(|&arity| {
                arity >= count && fbc.required <= count && (!fbc.variadic || arity == count)
            })
    }

    /// The arguments of a frameless branch, compiled for reading
    /// (`zend_compile_frameless_icall_ex`): "Cannot use [] for reading".
    fn check_frameless_args(&self, args: &ArgList) -> CResult<()> {
        for a in &args.args {
            if let ArgKind::Positional(e) = &a.kind {
                if reads_append(e) {
                    return self.err("Cannot use [] for reading", e.span.line);
                }
            }
        }
        Ok(())
    }

    /// The arguments of a call (`zend_compile_args`). With a known `fbc`,
    /// an argument for a by-value parameter is compiled for reading
    /// (`BP_VAR_R`): `f($a[])` and `f($a[]->b)` are the compile error
    /// "Cannot use [] for reading". Otherwise a variable is a
    /// `BP_VAR_FUNC_ARG` fetch, which appends or throws at run time. A
    /// named argument for a parameter of a known `fbc` (`zend_get_arg_num`)
    /// is compiled for that parameter's mode.
    pub(crate) fn compile_args(
        &mut self,
        args: &ArgList,
        fbc: Option<&super::FunctionInfo>,
    ) -> CResult<()> {
        self.compile_args_piped(args, fbc, None)
    }

    /// [`Self::compile_args`] of a call whose placeholder takes a piped
    /// operand (`zend_partial_apply` puts the operand's node in the
    /// placeholder's place, so `zend_compile_args` sends it as a positional
    /// or named value). Without `pipe`, placeholders send nothing.
    fn compile_args_piped(
        &mut self,
        args: &ArgList,
        fbc: Option<&super::FunctionInfo>,
        mut pipe: Option<Operand>,
    ) -> CResult<()> {
        let mut seen_named = false;
        let mut seen_unpack = false;
        for (i, a) in args.args.iter().enumerate() {
            self.f().line = a.span.line;
            match &a.kind {
                ArgKind::Positional(ex) => {
                    if seen_named {
                        return self.err(
                            "Cannot use positional argument after named argument",
                            a.span.line,
                        );
                    }
                    if seen_unpack {
                        return self.err(
                            "Cannot use positional argument after argument unpacking",
                            a.span.line,
                        );
                    }
                    if fbc.is_some_and(|f| !f.sends_by_ref(i)) && reads_append(ex) {
                        return self.err("Cannot use [] for reading", ex.span.line);
                    }
                    let src =
                        if fbc.is_some_and(|f| f.internal && !f.sends_by_ref(i)) && read_arg(ex) {
                            // `zend_compile_args` with a known `fbc` and a
                            // by-value parameter (not prefer-ref):
                            // `zend_compile_var(..., BP_VAR_R)`, the ordinary
                            // FETCH_DIM_R / FETCH_OBJ_R into a temporary, sent
                            // as a value.
                            ArgSrc::Val(self.compile_expr(ex)?)
                        } else {
                            self.arg_src_for(ex)?
                        };
                    self.emit(Op::Send { src });
                }
                ArgKind::Unpack(ex) => {
                    if seen_named {
                        return self.err(
                            "Cannot use argument unpacking after named arguments",
                            a.span.line,
                        );
                    }
                    seen_unpack = true;
                    let v = self.compile_expr(ex)?;
                    self.emit(Op::SendUnpack { src: v });
                }
                ArgKind::Named { name, value } => {
                    seen_named = true;
                    // An unpacked argument makes `fbc` unknown. (php-src also
                    // sends an argument in its place positionally; a named send
                    // behaves the same, and keeps the call off the frameless
                    // path, as `zend_args_contain_unpack_or_named` does.)
                    let by_value = fbc.filter(|_| !seen_unpack).is_some_and(|f| {
                        f.arg_num(&name.name)
                            .is_some_and(|n| !f.sends_by_ref(n - 1))
                    });
                    if by_value && reads_append(value) {
                        return self.err("Cannot use [] for reading", value.span.line);
                    }
                    let n = self.lit_str(&name.name);
                    let src = self.arg_src_for(value)?;
                    self.emit(Op::SendNamed { name: n, src });
                }
                ArgKind::Placeholder | ArgKind::VariadicPlaceholder => {
                    let Some(p) = pipe.take() else { continue };
                    if seen_named {
                        return self.err(
                            "Cannot use positional argument after named argument",
                            a.span.line,
                        );
                    }
                    if seen_unpack {
                        return self.err(
                            "Cannot use positional argument after argument unpacking",
                            a.span.line,
                        );
                    }
                    self.emit(Op::Send {
                        src: ArgSrc::Val(p),
                    });
                }
                ArgKind::NamedPlaceholder(id) => {
                    let Some(p) = pipe.take() else { continue };
                    seen_named = true;
                    let n = self.lit_str(&id.name);
                    self.emit(Op::SendNamed {
                        name: n,
                        src: ArgSrc::Val(p),
                    });
                }
            }
        }
        Ok(())
    }

    /// `zend_compile_func_array_map()`: `array_map(f(...), $array)` and
    /// `array_map(C::m(...), $array)` compile into a foreach loop calling the
    /// callback directly, so no Closure is created and no `array_map()`
    /// frame shows in backtraces. `None` compiles a regular call (other
    /// callbacks, or a literal that is not an array).
    fn compile_array_map_foreach(
        &mut self,
        callback: &Expr,
        array: &Expr,
        line: u32,
    ) -> CResult<Option<Operand>> {
        enum Target<'e> {
            Func(&'e ast::Name),
            /// A function name computed before the array (`$f(...)`).
            DynFunc(Operand),
            Static(ClassSpec, ZStr),
        }
        // Cheap shape checks first: nothing is emitted for a callback the
        // optimisation does not cover.
        let pfa = match &callback.kind {
            ExprKind::Call { args, callee, .. } => {
                // `zend_is_pipe_optimizable_callable_name`.
                if matches!(&**callee, Callee::Name(n) if n.name.eq_ignore_ascii_case(b"assert")) {
                    return Ok(None);
                }
                args
            }
            ExprKind::StaticCall {
                name: MemberName::Ident(_),
                args,
                ..
            } => args,
            _ => return Ok(None),
        };
        if !partial_applies(pfa) {
            return Ok(None);
        }
        let mark = self.fref().ops.len();
        let num_tmps = self.fref().num_tmps;
        // The class, then the function name, are evaluated once, before
        // the array, into temporaries the loop reuses and frees afterwards.
        let mut owned_tmps: Vec<u32> = Vec::new();
        let target = match &callback.kind {
            ExprKind::Call { callee, .. } => match &**callee {
                Callee::Name(n) => Target::Func(n),
                Callee::Expr(ex) => {
                    let c = self.compile_expr(ex)?;
                    Target::DynFunc(self.array_map_operand(c, &mut owned_tmps))
                }
            },
            ExprKind::StaticCall {
                class,
                name: MemberName::Ident(id),
                ..
            } => {
                let spec = match &**class {
                    ast::ClassRef::Name(cn) => self.class_spec_name(cn)?,
                    ast::ClassRef::Expr(e) => {
                        if !matches!(self.try_fold(e), None | Some(Value::Str(_))) {
                            return self.err("Illegal class name", e.span.line);
                        }
                        let c = self.compile_expr(e)?;
                        ClassSpec::Dyn(self.array_map_operand(c, &mut owned_tmps))
                    }
                };
                Target::Static(spec, ZStr::from(id.name.as_slice()))
            }
            _ => return Ok(None),
        };
        let arr = self.compile_expr(array)?;
        if let Operand::Lit(i) = arr {
            if !matches!(self.fref().literals.get(i as usize), Some(Value::Array(_))) {
                let f = self.f();
                f.ops.truncate(mark);
                f.lines.truncate(mark);
                f.num_tmps = num_tmps;
                return Ok(None);
            }
        }
        self.f().line = line;
        let msg = self.lit_str(b"array_map(): Argument #2 ($array) must be of type array");
        self.emit(Op::TypeAssertArray {
            val: Compiler::keep(arr),
            msg,
        });
        let result = self.tmp();
        self.emit(Op::InitArray { dst: result });
        let iter = {
            let f = self.f();
            f.num_iters += 1;
            f.num_iters - 1
        };
        let reset = self.emit(Op::FeReset {
            subject: arr,
            place: None,
            kind: crate::opcode::ForeachKind::Value,
            iter,
            end: 0,
        });
        let fetch_at = self.here();
        let reset_at = u32::try_from(reset).unwrap_or(u32::MAX);
        self.f().iter_starts.insert(iter, reset_at);
        let val = self.tmp();
        let key = self.tmp();
        let fetch = self.emit(Op::FeFetch {
            iter,
            end: 0,
            val,
            key: Some(key),
        });
        match target {
            Target::Func(n) => self.emit_init_fcall(n),
            Target::DynFunc(callee) => {
                self.emit(Op::InitDynCall { callee });
            }
            Target::Static(class, m) => {
                let name = self.lit(Value::Str(m));
                self.emit(Op::InitStaticCall { class, name });
            }
        }
        // `zend_partial_apply`: the element where the placeholder stands,
        // the literals where they stand.
        for a in &pfa.args {
            let elem = ArgSrc::Val(Operand::Tmp(val));
            match &a.kind {
                ArgKind::Placeholder | ArgKind::VariadicPlaceholder => {
                    self.emit(Op::Send { src: elem });
                }
                ArgKind::NamedPlaceholder(id) => {
                    let name = self.lit_str(&id.name);
                    self.emit(Op::SendNamed { name, src: elem });
                }
                ArgKind::Positional(e) => {
                    let lit = self.compile_expr(e)?;
                    self.emit(Op::Send {
                        src: ArgSrc::Val(lit),
                    });
                }
                ArgKind::Named { name, value } => {
                    let n = self.lit_str(&name.name);
                    let lit = self.compile_expr(value)?;
                    self.emit(Op::SendNamed {
                        name: n,
                        src: ArgSrc::Val(lit),
                    });
                }
                // (Refused by `partial_applies`.)
                ArgKind::Unpack(_) => {}
            }
        }
        let ret = self.tmp();
        self.emit(Op::DoCall { dst: Some(ret) });
        self.emit(Op::ArrayAdd {
            arr: result,
            key: Some(Operand::Tmp(key)),
            val: Operand::Tmp(ret),
        });
        self.emit(Op::Jmp(fetch_at));
        self.patch_here(reset);
        self.patch_here(fetch);
        self.emit(Op::FeFree { iter });
        // `ZEND_FREE` of the function name, then of the class.
        for t in owned_tmps.into_iter().rev() {
            self.emit(Op::Free(Operand::Tmp(t)));
        }
        Ok(Some(Operand::Tmp(result)))
    }
}

/// The argument list of a call or `new`.
fn args_of(e: &Expr) -> Option<&ArgList> {
    match &e.kind {
        ExprKind::Call { args, .. }
        | ExprKind::MethodCall { args, .. }
        | ExprKind::StaticCall { args, .. }
        | ExprKind::New { args, .. } => Some(args),
        _ => None,
    }
}

/// `zend_partial_apply()` for `zend_compile_pipe()`: whether `$x |> e`
/// compiles as the plain call `e` with `$x` in its placeholder's place.
/// `e` is a function, method or static-method call (not `new`, not `?->`)
/// that is a partial application with exactly one placeholder: `?`,
/// `name: ?` or `...` (so `f(...)` too), and not `...` after a named
/// argument (`f(a: 1, ...)` would become a positional argument after a
/// named one). A call of a function named `assert` is never rewritten
/// (`zend_is_pipe_optimizable_callable_name`): its compilation adds the
/// message argument.
fn pipe_partial_applies(e: &Expr) -> bool {
    let args = match &e.kind {
        ExprKind::Call { callee, args } => {
            let assert = match &**callee {
                Callee::Name(n) => n.name.eq_ignore_ascii_case(b"assert"),
                Callee::Expr(ex) => {
                    matches!(&ex.kind, ExprKind::String(s) if s.eq_ignore_ascii_case(b"assert"))
                }
            };
            if assert {
                return false;
            }
            args
        }
        ExprKind::MethodCall {
            args,
            nullsafe: false,
            ..
        }
        | ExprKind::StaticCall { args, .. } => args,
        _ => return false,
    };
    let mut placeholders = 0u32;
    let mut named = false;
    for a in &args.args {
        match &a.kind {
            ArgKind::Named { .. } => named = true,
            ArgKind::NamedPlaceholder(_) => {
                named = true;
                placeholders += 1;
            }
            ArgKind::Placeholder => placeholders += 1,
            ArgKind::VariadicPlaceholder => {
                if named {
                    return false;
                }
                placeholders += 1;
            }
            ArgKind::Positional(_) | ArgKind::Unpack(_) => {}
        }
    }
    placeholders == 1
}

/// Whether the arguments `args` of `array_map()`'s callback make a partial
/// application the loop can call directly (`zend_compile_func_array_map`
/// with `zend_partial_apply`): exactly one placeholder (`?`, `name: ?` or
/// `...`), every other argument an integer, float or string literal, and no
/// variadic placeholder together with named arguments.
fn partial_applies(args: &ArgList) -> bool {
    let mut placeholders = 0;
    let mut variadic = false;
    let mut named = false;
    for a in &args.args {
        match &a.kind {
            ArgKind::Placeholder | ArgKind::NamedPlaceholder(_) => placeholders += 1,
            ArgKind::VariadicPlaceholder => {
                placeholders += 1;
                variadic = true;
            }
            ArgKind::Positional(e) | ArgKind::Named { value: e, .. } => {
                if !matches!(
                    e.kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
                ) {
                    return false;
                }
                named |= matches!(a.kind, ArgKind::Named { .. });
            }
            ArgKind::Unpack(_) => return false,
        }
    }
    placeholders == 1 && !(variadic && named)
}

impl Compiler<'_> {
    /// An operand of [`Self::compile_array_map_foreach`]'s loop: a literal
    /// as is; anything else in a temporary (`ZEND_QM_ASSIGN` of a
    /// variable) that every iteration reads without consuming.
    fn array_map_operand(&mut self, op: Operand, owned: &mut Vec<u32>) -> Operand {
        let t = match op {
            Operand::Lit(_) => return op,
            Operand::Tmp(t) => t,
            other => {
                let t = self.tmp();
                self.emit(Op::Move { src: other, dst: t });
                t
            }
        };
        owned.push(t);
        Operand::TmpKeep(t)
    }

    /// Compile-time special functions. Returns `None` when `n` is not one.
    #[allow(clippy::too_many_lines)] // one arm per special function, as zend_try_compile_special_func
    fn compile_special_call(
        &mut self,
        n: &ast::Name,
        args: &ArgList,
        line: u32,
        need: bool,
    ) -> CResult<Option<Operand>> {
        let (resolved, fb) = self.resolve_function_name(n);
        let lc = fb.as_ref().unwrap_or(&resolved).to_ascii_lowercase();
        let is_global = fb.is_some() || !resolved.contains(&b'\\');
        if !is_global {
            return Ok(None);
        }
        let positional: Vec<&Expr> = args
            .args
            .iter()
            .filter_map(|a| match &a.kind {
                ArgKind::Positional(e) => Some(e),
                _ => None,
            })
            .collect();
        let only_positional = positional.len() == args.args.len();
        match lc.as_slice() {
            b"sprintf" if fb.is_none() && only_positional && self.opts.sprintf_builtin => {
                self.compile_sprintf_rope(&positional, line)
            }
            // zend_compile_func_array_key_exists: both arguments compiled
            // for reading, then one opcode in the caller's frame. Like
            // `zend_emit_op_tmp`, it takes the line the arguments left
            // behind (`CG(zend_lineno)`), where its diagnostics appear.
            b"array_key_exists"
                if fb.is_none()
                    && only_positional
                    && positional.len() == 2
                    && self.special_builtin_enabled(b"array_key_exists") =>
            {
                let key = self.compile_expr(positional[0])?;
                let array = self.compile_expr(positional[1])?;
                let dst = need.then(|| self.tmp());
                self.emit(Op::KeyExists { key, array, dst });
                Ok(Some(
                    dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
                ))
            }
            // zend_compile_func_defined / _in_array / _chr / _ord decide
            // from the literal arguments and otherwise keep the call.
            b"defined" if fb.is_none() && only_positional && positional.len() == 1 => {
                Ok(self.compile_defined(positional[0], need))
            }
            b"in_array" if fb.is_none() && only_positional => {
                self.compile_in_array(&positional, need)
            }
            b"chr" | b"ord" if fb.is_none() && only_positional && positional.len() == 1 => {
                Ok(self.fold_chr_ord(&lc, positional[0]))
            }
            // zend_compile_func_array_slice: `array_slice(func_get_args(), n)`
            // inside a function, `n` a non-negative int literal.
            b"array_slice"
                if fb.is_none()
                    && only_positional
                    && positional.len() == 2
                    && !self.f().func_name.is_empty()
                    && self.special_builtin_enabled(b"array_slice") =>
            {
                let ExprKind::Int(n) = positional[1].kind else {
                    return Ok(None);
                };
                let (Ok(skip), true) = (u32::try_from(n), self.is_func_get_args(positional[0]))
                else {
                    return Ok(None);
                };
                self.f().line = line;
                let dst = need.then(|| self.tmp());
                self.emit(Op::FuncGetArgs {
                    dst,
                    skip: Some(skip),
                });
                Ok(Some(
                    dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
                ))
            }
            // Without `array_key_exists` registered as an internal function,
            // only the float-key lifetime case finishes a pending call.
            b"array_key_exists" if fb.is_none() && only_positional && positional.len() == 2 => {
                if !matches!(
                    positional[0].kind,
                    ExprKind::Float(f) if !f.is_finite() || !php_runtime::convert::double_fits_long(f)
                ) {
                    return Ok(None);
                }
                self.emit_init_fcall(n);
                self.f().line = args.args[0].span.line;
                let src = self.arg_src_for(positional[0])?;
                self.emit(Op::Send { src });
                self.f().line = args.args[1].span.line;
                let array = self.arg_src_for(positional[1])?;
                self.f().line = line;
                let dst = need.then(|| self.tmp());
                self.emit(Op::ArrayKeyExists { array, dst });
                Ok(Some(
                    dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
                ))
            }
            b"array_map"
                if fb.is_none()
                    && only_positional
                    && positional.len() == 2
                    && self.opts.array_map_builtin
                    && !self.strict_types =>
            {
                self.compile_array_map_foreach(positional[0], positional[1], line)
            }
            // zend_compile_func_strlen: the length of a constant string is
            // a compile-time constant, and no call is made.
            b"strlen"
                if fb.is_none()
                    && only_positional
                    && positional.len() == 1
                    && self.opts.strlen_builtin =>
            {
                match self.try_fold(positional[0]) {
                    Some(Value::Str(s)) => {
                        let len = i64::try_from(s.len()).unwrap_or(i64::MAX);
                        Ok(Some(self.lit(Value::Int(len))))
                    }
                    _ => self.compile_special_opcode(&lc, positional[0], need),
                }
            }
            // zend_compile_func_strlen / _count / _typecheck / _is_scalar /
            // _cast / _gettype: one argument, a dedicated opcode.
            b"strlen" | b"count" | b"sizeof" | b"is_null" | b"is_bool" | b"is_int"
            | b"is_float" | b"is_string" | b"is_array" | b"is_object" | b"is_resource"
            | b"is_scalar" | b"boolval" | b"intval" | b"floatval" | b"strval" | b"gettype"
                if fb.is_none() && only_positional && positional.len() == 1 =>
            {
                self.compile_special_opcode(&lc, positional[0], need)
            }
            // zend_compile_func_get_class / zend_compile_func_get_called_class.
            b"get_class" if fb.is_none() && only_positional && positional.len() <= 1 => {
                self.compile_class_special(&lc, positional.first().copied(), line, need)
            }
            b"get_called_class" if fb.is_none() && args.args.is_empty() => {
                self.compile_class_special(&lc, None, line, need)
            }
            // zend_compile_func_num_args / zend_compile_func_get_args: only
            // inside a function body, and not as a namespaced fallback.
            b"func_num_args" | b"func_get_args"
                if fb.is_none() && args.args.is_empty() && !self.f().func_name.is_empty() =>
            {
                self.f().line = line;
                let dst = need.then(|| self.tmp());
                if lc.as_slice() == b"func_num_args" {
                    self.emit(Op::FuncNumArgs { dst });
                } else {
                    self.emit(Op::FuncGetArgs { dst, skip: None });
                }
                Ok(Some(
                    dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
                ))
            }
            // zend_compile_func_cuf / zend_compile_func_cufa: the callback
            // is called by the ops of a call, not by the builtin.
            b"call_user_func"
                if fb.is_none()
                    && only_positional
                    && !positional.is_empty()
                    && self.special_builtin_enabled(b"call_user_func") =>
            {
                self.compile_cuf(&positional, line, need)
            }
            b"call_user_func_array"
                if fb.is_none()
                    && only_positional
                    && positional.len() == 2
                    && self.special_builtin_enabled(b"call_user_func_array") =>
            {
                self.compile_cufa(positional[0], positional[1], line, need)
            }
            b"clone" if only_positional && positional.len() == 1 => {
                let v = self.compile_expr(positional[0])?;
                self.f().line = line;
                let t = self.tmp();
                self.emit(Op::Clone { a: v, dst: t });
                Ok(Some(Operand::Tmp(t)))
            }
            b"assert" => {
                if self.opts.assertions < 0 {
                    return Ok(Some(self.lit(Value::Bool(true))));
                }
                let t = self.tmp();
                let check = self.emit(Op::AssertCheck { dst: t, skip: 0 });
                self.emit_init_fcall(n);
                // `zend_compile_assert`: the internal `assert()` is the
                // `fbc`, except for a namespaced call.
                let fbc = self.known_fbc(n);
                self.compile_args(args, fbc.as_ref())?;
                if args.args.len() == 1 {
                    if matches!(args.args[0].kind, ArgKind::Unpack(_)) {
                        return self.err(
                            "Cannot use positional argument after argument unpacking",
                            line,
                        );
                    }
                    let msg = php_parser::export::assert_message(&args.args[0]);
                    let l = self.lit(Value::Str(ZStr::from(msg)));
                    if matches!(args.args[0].kind, ArgKind::Named { .. }) {
                        let name = self.lit_str(b"description");
                        self.emit(Op::SendNamed {
                            name,
                            src: ArgSrc::Val(l),
                        });
                    } else {
                        self.emit(Op::Send {
                            src: ArgSrc::Val(l),
                        });
                    }
                }
                self.f().line = line;
                self.emit(Op::DoCall { dst: Some(t) });
                self.patch_here(check);
                let _ = need;
                Ok(Some(Operand::Tmp(t)))
            }
            _ => Ok(None),
        }
    }

    /// `zend_compile_init_user_func`: the start of the call of a
    /// `call_user_func[_array]()` callback. A string literal naming a
    /// function known at compile time is bound then
    /// (`zend_try_compile_ct_bound_init_user_func`: `ZEND_INIT_FCALL`, not
    /// a dynamic call); anything else is compiled for reading and
    /// resolved by [`Op::InitUserCall`].
    fn compile_init_user_func(&mut self, callee: &Expr, line: u32, array: bool) -> CResult<()> {
        if let ExprKind::String(s) = &callee.kind {
            let lc = s.to_ascii_lowercase();
            if self.known_functions.contains_key(&lc)
                || self.functions.is_some_and(|t| t.find(&lc).is_some())
            {
                let display = self.lit_str(s);
                let name = self.lit_str(&lc);
                self.f().line = line;
                self.emit(Op::InitFCall {
                    name,
                    fallback: None,
                    display,
                });
                return Ok(());
            }
        }
        let callee = self.compile_expr(callee)?;
        self.emit(Op::InitUserCall { callee, array });
        Ok(())
    }

    /// `zend_compile_func_cuf`: `call_user_func($f, $a, ...)` as
    /// `ZEND_INIT_USER_CALL`, one `ZEND_SEND_USER` per argument and
    /// `ZEND_DO_FCALL`, each taking the line compiling what came before it
    /// left (`CG(zend_lineno)`).
    fn compile_cuf(&mut self, args: &[&Expr], line: u32, need: bool) -> CResult<Option<Operand>> {
        self.f().line = line;
        self.compile_init_user_func(args[0], line, false)?;
        for (i, a) in args.iter().enumerate().skip(1) {
            let src = self.compile_expr(a)?;
            let n = u32::try_from(i).unwrap_or(u32::MAX);
            self.emit(Op::SendUser { src, n });
        }
        let dst = need.then(|| self.tmp());
        self.emit(Op::DoCall { dst });
        Ok(Some(
            dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
        ))
    }

    /// `zend_compile_func_cufa`: `call_user_func_array($f, $args)` as
    /// `ZEND_INIT_USER_CALL`, `ZEND_SEND_ARRAY` and `ZEND_DO_FCALL`. An
    /// `$args` written `array_slice($a, n, $len)` (`n` a non-negative int
    /// literal) sends that slice of `$a` without calling `array_slice()`.
    fn compile_cufa(
        &mut self,
        callee: &Expr,
        args: &Expr,
        line: u32,
        need: bool,
    ) -> CResult<Option<Operand>> {
        self.f().line = line;
        self.compile_init_user_func(callee, line, true)?;
        if let Some((array, skip, len)) = self.cufa_slice(args) {
            let src = self.compile_expr(array)?;
            let len = self.compile_expr(len)?;
            self.emit(Op::SendArray { src, len, skip });
        } else {
            let src = self.compile_expr(args)?;
            self.emit(Op::SendArray {
                src,
                len: Operand::Unused,
                skip: 0,
            });
        }
        let dst = need.then(|| self.tmp());
        self.emit(Op::DoCall { dst });
        Ok(Some(
            dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
        ))
    }

    /// The array, offset and length of an `array_slice($a, n, $len)`
    /// argument of `call_user_func_array()` that `zend_compile_func_cufa`
    /// sends with `ZEND_SEND_ARRAY`'s length operand: a call named
    /// `array_slice` as resolved at compile time (not a namespaced
    /// fallback), with exactly three positional arguments, the second an
    /// int literal in `0..=0x7fffffff`.
    fn cufa_slice<'e>(&self, e: &'e Expr) -> Option<(&'e Expr, u32, &'e Expr)> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return None;
        };
        let literal = match &**callee {
            Callee::Expr(ex) => literal_function_name(ex),
            Callee::Name(_) => None,
        };
        let n = match &**callee {
            Callee::Name(n) => n,
            Callee::Expr(_) => literal.as_ref()?,
        };
        let (resolved, fb) = self.resolve_function_name(n);
        if fb.is_some() || !resolved.eq_ignore_ascii_case(b"array_slice") {
            return None;
        }
        let [a, b, c] = args.args.as_slice() else {
            return None;
        };
        let (ArgKind::Positional(array), ArgKind::Positional(skip), ArgKind::Positional(len)) =
            (&a.kind, &b.kind, &c.kind)
        else {
            return None;
        };
        let ExprKind::Int(skip) = skip.kind else {
            return None;
        };
        let skip = u32::try_from(skip).ok().filter(|s| *s <= 0x7fff_ffff)?;
        Some((array, skip, len))
    }

    /// The one-argument builtins `zend_try_compile_special_func_ex()`
    /// compiles to an opcode instead of a call (`ZEND_STRLEN`,
    /// `ZEND_COUNT`, `ZEND_TYPE_CHECK`, `ZEND_CAST`, `ZEND_GET_TYPE`): the
    /// argument is compiled for reading (`zend_compile_expr`), and no call
    /// frame exists while the opcode runs. `None` (a regular call) unless
    /// `lc` is registered as an internal function
    /// ([`super::CompileOptions::special_builtins`]).
    fn compile_special_opcode(
        &mut self,
        lc: &[u8],
        arg: &Expr,
        need: bool,
    ) -> CResult<Option<Operand>> {
        enum Special {
            Cast(CastKind),
            TypeCheck(u8),
            Strlen,
            Count { sizeof: bool },
            GetType,
        }
        let Some(bit) = super::special_builtin_bit(lc) else {
            return Ok(None);
        };
        if self.opts.special_builtins & bit == 0 {
            return Ok(None);
        }
        let special = match lc {
            b"boolval" => Special::Cast(CastKind::Bool),
            b"intval" => Special::Cast(CastKind::Int),
            b"floatval" => Special::Cast(CastKind::Float),
            b"strval" => Special::Cast(CastKind::String),
            b"is_null" => Special::TypeCheck(type_mask::NULL),
            b"is_bool" => Special::TypeCheck(type_mask::BOOL),
            b"is_int" => Special::TypeCheck(type_mask::INT),
            b"is_float" => Special::TypeCheck(type_mask::FLOAT),
            b"is_string" => Special::TypeCheck(type_mask::STRING),
            b"is_array" => Special::TypeCheck(type_mask::ARRAY),
            b"is_object" => Special::TypeCheck(type_mask::OBJECT),
            b"is_resource" => Special::TypeCheck(type_mask::RESOURCE),
            b"is_scalar" => Special::TypeCheck(type_mask::SCALAR),
            b"strlen" => Special::Strlen,
            b"count" => Special::Count { sizeof: false },
            b"sizeof" => Special::Count { sizeof: true },
            b"gettype" => Special::GetType,
            _ => return Ok(None),
        };
        // The opcode is emitted on the line compiling the argument left
        // (`zend_compile_func_strlen` and its siblings emit right after
        // `zend_compile_expr()` of the argument), so `strlen(\n$s\n)`
        // reports the line of `$s`.
        let a = self.compile_expr(arg)?;
        let dst = need.then(|| self.tmp());
        let op = match special {
            Special::Cast(kind) => {
                // zend_compile_func_cast: `ZEND_CAST` always has a result.
                let dst = dst.unwrap_or_else(|| self.tmp());
                self.emit(Op::Cast { kind, a, dst });
                return Ok(Some(Operand::Tmp(dst)));
            }
            Special::TypeCheck(mask) => Op::TypeCheck { a, mask, dst },
            Special::Strlen => Op::Strlen { a, dst },
            Special::Count { sizeof } => Op::Count { a, dst, sizeof },
            Special::GetType => Op::GetType { a, dst },
        };
        self.emit(op);
        Ok(Some(
            dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
        ))
    }

    /// `get_class([$x])` / `get_called_class()` as `ZEND_GET_CLASS` /
    /// `ZEND_GET_CALLED_CLASS` when they are registered builtins: the scope
    /// is the running function's, so no call frame is needed.
    fn compile_class_special(
        &mut self,
        lc: &[u8],
        arg: Option<&Expr>,
        line: u32,
        need: bool,
    ) -> CResult<Option<Operand>> {
        let Some(bit) = super::special_builtin_bit(lc) else {
            return Ok(None);
        };
        if self.opts.special_builtins & bit == 0 {
            return Ok(None);
        }
        // `ZEND_GET_CLASS` with an argument is emitted after compiling it,
        // on the line that left (see `compile_special_opcode`).
        let a = match arg {
            Some(e) => self.compile_expr(e)?,
            None => {
                self.f().line = line;
                Operand::Unused
            }
        };
        let dst = need.then(|| self.tmp());
        if lc == b"get_called_class".as_slice() {
            self.emit(Op::GetCalledClass { dst });
        } else {
            self.emit(Op::GetClass { a, dst });
        }
        Ok(Some(
            dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
        ))
    }

    /// Whether `e` is a call `func_get_args()` without arguments whose name
    /// resolves to the global function (`zend_resolve_function_name`).
    fn is_func_get_args(&self, e: &Expr) -> bool {
        let ExprKind::Call { callee, args } = &e.kind else {
            return false;
        };
        let ast::Callee::Name(n) = &**callee else {
            return false;
        };
        let (resolved, fb) = self.resolve_function_name(n);
        fb.is_none() && args.args.is_empty() && resolved.eq_ignore_ascii_case(b"func_get_args")
    }

    /// Whether `lc` is currently registered as an internal function
    /// ([`super::CompileOptions::special_builtins`]).
    fn special_builtin_enabled(&self, lc: &[u8]) -> bool {
        super::special_builtin_bit(lc).is_some_and(|bit| self.opts.special_builtins & bit != 0)
    }

    /// `zend_compile_func_defined`: `defined()` of a literal name without
    /// `\` or `:`. The call is `true` at compile time when the constant is
    /// substituted at compile time (`zend_try_ct_eval_const`: `true`,
    /// `false` and `null` in any case, and persistent internal constants),
    /// and `ZEND_DEFINED`, an exact-name lookup at run time, otherwise. A
    /// number is converted with `zval_get_string` instead of the call's
    /// parameter check, so `defined(1)` is `false` even under
    /// `strict_types`. `None` keeps the call.
    fn compile_defined(&mut self, arg: &Expr, need: bool) -> Option<Operand> {
        if !self.special_builtin_enabled(b"defined") || !defined_literal_name(arg) {
            return None;
        }
        let name = match &arg.kind {
            ExprKind::String(s) => Value::Str(ZStr::from(s.as_slice())),
            ExprKind::Int(i) => Value::Str(ZStr::from(i.to_string())),
            // Formatted at run time, with the `precision` setting in effect.
            ExprKind::Float(f) => Value::Float(*f),
            _ => return None,
        };
        if let Value::Str(s) = &name {
            let n = s.as_bytes();
            if n.eq_ignore_ascii_case(b"true")
                || n.eq_ignore_ascii_case(b"false")
                || n.eq_ignore_ascii_case(b"null")
                || self.ct_eval_const(n).is_some()
            {
                return Some(self.lit(Value::Bool(true)));
            }
        }
        let name = self.lit_idx(name);
        let dst = need.then(|| self.tmp());
        self.emit(Op::Defined { name, dst });
        Some(dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp))
    }

    /// `zend_compile_func_in_array`: `in_array()` whose haystack is a
    /// literal array of strings (none numeric, so that loose comparison
    /// with a string needle is byte equality) or, in strict mode, of
    /// strings and integers, and whose `strict` argument is a literal or a
    /// compile-time constant, is `ZEND_IN_ARRAY` over the set of those
    /// values. `None` keeps the call.
    fn compile_in_array(&mut self, args: &[&Expr], need: bool) -> CResult<Option<Operand>> {
        let Some((set, strict)) = self.in_array_set(args) else {
            return Ok(None);
        };
        // The haystack is evaluated, not compiled; the needle is compiled
        // last, so the opcode takes its line (`CG(zend_lineno)`).
        let a = self.compile_expr(args[0])?;
        let set = self.lit_idx(Value::Array(set));
        let dst = need.then(|| self.tmp());
        self.emit(Op::InArray {
            a,
            set,
            strict,
            dst,
        });
        Ok(Some(
            dst.map_or_else(|| self.lit(Value::Null), Operand::Tmp),
        ))
    }

    /// The key set and strictness of an `in_array()` call that
    /// [`Compiler::compile_in_array`] compiles to `ZEND_IN_ARRAY`; `None`
    /// for a call. Pure: nothing is emitted.
    fn in_array_set(&self, args: &[&Expr]) -> Option<(ZArray, bool)> {
        if !(2..=3).contains(&args.len()) || !self.special_builtin_enabled(b"in_array") {
            return None;
        }
        let strict = match args.get(2) {
            None => false,
            Some(e) => {
                // A literal (`zend_is_true`) or a constant known at compile
                // time (`zend_try_ct_eval_const`); anything else is a call.
                if !matches!(
                    e.kind,
                    ExprKind::Int(_)
                        | ExprKind::Float(_)
                        | ExprKind::String(_)
                        | ExprKind::Const(_)
                ) {
                    return None;
                }
                match self.try_fold(e)? {
                    Value::Float(f) if f.is_nan() => return None,
                    v => v.to_bool(&mut CollectingContext::new()).ok()?,
                }
            }
        };
        if !matches!(args[1].kind, ExprKind::Array { .. }) {
            return None;
        }
        let Value::Array(haystack) = self.try_fold(args[1])? else {
            return None;
        };
        let mut set = php_runtime::HashTable::with_capacity(haystack.len());
        for v in haystack.values() {
            let key = match v {
                Value::Str(s) if strict || !php_runtime::numeric::is_numeric(s.as_bytes()) => {
                    php_runtime::ArrayKey::Str(s.clone())
                }
                Value::Int(i) if strict => php_runtime::ArrayKey::Int(*i),
                _ => return None,
            };
            // A value listed twice is one key (`zend_hash_add` fails quietly).
            let _ = set.add(key, Value::Bool(true));
        }
        Some((ZArray::from_table(set), strict))
    }

    /// `zend_compile_func_chr` / `zend_compile_func_ord`: `chr()` of an
    /// integer literal in `0..=255` and `ord()` of a one-byte string
    /// literal are compile-time constants. php-src folds them in read
    /// context only (`BP_VAR_R`) and calls the function in write context,
    /// which is not observable: neither call can fail or run user code,
    /// and the write paths treat the literal like the call's temporary
    /// (`$x = &chr(65)` still notices and assigns the value).
    fn fold_chr_ord(&mut self, lc: &[u8], arg: &Expr) -> Option<Operand> {
        if !self.special_builtin_enabled(lc) {
            return None;
        }
        let v = match (lc, &arg.kind) {
            (b"chr", ExprKind::Int(i)) => Value::Str(ZStr::from_byte(u8::try_from(*i).ok()?)),
            (b"ord", ExprKind::String(s)) if s.len() == 1 => Value::Int(i64::from(s[0])),
            _ => return None,
        };
        Some(self.lit(v))
    }

    /// Lowers the constant formats accepted by `zend_compile_func_sprintf()`.
    fn compile_sprintf_rope(&mut self, args: &[&Expr], line: u32) -> CResult<Option<Operand>> {
        let Some(format) = args.first().and_then(|e| self.try_fold(e)) else {
            return Ok(None);
        };
        let Value::Str(format) = format else {
            return Ok(None);
        };
        let format = format.as_bytes();
        if format.len() >= 256 {
            return Ok(None);
        }
        let mut directives = Vec::new();
        let mut i = 0;
        while i < format.len() {
            if format[i] == b'%' {
                let Some(&kind) = format.get(i + 1) else {
                    return Ok(None);
                };
                if !matches!(kind, b's' | b'd' | b'%') {
                    return Ok(None);
                }
                directives.push((i, kind));
                i += 1;
            }
            i += 1;
        }
        if directives.iter().filter(|(_, kind)| *kind != b'%').count() != args.len() - 1 {
            return Ok(None);
        }
        // Evaluate every argument before performing any conversion, just as
        // a function call does. Conversions then run in placeholder order.
        let mut values = Vec::new();
        for arg in &args[1..] {
            values.push(self.compile_expr(arg)?);
        }
        // The conversions report the line the arguments left behind.
        self.f().line = args.last().map_or(line, |e| e.span.line);
        let mut parts = Vec::new();
        let mut offset = 0;
        let mut values = values.into_iter();
        for (pos, kind) in directives {
            let end = pos + usize::from(kind == b'%');
            if end != offset {
                parts.push(self.lit(Value::Str(ZStr::from(&format[offset..end]))));
            }
            offset = pos + 2;
            if kind == b'%' {
                continue;
            }
            let mut value = values.next().expect("validated placeholder count");
            if kind == b'd' {
                let dst = self.tmp();
                self.emit(Op::Cast {
                    kind: CastKind::Int,
                    a: value,
                    dst,
                });
                value = Operand::Tmp(dst);
            }
            if let Operand::Lit(idx) = value {
                let constant = &self.fref().literals[idx as usize];
                // Arrays warn at run time; floats follow the runtime
                // `precision` INI setting.
                if !matches!(constant, Value::Array(_) | Value::Float(_)) {
                    if let Ok(string) = constant.to_zstr(&mut CollectingContext::new()) {
                        parts.push(self.lit(Value::Str(string)));
                        continue;
                    }
                }
            }
            let dst = self.tmp();
            self.emit(Op::Cast {
                kind: CastKind::String,
                a: value,
                dst,
            });
            parts.push(Operand::Tmp(dst));
        }
        if offset != format.len() {
            parts.push(self.lit(Value::Str(ZStr::from(&format[offset..]))));
        }
        let result = match parts.len() {
            0 => self.lit(Value::Str(ZStr::empty())),
            1 => parts[0],
            _ => {
                let dst = self.tmp();
                self.emit(Op::Rope {
                    parts: parts.into_boxed_slice(),
                    dst,
                });
                Operand::Tmp(dst)
            }
        };
        Ok(Some(result))
    }

    /// `zend_compile_class_name` (`Zend/zend_compile.c`) on `(expr)::class`:
    /// a number literal is an illegal class name, and an expression folded
    /// to a constant other than a string cannot be used with `::class`.
    #[cold]
    fn check_class_name_operand(&self, e: &Expr) -> CResult<()> {
        if matches!(e.kind, ExprKind::Int(_) | ExprKind::Float(_)) {
            return self.err("Illegal class name", e.span.line);
        }
        match self.try_fold(e) {
            Some(Value::Str(_)) | None => Ok(()),
            Some(v) => self.err(
                format!("Cannot use \"::class\" on {}", v.value_name()),
                e.span.line,
            ),
        }
    }

    /// The rest of `zend_try_ct_eval_const` (`Zend/zend_compile.c`) after
    /// the special constants: a persistent internal constant found under
    /// the resolved name is substituted (`can_ct_eval_const`). An
    /// unqualified name in a namespace resolves to the namespaced name, so
    /// it is not substituted (its global fallback is decided at runtime).
    /// Parameter defaults are compiled without substitution.
    fn ct_eval_const(&self, resolved: &[u8]) -> Option<Value> {
        if self.no_const_subst {
            return None;
        }
        self.opts.ct_constants.as_ref()?.get(resolved).cloned()
    }

    /// Constant folding of simple expressions, after
    /// `zend_eval_const_expr()`'s `zend_check_stack_limit()`: past the
    /// limit nothing folds and the compiler reports the overflow.
    pub(crate) fn try_fold(&self, e: &Expr) -> Option<Value> {
        if !self.stack.enter(stack::FOLD_FRAME_BYTES) {
            return None;
        }
        let r = self.try_fold_inner(e);
        self.stack.leave(stack::FOLD_FRAME_BYTES);
        r
    }

    fn try_fold_inner(&self, e: &Expr) -> Option<Value> {
        match &e.kind {
            ExprKind::Int(i) => Some(Value::Int(*i)),
            ExprKind::Float(f) => Some(Value::Float(*f)),
            ExprKind::String(s) => Some(Value::Str(ZStr::from(s.as_slice()))),
            ExprKind::Const(n) => {
                let (name, fb) = self.resolve_const_name(n);
                if n.kind != ast::NameKind::Qualified {
                    let last = fb.as_deref().unwrap_or(&name);
                    if last.eq_ignore_ascii_case(b"true") {
                        return Some(Value::Bool(true));
                    }
                    if last.eq_ignore_ascii_case(b"false") {
                        return Some(Value::Bool(false));
                    }
                    if last.eq_ignore_ascii_case(b"null") {
                        return Some(Value::Null);
                    }
                }
                self.ct_eval_const(&name)
            }
            ExprKind::Unary { op, expr } => {
                let v = self.try_fold(expr)?;
                let mut ctx = CollectingContext::new();
                let r = match op {
                    UnaryOp::Minus => php_runtime::ops::mul(&v, &Value::Int(-1), &mut ctx).ok()?,
                    UnaryOp::Plus => php_runtime::ops::mul(&v, &Value::Int(1), &mut ctx).ok()?,
                    UnaryOp::Not => php_runtime::ops::bool_not(&v, &mut ctx).ok()?,
                    UnaryOp::BitNot => php_runtime::ops::bit_not(&v, &mut ctx).ok()?,
                };
                ctx.diagnostics.is_empty().then_some(r)
            }
            ExprKind::Binary { op, left, right } => {
                let bop = map_binop(*op)?;
                let l = self.try_fold(left)?;
                let r = self.try_fold(right)?;
                if matches!(l, Value::Array(_)) || matches!(r, Value::Array(_)) {
                    return None;
                }
                let mut ctx = CollectingContext::new();
                let v = php_runtime::ops::binary_op(bop, &l, &r, &mut ctx).ok()?;
                ctx.diagnostics.is_empty().then_some(v)
            }
            ExprKind::Array { items, syntax } if *syntax != ast::ArraySyntax::List => {
                let mut arr = ZArray::new();
                for it in items {
                    let it = it.as_ref()?;
                    if it.by_ref || it.unpack {
                        return None;
                    }
                    let v = self.try_fold(&it.value)?;
                    let t = arr.make_mut();
                    match &it.key {
                        None => {
                            t.push(v).ok()?;
                        }
                        Some(k) => match self.try_fold(k)? {
                            Value::Int(i) => {
                                t.insert(i, v);
                            }
                            Value::Str(s) => {
                                t.insert(php_runtime::ArrayKey::from_zstr(&s), v);
                            }
                            _ => return None,
                        },
                    }
                }
                Some(Value::Array(arr))
            }
            ExprKind::Interpolated(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        InterpPart::Literal(s, _) => out.extend_from_slice(s),
                        InterpPart::Expr { .. } => return None,
                    }
                }
                Some(Value::Str(ZStr::from(out)))
            }
            ExprKind::ClassName(class) => self.fold_class_name(class),
            _ => None,
        }
    }

    /// `zend_try_compile_const_expr_resolve_class_name`: `X::class` of a
    /// plain class name, or `self::class` where the scope is known, is a
    /// string literal (so `[A::class, 'm']` is an immutable array literal,
    /// which the cycle collector never counts). The same cases
    /// `compile_expr_inner` compiles to a literal.
    fn fold_class_name(&self, class: &ast::ClassRef) -> Option<Value> {
        let ast::ClassRef::Name(n) = class else {
            return None;
        };
        let lc = n.name.to_ascii_lowercase();
        if n.kind != ast::NameKind::Unqualified
            || !matches!(lc.as_slice(), b"self" | b"static" | b"parent")
        {
            return Some(Value::Str(ZStr::from(self.resolve_class_name(n))));
        }
        if lc.as_slice() != b"self" || self.fns.is_empty() || self.in_const_expr() {
            // `compile_expr_inner` validates `self` in constant expressions.
            return None;
        }
        let c = self.current_class()?;
        (!c.is_trait && !c.is_anon && self.closure_depth == 0).then(|| Value::Str(c.name.clone()))
    }
}

fn map_incdec(op: ast::IncDecOp) -> IncDecKind {
    match op {
        ast::IncDecOp::PreInc => IncDecKind::PreInc,
        ast::IncDecOp::PreDec => IncDecKind::PreDec,
        ast::IncDecOp::PostInc => IncDecKind::PostInc,
        ast::IncDecOp::PostDec => IncDecKind::PostDec,
    }
}

/// Whether `e`, the argument of `defined()`, is a literal that
/// `zend_compile_func_defined` accepts (`ZEND_AST_ZVAL`): a string without
/// `\` or `:`, or a number. `true`, `false` and `null` are constants, not
/// literals, so `defined(null)` keeps the call and its `TypeError`.
fn defined_literal_name(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::String(s) => !s.contains(&b'\\') && !s.contains(&b':'),
        _ => false,
    }
}

/// A string literal used as a callee, as the fully qualified function name
/// it denotes (`zend_resolve_function_name` with `ZEND_NAME_FQ`); `None`
/// for other callees and `"A::m"` strings (static method calls).
fn literal_function_name(e: &Expr) -> Option<ast::Name> {
    let ExprKind::String(s) = &e.kind else {
        return None;
    };
    let s: &[u8] = s;
    let s = s.strip_prefix(b"\\").unwrap_or(s);
    if s.windows(2).any(|w| w == b"::") {
        return None;
    }
    Some(ast::Name {
        kind: ast::NameKind::FullyQualified,
        name: s.to_vec(),
        span: e.span,
    })
}

/// The temporaries holding a place's operands.
fn place_tmps(p: &Place) -> Vec<u32> {
    let mut out = Vec::new();
    let mut k = |o: &Operand| {
        if let Operand::Tmp(t) = *o {
            out.push(t);
        }
    };
    match &p.base {
        PlaceBase::VarVar(o) | PlaceBase::Value(o) | PlaceBase::Global(o) => k(o),
        PlaceBase::StaticProp { class, name } => {
            k(name);
            if let ClassSpec::Dyn(o) = class {
                k(o);
            }
        }
        PlaceBase::Cv(_) | PlaceBase::This | PlaceBase::GlobalsArray => {}
    }
    for s in &p.segs {
        match s {
            Seg::Dim(o) | Seg::Prop(o) => k(o),
            Seg::Append => {}
        }
    }
    out
}

/// Replaces consumable temporaries of a place with keep-reads.
pub(crate) fn keep_place(p: &mut Place) {
    let k = |o: &mut Operand| {
        if let Operand::Tmp(t) = *o {
            *o = Operand::TmpKeep(t);
        }
    };
    match &mut p.base {
        PlaceBase::VarVar(o) | PlaceBase::Value(o) | PlaceBase::Global(o) => k(o),
        PlaceBase::StaticProp { class, name } => {
            k(name);
            if let ClassSpec::Dyn(o) = class {
                k(o);
            }
        }
        PlaceBase::Cv(_) | PlaceBase::This | PlaceBase::GlobalsArray => {}
    }
    for s in &mut p.segs {
        match s {
            Seg::Dim(o) | Seg::Prop(o) => k(o),
            Seg::Append => {}
        }
    }
}

/// `zend_list_assign_from_array_applies` (`Zend/zend_compile.c)`: a flat,
/// unkeyed list without references and an unkeyed array literal without
/// references or spread, one value per target. Returns the values.
/// The line of a list element's value down to its first variable: the line
/// php-src gives a list node (`zend_ast_create_list` takes its first
/// child's line).
fn first_leaf_line(it: &ArrayItem) -> u32 {
    match &it.value.kind {
        ExprKind::Array { items, .. } => items
            .iter()
            .flatten()
            .next()
            .map_or(it.span.line, first_leaf_line),
        _ => it.value.span.line,
    }
}

fn list_from_array_values<'a>(
    targets: &[Option<ArrayItem>],
    value: &'a Expr,
) -> Option<&'a [Option<ArrayItem>]> {
    let ExprKind::Array { items, syntax } = &value.kind else {
        return None;
    };
    if *syntax == ast::ArraySyntax::List || targets.is_empty() || items.len() != targets.len() {
        return None;
    }
    for (t, v) in targets.iter().zip(items) {
        let (t, v) = (t.as_ref()?, v.as_ref()?);
        if t.key.is_some()
            || t.by_ref
            || t.unpack
            || matches!(t.value.kind, ExprKind::Array { .. })
            || v.key.is_some()
            || v.by_ref
            || v.unpack
        {
            return None;
        }
    }
    Some(items.as_slice())
}

/// `is_this_fetch` (`Zend/zend_compile.c`): the plain variable `$this`.
pub(crate) fn is_this_var(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Var(VarName::Name(id)) if id.name == b"this")
}

/// `zend_is_call` (`Zend/zend_compile.c`).
pub(crate) fn is_call_expr(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StaticCall { .. }
            | ExprKind::Pipe { .. }
    )
}

/// `zend_ast_is_short_circuited` (`Zend/zend_compile.c)`: whether `e` ends a
/// chain containing `?->`.
pub(crate) fn contains_nullsafe(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Prop {
            object, nullsafe, ..
        }
        | ExprKind::MethodCall {
            object, nullsafe, ..
        } => *nullsafe || contains_nullsafe(object),
        ExprKind::Dim { base, .. } => contains_nullsafe(base),
        ExprKind::StaticProp { class, .. } | ExprKind::StaticCall { class, .. } => {
            matches!(&**class, ast::ClassRef::Expr(c) if contains_nullsafe(c))
        }
        _ => false,
    }
}

pub(crate) fn has_nested_ref(i: &ArrayItem) -> bool {
    if let ExprKind::Array { items, .. } = &i.value.kind {
        items
            .iter()
            .flatten()
            .any(|x| x.by_ref || has_nested_ref(x))
    } else {
        false
    }
}

/// `dirname()` for `__DIR__`.
pub(crate) fn dirname(path: &[u8]) -> Vec<u8> {
    match path.iter().rposition(|&c| c == b'/') {
        Some(0) => b"/".to_vec(),
        Some(p) => path[..p].to_vec(),
        None => b".".to_vec(),
    }
}

/// `zend_is_true` of a compile-time literal (`None` for values whose
/// truthiness is not static).
pub(crate) fn const_truthiness(v: &Value) -> Option<bool> {
    match v {
        Value::Null => Some(false),
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        // Coercing NAN to bool warns at run time ("unexpected NAN value was
        // coerced to bool"), so its truthiness is never folded.
        Value::Float(f) if f.is_nan() => None,
        Value::Float(f) => Some(*f != 0.0),
        Value::Str(s) => Some(!(s.is_empty() || s.as_bytes() == b"0")),
        Value::Array(a) => Some(!a.is_empty()),
        _ => None,
    }
}

/// The line of a method name (`zend_ast_get_lineno(method_ast)`).
fn member_name_line(name: &MemberName) -> u32 {
    match name {
        MemberName::Ident(i) => i.span.line,
        MemberName::Expr(e) => e.span.line,
    }
}

/// Whether argument `e` of a call to a known internal function is compiled
/// as an ordinary read when its parameter is by value (see
/// `compile_args`): an element or a property (`$a['k']`, `$this->p`,
/// `$this->p[$k]`, `f()['k']`), not a short-circuited `?->` chain (whose
/// jumps `arg_src_for` ends at the argument) nor a `[]` fetch (the compile
/// error). A plain variable stays the plain-variable send.
fn read_arg(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Dim { .. } | ExprKind::Prop { .. })
        && !contains_nullsafe(e)
        && !reads_append(e)
}

/// Whether compiling `e` for reading (`BP_VAR_R`) reaches a `[]` fetch:
/// the expression or a dimension/property base it is fetched from.
fn reads_append(e: &Expr) -> bool {
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::Dim { index: None, .. } => return true,
            ExprKind::Dim { base, .. } => cur = &**base,
            ExprKind::Prop { object, .. } => cur = &**object,
            _ => return false,
        }
    }
}

/// How many conditions (in order) `can_match_use_jumptable` runs
/// `zend_eval_const_expr` on and turns into literals: up to and including
/// the first that folds to something other than an int or a string, or up
/// to (not including) the first that does not fold.
fn match_prefolded_conds(c: &Compiler<'_>, arms: &[ast::MatchArm]) -> usize {
    let mut n = 0;
    for cond in arms.iter().filter_map(|a| a.conds.as_ref()).flatten() {
        match c.try_fold(cond) {
            None => break,
            Some(Value::Int(_) | Value::Str(_)) => n += 1,
            Some(_) => return n + 1,
        }
    }
    n
}

/// `can_match_use_jumptable` and the `num_conds >= 2` test of
/// `zend_compile_match`: the (arm index, key) of every condition when all
/// of them fold to integer or string literals and there are at least two;
/// `None` when the match tests its conditions one by one.
fn match_jumptable_keys(
    c: &Compiler<'_>,
    arms: &[ast::MatchArm],
) -> Option<Vec<(usize, MatchKey)>> {
    let mut keys = Vec::new();
    for (i, arm) in arms.iter().enumerate() {
        for cond in arm.conds.iter().flatten() {
            let key = match c.try_fold(cond)? {
                Value::Int(n) => MatchKey::Int(n),
                Value::Str(s) => MatchKey::Str(s.as_bytes().into()),
                _ => return None,
            };
            keys.push((i, key));
        }
    }
    (keys.len() >= 2).then_some(keys)
}

#[cfg(test)]
mod match_tests {
    //! `zend_compile_match`: jump-table selection and layout, and what the
    //! optimizer makes of it.
    use crate::compiler::{compile, optimizer, CompileOptions};
    use crate::opcode::{BinKind, MatchKey, Op, OpArray, Operand};
    use std::rc::Rc;

    /// The first declared function's code, optimized at `level`.
    fn function_code(src: &str, level: u32) -> Rc<OpArray> {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            optimization_level: level,
            ..CompileOptions::default()
        };
        let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
        let code = unit.decls.functions.borrow()[0].code.clone();
        code
    }

    fn match_op(code: &OpArray) -> Option<(Operand, Vec<(MatchKey, u32)>, u32)> {
        code.ops.iter().find_map(|op| match op {
            Op::Match { subject, table } => Some((*subject, table.entries.clone(), table.default)),
            _ => None,
        })
    }

    /// Whether `op` is a `===` test of the subject CV 0 (plain or fused
    /// with its jump by the handler specialisation).
    fn identical_on_cv0(op: &Op) -> bool {
        matches!(
            op,
            Op::Bin {
                op: BinKind::Identical,
                a: Operand::Cv(0),
                ..
            } | Op::CmpJmp {
                op: BinKind::Identical,
                a: Operand::Cv(0),
                ..
            }
        )
    }

    #[test]
    fn string_literals_use_a_jump_table_with_match_error_first() {
        let code = function_code(
            "<?php function t($c) { return match ($c) { 'a' => 1, 'b', 'c' => 2 }; }",
            0,
        );
        let (subject, entries, default) = match_op(&code).expect("MATCH");
        assert_eq!(subject, Operand::Cv(0));
        // MATCH_ERROR right after MATCH, then the arms.
        assert!(matches!(
            code.ops[default as usize],
            Op::MatchError {
                subject: Operand::Cv(0)
            }
        ));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, MatchKey::Str(b"a".as_slice().into()));
        assert_eq!(entries[1].1, entries[2].1);
        assert_eq!(entries[0].1, default + 1);
        // No copy of the subject and no `===` chain.
        assert!(!code.ops.iter().any(identical_on_cv0));
        assert!(!code.ops.iter().any(|op| matches!(op, Op::Free(_))));
    }

    #[test]
    fn ints_and_numeric_strings_are_distinct_keys_and_duplicates_keep_the_first() {
        let code = function_code(
            "<?php function t($v) { return match ($v) { 1 => 'i', '1' => 's', 1 => 'x', default => 'd' }; }",
            0,
        );
        let (_, entries, default) = match_op(&code).expect("MATCH");
        assert_eq!(
            entries.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            vec![MatchKey::Int(1), MatchKey::Str(b"1".as_slice().into())]
        );
        // The default arm is the last arm, no MATCH_ERROR.
        assert!(default > entries[1].1);
        assert!(!code
            .ops
            .iter()
            .any(|op| matches!(op, Op::MatchError { .. })));
    }

    #[test]
    fn a_single_condition_or_a_non_literal_one_tests_with_identical() {
        for src in [
            "<?php function t($v) { return match ($v) { 1 => 'a', default => 'b' }; }",
            "<?php function t($v) { return match ($v) { 1 => 'a', 2.5 => 'b' }; }",
            "<?php function t($v) { return match ($v) { 1 => 'a', f() => 'b' }; }",
        ] {
            let code = function_code(src, 0);
            assert!(match_op(&code).is_none(), "{src}");
            assert!(
                code.ops.iter().any(identical_on_cv0),
                "{src}: {:?}",
                code.ops
            );
        }
    }

    #[test]
    fn optimizer_turns_arms_into_returns_and_folds_a_known_subject() {
        let code = function_code(
            "<?php function t($c) { return match ($c) { 1 => 'a', 2, 3 => 'b', default => 'c' }; }",
            optimizer::DEFAULT_OPTIMIZATION_LEVEL,
        );
        let live: Vec<&Op> = code
            .ops
            .iter()
            .filter(|op| !matches!(op, Op::Nop))
            .collect();
        assert_eq!(live.len(), 5, "{live:?}");
        assert!(matches!(live[1], Op::Match { .. }));
        for op in &live[2..] {
            assert!(
                matches!(
                    op,
                    Op::Return {
                        val: Operand::Lit(_)
                    }
                ),
                "{live:?}"
            );
        }
        let (_, entries, default) = match_op(&code).expect("MATCH");
        assert_eq!(entries[1].1, entries[2].1);
        assert_eq!(default, 4);

        let code = function_code(
            "<?php function t() { $x = '2'; return match ($x) { 1, 2 => 'a', default => 'b' }; }",
            optimizer::DEFAULT_OPTIMIZATION_LEVEL,
        );
        assert!(match_op(&code).is_none(), "{:?}", code.ops);
    }
}

#[cfg(test)]
mod special_call_tests {
    //! `zend_try_compile_special_func`: calls compiled to opcodes.
    use crate::compiler::{compile, special_builtin_bit, CompileOptions};
    use crate::opcode::Op;
    use std::rc::Rc;

    /// The ops of the first declared function, with `array_slice` an
    /// internal function or not.
    fn function_ops(src: &str, builtin: bool) -> Vec<Op> {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            special_builtins: if builtin {
                special_builtin_bit(b"array_slice").unwrap_or(0)
            } else {
                0
            },
            ..CompileOptions::default()
        };
        let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
        let found = unit.decls.functions.borrow()[0].code.ops.clone();
        found
    }

    fn skips(ops: &[Op]) -> Vec<Option<u32>> {
        ops.iter()
            .filter_map(|op| match op {
                Op::FuncGetArgs { skip, .. } => Some(*skip),
                _ => None,
            })
            .collect()
    }

    /// `zend_compile_func_array_slice`: `array_slice(func_get_args(), n)`
    /// with a non-negative literal `n` is `FUNC_GET_ARGS n`; other forms
    /// stay calls.
    #[test]
    fn array_slice_of_func_get_args_is_one_opcode() {
        let src = "<?php function f() { return array_slice(func_get_args(), 2); }";
        assert_eq!(skips(&function_ops(src, true)), vec![Some(2)]);
        let fq = "<?php function f() { return \\array_slice(\\func_get_args(), 0); }";
        assert_eq!(skips(&function_ops(fq, true)), vec![Some(0)]);
        // Not registered as an internal function: a call of `array_slice`.
        assert_eq!(skips(&function_ops(src, false)), vec![None]);
        for other in [
            "<?php function f() { $n = 1; return array_slice(func_get_args(), $n); }",
            "<?php function f() { return array_slice(func_get_args(), 1, 1); }",
            "<?php namespace N; function f() { return \\array_slice(func_get_args(), 1); }",
        ] {
            assert!(
                skips(&function_ops(other, true))
                    .iter()
                    .all(Option::is_none),
                "{other}"
            );
        }
    }

    /// The call ops of the first declared function, as names, with
    /// `call_user_func` and `call_user_func_array` internal functions.
    fn cuf_ops(src: &str) -> Vec<&'static str> {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            special_builtins: special_builtin_bit(b"call_user_func").unwrap_or(0)
                | special_builtin_bit(b"call_user_func_array").unwrap_or(0),
            ..CompileOptions::default()
        };
        let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
        let ops = unit.decls.functions.borrow()[0].code.ops.clone();
        ops.iter()
            .filter_map(|op| match op {
                Op::InitFCall { .. } => Some("InitFCall"),
                Op::InitUserCall { array: false, .. } => Some("InitUserCall"),
                Op::InitUserCall { array: true, .. } => Some("InitUserCallArray"),
                Op::SendUser { .. } => Some("SendUser"),
                Op::SendArray { len, .. } if *len == crate::opcode::Operand::Unused => {
                    Some("SendArray")
                }
                Op::SendArray { .. } => Some("SendArraySlice"),
                Op::Send { .. } => Some("Send"),
                Op::SendNamed { .. } => Some("SendNamed"),
                Op::SendUnpack { .. } => Some("SendUnpack"),
                Op::DoCall { .. } => Some("DoCall"),
                _ => None,
            })
            .collect()
    }

    /// `zend_compile_func_cuf` / `zend_compile_func_cufa`: the forms
    /// lowered to `ZEND_INIT_USER_CALL` and those that keep the builtin.
    #[test]
    fn call_user_func_lowering() {
        let cases: &[(&str, &[&str])] = &[
            (
                "function f($g, $a) { return call_user_func($g, $a, 2); }",
                &["InitUserCall", "SendUser", "SendUser", "DoCall"],
            ),
            (
                "function f($g) { call_user_func($g); }",
                &["InitUserCall", "DoCall"],
            ),
            (
                "function f($g, $a) { return \\call_user_func_array($g, $a); }",
                &["InitUserCallArray", "SendArray", "DoCall"],
            ),
            (
                "function f($g, $a, $n) { return call_user_func_array($g, array_slice($a, 1, $n)); }",
                &["InitUserCallArray", "SendArraySlice", "DoCall"],
            ),
            // Not the three-argument form: `array_slice()` is called.
            (
                "function f($g, $a) { return call_user_func_array($g, array_slice($a, 1)); }",
                &["InitUserCallArray", "InitFCall", "Send", "Send", "DoCall", "SendArray", "DoCall"],
            ),
            // A callback argument that is itself such a call nests.
            (
                "function f($g) { return call_user_func($g, call_user_func($g)); }",
                &["InitUserCall", "InitUserCall", "DoCall", "SendUser", "DoCall"],
            ),
            // Named arguments, unpacking and the argument counts the
            // builtins reject keep the builtin call.
            (
                "function f($g) { return call_user_func($g, x: 1); }",
                &["InitFCall", "Send", "SendNamed", "DoCall"],
            ),
            (
                "function f($a) { return call_user_func(...$a); }",
                &["InitFCall", "SendUnpack", "DoCall"],
            ),
            ("function f() { return call_user_func(); }", &["InitFCall", "DoCall"]),
            (
                "function f($g) { return call_user_func_array($g); }",
                &["InitFCall", "Send", "DoCall"],
            ),
            // An unqualified name in a namespace is resolved at run time.
            (
                "namespace N; function f($g) { return call_user_func($g); }",
                &["InitFCall", "Send", "DoCall"],
            ),
            (
                "namespace N; function f($g) { return \\call_user_func($g); }",
                &["InitUserCall", "DoCall"],
            ),
        ];
        for (src, expected) in cases {
            let src = format!("<?php {src}");
            assert_eq!(cuf_ops(&src), *expected, "{src}");
        }
    }
}

#[cfg(test)]
mod pipe_tests {
    //! `zend_compile_pipe()` with `zend_partial_apply()`: which right-hand
    //! sides of `|>` become the plain call with the piped operand in the
    //! placeholder's place, and which are called as a Closure.
    use crate::compiler::{compile, CompileOptions};
    use crate::opcode::{Op, Operand};
    use std::rc::Rc;

    /// The ops of the first declared function, unoptimized, as names.
    fn pipe_ops(src: &str) -> Vec<&'static str> {
        let src = format!("<?php {src}");
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            optimization_level: 0,
            ..CompileOptions::default()
        };
        let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
        let ops = unit.decls.functions.borrow()[0].code.ops.clone();
        ops.iter()
            .filter_map(|op| match op {
                Op::Move {
                    src: Operand::Cv(_),
                    ..
                } => Some("MoveCv"),
                Op::InitFCall { .. } => Some("InitFCall"),
                Op::InitDynCall { .. } => Some("InitDynCall"),
                Op::InitMethodCall { .. } => Some("InitMethodCall"),
                Op::InitStaticCall { .. } => Some("InitStaticCall"),
                Op::Send { .. } => Some("Send"),
                Op::SendNamed { .. } => Some("SendNamed"),
                Op::SendPlaceholder { .. } => Some("SendPlaceholder"),
                Op::CallablePartial { .. } => Some("CallablePartial"),
                Op::CallableConvert { .. } => Some("CallableConvert"),
                Op::DoCall { .. } => Some("DoCall"),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn partial_applications_with_one_placeholder_become_direct_calls() {
        let direct: &[(&str, &[&str])] = &[
            // A variable is copied first (QM_ASSIGN): pipe_optimization_011.
            (
                "function f($x) { return $x |> g(?); }",
                &["MoveCv", "InitFCall", "Send", "DoCall"],
            ),
            // A literal or a temporary is sent as it is.
            (
                "function f() { return 2 |> g(1, ?); }",
                &["InitFCall", "Send", "Send", "DoCall"],
            ),
            (
                "function f() { return 2 |> g(?, 1); }",
                &["InitFCall", "Send", "Send", "DoCall"],
            ),
            (
                "function f() { return h() |> g(?); }",
                &["InitFCall", "DoCall", "InitFCall", "Send", "DoCall"],
            ),
            // `...` is the one placeholder, so `g(...)` and `g(1, ...)` too.
            (
                "function f() { return 2 |> g(...); }",
                &["InitFCall", "Send", "DoCall"],
            ),
            (
                "function f() { return 2 |> g(1, ...); }",
                &["InitFCall", "Send", "Send", "DoCall"],
            ),
            // A named placeholder becomes a named argument, and `?` before
            // named arguments stays positional.
            (
                "function f() { return 2 |> g(1, c: ?); }",
                &["InitFCall", "Send", "SendNamed", "DoCall"],
            ),
            (
                "function f() { return 1 |> g(?, b: 2); }",
                &["InitFCall", "Send", "SendNamed", "DoCall"],
            ),
            // Other arguments, call results among them, run after the
            // operand, in their own places.
            (
                "function f() { return h() |> g(a(), ?, b()); }",
                &[
                    "InitFCall",
                    "DoCall",
                    "InitFCall",
                    "InitFCall",
                    "DoCall",
                    "Send",
                    "Send",
                    "InitFCall",
                    "DoCall",
                    "Send",
                    "DoCall",
                ],
            ),
            // Methods, static methods and dynamic function names.
            (
                "function f($o) { return 1 |> $o->m(?); }",
                &["InitMethodCall", "Send", "DoCall"],
            ),
            (
                "function f($o) { return 1 |> $o->m(...); }",
                &["InitMethodCall", "Send", "DoCall"],
            ),
            (
                "function f() { return 1 |> C::m(2, ?); }",
                &["InitStaticCall", "Send", "Send", "DoCall"],
            ),
            (
                "function f($g) { return 1 |> $g(?); }",
                &["InitDynCall", "Send", "DoCall"],
            ),
            // `\assert` is excluded only when written as a plain name.
            (
                "function f() { return 1 |> '\\assert'(?); }",
                &["InitFCall", "Send", "DoCall"],
            ),
        ];
        for (src, expected) in direct {
            assert_eq!(pipe_ops(src), *expected, "{src}");
        }
    }

    #[test]
    fn other_right_hand_sides_are_called_as_closures() {
        let closure: &[(&str, &[&str])] = &[
            // Two placeholders: pipe_optimization_004 / 007.
            (
                "function f() { return 2 |> g(?, ?); }",
                &[
                    "InitFCall",
                    "SendPlaceholder",
                    "SendPlaceholder",
                    "CallablePartial",
                    "InitDynCall",
                    "Send",
                    "DoCall",
                ],
            ),
            (
                "function f() { return 2 |> g(a: ?, b: ?); }",
                &[
                    "InitFCall",
                    "SendPlaceholder",
                    "SendPlaceholder",
                    "CallablePartial",
                    "InitDynCall",
                    "Send",
                    "DoCall",
                ],
            ),
            // `...` after a named argument: pipe_optimization_008 / 013.
            (
                "function f() { return 2 |> g(a: 1, ...); }",
                &[
                    "InitFCall",
                    "SendNamed",
                    "CallablePartial",
                    "InitDynCall",
                    "Send",
                    "DoCall",
                ],
            ),
            // `assert`, whose compilation adds the message argument.
            (
                "function f() { return 1 |> assert(...); }",
                &[
                    "InitFCall",
                    "CallableConvert",
                    "InitDynCall",
                    "Send",
                    "DoCall",
                ],
            ),
            // Not a partial application: the value is called.
            (
                "function f($x, $g) { return $x |> $g; }",
                &["MoveCv", "InitDynCall", "Send", "DoCall"],
            ),
            (
                "function f() { return 1 |> g(2); }",
                &[
                    "InitFCall",
                    "Send",
                    "DoCall",
                    "InitDynCall",
                    "Send",
                    "DoCall",
                ],
            ),
        ];
        for (src, expected) in closure {
            assert_eq!(pipe_ops(src), *expected, "{src}");
        }
    }
}
