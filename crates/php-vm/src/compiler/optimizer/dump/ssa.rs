//! The SSA dumps of the DFA pass.
//!
//! `opcache.opt_debug_level` with `ZEND_DUMP_BEFORE_DFA_PASS` (`0x200000`)
//! or `ZEND_DUMP_AFTER_DFA_PASS` (`0x400000`) prints each op array the way
//! `zend_dfa_optimize_op_array` (`Zend/Optimizer/dfa_pass.c`) does with
//! `zend_dump_op_array(op_array, ZEND_DUMP_SSA, ...)`
//! (`Zend/Optimizer/zend_dump.c`): the basic blocks with their dominator
//! tree (`zend_cfg.c`), the SSA variables and phi nodes of `zend_build_ssa`
//! (`zend_ssa.c`), and the types and ranges `zend_ssa_inference`
//! (`zend_inference.c`) infers for them.
//!
//! This is for printing only: it works on the lowered oplines of
//! [`super::lower`] and never changes executed code. The inference covers
//! the opcodes the `ext/opcache/tests/opt` dumps show; any other result is
//! "anything", and ranges are not widened in loops.
#![allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::many_single_char_names
)]

use super::lower::{Lower, Lowered, Resolver};
use super::model::{ZLine, Z};
use super::passes;
use super::{collect, stack_size, write_const, Entry};
use crate::class::ClassDecl;
use crate::compiler::optimizer::{PASS_6, PASS_7};
use crate::compiler::{CompiledUnit, FunctionTable};
use crate::func::flags;
use crate::types::TypeDecl;
use php_runtime::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

/// `ZEND_DUMP_BEFORE_DFA_PASS`.
pub const DUMP_BEFORE_DFA_PASS: i64 = 1 << 21;
/// `ZEND_DUMP_AFTER_DFA_PASS`.
pub const DUMP_AFTER_DFA_PASS: i64 = 1 << 22;

/// `ZEND_OPTIMIZER_IGNORE_OVERLOADING`.
const IGNORE_OVERLOADING: u32 = 1 << 16;

/// Type bits as `Zend/zend_type_info.h` lays them out (`MAY_BE_*`).
mod t {
    pub const UNDEF: u32 = 1 << 0;
    pub const NULL: u32 = 1 << 1;
    pub const FALSE: u32 = 1 << 2;
    pub const TRUE: u32 = 1 << 3;
    pub const LONG: u32 = 1 << 4;
    pub const DOUBLE: u32 = 1 << 5;
    pub const STRING: u32 = 1 << 6;
    pub const ARRAY: u32 = 1 << 7;
    pub const OBJECT: u32 = 1 << 8;
    pub const RESOURCE: u32 = 1 << 9;
    pub const REF: u32 = 1 << 10;
    pub const BOOL: u32 = FALSE | TRUE;
    pub const ANY: u32 = NULL | BOOL | LONG | DOUBLE | STRING | ARRAY | OBJECT | RESOURCE;
    /// `MAY_BE_ARRAY_SHIFT`.
    pub const OF_SHIFT: u32 = 10;
    pub const OF_ANY: u32 = ANY << OF_SHIFT;
    pub const OF_REF: u32 = REF << OF_SHIFT;
    pub const PACKED: u32 = 1 << 21;
    pub const NUMERIC_HASH: u32 = 1 << 22;
    pub const STRING_HASH: u32 = 1 << 23;
    pub const CLASS: u32 = 1 << 24;
    pub const INDIRECT: u32 = 1 << 25;
    pub const EMPTY: u32 = 1 << 29;
    pub const KEY_LONG: u32 = PACKED | NUMERIC_HASH;
    pub const KEY_STRING: u32 = STRING_HASH;
    pub const KEY_ANY: u32 = KEY_LONG | KEY_STRING | EMPTY;
    /// A value nothing is known about.
    pub const UNKNOWN: u32 = ANY | KEY_ANY | OF_ANY | OF_REF;
}

/// `zend_ssa_range`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Range {
    min: i64,
    max: i64,
    underflow: bool,
    overflow: bool,
}

impl Range {
    /// A range without overflow.
    fn new(min: i64, max: i64) -> Self {
        Range {
            min,
            max,
            underflow: false,
            overflow: false,
        }
    }

    /// The range of nothing known.
    const FULL: Range = Range {
        min: i64::MIN,
        max: i64::MAX,
        underflow: true,
        overflow: true,
    };
}

/// A class an SSA variable holds an object of (`ce`, `is_instanceof`).
#[derive(Clone, PartialEq, Eq, Debug)]
struct Ce {
    /// The declared name.
    name: Vec<u8>,
    instanceof: bool,
}

/// `zend_ssa_var_info` plus `zend_ssa_var.no_val`.
#[derive(Clone, Default, Debug)]
struct Info {
    ty: u32,
    ce: Option<Ce>,
    /// `has_range` and `range`.
    range: Option<Range>,
    no_val: bool,
}

/// `zend_ssa_op`.
#[derive(Clone, Copy, Default, Debug)]
struct SsaOp {
    op1_use: Option<u32>,
    op2_use: Option<u32>,
    result_use: Option<u32>,
    op1_def: Option<u32>,
    op2_def: Option<u32>,
    result_def: Option<u32>,
}

/// A phi node (`zend_ssa_phi` without pi constraints).
#[derive(Clone, Debug)]
struct Phi {
    /// The variable slot (a CV, or `ncv + T`).
    var: usize,
    ssa: u32,
    /// By predecessor.
    sources: Vec<Option<u32>>,
}

/// The SSA form of one op array.
struct Ssa {
    ops: Vec<SsaOp>,
    /// By block.
    phis: Vec<Vec<Phi>>,
    /// `vars_count`.
    count: u32,
}

/// Where SSA numbers are handed out while renaming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Site {
    /// The phi of block `.0` for variable slot `.1`.
    Phi(usize, usize),
    /// A definition of variable slot `.1` by opline `.0`.
    Op(usize, usize),
}

/// The definition of an SSA variable.
#[derive(Clone, Copy, Debug)]
enum DefAt {
    None,
    Op(usize),
    /// Block, index among its phis.
    Phi(usize, usize),
}

const BB_START: u32 = 1 << 0;
const BB_FOLLOW: u32 = 1 << 1;
const BB_TARGET: u32 = 1 << 2;
const BB_EXIT: u32 = 1 << 3;
const BB_REACHABLE: u32 = 1 << 4;

/// `zend_basic_block`.
#[derive(Debug)]
struct Block {
    start: usize,
    len: usize,
    succ: Vec<usize>,
    preds: Vec<usize>,
    flags: u32,
    idom: Option<usize>,
    level: Option<u32>,
    children: Vec<usize>,
}

/// `zend_cfg`.
struct Cfg {
    blocks: Vec<Block>,
    /// The block of each opline.
    block_of: Vec<usize>,
    /// `ZEND_FUNC_NO_LOOPS`.
    no_loops: bool,
}

/// The op array being dumped.
struct Func<'a, 'r> {
    e: &'a Entry,
    res: &'a Resolver<'r>,
    /// `last_var`.
    ncv: usize,
    /// `last_var + T`.
    nvars: usize,
    ret_ref: bool,
}

impl Func<'_, '_> {
    /// The variable slot of an operand.
    fn var(&self, z: &Z) -> Option<usize> {
        let v = match z {
            Z::Cv(c) => *c as usize,
            Z::Tmp(t) => self.ncv + *t as usize,
            _ => return None,
        };
        (v < self.nvars).then_some(v)
    }

    /// The class whose method this is (`op_array->scope`).
    fn scope(&self) -> Option<Rc<ClassDecl>> {
        let lc = self.e.scope.as_ref()?;
        self.res.classes.get(lc).map(|(c, _)| c.clone())
    }
}

/// Dumps the op arrays of `unit` as `zend_dfa_optimize_op_array` does for
/// the `ZEND_DUMP_*_DFA_PASS` bits of `debug` (`opcache.opt_debug_level`);
/// the other arguments are those of [`super::dump_unit`]. php-src runs the
/// DFA pass only when passes 6 and 7 are both enabled.
#[must_use]
pub fn dump_unit_dfa(
    unit: &CompiledUnit,
    functions: &dyn FunctionTable,
    internal: &dyn Fn(&[u8]) -> (u32, Option<TypeDecl>),
    level: u32,
    main_line_end: u32,
    debug: i64,
) -> Vec<u8> {
    let mut out = Vec::new();
    if debug & (DUMP_BEFORE_DFA_PASS | DUMP_AFTER_DFA_PASS) == 0
        || level & (PASS_6 | PASS_7) != (PASS_6 | PASS_7)
    {
        return out;
    }
    let entries = collect(unit);
    let res = resolver(unit, &entries, functions, internal, level);
    for e in &entries {
        dump_entry(&mut out, e, &res, debug, main_line_end);
    }
    out
}

/// The [`Resolver`] of [`super::dump_unit`] for `entries`.
fn resolver<'a>(
    unit: &CompiledUnit,
    entries: &[Entry],
    functions: &'a dyn FunctionTable,
    internal: &'a dyn Fn(&[u8]) -> (u32, Option<TypeDecl>),
    level: u32,
) -> Resolver<'a> {
    let mut fn_index = HashMap::new();
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
    // As the after-optimizer dump builds it (`dump::dump_unit`).
    Resolver {
        table: functions,
        functions: fn_index,
        classes,
        internal,
        level,
        bound: super::lower::early_bound_classes(&unit.main),
        ambiguous,
        funcs: entries.iter().map(|e| e.func.clone()).collect(),
    }
}

