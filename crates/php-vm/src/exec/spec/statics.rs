// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Fast paths of the static-property ops ([`Op::FetchStaticProp`],
//! [`Op::AssignStaticProp`], [`Op::AssignOpStaticProp`],
//! [`Op::IssetStaticProp`]): php-src's `ZEND_FETCH_STATIC_PROP_R` / `_IS`,
//! `ZEND_ASSIGN_STATIC_PROP`, `ZEND_ASSIGN_STATIC_PROP_OP` and
//! `ZEND_ISSET_ISEMPTY_STATIC_PROP` when `zend_fetch_static_property_address`
//! finds the property in the op's run-time cache (`CACHED_PTR`,
//! `Zend/zend_execute.c`): the storage cell is used directly, without the
//! generic place walk.
//!
//! The cache ([`Vm::static_prop_cached`], `rtcache.rs`) only holds
//! declared, initialised, accessible properties of a class whose class
//! the op already resolved, so a hit can raise no error. Each fast path
//! below also stays clear of anything with a side effect: references (the
//! property may be bound to a typed reference), arrays and objects (their
//! release may run a destructor or buffer a GC root), conversions (they
//! may warn or call user code) and typed properties where the write would
//! need the type check. Those, and every miss, return "not done" having
//! changed nothing, and the generic op runs.
//!
//! [`Op::FetchStaticProp`]: crate::opcode::Op::FetchStaticProp
//! [`Op::AssignStaticProp`]: crate::opcode::Op::AssignStaticProp
//! [`Op::AssignOpStaticProp`]: crate::opcode::Op::AssignOpStaticProp
//! [`Op::IssetStaticProp`]: crate::opcode::Op::IssetStaticProp

