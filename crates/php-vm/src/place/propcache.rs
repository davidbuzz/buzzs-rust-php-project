//! Per-place runtime cache of declared property slots: php-src's
//! `CACHE_ADDR(opline->extended_value)` slot of `ZEND_FETCH_OBJ_R`/`_IS`,
//! `ZEND_ASSIGN_OBJ`, `ZEND_ASSIGN_OBJ_OP`, `ZEND_PRE/POST_INC/DEC_OBJ`
//! and `ZEND_ISSET_ISEMPTY_PROP_OBJ` (ref: Zend/zend_vm_def.h,
//! Zend/zend_execute.c `zend_fetch_property_address`,
//! Zend/zend_object_handlers.c `zend_get_property_offset`).
//!
//! A one-segment property place with a literal name on a CV or `$this`
//! (`$o->p`, `$this->p`) remembers the declared property its last lookup
//! found, keyed by the object's class and the calling scope. Both decide
//! `zend_get_property_offset`'s answer; php-src's runtime cache belongs to
//! one op array in one scope (a closure rebound to another scope gets its
//! own), ours to the op array alone, so the scope is part of the key. A hit
//! reads or writes the slot directly.
//!
//! Every case a cached slot does not cover takes the generic place walk
//! unchanged: lazy objects, undefined (uninitialized or unset) slots,
//! slots holding references, hooked and virtual properties, classes with
//! native property handlers, readonly and asymmetric-visibility properties
//! for writes, and typed properties when the value would need a coercion.
//! An initialized, accessible declared slot is exactly where
//! `zend_std_read_property` & co. return without consulting `__get`,
//! `__set` or `__isset`, so a hit needs no magic-method check.

use crate::class::Class;
use crate::func::flags;
use crate::object;
use crate::opcode::{
    IncDecKind, Operand, Place, PlaceBase, PlaceId, PropCacheEntry, Seg, PROP_CACHE_THIS,
};
use crate::props::PropLookup;
use crate::types::mask;
use crate::vm::{Frame, FrameKind, Vm, VmResult};
use php_runtime::{ops, BinaryOp, Obj, Value, ZStr};
use std::rc::Rc;

/// The entry was filled (an unfilled entry never matches).
const PC_FILLED: u8 = 1 << 0;
/// Reads (`FETCH_OBJ_R`/`_IS`) may use the slot.
const PC_READ: u8 = 1 << 1;
/// `isset()`/`empty()` may use the slot.
const PC_ISSET: u8 = 1 << 2;
/// Writes may store into the slot (no readonly, set visibility or native
/// write handler); typed properties only for values in `accept`.
const PC_WRITE: u8 = 1 << 3;

/// The result of probing a place's cache entry.
enum Probe<T> {
    /// The fast path applied.
    Hit(T),
    /// The entry matches, but this access needs the generic walk.
    Slow,
    /// The entry is unfilled or for another class or scope.
    Miss,
}

/// Whether place `p` has the shape the cache covers: a CV or `$this` with
/// one property segment named by a literal.
#[inline]
pub(crate) fn cacheable(p: &Place) -> bool {
    matches!(
        (&p.base, p.segs.as_slice()),
        (
            PlaceBase::Cv(_) | PlaceBase::This,
            [Seg::Prop(Operand::Lit(_))]
        )
    )
}

/// Whether place `p` starts with a cacheable property (`$this->a[$i]`,
/// `$cv->a->b`): reads may take its first segment from the cache. The
/// entry only describes that first property.
#[inline]
pub(crate) fn cacheable_head(p: &Place) -> bool {
    matches!(
        (&p.base, p.segs.as_slice()),
        (
            PlaceBase::Cv(_) | PlaceBase::This,
            [Seg::Prop(Operand::Lit(_)), ..]
        )
    )
}

