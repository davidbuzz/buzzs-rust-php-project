// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/ftp` for the phprust VM: binds the [`php_ftp`] engine through the
//! `php_vm::ext` extension API (`ext/ftp/php_ftp.c`).
//!
//! * `FTP\Connection` is a final, non-serializable, non-cloneable opaque
//!   class whose native state is a shared [`Handle`]. The connection sits
//!   in a `RefCell` that is borrowed for the whole of an operation, so a
//!   user stream wrapper re-entering the extension mid-transfer finds it
//!   busy — PHP's `ftp->in_use` (`"Cannot close FTP\Connection while a
//!   transfer is in progress"`, `"Cannot start a transfer while another
//!   transfer is in progress"`, `"FTP\Connection is already in use"`).
//! * Transfers read and write PHP streams (user wrappers included) inside
//!   `php_ext_streams::bridge::enter`.
//! * On failure the functions emit the server's last reply text as an
//!   `E_WARNING` (`ftp_login(): Login incorrect`), exactly where
//!   `php_ftp.c` does.

mod streamio;

use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use php_ext_streams::{bridge, res};
use php_ftp::{NbStatus, TransferType, AUTORESUME, TIMEOUT_SEC_MAX};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, Extension, ModuleBuilder, Throw, VmResult,
};
use php_vm::{object, Vm};

use streamio::{open, open_for_download, unlink, NullSink, PhpStream, SEEK_END, SEEK_SET};

/// The `ftp` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ftp;

/// The class name.
const CLASS: &str = "FTP\\Connection";
/// The first parameter of every function but the connectors.
const CONN_PARAM: &str = "FTP\\Connection $ftp";
/// `ftp->in_use` warning of the engine functions.
const IN_USE: &str = "FTP\\Connection is already in use";
/// `ftp->in_use` error of the non-blocking starters.
const START_BUSY: &str = "Cannot start a transfer while another transfer is in progress";

/// Native state of an `FTP\Connection` object (`php_ftp_object`).
#[derive(Debug, Default)]
struct Handle {
    /// The connection; `None` once closed. Borrowed for the duration of
    /// every operation.
    conn: RefCell<Option<php_ftp::Ftp>>,
    /// The stream of the pending non-blocking transfer (`ftp->stream`);
    /// owned streams are closed when the transfer ends (`closestream`).
    nb: RefCell<Option<PhpStream>>,
}

/// The connection of an object, or why it is unavailable.
enum Access<'a> {
    /// Ready for use.
    Ready(RefMut<'a, php_ftp::Ftp>),
    /// An operation on it is in progress (re-entered from a callback).
    Busy,
}

/// The handle of argument 0 (an object without state behaves as closed).
fn handle(a: &Args) -> Rc<Handle> {
    if let Value::Object(o) = a.at(0) {
        if let Some(h) = object::internal::<Rc<Handle>, _>(&o, |h| Rc::clone(h)) {
            return h;
        }
    }
    Rc::new(Handle::default())
}

/// `GET_FTPBUF`: the open connection, `Busy`, or the "already closed"
/// `ValueError`.
fn access<'a>(vm: &mut Vm, h: &'a Handle) -> VmResult<Access<'a>> {
    match h.conn.try_borrow_mut() {
        Err(_) => Ok(Access::Busy),
        Ok(g) => match RefMut::filter_map(g, Option::as_mut) {
            Ok(f) => Ok(Access::Ready(f)),
            Err(_) => Err(vm.throw("ValueError", "FTP\\Connection is already closed")),
        },
    }
}

/// Emits engine warnings with the function prefix.
fn emit(vm: &mut Vm, warns: Vec<Vec<u8>>) -> VmResult<()> {
    for w in warns {
        vm.php_warning_bytes(&w)?;
    }
    Ok(())
}

/// Takes the engine's warnings and last reply, then releases the
/// connection (before any warning can run user code).
fn release(mut ftp: RefMut<'_, php_ftp::Ftp>) -> (Vec<Vec<u8>>, Vec<u8>) {
    let warns = ftp.take_warnings();
    let reply = ftp.last_reply().to_vec();
    drop(ftp);
    (warns, reply)
}

