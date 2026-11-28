// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for calls of user functions: php-src's call protocol.
//!
//! php-src prepares a call's frame when the call starts: `ZEND_INIT_FCALL`
//! reserves the callee's `zend_execute_data` on the VM stack, each
//! `ZEND_SEND_VAL` / `ZEND_SEND_VAR` writes its argument straight into the
//! callee's compiled-variable slot, and `ZEND_DO_UCALL` only links the frame
//! in and switches to it (ref: `Zend/zend_vm_def.h` `ZEND_INIT_FCALL`,
//! `ZEND_SEND_VAL`, `ZEND_DO_UCALL`; `Zend/zend_execute.h`
//! `zend_vm_stack_push_call_frame`).
//!
//! The generic protocol here builds a [`PendingCall`] record at
//! `InitFCall`, stages the arguments in it at `Send`, and at `DoCall`
//! re-initialises a pooled frame and moves the arguments across
//! (`Vm::push_simple_ucall`). The tape does what php-src does for the
//! calls that path accepts: a statically named user function, found in
//! the call site's run-time cache, whose declaration and arguments make a
//! plain call (`UserFunction::call_shape`). [`prepare`] readies the pooled
//! frame above the current one for the function at `InitFCall`, [`send`]
//! writes each argument into that frame's compiled variable, and [`enter`]
//! makes the frame current at `DoCall`.
//!
//! Calls of builtin functions by name follow php-src's `ZEND_DO_ICALL`
//! the same way: [`prepare`] takes the function from the call site's
//! cache, [`send`] writes each argument into the argument buffer of the
//! pooled frame above the current one (the builtin's call frame, which
//! backtraces read), and [`call_native`] pushes that frame, runs the
//! handler and pops it, all in the tape loop; a function with a frameless
//! class runs without the frame when its arguments allow it
//! (`ZEND_FRAMELESS_ICALL_n`, `Vm::frameless_icall_prepared`). Calls the generic builtin
//! call would treat specially (an argument it would coerce or reject, a
//! function with a by-reference parameter, a deprecated or `#[\NoDiscard]`
//! one, an observer) take the generic path.
//!
//! At most one call is prepared at a time, and only by the tape loop
//! (`run_tape`), which holds it in a local ([`Prepared`]). Whenever
//! anything else could see or change the caller's calls (another call
//! starting, an op the tape hands back to the generic code, the tape loop
//! returning to `Vm::run`, a safe point), the loop first turns the
//! prepared call into the [`PendingCall`] the generic protocol would have
//! built ([`materialize`]) and returns the frame to the pool, so the rest
//! of the VM never sees a half-prepared frame.

use super::{rel, Inst, TOp, SLOT_SHIFT};
use crate::call::{accepts_without_coercion, clone_value};
use crate::func::{
    flags, Accessor, Args, Builtin, CallShape, Func, PlainCall, UserFunction, FOLD_STEPS,
};
use crate::opcode::{
    ArgSrc, BinKind, ClassSpec, FetchMode, Op, OpArray, Operand, PlaceBase, PROP_CACHE_THIS,
};
use crate::place::propcache::{accepts, PC_DYN, PC_NO_SET, PC_WRITE};
use crate::vm::{FrameKind, PendingCall, RetTarget, Throw, Vm, VmResult};
use php_runtime::{BinaryOp, Obj, Value};
use std::rc::Rc;

/// The most arguments a prepared call takes: the pending call's inline
/// buffer (what [`materialize`] moves them into) and the precomputed
/// first ops of [`UserFunction::plain_first_op`].
const MAX_ARGS: usize = 6;

/// The specialised instruction of the op at `i` when this family has one.
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    match code.ops.get(i)? {
        // A builtin whose one-argument call needs no handler, called with
        // one computed argument: the function's id in `e`, its frameless
        // class in `d` (`prepare_quick`).
        Op::InitFCall { .. } if quick_call(code, i).is_some() => {
            let (id, class) = quick_call(code, i)?;
            Some(Inst {
                k: CALL_QUICK,
                e: id,
                d: class,
                ..plain(TOp::CallInit)
            })
        }
        // `new C` with nothing to evaluate before its constructor call
        // (`skip`: the op after that call): the class literal in `a`, the
        // result temporary in `b` and the jump to `skip` in `c`, for the
        // core loop's own attempt (`new_plain`).
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip,
        } if *skip != 0 => Some(Inst {
            k: CALL_NEW,
            a: *lit,
            b: *dst,
            c: rel(code, i, *skip)?,
            ..plain(TOp::CallInit)
        }),
        // `new C(arguments)` whose arguments are values at hand
        // ([`new_args_end`]): as above, the jump going to the op after the
        // constructor call, with `e` set and the op's own index in `d`
        // (the handler reads the arguments from the ops after it).
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip: 0,
        } if new_args_end(code, i).is_some() => Some(Inst {
            k: CALL_NEW,
            e: 1,
            a: *lit,
            b: *dst,
            c: rel(code, i, new_args_end(code, i)?)?,
            d: u32::try_from(i).ok()?,
            ..plain(TOp::CallInit)
        }),
        // A function called by name or a method called on an object, with
        // plain arguments: the site may get a call plan ([`call_plain`]).
        op @ (Op::InitFCall { .. }
        | Op::InitMethodCall {
            obj: Operand::Cv(_) | Operand::Tmp(_) | Operand::Unused,
            name: Operand::Lit(_),
        }
        | Op::InitStaticCall {
            class:
                ClassSpec::Named(_) | ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static,
            name: Operand::Lit(_),
        }) if preparable(op) && stays_prepared(code, i) && plan_sends(code, i).is_some() => {
            Some(Inst {
                k: CALL_PLAN,
                ..plain(TOp::CallInit)
            })
        }
        op if preparable(op) && stays_prepared(code, i) => Some(plain(TOp::CallInit)),
        // `$obj->$name()`: started on the site's method cache hit
        // (`spec::init_dyn_method_call_in`, from `run_call`) as a pending
        // call, not left to `Vm::run`.
        Op::InitMethodCall {
            name: Operand::Cv(_) | Operand::Tmp(_),
            ..
        } => Some(plain(TOp::CallInit)),
        // `$c::name()`: a declared class's public static method
        // (`init_dyn_static_call`, from `run_call`).
        Op::InitStaticCall {
            class: ClassSpec::Dyn(Operand::Cv(_) | Operand::Tmp(_)),
            name: Operand::Lit(_),
        } => Some(plain(TOp::CallInit)),
        // `call_user_func[_array]()` written by name (`ZEND_INIT_USER_CALL`),
        // and the arguments it passes (`ZEND_SEND_USER`, `ZEND_SEND_ARRAY`):
        // prepared on the call site's cache hit ([`prepare_user_call`]).
        Op::InitUserCall { .. } if user_call_stays_prepared(code, i) => Some(plain(TOp::CallInit)),
        op @ (Op::SendUser { .. } | Op::SendArray { .. })
            if user_send_src(op).is_some()
                && call_start_at(code, i).is_some_and(|s| {
                    let start = &code.ops[s];
                    (matches!(start, Op::InitUserCall { .. }) && user_call_stays_prepared(code, s))
                        || (matches!(start, Op::InitFCall { .. })
                            && preparable(start)
                            && stays_prepared(code, s))
                }) =>
        {
            Some(plain(TOp::CallSend))
        }
        // `new` is prepared as before whatever its arguments: its prepared
        // start also makes the object (`new_object`).
        op @ Op::New { .. } if preparable(op) => Some(plain(TOp::CallInit)),
        // `ZEND_FRAMELESS_ICALL_n` (the forms `strs::lower` left): run with
        // the VM at hand (`run_vm_op`).
        Op::FramelessICall(_) => Some(plain(TOp::VmOp)),
        // `ZEND_RECV_VARIADIC` (`Vm::recv_variadic_plain`, out of line in
        // `run_vm_op`: an arm in the tape's hot block cost every call).
        Op::RecvVariadic { .. } => Some(plain(TOp::VmOp)),
        // A read of a superglobal (`$_GET`) or of a global variable by
        // name (`$GLOBALS['name']`): resolved through the VM's global
        // scope, so run there as well (`Vm::fetch_place_fast`,
        // `Vm::fetch_global_fast`).
        Op::Fetch {
            place,
            mode: FetchMode::Read,
            ..
        } if auto_global_place(code, *place) || static_dim_place(code, *place) => {
            Some(plain(TOp::VmOp))
        }
        // `$a instanceof $b` of two variables (`Vm::instanceof_cv_fast`).
        Op::InstanceOf {
            a: Operand::Cv(_),
            class: ClassSpec::Dyn(Operand::Cv(_)),
            ..
        } => Some(plain(TOp::VmOp)),
        // `self::class`, `static::class`, `parent::class`
        // (`Vm::class_name_fast`).
        Op::FetchClassName {
            class:
                ClassSpec::SelfClass
                | ClassSpec::Static
                | ClassSpec::Parent
                | ClassSpec::Dyn(Operand::Cv(_)),
            ..
        } => Some(plain(TOp::VmOp)),
        // `isset(A::$p[k])` (`Vm::static_dim_fast`).
        Op::Isset { place, .. } if static_dim_place(code, *place) => Some(plain(TOp::VmOp)),
        Op::FetchGlobal { .. } => Some(plain(TOp::VmOp)),
        // `ZEND_ASSERT_CHECK`: reads `zend.assertions`, so run with the VM
        // at hand (`run_vm_op`) rather than leaving the tape.
        Op::AssertCheck { .. } => Some(plain(TOp::VmOp)),
        // An element of an object's property (`$o->p[k]` read, written,
        // compounded, incremented, tested): the property cache hits of
        // `exec/spec/props.rs`, which need the VM.
        Op::FetchObjPropDim { .. }
        | Op::AssignObjPropDim { .. }
        | Op::AssignOpObjPropDim { .. }
        | Op::IncDecObjPropDim { .. }
        | Op::IssetObjPropDim { .. } => Some(plain(TOp::VmOp)),
        // Only an argument of a call one of those started can go to a
        // prepared call; the others keep the generic `Send`, without the
        // detour.
        Op::Send { src }
            if direct_src(code, src)
                && call_start_at(code, i).is_some_and(|s| {
                    preparable(&code.ops[s])
                        && (matches!(code.ops[s], Op::New { .. }) || stays_prepared(code, s))
                }) =>
        {
            Some(plain(TOp::CallSend))
        }
        _ => None,
    }
}

/// The specialised instruction of a function-entry op that binds a
/// variable with the VM at hand (`run_vm_op`): `global $name`
/// (`ZEND_BIND_GLOBAL`), `static $x` (`ZEND_BIND_STATIC`) and a variadic
/// parameter's `ZEND_RECV_VARIADIC`. Each left the tape for the generic
/// loop and came back, once per name; a large framework application's
/// page runs 51,000 `global` binds a request.
pub(super) fn lower_entry(code: &OpArray, i: usize) -> Option<Inst> {
    let ncv = code.cv_names.len();
    match *code.ops.get(i)? {
        Op::Global { cv, .. } | Op::RecvVariadic { cv, .. } if (cv as usize) < ncv => {
            Some(Inst::plain(TOp::VmOp))
        }
        Op::BindStatic { cv, skip, .. }
            if (cv as usize) < ncv && (skip as usize) < code.ops.len() =>
        {
            Some(Inst::plain(TOp::VmOp))
        }
        _ => None,
    }
}

/// Whether the call started at `start` can be made from its preparation
/// to its `DoCall` without the tape giving it up: the ops in between are
/// argument sends it makes itself and hot ops, and none starts a call of
/// its own (an argument that is a call, `f(g($x))`, a send of another
/// kind or an op the tape leaves at flushes the prepared call into a
/// `PendingCall`, `materialize`, which cost more than the preparation
/// saved). Such calls start the generic way.
fn stays_prepared(code: &OpArray, start: usize) -> bool {
    /// Ops scanned at most (a call with many computed arguments starts
    /// the generic way).
    const SCAN: usize = 24;
    for op in code.ops.iter().skip(start + 1).take(SCAN) {
        match op {
            Op::DoCall { .. } => return true,
            Op::Send { src } if direct_src(code, src) => {}
            // `call_user_func[_array]('known_function', ...)`: the compiler
            // bound the name (a plain `InitFCall`), the arguments are still
            // `SendUser`s or a `SendArray` ([`send_user`], [`send_array`]).
            Op::SendUser { .. } | Op::SendArray { .. } if user_send_src(op).is_some() => {}
            Op::Send { .. }
            | Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. }
            | Op::CallableConvert { .. }
            | Op::CallablePartial { .. } => return false,
            op if crate::exec::hot::is_hot(op) => {}
            _ => return false,
        }
    }
    false
}

/// Whether `place` is `A::$name[k]`, one dimension of a static property
/// named by a literal, keyed by a literal, a variable or a temporary: run
/// with the VM
/// at hand (`Vm::static_dim_fast`), rather than leaving the tape.
fn static_dim_place(code: &OpArray, place: u32) -> bool {
    code.places.get(place as usize).is_some_and(|p| {
        matches!(
            (&p.base, p.segs.as_slice()),
            (
                PlaceBase::StaticProp {
                    name: Operand::Lit(_),
                    ..
                },
                [crate::opcode::Seg::Dim(
                    Operand::Lit(_) | Operand::Cv(_) | Operand::Tmp(_)
                )]
            )
        )
    })
}

/// [`stays_prepared`] of the call `Op::InitUserCall` started at `start`:
/// the ops up to its `DoCall` are hot ops and the arguments
/// [`send_user`] / [`send_array`] / [`send_array_slice`] may pass (a
/// `SendArray` of the whole array or of its `array_slice()`), and the elements of an array
/// literal the tape builds (`call_user_func_array($f, [$a, $b])`); no
/// other call starts in between.
fn user_call_stays_prepared(code: &OpArray, start: usize) -> bool {
    /// Ops scanned at most, as [`stays_prepared`]'s.
    const SCAN: usize = 24;
    for (j, op) in code.ops.iter().enumerate().skip(start + 1).take(SCAN) {
        match op {
            Op::DoCall { .. } => return true,
            Op::SendUser { .. } | Op::SendArray { .. } if user_send_src(op).is_some() => {}
            Op::Send { .. }
            | Op::SendUser { .. }
            | Op::SendArray { .. }
            | Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitUserCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. }
            | Op::CallableConvert { .. }
            | Op::CallablePartial { .. } => return false,
            Op::InitArray { .. } | Op::ArrayAdd { .. } if super::dims::lower(code, j).is_some() => {
            }
            op if crate::exec::hot::is_hot(op) => {}
            _ => return false,
        }
    }
    false
}

/// The operand a `SendUser`, or a `SendArray` of a whole array or of its
/// `array_slice()` (the length a literal, a temporary or a compiled
/// variable), passes when [`send_user`] / [`send_array`] /
/// [`send_array_slice`] can read it: a literal, a temporary or a compiled
/// variable.
fn user_send_src(op: &Op) -> Option<Operand> {
    let at_hand = |o: Operand| matches!(o, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_));
    let src = match *op {
        Op::SendUser { src, .. }
        | Op::SendArray {
            src,
            len: Operand::Unused,
            skip: 0,
        } => src,
        // `array_slice($args, skip, $len)` ([`send_array_slice`]).
        Op::SendArray { src, len, .. } if at_hand(len) => src,
        _ => return None,
    };
    at_hand(src).then_some(src)
}

/// Whether `place` is a variable named by a literal starting with an
/// underscore, read as a whole: what `Vm::fetch_place_fast` looks up among
/// the superglobals (any other name is not one, and its read stays the
/// generic op's).
fn auto_global_place(code: &OpArray, place: u32) -> bool {
    let Some(p) = code.places.get(place as usize) else {
        return false;
    };
    if !p.segs.is_empty() {
        return false;
    }
    match &p.base {
        PlaceBase::Global(Operand::Lit(lit)) => matches!(
            code.literals.get(*lit as usize),
            Some(Value::Str(name)) if name.as_bytes().first() == Some(&b'_')
        ),
        _ => false,
    }
}

/// The mark (`k`) of a `CallInit` instruction whose call [`prepare_quick`]
/// may take.
pub(super) const CALL_QUICK: u8 = 1;
/// The mark of a `CallInit` instruction that is a `new C` the core loop
/// tries itself ([`new_plain`]).
pub(super) const CALL_NEW: u8 = 2;

/// The mark of a `CallInit` instruction whose call site may have a call
/// plan ([`Plan`], [`call_plain`]).
pub(super) const CALL_PLAN: u8 = 3;

/// The most arguments a call plan passes.
const PLAN_ARGS: usize = 6;
/// The further classes a method's plan takes ([`Plan::more`]).
const PLAN_CLASSES: usize = 7;
/// [`Plan::kinds`]: the argument is a compiled variable of the caller.
const PLAN_CV: u8 = 0;
/// [`Plan::kinds`]: the argument is a literal of the caller's code.
const PLAN_LIT: u8 = 1;
/// [`Plan::kinds`]: the argument is a temporary of the caller, moved out.
const PLAN_TMP: u8 = 2;
/// [`Plan::kinds`]: the argument is a place of the caller read by value
/// (`$this->p`, `$a['k']`, a chain of them; [`plan_reads_place`]).
const PLAN_PLACE: u8 = 3;
/// [`Plan::kinds`]: the argument is `$this`.
const PLAN_THIS: u8 = 4;

/// What a planned call site is.
#[derive(Clone, PartialEq)]
enum PlanKind {
    /// A function called by name.
    Function,
    /// `$obj->name()`: the object operand.
    Method(Operand),
    /// `Name::name()` of a static method.
    Static,
    /// `self::name()`, `parent::name()`, `static::name()`.
    Relative(ClassSpec),
}

/// What a call site calls and how, worked out once: a function called
/// by name, a method called on an object or a static call, of a plain
/// declaration, each argument passed by value from a compiled variable,
/// a literal, a temporary, `$this` or a place read on its specialised
/// hit.
///
/// A prepared call decides the same things on every call, in each of the
/// functions it passes through (`prepare*`, `send_run`, `enter`, then
/// `return_simple`): of a plain call's ~870 instructions about 280 are
/// compares and branches and 50 are stores. A plan decides them when the
/// site first makes a prepared call, and [`call_plain`] then runs the
/// call as one sequence behind one guard (the object's class and the
/// calling scope for a method, nothing for a resolved function); the
/// frame it enters carries the mark ([`crate::vm::Frame::plain`]) that
/// lets `Vm::return_plain` release it without `return_simple`'s tests.
/// Whatever a plan does not take (an undefined or by-reference variable,
/// a value its parameter's type would coerce or refuse, an object of a
/// class the site has no plan for) is handed back untouched to the
/// prepared call, which keeps every diagnostic and rare shape.
///
/// A function call went from 868 to about 570 instructions with it, a
/// method call from 1,071 to about 740 (PHP 8.5: 132 and 178).
pub(super) struct Plan {
    /// What the site is.
    kind: PlanKind,
    /// The callee.
    uf: std::rc::Weak<UserFunction>,
    /// Its address, for the test against the function the pooled frame
    /// kept.
    uf_addr: usize,
    /// The number of arguments.
    n: u8,
    /// How many of them go into the callee's compiled variables: all but
    /// a variadic call's extras, which go to the frame's argument buffer
    /// for `RECV_VARIADIC` (after an empty entry for each bound one, the
    /// prepared call's layout).
    bound: u8,
    /// Whether a parameter declares a type (each argument is then tested
    /// against its parameter's).
    typed: bool,
    /// Whether the call passes fewer arguments than the callee declares
    /// (the others take their defaults, [`bind_literal_defaults`]).
    defaults: bool,
    /// Whether the method is static (no `$this`).
    is_static: bool,
    /// Whether an argument is read from a place or is `$this`
    /// ([`PLAN_PLACE`], [`PLAN_THIS`]).
    fetches: bool,
    /// Where each argument comes from.
    kinds: [u8; PLAN_ARGS],
    /// Its slot or literal index.
    idx: [u32; PLAN_ARGS],
    /// The caller's temporary the result goes to.
    dst: Option<u32>,
    /// The callee's first op after the `RECV`s of the passed arguments.
    first_op: usize,
    /// The caller's op after the `DoCall`.
    done: usize,
    /// A method's or a relative static call's: the address of the class
    /// the plan is for (the object's, or the one the spec resolved to)
    /// and of the calling scope.
    class_addr: usize,
    /// See `class_addr`.
    scope_addr: usize,
    /// Further classes whose objects the site saw and whose method is the
    /// same one (an inherited method called on several subclasses), held
    /// weakly so that their addresses stay theirs.
    more: [std::cell::RefCell<Option<std::rc::Weak<crate::class::Class>>>; PLAN_CLASSES],
    /// A method's class scope.
    method_scope: Option<std::rc::Weak<crate::class::Class>>,
    /// The called class of `Name::name()`.
    called: Option<std::rc::Weak<crate::class::Class>>,
}

/// What [`plan_store`] is told about a site's callee.
struct PlanCallee {
    kind: PlanKind,
    uf: Rc<UserFunction>,
    is_static: bool,
    class_addr: usize,
    scope_addr: usize,
    method_scope: Option<std::rc::Weak<crate::class::Class>>,
    called: Option<std::rc::Weak<crate::class::Class>>,
}

/// The arguments of the call started at `i`, when a plan can pass them:
/// each a compiled variable, a literal or a temporary, by value, at most
/// [`PLAN_ARGS`], then the `DoCall`.
#[allow(clippy::type_complexity)]
fn plan_sends(
    code: &OpArray,
    i: usize,
) -> Option<(usize, [u8; PLAN_ARGS], [u32; PLAN_ARGS], Option<u32>)> {
    let mut kinds = [0u8; PLAN_ARGS];
    let mut idx = [0u32; PLAN_ARGS];
    let mut n = 0;
    loop {
        match code.ops.get(i + 1 + n)? {
            Op::DoCall { dst } => return Some((n, kinds, idx, *dst)),
            Op::Send { src } if n < PLAN_ARGS => {
                let (k, x) = match src {
                    ArgSrc::Val(Operand::Cv(cv)) => (PLAN_CV, *cv),
                    ArgSrc::Val(Operand::Lit(l)) => (PLAN_LIT, *l),
                    ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
                        (PLAN_TMP, *t)
                    }
                    ArgSrc::Place(pid) => match place_cv(code, *pid) {
                        Some(cv) => (PLAN_CV, cv),
                        None if this_place(code, *pid) => (PLAN_THIS, 0),
                        None if plan_reads_place(code, *pid) => (PLAN_PLACE, *pid),
                        None => return None,
                    },
                    _ => return None,
                };
                kinds[n] = k;
                idx[n] = x;
                n += 1;
            }
            _ => return None,
        }
    }
}

/// Whether a plan may read place `pid` as an argument: a variable or
/// `$this` followed by dimensions keyed by literals or variables and
/// properties named by literals. Reading such a place on its specialised
/// hit ([`crate::vm::Frame::send_place_hit`]) consumes no temporary, so a
/// plan that then hands the call back has changed nothing.
fn plan_reads_place(code: &OpArray, pid: u32) -> bool {
    code.places.get(pid as usize).is_some_and(|p| {
        matches!(p.base, PlaceBase::Cv(_) | PlaceBase::This)
            && !p.segs.is_empty()
            && p.segs.iter().all(|seg| {
                matches!(
                    seg,
                    crate::opcode::Seg::Dim(Operand::Lit(_) | Operand::Cv(_))
                        | crate::opcode::Seg::Prop(Operand::Lit(_))
                )
            })
    })
}

/// Whether a plan can call `uf` with `n` arguments: a plain declaration
/// (typed or not) without static variables, every required parameter
/// passed and none past the declared ones.
fn plan_takes(uf: &UserFunction, n: usize) -> bool {
    let shape_ok = match uf.call_shape() {
        CallShape::UNTYPED | CallShape::TYPED => true,
        // A near-plain declaration (`UserFunction::near_plain_limit`: a
        // trailing variadic parameter, by-reference parameters past the
        // arguments passed): its `RECV_VARIADIC` makes the array of the
        // extras as the body runs (Laravel's `value($v)`, Doctrine's
        // `triggerIfCalledFromOutside($p, $l, $m, $x)`); the extras wait in
        // the frame's argument buffer (`Plan::bound`).
        CallShape::NOT_PLAIN => crate::call::near_plain(uf, n),
        _ => false,
    };
    shape_ok
        && uf.code.num_statics == 0
        && uf.flags & (flags::RETURN_REF | flags::GENERATOR) == 0
        && n >= uf.required as usize
        && (n <= uf.params.len() || uf.flags & flags::VARIADIC != 0)
        && uf.code.cv_names.len() >= uf.params.len()
        // (A variadic call's extras pass through the compiled variables
        // after the bound ones: `variadic_extras_of`.)
        && uf.code.cv_names.len() >= n
}

/// A call site's plan: unset until the site is looked at, then its plan
/// or none.
pub(super) type PlanSlot = std::cell::OnceCell<Option<Box<Plan>>>;

/// Gives every [`CALL_PLAN`] instruction of `tape` its site number in `p`
/// (the index of its [`PlanSlot`]); returns how many there are.
pub(super) fn number_plan_sites(tape: &mut [Inst]) -> usize {
    let mut n = 0;
    for inst in tape.iter_mut() {
        if inst.op == TOp::CallInit && inst.k == CALL_PLAN {
            inst.p = n;
            n += 1;
        }
    }
    n
}

/// The slot of plan site `site` of `code`'s tape.
#[inline(always)]
#[allow(clippy::inline_always)] // two loads
fn plan_slot(code: &OpArray, site: usize) -> Option<&PlanSlot> {
    code.tape.plans.get()?.get(site)
}

/// Whether plan site `site` has a plan: tested in line where the core
/// loop's call exit is handled, so that a site without one (an
/// accessor's, a builtin's) pays three loads and not a call.
#[inline(always)]
#[allow(clippy::inline_always)] // one test per call exit
pub(super) fn has_plan(code: &OpArray, site: usize) -> bool {
    plan_slot(code, site)
        .and_then(std::cell::OnceCell::get)
        .is_some_and(Option::is_some)
}

/// Whether plan site `site` has not been looked at for a plan yet.
#[inline]
pub(super) fn plan_missing(code: &OpArray, site: usize) -> bool {
    plan_slot(code, site).is_some_and(|slot| slot.get().is_none())
}

/// Stores the plan of the call site at `at` for `callee`, or the mark
/// that the site gets none.
#[cold]
#[inline(never)]
fn plan_store(code: &OpArray, at: usize, site: usize, callee: Option<PlanCallee>) {
    let Some(slot) = plan_slot(code, site) else {
        return;
    };
    let Some((n, kinds, idx, dst)) = plan_sends(code, at) else {
        return;
    };
    // A function the JIT may still compile whole (`compiled`, which only
    // the prepared call tries, counts its calls first): the site is
    // looked at again once that is settled, and a function that runs as
    // machine code gets no plan.
    if let Some(c) = &callee {
        let native = c.uf.call_shape.native.get();
        // (Only a call that passes every declared parameter is counted
        // towards that decision.)
        // (A variadic function is never compiled whole, and nothing
        // settles it: `compiled` declines it without refusing it. Waiting
        // for it left the site unplanned for good, its plan attempted on
        // every call.)
        if native == 0
            && super::jit::enabled()
            && n == c.uf.params.len()
            && n <= super::jit::NATIVE_MAX_ARGS
            && c.uf.flags & flags::VARIADIC == 0
        {
            return;
        }
        if native > super::jit::NATIVE_REFUSED {
            let _ = slot.set(None);
            return;
        }
    }
    let Some(c) = callee.filter(|c| plan_takes(&c.uf, n)) else {
        let _ = slot.set(None);
        return;
    };
    let mut plan = Plan {
        kind: c.kind.clone(),
        uf: std::rc::Weak::new(),
        uf_addr: 0,
        n: 0,
        bound: 0,
        typed: false,
        defaults: false,
        is_static: false,
        fetches: kinds[..n]
            .iter()
            .any(|k| matches!(*k, PLAN_PLACE | PLAN_THIS)),
        kinds,
        idx,
        dst,
        first_op: 0,
        done: at + n + 2,
        class_addr: 0,
        scope_addr: 0,
        more: Default::default(),
        method_scope: None,
        called: None,
    };
    plan.uf = Rc::downgrade(&c.uf);
    plan.uf_addr = Rc::as_ptr(&c.uf) as usize;
    // A variadic call's arguments past the declared parameters before the
    // variadic one are its extras.
    let bound = if c.uf.flags & flags::VARIADIC != 0 {
        n.min(c.uf.params.len().saturating_sub(1))
    } else {
        n
    };
    #[allow(clippy::cast_possible_truncation)] // at most `PLAN_ARGS`
    {
        plan.n = n as u8;
        plan.bound = bound as u8;
    }
    plan.typed = match c.uf.call_shape() {
        CallShape::TYPED => true,
        // (A near-plain declaration: whether a declared parameter is
        // typed, `UserFunction::near_typed`.)
        CallShape::NOT_PLAIN => c.uf.near_typed(),
        _ => false,
    };
    plan.defaults = n < c.uf.params.len();
    plan.is_static = c.is_static;
    plan.first_op = c.uf.plain_first_op(n);
    plan.class_addr = c.class_addr;
    plan.scope_addr = c.scope_addr;
    plan.method_scope = c.method_scope;
    plan.called = c.called;
    // (Kept out of `memory_get_usage()`, like the other run-time caches.)
    let _ = slot.set(Some(php_memlimit::persistent(|| Box::new(plan))));
}