/// `zend_dfa_analyze_op_array` and the dumps of `zend_dfa_optimize_op_array`
/// for one op array.
fn dump_entry(out: &mut Vec<u8>, e: &Entry, res: &Resolver<'_>, debug: i64, main_line_end: u32) {
    let Lowered { mut lines, first_z } =
        Lower::new(&e.func, e.scope.clone(), &e.dyn_defs, res).run();
    passes::resolve_labels(&mut lines, &first_z);
    // No SSA for code with try/catch or indirect variable access.
    if !e.func.code.try_table.is_empty() || !passes::dfa_allowed(&lines) {
        return;
    }
    let shape = shape_of(e);
    let types = passes::infer(&lines, &shape);
    let pre_raw = lines.clone();
    if res.level & PASS_6 != 0 && !e.is_main {
        passes::dfa(&mut lines, &types, &shape);
    }
    let kept =
        |ls: &[ZLine]| -> Vec<usize> { (0..ls.len()).filter(|&i| !ls[i].is_nop()).collect() };
    let pre_kept = kept(&pre_raw[..]);
    let post_kept = kept(&lines[..]);
    let (mut pre, _) = passes::remove_nops(pre_raw);
    let (mut post, _) = passes::remove_nops(lines);
    let tmps = number(&mut pre, &mut post);
    let ncv = e.func.code.cv_names.len();
    let f = Func {
        e,
        res,
        ncv,
        nvars: ncv + tmps as usize,
        ret_ref: e.func.flags & flags::RETURN_REF != 0,
    };
    let pre_cfg = build_cfg(&pre);
    // `zend_build_ssa` gives up on very big functions.
    if pre_cfg.blocks.len().saturating_mul(f.nvars) > 4 * 1024 * 1024 {
        return;
    }
    let mut phis = place_phis(&f, &pre, &pre_cfg);
    let mut defs: Vec<(Site, u32)> = Vec::new();
    let mut next = u32::try_from(ncv).unwrap_or(0);
    let ops = rename(&f, &pre, &pre_cfg, &mut phis, &mut |s: Site| {
        let v = next;
        next += 1;
        defs.push((s, v));
        v
    });
    let pre_ssa = Ssa {
        ops,
        phis,
        count: next,
    };
    let an = analyze(&f, &pre, &pre_cfg, &pre_ssa);
    if debug & DUMP_BEFORE_DFA_PASS != 0 {
        write_op_array(
            out,
            &f,
            &pre,
            &pre_cfg,
            &pre_ssa,
            &an,
            "before dfa pass",
            main_line_end,
            tmps,
        );
    }
    if debug & DUMP_AFTER_DFA_PASS != 0 {
        let post_cfg = build_cfg(&post);
        let post_ssa = remap(
            &f, &pre_cfg, &pre_ssa, &defs, &pre_kept, &post, &post_cfg, &post_kept,
        );
        write_op_array(
            out,
            &f,
            &post,
            &post_cfg,
            &post_ssa,
            &an,
            "after dfa pass",
            main_line_end,
            tmps,
        );
    }
}

/// The [`passes::FuncShape`] of `e` (as `finish` builds it).
fn shape_of(e: &Entry) -> passes::FuncShape {
    passes::FuncShape {
        is_main: e.is_main,
        ncv: e.func.code.cv_names.len(),
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
    }
}

/// Gives the temporaries of both forms the numbers `zend_compile` gave them
/// (pass 9 has not run yet) and marks `IS_VAR` operands; returns
/// `op_array->T`.
fn number(pre: &mut [ZLine], post: &mut [ZLine]) -> u32 {
    let mut numbered = pre.to_vec();
    let tmps = passes::number_tmps(&mut numbered);
    let mut map: HashMap<u32, u32> = HashMap::new();
    for (a, b) in pre.iter().zip(&numbered) {
        for (x, y) in [(&a.res, &b.res), (&a.op1, &b.op1), (&a.op2, &b.op2)] {
            if let (Z::Tmp(p), Z::Tmp(q)) = (x, y) {
                map.insert(*p, *q);
            }
        }
    }
    let mut is_var: HashMap<u32, bool> = HashMap::new();
    for l in pre.iter() {
        if let Some(t) = l.res.tmp() {
            is_var.insert(t, l.res_var);
        }
        if l.op == "FE_FETCH_RW" {
            if let Some(t) = l.op2.tmp() {
                is_var.insert(t, true);
            }
        }
    }
    let var_of = |z: &Z| {
        z.tmp()
            .is_some_and(|t| is_var.get(&t).copied().unwrap_or(false))
    };
    for l in pre.iter_mut().chain(post.iter_mut()) {
        l.op1_var = var_of(&l.op1);
        l.op2_var = var_of(&l.op2);
        for z in [&mut l.res, &mut l.op1, &mut l.op2] {
            if let Z::Tmp(t) = z {
                if let Some(&n) = map.get(t) {
                    *t = n;
                }
            }
        }
    }
    tmps
}

/// Opcodes after which control does not fall through.
fn ends_flow(op: &str) -> bool {
    matches!(
        op,
        "JMP"
            | "RETURN"
            | "RETURN_BY_REF"
            | "GENERATOR_RETURN"
            | "THROW"
            | "EXIT"
            | "MATCH_ERROR"
            | "MATCH"
            | "FAST_RET"
            | "VERIFY_NEVER_TYPE"
    )
}

/// The jump targets of an opline, in successor order.
fn jump_targets(l: &ZLine) -> Vec<usize> {
    l.targets()
        .filter_map(|z| match z {
            Z::ZLabel(t) => Some(*t as usize),
            _ => None,
        })
        .collect()
}