/// Runs `f` with the VM lent to stream callbacks; returns its result and
/// an exception raised by a callback (re-raised by the caller once the
/// function's own warnings are out).
fn streams<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> (R, Option<Throw>) {
    let outer = bridge::take_pending();
    let r = bridge::enter(vm, f);
    bridge::flush_diags(vm);
    let inner = bridge::take_pending();
    if let Some(t) = outer {
        bridge::set_pending(t);
    }
    (r, inner)
}

/// `XTYPE`: argument `n` (0-based) as a transfer type.
fn xtype(vm: &mut Vm, a: &Args, n: usize) -> VmResult<TransferType> {
    let mode = if a.has(n) { a.int(n) } else { 2 };
    TransferType::from_mode(mode)
        .ok_or_else(|| a.value_error(vm, n + 1, "must be either FTP_ASCII or FTP_BINARY"))
}

/// An optional `int` argument with a constant default.
fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// A `Z_PARAM_PATH` argument: no NUL bytes.
fn path_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Vec<u8>> {
    let s = a.str(n).as_bytes().to_vec();
    if s.contains(&0) {
        return Err(a.value_error(vm, n + 1, "must not contain any null bytes"));
    }
    Ok(s)
}

fn bytes(b: &[u8]) -> Value {
    Value::Str(ZStr::from(b))
}

fn list_value(lines: Vec<Vec<u8>>) -> Value {
    let mut arr = ZArray::new();
    for l in lines {
        let _ = arr.make_mut().push(bytes(&l));
    }
    Value::Array(arr)
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

fn ctor(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Error",
        "Cannot directly construct FTP\\Connection, use ftp_connect() or ftp_ssl_connect() instead",
    ))
}

fn connect(vm: &mut Vm, a: &Args, ssl: bool) -> VmResult<Value> {
    let host = String::from_utf8_lossy(a.str(0).as_bytes()).into_owned();
    let port = int_or(a, 1, 21);
    let timeout = int_or(a, 2, php_ftp::DEFAULT_TIMEOUT);
    if timeout <= 0 {
        return Err(a.value_error(vm, 3, "must be greater than 0"));
    }
    if timeout.unsigned_abs() >= TIMEOUT_SEC_MAX {
        return Err(a.value_error(vm, 3, &format!("must be less than {TIMEOUT_SEC_MAX}")));
    }
    // `(short) port`, then `(unsigned short)`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port = port as i16 as u16;
    let mut ftp = match php_ftp::Ftp::open(&host, port, timeout) {
        Ok(f) => f,
        Err(warns) => {
            emit(vm, warns)?;
            return Ok(Value::Bool(false));
        }
    };
    ftp.set_use_ssl(ssl);
    let warns = ftp.take_warnings();
    emit(vm, warns)?;
    let Some(cls) = vm.find_class(CLASS.as_bytes()) else {
        return Err(vm.throw("Error", "Class \"FTP\\Connection\" not found"));
    };
    let o: Obj = vm.new_object(&cls)?;
    let h = Handle {
        conn: RefCell::new(Some(ftp)),
        nb: RefCell::new(None),
    };
    object::set_internal(&o, Rc::new(h));
    Ok(Value::Object(o))
}

fn ftp_connect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    connect(vm, a, false)
}

fn ftp_ssl_connect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    connect(vm, a, true)
}

fn ftp_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = handle(a);
    let Ok(mut slot) = h.conn.try_borrow_mut() else {
        return Err(vm.throw(
            "Error",
            "Cannot close FTP\\Connection while a transfer is in progress",
        ));
    };
    let Some(mut ftp) = slot.take() else {
        return Ok(Value::Bool(true));
    };
    drop(slot);
    let ok = ftp.quit();
    let nb = h.nb.try_borrow_mut().ok().and_then(|mut s| s.take());
    let ((), pending) = streams(vm, || {
        ftp.shutdown();
        if let Some(s) = nb {
            s.close();
        }
    });
    let warns = ftp.take_warnings();
    drop(ftp);
    emit(vm, warns)?;
    match pending {
        Some(t) => Err(t),
        None => Ok(Value::Bool(ok)),
    }
}

// ---------------------------------------------------------------------------
// Control-connection commands
// ---------------------------------------------------------------------------

