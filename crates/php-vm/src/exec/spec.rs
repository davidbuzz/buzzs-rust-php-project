// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Fast paths of the specialised ops ([`Op::CmpJmp`], [`Op::BinAssignCv`],
//! [`Op::IncDecCv`], [`Op::AssignOpCv`], [`Op::FetchDimCv`],
//! [`Op::FetchDimsCv`], [`Op::IssetDimCv`], [`Op::AssignDimCv`],
//! [`Op::AssignOpDimCv`], [`Op::IncDecDimCv`]) and of the jump conditions:
//! the typed handlers php-src generates from the `SPEC(...)` annotations of
//! `Zend/zend_vm_def.h` (`ZEND_IS_SMALLER_LONG_SPEC`,
//! `ZEND_PRE_INC_LONG_SPEC_CV`, `ZEND_ASSIGN_OP_SPEC_CV_*`,
//! `ZEND_FETCH_DIM_R_SPEC_CV_*`, `ZEND_ISSET_ISEMPTY_DIM_OBJ_SPEC_CV_*`,
//! `ZEND_ASSIGN_DIM_SPEC_CV_*_OP_DATA_*`, `ZEND_ASSIGN_DIM_OP_SPEC_CV_*`,
//! `ZEND_JMPZ_SPEC_CV`).
//!
//! Each fast path either completes the op without any diagnostic, user
//! code or frame change (so the dispatch loop runs it on its local `ip`,
//! see `Vm::run`), or changes nothing and reports that the generic code
//! must run (which then sees exactly the state the generic op would have
//! seen). See `compiler/specialise.rs` for when the compiler selects these
//! ops. Variable slots are read in place through [`Frame::peek_cv`] /
//! [`Frame::peek_cv_dim_int`] where one slot is inspected, through a shared
//! borrow of the store where two slots or a chain of containers are
//! compared, and written through the store's `try_borrow_mut`, like the
//! `AssignCv` fast path (`Vm::assign_cv_fast`).
//!
//! [`Op::CmpJmp`]: crate::opcode::Op::CmpJmp
//! [`Op::BinAssignCv`]: crate::opcode::Op::BinAssignCv
//! [`Op::IncDecCv`]: crate::opcode::Op::IncDecCv
//! [`Op::AssignOpCv`]: crate::opcode::Op::AssignOpCv
//! [`Op::FetchDimCv`]: crate::opcode::Op::FetchDimCv
//! [`Op::FetchDimsCv`]: crate::opcode::Op::FetchDimsCv
//! [`Op::IssetDimCv`]: crate::opcode::Op::IssetDimCv
//! [`Op::AssignDimCv`]: crate::opcode::Op::AssignDimCv
//! [`Op::AssignOpDimCv`]: crate::opcode::Op::AssignOpDimCv
//! [`Op::IncDecDimCv`]: crate::opcode::Op::IncDecDimCv
//! [`Frame::peek_cv`]: crate::vm::Frame::peek_cv
//! [`Frame::peek_cv_dim_int`]: crate::vm::Frame::peek_cv_dim_int

use super::{discard_unrooted, num_bin, CvOpData, Num};
use crate::opcode::{cmp_outcome, BinKind, FetchMode, Operand, PlaceBase, PlaceId, Seg};
use crate::rtcache::GlobalSlot;
use crate::vm::{CvSlot, Frame, FrameKind, Vm, VmResult};
use php_runtime::{ArrayKey, BinaryOp, HashTable, Value, ZArray, ZStr};

mod casts;
mod dims;
mod dyncalls;
pub(in crate::exec) mod props;
mod refs;
mod static_dims;
mod statics;
mod strings;

pub(super) use casts::cast_in;
pub(super) use dims::{
    assign_dim_cv_in, assign_dims_cv_in, assign_op_dim_cv_in, assign_op_dims_cv_in, concat_cv_in,
    fe_assign_in, fe_fetch_array_in, fetch_dim_cv_in, fetch_dim_is_miss_in, fetch_dims_cv_in,
    fetch_dims_is_miss_in, fetch_tmp_dims_in, in_array_in, incdec_dim_cv_in, isset_cv_dims_in,
    isset_dim_cv_in, isset_tmp_dims_in, key_exists_in,
};
pub(super) use dyncalls::init_dyn_method_call_in;
pub(super) use props::{concat_prop_in, fetch_prop_dim_in, prop_op_in, read_hit};
pub(super) use refs::step_ref;
pub(super) use static_dims::{assign_static_dims_in, fetch_static_dims_in, isset_static_dims_in};
pub(super) use statics::{
    assign_static_prop_in, fetch_class_const_in, fetch_const_in, fetch_static_prop_in,
    incdec_static_prop_in, isset_static_prop_in,
};
pub(super) use strings::{cast_string_in, concat_in, rope_in};

/// Whether replacing `v` needs no destructor, GC root buffering or
/// reference semantics: no value, or null, bool, int, float or string
/// (`!Z_REFCOUNTED` in php-src, plus strings, whose release has no side
/// effect).
#[inline]
pub(super) fn is_plain_slot(v: Option<&Value>) -> bool {
    matches!(
        v,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_))
    )
}

/// Whether `v` is a scalar or string value that can be stored into an
/// array element without the generic write walk.
#[inline(always)]
fn is_plain_value(v: &Value) -> bool {
    v.is_plain()
}

/// A copy of `v` (`ZVAL_COPY`): scalars are copied inline, without the
/// out-of-line `Clone` call the other kinds (a refcount increment) take.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test and a copy on the hot element reads
fn copy_value(v: &Value) -> Value {
    match *v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(b),
        Value::Int(i) => Value::Int(i),
        Value::Float(x) => Value::Float(x),
        _ => v.clone(),
    }
}

/// Releases a value taken out of an array element or variable slot that
/// held a plain value (or any value no longer referenced elsewhere):
/// scalars own nothing and are forgotten, skipping the out-of-line
/// `drop_in_place` call; anything else is dropped.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test on the hot element writes
fn discard_plain(old: Value) {
    if matches!(
        old,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        std::mem::forget(old);
    } else {
        drop(old);
    }
}

/// The array key an int or string value converts to without diagnostics
/// (symbol-table semantics: `"1"` is key 1).
#[inline]
fn plain_key(v: Option<&Value>) -> Option<ArrayKey> {
    match v {
        Some(Value::Int(i)) => Some(ArrayKey::Int(*i)),
        Some(Value::Str(s)) => Some(ArrayKey::from_zstr(s)),
        _ => None,
    }
}

/// The one-byte string at int offset `offset` of `s` (a negative offset
/// counts from the end): `$str[$i]` for an offset inside the string, which
/// php-src answers with the interned `ZSTR_CHAR` and no diagnostic. `None`
/// outside the string (the generic fetch warns, or yields null in IS mode).
#[inline]
fn string_offset_char(s: &ZStr, offset: i64) -> Option<Value> {
    let len = i64::try_from(s.len()).ok()?;
    let real = if offset < 0 {
        offset.checked_add(len)?
    } else {
        offset
    };
    let idx = usize::try_from(real).ok()?;
    s.as_bytes()
        .get(idx)
        .map(|&c| Value::Str(ZStr::from_byte(c)))
}

/// The value of an operand for a comparison without conversions: a
/// defined compiled variable, a literal or a temporary (left in place), not
/// holding a reference. A consumed temporary (`Tmp`) qualifies only when it
/// holds a plain value, which [`Vm::release_tmp_str`] can release: an
/// array, object or resource in it must be freed by the generic op's read
/// (`FREE_OP1`/`FREE_OP2` of `ZEND_IS_IDENTICAL` & co.), with the
/// destructor that may run, so it does not outlive its statement in the
/// slot (`$weak->get() !== null`, `new D !== null`).
#[inline]
pub(super) fn operand_simple(f: &Frame, op: Operand) -> Option<&Value> {
    let v = match op {
        Operand::Cv(cv) => f.cvs.get(cv as usize).and_then(CvSlot::get)?,
        Operand::Lit(i) => f.code.literals.get(i as usize)?,
        Operand::Tmp(t) => {
            let v = f.tmps.get(t as usize)?;
            return is_plain_value(v).then_some(v);
        }
        Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Unused => return None,
    };
    match v {
        Value::Reference(_) => None,
        v => Some(v),
    }
}

/// `zend_is_true` of `v` when it needs neither a diagnostic nor a copy:
/// null, bool, int, a non-NaN float, a string, an array or an object
/// whose class has no cast handler ([`object_truth`]). References, other
/// objects and resources give `None`.
#[inline(always)]
#[allow(clippy::inline_always)] // hot JmpZ/JmpNz truth check in the dispatch pre-match
pub(super) fn plain_truth(v: &Value) -> Option<bool> {
    match v {
        Value::Null => Some(false),
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Float(x) if !x.is_nan() => Some(*x != 0.0),
        Value::Str(s) => Some(!(s.is_empty() || s.as_bytes() == b"0")),
        Value::Array(a) => Some(!a.is_empty()),
        Value::Object(o) => object_truth(o),
        _ => None,
    }
}

/// `zend_is_true` of an object: `true` unless its class casts objects
/// itself (`cast_object`, as `SimpleXML` and GMP do: `None`, the generic
/// path asks it). Out of line: the inlined truth tests stay small.
#[inline(never)]
fn object_truth(o: &php_runtime::Obj) -> Option<bool> {
    crate::object::class_of(o)
        .hooks
        .as_ref()
        .is_none_or(|h| h.cast.is_none())
        .then_some(true)
}

/// [`plain_truth`] of a variable slot (`None` = undefined: generic path).
#[inline(always)]
#[allow(clippy::inline_always)] // hot JmpZ/JmpNz CV truth check in the dispatch pre-match
pub(super) fn slot_truth(v: Option<&Value>) -> Option<bool> {
    v.and_then(plain_truth)
}

/// The outcome of comparing the plain numbers `x` and `y`, as one
/// [`cmp_outcome`] bit: two ints compare as ints, an int and a float as
/// doubles (php-src's `(double)` widening) unless `identity` (`===`,
/// `!==`), where they are a type mismatch; a NaN is unordered.
#[inline(always)]
#[allow(clippy::inline_always, clippy::float_cmp)] // the smart branch's test; PHP compares doubles exactly
pub(super) fn num_outcome(x: Num, y: Num, identity: bool) -> u8 {
    let (xf, yf) = match (x, y) {
        // 0, 1 or 2: LESS, EQUAL or GREATER.
        (Num::Int(p), Num::Int(q)) => return 1 << (u8::from(p > q) + u8::from(p >= q)),
        (Num::Float(p), Num::Float(q)) => (p, q),
        _ if identity => return cmp_outcome::MISMATCH,
        _ => (x.as_f64(), y.as_f64()),
    };
    if xf < yf {
        cmp_outcome::LESS
    } else if xf > yf {
        cmp_outcome::GREATER
    } else if xf == yf {
        cmp_outcome::EQUAL
    } else {
        cmp_outcome::UNORDERED
    }
}

/// Stores `v` into the temporary slot `slot` as [`Vm::set_tmp`] does: an
/// old scalar (or the `Null` a consuming read left) is overwritten without
/// the out-of-line drop call.
#[inline(always)]
#[allow(clippy::inline_always)] // a store and a tag test
pub(super) fn put_tmp(slot: &mut Value, v: Value) {
    let old = std::mem::replace(slot, v);
    if matches!(
        old,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        std::mem::forget(old);
    }
}

/// The 64-bit integer index an offset operand of frame `f` holds directly,
/// without diagnostics or string conversions.
#[inline(always)]
#[allow(clippy::inline_always)]
fn dim_int_in(f: &Frame, dim: Operand) -> Option<i64> {
    match dim {
        Operand::Cv(cv) => match f.cvs.get(cv as usize).and_then(CvSlot::get) {
            Some(Value::Int(i)) => Some(*i),
            Some(Value::Reference(r)) => match &*r.try_borrow()? {
                Value::Int(i) => Some(*i),
                _ => None,
            },
            _ => None,
        },
        Operand::Tmp(t) | Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        },
        Operand::Lit(i) => match f.code.literals.get(i as usize) {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        },
        Operand::Unused => None,
    }
}

/// The array key an offset operand of frame `f` converts to without
/// diagnostics: an int, or a string (symbol-table semantics, `"1"` is key
/// 1). Nothing is consumed: an int in a temporary can stay in its slot
/// (like [`Vm::peek_num`]); a string there is cloned into the key and the
/// caller releases the temporary once its fast path succeeded
/// ([`release_str_tmp_in`], the `FREE_OP2` of the dimension fetch), or
/// leaves it to the generic read.
#[inline(always)]
#[allow(clippy::inline_always)] // hot dimension-key extraction in the dispatch pre-match
fn dim_key_in(f: &Frame, dim: Operand) -> Option<ArrayKey> {
    match dim {
        Operand::Tmp(t) | Operand::TmpKeep(t) => plain_key(f.tmps.get(t as usize)),
        Operand::Lit(i) => plain_key(f.code.literals.get(i as usize)),
        Operand::Cv(cv) => f.peek_cv(cv, plain_key),
        Operand::Unused => None,
    }
}

/// An int or string offset operand, borrowed where it lives (a literal, a
/// temporary or a compiled variable; a reference is not looked through).
#[derive(Clone, Copy)]
enum DimRef<'a> {
    Int(i64),
    Str(&'a ZStr),
}

/// The offset operand `dim` of frame `f` as a [`DimRef`] when it holds an
/// int or a string: the keys [`dim_key_in`] accepts, without cloning the
/// string into an [`ArrayKey`].
#[inline(always)]
#[allow(clippy::inline_always)] // hot offset peek of the array-dimension fast paths
fn dim_ref_in(f: &Frame, dim: Operand) -> Option<DimRef<'_>> {
    let v = match dim {
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Lit(i) => f.code.literals.get(i as usize)?,
        Operand::Cv(cv) => f.cvs.get(cv as usize).and_then(CvSlot::get)?,
        Operand::Unused => return None,
    };
    match v {
        Value::Int(i) => Some(DimRef::Int(*i)),
        Value::Str(s) => Some(DimRef::Str(s)),
        _ => None,
    }
}

/// The element of `arr` at offset `d` (`zend_fetch_dimension_address_inner`'s
/// lookup: `zend_hash_index_find`, or `zend_symtable_find` for a string).
#[inline(always)]
#[allow(clippy::inline_always)] // hot element lookup of the array-dimension fast paths
fn table_dim<'t>(arr: &'t HashTable, d: DimRef<'_>) -> Option<&'t Value> {
    match d {
        // A packed table indexes its values directly (inlined here).
        DimRef::Int(i) if arr.is_packed() => arr.get_packed(i),
        DimRef::Int(i) => arr.get_int(i),
        DimRef::Str(s) => arr.get_sym_zstr(s),
    }
}

/// Mutable [`table_dim`].
#[inline(always)]
#[allow(clippy::inline_always)] // hot element lookup of the array-dimension fast paths
fn table_dim_mut<'t>(arr: &'t mut HashTable, d: DimRef<'_>) -> Option<&'t mut Value> {
    match d {
        DimRef::Int(i) if arr.is_packed() => arr.get_packed_mut(i),
        DimRef::Int(i) => arr.get_int_mut(i),
        DimRef::Str(s) => arr.get_sym_zstr_mut(s),
    }
}

