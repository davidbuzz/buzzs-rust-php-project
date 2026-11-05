//! Request variables: `$_GET`, `$_POST`, `$_COOKIE`, `$_FILES`,
//! `$_SERVER`, `$_ENV` and `$_REQUEST` (`php_hash_environment()`,
//! `php_apache_sapi_register_variables()`, `sapi_read_post_data()` and the
//! rfc1867 handler).

use crate::backend::{IntField, StrField, Table};
use crate::multipart::{self, RegValue, Target, UploadSettings};
use crate::request::{Body, RequestCtx, POST_BLOCK_SIZE};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_stdmisc::query::{self, InputSettings, ParseKind, RegisterOptions};
use php_vm::Vm;
use std::path::PathBuf;

/// HTTP authentication data (`SG(request_info).auth_*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Auth {
    /// `PHP_AUTH_USER`.
    pub user: Option<Vec<u8>>,
    /// `PHP_AUTH_PW`.
    pub password: Option<Vec<u8>>,
    /// `PHP_AUTH_DIGEST`.
    pub digest: Option<Vec<u8>>,
}

const STARTUP: &str = "PHP Request Startup: ";

fn ini_bytes(vm: &Vm, name: &str) -> Vec<u8> {
    vm.ini_string(name).unwrap_or_default()
}

fn input_settings(vm: &Vm) -> InputSettings {
    let sep = ini_bytes(vm, "arg_separator.input");
    InputSettings {
        arg_separator_input: if sep.is_empty() { b"&".to_vec() } else { sep },
        max_input_vars: vm.ini_long("max_input_vars"),
        max_input_nesting_level: vm.ini_long("max_input_nesting_level"),
        display_errors: crate::request::display_errors(vm),
    }
}

/// Emits a startup warning (`php_error_docref` during request startup or
/// `sapi_error`), located at `Unknown:0`.
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

/// `php_register_variable_safe()` into `track`.
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

/// `sapi_read_standard_form_data()`.
fn read_standard_form_data(vm: &mut Vm, ctx: &RequestCtx, content_length: i64) {
    let post_max_size = vm.ini_long("post_max_size");
    if post_max_size > 0 && content_length > post_max_size {
        startup_warning(
            vm,
            &format!(
                "POST Content-Length of {content_length} bytes exceeds the limit of {post_max_size} bytes"
            ),
            true,
        );
        return;
    }
    let mut body = Vec::new();
    let mut buf = vec![0u8; POST_BLOCK_SIZE];
    loop {
        let n = ctx.read_post(&mut buf);
        body.extend_from_slice(&buf[..n]);
        if post_max_size > 0 && ctx.read_post_bytes.get() > post_max_size {
            startup_warning(
                vm,
                &format!(
                    "Actual POST length does not match Content-Length, and exceeds {post_max_size} bytes"
                ),
                true,
            );
            break;
        }
        if n < POST_BLOCK_SIZE {
            break;
        }
    }
    *ctx.body.borrow_mut() = Body::Buffered(body);
}

/// The directories tried for uploaded files.
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

