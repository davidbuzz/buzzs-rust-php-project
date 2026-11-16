// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The HTTP transport of `SoapClient::__doRequest` (`php_http.c`):
//! request bytes (headers in PHP's order, cookies, basic/digest
//! authentication, proxies, compression), response reading (status,
//! keep-alive, `Content-Length`/chunked/close-delimited bodies), cookies,
//! redirects and digest challenges. Sockets come from
//! [`SoapHost::connector`].

use php_runtime::array::ArrayKey;
use php_runtime::{ErrorLevel, Obj, RtResult, RuntimeError, Value, ZArray};

use crate::consts::*;
use crate::encoding::base64_encode;
use crate::fault::{add_soap_fault, FaultSpec};
use crate::host::{ConnectTarget, HttpStream, SoapHost};

/// A parsed URL (`php_url` / `php_uri`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Url {
    /// Scheme.
    pub scheme: Option<Vec<u8>>,
    /// User.
    pub user: Option<Vec<u8>>,
    /// Password.
    pub pass: Option<Vec<u8>>,
    /// Host.
    pub host: Option<Vec<u8>>,
    /// Port (0 when absent).
    pub port: u16,
    /// Path.
    pub path: Option<Vec<u8>>,
    /// Query.
    pub query: Option<Vec<u8>>,
    /// Fragment.
    pub fragment: Option<Vec<u8>>,
}

fn ctrl(s: &[u8]) -> Vec<u8> {
    s.iter()
        .map(|&c| if c.is_ascii_control() { b'_' } else { c })
        .collect()
}

