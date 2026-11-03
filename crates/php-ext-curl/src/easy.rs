//! `CurlHandle` and the easy-handle functions (interface.c).

use crate::dispatch::{verify_handlers, Dispatch};
use crate::setopt::{self, Ctx};
use crate::state::{curl_of, Curl, Method, State};
use crate::streams;
use php_curl::consts::{
    CURLINFO_APPCONNECT_TIME_T, CURLINFO_CAINFO, CURLINFO_CAPATH, CURLINFO_CERTINFO,
    CURLINFO_CONNECT_TIME, CURLINFO_CONNECT_TIME_T, CURLINFO_CONN_ID,
    CURLINFO_CONTENT_LENGTH_DOWNLOAD, CURLINFO_CONTENT_LENGTH_UPLOAD, CURLINFO_CONTENT_TYPE,
    CURLINFO_DOUBLE, CURLINFO_EFFECTIVE_METHOD, CURLINFO_EFFECTIVE_URL, CURLINFO_FILETIME,
    CURLINFO_HEADER_OUT, CURLINFO_HEADER_SIZE, CURLINFO_HTTP_CODE, CURLINFO_HTTP_VERSION,
    CURLINFO_LOCAL_IP, CURLINFO_LOCAL_PORT, CURLINFO_LONG, CURLINFO_NAMELOOKUP_TIME,
    CURLINFO_NAMELOOKUP_TIME_T, CURLINFO_OFF_T, CURLINFO_PRETRANSFER_TIME,
    CURLINFO_PRETRANSFER_TIME_T, CURLINFO_PRIMARY_IP, CURLINFO_PRIMARY_PORT, CURLINFO_PRIVATE,
    CURLINFO_PROTOCOL, CURLINFO_PROXY_SSL_VERIFYRESULT, CURLINFO_REDIRECT_COUNT,
    CURLINFO_REDIRECT_TIME, CURLINFO_REDIRECT_TIME_T, CURLINFO_REDIRECT_URL, CURLINFO_REQUEST_SIZE,
    CURLINFO_SCHEME, CURLINFO_SIZE_DOWNLOAD, CURLINFO_SIZE_UPLOAD, CURLINFO_SLIST,
    CURLINFO_SPEED_DOWNLOAD, CURLINFO_SPEED_UPLOAD, CURLINFO_SSL_VERIFYRESULT,
    CURLINFO_STARTTRANSFER_TIME, CURLINFO_STARTTRANSFER_TIME_T, CURLINFO_STRING,
    CURLINFO_TOTAL_TIME, CURLINFO_TOTAL_TIME_T, CURLINFO_TYPEMASK,
};
use php_curl::Easy;
use php_runtime::{CastTarget, Obj, Value, ZArray};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};
use std::rc::Rc;

/// `ZEND_ACC_NO_DYNAMIC_PROPERTIES` (`@strict-properties`), the value of
/// `php_vm::class::cflags::NO_DYNAMIC_PROPERTIES`.
pub(crate) const NO_DYNAMIC_PROPERTIES: u32 = 1 << 14;

/// `openssl.cafile`, else `curl.cainfo`.
pub(crate) fn cainfo(vm: &Vm) -> Option<Vec<u8>> {
    vm.ini_string("openssl.cafile")
        .filter(|v| !v.is_empty())
        .or_else(|| vm.ini_string("curl.cainfo"))
        .filter(|v| !v.is_empty())
}

/// The `CurlHandle` argument `n` (ZPP has checked the class).
pub(crate) fn handle_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<(Obj, Rc<Curl>)> {
    if let Value::Object(o) = a.at(n) {
        if let Some(c) = curl_of(&o) {
            return Ok((o, c));
        }
    }
    Err(vm.throw("Error", "Invalid cURL handle"))
}

/// Wraps native state in a new `CurlHandle` object.
pub(crate) fn wrap_curl(vm: &mut Vm, curl: Rc<Curl>) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(b"CurlHandle") else {
        return Err(vm.throw("Error", "Class \"CurlHandle\" not found"));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(&o, curl);
    Ok(o)
}