/// Runs a command; `None` from `f` is failure: `false` plus the server's
/// reply as a warning (when non-empty).
fn command(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut php_ftp::Ftp) -> Option<Value>,
) -> VmResult<Value> {
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => {
            vm.php_warning(IN_USE)?;
            return Ok(Value::Bool(false));
        }
    };
    let r = f(&mut ftp);
    let (warns, reply) = release(ftp);
    emit(vm, warns)?;
    if let Some(v) = r {
        return Ok(v);
    }
    if !reply.is_empty() {
        vm.php_warning_bytes(&reply)?;
    }
    Ok(Value::Bool(false))
}

/// Runs a command whose failure carries no reply warning.
fn quiet(
    vm: &mut Vm,
    a: &Args,
    busy: Value,
    f: impl FnOnce(&mut php_ftp::Ftp) -> Value,
) -> VmResult<Value> {
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => return Ok(busy),
    };
    let r = f(&mut ftp);
    let (warns, _) = release(ftp);
    emit(vm, warns)?;
    Ok(r)
}

fn ok(b: bool) -> Option<Value> {
    b.then_some(Value::Bool(true))
}

fn ftp_login(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (user, pass) = (a.str(1), a.str(2));
    command(vm, a, |f| ok(f.login(user.as_bytes(), pass.as_bytes())))
}

fn ftp_pwd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    command(vm, a, |f| f.pwd().map(|p| bytes(&p)))
}

fn ftp_cdup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    command(vm, a, |f| ok(f.cdup()))
}

fn ftp_chdir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = a.str(1);
    command(vm, a, |f| ok(f.chdir(dir.as_bytes())))
}

fn ftp_exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = a.str(1);
    command(vm, a, |f| ok(f.exec(cmd.as_bytes())))
}

fn ftp_raw(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = a.str(1);
    quiet(vm, a, Value::Null, |f| {
        f.raw(cmd.as_bytes()).map_or(Value::Null, list_value)
    })
}

fn ftp_mkdir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = a.str(1);
    command(vm, a, |f| f.mkdir(dir.as_bytes()).map(|d| bytes(&d)))
}

fn ftp_rmdir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = a.str(1);
    command(vm, a, |f| ok(f.rmdir(dir.as_bytes())))
}

fn ftp_chmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mode = a.int(1);
    let file = path_arg(vm, a, 2)?;
    command(vm, a, |f| f.chmod(mode, &file).then_some(Value::Int(mode)))
}

fn ftp_alloc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let size = a.int(1);
    let want = a.has(2);
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => return Ok(Value::Bool(false)),
    };
    let (r, response) = ftp.alloc(size);
    let (warns, _) = release(ftp);
    emit(vm, warns)?;
    if want {
        if let Some(text) = response {
            a.set_ref(2, bytes(&text));
        }
    }
    Ok(Value::Bool(r))
}

fn ftp_nlist(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = path_arg(vm, a, 1)?;
    quiet(vm, a, Value::Bool(false), |f| {
        f.nlist(&dir).map_or(Value::Bool(false), list_value)
    })
}

fn ftp_rawlist(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = a.str(1);
    let recursive = a.has(2) && a.bool(2);
    quiet(vm, a, Value::Bool(false), |f| {
        f.list(dir.as_bytes(), recursive)
            .map_or(Value::Bool(false), list_value)
    })
}

fn ftp_mlsd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = a.str(1);
    let lines = quiet(vm, a, Value::Bool(false), |f| {
        f.mlsd(dir.as_bytes())
            .map_or(Value::Bool(false), list_value)
    })?;
    let Value::Array(lines) = lines else {
        return Ok(lines);
    };
    let mut out = ZArray::new();
    for line in lines.values() {
        let Value::Str(line) = line else { continue };
        match php_ftp::parse_mlsd_line(line.as_bytes()) {
            Ok(entry) => {
                let mut e = ZArray::new();
                for (k, v) in entry {
                    e.make_mut()
                        .insert(ArrayKey::Str(ZStr::from(k.as_slice())), bytes(&v));
                }
                let _ = out.make_mut().push(Value::Array(e));
            }
            Err(err) => vm.php_warning(err.message())?,
        }
    }
    Ok(Value::Array(out))
}

