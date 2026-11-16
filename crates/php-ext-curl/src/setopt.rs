// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `_php_curl_setopt()`: per-option coercion, validation and application.

use crate::mime;
use crate::state::{Curl, Method};
use crate::streams;
use php_curl::consts::{
    CURLE_UNKNOWN_OPTION, CURLOPT_FILE, CURLOPT_INFILE, CURLOPT_PROTOCOLS, CURLOPT_PROTOCOLS_STR,
    CURLOPT_REDIR_PROTOCOLS, CURLOPT_REDIR_PROTOCOLS_STR, CURLOPT_SSL_VERIFYHOST, CURLOPT_STDERR,
    CURLOPT_WRITEHEADER, CURLPROTO_FILE, CURL_REDIR_POST_ALL,
};
use php_curl::{option_kind, Callback, OptKind};
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, VmResult};
use php_vm::{object, Vm};
use std::rc::Rc;

/// The call context of a `curl_setopt*()` call.
pub(crate) struct Ctx<'a> {
    pub(crate) args: &'a Args,
    /// `curl_setopt_array()` (argument numbers and messages differ).
    pub(crate) is_array: bool,
    /// The running function's name (`get_active_function_name()`).
    pub(crate) fname: String,
}

/// Whether `open_basedir` is set.
pub(crate) fn open_basedir_active(vm: &Vm) -> bool {
    vm.ini_string("open_basedir").is_some_and(|v| !v.is_empty())
}

/// `php_check_open_basedir()`: `true` when `path` is NOT allowed (the
/// warning has been emitted).
pub(crate) fn basedir_denied(vm: &mut Vm, path: &[u8]) -> VmResult<bool> {
    if !open_basedir_active(vm) {
        return Ok(false);
    }
    let allowed =
        php_ext_streams::bridge::run(vm, || php_streams::paths::check_open_basedir(path))?;
    Ok(!allowed)
}

/// `php_curl_option_get_name()`.
fn option_name(opt: i64) -> String {
    php_curl::easy::option_name(opt).unwrap_or_else(|| "UNKNOWN_OPTION".to_string())
}

/// `php_curl_option_str()`.
fn option_str(vm: &mut Vm, cx: &Ctx<'_>, curl: &Curl, opt: i64, s: &[u8]) -> VmResult<bool> {
    if s.contains(&0) {
        return Err(vm.throw(
            "ValueError",
            &format!(
                "{}(): cURL option CURLOPT_{} must not contain any null bytes",
                cx.fname,
                option_name(opt)
            ),
        ));
    }
    let rc = curl.easy.set_str(opt, s);
    curl.err_no.set(rc);
    Ok(rc == 0)
}

/// `php_curl_option_url()`.
pub(crate) fn option_url(vm: &mut Vm, cx: &Ctx<'_>, curl: &Curl, url: &[u8]) -> VmResult<bool> {
    if open_basedir_active(vm) {
        curl.easy.disable_file_protocol();
    }
    option_str(vm, cx, curl, php_curl::consts::CURLOPT_URL, url)
}

/// `php_curl_set_callable_handler()`: `Ok(None)` for `null`, `Err` with
/// the `TypeError` for a non-callable.
fn callable(vm: &mut Vm, cx: &Ctx<'_>, value: &Value, cb: Callback) -> VmResult<Option<Value>> {
    if value.is_null() {
        return Ok(None);
    }
    if vm.is_callable(value) {
        return Ok(Some(value.clone()));
    }
    let why = vm.callable_error_message(value);
    let n = if cx.is_array { 2 } else { 3 };
    Err(cx.args.type_error(
        vm,
        n,
        &format!(
            "must be a valid callback for option {}, {why}",
            cb.option_name()
        ),
    ))
}