use super::super::{num_bin, CvOpData, Flow, Num};
use super::{is_plain_value, plain_truth, plain_value_in, put_tmp, store_fetched_in};
use crate::opcode::{BinKind, ClassSpec, FetchMode, IncDecKind, Operand, PlaceId};
use crate::rtcache::{
    class_const_hit_dyn_in, class_const_hit_in, const_hit_in, static_place_cell_in,
    static_prop_entry_in,
};
use crate::vm::{Frame, Vm, VmResult};
use php_runtime::{BinaryOp, Value, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

/// The value (dereferenced, copied) a cached static property cell holds
/// for a read in `mode` (`plain`: the property is untyped): `None` when
/// the cell or its reference is borrowed, and for a typed property holding
/// null in `Read` mode, which may be uninitialized (the generic read
/// throws "must not be accessed before initialization").
#[inline(always)]
#[allow(clippy::inline_always)] // the read of every static property cache hit
fn static_cell_value(cell: &RefCell<Value>, plain: bool, mode: FetchMode) -> Option<Value> {
    let slot = cell.try_borrow().ok()?;
    match &*slot {
        Value::Int(i) => Some(Value::Int(*i)),
        Value::Null if !plain && mode != FetchMode::Is => None,
        Value::Reference(r) => r.try_borrow().map(|inner| inner.clone()),
        v => Some(v.clone()),
    }
}

/// [`Op::FetchConstTo`] / [`Op::FetchConst`] (`to: None`) on a cache hit,
/// on the running frame `f` alone (the frame-local hot loop,
/// `exec/hot.rs`): the cached value of the constant named by literal
/// `name` is stored like `Vm::store_fetched` does. `Some(true)`: stored
/// into `to` (skip the `AssignCv` after the op); `Some(false)`: written to
/// `dst`; `None`: a miss, nothing changed.
///
/// [`Op::FetchConstTo`]: crate::opcode::Op::FetchConstTo
/// [`Op::FetchConst`]: crate::opcode::Op::FetchConst
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn fetch_const_in(
    f: &mut Frame,
    name: u32,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    let v = const_hit_in(f, name)?;
    Some(store_fetched_in(f, dst, to, v))
}

/// [`Op::FetchClassConstTo`] / [`Op::FetchClassConst`] (`to: None`) of a
/// class named, `self`, `parent`, `static` or held as an object by a
/// temporary or variable (`$this::X`, `$obj::X`) on a cache hit, on the
/// running frame `f` alone, as [`fetch_const_in`].
///
/// [`Op::FetchClassConstTo`]: crate::opcode::Op::FetchClassConstTo
/// [`Op::FetchClassConst`]: crate::opcode::Op::FetchClassConst
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn fetch_class_const_in(
    f: &mut Frame,
    class: &ClassSpec,
    name: u32,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    let v = if let ClassSpec::Dyn(op) = *class {
        let v = class_const_hit_dyn_in(f, op, name)?;
        // The class operand is consumed (`FREE_OP1`): one of several
        // handles to the object, so only a count drops.
        if let Operand::Tmp(t) = op {
            if let Some(slot) = f.tmps.get_mut(t as usize) {
                drop(std::mem::take(slot));
            }
        }
        v
    } else {
        class_const_hit_in(f, class, name)?
    };
    Some(store_fetched_in(f, dst, to, v))
}

/// [`Op::FetchStaticProp`] on a hit of the place's fast entry, on the
/// running frame `f` alone, as [`fetch_const_in`] (see
/// [`static_cell_value`] for what declines).
///
/// [`Op::FetchStaticProp`]: crate::opcode::Op::FetchStaticProp
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn fetch_static_prop_in(
    f: &mut Frame,
    pid: PlaceId,
    dst: u32,
    to: Option<u32>,
    mode: FetchMode,
) -> Option<bool> {
    // `Some(None)`: a hit whose value takes the generic read.
    let v = match static_prop_entry_in(f, pid, |e| {
        Some(static_cell_value(&e.cell, e.fast_plain, mode))
    }) {
        Some(v) => v?,
        None => {
            // The fast entry is filled from the member cache (the generic
            // op only fills that one).
            let (cell, typed) = static_place_cell_in(f, pid)?;
            static_cell_value(&cell, !typed, mode)?
        }
    };
    Some(store_fetched_in(f, dst, to, v))
}

/// [`Op::IssetStaticProp`] on a hit of the place's fast entry, on the
/// running frame `f` alone: `Vm::isset_static_prop_fast`'s answer written
/// to `dst`. `false`, nothing changed, on a miss or when the answer needs
/// the generic path.
///
/// [`Op::IssetStaticProp`]: crate::opcode::Op::IssetStaticProp
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn isset_static_prop_in(
    f: &mut Frame,
    pid: PlaceId,
    dst: u32,
    empty: bool,
) -> bool {
    let r = match static_prop_entry_in(f, pid, |e| Some(static_cell_isset(&e.cell, empty))) {
        Some(Some(r)) => r,
        Some(None) => return false,
        None => {
            match static_place_cell_in(f, pid).and_then(|(c, _)| static_cell_isset(&c, empty)) {
                Some(r) => r,
                None => return false,
            }
        }
    };
    let Some(slot) = f.tmps.get_mut(dst as usize) else {
        return false;
    };
    put_tmp(slot, Value::Bool(r));
    true
}

/// `isset()` / `empty()` of the value a static property cell holds
/// (dereferenced): not null, or, for `empty`, the negated truth value when
/// that needs no conversion ([`plain_truth`]). A typed property holding
/// null gives the answer an uninitialized one gives (`isset` false,
/// `empty` true). `None` when a borrow fails or a conversion is needed.
#[inline(always)]
#[allow(clippy::inline_always)]
fn static_cell_isset(cell: &RefCell<Value>, empty: bool) -> Option<bool> {
    let slot = cell.try_borrow().ok()?;
    let guard;
    let v: &Value = match &*slot {
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            &*guard
        }
        v => v,
    };
    if empty {
        plain_truth(v).map(|t| !t)
    } else {
        Some(!matches!(v, Value::Null))
    }
}