fn ftp_systype(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    command(vm, a, |f| f.syst().map(|s| bytes(&s)))
}

fn ftp_pasv(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let on = a.bool(1);
    quiet(vm, a, Value::Bool(false), |f| Value::Bool(f.pasv(on)))
}

fn ftp_size(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let file = path_arg(vm, a, 1)?;
    quiet(vm, a, Value::Int(-1), |f| Value::Int(f.size(&file)))
}

fn ftp_mdtm(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let file = path_arg(vm, a, 1)?;
    quiet(vm, a, Value::Int(-1), |f| Value::Int(f.mdtm(&file)))
}

fn ftp_rename(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (from, to) = (a.str(1), a.str(2));
    command(vm, a, |f| ok(f.rename(from.as_bytes(), to.as_bytes())))
}

fn ftp_delete(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let file = a.str(1);
    command(vm, a, |f| ok(f.delete(file.as_bytes())))
}

fn ftp_site(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = a.str(1);
    command(vm, a, |f| ok(f.site(cmd.as_bytes())))
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `zend_zval_value_name`.
fn value_name(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => object::class_of(o).name.to_string_lossy(),
        Value::Resource(r) if r.is_closed() => "resource (closed)".into(),
        Value::Resource(_) => "resource".into(),
        Value::Reference(r) => value_name(&r.get()),
    }
}

const BAD_OPTION: &str = "must be one of FTP_TIMEOUT_SEC, FTP_AUTOSEEK, or FTP_USEPASVADDRESS";

fn ftp_set_option(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let option = a.int(1);
    let value = a.at(2);
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => return Ok(Value::Bool(true)),
    };
    match option {
        php_ftp::OPT_TIMEOUT_SEC => {
            let Value::Int(t) = value else {
                drop(ftp);
                let msg = format!(
                    "must be of type int for the FTP_TIMEOUT_SEC option, {} given",
                    value_name(&value)
                );
                return Err(a.type_error(vm, 3, &msg));
            };
            if t <= 0 {
                drop(ftp);
                return Err(a.value_error(
                    vm,
                    3,
                    "must be greater than 0 for the FTP_TIMEOUT_SEC option",
                ));
            }
            if t.unsigned_abs() >= TIMEOUT_SEC_MAX {
                drop(ftp);
                let msg =
                    format!("must be less than {TIMEOUT_SEC_MAX} for the FTP_TIMEOUT_SEC option");
                return Err(a.value_error(vm, 3, &msg));
            }
            ftp.timeout_sec = t;
        }
        php_ftp::OPT_AUTOSEEK | php_ftp::OPT_USEPASVADDRESS => {
            let name = if option == php_ftp::OPT_AUTOSEEK {
                "FTP_AUTOSEEK"
            } else {
                "FTP_USEPASVADDRESS"
            };
            let Value::Bool(b) = value else {
                drop(ftp);
                let msg = format!(
                    "must be of type bool for the {name} option, {} given",
                    value_name(&value)
                );
                return Err(a.type_error(vm, 3, &msg));
            };
            if option == php_ftp::OPT_AUTOSEEK {
                ftp.autoseek = b;
            } else {
                ftp.usepasvaddress = b;
            }
        }
        _ => {
            drop(ftp);
            return Err(a.value_error(vm, 2, BAD_OPTION));
        }
    }
    Ok(Value::Bool(true))
}

fn ftp_get_option(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let option = a.int(1);
    let h = handle(a);
    let ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => return Ok(Value::Bool(false)),
    };
    let v = match option {
        php_ftp::OPT_TIMEOUT_SEC => Value::Int(ftp.timeout_sec),
        php_ftp::OPT_AUTOSEEK => Value::Bool(ftp.autoseek),
        php_ftp::OPT_USEPASVADDRESS => Value::Bool(ftp.usepasvaddress),
        _ => {
            drop(ftp);
            return Err(a.value_error(vm, 2, BAD_OPTION));
        }
    };
    Ok(v)
}

// ---------------------------------------------------------------------------
// Transfers
// ---------------------------------------------------------------------------