/// The callback slot of the non-I/O callbacks.
fn slot(st: &mut crate::state::State, cb: Callback) -> &mut Option<Value> {
    match cb {
        Callback::Seek => &mut st.seek,
        Callback::Progress => &mut st.progress,
        Callback::XferInfo => &mut st.xferinfo,
        Callback::Fnmatch => &mut st.fnmatch,
        Callback::Debug => &mut st.debug,
        Callback::Prereq => &mut st.prereq,
        Callback::SshHostKey => &mut st.sshhostkey,
        Callback::Write => &mut st.write.fcc,
        Callback::Header => &mut st.write_header.fcc,
        Callback::Read => &mut st.read.fcc,
    }
}

fn set_callable(
    vm: &mut Vm,
    cx: &Ctx<'_>,
    curl: &Curl,
    value: &Value,
    cb: Callback,
) -> VmResult<bool> {
    // The old callback is released first, even when the new one is invalid.
    *slot(&mut curl.st.borrow_mut(), cb) = None;
    let r = callable(vm, cx, value, cb);
    match cb {
        Callback::Write | Callback::Header | Callback::Read => {
            let default = match cb {
                Callback::Write => Method::Stdout,
                Callback::Header => Method::Ignore,
                _ => Method::Direct,
            };
            let f = r.inspect_err(|_| {
                let mut st = curl.st.borrow_mut();
                io_of(&mut st, cb).method = default;
            })?;
            let mut st = curl.st.borrow_mut();
            let io = io_of(&mut st, cb);
            match f {
                None => {
                    io.method = default;
                    return Ok(true);
                }
                Some(f) => {
                    io.fcc = Some(f);
                    io.method = Method::User;
                }
            }
        }
        _ => {
            let f = r?;
            *slot(&mut curl.st.borrow_mut(), cb) = f;
            curl.easy.set_callback(cb, true);
        }
    }
    curl.err_no.set(0);
    Ok(true)
}

fn io_of(st: &mut crate::state::State, cb: Callback) -> &mut crate::state::Io {
    match cb {
        Callback::Write => &mut st.write,
        Callback::Header => &mut st.write_header,
        _ => &mut st.read,
    }
}

/// `CURLOPT_FILE`, `INFILE`, `STDERR`, `WRITEHEADER`.
fn set_file(vm: &mut Vm, cx: &Ctx<'_>, curl: &Curl, opt: i64, value: &Value) -> VmResult<bool> {
    let mut info = None;
    if !value.is_null() {
        let Value::Resource(r) = value else {
            return Err(vm.throw(
                "TypeError",
                &format!(
                    "{}(): supplied argument is not a valid File-Handle resource",
                    cx.fname
                ),
            ));
        };
        if !streams::is_open(r) {
            return Err(vm.throw(
                "TypeError",
                &format!(
                    "{}(): supplied resource is not a valid File-Handle resource",
                    cx.fname
                ),
            ));
        }
        match streams::cast_stdio(vm, r)? {
            Some(i) => info = Some(i),
            None => return Ok(false),
        }
    }
    let writable = |m: &str| {
        let b = m.as_bytes();
        b.first() != Some(&b'r') || b.get(1) == Some(&b'+')
    };
    let not_writable = |vm: &mut Vm, name: &str| {
        vm.throw(
            "ValueError",
            &format!(
                "{}(): The file handle provided for {name} must be writable",
                cx.fname
            ),
        )
    };
    match opt {
        CURLOPT_FILE | CURLOPT_WRITEHEADER => {
            let header = opt == CURLOPT_WRITEHEADER;
            match &info {
                None => {
                    let mut st = curl.st.borrow_mut();
                    let io = if header {
                        &mut st.write_header
                    } else {
                        &mut st.write
                    };
                    io.stream = None;
                    io.method = if header {
                        Method::Ignore
                    } else {
                        Method::Stdout
                    };
                }
                Some(i) if writable(&i.mode) => {
                    let mut st = curl.st.borrow_mut();
                    let io = if header {
                        &mut st.write_header
                    } else {
                        &mut st.write
                    };
                    io.stream = Some(value.clone());
                    io.method = Method::File;
                }
                Some(_) => {
                    let name = if header {
                        "CURLOPT_WRITEHEADER"
                    } else {
                        "CURLOPT_FILE"
                    };
                    return Err(not_writable(vm, name));
                }
            }
        }
        CURLOPT_INFILE => {
            curl.st.borrow_mut().read.stream = info.as_ref().map(|_| value.clone());
        }
        CURLOPT_STDERR => {
            match &info {
                None => curl.st.borrow_mut().std_err = None,
                Some(i) if writable(&i.mode) => curl.st.borrow_mut().std_err = Some(value.clone()),
                Some(_) => return Err(not_writable(vm, "CURLOPT_STDERR")),
            }
            let rc = curl.easy.set_stderr(info.and_then(|i| i.fd));
            curl.err_no.set(rc);
            return Ok(rc == 0);
        }
        _ => {}
    }
    curl.err_no.set(0);
    Ok(true)
}