/// `$cv[dim]` of frame `f` when `$cv` holds an array (directly or through
/// a reference) that has the int or string offset `dim` peeks as: the
/// element, dereferenced and cloned (`ZEND_FETCH_DIM_R` on a hit). `None`
/// leaves the operation to the caller's slower path, nothing consumed.
#[inline(always)]
#[allow(clippy::inline_always)] // hot array-dimension read in the dispatch pre-match
fn cv_dim_hit_in(f: &Frame, cv: u32, dim: Operand) -> Option<Value> {
    let d = dim_ref_in(f, dim)?;
    let slot = f.cvs.get(cv as usize).and_then(CvSlot::get)?;
    let guard;
    let arr = match slot {
        Value::Array(a) => a,
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            match &*guard {
                Value::Array(a) => a,
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(match table_dim(arr, d)? {
        Value::Reference(r) => r.get(),
        v => copy_value(v),
    })
}

/// Releases the string a consumed temporary operand of frame `f` holds;
/// other temporaries are left in place (see [`Vm::peek_num`]).
#[inline(always)]
#[allow(clippy::inline_always)] // hot FREE_OP2 string-temp release in the dispatch pre-match
pub(super) fn release_str_tmp_in(f: &mut Frame, op: Operand) {
    if let Operand::Tmp(t) = op {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            if matches!(slot, Value::Str(_)) {
                *slot = Value::Null;
            }
        }
    }
}

/// Stores a fetched value `v` (never a reference) into compiled variable
/// `to` of frame `f` when that is undefined or holds a plain value (the
/// folded `AssignCv`: `zend_assign_to_variable` for a variable that is
/// neither a reference nor refcounted; `true`, the loop skips that op),
/// else into temporary `dst` (`false`).
#[inline(always)]
#[allow(clippy::inline_always)] // hot folded AssignCv store in the dispatch pre-match
pub(in crate::exec) fn store_fetched_in(
    f: &mut Frame,
    dst: u32,
    to: Option<u32>,
    v: Value,
) -> bool {
    if let Some(to) = to {
        let idx = to as usize;
        if f.cvs.get(idx).is_some_and(|slot| is_plain_slot(slot.get())) {
            // The table's mirror first (it only touches the table), so
            // the value moves into the slot without a clone.
            f.sync_cv_to_symtab(to, &v);
            let old = f.cvs[idx].set(Some(v));
            // A scalar is forgotten; a string is released.
            discard_unrooted(old);
            return true;
        }
    }
    put_tmp(&mut f.tmps[dst as usize], v);
    false
}

/// The value of `$cv[d1][d2]...` (place `place` of frame `f`) when every
/// container on the way is an array (the variable itself possibly through
/// a reference) holding the offset and every offset peeks as an int or a
/// string ([`dim_ref_in`]); dereferenced and cloned.
#[inline]
fn dims_value_in(f: &Frame, cv: u32, place: PlaceId) -> Option<Value> {
    let segs = &f.code.places.get(place as usize)?.segs;
    let slot = f.cvs.get(cv as usize).and_then(CvSlot::get)?;
    let guard;
    let mut cur = match slot {
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            &*guard
        }
        v => v,
    };
    for seg in segs {
        let Seg::Dim(dim) = seg else {
            return None;
        };
        let key = dim_ref_in(f, *dim)?;
        let Value::Array(a) = cur else {
            return None;
        };
        cur = table_dim(a, key)?;
    }
    Some(match cur {
        Value::Reference(r) => Value::clone(&*r.try_borrow()?),
        v => v.clone(),
    })
}

/// The value of `cur[d1]...[dn]` along the dimension segments `segs` of
/// frame `f` (`segs` not empty) when every container is an array holding
/// the int or string offset its operand peeks as, the last element
/// dereferenced and copied: [`dims_value_in`] from a value instead of a
/// variable. `None` for anything else (`cur` itself is not looked
/// through a reference).
#[inline]
fn dims_value_of(f: &Frame, cur: &Value, segs: &[Seg]) -> Option<Value> {
    let mut cur = cur;
    for seg in segs {
        let Seg::Dim(dim) = seg else {
            return None;
        };
        let key = dim_ref_in(f, *dim)?;
        let Value::Array(a) = cur else {
            return None;
        };
        cur = table_dim(a, key)?;
    }
    Some(match cur {
        Value::Reference(r) => copy_value(&*r.try_borrow()?),
        v => copy_value(v),
    })
}

/// The shapes of an argument place [`Frame::send_place_hit`] reads.
#[derive(Clone, Copy)]
enum ArgPlace {
    /// `$cv[d1]...[dn]`: only dimensions.
    CvDims(u32),
    /// `$this->name` / `$cv->name`, a literal name.
    Prop,
    /// `$this->name[d1]...[dn]` / `$cv->name[...]`.
    PropDims,
    /// A place ending in `->name` after other segments (`$this->a->b`,
    /// `$list[$k]->name`).
    Chain,
    /// `tmp[d1]...[dn]` (`f()['k']`).
    TmpDims(u32),
    /// `A::$name`, `static::$name`, `self::$name`: a static property named
    /// by a literal.
    Static,
}

impl ArgPlace {
    /// The shape of place `pid` of frame `f`, `None` for one no hit path
    /// reads (a plain variable is the caller's).
    #[inline]
    fn of(f: &Frame, pid: PlaceId) -> Option<Self> {
        let p = f.code.places.get(pid as usize)?;
        let all_dims = |segs: &[Seg]| segs.iter().all(|s| matches!(s, Seg::Dim(_)));
        let cv_base = matches!(p.base, PlaceBase::Cv(_));
        // A variable of a frame with an attached symbol table is left to
        // the walk, as the plain variable sends leave it (`send_in`).
        if cv_base && f.symtab_attached {
            return None;
        }
        Some(match (&p.base, p.segs.as_slice()) {
            (PlaceBase::Cv(cv), segs) if !segs.is_empty() && all_dims(segs) => {
                ArgPlace::CvDims(*cv)
            }
            (PlaceBase::Value(Operand::Tmp(t)), segs) if !segs.is_empty() && all_dims(segs) => {
                ArgPlace::TmpDims(*t)
            }
            (PlaceBase::Cv(_) | PlaceBase::This, [Seg::Prop(Operand::Lit(_))]) => ArgPlace::Prop,
            (PlaceBase::Cv(_) | PlaceBase::This, [Seg::Prop(Operand::Lit(_)), rest @ ..])
                if all_dims(rest) =>
            {
                ArgPlace::PropDims
            }
            (PlaceBase::Cv(_) | PlaceBase::This, [_, .., Seg::Prop(Operand::Lit(_))]) => {
                ArgPlace::Chain
            }
            (
                PlaceBase::StaticProp {
                    name: Operand::Lit(_),
                    ..
                },
                [],
            ) => ArgPlace::Static,
            _ => return None,
        })
    }
}

impl Frame {
    /// The value of place `pid` of this (user) frame sent by value as an
    /// argument (`ZEND_SEND_VAR` of what `ZEND_FETCH_DIM_R` /
    /// `ZEND_FETCH_OBJ_R`, or their `_FUNC_ARG` forms for a by-value
    /// parameter, fetched: `zend_compile_args` in `Zend/zend_compile.c`),
    /// on the hits the specialised fetch ops take: `$cv[d1]...[dn]` with
    /// every key present ([`dims_value_in`]), `$this->name` / `$cv->name`
    /// on a property cache hit (`props::read_hit`), `$this->name[...]`
    /// (`props::read_prop_dims_in`), `$this->a->b` / `$list[$k]->name`
    /// with their cache entries filled (`props::read_chain_in`), and
    /// `f()[d1]...` on a temporary array another handle shares (so that
    /// releasing the temporary, as `FREE_OP1` does, frees nothing).
    /// Consumed string offsets and the temporary base are released. `None`,
    /// having changed nothing, for anything else (an undefined variable,
    /// property or key and its diagnostic, `__get`, `ArrayAccess`, a
    /// string offset, a hooked or lazy property, a cache not filled yet):
    /// the generic place walk (`Vm::place_fetch`) then reads it.
    #[inline(never)]
    pub(crate) fn send_place_hit(&mut self, pid: PlaceId) -> Option<Value> {
        if self.kind == FrameKind::Builtin {
            return None;
        }
        match ArgPlace::of(self, pid)? {
            ArgPlace::CvDims(cv) => {
                let v = dims_value_in(self, cv, pid)?;
                release_place_str_tmps(self, pid);
                Some(v)
            }
            ArgPlace::Prop => props::read_hit(self, pid),
            ArgPlace::PropDims => props::read_prop_dims_in(self, pid),
            ArgPlace::Chain => props::read_chain_in(self, pid),
            ArgPlace::Static => statics::static_prop_value_in(self, pid),
            ArgPlace::TmpDims(t) => {
                let v = {
                    let Value::Array(a) = self.tmps.get(t as usize)? else {
                        return None;
                    };
                    if a.refcount() < 2 {
                        return None;
                    }
                    let segs = &self.code.places.get(pid as usize)?.segs;
                    dims_value_of(self, &self.tmps[t as usize], segs)?
                };
                release_place_str_tmps(self, pid);
                // Another handle holds the array: this release only
                // drops a count.
                drop(std::mem::take(&mut self.tmps[t as usize]));
                Some(v)
            }
        }
    }
}

/// A plain value (see [`is_plain_value`]) an `OP_DATA` operand of frame
/// `f` holds, cloned, without consuming it: a literal, a defined
/// non-reference variable or a temporary.
#[inline(always)]
#[allow(clippy::inline_always)] // hot OP_DATA operand read for AssignDimCv
fn plain_value_in(f: &Frame, op: Operand) -> Option<Value> {
    match op {
        Operand::Tmp(t) | Operand::TmpKeep(t) => {
            let v = f.tmps.get(t as usize)?;
            is_plain_value(v).then(|| copy_value(v))
        }
        Operand::Lit(i) => {
            let v = f.code.literals.get(i as usize)?;
            is_plain_value(v).then(|| copy_value(v))
        }
        Operand::Cv(cv) => f.peek_cv(cv, |s| match s {
            Some(v) if is_plain_value(v) => Some(copy_value(v)),
            _ => None,
        }),
        Operand::Unused => None,
    }
}

/// [`plain_value_in`], and an array or an object as well (a handle added:
/// `ZVAL_COPY` of the `OP_DATA` operand of `ZEND_ASSIGN_DIM`): the value
/// an element write stores, which, unlike a plain value read elsewhere,
/// needs no release where it is read.
#[inline(always)]
#[allow(clippy::inline_always)] // hot OP_DATA operand read for AssignDimCv
pub(super) fn dim_value_in(f: &Frame, op: Operand) -> Option<Value> {
    let v = match op {
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Cv(cv) => f.cvs.get(cv as usize).and_then(CvSlot::get)?,
        Operand::Lit(_) | Operand::Unused => return plain_value_in(f, op),
    };
    match v {
        Value::Array(_) | Value::Object(_) => Some(v.clone()),
        _ => plain_value_in(f, op),
    }
}

/// The deepest dimension chain of [`Op::AssignDimsCv`] /
/// [`Op::AssignOpDimsCv`] (the compiler's `MAX_WRITE_DIMS`).
///
/// [`Op::AssignDimsCv`]: crate::opcode::Op::AssignDimsCv
/// [`Op::AssignOpDimsCv`]: crate::opcode::Op::AssignOpDimsCv
const MAX_WRITE_DIMS: usize = 4;

/// The keys of the dimension chain `place` of frame `f` (and how many):
/// every segment a dimension whose offset peeks as an int or a string
/// ([`dim_key_in`]), at most [`MAX_WRITE_DIMS`] of them. All are computed
/// before a write walk changes anything.
fn write_keys_in(f: &Frame, place: PlaceId) -> Option<([Option<ArrayKey>; MAX_WRITE_DIMS], usize)> {
    let segs = &f.code.places.get(place as usize)?.segs;
    if segs.len() > MAX_WRITE_DIMS {
        return None;
    }
    let mut keys: [Option<ArrayKey>; MAX_WRITE_DIMS] = Default::default();
    for (key, seg) in keys.iter_mut().zip(segs) {
        let Seg::Dim(dim) = seg else {
            return None;
        };
        *key = Some(dim_key_in(f, *dim)?);
    }
    Some((keys, segs.len()))
}

/// Releases the consumed string temporaries among the offsets of `place`
/// of frame `f` (see [`release_str_tmp_in`]).
fn release_place_str_tmps(f: &mut Frame, place: PlaceId) {
    let Some(p) = f.code.places.get(place as usize) else {
        return;
    };
    for seg in &p.segs {
        if let Seg::Dim(Operand::Tmp(t)) = seg {
            if let Some(slot) = f.tmps.get_mut(*t as usize) {
                if matches!(slot, Value::Str(_)) {
                    *slot = Value::Null;
                }
            }
        }
    }
}

/// `tbl[k1][k2]... = v` along `keys` (all `Some`): the `FETCH_DIM_W`
/// chain, separating each nested array (`SEPARATE_ARRAY`) and turning a
/// missing or null container element into an empty array (as the generic
/// walk's `descend` does, silently), then the `ASSIGN_DIM` store of `v`
/// over a plain element or none. The old element on success; `v` back
/// otherwise: the walk then met an existing element of another kind
/// (reference, object, string, `false`, ...), having separated arrays on
/// the way, which the generic write repeats as a no-op, and created
/// nothing (a created array has no elements to stop at).
fn assign_dims(tbl: &mut HashTable, keys: &[Option<ArrayKey>], v: Value) -> Result<Value, Value> {
    let Some((Some(key), rest)) = keys.split_first() else {
        return Err(v);
    };
    if rest.is_empty() {
        return match tbl.get_mut(key) {
            Some(slot) if is_plain_value(slot) => Ok(std::mem::replace(slot, v)),
            Some(_) => Err(v),
            None => Ok(std::mem::replace(tbl.lookup_ex(key.clone()).0, v)),
        };
    }
    let (elem, _) = tbl.lookup_ex(key.clone());
    if matches!(elem, Value::Null) {
        *elem = Value::Array(ZArray::new());
    }
    let Value::Array(inner) = elem else {
        return Err(v);
    };
    assign_dims(inner.make_mut(), rest, v)
}