/// Works out the plan of the function call site at `at` once its
/// function cache is filled (the site's first call fills it the usual
/// way).
#[cold]
#[inline(never)]
pub(super) fn plan_build_fcall(code: &OpArray, at: usize, site: usize) {
    let Some(&Op::InitFCall { name, fallback, .. }) = code.ops.get(at) else {
        return;
    };
    let func = {
        let cache = code.fcall_cache.borrow();
        let by_name = || {
            cache
                .get(name as usize)
                .and_then(Option::as_ref)
                .and_then(crate::opcode::CachedFunc::get)
        };
        match fallback {
            None => by_name(),
            // As `init_ns_fcall_in` looks it up: the site's own slot (a
            // resolved global fallback), else the namespaced name's.
            Some(_) => match cache
                .get(code.literals.len() + at + 1)
                .and_then(Option::as_ref)
            {
                Some(entry) => entry.get(),
                None => by_name(),
            },
        }
    };
    match func {
        Some(Func::User(uf)) => plan_store(
            code,
            at,
            site,
            Some(PlanCallee {
                kind: PlanKind::Function,
                uf,
                is_static: false,
                class_addr: 0,
                scope_addr: 0,
                method_scope: None,
                called: None,
            }),
        ),
        Some(Func::Builtin(_)) => plan_store(code, at, site, None),
        // Not resolved yet: looked at again by the next call.
        None => {}
    }
}

/// Works out the plan of the method call site at `at` (object operand
/// `obj`) from its method cache entry for objects of the class at
/// `cls_addr` called from the scope at `scope_addr`.
#[cold]
#[inline(never)]
pub(super) fn plan_build_method(
    code: &OpArray,
    at: usize,
    site: usize,
    obj: Operand,
    cls_addr: usize,
    scope_addr: usize,
    lit: u32,
) {
    match crate::call::method_entry_hit(code, at, cls_addr, scope_addr, lit) {
        Some((Func::User(uf), scope, false)) => plan_store(
            code,
            at,
            site,
            Some(PlanCallee {
                kind: PlanKind::Method(obj),
                uf,
                is_static: false,
                class_addr: cls_addr,
                scope_addr,
                method_scope: scope.as_ref().map(Rc::downgrade),
                called: None,
            }),
        ),
        Some(_) => plan_store(code, at, site, None),
        None => {}
    }
}

/// Works out the plan of the static call site at `at` once its cache is
/// filled: `Name::name()` from the site's function cache slot
/// ([`prepare_static`]'s), `self::` / `parent::` / `static::name()` from
/// its method cache entry ([`prepare_relative_static`]'s).
#[cold]
#[inline(never)]
pub(super) fn plan_build_static(code: &OpArray, at: usize, site: usize) {
    let Some(Op::InitStaticCall {
        class,
        name: Operand::Lit(_),
    }) = code.ops.get(at)
    else {
        return;
    };
    if matches!(class, ClassSpec::Named(_)) {
        let cache = code.fcall_cache.borrow();
        let Some(entry) = cache
            .get(code.literals.len() + at + 1)
            .and_then(Option::as_ref)
        else {
            return;
        };
        let callee = match &entry.func {
            crate::opcode::CachedFuncKind::User(w) => w.upgrade().map(|uf| PlanCallee {
                kind: PlanKind::Static,
                uf,
                is_static: true,
                class_addr: 0,
                scope_addr: 0,
                method_scope: entry.scope.clone(),
                called: entry.called.clone(),
            }),
            crate::opcode::CachedFuncKind::Builtin(_) => None,
        };
        drop(cache);
        plan_store(code, at, site, callee);
        return;
    }
    let callee = {
        let cache = code.method_cache.borrow();
        let Some(e) = cache.get(at).and_then(Option::as_ref) else {
            return;
        };
        match &e.func.func {
            crate::opcode::CachedFuncKind::User(w) => w.upgrade().map(|uf| PlanCallee {
                kind: PlanKind::Relative(class.clone()),
                uf,
                is_static: e.is_static,
                class_addr: e.class_addr,
                scope_addr: e.scope_addr,
                method_scope: e.method_scope.clone(),
                called: None,
            }),
            crate::opcode::CachedFuncKind::Builtin(_) => None,
        }
    };
    plan_store(code, at, site, callee);
}

/// Whether the method plan `plan` of the site at `at` is also the plan of
/// objects of the class at `cls_addr`: one of the further classes it
/// took, or a class whose method (the site's method cache hit for it) is
/// the plan's, which then becomes one of them while there is room.
#[inline(never)]
fn plan_takes_class(plan: &Plan, code: &OpArray, at: usize, cls_addr: usize) -> bool {
    let Some(&Op::InitMethodCall {
        name: Operand::Lit(lit),
        ..
    }) = code.ops.get(at)
    else {
        return false;
    };
    let mut free = None;
    for slot in &plan.more {
        match slot.borrow().as_ref() {
            Some(w) if w.as_ptr() as usize == cls_addr && w.strong_count() > 0 => return true,
            Some(_) => {}
            None => free = free.or(Some(slot)),
        }
    }
    let Some(free) = free else {
        return false;
    };
    let Some((Func::User(uf), scope, false)) =
        crate::call::method_entry_hit(code, at, cls_addr, plan.scope_addr, lit)
    else {
        return false;
    };
    let same_scope = match (&scope, &plan.method_scope) {
        (Some(a), Some(b)) => Rc::as_ptr(a) == b.as_ptr(),
        (None, None) => true,
        _ => false,
    };
    if Rc::as_ptr(&uf) as usize != plan.uf_addr || !same_scope {
        return false;
    }
    // The class itself, to hold it weakly: the method cache entry now
    // first at the site is this class's.
    let cache = code.method_cache.borrow();
    match cache.get(at).and_then(Option::as_ref) {
        Some(e) if e.class_addr == cls_addr => {
            *free.borrow_mut() = Some(e.class.clone());
            true
        }
        _ => false,
    }
}

/// The call's `$this`, class scope and called class.
type PlanHandles = (
    Option<Obj>,
    Option<Rc<crate::class::Class>>,
    Option<Rc<crate::class::Class>>,
);

/// The handles of a static call's frame, as [`prepare_static`] and
/// [`prepare_relative_static`] set them: for `Name::name()` the cached
/// scope and called class; for `self::` / `parent::` / `static::name()`
/// the class the spec resolves to in the caller must be the plan's and
/// the calling scope too, a static method then takes the caller's called
/// class when that is a subclass of it (the three specs forward it), an
/// instance method the caller's `$this` when its class is one. `None`
/// for anything else (the call is then made the usual way).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `call_plain`
fn plan_static_handles(plan: &Plan, caller: &crate::vm::Frame) -> Option<PlanHandles> {
    let scope = match &plan.method_scope {
        Some(w) => Some(w.upgrade()?),
        None => None,
    };
    let PlanKind::Relative(spec) = &plan.kind else {
        let called = match &plan.called {
            Some(w) => Some(w.upgrade()?),
            None => None,
        };
        return Some((None, scope, called));
    };
    if caller.kind == FrameKind::Builtin {
        return None;
    }
    let cls = crate::call::relative_spec_class(caller, spec)?;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    if Rc::as_ptr(cls) as usize != plan.class_addr || scope_addr != plan.scope_addr {
        return None;
    }
    if plan.is_static {
        let late_bound = match caller.called.as_ref() {
            Some(c) if c.is_subclass_of(cls) => Rc::clone(c),
            _ => Rc::clone(cls),
        };
        return Some((None, scope, Some(late_bound)));
    }
    let this = caller.this.as_ref()?;
    let late_bound = crate::object::class_of(this);
    if !late_bound.is_subclass_of(cls) {
        return None;
    }
    Some((Some(this.clone()), scope, Some(Rc::clone(late_bound))))
}

/// The call of the planned site at `at` of `code`, the current frame's,
/// from its init to the callee's first op: the pooled frame readied for
/// the plan's function, each argument copied or moved into its compiled
/// variable, a method's `$this` and scopes set, the frame entered as
/// [`enter`] enters it. `false`, nothing changed, when the object or the
/// class is not the plan's, or an argument is not a defined value its
/// parameter takes as it is: the call is then made the usual way.
#[inline(never)]
pub(super) fn call_plain(vm: &mut Vm, code: &OpArray, at: usize, site: usize) -> bool {
    let Some(Some(plan)) = plan_slot(code, site).and_then(std::cell::OnceCell::get) else {
        return false;
    };
    if plan.fetches {
        return call_plain_fetching(vm, code, at, plan);
    }
    call_planned(vm, code, at, plan, None)
}

/// [`call_plain`] of a plan with arguments read from places or `$this`:
/// the values read wait in a buffer of this function's until every
/// argument is known to pass. (Out of line, so that a plan without such
/// arguments has no buffer to set up and to drop.)
#[inline(never)]
fn call_plain_fetching(vm: &mut Vm, code: &OpArray, at: usize, plan: &Plan) -> bool {
    let mut fetched: [Value; PLAN_ARGS] = std::array::from_fn(|_| Value::Null);
    call_planned(vm, code, at, plan, Some(&mut fetched))
}

/// [`call_plain`] of the site's plan `plan`. `fetched`: the buffer for
/// the values of arguments read from places or `$this`, for a plan that
/// has such (inlined into each of its two callers, so that the one
/// without a buffer carries nothing for them).
#[inline(always)]
#[allow(clippy::inline_always, clippy::too_many_lines)] // the call sequence, in one piece
fn call_planned(
    vm: &mut Vm,
    code: &OpArray,
    at: usize,
    plan: &Plan,
    mut fetched: Option<&mut [Value; PLAN_ARGS]>,
) -> bool {
    let live = vm.frames.live;
    if vm.observer.enabled || live == 0 || live > 50_000 {
        return false;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    // What the call's frame holds besides its arguments. A method's
    // object must be of the plan's class, called from its scope; its
    // handle is taken after the arguments are known to pass.
    let mut handles: Option<PlanHandles> = None;
    match &plan.kind {
        PlanKind::Function => {}
        PlanKind::Method(obj) => {
            let o = match *obj {
                Operand::Cv(cv) => {
                    match caller
                        .slots
                        .cvs()
                        .get(cv as usize)
                        .and_then(crate::vm::CvSlot::get)
                    {
                        Some(Value::Object(o)) => o,
                        _ => return false,
                    }
                }
                Operand::Tmp(t) => match caller.slots.tmps().get(t as usize) {
                    Some(Value::Object(o)) => o,
                    _ => return false,
                },
                Operand::Unused => match caller.this.as_ref() {
                    Some(o) => o,
                    None => return false,
                },
                _ => return false,
            };
            let cls = crate::object::class_of(o);
            let cls_addr = Rc::as_ptr(cls) as usize;
            let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
            if plan.scope_addr != scope_addr
                || cls.hooks.as_ref().and_then(|h| h.get_method).is_some()
                || (plan.class_addr != cls_addr && !plan_takes_class(plan, code, at, cls_addr))
            {
                return false;
            }
        }
        PlanKind::Static | PlanKind::Relative(_) => match plan_static_handles(plan, caller) {
            Some(h) => handles = Some(h),
            None => return false,
        },
    }
    let n = plan.n as usize;
    // Every argument a defined value held as it is, which its parameter
    // takes as it is.
    let typed = if plan.typed {
        match plan.uf.upgrade() {
            Some(uf) => Some(uf),
            None => return false,
        }
    } else {
        None
    };
    for i in 0..n {
        let v = match plan.kinds[i] {
            PLAN_CV => match caller
                .slots
                .cvs()
                .get(plan.idx[i] as usize)
                .and_then(crate::vm::CvSlot::get)
            {
                Some(Value::Reference(_)) | None => return false,
                Some(v) => v,
            },
            PLAN_TMP => match caller.slots.tmps().get(plan.idx[i] as usize) {
                Some(Value::Reference(_)) | None => return false,
                Some(v) => v,
            },
            // Read now, moved in by the second pass.
            PLAN_PLACE | PLAN_THIS => {
                let Some(fetched) = fetched.as_deref_mut() else {
                    return false;
                };
                let v = if plan.kinds[i] == PLAN_THIS {
                    caller.this.as_ref().map(|o| Value::Object(o.clone()))
                } else {
                    caller.send_place_hit(plan.idx[i])
                };
                match v {
                    Some(v) if !matches!(v, Value::Reference(_)) => {
                        fetched[i] = v;
                        &fetched[i]
                    }
                    _ => return false,
                }
            }
            _ => match code.literals.get(plan.idx[i] as usize) {
                Some(v) => v,
                None => return false,
            },
        };
        if let Some(uf) = &typed {
            if let Some(ty) = uf.params.get(i).and_then(|p| p.ty.as_ref()) {
                if !crate::call::param_accepts(ty, v) {
                    return false;
                }
            }
        }
    }
    if frame.released_direct
        && frame
            .ufunc
            .as_ref()
            .is_some_and(|f| Rc::as_ptr(f) as usize == plan.uf_addr)
    {
        frame.released_direct = false;
    } else {
        let uf = match typed {
            Some(uf) => uf,
            None => match plan.uf.upgrade() {
                Some(uf) => uf,
                None => return false,
            },
        };
        // (Out of line: the readying inlined here cost the call sequence
        // more in register spills than its prologue.)
        ready_frame_slow(frame, &uf);
    }
    // The frame is released: its compiled variables are undefined, and
    // each argument is stored over nothing (no old value to test and drop).
    // The first loop found every source defined.
    let slots = frame.slots.cvs_mut();
    for i in 0..n.min(slots.len()) {
        let idx = plan.idx[i] as usize;
        let v = match plan.kinds[i] {
            PLAN_CV => match caller.slots.cvs().get(idx).and_then(crate::vm::CvSlot::get) {
                Some(v) => clone_value(v),
                None => continue,
            },
            PLAN_TMP => match caller.slots.tmps_mut().get_mut(idx) {
                Some(t) => std::mem::take(t),
                None => continue,
            },
            PLAN_PLACE | PLAN_THIS => match fetched.as_deref_mut() {
                Some(fetched) => std::mem::take(&mut fetched[i]),
                None => continue,
            },
            _ => match code.literals.get(idx) {
                Some(v) => clone_value(v),
                None => continue,
            },
        };
        let slot = slots[i].as_mut();
        if slot.is_some() {
            // (Not reached: a readied frame's variables are undefined.)
            drop(slot.take());
        }
        // SAFETY: the slot is `None`, which owns nothing: writing over it
        // without a drop forgets nothing.
        unsafe { std::ptr::write(slot, Some(v)) };
    }
    if let PlanKind::Method(obj) = &plan.kind {
        // `$this`: the temporary's handle moved out, else a copy.
        let this = match *obj {
            Operand::Tmp(t) => match caller
                .slots
                .tmps_mut()
                .get_mut(t as usize)
                .map(std::mem::take)
            {
                Some(Value::Object(o)) => Some(o),
                _ => None,
            },
            Operand::Cv(cv) => match caller
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get)
            {
                Some(Value::Object(o)) => Some(o.clone()),
                _ => None,
            },
            _ => caller.this.clone(),
        };
        let called = this.as_ref().map(|o| Rc::clone(crate::object::class_of(o)));
        let scope = plan.method_scope.as_ref().and_then(std::rc::Weak::upgrade);
        handles = Some((this, scope, called));
    }
    if let Some((this, scope, called)) = handles {
        fill(&mut frame.this, this);
        fill(&mut frame.scope, scope);
        fill(&mut frame.called, called);
    }
    frame.ret = plan.dst.map_or(RetTarget::Discard, RetTarget::Tmp);
    frame.caller_strict = code.strict_types;
    frame.direct_argc = u32::from(plan.n);
    frame.ip = plan.first_op;
    if plan.defaults {
        if let Some(uf) = frame.ufunc.clone() {
            bind_literal_defaults(frame, &uf, n);
        }
    }
    frame.direct = true;
    frame.plain = true;
    // A variadic call's extras, put into the compiled variables after the
    // bound ones by the loop above, go to the argument buffer.
    if plan.bound < plan.n {
        variadic_extras_of(plan, frame);
    }
    caller.ip = plan.done;
    vm.frames.live = live + 1;
    true
}

/// The arguments of a planned variadic call past the declared parameters
/// (`Plan::bound` of them stay in their compiled variables), which
/// [`call_planned`] put into the compiled variables that follow (the plan
/// takes a call only when the callee has that many, `plan_takes`): moved
/// to the frame's argument buffer, after an empty entry for each bound
/// argument, where `RECV_VARIADIC` collects them (as after the prepared
/// call: `variadic_extras`), those variables left empty. The buffer is
/// not the plain release's to empty: the frame loses its mark
/// (`return_simple` releases it). Out of line: a call without extras pays
/// one test.
#[cold]
#[inline(never)]
fn variadic_extras_of(plan: &Plan, frame: &mut crate::vm::Frame) {
    let bound = usize::from(plan.bound);
    for _ in 0..bound {
        frame.args.push(None);
    }
    for i in bound..usize::from(plan.n) {
        let v = frame
            .slots
            .cvs_mut()
            .get_mut(i)
            .and_then(|slot| slot.as_mut().take());
        frame.args.push(v);
    }
    frame.plain = false;
}

/// The most sums [`new_stored`] computes between the sends of one `new`.
const NEW_SUMS: usize = 3;

/// The most `new` instructions of one tape with a site entry
/// ([`NewSite`]); the ones past it make their checks every time.
const NEW_SITES: usize = 256;

/// What [`new_plain`] answers the core loop and a trace: nothing done;
/// the object in the result temporary; or the object made in the block of
/// the one the assigned variable held ([`new_target`]), which holds it:
/// the assignment the instruction's jump goes to is done as well.
pub(super) const NEW_DECLINED: u8 = 0;
/// See [`NEW_DECLINED`].
pub(super) const NEW_MADE: u8 = 1;
/// See [`NEW_DECLINED`].
pub(super) const NEW_OVER: u8 = 2;

/// Where a [`CALL_NEW`] instruction's `p` holds the variable its result
/// is assigned to, plus one, above its site number.
const NEW_TO_SHIFT: u32 = usize::BITS / 2;

/// Gives every [`CALL_NEW`] instruction of `tape` its site number in `p`
/// (its index in the tape's `new_sites`) and, above it, the variable the
/// instruction its jump goes to assigns the result to ([`new_target`]).
pub(super) fn number_new_sites(tape: &mut [Inst]) {
    let mut next = 0;
    for i in 0..tape.len() {
        if tape[i].op == TOp::CallInit && tape[i].k == CALL_NEW {
            let to = assigned_cv(tape, i).map_or(0, |cv| cv as usize + 1);
            tape[i].p = next | (to << NEW_TO_SHIFT);
            next += 1;
        }
    }
}

/// The variable `$v` of `$v = new C(...)`: the instruction the jump of the
/// [`CALL_NEW`] instruction at `i` goes to (past the constructor call)
/// assigns the result temporary to it, the result unused otherwise.
fn assigned_cv(tape: &[Inst], i: usize) -> Option<u32> {
    let inst = tape.get(i)?;
    #[allow(clippy::cast_possible_wrap)]
    let off = i64::from(inst.c as i32) / i64::try_from(std::mem::size_of::<Inst>()).ok()?;
    let at = usize::try_from(i64::try_from(i).ok()?.checked_add(off)?).ok()?;
    let next = tape.get(at)?;
    (at > i && next.op == TOp::AssignT && next.b >> SLOT_SHIFT == inst.b)
        .then_some(next.a >> SLOT_SHIFT)
}

/// The site number of a [`CALL_NEW`] instruction.
#[inline]
fn site_no(inst: &Inst) -> usize {
    inst.p & ((1 << NEW_TO_SHIFT) - 1)
}

/// The variable a [`CALL_NEW`] instruction's result is assigned to at
/// once, when it is ([`number_new_sites`]).
#[inline]
pub(super) fn new_target(inst: &Inst) -> Option<u32> {
    u32::try_from(inst.p >> NEW_TO_SHIFT).ok()?.checked_sub(1)
}

/// What the full checks of [`new_stored`] found out about a `new`
/// instruction, kept for the next `new` there (`Tape::new_sites`): what
/// depends on the instruction alone (where its arguments come from) and
/// what depends on the class alone. A class's constructor, its slots and
/// what its properties take do not change.
#[derive(Default)]
pub(super) struct NewSite {
    /// The class (none yet: `Weak::new()`). The handle pins the class's
    /// address, so an equal address is this class.
    class: std::rc::Weak<crate::class::Class>,
    /// The constructor's parameters: the arguments passed, then those
    /// left to their literal defaults (`dflt`, [`literal_defaults`]).
    n: u8,
    /// The stores (`steps[..len]`).
    len: u8,
    /// A parameter or a property stored to has a type: the arguments'
    /// types are tested (`params`, the steps' `accept`).
    typed: bool,
    /// Where each argument comes from.
    srcs: [Src; MAX_ARGS],
    /// The builtin type bits each parameter's declared type takes as they
    /// are (`u32::MAX`: no type, or `mixed`).
    params: [u32; MAX_ARGS],
    /// The variable the result is assigned to at once ([`new_target`]),
    /// when the object may be made in the block of the one it holds
    /// ([`site_over`]): no argument is read from that variable (it would
    /// be the old object). `u32::MAX`: none.
    over: u32,
    steps: [SiteStep; FOLD_STEPS],
    /// The defaults of the parameters the `new` leaves out
    /// ([`Src::ParamDefault`]): scalars, which own nothing.
    dflt: [Value; MAX_ARGS],
    /// [`new_checked`] refused the class for what depends on it and on the
    /// instruction alone ([`refuse_site`]): later `new`s there leave at
    /// once.
    refused: bool,
}

/// Where an argument of a `new` comes from: what the `Send`s after the
/// instruction pass ([`sent_args`] reads the same ops every time).
#[derive(Clone, Copy, Default)]
enum Src {
    #[default]
    None,
    /// A literal of the op array.
    Lit(u32),
    /// What a compiled variable holds.
    Cv(u32),
    /// `a + b`, `a - b` or `a * b` of literals and compiled variables.
    Sum(BinaryOp, Operand, Operand),
    /// A parameter no argument is passed for: its literal default, kept in
    /// the site ([`NewSite::dflt`], by parameter).
    ParamDefault(u8),
}

/// An argument of a `new`, read where it is: a value the frame or the op
/// array holds, or a sum just computed. Nothing is owned.
#[derive(Clone, Copy)]
enum Arg<'a> {
    Held(&'a Value),
    Int(i64),
}

impl Arg<'_> {
    /// Whether a parameter with the type bits `mask` takes it as it is
    /// (`enter`'s test).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn passes(self, mask: u32) -> bool {
        match self {
            Arg::Held(v) => accepts_without_coercion(mask, v),
            Arg::Int(_) => mask & crate::types::mask::LONG != 0,
        }
    }

    /// Whether a property accepting the type bits `accept` takes it as it
    /// is (`propcache::accepts`).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn fits(self, accept: u32) -> bool {
        match self {
            Arg::Held(v) => accepts(accept, v),
            Arg::Int(_) => accept & crate::types::mask::LONG != 0,
        }
    }
}

/// One store of a [`NewSite`]: the parameter, the slot it goes to and the
/// builtin type bits the property takes as they are (`u32::MAX`: untyped).
#[derive(Clone, Copy, Default)]
struct SiteStep {
    param: u8,
    slot: u32,
    accept: u32,
}

impl NewSite {
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn steps(&self) -> &[SiteStep] {
        self.steps.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Whether every parameter's type takes its argument as it is and
    /// every property the argument stored in it.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn takes(&self, args: &[Arg<'_>]) -> bool {
        self.params
            .iter()
            .zip(args)
            .all(|(mask, a)| *mask == u32::MAX || a.passes(*mask))
            && self.steps().iter().all(|s| {
                args.get(usize::from(s.param))
                    .is_some_and(|a| a.fits(s.accept))
            })
    }

    /// The constructor's stores: each argument (`args`, by parameter) into
    /// its slot of `slots`, a new object's.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn store(&self, slots: &mut [Option<Value>], args: &[Arg<'_>]) {
        for step in self.steps() {
            if let (Some(cell), Some(arg)) = (
                slots.get_mut(step.slot as usize),
                args.get(usize::from(step.param)),
            ) {
                store_arg(cell, *arg);
            }
        }
    }

    /// The object: its slots copied from the class's defaults, then the
    /// arguments (`args`, by parameter) stored. `None` is not reached (the
    /// object is this VM's and nothing else holds its slots); nothing is
    /// left behind then.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn build(
        &self,
        cls: &Rc<crate::class::Class>,
        defaults: &[Option<Value>],
        args: &[Arg<'_>],
    ) -> Option<Obj> {
        let made = crate::object::new_object_from_defaults(Rc::clone(cls), defaults);
        let stored = match made
            .downcast_ref::<crate::object::ObjectData>()
            .map(|data| data.slots.try_borrow_mut())
        {
            Some(Ok(mut held)) => {
                self.store(&mut held, args);
                true
            }
            _ => false,
        };
        if stored {
            Some(made)
        } else {
            crate::object::discard_rejected(made);
            None
        }
    }
}

/// Stores `arg` in `cell`, a slot of an object just made from its class's
/// defaults: a default that owns something (a string, an array the class's
/// table holds too) is released, anything else written over. (No slot of
/// a new object is marked unset: there is no such mark to clear.)
#[inline(always)]
#[allow(clippy::inline_always)] // once per store
fn store_arg(cell: &mut Option<Value>, arg: Arg<'_>) {
    if !matches!(
        cell,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))
    ) {
        *cell = None;
    }
    let new = match arg {
        Arg::Int(i) => Value::Int(i),
        Arg::Held(v @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) => {
            // SAFETY: both are valid and distinct (an argument's place and
            // an object's slot), 16 bytes each, and a `Some` has its
            // value's layout (the tag is the value's own: the trace JIT's
            // `value_layout` test pins it). A scalar owns nothing, so its
            // bytes are its copy, and the slot holds nothing to release
            // (cleared above). The two safe forms were measured first, at
            // 55 and 45 instructions a store: the value rebuilt through a
            // jump on its tag, then written tag, padding and payload apart.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    std::ptr::from_ref(v).cast::<u8>(),
                    std::ptr::from_mut(cell).cast::<u8>(),
                    std::mem::size_of::<Value>(),
                );
            }
            return;
        }
        Arg::Held(v) => clone_value(v),
    };
    // What the slot holds now owns nothing.
    std::mem::forget(cell.replace(new));
}