/// The end of a blocking transfer: engine warnings, the reply warning on
/// failure, then a callback's exception.
fn finish(
    vm: &mut Vm,
    ftp: RefMut<'_, php_ftp::Ftp>,
    success: bool,
    pending: Option<Throw>,
) -> VmResult<Value> {
    let (warns, reply) = release(ftp);
    emit(vm, warns)?;
    if !success && !reply.is_empty() {
        vm.php_warning_bytes(&reply)?;
    }
    match pending {
        Some(t) => Err(t),
        None => Ok(Value::Bool(success)),
    }
}

/// The ready connection or the "already in use" result of a blocking
/// transfer.
macro_rules! ready_or_in_use {
    ($vm:expr, $h:expr) => {
        match access($vm, &$h)? {
            Access::Ready(f) => f,
            Access::Busy => {
                $vm.php_warning(IN_USE)?;
                return Ok(Value::Bool(false));
            }
        }
    };
}

/// The ready connection, or the `Error` of the non-blocking starters.
macro_rules! ready_or_throw {
    ($vm:expr, $h:expr) => {
        match access($vm, &$h)? {
            Access::Ready(f) => f,
            Access::Busy => return Err($vm.throw("Error", START_BUSY)),
        }
    };
}

/// Resume handling of the stream-based downloads: positions `s` and
/// returns the effective offset.
fn download_seek(s: &mut PhpStream, autoseek: bool, mut pos: i64) -> i64 {
    if !autoseek && pos == AUTORESUME {
        pos = 0;
    }
    if autoseek && pos != 0 {
        if pos == AUTORESUME {
            s.seek(0, SEEK_END);
            pos = s.tell();
        } else {
            s.seek(pos, SEEK_SET);
        }
    }
    pos
}

/// Resume handling of the uploads: `FTP_AUTORESUME` asks the server for
/// the remote size; the local stream is positioned at the offset.
fn upload_seek(ftp: &mut php_ftp::Ftp, s: &mut PhpStream, remote: &[u8], mut pos: i64) -> i64 {
    if !ftp.autoseek && pos == AUTORESUME {
        pos = 0;
    }
    if ftp.autoseek && pos != 0 {
        if pos == AUTORESUME {
            pos = ftp.size(remote).max(0);
        }
        if pos != 0 {
            s.seek(pos, SEEK_SET);
        }
    }
    pos
}

/// Replaces the pending non-blocking stream, closing the previous one.
fn set_nb(h: &Handle, s: Option<PhpStream>) {
    if let Ok(mut slot) = h.nb.try_borrow_mut() {
        if let Some(old) = std::mem::replace(&mut *slot, s) {
            old.close();
        }
    }
}

fn ftp_fget(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = res::stream_arg(vm, a, 1)?;
    let remote = a.str(2);
    let h = handle(a);
    let mut ftp = ready_or_in_use!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let pos = int_or(a, 4, 0);
    let mut s = PhpStream::Shared(cell);
    let (r, pending) = streams(vm, || {
        let autoseek = ftp.autoseek;
        let pos = download_seek(&mut s, autoseek, pos);
        ftp.get(&mut s, remote.as_bytes(), ty, pos)
    });
    finish(vm, ftp, r, pending)
}

fn ftp_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let local = path_arg(vm, a, 1)?;
    let remote = path_arg(vm, a, 2)?;
    let h = handle(a);
    let mut ftp = ready_or_in_use!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let mut pos = int_or(a, 4, 0);
    if !ftp.autoseek && pos == AUTORESUME {
        pos = 0;
    }
    let (r, pending) = streams(vm, || {
        let ascii = ty == TransferType::Ascii;
        let mut s = open_for_download(&local, ascii, ftp.autoseek, &mut pos)?;
        let ok = ftp.get(&mut s, &remote, ty, pos);
        s.close();
        if !ok {
            unlink(&local);
        }
        Some(ok)
    });
    let Some(r) = r else {
        drop(ftp);
        let mut msg = b"Error opening ".to_vec();
        msg.extend_from_slice(&local);
        vm.php_warning_bytes(&msg)?;
        return match pending {
            Some(t) => Err(t),
            None => Ok(Value::Bool(false)),
        };
    };
    finish(vm, ftp, r, pending)
}