/// [`Op::AssignStaticProp`] on a hit of the place's fast entry, on the
/// running frame `f` alone: `Vm::assign_static_prop_fast` (an untyped
/// property holding a scalar or string gets a scalar or string value; the
/// old value's release has no side effect). `false`, nothing changed,
/// otherwise.
///
/// [`Op::AssignStaticProp`]: crate::opcode::Op::AssignStaticProp
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn assign_static_prop_in(
    f: &mut Frame,
    pid: PlaceId,
    val: Operand,
    dst: Option<u32>,
) -> bool {
    let cell = match static_prop_entry_in(f, pid, |e| Some(e.fast_plain.then(|| e.cell.clone()))) {
        Some(Some(cell)) => cell,
        // A typed property: the write needs the type check.
        Some(None) => return false,
        None => match static_place_cell_in(f, pid) {
            Some((cell, false)) => cell,
            _ => return false,
        },
    };
    let Some(v) = plain_value_in(f, val) else {
        return false;
    };
    let Ok(mut slot) = cell.try_borrow_mut() else {
        return false;
    };
    if !is_plain_value(&slot) {
        return false;
    }
    let keep = dst.map(|_| v.clone());
    let old = std::mem::replace(&mut *slot, v);
    drop(slot);
    drop(old);
    // The OP_DATA temporary is consumed.
    if let Operand::Tmp(t) = val {
        if let Some(s) = f.tmps.get_mut(t as usize) {
            drop(std::mem::take(s));
        }
    }
    if let (Some(d), Some(k)) = (dst, keep) {
        if let Some(s) = f.tmps.get_mut(d as usize) {
            put_tmp(s, k);
        }
    }
    true
}

/// `++`/`--` of the static property place `pid` on a cache hit, on the
/// running frame `f` alone: `Vm::inc_dec_static_prop_fast` (an untyped
/// property holding an int, without overflow, or a float is updated in its
/// cell), with the result (the new value, or the old one for a post
/// operator) stored in `dst`. `false`, nothing changed, otherwise: a typed
/// property, a reference, an overflow, another type, a miss.
#[inline(always)]
#[allow(clippy::inline_always)] // one arm of the hot loop
pub(in crate::exec) fn incdec_static_prop_in(
    f: &mut Frame,
    pid: PlaceId,
    kind: IncDecKind,
    dst: Option<u32>,
) -> bool {
    if dst.is_some_and(|d| d as usize >= f.tmps.len()) {
        return false;
    }
    let cell = match static_prop_entry_in(f, pid, |e| Some(e.fast_plain.then(|| e.cell.clone()))) {
        Some(Some(cell)) => cell,
        // A typed property: the result needs the type check.
        Some(None) => return false,
        None => match static_place_cell_in(f, pid) {
            Some((cell, false)) => cell,
            _ => return false,
        },
    };
    let result = {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        match &mut *slot {
            Value::Int(cur) => {
                let old = *cur;
                let new = if kind.is_inc() {
                    old.checked_add(1)
                } else {
                    old.checked_sub(1)
                };
                let Some(new) = new else {
                    return false;
                };
                *cur = new;
                Value::Int(if kind.is_post() { old } else { new })
            }
            Value::Float(cur) => {
                let old = *cur;
                let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
                *cur = new;
                Value::Float(if kind.is_post() { old } else { new })
            }
            _ => return false,
        }
    };
    if let Some(slot) = dst.and_then(|d| f.tmps.get_mut(d as usize)) {
        put_tmp(slot, result);
    }
    true
}

#[allow(dead_code)]
impl Vm {
    /// The cached storage cell of the static-property place `pid`
    /// (`A::$name`, literal name) and whether the property is typed.
    #[inline]
    fn static_place_cell(&self, pid: PlaceId) -> Option<(Rc<RefCell<Value>>, bool)> {
        if let Some((cell, plain)) = self.static_prop_cache_probe(pid) {
            return Some((cell, !plain));
        }
        static_place_cell_in(self.frame(), pid)
    }

    /// `A::$name` read (`Read` or `Is` mode) on a cache hit: the value
    /// (dereferenced). `None` on a miss, and for a typed property holding
    /// null in `Read` mode, which may be uninitialized (the generic read
    /// throws "must not be accessed before initialization").
    #[inline(always)]
    pub(in crate::exec) fn fetch_static_prop_fast(
        &self,
        pid: PlaceId,
        mode: FetchMode,
    ) -> Option<Value> {
        if let Some(v) = static_prop_entry_in(self.frame(), pid, |e| {
            static_cell_value(&e.cell, e.fast_plain, mode)
        }) {
            return Some(v);
        }
        let (cell, typed) = self.static_place_cell(pid)?;
        let slot = cell.try_borrow().ok()?;
        match &*slot {
            Value::Null if typed && mode != FetchMode::Is => None,
            Value::Reference(r) => r.try_borrow().map(|inner| inner.clone()),
            v => Some(v.clone()),
        }
    }