/// `_php_setup_easy_copy_handlers()` (the PHP-side part).
pub(crate) fn copy_state(src: &State) -> State {
    State {
        write: crate::state::Io {
            buf: None,
            ..src.write.clone()
        },
        write_header: crate::state::Io {
            buf: None,
            ..src.write_header.clone()
        },
        read: crate::state::Io {
            buf: None,
            ..src.read.clone()
        },
        header_out: None,
        postfields: None,
        ..src.clone()
    }
}

/// `curl_clone_obj()`: duplicates the handle; `Ok(None)` when libcurl
/// failed ("Failed to clone `CurlHandle`").
fn clone_curl(vm: &mut Vm, src: &Curl) -> VmResult<Option<Rc<Curl>>> {
    let Some(dup) = src.easy.duplicate() else {
        return Ok(None);
    };
    let (st, postfields) = {
        let s = src.st.borrow();
        (copy_state(&s), s.postfields.clone())
    };
    let c = Curl::new(dup, st);
    if let Some(pf) = postfields {
        if !crate::mime::build_from_hash(vm, "curl_copy_handle", &c, &pf)? {
            return Ok(None);
        }
    }
    Ok(Some(c))
}

fn clone_hook(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let Some(src) = curl_of(old) else {
        return Ok(());
    };
    match clone_curl(vm, &src)? {
        Some(c) => {
            object::set_internal(new, c);
            Ok(())
        }
        None => Err(vm.throw("Exception", "Failed to clone CurlHandle")),
    }
}

/// `curl_cast_object()`: `(int) $handle` is the object id.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn cast_hook(_vm: &mut Vm, o: &Obj, t: CastTarget) -> VmResult<Option<Value>> {
    Ok(match t {
        CastTarget::Int => Some(Value::Int(i64::from(o.handle()))),
        _ => None,
    })
}

/// `zend_objects_not_comparable`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn not_comparable(_vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    Ok(Some(match (a, b) {
        (Value::Object(x), Value::Object(y)) if Obj::ptr_eq(x, y) => 0,
        _ => 1,
    }))
}

fn ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let class = a
        .this
        .as_ref()
        .map(|o| object::class_of(o).name.to_string())
        .unwrap_or_default();
    let how = match class.as_str() {
        "CurlMultiHandle" => "curl_multi_init()",
        "CurlShareHandle" => "curl_share_init()",
        "CurlSharePersistentHandle" => "curl_share_init_persistent()",
        _ => "curl_init()",
    };
    Err(vm.throw(
        "Error",
        &format!("Cannot directly construct {class}, use {how} instead"),
    ))
}

/// Registers `CurlHandle`.
pub(crate) fn register_class(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new("CurlHandle")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE | NO_DYNAMIC_PROPERTIES)
        .method("__construct()", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            clone: Some(clone_hook),
            cast: Some(cast_hook),
            compare: Some(not_comparable),
            ..Default::default()
        })
        .register(m.vm());
}

/// The constructor stub shared by the other handle classes.
pub(crate) fn ctor_fn() -> php_vm::ext::NativeFn {
    ctor
}

fn curl_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ca = cainfo(vm);
    let Some(easy) = Easy::new(ca.as_deref()) else {
        vm.php_warning("Could not initialize a new cURL handle")?;
        return Ok(Value::Bool(false));
    };
    let curl = Curl::new(easy, State::fresh());
    let o = wrap_curl(vm, Rc::clone(&curl))?;
    if let Some(Value::Str(url)) = a.opt(0) {
        let cx = Ctx {
            args: a,
            is_array: false,
            fname: "curl_init".into(),
        };
        if !setopt::option_url(vm, &cx, &curl, url.as_bytes())? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Object(o))
}

fn curl_copy_handle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    if let Ok(Some(n)) = clone_curl(vm, &c) {
        Ok(Value::Object(wrap_curl(vm, n)?))
    } else {
        vm.php_warning("Cannot duplicate cURL handle")?;
        Ok(Value::Bool(false))
    }
}

