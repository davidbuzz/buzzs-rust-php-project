//! Request variables: `$_SERVER` (`sapi_cli_server_register_variables` +
//! `php_register_server_variables`), `$_GET`, `$_POST`, `$_COOKIE`,
//! `$_FILES`, `$_ENV` and `$_REQUEST` (`php_hash_environment()`,
//! `sapi_read_post_data()` and the rfc1867 handler).

use crate::multipart::{self, RegValue, Target, UploadSettings};
use crate::request::Request;
use crate::sapi::{Body, RequestCtx};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_stdmisc::query::{self, InputSettings, ParseKind, RegisterOptions};
use php_vm::ext::Rfc1867Event;
use php_vm::Vm;
use std::path::PathBuf;

/// How a `$_SERVER` entry is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    /// `php_register_known_variable`: stored under the exact name.
    Known,
    /// `php_register_variable_safe`: the name is normalised (`.`/` ` → `_`,
    /// `[` starts an array).
    Filtered,
}

/// What the server knows about itself, for `$_SERVER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFacts<'a> {
    /// `DOCUMENT_ROOT`.
    pub document_root: &'a [u8],
    /// `SERVER_NAME` (the host given to `-S`).
    pub host: &'a [u8],
    /// `SERVER_PORT`.
    pub port: u16,
    /// The router script, if any.
    pub router: Option<&'a [u8]>,
    /// `ip:port` of the client.
    pub client_addr: &'a str,
}