/// The end of a non-blocking start/step: engine warnings, the reply
/// warning on failure (`always_warn`: even when empty, as
/// `ftp_nb_continue`/`ftp_nb_put` do), then a callback's exception.
fn finish_nb(
    vm: &mut Vm,
    ftp: RefMut<'_, php_ftp::Ftp>,
    st: NbStatus,
    always_warn: bool,
    pending: Option<Throw>,
) -> VmResult<Value> {
    let (warns, reply) = release(ftp);
    emit(vm, warns)?;
    if st == NbStatus::Failed && (always_warn || !reply.is_empty()) {
        vm.php_warning_bytes(&reply)?;
    }
    match pending {
        Some(t) => Err(t),
        None => Ok(Value::Int(st.code())),
    }
}

fn ftp_nb_fget(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = res::stream_arg(vm, a, 1)?;
    let remote = a.str(2);
    let h = handle(a);
    let mut ftp = ready_or_throw!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let pos = int_or(a, 4, 0);
    let mut s = PhpStream::Shared(cell);
    let (st, pending) = streams(vm, || {
        let autoseek = ftp.autoseek;
        let pos = download_seek(&mut s, autoseek, pos);
        let st = ftp.nb_get(&mut s, remote.as_bytes(), ty, pos);
        set_nb(&h, (st == NbStatus::MoreData).then_some(s));
        st
    });
    finish_nb(vm, ftp, st, false, pending)
}

fn ftp_nb_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let local = a.str(1).as_bytes().to_vec();
    let remote = a.str(2);
    let h = handle(a);
    let mut ftp = ready_or_throw!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let mut pos = int_or(a, 4, 0);
    if !ftp.autoseek && pos == AUTORESUME {
        pos = 0;
    }
    let (st, pending) = streams(vm, || {
        let ascii = ty == TransferType::Ascii;
        let mut s = open_for_download(&local, ascii, ftp.autoseek, &mut pos)?;
        let st = ftp.nb_get(&mut s, remote.as_bytes(), ty, pos);
        match st {
            NbStatus::MoreData => set_nb(&h, Some(s)),
            NbStatus::Finished => {
                set_nb(&h, None);
                s.close();
            }
            NbStatus::Failed => {
                set_nb(&h, None);
                s.close();
                unlink(&local);
            }
        }
        Some(st)
    });
    let Some(st) = st else {
        drop(ftp);
        let mut msg = b"Error opening ".to_vec();
        msg.extend_from_slice(&local);
        vm.php_warning_bytes(&msg)?;
        return match pending {
            Some(t) => Err(t),
            None => Ok(Value::Bool(false)),
        };
    };
    finish_nb(vm, ftp, st, false, pending)
}

fn ftp_nb_continue(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => {
            vm.php_warning(IN_USE)?;
            return Ok(Value::Int(NbStatus::Failed.code()));
        }
    };
    if !ftp.nb_active() {
        drop(ftp);
        vm.php_warning("No non-blocking transfer to continue")?;
        return Ok(Value::Int(NbStatus::Failed.code()));
    }
    let mut stream = h.nb.try_borrow_mut().ok().and_then(|mut s| s.take());
    let (st, pending) = streams(vm, || {
        let sending = ftp.nb_sending();
        let st = match stream.as_mut() {
            Some(s) if sending => ftp.nb_continue_write(s),
            Some(s) => ftp.nb_continue_read(s),
            None if sending => ftp.nb_continue_write(&mut NullSink),
            None => ftp.nb_continue_read(&mut NullSink),
        };
        if st != NbStatus::MoreData {
            if let Some(s) = stream.take() {
                s.close();
            }
        }
        st
    });
    if let Some(s) = stream {
        set_nb(&h, Some(s));
    }
    finish_nb(vm, ftp, st, true, pending)
}

fn ftp_fput(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let remote = a.str(1);
    let cell = res::stream_arg(vm, a, 2)?;
    let h = handle(a);
    let mut ftp = ready_or_in_use!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let pos = int_or(a, 4, 0);
    let mut s = PhpStream::Shared(cell);
    let (r, pending) = streams(vm, || {
        let pos = upload_seek(&mut ftp, &mut s, remote.as_bytes(), pos);
        ftp.put(remote.as_bytes(), &mut s, ty, pos)
    });
    finish(vm, ftp, r, pending)
}