fn curl_setopt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, c) = handle_arg(vm, a, 0)?;
    let opt = a.int(1);
    let value = a.at(2);
    let cx = Ctx {
        args: a,
        is_array: false,
        fname: "curl_setopt".into(),
    };
    Ok(Value::Bool(setopt::setopt(vm, &cx, &o, &c, opt, &value)?))
}

fn curl_setopt_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, c) = handle_arg(vm, a, 0)?;
    let arr = a.array(1);
    let cx = Ctx {
        args: a,
        is_array: true,
        fname: "curl_setopt_array".into(),
    };
    for (k, v) in arr.iter() {
        let php_runtime::ArrayKey::Int(opt) = k else {
            return Err(a.value_error(vm, 2, "contains an invalid cURL option"));
        };
        let v = v.deref();
        if !setopt::setopt(vm, &cx, &o, &c, *opt, &v)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

/// `_php_curl_cleanup_handle()`.
pub(crate) fn cleanup_handle(c: &Curl) {
    {
        let mut st = c.st.borrow_mut();
        st.write.buf = None;
        st.header_out = None;
    }
    c.easy.clear_error();
    c.err_no.set(0);
}

fn curl_exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, c) = handle_arg(vm, a, 0)?;
    verify_handlers(vm, &c, true)?;
    cleanup_handle(&c);
    let (rc, pending) = {
        let mut d = Dispatch::new(vm, std::slice::from_ref(&o));
        let rc = c.easy.perform(&mut d);
        (rc, d.pending.take())
    };
    c.err_no.set(rc);
    if let Some(t) = pending {
        if rc != 0 {
            c.st.borrow_mut().write.buf = None;
        }
        return Err(t);
    }
    if rc != 0 {
        c.st.borrow_mut().write.buf = None;
        return Ok(Value::Bool(false));
    }
    let (std_err, method, buf, wstream, hstream, hmethod) = {
        let st = c.st.borrow();
        (
            st.std_err.clone(),
            st.write.method,
            st.write.buf.clone(),
            st.write.stream.clone(),
            st.write_header.stream.clone(),
            st.write_header.method,
        )
    };
    if let Some(Value::Resource(r)) = &std_err {
        c.easy.flush_stderr();
        streams::flush(vm, r)?;
    }
    if method == Method::Return {
        if let Some(b) = buf {
            return Ok(Value::from(b.as_slice()));
        }
    }
    if method == Method::File {
        if let Some(Value::Resource(r)) = &wstream {
            streams::flush(vm, r)?;
        }
    }
    if hmethod == Method::File {
        if let Some(Value::Resource(r)) = &hstream {
            streams::flush(vm, r)?;
        }
    }
    Ok(if method == Method::Return {
        Value::from("")
    } else {
        Value::Bool(true)
    })
}

fn bstr(b: Option<Vec<u8>>) -> Value {
    Value::from(b.unwrap_or_default().as_slice())
}

/// `create_certinfo()`.
fn certinfo_array(vm: &mut Vm, certs: Vec<Vec<Vec<u8>>>) -> VmResult<ZArray> {
    let mut list = ZArray::new();
    for cert in certs {
        let mut h = ZArray::new();
        for line in cert {
            let head = &line[..line.len().min(63)];
            if let Some(p) = head.iter().position(|&b| b == b':') {
                let key = php_runtime::ZStr::from(&line[..p]);
                h.make_mut().insert(key, Value::from(&line[p + 1..]));
            } else {
                vm.php_warning("Could not extract hash key from certificate info")?;
            }
        }
        let _ = list.make_mut().push(Value::Array(h));
    }
    Ok(list)
}