/// Populates every superglobal for the request.
#[allow(clippy::too_many_lines)]
pub fn populate(vm: &mut Vm, ctx: &RequestCtx, auth: &Auth) {
    let b = &*ctx.backend;
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

    // $_GET
    let mut get = ZArray::new();
    if has(b'G') {
        let q = b.str_field(StrField::Args).unwrap_or_default();
        let _ = query::treat_data(ParseKind::Get, &q, &mut get, &settings, "", None, vm);
    }

    // $_POST / $_FILES (sapi_activate + php_auto_globals_create_post)
    let mut post = ZArray::new();
    let mut files = ZArray::new();
    let method = b.str_field(StrField::Method).unwrap_or_default();
    let content_type = b.table_get(Table::HeadersIn, b"Content-Type");
    let content_length = b
        .table_get(Table::HeadersIn, b"Content-Length")
        .map_or(0, |v| crate::headers::strtol(&v));
    if vm.ini_bool("enable_post_data_reading") && method == b"POST" {
        if let Some(ct) = &content_type {
            match content_type_key(ct).as_slice() {
                b"application/x-www-form-urlencoded" => {
                    read_standard_form_data(vm, ctx, content_length);
                    if has(b'P') {
                        let data = match &*ctx.body.borrow() {
                            Body::Buffered(d) => d.clone(),
                            _ => Vec::new(),
                        };
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
                        let mut read = |buf: &mut [u8]| ctx.read_post(buf);
                        let outcome = multipart::parse(ct, content_length, &upload, &mut read);
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
                        vm.rfc1867_uploaded_files.extend(outcome.uploaded);
                    }
                }
                _ => read_standard_form_data(vm, ctx, content_length),
            }
        }
    }

    // $_COOKIE
    let mut cookie = ZArray::new();
    if has(b'C') {
        if let Some(c) = b.table_get(Table::HeadersIn, b"Cookie") {
            let _ = query::treat_data(ParseKind::Cookie, &c, &mut cookie, &settings, "", None, vm);
        }
    }

    // $_SERVER
    let mut server = ZArray::new();
    if has(b'S') {
        for (k, v) in b.table_entries(Table::SubprocessEnv) {
            register(vm, &k, Value::Str(ZStr::from(v)), &mut server, false);
        }
        let uri = b.str_field(StrField::Uri).unwrap_or_default();
        register(
            vm,
            b"PHP_SELF",
            Value::Str(ZStr::from(uri)),
            &mut server,
            false,
        );
        let t = server.make_mut();
        let s = |v: &[u8]| Value::Str(ZStr::from(v));
        if let Some(u) = &auth.user {
            t.insert("PHP_AUTH_USER", s(u));
        }
        if let Some(p) = &auth.password {
            t.insert("PHP_AUTH_PW", s(p));
        }
        if let Some(d) = &auth.digest {
            t.insert("PHP_AUTH_DIGEST", s(d));
        }
        #[allow(clippy::cast_precision_loss)]
        let now = b.int_field(IntField::RequestTime) as f64 / 1_000_000.0;
        t.insert("REQUEST_TIME_FLOAT", Value::Float(now));
        #[allow(clippy::cast_possible_truncation)]
        t.insert("REQUEST_TIME", Value::Int(now as i64));
        if vm.ini_bool("register_argc_argv") {
            let q = b.str_field(StrField::Args).unwrap_or_default();
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

    #[test]
    fn content_type_keys() {
        assert_eq!(
            content_type_key(b"Multipart/Form-Data; boundary=x"),
            b"multipart/form-data"
        );
        assert_eq!(
            content_type_key(b"application/x-www-form-urlencoded"),
            b"application/x-www-form-urlencoded"
        );
    }

    #[test]
    fn argv_from_query() {
        assert_eq!(
            build_argv(b"a+b+c"),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        assert!(build_argv(b"").is_empty());
    }

    #[test]
    fn merge_is_recursive() {
        let mut a = ZArray::new();
        {
            let t = a.make_mut();
            let mut inner = ZArray::new();
            inner.make_mut().insert("x", Value::Int(1));
            t.insert("k", Value::Array(inner));
            t.insert("s", Value::Int(1));
        }
        let mut b = ZArray::new();
        {
            let t = b.make_mut();
            let mut inner = ZArray::new();
            inner.make_mut().insert("y", Value::Int(2));
            t.insert("k", Value::Array(inner));
            t.insert("s", Value::Int(2));
        }
        let mut dest = HashTable::new();
        merge(&mut dest, &a);
        merge(&mut dest, &b);
        let Some(Value::Array(k)) = dest.get_str(b"k") else {
            panic!("k missing")
        };
        assert_eq!(k.len(), 2);
        assert!(matches!(dest.get_str(b"s"), Some(Value::Int(2))));
    }
}
