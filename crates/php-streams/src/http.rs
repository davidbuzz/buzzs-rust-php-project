// Derived from php-src ext/standard/http_fopen_wrapper.c. Similarity metric to original: 16%
//   Copyright © The PHP Group and Contributors.
//   Authors: Rasmus Lerdorf <rasmus@php.net>
//            Jim Winstead <jimw@php.net>
//            Hartmut Holzgraefe <hholzgra@php.net>
//            Wez Furlong <wez@thebrainroom.com>
//            Sara Golemon <pollita@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `http://` / `https://` stream wrapper
//! (`ext/standard/http_fopen_wrapper.c`).
//!
//! [`HttpWrapper`] builds the request (method, request line, `Host`,
//! `Authorization`, `From`, `User-Agent`, user headers with the POST-header
//! and credential stripping rules on redirects, `content`), sends it over a
//! transport stream, parses the status line and headers (folding,
//! `Location`, `Content-Type`/`Content-Length` notifications, chunked
//! transfer decoding through the `dechunk` filter), follows redirects and
//! returns the socket stream positioned at the body, its `wrapper_data`
//! holding the response header lines.
//!
//! What needs the VM goes through [`HttpEnv`] (installed with [`set_env`]):
//! connecting through the registered transports (`tcp`, `ssl`), enabling
//! TLS on a proxy tunnel, throwing the `uri_parser_class` `ValueError` and
//! publishing `$http_response_header`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::context::{self, notify, ContextRef};
use crate::diag::{Severity, StreamErrorCode};
use crate::filter::{ChainKind, StreamFilter};
use crate::stream::{flags, Stream, StreamOption};
use crate::util::starts_with_ci;
use crate::value::{Key, Value};
use crate::wrapper::{OpenError, Wrapper, REPORT_ERRORS, STREAM_WILL_CAST};

/// The wrapper label (`wrapper_type` in the meta data).
pub const LABEL: &str = "http";
/// `PHP_URL_REDIRECT_MAX`.
pub const REDIRECT_MAX: i64 = 20;
/// `STREAM_ONLY_GET_HEADERS` (`get_headers()`).
pub const STREAM_ONLY_GET_HEADERS: u32 = 0x200;
/// `HTTP_HEADER_MAX_LOCATION_SIZE`.
const MAX_LOCATION_SIZE: usize = 8182;
/// `PHP_TIMEOUT_ULL_MAX / 1000000.0`.
const TIMEOUT_MAX: f64 = 18_446_744_073_709.55;

/// `HTTP_HEADER_*` bits of the user-supplied headers.
mod have {
    pub const USER_AGENT: u32 = 1;
    pub const HOST: u32 = 2;
    pub const AUTH: u32 = 4;
    pub const FROM: u32 = 8;
    pub const CONTENT_LENGTH: u32 = 16;
    pub const TYPE: u32 = 32;
    pub const CONNECTION: u32 = 64;
}

/// `HTTP_WRAPPER_*` request flags.
mod wflags {
    pub const HEADER_INIT: u32 = 1;
    pub const REDIRECTED: u32 = 2;
    pub const KEEP_METHOD: u32 = 4;
    pub const STRIP_AUTH: u32 = 8;
}

/// Services of the embedding VM.
pub trait HttpEnv {
    /// `php_stream_xport_create(transport, CLIENT | CONNECT, timeout,
    /// context)`.
    ///
    /// # Errors
    /// `Some(errstr)`: the connection error logged as the open failure
    /// message.
    fn connect(
        &self,
        transport: &[u8],
        timeout: Option<Duration>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, Option<Vec<u8>>>;

    /// `php_stream_xport_crypto_setup()` + `crypto_enable()` on a proxy
    /// tunnel; `peer_name` is set as the `ssl` context option for the
    /// handshake (when the context has none). Returns the (possibly
    /// replaced) stream, `None` (stream closed) when the handshake failed.
    fn enable_crypto(&self, stream: Stream, peer_name: Option<&[u8]>) -> Option<Stream>;

    /// Throws `ValueError: f(): Provided stream context has invalid value
    /// for the "uri_parser_class" option`.
    fn invalid_uri_parser(&self);

    /// Whether an exception is pending (raised by a callback such as the
    /// notifier or a transport).
    fn exception_pending(&self) -> bool {
        false
    }

    /// Stores the response headers of the finished request:
    /// `http_get_last_response_headers()` and the caller's
    /// `$http_response_header` (not called when no request was sent).
    fn publish_headers(&self, headers: &[Vec<u8>]);
}

/// The environment used when no VM installed one: plain TCP only.
#[derive(Debug)]
struct DefaultEnv;

impl HttpEnv for DefaultEnv {
    fn connect(
        &self,
        transport: &[u8],
        timeout: Option<Duration>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, Option<Vec<u8>>> {
        match crate::transports::socket_client(transport, timeout) {
            Ok(mut s) => {
                s.context = context.cloned();
                Ok(s)
            }
            Err(e) => Err(Some(e.message.into_bytes())),
        }
    }

    fn enable_crypto(&self, mut stream: Stream, _peer_name: Option<&[u8]>) -> Option<Stream> {
        stream.close();
        None
    }

    fn invalid_uri_parser(&self) {}

    fn publish_headers(&self, headers: &[Vec<u8>]) {
        LAST_HEADERS.with(|l| *l.borrow_mut() = Some(headers.to_vec()));
    }
}

thread_local! {
    static ENV: RefCell<Rc<dyn HttpEnv>> = RefCell::new(Rc::new(DefaultEnv));
    static LAST_HEADERS: RefCell<Option<Vec<Vec<u8>>>> = const { RefCell::new(None) };
}

/// Installs the VM services.
pub fn set_env(env: Rc<dyn HttpEnv>) {
    ENV.with(|e| *e.borrow_mut() = env);
}

pub(crate) fn env() -> Rc<dyn HttpEnv> {
    ENV.with(|e| Rc::clone(&e.borrow()))
}

/// The headers stored by the default environment (library tests).
pub fn take_default_headers() -> Option<Vec<Vec<u8>>> {
    LAST_HEADERS.with(|l| l.borrow_mut().take())
}

// ---------------------------------------------------------------------------
// URI parsing (`php_uri_parse_to_struct` with the parser chosen by the
// `uri_parser_class` context option)
// ---------------------------------------------------------------------------

/// The components the wrapper uses (`php_uri`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Uri {
    /// `scheme`.
    pub scheme: Option<Vec<u8>>,
    /// `user`.
    pub user: Option<Vec<u8>>,
    /// `password`.
    pub password: Option<Vec<u8>>,
    /// `host`.
    pub host: Option<Vec<u8>>,
    /// `port` (0 when absent).
    pub port: i64,
    /// `path`.
    pub path: Option<Vec<u8>>,
    /// `query`.
    pub query: Option<Vec<u8>>,
}

/// A registered URI parser (`php_uri_parser`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriParser {
    /// `parse_url()` (`PHP_URI_PARSER_PHP_PARSE_URL`, the default).
    ParseUrl,
    /// `Uri\Rfc3986\Uri`.
    Rfc3986,
    /// `Uri\WhatWg\Url`.
    WhatWg,
}

