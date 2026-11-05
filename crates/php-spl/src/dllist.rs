//! `SplDoublyLinkedList`, `SplQueue` and `SplStack` (`spl_dllist.c`).
//!
//! Elements are reference-counted nodes like `spl_ptr_llist_element`: an
//! iterator positioned on an element keeps it alive after it was removed
//! from the list (its data then reads as "undefined" and it has no
//! neighbours), which reproduces PHP's traversal quirks.

use crate::args::Args;
use crate::exceptions::{runtime, unexpected_value};
use crate::host::{ObjectIterator, SplHost};
use crate::object::{state_of, SplObject};
use crate::serial;
use crate::util::{private_prop, this_obj, to_long};
use php_runtime::{ArrayKey, HashTable, Obj, RtResult, RuntimeError, Value, ZArray};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// `SPL_DLLIST_IT_LIFO`.
pub const IT_LIFO: i64 = 2;
/// `SPL_DLLIST_IT_FIFO`.
pub const IT_FIFO: i64 = 0;
/// `SPL_DLLIST_IT_DELETE`.
pub const IT_DELETE: i64 = 1;
/// `SPL_DLLIST_IT_KEEP`.
pub const IT_KEEP: i64 = 0;
/// `SPL_DLLIST_IT_FIX`: the LIFO/FIFO mode cannot be changed.
pub const IT_FIX: i64 = 4;
const IT_MASK: i64 = 3;

/// A list node (`spl_ptr_llist_element`).
#[derive(Debug)]
pub struct Node {
    prev: Option<Weak<RefCell<Node>>>,
    next: Option<NodeRef>,
    /// The data; `None` once the element was removed (`IS_UNDEF`).
    data: Option<Value>,
}

/// A shared node handle.
pub type NodeRef = Rc<RefCell<Node>>;

fn prev_of(n: &NodeRef) -> Option<NodeRef> {
    n.borrow().prev.as_ref().and_then(Weak::upgrade)
}

fn next_of(n: &NodeRef) -> Option<NodeRef> {
    n.borrow().next.clone()
}

/// The list itself (`spl_ptr_llist`).
#[derive(Debug, Default)]
pub struct LinkedList {
    head: Option<NodeRef>,
    tail: Option<Weak<RefCell<Node>>>,
    count: usize,
}

impl Drop for LinkedList {
    fn drop(&mut self) {
        // Unlink iteratively to avoid deep recursive drops.
        let mut cur = self.head.take();
        while let Some(n) = cur {
            cur = n.borrow_mut().next.take();
        }
    }
}

