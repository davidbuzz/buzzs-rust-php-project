//! The interpreter loop (`execute_ex`) and simple instructions.

use crate::object;
use crate::opcode::{BinKind, CastKind, Op, Operand, UnKind};
use crate::vm::{FinState, Frame, RetTarget, Throw, Vm, VmResult};
use php_runtime::{ops, BinaryOp, ErrorLevel, PhpRef, Value, ZArray, ZStr};
use std::rc::Rc;

/// Upper bound on nested `execute()` runs (internal-to-user callbacks,
/// destructors, `__clone`, magic methods); each one uses native stack.
const MAX_NESTED_EXECUTIONS: u32 = 20000;

/// A number peeked from an operand for the sealed [`Op::Bin`] forms.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    #[inline]
    #[allow(clippy::cast_precision_loss)] // php-src's (double) widening
    fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

/// What an instruction asks the loop to do next.
pub(crate) enum Flow {
    /// Continue with the next instruction of the current frame.
    Next,
    /// The frame stack changed: reload the current frame.
    Reload,
    /// The base frame finished with this value.
    Done(Value),
}

impl Vm {
    /// The current frame.
    #[inline]
    pub(crate) fn frame(&self) -> &Frame {
        self.frames.last().expect("active frame")
    }

    /// The current frame (mutable).
    #[inline]
    pub(crate) fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("active frame")
    }

    /// Reads a literal of the current frame.
    #[inline]
    pub(crate) fn lit(&self, i: u32) -> Value {
        self.frame().code.literals[i as usize].clone()
    }

    /// Reads a literal as a string.
    pub(crate) fn lit_str(&self, i: u32) -> ZStr {
        match &self.frame().code.literals[i as usize] {
            Value::Str(s) => s.clone(),
            other => ZStr::from(format!("{other:?}")),
        }
    }

    /// The current numeric value of a CV or literal operand, without
    /// cloning, warnings or reference traversal (`None` sends the op down
    /// the generic path). Always inlined into [`Vm::bin_num_fast`]: as a
    /// call it returns its `Option<Num>` through memory.
    #[inline(always)]
    #[allow(clippy::inline_always)] // as a call, the `Option<Num>` is returned through memory
    fn peek_num(f: &Frame, op: Operand) -> Option<Num> {
        match op {
            // In place, without a borrow guard (`EX_VAR`).
            Operand::Cv(cv) => f.peek_cv(cv, |s| match s {
                Some(Value::Int(i)) => Some(Num::Int(*i)),
                Some(Value::Float(x)) => Some(Num::Float(*x)),
                _ => None,
            }),
            Operand::Lit(i) => match f.code.literals.get(i as usize) {
                Some(Value::Int(v)) => Some(Num::Int(*v)),
                Some(Value::Float(v)) => Some(Num::Float(*v)),
                _ => None,
            },
            // Numbers have no Drop: the slot's value can be left in place
            // (consuming reads leave Null behind, but every later use of a
            // tmp slot is preceded by a write).
            Operand::Tmp(t) | Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
                Some(Value::Int(i)) => Some(Num::Int(*i)),
                Some(Value::Float(x)) => Some(Num::Float(*x)),
                _ => None,
            },
            Operand::Unused => None,
        }
    }

    /// Sealed numeric form of [`Op::Bin`]: the handlers php-src specializes
    /// for `LONG`/`DOUBLE` operand pairs, computed without materializing
    /// `Value` temporaries. `None` when an operand is not a plain defined
    /// number or the result leaves the int domain (overflow → float
    /// promotion is the generic path's job). Mixed int/float pairs use
    /// php-src's `(double)` widening; `===`/`!==` on mixed types is a type
    /// mismatch.
    #[inline]
    fn bin_num_fast(&self, op: BinKind, a: Operand, b: Operand) -> Option<Value> {
        let f = self.frame();
        let x = Self::peek_num(f, a)?;
        let y = Self::peek_num(f, b)?;
        if let (Num::Int(x), Num::Int(y)) = (x, y) {
            return Some(match op {
                BinKind::Arith(BinaryOp::Add) => Value::Int(x.checked_add(y)?),
                BinKind::Arith(BinaryOp::Sub) => Value::Int(x.checked_sub(y)?),
                BinKind::Arith(BinaryOp::Mul) => Value::Int(x.checked_mul(y)?),
                BinKind::Arith(BinaryOp::BitOr) => Value::Int(x | y),
                BinKind::Arith(BinaryOp::BitAnd) => Value::Int(x & y),
                BinKind::Arith(BinaryOp::BitXor) => Value::Int(x ^ y),
                BinKind::Equal | BinKind::Identical => Value::Bool(x == y),
                BinKind::NotEqual | BinKind::NotIdentical => Value::Bool(x != y),
                BinKind::Less => Value::Bool(x < y),
                BinKind::LessEqual => Value::Bool(x <= y),
                BinKind::Spaceship => Value::Int(match x.cmp(&y) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }),
                _ => return None,
            });
        }
        let mixed = matches!(
            (x, y),
            (Num::Int(_), Num::Float(_)) | (Num::Float(_), Num::Int(_))
        );
        #[allow(clippy::cast_precision_loss)] // php-src's (double) widening
        let (xf, yf) = (x.as_f64(), y.as_f64());
        Some(match op {
            BinKind::Arith(BinaryOp::Add) => Value::Float(xf + yf),
            BinKind::Arith(BinaryOp::Sub) => Value::Float(xf - yf),
            BinKind::Arith(BinaryOp::Mul) => Value::Float(xf * yf),
            BinKind::Equal => Value::Bool(xf == yf),
            BinKind::NotEqual => Value::Bool(xf != yf),
            BinKind::Identical => Value::Bool(!mixed && xf == yf),
            BinKind::NotIdentical => Value::Bool(mixed || xf != yf),
            BinKind::Less => Value::Bool(xf < yf),
            BinKind::LessEqual => Value::Bool(xf <= yf),
            _ => return None,
        })
    }

    /// Generic [`Op::Bin`] evaluation, out of line so the dispatch loop
    /// stays small (the sealed numeric forms handle the hot cases).
    #[inline(never)]
    fn bin_slow(&mut self, op: BinKind, a: Operand, b: Operand, dst: u32) -> VmResult<()> {
        let x = self.read(a)?;
        let y = self.read(b)?;
        let r = match (op, a, x, y) {
            // `ZEND_CONCAT` with a TMP op1: a string nothing else holds is
            // extended in place (`zend_string_extend`), not copied.
            (BinKind::Arith(BinaryOp::Concat), Operand::Tmp(_), Value::Str(s), Value::Str(t)) => {
                Value::Str(s.concat_owned(t.as_bytes()))
            }
            // The same with an integer right operand (`"k" . $i . ...`
            // chains): its digits are appended straight from a stack
            // buffer, as `concat_function` does after `zval_get_string`.
            (BinKind::Arith(BinaryOp::Concat), Operand::Tmp(_), Value::Str(s), Value::Int(i)) => {
                let mut buf = [0u8; php_runtime::string::LONG_MAX_LEN];
                Value::Str(s.concat_owned(php_runtime::string::format_long(i, &mut buf)))
            }
            (_, _, x, y) => {
                let r = self.binary(op, &x, &y)?;
                drop((x, y));
                r
            }
        };
        self.set_tmp(dst, r);
        Ok(())
    }

    /// `ZEND_ROPE_INIT` / `ZEND_ROPE_ADD` / `ZEND_ROPE_END`: each part is
    /// read and converted to a string in order (warnings, `__toString`),
    /// then the result is built in ONE exactly sized allocation
    /// (`zend_string_alloc(len)` + `memcpy` per part), with integer parts
    /// formatted straight into it instead of through temporary strings.
    #[inline(never)]
    pub(crate) fn op_rope(&mut self, parts: &[Operand]) -> VmResult<ZStr> {
        // Most ropes have a handful of parts: keep them on the stack.
        const INLINE: usize = 8;
        let mut inline: [Value; INLINE] = Default::default();
        let mut spill: Vec<Value> = Vec::new();
        let vals: &mut [Value] = if parts.len() <= INLINE {
            &mut inline[..parts.len()]
        } else {
            spill.resize(parts.len(), Value::Null);
            spill.as_mut_slice()
        };
        let mut len = 0usize;
        for (slot, p) in vals.iter_mut().zip(parts) {
            let v = match self.read(*p)? {
                v @ (Value::Str(_) | Value::Int(_)) => v,
                v => Value::Str(self.to_zstr(&v)?),
            };
            len = len.saturating_add(match &v {
                Value::Str(s) => s.len(),
                Value::Int(i) => php_runtime::string::long_str_len(*i),
                _ => 0,
            });
            *slot = v;
        }
        let mut out = php_runtime::string::ZStrBuf::with_capacity(len);
        for v in vals.iter() {
            match v {
                Value::Str(s) => out.push(s.as_bytes()),
                Value::Int(i) => out.push_long(*i),
                _ => {}
            }
        }
        Ok(out.finish())
    }

    /// Reads an operand (dereferenced), warning on undefined variables.
    pub(crate) fn read(&mut self, op: Operand) -> VmResult<Value> {
        match op {
            Operand::Tmp(t) => {
                let v = std::mem::take(&mut self.frame_mut().tmps[t as usize]);
                Ok(match v {
                    Value::Reference(r) => r.get(),
                    v => v,
                })
            }
            Operand::TmpKeep(t) => Ok(self.frame().tmps[t as usize].deref()),
            Operand::Lit(i) => Ok(self.lit(i)),
            Operand::Cv(cv) => self.read_cv(cv),
            Operand::Unused => Ok(Value::Null),
        }
    }

    /// Reads an operand without dereferencing temporaries holding
    /// references.
    pub(crate) fn read_raw(&mut self, op: Operand) -> VmResult<Value> {
        match op {
            Operand::Tmp(t) => Ok(std::mem::take(&mut self.frame_mut().tmps[t as usize])),
            Operand::TmpKeep(t) => Ok(self.frame().tmps[t as usize].clone()),
            other => self.read(other),
        }
    }

    /// Reads a compiled variable (dereferenced), warning when undefined.
    /// The defined case is a borrow-free peek and a clone ([`Frame::peek_cv`]);
    /// the warning is out of line.
    #[inline]
    pub(crate) fn read_cv(&mut self, cv: u32) -> VmResult<Value> {
        match self.read_cv_defined(cv) {
            Some(v) => Ok(v),
            None => self.read_undefined_cv(cv),
        }
    }

    /// The undefined-variable path of [`Vm::read_cv`]: the warning, then null.
    #[cold]
    #[inline(never)]
    fn read_undefined_cv(&mut self, cv: u32) -> VmResult<Value> {
        let name = self.frame().code.cv_names[cv as usize].clone();
        self.warning(&crate::exec::undefined_var_msg(&name))?;
        Ok(Value::Null)
    }

    /// [`Vm::read_cv`] for a defined variable (dereferenced); `None` when
    /// undefined, leaving the warning to the caller.
    #[inline]
    pub(crate) fn read_cv_defined(&self, cv: u32) -> Option<Value> {
        self.frame().peek_cv(cv, |s| match s {
            Some(Value::Reference(r)) => Some(r.get()),
            Some(v) => Some(v.clone()),
            None => None,
        })
    }

    /// The `OP_DATA` read of [`Op::Assign`] from a compiled variable:
    /// [`Vm::read_cv_defined`], except that a reference to null, false or
    /// an array is left to the place walk, which reads it after fetching
    /// the containers ([`Vm::place_assign_cv_ref`]).
    #[inline]
    pub(crate) fn read_cv_op_data(&self, cv: u32) -> CvOpData {
        self.frame().peek_cv(cv, |s| match s {
            Some(Value::Reference(r)) => {
                let inner = r.borrow();
                if matches!(*inner, Value::Null | Value::Bool(false) | Value::Array(_)) {
                    CvOpData::Deferred
                } else {
                    CvOpData::Value(inner.clone())
                }
            }
            Some(v) => CvOpData::Value(v.clone()),
            None => CvOpData::Undefined,
        })
    }

    /// Reads a compiled variable without warning (`None` if undefined).
    #[inline]
    pub(crate) fn cv_get(&self, cv: u32) -> Option<Value> {
        self.frame().peek_cv(cv, |s| s.cloned())
    }

    /// Assigns to a compiled variable (through references). A typed
    /// reference is handed back with the value, unassigned: the caller
    /// assigns through it with [`Vm::assign_ref`].
    ///
    /// The common case, a slot that is undefined or holds no reference,
    /// array or object (nothing to assign through, no possible GC root to
    /// buffer), is a plain store; the old value is dropped without the
    /// out-of-line drop call when it is a scalar.
    #[must_use]
    #[inline]
    pub(crate) fn cv_assign(&mut self, cv: u32, v: Value) -> Option<(PhpRef, Value)> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        let slot = &mut vars.slots[idx];
        if matches!(
            slot,
            Some(Value::Reference(_) | Value::Array(_) | Value::Object(_))
        ) {
            drop(vars);
            return self.cv_assign_slow(cv, v);
        }
        let old = slot.replace(v);
        drop(vars);
        discard_unrooted(old);
        None
    }

    /// [`Vm::cv_assign`] when the slot holds a reference (assigned
    /// through), an array or an object (a possible GC root once released).
    #[must_use]
    #[inline(never)]
    fn cv_assign_slow(&mut self, cv: u32, v: Value) -> Option<(PhpRef, Value)> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        let slot = &mut vars.slots[idx];
        let old = match slot {
            Some(Value::Reference(r)) if crate::typed_refs::has_type_sources(r) => {
                return Some((r.clone(), v));
            }
            Some(Value::Reference(r)) => Some(r.set(v)),
            _ => slot.replace(v),
        };
        drop(vars);
        if let Some(old) = &old {
            crate::gc::possible_root(old);
        }
        drop(old);
        None
    }

    /// `ZEND_ASSIGN` with a CV result operand (`$cv = val`, the
    /// [`Op::AssignCv`] instruction): `zend_assign_to_variable`. The
    /// common forms are done in place ([`Vm::assign_cv_fast`]); the rest
    /// (reads that warn, references on either side, old values that are
    /// possible GC roots) take the generic path.
    #[inline]
    pub(crate) fn op_assign_cv(&mut self, cv: u32, val: Operand, dst: Option<u32>) -> VmResult<()> {
        if self.assign_cv_fast(cv, val, dst) {
            return Ok(());
        }
        self.assign_cv_generic(cv, val, dst)
    }

    /// The in-place form of [`Vm::op_assign_cv`], php-src's
    /// `ZEND_ASSIGN_SPEC_CV_{CONST,TMP,CV}` fast case: the source is a
    /// literal, a temporary or a defined compiled variable whose value
    /// needs no dereferencing work with side effects, and the target slot
    /// is undefined or holds a value whose release needs no GC root (no
    /// reference to assign through, no array or object). One store into
    /// the slot, no intermediate `VmResult`, no drop call for a scalar old
    /// value. Returns `false`, having changed nothing, when the generic
    /// path must run.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the point is to avoid the call and the Value moves through memory
    fn assign_cv_fast(&mut self, cv: u32, val: Operand, dst: Option<u32>) -> bool {
        let Some(f) = self.frames.last_mut() else {
            return false;
        };
        let idx = f.cv_idx(cv);
        let Ok(mut vars) = f.vars.try_borrow_mut() else {
            return false;
        };
        match vars.slots.get(idx) {
            Some(Some(Value::Reference(_) | Value::Array(_) | Value::Object(_))) | None => {
                return false;
            }
            Some(_) => {}
        }
        let v = match val {
            // Consumed, as `read` does; a reference held by the temporary
            // is left to the generic path (it derefs and releases it).
            Operand::Tmp(t) => match f.tmps.get_mut(t as usize) {
                Some(s) if !matches!(s, Value::Reference(_)) => std::mem::take(s),
                _ => return false,
            },
            Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
                Some(s) if !matches!(s, Value::Reference(_)) => s.clone(),
                _ => return false,
            },
            Operand::Lit(i) => match f.code.literals.get(i as usize) {
                Some(l) if !matches!(l, Value::Reference(_)) => l.clone(),
                _ => return false,
            },
            // `ZVAL_COPY_DEREF` of a defined variable (an undefined one
            // warns: generic path).
            Operand::Cv(src) => match vars.slots.get(f.cv_idx(src)) {
                Some(Some(Value::Reference(r))) => r.get(),
                Some(Some(s)) => s.clone(),
                _ => return false,
            },
            Operand::Unused => return false,
        };
        let keep = dst.map(|_| v.clone());
        let old = std::mem::replace(&mut vars.slots[idx], Some(v));
        drop(vars);
        discard_unrooted(old);
        if let (Some(d), Some(k)) = (dst, keep) {
            self.set_tmp(d, k);
        }
        true
    }

    /// The generic path of [`Vm::op_assign_cv`].
    #[inline(never)]
    fn assign_cv_generic(&mut self, cv: u32, val: Operand, dst: Option<u32>) -> VmResult<()> {
        let v = self.read(val)?;
        let v = self.copy_on_assign(v);
        if let Some(d) = dst {
            self.set_tmp(d, v.clone());
        }
        if let Some((r, v)) = self.cv_assign(cv, v) {
            let v = self.assign_ref(&r, v)?;
            if let Some(d) = dst {
                self.set_tmp(d, v);
            }
        }
        Ok(())
    }

    /// Binds a compiled variable to a reference.
    ///
    /// Also the store of `RECV`: the slot is then undefined, and nothing
    /// is released.
    #[inline]
    pub(crate) fn cv_bind(&mut self, cv: u32, v: Value) {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let old = f.vars.borrow_mut().slots[idx].replace(v);
        release_slot_value(old);
    }

    /// Stores a temporary.
    #[inline]
    pub(crate) fn set_tmp(&mut self, t: u32, v: Value) {
        let old = std::mem::replace(&mut self.frame_mut().tmps[t as usize], v);
        // The slot almost always holds a scalar (or the `Null` a consuming
        // read left behind): skip the out-of-line `drop_in_place` call.
        if matches!(
            old,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
        ) {
            std::mem::forget(old);
        }
    }

    /// The boolean held by temporary `op`, consumed, if it holds one: the
    /// `IS_TRUE`/`IS_FALSE` fast path of php-src's `ZEND_JMPZ`/`ZEND_JMPNZ`
    /// handlers (comparison results), without the out-of-line read,
    /// conversion and drop of the generic path. A compiled variable holding
    /// a bool, null or int is tested in place (`if ($found)`), without the
    /// clone; anything else (undefined: warning, references, strings,
    /// arrays, objects) takes the generic path.
    #[inline]
    fn take_tmp_bool(&mut self, op: Operand) -> Option<bool> {
        let t = match op {
            Operand::Tmp(t) => t,
            Operand::Cv(cv) => {
                return self.frame().peek_cv(cv, |s| match s {
                    Some(Value::Bool(b)) => Some(*b),
                    Some(Value::Null) => Some(false),
                    Some(Value::Int(i)) => Some(*i != 0),
                    _ => None,
                });
            }
            _ => return None,
        };
        let slot = self.frame_mut().tmps.get_mut(t as usize)?;
        if let Value::Bool(b) = *slot {
            *slot = Value::Null;
            Some(b)
        } else {
            None
        }
    }

    /// Stores an optional result.
    #[inline]
    pub(crate) fn set_dst(&mut self, dst: Option<u32>, v: Value) {
        if let Some(t) = dst {
            self.set_tmp(t, v);
        }
    }

    /// Converts a runtime error into a throw (creating the exception).
    pub(crate) fn rt_throw(&mut self, e: php_runtime::RuntimeError) -> Throw {
        use php_runtime::RuntimeError as E;
        if let Some(t) = self.pending_throw.take() {
            return t;
        }
        match e {
            E::Thrown(Value::Object(o)) => Throw::Exception(o),
            E::Thrown(_) => Throw::Fatal,
            E::Fatal(m) => {
                let _ = self.fatal(&m);
                Throw::Fatal
            }
            E::Error(m) => self.make_throw("Error", &m),
            E::TypeError(m) => self.make_throw("TypeError", &m),
            E::ValueError(m) => self.make_throw("ValueError", &m),
            E::ArithmeticError(m) => self.make_throw("ArithmeticError", &m),
            E::DivisionByZeroError(m) => self.make_throw("DivisionByZeroError", &m),
            E::ArgumentCountError(m) => self.make_throw("ArgumentCountError", &m),
            E::Custom { class, message } => {
                let c = class.to_string_lossy();
                self.make_throw(&c, &message)
            }
        }
    }

    /// Runs the frames from `base` until the base frame returns.
    pub fn execute(&mut self, base: usize) -> VmResult<Value> {
        // `zend_call_stack_overflowed()`: nested executions (callbacks,
        // destructors, __clone, ...) use the native stack.
        if self.native_stack_exhausted() {
            self.observer_unwind(base, None);
            self.frames.truncate(base);
            return Err(crate::builtins::dump::stack_size_error(self));
        }
        // `zend_call_function`: a pending OOM is raised before user code
        // called from a builtin runs.
        if php_memlimit::is_pending() {
            self.observer_unwind(base, None);
            self.frames.truncate(base);
            self.raise_pending_oom()?;
            return Err(Throw::Fatal);
        }
        if self.depth >= MAX_NESTED_EXECUTIONS {
            self.observer_unwind(base, None);
            // A backstop for native stacks whose size we were not told.
            self.frames.truncate(base);
            return Err(crate::builtins::dump::stack_size_error(self));
        }
        self.depth += 1;
        let guard = crate::interrupt::ExecutionGuard::enter();
        let r = self.execute_inner(base);
        drop(guard);
        self.depth -= 1;
        r
    }

    fn execute_inner(&mut self, base: usize) -> VmResult<Value> {
        loop {
            match self.run(base) {
                Ok(v) => return Ok(v),
                Err(Throw::Exception(e)) => {
                    if let Err(t) = self.handle_exception(e, base) {
                        return Err(t);
                    }
                }
                Err(t) => {
                    self.observer_unwind(base, Some(&t));
                    self.frames.truncate(base);
                    return Err(t);
                }
            }
        }
    }

    /// Finds a handler for `exc` in the frames above `base`. Returns
    /// `Ok(())` when execution can continue, or the throw that escapes.
    pub(crate) fn handle_exception(
        &mut self,
        mut exc: php_runtime::Obj,
        base: usize,
    ) -> VmResult<()> {
        loop {
            if self.frames.len() <= base {
                // Thrown after the frames of this execution were left
                // (e.g. by a destructor run on the way out).
                return Err(Throw::Exception(exc));
            }
            let fidx = self.frames.len() - 1;
            let f = self.frames.last_mut().expect("frame");
            if !f.silence_saved.is_empty() {
                // `cleanup_live_vars` for ZEND_LIVE_SILENCE ranges
                // (Zend/zend_execute.c): each pending `@` restores its
                // saved mask only while the current one is still silenced.
                let saved = std::mem::take(&mut f.silence_saved);
                for old in saved.into_iter().rev() {
                    self.error_reporting = silence_restore(self.error_reporting, old);
                }
            }
            if !self.frame().code.live_tmps.is_empty() {
                exc = match self.release_live_tmps(exc) {
                    Ok(e) => e,
                    Err(t) => return Err(self.abandon_unwinding(t, base)),
                };
            }
            let f = self.frames.last_mut().expect("frame");
            let throw_ip = f.ip.saturating_sub(1) as u32;
            let code = f.code.clone();
            for entry in &code.try_table {
                let in_try = throw_ip >= entry.try_start && throw_ip < entry.try_end;
                if in_try {
                    if let Some(cs) = entry.catch_start {
                        exc = match self.free_left_iterators(&code, throw_ip, cs, exc) {
                            Ok(e) => e,
                            Err(t) => return Err(self.abandon_unwinding(t, base)),
                        };
                        let f = self.frame_mut();
                        f.ip = cs as usize;
                        ctor_failed_unfinished(&f.calls);
                        f.calls.clear();
                        // `cleanup_live_vars`: the try body's temporaries
                        // (e.g. the left operand of `new A + new B()`) are
                        // released, and destructed, before the catch runs.
                        let (ts, te) = entry.try_tmps;
                        let dead: Vec<Value> = f
                            .tmps
                            .get_mut(ts as usize..te as usize)
                            .map(|t| t.iter_mut().map(std::mem::take).collect())
                            .unwrap_or_default();
                        drop(dead);
                        let exc = match self.destruct_while_unwinding(exc) {
                            Ok(e) => e,
                            Err(t) => return Err(self.abandon_unwinding(t, base)),
                        };
                        self.frame_mut().pending_exc = Some(exc);
                        return Ok(());
                    }
                }
                if let Some((fin, fs, fe)) = entry.finally {
                    if throw_ip >= entry.try_start && throw_ip < entry.catch_end {
                        exc = match self.free_left_iterators(&code, throw_ip, fs, exc) {
                            Ok(e) => e,
                            Err(t) => return Err(self.abandon_unwinding(t, base)),
                        };
                        let f = self.frame_mut();
                        f.fins[fin as usize] = FinState::Exception(exc);
                        f.ip = fs as usize;
                        ctor_failed_unfinished(&f.calls);
                        f.calls.clear();
                        return Ok(());
                    }
                    if throw_ip >= fs && throw_ip < fe {
                        let f = self.frame_mut();
                        let prev = std::mem::take(&mut f.fins[fin as usize]);
                        if let FinState::Exception(p) = prev {
                            if crate::fiber::is_graceful_exit(&exc)
                                && crate::generator::is_close_marker(&exc)
                            {
                                // A force-closed generator rethrows it.
                                exc = p;
                            } else {
                                crate::exceptions::chain_previous(&exc, &p);
                            }
                        }
                    }
                }
            }
            // Unwind this frame (observers see its end first). Then
            // `cleanup_unfinished_calls` + `cleanup_live_vars`: its
            // unfinished calls and live temporaries (foreach iterators,
            // intermediate results) are released while it is still the
            // current frame.
            if self.observer.enabled {
                self.observer_end_frame(fidx, None, Some(&Throw::Exception(exc.clone())));
            }
            self.release_frame_temporaries();
            exc = match self.destruct_while_unwinding(exc) {
                Ok(e) => e,
                Err(t) => return Err(self.abandon_unwinding(t, base)),
            };
            let popped = self.frames.pop();
            if let Some(p) = &popped {
                // `i_free_compiled_variables`: released values may be
                // cycle roots (and may fill the root buffer).
                crate::gc::frame_released(p, self.frames.last().and_then(|c| c.this.as_ref()));
                if let Some(g) = &p.generator {
                    crate::generator::mark_finished(g);
                }
                // `zend_object_store_ctor_failed`: an object whose
                // constructor threw is never destructed.
                if p.is_ctor {
                    if let Some(t) = &p.this {
                        let d = object::data(t);
                        d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
                    }
                }
            }
            // `zend_leave_helper`: the compiled variables (and `$this`) are
            // released with the caller current.
            drop(popped);
            exc = match self.destruct_while_unwinding(exc) {
                Ok(e) => e,
                Err(t) => return Err(self.abandon_unwinding(t, base)),
            };
            if fidx <= base {
                return Err(Throw::Exception(exc));
            }
        }
    }

    /// `cleanup_live_vars` (`Zend/zend_execute.c)`: releases the temporaries
    /// live at the throwing op, in order, before a handler of this frame
    /// runs; their destructors see `exc` in flight.
    #[cold]
    fn release_live_tmps(&mut self, exc: php_runtime::Obj) -> Result<php_runtime::Obj, Throw> {
        let f = self.frame_mut();
        #[allow(clippy::cast_possible_truncation)]
        let throw_ip = f.ip.saturating_sub(1) as u32;
        let code = f.code.clone();
        let mut dead = Vec::new();
        for l in &code.live_tmps {
            if (l.start..l.end).contains(&throw_ip) {
                if let Some(v) = f.tmps.get_mut(l.tmp as usize) {
                    dead.push(std::mem::take(v));
                }
            }
        }
        if dead.is_empty() {
            return Ok(exc);
        }
        drop(dead);
        self.destruct_while_unwinding(exc)
    }

    /// `cleanup_live_vars(op_num, catch_op_num)` for foreach iterators: an
    /// exception handled in the frame it was thrown in first releases the
    /// iterators of the loops it leaves (outermost first, the order of
    /// the live-range table). Their destructors, and those of the operands
    /// the throwing op freed, run with `exc` in flight before the handler
    /// (`zend_objects_destroy_object`), so their exceptions chain to it.
    #[cold]
    fn free_left_iterators(
        &mut self,
        code: &crate::opcode::OpArray,
        throw_ip: u32,
        target: u32,
        exc: php_runtime::Obj,
    ) -> Result<php_runtime::Obj, Throw> {
        if self.frame().iters.iter().any(Option::is_some) {
            for (at, op) in code.ops.iter().enumerate() {
                if let Op::FeReset { iter, end, .. } = op {
                    // Live from after FE_RESET up to (excluding) the FE_FREE.
                    let start = u32::try_from(at).unwrap_or(u32::MAX);
                    let live = |ip: u32| start < ip && ip < *end;
                    if live(throw_ip) && !live(target) {
                        self.op_fe_free(*iter);
                    }
                }
            }
        }
        self.destruct_while_unwinding(exc)
    }

    /// Releases the temporaries, foreach iterators and unfinished calls of
    /// the current frame (which an exception is about to leave).
    fn release_frame_temporaries(&mut self) {
        // `cleanup_unfinished_calls` first, innermost call first, each
        // releasing its arguments before `$this`; then `cleanup_live_vars`.
        // (The order decides which object handles are reused first.)
        let mut calls = std::mem::take(&mut self.frame_mut().calls);
        // An abandoned `new` is not destructed (`zend_object_store_ctor_failed`).
        ctor_failed_unfinished(&calls);
        while let Some(mut call) = calls.pop() {
            call.args.clear();
            call.named.clear();
            drop(call);
        }
        let n = u32::try_from(self.frame().iters.len()).unwrap_or(u32::MAX);
        for i in 0..n {
            self.op_fe_free(i);
        }
        let f = self.frame_mut();
        let tmps: Vec<Value> = f.tmps.iter_mut().map(std::mem::take).collect();
        drop(tmps);
    }

    /// Runs the destructors of objects released while `exc` unwinds the
    /// stack (`zend_objects_destroy_object` with an exception in flight):
    /// an exception a destructor throws gets `exc` as its previous one and
    /// continues the unwinding in its place.
    fn destruct_while_unwinding(
        &mut self,
        exc: php_runtime::Obj,
    ) -> Result<php_runtime::Obj, Throw> {
        if !object::has_pending_destructors() {
            if crate::gc::wants_run() {
                return self.collect_while_unwinding(exc);
            }
            return Ok(exc);
        }
        let exc = match self.run_pending_destructors() {
            Ok(()) => exc,
            Err(Throw::Exception(new)) if !self.is_executing() => {
                self.destructor_threw_without_frame(new)?;
                exc
            }
            Err(Throw::Exception(new)) => {
                crate::exceptions::chain_previous(&new, &exc);
                new
            }
            Err(t) => return Err(t),
        };
        if crate::gc::wants_run() {
            return self.collect_while_unwinding(exc);
        }
        Ok(exc)
    }

    /// The values released while `exc` unwinds filled the root buffer
    /// (`gc_possible_root_when_full` inside `i_free_compiled_variables`):
    /// the collection runs now, before a handler is found. An exception
    /// thrown by a garbage destructor gets `exc` as its previous one and
    /// continues the unwinding in its place (`zend_objects_destroy_object`,
    /// `remember_prev_exception` in a fiber).
    #[cold]
    #[inline(never)]
    fn collect_while_unwinding(
        &mut self,
        exc: php_runtime::Obj,
    ) -> Result<php_runtime::Obj, Throw> {
        match self.gc_safe_point() {
            Ok(()) => Ok(exc),
            Err(Throw::Exception(new)) => {
                crate::exceptions::chain_previous(&new, &exc);
                Ok(new)
            }
            Err(t) => Err(t),
        }
    }

    /// A destructor run while an exception leaves the outermost frame
    /// (`zend_leave_helper` makes the caller, here none, current before
    /// freeing the compiled variables) threw: `zend_call_function` with no
    /// `EG(current_execute_data)` reports it at once
    /// (`zend_throw_exception_internal`: the user exception handler, else
    /// the uncaught fatal and a bailout), without chaining it to the
    /// exception in flight. `Ok` (handled by the user handler) keeps that
    /// one unwinding (`zend_objects_destroy_object` restores it).
    #[cold]
    #[inline(never)]
    fn destructor_threw_without_frame(&mut self, new: php_runtime::Obj) -> VmResult<()> {
        if let Err(t) = self.uncaught_exception(new) {
            object::set_destructors_enabled(false);
            return Err(t);
        }
        Ok(())
    }

    /// A destructor run during unwinding exited or bailed out: the frames
    /// of this execution are abandoned.
    fn abandon_unwinding(&mut self, t: Throw, base: usize) -> Throw {
        self.observer_unwind(base, Some(&t));
        self.frames.truncate(base);
        t
    }

    /// The interpreter loop proper.
    #[allow(clippy::too_many_lines)]
    fn run(&mut self, base: usize) -> VmResult<Value> {
        'frames: loop {
            let code = self.frame().code.clone();
            loop {
                let ip = {
                    let f = self.frame_mut();
                    let ip = f.ip;
                    f.ip += 1;
                    ip
                };
                let flow = match &code.ops[ip] {
                    Op::Nop => Flow::Next,
                    Op::Move { src, dst } => {
                        let v = self.read(*src)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::Bin { op, a, b, dst } => {
                        // Sealed numeric fast path (php-src's specialized
                        // long/double handlers): operands are CVs or
                        // literals currently holding plain numbers.
                        // Anything else - references, undefined variables,
                        // strings, overflow, division - takes the
                        // out-of-line generic path with its exact
                        // semantics.
                        if let Some(r) = self.bin_num_fast(*op, *a, *b) {
                            self.set_tmp(*dst, r);
                        } else {
                            self.bin_slow(*op, *a, *b, *dst)?;
                        }
                        Flow::Next
                    }
                    Op::Unary { op, a, dst } => {
                        let x = self.read(*a)?;
                        let r =
                            match op {
                                UnKind::Not => Value::Bool(!self.to_bool(&x)?),
                                UnKind::Bool => Value::Bool(self.to_bool(&x)?),
                                UnKind::BitNot => {
                                    ops::bit_not(&x, self).map_err(|e| self.rt_throw(e))?
                                }
                                UnKind::Neg => ops::mul(&x, &Value::Int(-1), self)
                                    .map_err(|e| self.rt_throw(e))?,
                                UnKind::Plus => ops::mul(&x, &Value::Int(1), self)
                                    .map_err(|e| self.rt_throw(e))?,
                            };
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Cast { kind, a, dst } => {
                        let x = if matches!(*kind, CastKind::Array | CastKind::Object) {
                            self.read_raw(*a)?
                        } else {
                            self.read(*a)?
                        };
                        let r = self.cast(*kind, *a, x)?;
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Rope { parts, dst } => {
                        let s = self.op_rope(parts)?;
                        self.set_tmp(*dst, Value::Str(s));
                        Flow::Next
                    }
                    Op::InitArray { dst } => {
                        self.set_tmp(*dst, Value::Array(ZArray::new()));
                        Flow::Next
                    }
                    Op::ArrayAdd { arr, key, val } => {
                        let v = self.read(*val)?;
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        self.array_literal_add(*arr, k, v)?;
                        Flow::Next
                    }
                    Op::ArrayAddRef { arr, key, place } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let r = self.place_make_ref(*place)?;
                        self.array_literal_add(*arr, k, Value::Reference(r))?;
                        Flow::Next
                    }
                    Op::ArrayUnpack { arr, src } => {
                        let v = self.read(*src)?;
                        self.array_unpack(*arr, v)?;
                        Flow::Next
                    }
                    Op::ConstArrayUnpack { arr, src } => {
                        let v = self.read(*src)?;
                        self.const_array_unpack(*arr, v)?;
                        Flow::Next
                    }
                    Op::Jmp(t) => {
                        self.frame_mut().ip = *t as usize;
                        Flow::Next
                    }
                    Op::JmpZ(a, t) => {
                        let b = match self.take_tmp_bool(*a) {
                            Some(b) => b,
                            None => {
                                let v = self.read(*a)?;
                                self.to_bool(&v)?
                            }
                        };
                        if !b {
                            self.frame_mut().ip = *t as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNz(a, t) => {
                        let b = match self.take_tmp_bool(*a) {
                            Some(b) => b,
                            None => {
                                let v = self.read(*a)?;
                                self.to_bool(&v)?
                            }
                        };
                        if b {
                            self.frame_mut().ip = *t as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpZEx { a, dst, target } => {
                        let v = self.read(*a)?;
                        let b = self.to_bool(&v)?;
                        if !b {
                            self.set_tmp(*dst, Value::Bool(false));
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNzEx { a, dst, target } => {
                        let v = self.read(*a)?;
                        let b = self.to_bool(&v)?;
                        if b {
                            self.set_tmp(*dst, Value::Bool(true));
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpSet { a, dst, target } => {
                        let v = self.read(*a)?;
                        if self.to_bool(&v)? {
                            self.set_tmp(*dst, v);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::Coalesce { a, dst, target } => {
                        let v = match a {
                            Operand::Cv(cv) => self.read_cv_defined(*cv).unwrap_or_default(),
                            o => self.read(*o)?,
                        };
                        if !v.is_null() {
                            self.set_tmp(*dst, v);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNull { a, dst, target } => {
                        let v = match a {
                            Operand::Cv(cv) => match self.read_cv_defined(*cv) {
                                Some(v) => v,
                                None => self.read(*a)?,
                            },
                            o => self.read(*o)?,
                        };
                        if v.is_null() {
                            if let Operand::TmpKeep(t) = a {
                                self.set_tmp(*t, Value::Null);
                            }
                            self.set_tmp(*dst, Value::Null);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::AssignCv { cv, val, dst } => {
                        self.op_assign_cv(*cv, *val, *dst)?;
                        Flow::Next
                    }
                    Op::Assign { place, val, dst } => {
                        let r = match *val {
                            Operand::Cv(cv) => match self.read_cv_op_data(cv) {
                                CvOpData::Value(v) => self.place_assign(*place, v)?,
                                CvOpData::Deferred => self.place_assign_cv_ref(*place, cv)?,
                                CvOpData::Undefined => self.place_assign_undef_cv(*place, cv)?,
                            },
                            other => {
                                let v = self.read(other)?;
                                let v = self.copy_on_assign(v);
                                self.place_assign(*place, v)?
                            }
                        };
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::AssignOp {
                        place,
                        op,
                        val,
                        dst,
                    } => {
                        let concat = *op == BinaryOp::Concat;
                        if !(concat && self.concat_assign_cv_lit(*place, *val, *dst)) {
                            let v = self.read(*val)?;
                            if !(concat && self.concat_assign_cv(*place, &v, *dst)) {
                                let r = self.place_assign_op(*place, *op, v)?;
                                self.set_dst(*dst, r);
                            }
                        }
                        Flow::Next
                    }
                    Op::IncDec { place, kind, dst } => {
                        let r = self.place_incdec(*place, *kind)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::AssignRef { place, src, dst } => {
                        let v = self.read_raw(*src)?;
                        let r = self.place_assign_ref(*place, v)?;
                        if dst.is_some() {
                            self.set_dst(*dst, r);
                        } else {
                            // An unused result is a duplicate handle of the
                            // reference just bound: not a GC possible root.
                            crate::gc::release_duplicate(r);
                        }
                        Flow::Next
                    }
                    Op::MakeRef { place, dst } => {
                        let r = self.place_make_ref(*place)?;
                        self.set_tmp(*dst, Value::Reference(r));
                        Flow::Next
                    }
                    Op::ListFetchRef { place, dst } => {
                        self.list_fetch_ref(*place, *dst)?;
                        Flow::Next
                    }
                    Op::Fetch { place, mode, dst } => {
                        let v = self.place_fetch(*place, *mode)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::Isset { place, dst } => {
                        let v = self.place_isset(*place, false)?;
                        self.set_tmp(*dst, Value::Bool(v));
                        Flow::Next
                    }
                    Op::Empty { place, dst } => {
                        let v = self.place_isset(*place, true)?;
                        self.set_tmp(*dst, Value::Bool(v));
                        Flow::Next
                    }
                    Op::ArrayKeyExists { array, dst } => {
                        if self.op_array_key_exists(array, *dst)? {
                            Flow::Reload
                        } else {
                            Flow::Next
                        }
                    }
                    Op::Unset { place } => {
                        self.place_unset(*place)?;
                        Flow::Next
                    }
                    Op::Global { cv, name } => {
                        let n = self.lit_str(*name);
                        let r = self.global_ref(n.as_bytes());
                        self.cv_bind(*cv, Value::Reference(r));
                        Flow::Next
                    }
                    Op::BindStatic { cv, idx, skip } => {
                        if let Some(r) = self.static_slot(*idx) {
                            self.cv_bind(*cv, Value::Reference(r));
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::InitStatic { cv, idx, val } => {
                        let v = self.read(*val)?;
                        let r = self.static_init(*idx, v);
                        self.cv_bind(*cv, Value::Reference(r));
                        Flow::Next
                    }
                    Op::FetchThis { dst } => {
                        let t = self.frame().this.clone();
                        match t {
                            Some(o) => self.set_tmp(*dst, Value::Object(o)),
                            None => {
                                return Err(self
                                    .make_throw("Error", "Using $this when not in object context"))
                            }
                        }
                        Flow::Next
                    }
                    Op::InitFCall {
                        name,
                        fallback,
                        display,
                    } => {
                        self.op_init_fcall(*name, *fallback, *display)?;
                        Flow::Next
                    }
                    Op::InitDynCall { callee } => {
                        let c = self.read(*callee)?;
                        self.op_init_dyn_call(c)?;
                        Flow::Next
                    }
                    Op::InitMethodCall { obj, name } => {
                        let o = self.read(*obj)?;
                        let n = self.read(*name)?;
                        self.op_init_method_call(o, n)?;
                        Flow::Next
                    }
                    Op::InitStaticCall { class, name } => {
                        self.op_init_static_call(class, *name)?;
                        Flow::Next
                    }
                    Op::InitParentHook { prop, set } => {
                        let p = self.lit_str(*prop);
                        self.op_init_parent_hook(p, *set)?;
                        Flow::Next
                    }
                    Op::New { class, dst, skip } => {
                        let has_ctor = self.op_new(class, *dst)?;
                        if !has_ctor {
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::Send { src } => {
                        self.op_send(src, None)?;
                        Flow::Next
                    }
                    Op::SendNamed { name, src } => {
                        let n = self.lit_str(*name);
                        self.op_send(src, Some(n))?;
                        Flow::Next
                    }
                    Op::SendUnpack { src } => {
                        self.op_send_unpack(*src)?;
                        Flow::Next
                    }
                    Op::DoCall { dst } => {
                        if self.op_do_call(*dst)? {
                            Flow::Reload
                        } else {
                            Flow::Next
                        }
                    }
                    Op::SendPlaceholder { name } => {
                        let n = name.map(|n| self.lit_str(n));
                        self.op_send_placeholder(n)?;
                        Flow::Next
                    }
                    Op::CallablePartial {
                        dst,
                        variadic,
                        const_args,
                        name,
                    } => {
                        let call = self.frame_mut().calls.pop().expect("pending call");
                        let n = self.lit_str(*name);
                        let c = self.make_partial(call, *variadic, *const_args, n)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::CallableConvert { dst, const_expr } => {
                        let call = self.frame_mut().calls.pop().expect("pending call");
                        if *const_expr && call.magic_name.is_some() {
                            return Err(self.make_throw(
                                "Error",
                                "Creating a callable for the magic __callStatic() method is not supported in constant expressions",
                            ));
                        }
                        let c = self.make_closure_from_call(call)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::Recv { n, cv } => {
                        self.op_recv(*n, *cv)?;
                        Flow::Next
                    }
                    Op::RecvInit { n, cv, skip } => {
                        if self.op_recv_init(*n, *cv)? {
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::RecvDefault { n, cv, val } => {
                        let v = self.read(*val)?;
                        self.op_recv_default(*n, *cv, v)?;
                        Flow::Next
                    }
                    Op::RecvVariadic { n, cv } => {
                        self.op_recv_variadic(*n, *cv)?;
                        Flow::Next
                    }
                    Op::VerifyReturn { val } => {
                        if let Operand::Tmp(t) = *val {
                            let v = self.read(*val)?;
                            let v = self.verify_return(Some(v))?;
                            self.set_tmp(t, v);
                        }
                        Flow::Next
                    }
                    Op::Return { val } => {
                        let v = match val {
                            Operand::Unused => None,
                            o => Some(self.read(*o)?),
                        };
                        self.op_return(v, base)?
                    }
                    Op::ReturnByRefValue { val } => {
                        let unused = *val == Operand::Unused;
                        let raw = if unused {
                            Value::Null
                        } else {
                            self.read_raw(*val)?
                        };
                        let v = match raw {
                            Value::Reference(r) => self.verify_return(Some(Value::Reference(r)))?,
                            other => {
                                let checked = self.verify_return((!unused).then_some(other))?;
                                self.notice(
                                    "Only variable references should be returned by reference",
                                )?;
                                Value::Reference(php_runtime::PhpRef::new(checked))
                            }
                        };
                        self.op_return_raw(Some(v), base)?
                    }
                    Op::ReturnRef { place } => {
                        let by_ref = self
                            .frame()
                            .ufunc
                            .as_ref()
                            .is_some_and(|u| u.flags & crate::func::flags::RETURN_REF != 0);
                        let v = if by_ref {
                            Value::Reference(self.place_make_ref(*place)?)
                        } else {
                            self.place_fetch(*place, crate::opcode::FetchMode::Read)?
                        };
                        self.op_return(Some(v), base)?
                    }
                    Op::GeneratorCreate => self.op_generator_create(base)?,
                    Op::Yield { key, val, dst } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let v = match val {
                            Some(v) => Some(self.read_raw(*v)?),
                            None => None,
                        };
                        self.op_yield(k, v, *dst, base)?
                    }
                    Op::YieldRef { key, place, dst } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let r = self.place_make_ref(*place)?;
                        self.op_yield(k, Some(Value::Reference(r)), *dst, base)?
                    }
                    Op::YieldByRefValue {
                        key,
                        val,
                        call,
                        dst,
                    } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let v = self.read_raw(*val)?;
                        self.op_yield_by_ref_value(k, v, *call, *dst, base)?
                    }
                    Op::YieldFrom { src, dst } => {
                        let v = self.read_raw(*src)?;
                        self.op_yield_from(v, *dst, base)?
                    }
                    Op::DeclareFunction(idx) => {
                        let f = code.decls.functions.borrow()[*idx as usize].clone();
                        self.declare_function(&f)?;
                        Flow::Next
                    }
                    Op::DeclareClass(idx) => {
                        let d = code.decls.classes.borrow()[*idx as usize].clone();
                        self.declare_class_stmt(&d)?;
                        Flow::Next
                    }
                    Op::DeclareAnonClass { idx, dst } => {
                        let d = code.decls.classes.borrow()[*idx as usize].clone();
                        let c = self.declare_class(&d, true)?;
                        self.set_tmp(*dst, Value::Str(c.name.clone()));
                        Flow::Next
                    }
                    Op::MakeClosure { idx, dst } => {
                        let f = code.decls.functions.borrow()[*idx as usize].clone();
                        let c = self.make_closure(&f)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::DeclareConst { name, val, attrs } => {
                        let n = self.lit_str(*name);
                        let v = self.read(*val)?;
                        if self.declare_constant(n.clone(), v)? && *attrs != u32::MAX {
                            if let Some(a) =
                                code.decls.const_attributes.borrow().get(*attrs as usize)
                            {
                                self.const_attributes.insert(n, a.clone());
                            }
                        }
                        Flow::Next
                    }
                    Op::FetchConst {
                        name,
                        fallback,
                        dst,
                    } => {
                        let v = self.fetch_constant_cached(*name, *fallback)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::FetchClassConst { class, name, dst } => {
                        let v = self.fetch_class_const_cached(class, *name)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::FetchClassName {
                        class,
                        dst,
                        const_expr,
                    } => {
                        let v = self.op_fetch_class_name(class, *const_expr)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::InstanceOf { a, class, dst } => {
                        let v = self.read(*a)?;
                        let r = self.op_instanceof(&v, class)?;
                        self.set_tmp(*dst, Value::Bool(r));
                        Flow::Next
                    }
                    Op::Clone { a, dst } => {
                        let v = self.read(*a)?;
                        let r = self.clone_value(v)?;
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::FuncNumArgs { dst } => {
                        let n = self.frame().args.len();
                        self.set_dst(*dst, Value::Int(i64::try_from(n).unwrap_or(i64::MAX)));
                        Flow::Next
                    }
                    Op::FuncGetArgs { dst } => {
                        let idx = self.frames.len() - 1;
                        let args = self.frame_args(idx);
                        self.set_dst(*dst, Value::Array(args));
                        Flow::Next
                    }
                    Op::Strlen { a, dst } => {
                        let v = self.read(*a)?;
                        let len = match &v {
                            Value::Str(s) => Some(s.len()),
                            _ => None,
                        };
                        let r = match len {
                            Some(n) => Value::Int(i64::try_from(n).unwrap_or(i64::MAX)),
                            None => self.op_strlen_slow(v)?,
                        };
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::Count { a, dst, sizeof } => {
                        let v = self.read(*a)?;
                        let len = match &v {
                            Value::Array(arr) => Some(arr.len()),
                            _ => None,
                        };
                        let r = match len {
                            Some(n) => Value::Int(i64::try_from(n).unwrap_or(i64::MAX)),
                            None => self.op_count_slow(v, *sizeof)?,
                        };
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::TypeCheck { a, mask, dst } => {
                        let v = self.read(*a)?;
                        let r = crate::opcode::type_mask::matches(*mask, &v);
                        self.set_dst(*dst, Value::Bool(r));
                        Flow::Next
                    }
                    Op::GetType { a, dst } => {
                        let v = self.read(*a)?;
                        let t = v.legacy_type_name();
                        self.set_dst(*dst, Value::Str(ZStr::from(t)));
                        Flow::Next
                    }
                    Op::GetClass { a, dst } => {
                        let r = self.op_get_class(*a)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::GetCalledClass { dst } => {
                        let r = self.op_get_called_class()?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::Echo(a) => {
                        let v = self.read(*a)?;
                        match &v {
                            Value::Str(s) => self.echo(s.as_bytes()),
                            // `zend_long_to_str` digits from a stack buffer:
                            // no string is materialised for an echoed int.
                            Value::Int(i) => {
                                let mut buf = [0u8; php_runtime::string::LONG_MAX_LEN];
                                self.echo(php_runtime::string::format_long(*i, &mut buf));
                            }
                            _ => {
                                let s = self.to_zstr(&v)?;
                                self.echo(s.as_bytes());
                            }
                        }
                        self.take_pending_throw()?;
                        Flow::Next
                    }
                    Op::Exit(a) => {
                        let v = self.read(*a)?;
                        self.op_exit(v)?;
                        return Err(Throw::Exit);
                    }
                    Op::Free(a) => {
                        let v = self.read_raw(*a)?;
                        drop(v);
                        Flow::Next
                    }
                    Op::Throw(a) => {
                        let v = self.read(*a)?;
                        return Err(self.op_throw(v));
                    }
                    Op::Catch { classes, cv, next } => {
                        let exc = self.frame().pending_exc.clone();
                        let Some(exc) = exc else {
                            self.frame_mut().ip = *next as usize;
                            continue;
                        };
                        let cls = object::class_of(&exc).clone();
                        let mut matched = false;
                        for c in classes.iter() {
                            if self.catch_class_matches(&cls, *c) {
                                matched = true;
                                break;
                            }
                        }
                        if matched {
                            self.frame_mut().pending_exc = None;
                            if let Some(cv) = cv {
                                if let Some((r, v)) = self.cv_assign(*cv, Value::Object(exc)) {
                                    self.assign_ref_strict(&r, v)?;
                                }
                            }
                        } else {
                            self.frame_mut().ip = *next as usize;
                        }
                        Flow::Next
                    }
                    Op::Rethrow => {
                        let exc = self.frame_mut().pending_exc.take();
                        match exc {
                            Some(e) => return Err(Throw::Exception(e)),
                            None => Flow::Next,
                        }
                    }
                    Op::FastCall { fin, target } => {
                        let f = self.frame_mut();
                        f.fins[*fin as usize] = FinState::Return(f.ip);
                        f.ip = *target as usize;
                        Flow::Next
                    }
                    Op::FastRet { fin } => {
                        let st = std::mem::take(&mut self.frame_mut().fins[*fin as usize]);
                        match st {
                            FinState::Return(ip) => {
                                self.frame_mut().ip = ip;
                                Flow::Next
                            }
                            FinState::Exception(e) => return Err(Throw::Exception(e)),
                            FinState::None => Flow::Next,
                        }
                    }
                    Op::DiscardFinally { fin } => {
                        self.frame_mut().fins[*fin as usize] = FinState::None;
                        Flow::Next
                    }
                    Op::BeginSilence { dst } => {
                        let old = self.error_reporting;
                        self.frame_mut().silence_saved.push(old);
                        self.set_tmp(*dst, Value::Int(old));
                        // ZEND_BEGIN_SILENCE: fatal errors are never silenced,
                        // and an already-silenced mask is left alone.
                        if !has_only_fatal_errors(old) {
                            self.error_reporting = old & E_FATAL_ERRORS;
                        }
                        Flow::Next
                    }
                    Op::EndSilence { saved } => {
                        let v = self.read(*saved)?;
                        self.frame_mut().silence_saved.pop();
                        if let Value::Int(old) = v {
                            self.error_reporting = silence_restore(self.error_reporting, old);
                        }
                        Flow::Next
                    }
                    Op::Include { kind, a, dst } => {
                        let v = self.read(*a)?;
                        if self.op_include(*kind, v, *dst)? {
                            Flow::Reload
                        } else {
                            Flow::Next
                        }
                    }
                    Op::FeReset {
                        subject,
                        place,
                        kind,
                        iter,
                        end,
                    } => {
                        let has = self.op_fe_reset(*subject, *place, *kind, *iter)?;
                        if !has {
                            self.frame_mut().ip = *end as usize;
                        }
                        Flow::Next
                    }
                    Op::FeFetch {
                        iter,
                        end,
                        val,
                        key,
                    } => {
                        let has = self.op_fe_fetch(*iter, *val, *key)?;
                        if !has {
                            self.frame_mut().ip = *end as usize;
                        }
                        Flow::Next
                    }
                    Op::FeFree { iter } => {
                        self.op_fe_free_and_destruct(*iter)?;
                        Flow::Next
                    }
                    Op::FeFreeOnReturn { iter, loop_at } => {
                        if let Err(e) = self.op_fe_free_and_destruct(*iter) {
                            if *loop_at != u32::MAX {
                                self.frame_mut().ip = *loop_at as usize + 1;
                            }
                            return Err(e);
                        }
                        if object::has_pending_destructors() {
                            if let Err(e) = self.run_pending_destructors() {
                                if *loop_at != u32::MAX {
                                    self.frame_mut().ip = *loop_at as usize + 1;
                                }
                                return Err(e);
                            }
                        }
                        Flow::Next
                    }
                    Op::MatchError { subject } => {
                        let v = self.read(*subject)?;
                        return Err(self.match_error(&v));
                    }
                    Op::TypeAssertArray { val, msg } => {
                        self.op_type_assert_array(*val, *msg)?;
                        Flow::Next
                    }
                    Op::Ticks(n) => {
                        self.op_ticks(*n)?;
                        Flow::Next
                    }
                    Op::Diag { level, msg } => {
                        let m = self.lit_str(*msg).to_string_lossy();
                        self.error(i64::from(*level), &m)?;
                        Flow::Next
                    }
                    Op::ConstDimCheck { obj } => {
                        if matches!(self.read(*obj)?, Value::Object(_)) {
                            return Err(self.const_dim_on_object());
                        }
                        Flow::Next
                    }
                    Op::UserOpcode { opcode } => {
                        self.run_user_opcode_handler(*opcode);
                        Flow::Next
                    }
                    Op::ConstPropCheck { obj, name, dst } => {
                        let n = self.read(*name)?;
                        let n = self.to_zstr(&n)?;
                        self.set_tmp(*dst, Value::Str(n));
                        if let Value::Object(o) = self.read(*obj)? {
                            if !object::class_of(&o).is_enum() {
                                return Err(self.non_enum_const_prop());
                            }
                        }
                        Flow::Next
                    }
                    Op::AssertCheck { dst, skip } => {
                        let a = self.ini.get_long("zend.assertions");
                        if a != 1 {
                            self.set_tmp(*dst, Value::Bool(true));
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                };
                // Safe point (`EG(vm_interrupt)`): one flag says whether
                // destructors, a collection or interrupt handlers wait; the
                // delayed memory_limit violation is still polled on its own
                // (php-memlimit cannot raise the flag).
                if crate::interrupt::safe_point_due() || php_memlimit::is_pending() {
                    self.safe_point(&code.ops[ip])?;
                }
                match flow {
                    Flow::Next => {}
                    Flow::Reload => continue 'frames,
                    Flow::Done(v) => return Ok(v),
                }
            }
        }
    }

    /// The work waiting for the end of an instruction, in this order:
    /// queued destructors, a due collection, a delayed `memory_limit`
    /// violation, interrupt handlers. Each source is checked on its own (the
    /// flag that led here may be stale); afterwards the flag is cleared
    /// unless a source is still set, also when the work threw.
    #[cold]
    #[inline(never)]
    fn safe_point(&mut self, op: &Op) -> VmResult<()> {
        let r = self.safe_point_work(op);
        crate::interrupt::resync_safe_point();
        r
    }

    fn safe_point_work(&mut self, op: &Op) -> VmResult<()> {
        if object::has_pending_destructors() {
            self.run_pending_destructors()?;
        }
        // Root buffer reached the threshold (`gc_possible_root_when_full`).
        if crate::gc::wants_run() {
            self.gc_safe_point()?;
        }
        // Delayed memory_limit violation (`zend_mm_delayed_error`).
        if php_memlimit::is_pending() {
            self.poll_pending_oom(op)?;
        }
        self.check_interrupt()
    }

    /// Applies a binary operator.
    pub(crate) fn binary(&mut self, op: BinKind, a: &Value, b: &Value) -> VmResult<Value> {
        let r = match op {
            BinKind::Arith(o) => match (o, a, b) {
                (BinaryOp::Add, Value::Int(x), Value::Int(y)) => match x.checked_add(*y) {
                    Some(r) => Ok(Value::Int(r)),
                    None => ops::add(a, b, self),
                },
                (BinaryOp::Sub, Value::Int(x), Value::Int(y)) => match x.checked_sub(*y) {
                    Some(r) => Ok(Value::Int(r)),
                    None => ops::sub(a, b, self),
                },
                (BinaryOp::Concat, Value::Str(x), Value::Str(y)) => {
                    Ok(Value::Str(ZStr::concat(x.as_bytes(), y.as_bytes())))
                }
                _ => ops::binary_op(o, a, b, self),
            },
            BinKind::Identical => ops::is_identical(a, b).map(Value::Bool),
            BinKind::NotIdentical => ops::is_identical(a, b).map(|r| Value::Bool(!r)),
            BinKind::Equal => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x == y)),
                _ => ops::is_equal(a, b, self).map(Value::Bool),
            },
            BinKind::NotEqual => ops::is_not_equal(a, b, self).map(Value::Bool),
            BinKind::Less => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x < y)),
                _ => ops::is_smaller(a, b, self).map(Value::Bool),
            },
            BinKind::LessEqual => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x <= y)),
                _ => ops::is_smaller_or_equal(a, b, self).map(Value::Bool),
            },
            BinKind::Spaceship => ops::spaceship(a, b, self),
        };
        r.map_err(|e| self.rt_throw(e))
    }

    /// `zend_is_true`.
    pub(crate) fn to_bool(&mut self, v: &Value) -> VmResult<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Int(i) => Ok(*i != 0),
            Value::Null => Ok(false),
            _ => v.to_bool(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_string`.
    pub(crate) fn to_zstr(&mut self, v: &Value) -> VmResult<ZStr> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            _ => v.to_zstr(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_long`.
    pub(crate) fn to_long(&mut self, v: &Value) -> VmResult<i64> {
        match v {
            Value::Int(i) => Ok(*i),
            _ => v.to_long(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_double`.
    pub(crate) fn to_double(&mut self, v: &Value) -> VmResult<f64> {
        match v {
            Value::Float(f) => Ok(*f),
            _ => v.to_double(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// Value copy semantics for assignment: references are unwrapped.
    #[inline]
    pub(crate) fn copy_on_assign(&self, v: Value) -> Value {
        match v {
            Value::Reference(r) => r.get(),
            v => v,
        }
    }

    fn cast(&mut self, kind: CastKind, source: Operand, raw: Value) -> VmResult<Value> {
        let (x, reference) = match raw {
            Value::Reference(r) => (r.get(), Some(r)),
            other => (other, None),
        };
        Ok(match kind {
            CastKind::Int => Value::Int(self.to_long(&x)?),
            CastKind::Float => Value::Float(self.to_double(&x)?),
            // A string is its own cast (`zval_get_string` adds a reference);
            // the rope compiler emits this for `"$str"`.
            CastKind::String => match x {
                Value::Str(_) => x,
                _ => Value::Str(self.to_zstr(&x)?),
            },
            CastKind::Bool => Value::Bool(self.to_bool(&x)?),
            CastKind::Null => Value::Null,
            CastKind::Array => match x {
                Value::Array(_) => x,
                Value::Object(o) => Value::Array(self.object_to_array(&o)?),
                Value::Null => Value::Array(ZArray::new()),
                other => {
                    let other = if matches!(other, Value::Float(f) if f.is_nan()) {
                        let held = match source {
                            Operand::Cv(cv) => self.cv_get(cv),
                            _ => reference.map(Value::Reference),
                        };
                        self.warning("unexpected NAN value was coerced to array")?;
                        // Zend wraps the value after the callback, without re-dispatching.
                        match held {
                            Some(Value::Reference(r)) => r.get(),
                            _ => match source {
                                Operand::Cv(cv) => self.cv_get(cv).unwrap_or(Value::Null).deref(),
                                _ => other,
                            },
                        }
                    } else {
                        other
                    };
                    Value::Array(ZArray::from_values([other]))
                }
            },
            CastKind::Object => match x {
                Value::Object(_) => x,
                Value::Array(a) => self.array_to_object(&a)?,
                Value::Null => self.create_std_object_vm()?,
                other => {
                    let o = self.create_std_object_vm()?;
                    let other = if matches!(other, Value::Float(f) if f.is_nan()) {
                        let held = match source {
                            Operand::Cv(cv) => self.cv_get(cv),
                            _ => reference.map(Value::Reference),
                        };
                        self.warning("unexpected NAN value was coerced to object")?;
                        match held {
                            Some(Value::Reference(r)) => r.get(),
                            _ => match source {
                                Operand::Cv(cv) => self.cv_get(cv).unwrap_or(Value::Null).deref(),
                                _ => other,
                            },
                        }
                    } else {
                        other
                    };
                    if let Value::Object(obj) = &o {
                        self.write_dynamic_prop(obj, ZStr::from("scalar"), other);
                    }
                    o
                }
            },
        })
    }

    fn array_literal_add(&mut self, arr: u32, key: Option<Value>, v: Value) -> VmResult<()> {
        let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
        let res = (|| -> VmResult<()> {
            let Value::Array(za) = &mut a else {
                return Ok(());
            };
            match key {
                None => {
                    if za.make_mut().push(v).is_err() {
                        return Err(self.make_throw(
                            "Error",
                            "Cannot add element to the array as the next element is already occupied",
                        ));
                    }
                }
                Some(k) => {
                    let key = self.array_key(&k, KeyCtx::Literal)?;
                    if let Some(key) = key {
                        za.make_mut().insert(key, v);
                    }
                }
            }
            Ok(())
        })();
        self.frame_mut().tmps[arr as usize] = a;
        res
    }

    /// `ZEND_FETCH_LIST_W` on a VAR container that is not a reference (a
    /// by-value call result, `Zend/zend_vm_def.h)`: notices and falls back
    /// to `ZEND_FETCH_LIST_R`; the element then becomes a fresh reference.
    #[cold]
    fn list_fetch_ref(&mut self, pid: crate::opcode::PlaceId, dst: Option<u32>) -> VmResult<()> {
        let referenceable = match self.frame().code.places[pid as usize].base {
            crate::opcode::PlaceBase::Value(
                crate::opcode::Operand::Tmp(t) | crate::opcode::Operand::TmpKeep(t),
            ) => matches!(self.frame().tmps[t as usize], Value::Reference(_)),
            _ => true,
        };
        if referenceable {
            let r = self.place_make_ref(pid)?;
            if let Some(d) = dst {
                self.set_tmp(d, Value::Reference(r));
            }
            return Ok(());
        }
        self.notice("Attempting to set reference to non referenceable value")?;
        let v = self.place_fetch(pid, crate::opcode::FetchMode::List)?;
        if let Some(d) = dst {
            self.set_tmp(d, Value::Reference(PhpRef::new(v)));
        }
        Ok(())
    }

    /// `zend_ast_add_unpacked_element` (`Zend/zend_ast.c)`: a constant
    /// expression spreads arrays only, never Traversables.
    #[cold]
    #[allow(clippy::needless_pass_by_value)] // the operand value is owned by the caller's read
    fn const_array_unpack(&mut self, arr: u32, src: Value) -> VmResult<()> {
        let src = src.deref();
        if !matches!(src, Value::Array(_)) {
            return Err(self.make_throw(
                "Error",
                "Only arrays can be unpacked in constant expression",
            ));
        }
        self.array_unpack(arr, src)
    }

    fn array_unpack(&mut self, arr: u32, src: Value) -> VmResult<()> {
        match src {
            Value::Array(s) => {
                let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
                if let Value::Array(za) = &mut a {
                    let t = za.make_mut();
                    for (k, v) in s.iter() {
                        let v = v.deref();
                        match k {
                            php_runtime::ArrayKey::Int(_) => {
                                if t.push(v).is_err() {
                                    self.frame_mut().tmps[arr as usize] = a;
                                    return Err(self.make_throw(
                                        "Error",
                                        "Cannot add element to the array as the next element is already occupied",
                                    ));
                                }
                            }
                            php_runtime::ArrayKey::Str(_) => {
                                t.insert(k.clone(), v);
                            }
                        }
                    }
                }
                self.frame_mut().tmps[arr as usize] = a;
                Ok(())
            }
            Value::Object(o) if self.instance_of(&o, b"traversable") => {
                let items = self.iterate_to_vec(&Value::Object(o))?;
                let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
                if let Value::Array(za) = &mut a {
                    let t = za.make_mut();
                    for (k, v) in items {
                        match k {
                            Value::Int(_) => {
                                let _ = t.push(v);
                            }
                            // ZEND_ADD_ARRAY_UNPACK: a numeric string key
                            // (ZEND_HANDLE_NUMERIC) is appended, not kept.
                            Value::Str(s) => match php_runtime::ArrayKey::from_zstr(&s) {
                                k @ php_runtime::ArrayKey::Str(_) => {
                                    t.insert(k, v);
                                }
                                php_runtime::ArrayKey::Int(_) => {
                                    let _ = t.push(v);
                                }
                            },
                            other => {
                                self.frame_mut().tmps[arr as usize] = a;
                                return Err(self.make_throw(
                                    "Error",
                                    &format!(
                                        "Keys must be of type int|string during array unpacking"
                                    ),
                                ))
                                .map(|()| drop(other));
                            }
                        }
                    }
                }
                self.frame_mut().tmps[arr as usize] = a;
                Ok(())
            }
            other => Err(self.make_throw(
                "Error",
                &format!(
                    "Only arrays and Traversables can be unpacked, {} given",
                    crate::typecheck::given_type(&other)
                ),
            )),
        }
    }

    /// `ZEND_THROW` + `zend_throw_exception_object`.
    fn op_throw(&mut self, v: Value) -> Throw {
        match v {
            Value::Object(o) if self.instance_of(&o, b"throwable") => Throw::Exception(o),
            Value::Object(_) => self.make_throw(
                "Error",
                "Cannot throw objects that do not implement Throwable",
            ),
            _ => self.make_throw("Error", "Can only throw objects"),
        }
    }

    fn op_exit(&mut self, v: Value) -> VmResult<()> {
        match v {
            Value::Str(s) => {
                self.echo(s.as_bytes());
            }
            Value::Int(i) => {
                #[allow(clippy::cast_possible_truncation)]
                let st = i as i32;
                self.exit_status = st;
            }
            Value::Null => {}
            Value::Bool(b) => {
                self.exit_status = i32::from(b);
            }
            Value::Float(f) => {
                #[allow(clippy::cast_possible_truncation)]
                let st = f as i32;
                self.exit_status = st;
            }
            other => {
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "exit(): Argument #1 ($status) must be of type string|int, {} given",
                        crate::typecheck::given_type(&other)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Runs the destructors of objects released since the last check.
    pub(crate) fn run_pending_destructors(&mut self) -> VmResult<()> {
        loop {
            let pending = object::take_pending_destructors();
            if pending.is_empty() {
                return Ok(());
            }
            // `zend_objects_destroy_object`: a destructor runs with the
            // pending exception set aside; a new exception gets the pending
            // one as its innermost `previous`.
            let mut pending_err: Option<Throw> = None;
            for o in pending {
                if let Err(e) = self.call_destructor(&o) {
                    pending_err = Some(match (pending_err.take(), e) {
                        (Some(Throw::Exception(old)), Throw::Exception(new)) => {
                            crate::exceptions::chain_previous(&new, &old);
                            Throw::Exception(new)
                        }
                        (Some(old), Throw::Exception(_)) => old,
                        (_, e) => e,
                    });
                }
                // Freed here unless the destructor stored it somewhere;
                // either way the handles of the objects whose release
                // queued it are free now.
                let h = o.handle();
                drop(o);
                object::release_deferred_handles(h);
            }
            if let Some(e) = pending_err {
                return Err(e);
            }
        }
    }

    /// Calls `__destruct` on an object.
    pub(crate) fn call_destructor(&mut self, o: &php_runtime::Obj) -> VmResult<()> {
        let cls = object::class_of(o).clone();
        let Some(m) = cls.magic.destruct.clone() else {
            // Native destructor hook without a `__destruct` method.
            if let Some(f) = cls.hooks.as_ref().and_then(|h| h.destruct) {
                return f(self, o);
            }
            return Ok(());
        };
        if m.visibility() != crate::func::flags::PUBLIC && !self.destructor_accessible(&cls, &m)? {
            return Ok(());
        }
        self.call_method(o, &m, Vec::new())?;
        // A native destructor hook also runs after a user `__destruct`
        // (the internal free path of the native parent).
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.destruct) {
            f(self, o)?;
        }
        Ok(())
    }

    /// The visibility check of `zend_objects_destroy_object` for a
    /// private/protected `__destruct`: while code executes, an inaccessible
    /// destructor throws an `Error`; with nothing executing (request
    /// shutdown) it is skipped with a warning. `Ok(false)` skips the call.
    #[cold]
    fn destructor_accessible(
        &mut self,
        cls: &Rc<crate::class::Class>,
        m: &crate::func::Method,
    ) -> VmResult<bool> {
        let vis = if m.visibility() == crate::func::flags::PRIVATE {
            "private"
        } else {
            "protected"
        };
        if !self.is_executing() {
            self.warning(&format!(
                "Call to {vis} {}::__destruct() from global scope during shutdown ignored",
                cls.name
            ))?;
            return Ok(false);
        }
        let scope = self.scope();
        if self.can_access_method(m, scope.as_ref()) {
            return Ok(true);
        }
        Err(self.make_throw(
            "Error",
            &format!(
                "Call to {vis} {}::__destruct() from {}",
                cls.name,
                scope_display(scope.as_ref())
            ),
        ))
    }

    /// The type of `ZEND_RETURN`: finishes the current frame.
    pub(crate) fn op_return(&mut self, v: Option<Value>, base: usize) -> VmResult<Flow> {
        let v = self.verify_return(v)?;
        self.op_return_raw(Some(v), base)
    }

    pub(crate) fn op_return_raw(&mut self, v: Option<Value>, base: usize) -> VmResult<Flow> {
        let v = v.unwrap_or_default();
        let fidx = self.frames.len() - 1;
        self.observer_end_frame(fidx, Some(&v), None);
        if self.frames[fidx].generator.is_some() {
            let frame = self.frames.pop().expect("frame");
            if let Some(g) = &frame.generator {
                crate::generator::finish_with(g, v);
            }
            drop(frame);
            if fidx <= base {
                return Ok(Flow::Done(Value::Null));
            }
            return Ok(Flow::Reload);
        }
        // The frame is released where it lives on the frame stack (a
        // `Frame` is a few hundred bytes, every move of one is a `memcpy`):
        // only a pooled frame moves, once. Values released here only queue
        // destructors, which run at the next safe point, after the pop.
        let (ret, by_ref, should_pool) = {
            let frame = &self.frames[fidx];
            let caller_this = fidx
                .checked_sub(1)
                .and_then(|i| self.frames[i].this.as_ref());
            crate::gc::frame_released(frame, caller_this);
            let by_ref = frame
                .ufunc
                .as_ref()
                .is_some_and(|u| u.flags & crate::func::flags::RETURN_REF != 0);
            let is_user_func =
                frame.kind == crate::vm::FrameKind::Function && frame.ufunc.is_some();
            let should_pool = is_user_func
                && self.call_bufs.user_frames.len() < 8
                && frame.tmps.capacity() <= 256
                && frame.calls.capacity() <= 64;
            (frame.ret, by_ref, should_pool)
        };
        if should_pool {
            self.frames[fidx].clear_for_pool(&self.empty_code);
            // One copy into the pool (`pop` + `push` would copy the frame
            // twice, through a temporary).
            self.call_bufs.user_frames.extend(self.frames.drain(fidx..));
        } else {
            self.frames.truncate(fidx);
        }
        let v = if by_ref { v } else { self.copy_on_assign(v) };
        if fidx <= base {
            return Ok(Flow::Done(v));
        }
        if let RetTarget::Tmp(t) = ret {
            self.set_tmp(t, v);
        }
        Ok(Flow::Reload)
    }

    /// `Fetching properties on non-enums in constant expressions is not
    /// allowed` (`zend_ast_evaluate_ex`, `ZEND_AST_PROP`).
    #[cold]
    fn non_enum_const_prop(&mut self) -> Throw {
        self.make_throw(
            "Error",
            "Fetching properties on non-enums in constant expressions is not allowed",
        )
    }

    /// `Cannot use [] on objects in constant expression`
    /// (`zend_ast_evaluate_ex`, `ZEND_AST_DIM`).
    #[cold]
    fn const_dim_on_object(&mut self) -> Throw {
        self.make_throw("Error", "Cannot use [] on objects in constant expression")
    }

    /// Creates an exception of an internal class and returns the throw.
    pub(crate) fn make_throw(&mut self, class: &str, msg: &str) -> Throw {
        match self.create_exception(class, msg, 0) {
            Ok(o) => Throw::Exception(o),
            Err(t) => t,
        }
    }

    /// Reports an `E_WARNING`.
    pub(crate) fn warning(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Warning.bits(), msg)
    }

    /// Reports an `E_NOTICE`.
    pub(crate) fn notice(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Notice.bits(), msg)
    }

    /// Reports an `E_DEPRECATED`.
    pub(crate) fn deprecated(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Deprecated.bits(), msg)
    }

    /// The current class scope.
    pub(crate) fn scope(&self) -> Option<Rc<crate::class::Class>> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .and_then(|f| f.scope.clone())
    }

    /// `ZEND_TYPE_ASSERT` (`Zend/zend_vm_def.h`) for an array parameter: the
    /// operand is read without an undefined-variable warning.
    fn op_type_assert_array(&mut self, val: Operand, msg: u32) -> VmResult<()> {
        let v = if let Operand::Cv(cv) = val {
            self.read_cv_defined(cv).unwrap_or(Value::Null)
        } else {
            self.read_raw(val)?
        };
        if matches!(v, Value::Array(_)) {
            return Ok(());
        }
        Err(self.type_assert_error(&v, msg))
    }

    #[cold]
    #[inline(never)]
    fn type_assert_error(&mut self, v: &Value, msg: u32) -> Throw {
        let prefix = self.lit_str(msg);
        let m = format!(
            "{}, {} given",
            prefix.to_string_lossy(),
            crate::typecheck::given_type(v)
        );
        self.make_throw("TypeError", &m)
    }
}

/// Releases the old value of a variable slot that holds no possible GC
/// root (no array, object or reference: `gc::possible_root` would ignore
/// it). Scalars own nothing and are forgotten, which skips the
/// out-of-line `drop_in_place` call; strings and resources are dropped.
#[inline(always)]
#[allow(clippy::inline_always)] // a call would defeat the point
fn discard_unrooted(old: Option<Value>) {
    match old {
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
            std::mem::forget(old);
        }
        other => drop(other),
    }
}

/// Releases the old value of a variable slot (`i_zval_ptr_dtor` /
/// `zval_ptr_dtor` of a variable): an array, object or reference may
/// become a GC possible root ([`crate::gc::possible_root`], out of line);
/// anything else is released at once by [`discard_unrooted`].
#[inline(always)]
#[allow(clippy::inline_always)] // the common case is an empty or scalar slot
fn release_slot_value(old: Option<Value>) {
    if matches!(
        old,
        Some(Value::Array(_) | Value::Object(_) | Value::Reference(_))
    ) {
        release_rooted(old);
    } else {
        discard_unrooted(old);
    }
}

/// [`release_slot_value`] of an array, object or reference.
#[inline(never)]
fn release_rooted(old: Option<Value>) {
    if let Some(old) = &old {
        crate::gc::possible_root(old);
    }
    drop(old);
}

/// A compiled variable read as the value of an assignment to a place
/// ([`Vm::read_cv_op_data`]).
pub(crate) enum CvOpData {
    /// The (dereferenced) value.
    Value(Value),
    /// A reference to null, false or an array, possibly a container of the
    /// place: read by the walk ([`Vm::place_assign_cv_ref`]).
    Deferred,
    /// Undefined.
    Undefined,
}

/// How a key is used (affects error messages).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyCtx {
    /// Array literal.
    Literal,
    /// Read access.
    Read,
    /// Write access.
    Write,
    /// isset/empty.
    Isset,
    /// unset.
    Unset,
}

/// `E_FATAL_ERRORS` (`Zend/zend_errors.h`): `E_ERROR | E_CORE_ERROR |
/// E_COMPILE_ERROR | E_USER_ERROR | E_RECOVERABLE_ERROR | E_PARSE`.
const E_FATAL_ERRORS: i64 = 1 | 16 | 64 | 256 | 4096 | 4;

/// `E_HAS_ONLY_FATAL_ERRORS(mask)` (`Zend/zend_errors.h`).
const fn has_only_fatal_errors(mask: i64) -> bool {
    mask & !E_FATAL_ERRORS == 0
}

/// The mask after leaving an `@` whose saved mask is `old`
/// (`ZEND_END_SILENCE` / `ZEND_LIVE_SILENCE` cleanup): restored only when the
/// silenced expression did not set a non-silenced mask of its own.
pub(crate) const fn silence_restore(current: i64, old: i64) -> i64 {
    if has_only_fatal_errors(current) && !has_only_fatal_errors(old) {
        old
    } else {
        current
    }
}

/// "scope X" / "global scope" for messages.
pub(crate) fn scope_display(scope: Option<&Rc<crate::class::Class>>) -> String {
    match scope {
        Some(c) => format!("scope {}", object::display_name(&c.name)),
        None => "global scope".to_string(),
    }
}

/// "Undefined variable $x" — or "Undefined global variable $x" for the
/// auto-globals, which php-src fetches with `ZEND_FETCH_GLOBAL_LOCK`.
pub(crate) fn undefined_var_msg(name: &impl std::fmt::Display) -> String {
    let n = name.to_string();
    let global = matches!(
        n.as_str(),
        "_GET" | "_POST" | "_COOKIE" | "_SERVER" | "_ENV" | "_REQUEST" | "_FILES" | "_SESSION"
    );
    format!(
        "Undefined {}variable ${n}",
        if global { "global " } else { "" }
    )
}

thread_local! {
    /// Where the pending `memory_limit` violation was first seen
    /// (`EG(delayed_oom).filename`/`lineno`, recorded by
    /// `zend_mm_delayed_verror` at allocation time): the fatal is reported
    /// there even when it is raised at a later interrupt point.
    static OOM_SITE: std::cell::RefCell<Option<(ZStr, u32)>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether a pending OOM may wait past this instruction. php-src only acts
/// on `EG(vm_interrupt)` at interrupt points (`ZEND_VM_ENTER`, taken jumps,
/// after internal calls: `ZEND_VM_FCALL_INTERRUPT_CHECK`), so the plain
/// data instructions after the allocation that crossed the limit still run
/// (`oom_after_main_script.phpt` prints "Done" before the fatal).
fn defers_oom(op: &Op) -> bool {
    matches!(
        op,
        Op::Nop
            | Op::Move { .. }
            | Op::Bin { .. }
            | Op::Unary { .. }
            | Op::Cast { .. }
            | Op::Rope { .. }
            | Op::InitArray { .. }
            | Op::ArrayAdd { .. }
            | Op::AssignCv { .. }
            | Op::Assign { .. }
            | Op::AssignOp { .. }
            | Op::Echo(_)
            | Op::Free(_)
    )
}

impl Vm {
    /// The per-instruction check of a pending `memory_limit` violation:
    /// raises it unless `op` [defers](defers_oom) it, in which case only
    /// the location is remembered.
    #[cold]
    #[inline(never)]
    fn poll_pending_oom(&mut self, op: &Op) -> VmResult<()> {
        if defers_oom(op) {
            if OOM_SITE.with(|s| s.borrow().is_none()) {
                let site = self.current_location();
                OOM_SITE.with(|s| *s.borrow_mut() = Some(site));
            }
            return Ok(());
        }
        self.raise_pending_oom()
    }

    /// Raises a recorded `memory_limit` violation as PHP's fatal error
    /// (`zend_handle_delayed_oom`), at the location it was first seen.
    pub(crate) fn raise_pending_oom(&mut self) -> VmResult<()> {
        let site = OOM_SITE.with(|s| s.borrow_mut().take());
        if let Some(over) = php_memlimit::take() {
            let (file, line) = site.unwrap_or_else(|| self.current_location());
            // `php_error_cb` (main/main.c) while the memory-limit error is
            // reported: the output buffers are discarded, unless a handler
            // is running, so the message reaches the SAPI and the buffered
            // output is neither printed nor copied again.
            self.reporting_oom = true;
            if self.output.status() & php_support::output::status::LOCKED == 0 {
                php_support::output::discard_all(self);
            }
            let r = self.error_at(crate::errors::level::ERROR, &over.to_string(), &file, line);
            self.reporting_oom = false;
            r?;
        }
        Ok(())
    }

    /// Forgets a pending violation and its location (start of request
    /// shutdown: `EG(delayed_oom).state = ZEND_OOM_STATE_NONE`).
    pub(crate) fn clear_pending_oom() {
        php_memlimit::clear();
        OOM_SITE.with(|s| *s.borrow_mut() = None);
    }
}

/// `ZEND_HAS_OOM()`: a delayed `memory_limit` fatal is pending. Builtins
/// that loop over script-sized input check it to stop early (the VM raises
/// the fatal right after the call returns).
#[must_use]
#[inline]
pub fn oom_pending() -> bool {
    php_memlimit::is_pending()
}

/// A fallible allocation of `bytes` (`safe_femalloc(..., fallible)`):
/// `false` when it would exceed `memory_limit`, in which case the violation
/// is recorded and the caller returns without allocating (`RETURN_OOM()`).
#[must_use]
pub fn try_reserve_memory(bytes: usize) -> bool {
    php_memlimit::try_reserve(bytes).is_ok()
}

impl Vm {
    /// Whether the native stack used since the first execution exceeds the
    /// usable budget (`zend.max_allowed_stack_size` minus
    /// `zend.reserved_stack_size`, or the thread's stack minus a safety
    /// margin for the interpreter's own frames).
    /// Extensions may use this check without raising the VM's stack error.
    pub fn native_stack_exhausted(&mut self) -> bool {
        let marker = 0u8;
        let here = std::ptr::addr_of!(marker) as usize;
        if self.native_stack_base == 0 || here > self.native_stack_base {
            self.native_stack_base = here;
            return false;
        }
        let used = self.native_stack_base - here;
        // Both INIs are INI_SYSTEM: read them once.
        let (max, reserved) = match self.stack_limit_ini {
            Some(t) => t,
            None => {
                let t = (
                    self.ini.get_long("zend.max_allowed_stack_size"),
                    usize::try_from(self.ini.get_long("zend.reserved_stack_size").max(0))
                        .unwrap_or(0),
                );
                self.stack_limit_ini = Some(t);
                t
            }
        };
        if max < 0 {
            return false;
        }
        // The real thread stack must never be smashed, whatever the INI says.
        let hard = self
            .native_stack_size
            .saturating_sub((self.native_stack_size / 8).max(256 << 10));
        let budget = if max > 0 {
            // `zend.max_allowed_stack_size` is sized for php-src's frames;
            // the interpreter's are ~4x larger (same scale as fiber stacks).
            usize::try_from(max)
                .unwrap_or(usize::MAX)
                .saturating_sub(reserved)
                .saturating_mul(crate::fiber::STACK_SCALE)
        } else {
            hard.saturating_sub(reserved)
        };
        used > budget.min(hard)
    }

    /// Declares the native stack size of the thread running the VM (the
    /// CLI's large VM stack); the default assumes 8 MiB.
    pub fn set_native_stack_size(&mut self, bytes: usize) {
        self.native_stack_size = bytes;
    }
}

/// `cleanup_unfinished_calls` + `ZEND_LIVE_NEW`: the object of a `new`
/// whose constructor call an exception abandons before it was made is
/// never destructed (`zend_object_store_ctor_failed`).
#[cold]
pub(crate) fn ctor_failed_unfinished(calls: &[crate::vm::PendingCall]) {
    for c in calls {
        if c.is_ctor {
            if let Some(t) = &c.this {
                let d = object::data(t);
                d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
            }
        }
    }
}
