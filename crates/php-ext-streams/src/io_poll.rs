//! master's `Io\Poll` API (`ext/standard/io_poll.c`): `Io\Poll\Context`,
//! `Io\Poll\Watcher`, `StreamPollHandle`, the `Backend`/`Event` enums and
//! the exception hierarchy, over the library's [`PollCtx`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::rc::{Rc, Weak};
use std::time::Duration;

use php_runtime::{Obj, Resource, Value, ZArray};
use php_streams::poll::{self, BackendType, PollCtx, PollError};
use php_streams::stream::CastAs;
use php_vm::class::{cflags, Class};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, Throw, VmResult,
};
use php_vm::object;
use php_vm::Vm;

use crate::bridge::bytes;
use crate::res;

const NS: &str = "Io\\Poll\\";

/// The event cases in declaration order (bit `1 << index`).
const EVENTS: [&str; 7] = [
    "Read",
    "Write",
    "Error",
    "HangUp",
    "ReadHangUp",
    "OneShot",
    "EdgeTriggered",
];

/// `php_io_poll_context_object`.
#[derive(Debug)]
struct CtxState {
    ctx: PollCtx,
    /// fd → watcher (insertion ordered, like the C hash table).
    watchers: Vec<(RawFd, Obj, Rc<RefCell<WatcherState>>)>,
    buffered_readers: bool,
}

type CtxRef = Rc<RefCell<CtxState>>;

/// `php_io_poll_watcher_object`.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // the C object's independent flags
struct WatcherState {
    handle: Option<Obj>,
    watched: u32,
    triggered: u32,
    data: Value,
    active: bool,
    closed: bool,
    disarmed: bool,
    reported: bool,
    ctx: Weak<RefCell<CtxState>>,
    fd: RawFd,
    stream_id: Option<u64>,
}

impl WatcherState {
    /// Level `Read` watcher whose handle can have buffered data.
    fn reads_buffered(&self) -> bool {
        self.stream_id.is_some() && self.watched & (poll::READ | poll::ET) == poll::READ
    }

    fn context(&self) -> Option<CtxRef> {
        if self.active {
            self.ctx.upgrade()
        } else {
            None
        }
    }
}

/// The `Context` internal state (`None` until constructed).
#[derive(Debug, Default)]
struct ContextData(Option<CtxRef>);

/// The `StreamPollHandle` internal state.
#[derive(Debug, Default)]
struct HandleData(Option<Resource>);

thread_local! {
    /// Watchers registered per stream id (`stream->poll_watchers`).
    static STREAM_WATCHERS: RefCell<HashMap<u64, Vec<Weak<RefCell<WatcherState>>>>> =
        RefCell::new(HashMap::new());
    static WATCHER_CLASS: RefCell<Option<Rc<Class>>> = const { RefCell::new(None) };
}

fn watcher_of(o: &Obj) -> Option<Rc<RefCell<WatcherState>>> {
    object::internal::<Rc<RefCell<WatcherState>>, _>(o, |w| Rc::clone(w))
}

fn context_of(o: &Obj) -> Option<CtxRef> {
    object::internal::<ContextData, _>(o, |c| c.0.clone()).flatten()
}

fn handle_resource(o: &Obj) -> Option<Resource> {
    object::internal::<HandleData, _>(o, |h| h.0.clone()).flatten()
}

/// `php_stream_poll_handle_get_fd()` with the stream id.
fn handle_fd(o: &Obj) -> Option<(RawFd, u64)> {
    let r = handle_resource(o)?;
    let cell = res::cell_of(&r).filter(|c| !r.is_closed() && !c.is_closed())?;
    let mut s = cell.get()?;
    let fd = s.cast(CastAs::FdForSelect, false)?;
    (fd >= 0).then(|| (fd, cell.id()))
}

/// Whether the handle's stream has unread buffered data.
fn has_buffered_data(o: &Obj) -> bool {
    let Some(r) = handle_resource(o) else {
        return false;
    };
    res::cell_of(&r)
        .filter(|c| !r.is_closed() && !c.is_closed())
        .and_then(|c| c.get().map(|s| s.unread_bytes() > 0))
        .unwrap_or(false)
}