impl LinkedList {
    fn tail(&self) -> Option<NodeRef> {
        self.tail.as_ref().and_then(Weak::upgrade)
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The values in order.
    #[must_use]
    pub fn values(&self) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.count);
        let mut cur = self.head.clone();
        while let Some(n) = cur {
            if let Some(d) = &n.borrow().data {
                out.push(d.clone());
            }
            cur = next_of(&n);
        }
        out
    }

    /// `spl_ptr_llist_push`.
    pub fn push(&mut self, data: Value) {
        let node = Rc::new(RefCell::new(Node {
            prev: self.tail.clone(),
            next: None,
            data: Some(data),
        }));
        match self.tail() {
            Some(t) => t.borrow_mut().next = Some(node.clone()),
            None => self.head = Some(node.clone()),
        }
        self.tail = Some(Rc::downgrade(&node));
        self.count += 1;
    }

    /// `spl_ptr_llist_unshift`.
    pub fn unshift(&mut self, data: Value) {
        let node = Rc::new(RefCell::new(Node {
            prev: None,
            next: self.head.clone(),
            data: Some(data),
        }));
        match &self.head {
            Some(h) => h.borrow_mut().prev = Some(Rc::downgrade(&node)),
            None => self.tail = Some(Rc::downgrade(&node)),
        }
        self.head = Some(node);
        self.count += 1;
    }

    /// `spl_ptr_llist_pop`.
    pub fn pop(&mut self) -> Option<Value> {
        let tail = self.tail()?;
        let prev = prev_of(&tail);
        match &prev {
            Some(p) => p.borrow_mut().next = None,
            None => self.head = None,
        }
        self.tail = prev.as_ref().map(Rc::downgrade);
        self.count -= 1;
        let mut t = tail.borrow_mut();
        t.prev = None;
        t.data.take()
    }

    /// `spl_ptr_llist_shift`.
    pub fn shift(&mut self) -> Option<Value> {
        let head = self.head.clone()?;
        let next = next_of(&head);
        match &next {
            Some(n) => n.borrow_mut().prev = None,
            None => self.tail = None,
        }
        self.head = next;
        self.count -= 1;
        let mut h = head.borrow_mut();
        h.next = None;
        h.data.take()
    }

    /// `spl_ptr_llist_offset`.
    fn offset(&self, offset: i64, backward: bool) -> Option<NodeRef> {
        let mut cur = if backward {
            self.tail()
        } else {
            self.head.clone()
        };
        let mut pos = 0;
        while let Some(c) = cur.clone() {
            if pos >= offset {
                break;
            }
            pos += 1;
            cur = if backward { prev_of(&c) } else { next_of(&c) };
        }
        cur
    }

    fn unlink(&mut self, element: &NodeRef) {
        let prev = prev_of(element);
        let next = next_of(element);
        if let Some(p) = &prev {
            p.borrow_mut().next.clone_from(&next);
        }
        if let Some(n) = &next {
            n.borrow_mut().prev = prev.as_ref().map(Rc::downgrade);
        }
        if self.head.as_ref().is_some_and(|h| Rc::ptr_eq(h, element)) {
            self.head = next;
        }
        if self.tail().is_some_and(|t| Rc::ptr_eq(&t, element)) {
            self.tail = prev.as_ref().map(Rc::downgrade);
        }
        let mut e = element.borrow_mut();
        e.prev = None;
        e.next = None;
        self.count -= 1;
    }
}

/// The internal state of a list (`spl_dllist_object`).
#[derive(Debug)]
pub struct DlList {
    /// The list, shared with clones made while iterating.
    pub list: LinkedList,
    /// `traverse_pointer`.
    traverse: Option<NodeRef>,
    /// `traverse_position`.
    position: i64,
    /// Iterator mode flags (`IT_*`).
    pub flags: i64,
}

impl DlList {
    /// A new list for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let flags = match base {
            "SplStack" => IT_FIX | IT_LIFO,
            "SplQueue" => IT_FIX,
            _ => 0,
        };
        DlList {
            list: LinkedList::default(),
            traverse: None,
            position: 0,
            flags,
        }
    }

    /// A copy for `clone` (`spl_ptr_llist_copy`).
    #[must_use]
    pub fn duplicate(&self) -> Self {
        let mut list = LinkedList::default();
        for v in self.list.values() {
            list.push(v);
        }
        DlList {
            traverse: list.head.clone(),
            list,
            position: 0,
            flags: self.flags,
        }
    }
}

fn rewind_helper(list: &LinkedList, flags: i64) -> (Option<NodeRef>, i64) {
    if flags & IT_LIFO != 0 {
        (list.tail(), to_long(list.count) - 1)
    } else {
        (list.head.clone(), 0)
    }
}

/// `spl_dllist_it_helper_move_forward`; returns the removed value (to be
/// dropped by the caller after releasing borrows).
fn move_forward(
    list: &mut LinkedList,
    ptr: &mut Option<NodeRef>,
    pos: &mut i64,
    flags: i64,
) -> Option<Value> {
    let old = ptr.take()?;
    let mut garbage = None;
    if flags & IT_LIFO != 0 {
        *ptr = prev_of(&old);
        *pos -= 1;
        if flags & IT_DELETE != 0 {
            garbage = list.pop();
        }
    } else {
        *ptr = next_of(&old);
        if flags & IT_DELETE != 0 {
            garbage = list.shift();
        } else {
            *pos += 1;
        }
    }
    garbage
}