/// Where the arguments of the `new` instruction `inst` come from, and how
/// many there are: [`sent_args`]'s reading of the ops after it, kept as
/// what to read. `None` for anything [`sent_args`] would not read the
/// same way at every `new` (a sum no send passes, another operator).
fn site_srcs(code: &OpArray, inst: &Inst) -> Option<([Src; MAX_ARGS], usize)> {
    let mut srcs = [Src::None; MAX_ARGS];
    let mut n = 0usize;
    if inst.e == 0 {
        return Some((srcs, n));
    }
    let mut sums = [(u32::MAX, Src::None); NEW_SUMS];
    let mut made = 0usize;
    for op in code.ops.get(inst.d as usize + 1..)? {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                let from = match src {
                    ArgSrc::Val(Operand::Lit(lit)) => Src::Lit(*lit),
                    ArgSrc::Val(Operand::Cv(cv)) => Src::Cv(*cv),
                    ArgSrc::Place(pid) => Src::Cv(place_cv(code, *pid)?),
                    ArgSrc::Val(Operand::Tmp(tmp)) => {
                        let sum = sums.iter_mut().find(|s| s.0 == *tmp)?;
                        sum.0 = u32::MAX;
                        sum.1
                    }
                    _ => return None,
                };
                *srcs.get_mut(n)? = from;
                n += 1;
            }
            Op::Bin {
                op: BinKind::Arith(kind @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)),
                a,
                b,
                dst,
            } => {
                *sums.get_mut(made)? = (*dst, Src::Sum(*kind, *a, *b));
                made += 1;
            }
            // The `DoCall`.
            _ => break,
        }
    }
    // Every sum was passed.
    sums.iter().all(|s| s.0 == u32::MAX).then_some((srcs, n))
}

/// The argument `src` names, read in frame `f` (a parameter's default in
/// `site`). `None` for what [`sent_args`] declines (an undefined variable,
/// a sum of anything but ints, an overflow) and for a variable held by
/// reference, whose value [`sent_args`] copies out.
#[inline(always)]
#[allow(clippy::inline_always)] // once per argument
fn arg_view<'a>(f: &'a crate::vm::Frame, site: &'a NewSite, src: Src) -> Option<Arg<'a>> {
    // A compiled variable first: the commonest argument.
    if let Src::Cv(cv) = src {
        return match f
            .slots
            .cvs()
            .get(cv as usize)
            .and_then(crate::vm::CvSlot::get)?
        {
            Value::Reference(_) => None,
            v => Some(Arg::Held(v)),
        };
    }
    Some(match src {
        Src::Lit(lit) => Arg::Held(f.code.literals.get(lit as usize)?),
        Src::Sum(kind, a, b) => {
            let (lhs, rhs) = (int_operand(f, a)?, int_operand(f, b)?);
            Arg::Int(match kind {
                BinaryOp::Add => lhs.checked_add(rhs)?,
                BinaryOp::Sub => lhs.checked_sub(rhs)?,
                BinaryOp::Mul => lhs.checked_mul(rhs)?,
                _ => return None,
            })
        }
        Src::ParamDefault(k) => Arg::Held(site.dflt.get(usize::from(k))?),
        Src::Cv(_) | Src::None => return None,
    })
}

/// The op after the constructor call of the `New` at `i` when the tape's
/// `new` instruction can stand for all of it ([`new_stored`]): every op up
/// to the `DoCall` is a `Send` of a literal, of a compiled variable or of
/// a sum computed here, or such a sum (`+`, `-` or `*` of variables and
/// literals into a temporary that a later `Send` passes). `None` for any
/// other argument list (a call, a ternary, a named or unpacked argument,
/// more than [`MAX_ARGS`] arguments).
fn new_args_end(code: &OpArray, i: usize) -> Option<u32> {
    let plain = |o: &Operand| matches!(o, Operand::Lit(_) | Operand::Cv(_));
    let mut sums = [u32::MAX; NEW_SUMS];
    let mut sends = 0usize;
    for (k, op) in code.ops.get(i + 1..)?.iter().enumerate() {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                match src {
                    ArgSrc::Val(Operand::Lit(_) | Operand::Cv(_)) => {}
                    ArgSrc::Place(pid) if place_cv(code, *pid).is_some() => {}
                    // A sum made here, passed once.
                    ArgSrc::Val(Operand::Tmp(t)) => {
                        *sums.iter_mut().find(|s| **s == *t)? = u32::MAX;
                    }
                    _ => return None,
                }
                sends += 1;
                if sends > MAX_ARGS {
                    return None;
                }
            }
            Op::Bin {
                op: BinKind::Arith(BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul),
                a,
                b,
                dst,
            } if plain(a) && plain(b) && *dst != u32::MAX => {
                *sums.iter_mut().find(|s| **s == u32::MAX)? = *dst;
            }
            // The call, every sum passed.
            Op::DoCall { dst: None } if sums.iter().all(|s| *s == u32::MAX) => {
                return u32::try_from(i + k + 2).ok();
            }
            _ => return None,
        }
    }
    None
}

/// `ZEND_NEW` on the call site's class cache hit, when nothing of it needs
/// a call: the object into the result temporary, and the jump over the
/// ops of the constructor call (the instruction's jump).
///
/// - A class without a constructor, `new C` without arguments: the object
///   (`props::new_plain`: its slots copied from the class's evaluated
///   defaults).
/// - A class whose constructor only stores its parameters in properties
///   ([`new_stored`]): the object with the arguments stored.
///
/// `false`, nothing changed, for anything else: a cache miss, any other
/// constructor, arguments to a class without one, or a class
/// `props::new_plain` leaves to `Vm::instantiate` (abstract, an enum, an
/// internal class with a creation hook, ...) - `run_call` then decides as
/// before. A trace calls this handler too (`jit::plan_new`).
///
/// The first case is the whole of this function, as it was before the
/// second existed (its call is one of `run_core`'s, and `$x = new Foo()` in
/// a loop is little more than it); the second lies behind a call
/// ([`new_filled`]).
///
/// # Safety
/// `inst` is a [`CALL_NEW`] instruction [`lower`] built for `frame`, the
/// current frame.
#[inline(never)]
pub(super) unsafe fn new_plain(frame: *mut crate::vm::Frame, inst: &Inst) -> u8 {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    if f.slots.tmps().len() <= inst.b as usize {
        return NEW_DECLINED;
    }
    if new_target(inst).is_some_and(|cv| new_over(f, inst, cv)) {
        return NEW_OVER;
    }
    let cls = {
        let Ok(cache) = f.code.class_cache.try_borrow() else {
            return NEW_DECLINED;
        };
        match cache.get(inst.a as usize).and_then(std::rc::Weak::upgrade) {
            Some(cls) => cls,
            None => return NEW_DECLINED,
        }
    };
    if cls.magic.construct.is_some() || inst.e != 0 {
        // SAFETY: the caller's contract; the borrows above have ended.
        return unsafe { new_filled(frame, inst, cls) };
    }
    let Some(o) = crate::props::new_plain(&cls) else {
        return NEW_DECLINED;
    };
    match f.slots.tmps_mut().get_mut(inst.b as usize) {
        Some(slot) => crate::exec::spec::put_tmp(slot, Value::Object(o)),
        // (Not reached: the slot was looked for above.)
        None => crate::object::discard_rejected(o),
    }
    NEW_MADE
}

/// `$v = new C(...)` where `$v` (`cv`) holds the last handle of an object
/// of the class the instruction's cache names: the new object made in
/// that block (`object::reuse_for_new`, which says when nothing can
/// tell), by the class the object itself holds (no handle of the class
/// is taken). A class with a constructor, or arguments: when the
/// constructor only stores them ([`site_over`]). `false`, nothing
/// changed, otherwise ([`new_plain`] goes on as without).
#[inline(never)]
fn new_over(f: &crate::vm::Frame, inst: &Inst, cv: u32) -> bool {
    let Some(Value::Object(held)) = f
        .slots
        .cvs()
        .get(cv as usize)
        .and_then(crate::vm::CvSlot::get)
    else {
        return false;
    };
    let Some(d) = held.downcast_ref::<crate::object::ObjectData>() else {
        return false;
    };
    let cls = &d.class;
    let named = f.code.class_cache.try_borrow().is_ok_and(|cache| {
        cache
            .get(inst.a as usize)
            .is_some_and(|named| std::ptr::eq(std::rc::Weak::as_ptr(named), Rc::as_ptr(cls)))
    });
    // (The commonest reason not to: the old object lives on elsewhere.)
    if !named || held.refcount() != 1 {
        return false;
    }
    let Ok(defaults) = cls.defaults.try_borrow() else {
        return false;
    };
    let Some(defaults) = defaults.as_deref() else {
        return false;
    };
    if cls.magic.construct.is_some() || inst.e != 0 {
        return !super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed)
            && site_over(f, inst, cv, held, d, defaults);
    }
    if !crate::props::plain_new_class(cls) {
        return false;
    }
    match crate::object::reuse_for_new(held, d, defaults, |_| {}) {
        Some(made) => {
            if made.buffered {
                crate::gc::reused(held, d, made.old, true, false);
            }
            true
        }
        None => false,
    }
}

/// [`new_over`] for a class whose constructor only stores its parameters
/// in properties, by the site's entry ([`NewSite`], when it is for this
/// class and allows it: `NewSite::over`): the arguments are read where
/// they are and checked as [`site_made`] checks them, the object is made
/// in the block of `held` with them stored, and the possible roots are
/// recorded in the order the call's return and the assignment would have
/// (each argument, the new object, then the old one's entry gone).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_over`
fn site_over(
    f: &crate::vm::Frame,
    inst: &Inst,
    cv: u32,
    held: &Obj,
    d: &crate::object::ObjectData,
    defaults: &[Option<Value>],
) -> bool {
    let Ok(sites) = f.code.tape.new_sites.try_borrow() else {
        return false;
    };
    let Some(site) = sites.get(site_no(inst)) else {
        return false;
    };
    if std::rc::Weak::as_ptr(&site.class) != Rc::as_ptr(&d.class) || site.over != cv {
        return false;
    }
    let mut args = [Arg::Int(0); MAX_ARGS];
    let n = usize::from(site.n);
    for (arg, src) in args.iter_mut().zip(&site.srcs).take(n) {
        let Some(view) = arg_view(f, site, *src) else {
            return false;
        };
        *arg = view;
    }
    let Some(args) = args.get(..n) else {
        return false;
    };
    if site.typed && !site.takes(args) {
        return false;
    }
    let Some(made) = crate::object::reuse_for_new(held, d, defaults, |slots| {
        site.store(slots, args);
    }) else {
        return false;
    };
    for arg in args {
        if let Arg::Held(v @ (Value::Array(_) | Value::Object(_) | Value::Reference(_))) = arg {
            crate::gc::possible_root(v);
        }
    }
    crate::gc::reused(held, d, made.old, made.buffered, true);
    true
}

/// [`new_plain`] for a class with a constructor, or with arguments: the
/// object [`new_stored`] makes, into the result temporary.
///
/// # Safety
/// As [`new_plain`].
// (Part of `new_plain`: one call less on the way to `new_stored`. The
// handle moves in: the call is the caller's last act.)
#[inline(always)]
#[allow(clippy::inline_always, clippy::needless_pass_by_value)]
unsafe fn new_filled(
    frame: *mut crate::vm::Frame,
    inst: &Inst,
    cls: Rc<crate::class::Class>,
) -> u8 {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(o) = new_stored(f, inst, &cls) else {
        return NEW_DECLINED;
    };
    match f.slots.tmps_mut().get_mut(inst.b as usize) {
        Some(slot) => crate::exec::spec::put_tmp(slot, Value::Object(o)),
        // (Not reached: `new_plain` found the slot.)
        None => crate::object::discard_rejected(o),
    }
    NEW_MADE
}

/// The int operand `o` of a sum between the sends of a `new` is: a
/// literal, or what a compiled variable holds itself.
fn int_operand(f: &crate::vm::Frame, o: Operand) -> Option<i64> {
    let v = match o {
        Operand::Lit(l) => f.code.literals.get(l as usize)?,
        Operand::Cv(cv) => f.slots.cvs().get(cv as usize)?.get()?,
        _ => return None,
    };
    match v {
        Value::Int(i) => Some(*i),
        _ => None,
    }
}

/// The object of `new C(arguments)` (the [`CALL_NEW`] instruction `inst` of
/// frame `f`, `cls` its class) when the constructor only stores its
/// parameters in properties ([`UserFunction::ctor_fold`]): made and
/// filled without the call, as the call would have left it.
///
/// - The arguments are what the `Send`s after the op would pass
///   ([`sent_args`]), then the scalar literal defaults of the parameters
///   left out ([`literal_defaults`]): every declared parameter has a
///   value, and no more are passed; each has a type that takes its value
///   as it is (`enter`'s test, `Vm::op_recv_default`'s for a default).
/// - Every store is checked before the object exists, as
///   `spec::props::assign_hit` checks it: the constructor's own property
///   cache entry for the place is filled for this class and the
///   constructor's scope, allows a plain write of a declared property and
///   takes the value as it is; the slot it names holds, in a new object,
///   a default that is replaced or nothing that `__set()` could claim.
///   (A first `new` of the class runs the call, which fills the entries.)
///   What of this depends on the class alone, and where the arguments come
///   from, is found out once per `new` instruction and class ([`NewSite`],
///   filled by [`new_checked`]); [`site_made`] then reads the arguments
///   where they are and stores them.
/// - What the call's return would have recorded is recorded
///   (`gc::frame_released`): each argument as a possible root, then the
///   object itself, which `new`'s result still holds.
///
/// `None`, nothing done, for anything else: an observer, another
/// constructor (not public, static, with other statements, with a
/// parameter left to a default that is not a scalar literal:
/// [`literal_defaults`]), a class `props::ctor_ref` refuses.
#[inline(never)]
fn new_stored(f: &crate::vm::Frame, inst: &Inst, cls: &Rc<crate::class::Class>) -> Option<Obj> {
    if super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let defaults = cls.defaults.try_borrow().ok()?;
    let defaults = defaults.as_deref()?;
    // The site's entry when it is for this class (what depends on the class
    // and on the instruction was found out when it was filled): its object,
    // or its refusal. Else the full checks.
    if let Ok(sites) = f.code.tape.new_sites.try_borrow() {
        if let Some(site) = sites
            .get(site_no(inst))
            .filter(|site| std::rc::Weak::as_ptr(&site.class) == Rc::as_ptr(cls))
        {
            if site.refused {
                return None;
            }
            if let Some(made) = site_made(f, site, cls, defaults) {
                return Some(made);
            }
        }
    }
    new_checked(f, inst, cls, defaults)
}

/// The object of a `new` whose site entry `site` ([`NewSite`]) is for
/// `cls` (and not a refusal), when the arguments are where the entry says
/// and have types the parameters and the properties take as they are.
/// `None`, nothing done, otherwise ([`new_checked`] then decides).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_stored`
fn site_made<'a>(
    f: &'a crate::vm::Frame,
    site: &'a NewSite,
    cls: &Rc<crate::class::Class>,
    defaults: &[Option<Value>],
) -> Option<Obj> {
    let mut args = [Arg::Int(0); MAX_ARGS];
    let n = usize::from(site.n);
    for (arg, src) in args.iter_mut().zip(&site.srcs).take(n) {
        *arg = arg_view(f, site, *src)?;
    }
    let args = args.get(..n)?;
    if site.typed && !site.takes(args) {
        return None;
    }
    let made = site.build(cls, defaults, args)?;
    for arg in args {
        if let Arg::Held(v) = arg {
            crate::gc::possible_root(v);
        }
    }
    crate::gc::root_ctor_result(&made);
    Some(made)
}

/// [`new_stored`]'s full checks, the object when they pass, and the site's
/// entry for the next `new` there (kept only when it answers for every
/// argument its type bits take: not when a value passed for being of the
/// class a class-typed property accepted last).
#[inline(never)]
fn new_checked(
    f: &crate::vm::Frame,
    inst: &Inst,
    cls: &Rc<crate::class::Class>,
    defaults: &[Option<Value>],
) -> Option<Obj> {
    // The constructor first: one with a body of its own is the commonest
    // reason to leave, and its answer is kept (`ctor_fold`).
    // What is refused here depends on the class and on the instruction
    // alone: the site keeps the answer (`refuse_site`).
    let Some((func, scope)) = crate::props::ctor_ref(cls) else {
        return refuse_site(f, inst, cls);
    };
    let Func::User(uf) = func else {
        return refuse_site(f, inst, cls);
    };
    let Some(fold) = uf.ctor_fold() else {
        return refuse_site(f, inst, cls);
    };
    let mut args = NewArgs::new();
    if inst.e != 0 {
        // (An undefined variable, a sum that overflows: this `new`'s.)
        sent_args(f, inst, &mut args)?;
    }
    // The parameters left out get their literal defaults, as their
    // `RECV_INIT`s would bind them. (How many are passed is the
    // instruction's: its sends.)
    let given = args.given().len();
    if given < uf.params.len() && literal_defaults(uf, given, &mut args).is_none() {
        return refuse_site(f, inst, cls);
    }
    let args = args.given();
    if args.len() != uf.params.len() {
        return refuse_site(f, inst, cls);
    }
    let mut site = NewSite {
        over: u32::MAX,
        class: Rc::downgrade(cls),
        n: u8::try_from(args.len()).ok()?,
        len: u8::try_from(fold.len()).ok()?,
        ..NewSite::default()
    };
    site.params.fill(u32::MAX);
    if uf.call_shape() == CallShape::TYPED {
        for (mask, p) in site.params.iter_mut().zip(&uf.params) {
            if let Some(ty) = p.ty.as_ref().filter(|ty| !ty.is_mixed()) {
                *mask = ty.mask;
                site.typed = true;
            }
        }
    }
    let params_take = site
        .params
        .iter()
        .zip(args)
        .all(|(mask, v)| *mask == u32::MAX || accepts_without_coercion(*mask, v));
    if !params_take {
        return None;
    }
    let class_addr = Rc::as_ptr(cls) as usize;
    // A value passed for being of the class a class-typed property
    // accepted last: the entry would not answer for it.
    let mut by_class = false;
    {
        let cache = uf.code.prop_cache.try_borrow().ok()?;
        for (step, at) in fold.iter().zip(&mut site.steps) {
            let entry = cache.get(step.place as usize)?;
            let v = args.get(step.param as usize)?;
            if entry.class_addr != class_addr
                || entry.scope_addr != scope
                || entry.base != PROP_CACHE_THIS
                || entry.flags & (PC_WRITE | PC_DYN) != PC_WRITE
            {
                return None;
            }
            if !accepts(entry.accept, v) {
                let stored_class = matches!(
                    v,
                    Value::Object(held) if entry.ok_class_addr != 0
                        && Rc::as_ptr(crate::object::class_of(held)) as usize
                            == entry.ok_class_addr
                );
                if !stored_class {
                    return None;
                }
                by_class = true;
            }
            match defaults.get(entry.slot as usize)? {
                Some(Value::Reference(_)) => return None,
                Some(_) => {}
                // An undefined slot is filled when `__set()` cannot claim
                // it: the class has none, or the property is typed (and,
                // in a new object, never unset).
                None if entry.flags & PC_NO_SET != 0 || entry.accept != u32::MAX => {}
                None => return None,
            }
            site.typed |= entry.accept != u32::MAX;
            *at = SiteStep {
                param: u8::try_from(step.param).ok()?,
                slot: entry.slot,
                accept: entry.accept,
            };
        }
    }
    let mut views = [Arg::Int(0); MAX_ARGS];
    for (view, v) in views.iter_mut().zip(args) {
        *view = Arg::Held(v);
    }
    let made = site.build(cls, defaults, views.get(..args.len())?)?;
    for v in args {
        crate::gc::possible_root(v);
    }
    crate::gc::root_ctor_result(&made);
    if !by_class && site_no(inst) < NEW_SITES {
        if let Some((srcs, n)) = site_srcs(&f.code, inst) {
            if let Ok(mut sites) = f.code.tape.new_sites.try_borrow_mut() {
                if n == given {
                    site.srcs = srcs;
                    // (The object is made: nothing here may decline. `k`
                    // is below `MAX_ARGS`, the room `NewArgs` has.)
                    for (k, v) in args.iter().enumerate().skip(given) {
                        if let (Some(src), Some(dflt), Ok(no)) =
                            (site.srcs.get_mut(k), site.dflt.get_mut(k), u8::try_from(k))
                        {
                            *src = Src::ParamDefault(no);
                            // (A default is a scalar: `literal_defaults`.)
                            *dflt = clone_value(v);
                        }
                    }
                    // (An argument read from the assigned variable would be
                    // the old object.)
                    site.over = new_target(inst)
                        .filter(|cv| {
                            !srcs.iter().take(n).any(|src| match src {
                                Src::Cv(c) => c == cv,
                                Src::Sum(_, a, b) => {
                                    *a == Operand::Cv(*cv) || *b == Operand::Cv(*cv)
                                }
                                Src::Lit(_) | Src::ParamDefault(_) | Src::None => false,
                            })
                        })
                        .unwrap_or(u32::MAX);
                    crate::rtcache::grow_slots(&mut sites, site_no(inst) + 1, NewSite::default);
                    if let Some(slot) = sites.get_mut(site_no(inst)) {
                        *slot = site;
                    }
                }
            }
        }
    }
    Some(made)
}

/// [`new_checked`]'s answer for a refusal that depends on the class `cls`
/// and on the instruction `inst` alone (the class's constructor, its
/// fold, the parameters the instruction's sends leave out): kept in the
/// site's entry ([`NewSite::refused`]) so that the next `new` there leaves
/// at once. Always `None`.
#[cold]
#[inline(never)]
fn refuse_site(f: &crate::vm::Frame, inst: &Inst, cls: &Rc<crate::class::Class>) -> Option<Obj> {
    let no = site_no(inst);
    if no < NEW_SITES {
        if let Ok(mut sites) = f.code.tape.new_sites.try_borrow_mut() {
            crate::rtcache::grow_slots(&mut sites, no + 1, NewSite::default);
            if let Some(slot) = sites.get_mut(no) {
                *slot = NewSite {
                    class: Rc::downgrade(cls),
                    refused: true,
                    // (`site_over` takes no entry for another variable.)
                    over: u32::MAX,
                    ..NewSite::default()
                };
            }
        }
    }
    None
}

