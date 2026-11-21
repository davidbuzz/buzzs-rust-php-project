// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Opcode array disassembler in `zend_dump.c` style.
//!
//! `opcache.opt_debug_level` with `ZEND_DUMP_AFTER_OPTIMIZER` (`0x20000`)
//! prints every op array of a compiled script the way php-src's
//! `zend_dump_op_array` (`Zend/Optimizer/zend_dump.c`) does after
//! `zend_optimize_script` (`Zend/Optimizer/zend_optimizer.c`): the op
//! arrays in `zend_foreach_op_array` order, each lowered to the Zend
//! oplines php-src compiles for the same code ([`lower`]), with the
//! variable contractions of the DFA pass, `zend_optimizer_compact_vars`,
//! `zend_optimize_temporary_variables` and the live ranges
//! ([`passes`]). The DFA pass's own SSA dumps (`ZEND_DUMP_BEFORE_DFA_PASS`,
//! `ZEND_DUMP_AFTER_DFA_PASS`) are [`ssa`]'s.

mod lower;
mod model;
mod passes;
mod ssa;
#[cfg(test)]
mod tests;

pub use ssa::{dump_unit_dfa, DUMP_AFTER_DFA_PASS, DUMP_BEFORE_DFA_PASS};

use crate::class::ClassDecl;
use crate::compiler::{CompiledUnit, FunctionTable};
use crate::func::{flags, UserFunction};
use crate::opcode::{ClassSpec, Op, OpArray, Operand};
use model::{StackOf, ZLine, Z};
use php_runtime::Value;
use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

/// `ZEND_DUMP_BEFORE_OPTIMIZER`.
pub const DUMP_BEFORE_OPTIMIZER: i64 = 1 << 16;
/// `ZEND_DUMP_AFTER_OPTIMIZER`.
pub const DUMP_AFTER_OPTIMIZER: i64 = 1 << 17;

thread_local! {
    /// The instruction buffers (`ops.as_ptr()`) of the op arrays that the
    /// VM's optimizer reduced to `return <literal>;` although their
    /// compiled code did more: php-src inlines a call only when the
    /// callee's code is that literal return before its own optimization
    /// (`zend_try_inline_call` sees the code of `zend_compile`, or of
    /// passes 1 to 5), never after SCCP.
    static REDUCED_TO_LITERAL: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Whether `code` does nothing but return a literal (or nothing) after
/// receiving its arguments: the shape `zend_try_inline_call` inlines.
pub(crate) fn returns_literal(code: &OpArray) -> bool {
    let mut body = code.ops.iter().map(lower::generic).filter(|op| {
        !matches!(
            op,
            Op::Nop | Op::Recv { .. } | Op::RecvInit { .. } | Op::RecvDefault { .. }
        )
    });
    matches!(
        body.next(),
        Some(Op::Return {
            val: Operand::Lit(_) | Operand::Unused
        })
    )
}

/// Records that the optimizer reduced `code` to a literal return that
/// its compiled code was not ([`returns_literal`]).
pub(crate) fn note_reduced_to_literal(code: &OpArray) {
    // Bounded: a long-running process forgets old entries (which only
    // makes the dump of an old function's callers less exact).
    const MAX_ENTRIES: usize = 1 << 16;
    let key = code.ops.as_ptr().addr();
    REDUCED_TO_LITERAL.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() >= MAX_ENTRIES {
            s.clear();
        }
        s.insert(key);
    });
}

pub(crate) use lower::literal_return_value;

/// Whether `code` was reduced to a literal return by the VM's optimizer
/// ([`note_reduced_to_literal`]).
fn reduced_to_literal(code: &OpArray) -> bool {
    let key = code.ops.as_ptr().addr();
    REDUCED_TO_LITERAL.with(|s| s.borrow().contains(&key))
}

