//! Handlers of the dedicated property ops ([`Op::FetchObjProp`],
//! [`Op::FetchObjPropDim`], [`Op::AssignObjProp`], [`Op::AssignObjPropDim`],
//! [`Op::AssignOpObjProp`], [`Op::AssignOpObjPropDim`], [`Op::IncDecObjProp`],
//! [`Op::IncDecObjPropDim`], [`Op::IssetObjProp`], [`Op::IssetObjPropDim`]):
//! `$this->p` and `$cv->p` with literal property names (and their one-dimension
//! or append forms) calling the property cache directly, skipping the place walk.
//!
//! On a cache hit the handler accesses the property's storage directly
//! (`place/propcache.rs`); anything the cache declines (magic methods, hooks,
//! readonly or visibility errors, typed-property coercion, references where
//! they matter, diagnostics) falls back to the generic place walk unchanged.
//!
//! [`Op::FetchObjProp`]: crate::opcode::Op::FetchObjProp
//! [`Op::FetchObjPropDim`]: crate::opcode::Op::FetchObjPropDim
//! [`Op::AssignObjProp`]: crate::opcode::Op::AssignObjProp
//! [`Op::AssignObjPropDim`]: crate::opcode::Op::AssignObjPropDim
//! [`Op::AssignOpObjProp`]: crate::opcode::Op::AssignOpObjProp
//! [`Op::AssignOpObjPropDim`]: crate::opcode::Op::AssignOpObjPropDim
//! [`Op::IncDecObjProp`]: crate::opcode::Op::IncDecObjProp
//! [`Op::IncDecObjPropDim`]: crate::opcode::Op::IncDecObjPropDim
//! [`Op::IssetObjProp`]: crate::opcode::Op::IssetObjProp
//! [`Op::IssetObjPropDim`]: crate::opcode::Op::IssetObjPropDim

use super::super::{Flow, Num};
use crate::opcode::{FetchMode, IncDecKind, Operand, PlaceId};
use crate::vm::{Vm, VmResult};
use php_runtime::{BinaryOp, Value};

