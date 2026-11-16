// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `SplHeap`, `SplMinHeap`, `SplMaxHeap` and `SplPriorityQueue`
//! (`spl_heap.c`).
//!
//! The sift-up/sift-down loops are exact ports of `spl_ptr_heap_insert` and
//! `spl_ptr_heap_delete_top`, so the (unstable) order of equal elements
//! matches PHP. A comparison that throws marks the heap as corrupted.

use crate::args::Args;
use crate::exceptions::{exception, runtime};
use crate::host::{ObjectIterator, SplHost};
use crate::object::{state_of, SplObject};
use crate::util::{private_prop, this_obj, to_long};
use php_runtime::{ops, ArrayKey, HashTable, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};
use std::rc::Rc;

/// `SPL_PQUEUE_EXTR_DATA`.
pub const EXTR_DATA: i64 = 1;
/// `SPL_PQUEUE_EXTR_PRIORITY`.
pub const EXTR_PRIORITY: i64 = 2;
/// `SPL_PQUEUE_EXTR_BOTH`.
pub const EXTR_BOTH: i64 = 3;
const EXTR_MASK: i64 = 3;

/// Which comparison the heap uses without a user `compare()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapKind {
    /// `SplMaxHeap` / `SplHeap`: `zend_compare(a, b)`.
    Max,
    /// `SplMinHeap`: `zend_compare(b, a)`.
    Min,
    /// `SplPriorityQueue`: `zend_compare(a.priority, b.priority)`.
    PriorityQueue,
}

/// One heap element: a value, plus the priority for priority queues.
#[derive(Debug, Clone)]
pub struct HeapElem {
    /// The data.
    pub data: Value,
    /// The priority (`null` for plain heaps).
    pub priority: Value,
}

/// The internal state of a heap (`spl_heap_object` + `spl_ptr_heap`).
#[derive(Debug, Clone)]
pub struct Heap {
    /// The comparison kind.
    pub kind: HeapKind,
    /// Element storage; only the first `count` entries are live.
    pub elems: Vec<HeapElem>,
    /// Number of live elements.
    pub count: usize,
    /// `SPL_HEAP_CORRUPTED`.
    pub corrupted: bool,
    /// `SPL_HEAP_WRITE_LOCKED`.
    pub write_locked: bool,
    /// The extract flags (priority queues; 0 for heaps).
    pub flags: i64,
}

impl Heap {
    /// A new empty heap for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let kind = match base {
            "SplMinHeap" => HeapKind::Min,
            "SplPriorityQueue" => HeapKind::PriorityQueue,
            _ => HeapKind::Max,
        };
        Heap {
            kind,
            elems: Vec::new(),
            count: 0,
            corrupted: false,
            write_locked: false,
            flags: if kind == HeapKind::PriorityQueue {
                EXTR_DATA
            } else {
                0
            },
        }
    }

    fn is_pqueue(&self) -> bool {
        self.kind == HeapKind::PriorityQueue
    }
}

/// `spl_pqueue_extract_helper`.
#[must_use]
pub fn extract_value(elem: &HeapElem, flags: i64) -> Value {
    if flags & EXTR_BOTH == EXTR_BOTH {
        let mut t = HashTable::new();
        t.insert(ArrayKey::from("data"), elem.data.clone());
        t.insert(ArrayKey::from("priority"), elem.priority.clone());
        return Value::Array(ZArray::from_table(t));
    }
    if flags & EXTR_DATA != 0 {
        return elem.data.clone();
    }
    elem.priority.clone()
}

/// The heap's comparison function (`heap->cmp`), calling an overridden
/// `compare()` when the class has one.
#[allow(clippy::many_single_char_names)]
fn compare(
    host: &mut dyn SplHost,
    obj: &Obj,
    kind: HeapKind,
    user: bool,
    a: &HeapElem,
    b: &HeapElem,
) -> RtResult<i32> {
    let (x, y) = if kind == HeapKind::PriorityQueue {
        (&a.priority, &b.priority)
    } else {
        (&a.data, &b.data)
    };
    if user {
        let r = host.call_method(obj, "compare", &[x.clone(), y.clone()])?;
        let n = r.to_long(host)?;
        return Ok(i32::from(n > 0) - i32::from(n < 0));
    }
    match kind {
        HeapKind::Min => ops::compare(y, x, host),
        _ => ops::compare(x, y, host),
    }
}

