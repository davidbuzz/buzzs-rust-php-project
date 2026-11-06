//! Fast paths of the specialised ops ([`Op::CmpJmp`], [`Op::BinAssignCv`],
//! [`Op::IncDecCv`], [`Op::FetchDimCv`], [`Op::AssignDimCv`]) and of
//! `AssignCv`: the typed handlers php-src generates from the `SPEC(...)`
//! annotations of `Zend/zend_vm_def.h` (`ZEND_IS_SMALLER_LONG_SPEC`,
//! `ZEND_PRE_INC_LONG_SPEC_CV`, `ZEND_FETCH_DIM_R_SPEC_CV_*`,
//! `ZEND_ASSIGN_DIM_SPEC_CV_*_OP_DATA_*`, `ZEND_ASSIGN_SPEC_CV_*`).
//!
//! Each fast path either completes the op without any diagnostic, or
//! changes nothing and reports that the generic code must run (which then
//! sees exactly the state the generic op would have seen). See
//! `compiler/specialise.rs` for when the compiler selects these ops.
//!
//! [`Op::CmpJmp`]: crate::opcode::Op::CmpJmp
//! [`Op::BinAssignCv`]: crate::opcode::Op::BinAssignCv
//! [`Op::IncDecCv`]: crate::opcode::Op::IncDecCv
//! [`Op::FetchDimCv`]: crate::opcode::Op::FetchDimCv
//! [`Op::AssignDimCv`]: crate::opcode::Op::AssignDimCv

use super::{CvOpData, Num};
use crate::opcode::{BinKind, FetchMode, IncDecKind, Operand, PlaceId};
use crate::vm::{Frame, VarStore, Vm, VmResult};
use php_runtime::{ArrayKey, BinaryOp, Value, ZStr};

/// Whether replacing `v` needs no destructor, GC root buffering or
/// reference semantics: no value, or null, bool, int, float or string
/// (`!Z_REFCOUNTED` in php-src, plus strings, whose release has no side
/// effect).
#[inline]
fn is_plain_slot(v: Option<&Value>) -> bool {
    matches!(
        v,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_))
    )
}