fn exception(vm: &mut Vm, class: &str, msg: &str, code: i64) -> Throw {
    match vm.create_exception(class, msg, code) {
        Ok(o) => Throw::Exception(o),
        Err(t) => t,
    }
}

fn failed(vm: &mut Vm, class: &str, msg: &str, e: PollError) -> Throw {
    exception(vm, &format!("{NS}{class}"), msg, e.code())
}

fn enum_case(vm: &mut Vm, enum_name: &str, case: &str) -> VmResult<Value> {
    vm.call_value(
        &bytes("constant"),
        vec![bytes(format!("{NS}{enum_name}::{case}"))],
    )
}

fn case_name(vm: &mut Vm, v: &Value) -> VmResult<Option<String>> {
    let Value::Object(o) = v.deref() else {
        return Ok(None);
    };
    match vm.get_property(&o, "name")? {
        Value::Str(s) => Ok(Some(s.to_string_lossy())),
        _ => Ok(None),
    }
}

/// `php_io_poll_event_enums_to_events()`: 0 when the array holds anything
/// but `Event` cases.
fn events_of(vm: &mut Vm, arr: &ZArray) -> VmResult<u32> {
    let mut events = 0;
    for (_, v) in arr.iter() {
        let v = v.deref();
        let is_event = matches!(&v, Value::Object(o)
            if o.class_name().to_string_lossy().eq_ignore_ascii_case(&format!("{NS}Event")));
        if !is_event {
            return Ok(0);
        }
        let Some(name) = case_name(vm, &v)? else {
            return Ok(0);
        };
        if let Some(i) = EVENTS.iter().position(|n| *n == name) {
            events |= 1 << i;
        }
    }
    Ok(events)
}

/// `php_io_poll_events_to_event_enums()`.
fn event_list(vm: &mut Vm, events: u32) -> VmResult<Value> {
    let mut a = ZArray::new();
    for (i, name) in EVENTS.iter().enumerate() {
        if events & (1 << i) != 0 {
            let c = enum_case(vm, "Event", name)?;
            let _ = a.make_mut().push(c);
        }
    }
    Ok(Value::Array(a))
}

fn this_watcher(vm: &mut Vm, a: &Args) -> VmResult<Rc<RefCell<WatcherState>>> {
    a.this
        .as_ref()
        .and_then(watcher_of)
        .ok_or_else(|| vm.throw("Error", "Watcher is not initialized"))
}

fn backend_of_this(vm: &mut Vm, a: &Args) -> VmResult<BackendType> {
    let v = a.this.clone().map_or(Value::Null, Value::Object);
    Ok(case_name(vm, &v)?
        .and_then(|n| BackendType::from_name(&n))
        .unwrap_or(BackendType::Auto))
}

// --- Backend ----------------------------------------------------------------

fn backend_get_available(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut out = ZArray::new();
    for b in BackendType::available() {
        let c = enum_case(vm, "Backend", b.name())?;
        let _ = out.make_mut().push(c);
    }
    Ok(Value::Array(out))
}

fn backend_is_available(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(backend_of_this(vm, a)?.is_available()))
}

fn backend_supports_et(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(
        backend_of_this(vm, a)?.supports_edge_triggering(),
    ))
}

// --- StreamPollHandle -----------------------------------------------------------

fn handle_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let (r, _) = res::stream_arg_res(vm, a, 0)?;
    if handle_resource(&this).is_some() {
        return Err(vm.throw("Error", "StreamPollHandle object is already constructed"));
    }
    object::set_internal(&this, HandleData(Some(r)));
    Ok(Value::Null)
}

fn handle_get_stream(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(handle_resource(&a.this_obj()).map_or(Value::Null, Value::Resource))
}

fn handle_is_valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(r) = handle_resource(&a.this_obj()) else {
        return Ok(Value::Bool(false));
    };
    let Some(cell) = res::cell_of(&r).filter(|c| !r.is_closed() && !c.is_closed()) else {
        return Ok(Value::Bool(false));
    };
    let eof = res::with_stream(vm, &cell, php_streams::stream::Stream::eof)?.unwrap_or(true);
    Ok(Value::Bool(!eof))
}

