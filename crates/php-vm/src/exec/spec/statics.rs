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
use super::{is_plain_value, plain_truth};
use crate::opcode::{BinKind, FetchMode, IncDecKind, Operand, PlaceBase, PlaceId};
use crate::rtcache::StaticPropCacheEntry;
use crate::vm::{Vm, VmResult};
use php_runtime::{BinaryOp, Value, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

#[allow(dead_code)]
impl Vm {
    /// The cached storage cell of the static-property place `pid`
    /// (`A::$name`, literal name) and whether the property is typed.
    #[inline]
    fn static_place_cell(&self, pid: PlaceId) -> Option<(Rc<RefCell<Value>>, bool)> {
        if let Some(entry) = self.static_prop_cache_probe(pid) {
            return Some((entry.cell, !entry.fast_plain));
        }
        let place = self.frame().code.places.get(pid as usize)?;
        let PlaceBase::StaticProp {
            class,
            name: Operand::Lit(name),
        } = &place.base
        else {
            return None;
        };
        let (cell, typed) = self.static_prop_cached(class, *name)?;
        self.static_prop_cache_fill(pid, class, *name, &cell, !typed);
        Some((cell, typed))
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
        if let Some(entry) = self.static_prop_cache_probe(pid) {
            if entry.fast_plain {
                let slot = entry.cell.borrow();
                return Some(match &*slot {
                    Value::Reference(r) => r.borrow().clone(),
                    v => v.clone(),
                });
            }
            let slot = entry.cell.try_borrow().ok()?;
            return match &*slot {
                Value::Null if mode != FetchMode::Is => None,
                Value::Reference(r) => r.try_borrow().map(|inner| inner.clone()),
                v => Some(v.clone()),
            };
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
        let cell = if let Some(entry) = self.static_prop_cache_probe(pid) {
            if !entry.fast_plain {
                return false;
            }
            entry.cell
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
        let cell = if let Some(entry) = self.static_prop_cache_probe(pid) {
            if !entry.fast_plain {
                return false;
            }
            entry.cell
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
        let cell = if let Some(entry) = self.static_prop_cache_probe(pid) {
            entry.cell
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

    /// `++`/`--` of static property `pid` on a cache hit.
    #[inline(always)]
    pub(in crate::exec) fn inc_dec_static_prop_fast(
        &self,
        pid: PlaceId,
        kind: IncDecKind,
    ) -> Option<Value> {
        let entry = if let Some(entry) = self.static_prop_cache_probe(pid) {
            entry
        } else {
            let (cell, typed) = self.static_place_cell(pid)?;
            StaticPropCacheEntry {
                scope_ptr: 0,
                called_ptr: 0,
                class_ptr: 0,
                cell,
                fast_plain: !typed,
            }
        };
        if !entry.fast_plain {
            return None;
        }
        let mut slot = entry.cell.try_borrow_mut().ok()?;
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
        let cell = if let Some(entry) = self.static_prop_cache_probe(pid) {
            entry.cell
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
        if let Some(entry) = self.static_prop_cache_probe(place) {
            if entry.fast_plain {
                let slot = entry.cell.borrow();
                let v = match &*slot {
                    Value::Reference(r) => r.borrow().clone(),
                    v => v.clone(),
                };
                drop(slot);
                if self.store_fetched(dst, to, v) {
                    *ip += 1;
                }
                return Ok(Flow::Next);
            }
        }
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
        if !(concat
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

    // Expected outputs are from `/usr/bin/php -n -d memory_limit=1G` (8.3).

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
}
