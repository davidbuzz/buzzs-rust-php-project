//! `ext/standard/streamsfuncs.c`: stream meta functions, contexts and
//! `stream_select()`.

use std::rc::Rc;

use php_runtime::{ArrayKey, Resource, Value, ZArray, ZStr};
use php_streams::context::{ContextRef, StreamContext};
use php_streams::file as lib;
use php_streams::stream::{CastAs, Stream};
use php_streams::{paths, state, transports, wrapper};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge::{self, bytes};
use crate::files::arg_error;
use crate::res::{self, path_arg, str_arg, stream_arg, with_stream, CtxCell};

fn opt_int(a: &Args, n: usize) -> Option<i64> {
    a.opt(n).map(|_| a.int(n))
}

fn stream_get_contents(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let maxlen = opt_int(a, 1);
    let offset = a.int(2);
    match with_stream(vm, &cell, |s| lib::stream_get_contents(s, maxlen, offset))? {
        Some(Ok(Some(v))) => Ok(bytes(v)),
        Some(Err(e)) => Err(arg_error(vm, a, &e)),
        _ => Ok(Value::Bool(false)),
    }
}

fn stream_get_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let len = a.int(1);
    let ending = str_arg(a, 2);
    match with_stream(vm, &cell, |s| lib::stream_get_line(s, len, &ending))? {
        Some(Ok(Some(v))) => Ok(bytes(v)),
        Some(Err(e)) => Err(arg_error(vm, a, &e)),
        _ => Ok(Value::Bool(false)),
    }
}

fn stream_copy_to_stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let src = stream_arg(vm, a, 0)?;
    let dest = stream_arg(vm, a, 1)?;
    let maxlen = opt_int(a, 2);
    let pos = a.int(3);
    let _ = res::context_arg(vm, a, 4, false)?;
    crate::files::sync_stdio(vm, &dest);
    let r = bridge::run(vm, || {
        if Rc::ptr_eq(&src, &dest) {
            return None;
        }
        let mut s = src.get()?;
        let mut d = dest.get()?;
        lib::stream_copy_to_stream(&mut s, &mut d, maxlen, pos)
    })?;
    Ok(r.map_or(Value::Bool(false), Value::Int))
}

fn stream_get_meta_data(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let meta = with_stream(vm, &cell, Stream::meta_data)?.unwrap_or_default();
    let crypto = crate::xport::crypto_hooks()
        .meta
        .and_then(|f| cell.get().and_then(|s| f(&s)));
    let mut arr = ZArray::new();
    let wrapper_data = cell.wrapper_data.borrow().clone();
    for (k, v) in meta {
        if k == "wrapper_type" {
            if let Some(w) = &wrapper_data {
                arr.make_mut().insert("wrapper_data", w.clone());
            }
        }
        arr.make_mut().insert(ArrayKey::Str(ZStr::from(k.as_str())), bridge::from_lib(&v));
    }
    if let Some(c) = crypto {
        arr.make_mut().insert("crypto", c);
    }
    Ok(Value::Array(arr))
}

fn stream_set_blocking(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let enable = a.bool(1);
    let r = with_stream(vm, &cell, |s| lib::stream_set_blocking(s, enable))?;
    Ok(Value::Bool(r.unwrap_or(false)))
}

fn stream_set_timeout(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let sec = a.int(1);
    let usec = a.int(2);
    let r = with_stream(vm, &cell, |s| lib::stream_set_timeout(s, sec, usec))?;
    Ok(Value::Bool(r.unwrap_or(false)))
}

fn set_buffer(vm: &mut Vm, a: &Args, read: bool) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let size = a.int(1);
    let r = with_stream(vm, &cell, |s| lib::stream_set_buffer(s, read, size))?;
    Ok(Value::Int(r.unwrap_or(-1)))
}

fn stream_set_write_buffer(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_buffer(vm, a, false)
}

fn stream_set_read_buffer(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_buffer(vm, a, true)
}

fn stream_set_chunk_size(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let size = a.int(1);
    match with_stream(vm, &cell, |s| lib::stream_set_chunk_size(s, size))? {
        Some(Ok(n)) => Ok(Value::Int(n)),
        Some(Err(e)) => Err(arg_error(vm, a, &e)),
        None => Ok(Value::Int(-1)),
    }
}

