//! Experiment: can Rust run PHP's `for ($i = 0; $i < $n; $i++) {}` and
//! `$x = $a + $b` the way the Zend VM does, i.e. with handlers chosen at
//! compile time and operands pre-decoded, and reach C PHP's instruction
//! counts (30 per iteration for the empty loop; phprust today: 95)?
//!
//! Same value model as phprust: a 16-byte Rust enum with reference-counted
//! variants, `Option<Value>` slots (None = undefined variable), slots of a
//! frame in a heap vector. Same semantics as the Zend handlers: type tests on
//! every operand, overflow test on the increment, an interrupt poll on the
//! backward jump, out-of-line slow paths for every other type.
//!
//! Variants (argv[1]):
//!   generic   run-time decoded generic ops, bounds-checked vectors, a poll
//!             after every op (a miniature of today's design)
//!   match     pre-decoded specialised ops, `match` dispatch, unchecked slots
//!   fused     the same with one superinstruction for `$i++; $i < $n; jump`
//!   call      handler function pointers, call threading (Zend's CALL VM)
//!   tail      handler function pointers, each handler tail-calls the next
//!             (Zend's HYBRID/GOTO style: `jmp *(%r15)`)
//! Program (argv[2]): loop | add.  Iterations: argv[3].

use std::hint::black_box;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

#[derive(Clone)]
#[allow(dead_code)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<Vec<u8>>),
    Array(Rc<Vec<Value>>),
}
type Slot = Option<Value>;

static INTERRUPT: AtomicBool = AtomicBool::new(false);

#[cold]
#[inline(never)]
fn interrupted() {
    INTERRUPT.store(false, Relaxed);
}

// ---- slow paths shared by every variant (what the generic PHP semantics need) ----

#[cold]
#[inline(never)]
fn slow_inc(s: &mut Slot) {
    *s = Some(match s.take() {
        Some(Value::Int(i)) => match i.checked_add(1) {
            Some(n) => Value::Int(n),
            None => Value::Float(i as f64 + 1.0),
        },
        Some(Value::Float(f)) => Value::Float(f + 1.0),
        None | Some(Value::Null) => Value::Int(1),
        Some(v) => v,
    });
}

