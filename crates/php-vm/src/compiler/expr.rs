//! Expression compilation.

use super::stmt::is_ref_expr;
use super::{CResult, Compiler, FnKind};
use crate::ast::{
    self, ArgKind, ArgList, ArrayItem, BinaryOp as AB, Callee, Expr, ExprKind, InterpPart,
    MagicConst, MemberName, NewClass, UnaryOp, VarName,
};
use crate::func::flags;
use crate::opcode::{
    ArgSrc, BinKind, CastKind, ClassSpec, FetchMode, IncDecKind, IncludeKind, Op, Operand, Place,
    PlaceBase, Seg, UnKind,
};
use php_runtime::{BinaryOp, CollectingContext, Value, ZArray, ZStr};

/// Superglobal names (always global).
pub(crate) fn is_superglobal(name: &[u8]) -> bool {
    matches!(
        name,
        b"_GET"
            | b"_POST"
            | b"_COOKIE"
            | b"_SERVER"
            | b"_ENV"
            | b"_REQUEST"
            | b"_FILES"
            | b"_SESSION"
    )
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

impl Compiler {
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
        let r = self.compile_expr_inner(e);
        let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
        let op = r?;
        if jumps.is_empty() {
            return Ok(op);
        }
        let dst = self.tmp();
        self.emit(Op::Move { src: op, dst });
        self.finish_short_circuit(jumps, dst);
        Ok(Operand::Tmp(dst))
    }

    pub(crate) fn finish_short_circuit(&mut self, jumps: Vec<usize>, dst: u32) {
        let here = self.here();
        for j in jumps {
            if let Op::JmpNull { dst: d, target, .. } = &mut self.f().ops[j] {
                *d = dst;
                *target = here;
            }
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

    /// Compiles an assignment-like expression without a result.
    pub(crate) fn compile_expr_nodst(&mut self, e: &Expr) -> CResult<Option<Operand>> {
        match &e.kind {
            ExprKind::AssignOp { op, target, value } => {
                self.compile_assign_op(*op, target, value, false, e.span.line)
            }
            ExprKind::IncDec { op, var } => {
                let p = self.compile_write_place(var)?;
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

    #[allow(clippy::too_many_lines)]
    fn compile_expr_inner(&mut self, e: &Expr) -> CResult<Operand> {
        let line = e.span.line;
        self.f().line = line;
        if let Some(v) = self.try_fold(e) {
            return Ok(self.lit(v));
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
            ExprKind::MagicConst(m) => self.compile_magic(*m, line)?,
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
                let spec = self.class_spec(class)?;
                let t = self.tmp();
                self.emit(Op::FetchClassName {
                    class: spec,
                    dst: t,
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
                    Operand::Cv(self.cv(&id.name))
                }
            }
            ExprKind::Var(VarName::Expr(_))
            | ExprKind::Dim { .. }
            | ExprKind::Prop { .. }
            | ExprKind::StaticProp { .. } => {
                if let ExprKind::Dim { index: None, .. } = &e.kind {
                    return self.err("Cannot use [] for reading", line);
                }
                let p = self.compile_place(e, false)?;
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
                self.compile_assign_ref(target, value, line)?
            }
            ExprKind::AssignOp { op, target, value } => self
                .compile_assign_op(*op, target, value, true, line)?
                .unwrap_or(Operand::Unused),
            ExprKind::Binary { op, left, right } => self.compile_binary(*op, left, right)?,
            ExprKind::Unary { op, expr } => {
                let a = self.compile_expr(expr)?;
                self.f().line = line;
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
                let p = self.compile_write_place(var)?;
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
                let a = self.compile_expr(expr)?;
                self.f().line = line;
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
            ExprKind::Isset(vars) => {
                let t = self.tmp();
                let mut jumps = Vec::new();
                for (i, v) in vars.iter().enumerate() {
                    let r = self.compile_isset_one(v)?;
                    if i + 1 < vars.len() {
                        jumps.push(self.emit(Op::JmpZEx {
                            a: r,
                            dst: t,
                            target: 0,
                        }));
                    } else {
                        self.emit(Op::Move { src: r, dst: t });
                    }
                }
                for j in jumps {
                    self.patch_here(j);
                }
                Operand::Tmp(t)
            }
            ExprKind::Empty(inner) => {
                let t = self.tmp();
                if is_ref_expr(inner)
                    && !matches!(inner.kind, ExprKind::Var(VarName::Name(ref id)) if id.name == b"this")
                {
                    let saved = std::mem::take(&mut self.f().sc_jumps);
                    let p = self.compile_place(inner, false);
                    let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
                    let p = p?;
                    let pid = self.place(p);
                    self.emit(Op::Empty { place: pid, dst: t });
                    if !jumps.is_empty() {
                        let skip = self.emit(Op::Jmp(0));
                        let tt = self.lit(Value::Bool(true));
                        let here = self.here();
                        for j in jumps {
                            if let Op::JmpNull { dst: d, target, .. } = &mut self.f().ops[j] {
                                *d = t;
                                *target = here;
                            }
                        }
                        self.emit(Op::Move { src: tt, dst: t });
                        self.patch_here(skip);
                    }
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
                self.f().line = line;
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
                if matches!(
                    expr.kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
                ) {
                    return self.err(
                        "instanceof expects an object instance, constant given",
                        line,
                    );
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
                    }
                }
                let v = match value {
                    Some(v) => Some(self.compile_expr(v)?),
                    None => None,
                };
                self.f().line = line;
                self.emit(Op::Yield {
                    key: k,
                    val: v,
                    dst: Some(t),
                });
                Operand::Tmp(t)
            }
            ExprKind::YieldFrom(inner) => {
                self.check_yield(line)?;
                let v = self.compile_expr(inner)?;
                self.f().line = line;
                let t = self.tmp();
                self.emit(Op::YieldFrom {
                    src: v,
                    dst: Some(t),
                });
                Operand::Tmp(t)
            }
            ExprKind::Throw(inner) => {
                let v = self.compile_expr(inner)?;
                self.f().line = line;
                self.emit(Op::Throw(v));
                self.lit(Value::Null)
            }
            ExprKind::Closure(c) => self.compile_closure(c)?,
            ExprKind::ArrowFn(a) => self.compile_arrow_fn(a)?,
            ExprKind::Match { subject, arms } => self.compile_match(subject, arms, line)?,
            ExprKind::Pipe { left, right } => {
                let l = self.compile_expr(left)?;
                // `x |> f(...)`: call the callable with x.
                match &right.kind {
                    ExprKind::Call { callee, args } if args.is_first_class_callable() => {
                        match &**callee {
                            Callee::Name(n) => self.emit_init_fcall(n),
                            Callee::Expr(ex) => {
                                let c = self.compile_expr(ex)?;
                                self.emit(Op::InitDynCall { callee: c });
                            }
                        }
                    }
                    _ => {
                        let c = self.compile_expr(right)?;
                        self.emit(Op::InitDynCall { callee: c });
                    }
                }
                self.emit(Op::Send {
                    src: ArgSrc::Val(l),
                });
                let t = self.tmp();
                self.f().line = line;
                self.emit(Op::DoCall { dst: Some(t) });
                Operand::Tmp(t)
            }
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
        let pid = self.place(p);
        let t = self.tmp();
        self.emit(Op::Fetch {
            place: pid,
            mode,
            dst: t,
        });
        Operand::Tmp(t)
    }

    fn compile_rope(&mut self, parts: &[InterpPart]) -> CResult<Operand> {
        let mut ops = Vec::new();
        for p in parts {
            match p {
                InterpPart::Literal(s, _) => {
                    ops.push(self.lit(Value::Str(ZStr::from(s.as_slice()))));
                }
                InterpPart::Expr { expr, .. } => {
                    let v = self.compile_expr(expr)?;
                    // Materialise CVs now: interpolation reads in order.
                    let v = if let Operand::Cv(_) = v {
                        let t = self.tmp();
                        self.emit(Op::Move { src: v, dst: t });
                        Operand::Tmp(t)
                    } else {
                        v
                    };
                    ops.push(v);
                }
            }
        }
        let t = self.tmp();
        self.emit(Op::Rope {
            parts: ops.into_boxed_slice(),
            dst: t,
        });
        Ok(Operand::Tmp(t))
    }

    fn compile_array(&mut self, items: &[Option<ArrayItem>], line: u32) -> CResult<Operand> {
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
                self.emit(Op::ArrayUnpack { arr: t, src: v });
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

    fn compile_magic(&mut self, m: MagicConst, line: u32) -> CResult<Operand> {
        let s: Vec<u8> = match m {
            MagicConst::Line => return Ok(self.lit(Value::Int(i64::from(line)))),
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
                        });
                        return Ok(Operand::Tmp(t));
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
        Ok(self.lit(Value::Str(ZStr::from(s))))
    }

    fn innermost_named_fn(&self) -> (Vec<u8>, bool) {
        let f = self.fref();
        match f.kind {
            FnKind::Main | FnKind::ConstExpr => (Vec::new(), false),
            FnKind::Closure => (f.func_name.as_bytes().to_vec(), false),
            FnKind::Function => (f.func_name.as_bytes().to_vec(), false),
            FnKind::Method => (f.method_name.as_bytes().to_vec(), true),
        }
    }

    pub(crate) fn member_name(&mut self, n: &MemberName) -> CResult<Operand> {
        Ok(match n {
            MemberName::Ident(id) => self.lit(Value::Str(ZStr::from(id.name.as_slice()))),
            MemberName::Expr(e) => self.compile_expr(e)?,
        })
    }

    /// Compiles a writable expression into a place (offsets evaluated now).
    pub(crate) fn compile_place(&mut self, e: &Expr, _for_unset: bool) -> CResult<Place> {
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
                let n = self.compile_expr(ne)?;
                Place {
                    base: PlaceBase::VarVar(n),
                    segs: Vec::new(),
                }
            }
            ExprKind::Dim { base, index } => {
                if let ExprKind::Var(VarName::Name(id)) = &base.kind {
                    if id.name == b"GLOBALS" {
                        if let Some(idx) = index {
                            let n = self.compile_expr(idx)?;
                            return Ok(Place {
                                base: PlaceBase::Global(n),
                                segs: Vec::new(),
                            });
                        }
                    }
                }
                let mut p = self.compile_place_base(base)?;
                let seg = match index {
                    Some(i) => Seg::Dim(self.compile_expr(i)?),
                    None => Seg::Append,
                };
                p.segs.push(seg);
                p
            }
            ExprKind::Prop {
                object,
                name,
                nullsafe,
            } => {
                let mut p = self.compile_place_base(object)?;
                if *nullsafe {
                    let v = if p.segs.is_empty() {
                        match p.base {
                            PlaceBase::Value(op) => op,
                            PlaceBase::Cv(cv) => {
                                let t = self.tmp();
                                self.emit(Op::Move {
                                    src: Operand::Cv(cv),
                                    dst: t,
                                });
                                Operand::Tmp(t)
                            }
                            base => self.fetch_place(
                                Place {
                                    base,
                                    segs: Vec::new(),
                                },
                                FetchMode::Read,
                            ),
                        }
                    } else {
                        self.fetch_place(p, FetchMode::Read)
                    };
                    let j = self.emit(Op::JmpNull {
                        a: Compiler::keep(v),
                        dst: 0,
                        target: 0,
                    });
                    self.f().sc_jumps.push(j);
                    p = Place {
                        base: PlaceBase::Value(v),
                        segs: Vec::new(),
                    };
                }
                let n = self.member_name(name)?;
                p.segs.push(Seg::Prop(n));
                p
            }
            ExprKind::StaticProp { class, name, .. } => {
                let spec = self.class_spec(class)?;
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
        self.compile_place(e, false)
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
        let saved = std::mem::take(&mut self.f().sc_jumps);
        let p = self.compile_place(v, false);
        let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
        let p = p?;
        let pid = self.place(p);
        self.emit(Op::Isset { place: pid, dst: t });
        if !jumps.is_empty() {
            let skip = self.emit(Op::Jmp(0));
            let here = self.here();
            for j in jumps {
                if let Op::JmpNull { dst: d, target, .. } = &mut self.f().ops[j] {
                    *d = t;
                    *target = here;
                }
            }
            let f = self.lit(Value::Bool(false));
            self.emit(Op::Move { src: f, dst: t });
            self.patch_here(skip);
        }
        Ok(Operand::Tmp(t))
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
                        } else {
                            let saved = std::mem::take(&mut self.f().sc_jumps);
                            let p = self.compile_place(left, false);
                            let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
                            let p = p?;
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
                        let saved = std::mem::take(&mut self.f().sc_jumps);
                        let p = self.compile_place(left, false);
                        let jumps = std::mem::replace(&mut self.f().sc_jumps, saved);
                        let p = p?;
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
                let (l, r) = if matches!(op, AB::Identical | AB::NotIdentical)
                    && matches!(l, Operand::Lit(_))
                    && !matches!(r, Operand::Lit(_))
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
                self.f().line = left.span.line;
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
                if has_ref && is_ref_expr(value) {
                    let src = self.compile_place(value, false)?;
                    self.compile_destructure_ref(items, &src, line)?;
                    if need {
                        return Ok(Some(self.fetch_place(src, FetchMode::Read)));
                    }
                    return Ok(None);
                }
                let v = self.compile_expr(value)?;
                let v = match v {
                    Operand::Cv(_) => {
                        let t = self.tmp();
                        self.emit(Op::Move { src: v, dst: t });
                        Operand::Tmp(t)
                    }
                    o => o,
                };
                self.f().line = line;
                self.compile_destructure_keep(items, v, line)?;
                if need {
                    Ok(Some(v))
                } else {
                    self.free(v);
                    Ok(None)
                }
            }
            _ => {
                let p = self.compile_write_place(target)?;
                let v = self.compile_expr(value)?;
                self.f().line = line;
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
    fn compile_destructure_keep(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
    ) -> CResult<()> {
        let keep = Compiler::keep(src);
        self.destructure_items(items, keep, line)
    }

    /// Destructures `src` and consumes it.
    pub(crate) fn compile_destructure(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
    ) -> CResult<()> {
        let keep = Compiler::keep(src);
        self.destructure_items(items, keep, line)?;
        self.free(src);
        Ok(())
    }

    fn destructure_items(
        &mut self,
        items: &[Option<ArrayItem>],
        src: Operand,
        line: u32,
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
        for it in items {
            let Some(it) = it else {
                idx += 1;
                continue;
            };
            if it.unpack {
                return self.err("Spread operator is not supported in assignments", line);
            }
            self.f().line = it.span.line;
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
                self.compile_destructure(sub, elem, it.span.line)?;
            } else {
                self.assign_to(&it.value, elem, None)?;
            }
        }
        Ok(())
    }

    fn compile_destructure_ref(
        &mut self,
        items: &[Option<ArrayItem>],
        src: &Place,
        line: u32,
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
                self.compile_destructure_ref(sub, &p, line)?;
            } else if it.by_ref {
                let pid = self.place(p);
                let t = self.tmp();
                self.emit(Op::MakeRef { place: pid, dst: t });
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

    fn compile_assign_ref(&mut self, target: &Expr, value: &Expr, line: u32) -> CResult<Operand> {
        if contains_nullsafe(value) {
            return self.err("Cannot take reference of a nullsafe chain", line);
        }
        if let ExprKind::Array { .. } = target.kind {
            return self.err("Cannot assign reference to array destructuring", line);
        }
        let src = if is_ref_expr(value) {
            let p = self.compile_place(value, false)?;
            let pid = self.place(p);
            let t = self.tmp();
            self.emit(Op::MakeRef { place: pid, dst: t });
            Operand::Tmp(t)
        } else {
            self.compile_expr(value)?
        };
        let p = self.compile_write_place(target)?;
        self.f().line = line;
        let pid = self.place(p);
        let t = self.tmp();
        self.emit(Op::AssignRef {
            place: pid,
            src,
            dst: Some(t),
        });
        Ok(Operand::Tmp(t))
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
            let p = self.compile_write_place(target)?;
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

    fn compile_match(
        &mut self,
        subject: &Expr,
        arms: &[ast::MatchArm],
        line: u32,
    ) -> CResult<Operand> {
        let s = self.compile_expr(subject)?;
        let s = match s {
            Operand::Cv(_) => {
                let t = self.tmp();
                self.emit(Op::Move { src: s, dst: t });
                Operand::Tmp(t)
            }
            o => o,
        };
        let sk = Compiler::keep(s);
        let result = self.tmp();
        let mut arm_jumps: Vec<(usize, usize)> = Vec::new();
        let mut default_arm = None;
        for (i, arm) in arms.iter().enumerate() {
            match &arm.conds {
                Some(conds) => {
                    for c in conds {
                        self.f().line = c.span.line;
                        let v = self.compile_expr(c)?;
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
                None => {
                    if default_arm.is_some() {
                        return self.err(
                            "Match expressions may only contain one default arm",
                            arm.span.line,
                        );
                    }
                    default_arm = Some(i);
                }
            }
        }
        let no_match = self.emit(Op::Jmp(0));
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for arm in arms {
            starts.push(self.here());
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
        if let Some(d) = default_arm {
            self.patch(no_match, starts[d]);
        } else {
            self.patch_here(no_match);
            self.f().line = line;
            self.emit(Op::MatchError { subject: sk });
        }
        for j in ends {
            self.patch_here(j);
        }
        self.free(s);
        Ok(Operand::Tmp(result))
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

    /// Compiles calls and `new`; returns the result operand.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn compile_call_like(&mut self, e: &Expr, need: bool) -> CResult<Option<Operand>> {
        let line = e.span.line;
        match &e.kind {
            ExprKind::Call { callee, args } => {
                if let Callee::Name(n) = &**callee {
                    if !args.is_partial() {
                        if let Some(r) = self.compile_special_call(n, args, line, need)? {
                            return Ok(Some(r));
                        }
                    }
                    self.emit_init_fcall(n);
                } else if let Callee::Expr(ex) = &**callee {
                    let c = self.compile_chain(ex)?;
                    self.f().line = line;
                    self.emit(Op::InitDynCall { callee: c });
                }
                self.finish_call(args, line, need)
            }
            ExprKind::MethodCall {
                object,
                name,
                args,
                nullsafe,
            } => {
                let o = self.compile_chain(object)?;
                let o = if let Operand::Cv(_) = o { o } else { o };
                if *nullsafe {
                    let j = self.emit(Op::JmpNull {
                        a: Compiler::keep(o),
                        dst: 0,
                        target: 0,
                    });
                    self.f().sc_jumps.push(j);
                }
                let n = self.member_name(name)?;
                self.f().line = line;
                self.emit(Op::InitMethodCall { obj: o, name: n });
                self.finish_call(args, line, need)
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
                self.finish_call(args, line, need)
            }
            ExprKind::StaticCall { class, name, args } => {
                let spec = self.class_spec(class)?;
                let n = self.member_name(name)?;
                self.f().line = line;
                self.emit(Op::InitStaticCall {
                    class: spec,
                    name: n,
                });
                self.finish_call(args, line, need)
            }
            ExprKind::New { class, args } => {
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
                self.f().line = line;
                let r = self.tmp();
                let at = self.emit(Op::New {
                    class: spec,
                    dst: r,
                    skip: 0,
                });
                if args.is_partial() {
                    return self.err("Cannot create Closure for new expression", line);
                }
                self.compile_args(args)?;
                self.f().line = line;
                self.emit(Op::DoCall { dst: None });
                self.patch_here(at);
                Ok(Some(Operand::Tmp(r)))
            }
            _ => Ok(Some(self.compile_expr(e)?)),
        }
    }

    fn finish_call(&mut self, args: &ArgList, line: u32, need: bool) -> CResult<Option<Operand>> {
        if args.is_first_class_callable() {
            let t = self.tmp();
            self.f().line = line;
            self.emit(Op::CallableConvert { dst: t });
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
        self.compile_args(args)?;
        self.f().line = line;
        let dst = need.then(|| self.tmp());
        self.emit(Op::DoCall { dst });
        Ok(dst.map(Operand::Tmp))
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

    pub(crate) fn compile_args(&mut self, args: &ArgList) -> CResult<()> {
        let mut seen_named = false;
        let mut seen_unpack = false;
        for a in &args.args {
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
                    let src = self.arg_src_for(ex)?;
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
                    let n = self.lit_str(&name.name);
                    let src = self.arg_src_for(value)?;
                    self.emit(Op::SendNamed { name: n, src });
                }
                ArgKind::Placeholder
                | ArgKind::VariadicPlaceholder
                | ArgKind::NamedPlaceholder(_) => {}
            }
        }
        Ok(())
    }

    /// Compile-time special functions. Returns `None` when `n` is not one.
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
            b"exit" | b"die" if only_positional && positional.len() <= 1 => {
                let v = match positional.first() {
                    Some(e) => self.compile_expr(e)?,
                    None => Operand::Unused,
                };
                self.f().line = line;
                self.emit(Op::Exit(v));
                Ok(Some(self.lit(Value::Null)))
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
                self.compile_args(args)?;
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

    /// Constant folding of simple expressions.
    pub(crate) fn try_fold(&self, e: &Expr) -> Option<Value> {
        match &e.kind {
            ExprKind::Int(i) => Some(Value::Int(*i)),
            ExprKind::Float(f) => Some(Value::Float(*f)),
            ExprKind::String(s) => Some(Value::Str(ZStr::from(s.as_slice()))),
            ExprKind::Const(n) => {
                let (name, fb) = self.resolve_const_name(n);
                if n.kind == ast::NameKind::Qualified {
                    return None;
                }
                let last = fb.unwrap_or(name).to_ascii_lowercase();
                match last.as_slice() {
                    b"true" => Some(Value::Bool(true)),
                    b"false" => Some(Value::Bool(false)),
                    b"null" => Some(Value::Null),
                    _ => None,
                }
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
            _ => None,
        }
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

fn contains_nullsafe(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Prop {
            object, nullsafe, ..
        } => *nullsafe || contains_nullsafe(object),
        ExprKind::MethodCall {
            object, nullsafe, ..
        } => *nullsafe || contains_nullsafe(object),
        ExprKind::Dim { base, .. } => contains_nullsafe(base),
        _ => false,
    }
}

fn has_nested_ref(i: &ArrayItem) -> bool {
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
fn const_truthiness(v: &Value) -> Option<bool> {
    match v {
        Value::Null => Some(false),
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Float(f) => Some(*f != 0.0),
        Value::Str(s) => Some(!(s.is_empty() || s.as_bytes() == b"0")),
        Value::Array(a) => Some(!a.is_empty()),
        _ => None,
    }
}