fn stream_is_local(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match a.at(0) {
        Value::Resource(r) => {
            let cell = res::stream_of_resource(vm, a, &r)?;
            let local = cell.get().is_some_and(|s| lib::stream_is_local(&s));
            Ok(Value::Bool(local))
        }
        v => {
            let s = vm.value_to_string(&v)?;
            let _ = res::context_arg(vm, a, 1, false)?;
            let r = bridge::run(vm, || lib::url_is_local(s.as_bytes()))?;
            Ok(Value::Bool(r))
        }
    }
}

fn stream_isatty(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let r = with_stream(vm, &cell, Stream::isatty)?;
    Ok(Value::Bool(r.unwrap_or(false)))
}

fn stream_supports_lock(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let r = with_stream(vm, &cell, Stream::supports_lock)?;
    Ok(Value::Bool(r.unwrap_or(false)))
}

fn stream_resolve_include_path(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = path_arg(vm, a, 0)?;
    let r = bridge::run(vm, || {
        let inc = state::settings().include_path;
        paths::resolve_path(&p, &inc)
    })?;
    Ok(r.map_or(Value::Bool(false), bytes))
}

fn stream_get_wrappers(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut arr = ZArray::new();
    for p in wrapper::protocols() {
        let _ = arr.make_mut().push(bytes(p));
    }
    Ok(Value::Array(arr))
}

fn stream_get_transports(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut arr = ZArray::new();
    for p in transports::transports() {
        let _ = arr.make_mut().push(bytes(p));
    }
    for p in crate::xport::schemes() {
        let _ = arr.make_mut().push(bytes(p));
    }
    Ok(Value::Array(arr))
}

// ---------------------------------------------------------------------------
// stream_select
// ---------------------------------------------------------------------------

/// The streams of a select array with their keys.
fn select_entries(arr: &ZArray) -> Vec<(ArrayKey, Value, Rc<res::StreamCell>)> {
    let mut out = Vec::new();
    for (k, v) in arr.iter() {
        if let Value::Resource(r) = v.deref() {
            if let Some(c) = res::cell_of(&r) {
                if !r.is_closed() && !c.is_closed() {
                    out.push((k.clone(), v.clone(), c));
                }
            }
        }
    }
    out
}

fn select_fd(cell: &res::StreamCell) -> Option<i32> {
    let mut s = cell.get()?;
    let fd = s.cast(CastAs::FdForSelect, true).filter(|&fd| fd >= 0);
    if fd.is_none() {
        // php_stream_cast()'s generic failure report (missing in the library).
        php_streams::diag::warning(format!(
            "Cannot represent a stream of type {} as a select()able descriptor",
            s.ops_label()
        ));
    }
    fd
}