/// Whether entry `e` was filled for class `cls` in scope `scope`.
#[inline]
fn key_matches(e: &PropCacheEntry, cls: &Rc<Class>, scope: Option<&Rc<Class>>) -> bool {
    std::ptr::eq(e.class.as_ptr(), Rc::as_ptr(cls))
        && match (&e.scope, scope) {
            (None, None) => true,
            (Some(w), Some(s)) => std::ptr::eq(w.as_ptr(), Rc::as_ptr(s)),
            _ => false,
        }
}

/// Whether a value may be stored as-is in a property accepting the
/// builtin type bits `m` (`u32::MAX`: untyped): the side-effect-free
/// subset of `zend_verify_property_type` that needs no coercion, class or
/// callable check.
#[inline]
fn accepts(m: u32, v: &Value) -> bool {
    if m == u32::MAX {
        return true;
    }
    let bit = match v {
        Value::Null => mask::NULL,
        Value::Bool(true) => mask::TRUE,
        Value::Bool(false) => mask::FALSE,
        Value::Int(_) => mask::LONG,
        Value::Float(_) => mask::DOUBLE,
        Value::Str(_) => mask::STRING,
        Value::Array(_) => mask::ARRAY | mask::ITERABLE,
        Value::Object(_) => mask::OBJECT,
        Value::Resource(_) => mask::RESOURCE,
        Value::Reference(_) => 0,
    };
    m & bit != 0
}

/// Runs `k` on the object the base of a cached place holds (a CV, also
/// through a reference, or `$this`), without cloning the handle; `None`
/// when it holds no object.
#[inline]
fn with_base_obj<R>(f: &Frame, base: u32, k: impl FnOnce(&Obj) -> R) -> Option<R> {
    if base == PROP_CACHE_THIS {
        return f.this.as_ref().map(k);
    }
    let vars = f.vars.borrow();
    let out = match vars.slots.get(f.cv_idx(base)) {
        Some(Some(Value::Object(o))) => Some(k(o)),
        Some(Some(Value::Reference(r))) => {
            let inner = r.borrow();
            match &*inner {
                Value::Object(o) => Some(k(o)),
                _ => None,
            }
        }
        _ => None,
    };
    drop(vars);
    out
}

/// Stores `v` in initialized, non-reference slot `slot` of `o`; returns
/// the old value (to be dropped once all borrows are released), or `None`
/// when the slot is undefined or holds a reference.
#[inline]
fn store_slot(o: &Obj, slot: u32, v: &Value) -> Option<Value> {
    let d = object::data(o);
    let old = {
        let mut slots = d.slots.borrow_mut();
        match slots.get_mut(slot as usize) {
            Some(Some(cur)) if !matches!(cur, Value::Reference(_)) => {
                Some(std::mem::replace(cur, v.clone()))
            }
            _ => None,
        }
    };
    if old.is_some() {
        d.set_slot_unset(slot as usize, false);
    }
    old
}

impl Vm {
    /// Runs `k` on the matching cache entry of place `pid` and the object
    /// its base holds. `need` are the `PC_*` bits the access requires;
    /// lazy objects always take the generic walk (`zend_lazy_object_init`
    /// runs there).
    #[inline]
    fn prop_probe<T>(
        &self,
        pid: PlaceId,
        need: u8,
        k: impl FnOnce(&PropCacheEntry, &Obj) -> Probe<T>,
    ) -> Probe<T> {
        let f = self.frame();
        if f.kind == FrameKind::Builtin {
            return Probe::Slow;
        }
        let cache = f.code.prop_cache.borrow();
        let Some(e) = cache.get(pid as usize).filter(|e| e.flags & PC_FILLED != 0) else {
            return Probe::Miss;
        };
        let scope = f.scope.as_ref();
        let r = with_base_obj(f, e.base, |o| {
            let d = object::data(o);
            if !key_matches(e, &d.class, scope) {
                return Probe::Miss;
            }
            if e.flags & need != need || d.lazy.borrow().is_some() {
                return Probe::Slow;
            }
            k(e, o)
        });
        drop(cache);
        r.unwrap_or(Probe::Slow)
    }