#[allow(clippy::too_many_lines)]
fn getinfo_all(vm: &mut Vm, c: &Curl) -> VmResult<Value> {
    let e = &c.easy;
    let mut r = ZArray::new();
    macro_rules! s {
        ($k:expr, $i:expr) => {
            if let Some(v) = e.info_str($i) {
                r.make_mut().insert($k, bstr(v));
            }
        };
    }
    macro_rules! l {
        ($k:expr, $i:expr) => {
            if let Some(v) = e.info_long($i) {
                r.make_mut().insert($k, Value::Int(v));
            }
        };
    }
    macro_rules! d {
        ($k:expr, $i:expr) => {
            if let Some(v) = e.info_double($i) {
                r.make_mut().insert($k, Value::Float(v));
            }
        };
    }
    macro_rules! o {
        ($k:expr, $i:expr) => {
            if let Some(v) = e.info_off_t($i) {
                r.make_mut().insert($k, Value::Int(v));
            }
        };
    }
    s!("url", CURLINFO_EFFECTIVE_URL);
    if let Some(v) = e.info_str(CURLINFO_CONTENT_TYPE) {
        r.make_mut().insert(
            "content_type",
            v.map_or(Value::Null, |b| Value::from(b.as_slice())),
        );
    }
    l!("http_code", CURLINFO_HTTP_CODE);
    l!("header_size", CURLINFO_HEADER_SIZE);
    l!("request_size", CURLINFO_REQUEST_SIZE);
    l!("filetime", CURLINFO_FILETIME);
    l!("ssl_verify_result", CURLINFO_SSL_VERIFYRESULT);
    l!("redirect_count", CURLINFO_REDIRECT_COUNT);
    d!("total_time", CURLINFO_TOTAL_TIME);
    d!("namelookup_time", CURLINFO_NAMELOOKUP_TIME);
    d!("connect_time", CURLINFO_CONNECT_TIME);
    d!("pretransfer_time", CURLINFO_PRETRANSFER_TIME);
    d!("size_upload", CURLINFO_SIZE_UPLOAD);
    d!("size_download", CURLINFO_SIZE_DOWNLOAD);
    d!("speed_download", CURLINFO_SPEED_DOWNLOAD);
    d!("speed_upload", CURLINFO_SPEED_UPLOAD);
    d!("download_content_length", CURLINFO_CONTENT_LENGTH_DOWNLOAD);
    d!("upload_content_length", CURLINFO_CONTENT_LENGTH_UPLOAD);
    d!("starttransfer_time", CURLINFO_STARTTRANSFER_TIME);
    d!("redirect_time", CURLINFO_REDIRECT_TIME);
    s!("redirect_url", CURLINFO_REDIRECT_URL);
    s!("primary_ip", CURLINFO_PRIMARY_IP);
    if let Some(ci) = e.info_certinfo() {
        let list = certinfo_array(vm, ci)?;
        r.make_mut().insert("certinfo", Value::Array(list));
    }
    l!("primary_port", CURLINFO_PRIMARY_PORT);
    s!("local_ip", CURLINFO_LOCAL_IP);
    l!("local_port", CURLINFO_LOCAL_PORT);
    l!("http_version", CURLINFO_HTTP_VERSION);
    l!("protocol", CURLINFO_PROTOCOL);
    l!("ssl_verifyresult", CURLINFO_PROXY_SSL_VERIFYRESULT);
    s!("scheme", CURLINFO_SCHEME);
    o!("appconnect_time_us", CURLINFO_APPCONNECT_TIME_T);
    o!("connect_time_us", CURLINFO_CONNECT_TIME_T);
    o!("namelookup_time_us", CURLINFO_NAMELOOKUP_TIME_T);
    o!("pretransfer_time_us", CURLINFO_PRETRANSFER_TIME_T);
    o!("redirect_time_us", CURLINFO_REDIRECT_TIME_T);
    o!("starttransfer_time_us", CURLINFO_STARTTRANSFER_TIME_T);
    o!("total_time_us", CURLINFO_TOTAL_TIME_T);
    if let Some(h) = c.st.borrow().header_out.clone() {
        r.make_mut()
            .insert("request_header", Value::from(h.as_slice()));
    }
    s!("effective_method", CURLINFO_EFFECTIVE_METHOD);
    s!("capath", CURLINFO_CAPATH);
    s!("cainfo", CURLINFO_CAINFO);
    o!("conn_id", CURLINFO_CONN_ID);
    Ok(Value::Array(r))
}