fn out_of_range(a: &Args<'_>) -> RuntimeError {
    a.arg_error("OutOfRangeException", 0, "is out of range")
}

fn invalid_offset(a: &Args<'_>) -> RuntimeError {
    a.arg_error("OutOfRangeException", 0, "is an invalid offset")
}

/// `SplDoublyLinkedList` / `SplQueue` methods. `method` is lowercase.
#[allow(clippy::too_many_lines)]
pub(crate) fn call(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> Option<RtResult<Value>> {
    let this = match this_obj(this) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    if class == "SplQueue" {
        let m = match method {
            "enqueue" => "push",
            "dequeue" => "shift",
            _ => return None,
        };
        let func = if m == "push" {
            "SplQueue::enqueue"
        } else {
            "SplQueue::dequeue"
        };
        return Some(list_method(host, this, m, func, args));
    }
    let func = match method {
        "push" => "SplDoublyLinkedList::push",
        "unshift" => "SplDoublyLinkedList::unshift",
        "pop" => "SplDoublyLinkedList::pop",
        "shift" => "SplDoublyLinkedList::shift",
        "top" => "SplDoublyLinkedList::top",
        "bottom" => "SplDoublyLinkedList::bottom",
        "count" => "SplDoublyLinkedList::count",
        "isempty" => "SplDoublyLinkedList::isEmpty",
        "setiteratormode" => "SplDoublyLinkedList::setIteratorMode",
        "getiteratormode" => "SplDoublyLinkedList::getIteratorMode",
        "offsetexists" => "SplDoublyLinkedList::offsetExists",
        "offsetget" => "SplDoublyLinkedList::offsetGet",
        "offsetset" => "SplDoublyLinkedList::offsetSet",
        "offsetunset" => "SplDoublyLinkedList::offsetUnset",
        "key" => "SplDoublyLinkedList::key",
        "prev" => "SplDoublyLinkedList::prev",
        "next" => "SplDoublyLinkedList::next",
        "valid" => "SplDoublyLinkedList::valid",
        "rewind" => "SplDoublyLinkedList::rewind",
        "current" => "SplDoublyLinkedList::current",
        "serialize" => "SplDoublyLinkedList::serialize",
        "unserialize" => "SplDoublyLinkedList::unserialize",
        "__serialize" => "SplDoublyLinkedList::__serialize",
        "__unserialize" => "SplDoublyLinkedList::__unserialize",
        "add" => "SplDoublyLinkedList::add",
        "__debuginfo" => "SplDoublyLinkedList::__debugInfo",
        _ => return None,
    };
    Some(list_method(host, this, method, func, args))
}

#[allow(clippy::too_many_lines)]
fn list_method(
    host: &mut dyn SplHost,
    this: &Obj,
    method: &str,
    func: &str,
    args: &[Value],
) -> RtResult<Value> {
    let st = state_of(host, this)?;
    match method {
        "push" | "unshift" => {
            let a = Args::new(func, args, &["value"], 1, Some(1))?;
            let v = a.raw(0);
            st.dllist(|d| {
                if method == "push" {
                    d.list.push(v);
                } else {
                    d.list.unshift(v);
                }
            })?;
            Ok(Value::Null)
        }
        "pop" | "shift" => {
            Args::none(func, args)?;
            let v = st.dllist(|d| {
                if method == "pop" {
                    d.list.pop()
                } else {
                    d.list.shift()
                }
            })?;
            v.ok_or_else(|| {
                runtime(if method == "pop" {
                    "Can't pop from an empty datastructure"
                } else {
                    "Can't shift from an empty datastructure"
                })
            })
        }
        "top" | "bottom" => {
            Args::none(func, args)?;
            let v = st.dllist(|d| {
                let n = if method == "top" {
                    d.list.tail()
                } else {
                    d.list.head.clone()
                };
                n.and_then(|n| n.borrow().data.clone())
            })?;
            v.map(|v| v.deref())
                .ok_or_else(|| runtime("Can't peek at an empty datastructure"))
        }
        "count" => {
            Args::none(func, args)?;
            st.dllist(|d| Value::Int(to_long(d.list.count)))
        }
        "isempty" => {
            Args::none(func, args)?;
            let n = count_elements(host, this, &st)?;
            Ok(Value::Bool(n == 0))
        }
        "setiteratormode" => {
            let a = Args::new(func, args, &["mode"], 1, Some(1))?;
            let value = a.long(host, 0)?;
            st.dllist(|d| {
                if d.flags & IT_FIX != 0 && (d.flags & IT_LIFO) != (value & IT_LIFO) {
                    return Err(runtime(
                        "Iterators' LIFO/FIFO modes for SplStack/SplQueue objects are frozen",
                    ));
                }
                d.flags = (value & IT_MASK) | (d.flags & IT_FIX);
                Ok(Value::Int(d.flags))
            })?
        }
        "getiteratormode" => {
            Args::none(func, args)?;
            st.dllist(|d| Value::Int(d.flags))
        }
        "offsetexists" => {
            let a = Args::new(func, args, &["index"], 1, Some(1))?;
            let index = a.long(host, 0)?;
            st.dllist(|d| Value::Bool(index >= 0 && index < to_long(d.list.count)))
        }
        "offsetget" => {
            let a = Args::new(func, args, &["index"], 1, Some(1))?;
            let index = a.long(host, 0)?;
            let r = st.dllist(|d| {
                if index < 0 || index >= to_long(d.list.count) {
                    return Err(0);
                }
                match d.list.offset(index, d.flags & IT_LIFO != 0) {
                    Some(n) => Ok(n.borrow().data.clone().unwrap_or_default()),
                    None => Err(1),
                }
            })?;
            match r {
                Ok(v) => Ok(v.deref()),
                Err(0) => Err(out_of_range(&a)),
                Err(_) => Err(invalid_offset(&a)),
            }
        }
        "offsetset" => {
            let a = Args::new(func, args, &["index", "value"], 2, Some(2))?;
            let index = a.long_or_null(host, 0)?;
            let value = a.raw(1);
            let r = st.dllist(|d| match index {
                None => {
                    d.list.push(value);
                    Ok(None)
                }
                Some(index) => {
                    if index < 0 || index >= to_long(d.list.count) {
                        return Err(0);
                    }
                    match d.list.offset(index, d.flags & IT_LIFO != 0) {
                        Some(n) => Ok(n.borrow_mut().data.replace(value)),
                        None => Err(1),
                    }
                }
            })?;
            match r {
                Ok(garbage) => {
                    drop(garbage);
                    Ok(Value::Null)
                }
                Err(0) => Err(out_of_range(&a)),
                Err(_) => Err(invalid_offset(&a)),
            }
        }
        "offsetunset" => {
            let a = Args::new(func, args, &["index"], 1, Some(1))?;
            let index = a.long(host, 0)?;
            let r = st.dllist(|d| {
                if index < 0 || index >= to_long(d.list.count) {
                    return Err(0);
                }
                match d.list.offset(index, d.flags & IT_LIFO != 0) {
                    Some(n) => {
                        d.list.unlink(&n);
                        if d.traverse.as_ref().is_some_and(|t| Rc::ptr_eq(t, &n)) {
                            d.traverse = None;
                        }
                        let garbage = n.borrow_mut().data.take();
                        Ok(garbage)
                    }
                    None => Err(1),
                }
            })?;
            match r {
                Ok(garbage) => {
                    drop(garbage);
                    Ok(Value::Null)
                }
                Err(0) => Err(out_of_range(&a)),
                Err(_) => Err(invalid_offset(&a)),
            }
        }
        "key" => {
            Args::none(func, args)?;
            st.dllist(|d| Value::Int(d.position))
        }
        "prev" | "next" => {
            Args::none(func, args)?;
            let garbage = st.dllist(|d| {
                let flags = if method == "prev" {
                    d.flags ^ IT_LIFO
                } else {
                    d.flags
                };
                let mut ptr = d.traverse.take();
                let mut pos = d.position;
                let g = move_forward(&mut d.list, &mut ptr, &mut pos, flags);
                d.traverse = ptr;
                d.position = pos;
                g
            })?;
            drop(garbage);
            Ok(Value::Null)
        }
        "valid" => {
            Args::none(func, args)?;
            st.dllist(|d| Value::Bool(d.traverse.is_some()))
        }
        "rewind" => {
            Args::none(func, args)?;
            st.dllist(|d| {
                let (p, pos) = rewind_helper(&d.list, d.flags);
                d.traverse = p;
                d.position = pos;
            })?;
            Ok(Value::Null)
        }
        "current" => {
            Args::none(func, args)?;
            st.dllist(|d| {
                d.traverse
                    .as_ref()
                    .and_then(|n| n.borrow().data.clone())
                    .map_or(Value::Null, |v| v.deref())
            })
        }
        "serialize" => {
            Args::none(func, args)?;
            let (flags, head) = st.dllist(|d| (d.flags, d.list.head.clone()))?;
            let mut buf = php_runtime::serialize::serialize(&Value::Int(flags), host)?;
            let mut current = head;
            while let Some(node) = current {
                buf.push(b':');
                let next = next_of(&node);
                let data = node.borrow().data.clone().unwrap_or_default();
                buf.extend(php_runtime::serialize::serialize(&data, host)?);
                // Stop when the next element was removed meanwhile
                // (`SPL_LLIST_CHECK_DELREF_EX(next, break)`).
                if next.as_ref().is_some_and(|n| n.borrow().data.is_none()) {
                    break;
                }
                current = next;
            }
            Ok(Value::Str(buf.into()))
        }
        "unserialize" => {
            let a = Args::new(func, args, &["data"], 1, Some(1))?;
            let data = a.str(host, 0)?;
            unserialize_legacy(host, &st, data.as_bytes())?;
            Ok(Value::Null)
        }
        "__serialize" => {
            Args::none(func, args)?;
            let (flags, values) = st.dllist(|d| (d.flags, d.list.values()))?;
            let props = host.std_properties(this);
            Ok(Value::Array(ZArray::from_values([
                Value::Int(flags),
                Value::Array(ZArray::from_values(values)),
                Value::Array(ZArray::from_table(props.to_symtable())),
            ])))
        }
        "__unserialize" => {
            let a = Args::new(func, args, &["data"], 1, Some(1))?;
            let data = a.array(0)?;
            let flags = data.get_int(0).map(Value::deref);
            let storage = data.get_int(1).map(Value::deref);
            let members = data.get_int(2).map(Value::deref);
            let (Some(Value::Int(flags)), Some(Value::Array(storage)), Some(Value::Array(members))) =
                (flags, storage, members)
            else {
                return Err(unexpected_value(
                    "Incomplete or ill-typed serialization data",
                ));
            };
            st.dllist(|d| {
                d.flags = flags;
                for v in storage.values() {
                    d.list.push(v.clone());
                }
            })?;
            host.properties_load(this, &members)?;
            Ok(Value::Null)
        }
        "add" => {
            let a = Args::new(func, args, &["index", "value"], 2, Some(2))?;
            let index = a.long(host, 0)?;
            let value = a.raw(1);
            let ok = st.dllist(|d| {
                if index < 0 || index > to_long(d.list.count) {
                    return false;
                }
                if index == to_long(d.list.count) {
                    d.list.push(value);
                    return true;
                }
                let Some(element) = d.list.offset(index, d.flags & IT_LIFO != 0) else {
                    return false;
                };
                let prev = prev_of(&element);
                let elem = Rc::new(RefCell::new(Node {
                    prev: prev.as_ref().map(Rc::downgrade),
                    next: Some(element.clone()),
                    data: Some(value),
                }));
                match &prev {
                    None => d.list.head = Some(elem.clone()),
                    Some(p) => p.borrow_mut().next = Some(elem.clone()),
                }
                element.borrow_mut().prev = Some(Rc::downgrade(&elem));
                d.list.count += 1;
                true
            })?;
            if ok {
                Ok(Value::Null)
            } else {
                Err(out_of_range(&a))
            }
        }
        "__debuginfo" => {
            Args::none(func, args)?;
            debug_info(host, this, &st).map(Value::Array)
        }
        _ => Err(RuntimeError::Error(format!(
            "Call to undefined method {func}()"
        ))),
    }
}

fn unserialize_legacy(host: &mut dyn SplHost, st: &SplObject, buf: &[u8]) -> RtResult<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let old: Vec<Value> = st.dllist(|d| {
        let mut out = Vec::new();
        while let Some(v) = d.list.pop() {
            out.push(v);
        }
        out
    })?;
    drop(old);
    let err = |p: usize| unexpected_value(format!("Error at offset {p} of {} bytes", buf.len()));
    let mut p = 0usize;
    let (flags, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
    let Value::Int(flags) = flags else {
        return Err(err(np));
    };
    p = np;
    st.dllist(|d| d.flags = flags)?;
    while buf.get(p) == Some(&b':') {
        p += 1;
        let (elem, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
        p = np;
        st.dllist(|d| d.list.push(elem))?;
    }
    if p < buf.len() {
        return Err(err(p));
    }
    Ok(())
}

/// `spl_dllist_object_get_debug_info`.
pub fn debug_info(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<ZArray> {
    let props = host.std_properties(this);
    let mut out: HashTable = (*props).clone();
    let (flags, values) = st.dllist(|d| (d.flags, d.list.values()))?;
    out.insert(
        ArrayKey::Str(private_prop("SplDoublyLinkedList", "flags")),
        Value::Int(flags),
    );
    out.insert(
        ArrayKey::Str(private_prop("SplDoublyLinkedList", "dllist")),
        Value::Array(ZArray::from_values(values)),
    );
    Ok(ZArray::from_table(out))
}

/// `count_elements` handler.
pub fn count_elements(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>) -> RtResult<i64> {
    if host.has_user_method(obj, "count") {
        let r = host.call_method(obj, "count", &[])?;
        return r.to_long(host);
    }
    st.dllist(|d| to_long(d.list.count))
}

/// The `foreach` iterator (`spl_dllist_it`): starts at the object's
/// current traverse position with the object's mode.
#[derive(Debug)]
pub struct DlListIter {
    st: Rc<SplObject>,
    ptr: Option<NodeRef>,
    pos: i64,
    flags: i64,
}

impl DlListIter {
    /// Creates the iterator.
    pub fn new(st: Rc<SplObject>) -> RtResult<Self> {
        let (ptr, pos, flags) =
            st.dllist(|d| (d.traverse.clone(), d.position, d.flags & IT_MASK))?;
        Ok(DlListIter {
            st,
            ptr,
            pos,
            flags,
        })
    }
}

impl ObjectIterator for DlListIter {
    fn rewind(&mut self, _host: &mut dyn SplHost) -> RtResult<()> {
        let flags = self.flags;
        let (p, pos) = self.st.dllist(|d| rewind_helper(&d.list, flags))?;
        self.ptr = p;
        self.pos = pos;
        Ok(())
    }
    fn valid(&mut self, _host: &mut dyn SplHost) -> RtResult<bool> {
        Ok(self.ptr.is_some())
    }
    fn current(&mut self, _host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        Ok(self.ptr.as_ref().and_then(|n| n.borrow().data.clone()))
    }
    fn key(&mut self, _host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        Ok(Some(Value::Int(self.pos)))
    }
    fn next(&mut self, _host: &mut dyn SplHost) -> RtResult<()> {
        let flags = self.flags;
        let mut ptr = self.ptr.take();
        let mut pos = self.pos;
        let garbage = self
            .st
            .dllist(|d| move_forward(&mut d.list, &mut ptr, &mut pos, flags))?;
        self.ptr = ptr;
        self.pos = pos;
        drop(garbage);
        Ok(())
    }
}