impl Vm {
    /// `Fetch` of `$cv->name` / `$this->name` on a property cache hit:
    /// reads the slot directly, folds the following `AssignCv` when `to` is
    /// set, and returns whether `to` was written (`Some(true)`: the loop skips
    /// that op) or `dst` was (`Some(false)`). `None` on a cache miss (or
    /// `FetchMode::List`).
    #[inline]
    pub(in crate::exec) fn fetch_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    ) -> Option<bool> {
        if mode == FetchMode::List {
            return None;
        }
        let v = self.prop_cache_read(pid)?;
        Some(self.store_fetched(dst, to, v))
    }

    /// `Fetch` of `$cv->name[dim]` / `$this->name[dim]` on a property cache
    /// hit: reads the array element directly; `to` as in
    /// [`Vm::fetch_obj_prop_fast`].
    #[inline]
    pub(in crate::exec) fn fetch_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    ) -> Option<bool> {
        if mode == FetchMode::List {
            return None;
        }
        let key = self.peek_dim_key(dim)?;
        let v = self.prop_cache_read_elem(pid, &key)?;
        self.release_tmp_str(dim);
        Some(self.store_fetched(dst, to, v))
    }

    /// `$cv->name = val` / `$this->name = val` on a property cache hit:
    /// writes a plain value directly into the property slot. Returns `true`
    /// on hit, `false` otherwise.
    #[inline]
    pub(in crate::exec) fn assign_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let Some(v) = self.peek_plain_value(val) else {
            return false;
        };
        match self.prop_cache_assign(pid, v) {
            Ok(r) => {
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// `$cv->name[dim] = val` / `$this->name[dim] = val` (or append `[]`
    /// when `dim` is [`Operand::Unused`]) on a property cache hit.
    #[inline]
    pub(in crate::exec) fn assign_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let key = match dim {
            Operand::Unused => None,
            other => Some(match self.peek_dim_key(other) {
                Some(k) => k,
                None => return false,
            }),
        };
        let Some(v) = self.peek_plain_value(val) else {
            return false;
        };
        match self.prop_cache_assign_elem(pid, key.as_ref(), v) {
            Ok(r) => {
                self.release_tmp_str(dim);
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// `$cv->name op= val` / `$this->name op= val` on a property cache hit:
    /// `.=` on a string property, or a numeric op on a number property.
    #[inline]
    pub(in crate::exec) fn assign_op_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        if op == BinaryOp::Concat {
            let Some(rhs) = self.peek_plain_value(val) else {
                return false;
            };
            if let Some(r) = self.prop_cache_concat(pid, &rhs) {
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                return true;
            }
            return false;
        }
        let Some(rhs) = Self::peek_num(self.frame(), val) else {
            return false;
        };
        let rhs_val = match rhs {
            Num::Int(i) => Value::Int(i),
            Num::Float(f) => Value::Float(f),
        };
        if let Some(r) = self.prop_cache_assign_op_pure(pid, op, &rhs_val) {
            if let Operand::Tmp(t) = val {
                self.set_tmp(t, Value::Null);
            }
            if let Some(d) = dst {
                self.set_tmp(d, r);
            }
            return true;
        }
        false
    }

    /// `$cv->name[dim] op= val` / `$this->name[dim] op= val` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn assign_op_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let Some(key) = self.peek_dim_key(dim) else {
            return false;
        };
        let Some(rhs) = self.peek_plain_value(val) else {
            return false;
        };
        if let Some(r) = self.prop_cache_assign_op_elem(pid, &key, op, &rhs) {
            self.release_tmp_str(dim);
            if let Operand::Tmp(t) = val {
                self.set_tmp(t, Value::Null);
            }
            if let Some(d) = dst {
                self.set_tmp(d, r);
            }
            return true;
        }
        false
    }

    /// `++`/`--` of `$cv->name` / `$this->name` on a property cache hit.
    #[inline]
    pub(in crate::exec) fn incdec_obj_prop_fast(
        &self,
        pid: PlaceId,
        kind: IncDecKind,
    ) -> Option<Value> {
        self.prop_cache_incdec(pid, kind)
    }

    /// `++`/`--` of `$cv->name[dim]` / `$this->name[dim]` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn incdec_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        kind: IncDecKind,
    ) -> Option<Value> {
        let key = self.peek_dim_key(dim)?;
        let r = self.prop_cache_incdec_elem(pid, &key, kind)?;
        self.release_tmp_str(dim);
        Some(r)
    }

    /// `isset($cv->name)` / `empty($cv->name)` on a property cache hit.
    #[inline]
    pub(in crate::exec) fn isset_obj_prop_fast(&self, pid: PlaceId, empty: bool) -> Option<bool> {
        self.prop_cache_isset_pure(pid, empty)
    }

    /// `isset($cv->name[dim])` / `empty($cv->name[dim])` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn isset_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        empty: bool,
    ) -> Option<bool> {
        let key = self.peek_dim_key(dim)?;
        let r = self.prop_cache_isset_elem_pure(pid, &key, empty)?;
        self.release_tmp_str(dim);
        Some(r)
    }

    /// Handler for [`Op::FetchObjProp`].
    pub(in crate::exec) fn op_fetch_obj_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
        ip: &mut usize,
    ) -> VmResult<Flow> {
        if let Some(skipped) = self.fetch_obj_prop_fast(place, dst, to, mode) {
            if skipped {
                *ip += 1;
            }
            return Ok(Flow::Next);
        }
        let v = self.place_fetch(place, mode)?;
        self.set_tmp(dst, v);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::FetchObjPropDim`].
    pub(in crate::exec) fn op_fetch_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
        ip: &mut usize,
    ) -> VmResult<Flow> {
        if let Some(skipped) = self.fetch_obj_prop_dim_fast(place, dim, dst, to, mode) {
            if skipped {
                *ip += 1;
            }
            return Ok(Flow::Next);
        }
        let v = self.place_fetch(place, mode)?;
        self.set_tmp(dst, v);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignObjProp`].
    pub(in crate::exec) fn op_assign_obj_prop(
        &mut self,
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_obj_prop_fast(place, val, dst) {
            return Ok(Flow::Next);
        }
        let v = self.read_raw(val)?;
        let r = match self.prop_cache_assign(place, v) {
            Ok(r) => r,
            Err(v) => self.place_assign(place, v)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignObjPropDim`].
    pub(in crate::exec) fn op_assign_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_obj_prop_dim_fast(place, dim, val, dst) {
            return Ok(Flow::Next);
        }
        let key = match dim {
            Operand::Unused => None,
            other => self.peek_dim_key(other),
        };
        let v = self.read_raw(val)?;
        let r = if dim == Operand::Unused || key.is_some() {
            match self.prop_cache_assign_elem(place, key.as_ref(), v) {
                Ok(r) => {
                    self.release_tmp_str(dim);
                    r
                }
                Err(v) => self.place_assign(place, v)?,
            }
        } else {
            self.place_assign(place, v)?
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignOpObjProp`].
    pub(in crate::exec) fn op_assign_op_obj_prop(
        &mut self,
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_op_obj_prop_fast(place, op, val, dst) {
            return Ok(Flow::Next);
        }
        let v = self.read_raw(val)?;
        let r = if op == BinaryOp::Concat {
            match self.prop_cache_concat(place, &v) {
                Some(r) => r,
                None => self.place_assign_op(place, op, v)?,
            }
        } else {
            match self.prop_cache_assign_op(place, op, &v)? {
                Some(r) => r,
                None => self.place_assign_op(place, op, v)?,
            }
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignOpObjPropDim`].
    pub(in crate::exec) fn op_assign_op_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_op_obj_prop_dim_fast(place, dim, op, val, dst) {
            return Ok(Flow::Next);
        }
        let key = self.peek_dim_key(dim);
        let v = self.read_raw(val)?;
        let r = if let Some(key) = key {
            match self.prop_cache_assign_op_elem(place, &key, op, &v) {
                Some(r) => {
                    self.release_tmp_str(dim);
                    r
                }
                None => self.place_assign_op(place, op, v)?,
            }
        } else {
            self.place_assign_op(place, op, v)?
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IncDecObjProp`].
    pub(in crate::exec) fn op_incdec_obj_prop(
        &mut self,
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        let r = match self.incdec_obj_prop_fast(place, kind) {
            Some(r) => r,
            None => self.place_incdec(place, kind)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IncDecObjPropDim`].
    pub(in crate::exec) fn op_incdec_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        let r = match self.incdec_obj_prop_dim_fast(place, dim, kind) {
            Some(r) => r,
            None => self.place_incdec(place, kind)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IssetObjProp`].
    pub(in crate::exec) fn op_isset_obj_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        empty: bool,
    ) -> VmResult<Flow> {
        let v = match self.isset_obj_prop_fast(place, empty) {
            Some(v) => v,
            None => match self.prop_cache_isset(place, empty)? {
                Some(v) => v,
                None => self.place_isset(place, empty)?,
            },
        };
        self.set_tmp(dst, Value::Bool(v));
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IssetObjPropDim`].
    pub(in crate::exec) fn op_isset_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        dst: u32,
        empty: bool,
    ) -> VmResult<Flow> {
        let v = match self.isset_obj_prop_dim_fast(place, dim, empty) {
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
        let mut vm = Vm::new(VmOptions {
            output: Box::new(sink.clone()),
            ..Default::default()
        });
        vm.run_script("test.php", src.as_bytes()).unwrap();
        let bytes = sink.0.borrow().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn dedicated_property_ops_execute_correctly() {
        let out = run(r#"<?php
class Test {
    public $p = 10;
    public $arr = ['a' => 1, 'b' => 'hello'];
    public $str = 'foo';
}

$o = new Test();
// Read
echo $o->p, "\n";
echo $o->arr['a'], "\n";
echo $o->arr['b'], "\n";

// Write
$o->p = 20;
echo $o->p, "\n";
$o->arr['a'] = 42;
echo $o->arr['a'], "\n";
$o->arr[] = 100;
echo $o->arr[0], "\n";

// Assign-op
$o->p += 5;
echo $o->p, "\n";
$o->p /= 5;
echo $o->p, "\n";
$o->str .= 'bar';
echo $o->str, "\n";
$o->arr['a'] += 8;
echo $o->arr['a'], "\n";

// Inc/dec
$o->p++;
echo $o->p, "\n";
$o->arr['a']--;
echo $o->arr['a'], "\n";

// Isset/empty
echo isset($o->p) ? "1\n" : "0\n";
echo empty($o->p) ? "1\n" : "0\n";
echo isset($o->arr['a']) ? "1\n" : "0\n";
echo empty($o->arr['a']) ? "1\n" : "0\n";
echo isset($o->nonexistent) ? "1\n" : "0\n";
"#);

        assert_eq!(
            out,
            "10\n1\nhello\n20\n42\n100\n25\n5\nfoobar\n50\n6\n49\n1\n0\n1\n0\n0\n"
        );
    }
}