fn curl_getinfo(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    let Some(Value::Int(opt)) = a.opt(1) else {
        return getinfo_all(vm, &c);
    };
    let e = &c.easy;
    Ok(match opt {
        CURLINFO_HEADER_OUT => {
            c.st.borrow()
                .header_out
                .as_ref()
                .map_or(Value::Bool(false), |h| Value::from(h.as_slice()))
        }
        CURLINFO_CERTINFO => match e.info_certinfo() {
            Some(ci) => Value::Array(certinfo_array(vm, ci)?),
            None => Value::Bool(false),
        },
        CURLINFO_PRIVATE => c.st.borrow().private.clone().unwrap_or(Value::Bool(false)),
        _ => match opt & CURLINFO_TYPEMASK {
            CURLINFO_STRING => match e.info_str(opt) {
                Some(Some(s)) => Value::from(s.as_slice()),
                _ => Value::Bool(false),
            },
            CURLINFO_LONG => e.info_long(opt).map_or(Value::Bool(false), Value::Int),
            CURLINFO_DOUBLE => e.info_double(opt).map_or(Value::Bool(false), Value::Float),
            CURLINFO_SLIST => match e.info_slist(opt) {
                Some(items) => {
                    let mut arr = ZArray::new();
                    for i in items {
                        let _ = arr.make_mut().push(Value::from(i.as_slice()));
                    }
                    Value::Array(arr)
                }
                None => Value::Bool(false),
            },
            CURLINFO_OFF_T => e.info_off_t(opt).map_or(Value::Bool(false), Value::Int),
            _ => Value::Bool(false),
        },
    })
}

fn curl_error(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    let no = c.err_no.get();
    if no == 0 {
        return Ok(Value::from(""));
    }
    let msg = c.easy.error_message();
    if msg.is_empty() {
        Ok(Value::from(php_curl::easy::strerror(i64::from(no))))
    } else {
        Ok(Value::from(msg.as_slice()))
    }
}

fn curl_errno(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(c.err_no.get())))
}

fn curl_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    if c.in_callback.get() {
        return Err(vm.throw(
            "Error",
            "curl_close(): Attempt to close cURL handle from a callback",
        ));
    }
    Ok(Value::Null)
}

fn curl_reset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    if c.in_callback.get() {
        return Err(vm.throw(
            "Error",
            "curl_reset(): Attempt to reset cURL handle from a callback",
        ));
    }
    let ca = cainfo(vm);
    c.easy.reset(ca.as_deref());
    let mut st = c.st.borrow_mut();
    st.write.stream = None;
    st.write.method = Method::Stdout;
    st.write_header.stream = None;
    st.write_header.method = Method::Ignore;
    st.read.stream = None;
    st.read.method = Method::Direct;
    st.std_err = None;
    st.seek = None;
    st.progress = None;
    st.xferinfo = None;
    st.fnmatch = None;
    st.debug = None;
    st.prereq = None;
    st.sshhostkey = None;
    Ok(Value::Null)
}

fn curl_strerror(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(php_curl::easy::strerror(a.int(0))))
}

fn curl_escape(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    Ok(c.easy
        .escape(a.str(1).as_bytes())
        .map_or(Value::Bool(false), |s| Value::from(s.as_slice())))
}

fn curl_unescape(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    Ok(c.easy
        .unescape(a.str(1).as_bytes())
        .map_or(Value::Bool(false), |s| Value::from(s.as_slice())))
}

fn curl_pause(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, c) = handle_arg(vm, a, 0)?;
    let (rc, pending) = {
        let mut d = Dispatch::new(vm, std::slice::from_ref(&o));
        let rc = c.easy.pause(a.int(1), &mut d);
        (rc, d.pending.take())
    };
    if let Some(t) = pending {
        return Err(t);
    }
    Ok(Value::Int(i64::from(rc)))
}

