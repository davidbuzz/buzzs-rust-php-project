//! Stream and stream-context resources.
//!
//! A stream resource's payload shares a [`StreamCell`] (the library
//! [`Stream`] plus VM-side data). A registry maps library stream ids to
//! cells so values the library hands back (`$this->stream` of user filters,
//! streams returned by user wrappers) can be turned into resources again.
//! When the last reference goes away the stream is closed; if closing needs
//! PHP code (user-space wrapper or filter) and no native call is running,
//! the stream is queued and closed by the next stream function or at
//! request shutdown.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use php_runtime::{Resource, Value, ZArray};
use php_streams::context::{ContextRef, StreamContext};
use php_streams::stream::Stream;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::bridge;

/// Resource type name of streams.
pub const STREAM: &str = "stream";
/// Resource type name of persistent streams.
pub const PERSISTENT_STREAM: &str = "persistent stream";
/// Resource type name of stream contexts.
pub const CONTEXT: &str = "stream-context";

/// The shared state behind a stream resource.
#[derive(Debug)]
pub struct StreamCell {
    stream: RefCell<Option<Stream>>,
    id: Cell<u64>,
    handle: Cell<i64>,
    /// The user-space wrapper object (`wrapper_data` in the meta data).
    pub wrapper_data: RefCell<Option<Value>>,
    /// Opened with `pfsockopen()`.
    pub persistent: bool,
    /// The context resource state attached to the stream (kept alive like
    /// PHP's `stream->ctx` reference).
    ctx_keep: RefCell<Option<Rc<CtxCell>>>,
}

impl StreamCell {
    /// Borrows the stream, `None` when closed or already in use (a user
    /// callback operating on the stream it was called for).
    pub fn get(&self) -> Option<RefMut<'_, Stream>> {
        let b = self.stream.try_borrow_mut().ok()?;
        RefMut::filter_map(b, Option::as_mut).ok()
    }

    /// Whether the stream has been closed.
    pub fn is_closed(&self) -> bool {
        self.stream.try_borrow().is_ok_and(|s| s.is_none())
    }

    /// Removes the stream (for closing).
    pub fn take(&self) -> Option<Stream> {
        self.stream.try_borrow_mut().ok()?.take()
    }

    /// The library stream id.
    pub fn id(&self) -> u64 {
        self.id.get()
    }

    /// Replaces the stream behind the resource (e.g. a TLS upgrade of a
    /// socket), keeping the resource identity. Returns the old stream.
    pub fn replace_stream(self: &Rc<Self>, new: Stream) -> Option<Stream> {
        let new_id = new.id();
        let old = self.stream.try_borrow_mut().ok()?.replace(new);
        STREAMS.with(|m| {
            let mut m = m.borrow_mut();
            m.remove(&self.id.get());
            m.insert(new_id, Rc::downgrade(self));
        });
        self.id.set(new_id);
        old
    }

    /// The resource handle number.
    pub fn handle(&self) -> i64 {
        self.handle.get()
    }

    /// Keeps a context resource alive with the stream.
    pub fn keep_context(&self, v: Value) {
        if let Value::Resource(r) = v {
            *self.ctx_keep.borrow_mut() = ctx_cell_of(&r);
        }
    }
}

/// Whether closing `s` runs PHP code.
/// Whether `s` was opened through a user-space wrapper (file or directory).
pub fn is_user_stream(s: &Stream) -> bool {
    s.ops().is_userspace()
        || s
            .wrapper
            .as_ref()
            .is_some_and(|w| w.label == php_streams::user::LABEL)
}

fn needs_vm(s: &Stream) -> bool {
    is_user_stream(s)
        || s.read_filters
            .filters
            .iter()
            .chain(s.write_filters.filters.iter())
            .any(|f| f.filter.label() == "user-filter")
}

impl Drop for StreamCell {
    fn drop(&mut self) {
        STREAMS.with(|m| {
            if let Ok(mut m) = m.try_borrow_mut() {
                m.remove(&self.id.get());
            }
        });
        let Some(mut s) = self.stream.get_mut().take() else {
            return;
        };
        if needs_vm(&s) && !bridge::has_vm() {
            DEFERRED.with(|d| d.borrow_mut().push(s));
        } else {
            s.close();
        }
    }
}

/// The resource payload of a stream.
#[derive(Debug)]
pub struct StreamRes(pub Rc<StreamCell>);

/// The shared state behind a stream-context resource.
#[derive(Debug)]
pub struct CtxCell {
    /// The library context.
    pub ctx: ContextRef,
    /// `params["notification"]`.
    pub notifier: RefCell<Option<Value>>,
    /// The options as PHP values (what `stream_context_get_options()`
    /// returns).
    pub options: RefCell<ZArray>,
    handle: i64,
}