/// `Io\Poll\Handle` cannot be implemented by user classes.
fn handle_implemented(vm: &mut Vm, cls: &Rc<Class>) -> VmResult<()> {
    if cls.flags & cflags::INTERNAL == 0 {
        return Err(vm.fatal("Io\\Poll\\Handle cannot be implemented by user classes"));
    }
    Ok(())
}

// --- Watcher ---------------------------------------------------------------

fn stream_unwatch(w: &Rc<RefCell<WatcherState>>) {
    let Some(id) = w.borrow_mut().stream_id.take() else {
        return;
    };
    STREAM_WATCHERS.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(list) = m.get_mut(&id) {
            list.retain(|x| x.upgrade().is_some_and(|x| !Rc::ptr_eq(&x, w)));
            if list.is_empty() {
                m.remove(&id);
            }
        }
    });
}

fn deactivate(w: &mut WatcherState) {
    w.active = false;
    w.ctx = Weak::new();
    w.fd = -1;
}

/// `php_io_poll_context_retire_watcher()`.
fn retire(ctx: &CtxRef, w: &Rc<RefCell<WatcherState>>) {
    let fd = w.borrow().fd;
    let _ = ctx.borrow_mut().ctx.remove(fd);
    stream_unwatch(w);
    deactivate(&mut w.borrow_mut());
    // Dropping the watcher object may run arbitrary code: do it after the
    // borrow ends.
    let removed = {
        let mut c = ctx.borrow_mut();
        let pos = c.watchers.iter().position(|(f, _, _)| *f == fd);
        pos.map(|p| c.watchers.remove(p))
    };
    drop(removed);
}

/// `php_io_poll_stream_notify_close()`: the stream `id` is being closed.
fn notify_close(id: u64) {
    let Some(list) = STREAM_WATCHERS.with(|m| m.borrow_mut().remove(&id)) else {
        return;
    };
    for w in list.iter().filter_map(Weak::upgrade) {
        w.borrow_mut().stream_id = None;
        w.borrow_mut().closed = true;
        let ctx = w.borrow().context();
        if let Some(ctx) = ctx {
            retire(&ctx, &w);
        }
    }
}

fn watcher_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Error",
        "Cannot directly construct Watcher, use Context::add",
    ))
}

fn watcher_get_handle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = this_watcher(vm, a)?;
    let h = w.borrow().handle.clone();
    Ok(h.map_or(Value::Null, Value::Object))
}

fn watcher_get_watched(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ev = this_watcher(vm, a)?.borrow().watched;
    event_list(vm, ev)
}

fn watcher_get_triggered(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ev = this_watcher(vm, a)?.borrow().triggered;
    event_list(vm, ev)
}

fn watcher_get_data(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = this_watcher(vm, a)?.borrow().data.clone();
    Ok(d)
}

fn watcher_has_triggered(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = this_watcher(vm, a)?;
    let name = case_name(vm, &a.at(0))?.unwrap_or_default();
    let bit = EVENTS.iter().position(|n| *n == name).map_or(0, |i| 1 << i);
    let t = w.borrow().triggered;
    Ok(Value::Bool(t & bit != 0))
}

fn watcher_is_active(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = this_watcher(vm, a)?;
    let active = w.borrow().context().is_some();
    Ok(Value::Bool(active))
}

fn inactive(vm: &mut Vm, what: &str) -> Throw {
    exception(
        vm,
        &format!("{NS}InactiveWatcherException"),
        &format!("Cannot {what} inactive watcher"),
        0,
    )
}

/// `php_io_poll_watcher_modify_events()`.
fn modify_events(vm: &mut Vm, w: &Rc<RefCell<WatcherState>>, events: u32) -> VmResult<()> {
    let Some(ctx) = w.borrow().context() else {
        return Err(inactive(vm, "modify"));
    };
    let fd = w.borrow().fd;
    let r = {
        let mut c = ctx.borrow_mut();
        match c.ctx.modify(fd, events) {
            Err(PollError::NotFound) => c.ctx.add(fd, events),
            other => other,
        }
    };
    if let Err(e) = r {
        return Err(failed(
            vm,
            "FailedWatcherModificationException",
            "Failed to modify watcher in polling system",
            e,
        ));
    }
    let mut s = w.borrow_mut();
    s.watched = events;
    s.disarmed = false;
    if s.reads_buffered() {
        ctx.borrow_mut().buffered_readers = true;
    }
    Ok(())
}