/// `tbl[k1][k2]... op= y` along `keys` (all `Some`) when every container
/// exists and is an array and the element is a plain number, the result
/// staying in the int domain or being a float (the `FETCH_DIM_RW` hits,
/// each array separated, and `ZEND_ASSIGN_DIM_OP`'s numeric fast path).
/// `false` otherwise, the walk having at most separated arrays (a missing
/// key warns on the generic path).
fn assign_op_dims(tbl: &mut HashTable, keys: &[Option<ArrayKey>], op: BinaryOp, y: Num) -> bool {
    let Some((Some(key), rest)) = keys.split_first() else {
        return false;
    };
    let Some(elem) = tbl.get_mut(key) else {
        return false;
    };
    if rest.is_empty() {
        let x = match *elem {
            Value::Int(i) => Num::Int(i),
            Value::Float(v) => Num::Float(v),
            _ => return false,
        };
        let Some(r) = num_bin(BinKind::Arith(op), x, y) else {
            return false;
        };
        // A number replaces a number: no drop glue to run.
        std::mem::forget(std::mem::replace(elem, r));
        return true;
    }
    let Value::Array(inner) = elem else {
        return false;
    };
    assign_op_dims(inner.make_mut(), rest, op, y)
}

fn dims_has_str(arr: &ZArray, keys: &[Option<ArrayKey>]) -> bool {
    let Some((Some(key), rest)) = keys.split_first() else {
        return false;
    };
    match (rest.is_empty(), arr.get(key)) {
        (true, Some(Value::Str(_))) => true,
        (false, Some(Value::Array(inner))) => dims_has_str(inner, rest),
        _ => false,
    }
}

fn concat_dims(tbl: &mut HashTable, keys: &[Option<ArrayKey>], add: &Value) -> bool {
    let Some((Some(key), rest)) = keys.split_first() else {
        return false;
    };
    let Some(elem) = tbl.get_mut(key) else {
        return false;
    };
    if rest.is_empty() {
        let Value::Str(s) = elem else {
            return false;
        };
        match add {
            Value::Str(t) => s.append_str(t),
            Value::Int(i) => s.append_long(*i),
            _ => return false,
        }
        return true;
    }
    let Value::Array(inner) = elem else {
        return false;
    };
    concat_dims(inner.make_mut(), rest, add)
}

/// `isset($cv[dim])` (`empty` false) or `empty($cv[dim])` of frame `f`
/// when `$cv` holds an array (directly or through a reference) and the
/// offset is an int or string; `None` for the generic path.
#[inline]
fn isset_dim_in(f: &Frame, cv: u32, dim: Operand, empty: bool) -> Option<bool> {
    let key = dim_ref_in(f, dim)?;
    let slot = f.cvs.get(cv as usize).and_then(CvSlot::get)?;
    let guard;
    let arr = match slot {
        Value::Array(a) => a,
        // `isset($s[$i])` of a string with an int offset
        // (`zend_isset_dim_slow`'s `str_offset`: a negative offset counts
        // from the end). `empty()` and other offsets: the generic op.
        Value::Str(s) if !empty => {
            let DimRef::Int(i) = key else {
                return None;
            };
            let len = i64::try_from(s.len()).ok()?;
            let at = if i < 0 { i + len } else { i };
            return Some((0..len).contains(&at));
        }
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            match &*guard {
                Value::Array(a) => a,
                _ => return None,
            }
        }
        _ => return None,
    };
    let Some(v) = table_dim(arr, key) else {
        return Some(empty);
    };
    if empty {
        return plain_truth(v).map(|t| !t);
    }
    Some(match v {
        Value::Reference(r) => !r.try_borrow()?.is_null(),
        v => !v.is_null(),
    })
}

/// [`Vm::cmp_simple_fast`] on frame `f`: `==`, `!=`, `===`, `!==` without
/// conversions on operands that are not both numbers. `None`, nothing
/// changed, for the generic comparison.
#[inline(never)]
#[allow(clippy::many_single_char_names)]
pub(super) fn cmp_simple_in(f: &mut Frame, op: BinKind, a: Operand, b: Operand) -> Option<bool> {
    let identity = match op {
        BinKind::Identical | BinKind::NotIdentical => true,
        BinKind::Equal | BinKind::NotEqual => false,
        _ => return None,
    };
    let negate = matches!(op, BinKind::NotIdentical | BinKind::NotEqual);
    let same = {
        let lhs = cmp_operand(f, a)?;
        let rhs = cmp_operand(f, b)?;
        match (lhs, rhs) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(p), Value::Bool(q)) => p == q,
            // `$x !== []`: two arrays one of which is empty are equal,
            // and identical, exactly when both are.
            (Value::Array(x), Value::Array(y)) if x.is_empty() || y.is_empty() => {
                x.is_empty() && y.is_empty()
            }
            // `===` of two objects (enum cases): the same object.
            (Value::Object(x), Value::Object(y)) if identity => php_runtime::Obj::ptr_eq(x, y),
            // `===` compares the bytes; `==` is the generic comparison's
            // `zendi_smart_streq` (numeric strings compare as numbers),
            // which needs no diagnostic.
            (Value::Str(s), Value::Str(t)) => {
                ZStr::ptr_eq(s, t)
                    || if identity {
                        s.as_bytes() == t.as_bytes()
                    } else {
                        php_runtime::ops::smart_streq(s.as_bytes(), t.as_bytes())
                    }
            }
            (lhs, rhs) => {
                if !identity || std::mem::discriminant(lhs) == std::mem::discriminant(rhs) {
                    return None;
                }
                false
            }
        }
    };
    release_cmp_tmp(f, a);
    release_cmp_tmp(f, b);
    Some(same != negate)
}

/// [`operand_simple`] for [`cmp_simple_in`], which also takes a consumed
/// temporary holding an array whose release frees nothing it holds: one
/// shared with another holder (a property or variable just read) or an
/// empty one ([`release_cmp_tmp`] releases it).
#[inline]
fn cmp_operand(f: &Frame, op: Operand) -> Option<&Value> {
    if let Operand::Tmp(t) = op {
        match f.tmps.get(t as usize) {
            Some(v @ Value::Array(a)) => return (a.refcount() > 1 || a.is_empty()).then_some(v),
            // Another holder keeps it alive: its release runs no destructor.
            Some(v @ Value::Object(o)) => return (o.refcount() > 1).then_some(v),
            _ => {}
        }
    }
    operand_simple(f, op)
}

/// Releases what a consumed temporary operand of [`cmp_simple_in`] holds:
/// a string ([`release_str_tmp_in`]) or an array [`cmp_operand`] took.
#[inline]
fn release_cmp_tmp(f: &mut Frame, op: Operand) {
    if let Operand::Tmp(t) = op {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            if matches!(slot, Value::Str(_) | Value::Array(_) | Value::Object(_)) {
                *slot = Value::Null;
            }
        }
    }
}

impl Vm {
    /// [`plain_truth`] of compiled variable `cv` in place (the type switch
    /// inlined into `ZEND_JMPZ_SPEC_CV`/`ZEND_JMPNZ_SPEC_CV`); undefined
    /// variables go generic.
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot JmpZ/JmpNz CV truth check in the dispatch pre-match
    pub(super) fn peek_cv_bool(&self, cv: u32) -> Option<bool> {
        self.frame().peek_cv(cv, slot_truth)
    }

    /// `==`, `!=`, `===`, `!==` on operands that are not both numbers,
    /// without conversions (`zend_is_identical`, `zend_fast_equal_strings`):
    /// same-type null, bool and string pairs, two arrays one of which is
    /// empty, and `===`/`!==` of two different types (false/true). Other
    /// arrays, objects, resources and references go generic (also when
    /// only one operand is one of those and it sits in a consumed
    /// temporary, see [`operand_simple`]). A consumed temporary holding a
    /// string is released, as the generic read would (`FREE_OP1`/`FREE_OP2`).
    #[inline(always)]
    #[allow(clippy::inline_always)] // a frame look and the out-of-line comparison
    pub(super) fn cmp_simple_fast(&mut self, op: BinKind, a: Operand, b: Operand) -> Option<bool> {
        cmp_simple_in(self.frames.last_mut()?, op, a, b)
    }

    /// Releases the string a consumed temporary operand holds; other
    /// temporaries are left in place (see [`Vm::peek_num`]).
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot FREE_OP string-temp release in the dispatch pre-match
    pub(super) fn release_tmp_str(&mut self, op: Operand) {
        release_str_tmp_in(self.frame_mut(), op);
    }

    /// Stores `v` (never a reference) into compiled variable `cv` when the
    /// variable is undefined or holds a plain value (see
    /// [`is_plain_slot`]): the `zend_assign_to_variable` path for a
    /// variable that is neither a reference nor refcounted. Gives `v` back
    /// otherwise, having changed nothing, for the generic assignment.
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot plain CV write in the dispatch pre-match
    pub(super) fn cv_store_plain(&mut self, cv: u32, v: Value) -> Result<(), Value> {
        let f = self.frame_mut();
        let idx = cv as usize;
        let Some(slot) = f.cvs.get_mut(idx) else {
            return Err(v);
        };
        if !is_plain_slot(slot.get()) {
            return Err(v);
        }
        if let (Some(Value::Float(dst_f)), Value::Float(rf)) = (slot.as_mut(), &v) {
            *dst_f = *rf;
            f.sync_cv_to_symtab(cv, &v);
            return Ok(());
        }
        let old = slot.set(Some(v.clone()));
        f.sync_cv_to_symtab(cv, &v);
        // A scalar is forgotten; a string is released (no side effects).
        discard_unrooted(old);
        Ok(())
    }

    /// Stores a fetched value: into CV `to` when that holds no refcounted
    /// value (the folded `AssignCv`; `true`: the loop skips it), else into
    /// `dst` ([`store_fetched_in`]).
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot folded AssignCv store in the dispatch pre-match
    pub(super) fn store_fetched(&mut self, dst: u32, to: Option<u32>, v: Value) -> bool {
        store_fetched_in(self.frame_mut(), dst, to, v)
    }

    /// Fast path for [`crate::opcode::Op::Abs`]: computes `abs` of an int or float operand inline.
    #[inline(always)]
    #[allow(clippy::inline_always)]
    pub(super) fn abs_fast(&mut self, src: Operand) -> Option<Value> {
        let f = self.frames.last_mut()?;
        match Self::peek_num(f, src)? {
            Num::Int(i) => Some(if i == i64::MIN {
                #[allow(clippy::cast_precision_loss)]
                Value::Float(-(i as f64))
            } else {
                Value::Int(i.abs())
            }),
            Num::Float(fl) => Some(Value::Float(fl.abs())),
        }
    }

    /// The array key an offset operand converts to without diagnostics
    /// ([`dim_key_in`] of the current frame).
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot dimension-key extraction in the dispatch pre-match
    pub(super) fn peek_dim_key(&self, dim: Operand) -> Option<ArrayKey> {
        dim_key_in(self.frame(), dim)
    }

    /// The run-time cache hit of [`Vm::fetch_global_fast`]: `Some(result)`
    /// when place `pid`'s slot was resolved before (only a literal name
    /// fills it), read from the global scope's variable or the global
    /// store without looking at the place again; `None` when unresolved.
    #[inline(always)]
    #[allow(clippy::inline_always, clippy::option_option)] // the FetchGlobal hit
    fn fetch_global_cached(&self, pid: PlaceId, mode: FetchMode) -> Option<Option<Value>> {
        let slot = *self
            .frame()
            .code
            .rt_cache
            .globals
            .try_borrow()
            .ok()?
            .get(pid as usize)?;
        let undefined = || (mode == FetchMode::Is).then_some(Value::Null);
        match slot {
            GlobalSlot::GlobalCv(cv) => {
                // The main frame's variable is the global's value only
                // while no top-level include or `eval` runs above it.
                if !self.main_is_global_top() {
                    return None;
                }
                let main = self.frames.first()?;
                let slot = main.cvs.get(cv as usize)?;
                Some(slot.get().map_or_else(undefined, |v| Some(v.deref())))
            }
            GlobalSlot::GlobalStore(idx) => {
                let g = self.globals.try_borrow().ok()?;
                Some(
                    g.slots
                        .get(idx as usize)
                        .and_then(Option::as_ref)
                        .map_or_else(undefined, |v| Some(v.deref())),
                )
            }
            GlobalSlot::Empty => None,
        }
    }

    /// [`crate::opcode::Op::FetchGlobal`] on its run-time cache hit alone
    /// ([`Vm::fetch_global_cached`]), the value stored as
    /// [`Vm::store_fetched`] stores it: `Some(true)` when it went into the
    /// variable of the folded `AssignCv` (which is then skipped),
    /// `Some(false)` into `dst`. `None`, nothing changed, on a miss and
    /// for an undefined variable read with a warning:
    /// [`Vm::fetch_global_fast`] or the generic op decides.
    ///
    /// Written out rather than built from [`Vm::fetch_global_cached`] and
    /// [`store_fetched_in`]: the value goes from the global's slot into
    /// the variable with one copy (a scalar's bits, a reference count),
    /// and the variable may hold an array or an object, released as an
    /// assignment releases it (the tape polls for a safe point after the
    /// op). Only a variable bound by reference, which the `AssignCv`
    /// writes through, sends the value to the temporary.
    #[inline]
    pub(in crate::exec) fn fetch_global_hit(
        &mut self,
        pid: PlaceId,
        mode: FetchMode,
        dst: u32,
        to: Option<u32>,
    ) -> Option<bool> {
        let live = self.frames.live;
        let slot = *self
            .frames
            .vec
            .get(live.checked_sub(1)?)?
            .code
            .rt_cache
            .globals
            .try_borrow()
            .ok()?
            .get(pid as usize)?;
        let fetched = |v: Option<&Value>| match v {
            Some(Value::Reference(r)) => Some(r.get()),
            Some(v) => Some(copy_value(v)),
            None if mode == FetchMode::Is => Some(Value::Null),
            None => None,
        };
        let v = match slot {
            GlobalSlot::GlobalCv(cv) => {
                // As in `fetch_global_cached`: not while a top-level
                // include or `eval` runs above the main frame.
                if !self.main_is_global_top() {
                    return None;
                }
                fetched(self.frames.vec.first()?.cvs.get(cv as usize)?.get())?
            }
            GlobalSlot::GlobalStore(idx) => {
                let g = self.globals.try_borrow().ok()?;
                fetched(g.slots.get(idx as usize).and_then(Option::as_ref))?
            }
            GlobalSlot::Empty => return None,
        };
        let f = self.frames.vec.get_mut(live - 1)?;
        if let Some(to) = to {
            if f.cvs
                .get(to as usize)
                .is_some_and(|slot| !matches!(slot.get(), Some(Value::Reference(_))))
            {
                f.sync_cv_to_symtab(to, &v);
                super::release_slot_value(f.cvs[to as usize].set(Some(v)));
                return Some(true);
            }
        }
        put_tmp(f.tmps.get_mut(dst as usize)?, v);
        Some(false)
    }