/// The resource payload of a stream context.
#[derive(Debug)]
pub struct CtxRes(pub Rc<CtxCell>);

thread_local! {
    static STREAMS: RefCell<HashMap<u64, Weak<StreamCell>>> = RefCell::new(HashMap::new());
    static CONTEXTS: RefCell<HashMap<usize, Weak<CtxCell>>> = RefCell::new(HashMap::new());
    static DEFERRED: RefCell<Vec<Stream>> = const { RefCell::new(Vec::new()) };
    /// Persistent ids of open persistent streams.
    static PERSISTENT: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    static DEFAULT_CONTEXT: RefCell<Option<Resource>> = const { RefCell::new(None) };
}

/// Wraps a library stream in a new resource.
pub fn new_stream(stream: Stream, wrapper_data: Option<Value>) -> Value {
    new_stream_ex(stream, wrapper_data, false)
}

/// Whether `ctx` is `FG(default_context)`.
pub fn is_default_context(ctx: &ContextRef) -> bool {
    DEFAULT_CONTEXT.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(ctx_cell_of)
            .is_some_and(|c| Rc::ptr_eq(&c.ctx, ctx))
    })
}

/// Wraps a library stream in a new (optionally persistent) resource.
pub fn new_stream_ex(mut stream: Stream, wrapper_data: Option<Value>, persistent: bool) -> Value {
    // A stream opened with the default context does not keep it.
    if stream.context.as_ref().is_some_and(is_default_context) {
        stream.context = None;
    }
    let id = stream.id();
    let cell = Rc::new(StreamCell {
        ctx_keep: RefCell::new(stream.context.as_ref().and_then(ctx_cell_of_lib)),
        stream: RefCell::new(Some(stream)),
        id: Cell::new(id),
        handle: Cell::new(0),
        wrapper_data: RefCell::new(wrapper_data),
        persistent,
    });
    let ty = if persistent {
        PERSISTENT_STREAM
    } else {
        STREAM
    };
    let r = Resource::new(ty, Some(Box::new(StreamRes(Rc::clone(&cell)))));
    cell.handle.set(r.handle());
    STREAMS.with(|m| m.borrow_mut().insert(id, Rc::downgrade(&cell)));
    Value::Resource(r)
}

/// Wraps a socket stream; with a persistent id it becomes a `persistent
/// stream` found again by [`persistent_stream`].
pub fn new_persistent(stream: Stream, pid: Option<String>) -> Value {
    let id = stream.id();
    let v = new_stream_ex(stream, None, pid.is_some());
    if let Some(p) = pid {
        PERSISTENT.with(|m| m.borrow_mut().insert(p, id));
    }
    v
}

/// `php_stream_from_persistent_id()`: the live stream opened with `pid`.
pub fn persistent_stream(pid: &str) -> Option<Value> {
    let id = PERSISTENT.with(|m| m.borrow().get(pid).copied())?;
    let v = stream_value_by_id(id);
    if v.is_none() {
        PERSISTENT.with(|m| m.borrow_mut().remove(pid));
    }
    v
}

/// Wraps a library stream in a resource with a fixed handle (`STDIN` & co.).
pub fn new_stream_with_handle(stream: Stream, handle: i64) -> Value {
    let id = stream.id();
    let cell = Rc::new(StreamCell {
        stream: RefCell::new(Some(stream)),
        id: Cell::new(id),
        handle: Cell::new(handle),
        wrapper_data: RefCell::new(None),
        persistent: false,
        ctx_keep: RefCell::new(None),
    });
    let r = Resource::with_handle(handle, STREAM, Some(Box::new(StreamRes(Rc::clone(&cell)))));
    STREAMS.with(|m| m.borrow_mut().insert(id, Rc::downgrade(&cell)));
    Value::Resource(r)
}

/// The cell of a stream resource (open or not).
pub fn cell_of(r: &Resource) -> Option<Rc<StreamCell>> {
    r.data()
        .as_ref()
        .and_then(|d| d.downcast_ref::<StreamRes>())
        .map(|s| Rc::clone(&s.0))
}

/// The library stream id of a stream resource.
pub fn stream_id_of(r: &Resource) -> Option<u64> {
    cell_of(r).map(|c| c.id())
}

/// A resource value for the live stream with library id `id`.
pub fn stream_value_by_id(id: u64) -> Option<Value> {
    let cell = STREAMS.with(|m| m.borrow().get(&id).and_then(Weak::upgrade))?;
    if cell.is_closed() {
        return None;
    }
    let ty = if cell.persistent {
        PERSISTENT_STREAM
    } else {
        STREAM
    };
    let r = Resource::with_handle(
        cell.handle(),
        ty,
        Some(Box::new(StreamRes(Rc::clone(&cell)))),
    );
    Some(Value::Resource(r))
}

