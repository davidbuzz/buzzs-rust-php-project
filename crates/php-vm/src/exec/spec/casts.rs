// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frame-local fast path of `(int)`, `(float)`, `(bool)` and `(array)`
//! ([`Op::Cast`](crate::opcode::Op::Cast); `(string)` is
//! `strings::cast_string_in`): php-src's `ZEND_CAST` with
//! `zval_get_long`, `zval_get_double`, `zend_is_true` and the array cast
//! of `Zend/zend_vm_def.h` on the operand types that convert without a
//! diagnostic, user code or allocation beyond the result.
//!
//! ref: Zend/zend_vm_def.h `ZEND_CAST`, Zend/zend_operators.c
//! (`zval_get_long_func`, `zend_dval_to_lval`, `ZEND_STRTOL` via
//! `_is_numeric_string_ex`).

use super::plain_truth;
use crate::opcode::{CastKind, Operand};
use crate::vm::{CvSlot, Frame};
use php_runtime::convert::{double_fits_long, dval_to_lval_cap, dval_to_lval_silent};
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{Value, ZArray};

/// `zval_get_long` of `v` when it needs no diagnostic: null, a bool, an
/// int, a float that fits an int (`zend_dval_to_lval`; NaN, the
/// infinities and floats out of range warn on the generic path), a string
/// (leading-numeric digits, `0` for none: `ZEND_STRTOL` semantics of
/// `_is_numeric_string_ex` with `allow_errors`, silent in a cast) or an
/// array (`0` / `1`). `None` for anything else.
fn long_of(v: &Value) -> Option<i64> {
    Some(match v {
        Value::Null | Value::Bool(false) => 0,
        Value::Bool(true) => 1,
        Value::Int(i) => *i,
        Value::Float(d) if d.is_finite() && double_fits_long(*d) => dval_to_lval_silent(*d),
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
            None => 0,
            Some(n) => match n.value {
                Number::Int(i) => i,
                Number::Float(d) => dval_to_lval_cap(d),
            },
        },
        Value::Array(a) => i64::from(!a.is_empty()),
        _ => return None,
    })
}

/// `(kind) v` for the kinds and operand types this path takes; `None`
/// for anything else (an object, a resource, a string to float, a NaN to
/// array: the generic `Vm::cast`).
#[allow(clippy::cast_precision_loss)] // `(float)` of an int is `(double) lval`
fn cast_of(kind: CastKind, v: &Value) -> Option<Value> {
    Some(match kind {
        CastKind::Int => Value::Int(long_of(v)?),
        CastKind::Float => Value::Float(match v {
            Value::Null | Value::Bool(false) => 0.0,
            Value::Bool(true) => 1.0,
            Value::Int(i) => *i as f64,
            Value::Float(d) => *d,
            _ => return None,
        }),
        CastKind::Bool => Value::Bool(plain_truth(v)?),
        CastKind::Array => match v {
            Value::Array(_) => v.clone(),
            Value::Null => Value::Array(ZArray::new()),
            Value::Float(d) if d.is_nan() => return None,
            Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => {
                Value::Array(ZArray::from_values([v.clone()]))
            }
            _ => return None,
        },
        CastKind::String | CastKind::Object | CastKind::Null => return None,
    })
}

/// [`Op::Cast`](crate::opcode::Op::Cast) `(int)`, `(float)`, `(bool)` or
/// `(array)` of operand `a` of frame `f` into temporary `dst`: the operand
/// read as `Vm::read` reads it (a defined variable, through a reference;
/// a literal; a temporary, consumed: its string or array is released, or
/// moved for `(array)` of an array) and converted by [`cast_of`].
/// `false`, nothing changed, for anything else (an undefined variable and
/// its warning, the conversions that warn, objects).
#[inline(never)]
pub(in crate::exec) fn cast_in(f: &mut Frame, kind: CastKind, a: Operand, dst: u32) -> bool {
    if f.tmps.len() <= dst as usize {
        return false;
    }
    let r = match a {
        Operand::Cv(cv) => match f.cvs.get(cv as usize).and_then(CvSlot::get) {
            Some(Value::Reference(r)) => match r.try_borrow() {
                Some(inner) => cast_of(kind, &inner),
                None => return false,
            },
            Some(v) => cast_of(kind, v),
            None => return false,
        },
        Operand::Lit(l) => match f.code.literals.get(l as usize) {
            Some(v) => cast_of(kind, v),
            None => return false,
        },
        Operand::Tmp(t) | Operand::TmpKeep(t) => {
            let consumed = matches!(a, Operand::Tmp(_));
            let Some(v) = f.tmps.get(t as usize) else {
                return false;
            };
            match v {
                Value::Reference(_) => return false,
                // `(array)` of a consumed array: the array itself.
                Value::Array(_) if kind == CastKind::Array && consumed => {
                    let v = std::mem::take(&mut f.tmps[t as usize]);
                    super::put_tmp(&mut f.tmps[dst as usize], v);
                    return true;
                }
                // A consumed array whose last handle this is: its release
                // could free objects (left to the generic op).
                Value::Array(arr) if consumed && arr.refcount() < 2 => return false,
                v => cast_of(kind, v),
            }
        }
        Operand::Unused => return false,
    };
    let Some(r) = r else {
        return false;
    };
    if let Operand::Tmp(t) = a {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            // `FREE_OP1`: a string or an array (a count the result may
            // still hold) is released.
            drop(std::mem::take(slot));
        }
    }
    super::put_tmp(&mut f.tmps[dst as usize], r);
    true
}