impl UriParser {
    /// `php_stream_context_get_uri_parser()`: `None` for an invalid
    /// `uri_parser_class` option.
    pub fn from_context(wrapper: &str, ctx: Option<&ContextRef>) -> Option<Self> {
        let Some(c) = ctx else {
            return Some(UriParser::ParseUrl);
        };
        let c = c.borrow();
        match c.get(wrapper, "uri_parser_class") {
            None | Some(Value::Null) => Some(UriParser::ParseUrl),
            Some(Value::Str(s)) => match s.as_slice() {
                b"Uri\\Rfc3986\\Uri" => Some(UriParser::Rfc3986),
                b"Uri\\WhatWg\\Url" => Some(UriParser::WhatWg),
                _ => None,
            },
            Some(_) => None,
        }
    }

    /// Parses `url` in raw read mode (silently); `None` when malformed.
    pub fn parse(self, url: &[u8]) -> Option<Uri> {
        match self {
            UriParser::ParseUrl => {
                let p = php_stdstr::url::parse_url_ex(url)?;
                Some(Uri {
                    scheme: p.scheme,
                    user: p.user,
                    password: p.pass,
                    host: p.host,
                    port: p.port.map_or(0, i64::from),
                    path: p.path,
                    query: p.query,
                })
            }
            UriParser::Rfc3986 => {
                let u = php_uri::api::Rfc3986Uri::parse(url, None)?;
                Some(Uri {
                    scheme: u.raw_scheme(),
                    user: u.raw_username(),
                    password: u.raw_password(),
                    host: u.raw_host(),
                    port: u.port().unwrap_or(0),
                    path: Some(u.raw_path()),
                    query: u.raw_query(),
                })
            }
            UriParser::WhatWg => {
                let (u, _) = php_uri::api::WhatWgUrl::parse(url, None).ok()?;
                Some(Uri {
                    scheme: Some(u.scheme()),
                    user: u.username(),
                    password: u.password(),
                    host: u.ascii_host(),
                    port: u.port().unwrap_or(0),
                    path: Some(u.path()),
                    query: u.query(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

/// `next_header_line()`: the index after the line starting at `i`.
fn next_header_line(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i] != b'\r' && s[i] != b'\n' {
        i += 1;
    }
    if s.get(i) == Some(&b'\r') {
        i += 1;
    }
    if s.get(i) == Some(&b'\n') {
        i += 1;
    }
    i
}

/// `strip_header()`: removes every `name:` line (with its folded
/// continuation lines) from `bag`; `lc` is the lower-cased copy kept in
/// step.
fn strip_header(bag: &mut Vec<u8>, lc: &mut Vec<u8>, name: &[u8]) {
    let mut line = 0usize;
    while line < lc.len() {
        if !lc[line..].starts_with(name) {
            line = next_header_line(lc, line);
            continue;
        }
        let mut colon = line + name.len();
        while matches!(lc.get(colon), Some(b' ' | b'\t')) {
            colon += 1;
        }
        if lc.get(colon) != Some(&b':') {
            line = next_header_line(lc, line);
            continue;
        }
        let mut next = next_header_line(lc, line);
        while matches!(lc.get(next), Some(b' ' | b'\t')) {
            next = next_header_line(lc, next);
        }
        let mut start = line;
        if next >= lc.len() {
            // Drop the preceding line break too, or the CRLF appended after
            // the bag would end the header block early.
            while start > 0 && matches!(lc[start - 1], b'\r' | b'\n') {
                start -= 1;
            }
        }
        bag.drain(start..next);
        lc.drain(start..next);
        line = start;
    }
}

/// `check_has_header()`: `header` at the start of the bag or of a line
/// following `\r\n`.
fn check_has_header(headers: &[u8], header: &[u8]) -> bool {
    let mut from = 0;
    while let Some(p) = crate::util::find_bytes(&headers[from..], header) {
        let s = from + p;
        if s == 0 || (s >= 2 && headers[s - 1] == b'\n' && headers[s - 2] == b'\r') {
            return true;
        }
        from = s + 1;
    }
    false
}

/// `php_stream_handle_proxy_authorization_header()`: appends the first
/// `Proxy-Authorization` line of `s` to `out`.
fn proxy_authorization_header(s: &[u8], out: &mut Vec<u8>) -> bool {
    let name: &[u8] = b"Proxy-Authorization:";
    let mut i = 0;
    loop {
        while matches!(s.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        let mut p = i;
        while p < s.len() && !matches!(s[p], b':' | b'\r' | b'\n' | 0) {
            p += 1;
        }
        if s.get(p) == Some(&b':') {
            p += 1;
            let found = s[i..p].eq_ignore_ascii_case(name);
            while p < s.len() && !matches!(s[p], b'\r' | b'\n' | 0) {
                p += 1;
            }
            if found {
                out.extend_from_slice(&s[i..p]);
                out.extend_from_slice(b"\r\n");
                return true;
            }
        }
        i = p;
        while matches!(s.get(i), Some(b'\r' | b'\n')) {
            i += 1;
        }
        if i >= s.len() || s[i] == 0 {
            return false;
        }
    }
}

/// `php_trim(str, NULL, 0, 3)`: strips `" \t\n\r\0\x0B"` on both sides.
fn php_trim(s: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0 | 0x0b);
    let Some(start) = s.iter().position(|c| !ws(c)) else {
        return &[];
    };
    let end = s.iter().rposition(|c| !ws(c)).map_or(start, |e| e + 1);
    &s[start..end]
}

/// `smart_str_append_header_value()`: the value up to the first CR/LF,
/// warning when it was truncated.
fn append_header_value(dest: &mut Vec<u8>, value: &[u8], name: &str) {
    match value.iter().position(|&c| c == b'\r' || c == b'\n') {
        Some(i) => {
            dest.extend_from_slice(&value[..i]);
            crate::diag::warning(format!(
                "Header {name} value contains newline characters and has been truncated"
            ));
        }
        None => dest.extend_from_slice(value),
    }
}

/// `php_stream_http_response_header_trim()`: drops the line terminator
/// and trailing blanks; returns whether blanks were removed.
fn header_trim(line: &mut Vec<u8>) -> bool {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    let space = matches!(line.last(), Some(b' ' | b'\t'));
    while matches!(line.last(), Some(b' ' | b'\t')) {
        line.pop();
    }
    space
}

/// The bytes of `s` up to the first NUL (a C string view).
fn cstr(s: &[u8]) -> &[u8] {
    s.iter().position(|&c| c == 0).map_or(s, |i| &s[..i])
}

/// `atoi()`.
fn atoi(s: &[u8]) -> i64 {
    let (v, _) = crate::util::strtol(s);
    i64::from(i32::try_from(v).unwrap_or(0))
}

/// `php_url_decode()`.
fn url_decode(s: &[u8]) -> Vec<u8> {
    php_stdstr::url::urldecode(s)
}

/// The response code of a status line (`atoi(line + 9)`).
fn status_code(line: &[u8]) -> i64 {
    if line.len() > 9 {
        atoi(&line[9..])
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// `php_stream_http_response_header_info`.
struct HeaderInfo {
    transfer_encoding: Option<Box<dyn StreamFilter>>,
    file_size: u64,
    error: bool,
    follow_location: bool,
    location: Option<Vec<u8>>,
}

impl HeaderInfo {
    fn new() -> Self {
        HeaderInfo {
            transfer_encoding: None,
            file_size: 0,
            error: false,
            follow_location: true,
            location: None,
        }
    }
}

/// State shared by every hop of one request.
struct Request<'a> {
    /// `wrapper->wops->label`: `"http"`, or `"ftp"` when the `ftp://`
    /// wrapper proxies a read through HTTP (selects the `proxy` and
    /// `timeout` options).
    label: &'static str,
    mode: &'a str,
    options: u32,
    context: Option<&'a ContextRef>,
    /// Messages logged with `php_stream_wrapper_log_warn()`.
    log: Vec<Vec<u8>>,
    /// The response headers (`None` until the first request was sent).
    headers: Option<Vec<Vec<u8>>>,
    /// Whether the failure is an exception (no warning is shown).
    exception: bool,
}

impl Request<'_> {
    /// `php_stream_context_get_option(context, "http", name)`.
    fn opt(&self, name: &str) -> Option<Value> {
        self.context
            .and_then(|c| c.borrow().get("http", name).cloned())
    }

    /// `php_stream_context_get_option(context, wrapper->wops->label, name)`.
    fn wrapper_opt(&self, name: &str) -> Option<Value> {
        self.context
            .and_then(|c| c.borrow().get(self.label, name).cloned())
    }

    /// `php_stream_wrapper_log_warn()`.
    fn log(&mut self, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
        let msg = msg.into();
        if self.options & REPORT_ERRORS == 0 {
            self.log.push(msg);
        } else {
            crate::diag::stream_error(Severity::Warning, LABEL, code, true, msg);
        }
    }

    fn headers(&mut self) -> &mut Vec<Vec<u8>> {
        self.headers.get_or_insert_with(Vec::new)
    }
}

/// The stream wrapper (`php_stream_http_wrapper`), registered for both
/// `http` and `https`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpWrapper;

impl Wrapper for HttpWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn is_url(&self) -> bool {
        true
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        _opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        open_url(path, mode, options, context)
    }
}

/// `php_stream_url_wrap_http()`.
///
/// # Errors
/// The logged messages, joined into "Failed to open stream: ..." by the
/// caller.
pub fn open_url(
    path: &[u8],
    mode: &str,
    options: u32,
    context: Option<&ContextRef>,
) -> Result<Stream, OpenError> {
    open_url_as(LABEL, path, mode, options, context)
}

/// [`open_url`] on behalf of the wrapper labelled `label` (the `ftp://`
/// wrapper proxying a read through HTTP).
///
/// # Errors
/// As [`open_url`].
pub fn open_url_as(
    label: &'static str,
    path: &[u8],
    mode: &str,
    options: u32,
    context: Option<&ContextRef>,
) -> Result<Stream, OpenError> {
    let mut req = Request {
        label,
        mode,
        options,
        context,
        log: Vec::new(),
        headers: None,
        exception: false,
    };
    let stream = open_ex(&mut req, path, REDIRECT_MAX, wflags::HEADER_INIT);
    if let Some(h) = &req.headers {
        env().publish_headers(h);
    }
    match stream {
        Some(s) => Ok(s),
        None if req.exception => Err(OpenError::exception()),
        None => Err(OpenError {
            messages: req.log,
            ..OpenError::default()
        }),
    }
}

/// How the request reaches the server.
struct Target {
    use_ssl: bool,
    use_proxy: bool,
    request_fulluri: bool,
    transport: Vec<u8>,
}

/// Picks the transport (`tcp://host:port`, `ssl://...` or the proxy).
fn target(req: &mut Request<'_>, resource: &mut Uri) -> Option<Target> {
    let scheme = resource.scheme.clone().unwrap_or_default();
    let proxy = match req.wrapper_opt("proxy") {
        Some(Value::Str(s)) if !s.is_empty() => Some(s),
        _ => None,
    };
    if !scheme.eq_ignore_ascii_case(b"http") && !scheme.eq_ignore_ascii_case(b"https") {
        // Only reachable when another wrapper proxies over HTTP (ftp).
        let p = proxy?;
        return Some(Target {
            use_ssl: false,
            use_proxy: true,
            request_fulluri: true,
            transport: p,
        });
    }
    if req.mode.contains(['a', 'w', 'x', '+']) {
        req.log(
            StreamErrorCode::ModeNotSupported,
            "HTTP wrapper does not support writeable connections",
        );
        return None;
    }
    let request_fulluri = req.opt("request_fulluri").is_some_and(|v| v.is_true());
    // The whole scheme, in any case (GHSA-f6gg-rchx-xw8j, CVE-2026-128064:
    // `HTTPS://` went out in plain text).
    let use_ssl = scheme.eq_ignore_ascii_case(b"https");
    if resource.port == 0 {
        resource.port = if use_ssl { 443 } else { 80 };
    }
    let (use_proxy, transport) = if let Some(p) = proxy {
        (true, p)
    } else {
        let mut t = if use_ssl {
            b"ssl://".to_vec()
        } else {
            b"tcp://".to_vec()
        };
        t.extend_from_slice(resource.host.as_deref().unwrap_or_default());
        t.push(b':');
        t.extend_from_slice(resource.port.to_string().as_bytes());
        (false, t)
    };
    Some(Target {
        use_ssl,
        use_proxy,
        request_fulluri,
        transport,
    })
}

/// The `timeout` option (`default_socket_timeout` otherwise); `Err` after
/// logging when it is too large. `None` waits forever.
fn timeout(req: &mut Request<'_>) -> Result<Option<Duration>, ()> {
    let secs = if let Some(v) = req.wrapper_opt("timeout") {
        let d = v.to_double();
        if d > TIMEOUT_MAX {
            req.log(
                StreamErrorCode::InvalidParam,
                "timeout must be lower than 18446744073709",
            );
            return Err(());
        }
        d
    } else {
        #[allow(clippy::cast_precision_loss)] // INI seconds
        let d = crate::state::settings().default_socket_timeout as f64;
        d
    };
    if secs.is_nan() || secs < 0.0 {
        return Ok(None);
    }
    Ok(Duration::try_from_secs_f64(secs).ok())
}

/// Opens the `CONNECT` tunnel through an HTTP proxy and enables TLS.
fn proxy_tunnel(req: &mut Request<'_>, mut stream: Stream, resource: &Uri) -> Option<Stream> {
    let host = resource.host.clone().unwrap_or_default();
    let mut header = b"CONNECT ".to_vec();
    header.extend_from_slice(&host);
    header.push(b':');
    header.extend_from_slice(resource.port.to_string().as_bytes());
    header.extend_from_slice(b" HTTP/1.0\r\n");
    match req.opt("header") {
        Some(Value::Array(items)) => {
            for (_, v) in &items {
                if let Value::Str(s) = v {
                    if proxy_authorization_header(s, &mut header) {
                        break;
                    }
                }
            }
        }
        Some(Value::Str(s)) if !s.is_empty() => {
            proxy_authorization_header(&s, &mut header);
        }
        _ => {}
    }
    header.extend_from_slice(b"\r\n");
    if stream.write(&header).ok() != Some(header.len()) {
        req.log(
            StreamErrorCode::ProtocolError,
            "Cannot connect to HTTPS server through proxy",
        );
        stream.close();
        return None;
    }
    while let Some(line) = stream.get_line(Some(1023)) {
        if matches!(line.first(), None | Some(b'\n' | b'\r' | 0)) {
            break;
        }
    }
    let has_peer_name = req
        .context
        .is_some_and(|c| c.borrow().get("ssl", "peer_name").is_some());
    let peer = (!has_peer_name).then_some(host.as_slice());
    let s = env().enable_crypto(stream, peer);
    if s.is_none() {
        req.log(
            StreamErrorCode::SslNotSupported,
            "Cannot connect to HTTPS server through proxy",
        );
    }
    s
}

/// Removes the `Proxy-Authorization` line (sent with `CONNECT`) from the
/// user headers.
fn remove_proxy_authorization(user: &mut Option<Vec<u8>>, t: &[u8]) {
    let name: &[u8] = b"proxy-authorization:";
    let Some(mut s) = crate::util::find_bytes(t, name) else {
        return;
    };
    if s != 0 && t[s - 1] != b'\n' {
        return;
    }
    let mut p = s + name.len();
    while s > 0 && matches!(t[s - 1], b' ' | b'\t') {
        s -= 1;
    }
    while p < t.len() && !matches!(t[p], b'\r' | b'\n') {
        p += 1;
    }
    while p < t.len() && matches!(t[p], b'\r' | b'\n') {
        p += 1;
    }
    let Some(u) = user.as_mut() else { return };
    if p >= t.len() {
        if s == 0 {
            *user = None;
        } else {
            while s > 0 && matches!(t[s - 1], b'\r' | b'\n') {
                s -= 1;
            }
            u.truncate(s);
        }
    } else if p <= u.len() {
        u.drain(s..p);
    }
}

/// The user headers after normalisation and stripping, and the `have`
/// bits.
fn user_headers(req: &Request<'_>, flags: u32, t: &Target) -> (Option<Vec<u8>>, u32) {
    let tmp: Vec<u8> = match req.opt("header") {
        Some(Value::Array(items)) => {
            let mut all = Vec::new();
            for (_, v) in &items {
                if let Value::Str(s) = v {
                    all.extend_from_slice(s);
                    all.extend_from_slice(b"\r\n");
                }
            }
            php_trim(&all).to_vec()
        }
        Some(Value::Str(s)) if !s.is_empty() => php_trim(&s).to_vec(),
        _ => Vec::new(),
    };
    let mut have = 0;
    if tmp.is_empty() {
        return (None, have);
    }
    let mut bag = tmp.clone();
    let mut lc = tmp.to_ascii_lowercase();
    if flags & (wflags::HEADER_INIT | wflags::KEEP_METHOD) == 0 {
        // Strip POST headers on redirect.
        strip_header(&mut bag, &mut lc, b"content-length");
        strip_header(&mut bag, &mut lc, b"content-type");
    }
    if flags & wflags::STRIP_AUTH != 0 {
        strip_header(&mut bag, &mut lc, b"authorization");
        strip_header(&mut bag, &mut lc, b"cookie");
        if !t.use_proxy {
            strip_header(&mut bag, &mut lc, b"proxy-authorization");
        }
    }
    let mut user = (!bag.is_empty()).then_some(bag);
    for (name, bit) in [
        (&b"user-agent:"[..], have::USER_AGENT),
        (b"host:", have::HOST),
        (b"from:", have::FROM),
        (b"authorization:", have::AUTH),
        (b"content-length:", have::CONTENT_LENGTH),
        (b"content-type:", have::TYPE),
        (b"connection:", have::CONNECTION),
    ] {
        if check_has_header(&lc, name) {
            have |= bit;
        }
    }
    if t.use_proxy && t.use_ssl {
        remove_proxy_authorization(&mut user, &lc);
    }
    (user, have)
}

/// The `content` option when it is a non-empty string.
fn content(req: &Request<'_>) -> Option<Vec<u8>> {
    match req.opt("content") {
        Some(Value::Str(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Appends `Content-Length: n\r\n`.
fn content_length(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(format!("Content-Length: {n}\r\n").as_bytes());
}

/// Builds the request (request line, headers and body).
#[allow(clippy::too_many_lines)] // the header assembly of the C function
fn build_request(
    req: &Request<'_>,
    path: &[u8],
    resource: &Uri,
    t: &Target,
    flags: u32,
) -> Vec<u8> {
    let redirected = flags & wflags::REDIRECTED != 0;
    let keep_method = flags & wflags::KEEP_METHOD != 0;
    let mut out = Vec::new();
    let mut custom_method = false;
    if let Some(Value::Str(m)) = req.opt("method") {
        // Redirected requests only keep GET and HEAD unless 307/308.
        if !m.is_empty() && (!redirected || keep_method || m == b"GET" || m == b"HEAD") {
            custom_method = true;
            out.extend_from_slice(&m);
            out.push(b' ');
        }
    }
    if !custom_method {
        out.extend_from_slice(b"GET ");
    }
    if t.request_fulluri {
        out.extend_from_slice(cstr(path));
    } else {
        match resource.path.as_deref() {
            Some(p) if !p.is_empty() => out.extend_from_slice(p),
            _ => out.push(b'/'),
        }
        if let Some(q) = &resource.query {
            out.push(b'?');
            out.extend_from_slice(q);
        }
    }
    match req.opt("protocol_version") {
        Some(v) => out.extend_from_slice(format!(" HTTP/{:.1}\r\n", v.to_double()).as_bytes()),
        None => out.extend_from_slice(b" HTTP/1.1\r\n"),
    }
    let (user, mut have) = user_headers(req, flags, t);
    if have & have::AUTH == 0 {
        if let Some(u) = &resource.user {
            let mut scratch = url_decode(u);
            scratch.push(b':');
            if let Some(p) = &resource.password {
                scratch.extend_from_slice(&url_decode(p));
            }
            out.extend_from_slice(b"Authorization: Basic ");
            out.extend_from_slice(&php_stdstr::base64::base64_encode(&scratch));
            out.extend_from_slice(b"\r\n");
            context::notify_info(req.context, notify::AUTH_REQUIRED, None, 0);
        }
    }
    let settings = crate::state::settings();
    if have & have::FROM == 0 {
        if let Some(from) = &settings.from_address {
            out.extend_from_slice(b"From: ");
            append_header_value(&mut out, from, "From");
            out.extend_from_slice(b"\r\n");
        }
    }
    if have & have::HOST == 0 {
        out.extend_from_slice(b"Host: ");
        out.extend_from_slice(resource.host.as_deref().unwrap_or_default());
        let port = resource.port;
        let default_port = if t.use_ssl { 443 } else { 80 };
        if port != default_port && port != 0 {
            out.push(b':');
            out.extend_from_slice(port.to_string().as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    if have & have::CONNECTION == 0 {
        out.extend_from_slice(b"Connection: close\r\n");
    }
    let ua = match req.opt("user_agent") {
        Some(Value::Str(s)) => Some(s),
        _ => settings.user_agent.clone(),
    };
    if have & have::USER_AGENT == 0 {
        if let Some(ua) = ua.filter(|u| !u.is_empty()) {
            out.extend_from_slice(b"User-Agent: ");
            append_header_value(&mut out, &ua, "User-Agent");
            out.extend_from_slice(b"\r\n");
        }
    }
    let body = if flags & (wflags::HEADER_INIT | wflags::KEEP_METHOD) == 0 {
        None
    } else {
        content(req)
    };
    if let Some(user) = user {
        // Some servers want Content-Length before Content-Type (#44603).
        if let Some(b) = &body {
            if have & have::CONTENT_LENGTH == 0 {
                content_length(&mut out, b.len());
                have |= have::CONTENT_LENGTH;
            }
        }
        out.extend_from_slice(&user);
        out.extend_from_slice(b"\r\n");
    }
    if let Some(b) = body {
        if have & have::CONTENT_LENGTH == 0 {
            content_length(&mut out, b.len());
        }
        if have & have::TYPE == 0 {
            out.extend_from_slice(b"Content-Type: application/x-www-form-urlencoded\r\n");
            if req.options & REPORT_ERRORS != 0 {
                crate::diag::stream_error(
                    Severity::Notice,
                    LABEL,
                    StreamErrorCode::InvalidHeader,
                    false,
                    "Content-type not specified assuming application/x-www-form-urlencoded",
                );
            }
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&b);
    } else {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `php_stream_http_response_headers_parse()`: folds `line` into `last`
/// (returning it) or stores the finished `last` header (returning
/// `None`; `info.error` is set when it was invalid).
fn parse_header(
    req: &mut Request<'_>,
    mut last: Vec<u8>,
    line: Option<&mut Vec<u8>>,
    response_code: i64,
    info: &mut HeaderInfo,
) -> Option<Vec<u8>> {
    if let Some(line) = line {
        if !matches!(line.first(), None | Some(b'\n' | b'\r')) {
            if header_trim(line) && line.is_empty() {
                // Only blanks: an empty folding line.
                return Some(last);
            }
            if matches!(line.first(), Some(b' ' | b'\t')) {
                let start = line
                    .iter()
                    .position(|c| !matches!(c, b' ' | b'\t'))
                    .unwrap_or(line.len());
                last.push(b' ');
                last.extend_from_slice(&line[start..]);
                return Some(last);
            }
        }
    }
    let Some(colon) = last.iter().position(|&c| c == b':') else {
        info.error = true;
        req.log(
            StreamErrorCode::InvalidResponse,
            "HTTP invalid response format (no colon in header line)!",
        );
        return None;
    };
    if last
        .get(1..colon)
        .is_some_and(|name| name.iter().any(|c| matches!(c, b' ' | b'\t')))
    {
        info.error = true;
        req.log(
            StreamErrorCode::InvalidResponse,
            "HTTP invalid response format (space in header name)!",
        );
        return None;
    }
    let mut v = colon + 1;
    let end = last.len().saturating_sub(1);
    while v < end && matches!(last[v], b' ' | b'\t') {
        v += 1;
    }
    let value = cstr(&last[v.min(last.len())..]).to_vec();
    let mut store = true;
    if starts_with_ci(&last, b"Location:") {
        if let Some(f) = req.opt("follow_location") {
            info.follow_location = f.is_true();
        } else if !((300..304).contains(&response_code)
            || response_code == 307
            || response_code == 308)
        {
            // Only 300-303, 307 and 308 redirect automatically.
            info.follow_location = false;
        }
        if value.len() > MAX_LOCATION_SIZE {
            info.error = true;
            req.log(
                StreamErrorCode::InvalidResponse,
                format!("HTTP Location header size is over the limit of {MAX_LOCATION_SIZE} bytes"),
            );
            return None;
        }
        info.location = Some(value);
    } else if starts_with_ci(&last, b"Content-Type:") {
        context::notify_info(req.context, notify::MIME_TYPE_IS, Some(&value), 0);
    } else if starts_with_ci(&last, b"Content-Length:") {
        // Digits only (RFC 9110), no sign.
        if !value.is_empty() && value.iter().all(u8::is_ascii_digit) {
            let parsed = value.iter().fold(0u64, |acc, &c| {
                acc.checked_mul(10)
                    .and_then(|a| a.checked_add(u64::from(c - b'0')))
                    .unwrap_or(u64::MAX)
            });
            info.file_size = parsed.min(i64::MAX.unsigned_abs());
            context::notify_file_size(req.context, info.file_size, cstr(&last), 0);
        }
    } else if starts_with_ci(&last, b"Transfer-Encoding:")
        && starts_with_ci(&value, b"Chunked")
        && req.options & STREAM_ONLY_GET_HEADERS == 0
    {
        let decode = req.opt("auto_decode").is_none_or(|v| v.is_true());
        if decode {
            info.transfer_encoding = crate::wrapper::create_filter("dechunk", None, false);
            if info.transfer_encoding.is_some() {
                // The decoded body no longer is chunked.
                store = false;
            }
        }
    }
    if store {
        req.headers().push(last);
    }
    None
}

/// Reads the status line (skipping 1xx informational responses other
/// than 101); `None` when the server sent nothing.
fn read_status(stream: &mut Stream) -> Option<(Vec<u8>, i64)> {
    if stream.eof() {
        return None;
    }
    let mut line = stream.get_line(Some(127))?;
    let mut code = status_code(&line);
    if (100..200).contains(&code) && code != 101 {
        while !stream.eof() {
            let Some(l) = stream.get_line(Some(127)) else {
                break;
            };
            line = l;
            if line.len() >= 6 && line[..6].eq_ignore_ascii_case(b"HTTP/1") {
                break;
            }
        }
        code = status_code(&line);
    }
    Some((line, code))
}

/// Reads the header lines after the status line; `None` (stream closed,
/// error logged) when the response is invalid.
fn read_headers(
    req: &mut Request<'_>,
    mut stream: Stream,
    response_code: i64,
    info: &mut HeaderInfo,
) -> Option<Stream> {
    let mut last_header: Option<Vec<u8>> = None;
    while !stream.eof() {
        let Some(mut line) = stream.get_line(None) else {
            break;
        };
        let last_line = match line[0] {
            b'\r' => {
                if line.get(1) != Some(&b'\n') {
                    stream.close();
                    req.log(
                        StreamErrorCode::InvalidResponse,
                        "HTTP invalid header name (cannot start with CR character)!",
                    );
                    return None;
                }
                true
            }
            b'\n' => true,
            _ => false,
        };
        if let Some(prev) = last_header.take() {
            if let Some(folded) = parse_header(req, prev, Some(&mut line), response_code, info) {
                last_header = Some(folded);
                continue;
            }
            if info.error {
                stream.close();
                return None;
            }
        } else if !last_line {
            if matches!(line[0], b' ' | b'\t') {
                stream.close();
                req.log(
                    StreamErrorCode::InvalidResponse,
                    "HTTP invalid response format (folding header at the start)!",
                );
                return None;
            }
            header_trim(&mut line);
        }
        if last_line {
            break;
        }
        last_header = Some(line);
    }
    if let Some(prev) = last_header {
        // The stream ended early: still process the last line (BC).
        let _ = parse_header(req, prev, None, response_code, info);
    }
    Some(stream)
}

/// `php_stream_url_wrap_http_ex()`.
#[allow(clippy::too_many_lines)] // a direct port of the C function
fn open_ex(req: &mut Request<'_>, path: &[u8], redirect_max: i64, flags: u32) -> Option<Stream> {
    let header_init = flags & wflags::HEADER_INIT != 0;
    let mut redirect_max = redirect_max;
    if redirect_max < 1 {
        req.log(
            StreamErrorCode::RedirectLimit,
            "Redirection limit reached, aborting",
        );
        return None;
    }
    let Some(parser) = UriParser::from_context("http", req.context) else {
        env().invalid_uri_parser();
        req.exception = true;
        return None;
    };
    let mut resource = parser.parse(cstr(path))?;
    let t = target(req, &mut resource)?;
    if t.request_fulluri && path.iter().any(|&c| c == b'\n' || c == b'\r') {
        req.log(
            StreamErrorCode::InvalidUrl,
            "HTTP wrapper full URI path does not allow CR or LF characters",
        );
        return None;
    }
    let timeout = timeout(req).ok()?;
    let e = env();
    let mut stream = match e.connect(&t.transport, timeout, req.context) {
        Ok(mut s) => {
            let (sec, usec) = timeout.map_or((-1, 0), |d| {
                (
                    i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                    i64::from(d.subsec_micros()),
                )
            });
            s.set_option(&mut StreamOption::ReadTimeout(sec, usec));
            s
        }
        Err(err) => {
            if let Some(msg) = err {
                req.log(StreamErrorCode::ProtocolError, msg);
            }
            if e.exception_pending() {
                req.exception = true;
            }
            return None;
        }
    };
    if t.use_proxy && t.use_ssl {
        stream = proxy_tunnel(req, stream, &resource)?;
    }
    let mut info = HeaderInfo::new();
    let chunk_size = (req.options & STREAM_WILL_CAST != 0).then(|| {
        let old = stream.chunk_size;
        stream.set_chunk_size(1);
        old
    });
    // Headers always use CRLF: no line ending detection while reading them.
    let eol_detect = stream.state.flags & (flags::DETECT_EOL | flags::EOL_MAC);
    stream.state.flags &= !(flags::DETECT_EOL | flags::EOL_MAC);
    stream.context = req.context.cloned();
    context::notify_info(req.context, notify::CONNECT, None, 0);
    if header_init {
        if let Some(m) = req.opt("max_redirects") {
            // `(int)zval_get_long()`.
            #[allow(clippy::cast_possible_truncation)]
            let m = m.to_long() as i32;
            redirect_max = i64::from(m);
        }
    }
    let request = build_request(req, path, &resource, &t, flags);
    let _ = stream.write(&request);
    req.headers();

    let Some((status, response_code)) = read_status(&mut stream) else {
        stream.close();
        req.log(StreamErrorCode::ProtocolError, "HTTP request failed!");
        return None;
    };
    let ignore_errors = req.opt("ignore_errors").is_some_and(|v| v.is_true());
    // When only the headers are wanted, error codes do not fail.
    let mut reqok = req.options & STREAM_ONLY_GET_HEADERS != 0 || ignore_errors;
    if (200..400).contains(&response_code) {
        reqok = true;
    } else {
        let code = if response_code == 403 {
            notify::AUTH_RESULT
        } else {
            notify::FAILURE
        };
        context::notify_error(req.context, code, Some(cstr(&status)), response_code);
    }
    let mut shown = status.clone();
    if shown.last() == Some(&b'\n') {
        shown.pop();
        if shown.last() == Some(&b'\r') {
            shown.pop();
        }
    } else {
        // Read and discard the rest of the status line.
        let _ = stream.get_line(None);
    }
    req.headers().push(shown);

    let stream = read_headers(req, stream, response_code, &mut info)?;

    let mut result = Some(stream);
    if !reqok || (info.location.is_some() && info.follow_location) {
        let only_headers = req.options & STREAM_ONLY_GET_HEADERS != 0;
        if info.follow_location && !((only_headers || ignore_errors) && redirect_max <= 1) {
            if let Some(loc) = &info.location {
                context::notify_info(req.context, notify::REDIRECTED, Some(loc), 0);
            }
            if let Some(mut s) = result.take() {
                s.close();
            }
            info.transfer_encoding = None;
            if let Some(loc) = info.location.take() {
                result = redirect(req, &resource, &t, &loc, response_code, flags, redirect_max);
            } else {
                let mut msg = b"HTTP request failed! ".to_vec();
                msg.extend_from_slice(cstr(&status));
                req.log(StreamErrorCode::ProtocolError, msg);
            }
        }
    }

    // out:
    let mut stream = result?;
    if header_init {
        let list = req
            .headers()
            .iter()
            .zip(0i64..)
            .map(|(h, i)| (Key::Int(i), Value::Str(h.clone())))
            .collect();
        stream.wrapper_data = Some(Value::Array(list));
    }
    context::notify_progress_init(req.context, 0, info.file_size);
    if let Some(c) = chunk_size {
        stream.set_chunk_size(c);
    }
    stream.state.flags |= eol_detect;
    // As far as streams are concerned, the body starts here.
    stream.position = 0;
    req.mode.clone_into(&mut stream.mode);
    if let Some(f) = info.transfer_encoding.take() {
        let _ = stream.append_filter(ChainKind::Read, f);
    }
    // The server may already have sent part of the body.
    let unread = stream.unread_bytes();
    if unread > 0 {
        context::notify_progress_increment(
            req.context,
            u64::try_from(unread).unwrap_or(u64::MAX),
            0,
        );
    }
    Some(stream)
}

/// The URL a relative `Location` resolves to.
fn redirect_url(resource: &Uri, t: &Target, location: &[u8]) -> Vec<u8> {
    let absolute = location.len() >= 8
        && [&b"http://"[..], b"https://", b"ftp://", b"ftps://"]
            .iter()
            .any(|p| starts_with_ci(location, p));
    if absolute {
        return location.to_vec();
    }
    let loc_path = if location.first() == Some(&b'/') {
        location.to_vec()
    } else if location.len() > 1 && resource.path.is_some() {
        let mut p = resource.path.clone().unwrap_or_default();
        match p.iter().rposition(|&c| c == b'/') {
            Some(s) => p.truncate(s + 1),
            None if p.is_empty() => p = b"/".to_vec(),
            None => {
                p[0] = b'/';
                p.truncate(1);
            }
        }
        let mut out = cstr(&p).to_vec();
        if out != b"/" {
            out.push(b'/');
        }
        out.extend_from_slice(location);
        out
    } else {
        let mut out = b"/".to_vec();
        out.extend_from_slice(location);
        out
    };
    let mut out = resource.scheme.clone().unwrap_or_default();
    out.extend_from_slice(b"://");
    out.extend_from_slice(cstr(resource.host.as_deref().unwrap_or_default()));
    if (t.use_ssl && resource.port != 443) || (!t.use_ssl && resource.port != 80) {
        out.push(b':');
        out.extend_from_slice(resource.port.to_string().as_bytes());
    }
    out.extend_from_slice(&loc_path);
    out
}

/// Follows a `Location` header.
fn redirect(
    req: &mut Request<'_>,
    resource: &Uri,
    t: &Target,
    location: &[u8],
    response_code: i64,
    flags: u32,
    redirect_max: i64,
) -> Option<Stream> {
    let new_path = redirect_url(resource, t, location);
    let invalid = |req: &mut Request<'_>| {
        let mut m = b"Invalid redirect URL! ".to_vec();
        m.extend_from_slice(cstr(&new_path));
        req.log(StreamErrorCode::InvalidUrl, m);
    };
    let parser = UriParser::from_context("http", req.context).unwrap_or(UriParser::ParseUrl);
    let Some(new_resource) = parser.parse(cstr(&new_path)) else {
        invalid(req);
        return None;
    };
    let default_port = if t.use_ssl { 443 } else { 80 };
    let port_or = |p: i64| if p == 0 { default_port } else { p };
    let same_origin = match (&resource.scheme, &new_resource.scheme, &new_resource.host) {
        (Some(s), Some(ns), Some(nh)) => {
            ns.eq_ignore_ascii_case(s)
                && resource
                    .host
                    .as_ref()
                    .is_some_and(|h| nh.eq_ignore_ascii_case(h))
                && port_or(resource.port) == port_or(new_resource.port)
        }
        _ => false,
    };
    // Control characters in login, password and path.
    let parts = [
        &new_resource.user,
        &new_resource.password,
        &new_resource.path,
    ];
    if parts
        .into_iter()
        .flatten()
        .any(|part| url_decode(part).iter().any(u8::is_ascii_control))
    {
        invalid(req);
        return None;
    }
    let mut new_flags = wflags::REDIRECTED | (flags & wflags::STRIP_AUTH);
    if !same_origin {
        new_flags |= wflags::STRIP_AUTH;
    }
    if response_code == 307 || response_code == 308 {
        // RFC 7538/7231: the method must not change (PATCH likewise).
        new_flags |= wflags::KEEP_METHOD;
    }
    open_ex(req, &new_path, redirect_max - 1, new_flags)
}

#[cfg(test)]
mod tests;
