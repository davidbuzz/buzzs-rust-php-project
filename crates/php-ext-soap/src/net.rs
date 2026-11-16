// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The HTTP transport's network layer: `php-soap`'s [`HttpStream`] over
//! socket streams opened with `stream_socket_client()` (so every transport
//! registered with php-ext-streams — `tcp`, `ssl`, `tls`, … — is
//! available).

use std::rc::Rc;

use php_ext_streams::res::{cell_of, StreamCell};
use php_runtime::{PhpRef, Value};
use php_soap::host::{ConnectTarget, HttpStream};
use php_vm::Vm;

/// The largest single read.
const READ_CHUNK: usize = 64 * 1024;

/// A connected socket stream.
struct SocketStream {
    /// The stream resource (kept alive while the connection is used).
    _resource: Value,
    cell: Rc<StreamCell>,
    /// Response framing seen so far (see [`SocketStream::sniff`]).
    in_headers: bool,
    chunked: bool,
}

impl SocketStream {
    /// Watches the response framing for sizes the peer announces: PHP
    /// allocates the body (`Content-Length`) or chunk buffer up front, so a
    /// size beyond `memory_limit` is the memory-limit fatal, not a huge
    /// allocation. Returns `false` when the line must not be handed on.
    fn sniff(&mut self, line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line);
        let t = text.trim();
        let announced = if t.starts_with("HTTP/") {
            self.in_headers = true;
            self.chunked = false;
            None
        } else if self.in_headers {
            if t.is_empty() {
                self.in_headers = false;
                None
            } else if let Some((n, v)) = t.split_once(':') {
                let v = v.trim();
                if n.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
                {
                    self.chunked = true;
                    None
                } else if n.eq_ignore_ascii_case("content-length") {
                    v.parse::<usize>().ok()
                } else {
                    None
                }
            } else {
                None
            }
        } else if self.chunked {
            let hex: String = t.chars().take_while(char::is_ascii_hexdigit).collect();
            usize::from_str_radix(&hex, 16).ok()
        } else {
            None
        };
        match announced {
            Some(n) if !php_memlimit::would_fit(n) => {
                let _ = php_memlimit::try_reserve(n.saturating_add(25));
                !out_of_memory()
            }
            _ => true,
        }
    }
}