fn num(s: &Slot) -> f64 {
    match s {
        Some(Value::Int(i)) => *i as f64,
        Some(Value::Float(f)) => *f,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

#[cold]
#[inline(never)]
fn slow_lt(a: &Slot, b: &Slot) -> bool {
    num(a) < num(b)
}

#[cold]
#[inline(never)]
fn slow_add(a: &Slot, b: &Slot) -> Value {
    match (a, b) {
        (Some(Value::Int(x)), Some(Value::Int(y))) => Value::Float(*x as f64 + *y as f64),
        _ => Value::Float(num(a) + num(b)),
    }
}

#[cold]
#[inline(never)]
fn slow_store(dst: &mut Slot, v: Value) {
    *dst = Some(v); // drops the old value (a string, an array ...)
}

// =====================================================================
// Variant "generic": a miniature of the current phprust design
// =====================================================================
mod generic {
    use super::*;

    #[derive(Clone, Copy, PartialEq)]
    #[allow(dead_code)]
    pub enum Operand {
        Cv(u32),
        Tmp(u32),
        Lit(u32),
        Unused,
    }
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    pub enum Op {
        Nop,
        AssignCv { cv: u32, val: Operand },
        Jmp(u32),
        IncDecCvCmpJmp { cv: u32, inc: bool },
        CmpJmp { a: Operand, b: Operand, target: u32, mask: u8 },
        BinAssignCv { add: bool, a: Operand, b: Operand, cv: u32 },
        Ret(Operand),
    }
    pub struct Frame {
        pub cvs: Vec<Slot>,
        pub tmps: Vec<Value>,
        pub symtab_attached: bool,
    }
    pub static SAFE_POINT: AtomicBool = AtomicBool::new(false);

    #[inline(always)]
    fn int_of(f: &Frame, lits: &[Value], o: Operand) -> Option<i64> {
        let v = match o {
            Operand::Cv(c) => f.cvs.get(c as usize)?.as_ref()?,
            Operand::Lit(l) => lits.get(l as usize)?,
            Operand::Tmp(t) => f.tmps.get(t as usize)?,
            Operand::Unused => return None,
        };
        if let Value::Int(i) = v {
            Some(*i)
        } else {
            None
        }
    }

    #[inline(never)]
    pub fn run(f: &mut Frame, ops: &[Op], lits: &[Value]) -> i64 {
        let mut ip = 0usize;
        loop {
            let Some(op) = ops.get(ip) else { return -1 };
            ip += 1;
            match *op {
                Op::Nop => {}
                Op::Jmp(t) => ip = t as usize,
                Op::AssignCv { cv, val } => {
                    let Some(v) = int_of(f, lits, val) else { return -2 };
                    let Some(slot) = f.cvs.get_mut(cv as usize) else { return -2 };
                    *slot = Some(Value::Int(v));
                }
                Op::IncDecCvCmpJmp { cv, inc } => {
                    let Some(slot) = f.cvs.get_mut(cv as usize) else { return -2 };
                    let new = match slot {
                        Some(Value::Int(i)) => {
                            let n = if inc { i.checked_add(1) } else { i.checked_sub(1) };
                            match n {
                                Some(n) => {
                                    *i = n;
                                    n
                                }
                                None => return -3,
                            }
                        }
                        _ => {
                            slow_inc(slot);
                            continue;
                        }
                    };
                    if f.symtab_attached {
                        return -4;
                    }
                    if let Some(&Op::CmpJmp { a, b, target, mask }) = ops.get(ip) {
                        if a == Operand::Cv(cv) {
                            if let Some(y) = int_of(f, lits, b) {
                                let outcome = 1u8 << (u8::from(new > y) + u8::from(new >= y));
                                ip = if mask & outcome != 0 { target as usize } else { ip + 1 };
                            }
                        }
                    }
                }
                Op::CmpJmp { a, b, target, mask } => {
                    let (Some(x), Some(y)) = (int_of(f, lits, a), int_of(f, lits, b)) else { return -5 };
                    let outcome = 1u8 << (u8::from(x > y) + u8::from(x >= y));
                    if mask & outcome != 0 {
                        ip = target as usize;
                    }
                }
                Op::BinAssignCv { add, a, b, cv } => {
                    let (Some(x), Some(y)) = (int_of(f, lits, a), int_of(f, lits, b)) else { return -5 };
                    let r = if add { x.checked_add(y) } else { x.checked_sub(y) };
                    let Some(r) = r else { return -6 };
                    let Some(slot) = f.cvs.get_mut(cv as usize) else { return -2 };
                    match slot {
                        Some(Value::Int(i)) => *i = r,
                        _ => slow_store(slot, Value::Int(r)),
                    }
                    if f.symtab_attached {
                        return -4;
                    }
                }
                Op::Ret(o) => return int_of(f, lits, o).unwrap_or(-7),
            }
            if SAFE_POINT.load(Relaxed) {
                interrupted();
            }
        }
    }

    pub fn program(which: &str) -> Vec<Op> {
        use Operand::*;
        // cv0 = $i, cv1 = $n, cv2 = $a, cv3 = $b, cv4 = $x ; lit0 = 0
        match which {
            // for ($i = 0; $i < $n; $i++) {}
            "loop" => vec![
                Op::AssignCv { cv: 0, val: Lit(0) },
                Op::Jmp(3),
                Op::IncDecCvCmpJmp { cv: 0, inc: true },
                Op::CmpJmp { a: Cv(0), b: Cv(1), target: 2, mask: 1 },
                Op::Ret(Cv(0)),
            ],
            // for (...) { $x = $a + $b; }
            _ => vec![
                Op::AssignCv { cv: 0, val: Lit(0) },
                Op::Jmp(4),
                Op::BinAssignCv { add: true, a: Cv(2), b: Cv(3), cv: 4 },
                Op::IncDecCvCmpJmp { cv: 0, inc: true },
                Op::CmpJmp { a: Cv(0), b: Cv(1), target: 2, mask: 1 },
                Op::Ret(Cv(4)),
            ],
        }
    }
}

// =====================================================================
// Pre-decoded, specialised instructions (what zend_vm_gen.php generates)
// =====================================================================
#[repr(u8)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Opc {
    AssignCvLit,
    Jmp,
    PreIncCv,
    IsSmallerCvCvJmpnz,
    IncCvLtCvJmp,
    AddCvCvToTmp,
    AssignCvTmp,
    AddCvCvToCv,
    RetCv,
}
/// 16 bytes: opcode + three operands (slot numbers, literal numbers or a
/// jump target), all decided when the function is compiled.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Inst {
    op: Opc,
    a: u32,
    b: u32,
    c: u32,
}
const fn i(op: Opc, a: u32, b: u32, c: u32) -> Inst {
    Inst { op, a, b, c }
}