/// The `sapi_cli_server_register_variables()` entries, in order.
pub fn server_entries(req: &Request, facts: &ServerFacts<'_>) -> Vec<(Vec<u8>, Vec<u8>, VarKind)> {
    use VarKind::{Filtered, Known};
    let mut out: Vec<(Vec<u8>, Vec<u8>, VarKind)> = Vec::new();
    let mut known = |k: &str, v: &[u8]| out.push((k.as_bytes().to_vec(), v.to_vec(), Known));
    known("DOCUMENT_ROOT", facts.document_root);
    let addr = facts.client_addr;
    if let Some(colon) = addr.rfind(':') {
        let mut host = &addr[..colon];
        host = host.strip_prefix('[').unwrap_or(host);
        host = host.strip_suffix(']').unwrap_or(host);
        let port: String = addr[colon + 1..].chars().take(7).collect();
        known("REMOTE_ADDR", host.as_bytes());
        known("REMOTE_PORT", port.as_bytes());
    } else {
        known("REMOTE_ADDR", addr.as_bytes());
    }
    known(
        "SERVER_SOFTWARE",
        format!("PHP/{} (Development Server)", php_vm::PHP_VERSION).as_bytes(),
    );
    known(
        "SERVER_PROTOCOL",
        format!(
            "HTTP/{}.{}",
            req.protocol_version / 100,
            req.protocol_version % 100
        )
        .as_bytes(),
    );
    known("SERVER_NAME", facts.host);
    known("SERVER_PORT", facts.port.to_string().as_bytes());
    known("REQUEST_URI", req.uri());
    known("REQUEST_METHOD", req.method.as_str().as_bytes());
    let vpath = req.vpath.clone().unwrap_or_default();
    known("SCRIPT_NAME", &vpath);
    if let Some(pt) = &req.path_translated {
        known("SCRIPT_FILENAME", c_str(pt));
    } else if let Some(r) = facts.router {
        known("SCRIPT_FILENAME", r);
    }
    if let Some(pi) = &req.path_info {
        known("PATH_INFO", pi);
    }
    match &req.path_info {
        Some(pi) if !pi.is_empty() => {
            let mut s = vpath.clone();
            s.extend_from_slice(pi);
            known("PHP_SELF", &s);
        }
        _ => known("PHP_SELF", &vpath),
    }
    if let Some(q) = &req.query_string {
        out.push((b"QUERY_STRING".to_vec(), c_str(q).to_vec(), Filtered));
    }
    for (k, v) in &req.headers {
        let key: Vec<u8> = k
            .iter()
            .map(|&c| {
                if c == b'-' {
                    b'_'
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let mut real = b"HTTP_".to_vec();
        real.extend_from_slice(&key);
        if key == b"CONTENT_TYPE" || key == b"CONTENT_LENGTH" {
            out.push((key, c_str(v).to_vec(), Filtered));
        }
        out.push((real, c_str(v).to_vec(), Filtered));
    }
    out
}

/// The part of `s` before the first NUL (values are C strings).
fn c_str(s: &[u8]) -> &[u8] {
    s.split(|&c| c == 0).next().unwrap_or_default()
}

const STARTUP: &str = "PHP Request Startup: ";

fn ini_bytes(vm: &Vm, name: &str) -> Vec<u8> {
    vm.ini_string(name).unwrap_or_default()
}

/// `PG(display_errors)` as a boolean.
pub fn display_errors(vm: &Vm) -> bool {
    let v = ini_bytes(vm, "display_errors").to_ascii_lowercase();
    match v.as_slice() {
        b"on" | b"yes" | b"true" | b"stdout" | b"stderr" => true,
        _ => crate::headers::strtol(&v) != 0,
    }
}

fn input_settings(vm: &Vm) -> InputSettings {
    let sep = ini_bytes(vm, "arg_separator.input");
    InputSettings {
        arg_separator_input: if sep.is_empty() { b"&".to_vec() } else { sep },
        max_input_vars: vm.ini_long("max_input_vars"),
        max_input_nesting_level: vm.ini_long("max_input_nesting_level"),
        display_errors: display_errors(vm),
    }
}

fn startup_warning(vm: &mut Vm, message: &str, docref: bool) {
    let msg = if docref {
        format!("{STARTUP}{message}")
    } else {
        message.to_string()
    };
    let _ = vm.raise(2, &msg);
}

fn set_global(vm: &mut Vm, name: &str, arr: ZArray) {
    let mut g = vm.globals.borrow_mut();
    let i = g.find_or_add(name.as_bytes()) as usize;
    g.slots[i] = Some(Value::Array(arr));
}

fn register(vm: &mut Vm, name: &[u8], value: Value, track: &mut ZArray, cookie: bool) {
    let settings = input_settings(vm);
    let opts = RegisterOptions {
        max_nesting_level: settings.max_input_nesting_level,
        display_errors: settings.display_errors,
        cookie,
        warning_prefix: STARTUP,
    };
    let _ = query::register_variable_ex(name, value, track, &opts, vm);
}

/// The lower-cased content type without parameters, as SAPI.c looks it up.
pub fn content_type_key(ct: &[u8]) -> Vec<u8> {
    let end = ct
        .iter()
        .position(|&c| matches!(c, b';' | b',' | b' '))
        .unwrap_or(ct.len());
    ct[..end].to_ascii_lowercase()
}

fn tmp_dirs(vm: &Vm) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut dirs = Vec::new();
    for name in ["upload_tmp_dir", "sys_temp_dir"] {
        let v = ini_bytes(vm, name);
        if !v.is_empty() {
            dirs.push(PathBuf::from(std::ffi::OsStr::from_bytes(&v)));
        }
    }
    if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
        dirs.push(PathBuf::from(t));
    }
    dirs.push(PathBuf::from("/tmp"));
    dirs
}

/// `php_build_argv()` for a web request: the query string split on `+`.
pub fn build_argv(query: &[u8]) -> Vec<Vec<u8>> {
    if query.is_empty() {
        return Vec::new();
    }
    query.split(|&c| c == b'+').map(<[u8]>::to_vec).collect()
}

/// `php_autoglobal_merge()`.
fn merge(dest: &mut HashTable, src: &ZArray) {
    for (k, v) in src.iter() {
        let v = v.deref();
        if let (Value::Array(sa), Some(Value::Array(da))) = (&v, dest.get_mut(k).map(|d| &mut *d)) {
            let mut da2 = da.clone();
            merge(da2.make_mut(), sa);
            *da = da2;
            continue;
        }
        dest.insert(k.clone(), v);
    }
}

/// `(user, password, digest)` of HTTP authentication.
pub type AuthData = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

/// `php_handle_auth_data()`.
pub fn auth_data(auth: Option<&[u8]>) -> AuthData {
    let Some(auth) = auth.filter(|a| !a.is_empty()) else {
        return (None, None, None);
    };
    if auth.len() >= 6 && auth[..6].eq_ignore_ascii_case(b"Basic ") {
        let decoded = base64_decode(&auth[6..]);
        let decoded = decoded.split(|&c| c == 0).next().unwrap_or_default();
        if let Some(colon) = decoded.iter().position(|&c| c == b':') {
            return (
                Some(decoded[..colon].to_vec()),
                Some(decoded[colon + 1..].to_vec()),
                None,
            );
        }
    }
    if auth.len() >= 7 && auth[..7].eq_ignore_ascii_case(b"Digest ") {
        return (None, None, Some(auth[7..].to_vec()));
    }
    (None, None, None)
}

/// Non-strict `php_base64_decode()`.
fn base64_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
        }
    }
    out
}