fn strtol_prefix(s: &[u8]) -> Option<i64> {
    let digits: Vec<u8> = s.iter().copied().take_while(u8::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

/// `php_url_parse_ex2` (the legacy `parse_url` parser).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_url(input: &[u8]) -> Option<Url> {
    let mut ret = Url::default();
    let ue = input.len();
    let mut s = 0usize;
    let cspn = |from: usize, set: &[u8]| -> usize {
        input[from..]
            .iter()
            .position(|c| set.contains(c))
            .map_or(ue, |p| p + from)
    };
    enum Next {
        ParsePort(usize),
        ParseHost,
        JustPath,
    }
    let colon = input.iter().position(|&c| c == b':');
    let next = match colon {
        Some(e) if e != 0 => {
            let valid = input[..e]
                .iter()
                .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'));
            if valid {
                if e + 1 == ue {
                    ret.scheme = Some(ctrl(&input[..e]));
                    return Some(ret);
                }
                if input[e + 1] == b'/' {
                    ret.scheme = Some(ctrl(&input[..e]));
                    if e + 2 < ue && input[e + 2] == b'/' {
                        s = e + 3;
                        if input[..e].eq_ignore_ascii_case(b"file")
                            && e + 3 < ue
                            && input[e + 3] == b'/'
                        {
                            if e + 5 < ue && input[e + 5] == b':' {
                                s = e + 4;
                            }
                            Next::JustPath
                        } else {
                            Next::ParseHost
                        }
                    } else {
                        s = e + 1;
                        Next::JustPath
                    }
                } else {
                    let mut p = e + 1;
                    while p < ue && input[p].is_ascii_digit() {
                        p += 1;
                    }
                    if (p == ue || input[p] == b'/') && p - e < 7 {
                        Next::ParsePort(e)
                    } else {
                        ret.scheme = Some(ctrl(&input[..e]));
                        s = e + 1;
                        Next::JustPath
                    }
                }
            } else if e + 1 < ue && e < cspn(0, b"?#") {
                Next::ParsePort(e)
            } else if ue > 1 && input[0] == b'/' && input[1] == b'/' {
                s = 2;
                Next::ParseHost
            } else {
                Next::JustPath
            }
        }
        Some(e) => Next::ParsePort(e),
        None => {
            if ue > 1 && input[0] == b'/' && input[1] == b'/' {
                s = 2;
                Next::ParseHost
            } else {
                Next::JustPath
            }
        }
    };
    let next = match next {
        Next::ParsePort(e) => {
            let p = e + 1;
            let mut pp = p;
            while pp < ue && pp - p < 6 && input[pp].is_ascii_digit() {
                pp += 1;
            }
            if pp - p > 0 && pp - p < 6 && (pp == ue || input[pp] == b'/') {
                match strtol_prefix(&input[p..pp]) {
                    Some(port) if (0..=65535).contains(&port) => {
                        ret.port = u16::try_from(port).unwrap_or(0);
                        if s + 1 < ue && input[s] == b'/' && input[s + 1] == b'/' {
                            s += 2;
                        }
                        Next::ParseHost
                    }
                    _ => return None,
                }
            } else if p == pp && pp == ue {
                return None;
            } else if s + 1 < ue && input[s] == b'/' && input[s + 1] == b'/' {
                s += 2;
                Next::ParseHost
            } else {
                Next::JustPath
            }
        }
        other => other,
    };
    if let Next::ParseHost = next {
        let e = cspn(s, b"/?#");
        if let Some(at) = input[s..e].iter().rposition(|&c| c == b'@').map(|p| p + s) {
            if let Some(c) = input[s..at].iter().position(|&c| c == b':').map(|p| p + s) {
                ret.user = Some(ctrl(&input[s..c]));
                ret.pass = Some(ctrl(&input[c + 1..at]));
            } else {
                ret.user = Some(ctrl(&input[s..at]));
            }
            s = at + 1;
        }
        let p = if s < ue && input[s] == b'[' && e > 0 && input[e - 1] == b']' {
            None
        } else {
            input[s..e].iter().rposition(|&c| c == b':').map(|p| p + s)
        };
        let host_end = if let Some(p) = p {
            if ret.port == 0 {
                let pstart = p + 1;
                if e - pstart > 5 {
                    return None;
                } else if e > pstart {
                    match strtol_prefix(&input[pstart..e]) {
                        Some(port) if (0..=65535).contains(&port) => {
                            ret.port = u16::try_from(port).unwrap_or(0);
                        }
                        _ => return None,
                    }
                }
            }
            p
        } else {
            e
        };
        if host_end <= s {
            return None;
        }
        ret.host = Some(ctrl(&input[s..host_end]));
        if e == ue {
            return Some(ret);
        }
        s = e;
    }
    // just_path
    let mut e = ue;
    if let Some(p) = input[s..e].iter().position(|&c| c == b'#').map(|p| p + s) {
        ret.fragment = Some(ctrl(&input[p + 1..e]));
        e = p;
    }
    if let Some(p) = input[s..e].iter().position(|&c| c == b'?').map(|p| p + s) {
        ret.query = Some(ctrl(&input[p + 1..e]));
        e = p;
    }
    if s < e || s == ue {
        ret.path = Some(ctrl(&input[s..e]));
    }
    Some(ret)
}

fn md5_hex(parts: &[&[u8]]) -> String {
    let data: Vec<u8> = parts.concat();
    hex_lower(&php_hash::md5(&data))
}

fn hex_lower(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// `get_http_header_value_nodup` (case-insensitive, at line starts).
#[must_use]
pub fn header_value(headers: &[u8], name: &str) -> Option<Vec<u8>> {
    let nb = name.as_bytes();
    let mut pos = 0;
    loop {
        if headers.len() >= pos + nb.len() && headers[pos..pos + nb.len()].eq_ignore_ascii_case(nb)
        {
            let mut tmp = pos + nb.len();
            while tmp < headers.len() && (headers[tmp] == b' ' || headers[tmp] == b'\t') {
                tmp += 1;
            }
            let mut eol = headers[tmp..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(headers.len(), |p| p + tmp);
            if eol < headers.len() || eol > tmp {
                if eol > tmp && headers.get(eol - 1) == Some(&b'\r') && eol < headers.len() {
                    eol -= 1;
                }
                while eol > tmp && (headers[eol - 1] == b' ' || headers[eol - 1] == b'\t') {
                    eol -= 1;
                }
            }
            return Some(headers[tmp..eol].to_vec());
        }
        match headers[pos..].iter().position(|&c| c == b'\n') {
            Some(p) => pos += p + 1,
            None => return None,
        }
    }
}

/// All values of a repeated header (for `Set-Cookie`).
fn header_values(headers: &[u8], name: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = headers;
    while let Some(v) = header_value(rest, name) {
        // advance past this occurrence
        let nb = name.as_bytes();
        let mut pos = 0;
        let mut found = None;
        loop {
            if rest.len() >= pos + nb.len() && rest[pos..pos + nb.len()].eq_ignore_ascii_case(nb) {
                found = Some(pos);
                break;
            }
            match rest[pos..].iter().position(|&c| c == b'\n') {
                Some(p) => pos += p + 1,
                None => break,
            }
        }
        out.push(v);
        match found.and_then(|f| {
            rest[f..]
                .iter()
                .position(|&c| c == b'\n')
                .map(|p| f + p + 1)
        }) {
            Some(n) => rest = &rest[n..],
            None => break,
        }
    }
    out
}

/// `get_http_headers`: reads header lines up to the empty line.
fn read_headers(stream: &mut dyn HttpStream) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(line) = stream.read_line() {
        if line == b"\r\n"
            || line == b"\n"
            || line.starts_with(b"\r\n")
            || line.first() == Some(&b'\n')
        {
            return Some(out);
        }
        out.extend_from_slice(&line);
    }
    None
}

/// `get_http_body`.
fn read_body(stream: &mut dyn HttpStream, close: bool, headers: &[u8]) -> Option<Vec<u8>> {
    let mut header_close = close;
    if !close {
        if let Some(c) = header_value(headers, "Connection:") {
            if c.len() >= 5 && c[..5].eq_ignore_ascii_case(b"close") {
                header_close = true;
            }
        }
    }
    let chunked = header_value(headers, "Transfer-Encoding:")
        .is_some_and(|t| t.len() >= 7 && t[..7].eq_ignore_ascii_case(b"chunked"));
    let mut length = 0usize;
    if let Some(cl) = header_value(headers, "Content-Length:") {
        let s = String::from_utf8_lossy(&cl);
        let t = s.trim_start();
        let digits: String = t
            .chars()
            .enumerate()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
            .map(|(_, c)| c)
            .collect();
        let v: i64 = digits.parse().unwrap_or(0);
        if v == 0 && !chunked {
            return Some(Vec::new());
        }
        if v < 0 {
            return None;
        }
        length = usize::try_from(v).unwrap_or(0);
    }
    if chunked {
        let mut buf = Vec::new();
        loop {
            let line = stream.read_line().unwrap_or_default();
            let hex: String = String::from_utf8_lossy(&line)
                .trim_start()
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .collect();
            let Ok(size) = usize::from_str_radix(&hex, 16) else {
                return None;
            };
            if size > 0 {
                let mut read = 0;
                while read < size {
                    let chunk = stream.read(size - read);
                    if chunk.is_empty() {
                        break;
                    }
                    read += chunk.len();
                    buf.extend_from_slice(&chunk);
                }
                let mut ch = stream.read(1);
                if ch == b"\r" {
                    ch = stream.read(1);
                }
                if ch != b"\n" {
                    return None;
                }
            } else {
                break;
            }
        }
        while let Some(line) = stream.read_line() {
            if line.starts_with(b"\r\n") || line.first() == Some(&b'\n') {
                break;
            }
        }
        Some(buf)
    } else if length > 0 {
        let mut buf = Vec::with_capacity(length);
        while buf.len() < length {
            let chunk = stream.read(length - buf.len());
            if chunk.is_empty() {
                return None;
            }
            buf.extend_from_slice(&chunk);
        }
        Some(buf)
    } else if header_close {
        let mut buf = Vec::new();
        loop {
            buf.extend_from_slice(&stream.read(4096));
            if stream.eof() {
                break;
            }
        }
        Some(buf)
    } else {
        None
    }
}

fn slot(host: &mut dyn SoapHost, this: &Obj, name: &str) -> Value {
    host.read_slot(this, name)
        .map(|v| v.deref())
        .unwrap_or_default()
}

fn slot_bytes(host: &mut dyn SoapHost, this: &Obj, name: &str) -> Option<Vec<u8>> {
    match slot(host, this, name) {
        Value::Str(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

/// `soap_smart_str_append_header_value_ex`.
fn append_header_value(
    host: &mut dyn SoapHost,
    out: &mut Vec<u8>,
    v: &[u8],
    name: &str,
) -> RtResult<()> {
    let end = v
        .iter()
        .position(|&c| c == b'\r' || c == b'\n')
        .unwrap_or(v.len());
    out.extend_from_slice(&v[..end]);
    if end < v.len() {
        host.diagnostic(
            ErrorLevel::Warning,
            &format!("SoapClient::__doRequest(): Header {name} value contains newline characters and has been truncated"),
        )?;
    }
    Ok(())
}

fn http_fault(host: &mut dyn SoapHost, this: &Obj, msg: &str) -> RtResult<Option<Vec<u8>>> {
    add_soap_fault(host, this, &FaultSpec::en("HTTP", msg))?;
    Ok(None)
}

/// `http_context_headers`: appends the `http.header` context option (a
/// string, or an array whose non-empty string elements are used), minus the
/// headers the caller generates itself.
pub(crate) fn http_context_headers(
    host: &mut dyn SoapHost,
    ctx: &Value,
    has_auth: bool,
    has_proxy_auth: bool,
    has_cookies: bool,
    out: &mut Vec<u8>,
) {
    match host
        .context_get_option(ctx, "http", "header")
        .map(|v| v.deref())
    {
        Some(Value::Str(s)) if !s.is_empty() => {
            context_headers(s.as_bytes(), has_auth, has_proxy_auth, has_cookies, out);
        }
        Some(Value::Array(a)) => {
            for v in a.values() {
                if let Value::Str(s) = v.deref() {
                    if !s.is_empty() {
                        context_headers(s.as_bytes(), has_auth, has_proxy_auth, has_cookies, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `http_context_add_header`: extra headers from the stream context,
/// skipping the ones the client generates itself.
fn context_headers(
    src: &[u8],
    has_auth: bool,
    has_proxy_auth: bool,
    has_cookies: bool,
    out: &mut Vec<u8>,
) {
    let mut s = 0;
    while s < src.len() {
        while s < src.len() && matches!(src[s], b' ' | b'\t' | b'\r' | b'\n') {
            s += 1;
        }
        let mut p = s;
        let mut name_len: Option<usize> = None;
        while p < src.len() {
            if src[p] == b':' {
                if name_len.is_none() {
                    name_len = Some(p - s);
                }
                break;
            } else if src[p] == b' ' || src[p] == b'\t' {
                if name_len.is_none() {
                    name_len = Some(p - s);
                }
            } else if src[p] == b'\r' || src[p] == b'\n' {
                break;
            }
            p += 1;
        }
        if p < src.len() && src[p] == b':' {
            while p < src.len() && src[p] != b'\r' && src[p] != b'\n' {
                p += 1;
            }
            let name = &src[s..s + name_len.unwrap_or(0)];
            let is = |n: &str| name.eq_ignore_ascii_case(n.as_bytes());
            let skip = is("host")
                || is("connection")
                || is("user-agent")
                || is("content-length")
                || is("content-type")
                || (has_cookies && is("cookie"))
                || (has_auth && is("authorization"))
                || (has_proxy_auth && is("proxy-authorization"));
            if !skip {
                out.extend_from_slice(&src[s..p]);
                out.extend_from_slice(b"\r\n");
            }
        }
        s = if p < src.len() { p + 1 } else { p };
    }
}

fn is_allowed_redirect(from: &Url, to: &Url) -> bool {
    let (Some(ts), Some(th)) = (&to.scheme, &to.host) else {
        return true;
    };
    if !from
        .host
        .as_deref()
        .is_some_and(|fh| fh.eq_ignore_ascii_case(th))
    {
        return false;
    }
    let to_port = if to.port == 0 {
        if ts.as_slice() == b"https" {
            443
        } else {
            80
        }
    } else {
        to.port
    };
    if from.scheme.as_deref() == Some(ts.as_slice()) {
        return from.port == to_port;
    }
    from.scheme.as_deref() == Some(b"http".as_slice())
        && ts.as_slice() == b"https"
        && to_port == 443
}

fn in_domain(host: &[u8], domain: &[u8]) -> bool {
    if domain.first() == Some(&b'.') {
        host.ends_with(domain)
    } else {
        host == domain
    }
}

/// `make_http_soap_request`: sends `buf` to `location`; returns the
/// response body (`None` with a fault stored in `__soap_fault` on
/// failure). With `want_response == false` (one-way) the body is not read.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn make_http_soap_request(
    host: &mut dyn SoapHost,
    this: &Obj,
    buf: &[u8],
    location: &[u8],
    soapaction: &[u8],
    soap_version: i64,
    uri_parser_class: Option<&[u8]>,
    want_response: bool,
) -> RtResult<Option<Vec<u8>>> {
    if let Some(cls) = uri_parser_class {
        let known = cls.is_empty()
            || cls.eq_ignore_ascii_case(b"Uri\\Rfc3986\\Uri")
            || cls.eq_ignore_ascii_case(b"Uri\\WhatWg\\Url");
        if !known {
            return Err(RuntimeError::ValueError(
                "SoapClient::__doRequest(): Argument #6 ($uriParserClass) must be a valid URI parser name".into(),
            ));
        }
    }
    let mut request = buf.to_vec();
    let mut headers_z: Vec<u8> = Vec::new();
    if let Value::Int(c) = slot(host, this, "compression") {
        let level = (c & 0x0f).min(9);
        let kind = c & SOAP_COMPRESSION_DEFLATE;
        if c & SOAP_COMPRESSION_ACCEPT != 0 {
            headers_z.extend_from_slice(b"Accept-Encoding: gzip, deflate\r\n");
        }
        if level > 0 {
            let (func, args, hdr): (&[u8], Vec<Value>, &[u8]) = if kind == SOAP_COMPRESSION_DEFLATE
            {
                (
                    b"gzcompress",
                    vec![Value::from(buf), Value::Int(level)],
                    b"Content-Encoding: deflate\r\n",
                )
            } else {
                (
                    b"gzencode",
                    vec![Value::from(buf), Value::Int(level), Value::Int(0x1f)],
                    b"Content-Encoding: gzip\r\n",
                )
            };
            headers_z.extend_from_slice(hdr);
            match host.call_function(func, &args)? {
                Some(Value::Str(s)) => request = s.as_bytes().to_vec(),
                _ => return Ok(None),
            }
        }
    }
    let mut uri = if location.is_empty() {
        None
    } else {
        parse_url(location)
    };
    let context = match slot(host, this, "_stream_context") {
        Value::Resource(r) => Some(Value::Resource(r)),
        _ => None,
    };
    let mut redirect_max: i64 = 20;
    if let Some(ctx) = &context {
        if let Some(v) = host.context_get_option(ctx, "http", "max_redirects") {
            match v.deref() {
                Value::Int(i) => redirect_max = i,
                Value::Str(s) => {
                    if let Some(php_runtime::numeric::Number::Int(i)) =
                        php_runtime::numeric::is_numeric_string(s.as_bytes(), true)
                    {
                        redirect_max = i;
                    }
                }
                _ => {}
            }
        }
    }
    let client_trace = matches!(slot(host, this, "trace"), Value::Bool(true));
    let mut http_msg: Option<Vec<u8>> = None;
    loop {
        let Some(u) = uri.clone().filter(|u| u.host.is_some()) else {
            return http_fault(host, this, "Unable to parse URL");
        };
        let mut u = u;
        let use_ssl = match u.scheme.as_deref() {
            Some(b"https") => true,
            Some(b"http") => false,
            _ => {
                return http_fault(
                    host,
                    this,
                    "Unknown protocol. Only http and https are allowed.",
                )
            }
        };
        if u.port == 0 {
            u.port = if use_ssl { 443 } else { 80 };
        }
        let proxy_host = slot_bytes(host, this, "_proxy_host");
        let proxy_port = match slot(host, this, "_proxy_port") {
            Value::Int(p) => Some(p),
            _ => None,
        };
        let use_proxy = proxy_host.is_some() && proxy_port.is_some();
        let protocol = if use_ssl && !use_proxy {
            match slot(host, this, "_ssl_method") {
                Value::Int(SOAP_SSL_METHOD_TLS) => "tls",
                Value::Int(SOAP_SSL_METHOD_SSLV2) => "sslv2",
                Value::Int(SOAP_SSL_METHOD_SSLV3) => "sslv3",
                _ => "ssl",
            }
        } else {
            "tcp"
        };
        let host_name = String::from_utf8_lossy(u.host.as_deref().unwrap_or(b"")).into_owned();
        let (conn_host, conn_port) = match (&proxy_host, proxy_port) {
            (Some(h), Some(p)) => (
                String::from_utf8_lossy(h).into_owned(),
                u16::try_from(p).unwrap_or(0),
            ),
            _ => (host_name.clone(), u.port),
        };
        let timeout = match slot(host, this, "_connection_timeout") {
            Value::Int(t) if t > 0 => Some(t),
            _ => None,
        };
        let proxy_tunnel = (use_proxy && use_ssl).then(|| {
            let mut c = format!(
                "CONNECT {host_name}:{} HTTP/1.1\r\nHost: {host_name}",
                u.port
            )
            .into_bytes();
            if u.port != 80 {
                c.extend_from_slice(format!(":{}", u.port).as_bytes());
            }
            c.extend_from_slice(b"\r\n");
            (c, "tls".to_owned())
        });
        let target = ConnectTarget {
            protocol: protocol.to_owned(),
            host: conn_host,
            port: conn_port,
            timeout,
            proxy_tunnel,
        };
        let stream = host.connector().and_then(|c| c.connect(&target));
        let Some(mut stream) = stream else {
            return http_fault(host, this, "Could not connect to host");
        };
        host.write_slot(this, "_use_proxy", Value::Int(i64::from(use_proxy)));

        let http_1_1 = !context.as_ref().is_some_and(|ctx| {
            matches!(host.context_get_option(ctx, "http", "protocol_version").map(|v| v.deref()), Some(Value::Float(f)) if (f - 1.0).abs() < f64::EPSILON)
        });
        let mut h: Vec<u8> = b"POST ".to_vec();
        if use_proxy && !use_ssl {
            h.extend_from_slice(u.scheme.as_deref().unwrap_or(b""));
            h.extend_from_slice(b"://");
            h.extend_from_slice(host_name.as_bytes());
            h.extend_from_slice(format!(":{}", u.port).as_bytes());
        }
        h.extend_from_slice(u.path.as_deref().unwrap_or(b"/"));
        if let Some(q) = &u.query {
            h.push(b'?');
            h.extend_from_slice(q);
        }
        if let Some(f) = &u.fragment {
            h.push(b'#');
            h.extend_from_slice(f);
        }
        h.extend_from_slice(if http_1_1 {
            b" HTTP/1.1\r\n"
        } else {
            b" HTTP/1.0\r\n"
        });
        h.extend_from_slice(b"Host: ");
        h.extend_from_slice(host_name.as_bytes());
        if u.port != if use_ssl { 443 } else { 80 } {
            h.extend_from_slice(format!(":{}", u.port).as_bytes());
        }
        if !http_1_1 || matches!(slot(host, this, "_keep_alive"), Value::Bool(false)) {
            h.extend_from_slice(b"\r\nConnection: close\r\n");
        } else {
            h.extend_from_slice(b"\r\nConnection: Keep-Alive\r\n");
        }
        let ua: Option<Vec<u8>> = match slot(host, this, "_user_agent") {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            _ => context
                .as_ref()
                .and_then(|ctx| host.context_get_option(ctx, "http", "user_agent"))
                .and_then(|v| match v.deref() {
                    Value::Str(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                })
                .or_else(|| host.ini_get("user_agent").filter(|u| !u.is_empty())),
        };
        match ua {
            Some(ua) => {
                if !ua.is_empty() {
                    h.extend_from_slice(b"User-Agent: ");
                    append_header_value(host, &mut h, &ua, "User-Agent")?;
                    h.extend_from_slice(b"\r\n");
                }
            }
            None => h.extend_from_slice(
                format!("User-Agent: PHP-SOAP/{}\r\n", crate::PHP_VERSION).as_bytes(),
            ),
        }
        h.extend_from_slice(&headers_z);
        let ctx_ct = context
            .as_ref()
            .and_then(|ctx| host.context_get_option(ctx, "http", "content_type"))
            .and_then(|v| match v.deref() {
                Value::Str(s) if !s.is_empty() => Some(s.as_bytes().to_vec()),
                _ => None,
            });
        if soap_version == SOAP_1_2 {
            match &ctx_ct {
                Some(ct) => {
                    h.extend_from_slice(b"Content-Type: ");
                    append_header_value(host, &mut h, ct, "Content-Type")?;
                }
                None => h.extend_from_slice(b"Content-Type: application/soap+xml; charset=utf-8"),
            }
            h.extend_from_slice(b"; action=\"");
            append_header_value(host, &mut h, soapaction, "SOAPAction")?;
            h.extend_from_slice(b"\"\r\n");
        } else {
            match &ctx_ct {
                Some(ct) => {
                    h.extend_from_slice(b"Content-Type: ");
                    append_header_value(host, &mut h, ct, "Content-Type")?;
                    h.extend_from_slice(b"\r\n");
                }
                None => h.extend_from_slice(b"Content-Type: text/xml; charset=utf-8\r\n"),
            }
            h.extend_from_slice(b"SOAPAction: \"");
            append_header_value(host, &mut h, soapaction, "SOAPAction")?;
            h.extend_from_slice(b"\"\r\n");
        }
        h.extend_from_slice(format!("Content-Length: {}\r\n", request.len()).as_bytes());

        let mut has_auth = false;
        if let Some(login) = slot_bytes(host, this, "_login") {
            has_auth = true;
            if let Value::Array(digest) = slot(host, this, "_digest") {
                let mut digest = digest;
                let nonce = host.random_bytes(16)?;
                let cnonce: String = hex_lower(&nonce);
                let nc = if let Some(Value::Int(n)) = digest.get_str(b"nc").map(Value::deref) {
                    let n = n + 1;
                    digest
                        .make_mut()
                        .insert(ArrayKey::from_bytes(b"nc"), Value::Int(n));
                    format!("{n:08}")
                } else {
                    digest
                        .make_mut()
                        .insert(ArrayKey::from_bytes(b"nc"), Value::Int(1));
                    "00000001".to_owned()
                };
                host.write_slot(this, "_digest", Value::Array(digest.clone()));
                let dstr = |k: &str| match digest.get_str(k.as_bytes()).map(Value::deref) {
                    Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                    _ => None,
                };
                let password = slot_bytes(host, this, "_password").unwrap_or_default();
                let realm = dstr("realm").unwrap_or_default();
                let mut ha1 = md5_hex(&[&login, b":", &realm, b":", &password]);
                if dstr("algorithm").is_some_and(|a| a.eq_ignore_ascii_case(b"md5-sess")) {
                    let nonce_v = dstr("nonce").unwrap_or_default();
                    ha1 = md5_hex(&[
                        ha1.as_bytes(),
                        b":",
                        &nonce_v,
                        b":",
                        &cnonce.as_bytes()[..8],
                    ]);
                }
                let mut a2 = b"POST:".to_vec();
                a2.extend_from_slice(u.path.as_deref().unwrap_or(b"/"));
                if let Some(q) = &u.query {
                    a2.push(b'?');
                    a2.extend_from_slice(q);
                }
                let ha2 = md5_hex(&[&a2]);
                let nonce_v = dstr("nonce").unwrap_or_default();
                let response = if dstr("qop").is_some() {
                    md5_hex(&[
                        ha1.as_bytes(),
                        b":",
                        &nonce_v,
                        b":",
                        nc.as_bytes(),
                        b":",
                        &cnonce.as_bytes()[..8],
                        b":auth:",
                        ha2.as_bytes(),
                    ])
                } else {
                    md5_hex(&[ha1.as_bytes(), b":", &nonce_v, b":", ha2.as_bytes()])
                };
                h.extend_from_slice(b"Authorization: Digest username=\"");
                h.extend_from_slice(&login);
                if let Some(r) = dstr("realm") {
                    h.extend_from_slice(b"\", realm=\"");
                    h.extend_from_slice(&r);
                }
                if let Some(n) = dstr("nonce") {
                    h.extend_from_slice(b"\", nonce=\"");
                    h.extend_from_slice(&n);
                }
                h.extend_from_slice(b"\", uri=\"");
                h.extend_from_slice(u.path.as_deref().unwrap_or(b"/"));
                if let Some(q) = &u.query {
                    h.push(b'?');
                    h.extend_from_slice(q);
                }
                if let Some(f) = &u.fragment {
                    h.push(b'#');
                    h.extend_from_slice(f);
                }
                if dstr("qop").is_some() {
                    h.extend_from_slice(b"\", qop=auth, nc=");
                    h.extend_from_slice(nc.as_bytes());
                    h.extend_from_slice(b", cnonce=\"");
                    h.extend_from_slice(&cnonce.as_bytes()[..8]);
                }
                h.extend_from_slice(b"\", response=\"");
                h.extend_from_slice(response.as_bytes());
                if let Some(o) = dstr("opaque") {
                    h.extend_from_slice(b"\", opaque=\"");
                    h.extend_from_slice(&o);
                }
                if let Some(a) = dstr("algorithm") {
                    h.extend_from_slice(b"\", algorithm=\"");
                    h.extend_from_slice(&a);
                }
                h.extend_from_slice(b"\"\r\n");
            } else {
                let mut auth = login.clone();
                auth.push(b':');
                if let Some(p) = slot_bytes(host, this, "_password") {
                    auth.extend_from_slice(&p);
                }
                h.extend_from_slice(b"Authorization: Basic ");
                h.extend_from_slice(&base64_encode(&auth));
                h.extend_from_slice(b"\r\n");
            }
        }
        let mut has_proxy_auth = false;
        if use_proxy && !use_ssl {
            if let Some(pl) = slot_bytes(host, this, "_proxy_login") {
                let mut auth = pl;
                auth.push(b':');
                if let Some(p) = slot_bytes(host, this, "_proxy_password") {
                    auth.extend_from_slice(&p);
                }
                h.extend_from_slice(b"Proxy-Authorization: Basic ");
                h.extend_from_slice(&base64_encode(&auth));
                h.extend_from_slice(b"\r\n");
                has_proxy_auth = true;
            }
        }
        let mut has_cookies = false;
        if let Value::Array(cookies) = slot(host, this, "_cookies") {
            if !cookies.is_empty() && !cookies.is_packed() {
                has_cookies = true;
                h.extend_from_slice(b"Cookie: ");
                let mut first = true;
                let path = u.path.clone().unwrap_or_else(|| b"/".to_vec());
                for (k, data) in cookies.iter() {
                    let ArrayKey::Str(key) = k else {
                        continue;
                    };
                    let Value::Array(data) = data.deref() else {
                        continue;
                    };
                    let Some(Value::Str(value)) = data.get_int(0).map(Value::deref) else {
                        continue;
                    };
                    let path_ok = match data.get_int(1).map(Value::deref) {
                        Some(Value::Str(p)) => path.starts_with(p.as_bytes()),
                        _ => true,
                    };
                    let domain_ok = match data.get_int(2).map(Value::deref) {
                        Some(Value::Str(d)) => in_domain(host_name.as_bytes(), d.as_bytes()),
                        _ => true,
                    };
                    let secure_ok = use_ssl || data.get_int(3).is_none();
                    if path_ok && domain_ok && secure_ok {
                        if !first {
                            h.extend_from_slice(b"; ");
                        }
                        first = false;
                        append_header_value(host, &mut h, key.as_bytes(), "Cookie")?;
                        h.push(b'=');
                        append_header_value(host, &mut h, value.as_bytes(), "Cookie")?;
                    }
                }
                h.extend_from_slice(b"\r\n");
            }
        }
        if let Some(ctx) = &context {
            http_context_headers(host, ctx, has_auth, has_proxy_auth, has_cookies, &mut h);
        }
        h.extend_from_slice(b"\r\n");
        if client_trace {
            host.write_slot(this, "__last_request_headers", Value::from(h.as_slice()));
        }
        h.extend_from_slice(&request);
        if stream.write(&h) != h.len() {
            return http_fault(host, this, "Failed Sending HTTP SOAP request");
        }

        if !want_response && !client_trace {
            return Ok(Some(Vec::new()));
        }
        let mut http_status;
        let mut resp_1_1;
        let http_headers = loop {
            let Some(hh) = read_headers(stream.as_mut()) else {
                return http_fault(host, this, "Error Fetching http headers");
            };
            if client_trace {
                host.write_slot(this, "__last_response_headers", Value::from(hh.as_slice()));
            }
            resp_1_1 = false;
            http_status = 0;
            if let Some(v) = header_value(&hh, "HTTP/") {
                if v.starts_with(b"1.1") {
                    resp_1_1 = true;
                }
                if let Some(sp) = v.iter().position(|&c| c == b' ') {
                    let rest = &v[sp + 1..];
                    http_status = strtol_prefix(rest).unwrap_or(0);
                    if let Some(sp2) = rest.iter().position(|&c| c == b' ') {
                        http_msg = Some(rest[sp2 + 1..].to_vec());
                    }
                }
            }
            if http_status != 100 {
                break hh;
            }
        };
        if !want_response {
            return Ok(Some(Vec::new()));
        }
        for cookie in header_values(&http_headers, "Set-Cookie:") {
            let eq = cookie.iter().position(|&c| c == b'=');
            let sem = cookie.iter().position(|&c| c == b';');
            let Some(eq) = eq.filter(|&e| sem.is_none_or(|s| s > e)) else {
                continue;
            };
            let name = cookie[..eq].to_vec();
            let value_end = sem.unwrap_or(cookie.len());
            let mut zc = ZArray::new();
            zc.make_mut()
                .insert(0, Value::from(&cookie[eq + 1..value_end]));
            if let Some(s) = sem {
                let mut opts = &cookie[s + 1..];
                loop {
                    while opts.first() == Some(&b' ') {
                        opts = &opts[1..];
                    }
                    let next = opts.iter().position(|&c| c == b';');
                    let part = &opts[..next.unwrap_or(opts.len())];
                    if let Some(p) = part.strip_prefix(b"path=") {
                        zc.make_mut().insert(1, Value::from(p));
                    } else if let Some(d) = part.strip_prefix(b"domain=") {
                        zc.make_mut().insert(2, Value::from(d));
                    } else if part.starts_with(b"secure") {
                        zc.make_mut().insert(3, Value::Bool(true));
                    }
                    match next {
                        Some(n) => opts = &opts[n + 1..],
                        None => break,
                    }
                }
            }
            if zc.get_int(1).is_none() {
                let t = u.path.clone().unwrap_or_else(|| b"/".to_vec());
                if let Some(c) = t.iter().rposition(|&c| c == b'/') {
                    zc.make_mut().insert(1, Value::from(&t[..c]));
                }
            }
            if zc.get_int(2).is_none() {
                zc.make_mut().insert(2, Value::from(host_name.as_bytes()));
            }
            let mut cookies = match slot(host, this, "_cookies") {
                Value::Array(a) => a,
                _ => ZArray::new(),
            };
            cookies
                .make_mut()
                .insert(ArrayKey::from_bytes(&name), Value::Array(zc));
            host.write_slot(this, "_cookies", Value::Array(cookies));
        }
        let connection_is = |name: &str, what: &str| {
            header_value(&http_headers, name).is_some_and(|c| {
                c.len() >= what.len() && c[..what.len()].eq_ignore_ascii_case(what.as_bytes())
            })
        };
        let http_close = if resp_1_1 {
            (use_proxy && !use_ssl && connection_is("Proxy-Connection:", "close"))
                || connection_is("Connection:", "close")
        } else {
            !((use_proxy && !use_ssl && connection_is("Proxy-Connection:", "Keep-Alive"))
                || connection_is("Connection:", "Keep-Alive"))
        };
        let Some(body) = read_body(stream.as_mut(), http_close, &http_headers) else {
            return http_fault(
                host,
                this,
                "Error Fetching http body, No Content-Length, connection closed or chunked data",
            );
        };
        if (300..400).contains(&http_status) {
            if let Some(loc) = header_value(&http_headers, "Location:") {
                if let Some(mut new_uri) = parse_url(&loc) {
                    if new_uri.scheme.is_none() && new_uri.path.is_some() {
                        new_uri.scheme.clone_from(&u.scheme);
                        if new_uri.host.is_none() {
                            new_uri.host.clone_from(&u.host);
                            new_uri.port = u.port;
                        }
                        if let Some(np) = new_uri.path.clone() {
                            if np.first() != Some(&b'/') {
                                let mut p = match &u.path {
                                    Some(op) => match op.iter().rposition(|&c| c == b'/') {
                                        Some(i) => op[..=i].to_vec(),
                                        None => Vec::new(),
                                    },
                                    None => b"/".to_vec(),
                                };
                                p.extend_from_slice(&np);
                                new_uri.path = Some(p);
                            }
                        }
                    }
                    if !matches!(
                        slot(host, this, "_follow_cross_origin_redirects"),
                        Value::Bool(true)
                    ) && !is_allowed_redirect(&u, &new_uri)
                    {
                        return http_fault(
                            host,
                            this,
                            "Redirection to a different origin is not allowed",
                        );
                    }
                    redirect_max -= 1;
                    if redirect_max < 1 {
                        return http_fault(host, this, "Redirection limit reached, aborting");
                    }
                    uri = Some(new_uri);
                    continue;
                }
            }
        } else if http_status == 401 {
            let auth = header_value(&http_headers, "WWW-Authenticate:");
            let has_digest = matches!(slot(host, this, "_digest"), Value::Array(_));
            let login = slot_bytes(host, this, "_login");
            let password = slot_bytes(host, this, "_password");
            if let (Some(auth), false, Some(_), Some(_)) = (auth, has_digest, login, password) {
                if let Some(rest) = auth.strip_prefix(b"Digest") {
                    let mut digest = ZArray::new();
                    let mut any = false;
                    let s = rest;
                    let mut i = 0;
                    while i < s.len() {
                        while i < s.len() && s[i] == b' ' {
                            i += 1;
                        }
                        let ns = i;
                        while i < s.len() && s[i] != b'=' {
                            i += 1;
                        }
                        if i >= s.len() {
                            break;
                        }
                        let name = s[ns..i].to_vec();
                        i += 1;
                        let val;
                        if s.get(i) == Some(&b'"') {
                            i += 1;
                            let vs = i;
                            while i < s.len() && s[i] != b'"' {
                                i += 1;
                            }
                            val = s[vs..i].to_vec();
                        } else {
                            let vs = i;
                            while i < s.len() && s[i] != b' ' && s[i] != b',' {
                                i += 1;
                            }
                            val = s[vs..i].to_vec();
                        }
                        if i < s.len() {
                            if s[i] == b',' {
                                i += 1;
                            } else {
                                i += 1;
                                while i < s.len() && s[i] != b',' {
                                    i += 1;
                                }
                                if i < s.len() {
                                    i += 1;
                                }
                            }
                        }
                        digest
                            .make_mut()
                            .insert(ArrayKey::from_bytes(&name), Value::from(val.as_slice()));
                        any = true;
                    }
                    if any {
                        host.write_slot(this, "_digest", Value::Array(digest));
                        uri = Some(u);
                        continue;
                    }
                }
            }
        }
        let content_type_xml = header_value(&http_headers, "Content-Type:").is_some_and(|ct| {
            let cmp = &ct[..ct.iter().position(|&c| c == b';').unwrap_or(ct.len())];
            b"text/xml".starts_with(cmp) || b"application/soap+xml".starts_with(cmp)
        });
        let mut ret = body;
        if let Some(ce) = header_value(&http_headers, "Content-Encoding:") {
            let func: &[u8] = match ce.as_slice() {
                b"gzip" | b"x-gzip" if host.function_exists(b"gzdecode").is_some() => b"gzdecode",
                b"deflate" if host.function_exists(b"gzuncompress").is_some() => b"gzuncompress",
                _ => return http_fault(host, this, "Unknown Content-Encoding"),
            };
            match host.call_function(func, &[Value::from(ret.as_slice())])? {
                Some(Value::Str(s)) => ret = s.as_bytes().to_vec(),
                _ => return http_fault(host, this, "Can't uncompress compressed response"),
            }
        }
        if http_status >= 400 {
            let error = if ret.is_empty() {
                true
            } else if !content_type_xml {
                let start = ret
                    .iter()
                    .position(|&c| c >= b' ' || c == 0)
                    .unwrap_or(ret.len());
                !ret[start..].starts_with(b"<?xml")
            } else {
                false
            };
            if error {
                let msg = String::from_utf8_lossy(http_msg.as_deref().unwrap_or(b"")).into_owned();
                return http_fault(host, this, &msg);
            }
        }
        return Ok(Some(ret));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        assert!(parse_url(b"test://").is_none());
        let u = parse_url(b"http://u:p@h:8080/a/b?x=1#f").unwrap();
        assert_eq!(u.host.as_deref(), Some(&b"h"[..]));
        assert_eq!(u.port, 8080);
        assert_eq!(u.user.as_deref(), Some(&b"u"[..]));
        assert_eq!(u.path.as_deref(), Some(&b"/a/b"[..]));
        assert_eq!(u.query.as_deref(), Some(&b"x=1"[..]));
        let u = parse_url(b"h:80/p").unwrap();
        assert_eq!((u.host.as_deref(), u.port), (Some(&b"h"[..]), 80));
        let u = parse_url(b"http://[::1]:81/x").unwrap();
        assert_eq!((u.host.as_deref(), u.port), (Some(&b"[::1]"[..]), 81));
        assert!(parse_url(b"httpx://").is_none());
    }

    #[test]
    fn header_lookup() {
        let h = b"HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nset-cookie: a=1\r\nSet-Cookie: b=2; path=/x\r\n";
        assert_eq!(header_value(h, "content-type:").unwrap(), b"text/xml");
        assert_eq!(header_value(h, "HTTP/").unwrap(), b"1.1 200 OK");
        assert_eq!(
            header_values(h, "Set-Cookie:"),
            vec![b"a=1".to_vec(), b"b=2; path=/x".to_vec()]
        );
    }

    struct MemStream(Vec<u8>, usize);

    impl HttpStream for MemStream {
        fn write(&mut self, data: &[u8]) -> usize {
            data.len()
        }
        fn read_line(&mut self) -> Option<Vec<u8>> {
            if self.1 >= self.0.len() {
                return None;
            }
            let end = self.0[self.1..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(self.0.len(), |p| self.1 + p + 1);
            let line = self.0[self.1..end].to_vec();
            self.1 = end;
            Some(line)
        }
        fn read(&mut self, n: usize) -> Vec<u8> {
            let end = (self.1 + n).min(self.0.len());
            let out = self.0[self.1..end].to_vec();
            self.1 = end;
            out
        }
        fn eof(&mut self) -> bool {
            self.1 >= self.0.len()
        }
    }

    #[test]
    fn response_framing() {
        let mut s = MemStream(b"HTTP/1.1 200 OK\r\nA: b\r\n\r\nrest".to_vec(), 0);
        assert_eq!(
            read_headers(&mut s).unwrap(),
            b"HTTP/1.1 200 OK\r\nA: b\r\n"
        );
        let chunked = b"Transfer-Encoding: chunked\r\n";
        let mut s = MemStream(b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n".to_vec(), 0);
        assert_eq!(read_body(&mut s, false, chunked).unwrap(), b"Wikipedia");
        let mut s = MemStream(b"hello world".to_vec(), 0);
        assert_eq!(
            read_body(&mut s, false, b"Content-Length: 5\r\n").unwrap(),
            b"hello"
        );
        let mut s = MemStream(b"abc".to_vec(), 0);
        assert!(read_body(&mut s, false, b"Content-Length: 5\r\n").is_none());
        let mut s = MemStream(b"until close".to_vec(), 0);
        assert_eq!(read_body(&mut s, true, b"").unwrap(), b"until close");
    }

    #[test]
    fn context_header_filtering() {
        let mut out = Vec::new();
        context_headers(
            b"Host: x\r\nX-A: 1\r\n  user-agent: y\nAuthorization: z\r\nCookie: c\r\nbogus\r\n",
            true,
            false,
            false,
            &mut out,
        );
        assert_eq!(out, b"X-A: 1\r\nCookie: c\r\n");
        let mut out = Vec::new();
        context_headers(
            b"Authorization: z\r\nCookie: c",
            false,
            false,
            true,
            &mut out,
        );
        assert_eq!(out, b"Authorization: z\r\n");
    }

    #[test]
    fn redirect_origin_rules() {
        let from = parse_url(b"http://example.com:8080/svc").unwrap();
        let ok = |to: &[u8]| is_allowed_redirect(&from, &parse_url(to).unwrap());
        assert!(ok(b"http://EXAMPLE.com:8080/other"));
        assert!(ok(b"/relative"));
        assert!(!ok(b"http://example.com:8081/x"));
        assert!(!ok(b"http://other.com:8080/x"));
        assert!(!ok(b"https://example.com:8080/x"));
        let plain = parse_url(b"http://example.com/svc").unwrap();
        assert!(is_allowed_redirect(
            &plain,
            &parse_url(b"https://example.com/x").unwrap()
        ));
        assert!(in_domain(b"a.example.com", b".example.com"));
        assert!(!in_domain(b"a.example.com", b"example.com"));
        assert!(in_domain(b"example.com", b"example.com"));
    }

    #[test]
    fn digest_helpers() {
        assert_eq!(hex_lower(&[0x00, 0xab, 0x10]), "00ab10");
        assert_eq!(md5_hex(&[b"a", b"bc"]), "900150983cd24fb0d6963f7d28e17f72");
    }
}