/// The arguments the `Send`s after the `new` instruction `inst` of frame
/// `f` would pass, into `args`: a copy of a literal or of a variable's
/// value (dereferenced; an undefined variable declines, its send warns),
/// or a sum computed between them (ints, no overflow: any other operand
/// declines). `None`, nothing changed in the frame, when a send has no
/// such value or there are more than [`MAX_ARGS`].
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_stored`
fn sent_args(f: &crate::vm::Frame, inst: &Inst, args: &mut NewArgs) -> Option<()> {
    let code = &*f.code;
    let mut sums = [(u32::MAX, 0i64); NEW_SUMS];
    let mut made = 0usize;
    for op in code.ops.get(inst.d as usize + 1..)? {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                let v = match src {
                    ArgSrc::Val(Operand::Lit(lit)) => {
                        clone_value(code.literals.get(*lit as usize)?)
                    }
                    ArgSrc::Val(Operand::Cv(cv)) => cv_arg(f, *cv)?,
                    ArgSrc::Place(pid) => cv_arg(f, place_cv(code, *pid)?)?,
                    ArgSrc::Val(Operand::Tmp(tmp)) => {
                        Value::Int(sums.iter().find(|s| s.0 == *tmp)?.1)
                    }
                    _ => return None,
                };
                args.push(v)?;
            }
            Op::Bin {
                op: BinKind::Arith(kind),
                a,
                b,
                dst,
            } => {
                let (lhs, rhs) = (int_operand(f, *a)?, int_operand(f, *b)?);
                let sum = match kind {
                    BinaryOp::Add => lhs.checked_add(rhs)?,
                    BinaryOp::Sub => lhs.checked_sub(rhs)?,
                    BinaryOp::Mul => lhs.checked_mul(rhs)?,
                    _ => return None,
                };
                *sums.get_mut(made)? = (*dst, sum);
                made += 1;
            }
            // The `DoCall`.
            _ => break,
        }
    }
    Some(())
}

/// The defaults of the parameters `given..` of `uf`, a constructor a
/// `new` passes `given` arguments to, into `args`, when each is a scalar
/// literal its `RECV_INIT` binds and nothing else runs before the body:
/// from the op a call with `given` arguments starts at, every parameter
/// left out has a `RecvInit` of its own variable followed by the
/// `RecvDefault` of a literal (`Vm::op_recv_default` checks its type;
/// [`new_checked`] checks it against the parameter's as for an argument)
/// that the `RecvInit`'s `skip` jumps over, and the ops end where a call
/// passing every parameter starts (where the fold's stores begin). A
/// string or array default (it owns something, and the call's return
/// would release it), a constant expression or a required parameter
/// declines.
fn literal_defaults(uf: &UserFunction, given: usize, args: &mut NewArgs) -> Option<()> {
    let code = &uf.code;
    let mut at = uf.plain_first_op(given);
    for k in given..uf.params.len() {
        let n = u32::try_from(k).ok()?;
        let (
            Some(&Op::RecvInit { n: rn, cv, skip }),
            Some(&Op::RecvDefault {
                n: dn,
                cv: dcv,
                val: Operand::Lit(lit),
            }),
        ) = (code.ops.get(at), code.ops.get(at + 1))
        else {
            return None;
        };
        if rn != n || dn != n || cv != n || dcv != n || skip as usize != at + 2 {
            return None;
        }
        let v = code.literals.get(lit as usize)?;
        if !matches!(
            v,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
        ) {
            return None;
        }
        args.push(clone_value(v))?;
        at += 2;
    }
    (at == uf.plain_first_op(uf.params.len())).then_some(())
}

/// The arguments [`new_stored`] collects: at most [`MAX_ARGS`] values, of
/// which only those given may own something. They are released by a tag
/// test each and nothing else of the array is ever dropped (an array of
/// `Value`s has drop glue for every slot, and a store into one a
/// `drop_in_place` call for what it replaces: about 150 instructions
/// around one int argument).
struct NewArgs {
    vals: std::mem::ManuallyDrop<[Value; MAX_ARGS]>,
    n: usize,
}

impl NewArgs {
    #[inline(always)]
    #[allow(clippy::inline_always)] // the head of `new_stored`
    fn new() -> Self {
        NewArgs {
            vals: std::mem::ManuallyDrop::new(std::array::from_fn(|_| Value::Null)),
            n: 0,
        }
    }

    /// Adds `v`; `None` (and `v` released) when there is no room.
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per argument
    fn push(&mut self, v: Value) -> Option<()> {
        let slot = self.vals.get_mut(self.n)?;
        // The slot holds the `Null` it started with.
        std::mem::forget(std::mem::replace(slot, v));
        self.n += 1;
        Some(())
    }

    /// The arguments given so far.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn given(&self) -> &[Value] {
        self.vals.get(..self.n).unwrap_or(&[])
    }
}

impl Drop for NewArgs {
    #[inline(always)]
    #[allow(clippy::inline_always)] // every way out of `new_stored`
    fn drop(&mut self) {
        let n = self.n;
        for v in self.vals.iter_mut().take(n) {
            match std::mem::take(v) {
                // A scalar owns nothing.
                old @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                    std::mem::forget(old);
                }
                other => drop(other),
            }
        }
    }
}

/// [`cv_value`] where [`sent_args`] reads an argument: the copy of a
/// scalar made in place.
#[inline(always)]
#[allow(clippy::inline_always)] // once per argument
fn cv_arg(f: &crate::vm::Frame, cv: u32) -> Option<Value> {
    Some(
        match f
            .slots
            .cvs()
            .get(cv as usize)
            .and_then(crate::vm::CvSlot::get)?
        {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(i) => Value::Int(*i),
            Value::Float(x) => Value::Float(*x),
            Value::Reference(r) => r.get(),
            v => v.clone(),
        },
    )
}

/// The id and frameless class (`special::quick_id`) of the builtin the
/// `InitFCall` at `i` names, when its call is `f(<expression>)`: one
/// argument, a temporary, passed right before the `DoCall`, with no other
/// call started while it is computed. `None` for any other call.
fn quick_call(code: &OpArray, i: usize) -> Option<(u8, u32)> {
    let Op::InitFCall {
        name,
        fallback: None,
        ..
    } = code.ops.get(i)?
    else {
        return None;
    };
    let Value::Str(lc) = code.literals.get(*name as usize)? else {
        return None;
    };
    let quick = crate::special::quick_id(lc.as_bytes())?;
    let mut j = i + 1;
    loop {
        match code.ops.get(j)? {
            Op::Send {
                src: ArgSrc::Val(Operand::Tmp(_)) | ArgSrc::CallResult(Operand::Tmp(_)),
            } => {
                return matches!(code.ops.get(j + 1)?, Op::DoCall { .. }).then_some(quick);
            }
            // Another argument, or another call: not this shape.
            Op::Send { .. }
            | Op::SendNamed { .. }
            | Op::SendUnpack { .. }
            | Op::SendPlaceholder { .. }
            | Op::DoCall { .. }
            | Op::CallableConvert { .. }
            | Op::CallablePartial { .. }
            | Op::FramelessICall(_)
            | Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitUserCall { .. }
            | Op::SendUser { .. }
            | Op::SendArray { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => return None,
            _ => j += 1,
        }
    }
}

/// Whether a call started by `op` may be prepared ([`prepare`],
/// [`prepare_method`]).
fn preparable(op: &Op) -> bool {
    matches!(
        op,
        Op::InitFCall { .. }
            | Op::InitMethodCall {
                name: Operand::Lit(_),
                ..
            }
            | Op::InitStaticCall {
                class: ClassSpec::Named(_)
                    | ClassSpec::SelfClass
                    | ClassSpec::Parent
                    | ClassSpec::Static,
                name: Operand::Lit(_),
            }
            | Op::InitDynCall {
                callee: Operand::Cv(_),
            }
            | Op::New {
                class: ClassSpec::Named(_),
                ..
            }
    )
}

/// The index of the op that started the call the `Send` at `i` passes an
/// argument to:
/// the nearest call-starting op before it whose call is not finished
/// before `i` (calls nest: an argument's own calls start and end in
/// between). `None` when there is none (not expected).
fn call_start_at(code: &OpArray, i: usize) -> Option<usize> {
    let mut finished = 0usize;
    for (at, op) in code.ops.get(..i)?.iter().enumerate().rev() {
        match op {
            Op::DoCall { .. } | Op::CallableConvert { .. } | Op::CallablePartial { .. } => {
                finished += 1;
            }
            Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitUserCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => {
                if finished == 0 {
                    return Some(at);
                }
                finished -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Whether [`send`] can pass `src` (by value: a literal, a temporary, or a
/// compiled variable).
fn direct_src(code: &OpArray, src: &ArgSrc) -> bool {
    match src {
        ArgSrc::Val(Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        | ArgSrc::CallResult(Operand::Tmp(_)) => true,
        ArgSrc::Place(pid) => {
            place_cv(code, *pid).is_some() || read_place(code, *pid) || this_place(code, *pid)
        }
        ArgSrc::TempPlace(pid) => read_place(code, *pid),
        _ => false,
    }
}

/// Whether a place is `$this` itself (`method_exists($this, $m)`): sent by
/// value, the frame's object.
fn this_place(code: &OpArray, pid: u32) -> bool {
    code.places
        .get(pid as usize)
        .is_some_and(|p| matches!(p.base, PlaceBase::This) && p.segs.is_empty())
}

/// Whether place `pid` is of a shape [`send`] reads by value on a hit
/// ([`crate::vm::Frame::send_place_hit`]): a variable, `$this` or a
/// temporary followed by dimensions and literally named properties
/// (`$a['k']`, `$this->p`, `$this->p[$k]`, `$list[$i]->name`,
/// `f()['k']`), no `[]`.
fn read_place(code: &OpArray, pid: u32) -> bool {
    use crate::opcode::Seg;
    code.places.get(pid as usize).is_some_and(|p| {
        // `A::$p` / `static::$p` (`static::$classAttributes`): the static
        // property cache hit (`spec::statics::static_prop_value_in`).
        if let PlaceBase::StaticProp {
            name: Operand::Lit(_),
            ..
        } = p.base
        {
            return p.segs.is_empty();
        }
        matches!(
            p.base,
            PlaceBase::Cv(_) | PlaceBase::This | PlaceBase::Value(Operand::Tmp(_))
        ) && !p.segs.is_empty()
            && p.segs
                .iter()
                .all(|s| matches!(s, Seg::Dim(_) | Seg::Prop(Operand::Lit(_))))
    })
}

/// The compiled variable a place stands for when it is one without
/// segments (`f($x)` compiles the argument as a place: by reference when
/// the parameter is).
pub(crate) fn place_cv(code: &OpArray, pid: u32) -> Option<u32> {
    let p = code.places.get(pid as usize)?;
    match (&p.base, p.segs.is_empty()) {
        (PlaceBase::Cv(cv), true) => Some(*cv),
        _ => None,
    }
}

/// What a prepared call calls.
enum Callee {
    /// A user function, method or closure: its frame is readied, and the
    /// arguments go into its compiled variables.
    User(Rc<UserFunction>),
    /// A builtin function called by name ([`Builtin::plain_call`]): the
    /// arguments go into the pooled frame's argument buffer.
    Native(Rc<Builtin>, PlainCall),
    /// A builtin whose one-argument call needs no handler
    /// ([`prepare_quick`]): its id (`special::quick_id`) and the index of
    /// its `InitFCall`. Nothing is readied: the `Send` computes the result
    /// ([`quick_send`]), or the call becomes the generic pending call.
    Quick(u8, u32),
}

/// A call prepared by [`prepare`]: the function, how many arguments
/// [`send`] wrote into the frame, and how many pending calls the caller
/// had when it started (the place of this call among them).
pub(super) struct Prepared {
    callee: Callee,
    n: usize,
    /// How many arguments [`send`] takes: the declared parameters (any
    /// number into a builtin's variadic one), at most [`MAX_ARGS`].
    limit: usize,
    calls_len: usize,
    /// Started by `InitFCall` (the pending call's `direct`); otherwise a
    /// method, static method or closure call, whose `$this`, scopes and
    /// closure are in the prepared frame.
    fcall: bool,
    /// A call of a function by its name, with no `$this`, scopes or
    /// closure in the prepared frame: an `fcall`, or a builtin an
    /// unqualified call from namespaced code resolved to (`prepare_ns`,
    /// php-src's `ZEND_JMP_FRAMELESS` then takes the frameless form too).
    /// The frameless and fast-evaluated forms need it.
    by_name: bool,
    /// A dynamic call (the pending call's `dynamic`): a closure's (`$f()`),
    /// whose bound variables are copied into the frame at `DoCall`, or a
    /// callback's (`call_user_func()`, [`prepare_user_call`]), which has
    /// none unless it is a closure.
    closure: bool,
    /// Whether the callee frame's closure may have bound variables
    /// (`use`) for [`enter`] to copy in ([`bind_closure_vars`]): known
    /// by [`prepare_closure`], which has the closure's data at hand;
    /// assumed by the callback paths that set `closure`.
    binds: bool,
    /// The constructor call of `new` (the pending call's `is_ctor`).
    ctor: bool,
}

/// `ZEND_INIT_FCALL` of `Op::InitFCall { name, fallback: None, .. }` in the
/// current frame: when the call site's cache holds a user function whose
/// declaration makes a plain call (as `Vm::push_simple_ucall` requires),
/// the pooled frame above the current one is readied for it and the call
/// is prepared. `None`, having changed nothing, otherwise: the generic
/// `InitFCall` runs.
pub(super) fn prepare(vm: &mut Vm, name: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    // `push_user_call`'s stack-depth backstop is the generic path's.
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        // No pooled frame at this depth yet: the generic call pushes one.
        return None;
    };
    let func = {
        let cache = caller.code.fcall_cache.borrow();
        let entry = cache.get(name as usize)?.as_ref()?;
        // A builtin the tape does not call goes the generic way without
        // its handle being copied first.
        if let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func {
            if b.plain_call_upto().0 == PlainCall::No {
                return None;
            }
        }
        entry.get()?
    };
    let (target, limit) = match func {
        Func::User(uf) => {
            let limit = prepares(&uf)?;
            ready_frame(frame, &uf);
            (Callee::User(uf), limit)
        }
        Func::Builtin(b) => {
            // Plain up to the first by-reference parameter (`first_ref`):
            // `send` stops there, and an argument at it is sent by
            // reference (`send_ref`) or the call becomes the generic one.
            // (Worked out by the test above: read from its two cells.)
            let (kind, first_ref) = b.plain_call_known();
            // A pooled frame's argument buffer is empty.
            if kind == PlainCall::No || !frame.args.is_empty() {
                return None;
            }
            let limit = native_limit(&b).min(first_ref);
            (Callee::Native(b, kind), limit)
        }
    };
    Some(Prepared {
        callee: target,
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: true,
        by_name: true,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// `ZEND_INIT_NS_FCALL_BY_NAME` (an unqualified call from namespaced
/// code, `InitFCall` with a fallback) at `at`, when the call site's
/// run-time cache resolves it to a builtin the tape calls by name: looked
/// up as `init_ns_fcall_in` looks it up (the site's own slot `site`, where
/// a resolved global fallback is cached, else the namespaced name's), the
/// call is prepared as [`prepare`] prepares a builtin's, not direct (as
/// the generic op pushes it). `None`, nothing changed, otherwise (a user
/// function's call starts the generic way).
pub(super) fn prepare_ns(vm: &mut Vm, name: u32, site: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    // A pooled frame's argument buffer is empty.
    if !frame.args.is_empty() {
        return None;
    }
    let func = {
        let cache = caller.code.fcall_cache.borrow();
        let entry = match cache.get(site).and_then(Option::as_ref) {
            Some(entry) => entry,
            None => cache.get(name as usize)?.as_ref()?,
        };
        if let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func {
            if b.plain_call_upto().0 != PlainCall::Named {
                return None;
            }
        }
        entry.get()?
    };
    let b = match func {
        Func::Builtin(b) => b,
        Func::User(uf) => {
            let limit = prepares(&uf)?;
            ready_frame(frame, &uf);
            return Some(Prepared {
                limit,
                callee: Callee::User(uf),
                n: 0,
                calls_len: caller.calls.len(),
                fcall: false,
                by_name: false,
                closure: false,
                binds: false,
                ctor: false,
            });
        }
    };
    // Plain up to the first by-reference parameter, as in [`prepare`].
    let (kind, first_ref) = b.plain_call_known();
    if kind != PlainCall::Named {
        return None;
    }
    let limit = native_limit(&b).min(first_ref);
    Some(Prepared {
        callee: Callee::Native(b, PlainCall::Named),
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: true,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// How many arguments a prepared call of builtin `b` takes: past the
/// declared parameters only into a variadic one (else the generic call
/// reports the count), at most [`MAX_ARGS`]. Always inline: a call of its
/// own (10 instructions, still one when merely `#[inline]`) for a load
/// and a compare.
#[inline(always)]
#[allow(clippy::inline_always)] // once per prepared builtin call
fn native_limit(b: &Builtin) -> usize {
    if b.flags & flags::VARIADIC != 0 {
        MAX_ARGS
    } else {
        b.signature().params.len().min(MAX_ARGS)
    }
}

/// `ZEND_INIT_FCALL` of a call [`CALL_QUICK`] marks (`f(<expression>)` of
/// a builtin whose one-argument call needs no handler: `id`, of frameless
/// class `class`), `at` its index: when the call site's cache holds that
/// builtin (neither deprecated nor `#[\NoDiscard]`) and no observer is
/// installed, the call is prepared without readying anything. Its `Send`
/// makes it ([`quick_send`]). `None`, nothing changed, otherwise.
pub(super) fn prepare_quick(
    vm: &mut Vm,
    name: u32,
    id: u8,
    class: u32,
    at: usize,
) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let caller = vm.frames.last()?;
    {
        let cache = caller.code.fcall_cache.try_borrow().ok()?;
        let entry = cache.get(name as usize)?.as_ref()?;
        let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func else {
            return None;
        };
        if !crate::special::frameless_class_is(b.flags, class) {
            return None;
        }
    }
    Some(Prepared {
        callee: Callee::Quick(id, u32::try_from(at).ok()?),
        n: 0,
        limit: 1,
        calls_len: caller.calls.len(),
        fcall: true,
        by_name: true,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// The `Send` of a call [`prepare_quick`] prepared (at `at`, the `DoCall`
/// right after it): when the argument, a temporary, has a shape the
/// function accepts as it is (`special::quick_eval`), the result is
/// computed, the temporary consumed and the result stored in the
/// `DoCall`'s temporary (the caller's resume point saved as for the call:
/// `SAVE_OPLINE()`). Returns the op after the `DoCall`. `None`, nothing
/// changed, for any other argument: the call becomes the pending call and
/// the generic `Send` and `DoCall` run.
pub(super) fn quick_send(
    vm: &mut Vm,
    id: u8,
    src: &ArgSrc,
    ops: &[Op],
    at: usize,
) -> Option<usize> {
    let (ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t))) = src else {
        return None;
    };
    let Some(&Op::DoCall { dst }) = ops.get(at + 1) else {
        return None;
    };
    let caller = vm.frames.last_mut()?;
    if dst.is_some_and(|d| d as usize >= caller.slots.tmps().len()) {
        return None;
    }
    let result = crate::special::quick_eval(id, caller.slots.tmps().get(*t as usize)?)?;
    // The argument is consumed: a number, or a string for `ord()`.
    drop(std::mem::take(
        caller.slots.tmps_mut().get_mut(*t as usize)?,
    ));
    caller.ip = at + 2;
    match dst.and_then(|d| caller.slots.tmps_mut().get_mut(d as usize)) {
        Some(slot) => crate::exec::spec::put_tmp(slot, result),
        None => drop(result),
    }
    Some(at + 2)
}

/// Whether a call of `uf` by name or as a method may be prepared, and how
/// many arguments [`send`] then takes: a declaration that makes a plain
/// call ([`UserFunction::call_shape`]) takes its declared parameters; one
/// whose only differences from that are its static variables, a trailing
/// variadic parameter or by-reference parameters
/// ([`UserFunction::near_plain_limit`]) takes as many as that allows (the
/// arguments for a variadic parameter wait in the frame's compiled
/// variables from the variadic one's on, so no more than it has: [`enter`]
/// moves them to the argument buffer). One load and compare for every
/// plain function.
#[inline(always)]
#[allow(clippy::inline_always)] // every prepared call
fn prepares(uf: &UserFunction) -> Option<usize> {
    if uf.call_shape() != CallShape::NOT_PLAIN {
        return Some(uf.params.len().min(MAX_ARGS));
    }
    near_limit(uf)
}

/// [`prepares`] of a declaration of shape [`CallShape::NOT_PLAIN`].
#[cold]
#[inline(never)]
fn near_limit(uf: &UserFunction) -> Option<usize> {
    let most = uf.near_plain_limit()?;
    Some(most.min(MAX_ARGS).min(uf.code.cv_names.len()))
}

/// Stores `v` into a handle field (`$this`, a scope, the closure) of the
/// pooled frame a call is being readied in: the field is empty there
/// (`Frame::clear_for_reuse`), so the old value is tested, not dropped
/// blind. A plain assignment called `Option<Obj>`'s out-of-line drop glue
/// on the `None` it overwrote (~8 instructions and the call, twice per
/// closure call).
#[inline(always)]
#[allow(clippy::inline_always)] // up to four per prepared call
fn fill<T>(slot: &mut Option<T>, v: Option<T>) {
    debug_assert!(slot.is_none());
    if let Some(old) = std::mem::replace(slot, v) {
        drop(old);
    }
}

/// Readies the pooled frame `frame` for a call of `uf`, as
/// `Vm::push_simple_ucall` readies it at `DoCall` (`FrameStack::push_in_place`).
///
/// A frame the last call of the same function left
/// (`Frame::release_direct`, [`crate::vm::Frame::released_direct`]) is
/// ready as it is: its slot vectors are sized for the code, the variable
/// store, the pending calls and the `@` masks are empty, and it holds the
/// function's handles; only the mark is cleared (`zend_vm_stack_push_call_frame`
/// has nothing to set up either: `ZEND_DO_UCALL` initialises the rest).
/// That test inline, the rest ([`ready_frame_slow`]) out of line: it
/// spent ~55 instructions a call, most of them in its own prologue and
/// in `reinit_same_call` storing what was already there.
#[inline(always)]
#[allow(clippy::inline_always)] // one test per prepared call
fn ready_frame(frame: &mut crate::vm::Frame, uf: &Rc<UserFunction>) {
    if frame.released_direct && frame.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf)) {
        debug_assert!(frame.is_released_for(uf));
        frame.released_direct = false;
        return;
    }
    ready_frame_slow(frame, uf);
}

/// [`ready_frame`] of a frame in any other released state.
#[inline(never)]
fn ready_frame_slow(frame: &mut crate::vm::Frame, uf: &Rc<UserFunction>) {
    // Released by the return of another function's call (two thirds of
    // an application's calls): only the slot sizes, the store's names and
    // the handles change.
    if frame.released_direct && frame.ready_released_for(uf) {
        return;
    }
    debug_assert!(
        frame.this.is_none()
            && frame.scope.is_none()
            && frame.called.is_none()
            && frame.closure.is_none()
            && frame.calls.is_empty()
    );
    let code = &uf.code;
    // (A released frame's iterators and finally states hold nothing:
    // `Frame::clear_for_reuse`.)
    if frame.slots.cvs().len() == code.cv_names.len()
        && frame.slots.tmps().len() == code.num_tmps as usize
        && frame.iters.len() == code.num_iters as usize
        && frame.fins.len() == code.num_finally as usize
    {
        frame.reinit_same_call(code, FrameKind::Function);
    } else {
        frame.reinit_for_call(code, FrameKind::Function);
    }
    if !frame.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf)) {
        // The frame keeps the function across calls in this slot
        // (`Frame::clear_for_reuse`): only another function replaces it.
        if let Some(old) = frame.ufunc.replace(Rc::clone(uf)) {
            drop(old);
        }
        if let Some(old) = frame.func.replace(Func::User(Rc::clone(uf))) {
            drop(old);
        }
    }
}

/// The frame of a user method called from native code
/// (`zend_call_known_instance_method`: `offsetGet()`, `__toString()`,
/// `__get()`, a callback's method), pushed as the tape's prepared calls
/// push theirs: the pooled frame above the current one readied for `uf`
/// (nothing to do when the last call of the same function in that slot
/// left it, [`ready_frame`]), `$this`, the method's scope and the
/// object's class filled, the arguments moved into the compiled
/// variables, the `RECV`s of the passed ones skipped and the literal
/// defaults of the others bound, and the frame entered directly
/// (`Frame::direct`: its return releases it in one pass and keeps it
/// ready for the next such call). The caller (`Vm::enter_method_direct`)
/// has checked that the declaration makes a plain call, that `args` fits
/// it, has the declared types as they are, holds no reference, and that
/// no observer is active. Returns the frame's index (the nested
/// execution's base), or `None`, having changed nothing, when no pooled
/// frame is at hand or the stack is past its backstop.
pub(crate) fn push_method_from_native(
    vm: &mut Vm,
    this: &Obj,
    scope: Rc<crate::class::Class>,
    uf: &Rc<UserFunction>,
    args: &mut [Value],
) -> Option<usize> {
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let frame = vm.frames.vec.get_mut(live)?;
    if !frame.args.is_empty() {
        return None;
    }
    let n = args.len();
    ready_frame(frame, uf);
    fill(&mut frame.this, Some(this.clone()));
    fill(&mut frame.scope, Some(scope));
    fill(
        &mut frame.called,
        Some(Rc::clone(crate::object::class_of(this))),
    );
    for (slot, a) in frame.slots.cvs_mut().iter_mut().zip(args.iter_mut()) {
        // A readied frame's compiled variables are empty.
        if let Some(old) = slot.as_mut().replace(std::mem::take(a)) {
            drop(old);
        }
    }
    frame.ret = RetTarget::Discard;
    frame.caller_strict = false;
    frame.called_from_internal = true;
    frame.is_ctor = false;
    #[allow(clippy::cast_possible_truncation)] // at most the parameter count
    {
        frame.direct_argc = n as u32;
    }
    frame.ip = uf.plain_first_op(n);
    if n < uf.params.len() {
        bind_literal_defaults(frame, uf, n);
    }
    frame.direct = true;
    // A plain declaration (the caller's test): its return, the nested
    // execution's end, is the short one (`Vm::return_plain` at the base).
    frame.plain = uf.flags & flags::RETURN_REF == 0;
    vm.frames.live = live + 1;
    Some(live)
}

/// The frame of a plain user callee native code calls back
/// (`zend_call_function` for `array_map()`'s callback, `usort()`'s
/// comparator, ...), pushed as [`push_method_from_native`] pushes a
/// method's: the pooled frame above the current one readied for `uf`
/// ([`ready_frame`]), the call's `$this`, scope and called class, the
/// arguments moved into the compiled variables, the `RECV`s of the passed
/// ones skipped and the literal defaults of the others bound, a closure's
/// object and bound variables (with its `$this` and scopes where the call
/// has none, as `Vm::bind_closure_vars` binds them), and the frame entered
/// directly and marked plain (its return, the nested execution's end, is
/// the short one). The caller (`Vm::call_prepared_direct`) has checked the
/// declaration, the arguments and their types. `None`, nothing changed,
/// when no pooled frame is at hand, the stack is past its backstop, or the
/// closure carries a `__call` name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_callback_from_native(
    vm: &mut Vm,
    uf: &Rc<UserFunction>,
    this: Option<&Obj>,
    scope: Option<&Rc<crate::class::Class>>,
    called: Option<&Rc<crate::class::Class>>,
    closure: Option<&Obj>,
    strict: bool,
    args: &mut [Value],
) -> Option<usize> {
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    // The closure's data, borrowed once for the test and the binding.
    let internal = closure.map(|c| crate::object::data(c).internal.borrow());
    let cdata = match &internal {
        Some(r) => match r
            .as_ref()
            .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
        {
            Some(d) if d.magic_name.is_none() => Some(d),
            _ => return None,
        },
        None => None,
    };
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let frame = vm.frames.vec.get_mut(live)?;
    if !frame.args.is_empty() {
        return None;
    }
    let n = args.len();
    ready_frame(frame, uf);
    fill(&mut frame.this, this.cloned());
    fill(&mut frame.scope, scope.cloned());
    fill(&mut frame.called, called.cloned());
    for (slot, a) in frame.slots.cvs_mut().iter_mut().zip(args.iter_mut()) {
        // A readied frame's compiled variables are empty.
        if let Some(old) = slot.as_mut().replace(std::mem::take(a)) {
            drop(old);
        }
    }
    frame.ret = RetTarget::Discard;
    frame.caller_strict = strict;
    frame.called_from_internal = true;
    frame.is_ctor = false;
    #[allow(clippy::cast_possible_truncation)] // at most the parameter count
    {
        frame.direct_argc = n as u32;
    }
    frame.ip = uf.plain_first_op(n);
    if n < uf.params.len() {
        bind_literal_defaults(frame, uf, n);
    }
    if let (Some(c), Some(d)) = (closure, cdata) {
        fill(&mut frame.closure, Some(c.clone()));
        if frame.this.is_none() && d.this.is_some() {
            frame.this.clone_from(&d.this);
        }
        if frame.scope.is_none() && d.scope.is_some() {
            frame.scope.clone_from(&d.scope);
        }
        if frame.called.is_none() && d.called.is_some() {
            frame.called.clone_from(&d.called);
        }
        for (cv, v) in &d.vars {
            if let Some(slot) = frame.slots.cvs_mut().get_mut(*cv as usize) {
                if let Some(old) = slot.as_mut().replace(v.clone()) {
                    drop(old);
                }
            }
        }
    }
    frame.direct = true;
    frame.plain = uf.flags & flags::RETURN_REF == 0;
    vm.frames.live = live + 1;
    Some(live)
}