#[allow(clippy::too_many_lines)]
fn stream_select(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arrays: Vec<Option<ZArray>> = (0..3)
        .map(|i| match a.at(i) {
            Value::Array(arr) => Some(arr),
            _ => None,
        })
        .collect();
    let sec = opt_int(a, 3);
    let usec = opt_int(a, 4);
    let _ = res::context_arg(vm, a, 5, false)?;
    let entries: Vec<Vec<(ArrayKey, Value, Rc<res::StreamCell>)>> = arrays
        .iter()
        .map(|o| o.as_ref().map(select_entries).unwrap_or_default())
        .collect();
    let fds: Vec<Vec<Option<i32>>> = bridge::run(vm, || {
        entries
            .iter()
            .map(|e| e.iter().map(|(_, _, c)| select_fd(c)).collect())
            .collect()
    })?;
    let sets = fds
        .iter()
        .filter(|f| f.iter().any(Option::is_some))
        .count();
    if sets == 0 {
        return Err(vm.throw("ValueError", "No stream arrays were passed"));
    }
    if sec.is_none() && usec.is_some_and(|u| u != 0) {
        return Err(a.value_error(vm, 5, "must be null when argument #4 ($seconds) is null"));
    }
    let timeout_ms: i32 = match sec {
        None => -1,
        Some(s) if s < 0 => {
            return Err(a.value_error(vm, 4, "must be greater than or equal to 0"));
        }
        Some(s) => {
            let u = usec.unwrap_or(0);
            if u < 0 {
                return Err(a.value_error(vm, 5, "must be greater than or equal to 0"));
            }
            let ms = s.saturating_mul(1000).saturating_add(u / 1000);
            i32::try_from(ms).unwrap_or(i32::MAX)
        }
    };
    // Buffered data counts as readable.
    if arrays[0].is_some() {
        let mut ready = ZArray::new();
        let mut n = 0;
        for (k, v, c) in &entries[0] {
            if c.get().is_some_and(|s| s.unread_bytes() > 0) {
                ready.make_mut().insert(k.clone(), v.clone());
                n += 1;
            }
        }
        if n > 0 {
            a.set_ref(0, Value::Array(ready));
            for i in 1..3 {
                if arrays[i].is_some() {
                    a.set_ref(i, Value::Array(ZArray::new()));
                }
            }
            return Ok(Value::Int(n));
        }
    }
    let mut pfds: Vec<libc::pollfd> = Vec::new();
    let events = [libc::POLLIN, libc::POLLOUT, libc::POLLPRI];
    let mut index: Vec<(usize, usize, usize)> = Vec::new();
    for (set, f) in fds.iter().enumerate() {
        for (i, fd) in f.iter().enumerate() {
            if let Some(fd) = fd {
                index.push((set, i, pfds.len()));
                pfds.push(libc::pollfd {
                    fd: *fd,
                    events: events[set],
                    revents: 0,
                });
            }
        }
    }
    let nfds = libc::nfds_t::try_from(pfds.len()).unwrap_or(0);
    // SAFETY: `pfds` is a valid, initialised array of `nfds` pollfd
    // structures that outlives the call.
    let rc = unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout_ms) };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        let errno = e.raw_os_error().unwrap_or(0);
        let max_fd = pfds.iter().map(|p| p.fd).max().unwrap_or(0);
        vm.php_warning(&format!(
            "Unable to select [{errno}]: {} (max_fd={max_fd})",
            php_streams::sys::strerror(errno)
        ))?;
        return Ok(Value::Bool(false));
    }
    let mut total = 0;
    for set in 0..3 {
        if arrays[set].is_none() {
            continue;
        }
        let mut out = ZArray::new();
        for &(s, i, p) in &index {
            if s != set {
                continue;
            }
            let re = pfds[p].revents;
            let hit = match set {
                0 => re & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0,
                1 => re & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) != 0,
                _ => re & libc::POLLPRI != 0,
            };
            if hit {
                let (k, v, _) = &entries[set][i];
                out.make_mut().insert(k.clone(), v.clone());
                total += 1;
            }
        }
        a.set_ref(set, Value::Array(out));
    }
    Ok(Value::Int(total))
}

// ---------------------------------------------------------------------------
// Contexts
// ---------------------------------------------------------------------------

/// `parse_context_options`.
fn parse_options(vm: &mut Vm, cell: &CtxCell, options: &ZArray) -> VmResult<bool> {
    for (wk, wv) in options.iter() {
        let wv = wv.deref();
        match (wk, &wv) {
            (ArrayKey::Str(w), Value::Array(opts)) => {
                for (ok, ov) in opts.iter() {
                    if let ArrayKey::Str(o) = ok {
                        set_option(cell, w.as_bytes(), o.as_bytes(), ov.deref());
                    }
                }
            }
            _ => {
                return Err(vm.throw(
                    "ValueError",
                    "Options should have the form [\"wrappername\"][\"optionname\"] = $value",
                ))
            }
        }
    }
    Ok(true)
}

/// `php_stream_context_set_option`, keeping the VM-side mirror.
fn set_option(cell: &CtxCell, w: &[u8], o: &[u8], v: Value) {
    let ws = String::from_utf8_lossy(w).into_owned();
    let os = String::from_utf8_lossy(o).into_owned();
    cell.ctx.borrow_mut().set(&ws, &os, bridge::to_lib(&v));
    let mut mirror = cell.options.borrow_mut();
    let wkey = ArrayKey::Str(ZStr::from(w));
    let slot = mirror.make_mut().lookup(wkey);
    if !matches!(slot, Value::Array(_)) {
        *slot = Value::Array(ZArray::new());
    }
    if let Value::Array(inner) = slot {
        inner.make_mut().insert(ArrayKey::Str(ZStr::from(o)), v);
    }
}

