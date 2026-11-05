//! AST → opcode compiler (`Zend/zend_compile.c`).

mod attrs;
mod constexpr;
mod decl;
mod expr;
mod stmt;

use crate::ast::{self, Name, NameKind};
use crate::class::{AttributeInfo, ClassDecl, ConstExpr};
use crate::func::UserFunction;
use crate::opcode::{ClassSpec, DeclTable, Label, Op, OpArray, Operand, Place, PlaceId, TryEntry};
use crate::types::TypeDecl;
use php_runtime::{Value, ZStr};
use std::collections::HashMap;
use std::rc::Rc;

/// A fatal error raised while compiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// The message, as PHP prints it.
    pub message: String,
    /// The line.
    pub line: u32,
    /// `E_COMPILE_ERROR` (false) or `E_ERROR`-class… kept for display.
    pub is_parse: bool,
    /// Compile-time warnings accumulated before the fatal error.
    pub warnings: Vec<(i64, String, u32)>,
}

impl CompileError {
    fn new(message: impl Into<String>, line: u32, warnings: Vec<(i64, String, u32)>) -> Self {
        CompileError {
            message: message.into(),
            line,
            is_parse: false,
            warnings,
        }
    }
}

/// Result type of compiler functions.
pub type CResult<T> = Result<T, CompileError>;

/// A compiled file (or `eval` string).
#[derive(Debug)]
pub struct CompiledUnit {
    /// The top-level code.
    pub main: Rc<UserFunction>,
    /// Functions to declare before execution (unconditional top-level).
    pub hoisted_functions: Vec<u32>,
    /// Classes to try to declare before execution (top-level).
    pub hoisted_classes: Vec<u32>,
    /// The declaration table shared by all code of the unit.
    pub decls: Rc<DeclTable>,
    /// Compile-time warnings (level bits, message, line) to emit before
    /// execution.
    pub warnings: Vec<(i64, String, u32)>,
}