    /// `$GLOBALS['name'][$k]...` (also written `$GLOBALS['name']` as a
    /// global base) read in `Read` or `Is` mode when `name` is a literal
    /// naming a variable of the global scope's frame (not a superglobal)
    /// that holds an array with every key on the way
    /// ([`props::dims_read`]): `ZEND_FETCH_R` with `ZEND_FETCH_GLOBAL`,
    /// then the `ZEND_FETCH_DIM_R`/`_IS` hits. The variable is found once
    /// and remembered in the op array's global cache at
    /// `places.len() + pid` (the entry at `pid` is
    /// [`Vm::fetch_global_fast`]'s, which answers the base alone). A
    /// consumed string offset is released. `None`, nothing consumed, for
    /// the generic fetch (an undefined variable or key warns there).
    pub(crate) fn fetch_global_dims_fast(
        &mut self,
        pid: PlaceId,
        mode: FetchMode,
    ) -> Option<Value> {
        // The main frame's variable is the global's value only while no
        // top-level include or `eval` runs above it
        // (`Vm::main_is_global_top`).
        if mode == FetchMode::List || !self.main_is_global_top() {
            return None;
        }
        let f = self.frames.last()?;
        if f.kind == FrameKind::Builtin {
            return None;
        }
        let p = f.code.places.get(pid as usize)?;
        let (lit, segs) = match (&p.base, p.segs.as_slice()) {
            (PlaceBase::Global(Operand::Lit(lit)), segs @ [Seg::Dim(_), ..]) => (*lit, segs),
            (PlaceBase::GlobalsArray, [Seg::Dim(Operand::Lit(lit)), segs @ ..])
                if !segs.is_empty() =>
            {
                (*lit, segs)
            }
            _ => return None,
        };
        let idx = f.code.places.len() + pid as usize;
        let cached = f.code.rt_cache.globals.try_borrow().ok()?.get(idx).copied();
        let main = self.frames.first()?;
        let cv = match cached {
            Some(GlobalSlot::GlobalCv(cv)) => cv,
            _ => {
                let Some(Value::Str(name)) = f.code.literals.get(lit as usize) else {
                    return None;
                };
                if main.kind != FrameKind::Main
                    || crate::place::is_superglobal_name(name.as_bytes())
                {
                    return None;
                }
                let cv = main
                    .code
                    .cv_names
                    .iter()
                    .position(|n| n.as_bytes() == name.as_bytes())?;
                let cv = u32::try_from(cv).ok()?;
                let mut cache = f.code.rt_cache.globals.try_borrow_mut().ok()?;
                if cache.len() <= idx {
                    crate::rtcache::grow_slots(&mut cache, idx + 1, || GlobalSlot::Empty);
                }
                cache[idx] = GlobalSlot::GlobalCv(cv);
                cv
            }
        };
        let v = props::dims_read(f, main.cvs.get(cv as usize)?.get()?, segs)?;
        release_place_str_tmps(self.frame_mut(), pid);
        Some(v)
    }

    /// [`crate::opcode::Op::FetchGlobal`]: global variable read
    /// (`$GLOBALS['name']`, superglobals) resolved directly from the VM's
    /// global scope or the global store, with runtime caching.
    #[inline(always)]
    pub(crate) fn fetch_global_fast(&mut self, pid: PlaceId, mode: FetchMode) -> Option<Value> {
        if let Some(hit) = self.fetch_global_cached(pid, mode) {
            return hit;
        }
        let (name_op, code) = {
            let f = self.frame();
            let p = f.code.places.get(pid as usize)?;
            let op = match (&p.base, p.segs.as_slice()) {
                (PlaceBase::Global(op), []) => *op,
                (PlaceBase::GlobalsArray, [Seg::Dim(op)]) => *op,
                _ => return None,
            };
            (op, f.code.clone())
        };

        // Only a literal name can be cached per instruction: `$GLOBALS[$k]`
        // names a different variable whenever `$k` changes.
        let cacheable = matches!(name_op, Operand::Lit(_));

        // Fast path: check runtime cache first.
        let cached = if cacheable {
            let cache = code.rt_cache.globals.borrow();
            cache.get(pid as usize).copied()
        } else {
            None
        };

        // The main frame's variables are the globals' values only while
        // no top-level include or `eval` runs above it
        // (`Vm::main_is_global_top`): else the caller reads the table.
        let main_top = self.main_is_global_top();
        if let Some(slot) = cached {
            match slot {
                // Resolved below, from the table.
                GlobalSlot::GlobalCv(_) if !main_top => {}
                GlobalSlot::GlobalCv(cv) => {
                    let main_frame = self.frames.first()?;
                    let slot = main_frame.cvs.get(cv as usize)?;
                    return match slot.get() {
                        Some(v) => Some(v.deref()),
                        None if mode == FetchMode::Is => Some(Value::Null),
                        None => None,
                    };
                }
                GlobalSlot::GlobalStore(idx) => {
                    let g = self.globals.borrow();
                    return match g.slots.get(idx as usize).and_then(Option::as_ref) {
                        Some(v) => Some(v.deref()),
                        None if mode == FetchMode::Is => Some(Value::Null),
                        None => None,
                    };
                }
                GlobalSlot::Empty => {}
            }
        }

        // Uncached path: resolve name operand.
        let name_bytes: std::borrow::Cow<[u8]> = match name_op {
            Operand::Lit(lit) => match code.literals.get(lit as usize) {
                Some(Value::Str(s)) => std::borrow::Cow::Borrowed(s.as_bytes()),
                Some(Value::Int(i)) => std::borrow::Cow::Owned(i.to_string().into_bytes()),
                _ => return None,
            },
            Operand::Cv(cv) => {
                let slot = self.frame().cvs.get(cv as usize)?;
                match slot.get()? {
                    Value::Str(s) => std::borrow::Cow::Owned(s.as_bytes().to_vec()),
                    Value::Int(i) => std::borrow::Cow::Owned(i.to_string().into_bytes()),
                    _ => return None,
                }
            }
            Operand::Tmp(t) => match self.frame().tmps.get(t as usize)? {
                Value::Str(s) => std::borrow::Cow::Owned(s.as_bytes().to_vec()),
                Value::Int(i) => std::borrow::Cow::Owned(i.to_string().into_bytes()),
                _ => return None,
            },
            _ => return None,
        };

        // Activate auto-global if armed:
        if crate::place::is_superglobal_name(&name_bytes) {
            self.is_auto_global(&name_bytes);
        }

        // Try VM's global scope first:
        let mut main_cv = false;
        if let Some(main_frame) = self.frames.first() {
            if main_frame.kind == FrameKind::Main {
                let cv = main_frame
                    .code
                    .cv_names
                    .iter()
                    .position(|n| n.as_bytes() == &name_bytes[..]);
                // A variable of the main script read while an include or
                // `eval` runs above it: from the table below, and not
                // remembered as a table slot (the main frame's own writes
                // do not reach the table once it runs again).
                main_cv = cv.is_some();
                if let Some(cv) = cv.filter(|_| main_top) {
                    let cv = cv as u32;
                    if cacheable {
                        let mut cache = code.rt_cache.globals.borrow_mut();
                        if cache.len() <= pid as usize {
                            crate::rtcache::grow_slots(&mut cache, pid as usize + 1, || {
                                GlobalSlot::Empty
                            });
                        }
                        cache[pid as usize] = GlobalSlot::GlobalCv(cv);
                    }

                    if let Some(slot) = main_frame.cvs.get(cv as usize) {
                        if let Some(v) = slot.get() {
                            return Some(v.deref());
                        }
                    }
                    return if mode == FetchMode::Is {
                        Some(Value::Null)
                    } else {
                        None
                    };
                }
            }
        }

        // Fall back to the global symbol table:
        self.attach_globals_symbol_table();
        let idx = self.globals.borrow_mut().find(&name_bytes);
        if let Some(idx) = idx {
            if cacheable && !main_cv {
                let mut cache = code.rt_cache.globals.borrow_mut();
                if cache.len() <= pid as usize {
                    crate::rtcache::grow_slots(&mut cache, pid as usize + 1, || GlobalSlot::Empty);
                }
                cache[pid as usize] = GlobalSlot::GlobalStore(idx);
            }
            let g = self.globals.borrow();
            return match g.slots.get(idx as usize).and_then(Option::as_ref) {
                Some(v) => Some(v.deref()),
                None if mode == FetchMode::Is => Some(Value::Null),
                None => None,
            };
        }

        if mode == FetchMode::Is {
            Some(Value::Null)
        } else {
            None
        }
    }

    /// `$cv[d1][d2]... .= val;` when `$cv` holds an array (not a reference),
    /// every offset is an int or a string, the target element exists and is a
    /// string, and `val` peeks as a string or an int: extends the nested string
    /// element in place after separating the arrays along the path, consuming
    /// offset and `OP_DATA` temporaries. Returns `false` otherwise, having
    /// changed nothing.
    #[inline]
    pub(super) fn concat_dims_cv_fast(&mut self, place: PlaceId, val: Operand) -> bool {
        let Some(f) = self.frames.last_mut() else {
            return false;
        };
        let Some(p) = f.code.places.get(place as usize) else {
            return false;
        };
        let crate::opcode::PlaceBase::Cv(cv) = p.base else {
            return false;
        };
        if p.segs.is_empty() {
            return false;
        }
        let Some((keys, n)) = write_keys_in(f, place) else {
            return false;
        };
        let add = match plain_value_in(f, val) {
            Some(v @ (Value::Str(_) | Value::Int(_))) => v,
            _ => return false,
        };
        let idx = cv as usize;
        let Some(slot) = f.cvs.get(idx) else {
            return false;
        };
        // SAFETY: no other reference into the slot is alive: the keys and
        // the appended value are owned copies.
        let val_mut = unsafe { slot.get_mut_unchecked() };
        let Some(Value::Array(arr)) = val_mut else {
            return false;
        };
        if !dims_has_str(arr, &keys[..n]) {
            return false;
        }
        if f.symtab_attached {
            f.unsync_cv_from_symtab(cv);
        }
        let ok = concat_dims(arr.make_mut(), &keys[..n], &add);
        if f.symtab_attached {
            f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
        }
        drop(add);
        if !ok {
            return false;
        }
        release_place_str_tmps(f, place);
        if let Operand::Tmp(t) = val {
            put_tmp(&mut f.tmps[t as usize], Value::Null);
        }
        true
    }

    /// A plain value (see [`is_plain_value`]) an `OP_DATA` operand holds,
    /// without consuming it ([`plain_value_in`] of the current frame).
    #[inline(always)]
    #[allow(clippy::inline_always)] // hot OP_DATA operand read in the dispatch pre-match
    pub(super) fn peek_plain_value(&self, op: Operand) -> Option<Value> {
        plain_value_in(self.frame(), op)
    }

    /// The object (not behind a reference) an `OP_DATA` operand of the
    /// current frame holds, its handle copied, without consuming it.
    pub(super) fn peek_object_value(&self, op: Operand) -> Option<Value> {
        let f = self.frame();
        let v = match op {
            Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
            Operand::Cv(cv) => f.cvs.get(cv as usize)?.get()?,
            Operand::Lit(_) | Operand::Unused => return None,
        };
        match v {
            Value::Object(o) => Some(Value::Object(o.clone())),
            _ => None,
        }
    }

    /// [`dim_value_in`] of the current frame.
    pub(super) fn peek_dim_value(&self, op: Operand) -> Option<Value> {
        dim_value_in(self.frame(), op)
    }