fn ftp_nb_fput(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let remote = a.str(1);
    let cell = res::stream_arg(vm, a, 2)?;
    let h = handle(a);
    let mut ftp = ready_or_throw!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let pos = int_or(a, 4, 0);
    let mut s = PhpStream::Shared(cell);
    let (st, pending) = streams(vm, || {
        let pos = upload_seek(&mut ftp, &mut s, remote.as_bytes(), pos);
        let st = ftp.nb_put(remote.as_bytes(), &mut s, ty, pos);
        set_nb(&h, (st == NbStatus::MoreData).then_some(s));
        st
    });
    finish_nb(vm, ftp, st, false, pending)
}

/// `ftp_put` / `ftp_append`.
fn put(vm: &mut Vm, a: &Args, append: bool) -> VmResult<Value> {
    let remote = path_arg(vm, a, 1)?;
    let local = path_arg(vm, a, 2)?;
    let h = handle(a);
    let mut ftp = ready_or_in_use!(vm, h);
    let ty = xtype(vm, a, 3)?;
    let pos = if append { 0 } else { int_or(a, 4, 0) };
    let mode = if ty == TransferType::Ascii {
        "rt"
    } else {
        "rb"
    };
    let (r, pending) = streams(vm, || {
        let mut s = PhpStream::Owned(Box::new(open(&local, mode)?));
        let ok = if append {
            ftp.append(&remote, &mut s, ty)
        } else {
            let pos = upload_seek(&mut ftp, &mut s, &remote, pos);
            ftp.put(&remote, &mut s, ty, pos)
        };
        s.close();
        Some(ok)
    });
    let Some(r) = r else {
        drop(ftp);
        return match pending {
            Some(t) => Err(t),
            None => Ok(Value::Bool(false)),
        };
    };
    finish(vm, ftp, r, pending)
}

fn ftp_put(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    put(vm, a, false)
}

fn ftp_append(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    put(vm, a, true)
}

fn ftp_nb_put(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let remote = path_arg(vm, a, 1)?;
    let local = path_arg(vm, a, 2)?;
    let h = handle(a);
    let mut ftp = match access(vm, &h)? {
        Access::Ready(f) => f,
        Access::Busy => {
            // PHP opens the local file before noticing the busy connection.
            let ((), pending) = streams(vm, || {
                if let Some(s) = open(&local, "rb") {
                    PhpStream::Owned(Box::new(s)).close();
                }
            });
            if let Some(t) = pending {
                return Err(t);
            }
            return Err(vm.throw("Error", START_BUSY));
        }
    };
    let ty = xtype(vm, a, 3)?;
    let pos = int_or(a, 4, 0);
    let mode = if ty == TransferType::Ascii {
        "rt"
    } else {
        "rb"
    };
    let (st, pending) = streams(vm, || {
        let mut s = PhpStream::Owned(Box::new(open(&local, mode)?));
        let pos = upload_seek(&mut ftp, &mut s, &remote, pos);
        let st = ftp.nb_put(&remote, &mut s, ty, pos);
        if st == NbStatus::MoreData {
            set_nb(&h, Some(s));
        } else {
            s.close();
        }
        Some(st)
    });
    let Some(st) = st else {
        drop(ftp);
        return match pending {
            Some(t) => Err(t),
            None => Ok(Value::Bool(false)),
        };
    };
    finish_nb(vm, ftp, st, true, pending)
}