    /// `A::$name = val;` on a cache hit: an untyped property holding a
    /// scalar or string gets a scalar or string value (literal, defined
    /// compiled variable, temporary); the old value's release has no side
    /// effect. Returns `false`, having changed nothing, otherwise.
    #[inline(always)]
    pub(in crate::exec) fn assign_static_prop_fast(
        &mut self,
        pid: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let Some(v) = self.peek_plain_value(val) else {
            return false;
        };
        let cell = if let Some((cell, plain)) = self.static_prop_cache_probe(pid) {
            if !plain {
                return false;
            }
            cell
        } else {
            let Some((cell, false)) = self.static_place_cell(pid) else {
                return false;
            };
            cell
        };
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        if !is_plain_value(&slot) {
            return false;
        }
        let keep = dst.map(|_| v.clone());
        let old = std::mem::replace(&mut *slot, v);
        drop(slot);
        drop(old);
        // The OP_DATA temporary is consumed.
        if let Operand::Tmp(t) = val {
            self.set_tmp(t, Value::Null);
        }
        if let (Some(d), Some(k)) = (dst, keep) {
            self.set_tmp(d, k);
        }
        true
    }

    /// `A::$name op= val` on a cache hit: `.=` on a property holding a
    /// string ([`Vm::concat_static_prop_fast`]), or `+=`, `-=`, `*=`,
    /// `|=`, `&=`, `^=` on an untyped property holding a number with a
    /// number, when the result stays in the int domain or is a float
    /// (`zend_binary_op` on the fetched slot). Returns `false`, having
    /// changed nothing, otherwise.
    #[inline(always)]
    pub(in crate::exec) fn assign_op_static_prop_fast(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        if op == BinaryOp::Concat {
            return self.concat_static_prop_fast(pid, val, dst);
        }
        let Some(y) = Self::peek_num(self.frame(), val) else {
            return false;
        };
        let cell = if let Some((cell, plain)) = self.static_prop_cache_probe(pid) {
            if !plain {
                return false;
            }
            cell
        } else {
            let Some((cell, false)) = self.static_place_cell(pid) else {
                return false;
            };
            cell
        };
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        let x = match *slot {
            Value::Int(i) => Num::Int(i),
            Value::Float(f) => Num::Float(f),
            _ => return false,
        };
        let Some(r) = num_bin(BinKind::Arith(op), x, y) else {
            return false;
        };
        let keep = dst.map(|_| r.clone());
        // A number replaces a number: nothing to release.
        *slot = r;
        drop(slot);
        if let (Some(d), Some(k)) = (dst, keep) {
            self.set_tmp(d, k);
        }
        true
    }

    /// `A::$name .= val` on a cache hit: a property holding a string
    /// directly (typed or not: a typed property holding a string accepts
    /// the string result, `zend_binary_assign_op_typed_prop` concatenates
    /// in place too) is extended in place with a string or the digits of
    /// an int (`concat_function` with `result == op1`: `zend_string_extend`
    /// when nothing else holds the string, one copy otherwise). The string
    /// itself as the operand, a reference, another type on either side
    /// (conversions may warn or call `__toString()`) take the generic path.
    #[inline(always)]
    fn concat_static_prop_fast(&mut self, pid: PlaceId, val: Operand, dst: Option<u32>) -> bool {
        let add = match self.peek_plain_value(val) {
            Some(v @ (Value::Str(_) | Value::Int(_))) => v,
            _ => return false,
        };
        let cell = if let Some((cell, _)) = self.static_prop_cache_probe(pid) {
            cell
        } else {
            let Some((cell, _)) = self.static_place_cell(pid) else {
                return false;
            };
            cell
        };
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        let Value::Str(s) = &mut *slot else {
            return false;
        };
        match &add {
            Value::Str(t) if !ZStr::ptr_eq(s, t) => s.append_str(t),
            Value::Int(i) => s.append_long(*i),
            _ => return false,
        }
        let keep = dst.map(|_| Value::Str(s.clone()));
        drop(slot);
        drop(add);
        // The OP_DATA temporary is consumed.
        if let Operand::Tmp(t) = val {
            self.set_tmp(t, Value::Null);
        }
        if let (Some(d), Some(k)) = (dst, keep) {
            self.set_tmp(d, k);
        }
        true
    }