/// `sapi_read_standard_form_data()`: enforces `post_max_size`; returns
/// whether the body was accepted.
fn read_standard_form_data(vm: &mut Vm, ctx: &RequestCtx) -> bool {
    let content_length =
        i64::try_from(ctx.request.content.as_ref().map_or(0, Vec::len)).unwrap_or(i64::MAX);
    let post_max_size = vm.ini_long("post_max_size");
    if post_max_size > 0 && content_length > post_max_size {
        startup_warning(
            vm,
            &format!(
                "POST Content-Length of {content_length} bytes exceeds the limit of {post_max_size} bytes"
            ),
            true,
        );
        return false;
    }
    // The body stays in the request (no copy): `php://input` and `$_POST`
    // read it from there.
    ctx.post_read_offset
        .set(ctx.request.content.as_ref().map_or(0, Vec::len));
    true
}

/// `$_GET` (`treat_data(PARSE_GET)`), empty unless `enabled`.
fn parse_get(vm: &mut Vm, req: &Request, settings: &InputSettings, enabled: bool) -> ZArray {
    let mut get = ZArray::new();
    if enabled {
        if let Some(q) = &req.query_string {
            let _ = query::treat_data(ParseKind::Get, c_str(q), &mut get, settings, "", None, vm);
        }
    }
    get
}

/// `$_COOKIE` (`treat_data(PARSE_COOKIE)`), empty unless `enabled`.
fn parse_cookie(vm: &mut Vm, req: &Request, settings: &InputSettings, enabled: bool) -> ZArray {
    let mut cookie = ZArray::new();
    if enabled {
        if let Some(c) = req.header(b"cookie") {
            let c = c_str(c).to_vec();
            let _ = query::treat_data(ParseKind::Cookie, &c, &mut cookie, settings, "", None, vm);
        }
    }
    cookie
}