/// One op array to dump.
struct Entry {
    /// `zend_dump_op_array_name`.
    name: Vec<u8>,
    func: Rc<UserFunction>,
    is_main: bool,
    /// Lower-case name of the class scope.
    scope: Option<Vec<u8>>,
    /// The function decl indices this op array declares
    /// (`dynamic_func_defs`).
    dyn_defs: Vec<u32>,
}

/// The function decls op array `func` declares itself
/// (`DECLARE_FUNCTION`, `DECLARE_LAMBDA_FUNCTION`), in decl order.
fn dyn_defs_of(func: &UserFunction) -> Vec<u32> {
    let mut v: Vec<u32> = func
        .code
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::DeclareFunction(i) | Op::MakeClosure { idx: i, .. } => Some(*i),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Adds `func` and, recursively, the functions it declares
/// (`zend_foreach_op_array_helper`).
fn add_entry(
    out: &mut Vec<Entry>,
    name: Vec<u8>,
    func: Rc<UserFunction>,
    is_main: bool,
    scope: Option<Vec<u8>>,
) {
    let dyn_defs = dyn_defs_of(&func);
    let nested: Vec<Rc<UserFunction>> = {
        let table = func.code.decls.functions.borrow();
        dyn_defs
            .iter()
            .filter_map(|&i| table.get(i as usize).cloned())
            .collect()
    };
    out.push(Entry {
        name,
        func,
        is_main,
        scope: scope.clone(),
        dyn_defs,
    });
    for f in nested {
        let n = f.name.as_bytes().to_vec();
        add_entry(out, n, f, false, scope.clone());
    }
}

/// The op arrays of `unit` in `zend_foreach_op_array` order: the main
/// code, the script's functions, then each class's methods and property
/// hooks.
fn collect(unit: &CompiledUnit) -> Vec<Entry> {
    let mut out = Vec::new();
    add_entry(&mut out, b"$_main".to_vec(), unit.main.clone(), true, None);
    let functions: Vec<Rc<UserFunction>> = {
        let table = unit.decls.functions.borrow();
        unit.hoisted_functions
            .iter()
            .filter_map(|&i| table.get(i as usize).cloned())
            .collect()
    };
    for f in functions {
        let n = f.name.as_bytes().to_vec();
        add_entry(&mut out, n, f, false, None);
    }
    let classes: Vec<Rc<ClassDecl>> = unit.decls.classes.borrow().clone();
    for c in classes {
        let lc = c.name.as_bytes().to_ascii_lowercase();
        for m in &c.methods {
            if m.flags & flags::ABSTRACT != 0 || m.func.flags & flags::NO_BODY != 0 {
                continue;
            }
            let mut n = printable(c.name.as_bytes()).to_vec();
            n.extend_from_slice(b"::");
            n.extend_from_slice(m.name.as_bytes());
            add_entry(&mut out, n, m.func.clone(), false, Some(lc.clone()));
        }
        for p in &c.props {
            let Some(h) = &p.hooks else { continue };
            for (kind, f) in [("get", &h.get), ("set", &h.set)] {
                let Some(f) = f else { continue };
                if f.flags & (flags::ABSTRACT | flags::NO_BODY) != 0 {
                    continue;
                }
                let mut n = printable(c.name.as_bytes()).to_vec();
                n.extend_from_slice(b"::$");
                n.extend_from_slice(p.name.as_bytes());
                n.extend_from_slice(b"::");
                n.extend_from_slice(kind.as_bytes());
                add_entry(&mut out, n, f.clone(), false, Some(lc.clone()));
            }
        }
    }
    out
}

/// A class name as `%s` prints it: an anonymous class's name ends at its
/// NUL byte (`zend_dump_op_array_name`).
fn printable(name: &[u8]) -> &[u8] {
    name.iter()
        .position(|&b| b == 0)
        .map_or(name, |end| &name[..end])
}

/// One lowered and finished op array.
struct Done {
    lines: Vec<ZLine>,
    /// The original index of each CV left.
    kept: Vec<u32>,
    /// `op_array->T`.
    tmps: u32,
    live: Vec<passes::LiveRange>,
    num_args: u32,
    /// The exception table.
    exc: Vec<[Option<u32>; 4]>,
}

fn finish(e: &Entry, res: &lower::Resolver<'_>) -> Done {
    let lower::Lowered { mut lines, first_z } =
        lower::Lower::new(&e.func, e.scope.clone(), &e.dyn_defs, res).run();
    passes::resolve_labels(&mut lines, &first_z);
    if res.level & (super::PASS_6 | super::PASS_8) != 0 {
        passes::drop_empty_foreach(&mut lines);
    }
    let oa = &e.func.code;
    let shape = passes::FuncShape {
        is_main: e.is_main,
        ncv: oa.cv_names.len(),
        params: e
            .func
            .params
            .iter()
            .map(|p| passes::decl_type(p.ty.as_ref()))
            .collect(),
        by_ref: e.func.params.iter().map(|p| p.by_ref).collect(),
        bound: e.func.uses.iter().map(|u| (u.cv, u.by_ref)).collect(),
        ret: e
            .func
            .return_type
            .as_ref()
            .filter(|_| e.func.flags & flags::RETURN_REF == 0)
            .map(passes::pure_type),
    };
    let level = res.level;
    let types = passes::infer(&lines, &shape);
    if level & super::PASS_6 != 0 && !e.is_main && passes::dfa_allowed(&lines) {
        passes::dfa(&mut lines, &types, &shape);
    }
    let (mut lines, new_idx) = passes::remove_nops(lines);
    // `try_catch_array`: try, catch, finally and finally-end oplines.
    let at = |l: u32| -> u32 {
        let z = first_z
            .get(l as usize)
            .or_else(|| first_z.last())
            .copied()
            .unwrap_or(0) as usize;
        new_idx
            .get(z)
            .or_else(|| new_idx.last())
            .copied()
            .unwrap_or(0)
    };
    let mut exc: Vec<[Option<u32>; 4]> = oa
        .try_table
        .iter()
        .rev()
        .map(|t| {
            let (fin, fin_end) = match t.finally {
                Some((_, start, end)) => {
                    let mut e = at(end);
                    // `finally_end` is the `FAST_RET`.
                    if e > 0
                        && lines.get(e as usize).is_none_or(|l| l.op != "FAST_RET")
                        && lines
                            .get(e as usize - 1)
                            .is_some_and(|l| l.op == "FAST_RET")
                    {
                        e -= 1;
                    }
                    (Some(at(start)), Some(e))
                }
                None => (None, None),
            };
            [Some(at(t.try_start)), t.catch_start.map(at), fin, fin_end]
        })
        .collect();
    exc.sort_by_key(|e| e[0]);
    let kept = if level & super::PASS_13 != 0 {
        passes::compact_cvs(&mut lines, oa.cv_names.len())
    } else {
        (0..u32::try_from(oa.cv_names.len()).unwrap_or(0)).collect()
    };
    let mut is_var: HashMap<u32, bool> = HashMap::new();
    for l in &lines {
        if let Some(t) = l.res.tmp() {
            is_var.insert(t, l.res_var);
        }
        // `FE_FETCH_RW` writes a VAR value operand.
        if l.op == "FE_FETCH_RW" {
            if let Some(t) = l.op2.tmp() {
                is_var.insert(t, true);
            }
        }
    }
    // Without the optimizer's type information every live range stays.
    let typed = level & super::PASS_6 != 0;
    let needs: Vec<bool> = lines
        .iter()
        .map(|l| {
            if !typed {
                return true;
            }
            l.res.tmp().is_none_or(|t| {
                types
                    .tmp
                    .get(&t)
                    .is_none_or(|ty| ty & passes::ty::REFCOUNTED != 0)
            })
        })
        .collect();
    for l in &mut lines {
        l.op1_var = l
            .op1
            .tmp()
            .is_some_and(|t| is_var.get(&t).copied().unwrap_or(false));
        l.op2_var = l
            .op2
            .tmp()
            .is_some_and(|t| is_var.get(&t).copied().unwrap_or(false));
    }
    let tmps = if level & super::PASS_9 != 0 {
        passes::compact_tmps(&mut lines, oa.num_finally > 0)
    } else {
        passes::number_tmps(&mut lines)
    };
    let mut live =
        passes::live_ranges(&mut lines, tmps, &|i| needs.get(i).copied().unwrap_or(true));
    // Without pass 7 `zend_optimize` itself dumps "after optimizer" with
    // no `ZEND_DUMP_LIVE_RANGES` flag (`zend_optimizer.c`), so no ranges print.
    if level & super::PASS_7 == 0 {
        live.clear();
    }
    let num_args = u32::try_from(e.func.params.iter().filter(|p| !p.variadic).count()).unwrap_or(0);
    Done {
        lines,
        kept,
        tmps,
        live,
        num_args,
        exc,
    }
}

/// `%g` of C's printf.
fn fmt_g(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let sci = format!("{f:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant), exp.abs())
    } else {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        trim(&format!("{f:.decimals$}"))
    }
}

/// `zend_dump_const` (without the leading space).
fn write_const(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(false) => out.extend_from_slice(b"bool(false)"),
        Value::Bool(true) => out.extend_from_slice(b"bool(true)"),
        Value::Int(i) => out.extend_from_slice(format!("int({i})").as_bytes()),
        Value::Float(f) => out.extend_from_slice(format!("float({})", fmt_g(*f)).as_bytes()),
        Value::Str(s) => {
            out.extend_from_slice(b"string(\"");
            for &c in s.as_bytes() {
                match c {
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'\r' => out.extend_from_slice(b"\\r"),
                    b'\t' => out.extend_from_slice(b"\\t"),
                    0x0c => out.extend_from_slice(b"\\f"),
                    0x0b => out.extend_from_slice(b"\\v"),
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    0x1b => out.extend_from_slice(b"\\e"),
                    b'"' => out.extend_from_slice(b"\\\""),
                    c if !(32..=126).contains(&c) => {
                        out.extend_from_slice(format!("\\x{c:02X}").as_bytes());
                    }
                    c => out.push(c),
                }
            }
            out.extend_from_slice(b"\")");
        }
        Value::Array(_) => out.extend_from_slice(b"array(...)"),
        Value::Object(_) => out.extend_from_slice(b"zval(type=8)"),
        Value::Resource(_) => out.extend_from_slice(b"zval(type=9)"),
        Value::Reference(_) => out.extend_from_slice(b"zval(type=10)"),
    }
}