/// `ZEND_INIT_METHOD_CALL` of `Op::InitMethodCall { obj, name: Lit(lit) }`
/// at `slot` in the current frame: on the method cache hit
/// `Vm::init_method_call_fast` takes (the object's class and the calling
/// scope as cached), when the method is a user function whose declaration
/// makes a plain call, the pooled frame above the current one is readied
/// for it with its `$this` (moved out of a temporary, else a copy; none
/// for a static method) and scopes, and the call is prepared. `None`,
/// having changed nothing, otherwise.
pub(super) fn prepare_method(
    vm: &mut Vm,
    obj: Operand,
    lit: u32,
    slot: usize,
    resolved: Option<MethodEntry>,
) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let o = match obj {
        Operand::Cv(cv) if !caller.symtab_attached => {
            match caller
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get)
            {
                Some(Value::Object(o)) => o,
                _ => return None,
            }
        }
        Operand::Tmp(t) => match caller.slots.tmps().get(t as usize) {
            Some(Value::Object(o)) => o,
            _ => return None,
        },
        Operand::Unused => caller.this.as_ref()?,
        _ => return None,
    };
    let cls = crate::object::class_of(o);
    if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
        return None;
    }
    let cls_addr = Rc::as_ptr(cls) as usize;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    // (`resolved`: the same lookup, made by `accessor_now` just before.)
    let (func, scope, is_static) = match resolved {
        Some(found) => found,
        None => crate::call::method_entry_hit(&caller.code, slot, cls_addr, scope_addr, lit)?,
    };
    let uf = match func {
        Func::User(uf) => uf,
        Func::Builtin(b) => {
            let called_class = Rc::clone(cls);
            return prepare_native_method(caller, frame, obj, b, scope, is_static, called_class);
        }
    };
    let limit = prepares(&uf)?;
    let called_class = Rc::clone(cls);
    // The call's `$this`, as `Vm::init_method_call_fast` takes it: the
    // temporary's handle moved out, or a copy of the variable's object;
    // a static method takes none (the temporary's handle is released
    // after the frame is readied).
    let (this, released) = match obj {
        Operand::Tmp(t) => match caller
            .slots
            .tmps_mut()
            .get_mut(t as usize)
            .map(std::mem::take)
        {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        _ => (Some(o.clone()), None),
    };
    ready_frame(frame, &uf);
    fill(&mut frame.this, this);
    fill(&mut frame.scope, scope);
    fill(&mut frame.called, Some(called_class));
    let calls_len = caller.calls.len();
    drop(released);
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len,
        fcall: false,
        by_name: false,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// `$obj->name(...)` of a pure builtin method ([`flags::PURE`]),
/// `ZEND_INIT_METHOD_CALL` at `at` followed by `Send`s of literals or
/// variables (by value, at most [`MAX_ARGS`]) and its `DoCall`: on the
/// method cache hit [`prepare_method`] takes, the handler is called at
/// once, without a pending call or a frame, with the arguments read in
/// place (each accepted by its parameter as it is, as [`call_native`]
/// checks them), the omitted parameters taking their declared defaults,
/// and the caller's resume point that of the `DoCall`. A temporary `$this`
/// is released after the call. Returns the op after the `DoCall`; `None`,
/// nothing changed that shows, otherwise (the handler's error is dropped,
/// and the call is made the usual way: its error and trace are the framed
/// call's).
pub(super) fn pure_method_now(
    vm: &mut Vm,
    ops: &[Op],
    obj: Operand,
    lit: u32,
    at: usize,
) -> Option<usize> {
    // (A pending interrupt is the usual call's to service, after it.)
    if vm.observer.enabled || crate::interrupt::safe_point_due() {
        return None;
    }
    let mut sends = [Operand::Unused; MAX_ARGS];
    // An argument a class constant gives (`ReflectionAttribute::IS_INSTANCEOF`):
    // the op index of its `FetchClassConst`, read from the op's cache.
    let mut consts: [Option<usize>; MAX_ARGS] = [None; MAX_ARGS];
    let mut n = 0;
    // The op after the init being looked at (an argument of a class
    // constant is two ops: its fetch into a temporary, then its `Send`).
    let mut next_op = at + 1;
    let (dst, done) = loop {
        match ops.get(next_op)? {
            Op::DoCall { dst } => break (*dst, next_op + 1),
            Op::Send {
                src: ArgSrc::Val(op @ (Operand::Lit(_) | Operand::Cv(_))),
            } if n < MAX_ARGS => {
                sends[n] = *op;
                n += 1;
                next_op += 1;
            }
            // `ZEND_SEND_VAR_EX` of a plain variable (a method's parameters
            // are not known when it is compiled): by value, the parameter
            // being none by reference (tested below).
            Op::Send {
                src: ArgSrc::Place(pid),
            } if n < MAX_ARGS => {
                let cv = place_cv(&vm.frames.last()?.code, *pid)?;
                sends[n] = Operand::Cv(cv);
                n += 1;
                next_op += 1;
            }
            // `A::NAME` sent as it is fetched: its temporary is written by
            // nothing else (the value is read from the fetch's cache below
            // and the temporary stays empty, as the `Send` leaves it).
            Op::FetchClassConst {
                class:
                    ClassSpec::Named(_) | ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static,
                name: Operand::Lit(_),
                dst: t,
            } if n < MAX_ARGS
                && matches!(
                    ops.get(next_op + 1),
                    Some(Op::Send { src: ArgSrc::Val(Operand::Tmp(s)) }) if s == t
                ) =>
            {
                consts[n] = Some(next_op);
                n += 1;
                next_op += 2;
            }
            _ => return None,
        }
    };
    let picked = {
        let caller = vm.frames.last()?;
        let o = match obj {
            Operand::Cv(cv) if !caller.symtab_attached => {
                match caller
                    .slots
                    .cvs()
                    .get(cv as usize)
                    .and_then(crate::vm::CvSlot::get)
                {
                    Some(Value::Object(o)) => o,
                    _ => return None,
                }
            }
            Operand::Tmp(t) => match caller.slots.tmps().get(t as usize) {
                Some(Value::Object(o)) => o,
                _ => return None,
            },
            _ => return None,
        };
        let cls = crate::object::class_of(o);
        if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
            return None;
        }
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let (func, _scope, is_static) =
            crate::call::method_entry_hit(&caller.code, at, cls_addr, scope_addr, lit)?;
        let Func::Builtin(b) = func else {
            return None;
        };
        if is_static || b.flags & flags::PURE == 0 || !pure_args(&b, n) {
            return None;
        }
        // A getter without arguments (`$m->getName()`): its result read
        // from the object, without the call's `Args` (`Builtin::getter`).
        let got = if n == 0 {
            b.getter().and_then(|getter| getter(o))
        } else {
            None
        };
        match got {
            Some(value) => Err(value),
            None => Ok((o.clone(), b, Rc::clone(cls))),
        }
    };
    let (o, b, cls) = match picked {
        Ok(found) => found,
        Err(value) => {
            vm.frames.last_mut()?.ip = done;
            return pure_method_done(vm, obj, dst, done, value);
        }
    };
    let sig = b.signature();
    let nparams = sig.params.len();
    let required = sig.required as usize;
    if n < required || (b.flags & flags::VARIADIC == 0 && n > nparams) {
        return None;
    }
    // (No parameter, the commonest getter: no buffer to borrow.)
    let (mut values, mut passed) = if nparams == 0 {
        (Vec::new(), Vec::new())
    } else {
        (
            vm.call_bufs.values.pop().unwrap_or_default(),
            vm.call_bufs.passed.pop().unwrap_or_default(),
        )
    };
    {
        let caller = vm.frames.last()?;
        for (i, op) in sends[..n].iter().enumerate() {
            let v = if let Some(ci) = consts[i] {
                match ops.get(ci) {
                    Some(Op::FetchClassConst {
                        class,
                        name: Operand::Lit(nl),
                        ..
                    }) => crate::rtcache::class_const_hit_in(caller, class, *nl),
                    _ => None,
                }
            } else {
                match *op {
                    Operand::Lit(l) => caller.code.literals.get(l as usize).map(clone_value),
                    Operand::Cv(cv) if !caller.symtab_attached => cv_value(caller, cv),
                    _ => None,
                }
            };
            let ok = v
                .as_ref()
                .zip(sig.params.get(i).or(sig.params.last()))
                .is_some_and(|(v, p)| {
                    !p.by_ref
                        && p.ty.as_ref().is_none_or(|ty| {
                            p.promote & crate::opcode::UNCHECKED_TYPE != 0
                                || ty.is_mixed()
                                || accepts_without_coercion(ty.mask, v)
                        })
                });
            match v {
                Some(v) if ok => {
                    values.push(v);
                    passed.push(true);
                }
                _ => {
                    if values.capacity() > 0 {
                        vm.put_val_vec(values);
                        vm.put_bool_vec(passed);
                    }
                    return None;
                }
            }
        }
    }
    while values.len() < nparams {
        let i = values.len();
        match vm.builtin_default(&b, i) {
            Some(d) if i >= required => {
                values.push(d);
                passed.push(false);
            }
            _ => break,
        }
    }
    if let Some(caller) = vm.frames.last_mut() {
        caller.ip = done;
    }
    let mut args = Args {
        values,
        passed,
        named: Vec::new(),
        this: Some(o),
        called: Some(cls),
        func: Some(Rc::clone(&b)),
    };
    let r = (b.handler)(vm, &mut args);
    let Args { values, passed, .. } = args;
    if values.capacity() > 0 {
        vm.put_val_vec(values);
        vm.put_bool_vec(passed);
    }
    pure_method_done(vm, obj, dst, done, r.ok()?)
}

/// The cache entry of `$this->name` (place `place` of the accessor method
/// `uf`'s code) for objects of the class at `cls_addr`, as the method's own
/// `FETCH_OBJ_R` / `ASSIGN_OBJ` tests it in its frame (`spec::props`'
/// `plain_key`): filled for that class in the method's scope (its address,
/// `method_scope`), a declared
/// property allowing every access in `want`. Its slot and the types a
/// written value may have as it is.
#[inline(always)]
#[allow(clippy::inline_always)] // as above
fn accessor_slot(
    uf: &UserFunction,
    place: u32,
    cls_addr: usize,
    method_scope: usize,
    want: u8,
) -> Option<(usize, u32)> {
    let cache = uf.code.prop_cache.try_borrow().ok()?;
    let e = cache.get(place as usize)?;
    if e.class_addr != cls_addr
        || e.scope_addr != method_scope
        || e.base != crate::opcode::PROP_CACHE_THIS
        || e.flags & (want | PC_DYN) != want
    {
        return None;
    }
    Some((e.slot as usize, e.accept))
}

/// [`accessor_read`] of `return $this->name;` (place `place`), in line:
/// the value of the declared, initialized slot, when the return type
/// takes it as it is.
#[inline(always)]
#[allow(clippy::inline_always)] // the getters' whole path, one caller that matters
fn accessor_get(
    uf: &UserFunction,
    place: u32,
    od: &crate::object::ObjectData,
    cls_addr: usize,
    scope: usize,
) -> Option<Value> {
    let (slot, _) = accessor_slot(uf, place, cls_addr, scope, crate::place::propcache::PC_READ)?;
    if !od.not_lazy() {
        return None;
    }
    let slots = od.slots.try_borrow().ok()?;
    let value = match slots.get(slot)? {
        Some(Value::Reference(_)) | None => return None,
        Some(v) => clone_value(v),
    };
    drop(slots);
    match &uf.return_type {
        Some(ty) if !crate::call::param_accepts(ty, &value) => None,
        _ => Some(value),
    }
}

/// The value a call of the accessor method `uf` ([`UserFunction::accessor`])
/// with the arguments `args` returns for the object `o` (`od`, of the
/// class at `cls_addr`), run without a frame; `scope` is the address of
/// the method's class scope (0: none). Nothing in such a body can be observed from inside it (no
/// diagnostic, no user code, no backtrace): a property is reached through
/// the method's own property cache, on the hits its ops take in a frame
/// (a declared, initialized slot of an object of the cached class,
/// holding no reference), which the first, framed call of the method
/// fills; an element test needs an array there and an int or string
/// offset; a write needs a value the property takes as it is. A declared
/// return type must take the result as it is. `None`, nothing changed,
/// for anything else: the call is made.
///
/// A sixth of the user calls of the `MediaWiki` integration tests are such
/// methods, each of which paid a whole call (~1,000 instructions).
#[inline(never)]
fn accessor_read(
    uf: &UserFunction,
    acc: Accessor,
    o: &Obj,
    od: &crate::object::ObjectData,
    cls_addr: usize,
    scope: usize,
    args: &mut [Value],
) -> Option<Value> {
    use crate::place::propcache::PC_READ;
    let returns = |value: &Value| {
        uf.return_type
            .as_ref()
            .is_none_or(|ty| crate::call::param_accepts(ty, value))
    };
    // The array a declared slot holds, for the element tests.
    let value = match acc {
        Accessor::Lit(l) => clone_value(uf.code.literals.get(l as usize)?),
        Accessor::Null => {
            // The implicit `return;`: null, for no return type or `void`.
            return uf
                .return_type
                .as_ref()
                .is_none_or(crate::types::TypeDecl::is_void)
                .then_some(Value::Null);
        }
        Accessor::This => Value::Object(o.clone()),
        Accessor::Arg(i) => std::mem::take(args.get_mut(i as usize)?),
        Accessor::Prop(place) => return accessor_get(uf, place, od, cls_addr, scope),
        Accessor::IssetDim(place, i) | Accessor::KeyExists(place, i) => {
            let (slot, _) = accessor_slot(uf, place, cls_addr, scope, PC_READ)?;
            if !od.not_lazy() {
                return None;
            }
            let slots = od.slots.try_borrow().ok()?;
            let Some(Some(Value::Array(a))) = slots.get(slot) else {
                return None;
            };
            let el = match args.get(i as usize)? {
                Value::Str(k) => a.get_sym(k.as_bytes()),
                Value::Int(k) => a.get_int(*k),
                _ => return None,
            };
            let found = match (acc, el) {
                (Accessor::KeyExists(..), el) => el.is_some(),
                // `isset()`: there and not null, through a reference.
                (_, Some(Value::Reference(r))) => !r.try_borrow()?.is_null(),
                (_, Some(v)) => !v.is_null(),
                (_, None) => false,
            };
            Value::Bool(found)
        }
        Accessor::SetProp(place, i, this) => {
            let result = if this {
                Value::Object(o.clone())
            } else {
                Value::Null
            };
            // Every test before the store: a refusal changes nothing.
            if this {
                if !returns(&result) {
                    return None;
                }
            } else if !uf
                .return_type
                .as_ref()
                .is_none_or(crate::types::TypeDecl::is_void)
            {
                return None;
            }
            let (slot, accept) = accessor_slot(uf, place, cls_addr, scope, PC_READ | PC_WRITE)?;
            let v = args.get_mut(i as usize)?;
            if matches!(v, Value::Reference(_)) || !accepts(accept, v) || !od.not_lazy() {
                return None;
            }
            let old = {
                let mut slots = od.slots.try_borrow_mut().ok()?;
                match slots.get_mut(slot)? {
                    Some(cur) if !matches!(cur, Value::Reference(_)) => {
                        std::mem::replace(cur, std::mem::take(v))
                    }
                    _ => return None,
                }
            };
            // `zval_ptr_dtor` of the old value, once nothing is borrowed.
            crate::exec::release_rooted(Some(old));
            return Some(result);
        }
    };
    returns(&value).then_some(value)
}

/// A method call site's cache hit (`call::method_entry_hit`): the method,
/// its class scope, whether it is static.
pub(super) type MethodEntry = (Func, Option<Rc<crate::class::Class>>, bool);

/// The most arguments a leaf call takes ([`leaf_now`]).
const LEAF_ARGS: usize = 4;

/// `f(...)` of a leaf builtin (`special::leaf`), the callee of the call
/// `call` just prepared (nothing was sent yet): `ZEND_INIT_FCALL` at `at`
/// followed by `Send`s of literals, variables or
/// temporaries (by value, at most [`LEAF_ARGS`]) and the `DoCall`. When
/// the arguments have one of the shapes of the function's leaf class
/// (`Vm::leaf_args`: the handler then reports nothing, throws nothing,
/// runs no PHP code and looks at no frame), the handler is called at
/// once on the caller's frame with copies of them, as a frameless
/// function is: nothing is readied in the pooled frame and nothing sent
/// through it (the prepared call's ~780 instructions around the handler,
/// 12M times in two framework test suites). The index of the `DoCall`
/// with `done_flag` or `threw_flag` (the caller's `CALLED`, `CALL_THREW`), as
/// `enter_after_sends` returns a builtin's call. `None`, nothing
/// changed, for anything else.
pub(super) fn leaf_now(
    vm: &mut Vm,
    ops: &[Op],
    call: &Prepared,
    at: usize,
    done_flag: usize,
    threw_flag: usize,
) -> Option<usize> {
    let Callee::Native(b, PlainCall::Named) = &call.callee else {
        return None;
    };
    if b.leaf_id() == crate::special::leaf::NONE
        || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
        || vm.observer.enabled
    {
        return None;
    }
    let caller = vm.frames.last()?;
    // The sent values, read where they are.
    let mut sent: [&Value; LEAF_ARGS] = [&Value::Null; LEAF_ARGS];
    let mut tmps: [Option<u32>; LEAF_ARGS] = [None; LEAF_ARGS];
    let mut n = 0;
    let dst = loop {
        let src = match ops.get(at + 1 + n)? {
            &Op::DoCall { dst } => break dst,
            Op::Send { src } if n < LEAF_ARGS => src,
            _ => return None,
        };
        sent[n] = match *src {
            ArgSrc::Val(Operand::Lit(l)) => caller.code.literals.get(l as usize)?,
            ArgSrc::Val(Operand::Tmp(t)) => {
                tmps[n] = Some(t);
                caller.slots.tmps().get(t as usize)?
            }
            ArgSrc::Val(Operand::Cv(cv)) if !caller.symtab_attached => {
                caller.slots.cvs().get(cv as usize)?.get()?
            }
            // `ZEND_SEND_VAR_EX` of a plain variable: by value (a leaf
            // function has no by-reference parameter). An undefined one
            // is the call's (its warning).
            ArgSrc::Place(pid) if !caller.symtab_attached => {
                let cv = place_cv(&caller.code, pid)?;
                caller.slots.cvs().get(cv as usize)?.get()?
            }
            _ => return None,
        };
        n += 1;
    };
    if !vm.leaf_args(b.leaf_id(), &sent[..n]) {
        return None;
    }
    // The handler's copies; a temporary is consumed (its `Send` would
    // have moved it).
    let mut args = vm.call_bufs.take_args();
    args.values.reserve(b.signature().params.len().max(n));
    for v in &sent[..n] {
        args.values.push(crate::func::copy_arg(v));
    }
    let done = at + 1 + n;
    let caller = vm.frames.last_mut()?;
    for t in tmps[..n].iter().flatten() {
        if let Some(slot) = caller.slots.tmps_mut().get_mut(*t as usize) {
            drop(std::mem::take(slot));
        }
    }
    // `SAVE_OPLINE()`: the op after the `DoCall`.
    caller.ip = done + 1;
    match vm.frameless_handler_run(b, args, dst) {
        Ok(()) => Some(done | done_flag),
        Err(t) => {
            vm.tape_throw = Some(t);
            Some(done | threw_flag)
        }
    }
}

/// What [`accessor_now`] found at a method call site.
pub(super) enum MethodNow {
    /// The call is done: the op to go on at.
    Done(usize),
    /// The site's cache hit, which is no accessor here: [`prepare_method`]
    /// takes it instead of looking it up again.
    Resolved(MethodEntry),
    /// Anything else, nothing changed.
    Unknown,
}

/// The most arguments [`accessor_now`] passes.
const ACCESSOR_ARGS: usize = 3;

/// [`accessor_read`] of an accessor that is no getter, with or without
/// arguments, out of the getters' line.
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the call site's pieces, as found
fn accessor_other(
    caller: &crate::vm::Frame,
    sends: &[Operand],
    uf: &UserFunction,
    acc: Accessor,
    o: &Obj,
    od: &crate::object::ObjectData,
    cls_addr: usize,
    scope: usize,
) -> Option<Value> {
    if sends.is_empty() {
        accessor_read(uf, acc, o, od, cls_addr, scope, &mut [])
    } else {
        accessor_with_args(caller, sends, uf, acc, o, od, cls_addr, scope)
    }
}

/// [`accessor_read`] of an accessor that takes the arguments `sends` (the
/// operands of the call's `Send`s, read in `caller`), as `ZEND_SEND_VAL` /
/// `ZEND_SEND_VAR` copy them: an undefined variable (its warning) and a
/// value the parameter's type coerces or refuses are the call's. Out of
/// line: the buffer and its release are not the getters' to pay.
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the call site's pieces, as found
fn accessor_with_args(
    caller: &crate::vm::Frame,
    sends: &[Operand],
    uf: &UserFunction,
    acc: Accessor,
    o: &Obj,
    od: &crate::object::ObjectData,
    cls_addr: usize,
    scope: usize,
) -> Option<Value> {
    if caller.symtab_attached {
        return None;
    }
    let mut args = [Value::Null, Value::Null, Value::Null];
    let n = sends.len().min(ACCESSOR_ARGS);
    for (i, op) in sends[..n].iter().enumerate() {
        let v = match *op {
            Operand::Lit(l) => caller.code.literals.get(l as usize).map(clone_value),
            Operand::Cv(cv) => cv_value(caller, cv),
            _ => None,
        }?;
        let ok = uf
            .params
            .get(i)?
            .ty
            .as_ref()
            .is_none_or(|ty| crate::call::param_accepts(ty, &v));
        if !ok || matches!(v, Value::Reference(_)) {
            return None;
        }
        args[i] = v;
    }
    accessor_read(uf, acc, o, od, cls_addr, scope, &mut args[..n])
}

/// `$obj->name(...)` of a user method that is an accessor
/// ([`UserFunction::accessor`]), `ZEND_INIT_METHOD_CALL` at `at` followed
/// by `Send`s of literals or variables (by value, at most
/// [`ACCESSOR_ARGS`]) and its `DoCall`: on the method cache hit
/// [`prepare_method`] takes, with one argument per parameter, each defined
/// and accepted by its parameter's type as it is, the method's one
/// statement runs without a frame ([`accessor_read`]) and its value is
/// the call's result; the op after the `DoCall` is where execution goes
/// on. A pending interrupt is the usual call's to service, after it.
/// `settled`: the site is already marked as calling an accessor.
#[inline]
pub(super) fn accessor_now(
    vm: &mut Vm,
    ops: &[Op],
    obj: Operand,
    lit: u32,
    at: usize,
    settled: bool,
) -> MethodNow {
    // Without arguments (the getters, most of these calls): no scan of
    // the sends, no buffer.
    if let Some(&Op::DoCall { dst }) = ops.get(at + 1) {
        return accessor_call(vm, obj, lit, at, &[], dst, settled);
    }
    accessor_now_args(vm, ops, obj, lit, at, settled)
}

/// [`accessor_now`] of a call with arguments: the scan of its `Send`s.
#[inline(never)]
fn accessor_now_args(
    vm: &mut Vm,
    ops: &[Op],
    obj: Operand,
    lit: u32,
    at: usize,
    settled: bool,
) -> MethodNow {
    let mut sends = [Operand::Unused; ACCESSOR_ARGS];
    let mut n = 0;
    let dst = loop {
        match ops.get(at + 1 + n) {
            Some(&Op::DoCall { dst }) => break dst,
            Some(&Op::Send {
                src: ArgSrc::Val(op @ (Operand::Lit(_) | Operand::Cv(_))),
            }) if n < ACCESSOR_ARGS => {
                sends[n] = op;
                n += 1;
            }
            // `ZEND_SEND_VAR_EX` of a plain variable: by value, the
            // parameter being none by reference (an accessor has none).
            Some(&Op::Send {
                src: ArgSrc::Place(pid),
            }) if n < ACCESSOR_ARGS => {
                let cv = vm.frames.last().and_then(|f| place_cv(&f.code, pid));
                match cv {
                    Some(cv) => sends[n] = Operand::Cv(cv),
                    None => return not_an_accessor_site(vm, at),
                }
                n += 1;
            }
            // An argument that is no plain value: never at this site.
            _ => return not_an_accessor_site(vm, at),
        }
    };
    accessor_call(vm, obj, lit, at, &sends[..n], dst, settled)
}

/// Marks the call site at `at` as one [`accessor_now`] has nothing to try
/// at (its arguments are not plain values): later calls skip the attempt.
#[cold]
#[inline(never)]
fn not_an_accessor_site(vm: &Vm, at: usize) -> MethodNow {
    if let Some(f) = vm.frames.last() {
        crate::call::mark_method_site(&f.code, at, false);
    }
    MethodNow::Unknown
}

/// [`accessor_now`] once the call's `Send`s (`sends`, their operands) and
/// its `DoCall` (result to `dst`) are known.
#[inline(always)]
#[allow(clippy::inline_always)] // two callers, one of them the getters'
fn accessor_call(
    vm: &mut Vm,
    obj: Operand,
    lit: u32,
    at: usize,
    sends: &[Operand],
    dst: Option<u32>,
    settled: bool,
) -> MethodNow {
    if vm.observer.enabled || crate::interrupt::safe_point_due() {
        return MethodNow::Unknown;
    }
    let n = sends.len();
    let found = {
        let Some(caller) = vm.frames.last() else {
            return MethodNow::Unknown;
        };
        let o = match obj {
            Operand::Cv(cv) if !caller.symtab_attached => {
                match caller
                    .slots
                    .cvs()
                    .get(cv as usize)
                    .and_then(crate::vm::CvSlot::get)
                {
                    Some(Value::Object(o)) => o,
                    _ => return MethodNow::Unknown,
                }
            }
            Operand::Tmp(t) => match caller.slots.tmps().get(t as usize) {
                Some(Value::Object(o)) => o,
                _ => return MethodNow::Unknown,
            },
            Operand::Unused => match caller.this.as_ref() {
                Some(o) => o,
                None => return MethodNow::Unknown,
            },
            _ => return MethodNow::Unknown,
        };
        let Some(od) = o.downcast_ref::<crate::object::ObjectData>() else {
            return MethodNow::Unknown;
        };
        let cls = &od.class;
        if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
            return MethodNow::Unknown;
        }
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        // A site already known to call an accessor: the hit read in place
        // (`method_site_user`); a miss there is the usual call's.
        if settled {
            let Some((uf, scope)) =
                crate::call::method_site_user(&caller.code, at, cls_addr, scope_addr, lit)
            else {
                return MethodNow::Unknown;
            };
            let value = match uf.accessor() {
                Some(_) if uf.params.len() != n => None,
                Some(Accessor::Prop(place)) => accessor_get(&uf, place, od, cls_addr, scope),
                Some(acc) => accessor_other(caller, sends, &uf, acc, o, od, cls_addr, scope),
                None => None,
            };
            drop(uf);
            return match value {
                Some(value) => accessor_done(vm, obj, dst, at + 2 + n, value),
                None => MethodNow::Unknown,
            };
        }
        let Some(entry) =
            crate::call::method_entry_hit(&caller.code, at, cls_addr, scope_addr, lit)
        else {
            return MethodNow::Unknown;
        };
        // What the site calls is settled by its first call: later ones
        // only come here when it is an accessor (`method_site_kind`).
        let accessor = match &entry {
            (Func::User(uf), _, false) => uf.accessor().filter(|_| uf.params.len() == n),
            _ => None,
        };
        if !settled {
            crate::call::mark_method_site(&caller.code, at, accessor.is_some());
        }
        let value = match &entry {
            (Func::User(uf), scope, false) => accessor.and_then(|acc| {
                let scope = scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
                match acc {
                    // (The getters' own, small path.)
                    Accessor::Prop(place) => accessor_get(uf, place, od, cls_addr, scope),
                    _ if n == 0 => accessor_read(uf, acc, o, od, cls_addr, scope, &mut []),
                    _ => accessor_with_args(caller, sends, uf, acc, o, od, cls_addr, scope),
                }
            }),
            _ => None,
        };
        match value {
            Some(value) => Ok(value),
            None => Err(entry),
        }
    };
    match found {
        Ok(value) => accessor_done(vm, obj, dst, at + 2 + n, value),
        Err(entry) => MethodNow::Resolved(entry),
    }
}

/// The end of [`accessor_call`]: the caller's resume point is the op after
/// the `DoCall` (`done`), a temporary `$this` is released and the value
/// stored as the call's result ([`pure_method_done`]).
#[inline(always)]
#[allow(clippy::inline_always)] // as above
fn accessor_done(
    vm: &mut Vm,
    obj: Operand,
    dst: Option<u32>,
    done: usize,
    value: Value,
) -> MethodNow {
    if let Some(f) = vm.frames.last_mut() {
        f.ip = done;
    }
    match pure_method_done(vm, obj, dst, done, value) {
        Some(next) => MethodNow::Done(next),
        None => MethodNow::Unknown,
    }
}