    /// The generic `Op::Assign { place, val, dst: None }`, the op
    /// [`crate::opcode::Op::AssignDimCv`] replaced.
    #[inline(never)]
    pub(super) fn assign_place_unused(&mut self, place: PlaceId, val: Operand) -> VmResult<Value> {
        match val {
            Operand::Cv(cv) => match self.read_cv_op_data(cv) {
                CvOpData::Value(v) => self.place_assign(place, v),
                CvOpData::Deferred => self.place_assign_cv_ref(place, cv),
                CvOpData::Undefined => self.place_assign_undef_cv(place, cv),
            },
            other => {
                let v = self.read(other)?;
                let v = self.copy_on_assign(v);
                self.place_assign(place, v)
            }
        }
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

    // Expected outputs below are from `/usr/bin/php8.5 -n`.

    /// `$GLOBALS[$k]` with a changing key reads the variable `$k` names
    /// each time: only literal names are cached per instruction (the
    /// user-stream wrapper of ext/standard/tests/file/userstreams.phpt).
    #[test]
    fn globals_with_a_variable_key_is_not_cached() {
        let out = run(r#"<?php
$a = "A"; $b = "B";
function g($k) { return $GLOBALS[$k]; }
echo g("a"), g("b"), g("a"), "\n";
foreach (["a", "b"] as $k) { echo $GLOBALS[$k]; }
echo "\n";
"#);
        assert_eq!(out, "ABA\nAB\n");
    }

    /// Cached `$GLOBALS['name']` / superglobal reads (`fetch_global_cached`)
    /// see later assignments, a global added after the first (undefined,
    /// `??`) read, a write through a reference and a changed superglobal.
    #[test]
    fn cached_global_reads_follow_later_writes() {
        let out = run(r#"<?php
$main = 'm';
function f() {
    $o = [];
    for ($i = 0; $i < 3; $i++) {
        $o[] = $GLOBALS['main'] . ($GLOBALS['late'] ?? 'none') . count($_GET) . ($GLOBALS['dyn'] ?? '-');
        if ($i == 0) { $GLOBALS['late'] = 'L'; }
        if ($i == 1) { $GLOBALS['main'] = 'M'; $GLOBALS['dyn'] = 'D'; $_GET['k'] = 1; }
    }
    $r = &$GLOBALS['main']; $r = 'R'; $o[] = $GLOBALS['main'];
    return implode(',', $o);
}
echo f(), "\n";
for ($i = 0; $i < 2; $i++) { $x = $_GET; echo count($x), "\n"; }
"#);
        assert_eq!(out, "mnone0-,mL0-,ML1D,R\n1\n1\n");
    }

    #[test]
    fn detached_symbol_table_leaves_no_duplicate_handle() {
        // After a generic `??=` (which attaches the table for that access
        // only), the array is owned by its variable alone: the refcount
        // php-src shows, and an append that does not separate a copy.
        let src = r#"<?php
function k() { $a = [1, 2]; $a['x'] ??= 3; $a[] = 4; debug_zval_dump($a); $b = $a; $a[] = 5; debug_zval_dump($b); echo count($a), "\n"; }
k();
$m = [1]; $m['y'] ??= 2; $m[] = 3; debug_zval_dump($m);
"#;
        let expected = r#"array(4) refcount(2){
  [0]=>
  int(1)
  [1]=>
  int(2)
  ["x"]=>
  int(3)
  [2]=>
  int(4)
}
array(4) refcount(2){
  [0]=>
  int(1)
  [1]=>
  int(2)
  ["x"]=>
  int(3)
  [2]=>
  int(4)
}
5
array(3) refcount(2){
  [0]=>
  int(1)
  ["y"]=>
  int(2)
  [1]=>
  int(3)
}
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn dimension_fast_paths_and_symbol_table_detach() {
        // String and numeric-string offsets looked up in place, compound
        // assignments and increments on unshared and shared arrays and
        // through references, nested reads, appends (`$a[] = v`, a full
        // array, an undefined variable, a string), string offsets, and a function whose
        // generic `??=` attaches its symbol table only for that access:
        // later `$$`, `get_defined_vars()`, `global` and `$GLOBALS` still
        // see every variable.
        let src = r#"<?php
$h = ["alpha" => 1, "beta" => 2, 5 => "five", "05" => "oh-five"];
$k = "beta"; $n = "5"; $z = "05";
echo $h["beta"], $h[$k], $h[$n], $h[$z], $h[5], "\n";
var_dump(isset($h["5"]), isset($h["gamma"]), empty($h["alpha"]), isset($h[$z]));
$h["beta"] += 40; $h[$n] .= "!"; $h["alpha"]++; ++$h[$k];
var_dump($h);
$r = &$h; echo $r["alpha"], $r[$n], "\n"; $r["alpha"] *= 3; echo $h["alpha"], "\n"; unset($r);
$c = $h; $c["beta"] -= 1; echo $h["beta"], " ", $c["beta"], "\n";
$m = [[1, 2], ["x" => [3]]]; $i = 1; $x = "x";
echo $m[0][1], $m[$i][$x][0], "\n";
$a = []; for ($j = 0; $j < 5; $j++) { $a[] = $j * 2; } $a[] = "s"; $a[] = 1.5; $a[] = null;
var_dump($a);
$b = $a; $b[] = 9; echo count($a), count($b), "\n";
$q = &$a; $q[] = 7; echo count($a), "\n"; unset($q);
$f = [PHP_INT_MAX => 1];
try { $f[] = 2; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$u[] = 1; var_dump($u);
$s = "str"; try { $s[] = "x"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
function g() {
    $a = []; $a['x'] ??= 1; $a['y'] = 2; $t = 0;
    for ($i = 0; $i < 3; $i++) { $a[$i] = $i; $t += $a[$i]; }
    $v = 'a'; echo count($$v), " ", $t, "\n";
    ${$v}['w'] = 9; $n = 'dyn'; $$n = 5; $t += $a['w'] + $dyn; echo $t, "\n";
    foreach (get_defined_vars() as $name => $val) { echo $name, ' '; } echo "\n";
    $a['z'] = 3; foreach ($a as $kk => $vv) { echo $kk, ','; } echo "\n";
}
g();
$G = [1]; $G[] = 2; $W = ['k' => 1]; $W['k2'] ??= 5;
function h() { global $G; $G[] = 3; $GLOBALS['W']['k3'] = 7; echo count($GLOBALS['G']), "\n"; }
h(); $G[] = 4; $W['k4'] = 1; echo implode(',', $G), " "; foreach ($W as $kk => $vv) { echo $kk, ','; } echo "\n";
$str = "test"; $o = "1"; $ri = 2; $rr = &$ri;
echo $str[1], $str["2"], $str[-1], $str[$o], $str[$rr], "\n";
$arr = [10, 20, 30]; echo $arr[$rr], "\n";
"#;
        let expected = r#"22fiveoh-fivefive
bool(true)
bool(false)
bool(false)
bool(true)
array(4) {
  ["alpha"]=>
  int(2)
  ["beta"]=>
  int(43)
  [5]=>
  string(5) "five!"
  ["05"]=>
  string(7) "oh-five"
}
2five!
6
43 42
23
array(8) {
  [0]=>
  int(0)
  [1]=>
  int(2)
  [2]=>
  int(4)
  [3]=>
  int(6)
  [4]=>
  int(8)
  [5]=>
  string(1) "s"
  [6]=>
  float(1.5)
  [7]=>
  NULL
}
89
9
Error: Cannot add element to the array as the next element is already occupied
array(1) {
  [0]=>
  int(1)
}
[] operator not supported for strings
5 3
17
a t i v n dyn 
x,y,0,1,2,w,z,
3
1,2,3,4 k,k2,k3,k4,
estes
30
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn smart_branches_on_numbers_and_other_values() {
        let src = r#"<?php
$n = 0;
for ($i = 0; $i < 5; $i++) { if ($i <= 2) { $n++; } }
echo $n, "\n";
for ($f = 0.5; $f < 3; $f += 1.0) { echo $f, " "; }
echo "\n";
$s = "10";
if ($s < 9) { echo "lt\n"; } else { echo "ge\n"; }
if (1 == 1.0) { echo "eq\n"; }
if (1 === 1.0) { echo "id\n"; } else { echo "not id\n"; }
if (1 !== 1.0) { echo "not id2\n"; }
if (NAN == NAN) { echo "nan\n"; } else { echo "no nan\n"; }
$a = [1]; $b = [1];
while ($a == $b) { echo "arrays\n"; $b[] = 2; }
$k = 3;
while ($k) { $k--; }
echo $k, "\n";
for (;;) { if ($k++ >= 2) { break; } }
echo $k, "\n";
for ($j = 0, $m = 10; $j < 3, $m > 7; $j++, $m--) { echo $j, $m, " "; }
echo "\n";
"#;
        let expected =
            "3\n0.5 1.5 2.5 \nge\neq\nnot id\nnot id2\nno nan\narrays\n0\n3\n010 19 28 \n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn smart_branches_and_numeric_ops_on_every_number_pair() {
        // Each comparison as an `if` (JmpZ) and a rotated `while` (JmpNz)
        // over int, float, -0.0, NaN, infinities and large ints (the
        // `(double)` widening of int/float pairs, `===` type mismatch),
        // then fused arithmetic, overflow and compound assignments.
        let src = r#"<?php
$vals = [0, 1, -1, 1.0, 0.5, -0.0, NAN, INF, -INF, PHP_INT_MAX, 9007199254740993, 9007199254740992.0];
foreach ($vals as $x) {
    $line = '';
    foreach ($vals as $y) {
        $r = '';
        if ($x < $y) { $r .= 'L'; } else { $r .= 'l'; }
        if ($x <= $y) { $r .= 'E'; } else { $r .= 'e'; }
        if ($x == $y) { $r .= 'Q'; } else { $r .= 'q'; }
        if ($x != $y) { $r .= 'N'; } else { $r .= 'n'; }
        if ($x === $y) { $r .= 'I'; } else { $r .= 'i'; }
        if ($x !== $y) { $r .= 'D'; } else { $r .= 'd'; }
        while ($x < $y) { $r .= '1'; break; }
        while ($x <= $y) { $r .= '2'; break; }
        while ($x == $y) { $r .= '3'; break; }
        while ($x != $y) { $r .= '4'; break; }
        while ($x === $y) { $r .= '5'; break; }
        while ($x !== $y) { $r .= '6'; break; }
        if ($x > 0.5) { $r .= 'g'; }
        if (1 >= $y) { $r .= 'h'; }
        $line .= $r . ' ';
    }
    echo $line, "\n";
}
$i = 0; $f = 0.0; $n = 0;
for ($k = 0; $k < 3; $k++) { $i++; $f += 0.5; $n = $n + $k * 2; }
var_dump($i, $f, $n);
$m = PHP_INT_MAX; $o = $m + 1; $p = $m; $p = $p + 1; var_dump($o, $p);
$q = PHP_INT_MIN; $q--; var_dump($q);
$s = 5; $t = $s++ + ++$s; var_dump($s, $t);
$u = 1.5; $v = $u-- * --$u; var_dump($u, $v);
$w = 7; $w -= 2; $w *= 3; $w ^= 1; $w |= 64; $w &= 0x7f; var_dump($w);
$z = 3; $z += $z; var_dump($z); $z -= $z; var_dump($z);
$big = PHP_INT_MAX; $big += 1; var_dump($big); $neg = PHP_INT_MIN; $neg -= 1; var_dump($neg);
$e = 2; $res = ($e *= 2.5); var_dump($e, $res);
"#;
        let expected = r#"lEQnId235h LEqNiD1246h leqNiD46h LEqNiD1246h LEqNiD1246h lEQniD236h leqNiD46 LEqNiD1246 leqNiD46h LEqNiD1246 LEqNiD1246 LEqNiD1246 
leqNiD46gh lEQnId235gh leqNiD46gh lEQniD236gh leqNiD46gh leqNiD46gh leqNiD46g LEqNiD1246g leqNiD46gh LEqNiD1246g LEqNiD1246g LEqNiD1246g 
LEqNiD1246h LEqNiD1246h lEQnId235h LEqNiD1246h LEqNiD1246h LEqNiD1246h leqNiD46 LEqNiD1246 leqNiD46h LEqNiD1246 LEqNiD1246 LEqNiD1246 
leqNiD46gh lEQniD236gh leqNiD46gh lEQnId235gh leqNiD46gh leqNiD46gh leqNiD46g LEqNiD1246g leqNiD46gh LEqNiD1246g LEqNiD1246g LEqNiD1246g 
leqNiD46h LEqNiD1246h leqNiD46h LEqNiD1246h lEQnId235h leqNiD46h leqNiD46 LEqNiD1246 leqNiD46h LEqNiD1246 LEqNiD1246 LEqNiD1246 
lEQniD236h LEqNiD1246h leqNiD46h LEqNiD1246h LEqNiD1246h lEQnId235h leqNiD46 LEqNiD1246 leqNiD46h LEqNiD1246 LEqNiD1246 LEqNiD1246 
leqNiD46h leqNiD46h leqNiD46h leqNiD46h leqNiD46h leqNiD46h leqNiD46 leqNiD46 leqNiD46h leqNiD46 leqNiD46 leqNiD46 
leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46g lEQnId235g leqNiD46gh leqNiD46g leqNiD46g leqNiD46g 
LEqNiD1246h LEqNiD1246h LEqNiD1246h LEqNiD1246h LEqNiD1246h LEqNiD1246h leqNiD46 LEqNiD1246 lEQnId235h LEqNiD1246 LEqNiD1246 LEqNiD1246 
leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46g LEqNiD1246g leqNiD46gh lEQnId235g leqNiD46g leqNiD46g 
leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46g LEqNiD1246g leqNiD46gh LEqNiD1246g lEQnId235g lEQniD236g 
leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46gh leqNiD46g LEqNiD1246g leqNiD46gh LEqNiD1246g lEQniD236g lEQnId235g 
int(3)
float(1.5)
int(6)
float(9.223372036854776E+18)
float(9.223372036854776E+18)
float(-9.223372036854776E+18)
int(7)
int(12)
float(-0.5)
float(-0.75)
int(78)
int(6)
int(0)
float(9.223372036854776E+18)
float(-9.223372036854776E+18)
float(5)
float(5)
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn concat_assign_on_cvs() {
        // `$cv .= v` (ConcatCv): strings extended in place from literals,
        // ints, floats, variables, references and temporaries, with a used
        // result; and the generic cases: self-append, an aliased string,
        // non-string targets, conversions with warnings, undefined
        // variables, typed references.
        let src = r#"<?php
class T { function __toString(): string { return 'T'; } }
class P { public string $p = 'p'; public int $n = 1; }
$s = '';
for ($i = 0; $i < 3; $i++) { $s .= 'ab'; $s .= $i; $s .= "c$i"; }
var_dump($s);
$a = 'x'; $b = 'y'; $n = 7;
for ($i = 0; $i < 2; $i++) { $a .= $b; $a .= $n; $a .= 1.5; }
var_dump($a);
$c = 'c'; $c .= $c; $c .= $c; var_dump($c);
$d = 'd'; $e = $d; $d .= $e; var_dump($d, $e);
$r = 'r'; $ref = &$r; $ref .= 'R'; $r .= 'S'; var_dump($r, $ref);
$t = 't'; $u = 'u'; $tr = &$u; $t .= $tr; var_dump($t);
$x = ($s2 = 's2') . ''; $y = ($x .= '!'); var_dump($x, $y);
$m = 5; $m .= 'm'; var_dump($m);
$nul = null; $nul .= 'n'; var_dump($nul);
$arr = [1]; $arr .= 'a'; var_dump($arr);
$v = 'v'; $v .= new T; var_dump($v);
$w = 'w'; $w .= [1]; var_dump($w);
$z = 'z'; $z .= $undef; var_dump($z);
$undef2 .= 'u2'; var_dump($undef2);
$o = new P; $pr = &$o->p; $pr .= 'q'; var_dump($o->p);
$ni = &$o->n; try { $ni .= 'x'; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } var_dump($o->n);
$big = str_repeat('-', 10); for ($i = 0; $i < 3; $i++) { $big .= $big; } var_dump(strlen($big));
$k = 'k'; $k .= true; $k .= false; $k .= null; var_dump($k);
$f = 'f'; $f .= -0.0; $f .= 1e100; var_dump($f);
"#;
        let expected = r#"string(15) "ab0c0ab1c1ab2c2"
string(11) "xy71.5y71.5"
string(4) "cccc"
string(2) "dd"
string(1) "d"
string(3) "rRS"
string(3) "rRS"
string(2) "tu"
string(3) "s2!"
string(3) "s2!"
string(2) "5m"
string(1) "n"

Warning: Array to string conversion in t.php on line 17
string(6) "Arraya"
string(2) "vT"

Warning: Array to string conversion in t.php on line 19
string(6) "wArray"

Warning: Undefined variable $undef in t.php on line 20
string(1) "z"

Warning: Undefined variable $undef2 in t.php on line 21
string(2) "u2"
string(2) "pq"
Cannot assign string to reference held by property P::$n of type int
int(1)
int(80)
string(2) "k1"
string(11) "f-01.0E+100"
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn nested_dimension_writes_on_cvs() {
        // `$cv[a][b]... = v` and `op= v` (AssignDimsCv, AssignOpDimsCv):
        // rows created on the way (`mkmatrix`), copy-on-write of a shared
        // array, null and missing containers, string keys from temporaries,
        // four levels; and the generic cases with their diagnostics: false
        // (deprecation), a string, ArrayAccess, a scalar, a reference, a
        // missing key under `op=`, numeric strings, overflow, `.=`, a float
        // offset, an undefined variable, five levels.
        let src = r#"<?php
function enc($v) { if (!is_array($v)) { return var_export($v, true); } $s = "["; foreach ($v as $k => $x) { $s .= var_export($k, true) . "=>" . enc($x) . ","; } return $s . "]"; }
class AO implements ArrayAccess { public $a = []; function offsetExists($k): bool { return isset($this->a[$k]); } function offsetGet($k): mixed { return $this->a[$k] ?? null; } function offsetSet($k, $v): void { echo "set $k\n"; $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
$mx = []; $count = 1;
for ($i = 0; $i < 3; $i++) { for ($j = 0; $j < 2; $j++) { $mx[$i][$j] = $count++; } }
echo enc($mx), "\n";
$b = $mx; $mx[1][1] = '7'; $mx[2][5] = 2.5; echo enc($mx), ' ', enc($b), "\n";
for ($i = 0; $i < 3; $i++) { $mx[$i][0] += 10; $mx[$i][1] *= 2; }
echo enc($mx), "\n";
$n = ['a' => null, 'f' => false, 's' => 'str', 'o' => new AO, 'i' => 5];
$n['a']['k'] = 1; $n['new']['k'] = 2; echo enc($n['a']), enc($n['new']), "\n";
$n['f']['k'] = 3; echo enc($n['f']), "\n";
$n['s'][1] = 'X'; echo $n['s'], "\n";
$n['o']['k'] = 4; echo enc($n['o']->a), "\n";
try { $n['i']['k'] = 5; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$t = [[1, 2]]; $r = &$t[0]; $t[0][1] = 7; echo $r[1], "\n"; unset($r);
$d = []; for ($i = 0; $i < 2; $i++) { $d["k$i"]["x"]["y"] = "v$i"; $d["1"][$i] = $i; $d[1]["z"][0][1] = 0; }
echo enc($d), "\n";
$c = [[[[1]]]]; $c[0][0][0][0] = 2; try { $c[0][0][0][0][0] = 3; } catch (Error $e) { echo $e->getMessage(), "\n"; } $c[0][0][0][1] = [4]; $c[0][0][0][1][0] = 5;
echo enc($c), "\n";
$m = [[1, 'a' => 1.5]]; $m[0][0] += 1; $m[0]['a'] -= 0.5; $m[0][0] |= 8; $m[0][0] ^= 3; $m[0][0] &= 7;
echo enc($m), "\n";
$m[0]['missing'] += 1; echo enc($m), "\n";
$m[0][0] = '5'; $m[0][0] += 1; var_dump($m[0][0]);
$m[0][0] = PHP_INT_MAX; $m[0][0] += 1; var_dump($m[0][0]);
$m[0][0] .= 'x'; var_dump($m[0][0]);
$m[1][0] += 1; echo enc($m), "\n";
$s = [[0]]; $s2 = $s; $s[0][0] += 5; echo enc($s), enc($s2), "\n";
$k = 1.5; $f = [[0, 1]]; $f[0][$k] = 9; echo enc($f), "\n";
$u[0][1] = 'u'; echo enc($u), "\n";
$z = [[0]]; $z[0][0] = [1]; $z[0][0] = 'over'; echo enc($z), "\n";
"#;
        let expected = r#"[0=>[0=>1,1=>2,],1=>[0=>3,1=>4,],2=>[0=>5,1=>6,],]
[0=>[0=>1,1=>2,],1=>[0=>3,1=>'7',],2=>[0=>5,1=>6,5=>2.5,],] [0=>[0=>1,1=>2,],1=>[0=>3,1=>4,],2=>[0=>5,1=>6,],]
[0=>[0=>11,1=>4,],1=>[0=>13,1=>14,],2=>[0=>15,1=>12,5=>2.5,],]
['k'=>1,]['k'=>2,]

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 12
['k'=>3,]
sXr
set k
['k'=>4,]
Cannot use a scalar value as an array
7
['k0'=>['x'=>['y'=>'v0',],],1=>[0=>0,'z'=>[0=>[1=>0,],],1=>1,],'k1'=>['x'=>['y'=>'v1',],],]
Cannot use a scalar value as an array
[0=>[0=>[0=>[0=>2,1=>[0=>5,],],],],]
[0=>[0=>1,'a'=>1.0,],]

Warning: Undefined array key "missing" in t.php on line 23
[0=>[0=>1,'a'=>1.0,'missing'=>1,],]
int(6)
float(9.223372036854776E+18)
string(20) "9.2233720368548E+18x"

Warning: Undefined array key 1 in t.php on line 27

Warning: Undefined array key 0 in t.php on line 27
[0=>[0=>'9.2233720368548E+18x','a'=>1.0,'missing'=>1,],1=>[0=>1,],]
[0=>[0=>5,],][0=>[0=>0,],]

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 29
[0=>[0=>0,1=>9,],]
[0=>[1=>'u',],]
[0=>[0=>'over',],]
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn coalesce_elvis_nullsafe_and_moves_on_cvs() {
        // `??`, `?:`, `?->`, ternary moves and assignments from variables
        // over every kind of value, through a reference and on undefined
        // variables (the generic ops' warnings), in the hot pre-match.
        let src = r#"<?php
class O { public $p = 'p'; function m() { return 'm'; } }
class F { function __toString(): string { return ''; } }
$vals = ['null' => null, 'false' => false, 'true' => true, 'zero' => 0, 'one' => 1, 'fzero' => 0.0, 'half' => 0.5,
    'empty' => '', 'szero' => '0', 'str' => 'a', 'arr0' => [], 'arr1' => [1], 'obj' => new O, 'f' => new F];
foreach ($vals as $name => $v) {
    $c = $v ?? 'dflt'; $e = $v ?: 'elvis'; $t = $v ? 'yes' : 'no'; $m = $v;
    $n = $v?->p ?? 'np';
    echo $name, ': ', is_object($c) ? 'obj' : var_export($c, true), ' ', is_object($e) ? 'obj' : var_export($e, true), ' ', $t, ' ', gettype($m), ' ', var_export($n, true), "\n";
}
$r = null; $ref = &$r; var_dump($ref ?? 'rd', $ref ?: 'rel', $ref?->p);
$r = 'set'; var_dump($ref ?? 'rd', $ref ?: 'rel');
$r = new O; var_dump($ref?->m());
var_dump($undef1 ?? 'u1');
var_dump($undef2 ?: 'u2');
var_dump($undef3?->p);
$x = $undef4; var_dump($x);
$a = 1; $b = $a; $a = 2; var_dump($b);
$s = 'str'; $s2 = $s; $s .= 'x'; var_dump($s2);
$arr = [1]; $arr2 = $arr; $arr[] = 2; var_dump(count($arr2));
for ($i = 0; $i < 3; $i++) { $k = $i ?: 'z'; $l = $vals['null'] ?? $i; echo $k, $l; } echo "\n";
"#;
        let expected = r#"null: 'dflt' 'elvis' no NULL 'np'
false: false 'elvis' no boolean 'np'
true: true true yes boolean 'np'
zero: 0 'elvis' no integer 'np'
one: 1 1 yes integer 'np'
fzero: 0.0 'elvis' no double 'np'
half: 0.5 0.5 yes double 'np'
empty: '' 'elvis' no string 'np'
szero: '0' 'elvis' no string 'np'
str: 'a' 'a' yes string 'np'
arr0: array (
) 'elvis' no array 'np'
arr1: array (
  0 => 1,
) array (
  0 => 1,
) yes array 'np'
obj: obj obj yes object 'p'
f: obj obj yes object 'np'
string(2) "rd"
string(3) "rel"
NULL
string(3) "set"
string(3) "set"
string(1) "m"
string(2) "u1"

Warning: Undefined variable $undef2 in t.php on line 15
string(2) "u2"

Warning: Undefined variable $undef3 in t.php on line 16
NULL

Warning: Undefined variable $undef4 in t.php on line 17
NULL
int(1)
string(3) "str"
int(1)
z01122
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn loop_steps_carry_their_conditions() {
        // IncDecCvCmpJmp: the step of `for`/`while` loops with int, float,
        // numeric-string, changing and reference bounds, `continue`, nested
        // loops, overflow at the step, a null counter, NaN, a loop that never
        // runs (entered at the condition) and `do`/`while`.
        let src = r#"<?php
$out = '';
for ($i = 0; $i < 5; $i++) { $out .= $i; } $out .= '|';
for ($i = 5; $i > 0; $i--) { $out .= $i; } $out .= '|';
for ($i = 0; $i <= 4; ++$i) { if ($i % 2) { continue; } $out .= $i; } $out .= '|';
for ($i = 0.5; $i < 3; $i++) { $out .= $i . ','; } $out .= '|';
for ($i = 0; $i < "3"; $i++) { $out .= $i; } $out .= '|';
for ($i = 0; $i != 4; $i++) { $out .= $i; } $out .= '|';
for ($i = 0; $i !== 3; $i++) { $out .= $i; } $out .= '|';
$n = 3; for ($i = 0; $i < $n; $i++) { $out .= $i; if ($i == 1) { $n = 2.5; } } $out .= '|';
for ($i = 10; $i < 5; $i++) { $out .= 'never'; } $out .= '|';
for ($i = PHP_INT_MAX - 2; $i < PHP_INT_MAX; $i++) { $out .= 'm'; } var_dump($i); $out .= '|';
$j = PHP_INT_MAX - 1; for (; $j < PHP_INT_MAX + 1; $j++) { $out .= 'o'; if ($j > PHP_INT_MAX) { break; } } var_dump($j); $out .= '|';
for ($i = 0; $i < 3; $i++) { for ($k = 0; $k < 2; $k++) { $out .= "$i$k "; } } $out .= '|';
$r = 0; $ref = &$r; for (; $r < 3; $ref++) { $out .= $r; } $out .= '|';
$w = 0; while ($w < 4) { $out .= $w; $w++; } $out .= '|';
$x = null; for (; $x < 2; $x++) { $out .= var_export($x, true); } $out .= '|';
$t = 0; do { $out .= $t; $t++; } while ($t < 3); $out .= '|';
for ($i = 0; $i < 3; $i++) { $nan = NAN; } $out .= ($i < NAN ? 'y' : 'n') . '|';
for ($i = 0, $e = 3; $i < $e; $i++) {} $out .= $i;
echo $out, "\n";
"#;
        let expected = r#"int(9223372036854775807)
int(9223372036854775806)
01234|54321|024|0.5,1.5,2.5,|012|0123|012|012||mm||00 01 10 11 20 21 |012|0123|NULL1|012|n|3
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn fused_compound_step_and_branch_ops() {
        let src = r#"<?php
$out = '';
for ($i = 0; $i < 5; $i = $i + 1) { $out .= $i; } $out .= '|';
for ($i = 0; $i < 6; $i += 2) { $out .= $i; } $out .= '|';
for ($i = 1; $i <= 3; $i += 1) { $out .= $i; } $out .= '|';
$r = 0; $ref = &$r; for (; $r < 3; $r = $r + 1) { $out .= $r; } $out .= '|';
$x = 1; $x = $x + 2; if ($x) { $out .= 'T'; } else { $out .= 'F'; }
$x = 1; $x = $x - 1; if ($x) { $out .= 'T'; } else { $out .= 'F'; }
$x = 0; $x += 5; if ($x) { $out .= 'T'; } else { $out .= 'F'; }
$x = 5; $x -= 5; if ($x) { $out .= 'T'; } else { $out .= 'F'; }
echo $out, "\n";
"#;
        let expected = "01234|024|123|012|TFTF\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn simple_identity_and_truth_tests() {
        let src = r#"<?php
$n = null; $f = false; $s = "abc"; $t = "abc"; $i = 1; $z = 0; $e = ""; $a = [];
if ($n === null) { echo "a"; }
if ($n !== null) { echo "X"; } else { echo "b"; }
if ($f === false) { echo "c"; }
if ($f == null) { echo "d"; }
if ($s === $t) { echo "e"; }
if ($s === "abd") { echo "X"; } else { echo "f"; }
if ($s == "abc") { echo "g"; }
if ($s != "abc") { echo "X"; } else { echo "h"; }
$ns = "1e1"; if ($ns == "10") { echo "i"; }
if ($ns === "10") { echo "X"; } else { echo "j"; }
if ($i === "1") { echo "X"; } else { echo "k"; }
if ($i == "1") { echo "l"; }
if ($i !== null) { echo "m"; }
if ($z) { echo "X"; } else { echo "n"; }
if ($e) { echo "X"; } else { echo "o"; }
$zs = "0"; if ($zs) { echo "X"; } else { echo "p"; }
$zz = "0.0"; if ($zz) { echo "q"; }
if ($a) { echo "X"; } else { echo "r"; }
$a[] = 1; if ($a) { echo "s"; }
$fl = 0.0; if ($fl) { echo "X"; } else { echo "t"; }
$fl = 0.5; while ($fl) { $fl = 0.0; echo "u"; }
$str = "x"; $cnt = 0; while ($str) { $str = ""; $cnt++; } echo $cnt;
$o = new stdClass; if ($o) { echo "v"; }
$r = 1; $ref = &$r; if ($ref) { echo "w"; } $r = 0; if ($ref) { echo "X"; } else { echo "x"; }
if ($n && $i) { echo "X"; } else { echo "y"; }
if ($z || $s) { echo "z"; }
echo "\n";
var_dump(strtolower("ABC") === "abc", "a" . "b" === "ab", null == false, [] === [], [1] === [1], $o === $o);
$x = ($i === "1"); var_dump($x);
$y = ($s !== $t); var_dump($y);
$w = ($n == $f); var_dump($w);
if ($undef) { echo "X"; } else { echo "undef-false\n"; }
if ($undef2 === null) { echo "undef-null\n"; }
"#;
        let out = run(src);
        let expected_head = "abcdefghijklmnopqrstu1vwxyz\nbool(true)\nbool(true)\nbool(true)\nbool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(false)\nbool(true)\n";
        assert!(out.starts_with(expected_head), "{out}");
        assert!(out.contains("Undefined variable $undef in"), "{out}");
        assert!(out.contains("undef-false\n"), "{out}");
        assert!(out.contains("Undefined variable $undef2 in"), "{out}");
        assert!(out.ends_with("undef-null\n"), "{out}");
    }

    #[test]
    fn smart_branch_warnings_keep_their_order() {
        let src = r#"<?php
for ($i = 0; $i < $undef; $i++) { echo "never\n"; }
if ($u2 < 1) { echo "yes\n"; }
echo "done\n";
"#;
        let out = run(src);
        let first = out
            .find("Undefined variable $undef")
            .expect("first warning");
        let second = out.find("Undefined variable $u2").expect("second warning");
        assert!(first < second, "{out}");
        assert!(out.contains("yes\n") && out.ends_with("done\n"), "{out}");
        assert!(!out.contains("never"), "{out}");
    }

    #[test]
    fn continue_in_rotated_loops_runs_the_step() {
        let src = r#"<?php
for ($i = 0; $i < 5; $i++) { if ($i % 2) { continue; } echo $i; }
echo "\n";
$i = 0;
while ($i < 5) { $i++; if ($i == 3) { continue; } echo $i; }
echo "\n";
"#;
        assert_eq!(run(src), "024\n1245\n");
    }

    #[test]
    fn fused_arithmetic_assignment() {
        let src = r#"<?php
$x = 0;
for ($i = 0; $i < 4; $i++) { $x = $x + $i; }
var_dump($x);
$y = 0.0; $f = 1.5;
$y = $y + $f * 2;
var_dump($y);
$o = PHP_INT_MAX; $o = $o + 1; var_dump($o);
$s = "abc"; $s = 1 + 2; var_dump($s);
$r = 1; $ref = &$r; $r = $r + 1; var_dump($ref);
$arr = [1]; $arr = 2 * 3; var_dump($arr);
class D { function __destruct() { echo "destructed\n"; } }
$d = new D; $d = 1 + 1; echo "after\n"; var_dump($d);
$c = 1 < 2; var_dump($c);
$m = "5"; $m = $m + 1; var_dump($m);
"#;
        let expected = "int(6)\nfloat(3)\nfloat(9.223372036854776E+18)\nint(3)\nint(2)\nint(6)\ndestructed\nafter\nint(2)\nbool(true)\nint(6)\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn compound_assignments_on_cvs() {
        let src = r#"<?php
$s = 0;
for ($i = 0; $i < 5; $i++) { $s += $i * 2 % 7; }
var_dump($s);
$f = 1.5; $f *= 2; var_dump($f);
$m = PHP_INT_MAX; $m += 1; var_dump($m);
$b = 6; $b &= 3; $b |= 8; $b ^= 1; var_dump($b);
$x = 1; $y = ($x += 2); var_dump($x, $y);
$n = "3"; $n += 1; var_dump($n);
$r = 1; $q = &$r; $r += 4; var_dump($q);
$u += 1; var_dump($u);
"#;
        let out = run(src);
        let expected_head = "int(13)\nfloat(3)\nfloat(9.223372036854776E+18)\nint(11)\nint(3)\nint(3)\nint(4)\nint(5)\n";
        assert!(out.starts_with(expected_head), "{out}");
        assert!(out.contains("Undefined variable $u"), "{out}");
        assert!(out.ends_with("int(1)\n"), "{out}");
    }

    #[test]
    fn incdec_on_cvs() {
        let src = r"<?php
$i = 1; $a = $i++; $b = ++$i; $c = $i--; $d = --$i;
var_dump($i, $a, $b, $c, $d);
$f = 1.5; $f++; ++$f; var_dump($f); $f--; var_dump($f);
$m = PHP_INT_MAX; $m++; var_dump($m);
$n = null; $n++; var_dump($n);
$r = 5; $q = &$r; $r++; var_dump($q);
$u++; var_dump($u);
";
        let out = run(src);
        let expected_head = "int(1)\nint(1)\nint(3)\nint(3)\nint(1)\nfloat(3.5)\nfloat(2.5)\nfloat(9.223372036854776E+18)\nint(1)\nint(6)\n";
        assert!(out.starts_with(expected_head), "{out}");
        assert!(out.contains("Undefined variable $u"), "{out}");
        assert!(out.ends_with("int(1)\n"), "{out}");
    }

    #[test]
    fn dim_reads_on_cvs() {
        let src = r#"<?php
$a = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]; $s = 0;
for ($i = 0; $i < 20; $i++) { $s = $a[$i & 7]; }
var_dump($s);
$h = ['x' => 1, '5' => 'five', 7 => [1]];
$k = 'x'; $v = $h[$k]; var_dump($v);
$v = $h['5']; var_dump($v);
$v = $h[5]; var_dump($v);
$v = $h[7]; var_dump($v);
$v = $h['missing'] ?? 'dflt'; var_dump($v);
$v = $h[1.5]; var_dump($v);
$t = 'str'; $v = $t[1]; var_dump($v);
$ref = &$a; $v = $a[3]; var_dump($v);
$e = [&$s]; $v = $e[0]; $s = 99; var_dump($v);
$self = [1, 2]; $self = $self[1]; var_dump($self);
$v = $h[$undef_key];
echo "end\n";
"#;
        let out = run(src);
        let expected_head = "int(3)\nint(1)\nstring(4) \"five\"\nstring(4) \"five\"\narray(1) {\n  [0]=>\n  int(1)\n}\nstring(4) \"dflt\"\n";
        assert!(out.starts_with(expected_head), "{out}");
        assert!(
            out.contains("Deprecated: Implicit conversion from float 1.5 to int loses precision"),
            "{out}"
        );
        assert!(out.contains("Warning: Undefined array key 1 in"), "{out}");
        assert!(
            out.contains("NULL\nstring(1) \"t\"\nint(3)\nint(3)\nint(2)\n"),
            "{out}"
        );
        assert!(out.contains("Undefined variable $undef_key"), "{out}");
        assert!(out.ends_with("end\n"), "{out}");
    }

    #[test]
    fn isset_empty_and_coalesce_on_cv_dimensions() {
        let src = r#"<?php class AO implements ArrayAccess { public $a; function __construct($a) { $this->a = $a; } function offsetExists($k): bool { return isset($this->a[$k]); } function offsetGet($k): mixed { return $this->a[$k]; } function offsetSet($k, $v): void { $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
$a = [1, 0, null, 'x' => 'y', '7' => 'seven', 'e' => '', 'z' => '0', 'arr' => [], 'f' => 0.0];
var_dump(isset($a[0]), isset($a[1]), isset($a[2]), isset($a[3]), isset($a['x']), isset($a[7]), isset($a['7']), isset($a['missing']));
var_dump(empty($a[0]), empty($a[1]), empty($a[2]), empty($a[3]), empty($a['x']), empty($a['e']), empty($a['z']), empty($a['arr']), empty($a['f']));
$k = 'x'; $i = 1; $fl = 1.5;
var_dump(isset($a[$k]), isset($a[$i]), isset($a[$fl]), empty($a[$k]), empty($a[$i]));
var_dump($a[$k] ?? 'd', $a[2] ?? 'd', $a['missing'] ?? 'd', $a[$i] ?? 'd');
$r = &$a; var_dump(isset($r['x']), isset($r['q']), $r['x'] ?? 'd');
$s = 'str'; var_dump(isset($s[1]), isset($s[5]), empty($s[0]), $s[1] ?? 'd');
$o = new AO(['k' => 1, 'n' => null]); var_dump(isset($o['k']), isset($o['n']), isset($o['m']), empty($o['k']), $o['k'] ?? 'd', $o['m'] ?? 'd');
$b = [&$i, [1]]; $i = null; var_dump(isset($b[0]), $b[0] ?? 'd', isset($b[1][0]), isset($b[1][1]), $b[1][0] ?? 'd');
var_dump(isset($undef[0]), empty($undef[0]), $undef[0] ?? 'd');
$ai = ['1' => 'a', '01' => 'b']; var_dump(isset($ai[1]), isset($ai['1']), isset($ai['01']), $ai['01'] ?? 'd');
$nan = [NAN]; var_dump(empty($nan[0]));
$t = ['x' => true]; $v = isset($t['x']) ? 'yes' : 'no'; echo $v, "\n";
if (isset($t['x'])) { echo "set\n"; }
if (!empty($t['y'])) { echo "X"; } else { echo "empty\n"; }
$m = ['q' => 5]; $w = $m['q'] ?? 0; $w2 = $m['zz'] ?? 0; var_dump($w, $w2);
"#;
        let out = run(src);
        let head = "bool(true)\nbool(true)\nbool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(true)\nbool(true)\nbool(true)\nbool(true)\n";
        assert!(out.starts_with(head), "{out}");
        assert!(
            out.contains("Implicit conversion from float 1.5 to int loses precision"),
            "{out}"
        );
        let middle = "bool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(true)\nstring(1) \"y\"\nstring(1) \"d\"\nstring(1) \"d\"\nint(0)\nbool(true)\nbool(false)\nstring(1) \"y\"\nbool(true)\nbool(false)\nbool(false)\nstring(1) \"t\"\nbool(true)\nbool(false)\nbool(false)\nbool(false)\nint(1)\nstring(1) \"d\"\nbool(false)\nstring(1) \"d\"\nbool(true)\nbool(false)\nint(1)\nbool(false)\nbool(true)\nstring(1) \"d\"\nbool(true)\nbool(true)\nbool(true)\nstring(1) \"b\"\n";
        assert!(out.contains(middle), "{out}");
        assert!(out.contains("NAN value was coerced to bool"), "{out}");
        assert!(
            out.ends_with("bool(false)\nyes\nset\nempty\nint(5)\nint(0)\n"),
            "{out}"
        );
    }

    #[test]
    fn string_keys_from_temporaries() {
        let src = r#"<?php
$X = [];
for ($i = 0; $i < 5; $i++) { $X["foo_$i"] = $i; }
$sum = 0;
for ($i = 0; $i < 5; $i++) { $sum += $X["foo_$i"]; }
var_dump($sum, cnt($X));
$n = 0;
for ($i = 0; $i < 7; $i++) { if (isset($X["foo_$i"])) { $n++; } }
var_dump($n);
$v = $X["foo_" . 2] ?? 'd'; var_dump($v);
$v = $X["bar_" . 2] ?? 'd'; var_dump($v);
$v = $X["foo_" . 9]; var_dump($v);
$k = 3; $X["foo_$k"] = 'three'; var_dump($X["foo_$k"]);
$Y = [5 => 'five']; var_dump($Y["" . 5], isset($Y[5 . ""]), $Y[strval(5)]);
$Y["0" . "7"] = 'oh-seven'; $ks = []; foreach ($Y as $kk => $_) { $ks[] = $kk; } var_dump($ks);
$s = "k"; $Z = []; $Z[$s . $s] = 1; $Z[$s . $s] += 2; var_dump($Z);
echo memory_get_usage() > 0 ? "ok\n" : "bad\n";
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
"#;
        let out = run(src);
        assert!(
            out.starts_with("int(10)\nint(5)\nint(5)\nint(2)\nstring(1) \"d\"\n"),
            "{out}"
        );
        assert!(out.contains("Undefined array key \"foo_9\""), "{out}");
        assert!(
            out.ends_with(
                "NULL\nstring(5) \"three\"\nstring(4) \"five\"\nbool(true)\nstring(4) \"five\"\narray(2) {\n  [0]=>\n  int(5)\n  [1]=>\n  string(2) \"07\"\n}\narray(1) {\n  [\"kk\"]=>\n  int(3)\n}\nok\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn compound_assignments_and_incdec_on_cv_dimensions() {
        let src = r#"<?php
$X = []; $Y = [];
for ($i = 0; $i < 5; $i++) { $X[$i] = $i + 1; $Y[$i] = 0; }
for ($k = 0; $k < 3; $k++) { for ($i = 4; $i >= 0; $i--) { $Y[$i] += $X[$i]; } }
var_dump($Y);
$h = ['a' => 1, 'b' => 1.5];
$h['a'] += 2; $h['b'] *= 2; $h['a'] -= 1; $h['a'] |= 8; $h['a'] &= 12; $h['a'] ^= 1;
var_dump($h);
$h['missing'] += 1; var_dump($h['missing']);
$h['s'] = "3"; $h['s'] += 1; var_dump($h['s']);
$h['a'] += 0.5; var_dump($h['a']);
$m = [PHP_INT_MAX]; $m[0] += 1; var_dump($m[0]);
$m[0] = PHP_INT_MAX; $m[0]++; var_dump($m[0]);
$c = [0, 1.5, "x", null, "9"]; $c[0]++; $c[1]++; $c[2]++; $c[3]++; $c[4]++; $c[0]--; var_dump($c);
$d = [5]; $e = $d; $d[0] += 1; $d[0]++; var_dump($d, $e);
$r = [1]; $v = 10; $r[0] = &$v; $r[0] += 5; $r[0]++; var_dump($v);
$u[0] += 1; var_dump($u);
$w = [1]; $z = ($w[0] += 2); $z2 = $w[0]++; $z3 = ++$w[0]; var_dump($z, $z2, $z3, $w);
$n = null; $n[1]++; var_dump($n);
$q = [1]; $qq = &$q; $q[0] += 1; $q[0]++; var_dump($qq);
$f = [1.5]; $f[0]++; $f[0]--; $f[0] += 1; var_dump($f[0]);
$g = ['k' => 2]; $kk = 'k'; $g[$kk] *= 3; $g[$kk]--; var_dump($g);
$b = [true]; $b[0]++; $b[0] += 1; var_dump($b);
"#;
        let out = run(src);
        let head = "array(5) {\n  [0]=>\n  int(3)\n  [1]=>\n  int(6)\n  [2]=>\n  int(9)\n  [3]=>\n  int(12)\n  [4]=>\n  int(15)\n}\narray(2) {\n  [\"a\"]=>\n  int(9)\n  [\"b\"]=>\n  float(3)\n}\n";
        assert!(out.starts_with(head), "{out}");
        assert!(out.contains("Undefined array key \"missing\""), "{out}");
        assert!(
            out.contains("int(1)\nint(4)\nfloat(9.5)\nfloat(9.223372036854776E+18)\nfloat(9.223372036854776E+18)\n"),
            "{out}"
        );
        assert!(
            out.contains("Increment on non-numeric string is deprecated"),
            "{out}"
        );
        assert!(
            out.contains("array(5) {\n  [0]=>\n  int(0)\n  [1]=>\n  float(2.5)\n  [2]=>\n  string(1) \"y\"\n  [3]=>\n  int(1)\n  [4]=>\n  int(10)\n}\narray(1) {\n  [0]=>\n  int(7)\n}\narray(1) {\n  [0]=>\n  int(5)\n}\nint(16)\n"),
            "{out}"
        );
        assert!(out.contains("Undefined variable $u"), "{out}");
        assert!(out.contains("Undefined array key 0"), "{out}");
        assert!(
            out.contains("array(1) {\n  [0]=>\n  int(1)\n}\nint(3)\nint(3)\nint(5)\narray(1) {\n  [0]=>\n  int(5)\n}\n"),
            "{out}"
        );
        assert!(out.contains("Undefined array key 1"), "{out}");
        assert!(
            out.contains("array(1) {\n  [1]=>\n  int(1)\n}\narray(1) {\n  [0]=>\n  int(3)\n}\nfloat(2.5)\narray(1) {\n  [\"k\"]=>\n  int(5)\n}\n"),
            "{out}"
        );
        assert!(
            out.contains("Increment on type bool has no effect"),
            "{out}"
        );
        assert!(out.ends_with("array(1) {\n  [0]=>\n  int(2)\n}\n"), "{out}");
    }

    #[test]
    fn nested_dimension_reads_on_cvs() {
        let src = r#"<?php class AO implements ArrayAccess { public $a; function __construct($a) { $this->a = $a; } function offsetExists($k): bool { return isset($this->a[$k]); } function offsetGet($k): mixed { return $this->a[$k]; } function offsetSet($k, $v): void { $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
$m = [[1, 2], [3, 4]]; $s = 0;
for ($i = 0; $i < 2; $i++) { for ($j = 0; $j < 2; $j++) { $s += $m[$i][$j]; } }
var_dump($s);
$v = $m[1][0]; var_dump($v);
$d = ['a' => ['b' => ['c' => 'deep']]];
var_dump($d['a']['b']['c'], $d['a']['b']['x'] ?? 'dflt', $d['a']['x']['c'] ?? 'dflt2', isset($d['a']['b']['c']), isset($d['a']['x']['c']));
$k = 'b'; var_dump($d['a'][$k]['c'], $d['a']["$k"]['c']);
$r = [&$m, 'x']; var_dump($r[0][1][1]);
$e = [1]; var_dump($e[0][0]);
var_dump($m[5][0]);
var_dump($m[0][7]);
$str = ['ab']; var_dump($str[0][1]);
$n = [null]; var_dump($n[0][0]);
$o = [new AO([[7]])]; var_dump($o[0][0][0]);
$ref = &$m[0]; $ref[0] = 99; var_dump($m[0][0]);
$t = [[1]]; $x = $t[0][0]; var_dump($x);
$w = ['k' => [1.5]]; $y = $w['k'][0] ?? 0; var_dump($y);
var_dump($undef[0][0] ?? 'u');
$f = [[0]]; var_dump($f[0][1.7] ?? 'fl');
echo "end\n";
"#;
        let out = run(src);
        let head = "int(10)\nint(3)\nstring(4) \"deep\"\nstring(4) \"dflt\"\nstring(5) \"dflt2\"\nbool(true)\nbool(false)\nstring(4) \"deep\"\nstring(4) \"deep\"\nint(4)\n";
        assert!(out.starts_with(head), "{out}");
        assert!(
            out.contains("Trying to access array offset on int"),
            "{out}"
        );
        assert!(out.contains("Undefined array key 5"), "{out}");
        assert!(
            out.contains("Trying to access array offset on null"),
            "{out}"
        );
        assert!(out.contains("Undefined array key 7"), "{out}");
        assert!(out.contains("NULL\nstring(1) \"b\"\n"), "{out}");
        assert!(
            out.contains("NULL\nint(7)\nint(99)\nint(1)\nfloat(1.5)\nstring(1) \"u\"\n"),
            "{out}"
        );
        assert!(
            out.contains("Implicit conversion from float 1.7 to int loses precision"),
            "{out}"
        );
        assert!(out.ends_with("string(2) \"fl\"\nend\n"), "{out}");
    }

    #[test]
    fn dim_writes_on_cvs() {
        let src = r#"<?php class AO implements ArrayAccess { public $a; function __construct($a) { $this->a = $a; } function offsetExists($k): bool { return isset($this->a[$k]); } function offsetGet($k): mixed { return $this->a[$k]; } function offsetSet($k, $v): void { $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
$a = [0, 0, 0, 0];
for ($i = 0; $i < 10; $i++) { $a[$i & 3] = $i; }
var_dump($a);
$b = $a; $b[0] = 'x'; var_dump($a[0], $b[0]);
$h = []; $h['k'] = 1.5; $h['3'] = true; $h[] = null; var_dump($h);
$o = [new AO([])]; $o[0] = 1; var_dump($o);
$r = [1]; $x = 5; $r[0] = &$x; $r[0] = 6; var_dump($x);
$str = 'abc'; $str[1] = 'X'; var_dump($str);
$n = null; $n[2] = 1; var_dump($n);
$t = [0]; $k = 0; $t[$k] = "s" . $k; var_dump($t);
$u[1] = 1; var_dump($u);
"#;
        let out = run(src);
        let expected = "array(4) {\n  [0]=>\n  int(8)\n  [1]=>\n  int(9)\n  [2]=>\n  int(6)\n  [3]=>\n  int(7)\n}\nint(8)\nstring(1) \"x\"\narray(3) {\n  [\"k\"]=>\n  float(1.5)\n  [3]=>\n  bool(true)\n  [4]=>\n  NULL\n}\narray(1) {\n  [0]=>\n  int(1)\n}\nint(6)\nstring(3) \"aXc\"\narray(1) {\n  [2]=>\n  int(1)\n}\narray(1) {\n  [0]=>\n  string(2) \"s0\"\n}\narray(1) {\n  [1]=>\n  int(1)\n}\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn assignments_to_cvs() {
        let src = r#"<?php
class D { function __destruct() { echo "gone\n"; } }
$a = new D; $a = 1; echo "after\n";
$s = "x"; $s = 2; var_dump($s);
$g = 1; function f() { global $g; $g = 5; } f(); var_dump($g);
$r = 1; $q = &$r; $r = 3; var_dump($q);
$arr = [1, 2]; $copy = $arr; $copy = 0; var_dump($arr);
$c = 'lit'; var_dump($c);
"#;
        let expected = "gone\nafter\nint(2)\nint(5)\nint(3)\narray(2) {\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n}\nstring(3) \"lit\"\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn specialised_ops_fall_back_after_a_failed_fast_path() {
        // Each fast path fails on the first iteration (string, null,
        // reference, undefined), so the generic op runs with its exact
        // diagnostics, and succeeds on later ones.
        let src = r#"<?php
$i = "0";
for (; $i < 3; $i++) { echo $i; }
echo "\n";
$a = null; $a[0] = 1; $a[1] = 2; var_dump(cnt($a));
$s = null; $s += 2; $s += 3; var_dump($s);
$x = [1, 2]; $y = &$x; $v = $x[1]; $v = $x[0]; var_dump($v);
$t = 0; $t = $t + "1"; $t = $t + 1; var_dump($t);
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
"#;
        let expected = "012\nint(2)\nint(5)\nint(1)\nint(2)\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn identity_comparisons_release_object_temporaries() {
        // `ZEND_IS_IDENTICAL` / `ZEND_IS_NOT_IDENTICAL` free both operands
        // (FREE_OP1/FREE_OP2): an object compared from a temporary is
        // destructed by the comparison, not at the end of the request, so
        // the weak reference is cleared and the cycle collected.
        let src = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~{$this->n}\n"; } }
$o = new stdClass; $o->o = $o; $w = WeakReference::create($o); unset($o);
var_dump($w->get() !== null);
var_dump(gc_collect_cycles());
var_dump($w->get());
var_dump(new D(1) !== null); echo "after 1\n";
var_dump(new D(2) === null); echo "after 2\n";
var_dump([new D(3)] !== null); echo "after 3\n";
if (new D(4) !== null) { echo "in if 4\n"; } echo "after 4\n";
$x = (new D(5) === null); echo "after 5\n";
var_dump(new D(6) == null); echo "after 6\n";
function f() { return new D(7); }
while (f() !== null) { echo "loop 7\n"; break; } echo "after 7\n";
$arr = [1]; var_dump($arr === null, null !== $arr);
echo "end\n";
"#;
        let expected = "bool(true)\nint(1)\nNULL\n~1\nbool(true)\nafter 1\n~2\nbool(false)\nafter 2\n~3\nbool(true)\nafter 3\n~4\nin if 4\nafter 4\n~5\nafter 5\n~6\nbool(false)\nafter 6\n~7\nloop 7\nafter 7\nbool(false)\nbool(true)\nend\n";
        assert_eq!(run(src), expected);
    }

    #[test]
    fn concat_assign_on_cv_dimensions() {
        // `$cv[k] .= v` (AssignOpDimCv): string elements extended in place
        // with string and int operands, from literals, variables and
        // temporaries; copy-on-write of a shared array; self-append; and
        // every case that keeps the generic op with its exact diagnostics:
        // missing and converted keys, non-string elements, elements held by
        // reference, a reference to the array, autovivification, string
        // offsets, array/float/object operands, undefined containers,
        // properties, ArrayAccess and scalars.
        let src = r#"<?php class AO implements ArrayAccess { public $a; function __construct($a) { $this->a = $a; } function offsetExists($k): bool { return isset($this->a[$k]); } function offsetGet($k): mixed { return $this->a[$k]; } function offsetSet($k, $v): void { $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
$a = ['k' => 'a', 5 => 'c', 'n' => 1, 'f' => 1.5, 'nul' => null];
for ($i = 0; $i < 3; $i++) { $a['k'] .= $i; $a[5] .= "-"; $a['k'] .= 'x'; }
var_dump($a['k'], $a[5]);
$b = $a; $a['k'] .= 'y'; var_dump($b['k'], $a['k']);
$a['n'] .= 'z'; $a['f'] .= 'w'; $a['nul'] .= 'v'; var_dump($a['n'], $a['f'], $a['nul']);
$a['k'] .= $a['k']; var_dump($a['k']);
$s = $a['k']; $a['k'] .= $s; var_dump($a['k'], $s);
$a['missing'] .= 'm'; var_dump($a['missing']);
$a[1.5] .= 'fl'; var_dump($a[1]);
$k = 'k'; $a[$k] .= $k; var_dump($a[$k]);
$a["k$k"] .= 'kk'; var_dump($a['kk']);
$r = 'ref'; $a['r'] = &$r; $a['r'] .= '!'; var_dump($r);
$ra = &$a; $ra['k'] .= 'R'; var_dump($a['k']);
$n = null; $n['x'] .= 'auto'; var_dump($n);
$str = 'abc'; try { $str[0] .= 'x'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$a['k'] = 'q'; $a['k'] .= [1];
$a['k'] .= 2.5; var_dump($a['k']);
class T { function __toString(): string { return 'T'; } }
$a['k'] .= new T; var_dump($a['k']);
$i = 7; $a['k'] .= $i; var_dump($a['k']);
$e = ['']; for ($j = 0; $j < 3; $j++) { $e[0] .= 'ab'; } var_dump($e);
$w = ['s' => 'w']; $x = ($w['s'] .= 'u'); var_dump($x, $w['s']);
$u['q'] .= 'undef'; var_dump($u);
$o = new stdClass; $o->p = ['s' => 'o']; $o->p['s'] .= 'p'; var_dump($o->p['s']);
$ao = new AO(['k' => 'v']); $ao['k'] .= 'w'; var_dump($ao['k']);
$int = 5; try { $int['x'] .= 'z'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump($int);
echo "end\n";
"#;
        let expected = r#"string(7) "a0x1x2x"
string(4) "c---"
string(7) "a0x1x2x"
string(8) "a0x1x2xy"
string(2) "1z"
string(4) "1.5w"
string(1) "v"
string(16) "a0x1x2xya0x1x2xy"
string(32) "a0x1x2xya0x1x2xya0x1x2xya0x1x2xy"
string(16) "a0x1x2xya0x1x2xy"

Warning: Undefined array key "missing" in t.php on line 9
string(1) "m"

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 10

Warning: Undefined array key 1 in t.php on line 10
string(2) "fl"
string(33) "a0x1x2xya0x1x2xya0x1x2xya0x1x2xyk"

Warning: Undefined array key "kk" in t.php on line 12
string(2) "kk"
string(4) "ref!"
string(34) "a0x1x2xya0x1x2xya0x1x2xya0x1x2xykR"

Warning: Undefined array key "x" in t.php on line 15
array(1) {
  ["x"]=>
  string(4) "auto"
}
Cannot use assign-op operators with string offsets

Warning: Array to string conversion in t.php on line 17
string(9) "qArray2.5"
string(10) "qArray2.5T"
string(11) "qArray2.5T7"
array(1) {
  [0]=>
  string(6) "ababab"
}
string(2) "wu"
string(2) "wu"

Warning: Undefined variable $u in t.php on line 24

Warning: Undefined array key "q" in t.php on line 24
array(1) {
  ["q"]=>
  string(5) "undef"
}
string(2) "op"
string(2) "vw"
Cannot use a scalar value as an array
int(5)
end
"#;
        assert_eq!(run(src), expected);
    }

    #[test]
    fn specialised_abs_computes_inline() {
        let src = r#"<?php
$x = -42;
$y = abs($x);
var_dump($y);
$f = -3.14;
$r = abs($f);
var_dump($r);
abs($x);
$min = -9223372036854775807 - 1;
var_dump(abs($min));
"#;
        let expected = "int(42)\nfloat(3.14)\nfloat(9.223372036854776E+18)\n";
        assert_eq!(run(src), expected);
    }

    /// The dimension ops and `.=` in a loop run in the frame-local hot loop
    /// (`exec/spec/dims.rs`): reads, writes, compound assignments,
    /// increments, `isset`, nested chains, string offsets, a container held
    /// by reference and copy-on-write, in a frame whose symbol table is
    /// attached (`$$name`), so every write is mirrored for
    /// `get_defined_vars()`.
    #[test]
    fn dimension_ops_in_the_hot_loop() {
        let src = r#"<?php
function f() {
    $a = [1, 2, 3]; $s = ''; $h = ['k' => 'v']; $n = 0;
    $name = 'a';
    $alias = $$name;
    for ($i = 0; $i < 6; $i++) {
        $a[$i % 3] = $i;
        $x = $a[$i % 3];
        $a[$i % 3] += 2;
        $a[1]++;
        $h['k'] .= $i;
        $s .= $i;
        if (isset($a[$i])) { $n++; }
    }
    $vars = get_defined_vars();
    var_dump($vars['a'], $vars['s'], $vars['h'], $vars['x'], $n, $alias);
    $m = [[1, 2], [3, 4]];
    for ($i = 0; $i < 4; $i++) { $m[$i & 1][$i >> 1] = $i * 10; $m[1][0] += 1; $y = $m[0][1]; }
    var_dump($m, $y);
    $str = 'hello';
    for ($i = -5; $i < 5; $i++) { $s .= $str[$i]; }
    var_dump($s);
    $r = [0, 0]; $ref = &$r;
    for ($i = 0; $i < 4; $i++) { $r[$i & 1] = $i; $ref[] = $i; }
    var_dump($r);
    $copy = $a; for ($i = 0; $i < 3; $i++) { $a[$i] = -$i; } var_dump($copy[2], $a[2]);
}
f();
"#;
        let expected = "array(3) {\n  [0]=>\n  int(5)\n  [1]=>\n  int(8)\n  [2]=>\n  int(7)\n}\nstring(6) \"012345\"\narray(1) {\n  [\"k\"]=>\n  string(7) \"v012345\"\n}\nint(5)\nint(3)\narray(3) {\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n  [2]=>\n  int(3)\n}\narray(2) {\n  [0]=>\n  array(2) {\n    [0]=>\n    int(0)\n    [1]=>\n    int(20)\n  }\n  [1]=>\n  array(2) {\n    [0]=>\n    int(13)\n    [1]=>\n    int(30)\n  }\n}\nint(20)\nstring(16) \"012345hellohello\"\narray(6) {\n  [0]=>\n  int(2)\n  [1]=>\n  int(3)\n  [2]=>\n  int(0)\n  [3]=>\n  int(1)\n  [4]=>\n  int(2)\n  [5]=>\n  int(3)\n}\nint(7)\nint(-2)\n";
        assert_eq!(run(src), expected);
    }

    /// `foreach` by value over arrays runs its fetch in the hot loop
    /// (packed with holes, hash, elements held by reference, appends while
    /// iterating the snapshot, `break`); by-reference, object and generator
    /// iteration decline to the generic op.
    #[test]
    fn foreach_by_value_in_the_hot_loop() {
        let src = r#"<?php
$p = [10, 20, 30, 40]; unset($p[1]);
$h = ['a' => 1, 'b' => 2.5, 'c' => 'x', 7 => null];
$x = 5; $r = [1, &$x, 3];
$s = '';
foreach ($p as $k => $v) { $s .= "$k=$v,"; }
foreach ($h as $k => $v) { $s .= $k . '=' . $v . ','; }
foreach ($r as $v) { $s .= $v; }
foreach ($p as $v) { $p[] = $v; if (count($p) > 9) break; }
var_dump($s, count($p));
foreach ($r as &$v) { $v *= 2; } unset($v);
var_dump($r, $x);
class O { public $a = 1; public $b = 2; }
function g() { yield 'k' => 3; yield 4; }
foreach (new O as $k => $v) { echo "$k:$v "; }
foreach (g() as $k => $v) { echo "$k:$v "; }
echo "\n";
$sum = 0; $big = [];
for ($i = 1; $i <= 100; $i++) { $big[] = $i; }
foreach ($big as $i => $v) { $sum += $v * $i; }
var_dump($sum);
"#;
        let expected = "string(35) \"0=10,2=30,3=40,a=1,b=2.5,c=x,7=,153\"\nint(6)\narray(3) {\n  [0]=>\n  int(2)\n  [1]=>\n  &int(10)\n  [2]=>\n  int(6)\n}\nint(10)\na:1 b:2 k:3 0:4 \nint(333300)\n";
        assert_eq!(run(src), expected);
    }
}