    /// Fills the cache entry of property place `pid` for the object its
    /// base holds now (`zend_get_property_offset` storing
    /// `CACHE_POLYMORPHIC_PTR`). Returns whether an entry was stored.
    #[inline(never)]
    fn prop_cache_fill(&mut self, pid: PlaceId) -> bool {
        let f = self.frame();
        if f.kind == FrameKind::Builtin {
            return false;
        }
        let code = &f.code;
        let p = &code.places[pid as usize];
        let (base, lit) = match (&p.base, p.segs.as_slice()) {
            (PlaceBase::Cv(cv), [Seg::Prop(Operand::Lit(l)), ..]) => (*cv, *l),
            (PlaceBase::This, [Seg::Prop(Operand::Lit(l)), ..]) => (PROP_CACHE_THIS, *l),
            _ => return false,
        };
        let Some(Value::Str(name)) = code.literals.get(lit as usize) else {
            return false;
        };
        let Some(cls) = with_base_obj(f, base, |o| object::class_of(o).clone()) else {
            return false;
        };
        let scope = f.scope.as_ref();
        let mut e = PropCacheEntry {
            class: Rc::downgrade(&cls),
            scope: scope.map(Rc::downgrade),
            base,
            slot: 0,
            accept: 0,
            flags: PC_FILLED,
        };
        if let PropLookup::Found(pi) = self.lookup_prop(&cls, name, scope) {
            if pi.hooks.is_none() && !pi.is_virtual() {
                let h = cls.hooks.as_deref();
                e.slot = pi.slot;
                if h.is_none_or(|h| h.read_property.is_none()) {
                    e.flags |= PC_READ;
                }
                if h.is_none_or(|h| h.has_property.is_none()) {
                    e.flags |= PC_ISSET;
                }
                if h.is_none_or(|h| h.write_property.is_none())
                    && pi.flags & (flags::READONLY | flags::SET_MASK) == 0
                {
                    e.flags |= PC_WRITE;
                    e.accept = pi.ty.as_ref().map_or(u32::MAX, |t| t.mask);
                }
            }
        }
        let n = code.places.len();
        let mut cache = code.prop_cache.borrow_mut();
        if cache.len() < n {
            cache.resize(n, PropCacheEntry::default());
        }
        cache[pid as usize] = e;
        true
    }

    /// [`Vm::prop_probe`], filling the entry and probing again on a miss.
    #[inline]
    fn prop_probe_fill<T>(
        &mut self,
        pid: PlaceId,
        need: u8,
        k: impl Fn(&PropCacheEntry, &Obj) -> Probe<T>,
    ) -> Option<T> {
        match self.prop_probe(pid, need, &k) {
            Probe::Hit(v) => Some(v),
            Probe::Slow => None,
            Probe::Miss => {
                if !self.prop_cache_fill(pid) {
                    return None;
                }
                match self.prop_probe(pid, need, &k) {
                    Probe::Hit(v) => Some(v),
                    Probe::Slow | Probe::Miss => None,
                }
            }
        }
    }

    /// The literal property name of cacheable place `pid`.
    fn prop_cache_name(&self, pid: PlaceId) -> ZStr {
        let code = &self.frame().code;
        match code.places[pid as usize].segs.first() {
            Some(Seg::Prop(Operand::Lit(l))) => match code.literals.get(*l as usize) {
                Some(Value::Str(s)) => s.clone(),
                _ => ZStr::default(),
            },
            _ => ZStr::default(),
        }
    }