/// Prints one operand with its leading space.
fn write_z(out: &mut Vec<u8>, z: &Z, is_var: bool, d: &Done, names: &[php_runtime::ZStr]) {
    let last_var = u32::try_from(d.kept.len()).unwrap_or(0);
    match z {
        Z::Unused => {}
        Z::Cv(c) => {
            out.push(b' ');
            write_cv(out, *c, d, names);
        }
        Z::Tmp(t) => {
            let k = if is_var { 'V' } else { 'T' };
            out.extend_from_slice(format!(" {k}{}", last_var + t).as_bytes());
        }
        Z::Const(v) => {
            out.push(b' ');
            write_const(out, v);
        }
        Z::Num(n) => out.extend_from_slice(format!(" {n}").as_bytes()),
        Z::Label(l) | Z::ZLabel(l) => out.extend_from_slice(format!(" {l:04}").as_bytes()),
        Z::Text(t) => {
            if !t.is_empty() {
                out.push(b' ');
                out.extend_from_slice(t.as_bytes());
            }
        }
    }
}

/// The jump table of `MATCH` as `zend_dump_op` prints it: ` "key": NNNN,`
/// (string keys raw, unescaped) or ` key: NNNN,` per entry, then
/// ` default:` (the default target follows as the extended jump).
fn write_jump_table(out: &mut Vec<u8>, jt: &[(Value, Z)]) {
    for (k, z) in jt {
        match k {
            Value::Str(s) => {
                out.extend_from_slice(b" \"");
                out.extend_from_slice(s.as_bytes());
                out.extend_from_slice(b"\":");
            }
            Value::Int(i) => out.extend_from_slice(format!(" {i}:").as_bytes()),
            _ => {}
        }
        if let Z::Label(l) | Z::ZLabel(l) = z {
            out.extend_from_slice(format!(" {l:04},").as_bytes());
        }
    }
    out.extend_from_slice(b" default:");
}