/// The string items of a `curl_slist` option.
fn set_slist(vm: &mut Vm, cx: &Ctx<'_>, curl: &Curl, opt: i64, value: &Value) -> VmResult<bool> {
    let Value::Array(arr) = value else {
        return Err(vm.throw(
            "TypeError",
            &format!(
                "{}(): The CURLOPT_{} option must have an array value",
                cx.fname,
                option_name(opt)
            ),
        ));
    };
    let mut items = Vec::with_capacity(arr.len());
    for v in arr.values() {
        let s = vm.value_to_string(&v.deref())?;
        if s.as_bytes().contains(&0) {
            return Err(vm.throw(
                "ValueError",
                &format!(
                    "{}(): cURL option CURLOPT_{} must not contain any null bytes",
                    cx.fname,
                    option_name(opt)
                ),
            ));
        }
        items.push(s.as_bytes().to_vec());
    }
    let Some(rc) = curl.easy.set_slist(opt, &items) else {
        vm.php_warning("Could not build curl_slist")?;
        return Ok(false);
    };
    curl.err_no.set(rc);
    Ok(rc == 0)
}

/// Saves `rc` as the handle's error and converts it to the result.
fn save(curl: &Curl, rc: i32) -> bool {
    curl.err_no.set(rc);
    rc == 0
}