fn events_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<u32> {
    let arr = a.array(n);
    let events = events_of(vm, &arr)?;
    if events == 0 {
        return Err(a.type_error(vm, n + 1, "must be array of Event enums"));
    }
    Ok(events)
}

fn watcher_modify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let events = events_arg(vm, a, 0)?;
    let w = this_watcher(vm, a)?;
    modify_events(vm, &w, events)?;
    if a.has(1) {
        if w.borrow().context().is_none() {
            return Err(inactive(vm, "modify"));
        }
        w.borrow_mut().data = a.at(1);
    }
    Ok(Value::Null)
}

fn watcher_modify_events(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let events = events_arg(vm, a, 0)?;
    let w = this_watcher(vm, a)?;
    modify_events(vm, &w, events)?;
    Ok(Value::Null)
}

fn watcher_modify_data(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = this_watcher(vm, a)?;
    if !w.borrow().active {
        return Err(inactive(vm, "modify"));
    }
    w.borrow_mut().data = a.at(0);
    Ok(Value::Null)
}

fn watcher_remove(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = this_watcher(vm, a)?;
    let ctx = w.borrow().context();
    let Some(ctx) = ctx else {
        // Closing the stream already removed it.
        if w.borrow().closed {
            return Ok(Value::Null);
        }
        return Err(inactive(vm, "remove"));
    };
    retire(&ctx, &w);
    Ok(Value::Null)
}

// --- Context ---------------------------------------------------------------

fn context_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let backend = match a.opt(0) {
        Some(v) => case_name(vm, &v)?
            .and_then(|n| BackendType::from_name(&n))
            .unwrap_or(BackendType::Auto),
        None => BackendType::Auto,
    };
    if context_of(&this).is_some() {
        return Err(vm.throw("Error", "Io\\Poll\\Context object is already constructed"));
    }
    match PollCtx::new(backend) {
        Ok(Some(ctx)) => {
            let state = CtxState {
                ctx,
                watchers: Vec::new(),
                buffered_readers: false,
            };
            object::set_internal(&this, ContextData(Some(Rc::new(RefCell::new(state)))));
            Ok(Value::Null)
        }
        Ok(None) => Err(exception(
            vm,
            &format!("{NS}BackendUnavailableException"),
            &format!("Backend {} not available", backend.name()),
            0,
        )),
        Err(e) => Err(failed(
            vm,
            "FailedContextInitializationException",
            "Failed to initialize polling context",
            e,
        )),
    }
}

fn this_context(vm: &mut Vm, a: &Args) -> VmResult<CtxRef> {
    a.this
        .as_ref()
        .and_then(context_of)
        .ok_or_else(|| vm.throw("Error", "Io\\Poll\\Context object is not initialized"))
}