/// Populates every superglobal for the request.
#[allow(clippy::too_many_lines)]
pub fn populate(vm: &mut Vm, ctx: &RequestCtx, facts: &ServerFacts<'_>, auth: &AuthData) {
    let req = &ctx.request;
    let order = {
        let o = ini_bytes(vm, "variables_order");
        if o.is_empty() {
            b"EGPCS".to_vec()
        } else {
            o
        }
    };
    let has = |c: u8| order.iter().any(|x| x.eq_ignore_ascii_case(&c));
    let settings = input_settings(vm);

    // $_POST / $_FILES are read at SAPI activation (before $_GET).
    let mut post = ZArray::new();
    let mut files = ZArray::new();
    // `$_GET`/`$_COOKIE` parsed early for the upload observer.
    let mut early: Option<(ZArray, ZArray)> = None;
    let content_type = req.header(b"content-type").map(<[u8]>::to_vec);
    if vm.ini_bool("enable_post_data_reading") && req.method == crate::parser::Method::Post {
        if let Some(ct) = &content_type {
            match content_type_key(ct).as_slice() {
                b"application/x-www-form-urlencoded" => {
                    if read_standard_form_data(vm, ctx) && has(b'P') {
                        // `php_stream_copy_to_mem()` of the request body:
                        // the copy counts towards memory_limit, as in PHP.
                        let data = req.content.clone().unwrap_or_default();
                        let _ = query::treat_data(
                            ParseKind::Post,
                            &data,
                            &mut post,
                            &settings,
                            "",
                            None,
                            vm,
                        );
                    }
                }
                b"multipart/form-data" => {
                    if has(b'P') {
                        let upload = UploadSettings {
                            max_file_uploads: vm.ini_long("max_file_uploads"),
                            max_multipart_body_parts: vm
                                .ini_string("max_multipart_body_parts")
                                .map_or(-1, |v| crate::headers::strtol(&v)),
                            post_max_size: vm.ini_long("post_max_size"),
                            max_input_vars: vm.ini_long("max_input_vars"),
                            upload_max_filesize: vm.ini_long("upload_max_filesize"),
                            file_uploads: vm.ini_bool("file_uploads"),
                            tmp_dirs: tmp_dirs(vm),
                        };
                        let content_length =
                            i64::try_from(req.content.as_ref().map_or(0, Vec::len))
                                .unwrap_or(i64::MAX);
                        let callback = vm.rfc1867_callback;
                        if callback.is_some() {
                            // php_session_rfc1867_early_find_sid(): the
                            // observer looks the session ID up in
                            // $_COOKIE / $_GET while the body is parsed.
                            let get = parse_get(vm, req, &settings, has(b'G'));
                            let cookie = parse_cookie(vm, req, &settings, has(b'C'));
                            set_global(vm, "_GET", get.clone());
                            set_global(vm, "_COOKIE", cookie.clone());
                            early = Some((get, cookie));
                        }
                        let mut read = |buf: &mut [u8]| ctx.read_post(buf);
                        let outcome = match callback {
                            Some(cb) => multipart::parse_with_events(
                                ct,
                                content_length,
                                &upload,
                                &mut read,
                                &mut |event: Rfc1867Event<'_>, bytes| cb(vm, event, bytes),
                            ),
                            None => multipart::parse(ct, content_length, &upload, &mut read),
                        };
                        *ctx.body.borrow_mut() = Body::Consumed;
                        for w in &outcome.warnings {
                            startup_warning(vm, &w.message, w.docref);
                        }
                        for r in outcome.registrations {
                            let v = match r.value {
                                RegValue::Str(s) => Value::Str(ZStr::from(s)),
                                RegValue::Long(l) => Value::Int(l),
                            };
                            let track = match r.target {
                                Target::Post => &mut post,
                                Target::Files => &mut files,
                            };
                            register(vm, &r.name, v, track, false);
                        }
                        ctx.uploaded.borrow_mut().extend(outcome.uploaded);
                    }
                }
                _ => {
                    read_standard_form_data(vm, ctx);
                }
            }
        }
    }

    // $_GET, $_COOKIE
    #[allow(clippy::single_match_else)] // the None arm is a multi-line computation
    let (get, cookie) = match early {
        Some(parsed) => parsed,
        None => {
            let get = parse_get(vm, req, &settings, has(b'G'));
            (get, parse_cookie(vm, req, &settings, has(b'C')))
        }
    };

    // $_SERVER
    let mut server = ZArray::new();
    if has(b'S') {
        for (k, v, kind) in server_entries(req, facts) {
            let v = Value::Str(ZStr::from(v));
            match kind {
                VarKind::Known => {
                    server.make_mut().insert(ArrayKey::from_bytes(&k), v);
                }
                VarKind::Filtered => register(vm, &k, v, &mut server, false),
            }
        }
        let t = server.make_mut();
        let s = |v: &[u8]| Value::Str(ZStr::from(v));
        if let Some(u) = &auth.0 {
            t.insert("PHP_AUTH_USER", s(u));
        }
        if let Some(p) = &auth.1 {
            t.insert("PHP_AUTH_PW", s(p));
        }
        if let Some(d) = &auth.2 {
            t.insert("PHP_AUTH_DIGEST", s(d));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        t.insert("REQUEST_TIME_FLOAT", Value::Float(now));
        #[allow(clippy::cast_possible_truncation)]
        t.insert("REQUEST_TIME", Value::Int(now as i64));
        if vm.ini_bool("register_argc_argv") {
            let _ = vm.php_deprecated(
                "Deriving $_SERVER['argv'] from the query string is deprecated. Configure register_argc_argv=0 to turn this message off",
            );
            let q = req.query_string.clone().unwrap_or_default();
            let args = build_argv(&q);
            let count = i64::try_from(args.len()).unwrap_or(0);
            let t = server.make_mut();
            t.insert(
                "argv",
                Value::Array(ZArray::from_values(
                    args.into_iter().map(|a| Value::Str(ZStr::from(a))),
                )),
            );
            t.insert("argc", Value::Int(count));
        }
    }

    // $_ENV
    let mut env = ZArray::new();
    if has(b'E') {
        use std::os::unix::ffi::OsStrExt;
        let t = env.make_mut();
        for (k, v) in std::env::vars_os() {
            t.insert(
                ArrayKey::from_bytes(k.as_bytes()),
                Value::Str(ZStr::from(v.as_bytes())),
            );
        }
    }

    // $_REQUEST
    let mut request = ZArray::new();
    {
        let ro = ini_bytes(vm, "request_order");
        let ro = if ro.is_empty() { order.clone() } else { ro };
        let t = request.make_mut();
        for c in ro {
            match c.to_ascii_lowercase() {
                b'g' => merge(t, &get),
                b'p' => merge(t, &post),
                b'c' => merge(t, &cookie),
                _ => {}
            }
        }
    }

    set_global(vm, "_GET", get);
    set_global(vm, "_POST", post);
    set_global(vm, "_COOKIE", cookie);
    set_global(vm, "_FILES", files);
    set_global(vm, "_SERVER", server);
    set_global(vm, "_ENV", env);
    set_global(vm, "_REQUEST", request);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Method;

    fn sample() -> Request {
        Request {
            method: Method::Get,
            protocol_version: 101,
            request_uri: Some(b"/s.php/x?a=1".to_vec()),
            vpath: Some(b"/s.php".to_vec()),
            path_translated: Some(b"/root/s.php".to_vec()),
            path_info: Some(b"/x".to_vec()),
            query_string: Some(b"a=1".to_vec()),
            headers: vec![
                (b"host".to_vec(), b"localhost".to_vec()),
                (b"content-type".to_vec(), b"text/plain".to_vec()),
                (b"x-my-header".to_vec(), b"v".to_vec()),
            ],
            ..Request::default()
        }
    }

    #[test]
    fn server_vars() {
        let facts = ServerFacts {
            document_root: b"/root",
            host: b"localhost",
            port: 8080,
            router: None,
            client_addr: "127.0.0.1:5555",
        };
        let e = server_entries(&sample(), &facts);
        let names: Vec<String> = e
            .iter()
            .map(|(k, _, _)| String::from_utf8_lossy(k).into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "DOCUMENT_ROOT",
                "REMOTE_ADDR",
                "REMOTE_PORT",
                "SERVER_SOFTWARE",
                "SERVER_PROTOCOL",
                "SERVER_NAME",
                "SERVER_PORT",
                "REQUEST_URI",
                "REQUEST_METHOD",
                "SCRIPT_NAME",
                "SCRIPT_FILENAME",
                "PATH_INFO",
                "PHP_SELF",
                "QUERY_STRING",
                "HTTP_HOST",
                "CONTENT_TYPE",
                "HTTP_CONTENT_TYPE",
                "HTTP_X_MY_HEADER"
            ]
        );
        let get = |n: &str| {
            e.iter()
                .find(|(k, _, _)| k == n.as_bytes())
                .map(|(_, v, _)| String::from_utf8_lossy(v).into_owned())
        };
        assert_eq!(get("REMOTE_ADDR").as_deref(), Some("127.0.0.1"));
        assert_eq!(get("REMOTE_PORT").as_deref(), Some("5555"));
        assert_eq!(get("PHP_SELF").as_deref(), Some("/s.php/x"));
        assert_eq!(get("SERVER_PROTOCOL").as_deref(), Some("HTTP/1.1"));
        assert_eq!(get("SERVER_PORT").as_deref(), Some("8080"));
    }

    #[test]
    fn ipv6_client_and_router_filename() {
        let mut r = sample();
        r.path_translated = None;
        r.path_info = None;
        let facts = ServerFacts {
            document_root: b"/root",
            host: b"::1",
            port: 1,
            router: Some(b"/root/router.php"),
            client_addr: "[::1]:4242",
        };
        let e = server_entries(&r, &facts);
        let get = |n: &str| {
            e.iter()
                .find(|(k, _, _)| k == n.as_bytes())
                .map(|(_, v, _)| String::from_utf8_lossy(v).into_owned())
        };
        assert_eq!(get("REMOTE_ADDR").as_deref(), Some("::1"));
        assert_eq!(get("SCRIPT_FILENAME").as_deref(), Some("/root/router.php"));
        assert_eq!(get("PATH_INFO"), None);
        assert_eq!(get("PHP_SELF").as_deref(), Some("/s.php"));
    }

    #[test]
    fn auth() {
        assert_eq!(
            auth_data(Some(b"Basic dXNlcjpwYXNz")),
            (Some(b"user".to_vec()), Some(b"pass".to_vec()), None)
        );
        assert_eq!(
            auth_data(Some(b"Digest x")),
            (None, None, Some(b"x".to_vec()))
        );
        assert_eq!(build_argv(b"a+b"), vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(
            content_type_key(b"Multipart/Form-Data; b=1"),
            b"multipart/form-data"
        );
    }
}