/// The end of [`pure_method_now`]'s call: a temporary `$this` released,
/// the result `value` stored, the op after the `DoCall` (`done`) returned.
fn pure_method_done(
    vm: &mut Vm,
    obj: Operand,
    dst: Option<u32>,
    done: usize,
    value: Value,
) -> Option<usize> {
    let caller = vm.frames.last_mut()?;
    if let Operand::Tmp(t) = obj {
        if let Some(slot) = caller.slots.tmps_mut().get_mut(t as usize) {
            drop(std::mem::take(slot));
        }
    }
    match dst.and_then(|d| caller.slots.tmps_mut().get_mut(d as usize)) {
        Some(slot) => crate::exec::spec::put_tmp(slot, value),
        None => drop(value),
    }
    Some(done)
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `Op::InitStaticCall { class:
/// Named(_), name: Lit(_) }` at `at` in the current frame: on the call
/// site's cache hit (`Vm::op_init_static_call`'s first case: the site's
/// slot past the literals, indexed by the op after it), when the method
/// is a user function whose declaration makes a plain call, the pooled
/// frame above the current one is readied for it with its scopes (no
/// `$this`) and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_static(vm: &mut Vm, at: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let slot = caller.code.literals.len() + at + 1;
    let (func, scope, called_class) = caller
        .code
        .fcall_cache
        .borrow()
        .get(slot)
        .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get_call))?;
    let Func::User(uf) = func else {
        return None;
    };
    let limit = prepares(&uf)?;
    ready_frame(frame, &uf);
    fill(&mut frame.scope, scope);
    fill(&mut frame.called, called_class);
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// [`prepare_method`] of a builtin method (`ReflectionMethod::getName()`)
/// whose call is plain: called by [`call_native`] like a builtin function,
/// with `$this` (taken as [`prepare_method`] takes it) and the scopes on
/// the pooled frame. Out of line, so that the user method's path keeps
/// its code.
#[inline(never)]
fn prepare_native_method(
    caller: &mut crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    obj: Operand,
    b: Rc<Builtin>,
    scope: Option<Rc<crate::class::Class>>,
    is_static: bool,
    called_class: Rc<crate::class::Class>,
) -> Option<Prepared> {
    // As a function's call (`prepare`'s builtin arm): plain up to its
    // first by-reference parameter (`mysqli_stmt::bind_param()`,
    // `NumberFormatter::parse()`, `SplFileObject::fscanf()`), which
    // `send_ref` then takes.
    let (kind, first_ref) = b.plain_call_upto();
    if kind != PlainCall::Named || !frame.args.is_empty() {
        return None;
    }
    let (this, released) = match obj {
        Operand::Tmp(t) => match caller
            .slots
            .tmps_mut()
            .get_mut(t as usize)
            .map(std::mem::take)
        {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        Operand::Cv(cv) => match caller
            .slots
            .cvs()
            .get(cv as usize)
            .and_then(crate::vm::CvSlot::get)
        {
            Some(Value::Object(o)) => (Some(o.clone()), None),
            _ => return None,
        },
        Operand::Unused => (Some(caller.this.clone()?), None),
        _ => return None,
    };
    // Past the declared parameters only into a variadic one (else the
    // generic call reports the count), and up to the first by-reference
    // parameter.
    let limit = native_limit(&b).min(first_ref);
    frame.this = this;
    frame.scope = scope;
    frame.called = Some(called_class);
    let calls_len = caller.calls.len();
    drop(released);
    Some(Prepared {
        limit,
        callee: Callee::Native(b, PlainCall::Named),
        n: 0,
        calls_len,
        fcall: false,
        by_name: false,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `self::name()`, `parent::name()` or
/// `static::name()` (`Op::InitStaticCall` with such a spec and the method
/// name literal `lit`) at `at` in the current frame: on the hit
/// `Vm::init_relative_static_call_cached` takes (the op's own slot of the
/// method cache holds the class the spec resolves to, the calling scope
/// and the method), when the method is a user function whose declaration
/// makes a plain call, the pooled frame above the current one is readied
/// for it as that hit sets the call up: a static method with the frame's
/// called scope when that is a subclass of the resolved class (the three
/// specs forward it), else the class; an instance method with the frame's
/// `$this` when its class is one. `None`, having changed nothing,
/// otherwise (a miss, and an instance method without such a `$this`: the
/// generic op raises the error).
pub(super) fn prepare_relative_static(
    vm: &mut Vm,
    spec: &ClassSpec,
    lit: u32,
    at: usize,
) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    if caller.kind == FrameKind::Builtin {
        return None;
    }
    let cls = crate::call::relative_spec_class(caller, spec)?;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let (uf, scope, is_static) = {
        let cache = caller.code.method_cache.borrow();
        let cached = cache.get(at)?.as_ref()?;
        if cached.class_addr != Rc::as_ptr(cls) as usize
            || cached.scope_addr != scope_addr
            || cached.name_lit != Some(lit)
        {
            return None;
        }
        let Some(Func::User(uf)) = cached.func.get() else {
            return None;
        };
        let scope = cached
            .method_scope
            .as_ref()
            .and_then(std::rc::Weak::upgrade)?;
        (uf, scope, cached.is_static)
    };
    let limit = prepares(&uf)?;
    let (this, late_bound) = if is_static {
        let late_bound = match caller.called.as_ref() {
            Some(c) if c.is_subclass_of(cls) => Rc::clone(c),
            _ => Rc::clone(cls),
        };
        (None, late_bound)
    } else {
        let this = caller.this.as_ref()?;
        let late_bound = crate::object::class_of(this);
        if !late_bound.is_subclass_of(cls) {
            return None;
        }
        (Some(this.clone()), Rc::clone(late_bound))
    };
    ready_frame(frame, &uf);
    fill(&mut frame.this, this);
    fill(&mut frame.scope, Some(scope));
    fill(&mut frame.called, Some(late_bound));
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: false,
        binds: false,
        ctor: false,
    })
}

/// `ZEND_INIT_DYNAMIC_CALL` of `Op::InitDynCall { callee: Cv(cv) }` in the
/// current frame: when the variable holds a closure of a user function
/// whose declaration makes a plain call (`Vm::init_closure_call_fast`,
/// without a `__call` trampoline), the pooled frame above the current one
/// is readied for it with the closure's `$this`, scopes and the closure
/// itself, and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_closure(vm: &mut Vm, cv: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    if caller.symtab_attached {
        return None;
    }
    let Some(Value::Object(o)) = caller
        .slots
        .cvs()
        .get(cv as usize)
        .and_then(crate::vm::CvSlot::get)
    else {
        return None;
    };
    let data = crate::object::data(o);
    if data.class.lc_name.as_bytes() != b"closure" {
        return None;
    }
    let internal = data.internal.try_borrow().ok()?;
    let d = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())?;
    let Func::User(uf) = &d.func else {
        return None;
    };
    if d.magic_name.is_some() {
        return None;
    }
    let limit = prepares(uf)?;
    let uf = Rc::clone(uf);
    ready_frame(frame, &uf);
    fill(&mut frame.this, d.this.clone());
    fill(&mut frame.scope, d.scope.clone());
    fill(&mut frame.called, d.called.clone());
    fill(
        &mut frame.closure,
        if d.fake { None } else { Some(o.clone()) },
    );
    // A closure without `use`s (or a fake one, which `enter` would not
    // bind from) spares `enter` the second look at its data.
    let binds = !d.fake && !d.vars.is_empty();
    drop(internal);
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: true,
        binds,
        ctor: false,
    })
}

/// What [`new_object`] did.
pub(super) enum NewDone {
    /// The object of a class without a constructor is in the result:
    /// continue at the op after the constructor call (`skip`).
    Skip(usize),
    /// The object is in the result and its constructor call is prepared.
    Prepared(Prepared),
    /// The object is in the result and its constructor call is the
    /// generic pending call (`props::new_with_ctor`).
    Pending,
}

/// `ZEND_NEW` of `Op::New { class: Named(lit), dst, skip }` in the current
/// frame, on the class cache hit the props fast path takes: a class
/// without a constructor gets its object (`props::new_plain`, then the
/// jump over the constructor call); a class whose constructor is a user
/// function that makes a plain call gets its object and the constructor's
/// frame prepared on it (`ZEND_NEW` pushing the call frame with the object
/// as `$this`); any other constructor, the generic pending call. `None`,
/// having changed nothing, when none of that applies (the generic `New`).
pub(super) fn new_object(vm: &mut Vm, lit: u32, dst: u32, skip: u32) -> Option<NewDone> {
    let live = vm.frames.live;
    if live == 0 {
        return None;
    }
    let observed = vm.observer.enabled;
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let caller = callers.last_mut()?;
    if dst as usize >= caller.slots.tmps().len() {
        return None;
    }
    let cls = caller
        .code
        .class_cache
        .try_borrow()
        .ok()?
        .get(lit as usize)
        .and_then(std::rc::Weak::upgrade)?;
    if cls.magic.construct.is_none() {
        // Without a `skip` target (arguments being evaluated), the generic
        // op starts the placeholder call that evaluates them.
        if skip == 0 {
            return None;
        }
        let o = crate::props::new_plain(&cls)?;
        crate::exec::spec::put_tmp(&mut caller.slots.tmps_mut()[dst as usize], Value::Object(o));
        return Some(NewDone::Skip(skip as usize));
    }
    let direct = match (crate::props::ctor_call(&cls), pool.first_mut()) {
        (Some((Func::User(uf), scope)), Some(frame))
            if !observed && live <= 50_000 && uf.call_shape() != CallShape::NOT_PLAIN =>
        {
            Some((uf, scope, frame))
        }
        _ => None,
    };
    let Some((uf, scope, frame)) = direct else {
        let o = crate::props::new_with_ctor(cls, &mut caller.calls)?;
        crate::exec::spec::put_tmp(&mut caller.slots.tmps_mut()[dst as usize], Value::Object(o));
        return Some(NewDone::Pending);
    };
    let o = crate::props::new_for_ctor(&cls)?;
    ready_frame(frame, &uf);
    fill(&mut frame.this, Some(o.clone()));
    fill(&mut frame.scope, Some(scope));
    fill(&mut frame.called, Some(cls));
    crate::exec::spec::put_tmp(&mut caller.slots.tmps_mut()[dst as usize], Value::Object(o));
    Some(NewDone::Prepared(Prepared {
        limit: uf.params.len().min(MAX_ARGS),
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: false,
        binds: false,
        ctor: true,
    }))
}

/// `ZEND_SEND_VAL` / `ZEND_SEND_VAR` of `src` for the prepared call: the
/// argument is copied (a literal, a variable: dereferenced) or moved (a
/// temporary) straight into the callee's compiled variable. `false`,
/// having changed nothing, when the send is not the prepared call's (a
/// later pending call, started by `new`, is on top), the argument has no
/// declared parameter or is more than [`MAX_ARGS`], or the variable is
/// undefined (the generic send warns): the caller then materializes the
/// call and the generic `Send` runs.
pub(super) fn send(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    if prep.n >= prep.limit {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    send_in(caller, frame, prep, src)
}

/// The `Send`s from `at` on of the prepared call, passed at once
/// (`exec::tape::send_following`): the frame stack is split and the
/// pending-call count tested once for the whole run, which nothing in it
/// can change (a send moves or copies a value, runs no user code and
/// starts no call), instead of once a `Send` (~12 instructions an
/// argument). A plain variable for a by-reference parameter goes to
/// [`send_ref`], and the run goes on after it. Returns the op after the
/// last one passed: the `DoCall`, or a `Send` neither takes, which then
/// runs as before.
#[inline(never)]
pub(super) fn send_run(vm: &mut Vm, prep: &mut Prepared, ops: &[Op], at: usize) -> usize {
    let mut next = at;
    loop {
        {
            let live = vm.frames.live;
            let (callers, pool) = vm.frames.vec.split_at_mut(live);
            let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
                return next;
            };
            if caller.calls.len() != prep.calls_len {
                return next;
            }
            while let Some(Op::Send { src }) = ops.get(next) {
                if prep.n >= prep.limit || !send_in(caller, frame, prep, src) {
                    break;
                }
                next += 1;
            }
        }
        match ops.get(next) {
            Some(Op::Send { src }) if send_ref(vm, prep, src) => next += 1,
            _ => return next,
        }
    }
}

/// [`send`] of one argument once the calling frame and the prepared one
/// are in hand and the call is known to be the newest pending one, with
/// room for the argument (`prep.n < prep.limit`).
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `send` and of `send_run`'s loop
fn send_in(
    caller: &mut crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    prep: &mut Prepared,
    src: &ArgSrc,
) -> bool {
    let n = prep.n;
    let v = match src {
        ArgSrc::Val(Operand::Lit(i)) => match caller.code.literals.get(*i as usize) {
            Some(v) => clone_value(v),
            None => return false,
        },
        ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
            match caller.slots.tmps_mut().get_mut(*t as usize) {
                Some(slot) => match std::mem::take(slot) {
                    Value::Reference(r) => r.get(),
                    v => v,
                },
                None => return false,
            }
        }
        ArgSrc::Val(Operand::Cv(cv)) => match cv_value(caller, *cv) {
            Some(v) => v,
            None => return false,
        },
        ArgSrc::Place(pid) => match place_cv(&caller.code, *pid) {
            Some(cv) => match cv_value(caller, cv) {
                Some(v) => v,
                None => return false,
            },
            // `$this` to a by-value parameter (`method_exists($this, $m)`):
            // the frame's object (a frame without one is the generic send's
            // error).
            None if this_place(&caller.code, *pid) => {
                if let Callee::User(uf) = &prep.callee {
                    if uf.params.get(n).is_none_or(|p| p.by_ref) {
                        return false;
                    }
                }
                match &caller.this {
                    Some(o) => Value::Object(o.clone()),
                    None => return false,
                }
            }
            None => match place_arg(caller, frame, &prep.callee, n, *pid) {
                Some(v) => v,
                None => return false,
            },
        },
        ArgSrc::TempPlace(pid) => match place_arg(caller, frame, &prep.callee, n, *pid) {
            Some(v) => v,
            None => return false,
        },
        _ => return false,
    };
    match prep.callee {
        Callee::User(_) => {
            let Some(slot) = frame.slots.cvs_mut().get_mut(n) else {
                return false;
            };
            // A prepared frame's compiled variables are empty.
            if let Some(old) = slot.as_mut().replace(v) {
                drop(old);
            }
        }
        // `ZEND_SEND_VAL` / `ZEND_SEND_VAR` into the call frame's argument
        // slot.
        Callee::Native(..) => frame.args.push(Some(v)),
        // (Its `Send` is `quick_send`'s.)
        Callee::Quick(..) => return false,
    }
    prep.n = n + 1;
    true
}

/// The run of `Send`s from `ops[start]` on, for a prepared builtin call
/// (`ZEND_SEND_VAL` / `ZEND_SEND_VAR` into the call frame's argument
/// slots): the caller and the call frame are looked up once and each
/// argument is read as [`send`] reads it ([`native_arg`]) and pushed,
/// where `send` per argument split the frame stack, tested the pending
/// calls and dispatched on the callee again (~68 instructions an
/// argument). Where the run meets a send it does not take (an argument
/// past the declared ones, a by-reference one, an undefined variable, a
/// place the specialised fetch does not read), the rest goes through
/// `send` / [`send_ref`] one by one, as before. Returns the op after the
/// last one passed.
#[inline(never)]
pub(super) fn send_native_run(vm: &mut Vm, prep: &mut Prepared, ops: &[Op], start: usize) -> usize {
    let mut next = start;
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    if let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) {
        if matches!(prep.callee, Callee::Native(..)) && caller.calls.len() == prep.calls_len {
            while let Some(Op::Send { src }) = ops.get(next) {
                if prep.n >= prep.limit {
                    break;
                }
                // Pushed as it is read: unwrapping the value and wrapping
                // it again moved it through the stack byte by byte (~25
                // instructions an argument).
                let arg = native_arg(caller, src);
                if arg.is_none() {
                    break;
                }
                frame.args.push(arg);
                prep.n += 1;
                next += 1;
            }
        }
    }
    while let Some(Op::Send { src }) = ops.get(next) {
        if !send(vm, prep, src) && !send_ref(vm, prep, src) {
            break;
        }
        next += 1;
    }
    next
}

/// [`send`]'s read of argument `src` of a builtin call from `caller`: a
/// literal copied, a temporary moved out (dereferenced), a variable or a
/// place copied. `None`, having changed nothing, where `send` declines.
#[inline(always)]
#[allow(clippy::inline_always)] // once, in `send_native_run`'s loop
fn native_arg(caller: &mut crate::vm::Frame, src: &ArgSrc) -> Option<Value> {
    match src {
        ArgSrc::Val(Operand::Lit(i)) => caller.code.literals.get(*i as usize).map(clone_value),
        ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
            let slot = caller.slots.tmps_mut().get_mut(*t as usize)?;
            Some(match std::mem::take(slot) {
                Value::Reference(r) => r.get(),
                v => v,
            })
        }
        ArgSrc::Val(Operand::Cv(cv)) => cv_value(caller, *cv),
        ArgSrc::Place(pid) => match place_cv(&caller.code, *pid) {
            Some(cv) => cv_value(caller, cv),
            None => caller.send_place_hit(*pid),
        },
        ArgSrc::TempPlace(pid) => caller.send_place_hit(*pid),
        _ => None,
    }
}

/// The value of place `pid` of `caller` (a property, an array element, a
/// chain of them, or an element of a temporary) sent by value as argument
/// `n` of the prepared call to `callee`, whose readied frame is `frame`:
/// the specialised fetch ops' hit ([`crate::vm::Frame::send_place_hit`]),
/// read only once the argument is known to be taken (a by-value
/// parameter: a prepared user function has no by-reference parameter,
/// `CallShape::NOT_PLAIN`, nor a plain builtin call,
/// `Builtin::plain_call`; a slot for it), so that `None` has changed
/// nothing and the generic `Send` reads it with the place walk.
#[inline(never)]
fn place_arg(
    caller: &mut crate::vm::Frame,
    frame: &crate::vm::Frame,
    callee: &Callee,
    n: usize,
    pid: u32,
) -> Option<Value> {
    match callee {
        Callee::User(uf) => {
            if frame.slots.cvs().len() <= n || uf.params.get(n).is_none_or(|p| p.by_ref) {
                return None;
            }
        }
        Callee::Native(..) => {}
        Callee::Quick(..) => return None,
    }
    caller.send_place_hit(pid)
}
/// `ZEND_SEND_REF` of `src` for the prepared call, where [`send`] stopped
/// at a by-reference parameter: a user callee's ([`send_ref_user`]) or a
/// builtin's ([`send_ref_native`]). `false`, having changed nothing, when
/// neither takes it: the call is materialized and the generic send runs.
#[inline(never)]
pub(super) fn send_ref(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    match prep.callee {
        Callee::User(_) => send_ref_user(vm, prep, src),
        Callee::Native(..) => send_ref_native(vm, prep, src),
        Callee::Quick(..) => false,
    }
}

/// `ZEND_SEND_REF` of a plain variable (`f($x)` of a by-reference
/// parameter) for the prepared call, which [`send`] declined because the
/// argument reached the first by-reference parameter ([`prepares`]'s
/// limit): the caller's variable becomes a reference if it is not one (an
/// undefined one a reference to null, without a notice, as
/// `Vm::place_make_ref` makes it) and the callee's compiled variable gets
/// that reference. The limit then moves on to the next by-reference
/// parameter. `false`, having changed nothing, for any other argument or
/// parameter (a value, a dimension or property, a symbol table attached in
/// the caller, a by-reference variadic): the call is materialized and the
/// generic send, with its diagnostics ("Only variables should be passed by
/// reference", "could not be passed by reference"), runs.
#[inline(never)]
fn send_ref_user(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    let n = prep.n;
    let Callee::User(uf) = &prep.callee else {
        return false;
    };
    if n != prep.limit || n >= MAX_ARGS || n >= uf.code.cv_names.len() {
        return false;
    }
    if !uf.params.get(n).is_some_and(|p| p.by_ref && !p.variadic) {
        return false;
    }
    let ArgSrc::Place(pid) = src else {
        return false;
    };
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len
        || caller.symtab_attached
        || !matches!(caller.kind, FrameKind::Main | FrameKind::Function)
    {
        return false;
    }
    let Some(cv) = place_cv(&caller.code, *pid) else {
        return false;
    };
    let Some(slot) = caller.slots.cvs_mut().get_mut(cv as usize) else {
        return false;
    };
    let r = slot.as_mut().get_or_insert(Value::Null).make_ref();
    let Some(dst) = frame.slots.cvs_mut().get_mut(n) else {
        return false;
    };
    // A prepared frame's compiled variables are empty.
    if let Some(old) = dst.as_mut().replace(Value::Reference(r)) {
        drop(old);
    }
    prep.n = n + 1;
    prep.limit = ref_limit(uf, n + 1);
    true
}

/// The send limit of a prepared call of `uf` once its arguments reached
/// parameter `from`: up to its next by-reference parameter, any number into
/// a variadic one (at most [`MAX_ARGS`] and its compiled variables, as
/// [`prepares`]).
#[cold]
#[inline(never)]
fn ref_limit(uf: &UserFunction, from: usize) -> usize {
    let most = match uf.params.get(from..) {
        Some(rest) => match rest.iter().position(|p| p.by_ref) {
            Some(i) => from + i,
            None if uf.params.last().is_some_and(|p| p.variadic) => usize::MAX,
            None => uf.params.len(),
        },
        None => from,
    };
    most.min(MAX_ARGS).min(uf.code.cv_names.len())
}

/// `ZEND_SEND_REF` (`ZEND_SEND_VAR_EX` of a by-reference parameter) into a
/// prepared builtin call, for the argument [`send`] stopped at: the
/// parameter at its position is by reference (`ZEND_ARG_SEND_PREFER_REF`
/// ones too) and the argument is a variable of a frame whose symbol table
/// is not attached. A bare compiled variable becomes a reference if it is
/// not one (an undefined one a reference to null, without a notice:
/// `Vm::cv_make_ref_fast`, what the generic send's `Vm::place_make_ref`
/// does for it); a property or an element (`next($this->list[$k])`,
/// `array_pop($this->stack)`) when making its reference can report
/// nothing and run no user code (`Vm::place_make_ref_quiet`). The
/// reference is the argument, and the send limit moves to the next
/// by-reference parameter.
///
/// `false`, having changed nothing, for anything else: a value or a
/// call's result (the generic send raises the error or the notice, or
/// passes a prefer-ref argument by value), a walk that may report or run
/// user code (which would take the pooled frame), a by-value parameter
/// past the limit, or a call that is not the caller's newest. The call
/// then becomes the generic one.
#[inline(never)]
fn send_ref_native(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    let ArgSrc::Place(pid) = src else {
        return send_prefer_ref_value(vm, prep, src);
    };
    let Callee::Native(b, _) = &prep.callee else {
        return false;
    };
    let n = prep.n;
    let limit = native_limit(b);
    if n >= limit || !b.param_ref_at(n).0 {
        return false;
    }
    let next = (n + 1..limit)
        .find(|&i| b.param_ref_at(i).0)
        .unwrap_or(limit);
    let live = vm.frames.live;
    let caller = live.checked_sub(1).and_then(|i| vm.frames.vec.get(i));
    match (caller, vm.frames.vec.get(live)) {
        (Some(caller), Some(frame))
            if caller.calls.len() == prep.calls_len && frame.args.len() == n => {}
        _ => return false,
    }
    // The current frame is the caller (the prepared one is above it).
    let Some(r) = vm
        .cv_make_ref_fast(*pid)
        .or_else(|| vm.place_make_ref_quiet(*pid))
    else {
        return false;
    };
    let Some(frame) = vm.frames.vec.get_mut(live) else {
        return false;
    };
    frame.args.push(Some(Value::Reference(r)));
    prep.n = n + 1;
    prep.limit = next;
    true
}

/// `ZEND_SEND_VAL_EX` / `ZEND_SEND_VAR_NO_REF_EX` of a value or a call's
/// result into a prepared builtin call at a `ZEND_ARG_SEND_PREFER_REF`
/// parameter (`current(f())`, `array_multisort($a, SORT_DESC)`): php-src
/// passes it by value, with no notice and no error, so [`send`] takes it
/// as it takes a by-value argument, and the send limit moves to the next
/// by-reference parameter. A temporary holding a reference (a function
/// returning by reference) is sent as that reference there
/// (`ZVAL_COPY_VALUE` of the variable), which `send` would dereference: it
/// is left to the generic send, as is every other argument and parameter.
#[inline(never)]
fn send_prefer_ref_value(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    let (ArgSrc::Val(op) | ArgSrc::CallResult(op)) = src else {
        return false;
    };
    let Callee::Native(b, _) = &prep.callee else {
        return false;
    };
    let n = prep.n;
    let limit = native_limit(b);
    if n >= limit || b.param_ref_at(n) != (true, true) {
        return false;
    }
    if let Operand::Tmp(t) = op {
        let held = vm
            .frames
            .live
            .checked_sub(1)
            .and_then(|i| vm.frames.vec.get(i))
            .and_then(|caller| caller.slots.tmps().get(*t as usize));
        if !matches!(held, Some(v) if !matches!(v, Value::Reference(_))) {
            return false;
        }
    }
    let next = (n + 1..limit)
        .find(|&i| b.param_ref_at(i).0)
        .unwrap_or(limit);
    let saved = prep.limit;
    prep.limit = n + 1;
    if !send(vm, prep, src) {
        prep.limit = saved;
        return false;
    }
    prep.limit = next;
    true
}

/// The value of compiled variable `cv` of `frame` as an argument: a copy,
/// dereferenced; `None` when it is undefined (`ZEND_SEND_VAR`'s
/// `ZVAL_COPY_DEREF`).
///
/// A scalar is copied inline; anything that owns a count (a string, an
/// array, an object) or sits behind a reference is copied by
/// [`cv_value_counted`]. Out of line as a whole, an int argument cost ~33
/// instructions here: the call, the tag dispatch of the copy and the
/// value returned through memory.
#[inline(always)]
#[allow(clippy::inline_always)] // twice in `send`, once per argument
fn cv_value(frame: &crate::vm::Frame, cv: u32) -> Option<Value> {
    let v = frame
        .slots
        .cvs()
        .get(cv as usize)
        .and_then(crate::vm::CvSlot::get)?;
    Some(match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(f) => Value::Float(*f),
        _ => cv_value_counted(v),
    })
}

/// [`cv_value`] of a value that is not a scalar, out of line: a string's,
/// an array's or an object's handle counted in line (`Z_TRY_ADDREF`),
/// where `Value::clone` was a further out-of-line call dispatching on the
/// tag again (~36 instructions an argument).
#[inline(never)]
fn cv_value_counted(v: &Value) -> Value {
    match v {
        Value::Reference(r) => r.get(),
        Value::Str(s) => Value::Str(s.clone()),
        Value::Array(a) => Value::Array(a.clone()),
        Value::Object(o) => Value::Object(o.clone()),
        v => clone_value(v),
    }
}

/// `ZEND_DO_UCALL` of the prepared call (result to `dst`; the caller
/// resumes at `caller_ip`): the frame becomes the current one, as
/// `Vm::push_simple_ucall` leaves it. `Err` gives the call back, having
/// changed nothing, when the arguments do not make a plain call (too few,
/// or one that a typed parameter does not accept as it is) or the call is
/// not the caller's newest: the caller materializes it and the generic
/// `DoCall` runs. A builtin's call is given back as well, for
/// [`call_native`].
pub(super) fn enter(
    vm: &mut Vm,
    prep: Prepared,
    dst: Option<u32>,
    caller_ip: usize,
) -> Result<(), Prepared> {
    let live = vm.frames.live;
    let n = prep.n;
    let Callee::User(uf) = &prep.callee else {
        return Err(prep);
    };
    // (`n <= prep.limit`, at most the declared parameters but into a
    // variadic one: `prepares`.)
    if n < uf.required as usize || n > prep.limit {
        return Err(prep);
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return Err(prep);
    };
    if caller.calls.len() != prep.calls_len {
        return Err(prep);
    }
    let shape = uf.call_shape();
    if shape == CallShape::TYPED || (shape == CallShape::NOT_PLAIN && uf.near_typed()) {
        // (A near-plain function's trailing variadic parameter is checked
        // by its own `RECV_VARIADIC`; its arguments past the declared
        // ones are not paired with a parameter here.)
        for (p, slot) in uf.params.iter().zip(&frame.slots.cvs()[..n]) {
            if p.variadic {
                break;
            }
            if let Some(ty) = &p.ty {
                if !slot
                    .get()
                    .is_some_and(|v| crate::call::param_accepts(ty, v))
                {
                    return Err(prep);
                }
            }
        }
    }
    frame.ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
    frame.caller_strict = caller.code.strict_types;
    frame.called_from_internal = false;
    frame.is_ctor = prep.ctor;
    // The passed-argument count (`ZEND_CALL_NUM_ARGS`): each argument is
    // already in its compiled variable, so the buffer stays empty and the
    // count is kept instead (`Frame::num_args`).
    if !frame.args.is_empty() {
        frame.args.clear();
    }
    // The arguments past a variadic function's declared parameters were
    // sent into the compiled variables from the variadic one's on: they
    // go to the argument buffer, where its `RECV_VARIADIC` collects them.
    if uf.flags & flags::VARIADIC != 0 && n >= uf.params.len() {
        variadic_extras(frame, uf.params.len() - 1, n);
    }
    #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
    {
        frame.direct_argc = n as u32;
    }
    // The `RECV`s of the passed arguments have nothing left to do.
    frame.ip = uf.plain_first_op(n);
    if n < uf.params.len() {
        bind_literal_defaults(frame, uf, n);
    }
    // (Its own borrow of the closure's data and the downcast: ~30
    // instructions a closure call, for nothing without `use`s.)
    if prep.binds {
        bind_closure_vars(frame);
    }
    // Released by `Frame::release_direct` at its return.
    frame.direct = true;
    // A plain declaration, a closure's too (its object and bound
    // variables are released by `Frame::release_plain`): its return is
    // the short one (`Vm::return_plain`), whatever started the call.
    frame.plain = matches!(shape, CallShape::UNTYPED | CallShape::TYPED)
        && uf.code.num_statics == 0
        && uf.flags & (flags::VARIADIC | flags::RETURN_REF | flags::GENERATOR) == 0;
    caller.ip = caller_ip;
    vm.frames.live = live + 1;
    Ok(())
}

/// The `RECV_INIT`s of the parameters from `n` on that the call did not
/// pass, each followed by its `RecvDefault` of a literal the parameter
/// accepts as it is (neither by reference nor variadic, the declared type
/// taking the value without coercion): what `hot::recv_literal_default_in`
/// does for each when the loop reaches it, done while the frame is readied
/// (a tape exit and a `step_ext` dispatch per default otherwise). The
/// frame's resume point moves past the ones bound; the first that does not
/// qualify runs as before.
fn bind_literal_defaults(frame: &mut crate::vm::Frame, uf: &UserFunction, n: usize) {
    let code = &uf.code;
    let mut ip = frame.ip;
    for (i, p) in uf.params.iter().enumerate().skip(n) {
        let (
            Some(&Op::RecvInit { n: rn, cv, .. }),
            Some(&Op::RecvDefault {
                n: dn,
                cv: dcv,
                val: Operand::Lit(lit),
            }),
        ) = (code.ops.get(ip), code.ops.get(ip + 1))
        else {
            break;
        };
        if rn as usize != i || dn != rn || dcv != cv || p.by_ref || p.variadic {
            break;
        }
        let Some(value) = code.literals.get(lit as usize) else {
            break;
        };
        if matches!(value, Value::Reference(_))
            || p.ty.as_ref().is_some_and(|ty| {
                !(ty.is_mixed() || crate::call::accepts_without_coercion(ty.mask, value))
            })
        {
            break;
        }
        let Some(slot) = frame.slots.cvs_mut().get_mut(cv as usize) else {
            break;
        };
        if slot.get().is_some() {
            break;
        }
        drop(slot.set(Some(clone_value(value))));
        ip += 2;
    }
    frame.ip = ip;
}