/// The slot `n` of the frame at `base`. Unchecked: the compiler that emits
/// the instructions guarantees `n` is below the frame's slot count.
#[inline(always)]
unsafe fn slot<'a>(base: *mut Slot, n: u32) -> &'a mut Slot {
    &mut *base.add(n as usize)
}

mod matched {
    use super::*;

    /// `match` dispatch over pre-decoded specialised instructions.
    #[inline(never)]
    pub unsafe fn run(code: *const Inst, base: *mut Slot, lits: *const Value) -> i64 {
        let mut ip = code;
        loop {
            let inst = &*ip;
            match inst.op {
                Opc::AssignCvLit => {
                    let v = (*lits.add(inst.b as usize)).clone();
                    slow_store(slot(base, inst.a), v);
                    ip = ip.add(1);
                }
                Opc::Jmp => ip = code.add(inst.a as usize),
                // ZEND_PRE_INC_SPEC_CV_RETVAL_UNUSED
                Opc::PreIncCv => {
                    let s = slot(base, inst.a);
                    match s {
                        Some(Value::Int(n)) => match n.checked_add(1) {
                            Some(m) => *n = m,
                            None => slow_inc(s),
                        },
                        _ => slow_inc(s),
                    }
                    ip = ip.add(1);
                }
                // ZEND_IS_SMALLER_SPEC_TMPVARCV_TMPVARCV_JMPNZ
                Opc::IsSmallerCvCvJmpnz => {
                    let (x, y) = (&*slot(base, inst.a), &*slot(base, inst.b));
                    let lt = match (x, y) {
                        (Some(Value::Int(x)), Some(Value::Int(y))) => *x < *y,
                        _ => slow_lt(x, y),
                    };
                    if lt {
                        ip = code.add(inst.c as usize);
                        if INTERRUPT.load(Relaxed) {
                            interrupted();
                        }
                    } else {
                        ip = ip.add(1);
                    }
                }
                // Superinstruction: `$i++` then `$i < $n` then the jump.
                Opc::IncCvLtCvJmp => {
                    let s = slot(base, inst.a);
                    let new = match s {
                        Some(Value::Int(n)) => match n.checked_add(1) {
                            Some(m) => {
                                *n = m;
                                Some(m)
                            }
                            None => None,
                        },
                        _ => None,
                    };
                    let rhs = &*slot(base, inst.b);
                    let lt = match (new, rhs) {
                        (Some(x), Some(Value::Int(y))) => x < *y,
                        (Some(_), _) => slow_lt(&*slot(base, inst.a), rhs),
                        (None, _) => {
                            slow_inc(slot(base, inst.a));
                            slow_lt(&*slot(base, inst.a), rhs)
                        }
                    };
                    if lt {
                        ip = code.add(inst.c as usize);
                        if INTERRUPT.load(Relaxed) {
                            interrupted();
                        }
                    } else {
                        ip = ip.add(1);
                    }
                }
                // ZEND_ADD_SPEC_TMPVARCV_TMPVARCV (result in a temporary slot)
                Opc::AddCvCvToTmp => {
                    let (x, y) = (&*slot(base, inst.a), &*slot(base, inst.b));
                    let r = match (x, y) {
                        (Some(Value::Int(x)), Some(Value::Int(y))) => match x.checked_add(*y) {
                            Some(r) => Value::Int(r),
                            None => slow_add(&*slot(base, inst.a), &*slot(base, inst.b)),
                        },
                        _ => slow_add(x, y),
                    };
                    // A temporary is dead before its definition: plain store.
                    std::ptr::write(base.add(inst.c as usize), Some(r));
                    ip = ip.add(1);
                }
                // ZEND_ASSIGN_SPEC_CV_TMP_RETVAL_UNUSED
                Opc::AssignCvTmp => {
                    let v = std::ptr::read(base.add(inst.b as usize));
                    let d = slot(base, inst.a);
                    match d {
                        // Nothing to release when the old value is a scalar.
                        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                            std::ptr::write(d, v);
                        }
                        _ => {
                            let old = std::ptr::replace(d, v);
                            drop_slow(old);
                        }
                    }
                    ip = ip.add(1);
                }
                // Superinstruction: `$x = $a + $b` (ADD + ASSIGN).
                Opc::AddCvCvToCv => {
                    let (x, y) = (&*slot(base, inst.a), &*slot(base, inst.b));
                    let r = match (x, y) {
                        (Some(Value::Int(x)), Some(Value::Int(y))) => x.checked_add(*y),
                        _ => None,
                    };
                    let d = slot(base, inst.c);
                    match (r, &mut *d) {
                        (Some(r), Some(Value::Int(old))) => *old = r,
                        (Some(r), _) => slow_store(d, Value::Int(r)),
                        (None, _) => {
                            let v = slow_add(&*slot(base, inst.a), &*slot(base, inst.b));
                            slow_store(slot(base, inst.c), v);
                        }
                    }
                    ip = ip.add(1);
                }
                Opc::RetCv => {
                    return match &*slot(base, inst.a) {
                        Some(Value::Int(n)) => *n,
                        _ => -1,
                    }
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn drop_slow(v: Slot) {
        drop(v);
    }
}

// cv0 = $i, cv1 = $n, cv2 = $a, cv3 = $b, cv4 = $x, slot 5 = a temporary
fn program(which: &str, fused: bool) -> Vec<Inst> {
    match (which, fused) {
        ("loop", false) => vec![
            i(Opc::AssignCvLit, 0, 0, 0),
            i(Opc::Jmp, 3, 0, 0),
            i(Opc::PreIncCv, 0, 0, 0),
            i(Opc::IsSmallerCvCvJmpnz, 0, 1, 2),
            i(Opc::RetCv, 0, 0, 0),
        ],
        ("loop", true) => vec![
            i(Opc::AssignCvLit, 0, 0, 0),
            i(Opc::IsSmallerCvCvJmpnz, 0, 1, 2),
            i(Opc::IncCvLtCvJmp, 0, 1, 2),
            i(Opc::RetCv, 0, 0, 0),
        ],
        (_, false) => vec![
            i(Opc::AssignCvLit, 0, 0, 0),
            i(Opc::Jmp, 5, 0, 0),
            i(Opc::AddCvCvToTmp, 2, 3, 5),
            i(Opc::AssignCvTmp, 4, 5, 0),
            i(Opc::PreIncCv, 0, 0, 0),
            i(Opc::IsSmallerCvCvJmpnz, 0, 1, 2),
            i(Opc::RetCv, 4, 0, 0),
        ],
        (_, true) => vec![
            i(Opc::AssignCvLit, 0, 0, 0),
            i(Opc::IsSmallerCvCvJmpnz, 0, 1, 2),
            i(Opc::AddCvCvToCv, 2, 3, 4),
            i(Opc::IncCvLtCvJmp, 0, 1, 2),
            i(Opc::RetCv, 4, 0, 0),
        ],
    }
}

// =====================================================================
// Handler function pointers: call threading and tail-call threading
// =====================================================================
mod threaded {
    use super::*;

    /// Like `zend_op`: the handler's address is in the instruction.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TInst {
        pub h: Handler,
        pub a: u32,
        pub b: u32,
        /// A jump target as a signed offset in instructions from this one.
        pub c: i32,
        pub pad: u32,
    }
    /// Tail-threaded handler: runs its op, then jumps to the next handler.
    /// Returns only from the `Ret` op.
    pub type Handler = unsafe extern "C" fn(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64;

    macro_rules! next {
        ($ip:expr, $base:expr, $lits:expr) => {{
            let n = $ip;
            return ((*n).h)(n, $base, $lits);
        }};
    }

    pub unsafe extern "C" fn assign_cv_lit(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        let v = (*lits.add(inst.b as usize)).clone();
        slow_store(slot(base, inst.a), v);
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn jmp(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        next!(ip.offset((*ip).c as isize), base, lits)
    }
    pub unsafe extern "C" fn pre_inc_cv(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        if let Some(Value::Int(n)) = slot(base, (*ip).a) {
            if let Some(m) = n.checked_add(1) {
                *n = m;
                next!(ip.add(1), base, lits)
            }
        }
        // ZEND_VM_DISPATCH_TO_HELPER: the slow path is a tail call too, so
        // the handler itself never needs to save registers.
        pre_inc_cv_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn pre_inc_cv_slow(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        slow_inc(slot(base, (*ip).a));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn is_smaller_cv_cv_jmpnz(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        if let (Some(Value::Int(x)), Some(Value::Int(y))) = (&*slot(base, inst.a), &*slot(base, inst.b)) {
            if *x < *y {
                if INTERRUPT.load(Relaxed) {
                    return interrupt_then(ip.offset(inst.c as isize), base, lits);
                }
                next!(ip.offset(inst.c as isize), base, lits)
            }
            next!(ip.add(1), base, lits)
        }
        is_smaller_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn interrupt_then(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        interrupted();
        next!(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn is_smaller_slow(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        if slow_lt(&*slot(base, inst.a), &*slot(base, inst.b)) {
            next!(ip.offset(inst.c as isize), base, lits)
        }
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn add_cv_cv_to_tmp(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        if let (Some(Value::Int(x)), Some(Value::Int(y))) = (&*slot(base, inst.a), &*slot(base, inst.b)) {
            if let Some(r) = x.checked_add(*y) {
                std::ptr::write(base.add(inst.c as usize), Some(Value::Int(r)));
                next!(ip.add(1), base, lits)
            }
        }
        add_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn add_slow(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        let r = slow_add(&*slot(base, inst.a), &*slot(base, inst.b));
        std::ptr::write(base.add(inst.c as usize), Some(r));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn assign_cv_tmp(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        let d = base.add(inst.a as usize);
        if let None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) = &*d {
            // Nothing to release: a 16-byte move.
            std::ptr::copy_nonoverlapping(base.add(inst.b as usize), d, 1);
            next!(ip.add(1), base, lits)
        }
        assign_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn assign_slow(ip: *const TInst, base: *mut Slot, lits: *const Value) -> i64 {
        let inst = &*ip;
        let v = std::ptr::read(base.add(inst.b as usize));
        slow_store(slot(base, inst.a), v.unwrap_or(Value::Null));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn ret_cv(ip: *const TInst, base: *mut Slot, _lits: *const Value) -> i64 {
        match &*slot(base, (*ip).a) {
            Some(Value::Int(n)) => *n,
            _ => -1,
        }
    }

    const fn t(h: Handler, a: u32, b: u32, c: i32) -> TInst {
        TInst { h, a, b, c, pad: 0 }
    }
    pub fn program(which: &str) -> Vec<TInst> {
        match which {
            "loop" => vec![
                t(assign_cv_lit, 0, 0, 0),
                t(jmp, 0, 0, 2),
                t(pre_inc_cv, 0, 0, 0),
                t(is_smaller_cv_cv_jmpnz, 0, 1, -1),
                t(ret_cv, 0, 0, 0),
            ],
            _ => vec![
                t(assign_cv_lit, 0, 0, 0),
                t(jmp, 0, 0, 4),
                t(add_cv_cv_to_tmp, 2, 3, 5),
                t(assign_cv_tmp, 4, 5, 0),
                t(pre_inc_cv, 0, 0, 0),
                t(is_smaller_cv_cv_jmpnz, 0, 1, -3),
                t(ret_cv, 4, 0, 0),
            ],
        }
    }

    // ---- call threading: the loop calls each handler, which returns the next ip ----
    pub type CHandler = unsafe extern "C" fn(ip: *const CInst, base: *mut Slot, lits: *const Value) -> *const CInst;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CInst {
        pub h: CHandler,
        pub a: u32,
        pub b: u32,
        pub c: i32,
        pub pad: u32,
    }
    pub unsafe extern "C" fn c_assign_cv_lit(ip: *const CInst, base: *mut Slot, lits: *const Value) -> *const CInst {
        let inst = &*ip;
        let v = (*lits.add(inst.b as usize)).clone();
        slow_store(slot(base, inst.a), v);
        ip.add(1)
    }
    pub unsafe extern "C" fn c_jmp(ip: *const CInst, _b: *mut Slot, _l: *const Value) -> *const CInst {
        ip.offset((*ip).c as isize)
    }
    pub unsafe extern "C" fn c_pre_inc_cv(ip: *const CInst, base: *mut Slot, _l: *const Value) -> *const CInst {
        let s = slot(base, (*ip).a);
        match s {
            Some(Value::Int(n)) => match n.checked_add(1) {
                Some(m) => *n = m,
                None => slow_inc(s),
            },
            _ => slow_inc(s),
        }
        ip.add(1)
    }
    pub unsafe extern "C" fn c_is_smaller_cv_cv_jmpnz(ip: *const CInst, base: *mut Slot, _l: *const Value) -> *const CInst {
        let inst = &*ip;
        let (x, y) = (&*slot(base, inst.a), &*slot(base, inst.b));
        let lt = match (x, y) {
            (Some(Value::Int(x)), Some(Value::Int(y))) => *x < *y,
            _ => slow_lt(x, y),
        };
        if lt {
            if INTERRUPT.load(Relaxed) {
                interrupted();
            }
            ip.offset(inst.c as isize)
        } else {
            ip.add(1)
        }
    }
    pub unsafe extern "C" fn c_add_cv_cv_to_tmp(ip: *const CInst, base: *mut Slot, _l: *const Value) -> *const CInst {
        let inst = &*ip;
        let (x, y) = (&*slot(base, inst.a), &*slot(base, inst.b));
        let r = match (x, y) {
            (Some(Value::Int(x)), Some(Value::Int(y))) => match x.checked_add(*y) {
                Some(r) => Value::Int(r),
                None => slow_add(&*slot(base, inst.a), &*slot(base, inst.b)),
            },
            _ => slow_add(x, y),
        };
        std::ptr::write(base.add(inst.c as usize), Some(r));
        ip.add(1)
    }
    pub unsafe extern "C" fn c_assign_cv_tmp(ip: *const CInst, base: *mut Slot, _l: *const Value) -> *const CInst {
        let inst = &*ip;
        let v = std::ptr::read(base.add(inst.b as usize));
        let d = slot(base, inst.a);
        match d {
            None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => std::ptr::write(d, v),
            _ => slow_store(d, v.unwrap_or(Value::Null)),
        }
        ip.add(1)
    }
    pub unsafe extern "C" fn c_ret(_ip: *const CInst, _b: *mut Slot, _l: *const Value) -> *const CInst {
        std::ptr::null()
    }
    const fn c(h: CHandler, a: u32, b: u32, c: i32) -> CInst {
        CInst { h, a, b, c, pad: 0 }
    }
    pub fn cprogram(which: &str) -> Vec<CInst> {
        match which {
            "loop" => vec![
                c(c_assign_cv_lit, 0, 0, 0),
                c(c_jmp, 0, 0, 2),
                c(c_pre_inc_cv, 0, 0, 0),
                c(c_is_smaller_cv_cv_jmpnz, 0, 1, -1),
                c(c_ret, 0, 0, 0),
            ],
            _ => vec![
                c(c_assign_cv_lit, 0, 0, 0),
                c(c_jmp, 0, 0, 4),
                c(c_add_cv_cv_to_tmp, 2, 3, 5),
                c(c_assign_cv_tmp, 4, 5, 0),
                c(c_pre_inc_cv, 0, 0, 0),
                c(c_is_smaller_cv_cv_jmpnz, 0, 1, -3),
                c(c_ret, 4, 0, 0),
            ],
        }
    }
    #[inline(never)]
    pub unsafe fn run_call(code: *const CInst, base: *mut Slot, lits: *const Value) {
        let mut ip = code;
        while !ip.is_null() {
            ip = ((*ip).h)(ip, base, lits);
        }
    }
}

// =====================================================================
// Variant "tail2": the tail-threaded handlers with Zend's operand encoding:
// operands are byte offsets from the frame base, jump targets are byte
// offsets from the current instruction (no index scaling at run time).
// =====================================================================
mod tail2 {
    use super::*;
    pub type Handler = unsafe extern "C" fn(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZInst {
        pub h: Handler,
        pub a: u32,
        pub b: u32,
        pub c: i32,
        pub pad: u32,
    }
    #[inline(always)]
    unsafe fn at<'a>(base: *mut u8, off: u32) -> &'a mut Slot {
        &mut *base.add(off as usize).cast::<Slot>()
    }
    macro_rules! next {
        ($ip:expr, $base:expr, $lits:expr) => {{
            let n = $ip;
            return ((*n).h)(n, $base, $lits);
        }};
    }
    #[inline(always)]
    unsafe fn jump(ip: *const ZInst, off: i32) -> *const ZInst {
        ip.cast::<u8>().offset(off as isize).cast()
    }
    pub unsafe extern "C" fn assign_cv_lit(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        let v = (*lits.add(inst.b as usize)).clone();
        slow_store(at(base, inst.a), v);
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn jmp(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        next!(jump(ip, (*ip).c), base, lits)
    }
    pub unsafe extern "C" fn pre_inc_cv(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        if let Some(Value::Int(n)) = at(base, (*ip).a) {
            if let Some(m) = n.checked_add(1) {
                *n = m;
                next!(ip.add(1), base, lits)
            }
        }
        pre_inc_cv_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn pre_inc_cv_slow(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        slow_inc(at(base, (*ip).a));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn is_smaller_cv_cv_jmpnz(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        if let Some(Value::Int(x)) = &*at(base, inst.a) {
            if let Some(Value::Int(y)) = &*at(base, inst.b) {
                if *x < *y {
                    if INTERRUPT.load(Relaxed) {
                        return interrupt_then(jump(ip, inst.c), base, lits);
                    }
                    next!(jump(ip, inst.c), base, lits)
                }
                next!(ip.add(1), base, lits)
            }
        }
        is_smaller_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn interrupt_then(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        interrupted();
        next!(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn is_smaller_slow(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        if slow_lt(&*at(base, inst.a), &*at(base, inst.b)) {
            next!(jump(ip, inst.c), base, lits)
        }
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn add_cv_cv_to_tmp(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        if let Some(Value::Int(x)) = &*at(base, inst.a) {
            if let Some(Value::Int(y)) = &*at(base, inst.b) {
                if let Some(r) = x.checked_add(*y) {
                    std::ptr::write(base.add(inst.c as usize).cast::<Slot>(), Some(Value::Int(r)));
                    next!(ip.add(1), base, lits)
                }
            }
        }
        add_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn add_slow(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        let r = slow_add(&*at(base, inst.a), &*at(base, inst.b));
        std::ptr::write(base.add(inst.c as usize).cast::<Slot>(), Some(r));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn assign_cv_tmp(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        let d = base.add(inst.a as usize).cast::<Slot>();
        if let None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) = &*d {
            std::ptr::copy_nonoverlapping(base.add(inst.b as usize).cast::<Slot>(), d, 1);
            next!(ip.add(1), base, lits)
        }
        assign_slow(ip, base, lits)
    }
    #[cold]
    #[inline(never)]
    unsafe extern "C" fn assign_slow(ip: *const ZInst, base: *mut u8, lits: *const Value) -> i64 {
        let inst = &*ip;
        let v = std::ptr::read(base.add(inst.b as usize).cast::<Slot>());
        slow_store(at(base, inst.a), v.unwrap_or(Value::Null));
        next!(ip.add(1), base, lits)
    }
    pub unsafe extern "C" fn ret_cv(ip: *const ZInst, base: *mut u8, _lits: *const Value) -> i64 {
        match &*at(base, (*ip).a) {
            Some(Value::Int(n)) => *n,
            _ => -1,
        }
    }
    const SZ: i32 = std::mem::size_of::<ZInst>() as i32;
    const fn z(h: Handler, a: u32, b: u32, c: i32) -> ZInst {
        ZInst { h, a: a * 16, b: b * 16, c: c * SZ, pad: 0 }
    }
    pub fn program(which: &str) -> Vec<ZInst> {
        let mut v = match which {
            "loop" => vec![
                z(assign_cv_lit, 0, 0, 0),
                z(jmp, 0, 0, 2),
                z(pre_inc_cv, 0, 0, 0),
                z(is_smaller_cv_cv_jmpnz, 0, 1, -1),
                z(ret_cv, 0, 0, 0),
            ],
            _ => vec![
                z(assign_cv_lit, 0, 0, 0),
                z(jmp, 0, 0, 4),
                z(add_cv_cv_to_tmp, 2, 3, 0),
                z(assign_cv_tmp, 4, 5, 0),
                z(pre_inc_cv, 0, 0, 0),
                z(is_smaller_cv_cv_jmpnz, 0, 1, -3),
                z(ret_cv, 4, 0, 0),
            ],
        };
        // The literal operand of assign_cv_lit is an index, and the add's
        // result slot goes in `c` as a byte offset.
        v[0].b = 0;
        if which != "loop" {
            v[2].c = 5 * 16;
        }
        v
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let variant = args.get(1).map_or("match", String::as_str);
    let which = args.get(2).map_or("loop", String::as_str);
    let n: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    // cv0 = $i, cv1 = $n, cv2 = $a, cv3 = $b, cv4 = $x, slot 5 = a temporary
    let mut slots: Vec<Slot> = vec![
        None,
        Some(Value::Int(black_box(n))),
        Some(Value::Int(black_box(20))),
        Some(Value::Int(black_box(22))),
        None,
        None,
    ];
    let lits = vec![Value::Int(black_box(0))];
    // The flags are set from outside in a real VM (signals, timeouts): keep
    // the compiler from proving they are always false.
    INTERRUPT.store(black_box(false), Relaxed);
    generic::SAFE_POINT.store(black_box(false), Relaxed);
    let r = match variant {
        "generic" => {
            let mut f = generic::Frame { cvs: slots.clone(), tmps: vec![Value::Null], symtab_attached: black_box(false) };
            let ops = generic::program(which);
            generic::run(&mut f, black_box(&ops), &lits)
        }
        "match" | "fused" => {
            let code = program(which, variant == "fused");
            unsafe { matched::run(black_box(code.as_ptr()), slots.as_mut_ptr(), lits.as_ptr()) }
        }
        "call" => {
            let code = threaded::cprogram(which);
            unsafe { threaded::run_call(black_box(code.as_ptr()), slots.as_mut_ptr(), lits.as_ptr()) };
            match &slots[if which == "loop" { 0 } else { 4 }] {
                Some(Value::Int(v)) => *v,
                _ => -1,
            }
        }
        "tail2" => {
            let code = tail2::program(which);
            let ip = black_box(code.as_ptr());
            unsafe { ((*ip).h)(ip, slots.as_mut_ptr().cast(), lits.as_ptr()) }
        }
        _ => {
            let code = threaded::program(which);
            let ip = black_box(code.as_ptr());
            unsafe { ((*ip).h)(ip, slots.as_mut_ptr(), lits.as_ptr()) }
        }
    };
    println!("{variant} {which} n={n} result={r}");
}