/// Whether `v` is a scalar or string value that can be stored into an
/// array element without the generic write walk.
#[inline]
fn is_plain_value(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

/// The value of an operand for a comparison without conversions: a
/// defined compiled variable, a literal or a temporary (left in place), not
/// holding a reference.
#[inline]
fn operand_simple<'a>(f: &'a Frame, vars: &'a VarStore, op: Operand) -> Option<&'a Value> {
    let v = match op {
        Operand::Cv(cv) => vars.slots.get(f.cv_idx(cv))?.as_ref()?,
        Operand::Lit(i) => f.code.literals.get(i as usize)?,
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Unused => return None,
    };
    match v {
        Value::Reference(_) => None,
        v => Some(v),
    }
}

impl Vm {
    /// `zend_is_true` of compiled variable `cv` when it needs neither a
    /// diagnostic nor a copy (the type switch inlined into
    /// `ZEND_JMPZ_SPEC_CV`/`ZEND_JMPNZ_SPEC_CV`): null, bool, int, a
    /// non-NaN float (NaN's coercion warns), a string or an array.
    /// References, objects, resources and undefined variables go generic.
    #[inline]
    pub(super) fn peek_cv_bool(&self, cv: u32) -> Option<bool> {
        let f = self.frame();
        let vars = f.vars.borrow();
        match vars.slots.get(f.cv_idx(cv))?.as_ref()? {
            Value::Null => Some(false),
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            Value::Float(x) if !x.is_nan() => Some(*x != 0.0),
            Value::Str(s) => Some(!(s.is_empty() || s.as_bytes() == b"0")),
            Value::Array(a) => Some(!a.is_empty()),
            _ => None,
        }
    }

    /// `==`, `!=`, `===`, `!==` on operands that are not both numbers,
    /// without conversions (`zend_is_identical`, the byte-equal case of
    /// `zend_fast_equal_strings`): same-type null, bool and string pairs,
    /// and `===`/`!==` of two different types (false/true). Byte-unequal
    /// strings under `==`/`!=` may still be numerically equal and go
    /// generic, as do arrays, objects, resources and references. A
    /// consumed temporary holding a string is released, as the generic
    /// read would (`FREE_OP1`/`FREE_OP2`).
    #[inline(never)]
    pub(super) fn cmp_simple_fast(&mut self, op: BinKind, a: Operand, b: Operand) -> Option<bool> {
        let identity = match op {
            BinKind::Identical | BinKind::NotIdentical => true,
            BinKind::Equal | BinKind::NotEqual => false,
            _ => return None,
        };
        let negate = matches!(op, BinKind::NotIdentical | BinKind::NotEqual);
        let same = {
            let f = self.frame();
            let vars = f.vars.borrow();
            let x = operand_simple(f, &vars, a)?;
            let y = operand_simple(f, &vars, b)?;
            match (x, y) {
                (Value::Null, Value::Null) => true,
                (Value::Bool(p), Value::Bool(q)) => p == q,
                (Value::Str(s), Value::Str(t)) => {
                    let eq = ZStr::ptr_eq(s, t) || s.as_bytes() == t.as_bytes();
                    if !eq && !identity {
                        return None;
                    }
                    eq
                }
                (x, y) => {
                    if !identity || std::mem::discriminant(x) == std::mem::discriminant(y) {
                        return None;
                    }
                    false
                }
            }
        };
        self.release_tmp_str(a);
        self.release_tmp_str(b);
        Some(same != negate)
    }

    /// Releases the string a consumed temporary operand holds; other
    /// temporaries are left in place (see [`Vm::peek_num`]).
    #[inline]
    fn release_tmp_str(&mut self, op: Operand) {
        if let Operand::Tmp(t) = op {
            if let Some(slot) = self.frame_mut().tmps.get_mut(t as usize) {
                if matches!(slot, Value::Str(_)) {
                    *slot = Value::Null;
                }
            }
        }
    }

    /// The comparison of a smart branch ([`crate::opcode::Op::CmpJmp`])
    /// when both operands are plain numbers: the `IS_LONG`/`IS_DOUBLE`
    /// pairs of `ZEND_IS_EQUAL`, `ZEND_IS_IDENTICAL`, `ZEND_IS_SMALLER`
    /// and `ZEND_IS_SMALLER_OR_EQUAL` (mixed pairs compare as doubles;
    /// `===` on a mixed pair is a type mismatch). The same results as
    /// [`Vm::bin_num_fast`], without building a `Value`.
    #[inline]
    #[allow(clippy::float_cmp)] // PHP compares doubles exactly
    pub(super) fn cmp_num_fast(&self, op: BinKind, a: Operand, b: Operand) -> Option<bool> {
        let x = self.peek_num(a)?;
        let y = self.peek_num(b)?;
        if let (Num::Int(x), Num::Int(y)) = (x, y) {
            return Some(match op {
                BinKind::Equal | BinKind::Identical => x == y,
                BinKind::NotEqual | BinKind::NotIdentical => x != y,
                BinKind::Less => x < y,
                BinKind::LessEqual => x <= y,
                _ => return None,
            });
        }
        let mixed = matches!(
            (x, y),
            (Num::Int(_), Num::Float(_)) | (Num::Float(_), Num::Int(_))
        );
        let (xf, yf) = (x.as_f64(), y.as_f64());
        Some(match op {
            BinKind::Equal => xf == yf,
            BinKind::NotEqual => xf != yf,
            BinKind::Identical => !mixed && xf == yf,
            BinKind::NotIdentical => mixed || xf != yf,
            BinKind::Less => xf < yf,
            BinKind::LessEqual => xf <= yf,
            _ => return None,
        })
    }

    /// Stores `v` (never a reference) into compiled variable `cv` when the
    /// variable is undefined or holds a plain value (see
    /// [`is_plain_slot`]): the `zend_assign_to_variable` path for a
    /// variable that is neither a reference nor refcounted. Gives `v` back
    /// otherwise, for the generic assignment.
    #[inline]
    pub(super) fn cv_store_plain(&self, cv: u32, v: Value) -> Result<(), Value> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        let Some(slot) = vars.slots.get_mut(idx) else {
            return Err(v);
        };
        match slot {
            None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                // The old value has no drop glue to run.
                std::mem::forget(std::mem::replace(slot, Some(v)));
                Ok(())
            }
            Some(Value::Str(_)) => {
                let old = std::mem::replace(slot, Some(v));
                drop(vars);
                drop(old);
                Ok(())
            }
            _ => Err(v),
        }
    }

    /// [`crate::opcode::Op::BinAssignCv`]: whether the result was stored
    /// into `cv` (the `AssignCv` after the op is then skipped). Otherwise
    /// the result is in `dst`.
    #[inline]
    pub(super) fn op_bin_assign_cv(
        &mut self,
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
        cv: u32,
    ) -> VmResult<bool> {
        let r = match self.bin_num_fast(op, a, b) {
            Some(r) => r,
            None => match self.cmp_simple_fast(op, a, b) {
                Some(r) => Value::Bool(r),
                None => {
                    self.bin_slow(op, a, b, dst)?;
                    return Ok(false);
                }
            },
        };
        match self.cv_store_plain(cv, r) {
            Ok(()) => Ok(true),
            Err(r) => {
                self.set_tmp(dst, r);
                Ok(false)
            }
        }
    }

    /// `++`/`--` of compiled variable `cv` holding an int that does not
    /// overflow, or a float (`ZEND_PRE_INC_LONG_SPEC_CV` & co.,
    /// `increment_function` on `IS_DOUBLE`): the result of the expression,
    /// or `None` (nothing changed) for the generic path.
    #[inline]
    pub(super) fn incdec_cv_fast(&self, cv: u32, kind: IncDecKind) -> Option<Value> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        match vars.slots.get_mut(idx)? {
            Some(Value::Int(i)) => {
                let old = *i;
                let new = if kind.is_inc() {
                    old.checked_add(1)?
                } else {
                    old.checked_sub(1)?
                };
                *i = new;
                Some(Value::Int(if kind.is_post() { old } else { new }))
            }
            Some(Value::Float(x)) => {
                let old = *x;
                let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
                *x = new;
                Some(Value::Float(if kind.is_post() { old } else { new }))
            }
            _ => None,
        }
    }

    /// [`crate::opcode::Op::IncDecCv`].
    #[inline]
    pub(super) fn op_incdec_cv(
        &mut self,
        cv: u32,
        kind: IncDecKind,
        dst: Option<u32>,
        place: PlaceId,
    ) -> VmResult<()> {
        if let Some(r) = self.incdec_cv_fast(cv, kind) {
            self.set_dst(dst, r);
            return Ok(());
        }
        let r = self.place_incdec(place, kind)?;
        self.set_dst(dst, r);
        Ok(())
    }

    /// [`crate::opcode::Op::AssignOpCv`]: `$cv op= val` in place when both
    /// hold plain numbers and the result stays in the int domain (or is a
    /// float); otherwise the generic `AssignOp`.
    #[inline]
    pub(super) fn op_assign_op_cv(
        &mut self,
        cv: u32,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
        place: PlaceId,
    ) -> VmResult<()> {
        // `peek_num` on the CV only succeeds for a non-reference int or
        // float, which the store below overwrites without side effects.
        if let Some(r) = self.bin_num_fast(BinKind::Arith(op), Operand::Cv(cv), val) {
            match dst {
                None => {
                    if self.cv_store_plain(cv, r).is_ok() {
                        return Ok(());
                    }
                }
                Some(d) => {
                    if self.cv_store_plain(cv, r.clone()).is_ok() {
                        self.set_tmp(d, r);
                        return Ok(());
                    }
                }
            }
        }
        let v = self.read(val)?;
        let r = self.place_assign_op(place, op, v)?;
        self.set_dst(dst, r);
        Ok(())
    }

    /// The array key an offset operand converts to without diagnostics:
    /// an int, or a string (symbol-table semantics, `"1"` is key 1). A
    /// consumed temporary only qualifies holding an int, which can stay in
    /// its slot (like [`Vm::peek_num`]); a string there must be released
    /// by the generic read.
    #[inline]
    fn peek_dim_key(&self, dim: Operand) -> Option<ArrayKey> {
        let f = self.frame();
        match dim {
            Operand::Tmp(t) => match f.tmps.get(t as usize)? {
                Value::Int(i) => Some(ArrayKey::Int(*i)),
                _ => None,
            },
            Operand::TmpKeep(t) => match f.tmps.get(t as usize)? {
                Value::Int(i) => Some(ArrayKey::Int(*i)),
                Value::Str(s) => Some(ArrayKey::from_zstr(s)),
                _ => None,
            },
            Operand::Lit(i) => match f.code.literals.get(i as usize)? {
                Value::Int(i) => Some(ArrayKey::Int(*i)),
                Value::Str(s) => Some(ArrayKey::from_zstr(s)),
                _ => None,
            },
            Operand::Cv(cv) => {
                let idx = f.cv_idx(cv);
                let vars = f.vars.borrow();
                match vars.slots.get(idx)? {
                    Some(Value::Int(i)) => Some(ArrayKey::Int(*i)),
                    Some(Value::Str(s)) => Some(ArrayKey::from_zstr(s)),
                    _ => None,
                }
            }
            Operand::Unused => None,
        }
    }

    /// `$cv[dim]` read when `$cv` holds an array (directly or through a
    /// reference) that has the key: `zend_fetch_dimension_address_inner`
    /// with `BP_VAR_R` on a hit. `None` (a miss, an offset that needs
    /// converting, a non-array) leaves everything to the generic fetch.
    #[inline]
    fn fetch_dim_cv_fast(&self, cv: u32, dim: Operand) -> Option<Value> {
        let key = self.peek_dim_key(dim)?;
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let vars = f.vars.borrow();
        match vars.slots.get(idx)? {
            Some(Value::Array(a)) => a.get(&key).map(Value::deref),
            Some(Value::Reference(r)) => {
                let inner = r.try_borrow()?;
                match &*inner {
                    Value::Array(a) => a.get(&key).map(Value::deref),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// [`crate::opcode::Op::FetchDimCv`]: whether the `AssignCv` after the
    /// op was done here too (and is to be skipped).
    #[inline]
    pub(super) fn op_fetch_dim_cv(
        &mut self,
        cv: u32,
        dim: Operand,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
    ) -> VmResult<bool> {
        let Some(v) = self.fetch_dim_cv_fast(cv, dim) else {
            // The generic fetch may warn or call user code: the
            // `AssignCv` then runs as its own op, after the safe point.
            let v = self.place_fetch(place, FetchMode::Read)?;
            self.set_tmp(dst, v);
            return Ok(false);
        };
        if let Some(to) = to {
            match self.cv_store_plain(to, v) {
                Ok(()) => return Ok(true),
                Err(v) => self.set_tmp(dst, v),
            }
        } else {
            self.set_tmp(dst, v);
        }
        Ok(false)
    }

    /// A plain value (see [`is_plain_value`]) an `OP_DATA` operand holds,
    /// without consuming it: a literal, a defined non-reference CV or a
    /// temporary.
    #[inline]
    fn peek_plain_value(&self, op: Operand) -> Option<Value> {
        let f = self.frame();
        match op {
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                let v = f.tmps.get(t as usize)?;
                is_plain_value(v).then(|| v.clone())
            }
            Operand::Lit(i) => {
                let v = f.code.literals.get(i as usize)?;
                is_plain_value(v).then(|| v.clone())
            }
            Operand::Cv(cv) => {
                let idx = f.cv_idx(cv);
                let vars = f.vars.borrow();
                let v = vars.slots.get(idx)?.as_ref()?;
                is_plain_value(v).then(|| v.clone())
            }
            Operand::Unused => None,
        }
    }

    /// `$cv[dim] = val;` when `$cv` holds an array (not a reference), the
    /// offset is an int or string, the value is plain and the element it
    /// replaces (if any) is plain: the `zend_fetch_dimension_address_inner_W`
    /// + `zend_assign_to_variable` path of `ZEND_ASSIGN_DIM` on an array,
    /// separating a shared array first (`SEPARATE_ARRAY`). Returns false,
    /// having changed nothing, otherwise.
    #[inline]
    fn assign_dim_cv_fast(&mut self, cv: u32, dim: Operand, val: Operand) -> bool {
        let Some(key) = self.peek_dim_key(dim) else {
            return false;
        };
        let Some(v) = self.peek_plain_value(val) else {
            return false;
        };
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        let Some(Some(Value::Array(arr))) = vars.slots.get_mut(idx) else {
            return false;
        };
        if !is_plain_slot(arr.get(&key)) {
            return false;
        }
        // The same write fetch as the generic walk (`descend`): the key is
        // looked up, or added as null, then overwritten.
        let (slot, _) = arr.make_mut().lookup_ex(key);
        let old = std::mem::replace(slot, v);
        drop(vars);
        drop(old);
        // The OP_DATA temporary is consumed.
        if let Operand::Tmp(t) = val {
            self.set_tmp(t, Value::Null);
        }
        true
    }

    /// [`crate::opcode::Op::AssignDimCv`].
    #[inline]
    pub(super) fn op_assign_dim_cv(
        &mut self,
        cv: u32,
        dim: Operand,
        val: Operand,
        place: PlaceId,
    ) -> VmResult<()> {
        if self.assign_dim_cv_fast(cv, dim, val) {
            return Ok(());
        }
        self.assign_place_unused(place, val)?;
        Ok(())
    }

    /// The generic `Op::Assign { place, val, dst: None }`.
    #[inline(never)]
    fn assign_place_unused(&mut self, place: PlaceId, val: Operand) -> VmResult<Value> {
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

    // Expected outputs below are from `/usr/bin/php -n` (8.3).

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
        let src = r#"<?php
$i = 1; $a = $i++; $b = ++$i; $c = $i--; $d = --$i;
var_dump($i, $a, $b, $c, $d);
$f = 1.5; $f++; ++$f; var_dump($f); $f--; var_dump($f);
$m = PHP_INT_MAX; $m++; var_dump($m);
$n = null; $n++; var_dump($n);
$r = 5; $q = &$r; $r++; var_dump($q);
$u++; var_dump($u);
"#;
        let out = run(src);
        let expected_head = "int(1)\nint(1)\nint(3)\nint(3)\nint(1)\nfloat(3.5)\nfloat(2.5)\nfloat(9.223372036854776E+18)\nint(1)\nint(6)\n";
        assert!(out.starts_with(expected_head), "{out}");
        assert!(out.contains("Undefined variable $u"), "{out}");
        assert!(out.ends_with("int(1)\n"), "{out}");
    }

    #[test]
    fn dim_reads_on_cvs() {
        let src = r#"<?php
$a = range(0, 9); $s = 0;
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
    fn dim_writes_on_cvs() {
        let src = r#"<?php
$a = array_fill(0, 4, 0);
for ($i = 0; $i < 10; $i++) { $a[$i & 3] = $i; }
var_dump($a);
$b = $a; $b[0] = 'x'; var_dump($a[0], $b[0]);
$h = []; $h['k'] = 1.5; $h['3'] = true; $h[] = null; var_dump($h);
$o = [new ArrayObject([])]; $o[0] = 1; var_dump($o);
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
}