/// `parse_context_params`.
fn parse_params(vm: &mut Vm, a: &Args, cell: &CtxCell, params: &ZArray) -> VmResult<()> {
    if let Some(n) = params.get_str(b"notification") {
        let n = n.deref();
        if !vm.is_callable(&n) {
            return Err(a.type_error(
                vm,
                1,
                "must be an array with valid callbacks as values, no array or string given",
            ));
        }
        *cell.notifier.borrow_mut() = Some(n);
    }
    if let Some(o) = params.get_str(b"options") {
        match o.deref() {
            Value::Array(opts) => {
                parse_options(vm, cell, &opts)?;
            }
            _ => return Err(vm.throw("TypeError", "Invalid stream/context parameter")),
        }
    }
    Ok(())
}

/// `decode_context_param`: a context, or the context of a stream
/// (created when the stream has none).
fn decode_context(vm: &mut Vm, a: &Args) -> VmResult<Rc<CtxCell>> {
    let r = res::resource_arg(vm, a, 0)?;
    if let Some(c) = res::ctx_cell_of(&r) {
        return Ok(c);
    }
    if let Some(cell) = res::cell_of(&r).filter(|c| !r.is_closed() && !c.is_closed()) {
        let existing = cell.get().and_then(|s| s.context.clone());
        if let Some(ctx) = existing {
            if let Some(c) = res::ctx_cell_of_lib(&ctx) {
                return Ok(c);
            }
        }
        let v = res::new_context(StreamContext::new());
        if let Value::Resource(cr) = &v {
            if let Some(c) = res::ctx_cell_of(cr) {
                if let Some(mut s) = cell.get() {
                    s.context = Some(Rc::clone(&c.ctx));
                }
                cell.keep_context(v.clone());
                return Ok(c);
            }
        }
    }
    Err(a.type_error(vm, 1, "must be a valid stream/context"))
}

fn ctx_from_value(v: &Value) -> Option<Rc<CtxCell>> {
    match v {
        Value::Resource(r) => res::ctx_cell_of(r),
        _ => None,
    }
}

fn stream_context_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = res::new_context(StreamContext::new());
    let Some(cell) = ctx_from_value(&v) else {
        return Ok(Value::Bool(false));
    };
    if let Some(Value::Array(o)) = a.opt(0) {
        parse_options(vm, &cell, &o)?;
    }
    if let Some(Value::Array(p)) = a.opt(1) {
        parse_params(vm, a, &cell, &p)?;
    }
    Ok(v)
}

fn stream_context_get_options(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = decode_context(vm, a)?;
    let o = c.options.borrow().clone();
    Ok(Value::Array(o))
}

fn stream_context_set_option(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if a.len() == 2 {
        vm.raise(
            8192,
            "Calling stream_context_set_option() with 2 arguments is deprecated, use stream_context_set_options() instead",
        )?;
    }
    let c = decode_context(vm, a)?;
    match a.at(1) {
        Value::Array(opts) => {
            if a.opt(2).is_some() {
                return Err(a.value_error(
                    vm,
                    3,
                    "must be null when argument #2 ($wrapper_or_options) is an array",
                ));
            }
            if a.has(3) {
                return Err(a.value_error(
                    vm,
                    4,
                    "cannot be provided when argument #2 ($wrapper_or_options) is an array",
                ));
            }
            parse_options(vm, &c, &opts)?;
        }
        w => {
            let w = vm.value_to_string(&w)?;
            let Some(name) = a.opt(2) else {
                return Err(a.value_error(
                    vm,
                    3,
                    "cannot be null when argument #2 ($wrapper_or_options) is a string",
                ));
            };
            if !a.has(3) {
                return Err(a.value_error(
                    vm,
                    4,
                    "must be provided when argument #2 ($wrapper_or_options) is a string",
                ));
            }
            let name = vm.value_to_string(&name)?;
            set_option(&c, w.as_bytes(), name.as_bytes(), a.at(3));
        }
    }
    Ok(Value::Bool(true))
}

fn stream_context_set_options(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = decode_context(vm, a)?;
    let opts = a.array(1);
    parse_options(vm, &c, &opts)?;
    Ok(Value::Bool(true))
}

fn stream_context_set_params(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = decode_context(vm, a)?;
    let p = a.array(1);
    parse_params(vm, a, &c, &p)?;
    Ok(Value::Bool(true))
}