/// The arguments `v..n` of a prepared call of a variadic function, sent
/// into the compiled variables from the variadic parameter's (`v`) on,
/// moved to the frame's argument buffer after a `None` for each declared
/// parameter bound into its variable: `Vm::push_user_call`'s layout, which
/// `RECV_VARIADIC` (`Vm::op_recv_variadic`), `func_get_args()` and
/// backtraces read. The variables are left undefined, as a call leaves
/// them.
#[cold]
#[inline(never)]
fn variadic_extras(frame: &mut crate::vm::Frame, v: usize, n: usize) {
    for _ in 0..v {
        frame.args.push(None);
    }
    for i in v..n {
        let arg = frame
            .slots
            .cvs_mut()
            .get_mut(i)
            .and_then(|slot| slot.as_mut().take());
        frame.args.push(arg);
    }
}

/// `ZEND_DO_UCALL` of the prepared call when the function's whole body
/// runs as machine code ([`super::jit::native_call`]: a pure int function,
/// its recursion included): a call that passes every declared parameter
/// an int. A call by name, of a method (static or not) or of a closure
/// alike: such a body reads no `$this`, no scope and no bound variable
/// (a function with `use`s is never compiled whole), so nothing it could
/// observe differs between them; a constructor's call is left alone (its
/// `new` has more to do). The result goes to `dst` and the readied frame,
/// which is never entered, back to its empty state: the arguments were
/// ints, and what a method's or a closure's prepare put there (`$this`,
/// the scopes, the closure) is released as the call's return would
/// release it. `false`, nothing changed, when it does
/// not apply or the native code declined (an overflow, a recursion deeper
/// than its stack budget, a pending safe point): [`enter`] then makes the
/// call as usual.
pub(super) fn compiled(vm: &mut Vm, prep: &Prepared, dst: Option<u32>) -> bool {
    let Callee::User(uf) = &prep.callee else {
        return false;
    };
    // One load for every function that is not compiled whole.
    if uf.call_shape.native.get() == super::jit::NATIVE_REFUSED || prep.ctor {
        return false;
    }
    let n = prep.n;
    if n != uf.params.len() || n > super::jit::NATIVE_MAX_ARGS || uf.flags & flags::VARIADIC != 0 {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let mut args = [0i64; super::jit::NATIVE_MAX_ARGS];
    let Some(slots) = frame.slots.cvs().get(..n) else {
        return false;
    };
    for (a, slot) in args.iter_mut().zip(slots) {
        let Some(Value::Int(v)) = slot.get() else {
            not_int_argument(uf);
            return false;
        };
        *a = *v;
    }
    let Some(r) = super::jit::native_call(uf, &args[..n]) else {
        return false;
    };
    if let Some(slots) = frame.slots.cvs_mut().get_mut(..n) {
        for slot in slots {
            // An int owns nothing.
            std::mem::forget(slot.as_mut().take());
        }
    }
    if !prep.fcall {
        frame.this = None;
        frame.scope = None;
        frame.called = None;
        frame.closure = None;
    }
    if let Some(slot) = dst.and_then(|d| caller.slots.tmps_mut().get_mut(d as usize)) {
        crate::exec::spec::put_tmp(slot, Value::Int(r));
    }
    true
}

/// The plain calls of a function not compiled whole yet, counted by
/// [`compiled`] as `jit::native_call` counts its own toward compiling the
/// function (its `NATIVE_HOT_CALLS`).
const NATIVE_TRY_CALLS: u32 = 100;

/// [`compiled`] of a call that passed a parameter something other than an
/// int, which native code cannot take: while the function is still being
/// counted ([`crate::func::CallShape::native`] 0), the call counts toward
/// it as `jit::native_call` counts an int call, and when this call reaches
/// the count first, the function is given up ([`super::jit::NATIVE_REFUSED`]):
/// its later calls are not looked at again. Before, such a call returned
/// before `native_call` counted anything, so a function called with
/// strings or arrays was never decided on and every call of it paid the
/// frame lookup and the argument test (53 instructions instead of the 19
/// of a refused function's call). Whether a function runs as native code
/// is not observable.
#[inline(never)]
fn not_int_argument(uf: &UserFunction) {
    let shape = &uf.call_shape;
    if shape.native.get() != 0 {
        return;
    }
    let n = shape.native_count.get() + 1;
    if n >= NATIVE_TRY_CALLS {
        shape.native_count.set(0);
        shape.native.set(super::jit::NATIVE_REFUSED);
    } else {
        shape.native_count.set(n);
    }
}

/// Whether the prepared call is a builtin's ([`call_native`]).
impl Prepared {
    pub(super) fn is_native(&self) -> bool {
        matches!(self.callee, Callee::Native(..))
    }

    /// The function id of a call [`prepare_quick`] prepared.
    pub(super) fn quick_id(&self) -> Option<u8> {
        match self.callee {
            Callee::Quick(id, _) => Some(id),
            _ => None,
        }
    }
}

/// Whether every argument of a direct builtin call is a value its
/// parameter takes as it is ([`crate::func::Builtin::accepts`]; `last`,
/// the table's last entry, covers the variadic parameter's positions).
///
/// Always inline, a plain loop: out of line, as an `all` over the
/// enumerated iterator, it cost ~33 instructions a one-argument call.
#[inline(always)]
#[allow(clippy::inline_always)] // once, in `call_native`
fn args_accepted(args: &[Option<Value>], table: &[u32], last: u32) -> bool {
    for (i, arg) in args.iter().enumerate() {
        let bits = table.get(i).copied().unwrap_or(last);
        match arg {
            Some(v) if crate::func::accept_bit(v) & bits != 0 => {}
            _ => return false,
        }
    }
    true
}

/// `ZEND_DO_ICALL`'s store of a builtin's result into the caller's
/// temporary `dst` (`Vm::set_tmp` once the call frame is popped), the old
/// value released only when it owns something. In line: the closure
/// calling `set_tmp` came out of line, ~35 instructions a call.
#[inline(always)]
#[allow(clippy::inline_always)] // once, in `call_native`
fn store_native_result(vm: &mut Vm, dst: u32, v: Value) {
    let top = vm.frames.live.wrapping_sub(1);
    let Some(slot) = vm
        .frames
        .vec
        .get_mut(top)
        .and_then(|f| f.slots.tmps_mut().get_mut(dst as usize))
    else {
        drop(v);
        return;
    };
    let old = std::mem::replace(slot, v);
    if matches!(
        old,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        std::mem::forget(old);
    } else {
        drop(old);
    }
}

/// `ZEND_DO_ICALL` of the prepared builtin call (result to `dst`; the
/// caller resumes at `caller_ip`), as `Vm::call_builtin_impl` makes a
/// plain call: the pooled frame the arguments were sent into becomes the
/// builtin's frame (php-src's call frame, which backtraces see), the
/// handler runs with copies of the arguments and the declared defaults of
/// the omitted trailing parameters, the interrupt check follows
/// (`ZEND_VM_FCALL_INTERRUPT_CHECK`), destructors queued meanwhile run
/// while the frame is still there, and the frame goes back to the pool.
///
/// `Ok` is the call's outcome: the handler's exception or exit, or the
/// result stored in `dst` (released when unused). `Err` gives the call
/// back, having changed nothing, when the generic call would treat it
/// otherwise (an argument count it reports, an argument it coerces or
/// rejects, the call not being the caller's newest): the caller
/// materializes it and the generic `DoCall` runs.
#[inline(never)]
#[allow(clippy::too_many_lines)] // one call's steps, in `call_builtin_impl`'s order
pub(super) fn call_native(
    vm: &mut Vm,
    prep: Prepared,
    dst: Option<u32>,
    caller_ip: usize,
) -> Result<VmResult<()>, Prepared> {
    let Callee::Native(b, kind) = &prep.callee else {
        return Err(prep);
    };
    // A pure method (`ReflectionMethod::getName()`): its handler without a
    // frame of its own; on an error the call is made as below.
    if b.flags & flags::PURE != 0 && pure_args(b, prep.n) {
        if let Some(r) = call_pure(vm, b, prep.n, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    // A one-argument call `Vm::fast_builtin_call` evaluates without the
    // handler (`is_string()`, `count()`, ...): the same, in place. The ids
    // are global functions', found by name: a method of the same name
    // (`BcMath\Number::sqrt()`) is not one of them, as there.
    if prep.by_name && prep.n == 1 && b.fast_eval_id() != crate::call::fe::NONE {
        if let Some(r) = vm.fast_eval_prepared(b, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    // `ZEND_FRAMELESS_ICALL_n`: a function with a frameless class, called
    // on the caller's frame when its arguments allow it.
    if prep.by_name && b.flags & flags::FRAMELESS != 0 {
        if let Some(r) = vm.frameless_icall_prepared(b, prep.n, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    // A leaf builtin (`special::leaf`) whose arguments were computed
    // after the call started, so `leaf_now` could not take it.
    if prep.by_name && b.leaf_id() != crate::special::leaf::NONE {
        if let Some(r) = vm.leaf_prepared(b, prep.n, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    let n = prep.n;
    let live = vm.frames.live;
    {
        let sig = b.signature();
        let nparams = sig.params.len();
        let variadic = b.flags & flags::VARIADIC != 0;
        if n < sig.required as usize || (!variadic && n > nparams) {
            return Err(prep);
        }
        let (callers, pool) = vm.frames.vec.split_at(live);
        let (Some(caller), Some(frame)) = (callers.last(), pool.first()) else {
            return Err(prep);
        };
        if caller.calls.len() != prep.calls_len || frame.args.len() != n {
            return Err(prep);
        }
        // The types zpp accepts as they are (`call_builtin_impl`'s
        // positional fast path; anything else is coerced or rejected
        // there), precomputed per parameter (`Builtin::accepts`): a load
        // and a test per argument, nothing for a function whose
        // parameters take anything. A native callee's sent arguments are
        // all `Some` (`send`, `send_ref`).
        let table = b.accepts();
        if let Some(&last) = table.last() {
            if !args_accepted(&frame.args, table, last) {
                return Err(prep);
            }
        }
    }
    // A leaf function (`special::leaf`) with arguments of its class's
    // shapes: its handler has no use for a frame (it reports nothing,
    // throws nothing, runs no PHP code), so it is called on the caller's,
    // as a frameless function is. Not a method that happens to have such
    // a name (readied with its `$this` or scopes).
    let leaf = b.leaf_id();
    if leaf != crate::special::leaf::NONE && b.flags & (flags::DEPRECATED | flags::NODISCARD) == 0 {
        let frame = &vm.frames.vec[live];
        let mut sent: [&Value; LEAF_ARGS] = [&Value::Null; LEAF_ARGS];
        let mut plain = frame.args.len() <= LEAF_ARGS;
        for (slot, arg) in sent.iter_mut().zip(&frame.args) {
            match arg {
                Some(v) => *slot = v,
                None => plain = false,
            }
        }
        if plain
            && frame.this.is_none()
            && frame.called.is_none()
            && frame.scope.is_none()
            && vm.leaf_args(leaf, &sent[..frame.args.len()])
        {
            vm.frames.vec[live - 1].ip = caller_ip;
            if let Some(r) = vm.frameless_handler_call(b, n, dst, live) {
                return Ok(r);
            }
        }
    }
    let kind = *kind;
    // The call is made from here on: the handle moves out of the prepared
    // call (a clone, and the drop of `Prepared` with its release, cost
    // ~15 instructions).
    let Callee::Native(b, _) = prep.callee else {
        // (Matched above.)
        return Ok(Ok(()));
    };
    // A by-reference argument was sent ([`send_ref`]).
    let sent_ref = b.sends_ref_within(n);
    // The handler's copies of the arguments (php-src's handler reads them
    // in the frame), then the declared defaults of the omitted trailing
    // parameters, not marked as passed, in a pooled record taken whole for
    // the call (`CallBufs::take_args`; a nested builtin call meanwhile
    // takes the next one).
    //
    // A `match`, not `map_or(Value::Null, ..)`: that builds the null
    // first and drops it (an out-of-line drop call per argument). One
    // `extend` of the frame's slice and one `resize` store each length
    // once (a `push` per argument tested the capacity and stored the
    // length every time).
    let mut args = vm.call_bufs.take_args();
    args.values
        .extend(vm.frames.vec[live].args.iter().map(|arg| match arg {
            Some(v) => crate::func::copy_arg(v),
            None => Value::Null,
        }));
    let given = args.values.len();
    args.passed.resize(given, true);
    let sig = b.signature();
    let nparams = sig.params.len();
    let upto = if b.flags & flags::VARIADIC != 0 {
        nparams.saturating_sub(1)
    } else {
        nparams
    };
    while args.values.len() < upto {
        let i = args.values.len();
        match vm.builtin_default(&b, i) {
            Some(d) if i >= sig.required as usize => {
                args.values.push(d);
                args.passed.push(false);
            }
            _ => break,
        }
    }
    // `zend_vm_stack_push_call_frame` + `EG(current_execute_data) = call`:
    // the frame as `FrameStack::push_builtin` leaves it.
    //
    // A released frame's `func` and `builtin_name` are empty
    // (`Frame::release_native`), as `magic_call_name` normally is: each
    // is stored through `replace`, whose old value is dropped only when
    // there is one (a plain assignment called the out-of-line drop of the
    // old `Option` every time).
    {
        let frame = &mut vm.frames.vec[live];
        frame.reinit_for_builtin(&vm.empty_code);
        if let Some(old) = frame.func.replace(Func::Builtin(Rc::clone(&b))) {
            drop(old);
        }
        let name = (kind == PlainCall::Named).then(|| b.name.clone());
        if let Some(old) = std::mem::replace(&mut frame.builtin_name, name) {
            drop(old);
        }
        frame.observer_hidden = false;
    }
    // `SAVE_OPLINE()`: the handler may report the caller's line.
    vm.frames.vec[live - 1].ip = caller_ip;
    vm.frames.live = live + 1;
    vm.pending_call_dst = dst;
    if vm.magic_call_name.is_some() {
        vm.magic_call_name = None;
    }
    let handler = b.handler;
    // A method's `$this` and called scope, readied on the frame by
    // `prepare_method` (a function's frame has none).
    {
        let frame = &vm.frames.vec[live];
        if let Some(this) = &frame.this {
            args.this = Some(this.clone());
        }
        if let Some(called) = &frame.called {
            args.called = Some(Rc::clone(called));
        }
    }
    args.func = Some(b);
    let mut r = handler(vm, &mut args);
    // `zend_try_assign_typed_ref`: what the handler wrote to a typed
    // by-reference argument (a typed property sent by reference, a
    // variable bound to one) is checked and stored when it returns, and a
    // refused value's `TypeError` is this call's, as on the general path
    // (`Vm::call_builtin_impl`). Only a call that was sent a reference
    // can have queued such a write.
    if sent_ref {
        r = vm.flush_typed_arg_writes(r);
    }
    if r.is_ok() {
        if let Err(e) = vm.check_interrupt() {
            r = Err(e);
        }
    }
    if sent_ref {
        // The handler's handles of the sent references are kept until the
        // variables are unwrapped below (`release_copies_but_refs`).
        release_copies_but_refs(&mut args.values, &vm.frames.vec[live].args);
    } else {
        args.release_in_place();
    }
    // Released by the handler, destructed while its frame is current.
    if crate::object::has_pending_destructors() && vm.frames.live == live + 1 {
        if let Err(e) = vm.run_pending_destructors() {
            r = match (r, e) {
                (Ok(_), e) => Err(e),
                (Err(Throw::Exception(old)), Throw::Exception(new)) => {
                    crate::exceptions::chain_previous(&new, &old);
                    Err(Throw::Exception(new))
                }
                (Err(old), _) => Err(old),
            };
        }
    }
    if sent_ref {
        if vm.frames.live == live + 1 {
            unwrap_sent_refs(vm, live, &args.values);
        }
        args.release_in_place();
    }
    // Emptied (`Args::release_in_place`), back to the pool.
    vm.call_bufs.put_args(args);
    // Popped off the VM stack, frames left above it (none, normally)
    // first. Released in place (`release_native`), its argument buffer
    // stays with the pooled frame for the next call at this depth.
    if vm.frames.live > live + 1 {
        vm.frames.truncate(live + 1);
    }
    if vm.frames.live == live + 1 {
        // A method's `$this` and scopes go first, as `pop_frames_to`
        // releases them with the frame.
        let frame = &mut vm.frames.vec[live];
        if frame.this.is_some() || frame.scope.is_some() || frame.called.is_some() {
            frame.this = None;
            frame.scope = None;
            frame.called = None;
        }
    }
    if vm.frames.live == live + 1 && vm.frames.vec[live].release_native(&vm.empty_vars) {
        vm.frames.pop_cleared_top();
    } else {
        // Anything else as the generic call releases it.
        vm.pop_frames_to(live + 1);
    }
    let v = match r {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    match dst {
        Some(d) => store_native_result(vm, d, v),
        None => {
            if matches!(
                v,
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
            ) {
                std::mem::forget(v);
            } else {
                // `i_zval_ptr_dtor` of the unused result (`ZEND_DO_ICALL`):
                // what `array_pop()` took out of an array becomes a
                // possible root when it stays referenced.
                crate::gc::possible_root(&v);
                drop(v);
            }
        }
    }
    Ok(Ok(()))
}

/// The first half of [`Args::release_into`]'s release of a builtin call's
/// argument copies when a reference was sent ([`call_native`]): every copy
/// released front to back, as there, except the handler's untouched
/// handles of the sent references (the same reference as the frame's
/// argument at that position), which are left in place for
/// [`unwrap_sent_refs`] and released by `release_into` after it. Those
/// can never be a last handle (the frame holds one), so holding them a
/// little longer frees and destructs nothing later than before.
#[inline(never)]
fn release_copies_but_refs(values: &mut [Value], sent: &[Option<Value>]) {
    for (i, slot) in values.iter_mut().enumerate() {
        let keep = match &*slot {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => true,
            Value::Reference(c) => {
                matches!(sent.get(i), Some(Some(Value::Reference(r))) if php_runtime::PhpRef::ptr_eq(c, r))
            }
            _ => false,
        };
        if !keep {
            drop(std::mem::take(slot));
        }
    }
}

/// After a builtin call [`send_ref`] sent a variable to by reference
/// (`live` the index of the builtin's frame, still current): each such
/// variable whose reference nobody else holds (two handles: the
/// variable's and the call frame's argument, about to be released) gets
/// the referenced value back as a plain value. php-src leaves it a
/// reference counted once, which no program can tell from the value: a
/// read, a write, `count()`, `foreach`, `get_defined_vars()` (whose copy
/// of the table derefs it, `zend_array_dup_element`) and a later `&` all
/// behave the same, and a variable bound by `global`, `static`, a
/// by-reference closure `use`, another variable or an element, or kept by
/// the handler (`mysqli_stmt_bind_param()`) has more handles and is left
/// alone, as is a reference with type sources (a typed property's). Our
/// fast paths for reads, writes and `count()` of a variable mostly decline
/// a reference: `$m[1]` after `preg_match($re, $s, $m)` cost ~95
/// instructions more, `count($stack)` after `array_pop($stack)` ~440 and
/// assigning the variable ~420.
///
/// `held` are the handler's copies of the arguments, released after this:
/// a reference still among them has one handle more (three, then), and is
/// unwrapped while that copy holds it, so that the copy's release later
/// finds the reference holding nothing collectable. Released first (the
/// variable's handle, then the copy's), each decrement left the reference
/// holding the array the handler wrote (`$matches`) with handles to spare,
/// a possible cycle root: `ref_roots::buffer` buffered it (~85
/// instructions a `preg_match($re, $s, $m)`), which php-src's
/// `zval_ptr_dtor_nogc` of the call's arguments never does.
#[inline(never)]
fn unwrap_sent_refs(vm: &mut Vm, live: usize, held: &[Value]) {
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first()) else {
        return;
    };
    // A handler that attached the caller's symbol table (`extract()`)
    // leaves the variables to it.
    if caller.symtab_attached {
        return;
    }
    for (i, arg) in frame.args.iter().enumerate() {
        let Some(Value::Reference(r)) = arg else {
            continue;
        };
        let copies = match held.get(i) {
            Some(Value::Reference(c)) if php_runtime::PhpRef::ptr_eq(c, r) => 3,
            _ => 2,
        };
        if r.refcount() != copies || r.meta().is_some() {
            continue;
        }
        let Some(slot) = caller.slots.cvs_mut().iter_mut().find(
            |s| matches!(s.get(), Some(Value::Reference(c)) if php_runtime::PhpRef::ptr_eq(c, r)),
        ) else {
            continue;
        };
        let Some(v) = r.try_borrow_mut().map(|mut v| std::mem::take(&mut *v)) else {
            continue;
        };
        // The variable's handle: the argument's still holds the
        // reference, so nothing is released here.
        drop(slot.set(Some(v)));
    }
}

/// Whether a call of pure builtin `b` with `n` arguments is pure: always,
/// for `getAttributes()` with at most its two parameters (`$flags` with
/// `ReflectionAttribute::IS_INSTANCEOF` look classes up, which may
/// autoload: called without its frame, the handler declines whatever is
/// not loaded yet, `reflection::attribute::attributes_array_decl`).
fn pure_args(b: &Builtin, n: usize) -> bool {
    n <= 2 || !b.name.as_bytes().eq_ignore_ascii_case(b"getattributes")
}

/// The prepared call of a pure builtin method `b` ([`flags::PURE`]) made
/// without its frame: the arguments sent into the pooled frame, checked as
/// [`call_native`] checks them (anything a coercion would change declines),
/// the declared defaults of the omitted trailing parameters after them,
/// and the handler called with the readied `$this` and called scope while
/// the caller is the current frame (its resume point saved first, as
/// `SAVE_OPLINE()`: a notice reports the caller's line, as with the
/// frame). On success the readied frame is emptied as its release would
/// empty it, the interrupt check follows and the result goes to `dst`.
/// `None`, having changed nothing that shows, when the arguments decline
/// or the handler fails: its error is dropped (such a handler changed
/// nothing before failing) and the call is made with its frame, so that
/// the error and its trace are the framed call's.
fn call_pure(
    vm: &mut Vm,
    b: &Rc<Builtin>,
    n: usize,
    calls_len: usize,
    dst: Option<u32>,
    caller_ip: usize,
) -> Option<VmResult<()>> {
    let live = vm.frames.live;
    let sig = b.signature();
    let nparams = sig.params.len();
    let required = sig.required as usize;
    if n < required || (b.flags & flags::VARIADIC == 0 && n > nparams) {
        return None;
    }
    let mut values = vm.call_bufs.values.pop().unwrap_or_default();
    let mut passed = vm.call_bufs.passed.pop().unwrap_or_default();
    let (this, called) = {
        let (callers, pool) = vm.frames.vec.split_at(live);
        let (Some(caller), Some(frame)) = (callers.last(), pool.first()) else {
            vm.put_val_vec(values);
            vm.put_bool_vec(passed);
            return None;
        };
        if caller.calls.len() != calls_len || frame.args.len() != n {
            vm.put_val_vec(values);
            vm.put_bool_vec(passed);
            return None;
        }
        for (i, arg) in frame.args.iter().enumerate() {
            let ok = match (arg, sig.params.get(i).or(sig.params.last())) {
                (Some(v), Some(p)) => p.ty.as_ref().is_none_or(|ty| {
                    !p.by_ref
                        && (p.promote & crate::opcode::UNCHECKED_TYPE != 0
                            || ty.is_mixed()
                            || accepts_without_coercion(ty.mask, v))
                }),
                _ => false,
            };
            if !ok {
                vm.put_val_vec(values);
                vm.put_bool_vec(passed);
                return None;
            }
            values.push(arg.as_ref().map_or(Value::Null, clone_value));
        }
        (frame.this.clone(), frame.called.clone())
    };
    passed.resize(n, true);
    while values.len() < nparams {
        let i = values.len();
        match vm.builtin_default(b, i) {
            Some(d) if i >= required => {
                values.push(d);
                passed.push(false);
            }
            _ => break,
        }
    }
    vm.frames.vec[live - 1].ip = caller_ip;
    let mut args = Args {
        values,
        passed,
        named: Vec::new(),
        this,
        called,
        func: Some(Rc::clone(b)),
    };
    let r = (b.handler)(vm, &mut args);
    let Args { values, passed, .. } = args;
    vm.put_val_vec(values);
    vm.put_bool_vec(passed);
    let v = r.ok()?;
    {
        let frame = &mut vm.frames.vec[live];
        frame.args.clear();
        frame.this = None;
        frame.scope = None;
        frame.called = None;
    }
    if let Err(e) = vm.check_interrupt() {
        drop(v);
        return Some(Err(e));
    }
    match dst {
        Some(d) => vm.set_tmp(d, v),
        None => drop(v),
    }
    Some(Ok(()))
}

/// The bound variables of the frame's closure (`use`), copied into their
/// compiled variables after the arguments, as `Vm::bind_closure_vars` does
/// once `Vm::push_simple_ucall` pushed the frame (the closure's `$this` and
/// scopes are the frame's already, and a plain call's closure has no
/// static variables).
///
/// Inline: out of line (as it came out once the tape's builtin call was
/// added) it cost every closure call ~20 instructions.
#[inline(always)]
#[allow(clippy::inline_always)]
fn bind_closure_vars(frame: &mut crate::vm::Frame) {
    let Some(c) = frame.closure.as_ref() else {
        return;
    };
    let internal = crate::object::data(c).internal.borrow();
    let Some(d) = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
    else {
        return;
    };
    for (cv, v) in &d.vars {
        if let Some(slot) = frame.slots.cvs_mut().get_mut(*cv as usize) {
            if let Some(old) = slot.as_mut().replace(v.clone()) {
                drop(old);
            }
        }
    }
}

/// Turns the prepared call into the [`PendingCall`] the generic
/// `InitFCall` and `Send`s would have built (the arguments sent so far in
/// its inline buffer), in its place among the caller's pending calls, and
/// returns the frame to the pool.
#[inline(never)]
pub(super) fn materialize(vm: &mut Vm, prep: Prepared) {
    // A quick call readied nothing: the pending call its `InitFCall` would
    // have built, from the call site's cache (resolved again if the cache
    // was cleared while the argument was computed: the builtin is still
    // there). Nothing newer is pending: a call started since would have
    // materialized this one first.
    if let Callee::Quick(_, init_at) = prep.callee {
        let Some(caller) = vm.frames.last_mut() else {
            return;
        };
        let Some(&Op::InitFCall { name, display, .. }) = caller.code.ops.get(init_at as usize)
        else {
            return;
        };
        if !crate::call::init_fcall_in(caller, name) {
            let made = vm.op_init_fcall(name, None, display);
            debug_assert!(made.is_ok(), "a builtin's call site resolves");
        }
        return;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return;
    };
    let uf = match prep.callee {
        Callee::User(uf) => uf,
        // (Returned above.)
        Callee::Quick(..) => return,
        Callee::Native(b, _) => {
            // `InitFCall` (or `InitMethodCall`) and the `Send`s of the
            // generic protocol: a direct call (a method's: with `$this`
            // and its scopes) with its arguments inline. The frame was not
            // readied.
            let mut call = PendingCall {
                direct: prep.fcall,
                dynamic: prep.closure,
                this: frame.this.take(),
                scope: frame.scope.take(),
                called: frame.called.take(),
                ..PendingCall::new(Func::Builtin(b))
            };
            for (arg, sent) in call.inline_args.iter_mut().zip(frame.args.drain(..)) {
                *arg = sent;
            }
            #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
            {
                call.inline_count = prep.n as u8;
            }
            let at = prep.calls_len.min(caller.calls.len());
            caller.calls.insert(at, call);
            return;
        }
    };
    let mut call = PendingCall {
        direct: prep.fcall,
        dynamic: prep.closure,
        is_ctor: prep.ctor,
        this: frame.this.take(),
        scope: frame.scope.take(),
        called: frame.called.take(),
        closure: frame.closure.take(),
        ..PendingCall::new(Func::User(uf))
    };
    for (arg, slot) in call
        .inline_args
        .iter_mut()
        .zip(&mut frame.slots.cvs_mut()[..prep.n])
    {
        *arg = slot.as_mut().take();
    }
    #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
    {
        call.inline_count = prep.n as u8;
    }
    let at = prep.calls_len.min(caller.calls.len());
    caller.calls.insert(at, call);
    frame.release_unentered(&vm.empty_code);
}

// ----------------------------------------------------------------------
// `call_user_func()` / `call_user_func_array()` written by name
// ----------------------------------------------------------------------

/// `ZEND_INIT_USER_CALL` of `Op::InitUserCall { callee, .. }` at `at` in
/// the current frame, on the call site's cache hit
/// (`Vm::user_call_cached`, which `Vm::remember_user_call` filled): the
/// callback, read from `callee` (a literal, a temporary, a compiled
/// variable), is the function a string names as last time, the method an
/// `[$object, "name"]` array names for the same class and calling scope,
/// the static method a `"Class::name"` string or a `['Class', 'name']`
/// array names likewise, a closure, or an invokable object. When the callee is a user function
/// whose declaration makes a plain call (or a builtin whose call is
/// plain), the pooled frame above the current one is readied for it as
/// [`prepare`], [`prepare_method`] and [`prepare_closure`] ready it, and
/// the call is prepared: a dynamic call (`ZEND_CALL_DYNAMIC`, the pending
/// call's `dynamic`), not a direct one. A temporary callback is consumed,
/// as the generic op reads it. `None`, having changed nothing, otherwise:
/// the generic op resolves the callback (`zend_is_callable`) and reports
/// what it must.
pub(super) fn prepare_user_call(vm: &mut Vm, callee: Operand, at: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let prepared = {
        let cb = match callee {
            Operand::Cv(cv) if !caller.symtab_attached => caller
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get)?,
            Operand::Tmp(t) => caller.slots.tmps().get(t as usize)?,
            Operand::Lit(l) => caller.code.literals.get(l as usize)?,
            _ => return None,
        };
        match cb {
            Value::Str(s) => user_call_named(caller, frame, s.as_bytes(), at)?,
            Value::Array(a) => user_call_method(caller, frame, a, at)?,
            Value::Object(o) => user_call_object(caller, frame, o)?,
            _ => return None,
        }
    };
    // `FREE_OP2` of a temporary callback (the call holds what it needs).
    if let Operand::Tmp(t) = callee {
        if let Some(slot) = caller.slots.tmps_mut().get_mut(t as usize) {
            drop(std::mem::take(slot));
        }
    }
    Some(prepared)
}

/// `zend_is_callable` of the callback `callee` of the `InitUserCall` at
/// `at` (the current frame) when the call site's cache missed, for the
/// two callbacks a hook list holds, without the generic resolution's
/// scope context and name copies: a function named by a string (any
/// case, one leading `\`), found by one lookup in the function table;
/// an `[$object, "name"]` array naming a public, concrete user method the
/// object's class declares or inherits, found by one lookup in its method
/// table, when the calling scope has no private method of that name (which
/// `zend_is_callable_check_func` would pick instead). The resolution is
/// what the generic one gives (`PendingCall` with `this`, the declaring
/// scope and the object's class as the called scope; a dynamic call), and
/// is remembered in the site's cache as `Vm::remember_user_call` would
/// remember it; the frame's resume point is stored first (`SAVE_OPLINE()`)
/// and a temporary callback is released (`FREE_OP2`). `None`, having
/// changed nothing, for anything else: names with `::`, references,
/// `__call`, private and protected methods, a class whose objects add
/// methods of their own (`get_method`), closures and invokable objects,
/// a name not found (the generic op reports it).
pub(super) fn user_call_quick(vm: &mut Vm, callee: Operand, at: usize) -> Option<PendingCall> {
    let (cb, call) = {
        let f = vm.frames.vec.get(vm.frames.live.checked_sub(1)?)?;
        let cb = match callee {
            Operand::Cv(cv) if !f.symtab_attached => f
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get)?,
            Operand::Tmp(t) => f.slots.tmps().get(t as usize)?,
            Operand::Lit(l) => f.code.literals.get(l as usize)?,
            _ => return None,
        };
        let call = match cb {
            Value::Str(s) => quick_function(&vm.functions, s.as_bytes())?,
            Value::Array(a) => quick_method(f, a)?,
            _ => return None,
        };
        (cb.clone(), call)
    };
    vm.frame_mut().ip = at + 1;
    vm.remember_user_call(&cb, &call);
    // `FREE_OP2` of a temporary callback (the call holds what it needs:
    // the function, or the method and its object).
    if let Operand::Tmp(t) = callee {
        if let Some(slot) = vm.frame_mut().slots.tmps_mut().get_mut(t as usize) {
            drop(std::mem::take(slot));
        }
    }
    Some(call)
}

/// The lowercase copy of `name` in `buf`, for a table lookup without an
/// allocation; `None` for a name longer than the buffer (the generic
/// resolution takes it).
#[inline]
fn lower_into<'b>(buf: &'b mut [u8; 64], name: &[u8]) -> Option<&'b [u8]> {
    let lc = buf.get_mut(..name.len())?;
    for (d, s) in lc.iter_mut().zip(name) {
        *d = s.to_ascii_lowercase();
    }
    Some(lc)
}

/// [`user_call_quick`] of a function named by a string: the function the
/// function table holds under its lowercase name, without a leading `\`
/// (`zend_is_callable`'s first lookup).
fn quick_function(functions: &crate::class::NameMap<Func>, name: &[u8]) -> Option<PendingCall> {
    let name = name.strip_prefix(b"\\").unwrap_or(name);
    if name.windows(2).any(|w| w == b"::") {
        return None;
    }
    let mut buf = [0u8; 64];
    let func = functions.get(lower_into(&mut buf, name)?)?.clone();
    let mut call = PendingCall::new(func);
    call.dynamic = true;
    Some(call)
}

/// [`user_call_quick`] of an `[$object, "name"]` array, in the calling
/// frame `f`.
fn quick_method(f: &crate::vm::Frame, a: &php_runtime::ZArray) -> Option<PendingCall> {
    use php_runtime::ArrayKey;
    if a.len() != 2 {
        return None;
    }
    let (Some(Value::Object(o)), Some(Value::Str(m))) =
        (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1)))
    else {
        return None;
    };
    let name = m.as_bytes();
    if name.windows(2).any(|w| w == b"::") {
        return None;
    }
    let cls = crate::object::class_of(o);
    if cls.hooks.as_ref().is_some_and(|h| h.get_method.is_some()) {
        return None;
    }
    let mut buf = [0u8; 64];
    let lc = lower_into(&mut buf, name)?;
    let meth = cls.methods.get(lc)?;
    if meth.visibility() != flags::PUBLIC
        || meth.is_abstract()
        || !matches!(meth.func, Func::User(_))
    {
        return None;
    }
    // `zend_is_callable_check_func`: a private method of the calling
    // scope with that name is the one called when the method's class is
    // related to it; any private one of that name goes the generic way.
    if f.scope
        .as_ref()
        .and_then(|s| s.methods.get(lc))
        .is_some_and(|pm| pm.visibility() == flags::PRIVATE)
    {
        return None;
    }
    let mut call = PendingCall {
        this: (!meth.is_static()).then(|| o.clone()),
        scope: Some(meth.scope()),
        called: Some(Rc::clone(cls)),
        ..PendingCall::new(meth.func.clone())
    };
    call.dynamic = true;
    Some(call)
}

/// The call `Vm::user_call_pending` resolved for an `InitUserCall` the
/// call site's cache did not hold, prepared as [`prepare_user_call`]
/// prepares a hit: a user function, method or closure whose declaration
/// makes a plain call, with the call's `$this`, scopes and closure on the
/// readied frame, or a plain builtin function. `Err` gives the call back,
/// nothing readied, for anything else (a `__call` trampoline, a callee
/// that makes no plain call, an observer): the caller pushes it as the
/// pending call, as the generic op would have.
#[allow(clippy::result_large_err)] // the call given back is pushed as it is
pub(super) fn prepare_pending(vm: &mut Vm, call: PendingCall) -> Result<Prepared, PendingCall> {
    if vm.observer.enabled
        || call.magic_name.is_some()
        || call.pfa.is_some()
        || call.is_ctor
        || call.has_named
        || call.inline_count != 0
        || !call.args.is_empty()
        || !call.named.is_empty()
    {
        return Err(call);
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return Err(call);
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return Err(call);
    };
    let (callee, limit) = match &call.func {
        Func::User(uf) => {
            // A closure's static variables are its own (as for `$f()`,
            // [`prepare_closure`]).
            let limit = if call.closure.is_some() {
                (uf.call_shape() != CallShape::NOT_PLAIN).then(|| uf.params.len().min(MAX_ARGS))
            } else {
                prepares(uf)
            };
            let Some(limit) = limit else {
                return Err(call);
            };
            (Callee::User(Rc::clone(uf)), limit)
        }
        Func::Builtin(b) => {
            if call.this.is_some()
                || call.scope.is_some()
                || call.called.is_some()
                || call.closure.is_some()
                || b.plain_call() != PlainCall::Named
                || forbids_dynamic_call(b)
                || !frame.args.is_empty()
            {
                return Err(call);
            }
            let limit = if b.flags & flags::VARIADIC != 0 {
                MAX_ARGS
            } else {
                b.signature().params.len().min(MAX_ARGS)
            };
            (Callee::Native(Rc::clone(b), PlainCall::Named), limit)
        }
    };
    if let Callee::User(uf) = &callee {
        ready_frame(frame, uf);
        let PendingCall {
            this,
            scope,
            called,
            closure,
            ..
        } = call;
        frame.this = this;
        frame.scope = scope;
        frame.called = called;
        frame.closure = closure;
    }
    Ok(Prepared {
        callee,
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: true,
        binds: true,
        ctor: false,
    })
}

/// The builtins `zend_forbid_dynamic_call()` refuses to run as a dynamic
/// call (`Vm::forbidden_dynamic_call`): left to the generic call, which
/// raises the error.
fn forbids_dynamic_call(b: &Builtin) -> bool {
    matches!(
        b.name.as_bytes(),
        b"extract"
            | b"compact"
            | b"get_defined_vars"
            | b"func_get_args"
            | b"func_get_arg"
            | b"func_num_args"
    )
}

/// [`prepare_user_call`] of a string: a function by its name
/// ([`user_call_function`]), else a static method by `"Class::name"`
/// ([`user_call_static`]).
fn user_call_named(
    caller: &crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    name: &[u8],
    at: usize,
) -> Option<Prepared> {
    if let Some(p) = user_call_function(caller, frame, name, at) {
        return Some(p);
    }
    let colons = crate::call::find_double_colon(name)?;
    user_call_static(
        caller,
        frame,
        name.get(..colons)?,
        name.get(colons + 2..)?,
        at,
    )
}

/// [`prepare_user_call`] of a static method named by a `"Class::name"`
/// string or a `['Class', 'name']` array (`class`, `method`): the method
/// the op's slot of the method cache holds for that class, name and
/// calling scope (`crate::call::user_call_static_hit`), a user method
/// whose declaration makes a plain call, with its declaring scope and the
/// class as the called scope (no `$this`).
fn user_call_static(
    caller: &crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    class: &[u8],
    method: &[u8],
    at: usize,
) -> Option<Prepared> {
    let (func, scope, called) = crate::call::user_call_static_hit(caller, at, class, method)?;
    let Func::User(uf) = func else {
        return None;
    };
    let limit = prepares(&uf)?;
    ready_frame(frame, &uf);
    frame.scope = scope;
    frame.called = Some(called);
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: true,
        binds: true,
        ctor: false,
    })
}

/// [`prepare_user_call`] of a function named by the string `name` (any
/// case, one leading `\`): the function the call site's slot past the
/// literals holds (`Vm::user_call_cached`'s), when its name is that one.
fn user_call_function(
    caller: &crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    name: &[u8],
    at: usize,
) -> Option<Prepared> {
    let name = name.strip_prefix(b"\\").unwrap_or(name);
    let slot = caller.code.literals.len() + at + 1;
    let func = {
        let cache = caller.code.fcall_cache.try_borrow().ok()?;
        let entry = cache.get(slot)?.as_ref()?;
        // A builtin the tape does not call goes the generic way without
        // its handle being copied first.
        if let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func {
            if b.plain_call() != PlainCall::Named || forbids_dynamic_call(b) {
                return None;
            }
        }
        entry.get()?
    };
    if !func.name().as_bytes().eq_ignore_ascii_case(name) {
        return None;
    }
    let (callee, limit) = match func {
        Func::User(uf) => {
            let limit = prepares(&uf)?;
            ready_frame(frame, &uf);
            (Callee::User(uf), limit)
        }
        Func::Builtin(b) => {
            // A pooled frame's argument buffer is empty.
            if !frame.args.is_empty() {
                return None;
            }
            // Past the declared parameters only into a variadic one (else
            // the generic call reports the count).
            let limit = if b.flags & flags::VARIADIC != 0 {
                MAX_ARGS
            } else {
                b.signature().params.len().min(MAX_ARGS)
            };
            (Callee::Native(b, PlainCall::Named), limit)
        }
    };
    Some(Prepared {
        callee,
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: true,
        binds: true,
        ctor: false,
    })
}

/// [`prepare_user_call`] of an `[$object, "name"]` array: the method the
/// op's slot of the method cache holds for the object's class, the calling
/// scope and that name (`Vm::user_call_cached`'s hit), a user method whose
/// declaration makes a plain call, with `$this` (none for a static method),
/// its declaring scope and the object's class as the called scope; of a
/// `['Class', 'name']` array: [`user_call_static`].
fn user_call_method(
    caller: &crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    a: &php_runtime::ZArray,
    at: usize,
) -> Option<Prepared> {
    use php_runtime::ArrayKey;
    if a.len() != 2 {
        return None;
    }
    let (o, m) = match (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1))) {
        (Some(Value::Object(o)), Some(Value::Str(m))) => (o, m),
        (Some(Value::Str(c)), Some(Value::Str(m))) => {
            return user_call_static(caller, frame, c.as_bytes(), m.as_bytes(), at);
        }
        _ => return None,
    };
    let cls = crate::object::class_of(o);
    let (uf, scope, is_static) = {
        let cache = caller.code.method_cache.try_borrow().ok()?;
        let e = cache.get(at)?.as_ref()?;
        let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        if e.class_addr != Rc::as_ptr(cls) as usize
            || e.scope_addr != scope_addr
            || !e.name.as_bytes().eq_ignore_ascii_case(m.as_bytes())
        {
            return None;
        }
        let Some(Func::User(uf)) = e.func.get() else {
            return None;
        };
        let scope = match &e.method_scope {
            Some(w) => Some(w.upgrade()?),
            None => None,
        };
        (uf, scope, e.is_static)
    };
    let limit = prepares(&uf)?;
    ready_frame(frame, &uf);
    frame.this = (!is_static).then(|| o.clone());
    frame.scope = scope;
    frame.called = Some(Rc::clone(cls));
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        by_name: false,
        closure: true,
        binds: true,
        ctor: false,
    })
}