/// Options for [`compile`].
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    /// The file name used by `__FILE__` and diagnostics.
    pub filename: ZStr,
    /// Compiling `eval()`'d code.
    pub is_eval: bool,
    /// Class scope name for eval inside a class (for `__CLASS__`).
    pub eval_class: Option<ZStr>,
    /// `zend.assertions` value at compile time (`-1` drops asserts).
    pub assertions: i64,
    /// Parent class for `parent` in eval'd class-scope code.
    pub eval_parent: Option<ZStr>,
    /// Name given to the top-level closure (partial applications).
    pub closure_name: Option<ZStr>,
    /// Initial `strict_types`.
    pub strict_types: bool,
    /// Whether `sprintf` is currently registered as an internal function.
    pub sprintf_builtin: bool,
    /// Whether `array_map` is currently registered as an internal function
    /// (`zend_compile_func_array_map()` needs its arginfo).
    pub array_map_builtin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FnKind {
    Main,
    Function,
    Method,
    Closure,
    ConstExpr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoopKind {
    Loop,
    Switch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cleanup {
    None,
    FeFree(u32),
    FreeTmp(u32),
}

#[derive(Debug)]
pub(crate) struct LoopInfo {
    pub kind: LoopKind,
    pub break_patches: Vec<usize>,
    pub continue_patches: Vec<usize>,
    pub cleanup: Cleanup,
    pub finally_depth: usize,
    /// Finally-body nesting (`FnCtx::in_finally`) the loop was opened at:
    /// a `break`/`continue` compiled deeper jumps out of a finally block.
    pub in_finally: u32,
}

/// An unwinding op emitted before a `goto` (`zend_compile_goto` calls
/// `zend_handle_loops_and_finally`); the ones the jump does not leave are
/// turned into NOPs once the label is known (`zend_resolve_goto_label`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GotoUnwind {
    /// Frees the variable of the loop with this id.
    Loop(usize),
    /// Calls the finally block with this id.
    Finally(u32),
}

/// A `goto` whose label is resolved when the function is finished.
#[derive(Debug)]
pub(crate) struct PendingGoto {
    /// The `Jmp` op.
    pub at: usize,
    pub label: Vec<u8>,
    pub line: u32,
    /// Ids of the loops enclosing the `goto`.
    pub loops: Vec<usize>,
    /// The unwinding ops emitted before the jump, innermost first.
    pub unwind: Vec<(usize, GotoUnwind)>,
}

/// Class context while compiling methods.
#[derive(Clone, Debug)]
pub(crate) struct ClassCtx {
    pub name: ZStr,
    pub parent: Option<ZStr>,
    pub is_trait: bool,
    pub is_interface: bool,
    pub is_anon: bool,
}

/// Per-function compilation state.
#[derive(Debug)]
pub(crate) struct FnCtx {
    pub kind: FnKind,
    pub ops: Vec<Op>,
    pub lines: Vec<u32>,
    pub literals: Vec<Value>,
    lit_strs: HashMap<Vec<u8>, u32>,
    pub places: Vec<Place>,
    pub cv_names: Vec<ZStr>,
    cv_map: HashMap<Vec<u8>, u32>,
    pub num_tmps: u32,
    pub num_iters: u32,
    pub num_finally: u32,
    pub num_statics: u32,
    pub try_table: Vec<(TryEntry, Label)>,
    pub loops: Vec<LoopInfo>,
    pub finally_stack: Vec<u32>,
    pub labels: HashMap<Vec<u8>, (Label, Vec<usize>)>,
    pub gotos: Vec<PendingGoto>,
    pub loop_ids: Vec<usize>,
    /// FE_RESET op of each foreach iterator (exceptions thrown while a
    /// `return` frees the loop are dispatched from the loop's position).
    pub iter_starts: HashMap<u32, u32>,
    next_loop_id: usize,
    pub line: u32,
    pub is_generator: bool,
    pub returns_ref: bool,
    pub has_return_type: bool,
    pub return_type: Option<TypeDecl>,
    pub func_name: ZStr,
    pub method_name: ZStr,
    pub is_static: bool,
    /// Short-circuit jumps of the current nullsafe chain.
    pub sc_jumps: Vec<usize>,
    /// Compiling an `isset()`/`empty()`/`??` operand: nullsafe bases are
    /// fetched in BP_VAR_IS mode.
    pub sc_quiet: bool,
    /// See [`OpArray::live_tmps`].
    pub live_tmps: Vec<crate::opcode::LiveTmp>,
    pub in_finally: u32,
    /// Pending `FastCall` patches: (finally id, op index).
    pub fast_calls: Vec<(u32, usize)>,
}

impl FnCtx {
    pub(crate) fn pending_fast_calls(&mut self) -> &mut Vec<(u32, usize)> {
        &mut self.fast_calls
    }

    pub(crate) fn new(kind: FnKind) -> Self {
        FnCtx {
            kind,
            ops: Vec::new(),
            lines: Vec::new(),
            literals: Vec::new(),
            lit_strs: HashMap::new(),
            places: Vec::new(),
            cv_names: Vec::new(),
            cv_map: HashMap::new(),
            num_tmps: 0,
            num_iters: 0,
            num_finally: 0,
            num_statics: 0,
            try_table: Vec::new(),
            loops: Vec::new(),
            finally_stack: Vec::new(),
            labels: HashMap::new(),
            gotos: Vec::new(),
            loop_ids: Vec::new(),
            iter_starts: HashMap::new(),
            next_loop_id: 0,
            line: 1,
            is_generator: false,
            returns_ref: false,
            has_return_type: false,
            return_type: None,
            func_name: ZStr::empty(),
            method_name: ZStr::empty(),
            is_static: false,
            sc_jumps: Vec::new(),
            sc_quiet: false,
            live_tmps: Vec::new(),
            in_finally: 0,
            fast_calls: Vec::new(),
        }
    }
}

/// The compiler for one file.
pub(crate) struct Compiler {
    pub filename: ZStr,
    pub namespace: Vec<u8>,
    pub class_uses: HashMap<Vec<u8>, Vec<u8>>,
    pub fn_uses: HashMap<Vec<u8>, Vec<u8>>,
    pub const_uses: HashMap<Vec<u8>, Vec<u8>>,
    /// `FC(seen_symbols)`: lower-case class/function names and constant
    /// names declared so far in the file, with their `ZEND_SYMBOL_*` kinds.
    pub seen_symbols: HashMap<Vec<u8>, u8>,
    pub strict_types: bool,
    /// `FC(declarables).ticks`: N of the active `declare(ticks=N)`.
    pub(crate) ticks: u32,
    pub decls: Rc<DeclTable>,
    pub fns: Vec<FnCtx>,
    pub classes: Vec<ClassCtx>,
    pub hoisted_functions: Vec<u32>,
    pub hoisted_classes: Vec<u32>,
    pub warnings: Vec<(i64, String, u32)>,
    pub anon_counter: u32,
    pub opts: CompileOptions,
    /// Names of functions declared in this file (for redeclare errors).
    pub declared_functions: HashMap<Vec<u8>, u32>,
    pub source: Rc<[u8]>,
    /// Original source offsets cannot recover multibyte semantic text.
    pub multibyte: bool,
    /// Property name while compiling a property hook (`__PROPERTY__`).
    pub hook_prop: Option<Vec<u8>>,
    pub closure_depth: u32,
    /// Compiling a parameter default: a constant expression whose
    /// `self`/`parent` (in `new` and `X::C`) are only checked when it is
    /// evaluated, as `zend_compile_const_expr_*` do not call
    /// `zend_ensure_valid_class_fetch_type()`.
    pub in_const_default: bool,
    /// Compiling the value of a top-level `const` statement, which PHP
    /// evaluates as a constant-expression AST (`zend_compile_const_decl`).
    pub in_const_decl: bool,
    /// `fns.len()` while a global `const` value or a parameter default is
    /// compiled inline (0 otherwise): these are constant expressions in
    /// php-src, evaluated by `zend_ast_evaluate_ex`.
    pub inline_const_depth: usize,
    /// Compiling the value of a top-level `const` statement, which runs
    /// inline rather than as a constant-expression op array.
    pub in_global_const: bool,
    /// While compiling a function's or method's attributes: its display
    /// name (`foo()`, `C::m()`), which names the closures in the attribute
    /// arguments as `CG(active_op_array)` is already the function.
    pub attr_owner: Option<String>,
}

/// Compiles a parsed program.
pub fn compile(
    program: &ast::Program,
    source: Rc<[u8]>,
    opts: CompileOptions,
) -> CResult<CompiledUnit> {
    let mut c = Compiler {
        filename: opts.filename.clone(),
        namespace: Vec::new(),
        class_uses: HashMap::new(),
        fn_uses: HashMap::new(),
        const_uses: HashMap::new(),
        seen_symbols: HashMap::new(),
        strict_types: false,
        ticks: 0,
        decls: Rc::new(DeclTable::default()),
        fns: Vec::new(),
        classes: Vec::new(),
        hoisted_functions: Vec::new(),
        hoisted_classes: Vec::new(),
        warnings: Vec::new(),
        anon_counter: 0,
        opts,
        declared_functions: HashMap::new(),
        source,
        multibyte: program.multibyte,
        hook_prop: None,
        closure_depth: 0,
        in_const_default: false,
        in_const_decl: false,
        inline_const_depth: 0,
        in_global_const: false,
        attr_owner: None,
    };
    for w in &program.warnings {
        let level = match w.level {
            ast::DiagnosticLevel::CompileWarning => 128,
            ast::DiagnosticLevel::Deprecated => 8192,
            ast::DiagnosticLevel::Warning => 2,
        };
        c.warnings.push((level, w.message.clone(), w.line));
    }
    c.strict_types = c.opts.strict_types;
    if let Some(cls) = c.opts.eval_class.clone() {
        c.classes.push(ClassCtx {
            name: cls,
            parent: c.opts.eval_parent.clone(),
            is_trait: false,
            is_interface: false,
            is_anon: false,
        });
    }
    let mut f = FnCtx::new(FnKind::Main);
    f.line = program.statements.first().map_or(1, |s| s.span.line);
    c.fns.push(f);
    c.compile_top_statements(&program.statements)?;
    let line = c.f().line;
    let one = c.lit(Value::Int(1));
    c.f().line = line;
    c.emit(Op::Return { val: one });
    let f = c.fns.pop().expect("main fn ctx");
    let code = c.finish_fn(f)?;
    let main = Rc::new(UserFunction {
        name: ZStr::empty(),
        params: Vec::new(),
        required: 0,
        return_type: None,
        flags: 0,
        code: Rc::new(code),
        line_start: 1,
        line_end: 0,
        doc_comment: None,
        statics: std::cell::RefCell::new(Vec::new()),
        uses: Vec::new(),
        class_name: None,
        attributes: Vec::new(),
        hook_prop: None,
    });
    Ok(CompiledUnit {
        main,
        hoisted_functions: c.hoisted_functions,
        hoisted_classes: c.hoisted_classes,
        decls: c.decls,
        warnings: c.warnings,
    })
}

impl Compiler {
    /// The current function context.
    pub(crate) fn f(&mut self) -> &mut FnCtx {
        self.fns.last_mut().expect("function context")
    }

    pub(crate) fn fref(&self) -> &FnCtx {
        self.fns.last().expect("function context")
    }

    pub(crate) fn err<T>(&self, msg: impl Into<String>, line: u32) -> CResult<T> {
        Err(CompileError::new(msg, line, self.warnings.clone()))
    }

    pub(crate) fn emit(&mut self, op: Op) -> usize {
        let f = self.f();
        let line = f.line;
        f.ops.push(op);
        f.lines.push(line);
        f.ops.len() - 1
    }

    pub(crate) fn here(&mut self) -> Label {
        u32::try_from(self.f().ops.len()).unwrap_or(u32::MAX)
    }

    pub(crate) fn tmp(&mut self) -> u32 {
        let f = self.f();
        f.num_tmps += 1;
        f.num_tmps - 1
    }

    pub(crate) fn lit(&mut self, v: Value) -> Operand {
        Operand::Lit(self.lit_idx(v))
    }

    pub(crate) fn lit_idx(&mut self, v: Value) -> u32 {
        let f = self.f();
        if let Value::Str(s) = &v {
            if let Some(&i) = f.lit_strs.get(s.as_bytes()) {
                return i;
            }
            let i = u32::try_from(f.literals.len()).unwrap_or(u32::MAX);
            f.lit_strs.insert(s.as_bytes().to_vec(), i);
            f.literals.push(Value::Str(s.interned()));
            return i;
        }
        f.literals.push(v);
        u32::try_from(f.literals.len() - 1).unwrap_or(u32::MAX)
    }

    pub(crate) fn lit_str(&mut self, s: &[u8]) -> u32 {
        self.lit_idx(Value::Str(ZStr::from(s)))
    }

    pub(crate) fn cv(&mut self, name: &[u8]) -> u32 {
        let f = self.f();
        if let Some(&i) = f.cv_map.get(name) {
            return i;
        }
        let i = u32::try_from(f.cv_names.len()).unwrap_or(u32::MAX);
        f.cv_names.push(ZStr::from(name));
        f.cv_map.insert(name.to_vec(), i);
        i
    }

    pub(crate) fn has_cv(&self, name: &[u8]) -> bool {
        self.fref().cv_map.contains_key(name)
    }

    pub(crate) fn place(&mut self, p: Place) -> PlaceId {
        let f = self.f();
        f.places.push(p);
        u32::try_from(f.places.len() - 1).unwrap_or(u32::MAX)
    }

    /// Patches the jump target of op `at` to `target`.
    pub(crate) fn patch(&mut self, at: usize, target: Label) {
        let op = &mut self.f().ops[at];
        match op {
            Op::Jmp(t)
            | Op::JmpZ(_, t)
            | Op::JmpNz(_, t)
            | Op::JmpZEx { target: t, .. }
            | Op::JmpNzEx { target: t, .. }
            | Op::JmpSet { target: t, .. }
            | Op::Coalesce { target: t, .. }
            | Op::JmpNull { target: t, .. }
            | Op::New { skip: t, .. }
            | Op::RecvInit { skip: t, .. }
            | Op::BindStatic { skip: t, .. }
            | Op::FastCall { target: t, .. }
            | Op::FeReset { end: t, .. }
            | Op::FeFetch { end: t, .. }
            | Op::Catch { next: t, .. }
            | Op::AssertCheck { skip: t, .. } => *t = target,
            _ => {}
        }
    }

    pub(crate) fn patch_here(&mut self, at: usize) {
        let h = self.here();
        self.patch(at, h);
    }

    /// Turns an operand into one that can be read twice: tmps become
    /// `TmpKeep`.
    pub(crate) fn keep(op: Operand) -> Operand {
        match op {
            Operand::Tmp(t) => Operand::TmpKeep(t),
            o => o,
        }
    }

    /// Finalises a function context into an op array.
    pub(crate) fn finish_fn(&mut self, mut f: FnCtx) -> CResult<OpArray> {
        // Resolve gotos.
        for PendingGoto {
            at,
            label,
            line,
            loops,
            unwind,
        } in std::mem::take(&mut f.gotos)
        {
            let Some((target, label_loops)) = f.labels.get(&label) else {
                return self.err(
                    format!(
                        "'goto' to undefined label '{}'",
                        String::from_utf8_lossy(&label)
                    ),
                    line,
                );
            };
            // `zend_check_finally_breakout`.
            let target_op = *target;
            let at_op = u32::try_from(at).unwrap_or(u32::MAX);
            for (e, _) in &f.try_table {
                let Some((_, fs, fe)) = e.finally else {
                    continue;
                };
                let last = fe.saturating_sub(1);
                let in_fin = |op: u32| op >= fs && op <= last;
                if !in_fin(at_op) && in_fin(target_op) {
                    return self.err("jump into a finally block is disallowed", line);
                }
                if in_fin(at_op) && !in_fin(target_op) {
                    return self.err("jump out of a finally block is disallowed", line);
                }
            }
            // A jump into a loop is not allowed.
            if !label_loops.iter().all(|l| loops.contains(l)) {
                return self.err("'goto' into loop or switch statement is disallowed", line);
            }
            // `zend_resolve_goto_label`: the loop frees and finally calls of
            // the constructs the jump stays inside become NOPs.
            for (op, u) in unwind {
                let crossed = match u {
                    GotoUnwind::Loop(id) => !label_loops.contains(&id),
                    GotoUnwind::Finally(fid) => f
                        .try_table
                        .iter()
                        .find_map(|(e, _)| match e.finally {
                            Some((id, _, fe)) if id == fid => {
                                Some(target_op < e.try_start || target_op >= fe)
                            }
                            _ => None,
                        })
                        .unwrap_or(true),
                };
                if !crossed {
                    f.ops[op] = Op::Nop;
                }
            }
            if let Op::Jmp(t) = &mut f.ops[at] {
                *t = target_op;
            }
        }
        let mut try_table: Vec<(TryEntry, Label)> = std::mem::take(&mut f.try_table);
        try_table.sort_by(|a, b| b.0.try_start.cmp(&a.0.try_start));
        Ok(OpArray {
            ops: f.ops,
            lines: f.lines,
            literals: f.literals,
            places: f.places,
            cv_names: f.cv_names,
            num_tmps: f.num_tmps,
            num_iters: f.num_iters,
            num_finally: f.num_finally,
            num_statics: f.num_statics,
            try_table: try_table.into_iter().map(|(e, _)| e).collect(),
            live_tmps: f.live_tmps,
            filename: self.filename.clone(),
            strict_types: self.strict_types,
            decls: self.decls.clone(),
            fcall_cache: std::cell::RefCell::default(),
            class_cache: std::cell::RefCell::default(),
        })
    }

    // ------------------------------------------------------------------
    // Name resolution
    // ------------------------------------------------------------------

    fn ns_prefixed(&self, name: &[u8]) -> Vec<u8> {
        if self.namespace.is_empty() {
            name.to_vec()
        } else {
            let mut v = self.namespace.clone();
            v.push(b'\\');
            v.extend_from_slice(name);
            v
        }
    }

    /// `zend_resolve_class_name`.
    pub(crate) fn resolve_class_name(&self, name: &Name) -> Vec<u8> {
        match name.kind {
            NameKind::FullyQualified => name.name.clone(),
            NameKind::Relative => self.ns_prefixed(&name.name),
            NameKind::Unqualified | NameKind::Qualified => {
                let (first, rest) = match name.name.iter().position(|&c| c == b'\\') {
                    Some(p) => (&name.name[..p], Some(&name.name[p..])),
                    None => (&name.name[..], None),
                };
                if let Some(imp) = self.class_uses.get(&first.to_ascii_lowercase()) {
                    let mut v = imp.clone();
                    if let Some(r) = rest {
                        v.extend_from_slice(r);
                    }
                    return v;
                }
                self.ns_prefixed(&name.name)
            }
        }
    }

    /// Resolves a function name: returns (name, fallback global name).
    pub(crate) fn resolve_function_name(&self, name: &Name) -> (Vec<u8>, Option<Vec<u8>>) {
        match name.kind {
            NameKind::FullyQualified => (name.name.clone(), None),
            NameKind::Relative => (self.ns_prefixed(&name.name), None),
            NameKind::Qualified => (self.resolve_class_name(name), None),
            NameKind::Unqualified => {
                if let Some(imp) = self.fn_uses.get(&name.name.to_ascii_lowercase()) {
                    return (imp.clone(), None);
                }
                if self.namespace.is_empty() {
                    (name.name.clone(), None)
                } else {
                    (self.ns_prefixed(&name.name), Some(name.name.clone()))
                }
            }
        }
    }

    /// Resolves a constant name: returns (name, fallback).
    pub(crate) fn resolve_const_name(&self, name: &Name) -> (Vec<u8>, Option<Vec<u8>>) {
        match name.kind {
            NameKind::FullyQualified => (name.name.clone(), None),
            NameKind::Relative => (self.ns_prefixed(&name.name), None),
            NameKind::Qualified => (self.resolve_class_name(name), None),
            NameKind::Unqualified => {
                if let Some(imp) = self.const_uses.get(&name.name) {
                    return (imp.clone(), None);
                }
                if self.namespace.is_empty() {
                    (name.name.clone(), None)
                } else {
                    (self.ns_prefixed(&name.name), Some(name.name.clone()))
                }
            }
        }
    }

    /// Builds a [`ClassSpec`] from a class name used in an expression.
    pub(crate) fn class_spec_name(&mut self, name: &Name) -> CResult<ClassSpec> {
        if name.kind == NameKind::Unqualified {
            let lc = name.name.to_ascii_lowercase();
            match lc.as_slice() {
                b"self" => {
                    self.check_class_scope("self", name.span.line)?;
                    return Ok(ClassSpec::SelfClass);
                }
                b"parent" => {
                    self.check_class_scope("parent", name.span.line)?;
                    return Ok(ClassSpec::Parent);
                }
                b"static" => {
                    self.check_class_scope("static", name.span.line)?;
                    return Ok(ClassSpec::Static);
                }
                _ => {}
            }
        }
        let resolved = self.resolve_class_name(name);
        let l = self.lit_str(&resolved);
        Ok(ClassSpec::Named(l))
    }

    /// The compile-time `self`/`parent`/`static` check of a class
    /// reference. Constant expressions (parameter defaults, constant and
    /// property initializers) defer it to evaluation: the
    /// `zend_compile_const_expr_*` helpers for `new` and `X::C` do not call
    /// `zend_ensure_valid_class_fetch_type()`.
    fn check_class_scope(&self, what: &str, line: u32) -> CResult<()> {
        if self.in_const_expr() {
            return Ok(());
        }
        self.ensure_valid_class_fetch(what, line)
    }

    /// Whether a constant expression is being compiled.
    pub(crate) fn in_const_expr(&self) -> bool {
        self.in_const_default || self.fref().kind == FnKind::ConstExpr
    }

    /// Whether the code being compiled is a constant expression evaluated
    /// by `zend_ast_evaluate_ex` (not a closure body inside one).
    pub(crate) fn in_evaluated_const_expr(&self) -> bool {
        self.fref().kind == FnKind::ConstExpr || self.inline_const_depth == self.fns.len()
    }

    /// `zend_ensure_valid_class_fetch_type()`: `self`/`parent`/`static`
    /// are rejected at compile time only where the scope is known
    /// (`zend_is_scope_known()`): not in closures, nor in file or eval
    /// code (which inherits the including scope), nor in traits; elsewhere
    /// the runtime fetch reports the error.
    pub(crate) fn ensure_valid_class_fetch(&self, what: &str, line: u32) -> CResult<()> {
        let in_closure = self
            .fns
            .iter()
            .rev()
            .take_while(|f| f.kind != FnKind::Main)
            .any(|f| f.kind == FnKind::Closure);
        if in_closure {
            return Ok(());
        }
        let Some(c) = self.classes.last() else {
            let kind = self
                .fns
                .iter()
                .rev()
                .map(|f| f.kind)
                .find(|k| *k != FnKind::ConstExpr)
                .unwrap_or(FnKind::Main);
            if kind == FnKind::Main {
                return Ok(());
            }
            return self.err(
                format!("Cannot use \"{what}\" when no class scope is active"),
                line,
            );
        };
        if what == "parent" && c.parent.is_none() && !c.is_trait {
            return self.err(
                "Cannot use \"parent\" when current class scope has no parent",
                line,
            );
        }
        Ok(())
    }

    pub(crate) fn class_spec(&mut self, cref: &ast::ClassRef) -> CResult<ClassSpec> {
        match cref {
            ast::ClassRef::Name(n) => self.class_spec_name(n),
            ast::ClassRef::Expr(e) => {
                let op = self.compile_expr(e)?;
                Ok(ClassSpec::Dyn(op))
            }
        }
    }

    /// The current class name (for `__CLASS__`, `self` in types).
    pub(crate) fn current_class(&self) -> Option<&ClassCtx> {
        self.classes.last()
    }

    pub(crate) fn attributes(&mut self, groups: &ast::Attributes) -> CResult<Vec<AttributeInfo>> {
        self.attributes_at(groups, None)
    }

    /// [`Compiler::attributes`] of a declaration starting on `decl_line`:
    /// `zend_compile_attributes()` reports its own argument-list errors on
    /// `CG(zend_lineno)`, the declaration's line rather than the attribute's.
    pub(crate) fn attributes_at(
        &mut self,
        groups: &ast::Attributes,
        decl_line: Option<u32>,
    ) -> CResult<Vec<AttributeInfo>> {
        let mut out = Vec::new();
        for g in groups {
            for a in &g.attributes {
                let name = ZStr::from(self.resolve_class_name(&a.name));
                let mut args: Vec<(Option<ZStr>, ConstExpr)> = Vec::new();
                if let Some(al) = &a.args {
                    if al.is_first_class_callable() {
                        return self.err(
                            "Cannot create Closure as attribute argument",
                            decl_line.unwrap_or(a.span.line),
                        );
                    }
                    let mut uses_named = false;
                    for arg in &al.args {
                        match &arg.kind {
                            ast::ArgKind::Positional(e) => {
                                if uses_named {
                                    return self.err(
                                        "Cannot use positional argument after named argument",
                                        decl_line.unwrap_or(arg.span.line),
                                    );
                                }
                                self.check_const_expr(e, true)?;
                                args.push((None, self.const_expr(e)?));
                            }
                            ast::ArgKind::Named { name, value } => {
                                uses_named = true;
                                let n = ZStr::from(name.name.as_slice());
                                if args.iter().any(|(k, _)| k.as_ref() == Some(&n)) {
                                    return self.err(
                                        format!(
                                            "Duplicate named parameter ${}",
                                            n.to_string_lossy()
                                        ),
                                        decl_line.unwrap_or(arg.span.line),
                                    );
                                }
                                self.check_const_expr(value, true)?;
                                args.push((Some(n), self.const_expr(value)?));
                            }
                            ast::ArgKind::Unpack(_) => {
                                return self.err(
                                    "Cannot use unpacking in attribute argument list",
                                    decl_line.unwrap_or(arg.span.line),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                out.push(AttributeInfo {
                    name,
                    args,
                    line: a.span.line,
                    strict: self.strict_types,
                });
            }
        }
        Ok(out)
    }

    /// Compiles a constant expression into a [`ConstExpr`].
    pub(crate) fn const_expr(&mut self, e: &ast::Expr) -> CResult<ConstExpr> {
        if let Some(v) = self.try_fold(e) {
            let v = match v {
                Value::Str(s) => Value::Str(s.interned()),
                other => other,
            };
            return Ok(ConstExpr::Value(v));
        }
        let mut f = FnCtx::new(FnKind::ConstExpr);
        f.line = e.span.line;
        self.fns.push(f);
        let r = self.compile_expr(e);
        let r = match r {
            Ok(op) => {
                self.emit(Op::Return { val: op });
                Ok(())
            }
            Err(err) => Err(err),
        };
        let f = self.fns.pop().expect("const expr ctx");
        r?;
        let code = self.finish_fn(f)?;
        Ok(ConstExpr::Code(Rc::new(code)))
    }

    /// Registers a class declaration.
    pub(crate) fn add_class(&mut self, decl: ClassDecl) -> u32 {
        let mut cs = self.decls.classes.borrow_mut();
        cs.push(Rc::new(decl));
        u32::try_from(cs.len() - 1).unwrap_or(u32::MAX)
    }

    /// Registers a function.
    pub(crate) fn add_function(&mut self, f: UserFunction) -> u32 {
        let mut fs = self.decls.functions.borrow_mut();
        fs.push(Rc::new(f));
        u32::try_from(fs.len() - 1).unwrap_or(u32::MAX)
    }
}