/// Comparison wrapper that turns the first error into `0` results
/// (`if (EG(exception)) return 0;`).
struct Cmp<'a> {
    obj: &'a Obj,
    kind: HeapKind,
    user: bool,
    err: Option<RuntimeError>,
}

impl Cmp<'_> {
    fn run(&mut self, host: &mut dyn SplHost, a: &HeapElem, b: &HeapElem) -> i32 {
        if self.err.is_some() {
            return 0;
        }
        match compare(host, self.obj, self.kind, self.user, a, b) {
            Ok(r) => r,
            Err(e) => {
                self.err = Some(e);
                0
            }
        }
    }
}

fn cmp_for<'a>(host: &mut dyn SplHost, obj: &'a Obj, kind: HeapKind) -> Cmp<'a> {
    Cmp {
        obj,
        kind,
        user: host.has_user_method(obj, "compare"),
        err: None,
    }
}

/// `spl_ptr_heap_insert`.
pub fn heap_insert(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &SplObject,
    elem: HeapElem,
) -> RtResult<()> {
    let (kind, mut pos) = st.heap(|h| {
        if h.elems.len() <= h.count {
            h.elems.resize(
                h.count + 1,
                HeapElem {
                    data: Value::Null,
                    priority: Value::Null,
                },
            );
        }
        h.write_locked = true;
        (h.kind, h.count)
    })?;
    let mut cmp = cmp_for(host, obj, kind);
    while pos > 0 {
        let parent = (pos - 1) / 2;
        let pe = st.heap(|h| h.elems[parent].clone())?;
        if cmp.run(host, &pe, &elem) < 0 {
            st.heap(|h| h.elems[pos] = pe)?;
            pos = parent;
        } else {
            break;
        }
    }
    st.heap(|h| {
        h.count += 1;
        h.write_locked = false;
        if cmp.err.is_some() {
            h.corrupted = true;
        }
        h.elems[pos] = elem;
    })?;
    cmp.err.map_or(Ok(()), Err)
}

/// `spl_ptr_heap_delete_top`: removes and returns the top element (`None`
/// when empty).
pub fn heap_delete_top(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &SplObject,
) -> RtResult<Option<HeapElem>> {
    let Some((kind, top, bottom, count)) = st.heap(|h| {
        if h.count == 0 {
            return None;
        }
        h.write_locked = true;
        let top = h.elems[0].clone();
        h.count -= 1;
        Some((h.kind, top, h.elems[h.count].clone(), h.count))
    })?
    else {
        return Ok(None);
    };
    let limit = count / 2; // (old_count - 1) / 2
    let mut cmp = cmp_for(host, obj, kind);
    let mut parent = 0usize;
    while parent < limit {
        let mut child = parent * 2 + 1;
        if child != count {
            let (c1, c0) = st.heap(|h| (h.elems[child + 1].clone(), h.elems[child].clone()))?;
            if cmp.run(host, &c1, &c0) > 0 {
                child += 1;
            }
        }
        let ce = st.heap(|h| h.elems[child].clone())?;
        if cmp.run(host, &bottom, &ce) < 0 {
            st.heap(|h| h.elems[parent] = ce)?;
            parent = child;
        } else {
            break;
        }
    }
    st.heap(|h| {
        h.write_locked = false;
        if cmp.err.is_some() {
            h.corrupted = true;
        }
        if parent != count {
            h.elems[parent] = bottom;
        }
        h.elems.truncate(h.count);
    })?;
    match cmp.err {
        Some(e) => Err(e),
        None => Ok(Some(top)),
    }
}