    /// `ZEND_FETCH_OBJ_R`/`_IS` of the first segment of place `pid` (see
    /// [`cacheable_head`]): the (dereferenced) value of an initialized
    /// declared slot, or `None` for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_read(&mut self, pid: PlaceId) -> Option<Value> {
        self.prop_probe_fill(pid, PC_FILLED | PC_READ, |e, o| {
            match object::data(o).slots.borrow().get(e.slot as usize) {
                Some(Some(v)) => Probe::Hit(v.deref()),
                _ => Probe::Slow,
            }
        })
    }

    /// `ZEND_ASSIGN_OBJ` of cacheable place `pid`: stores `v` in an
    /// initialized declared slot and returns the assigned value, or hands
    /// `v` back (`Err`) for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_assign(&mut self, pid: PlaceId, v: Value) -> Result<Value, Value> {
        if matches!(v, Value::Reference(_)) {
            return Err(v);
        }
        let old = self.prop_probe_fill(pid, PC_FILLED | PC_WRITE, |e, o| {
            if !accepts(e.accept, &v) {
                return Probe::Slow;
            }
            match store_slot(o, e.slot, &v) {
                Some(old) => Probe::Hit(old),
                None => Probe::Slow,
            }
        });
        match old {
            Some(old) => {
                drop(old);
                Ok(v)
            }
            None => Err(v),
        }
    }

    /// `ZEND_PRE/POST_INC/DEC_OBJ` of cacheable place `pid` on a declared
    /// slot holding an int (without overflow) or a float, updated in place
    /// (`zend_pre_incdec_property_zval`): the result, or `None` for the
    /// generic walk. The value keeps its type, so a typed property stays
    /// valid.
    #[inline]
    pub(crate) fn prop_cache_incdec(&mut self, pid: PlaceId, kind: IncDecKind) -> Option<Value> {
        self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |e, o| {
            let d = object::data(o);
            let mut slots = d.slots.borrow_mut();
            match slots.get_mut(e.slot as usize) {
                Some(Some(Value::Int(i))) => {
                    let old = *i;
                    let new = if kind.is_inc() {
                        old.checked_add(1)
                    } else {
                        old.checked_sub(1)
                    };
                    match new {
                        Some(n) => {
                            *i = n;
                            Probe::Hit(Value::Int(if kind.is_post() { old } else { n }))
                        }
                        None => Probe::Slow,
                    }
                }
                Some(Some(Value::Float(x))) => {
                    let old = *x;
                    let n = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
                    *x = n;
                    Probe::Hit(Value::Float(if kind.is_post() { old } else { n }))
                }
                _ => Probe::Slow,
            }
        })
    }

    /// `ZEND_ASSIGN_OBJ_OP` of cacheable place `pid` with a numeric
    /// right-hand side on a declared slot holding a number: reads the
    /// slot, operates and writes the result back like the generic walk
    /// (`fetch_property_rw`, `assign_op_value`, `write_property`) does.
    /// `Ok(None)` leaves the whole operation to the generic walk.
    ///
    /// # Errors
    /// The operation's error (`DivisionByZeroError` & co.) or the
    /// write-back's.
    pub(crate) fn prop_cache_assign_op(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        rhs: &Value,
    ) -> VmResult<Option<Value>> {
        if !matches!(rhs, Value::Int(_) | Value::Float(_)) {
            return Ok(None);
        }
        let hit =
            self.prop_probe_fill(
                pid,
                PC_FILLED | PC_READ | PC_WRITE,
                |e, o| match object::data(o).slots.borrow().get(e.slot as usize) {
                    Some(Some(cur @ (Value::Int(_) | Value::Float(_)))) => {
                        Probe::Hit((o.clone(), cur.clone(), e.slot, e.accept))
                    }
                    _ => Probe::Slow,
                },
            );
        let Some((obj, cur, slot, accept)) = hit else {
            return Ok(None);
        };
        let nv = ops::binary_op(op, &cur, rhs, self).map_err(|e| self.rt_throw(e))?;
        // A diagnostic of the operation (a float-to-int deprecation) may
        // have run an error handler that changed the object: the direct
        // store needs the slot as it was, anything else goes through the
        // property handlers.
        if accepts(accept, &nv) && object::data(&obj).lazy.borrow().is_none() {
            if let Some(old) = store_slot(&obj, slot, &nv) {
                drop(old);
                return Ok(Some(nv));
            }
        }
        let name = self.prop_cache_name(pid);
        self.write_property(&obj, &name, nv).map(Some)
    }

    /// `ZEND_ISSET_ISEMPTY_PROP_OBJ` of cacheable place `pid` on an
    /// initialized declared slot: `isset()` (or, with `empty`, `empty()`),
    /// or `Ok(None)` for the generic walk.
    ///
    /// # Errors
    /// The conversion of the value to bool for `empty()`.
    #[inline]
    pub(crate) fn prop_cache_isset(&mut self, pid: PlaceId, empty: bool) -> VmResult<Option<bool>> {
        let v = self.prop_probe_fill(pid, PC_FILLED | PC_ISSET, |e, o| {
            match object::data(o).slots.borrow().get(e.slot as usize) {
                Some(Some(v)) => Probe::Hit(v.deref()),
                _ => Probe::Slow,
            }
        });
        let Some(v) = v else {
            return Ok(None);
        };
        Ok(Some(if empty {
            !self.to_bool(&v)?
        } else {
            !v.is_null()
        }))
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

    #[test]
    fn accepts_matches_type_bits() {
        use super::accepts;
        use crate::types::mask;
        use php_runtime::Value;
        assert!(accepts(u32::MAX, &Value::Null));
        assert!(accepts(mask::LONG, &Value::Int(1)));
        assert!(!accepts(mask::DOUBLE, &Value::Int(1)));
        assert!(accepts(mask::LONG | mask::NULL, &Value::Null));
        assert!(!accepts(mask::BOOL, &Value::Null));
        assert!(accepts(mask::TRUE, &Value::Bool(true)));
        assert!(!accepts(mask::TRUE, &Value::Bool(false)));
        assert!(accepts(
            mask::ITERABLE,
            &Value::Array(php_runtime::ZArray::new())
        ));
        assert!(!accepts(
            mask::ANY,
            &Value::Reference(php_runtime::PhpRef::new(Value::Null))
        ));
    }

    /// Repeated reads, writes, compound assignments, `++`/`--` and
    /// `isset`/`empty` of one place, as a loop hits the cache.
    #[test]
    fn cached_accesses_in_a_loop() {
        let src = r#"<?php
class Foo {
    public $b = 0;
    public int $t = 5;
    public float $f = 1.5;
    function run() {
        $x = 0;
        for ($i = 0; $i < 3; $i++) {
            $x += $this->b;
            $this->b = $i;
            $this->b += 2;
            ++$this->b;
            $this->b--;
            $this->t++;
            $this->f += 1;
            $this->t *= 2;
            $s = isset($this->b) ? 'set' : 'unset';
            $e = empty($this->b) ? 'empty' : 'full';
            echo "$x {$this->b} {$this->t} {$this->f} $s $e\n";
        }
        $this->t = PHP_INT_MAX;
        try { $this->t++; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->b = PHP_INT_MAX;
        $this->b++;
        var_dump($this->b);
        $this->b = 1;
        try { $this->b %= 0; } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; }
        try { $this->t = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->t = "7";
        var_dump($this->t);
    }
}
(new Foo)->run();
$o = new Foo;
for ($i = 0; $i < 3; $i++) { $o->b = $i * 10; echo $o->b, " "; }
echo "\n";
"#;
        let expected = "0 2 12 2.5 set full\n2 3 26 3.5 set full\n5 4 54 4.5 set full\n\
Cannot increment property Foo::$t of type int past its maximal value\n\
float(9.223372036854776E+18)\n\
Modulo by zero\n\
Cannot assign string to property Foo::$t of type int\n\
int(7)\n\
0 10 20 \n";
        assert_eq!(run(src), expected);
    }

    /// One place seeing objects of different classes, a subclass shadowed
    /// by a private property of the scope, and a closure rebound to
    /// another scope: the entry is keyed by class and scope.
    #[test]
    fn class_and_scope_key_the_entry() {
        let src = r#"<?php
class A { private $p = 'A-private'; function get($o) { return $o->p; } }
class B extends A { public $p = 'B-public'; }
class C { public $p = 'C'; }
$a = new A; $b = new B; $c = new C;
foreach ([$a, $b, $c, $b, $a] as $o) { echo $a->get($o), "\n"; }
function rd($o) { return $o->p; }
foreach ([$b, $c, $b] as $o) { echo rd($o), "\n"; }
class P { private $q = 'P-private'; }
class Q { private $q = 'Q-private'; }
$f = function () { return $this->q; };
foreach ([new P, new Q, new P] as $o) {
    echo Closure::bind($f, $o, get_class($o))(), "\n";
}
try { echo Closure::bind($f, new P, Q::class)(), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#;
        let expected = "A-private\nA-private\nC\nA-private\nA-private\n\
B-public\nC\nB-public\n\
P-private\nQ-private\nP-private\n\
Cannot access private property P::$q\n";
        assert_eq!(run(src), expected);
    }

    /// Unset slots go back to `__get`/`__set`/`__isset`, references stay
    /// shared, readonly properties still throw and hooks still run, after
    /// the place has cached the slot.
    #[test]
    fn uncovered_cases_take_the_generic_walk() {
        let src = r#"<?php
class M {
    public $p = 1;
    function __get($n) { echo "get $n\n"; return 42; }
    function __set($n, $v) { echo "set $n\n"; }
    function __isset($n) { echo "isset $n\n"; return true; }
}
function touch_m(M $m) {
    $x = $m->p;
    $m->p = 2;
    $s = isset($m->p);
    return "$x " . var_export($s, true);
}
$m = new M;
echo touch_m($m), "\n";
unset($m->p);
echo touch_m($m), "\n";
class R { public $v = 1; public readonly int $ro; function __construct() { $this->ro = 1; } }
$r = new R;
$x = 5;
$r->v = &$x;
for ($i = 0; $i < 2; $i++) { $r->v = $i + 10; }
var_dump($x);
for ($i = 0; $i < 2; $i++) {
    try { $r->ro = 2; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $r->ro++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
class H { public int $h = 0 { set { echo "hook\n"; $this->h = $value * 2; } } }
$h = new H;
for ($i = 1; $i <= 2; $i++) { $h->h = $i; echo $h->h, "\n"; }
"#;
        let expected = "1 true\n\
get p\nset p\nisset p\n42 true\n\
int(11)\n\
Cannot modify readonly property R::$ro\nCannot modify readonly property R::$ro\n\
Cannot modify readonly property R::$ro\nCannot modify readonly property R::$ro\n\
hook\n2\nhook\n4\n";
        assert_eq!(run(src), expected);
    }

    /// A chain starting with a cached property (`$this->a[$i]`,
    /// `$this->o->p`) goes on from the cached value, with the later
    /// segments' warnings and `??` semantics.
    #[test]
    fn cached_head_of_a_longer_chain() {
        let src = r#"<?php
class N {
    public $a = [1, 2, 3];
    public $o;
    public $n = null;
    function __construct() { $this->o = new stdClass; $this->o->p = 'deep'; }
    function run() {
        $s = 0;
        for ($i = 0; $i < 3; $i++) { $s += $this->a[$i]; }
        echo $s, "\n";
        for ($i = 0; $i < 2; $i++) { echo $this->o->p, "\n"; }
        echo $this->a[7] ?? 'dflt', "\n";
        echo $this->n['k'] ?? 'null-head', "\n";
        echo $this->n->q ?? 'null-obj', "\n";
        echo $this->a[5], "|\n";
        echo $this->n->q, "|\n";
    }
}
(new N)->run();
"#;
        let expected = "6\ndeep\ndeep\ndflt\nnull-head\nnull-obj\n\
\nWarning: Undefined array key 5 in t.php on line 15\n|\n\
\nWarning: Attempt to read property \"q\" on null in t.php on line 16\n|\n";
        assert_eq!(run(src), expected);
    }
}