fn write_cv(out: &mut Vec<u8>, c: u32, d: &Done, names: &[php_runtime::ZStr]) {
    let name = d
        .kept
        .get(c as usize)
        .and_then(|&o| names.get(o as usize))
        .map(|n| n.as_bytes().to_vec())
        .unwrap_or_default();
    out.extend_from_slice(format!("CV{c}($").as_bytes());
    out.extend_from_slice(&name);
    out.push(b')');
}

/// `zend_vm_calc_used_stack` after `zend_adjust_fcall_stack_size`.
fn stack_size(s: StackOf, done: &[Done]) -> u32 {
    const FRAME_SLOTS: u32 = 5;
    match s {
        StackOf::Internal(n) => (FRAME_SLOTS + n) * 16,
        StackOf::User(i, n) => done.get(i).map_or((FRAME_SLOTS + n) * 16, |d| {
            let vars = u32::try_from(d.kept.len()).unwrap_or(0);
            (FRAME_SLOTS + n + d.tmps + vars - d.num_args.min(n).min(vars)) * 16
        }),
    }
}

fn write_entry(out: &mut Vec<u8>, e: &Entry, d: &Done, done: &[Done], msg: &str, line_end: u32) {
    let names = &e.func.code.cv_names;
    out.push(b'\n');
    out.extend_from_slice(&e.name);
    out.extend_from_slice(
        format!(
            ":\n     ; (lines={}, args={}, vars={}, tmps={})\n",
            d.lines.len(),
            d.num_args,
            d.kept.len(),
            d.tmps
        )
        .as_bytes(),
    );
    out.extend_from_slice(format!("     ; ({msg})\n     ; ").as_bytes());
    out.extend_from_slice(e.func.code.filename.as_bytes());
    let (start, end) = if e.is_main {
        (1, line_end)
    } else {
        (e.func.line_start, e.func.line_end)
    };
    out.extend_from_slice(format!(":{start}-{end}\n").as_bytes());
    let last_var = u32::try_from(d.kept.len()).unwrap_or(0);
    for (i, l) in d.lines.iter().enumerate() {
        out.extend_from_slice(format!("{i:04} ").as_bytes());
        match &l.res {
            Z::Cv(c) => {
                write_cv(out, *c, d, names);
                out.extend_from_slice(b" = ");
            }
            Z::Tmp(t) => {
                let k = if l.res_var { 'V' } else { 'T' };
                out.extend_from_slice(format!("{k}{} = ", last_var + t).as_bytes());
            }
            _ => {}
        }
        out.extend_from_slice(l.op.as_bytes());
        out.extend_from_slice(l.suffix.as_bytes());
        out.extend_from_slice(l.ext.as_bytes());
        let op1 = match l.stack {
            Some(s) => Z::Num(stack_size(s, done)),
            None => l.op1.clone(),
        };
        write_z(out, &op1, l.op1_var, d, names);
        if let Some(jt) = &l.jt {
            write_jump_table(out, jt);
        }
        write_z(out, &l.op2, l.op2_var, d, names);
        if let Some(j) = &l.ext_jmp {
            write_z(out, j, false, d, names);
        }
        out.push(b'\n');
    }
    if !d.live.is_empty() {
        out.extend_from_slice(b"LIVE RANGES:\n");
        for r in &d.live {
            out.extend_from_slice(
                format!(
                    "     {}: {:04} - {:04} ({})\n",
                    last_var + r.var,
                    r.start,
                    r.end,
                    r.kind
                )
                .as_bytes(),
            );
        }
    }
    if !d.exc.is_empty() {
        out.extend_from_slice(b"EXCEPTION TABLE:\n");
        for e in &d.exc {
            let f = |x: Option<u32>| x.map_or_else(|| "-".to_string(), |v| format!("{v:04}"));
            out.extend_from_slice(
                format!("     {}, {}, {}, {}\n", f(e[0]), f(e[1]), f(e[2]), f(e[3])).as_bytes(),
            );
        }
    }
}