    /// `++`/`--` of static property `pid` on a cache hit
    /// (`ZEND_PRE_INC_STATIC_PROP` / `ZEND_POST_INC_STATIC_PROP` with
    /// `CACHED_PTR`): an untyped property holding an int (no overflow) or a
    /// float directly is updated in its cell. The result is the new value
    /// (pre) or the old one (post). Typed properties (the result must be
    /// checked against the type), references (possibly typed), overflow
    /// (int to float), other types (conversions, warnings) and misses give
    /// `None` with nothing changed; the generic place walk then runs.
    #[inline(always)]
    pub(crate) fn inc_dec_static_prop_fast(&self, pid: PlaceId, kind: IncDecKind) -> Option<Value> {
        let (cell, typed) = self.static_place_cell(pid)?;
        if typed {
            return None;
        }
        let mut slot = cell.try_borrow_mut().ok()?;
        match &mut *slot {
            Value::Int(cur) => {
                let old = *cur;
                let new = if kind.is_inc() {
                    old.checked_add(1)?
                } else {
                    old.checked_sub(1)?
                };
                *cur = new;
                Some(Value::Int(if kind.is_post() { old } else { new }))
            }
            Value::Float(cur) => {
                let old = *cur;
                let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
                *cur = new;
                Some(Value::Float(if kind.is_post() { old } else { new }))
            }
            _ => None,
        }
    }

    /// `isset(A::$name)` / `empty(A::$name)` on a cache hit: whether the
    /// value (dereferenced) is not null, or, for `empty`, its truth value
    /// when that needs no conversion (see `plain_truth`; objects,
    /// resources and NaN go generic). A typed property holding null gives
    /// the answer an uninitialized one gives (`isset` false, `empty`
    /// true).
    #[inline(always)]
    pub(in crate::exec) fn isset_static_prop_fast(
        &self,
        pid: PlaceId,
        empty: bool,
    ) -> Option<bool> {
        let cell = if let Some((cell, _)) = self.static_prop_cache_probe(pid) {
            cell
        } else {
            let (cell, _) = self.static_place_cell(pid)?;
            cell
        };
        let slot = cell.try_borrow().ok()?;
        let guard;
        let v: &Value = match &*slot {
            Value::Reference(r) => {
                guard = r.try_borrow()?;
                &*guard
            }
            v => v,
        };
        if empty {
            plain_truth(v).map(|t| !t)
        } else {
            Some(!matches!(v, Value::Null))
        }
    }