/// `_php_curl_setopt()`.
#[allow(clippy::too_many_lines)]
pub(crate) fn setopt(
    vm: &mut Vm,
    cx: &Ctx<'_>,
    obj: &Obj,
    curl: &Rc<Curl>,
    opt: i64,
    value: &Value,
) -> VmResult<bool> {
    let _ = obj;
    let Some(kind) = option_kind(opt) else {
        let msg = if cx.is_array {
            "must contain only valid cURL options"
        } else {
            "is not a valid cURL option"
        };
        curl.err_no
            .set(i32::try_from(CURLE_UNKNOWN_OPTION).unwrap_or(48));
        return Err(cx.args.value_error(vm, 2, msg));
    };
    match kind {
        OptKind::Callable(cb) => set_callable(vm, cx, curl, value, cb),
        OptKind::SslVerifyHost | OptKind::Long => {
            let l = vm.value_to_int(value)?;
            if kind == OptKind::SslVerifyHost && l == 1 {
                debug_assert_eq!(opt, CURLOPT_SSL_VERIFYHOST);
                vm.php_notice("CURLOPT_SSL_VERIFYHOST no longer accepts the value 1, value 2 will be used instead")?;
                return Ok(save(curl, curl.easy.set_long(opt, 2)));
            }
            if (opt == CURLOPT_PROTOCOLS || opt == CURLOPT_REDIR_PROTOCOLS)
                && open_basedir_active(vm)
                && (l & CURLPROTO_FILE) != 0
            {
                vm.php_warning("CURLPROTO_FILE cannot be activated when an open_basedir is set")?;
                return Ok(false);
            }
            Ok(save(curl, curl.easy.set_long(opt, l)))
        }
        OptKind::SafeUpload => {
            if !vm.value_to_bool(value)? {
                return Err(vm.throw(
                    "ValueError",
                    &format!(
                        "{}(): Disabling safe uploads is no longer supported",
                        cx.fname
                    ),
                ));
            }
            Ok(save(curl, 0))
        }
        OptKind::Str => {
            let s = vm.value_to_string(value)?;
            let b = s.as_bytes();
            if (opt == CURLOPT_PROTOCOLS_STR || opt == CURLOPT_REDIR_PROTOCOLS_STR)
                && open_basedir_active(vm)
            {
                let lc = b.to_ascii_lowercase();
                let has = |n: &[u8]| lc.windows(n.len()).any(|w| w == n);
                if has(b"file") || has(b"all") {
                    vm.php_warning(
                        "The FILE protocol cannot be activated when an open_basedir is set",
                    )?;
                    return Ok(false);
                }
            }
            option_str(vm, cx, curl, opt, b)
        }
        OptKind::NullableStr => {
            if value.is_null() {
                Ok(save(curl, curl.easy.set_null(opt)))
            } else {
                let s = vm.value_to_string(value)?;
                option_str(vm, cx, curl, opt, s.as_bytes())
            }
        }
        OptKind::Private => {
            curl.st.borrow_mut().private = Some(value.clone());
            Ok(true)
        }
        OptKind::Url => {
            let s = vm.value_to_string(value)?;
            option_url(vm, cx, curl, s.as_bytes())
        }
        OptKind::File => set_file(vm, cx, curl, opt, value),
        OptKind::Slist => set_slist(vm, cx, curl, opt, value),
        OptKind::Ignored => Ok(save(curl, 0)),
        OptKind::PostFields => {
            if let Value::Array(a) = value {
                if a.is_empty() {
                    Ok(save(curl, curl.easy.set_empty_postfields()))
                } else {
                    mime::build_from_hash(vm, cx.fname.as_str(), curl, value)
                }
            } else {
                let s = vm.value_to_string(value)?;
                Ok(save(curl, curl.easy.set_postfields(s.as_bytes())))
            }
        }
        OptKind::ReturnTransfer => {
            let on = vm.value_to_bool(value)?;
            curl.st.borrow_mut().write.method = if on { Method::Return } else { Method::Stdout };
            Ok(save(curl, 0))
        }
        OptKind::OffT => {
            let l = vm.value_to_int(value)?;
            Ok(save(curl, curl.easy.set_off_t(opt, l)))
        }
        OptKind::PostRedir => {
            let l = vm.value_to_int(value)?;
            Ok(save(curl, curl.easy.set_long(opt, l & CURL_REDIR_POST_ALL)))
        }
        OptKind::FileStr => {
            let s = vm.value_to_string(value)?;
            if !s.is_empty() && basedir_denied(vm, s.as_bytes())? {
                return Ok(false);
            }
            option_str(vm, cx, curl, opt, s.as_bytes())
        }
        OptKind::HeaderOut => {
            if curl.st.borrow().debug.is_some() {
                return Err(vm.throw(
                    "ValueError",
                    "CURLINFO_HEADER_OUT option must not be set when the CURLOPT_DEBUGFUNCTION option is set",
                ));
            }
            let on = vm.value_to_bool(value)?;
            curl.easy.set_header_out(on);
            Ok(save(curl, 0))
        }
        OptKind::Share => {
            if let Value::Object(o) = value {
                let cls = object::class_of(o);
                let lc = cls.name.as_bytes().to_ascii_lowercase();
                if lc == b"curlsharehandle" || lc == b"curlsharepersistenthandle" {
                    if let Some(sh) = crate::state::share_of(o) {
                        curl.easy.set_share(&sh.share);
                        curl.st.borrow_mut().share = Some(value.clone());
                    }
                }
            }
            Ok(save(curl, 0))
        }
        OptKind::Blob => {
            let s = vm.value_to_string(value)?;
            Ok(save(curl, curl.easy.set_blob(opt, s.as_bytes())))
        }
    }
}