/// `zend_build_cfg`, `zend_cfg_build_predecessors`,
/// `zend_cfg_compute_dominators_tree` and the loop check of
/// `zend_cfg_identify_loops`.
fn build_cfg(lines: &[ZLine]) -> Cfg {
    let n = lines.len();
    let mut starts = vec![false; n + 1];
    starts[0] = true;
    for (i, l) in lines.iter().enumerate() {
        let targets = jump_targets(l);
        if !targets.is_empty() || ends_flow(l.op) {
            for t in targets {
                if t < n {
                    starts[t] = true;
                }
            }
            starts[i + 1] = true;
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut block_of = vec![0; n];
    for (i, &start) in starts.iter().take(n).enumerate() {
        if start || blocks.is_empty() {
            blocks.push(Block {
                start: i,
                len: 0,
                succ: Vec::new(),
                preds: Vec::new(),
                flags: 0,
                idom: None,
                level: None,
                children: Vec::new(),
            });
        }
        let b = blocks.len() - 1;
        block_of[i] = b;
        blocks[b].len += 1;
    }
    let nb = blocks.len();
    for (b, block) in blocks.iter_mut().enumerate() {
        let l = &lines[block.start + block.len - 1];
        let mut succ: Vec<usize> = jump_targets(l)
            .into_iter()
            .filter(|&t| t < n)
            .map(|t| block_of[t])
            .collect();
        if !ends_flow(l.op) && b + 1 < nb {
            succ.push(b + 1);
        }
        block.succ = succ;
    }
    // `zend_mark_reachable`.
    if nb > 0 {
        blocks[0].flags = BB_START;
    }
    let mut seen = vec![false; nb];
    let mut work = Vec::new();
    if nb > 0 {
        seen[0] = true;
        work.push(0);
    }
    while let Some(b) = work.pop() {
        blocks[b].flags |= BB_REACHABLE;
        let succ = blocks[b].succ.clone();
        if succ.is_empty() {
            blocks[b].flags |= BB_EXIT;
            continue;
        }
        let op = lines[blocks[b].start + blocks[b].len - 1].op;
        let count = succ.len();
        for (i, &s) in succ.iter().enumerate() {
            let fl = if op == "MATCH" {
                BB_TARGET
            } else if op == "SWITCH_LONG" || op == "SWITCH_STRING" {
                if i == count - 1 {
                    BB_FOLLOW | BB_TARGET
                } else {
                    BB_TARGET
                }
            } else if count == 1 {
                if op == "JMP" {
                    BB_TARGET
                } else {
                    BB_FOLLOW
                }
            } else if i == 0 {
                BB_TARGET
            } else {
                BB_FOLLOW
            };
            blocks[s].flags |= fl;
            if !seen[s] {
                seen[s] = true;
                work.push(s);
            }
        }
    }
    for b in 0..nb {
        if blocks[b].flags & BB_REACHABLE == 0 {
            continue;
        }
        let succ = blocks[b].succ.clone();
        for (k, &s) in succ.iter().enumerate() {
            if !succ[..k].contains(&s) {
                blocks[s].preds.push(b);
            }
        }
    }
    // Dominators (Cooper, Harvey and Kennedy) over the reverse postorder.
    let mut order = Vec::new();
    let mut visited = vec![false; nb];
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if nb > 0 {
        visited[0] = true;
        stack.push((0, 0));
    }
    while let Some(top) = stack.last_mut() {
        let (b, i) = *top;
        if let Some(&s) = blocks[b].succ.get(i) {
            top.1 += 1;
            if !visited[s] {
                visited[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    let mut po = vec![usize::MAX; nb];
    for (k, &b) in order.iter().enumerate() {
        po[b] = k;
    }
    let mut idom: Vec<Option<usize>> = vec![None; nb];
    if nb > 0 {
        idom[0] = Some(0);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().rev() {
            if b == 0 {
                continue;
            }
            let mut new: Option<usize> = None;
            for &p in &blocks[b].preds {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(x) => intersect(&idom, &po, x, p),
                });
            }
            if new != idom[b] {
                idom[b] = new;
                changed = true;
            }
        }
    }
    for (b, block) in blocks.iter_mut().enumerate().skip(1) {
        block.idom = idom[b];
    }
    let parents: Vec<Option<usize>> = blocks.iter().map(|b| b.idom).collect();
    for (b, p) in parents.iter().enumerate() {
        if let Some(d) = *p {
            blocks[d].children.push(b);
        }
    }
    if nb > 0 {
        blocks[0].level = Some(0);
    }
    let mut st = if nb > 0 { vec![0usize] } else { Vec::new() };
    while let Some(b) = st.pop() {
        let lvl = blocks[b].level.unwrap_or(0) + 1;
        let children = blocks[b].children.clone();
        for c in children {
            blocks[c].level = Some(lvl);
            st.push(c);
        }
    }
    let dominates = |a: usize, mut b: usize| -> bool {
        loop {
            if a == b {
                return true;
            }
            match blocks[b].idom {
                Some(d) => b = d,
                None => return false,
            }
        }
    };
    let no_loops = !(0..nb).any(|b| {
        blocks[b].flags & BB_REACHABLE != 0 && blocks[b].succ.iter().any(|&s| dominates(s, b))
    });
    Cfg {
        blocks,
        block_of,
        no_loops,
    }
}

/// The common dominator of `a` and `b`.
fn intersect(idom: &[Option<usize>], po: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while po[a] < po[b] {
            a = idom[a].unwrap_or(0);
        }
        while po[b] < po[a] {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

/// Whether `zend_ssa_rename_op` gives the CV (or, for
/// `VERIFY_RETURN_TYPE`, any variable) in `op1` a new SSA variable.
fn op1_defines(l: &ZLine, ret_ref: bool) -> bool {
    let cv = matches!(l.op1, Z::Cv(_));
    match l.op {
        "ASSIGN"
        | "ASSIGN_REF"
        | "ASSIGN_DIM"
        | "ASSIGN_OBJ"
        | "ASSIGN_DIM_OP"
        | "ASSIGN_OBJ_OP"
        | "ASSIGN_OP"
        | "PRE_INC"
        | "PRE_DEC"
        | "POST_INC"
        | "POST_DEC"
        | "BIND_GLOBAL"
        | "BIND_STATIC"
        | "BIND_INIT_STATIC_OR_JMP"
        | "SEND_VAR_NO_REF"
        | "SEND_VAR_NO_REF_EX"
        | "SEND_VAR_EX"
        | "SEND_FUNC_ARG"
        | "SEND_REF"
        | "SEND_UNPACK"
        | "FE_RESET_RW"
        | "MAKE_REF"
        | "PRE_INC_OBJ"
        | "PRE_DEC_OBJ"
        | "POST_INC_OBJ"
        | "POST_DEC_OBJ"
        | "UNSET_DIM"
        | "UNSET_OBJ"
        | "FETCH_DIM_W"
        | "FETCH_DIM_RW"
        | "FETCH_DIM_FUNC_ARG"
        | "FETCH_DIM_UNSET"
        | "FETCH_LIST_W"
        | "UNSET_CV" => cv,
        "INIT_ARRAY" | "ADD_ARRAY_ELEMENT" => cv && l.ext.contains("(ref)"),
        "YIELD" => cv && ret_ref,
        "VERIFY_RETURN_TYPE" => matches!(l.op1, Z::Cv(_) | Z::Tmp(_)),
        _ => false,
    }
}

/// Whether `zend_ssa_rename_op` gives `op2` a new SSA variable.
fn op2_defines(l: &ZLine) -> bool {
    match l.op {
        "ASSIGN_REF" => matches!(l.op2, Z::Cv(_)),
        "FE_FETCH_R" | "FE_FETCH_RW" => matches!(l.op2, Z::Cv(_) | Z::Tmp(_)),
        "BIND_LEXICAL" => l.ext.contains("(ref)") && matches!(l.op2, Z::Cv(_)),
        _ => false,
    }
}

/// Opcodes whose `OP_DATA` operand `zend_ssa_rename_op` reads before the
/// opcode's own definitions.
fn reads_op_data_first(op: &str) -> bool {
    matches!(
        op,
        "ASSIGN_DIM"
            | "ASSIGN_OBJ"
            | "ASSIGN_OBJ_REF"
            | "ASSIGN_STATIC_PROP"
            | "ASSIGN_STATIC_PROP_REF"
            | "ASSIGN_STATIC_PROP_OP"
            | "FRAMELESS_ICALL_3"
    )
}

fn mark_use(v: Option<usize>, def: &[bool], uses: &mut [bool]) {
    if let Some(v) = v {
        if !def[v] {
            uses[v] = true;
        }
    }
}

/// `zend_build_dfg` and steps 1 and 2 of `zend_build_ssa`: the phi nodes
/// of each block, by variable slot.
fn place_phis(f: &Func<'_, '_>, lines: &[ZLine], cfg: &Cfg) -> Vec<Vec<Phi>> {
    let nb = cfg.blocks.len();
    let nv = f.nvars;
    let mut def = vec![vec![false; nv]; nb];
    let mut uses = vec![vec![false; nv]; nb];
    for (b, block) in cfg.blocks.iter().enumerate() {
        let (d, u) = (&mut def[b], &mut uses[b]);
        for l in &lines[block.start..block.start + block.len] {
            let v1 = f.var(&l.op1);
            let v2 = f.var(&l.op2);
            let vr = f.var(&l.res);
            // A definition of `op1` also reads it.
            mark_use(v1, d.as_slice(), u.as_mut_slice());
            if let Some(v) = v1 {
                if op1_defines(l, f.ret_ref) {
                    d[v] = true;
                }
            }
            if !(matches!(l.op, "FE_FETCH_R" | "FE_FETCH_RW") && !matches!(l.op2, Z::Cv(_))) {
                mark_use(v2, d.as_slice(), u.as_mut_slice());
            }
            if let Some(v) = v2 {
                if op2_defines(l) {
                    d[v] = true;
                }
            }
            if matches!(l.op, "ADD_ARRAY_ELEMENT" | "ADD_ARRAY_UNPACK") {
                mark_use(vr, d.as_slice(), u.as_mut_slice());
            }
            if let Some(v) = vr {
                d[v] = true;
            }
        }
    }
    let mut live_in = vec![vec![false; nv]; nb];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut out = vec![false; nv];
            for &s in &cfg.blocks[b].succ {
                for (o, &i) in out.iter_mut().zip(&live_in[s]) {
                    *o |= i;
                }
            }
            let new: Vec<bool> = (0..nv)
                .map(|v| uses[b][v] || (out[v] && !def[b][v]))
                .collect();
            if new != live_in[b] {
                live_in[b] = new;
                changed = true;
            }
        }
    }
    let mut phi = vec![vec![false; nv]; nb];
    loop {
        let mut changed = false;
        for j in 0..nb {
            let bj = &cfg.blocks[j];
            if bj.flags & BB_REACHABLE == 0 || bj.preds.len() < 2 {
                continue;
            }
            for &p in &bj.preds {
                let mut i = Some(p);
                while let Some(x) = i {
                    if Some(x) == bj.idom {
                        break;
                    }
                    for ((p, &d), &live) in phi[j].iter_mut().zip(&def[x]).zip(&live_in[j]) {
                        if d && live {
                            *p = true;
                        }
                    }
                    i = cfg.blocks[x].idom;
                }
            }
            for (d, &p) in def[j].iter_mut().zip(&phi[j]) {
                if p && !*d {
                    *d = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (0..nb)
        .map(|j| {
            if cfg.blocks[j].flags & BB_REACHABLE == 0 {
                return Vec::new();
            }
            (0..nv)
                .filter(|&v| phi[j][v])
                .map(|v| Phi {
                    var: v,
                    ssa: 0,
                    sources: vec![None; cfg.blocks[j].preds.len()],
                })
                .collect()
        })
        .collect()
}

/// `zend_ssa_rename`: walks the dominator tree in preorder, handing out
/// SSA numbers through `num`.
fn rename(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    cfg: &Cfg,
    phis: &mut [Vec<Phi>],
    num: &mut dyn FnMut(Site) -> u32,
) -> Vec<SsaOp> {
    let mut ops = vec![SsaOp::default(); lines.len()];
    if cfg.blocks.is_empty() {
        return ops;
    }
    let init: Vec<Option<u32>> = (0..f.nvars)
        .map(|v| {
            if v < f.ncv {
                u32::try_from(v).ok()
            } else {
                None
            }
        })
        .collect();
    let mut stack = vec![(0usize, init)];
    while let Some((b, mut var)) = stack.pop() {
        rename_block(f, lines, cfg, phis, &mut ops, &mut var, b, num);
        for &c in cfg.blocks[b].children.iter().rev() {
            stack.push((c, var.clone()));
        }
    }
    ops
}

/// `zend_ssa_rename_in_block`.
fn rename_block(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    cfg: &Cfg,
    phis: &mut [Vec<Phi>],
    ops: &mut [SsaOp],
    var: &mut [Option<u32>],
    b: usize,
    num: &mut dyn FnMut(Site) -> u32,
) {
    for p in &mut phis[b] {
        let s = num(Site::Phi(b, p.var));
        p.ssa = s;
        var[p.var] = Some(s);
    }
    let block = &cfg.blocks[b];
    for k in block.start..block.start + block.len {
        if lines[k].op != "OP_DATA" {
            rename_op(f, lines, k, ops, var, num);
        }
    }
    for &s in &block.succ {
        if let Some(j) = cfg.blocks[s].preds.iter().position(|&p| p == b) {
            for p in &mut phis[s] {
                if let Some(src) = p.sources.get_mut(j) {
                    *src = var[p.var];
                }
            }
        }
    }
}

/// The `OP_DATA` operand of opline `k`.
fn rename_op_data(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    k: usize,
    ops: &mut [SsaOp],
    var: &mut [Option<u32>],
    num: &mut dyn FnMut(Site) -> u32,
) {
    let Some(next) = lines.get(k + 1) else {
        return;
    };
    let vn = f.var(&next.op1);
    let Some(slot) = ops.get_mut(k + 1) else {
        return;
    };
    slot.op1_use = vn.and_then(|v| var[v]);
    if matches!(lines[k].op, "ASSIGN_OBJ_REF" | "ASSIGN_STATIC_PROP_REF")
        && matches!(next.op1, Z::Cv(_))
    {
        if let Some(v) = vn {
            let s = num(Site::Op(k + 1, v));
            slot.op1_def = Some(s);
            var[v] = Some(s);
        }
    }
}

/// `_zend_ssa_rename_op` (without `ZEND_SSA_RC_INFERENCE`).
fn rename_op(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    k: usize,
    ops: &mut [SsaOp],
    var: &mut [Option<u32>],
    num: &mut dyn FnMut(Site) -> u32,
) {
    let l = &lines[k];
    let v1 = f.var(&l.op1);
    let v2 = f.var(&l.op2);
    let vr = f.var(&l.res);
    let mut op = SsaOp {
        op1_use: v1.and_then(|v| var[v]),
        op2_use: v2.and_then(|v| var[v]),
        ..SsaOp::default()
    };
    if matches!(l.op, "ADD_ARRAY_ELEMENT" | "ADD_ARRAY_UNPACK") {
        op.result_use = vr.and_then(|v| var[v]);
    }
    if matches!(l.op, "FE_FETCH_R" | "FE_FETCH_RW") && !matches!(l.op2, Z::Cv(_)) {
        op.op2_use = None;
    }
    if reads_op_data_first(l.op) {
        rename_op_data(f, lines, k, ops, var, num);
    }
    if op2_defines(l) {
        if let Some(v) = v2 {
            let s = num(Site::Op(k, v));
            op.op2_def = Some(s);
            var[v] = Some(s);
        }
    }
    if op1_defines(l, f.ret_ref) {
        if let Some(v) = v1 {
            let s = num(Site::Op(k, v));
            op.op1_def = Some(s);
            var[v] = Some(s);
        }
    }
    if matches!(l.op, "ASSIGN_DIM_OP" | "ASSIGN_OBJ_OP") {
        rename_op_data(f, lines, k, ops, var, num);
    }
    if let Some(v) = vr {
        let s = num(Site::Op(k, v));
        op.result_def = Some(s);
        var[v] = Some(s);
    }
    ops[k] = op;
}

/// The SSA form after the DFA pass: php-src keeps the SSA variables of
/// the analysis while rewriting oplines, so every definition keeps the
/// number it had (a definition moved to an earlier opline of its block,
/// as a contracted assignment, takes the number of the one it replaces)
/// and removed ones leave gaps.
fn remap(
    f: &Func<'_, '_>,
    pre_cfg: &Cfg,
    pre_ssa: &Ssa,
    defs: &[(Site, u32)],
    pre_kept: &[usize],
    post: &[ZLine],
    post_cfg: &Cfg,
    post_kept: &[usize],
) -> Ssa {
    let pre_of_raw: HashMap<usize, usize> =
        pre_kept.iter().enumerate().map(|(i, &r)| (r, i)).collect();
    let pre_line =
        |q: usize| -> Option<usize> { post_kept.get(q).and_then(|r| pre_of_raw.get(r)).copied() };
    let pre_block = |b: usize| -> Option<usize> {
        let start = post_cfg.blocks.get(b)?.start;
        pre_line(start).and_then(|p| pre_cfg.block_of.get(p).copied())
    };
    let mut by_var: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, (s, _)) in defs.iter().enumerate() {
        let v = match *s {
            Site::Phi(_, v) | Site::Op(_, v) => v,
        };
        by_var.entry(v).or_default().push(i);
    }
    let mut phis: Vec<Vec<Phi>> = (0..post_cfg.blocks.len())
        .map(|b| {
            let n = post_cfg.blocks[b].preds.len();
            pre_block(b)
                .and_then(|pb| pre_ssa.phis.get(pb))
                .map(|ps| {
                    ps.iter()
                        .map(|p| Phi {
                            var: p.var,
                            ssa: 0,
                            sources: vec![None; n],
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    let mut claimed = vec![false; defs.len()];
    let mut fresh = pre_ssa.count;
    let ops = rename(f, post, post_cfg, &mut phis, &mut |s: Site| {
        let found = match s {
            Site::Phi(b, v) => pre_block(b).and_then(|pb| {
                by_var.get(&v).and_then(|ix| {
                    ix.iter()
                        .copied()
                        .find(|&i| !claimed[i] && defs[i].0 == Site::Phi(pb, v))
                })
            }),
            Site::Op(q, v) => pre_line(q).and_then(|p| {
                let pb = pre_cfg.block_of.get(p)?;
                let blk = pre_cfg.blocks.get(*pb)?;
                let end = blk.start + blk.len;
                by_var.get(&v).and_then(|ix| {
                    ix.iter().copied().find(|&i| {
                        !claimed[i] && matches!(defs[i].0, Site::Op(j, _) if j >= p && j < end)
                    })
                })
            }),
        };
        if let Some(i) = found {
            claimed[i] = true;
            defs[i].1
        } else {
            let s = fresh;
            fresh += 1;
            s
        }
    });
    Ssa {
        ops,
        phis,
        count: pre_ssa.count,
    }
}

/// The analysis results.
struct Analysis {
    vars: Vec<Info>,
    /// `func_info->return_info`: type, class and range.
    ret: (u32, Option<Ce>, Range),
}

/// `zend_ssa_is_no_val_use`.
fn is_no_val_use(l: &ZLine, op: &SsaOp, v: u32) -> bool {
    let v = Some(v);
    match l.op {
        "ASSIGN" | "UNSET_CV" | "BIND_GLOBAL" | "BIND_STATIC" => op.op1_use == v && op.op2_use != v,
        "FE_FETCH_R" | "FE_FETCH_RW" => op.op2_use == v && op.op1_use != v,
        "ADD_ARRAY_ELEMENT" | "ADD_ARRAY_UNPACK" => false,
        _ => op.result_use == v && op.op1_use != v && op.op2_use != v,
    }
}

/// `zend_ssa_find_false_dependencies`, `zend_infer_ranges`,
/// `zend_infer_types` and `zend_func_return_info`.
fn analyze(f: &Func<'_, '_>, lines: &[ZLine], cfg: &Cfg, ssa: &Ssa) -> Analysis {
    let n = ssa.count as usize;
    let mut def_at = vec![DefAt::None; n];
    for (k, op) in ssa.ops.iter().enumerate() {
        for d in [op.op1_def, op.op2_def, op.result_def]
            .into_iter()
            .flatten()
        {
            if let Some(slot) = def_at.get_mut(d as usize) {
                *slot = DefAt::Op(k);
            }
        }
    }
    for (b, ps) in ssa.phis.iter().enumerate() {
        for (i, p) in ps.iter().enumerate() {
            if let Some(slot) = def_at.get_mut(p.ssa as usize) {
                *slot = DefAt::Phi(b, i);
            }
        }
    }
    let mut vars = vec![Info::default(); n];
    for v in vars.iter_mut().take(f.ncv) {
        v.ty = if f.e.is_main {
            t::UNDEF | t::REF | t::UNKNOWN
        } else {
            t::UNDEF
        };
    }
    // `zend_ssa_find_false_dependencies` (functions only).
    if !f.e.is_main {
        let mut used = vec![false; n];
        for (k, op) in ssa.ops.iter().enumerate() {
            for u in [op.op1_use, op.op2_use, op.result_use]
                .into_iter()
                .flatten()
            {
                if !is_no_val_use(&lines[k], op, u) {
                    if let Some(x) = used.get_mut(u as usize) {
                        *x = true;
                    }
                }
            }
        }
        let mut work: Vec<usize> = (0..n).filter(|&v| used[v]).collect();
        while let Some(v) = work.pop() {
            if let DefAt::Phi(b, i) = def_at[v] {
                for &s in ssa.phis[b][i].sources.iter().flatten() {
                    let s = s as usize;
                    if s < n && !used[s] {
                        used[s] = true;
                        work.push(s);
                    }
                }
            }
        }
        for (info, &u) in vars.iter_mut().zip(&used) {
            info.no_val = !u;
        }
    }
    // Ranges: every variable of an SCC (not `no_val`) that is not a
    // reference gets one; without an inferable range it is unbounded.
    for _ in 0..32 {
        let mut changed = false;
        for v in 0..n {
            if vars[v].no_val || vars[v].ty & t::REF != 0 {
                continue;
            }
            let r = calc_range(f, lines, ssa, &vars, &def_at, v).unwrap_or(Range::FULL);
            if vars[v].range != Some(r) {
                vars[v].range = Some(r);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Types, to a fixed point.
    for _ in 0..32 {
        let mut changed = false;
        for v in 0..n {
            let (ty, ce) = match def_at[v] {
                DefAt::None => continue,
                DefAt::Phi(b, i) => phi_type(&ssa.phis[b][i], &vars),
                DefAt::Op(k) => op_type(f, lines, ssa, &vars, k, v),
            };
            let mut ty = ty;
            if ty & t::REF != 0 {
                ty |= t::UNKNOWN;
            }
            let ty = vars[v].ty | ty;
            if ty != vars[v].ty || vars[v].ce != ce {
                vars[v].ty = ty;
                vars[v].ce = ce;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let ret = return_info(f, lines, cfg, ssa, &vars);
    Analysis { vars, ret }
}

/// The range of an operand (`OP1_HAS_RANGE` and friends).
fn op_range(z: &Z, u: Option<u32>, vars: &[Info]) -> Option<Range> {
    match z {
        Z::Const(Value::Int(i)) => Some(Range::new(*i, *i)),
        Z::Cv(_) | Z::Tmp(_) => u.and_then(|u| vars.get(u as usize)).and_then(|x| x.range),
        _ => None,
    }
}

/// The type of an operand (`OP1_INFO()`).
fn op_type_of(z: &Z, u: Option<u32>, vars: &[Info]) -> u32 {
    match z {
        Z::Const(v) => const_type(v),
        Z::Cv(_) | Z::Tmp(_) => u
            .and_then(|u| vars.get(u as usize))
            .map_or(t::UNKNOWN | t::UNDEF, |x| x.ty),
        _ => 0,
    }
}

/// `_const_op_type`.
fn const_type(v: &Value) -> u32 {
    match v {
        Value::Null => t::NULL,
        Value::Bool(false) => t::FALSE,
        Value::Bool(true) => t::TRUE,
        Value::Int(_) => t::LONG,
        Value::Float(_) => t::DOUBLE,
        Value::Str(_) => t::STRING,
        Value::Array(a) => {
            // `zend_array_type_info`.
            let mut r = t::ARRAY;
            if a.is_empty() {
                return r | t::EMPTY;
            }
            let packed = a.is_packed();
            for (key, val) in a.iter() {
                if !packed {
                    r |= match key {
                        php_runtime::ArrayKey::Int(_) => t::NUMERIC_HASH,
                        php_runtime::ArrayKey::Str(_) => t::STRING_HASH,
                    };
                }
                r |= (const_type(val) & (t::ANY | t::REF)) << t::OF_SHIFT;
            }
            if packed {
                r |= t::PACKED;
            }
            r
        }
        Value::Object(_) => t::OBJECT,
        Value::Resource(_) => t::RESOURCE,
        Value::Reference(_) => t::UNKNOWN | t::REF,
    }
}

/// The name `ASSIGN_OP` prints in its extended value (` (ADD)`).
fn assign_op_name(ext: &str) -> Option<&'static str> {
    let name = ext.trim().strip_prefix('(')?.strip_suffix(')')?;
    [
        "ADD", "SUB", "MUL", "DIV", "MOD", "SL", "SR", "BW_OR", "BW_AND", "BW_XOR", "CONCAT", "POW",
    ]
    .into_iter()
    .find(|n| *n == name)
}

/// `zend_inference_calc_range` (not widening, narrowing).
fn calc_range(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    ssa: &Ssa,
    vars: &[Info],
    def_at: &[DefAt],
    v: usize,
) -> Option<Range> {
    match def_at[v] {
        DefAt::None => None,
        DefAt::Phi(b, i) => {
            let p = &ssa.phis[b][i];
            let mut r = Range {
                min: i64::MAX,
                max: i64::MIN,
                underflow: false,
                overflow: false,
            };
            for s in &p.sources {
                match s.and_then(|s| vars.get(s as usize)).and_then(|x| x.range) {
                    Some(sr) => {
                        r.underflow |= sr.underflow;
                        r.min = r.min.min(sr.min);
                        r.max = r.max.max(sr.max);
                        r.overflow |= sr.overflow;
                    }
                    None => r = Range::FULL,
                }
            }
            (r.min <= r.max).then_some(r)
        }
        DefAt::Op(k) => propagate_range(f, lines, ssa, vars, k, v),
    }
}

/// `zend_inference_propagate_range`.
fn propagate_range(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    ssa: &Ssa,
    vars: &[Info],
    k: usize,
    v: usize,
) -> Option<Range> {
    let l = &lines[k];
    let op = &ssa.ops[k];
    let v = Some(u32::try_from(v).ok()?);
    let r1 = || op_range(&l.op1, op.op1_use, vars);
    let r2 = || op_range(&l.op2, op.op2_use, vars);
    match l.op {
        "ADD" | "SUB" | "MUL" | "MOD" | "BW_OR" | "BW_AND" if op.result_def == v => {
            binary_range(l.op, r1()?, r2()?)
        }
        "ASSIGN_OP" if op.op1_def == v || op.result_def == v => {
            let name = assign_op_name(&l.ext).filter(|n| *n != "CONCAT" && *n != "POW")?;
            binary_range(name, r1()?, r2()?)
        }
        "BW_NOT" if op.result_def == v => {
            let a = r1()?;
            Some(if a.underflow || a.overflow {
                Range::new(i64::MIN, i64::MAX)
            } else {
                Range::new(!a.max, !a.min)
            })
        }
        "CAST" if op.result_def == v && l.ext == " (long)" => {
            Some(r1().map_or(Range::new(i64::MIN, i64::MAX), |a| Range::new(a.min, a.max)))
        }
        "QM_ASSIGN" | "JMP_SET" | "COALESCE" | "COPY_TMP" if op.result_def == v => r1(),
        "ASSIGN" if op.op1_def == v || op.op2_def == v || op.result_def == v => {
            let op1_ref = op
                .op1_use
                .and_then(|u| vars.get(u as usize))
                .is_some_and(|x| x.ty & t::REF != 0);
            if op.op2_def == v || (matches!(l.op1, Z::Cv(_)) && !op1_ref) {
                r2()
            } else {
                None
            }
        }
        "RECV" | "RECV_INIT" if op.result_def == v => {
            use crate::types::mask;
            let Z::Num(n) = l.op1 else {
                return None;
            };
            let p = f.e.func.params.get((n as usize).checked_sub(1)?)?;
            let ty = p.ty.as_ref()?;
            // `ZEND_TYPE_PURE_MASK_WITHOUT_NULL(type) == MAY_BE_LONG`.
            (ty.mask & !mask::NULL == mask::LONG).then_some(Range::new(i64::MIN, i64::MAX))
        }
        "STRLEN" if op.result_def == v => Some(Range::new(0, i64::MAX)),
        "FUNC_NUM_ARGS" => Some(Range::new(0, i64::MAX)),
        "SPACESHIP" => Some(Range::new(-1, 1)),
        "COUNT" => Some(Range::new(i64::MIN, i64::MAX)),
        _ => None,
    }
}

/// `zend_inference_calc_binary_op_range`.
fn binary_range(op: &str, a: Range, b: Range) -> Option<Range> {
    let mut r = Range::new(0, 0);
    match op {
        "ADD" => {
            match a.min.checked_add(b.min) {
                Some(m) if !a.underflow && !b.underflow => r.min = m,
                _ => {
                    r.underflow = true;
                    r.min = i64::MIN;
                }
            }
            match a.max.checked_add(b.max) {
                Some(m) if !a.overflow && !b.overflow => r.max = m,
                _ => {
                    r.overflow = true;
                    r.max = i64::MAX;
                }
            }
        }
        "SUB" => {
            match a.min.checked_sub(b.max) {
                Some(m) if !a.underflow && !b.overflow => r.min = m,
                _ => {
                    r.underflow = true;
                    r.min = i64::MIN;
                }
            }
            match a.max.checked_sub(b.min) {
                Some(m) if !a.overflow && !b.underflow => r.max = m,
                _ => {
                    r.overflow = true;
                    r.max = i64::MAX;
                }
            }
        }
        "MUL" => {
            let products = [
                a.min.checked_mul(b.min),
                a.min.checked_mul(b.max),
                a.max.checked_mul(b.min),
                a.max.checked_mul(b.max),
            ];
            if a.underflow
                || b.underflow
                || a.overflow
                || b.overflow
                || products.iter().any(Option::is_none)
            {
                r = Range::FULL;
            } else {
                let p: Vec<i64> = products.into_iter().flatten().collect();
                r.min = p.iter().copied().min().unwrap_or(i64::MIN);
                r.max = p.iter().copied().max().unwrap_or(i64::MAX);
            }
        }
        "MOD" => {
            if a.underflow || b.underflow || a.overflow || b.overflow {
                r = Range::new(i64::MIN, i64::MAX);
            } else {
                let (abs_min, abs_max) = abs_range(b.min, b.max)?;
                if abs_max == 0 {
                    return None;
                }
                let abs_min = if abs_min == 0 { 1 } else { abs_min };
                if a.min >= 0 {
                    r.min = if a.max < abs_min { a.min } else { 0 };
                    r.max = a.max.min(abs_max - 1);
                } else if a.max <= 0 {
                    r.min = a.min.max(-abs_max + 1);
                    r.max = if a.min > -abs_min { a.max } else { 0 };
                } else {
                    r.min = a.min.max(-abs_max + 1);
                    r.max = a.max.min(abs_max - 1);
                }
            }
        }
        "BW_OR" | "BW_AND" => {
            if a.underflow || b.underflow || a.overflow || b.overflow {
                r = Range::new(i64::MIN, i64::MAX);
            } else {
                let (min, max) = if op == "BW_OR" {
                    range_or(a.min, a.max, b.min, b.max)
                } else {
                    range_and(a.min, a.max, b.min, b.max)
                };
                r = Range::new(min, max);
            }
        }
        _ => return None,
    }
    Some(r)
}

/// `zend_abs_range`.
fn abs_range(min: i64, max: i64) -> Option<(i64, i64)> {
    if min == i64::MIN {
        None
    } else if min >= 0 {
        Some((min, max))
    } else if max <= 0 {
        Some((-max, -min))
    } else {
        Some((0, max.max(-min)))
    }
}

fn min_or(mut a: u64, b: u64, mut c: u64, d: u64) -> u64 {
    let mut m: u64 = 1 << 63;
    while m != 0 {
        if (!a & c & m) != 0 {
            let temp = (a | m) & m.wrapping_neg();
            if temp <= b {
                a = temp;
                break;
            }
        } else if (a & !c & m) != 0 {
            let temp = (c | m) & m.wrapping_neg();
            if temp <= d {
                c = temp;
                break;
            }
        }
        m >>= 1;
    }
    a | c
}

fn max_or(a: u64, mut b: u64, c: u64, mut d: u64) -> u64 {
    let mut m: u64 = 1 << 63;
    while m != 0 {
        if (b & d & m) != 0 {
            let temp = (b - m) | (m - 1);
            if temp >= a {
                b = temp;
                break;
            }
            let temp = (d - m) | (m - 1);
            if temp >= c {
                d = temp;
                break;
            }
        }
        m >>= 1;
    }
    b | d
}

fn min_and(mut a: u64, b: u64, mut c: u64, d: u64) -> u64 {
    let mut m: u64 = 1 << 63;
    while m != 0 {
        if (!a & !c & m) != 0 {
            let temp = (a | m) & m.wrapping_neg();
            if temp <= b {
                a = temp;
                break;
            }
            let temp = (c | m) & m.wrapping_neg();
            if temp <= d {
                c = temp;
                break;
            }
        }
        m >>= 1;
    }
    a & c
}

fn max_and(a: u64, mut b: u64, c: u64, mut d: u64) -> u64 {
    let mut m: u64 = 1 << 63;
    while m != 0 {
        if (b & !d & m) != 0 {
            // As php-src writes it (`b | ~m`).
            let temp = (b | !m) | (m - 1);
            if temp >= a {
                b = temp;
                break;
            }
        } else if (!b & d & m) != 0 {
            let temp = (d | !m) | (m - 1);
            if temp >= c {
                d = temp;
                break;
            }
        }
        m >>= 1;
    }
    b & d
}

/// The four-argument signedness case of `zend_ssa_range_or` /
/// `zend_ssa_range_and`.
fn sign_case(a: i64, b: i64, c: i64, d: i64) -> u8 {
    (u8::from(a < 0) << 3) | (u8::from(b < 0) << 2) | (u8::from(c < 0) << 1) | u8::from(d < 0)
}

/// `zend_ssa_range_or` (after "Hacker's Delight").
fn range_or(a: i64, b: i64, c: i64, d: i64) -> (i64, i64) {
    let u = i64::cast_unsigned;
    let s = u64::cast_signed;
    match sign_case(a, b, c, d) {
        0x0 | 0x3 | 0xc | 0xf => (
            s(min_or(u(a), u(b), u(c), u(d))),
            s(max_or(u(a), u(b), u(c), u(d))),
        ),
        0x2 => (
            s(min_or(u(a), u(b), u(c), u(-1))),
            s(max_or(u(a), u(b), 0, u(d))),
        ),
        0x8 => (
            s(min_or(u(a), u(-1), u(c), u(d))),
            s(max_or(0, u(b), u(c), u(d))),
        ),
        0xa => (a.min(c), s(max_or(0, u(b), 0, u(d)))),
        0xb => (c, -1),
        0xe => (a, -1),
        _ => (i64::MIN, i64::MAX),
    }
}

/// `zend_ssa_range_and`.
fn range_and(a: i64, b: i64, c: i64, d: i64) -> (i64, i64) {
    let u = i64::cast_unsigned;
    let s = u64::cast_signed;
    match sign_case(a, b, c, d) {
        0x0 | 0x3 | 0xc | 0xf => (
            s(min_and(u(a), u(b), u(c), u(d))),
            s(max_and(u(a), u(b), u(c), u(d))),
        ),
        0x2 => (0, b),
        0x8 => (0, d),
        0xa => (s(min_and(u(a), u(-1), u(c), u(-1))), b.max(d)),
        0xb => (
            s(min_and(u(a), u(-1), u(c), u(d))),
            s(max_and(0, u(b), u(c), u(d))),
        ),
        0xe => (
            s(min_and(u(a), u(b), u(c), u(-1))),
            s(max_and(u(a), u(b), 0, u(d))),
        ),
        _ => (i64::MIN, i64::MAX),
    }
}

/// The type of a phi: the union of its sources, with their class when
/// they agree.
fn phi_type(p: &Phi, vars: &[Info]) -> (u32, Option<Ce>) {
    let mut ty = 0;
    let mut ce: Option<Option<Ce>> = None;
    for s in p.sources.iter().flatten() {
        let Some(info) = vars.get(*s as usize) else {
            continue;
        };
        ty |= info.ty;
        if info.ty & t::OBJECT != 0 {
            ce = Some(match (ce, &info.ce) {
                (None, c) => c.clone(),
                (Some(Some(a)), Some(b)) if a.name == b.name => Some(Ce {
                    name: a.name,
                    instanceof: a.instanceof || b.instanceof,
                }),
                _ => None,
            });
        }
    }
    (ty, ce.flatten())
}

/// `binary_op_result_type`.
fn binary_type(op: &str, t1: u32, t2: u32, result: Option<Range>, level: u32) -> u32 {
    let ty = |x: u32| (x & t::ANY) | if x & t::UNDEF != 0 { t::NULL } else { 0 };
    let (a, b) = (ty(t1), ty(t2));
    let mut tmp = 0;
    if level & IGNORE_OVERLOADING == 0 && (a | b) & t::OBJECT != 0 {
        tmp |= t::OBJECT | t::FALSE;
    }
    let exact = result.is_some_and(|r| !r.underflow && !r.overflow);
    let arrays = t::KEY_ANY | t::OF_ANY | t::OF_REF;
    match op {
        "ADD" => {
            if a == t::LONG && b == t::LONG {
                tmp |= if exact { t::LONG } else { t::LONG | t::DOUBLE };
            } else if a == t::DOUBLE || b == t::DOUBLE {
                tmp |= t::DOUBLE;
            } else if a == t::ARRAY && b == t::ARRAY {
                tmp |= t::ARRAY | (t1 & arrays) | (t2 & arrays);
            } else {
                tmp |= t::LONG | t::DOUBLE;
                if a & t::ARRAY != 0 && b & t::ARRAY != 0 {
                    tmp |= t::ARRAY | (t1 & arrays) | (t2 & arrays);
                }
            }
        }
        "SUB" | "MUL" => {
            if a == t::LONG && b == t::LONG {
                tmp |= if exact { t::LONG } else { t::LONG | t::DOUBLE };
            } else if a == t::DOUBLE || b == t::DOUBLE {
                tmp |= t::DOUBLE;
            } else {
                tmp |= t::LONG | t::DOUBLE;
            }
        }
        "DIV" | "POW" => {
            tmp |= if a == t::DOUBLE || b == t::DOUBLE {
                t::DOUBLE
            } else {
                t::LONG | t::DOUBLE
            };
        }
        "MOD" | "SL" | "SR" => tmp |= t::LONG,
        "BW_OR" | "BW_AND" | "BW_XOR" => {
            if a & t::STRING != 0 && b & t::STRING != 0 {
                tmp |= t::STRING;
            }
            if a & !t::STRING != 0 || b & !t::STRING != 0 {
                tmp |= t::LONG;
            }
        }
        "CONCAT" | "FAST_CONCAT" => tmp = t::STRING,
        _ => tmp = t::UNKNOWN,
    }
    tmp
}

/// The script class named `name`.
fn class_by_name(res: &Resolver<'_>, name: &[u8]) -> Option<Rc<ClassDecl>> {
    let name = name.strip_prefix(b"\\").unwrap_or(name);
    res.classes
        .get(&name.to_ascii_lowercase())
        .map(|(c, _)| c.clone())
}

/// `zend_convert_type`: the type bits and class of a declared type.
fn convert_type(res: &Resolver<'_>, ty: Option<&TypeDecl>) -> (u32, Option<Ce>) {
    use crate::types::mask as m;
    let Some(ty) = ty else {
        return (t::UNKNOWN, None);
    };
    let mut r = 0;
    for (bit, out) in [
        (m::NULL, t::NULL),
        (m::FALSE, t::FALSE),
        (m::TRUE, t::TRUE),
        (m::LONG, t::LONG),
        (m::DOUBLE, t::DOUBLE),
        (m::STRING, t::STRING),
        (m::ARRAY, t::ARRAY),
        (m::OBJECT, t::OBJECT),
        (m::RESOURCE, t::RESOURCE),
        (m::VOID, t::NULL),
        (m::CALLABLE, t::STRING | t::ARRAY | t::OBJECT),
        (m::STATIC, t::OBJECT),
        (m::ITERABLE, t::ARRAY | t::OBJECT),
    ] {
        if ty.mask & bit != 0 {
            r |= out;
        }
    }
    let mut ce = None;
    if !ty.classes.is_empty() || !ty.intersections.is_empty() {
        r |= t::OBJECT;
        // One class name (`ZEND_TYPE_HAS_NAME`): its entry.
        if let [name] = ty.classes.as_slice() {
            if ty.intersections.is_empty() {
                ce = class_by_name(res, name.as_bytes()).map(|c| Ce {
                    name: c.name.as_bytes().to_vec(),
                    instanceof: true,
                });
            }
        }
    }
    if r & t::ARRAY != 0 {
        r |= t::KEY_ANY | t::OF_ANY | t::OF_REF;
    }
    (r, ce)
}

/// The class chain of `ce`: itself, then its ancestors.
fn class_chain(res: &Resolver<'_>, ce: &Rc<ClassDecl>) -> Vec<Rc<ClassDecl>> {
    let mut out = vec![ce.clone()];
    let mut cur = ce.clone();
    while let Some(p) = cur
        .parent
        .as_ref()
        .and_then(|n| class_by_name(res, n.as_bytes()))
    {
        if out.iter().any(|c| Rc::ptr_eq(c, &p)) || out.len() > 64 {
            break;
        }
        out.push(p.clone());
        cur = p;
    }
    out
}

/// `zend_get_property_info` with `EG(fake_scope)` set to `scope` (the
/// classes are linked): the declaration of property `name` that code in
/// `scope` sees on an instance of `ce`, as its class and index.
fn lookup_prop(
    res: &Resolver<'_>,
    ce: &Rc<ClassDecl>,
    name: &[u8],
    scope: Option<&Rc<ClassDecl>>,
) -> Option<(Rc<ClassDecl>, usize)> {
    let chain = class_chain(res, ce);
    let decls: Vec<(usize, usize)> = chain
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            c.props
                .iter()
                .position(|p| p.name.as_bytes() == name)
                .map(|j| (i, j))
        })
        .collect();
    let &(ci, pi) = decls.first()?;
    let owner = &chain[ci];
    let fl = owner.props[pi].flags;
    let found = Some((owner.clone(), pi));
    // `ZEND_ACC_CHANGED`: it shadows a private property of an ancestor.
    let changed = decls
        .iter()
        .skip(1)
        .any(|&(i, j)| chain[i].props[j].flags & flags::PRIVATE != 0);
    if fl & (flags::PRIVATE | flags::PROTECTED) == 0 && !changed {
        return found;
    }
    if scope.is_some_and(|s| Rc::ptr_eq(s, owner)) {
        return found;
    }
    if changed {
        // `zend_get_parent_private_property`.
        if let Some(s) = scope {
            if !Rc::ptr_eq(s, ce) && chain.iter().skip(1).any(|c| Rc::ptr_eq(c, s)) {
                if let Some(j) = s
                    .props
                    .iter()
                    .position(|p| p.name.as_bytes() == name && p.flags & flags::PRIVATE != 0)
                {
                    if s.props[j].flags & flags::STATIC == 0 || fl & flags::STATIC != 0 {
                        return Some((s.clone(), j));
                    }
                }
            }
        }
        if fl & (flags::PRIVATE | flags::PROTECTED) == 0 {
            return found;
        }
    }
    if fl & flags::PRIVATE != 0 {
        return None;
    }
    // Protected: `is_protected_compatible_scope` of the first declaration.
    let proto = &chain[decls.last()?.0];
    let s = scope?;
    let related = class_chain(res, s).iter().any(|c| Rc::ptr_eq(c, proto))
        || class_chain(res, proto).iter().any(|c| Rc::ptr_eq(c, s));
    related.then_some(found).flatten()
}

/// `_zend_update_type_info` for the definition `v` of opline `k`.
fn op_type(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    ssa: &Ssa,
    vars: &[Info],
    k: usize,
    v: usize,
) -> (u32, Option<Ce>) {
    let l = &lines[k];
    let op = &ssa.ops[k];
    let vv = u32::try_from(v).ok();
    let t1 = op_type_of(&l.op1, op.op1_use, vars);
    let t2 = op_type_of(&l.op2, op.op2_use, vars);
    let ce_of = |u: Option<u32>| {
        u.and_then(|u| vars.get(u as usize))
            .and_then(|x| x.ce.clone())
    };
    let is_result = op.result_def == vv;
    let level = f.res.level;
    match l.op {
        "RECV" | "RECV_INIT" if is_result => {
            let Z::Num(n) = l.op1 else {
                return (t::UNKNOWN, None);
            };
            let Some(p) = (n as usize)
                .checked_sub(1)
                .and_then(|i| f.e.func.params.get(i))
            else {
                return (t::UNKNOWN, None);
            };
            let (mut ty, mut ce) = convert_type(f.res, p.ty.as_ref());
            if p.by_ref {
                ty |= t::REF;
                ce = None;
            }
            (ty, ce)
        }
        "QM_ASSIGN" | "JMP_SET" | "COALESCE" | "COPY_TMP" if is_result => {
            let mut ty = t1 & !(t::UNDEF | t::REF);
            if t1 & t::UNDEF != 0 {
                ty |= t::NULL;
            }
            if l.op == "COALESCE" || l.op == "JMP_SET" {
                ty &= !t::NULL;
            }
            (ty, ce_of(op.op1_use))
        }
        "ASSIGN" => {
            let mut ty = t2 & !(t::UNDEF | t::REF);
            if t2 & t::UNDEF != 0 {
                ty |= t::NULL;
            }
            if op.op1_def == vv && t1 & t::REF != 0 {
                ty |= t::REF;
            }
            (ty, ce_of(op.op2_use))
        }
        "ADD" | "SUB" | "MUL" | "DIV" | "POW" | "MOD" | "SL" | "SR" | "BW_OR" | "BW_AND"
        | "BW_XOR" | "CONCAT" | "FAST_CONCAT"
            if is_result =>
        {
            let r = vars.get(v).and_then(|x| x.range);
            (binary_type(l.op, t1, t2, r, level), None)
        }
        "ASSIGN_OP" => {
            let name = assign_op_name(&l.ext).unwrap_or("");
            let r = op
                .op1_def
                .and_then(|d| vars.get(d as usize))
                .and_then(|x| x.range);
            let mut ty = binary_type(name, t1, t2, r, level);
            if op.op1_def == vv && t1 & t::REF != 0 {
                ty |= t::REF;
            }
            (ty, None)
        }
        "SPACESHIP" | "STRLEN" | "COUNT" | "FUNC_NUM_ARGS" => (t::LONG, None),
        "IS_EQUAL"
        | "IS_NOT_EQUAL"
        | "IS_IDENTICAL"
        | "IS_NOT_IDENTICAL"
        | "IS_SMALLER"
        | "IS_SMALLER_OR_EQUAL"
        | "CASE"
        | "CASE_STRICT"
        | "BOOL"
        | "BOOL_NOT"
        | "BOOL_XOR"
        | "ISSET_ISEMPTY_CV"
        | "ISSET_ISEMPTY_DIM_OBJ"
        | "ISSET_ISEMPTY_PROP_OBJ"
        | "ISSET_ISEMPTY_STATIC_PROP"
        | "INSTANCEOF"
        | "JMPZ_EX"
        | "JMPNZ_EX"
        | "TYPE_CHECK"
        | "DEFINED"
        | "IN_ARRAY"
        | "ARRAY_KEY_EXISTS"
            if is_result =>
        {
            (t::BOOL, None)
        }
        "DO_ICALL" | "DO_UCALL" | "DO_FCALL" | "DO_FCALL_BY_NAME" if is_result => {
            let ty = match l.ret {
                Some(r) => {
                    let mut ty = r & (t::ANY | t::REF);
                    if ty & t::ARRAY != 0 {
                        ty |= t::KEY_ANY | t::OF_ANY | t::OF_REF;
                    }
                    ty
                }
                None => t::UNKNOWN,
            };
            (ty, None)
        }
        "FETCH_OBJ_R" | "FETCH_OBJ_IS" if is_result => {
            let this = matches!(&l.op1, Z::Text(s) if s == "THIS");
            let mut ty = 0;
            if !this && t1 & (t::ANY | t::UNDEF) & !t::OBJECT != 0 {
                ty |= t::NULL;
            }
            if this || t1 & t::OBJECT != 0 {
                // `zend_fetch_prop_info`.
                let scope = f.scope();
                let ce = if this {
                    scope.clone()
                } else {
                    ce_of(op.op1_use).and_then(|c| class_by_name(f.res, &c.name))
                };
                let prop = match (&l.op2, ce) {
                    (Z::Const(Value::Str(name)), Some(ce)) => {
                        lookup_prop(f.res, &ce, name.as_bytes(), scope.as_ref())
                            .filter(|(c, i)| c.props[*i].flags & flags::STATIC == 0)
                    }
                    _ => None,
                };
                let (pt, pce) = match prop {
                    Some((c, i)) => convert_type(f.res, c.props[i].ty.as_ref()),
                    None => (t::UNKNOWN, None),
                };
                return (ty | pt, pce);
            }
            (ty, None)
        }
        _ => {
            let mut ty = t::UNKNOWN;
            if !is_result {
                ty |= t::REF;
            }
            (ty, None)
        }
    }
}

/// `zend_func_return_info` (recursive, not widening).
fn return_info(
    f: &Func<'_, '_>,
    lines: &[ZLine],
    cfg: &Cfg,
    ssa: &Ssa,
    vars: &[Info],
) -> (u32, Option<Ce>, Range) {
    if f.e.func.is_generator() {
        return (
            t::OBJECT,
            Some(Ce {
                name: b"Generator".to_vec(),
                instanceof: false,
            }),
            Range::new(0, 0),
        );
    }
    let mut tmp = 0;
    let mut ce_state: Option<Option<Ce>> = None;
    let mut has_range: Option<bool> = None;
    let mut range = Range::new(0, 0);
    for b in &cfg.blocks {
        if b.flags & BB_REACHABLE == 0 || b.len == 0 {
            continue;
        }
        let k = b.start + b.len - 1;
        let l = &lines[k];
        if l.op != "RETURN" && l.op != "RETURN_BY_REF" {
            continue;
        }
        let op = &ssa.ops[k];
        let mut t1 = op_type_of(&l.op1, op.op1_use, vars);
        if t1 & t::UNDEF != 0 {
            t1 |= t::NULL;
        }
        if l.op == "RETURN" {
            t1 &= !(t::UNDEF | t::REF);
        } else {
            t1 |= t::REF;
            t1 &= !t::UNDEF;
        }
        tmp |= t1;
        let arg_ce = if t1 & t::REF == 0 {
            op.op1_use
                .and_then(|u| vars.get(u as usize))
                .and_then(|x| x.ce.clone())
        } else {
            None
        };
        ce_state = Some(match (ce_state, arg_ce) {
            (None, c) => c,
            (Some(Some(a)), Some(c)) if a.name == c.name => Some(Ce {
                name: a.name,
                instanceof: a.instanceof || c.instanceof,
            }),
            _ => None,
        });
        match &l.op1 {
            Z::Const(Value::Int(i)) => match has_range {
                None => {
                    has_range = Some(true);
                    range = Range::new(*i, *i);
                }
                Some(true) => {
                    if !range.underflow {
                        range.min = range.min.min(*i);
                    }
                    if !range.overflow {
                        range.max = range.max.max(*i);
                    }
                }
                Some(false) => {}
            },
            Z::Cv(_) | Z::Tmp(_) if op.op1_use.is_some() => {
                match op
                    .op1_use
                    .and_then(|u| vars.get(u as usize))
                    .and_then(|x| x.range)
                {
                    Some(r) => match has_range {
                        None => {
                            has_range = Some(true);
                            range = r;
                        }
                        Some(true) => {
                            if r.underflow {
                                range.underflow = true;
                                range.min = i64::MIN;
                            } else {
                                range.min = range.min.min(r.min);
                            }
                            if r.overflow {
                                range.overflow = true;
                                range.max = i64::MAX;
                            } else {
                                range.max = range.max.max(r.max);
                            }
                        }
                        Some(false) => {}
                    },
                    None => {
                        has_range = Some(true);
                        range = Range::FULL;
                    }
                }
            }
            _ => has_range = Some(false),
        }
    }
    let (ty, ce) = match &f.e.func.return_type {
        Some(rt) => {
            let (mut ty, ce) = convert_type(f.res, Some(rt));
            if f.ret_ref {
                ty |= t::REF;
            }
            (ty & tmp, ce)
        }
        None => ((t::UNKNOWN | t::REF) & tmp, ce_state.flatten()),
    };
    (ty, ce, range)
}

/// `zend_dump_type_info` (without reference counting).
fn write_type(out: &mut Vec<u8>, info: u32, ce: Option<&Ce>) {
    let ce_text = |ce: Option<&Ce>| -> Vec<u8> {
        let Some(c) = ce else {
            return Vec::new();
        };
        let mut v = b" (".to_vec();
        if c.instanceof {
            v.extend_from_slice(b"instanceof ");
        }
        v.extend_from_slice(super::printable(&c.name));
        v.push(b')');
        v
    };
    let mut items: Vec<Vec<u8>> = Vec::new();
    if info & t::UNDEF != 0 {
        items.push(b"undef".to_vec());
    }
    if info & t::INDIRECT != 0 {
        items.push(b"ind".to_vec());
    }
    if info & t::REF != 0 {
        items.push(b"ref".to_vec());
    }
    if info & t::CLASS != 0 {
        let mut v = b"class".to_vec();
        v.extend(ce_text(ce));
        items.push(v);
    } else if info & t::ANY == t::ANY {
        items.push(b"any".to_vec());
    } else {
        if info & t::NULL != 0 {
            items.push(b"null".to_vec());
        }
        if info & t::BOOL == t::BOOL {
            items.push(b"bool".to_vec());
        } else if info & t::FALSE != 0 {
            items.push(b"false".to_vec());
        } else if info & t::TRUE != 0 {
            items.push(b"true".to_vec());
        }
        for (bit, name) in [
            (t::LONG, &b"long"[..]),
            (t::DOUBLE, &b"double"[..]),
            (t::STRING, &b"string"[..]),
        ] {
            if info & bit != 0 {
                items.push(name.to_vec());
            }
        }
        if info & t::ARRAY != 0 {
            items.push(array_text(info));
        }
        if info & t::OBJECT != 0 {
            let mut v = b"object".to_vec();
            v.extend(ce_text(ce));
            items.push(v);
        }
        if info & t::RESOURCE != 0 {
            items.push(b"resource".to_vec());
        }
    }
    out.extend_from_slice(b" [");
    out.extend(items.join(&b", "[..]));
    out.push(b']');
}

/// The `array` part of `zend_dump_type_info`.
fn array_text(info: u32) -> Vec<u8> {
    let mut s = Vec::new();
    let keys = info & t::KEY_ANY;
    if keys == t::EMPTY {
        s.extend_from_slice(b"empty ");
    } else if keys == t::PACKED {
        s.extend_from_slice(b"packed ");
    } else if info & (t::NUMERIC_HASH | t::KEY_STRING) != 0 && info & (t::PACKED | t::EMPTY) == 0 {
        s.extend_from_slice(b"hash ");
    } else if keys != t::KEY_ANY && keys != 0 {
        let mut parts: Vec<&[u8]> = Vec::new();
        if info & t::EMPTY != 0 {
            parts.push(b"empty");
        }
        if info & t::PACKED != 0 {
            parts.push(b"packed");
        }
        if info & (t::NUMERIC_HASH | t::KEY_STRING) != 0 {
            parts.push(b"hash");
        }
        s.push(b'[');
        s.extend(parts.join(&b", "[..]));
        s.extend_from_slice(b"] ");
    }
    s.extend_from_slice(b"array");
    if info & (t::KEY_LONG | t::KEY_STRING) != 0
        && (info & t::KEY_LONG == 0 || info & t::KEY_STRING == 0)
    {
        let mut parts: Vec<&[u8]> = Vec::new();
        if info & t::KEY_LONG != 0 {
            parts.push(b"long");
        }
        if info & t::KEY_STRING != 0 {
            parts.push(b"string");
        }
        s.extend_from_slice(b" [");
        s.extend(parts.join(&b", "[..]));
        s.push(b']');
    }
    if info & (t::OF_ANY | t::OF_REF) != 0 {
        let mut parts: Vec<&[u8]> = Vec::new();
        if info & t::OF_ANY == t::OF_ANY {
            parts.push(b"any");
        } else {
            for (bit, name) in [
                (t::NULL, &b"null"[..]),
                (t::FALSE, &b"false"[..]),
                (t::TRUE, &b"true"[..]),
                (t::LONG, &b"long"[..]),
                (t::DOUBLE, &b"double"[..]),
                (t::STRING, &b"string"[..]),
                (t::ARRAY, &b"array"[..]),
                (t::OBJECT, &b"object"[..]),
                (t::RESOURCE, &b"resource"[..]),
            ] {
                if info & (bit << t::OF_SHIFT) != 0 {
                    parts.push(name);
                }
            }
        }
        if info & t::OF_REF != 0 {
            parts.push(b"ref");
        }
        s.extend_from_slice(b" of [");
        s.extend(parts.join(&b", "[..]));
        s.push(b']');
    }
    s
}

/// `zend_dump_range`.
fn write_range(out: &mut Vec<u8>, r: &Range) {
    if r.underflow && r.overflow {
        return;
    }
    let mut s = String::from(" RANGE[");
    if r.underflow {
        s.push_str("--..");
    } else if r.min == i64::MIN {
        s.push_str("MIN..");
    } else {
        let _ = write!(s, "{}..", r.min);
    }
    if r.overflow {
        s.push_str("++]");
    } else if r.max == i64::MAX {
        s.push_str("MAX]");
    } else {
        let _ = write!(s, "{}]", r.max);
    }
    out.extend_from_slice(s.as_bytes());
}

/// `zend_dump_var`: `kind` is `T`, `V` or `X` for a temporary.
fn write_var(out: &mut Vec<u8>, f: &Func<'_, '_>, kind: u8, v: usize) {
    if v < f.ncv {
        out.extend_from_slice(format!("CV{v}($").as_bytes());
        if let Some(n) = f.e.func.code.cv_names.get(v) {
            out.extend_from_slice(n.as_bytes());
        }
        out.push(b')');
    } else {
        out.push(kind);
        out.extend_from_slice(v.to_string().as_bytes());
    }
}

/// `zend_dump_ssa_var` (without escape states).
fn write_ssa(
    out: &mut Vec<u8>,
    f: &Func<'_, '_>,
    an: &Analysis,
    n: Option<u32>,
    kind: u8,
    v: usize,
) {
    match n {
        Some(n) => out.extend_from_slice(format!("#{n}.").as_bytes()),
        None => out.extend_from_slice(b"#?."),
    }
    write_var(out, f, kind, v);
    if let Some(info) = n.and_then(|n| an.vars.get(n as usize)) {
        if info.no_val {
            out.extend_from_slice(b" NOVAL");
        }
        write_type(out, info.ty, info.ce.as_ref());
        if let Some(r) = info.range {
            write_range(out, &r);
        }
    }
}

/// The kind letter of a temporary operand.
fn kind_of(z: &Z, is_var: bool) -> u8 {
    match z {
        Z::Tmp(_) if is_var => b'V',
        Z::Tmp(_) => b'T',
        _ => b'X',
    }
}

/// A variable operand of `zend_dump_op` with its SSA use and definition.
fn write_operand(
    out: &mut Vec<u8>,
    f: &Func<'_, '_>,
    an: &Analysis,
    z: &Z,
    is_var: bool,
    use_: Option<u32>,
    def: Option<u32>,
) {
    let Some(v) = f.var(z) else {
        return;
    };
    let kind = kind_of(z, is_var);
    if use_.is_some() {
        out.push(b' ');
        write_ssa(out, f, an, use_, kind, v);
    } else if def.is_none() {
        out.push(b' ');
        write_var(out, f, kind, v);
    }
    if def.is_some() {
        out.extend_from_slice(b" -> ");
        write_ssa(out, f, an, def, kind, v);
    }
}

/// A jump operand: the next successor of the block.
fn write_succ(out: &mut Vec<u8>, block: &Block, n: &mut usize) {
    let s = block.succ.get(*n).copied().unwrap_or(0);
    *n += 1;
    out.extend_from_slice(format!(" BB{s}").as_bytes());
}

/// `zend_dump_op_line` with an SSA op.
fn write_line(
    out: &mut Vec<u8>,
    f: &Func<'_, '_>,
    an: &Analysis,
    lines: &[ZLine],
    k: usize,
    block: &Block,
    op: &SsaOp,
) {
    let l = &lines[k];
    out.extend_from_slice(format!("{k:04} ").as_bytes());
    if op.result_use.is_none() {
        if let Some(v) = f.var(&l.res) {
            let kind = kind_of(&l.res, l.res_var);
            if op.result_def.is_some() {
                write_ssa(out, f, an, op.result_def, kind, v);
            } else {
                write_var(out, f, kind, v);
            }
            out.extend_from_slice(b" = ");
        }
    }
    out.extend_from_slice(l.op.as_bytes());
    out.extend_from_slice(l.suffix.as_bytes());
    out.extend_from_slice(l.ext.as_bytes());
    let mut n = 0usize;
    let op1 = match l.stack {
        Some(s) => Z::Num(stack_size(s, &[])),
        None => l.op1.clone(),
    };
    for (i, (z, is_var, use_, def)) in [
        (&op1, l.op1_var, op.op1_use, op.op1_def),
        (&l.op2, l.op2_var, op.op2_use, op.op2_def),
    ]
    .into_iter()
    .enumerate()
    {
        match z {
            Z::Const(v) => {
                out.push(b' ');
                write_const(out, v);
            }
            Z::Cv(_) | Z::Tmp(_) => write_operand(out, f, an, z, is_var, use_, def),
            Z::Label(_) | Z::ZLabel(_) => write_succ(out, block, &mut n),
            Z::Num(x) => out.extend_from_slice(format!(" {x}").as_bytes()),
            Z::Text(s) => {
                if !s.is_empty() {
                    out.push(b' ');
                    out.extend_from_slice(s.as_bytes());
                }
            }
            Z::Unused => {}
        }
        // The jump table follows `op1` (as the plain dump prints it).
        if i == 0 {
            if let Some(jt) = &l.jt {
                for (key, _) in jt {
                    match key {
                        Value::Str(s) => {
                            out.extend_from_slice(b" \"");
                            out.extend_from_slice(s.as_bytes());
                            out.extend_from_slice(b"\":");
                        }
                        Value::Int(i) => out.extend_from_slice(format!(" {i}:").as_bytes()),
                        _ => {}
                    }
                    write_succ(out, block, &mut n);
                    out.push(b',');
                }
                out.extend_from_slice(b" default:");
            }
        }
    }
    if l.ext_jmp.is_some() {
        write_succ(out, block, &mut n);
    }
    if op.result_use.is_some() {
        if let Some(v) = f.var(&l.res) {
            let kind = kind_of(&l.res, l.res_var);
            out.push(b' ');
            write_ssa(out, f, an, op.result_use, kind, v);
            if op.result_def.is_some() {
                out.extend_from_slice(b" -> ");
                write_ssa(out, f, an, op.result_def, kind, v);
            }
        }
    }
    out.push(b'\n');
}

/// `zend_dump_block_header`.
fn write_block_header(
    out: &mut Vec<u8>,
    f: &Func<'_, '_>,
    an: &Analysis,
    cfg: &Cfg,
    ssa: &Ssa,
    b: usize,
) {
    let block = &cfg.blocks[b];
    let mut s = String::new();
    if b > 0 {
        s.push('\n');
    }
    let _ = write!(s, "BB{b}:\n     ;");
    for (bit, name) in [
        (BB_START, " start"),
        (BB_FOLLOW, " follow"),
        (BB_TARGET, " target"),
        (BB_EXIT, " exit"),
    ] {
        if block.flags & bit != 0 {
            s.push_str(name);
        }
    }
    if block.flags & BB_REACHABLE == 0 {
        s.push_str(" unreachable");
    }
    let _ = writeln!(
        s,
        " lines=[{}-{}]",
        block.start,
        block.start + block.len - 1
    );
    let list = |v: &[usize]| -> String {
        v.iter()
            .map(|b| format!("BB{b}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !block.preds.is_empty() {
        let _ = writeln!(s, "     ; from=({})", list(&block.preds[..]));
    }
    if !block.succ.is_empty() {
        let _ = writeln!(s, "     ; to=({})", list(&block.succ[..]));
    }
    if let Some(d) = block.idom {
        let _ = writeln!(s, "     ; idom=BB{d}");
    }
    if let Some(l) = block.level {
        let _ = writeln!(s, "     ; level={l}");
    }
    if !block.children.is_empty() {
        let _ = writeln!(s, "     ; children=({})", list(&block.children[..]));
    }
    out.extend_from_slice(s.as_bytes());
    for p in ssa.phis.get(b).into_iter().flatten() {
        out.extend_from_slice(b"     ");
        write_ssa(out, f, an, Some(p.ssa), b'X', p.var);
        out.extend_from_slice(b" = Phi(");
        for (j, src) in p.sources.iter().enumerate() {
            if j > 0 {
                out.extend_from_slice(b", ");
            }
            write_ssa(out, f, an, *src, b'X', p.var);
        }
        out.extend_from_slice(b")\n");
    }
}

/// `zend_dump_op_array(op_array, ZEND_DUMP_SSA, msg, ssa)`.
fn write_op_array(
    out: &mut Vec<u8>,
    f: &Func<'_, '_>,
    lines: &[ZLine],
    cfg: &Cfg,
    ssa: &Ssa,
    an: &Analysis,
    msg: &str,
    main_line_end: u32,
    tmps: u32,
) {
    let e = f.e;
    let num_args = e.func.params.iter().filter(|p| !p.variadic).count();
    out.push(b'\n');
    out.extend_from_slice(&e.name);
    let mut s = format!(
        ":\n     ; (lines={}, args={num_args}, vars={}, tmps={tmps}, ssa_vars={}",
        lines.len(),
        f.ncv,
        ssa.count
    );
    if cfg.no_loops {
        s.push_str(", no_loops");
    }
    let _ = write!(s, ")\n     ; ({msg})\n     ; ");
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(e.func.code.filename.as_bytes());
    let (start, end) = if e.is_main {
        (1, main_line_end)
    } else {
        (e.func.line_start, e.func.line_end)
    };
    out.extend_from_slice(format!(":{start}-{end}\n     ; return ").as_bytes());
    let (rt, rce, rr) = &an.ret;
    write_type(out, *rt, rce.as_ref());
    write_range(out, rr);
    out.push(b'\n');
    for v in 0..f.ncv {
        out.extend_from_slice(b"     ; ");
        write_ssa(out, f, an, u32::try_from(v).ok(), b'X', v);
        out.push(b'\n');
    }
    for (b, block) in cfg.blocks.iter().enumerate() {
        write_block_header(out, f, an, cfg, ssa, b);
        for k in block.start..block.start + block.len {
            let op = ssa.ops.get(k).copied().unwrap_or_default();
            write_line(out, f, an, lines, k, block, &op);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{range_and, range_or, t, write_range, write_type, Range};

    /// gh11170: the ranges of `BW_OR` and `BW_AND` of two negative ranges.
    #[test]
    fn or_and_ranges_match_php() {
        assert_eq!(range_or(-27, -7, -20, -10), (-20, -1));
        assert_eq!(range_and(-12, -9, -27, -25), (-28, -25));
    }

    #[test]
    fn ranges_print_like_zend_dump_range() {
        let mut out = Vec::new();
        write_range(&mut out, &Range::new(i64::MIN, i64::MAX));
        write_range(&mut out, &Range::new(-1, 1));
        write_range(&mut out, &Range::FULL);
        assert_eq!(out, b" RANGE[MIN..MAX] RANGE[-1..1]");
    }

    #[test]
    fn types_print_like_zend_dump_type_info() {
        let mut out = Vec::new();
        write_type(&mut out, t::UNKNOWN, None);
        write_type(
            &mut out,
            t::ARRAY | t::KEY_ANY | t::OF_ANY | t::OF_REF,
            None,
        );
        write_type(
            &mut out,
            t::ARRAY | t::PACKED | t::NUMERIC_HASH | (t::LONG << t::OF_SHIFT),
            None,
        );
        write_type(&mut out, t::UNDEF, None);
        write_type(&mut out, t::LONG | t::DOUBLE, None);
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            " [any] [array of [any, ref]] [[packed, hash] array [long] of [long]] [undef] [long, double]"
        );
    }
}