/// Dumps every op array of `unit` as `zend_dump_op_array` with the
/// message `msg` (`"after optimizer"`). `functions` is the function table
/// the unit was compiled against and `internal` gives the [`flags`] and
/// return type of its internal functions; `level` is
/// `opcache.optimization_level`; `main_line_end` the last line of the
/// file (`op_array->line_end` of the main code).
#[must_use]
pub fn dump_unit(
    unit: &CompiledUnit,
    functions: &dyn FunctionTable,
    internal: &dyn Fn(&[u8]) -> (u32, Option<crate::types::TypeDecl>),
    level: u32,
    main_line_end: u32,
    msg: &str,
) -> Vec<u8> {
    let (main, rest) = dump_unit_parts(unit, functions, internal, level, main_line_end, msg);
    let mut out = main;
    out.extend(rest);
    out
}

/// [`dump_unit`] in two parts: the main code's dump, and those of the other
/// op arrays.
fn dump_unit_parts(
    unit: &CompiledUnit,
    functions: &dyn FunctionTable,
    internal: &InternalInfo<'_>,
    level: u32,
    main_line_end: u32,
    msg: &str,
) -> (Vec<u8>, Vec<u8>) {
    let entries = collect(unit);
    let mut fn_index: HashMap<Vec<u8>, (usize, Rc<UserFunction>)> = HashMap::new();
    {
        let table = unit.decls.functions.borrow();
        for &i in &unit.hoisted_functions {
            if let Some(f) = table.get(i as usize) {
                if let Some(pos) = entries.iter().position(|e| Rc::ptr_eq(&e.func, f)) {
                    fn_index.insert(f.name.as_bytes().to_ascii_lowercase(), (pos, f.clone()));
                }
            }
        }
    }
    let mut classes: HashMap<Vec<u8>, _> = HashMap::new();
    let mut ambiguous = Vec::new();
    for c in unit.decls.classes.borrow().iter() {
        let mut methods = HashMap::new();
        for m in &c.methods {
            if let Some(pos) = entries.iter().position(|e| Rc::ptr_eq(&e.func, &m.func)) {
                methods.insert(m.name.as_bytes().to_ascii_lowercase(), pos);
            }
        }
        let lc = c.name.as_bytes().to_ascii_lowercase();
        if classes.contains_key(&lc) {
            ambiguous.push(lc.clone());
        }
        classes.entry(lc).or_insert_with(|| (c.clone(), methods));
    }
    let res = lower::Resolver {
        table: functions,
        functions: fn_index,
        classes,
        internal,
        level,
        bound: lower::early_bound_classes(&unit.main),
        ambiguous,
        funcs: entries.iter().map(|e| e.func.clone()).collect(),
    };
    let done: Vec<Done> = entries.iter().map(|e| finish(e, &res)).collect();
    let (mut main, mut rest) = (Vec::new(), Vec::new());
    for (e, d) in entries.iter().zip(&done) {
        let out = if e.is_main { &mut main } else { &mut rest };
        write_entry(out, e, d, &done, msg, main_line_end);
    }
    (main, rest)
}