impl HttpStream for SocketStream {
    fn write(&mut self, data: &[u8]) -> usize {
        let Some(mut s) = self.cell.get() else {
            return 0;
        };
        let mut done = 0;
        while done < data.len() {
            match s.write(&data[done..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => done += n,
            }
        }
        done
    }

    fn read_line(&mut self) -> Option<Vec<u8>> {
        if out_of_memory() {
            return None;
        }
        let line = self.cell.get()?.get_line(None)?;
        self.sniff(&line).then_some(line)
    }

    fn read(&mut self, n: usize) -> Vec<u8> {
        if out_of_memory() {
            return Vec::new();
        }
        // Short reads are allowed: never pre-allocate a peer-controlled size.
        let n = n.min(READ_CHUNK);
        self.cell
            .get()
            .and_then(|mut s| s.read_vec(n))
            .unwrap_or_default()
    }

    fn eof(&mut self) -> bool {
        if out_of_memory() {
            return true;
        }
        self.cell.get().is_none_or(|mut s| s.eof())
    }
}

/// Whether `memory_limit` was crossed while reading: the response is cut
/// short and the fatal is raised when the library returns.
fn out_of_memory() -> bool {
    // A response buffer grows by doubling: stop before the next growth
    // could cross the hard ceiling (2x the limit).
    let usage = php_memlimit::usage();
    if !php_memlimit::is_pending() && !php_memlimit::would_fit(usage / 2) {
        let _ = php_memlimit::try_reserve(usage);
    }
    if php_memlimit::is_pending() {
        if let Some(over) = php_memlimit::take() {
            let _ = crate::host::oom_error(over.to_string());
        }
        return true;
    }
    false
}

/// `php_stream_xport_create` for a SOAP connection (`http_connect`).
pub(crate) fn connect(vm: &mut Vm, t: &ConnectTarget) -> Option<Box<dyn HttpStream>> {
    let proto = if t.proxy_tunnel.is_some() {
        "tcp"
    } else {
        t.protocol.as_str()
    };
    let host = if t.host.contains(':') && !t.host.starts_with('[') {
        format!("[{}]", t.host)
    } else {
        t.host.clone()
    };
    let addr = format!("{proto}://{host}:{}", t.port);
    #[allow(clippy::cast_precision_loss)]
    let timeout = t.timeout.map_or_else(
        || vm.ini_long("default_socket_timeout") as f64,
        |s| s as f64,
    );
    let errno = Value::Reference(PhpRef::new(Value::Null));
    let errstr = Value::Reference(PhpRef::new(Value::Null));
    let r = crate::host::silenced(vm, |vm| {
        vm.call_value(
            &Value::from("stream_socket_client"),
            vec![
                Value::from(addr.as_str()),
                errno,
                errstr,
                Value::Float(timeout),
            ],
        )
    });
    let Ok(res @ Value::Resource(_)) = r else {
        return None;
    };
    let Value::Resource(r) = &res else {
        return None;
    };
    let cell = cell_of(r)?;
    let mut stream = SocketStream {
        _resource: res.clone(),
        cell,
        in_headers: false,
        chunked: false,
    };
    if let Some((request, _method)) = &t.proxy_tunnel {
        let mut req = request.clone();
        req.extend_from_slice(b"\r\n");
        if stream.write(&req) != req.len() {
            return None;
        }
        let status = stream.read_line()?;
        let ok = status.starts_with(b"HTTP/1.") && status.get(9..12) == Some(b"200");
        loop {
            match stream.read_line() {
                Some(l) if l != b"\r\n" && l != b"\n" => {}
                _ => break,
            }
        }
        if !ok {
            return None;
        }
        let method = vm
            .constants
            .get(b"STREAM_CRYPTO_METHOD_TLS_CLIENT".as_slice())
            .cloned()
            .unwrap_or(Value::Int(121));
        let enabled = crate::host::silenced(vm, |vm| {
            vm.call_value(
                &Value::from("stream_socket_enable_crypto"),
                vec![res.clone(), Value::Bool(true), method],
            )
        });
        if !matches!(enabled, Ok(Value::Bool(true))) {
            return None;
        }
    }
    Some(Box::new(stream))
}

/// Whether a stream wrapper is registered for `scheme`.
pub(crate) fn has_wrapper(vm: &mut Vm, scheme: &str) -> bool {
    match vm.call_function("stream_get_wrappers", Vec::new()) {
        Ok(Value::Array(a)) => a.values().any(|v| {
            matches!(v.deref(), Value::Str(s) if s.as_bytes().eq_ignore_ascii_case(scheme.as_bytes()))
        }),
        _ => false,
    }
}

/// A `http` context option.
fn ctx_option(vm: &mut Vm, context: Option<&Value>, name: &str) -> Option<Value> {
    let ctx = context?;
    let Ok(Value::Array(opts)) = vm.call_value(
        &Value::from("stream_context_get_options"),
        vec![ctx.clone()],
    ) else {
        return None;
    };
    match opts.get_str(b"http").map(Value::deref) {
        Some(Value::Array(w)) => w.get_str(name.as_bytes()).map(Value::deref),
        _ => None,
    }
}

/// The `header` context option flattened to one string (array entries
/// joined with CRLF), trimmed like
/// `php_stream_url_wrap_http_ex` does.
fn user_headers(vm: &mut Vm, context: Option<&Value>) -> Vec<u8> {
    let mut out = Vec::new();
    // Only strings count: objects (even stringable ones) are ignored.
    match ctx_option(vm, context, "header") {
        Some(Value::Array(a)) => {
            for v in a.values() {
                if let Value::Str(s) = v.deref() {
                    out.extend_from_slice(s.as_bytes());
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
        Some(Value::Str(s)) => out.extend_from_slice(s.as_bytes()),
        _ => {}
    }
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'\0' | 0x0b);
    let start = out.iter().position(|c| !ws(c)).unwrap_or(out.len());
    let end = out.iter().rposition(|c| !ws(c)).map_or(start, |e| e + 1);
    out[start..end].to_vec()
}

/// Whether the user headers contain `name:` at the start of a line.
fn has_header(headers: &[u8], name: &str) -> bool {
    headers.split(|&c| c == b'\n').any(|l| {
        let l: Vec<u8> = l
            .iter()
            .skip_while(|c| c.is_ascii_whitespace())
            .copied()
            .collect();
        l.len() > name.len()
            && l[..name.len()].eq_ignore_ascii_case(name.as_bytes())
            && l[name.len()] == b':'
    })
}

/// A minimal `http://`/`https://` GET (the request layout of PHP's http
/// stream wrapper) for loading WSDL/XSD documents when no `http` wrapper
/// is registered. `None` on any failure or a non-2xx status.
pub(crate) fn http_get(vm: &mut Vm, url: &str, context: Option<&Value>) -> Option<Vec<u8>> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        (true, url.strip_prefix("https://")?)
    };
    let (authority, path) = rest.find(['/', '?']).map_or((rest, "/".to_owned()), |p| {
        let path = &rest[p..];
        let path = if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_owned()
        };
        (&rest[..p], path)
    });
    let path = path.split('#').next().unwrap_or("/").to_owned();
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let default_port = if tls { 443 } else { 80 };
    let (host_name, port_no) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => {
            (h.trim_matches(['[', ']']).to_owned(), p.parse().ok()?)
        }
        _ => (authority.trim_matches(['[', ']']).to_owned(), default_port),
    };
    let protocol = match ctx_option(vm, context, "protocol_version") {
        Some(Value::Float(f)) => format!("{f:.1}"),
        Some(Value::Int(i)) => format!("{i}.0"),
        Some(Value::Str(s)) => s.to_string_lossy(),
        _ => "1.1".to_owned(),
    };
    let user = user_headers(vm, context);
    let mut req = format!("GET {path} HTTP/{protocol}\r\n").into_bytes();
    if !has_header(&user, "host") {
        if port_no == default_port {
            req.extend_from_slice(format!("Host: {host_name}\r\n").as_bytes());
        } else {
            req.extend_from_slice(format!("Host: {host_name}:{port_no}\r\n").as_bytes());
        }
    }
    if !has_header(&user, "connection") {
        req.extend_from_slice(b"Connection: close\r\n");
    }
    if !user.is_empty() {
        req.extend_from_slice(&user);
        req.extend_from_slice(b"\r\n");
    }
    req.extend_from_slice(b"\r\n");
    let target = ConnectTarget {
        protocol: if tls { "ssl" } else { "tcp" }.to_owned(),
        host: host_name,
        port: port_no,
        timeout: None,
        proxy_tunnel: None,
    };
    let mut s = connect(vm, &target)?;
    if s.write(&req) != req.len() {
        return None;
    }
    let status = s.read_line()?;
    let code = status.split(|&c| c == b' ').nth(1)?;
    if !code.starts_with(b"2") {
        return None;
    }
    let mut length = None;
    let mut chunked = false;
    loop {
        let l = s.read_line()?;
        let t = String::from_utf8_lossy(&l).trim().to_owned();
        if t.is_empty() {
            break;
        }
        if let Some((n, v)) = t.split_once(':') {
            let v = v.trim();
            if n.eq_ignore_ascii_case("content-length") {
                length = v.parse::<usize>().ok();
            } else if n.eq_ignore_ascii_case("transfer-encoding")
                && v.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    read_body(s.as_mut(), chunked, length)
}

/// Reads a response body framed by `Transfer-Encoding: chunked`,
/// `Content-Length` or the end of the connection.
fn read_body(s: &mut dyn HttpStream, chunked: bool, length: Option<usize>) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    if chunked {
        loop {
            let l = s.read_line()?;
            let t = String::from_utf8_lossy(&l);
            let size = usize::from_str_radix(t.trim().split(';').next()?.trim(), 16).ok()?;
            if size == 0 {
                break;
            }
            let mut left = size;
            while left > 0 {
                let part = s.read(left);
                if part.is_empty() {
                    return Some(body);
                }
                left -= part.len();
                body.extend_from_slice(&part);
            }
            s.read_line();
        }
    } else if let Some(n) = length {
        while body.len() < n {
            let part = s.read(n - body.len());
            if part.is_empty() {
                break;
            }
            body.extend_from_slice(&part);
        }
    } else {
        while !s.eof() {
            let part = s.read(8192);
            if part.is_empty() {
                break;
            }
            body.extend_from_slice(&part);
        }
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_detection_is_line_anchored_and_case_insensitive() {
        let h = b"X-Foo: 1\r\n  host: example.org\r\nConnection-Thing: x";
        assert!(has_header(h, "host"));
        assert!(has_header(h, "HOST"));
        assert!(!has_header(h, "connection"));
        assert!(!has_header(b"", "host"));
        assert!(!has_header(b"hostname: x", "host"));
    }
}