/// [`prepare_user_call`] of an object: a closure of a user function whose
/// declaration makes a plain call, with the closure's `$this`, scopes and
/// the closure itself (none for a fake one), as [`prepare_closure`]
/// readies `$f()` (`zend_is_callable` resolves it as `Vm::closure_call`
/// does); or an object of a class with an `__invoke` method that makes a
/// plain call, the object its `$this` (`zend_is_callable`'s
/// `get_closure` of an invokable object: no cache needed).
fn user_call_object(
    caller: &crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    o: &Obj,
) -> Option<Prepared> {
    let data = crate::object::data(o);
    let calls_len = caller.calls.len();
    if data.class.lc_name.as_bytes() == b"closure" {
        let internal = data.internal.try_borrow().ok()?;
        let d = internal
            .as_ref()
            .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())?;
        let Func::User(uf) = &d.func else {
            return None;
        };
        if d.magic_name.is_some() || uf.call_shape() == CallShape::NOT_PLAIN {
            return None;
        }
        let uf = Rc::clone(uf);
        ready_frame(frame, &uf);
        frame.this = d.this.clone();
        frame.scope = d.scope.clone();
        frame.called = d.called.clone();
        frame.closure = if d.fake { None } else { Some(o.clone()) };
        drop(internal);
        return Some(Prepared {
            limit: uf.params.len().min(MAX_ARGS),
            callee: Callee::User(uf),
            n: 0,
            calls_len,
            fcall: false,
            by_name: false,
            closure: true,
            binds: true,
            ctor: false,
        });
    }
    let cls = crate::object::class_of(o);
    let m = cls.magic.invoke.as_ref()?;
    let Func::User(uf) = &m.func else {
        return None;
    };
    let limit = prepares(uf)?;
    let scope = m.scope.upgrade()?;
    let uf = Rc::clone(uf);
    ready_frame(frame, &uf);
    frame.this = Some(o.clone());
    frame.scope = Some(scope);
    frame.called = Some(Rc::clone(cls));
    Some(Prepared {
        limit,
        callee: Callee::User(uf),
        n: 0,
        calls_len,
        fcall: false,
        by_name: false,
        closure: true,
        binds: true,
        ctor: false,
    })
}

/// `ZEND_SEND_USER` of `src` as argument `n` (1-based) of the prepared
/// call: [`send`] of the value. The prepared callee has no by-reference
/// parameter (a plain call's declaration has none, a plain builtin call's
/// signature neither), so the generic op's "must be passed by reference"
/// case does not arise. `false`, having changed nothing, where [`send`]
/// declines, and for an argument out of order.
pub(super) fn send_user(vm: &mut Vm, prep: &mut Prepared, src: Operand, n: u32) -> bool {
    if n as usize != prep.n + 1 {
        return false;
    }
    send(vm, prep, &ArgSrc::Val(src))
}

/// `ZEND_SEND_ARRAY` of the whole array `src` (a literal, a temporary,
/// a compiled variable) as the arguments of the prepared call, none sent
/// before: when every key is an integer (positional arguments, in the
/// array's order), no element is a reference, and the element count fits
/// the callee (for a user function: its required parameters up to its
/// declared ones, at most [`MAX_ARGS`]), each element is copied into the
/// callee's compiled variable (a builtin's: its argument buffer) as the
/// generic op's by-value sends copy it, and a temporary array is released
/// after, as the generic op releases it. `false`, having changed nothing,
/// otherwise: named arguments, references, by-reference parameters, the
/// counts the generic call reports, a value that is not an array (the
/// `TypeError`), an undefined variable (the warning).
pub(super) fn send_array(vm: &mut Vm, prep: &mut Prepared, src: Operand) -> bool {
    use php_runtime::ArrayKey;
    if prep.n != 0 {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let n = {
        let v = match src {
            Operand::Lit(l) => caller.code.literals.get(l as usize),
            Operand::Tmp(t) => caller.slots.tmps().get(t as usize),
            Operand::Cv(cv) if !caller.symtab_attached => caller
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get),
            _ => None,
        };
        let Some(Value::Array(ht)) = v else {
            return false;
        };
        let n = ht.len();
        let fits = match &prep.callee {
            Callee::User(uf) => n >= uf.required as usize && n <= prep.limit,
            Callee::Native(..) => n <= prep.limit,
            Callee::Quick(..) => false,
        };
        if !fits {
            return false;
        }
        for (k, v) in ht.iter() {
            if !matches!(k, ArrayKey::Int(_)) || v.is_reference() {
                return false;
            }
        }
        match &prep.callee {
            Callee::User(_) => {
                let Some(slots) = frame.slots.cvs_mut().get_mut(..n) else {
                    return false;
                };
                for (slot, (_, v)) in slots.iter_mut().zip(ht.iter()) {
                    // A prepared frame's compiled variables are empty.
                    if let Some(old) = slot.as_mut().replace(clone_value(v)) {
                        drop(old);
                    }
                }
            }
            // `ZEND_SEND_ARRAY` into the call frame's argument slots.
            Callee::Native(..) => {
                for (_, v) in ht.iter() {
                    frame.args.push(Some(clone_value(v)));
                }
            }
            Callee::Quick(..) => return false,
        }
        n
    };
    // `FREE_OP1` of a temporary array.
    if let Operand::Tmp(t) = src {
        if let Some(slot) = caller.slots.tmps_mut().get_mut(t as usize) {
            drop(std::mem::take(slot));
        }
    }
    prep.n = n;
    true
}

/// `ZEND_SEND_ARRAY` of `array_slice($args, skip, $len)`
/// (`zend_compile_func_cufa` with a literal offset; `Vm::op_send_array`'s
/// sliced form): the elements `array_slice()` would give, by position
/// whatever their keys, as the arguments of the prepared call, none sent
/// before, when `src` is an array, `len` an int or `null` (the rest), no
/// element passed is a reference and the count fits the callee as for
/// [`send_array`]; each is copied in as [`send_array`] copies it, and a
/// temporary array and length are released after (`FREE_OP1`,
/// `FREE_OP2`). `false`, having changed nothing, otherwise: a length the
/// generic op converts or rejects (`array_slice()`'s `?int $length`), an
/// undefined variable (the warning), a value that is not an array (the
/// `TypeError`), references, the counts the generic call reports.
pub(super) fn send_array_slice(
    vm: &mut Vm,
    prep: &mut Prepared,
    src: Operand,
    len: Operand,
    skip: u32,
) -> bool {
    if prep.n != 0 {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let n = {
        let read = |o: Operand| match o {
            Operand::Lit(l) => caller.code.literals.get(l as usize),
            Operand::Tmp(t) => caller.slots.tmps().get(t as usize),
            Operand::Cv(cv) if !caller.symtab_attached => caller
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get),
            _ => None,
        };
        let Some(Value::Array(ht)) = read(src) else {
            return false;
        };
        let (start, n) = match slice_range(ht.len(), read(len), skip) {
            Some(r) => r,
            None => return false,
        };
        let fits = match &prep.callee {
            Callee::User(uf) => n >= uf.required as usize && n <= prep.limit,
            Callee::Native(..) => n <= prep.limit,
            Callee::Quick(..) => false,
        };
        if !fits || ht.iter().skip(start).take(n).any(|(_, v)| v.is_reference()) {
            return false;
        }
        match &prep.callee {
            Callee::User(_) => {
                let Some(slots) = frame.slots.cvs_mut().get_mut(..n) else {
                    return false;
                };
                for (slot, (_, v)) in slots.iter_mut().zip(ht.iter().skip(start)) {
                    // A prepared frame's compiled variables are empty.
                    if let Some(old) = slot.as_mut().replace(clone_value(v)) {
                        drop(old);
                    }
                }
            }
            Callee::Native(..) => {
                for (_, v) in ht.iter().skip(start).take(n) {
                    frame.args.push(Some(clone_value(v)));
                }
            }
            Callee::Quick(..) => return false,
        }
        n
    };
    // `FREE_OP1` of a temporary array, `FREE_OP2` of a temporary length
    // (an int or null: its release runs nothing).
    for o in [src, len] {
        if let Operand::Tmp(t) = o {
            if let Some(slot) = caller.slots.tmps_mut().get_mut(t as usize) {
                drop(std::mem::take(slot));
            }
        }
    }
    prep.n = n;
    true
}

/// The first element and the count `array_slice()` of an array of
/// `count` elements gives from `skip` (a non-negative literal) with the
/// length `len` (`Vm::send_array_slice`'s arithmetic); `None` for a
/// length that is not an int or `null` (converted or rejected by the
/// generic op) or not at hand.
fn slice_range(count: usize, len: Option<&Value>, skip: u32) -> Option<(usize, usize)> {
    let skip = skip as usize;
    let len = match len? {
        Value::Int(l) => *l,
        Value::Null => i64::try_from(count.wrapping_sub(skip)).unwrap_or(i64::MAX),
        _ => return None,
    };
    if skip >= count {
        return Some((skip, 0));
    }
    let rest = i64::try_from(count - skip).unwrap_or(i64::MAX);
    let len = if len < 0 { len + rest } else { len };
    if len <= 0 {
        return Some((skip, 0));
    }
    Some((skip, usize::try_from(len.min(rest)).unwrap_or(usize::MAX)))
}

// ----------------------------------------------------------------------
// Calls by a name known only at run time
// ----------------------------------------------------------------------

/// `ZEND_INIT_DYNAMIC_CALL` of `$f()` for a compiled variable holding a
/// function's name (`zend_init_dynamic_call_string`): the function found
/// by one lookup of its lowercase name (one leading `\` dropped), pushed
/// as the pending call the generic op pushes (`ZEND_CALL_DYNAMIC`).
/// `false`, having changed nothing, for anything else: a `Class::name`
/// string, a name not found (the generic op's `Error`), another value, a
/// reference, an undefined variable, a frame with an attached symbol
/// table.
pub(super) fn init_dyn_call_named(vm: &mut Vm, cv: u32) -> bool {
    let call = {
        let Some(f) = vm.frames.vec.get(vm.frames.live.wrapping_sub(1)) else {
            return false;
        };
        if f.symtab_attached {
            return false;
        }
        let Some(Value::Str(s)) = f
            .slots
            .cvs()
            .get(cv as usize)
            .and_then(crate::vm::CvSlot::get)
        else {
            return false;
        };
        match quick_function(&vm.functions, s.as_bytes()) {
            Some(call) => call,
            None => return false,
        }
    };
    vm.frame_mut().calls.push(call);
    true
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `$c::name()` (`class` the operand
/// of a dynamic class: a compiled variable or a temporary holding a
/// class's name, or a compiled variable holding an object; `lit` the
/// method name literal) for a class already declared, found by one
/// lookup of its name (`Vm::find_class`: no autoload), and a public,
/// static, concrete method of it: pushed as the pending call with the
/// method's declaring scope and the class as the called scope, no
/// `$this` (php-src: `ce` the fetched class, `zend_std_get_static_method`
/// finding a public method). A temporary name is released (`FREE_OP1`).
/// `false`, having changed nothing, for anything else: an object in a
/// temporary, a class not declared yet (the autoload, the `Error`), a
/// method that is not public, not static (the `$this` rules), abstract,
/// missing (`__callStatic`, the `Error`), a reference, an undefined
/// variable, a frame with an attached symbol table.
pub(super) fn init_dyn_static_call(vm: &mut Vm, class: Operand, lit: u32) -> bool {
    let (cls, m) = {
        let Some(f) = vm.frames.vec.get(vm.frames.live.wrapping_sub(1)) else {
            return false;
        };
        let v = match class {
            Operand::Cv(cv) if !f.symtab_attached => f
                .slots
                .cvs()
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get),
            Operand::Tmp(t) => f.slots.tmps().get(t as usize),
            _ => None,
        };
        let cls = match v {
            Some(Value::Str(name)) => match vm.find_class(name.as_bytes()) {
                Some(c) => c,
                None => return false,
            },
            Some(Value::Object(o)) if matches!(class, Operand::Cv(_)) => {
                Rc::clone(crate::object::class_of(o))
            }
            _ => return false,
        };
        let Some(Value::Str(mname)) = f.code.literals.get(lit as usize) else {
            return false;
        };
        let mut buf = [0u8; 64];
        let Some(lc) = lower_into(&mut buf, mname.as_bytes()) else {
            return false;
        };
        let Some(m) = cls.methods.get(lc) else {
            return false;
        };
        if m.visibility() != flags::PUBLIC || !m.is_static() || m.is_abstract() {
            return false;
        }
        let m = Rc::clone(m);
        (cls, m)
    };
    if let Operand::Tmp(t) = class {
        if let Some(slot) = vm.frame_mut().slots.tmps_mut().get_mut(t as usize) {
            drop(std::mem::take(slot));
        }
    }
    vm.frame_mut().calls.push(PendingCall {
        scope: Some(m.scope()),
        called: Some(cls),
        ..PendingCall::new(m.func.clone())
    });
    true
}