fn curl_upkeep(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    let rc = c.easy.upkeep();
    c.err_no.set(rc);
    Ok(Value::Bool(rc == 0))
}

fn curl_version(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let Some(d) = php_curl::version_info() else {
        return Ok(Value::Bool(false));
    };
    let mut r = ZArray::new();
    let b = |v: &[u8]| Value::from(v);
    r.make_mut()
        .insert("version_number", Value::Int(i64::from(d.version_num)));
    r.make_mut().insert("age", Value::Int(i64::from(d.age)));
    r.make_mut()
        .insert("features", Value::Int(i64::from(d.features)));
    let mut fl = ZArray::new();
    for (name, bit) in php_curl::version::FEATURES {
        fl.make_mut()
            .insert(*name, Value::Bool(i64::from(d.features) & bit != 0));
    }
    r.make_mut().insert("feature_list", Value::Array(fl));
    r.make_mut()
        .insert("ssl_version_number", Value::Int(d.ssl_version_num));
    r.make_mut().insert("version", b(&d.version));
    r.make_mut().insert("host", b(&d.host));
    r.make_mut().insert("ssl_version", b(&d.ssl_version));
    r.make_mut().insert("libz_version", b(&d.libz_version));
    let mut pl = ZArray::new();
    for p in &d.protocols {
        let _ = pl.make_mut().push(b(p));
    }
    r.make_mut().insert("protocols", Value::Array(pl));
    if d.age >= 1 {
        r.make_mut().insert("ares", b(&d.ares));
        r.make_mut()
            .insert("ares_num", Value::Int(i64::from(d.ares_num)));
    }
    if d.age >= 2 {
        r.make_mut().insert("libidn", b(&d.libidn));
    }
    if d.age >= 3 {
        r.make_mut()
            .insert("iconv_ver_num", Value::Int(i64::from(d.iconv_ver_num)));
        r.make_mut().insert("libssh_version", b(&d.libssh_version));
    }
    if d.age >= 4 {
        r.make_mut()
            .insert("brotli_ver_num", Value::Int(i64::from(d.brotli_ver_num)));
        r.make_mut().insert("brotli_version", b(&d.brotli_version));
    }
    Ok(Value::Array(r))
}

/// Registers the easy-handle functions.
pub(crate) fn register_functions(m: &mut ModuleBuilder<'_>) {
    m.function("curl_close(CurlHandle $handle): void", curl_close)
        .deprecated("8.5", "as it has no effect since PHP 8.0")
        .function(
            "curl_copy_handle(CurlHandle $handle): CurlHandle|false",
            curl_copy_handle,
        )
        .function("curl_errno(CurlHandle $handle): int", curl_errno)
        .function("curl_error(CurlHandle $handle): string", curl_error)
        .function(
            "curl_escape(CurlHandle $handle, string $string): string|false",
            curl_escape,
        )
        .function(
            "curl_unescape(CurlHandle $handle, string $string): string|false",
            curl_unescape,
        )
        .function("curl_exec(CurlHandle $handle): string|bool", curl_exec)
        .function(
            "curl_getinfo(CurlHandle $handle, ?int $option = null): mixed",
            curl_getinfo,
        )
        .function(
            "curl_init(?string $url = null): CurlHandle|false",
            curl_init,
        )
        .function("curl_upkeep(CurlHandle $handle): bool", curl_upkeep)
        .function(
            "curl_pause(CurlHandle $handle, int $flags): int",
            curl_pause,
        )
        .function("curl_reset(CurlHandle $handle): void", curl_reset)
        .function(
            "curl_setopt_array(CurlHandle $handle, array $options): bool",
            curl_setopt_array,
        )
        .function(
            "curl_setopt(CurlHandle $handle, int $option, mixed $value): bool",
            curl_setopt,
        )
        .function("curl_strerror(int $error_code): ?string", curl_strerror)
        .function("curl_version(): array|false", curl_version);
}