fn stream_context_get_params(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = decode_context(vm, a)?;
    let mut arr = ZArray::new();
    if let Some(n) = c.notifier.borrow().clone() {
        arr.make_mut().insert("notification", n);
    }
    arr.make_mut().insert("options", Value::Array(c.options.borrow().clone()));
    Ok(Value::Array(arr))
}

fn default_cell() -> (Resource, Option<Rc<CtxCell>>) {
    let r = res::default_context();
    let c = res::ctx_cell_of(&r);
    (r, c)
}

fn stream_context_get_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (r, c) = default_cell();
    if let (Some(Value::Array(o)), Some(c)) = (a.opt(0), c) {
        parse_options(vm, &c, &o)?;
    }
    Ok(Value::Resource(r))
}

fn stream_context_set_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (r, c) = default_cell();
    let o = a.array(0);
    if let Some(c) = c {
        parse_options(vm, &c, &o)?;
    }
    Ok(Value::Resource(r))
}

/// The library context of a context resource value (for wrappers).
pub fn lib_context(v: &Value) -> Option<ContextRef> {
    ctx_from_value(v).map(|c| Rc::clone(&c.ctx))
}

fn get_resources(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let filter = a.opt(0).map(|_| a.str(0).to_string_lossy());
    let mut arr = ZArray::new();
    for (h, ty, v) in res::live_resources() {
        if filter.as_deref().is_none_or(|f| f == ty) {
            arr.make_mut().insert(h, v);
        }
    }
    Ok(Value::Array(arr))
}

/// Registers the functions of this module.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function("get_resources(?string $type = null): array", get_resources);
    m.function(
        "stream_get_contents($stream, ?int $length = null, int $offset = -1): string|false",
        stream_get_contents,
    )
    .function(
        "stream_get_line($stream, int $length, string $ending = \"\"): string|false",
        stream_get_line,
    )
    .function(
        "stream_copy_to_stream($from, $to, ?int $length = null, int $offset = 0, $context = null): int|false",
        stream_copy_to_stream,
    )
    .function("stream_get_meta_data($stream): array", stream_get_meta_data)
    .function("socket_get_status($stream): array", stream_get_meta_data)
    .function("stream_set_blocking($stream, bool $enable): bool", stream_set_blocking)
    .function("socket_set_blocking($stream, bool $enable): bool", stream_set_blocking)
    .function(
        "stream_set_timeout($stream, int $seconds, int $microseconds = 0): bool",
        stream_set_timeout,
    )
    .function(
        "socket_set_timeout($stream, int $seconds, int $microseconds = 0): bool",
        stream_set_timeout,
    )
    .function("stream_set_write_buffer($stream, int $size): int", stream_set_write_buffer)
    .function("set_file_buffer($stream, int $size): int", stream_set_write_buffer)
    .function("stream_set_read_buffer($stream, int $size): int", stream_set_read_buffer)
    .function("stream_set_chunk_size($stream, int $size): int", stream_set_chunk_size)
    .function("stream_is_local($stream, $context = null): bool", stream_is_local)
    .function("stream_isatty($stream): bool", stream_isatty)
    .function("stream_supports_lock($stream): bool", stream_supports_lock)
    .function(
        "stream_resolve_include_path(string $filename): string|false",
        stream_resolve_include_path,
    )
    .function("stream_get_wrappers(): array", stream_get_wrappers)
    .function("stream_get_transports(): array", stream_get_transports)
    .function(
        "stream_select(?array &$read, ?array &$write, ?array &$except, ?int $seconds, ?int $microseconds = null, $context = null): int|false",
        stream_select,
    )
    .function(
        "stream_context_create(?array $options = null, ?array $params = null)",
        stream_context_create,
    )
    .function("stream_context_set_params($context, array $params): true", stream_context_set_params)
    .function("stream_context_get_params($context): array", stream_context_get_params)
    .function(
        "stream_context_set_option($context, array|string $wrapper_or_options, ?string $option_name = null, mixed $value = UNKNOWN): true",
        stream_context_set_option,
    )
    .function("stream_context_set_options($context, array $options): true", stream_context_set_options)
    .function("stream_context_get_options($stream_or_context): array", stream_context_get_options)
    .function("stream_context_get_default(?array $options = null)", stream_context_get_default)
    .function("stream_context_set_default(array $options)", stream_context_set_default);
}