/// Closes streams whose last reference was dropped while no native call
/// was running.
pub fn close_deferred(vm: &mut Vm) {
    loop {
        let pending = DEFERRED.with(|d| std::mem::take(&mut *d.borrow_mut()));
        if pending.is_empty() {
            break;
        }
        for mut s in pending {
            let _ = bridge::run(vm, || s.close());
        }
    }
}

/// Request shutdown: closes every stream still open, most recent first
/// (`zend_close_rsrc_list`).
pub fn shutdown(vm: &mut Vm) {
    close_deferred(vm);
    let mut cells: Vec<Rc<StreamCell>> =
        STREAMS.with(|m| m.borrow().values().filter_map(Weak::upgrade).collect());
    cells.sort_by_key(|c| std::cmp::Reverse(c.handle()));
    for c in cells {
        if c.handle() <= 3 {
            continue;
        }
        if let Some(mut s) = c.take() {
            let _ = bridge::run(vm, || s.close());
        }
    }
    close_deferred(vm);
}

/// `get_resources()`: the live stream and context resources (the ones this
/// extension owns), by handle.
pub fn live_resources() -> Vec<(i64, &'static str, Value)> {
    let mut out: Vec<(i64, &'static str, Value)> = Vec::new();
    let cells: Vec<Rc<StreamCell>> =
        STREAMS.with(|m| m.borrow().values().filter_map(Weak::upgrade).collect());
    for c in cells {
        if c.is_closed() {
            continue;
        }
        let ty = if c.persistent {
            PERSISTENT_STREAM
        } else {
            STREAM
        };
        let r = Resource::with_handle(c.handle(), ty, Some(Box::new(StreamRes(Rc::clone(&c)))));
        out.push((c.handle(), ty, Value::Resource(r)));
    }
    let ctxs: Vec<Rc<CtxCell>> =
        CONTEXTS.with(|m| m.borrow().values().filter_map(Weak::upgrade).collect());
    for c in ctxs {
        let h = c.handle;
        let r = Resource::with_handle(h, CONTEXT, Some(Box::new(CtxRes(c))));
        out.push((h, CONTEXT, Value::Resource(r)));
    }
    out.sort_by_key(|(h, _, _)| *h);
    out
}

// ---------------------------------------------------------------------------
// Contexts
// ---------------------------------------------------------------------------

/// Creates a context resource.
pub fn new_context(ctx: StreamContext) -> Value {
    let lib = ctx.shared();
    let r = Resource::new(CONTEXT, None);
    let cell = Rc::new(CtxCell {
        ctx: Rc::clone(&lib),
        notifier: RefCell::new(None),
        options: RefCell::new(ZArray::new()),
        handle: r.handle(),
    });
    *r.data_mut() = Some(Box::new(CtxRes(Rc::clone(&cell))));
    CONTEXTS.with(|m| {
        m.borrow_mut()
            .insert(Rc::as_ptr(&lib) as usize, Rc::downgrade(&cell));
    });
    Value::Resource(r)
}

/// The cell of a context resource.
pub fn ctx_cell_of(r: &Resource) -> Option<Rc<CtxCell>> {
    if r.is_closed() {
        return None;
    }
    r.data()
        .as_ref()
        .and_then(|d| d.downcast_ref::<CtxRes>())
        .map(|s| Rc::clone(&s.0))
}

/// A resource value for a library context (user wrappers' `$context`).
pub fn context_value(ctx: &ContextRef) -> Value {
    let cell = CONTEXTS.with(|m| {
        m.borrow()
            .get(&(Rc::as_ptr(ctx) as usize))
            .and_then(Weak::upgrade)
    });
    match cell {
        Some(c) => {
            let handle = c.handle;
            Value::Resource(Resource::with_handle(
                handle,
                CONTEXT,
                Some(Box::new(CtxRes(c))),
            ))
        }
        None => Value::Null,
    }
}

/// The context cell of a library context.
pub fn ctx_cell_of_lib(ctx: &ContextRef) -> Option<Rc<CtxCell>> {
    CONTEXTS.with(|m| {
        m.borrow()
            .get(&(Rc::as_ptr(ctx) as usize))
            .and_then(Weak::upgrade)
    })
}

/// `FG(default_context)`, created on first use.
pub fn default_context() -> Resource {
    DEFAULT_CONTEXT.with(|d| {
        let mut d = d.borrow_mut();
        if let Some(r) = d.as_ref() {
            return r.clone();
        }
        let Value::Resource(r) = new_context(StreamContext::new()) else {
            unreachable!("new_context returns a resource")
        };
        *d = Some(r.clone());
        r
    })
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

/// The running function's name (for messages).
pub fn fname(a: &Args) -> String {
    a.func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy())
}

/// Argument `n` (0-based) as a resource, with ZPP's `TypeError`.
pub fn resource_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Resource> {
    match a.at(n) {
        Value::Resource(r) => Ok(r),
        other => Err(a.type_error(
            vm,
            n + 1,
            &format!(
                "must be of type resource, {} given",
                bridge::given_type(&other)
            ),
        )),
    }
}

/// `PHP_Z_PARAM_STREAM`: argument `n` as an open stream.
pub fn stream_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Rc<StreamCell>> {
    let r = resource_arg(vm, a, n)?;
    match cell_of(&r) {
        Some(c) if !r.is_closed() && !c.is_closed() => Ok(c),
        _ => Err(a.type_error(vm, n + 1, "must be an open stream resource")),
    }
}

/// `PHP_Z_PARAM_STREAM` returning the resource too.
pub fn stream_arg_res(vm: &mut Vm, a: &Args, n: usize) -> VmResult<(Resource, Rc<StreamCell>)> {
    let r = resource_arg(vm, a, n)?;
    match cell_of(&r) {
        Some(c) if !r.is_closed() && !c.is_closed() => Ok((r, c)),
        _ => Err(a.type_error(vm, n + 1, "must be an open stream resource")),
    }
}

/// The open stream behind a resource, or the "not a valid stream
/// resource" `TypeError`.
pub fn stream_of_resource(vm: &mut Vm, a: &Args, r: &Resource) -> VmResult<Rc<StreamCell>> {
    match cell_of(r) {
        Some(c) if !r.is_closed() && !c.is_closed() => Ok(c),
        _ => Err(vm.throw(
            "TypeError",
            &format!(
                "{}(): supplied resource is not a valid stream resource",
                fname(a)
            ),
        )),
    }
}

/// `php_stream_context_from_zval`: argument `n` as a context; `null` gives
/// the default context unless `no_default`.
pub fn context_arg(
    vm: &mut Vm,
    a: &Args,
    n: usize,
    no_default: bool,
) -> VmResult<Option<ContextRef>> {
    match a.at(n) {
        Value::Null => {
            if no_default {
                Ok(None)
            } else {
                Ok(ctx_cell_of(&default_context()).map(|c| Rc::clone(&c.ctx)))
            }
        }
        Value::Resource(r) => match ctx_cell_of(&r) {
            Some(c) => Ok(Some(Rc::clone(&c.ctx))),
            None => Err(vm.throw(
                "TypeError",
                &format!(
                    "{}(): supplied resource is not a valid Stream-Context resource",
                    fname(a)
                ),
            )),
        },
        other => Err(a.type_error(
            vm,
            n + 1,
            &format!(
                "must be of type resource or null, {} given",
                bridge::given_type(&other)
            ),
        )),
    }
}

/// A `string` argument as bytes.
pub fn str_arg(a: &Args, n: usize) -> Vec<u8> {
    a.str(n).as_bytes().to_vec()
}

/// A path argument (`Z_PARAM_PATH`): rejects embedded NUL bytes.
pub fn path_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Vec<u8>> {
    let s = str_arg(a, n);
    if s.contains(&0) {
        return Err(a.value_error(vm, n + 1, "must not contain any null bytes"));
    }
    Ok(s)
}

/// Runs `f` on the open stream of `cell` inside a library scope. Returns
/// `None` when the stream is busy (re-entered from its own callback).
pub fn with_stream<R>(
    vm: &mut Vm,
    cell: &StreamCell,
    f: impl FnOnce(&mut Stream) -> R,
) -> VmResult<Option<R>> {
    bridge::run_stream(vm, || cell.get().map(|mut s| f(&mut s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_registry() {
        let s = php_streams::memory::memory_create(php_streams::mode::TempMode::Default);
        let id = s.id();
        let v = new_stream(s, None);
        let Value::Resource(r) = &v else { panic!() };
        assert_eq!(stream_id_of(r), Some(id));
        let alias = stream_value_by_id(id).expect("alias");
        let Value::Resource(ar) = &alias else {
            panic!()
        };
        assert_eq!(ar.handle(), r.handle());
        drop(alias);
        drop(v);
        assert!(stream_value_by_id(id).is_none());
    }

    #[test]
    fn context_alias() {
        let v = new_context(StreamContext::new());
        let Value::Resource(r) = &v else { panic!() };
        let cell = ctx_cell_of(r).expect("ctx");
        let alias = context_value(&cell.ctx);
        let Value::Resource(ar) = &alias else {
            panic!()
        };
        assert_eq!(ar.handle(), r.handle());
    }
}