fn context_add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ctx = this_context(vm, a)?;
    let Value::Object(handle) = a.at(0) else {
        return Ok(Value::Null);
    };
    let events = events_arg(vm, a, 1)?;
    let Some((fd, stream_id)) = handle_fd(&handle) else {
        return Err(exception(
            vm,
            &format!("{NS}InvalidHandleException"),
            "Invalid handle for polling",
            0,
        ));
    };
    let existing = ctx
        .borrow()
        .watchers
        .iter()
        .find(|(f, _, _)| *f == fd)
        .map(|(_, _, w)| Rc::clone(w));
    if let Some(ex) = existing {
        let same = ex
            .borrow()
            .handle
            .as_ref()
            .and_then(handle_fd)
            .is_some_and(|(f, _)| f == fd);
        if same {
            return Err(exception(
                vm,
                &format!("{NS}HandleAlreadyWatchedException"),
                "Handle already added",
                0,
            ));
        }
        retire(&ctx, &ex);
    }
    let Some(cls) = WATCHER_CLASS.with(|c| c.borrow().clone()) else {
        return Ok(Value::Null);
    };
    let obj = vm.new_object(&cls)?;
    let state = Rc::new(RefCell::new(WatcherState {
        handle: Some(handle),
        watched: events,
        data: a.opt(2).unwrap_or(Value::Null),
        fd: -1,
        ..WatcherState::default()
    }));
    object::set_internal(&obj, Rc::clone(&state));
    let added = ctx.borrow_mut().ctx.add(fd, events);
    if let Err(e) = added {
        if e == PollError::Exists {
            return Err(exception(
                vm,
                &format!("{NS}HandleAlreadyWatchedException"),
                "Handle already added",
                0,
            ));
        }
        return Err(failed(
            vm,
            "FailedHandleAddException",
            "Failed to add handle",
            e,
        ));
    }
    {
        let mut s = state.borrow_mut();
        s.active = true;
        s.ctx = Rc::downgrade(&ctx);
        s.fd = fd;
        s.stream_id = Some(stream_id);
    }
    let mut c = ctx.borrow_mut();
    c.watchers.push((fd, obj.clone(), Rc::clone(&state)));
    if state.borrow().reads_buffered() {
        c.buffered_readers = true;
    }
    drop(c);
    STREAM_WATCHERS.with(|m| {
        m.borrow_mut()
            .entry(stream_id)
            .or_default()
            .push(Rc::downgrade(&state));
    });
    Ok(Value::Object(obj))
}

/// The `?Time\Duration $timeout` argument.
fn timeout_arg(vm: &mut Vm, a: &Args) -> VmResult<Option<Duration>> {
    let Some(Value::Object(d)) = a.opt(0) else {
        return Ok(None);
    };
    if matches!(vm.get_property(&d, "negative")?.deref(), Value::Bool(true)) {
        return Err(a.value_error(vm, 1, "must not be negative"));
    }
    let secs = match vm.get_property(&d, "seconds")?.deref() {
        Value::Int(s) => u64::try_from(s).unwrap_or(0),
        _ => 0,
    };
    let nanos = match vm.get_property(&d, "nanoseconds")?.deref() {
        Value::Int(n) => u32::try_from(n).unwrap_or(0),
        _ => 0,
    };
    Ok(Some(Duration::new(secs, nanos)))
}

#[allow(clippy::too_many_lines)] // a direct port of the C method
fn context_wait(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ctx = this_context(vm, a)?;
    let timeout = timeout_arg(vm, a)?;
    let max_events = if a.opt(1).is_none() {
        ctx.borrow().ctx.suitable_max_events()
    } else {
        let m = a.int(1);
        if m <= 0 {
            return Err(a.value_error(vm, 2, "must be greater than 0"));
        }
        if m > i64::from(i32::MAX) {
            return Err(a.value_error(
                vm,
                2,
                &format!("must be less than or equal to {}", i32::MAX),
            ));
        }
        usize::try_from(m).unwrap_or(usize::MAX)
    };
    // Buffered watchers go first, then a non-blocking backend check.
    let mut buffered: Vec<(Obj, Rc<RefCell<WatcherState>>)> = Vec::new();
    if ctx.borrow().buffered_readers {
        let list: Vec<(Obj, Rc<RefCell<WatcherState>>)> = ctx
            .borrow()
            .watchers
            .iter()
            .map(|(_, o, w)| (o.clone(), Rc::clone(w)))
            .collect();
        for (o, w) in list {
            let ok = {
                let s = w.borrow();
                !s.disarmed
                    && s.reads_buffered()
                    && s.handle.as_ref().is_some_and(has_buffered_data)
            };
            if !ok {
                continue;
            }
            buffered.push((o, w));
            if buffered.len() == max_events {
                break;
            }
        }
    }
    let mut events = Vec::new();
    if buffered.len() < max_events {
        let t = if buffered.is_empty() {
            timeout
        } else {
            Some(Duration::ZERO)
        };
        let r = ctx.borrow_mut().ctx.wait(max_events - buffered.len(), t);
        match r {
            Ok(e) => events = e,
            Err(e) => {
                if buffered.is_empty() || e != PollError::Interrupted {
                    return Err(failed(vm, "FailedPollWaitException", "Poll wait failed", e));
                }
            }
        }
    }
    let mut out = ZArray::new();
    for (o, w) in &buffered {
        let mut s = w.borrow_mut();
        s.triggered = poll::READ;
        s.reported = true;
        drop(s);
        let _ = out.make_mut().push(Value::Object(o.clone()));
    }
    for e in events {
        let found = ctx
            .borrow()
            .watchers
            .iter()
            .find(|(f, _, _)| *f == e.fd)
            .map(|(_, o, w)| (o.clone(), Rc::clone(w)));
        let Some((o, w)) = found else {
            continue;
        };
        let mut s = w.borrow_mut();
        if s.watched & poll::ONESHOT != 0 {
            s.disarmed = true;
        }
        if s.reported {
            s.triggered |= e.revents;
            continue;
        }
        s.triggered = e.revents;
        drop(s);
        let _ = out.make_mut().push(Value::Object(o));
    }
    // A buffer-only one-shot is still armed in the backend.
    for (_, w) in &buffered {
        let mut s = w.borrow_mut();
        s.reported = false;
        if s.watched & poll::ONESHOT != 0 && !s.disarmed {
            s.disarmed = true;
            let fd = s.fd;
            drop(s);
            let _ = ctx.borrow_mut().ctx.remove(fd);
        }
    }
    Ok(Value::Array(out))
}