/// `spl_heap_consistency_validations`.
fn validate(st: &SplObject, write: bool) -> RtResult<()> {
    let (corrupted, locked) = st.heap(|h| (h.corrupted, h.write_locked))?;
    if corrupted {
        return Err(runtime(
            "Heap is corrupted, heap properties are no longer ensured.",
        ));
    }
    if write && locked {
        return Err(runtime(
            "Heap cannot be changed when it is already being modified.",
        ));
    }
    Ok(())
}

/// Methods of `SplHeap` (and the subclasses' `compare`) and
/// `SplPriorityQueue`. `class` is the declaring class.
#[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
pub(crate) fn call(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> Option<RtResult<Value>> {
    let pq = class == "SplPriorityQueue";
    // compare() of SplMinHeap / SplMaxHeap / SplPriorityQueue is usable
    // without heap state.
    if method == "compare" {
        let func = match class {
            "SplMinHeap" => "SplMinHeap::compare",
            "SplMaxHeap" => "SplMaxHeap::compare",
            _ => "SplPriorityQueue::compare",
        };
        let names: &[&str] = if pq {
            &["priority1", "priority2"]
        } else {
            &["value1", "value2"]
        };
        return Some((|| {
            let a = Args::new(func, args, names, 2, Some(2))?;
            let (x, y) = (a.raw(0), a.raw(1));
            let r = if class == "SplMinHeap" {
                ops::compare(&y, &x, host)?
            } else {
                ops::compare(&x, &y, host)?
            };
            Ok(Value::Int(i64::from(r)))
        })());
    }
    let this = match this_obj(this) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    let cls = if pq { "SplPriorityQueue" } else { "SplHeap" };
    let f = |m: &str| format!("{cls}::{m}");
    Some((|| -> RtResult<Value> {
        let st = state_of(host, this)?;
        match method {
            "count" => {
                Args::none(&f("count"), args)?;
                st.heap(|h| Value::Int(to_long(h.count)))
            }
            "isempty" => {
                Args::none(&f("isEmpty"), args)?;
                st.heap(|h| Value::Bool(h.count == 0))
            }
            "insert" if pq => {
                let fname = f("insert");
                let a = Args::new(&fname, args, &["value", "priority"], 2, Some(2))?;
                validate(&st, true)?;
                heap_insert(
                    host,
                    this,
                    &st,
                    HeapElem {
                        data: a.raw(0),
                        priority: a.raw(1),
                    },
                )?;
                Ok(Value::Bool(true))
            }
            "insert" => {
                let fname = f("insert");
                let a = Args::new(&fname, args, &["value"], 1, Some(1))?;
                validate(&st, true)?;
                heap_insert(
                    host,
                    this,
                    &st,
                    HeapElem {
                        data: a.raw(0),
                        priority: Value::Null,
                    },
                )?;
                Ok(Value::Bool(true))
            }
            "extract" => {
                Args::none(&f("extract"), args)?;
                validate(&st, true)?;
                match heap_delete_top(host, this, &st)? {
                    None => Err(runtime("Can't extract from an empty heap")),
                    Some(e) if pq => {
                        let flags = st.heap(|h| h.flags)?;
                        Ok(extract_value(&e, flags))
                    }
                    Some(e) => Ok(e.data),
                }
            }
            "top" => {
                Args::none(&f("top"), args)?;
                validate(&st, false)?;
                let (top, flags) = st.heap(|h| {
                    (
                        if h.count == 0 {
                            None
                        } else {
                            Some(h.elems[0].clone())
                        },
                        h.flags,
                    )
                })?;
                match top {
                    None => Err(runtime("Can't peek at an empty heap")),
                    Some(e) if pq => Ok(extract_value(&e, flags)),
                    Some(e) => Ok(e.data.deref()),
                }
            }
            "setextractflags" if pq => {
                let fname = f("setExtractFlags");
                let a = Args::new(&fname, args, &["flags"], 1, Some(1))?;
                let v = a.long(host, 0)? & EXTR_MASK;
                if v == 0 {
                    return Err(runtime("Must specify at least one extract flag"));
                }
                st.heap(|h| h.flags = v)?;
                Ok(Value::Int(v))
            }
            "getextractflags" if pq => {
                Args::none(&f("getExtractFlags"), args)?;
                st.heap(|h| Value::Int(h.flags))
            }
            "recoverfromcorruption" => {
                Args::none(&f("recoverFromCorruption"), args)?;
                st.heap(|h| h.corrupted = false)?;
                Ok(Value::Bool(true))
            }
            "iscorrupted" => {
                Args::none(&f("isCorrupted"), args)?;
                st.heap(|h| Value::Bool(h.corrupted))
            }
            "key" => {
                Args::none(&f("key"), args)?;
                st.heap(|h| Value::Int(to_long(h.count) - 1))
            }
            "next" => {
                Args::none(&f("next"), args)?;
                validate(&st, true)?;
                heap_delete_top(host, this, &st)?;
                Ok(Value::Null)
            }
            "valid" => {
                Args::none(&f("valid"), args)?;
                st.heap(|h| Value::Bool(h.count != 0))
            }
            "rewind" => {
                Args::none(&f("rewind"), args)?;
                Ok(Value::Null)
            }
            "current" => {
                Args::none(&f("current"), args)?;
                st.heap(|h| {
                    if h.count == 0 {
                        Value::Null
                    } else if pq {
                        extract_value(&h.elems[0], h.flags)
                    } else {
                        h.elems[0].data.deref()
                    }
                })
            }
            "__debuginfo" => {
                Args::none(&f("__debugInfo"), args)?;
                debug_info(host, this, &st, cls).map(Value::Array)
            }
            "__serialize" => {
                Args::none(&f("__serialize"), args)?;
                serialize(host, this, &st)
            }
            "__unserialize" => {
                let fname = f("__unserialize");
                let a = Args::new(&fname, args, &["data"], 1, Some(1))?;
                let data = a.array(0)?;
                unserialize(host, this, &st, &data, pq)
            }
            _ => Err(RuntimeError::Error(format!(
                "Call to undefined method {cls}::{method}()"
            ))),
        }
    })())
}

/// `spl_heap_object_get_debug_info` with the given class (`SplHeap` or
/// `SplPriorityQueue`) used to mangle the private names.
pub fn debug_info(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    class: &str,
) -> RtResult<ZArray> {
    let props = host.std_properties(this);
    let mut out: HashTable = (*props).clone();
    let (flags, corrupted, heap) = st.heap(|h| {
        let mut arr = HashTable::new();
        for e in &h.elems[..h.count] {
            let v = if class == "SplPriorityQueue" {
                extract_value(e, EXTR_BOTH)
            } else {
                e.data.clone()
            };
            let _ = arr.push(v);
        }
        (h.flags, h.corrupted, arr)
    })?;
    out.insert(
        ArrayKey::Str(private_prop(class, "flags")),
        Value::Int(flags),
    );
    out.insert(
        ArrayKey::Str(private_prop(class, "isCorrupted")),
        Value::Bool(corrupted),
    );
    out.insert(
        ArrayKey::Str(private_prop(class, "heap")),
        Value::Array(ZArray::from_table(heap)),
    );
    Ok(ZArray::from_table(out))
}

fn serialize(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<Value> {
    validate(st, false)?;
    if st.heap(|h| h.write_locked)? {
        return Err(runtime("Cannot serialize heap while it is being modified."));
    }
    let props = host.std_properties(this);
    let state = st.heap(|h| {
        let mut elements = HashTable::with_capacity(h.count);
        for e in &h.elems[..h.count] {
            let v = if h.is_pqueue() {
                extract_value(e, EXTR_BOTH)
            } else {
                e.data.clone()
            };
            let _ = elements.push(v);
        }
        let mut state = HashTable::new();
        state.insert(ArrayKey::from("flags"), Value::Int(h.flags));
        state.insert(
            ArrayKey::from("heap_elements"),
            Value::Array(ZArray::from_table(elements)),
        );
        state
    })?;
    Ok(Value::Array(ZArray::from_values([
        Value::Array(props),
        Value::Array(ZArray::from_table(state)),
    ])))
}

fn unserialize(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    data: &ZArray,
    pq: bool,
) -> RtResult<Value> {
    if !pq {
        validate(st, true)?;
    }
    let invalid = || {
        exception(format!(
            "Invalid serialization data for {} object",
            this.class_name()
        ))
    };
    if data.len() != 2 {
        return Err(invalid());
    }
    let Some(Value::Array(props)) = data.get_int(0).map(Value::deref) else {
        return Err(invalid());
    };
    for (k, v) in props.iter() {
        if host.property_write(this, &k.to_zstr(), v.clone()).is_err() {
            return Err(invalid());
        }
    }
    let Some(Value::Array(state)) = data.get_int(1).map(Value::deref) else {
        return Err(invalid());
    };
    let Some(Value::Int(mut flags)) = state.get_str(b"flags").map(Value::deref) else {
        return Err(invalid());
    };
    if pq {
        flags &= EXTR_MASK;
        if flags == 0 {
            return Err(invalid());
        }
    } else if flags != 0 {
        return Err(invalid());
    }
    st.heap(|h| h.flags = flags)?;
    let Some(Value::Array(elements)) = state.get_str(b"heap_elements").map(Value::deref) else {
        return Err(invalid());
    };
    for v in elements.values() {
        let elem = if pq {
            let Value::Array(e) = v.deref() else {
                return Err(invalid());
            };
            if e.len() != 2 {
                return Err(invalid());
            }
            let (Some(d), Some(p)) = (e.get_str(b"data"), e.get_str(b"priority")) else {
                return Err(invalid());
            };
            HeapElem {
                data: d.clone(),
                priority: p.clone(),
            }
        } else {
            HeapElem {
                data: v.clone(),
                priority: Value::Null,
            }
        };
        if heap_insert(host, this, st, elem).is_err() {
            return Err(invalid());
        }
    }
    validate(st, false)?;
    Ok(Value::Null)
}

/// `count_elements` handler (honours an overridden `count()`).
pub fn count_elements(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>) -> RtResult<i64> {
    if host.has_user_method(obj, "count") {
        let r = host.call_method(obj, "count", &[])?;
        return r.to_long(host);
    }
    st.heap(|h| to_long(h.count))
}

/// The heap / priority queue `foreach` iterator (`spl_heap_it_funcs`):
/// iterating extracts the elements.
#[derive(Debug)]
pub struct HeapIter {
    obj: Obj,
    st: Rc<SplObject>,
}

impl HeapIter {
    /// Creates the iterator.
    #[must_use]
    pub fn new(obj: Obj, st: Rc<SplObject>) -> Self {
        HeapIter { obj, st }
    }
}

impl ObjectIterator for HeapIter {
    fn rewind(&mut self, _host: &mut dyn SplHost) -> RtResult<()> {
        Ok(())
    }
    fn valid(&mut self, _host: &mut dyn SplHost) -> RtResult<bool> {
        self.st.heap(|h| h.count != 0)
    }
    fn current(&mut self, _host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        validate(&self.st, false)?;
        self.st.heap(|h| {
            if h.count == 0 {
                None
            } else if h.is_pqueue() {
                Some(extract_value(&h.elems[0], h.flags))
            } else {
                Some(h.elems[0].data.clone())
            }
        })
    }
    fn key(&mut self, _host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        self.st.heap(|h| Some(Value::Int(to_long(h.count) - 1)))
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        validate(&self.st, true)?;
        heap_delete_top(host, &self.obj, &self.st).map(|_| ())
    }
}

/// Clones heap state (`spl_ptr_heap_clone`).
#[must_use]
pub fn clone_heap(h: &Heap) -> Heap {
    h.clone()
}

/// The private property names used in the debug info, for tests.
#[must_use]
pub fn debug_keys(class: &str) -> [ZStr; 3] {
    [
        private_prop(class, "flags"),
        private_prop(class, "isCorrupted"),
        private_prop(class, "heap"),
    ]
}