impl Extension for Ftp {
    fn name(&self) -> &'static str {
        "ftp"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, v) in [
            ("FTP_ASCII", 1),
            ("FTP_TEXT", 1),
            ("FTP_BINARY", 2),
            ("FTP_IMAGE", 2),
            ("FTP_AUTORESUME", AUTORESUME),
            ("FTP_TIMEOUT_SEC", php_ftp::OPT_TIMEOUT_SEC),
            ("FTP_AUTOSEEK", php_ftp::OPT_AUTOSEEK),
            ("FTP_USEPASVADDRESS", php_ftp::OPT_USEPASVADDRESS),
            ("FTP_FAILED", NbStatus::Failed.code()),
            ("FTP_FINISHED", NbStatus::Finished.code()),
            ("FTP_MOREDATA", NbStatus::MoreData.code()),
        ] {
            m.constant(name, Value::Int(v));
        }

        ClassBuilder::new(CLASS)
            .flags(
                class_flags::FINAL
                    | class_flags::NOT_SERIALIZABLE
                    | class_flags::NO_CLONE
                    | class_flags::NO_DYNAMIC_PROPERTIES,
            )
            .method("__construct()", acc::PUBLIC, ctor)
            .register(m.vm());

        let sig = |s: &str| s.replace("$C", CONN_PARAM);
        for (s, f) in [
            ("ftp_connect(string $hostname, int $port = 21, int $timeout = 90): FTP\\Connection|false", ftp_connect as php_vm::ext::NativeFn),
            ("ftp_ssl_connect(string $hostname, int $port = 21, int $timeout = 90): FTP\\Connection|false", ftp_ssl_connect),
            ("ftp_login($C, string $username, string $password): bool", ftp_login),
            ("ftp_pwd($C): string|false", ftp_pwd),
            ("ftp_cdup($C): bool", ftp_cdup),
            ("ftp_chdir($C, string $directory): bool", ftp_chdir),
            ("ftp_exec($C, string $command): bool", ftp_exec),
            ("ftp_raw($C, string $command): ?array", ftp_raw),
            ("ftp_mkdir($C, string $directory): string|false", ftp_mkdir),
            ("ftp_rmdir($C, string $directory): bool", ftp_rmdir),
            ("ftp_chmod($C, int $permissions, string $filename): int|false", ftp_chmod),
            ("ftp_alloc($C, int $size, &$response = null): bool", ftp_alloc),
            ("ftp_nlist($C, string $directory): array|false", ftp_nlist),
            ("ftp_rawlist($C, string $directory, bool $recursive = false): array|false", ftp_rawlist),
            ("ftp_mlsd($C, string $directory): array|false", ftp_mlsd),
            ("ftp_systype($C): string|false", ftp_systype),
            ("ftp_fget($C, $stream, string $remote_filename, int $mode = FTP_BINARY, int $offset = 0): bool", ftp_fget),
            ("ftp_nb_fget($C, $stream, string $remote_filename, int $mode = FTP_BINARY, int $offset = 0): int", ftp_nb_fget),
            ("ftp_pasv($C, bool $enable): bool", ftp_pasv),
            ("ftp_get($C, string $local_filename, string $remote_filename, int $mode = FTP_BINARY, int $offset = 0): bool", ftp_get),
            ("ftp_nb_get($C, string $local_filename, string $remote_filename, int $mode = FTP_BINARY, int $offset = 0): int|false", ftp_nb_get),
            ("ftp_nb_continue($C): int", ftp_nb_continue),
            ("ftp_fput($C, string $remote_filename, $stream, int $mode = FTP_BINARY, int $offset = 0): bool", ftp_fput),
            ("ftp_nb_fput($C, string $remote_filename, $stream, int $mode = FTP_BINARY, int $offset = 0): int", ftp_nb_fput),
            ("ftp_put($C, string $remote_filename, string $local_filename, int $mode = FTP_BINARY, int $offset = 0): bool", ftp_put),
            ("ftp_append($C, string $remote_filename, string $local_filename, int $mode = FTP_BINARY): bool", ftp_append),
            ("ftp_nb_put($C, string $remote_filename, string $local_filename, int $mode = FTP_BINARY, int $offset = 0): int|false", ftp_nb_put),
            ("ftp_size($C, string $filename): int", ftp_size),
            ("ftp_mdtm($C, string $filename): int", ftp_mdtm),
            ("ftp_rename($C, string $from, string $to): bool", ftp_rename),
            ("ftp_delete($C, string $filename): bool", ftp_delete),
            ("ftp_site($C, string $command): bool", ftp_site),
            ("ftp_close($C): bool", ftp_close),
            ("ftp_quit($C): bool", ftp_close),
            ("ftp_set_option($C, int $option, $value): true", ftp_set_option),
            ("ftp_get_option($C, int $option): int|bool", ftp_get_option),
        ] {
            m.function(&sig(s), f);
        }
    }
}