    /// Handler for [`Op::FetchStaticProp`].
    pub(in crate::exec) fn op_fetch_static_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
        ip: &mut usize,
    ) -> VmResult<Flow> {
        if let Some(v) = self.fetch_static_prop_fast(place, mode) {
            if self.store_fetched(dst, to, v) {
                *ip += 1;
            }
            return Ok(Flow::Next);
        }
        let v = self.place_fetch(place, mode)?;
        self.set_tmp(dst, v);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignStaticProp`].
    pub(in crate::exec) fn op_assign_static_prop(
        &mut self,
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_static_prop_fast(place, val, dst) {
            return Ok(Flow::Next);
        }
        let r = match val {
            Operand::Cv(cv) => match self.read_cv_op_data(cv) {
                CvOpData::Value(v) => self.place_assign(place, v)?,
                CvOpData::Deferred => self.place_assign_cv_ref(place, cv)?,
                CvOpData::Undefined => self.place_assign_undef_cv(place, cv)?,
            },
            other => {
                let v = self.read(other)?;
                let v = self.copy_on_assign(v);
                self.place_assign(place, v)?
            }
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignOpStaticProp`].
    pub(in crate::exec) fn op_assign_op_static_prop(
        &mut self,
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_op_static_prop_fast(place, op, val, dst) {
            return Ok(Flow::Next);
        }
        let concat = op == BinaryOp::Concat;
        if let Some(cv) = self.assign_op_undef_dim_cv(place, val) {
            let r = self.place_assign_op_undef_cv(place, op, cv)?;
            self.set_dst(dst, r);
        } else if !(concat
            && (self.concat_assign_cv_lit(place, val, dst)
                || (dst.is_none() && self.concat_dims_cv_fast(place, val))))
        {
            let v = self.read(val)?;
            if !(concat && self.concat_assign_cv(place, &v, dst)) {
                let r = self.place_assign_op(place, op, v)?;
                self.set_dst(dst, r);
            }
        }
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IncDecStaticProp`].
    pub(in crate::exec) fn op_inc_dec_static_prop(
        &mut self,
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        let r = match self.inc_dec_static_prop_fast(place, kind) {
            Some(r) => r,
            None => self.place_incdec(place, kind)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Alias for [`Vm::op_inc_dec_static_prop`].
    #[inline(always)]
    pub(in crate::exec) fn op_incdec_static_prop(
        &mut self,
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        self.op_inc_dec_static_prop(place, kind, dst)
    }

    /// Handler for [`Op::IssetStaticProp`].
    pub(in crate::exec) fn op_isset_static_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        empty: bool,
    ) -> VmResult<Flow> {
        let v = match self.isset_static_prop_fast(place, empty) {
            Some(v) => v,
            None => self.place_isset(place, empty)?,
        };
        self.set_tmp(dst, Value::Bool(v));
        Ok(Flow::Next)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(src: &str) -> String {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(src.as_bytes(), "t.php");
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    // Expected outputs are from `/usr/bin/php8.5 -n -d memory_limit=1G`.

    #[test]
    fn static_property_reads_writes_and_tests_in_loops() {
        // Each statement runs three times: the first run fills the cache
        // (generic path), the later ones hit it.
        let src = r#"<?php
class A {
    public static $n = 0;
    public static $s = 's';
    public static $f = 1.5;
    public static $nul = null;
    public static $arr = [1];
    private static $p = 'p';
    public static int $ti = 1;
    public static ?string $ts = null;
    public static string $str = 'T';
    static function run() {
        for ($i = 0; $i < 3; $i++) {
            $x = self::$n; self::$n = $x + 1;
            self::$n += 2; self::$f *= 2; self::$s .= $i; self::$s .= 'x';
            $p = self::$p; self::$p = $p . $i;
            $y = static::$nul ?? 'dflt';
            echo $x, ' ', self::$n, ' ', self::$f, ' ', self::$s, ' ', self::$p, ' ', $y, "\n";
            var_dump(isset(self::$n), isset(self::$nul), empty(self::$nul), empty(self::$arr), isset(self::$ts), empty(self::$s));
            self::$ti += 1; self::$str .= $i;
            echo self::$ti, ' ', self::$str, "\n";
        }
    }
}
A::run();
for ($i = 0; $i < 3; $i++) {
    $v = A::$n; A::$n = 'str'; A::$n .= $i; $w = (A::$n .= '!'); $z = (A::$s = 'r');
    echo $v, ' ', A::$n, ' ', $w, ' ', $z, ' ', A::$arr[0], "\n";
    A::$n = $i;
}
"#;
        let expected = "0 3 3 s0x p0 dflt\nbool(true)\nbool(false)\nbool(true)\nbool(false)\nbool(false)\nbool(false)\n2 T0\n3 6 6 s0x1x p01 dflt\nbool(true)\nbool(false)\nbool(true)\nbool(false)\nbool(false)\nbool(false)\n3 T01\n6 9 12 s0x1x2x p012 dflt\nbool(true)\nbool(false)\nbool(true)\nbool(false)\nbool(false)\nbool(false)\n4 T012\n9 str0! str0! r 1\n0 str1! str1! r 1\n1 str2! str2! r 1\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn static_properties_keep_their_generic_semantics() {
        // References, typed properties (coercion, uninitialized reads),
        // arrays, objects with destructors, conversions with warnings and
        // inaccessible properties, each after the cache was filled.
        let src = r#"<?php
class D { function __destruct() { echo "~D\n"; } }
class B {
    public static $r = 1;
    public static int $t = 1;
    public static int $u;
    public static $o = null;
    public static $a = 'a';
    protected static $prot = 1;
    static function show() { return self::$prot; }
}
for ($i = 0; $i < 2; $i++) {
    $x = B::$r; B::$r = $x;
    B::$t = 2; B::$a .= 'b'; B::$o = 1; $q = B::$o;
}
$ref = &B::$r; $ref = 5; echo B::$r, "\n";
B::$r = 6; echo $ref, "\n";
B::$r .= 'x'; echo $ref, "\n";
B::$t = "7"; var_dump(B::$t);
try { B::$t = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
B::$t += 1.5; var_dump(B::$t);
try { echo B::$u; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(isset(B::$u), empty(B::$u));
B::$o = new D; echo "set\n"; B::$o = 1; echo "replaced\n";
B::$a .= []; var_dump(B::$a);
B::$a = [1]; B::$a .= 'z'; var_dump(B::$a);
try { $p = B::$prot; } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo B::show(), "\n";
try { $m = B::$missing; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#;
        let out = run(src);
        let head = "5\n6\n6x\nint(7)\nCannot assign string to property B::$t of type int\n";
        assert!(out.starts_with(head), "{out}");
        assert!(
            out.contains("Deprecated: Implicit conversion from float 8.5 to int loses precision"),
            "{out}"
        );
        assert!(
            out.contains("int(8)\nTyped static property B::$u must not be accessed before initialization\nbool(false)\nbool(true)\nset\n~D\nreplaced\n"),
            "{out}"
        );
        assert!(out.contains("Warning: Array to string conversion"), "{out}");
        assert!(out.contains("string(8) \"abbArray\"\n"), "{out}");
        assert!(
            out.contains("string(6) \"Arrayz\"\nCannot access protected property B::$prot\n1\nAccess to undeclared static property B::$missing\n"),
            "{out}"
        );
    }

    #[test]
    fn test_fast_plain_static_property_cache() {
        let src = r#"<?php
class Foo {
    public static $count = 0;
    public static $prefix = 'item_';
    public static function test() {
        for ($i = 0; $i < 5; $i++) {
            self::$count++;
            self::$prefix .= $i;
            $x = self::$count;
            echo $x, ':', self::$prefix, "\n";
        }
    }
}
Foo::test();
for ($i = 0; $i < 3; $i++) {
    Foo::$count += 10;
    echo Foo::$count, "\n";
}
"#;
        let out = run(src);
        assert_eq!(
            out,
            "1:item_0\n2:item_01\n3:item_012\n4:item_0123\n5:item_01234\n15\n25\n35\n"
        );
    }

    #[test]
    fn static_property_increments_and_decrements() {
        // `++`/`--` on cached static properties: pre/post, int and float,
        // overflow to float, null, typed (overflow error), private,
        // `parent::`/`static::`, and a property bound to a reference, each
        // after the cache was filled.
        let src = r#"<?php
class A {
    public static $n = 0;
    public static $f = 1.5;
    public static $big = PHP_INT_MAX - 1;
    public static $nul = null;
    public static int $t = PHP_INT_MAX - 1;
    private static $p = 10;
    static function run() {
        for ($i = 0; $i < 3; $i++) {
            $a = self::$n++; $b = ++self::$n; $c = self::$n--; $d = --self::$f; $e = self::$f++;
            echo $a, ' ', $b, ' ', $c, ' ', $d, ' ', $e, ' ', self::$n, ' ', self::$f, "\n";
            var_dump(self::$big++); var_dump(self::$p--, static::$p);
            self::$nul++; var_dump(self::$nul); self::$nul = null;
            try { self::$t++; var_dump(self::$t); } catch (Error $x) { echo get_class($x), ': ', $x->getMessage(), "\n"; }
        }
    }
}
class B extends A {
    static function go() { for ($i = 0; $i < 2; $i++) { parent::$n++; static::$n++; echo A::$n, "\n"; } }
}
A::run();
B::go();
$r = &A::$n;
for ($i = 0; $i < 2; $i++) { A::$n++; echo $r, "\n"; }
for ($i = 0; $i < 2; $i++) { try { A::$p++; } catch (Error $x) { echo $x->getMessage(), "\n"; } }
"#;
        let expected = "0 2 2 0.5 0.5 1 1.5\nint(9223372036854775806)\nint(10)\nint(9)\nint(1)\nint(9223372036854775807)\n1 3 3 0.5 0.5 2 1.5\nint(9223372036854775807)\nint(9)\nint(8)\nint(1)\nTypeError: Cannot increment property A::$t of type int past its maximal value\n2 4 4 0.5 0.5 3 1.5\nfloat(9.223372036854776E+18)\nint(8)\nint(7)\nint(1)\nTypeError: Cannot increment property A::$t of type int past its maximal value\n5\n7\n8\n9\nCannot access private property A::$p\nCannot access private property A::$p\n";
        assert_eq!(run(src), expected);
    }
}