fn context_get_backend(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ctx = this_context(vm, a)?;
    let b = ctx.borrow().ctx.backend();
    enum_case(vm, "Backend", b.name())
}

/// Registers the classes and the stream close hook.
#[allow(clippy::too_many_lines)] // the class table of io_poll.stub.php
pub fn register(m: &mut ModuleBuilder<'_>) {
    php_streams::stream::set_close_hook(Some(Rc::new(notify_close)));
    let vm = m.vm();
    let io = ClassBuilder::new("Io\\IoException")
        .extends(vm, "Exception")
        .register(vm);
    let _ = io;
    let mut backend = ClassBuilder::new("Io\\Poll\\Backend").unit_enum();
    for c in ["Auto", "Poll", "Epoll", "Kqueue", "EventPorts", "WSAPoll"] {
        backend = backend.case(c, None);
    }
    backend
        .method(
            "getAvailableBackends(): array",
            acc::PUBLIC | acc::STATIC,
            backend_get_available,
        )
        .method("isAvailable(): bool", acc::PUBLIC, backend_is_available)
        .method(
            "supportsEdgeTriggering(): bool",
            acc::PUBLIC,
            backend_supports_et,
        )
        .register(vm);
    let mut event = ClassBuilder::new("Io\\Poll\\Event").unit_enum();
    for c in EVENTS {
        event = event.case(c, None);
    }
    event.register(vm);
    ClassBuilder::new("Io\\Poll\\Handle")
        .flags(class_flags::INTERFACE)
        .hooks(ClassHooks {
            interface_gets_implemented: Some(handle_implemented),
            ..ClassHooks::default()
        })
        .register(vm);
    let strict = class_flags::FINAL
        | class_flags::NOT_SERIALIZABLE
        | class_flags::NO_CLONE
        | class_flags::NO_DYNAMIC_PROPERTIES;
    let watcher = ClassBuilder::new("Io\\Poll\\Watcher")
        .flags(strict)
        .method(
            "__construct()",
            acc::PRIVATE | acc::FINAL,
            watcher_construct,
        )
        .method(
            "getHandle(): Io\\Poll\\Handle",
            acc::PUBLIC,
            watcher_get_handle,
        )
        .method(
            "getWatchedEvents(): array",
            acc::PUBLIC,
            watcher_get_watched,
        )
        .method(
            "getTriggeredEvents(): array",
            acc::PUBLIC,
            watcher_get_triggered,
        )
        .method("getData(): mixed", acc::PUBLIC, watcher_get_data)
        .method(
            "hasTriggered(Io\\Poll\\Event $event): bool",
            acc::PUBLIC,
            watcher_has_triggered,
        )
        .method("isActive(): bool", acc::PUBLIC, watcher_is_active)
        .method(
            "modify(array $events, mixed $data = null): void",
            acc::PUBLIC,
            watcher_modify,
        )
        .method(
            "modifyEvents(array $events): void",
            acc::PUBLIC,
            watcher_modify_events,
        )
        .method(
            "modifyData(mixed $data): void",
            acc::PUBLIC,
            watcher_modify_data,
        )
        .method("remove(): void", acc::PUBLIC, watcher_remove)
        .register(vm);
    WATCHER_CLASS.with(|c| *c.borrow_mut() = Some(watcher));
    ClassBuilder::new("Io\\Poll\\Context")
        .flags(strict)
        .method(
            "__construct(Io\\Poll\\Backend $backend = Io\\Poll\\Backend::Auto)",
            acc::PUBLIC,
            context_construct,
        )
        .method(
            "add(Io\\Poll\\Handle $handle, array $events, mixed $data = null): Io\\Poll\\Watcher",
            acc::PUBLIC,
            context_add,
        )
        .method(
            "wait(?Time\\Duration $timeout = null, ?int $maxEvents = null): array",
            acc::PUBLIC,
            context_wait,
        )
        .method(
            "getBackend(): Io\\Poll\\Backend",
            acc::PUBLIC,
            context_get_backend,
        )
        .register(vm);
    let poll_exc = ClassBuilder::new("Io\\Poll\\PollException")
        .extends(vm, "Io\\IoException")
        .register(vm);
    let _ = poll_exc;
    let mut failed_op = ClassBuilder::new("Io\\Poll\\FailedPollOperationException")
        .flags(class_flags::ABSTRACT)
        .extends(vm, "Io\\Poll\\PollException");
    for (name, v) in [
        ("ERROR_NONE", 0),
        ("ERROR_SYSTEM", 1),
        ("ERROR_NOMEM", 2),
        ("ERROR_INVALID", 3),
        ("ERROR_EXISTS", 4),
        ("ERROR_NOTFOUND", 5),
        ("ERROR_TIMEOUT", 6),
        ("ERROR_INTERRUPTED", 7),
        ("ERROR_PERMISSION", 8),
        ("ERROR_TOOBIG", 9),
        ("ERROR_AGAIN", 10),
        ("ERROR_NOSUPPORT", 11),
    ] {
        failed_op = failed_op.constant(name, Value::Int(v));
    }
    failed_op.register(vm);
    for n in [
        "FailedContextInitializationException",
        "FailedHandleAddException",
        "FailedWatcherModificationException",
        "FailedPollWaitException",
    ] {
        ClassBuilder::new(&format!("{NS}{n}"))
            .extends(vm, "Io\\Poll\\FailedPollOperationException")
            .register(vm);
    }
    for n in [
        "BackendUnavailableException",
        "InactiveWatcherException",
        "HandleAlreadyWatchedException",
        "InvalidHandleException",
    ] {
        ClassBuilder::new(&format!("{NS}{n}"))
            .extends(vm, "Io\\Poll\\PollException")
            .register(vm);
    }
    ClassBuilder::new("StreamPollHandle")
        .flags(strict)
        .implements(vm, &["Io\\Poll\\Handle"])
        .method("__construct($stream)", acc::PUBLIC, handle_construct)
        .method("getStream()", acc::PUBLIC, handle_get_stream)
        .method("isValid(): bool", acc::PUBLIC, handle_is_valid)
        .register(vm);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bits_follow_declaration_order() {
        assert_eq!(
            1 << EVENTS.iter().position(|e| *e == "OneShot").unwrap_or(0),
            poll::ONESHOT
        );
        assert_eq!(
            1 << EVENTS
                .iter()
                .position(|e| *e == "EdgeTriggered")
                .unwrap_or(0),
            poll::ET
        );
        let w = WatcherState {
            stream_id: Some(1),
            watched: poll::READ,
            ..WatcherState::default()
        };
        assert!(w.reads_buffered());
    }
}