/// What the dump knows of an internal function: its flags and return type.
pub type InternalInfo<'a> = dyn Fn(&[u8]) -> (u32, Option<crate::types::TypeDecl>) + 'a;

thread_local! {
    /// The dumps of the files compiled while preloading: each file's main
    /// code and its other op arrays apart ([`keep_preload_dump`]).
    static PRELOAD_DUMPS: std::cell::RefCell<Vec<(Vec<u8>, Vec<u8>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// [`dump_unit`] of a file compiled while preloading, kept for
/// [`flush_preload_dumps`].
pub fn keep_preload_dump(
    unit: &CompiledUnit,
    functions: &dyn FunctionTable,
    internal: &InternalInfo<'_>,
    level: u32,
    main_line_end: u32,
    msg: &str,
) {
    let parts = dump_unit_parts(unit, functions, internal, level, main_line_end, msg);
    PRELOAD_DUMPS.with(|d| d.borrow_mut().push(parts));
}

/// The dumps [`keep_preload_dump`] kept, in the order `accel_preload()`
/// optimizes them: the `$PRELOAD$` script (an empty main and the preloaded
/// functions and methods), then each preloaded file's main code. Empty when
/// none was kept.
#[must_use]
pub fn flush_preload_dumps(msg: &str) -> Vec<u8> {
    let kept = PRELOAD_DUMPS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    if kept.is_empty() {
        return Vec::new();
    }
    let mut out = format!(
        "\n$_main:\n     ; (lines=1, args=0, vars=0, tmps=0)\n     ; ({msg})\n     ; $PRELOAD$:0-0\n0000 RETURN null\n"
    )
    .into_bytes();
    for (_, rest) in &kept {
        out.extend_from_slice(rest);
    }
    for (main, _) in &kept {
        out.extend_from_slice(main);
    }
    out
}

/// `op_array->line_end` of a file's main code: the lexer's line at the end
/// of `src` (up to `__halt_compiler();` when there is one).
#[must_use]
pub fn main_line_end(src: &[u8], halt_offset: Option<usize>) -> u32 {
    let src = halt_offset.map_or(src, |o| &src[..o.min(src.len())]);
    let n = src.iter().filter(|&&c| c == b'\n').count();
    u32::try_from(n + 1).unwrap_or(u32::MAX)
}

/// Formats `op_array` in `zend_dump.c` style.
#[must_use]
pub fn dump_op_array(op_array: &OpArray, title: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; (lines={}, vars={}, tmps={})",
        op_array.ops.len(),
        op_array.cv_names.len(),
        op_array.num_tmps
    );
    if !title.is_empty() {
        let _ = writeln!(out, "; ({title})");
    }
    for (i, op) in op_array.ops.iter().enumerate() {
        let _ = write!(out, "{i:04} ");
        format_op(&mut out, op, op_array);
        let _ = writeln!(out);
    }
    out
}

fn format_operand(out: &mut String, op: &Operand, op_array: &OpArray) {
    match op {
        Operand::Cv(idx) => {
            if let Some(name) = op_array.cv_names.get(*idx as usize) {
                let _ = write!(
                    out,
                    "CV{idx}(${})",
                    String::from_utf8_lossy(name.as_bytes())
                );
            } else {
                let _ = write!(out, "CV{idx}");
            }
        }
        Operand::Tmp(t) => {
            let _ = write!(out, "T{t}");
        }
        Operand::TmpKeep(t) => {
            let _ = write!(out, "T{t}[keep]");
        }
        Operand::Lit(l) => {
            if let Some(val) = op_array.literals.get(*l as usize) {
                match val {
                    Value::Null => out.push_str("null"),
                    Value::Bool(b) => {
                        let _ = write!(out, "bool({b})");
                    }
                    Value::Int(i) => {
                        let _ = write!(out, "int({i})");
                    }
                    Value::Float(f) => {
                        let _ = write!(out, "float({f})");
                    }
                    Value::Str(s) => {
                        let _ =
                            write!(out, "string(\"{}\")", String::from_utf8_lossy(s.as_bytes()));
                    }
                    Value::Array(_) => out.push_str("array(...)"),
                    _ => {
                        let _ = write!(out, "lit({l})");
                    }
                }
            } else {
                let _ = write!(out, "lit({l})");
            }
        }
        Operand::Unused => out.push_str("unused"),
    }
}

#[allow(dead_code)]
fn format_class_spec(out: &mut String, class: &ClassSpec, op_array: &OpArray) {
    match class {
        ClassSpec::Named(l) => {
            if let Some(Value::Str(s)) = op_array.literals.get(*l as usize) {
                let _ = write!(out, "{}", String::from_utf8_lossy(s.as_bytes()));
            } else {
                let _ = write!(out, "class_lit({l})");
            }
        }
        ClassSpec::SelfClass => out.push_str("self"),
        ClassSpec::Parent => out.push_str("parent"),
        ClassSpec::Static => out.push_str("static"),
        ClassSpec::Dyn(op) => format_operand(out, op, op_array),
    }
}

fn format_op(out: &mut String, op: &Op, op_array: &OpArray) {
    match op {
        Op::Nop => out.push_str("NOP"),
        Op::Move { src, dst } => {
            let _ = write!(out, "T{dst} = QM_ASSIGN ");
            format_operand(out, src, op_array);
        }
        Op::Bin { op, a, b, dst } => {
            let _ = write!(out, "T{dst} = {:?} ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        Op::Unary { op, a, dst } => {
            let _ = write!(out, "T{dst} = {:?} ", op);
            format_operand(out, a, op_array);
        }
        Op::Cast { kind, a, dst } => {
            let _ = write!(out, "T{dst} = CAST({:?}) ", kind);
            format_operand(out, a, op_array);
        }
        Op::Jmp(t) => {
            let _ = write!(out, "JMP {t:04}");
        }
        Op::JmpZ(cond, t) => {
            out.push_str("JMPZ ");
            format_operand(out, cond, op_array);
            let _ = write!(out, " {t:04}");
        }
        Op::JmpNz(cond, t) => {
            out.push_str("JMPNZ ");
            format_operand(out, cond, op_array);
            let _ = write!(out, " {t:04}");
        }
        Op::JmpZEx { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMPZ_EX ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpNzEx { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMPNZ_EX ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpSet { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMP_SET ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::Coalesce { a, dst, target } => {
            let _ = write!(out, "T{dst} = COALESCE ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpNull { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMP_NULL ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::AssignCv { cv, val, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            if let Some(name) = op_array.cv_names.get(*cv as usize) {
                let _ = write!(
                    out,
                    "ASSIGN CV{cv}(${}) ",
                    String::from_utf8_lossy(name.as_bytes())
                );
            } else {
                let _ = write!(out, "ASSIGN CV{cv} ");
            }
            format_operand(out, val, op_array);
        }
        Op::Assign { place, val, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            let _ = write!(out, "ASSIGN place({place}) ");
            format_operand(out, val, op_array);
        }
        Op::Echo(op) => {
            out.push_str("ECHO ");
            format_operand(out, op, op_array);
        }
        Op::Return { val } => {
            out.push_str("RETURN ");
            format_operand(out, val, op_array);
        }
        Op::Free(op) => {
            out.push_str("FREE ");
            format_operand(out, op, op_array);
        }
        Op::Strlen { a, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            out.push_str("STRLEN ");
            format_operand(out, a, op_array);
        }
        Op::TypeCheck { a, mask, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            let _ = write!(out, "TYPE_CHECK(0x{mask:02x}) ");
            format_operand(out, a, op_array);
        }
        Op::CmpJmp {
            op,
            a,
            b,
            dst,
            target,
            jmp_if,
            ..
        } => {
            let _ = write!(out, "T{dst} = CMP_JMP({:?}, if={jmp_if}, {target:04}) ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        Op::BinAssignCv { op, a, b, dst, cv } => {
            let _ = write!(out, "T{dst} = BIN_ASSIGN_CV(CV{cv}, {:?}) ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
}
