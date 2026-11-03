//! `get_sdl`: WSDL loading through the engine's stream layer with the
//! in-memory cache (`soap.wsdl_cache*` INI settings).
//!
//! The on-disk cache of PHP (`wsdl_cache_*` files in `soap.wsdl_cache_dir`)
//! is not implemented; `WSDL_CACHE_DISK` behaves like `WSDL_CACHE_NONE`.

use std::rc::Rc;

use php_runtime::{Obj, Value};

use crate::consts::*;
use crate::error::{SoapError, SoapResult};
use crate::globals;
use crate::host::SoapHost;
use crate::sdl::Sdl;
use crate::wsdl::{load_wsdl, ResourceLoader};

/// INI entries of the extension: `(name, default)`.
pub const INI_ENTRIES: &[(&str, &str)] = &[
    ("soap.wsdl_cache_enabled", "1"),
    ("soap.wsdl_cache_dir", "/tmp"),
    ("soap.wsdl_cache_ttl", "86400"),
    ("soap.wsdl_cache", "1"),
    ("soap.wsdl_cache_limit", "5"),
];

fn ini_long(host: &mut dyn SoapHost, name: &str) -> i64 {
    let v = host
        .ini_get(name)
        .or_else(|| {
            INI_ENTRIES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d.as_bytes().to_vec())
        })
        .unwrap_or_default();
    let s = String::from_utf8_lossy(&v);
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" => 1,
        "off" | "no" | "false" | "none" | "" => 0,
        _ => {
            let digits: String = t
                .chars()
                .enumerate()
                .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
                .map(|(_, c)| c)
                .collect();
            digits.parse().unwrap_or(0)
        }
    }
}

/// The effective default `cache_wsdl` mode
/// (`soap.wsdl_cache_enabled ? soap.wsdl_cache : 0`).
pub fn default_cache_mode(host: &mut dyn SoapHost) -> i64 {
    if ini_long(host, "soap.wsdl_cache_enabled") != 0 {
        ini_long(host, "soap.wsdl_cache")
    } else {
        0
    }
}

struct HostLoader<'a> {
    host: &'a mut dyn SoapHost,
    context: Option<Value>,
    source: String,
    keep_headers: i64,
}

impl ResourceLoader for HostLoader<'_> {
    fn load(&mut self, url: &str) -> Result<Vec<u8>, String> {
        // `sdl_set_uri_credentials`: credentials are not sent to other hosts.
        let mut saved = None;
        if let Some(ctx) = &self.context {
            if !same_origin(&self.source, url) {
                if let Some(h) = self.host.context_get_option(ctx, "http", "header") {
                    if let Some(redacted) = redact_headers(&flatten_headers(&h), self.keep_headers)
                    {
                        self.host.context_set_option(
                            ctx,
                            "http",
                            "header",
                            Value::from(redacted.as_slice()),
                        );
                        saved = Some(h);
                    }
                }
            }
        }
        let r = self.host.load_resource(url, self.context.as_ref());
        if let (Some(ctx), Some(h)) = (&self.context, saved) {
            self.host.context_set_option(ctx, "http", "header", h);
        }
        r
    }
}

/// The scheme+authority prefix length of `s` (`sdl_set_uri_credentials`),
/// `None` when it is not a URL.
fn origin_len(s: &str) -> Option<usize> {
    let p = s.find("://")?;
    Some(s[p + 3..].find('/').map_or(s.len(), |q| p + 3 + q))
}

/// Whether two URIs address the same server (default ports ignored).
#[must_use]
pub fn same_origin(source: &str, uri: &str) -> bool {
    let (Some(mut l1), Some(mut l2)) = (origin_len(source), origin_len(uri)) else {
        return true;
    };
    let (a, b) = (source.as_bytes(), uri.as_bytes());
    if l1 != l2 {
        let strip = |s: &[u8], l: &mut usize| {
            if *l > 11 && s.get(4) == Some(&b':') && s[..*l].ends_with(b":80") {
                *l -= 3;
            }
            if *l > 13 && s.get(4) == Some(&b's') && s[..*l].ends_with(b":443") {
                *l -= 4;
            }
        };
        strip(a, &mut l1);
        strip(b, &mut l2);
    }
    l1 == l2 && a[..l1] == b[..l2]
}

fn flatten_headers(h: &Value) -> Vec<u8> {
    match h.deref() {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Array(a) => {
            let mut out = Vec::new();
            for v in a.values() {
                if let Value::Str(s) = v.deref() {
                    if !s.is_empty() {
                        if !out.is_empty() {
                            out.extend_from_slice(b"\r\n");
                        }
                        out.extend_from_slice(s.as_bytes());
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

const REDACT_AUTHORIZATION: i64 = 1;
const REDACT_PROXY_AUTH: i64 = 2;
const REDACT_COOKIE: i64 = 4;

/// Removes `Authorization`, `Proxy-Authorization` and `Cookie` headers
/// (unless kept with the `keep_headers` option); `None` when nothing has to
/// be removed.
#[must_use]
pub fn redact_headers(flat: &[u8], keep: i64) -> Option<Vec<u8>> {
    let lc = flat.to_ascii_lowercase();
    let at_line_start = |name: &[u8]| {
        let mut i = 0;
        while let Some(p) = lc[i..].windows(name.len()).position(|w| w == name) {
            let pos = i + p;
            if pos == 0 || lc[pos - 1] == b'\n' {
                return true;
            }
            i = pos + 1;
        }
        false
    };
    let mut redaction = 0;
    if at_line_start(b"authorization:") {
        redaction |= REDACT_AUTHORIZATION;
    }
    if at_line_start(b"proxy-authorization:") {
        redaction |= REDACT_PROXY_AUTH;
    }
    if at_line_start(b"cookie:") {
        redaction |= REDACT_COOKIE;
    }
    if keep & WSDL_HEADER_KEEP_AUTHORIZATION != 0 {
        redaction &= !REDACT_AUTHORIZATION;
    }
    if keep & WSDL_HEADER_KEEP_PROXY_AUTHORIZATION != 0 {
        redaction &= !REDACT_PROXY_AUTH;
    }
    if keep & WSDL_HEADER_KEEP_COOKIES != 0 {
        redaction &= !REDACT_COOKIE;
    }
    if redaction == 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut off = 0;
    while off < flat.len() {
        let line_end = flat[off..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(flat.len(), |p| off + p + 1);
        let l = &lc[off..];
        let drop = (redaction & REDACT_AUTHORIZATION != 0 && l.starts_with(b"authorization:"))
            || (redaction & REDACT_PROXY_AUTH != 0 && l.starts_with(b"proxy-authorization:"))
            || (redaction & REDACT_COOKIE != 0 && l.starts_with(b"cookie:"));
        if !drop {
            out.extend_from_slice(&flat[off..line_end]);
        }
        off = line_end;
    }
    Some(out)
}

fn basic_auth(login: &[u8], password: Option<&[u8]>) -> Vec<u8> {
    let mut auth = login.to_vec();
    auth.push(b':');
    if let Some(p) = password {
        auth.extend_from_slice(p);
    }
    crate::encoding::base64_encode(&auth)
}

fn slot_str(host: &mut dyn SoapHost, obj: &Obj, name: &str) -> Option<Vec<u8>> {
    match host.read_slot(obj, name).map(|v| v.deref()) {
        Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

/// Builds the stream context used to fetch the WSDL of a client.
fn wsdl_context(host: &mut dyn SoapHost, client: Option<&Obj>, uri: &str) -> Value {
    let mut headers: Vec<u8> = Vec::new();
    let mut context: Option<Value> = None;
    let mut has_authorization = false;
    let mut has_proxy_authorization = false;
    if let Some(this) = client {
        if let Some(Value::Resource(r)) = host.read_slot(this, "_stream_context").map(|v| v.deref())
        {
            context = Some(Value::Resource(r));
        }
        if let Some(ua) = slot_str(host, this, "_user_agent").filter(|u| !u.is_empty()) {
            headers.extend_from_slice(b"User-Agent: ");
            headers.extend_from_slice(&ua);
            headers.extend_from_slice(b"\r\n");
        }
        let proxy_host = slot_str(host, this, "_proxy_host");
        let proxy_port = match host.read_slot(this, "_proxy_port").map(|v| v.deref()) {
            Some(Value::Int(p)) => Some(p),
            _ => None,
        };
        if let (Some(ph), Some(pp)) = (proxy_host, proxy_port) {
            let ctx = context.get_or_insert_with(|| host.context_create()).clone();
            let proxy = format!("tcp://{}:{pp}", String::from_utf8_lossy(&ph));
            host.context_set_option(&ctx, "http", "proxy", Value::from(proxy.as_str()));
            if !uri.to_ascii_lowercase().starts_with("https://") {
                host.context_set_option(&ctx, "http", "request_fulluri", Value::Bool(true));
            }
            if let Some(login) = slot_str(host, this, "_proxy_login") {
                has_proxy_authorization = true;
                let pw = slot_str(host, this, "_proxy_password");
                headers.extend_from_slice(b"Proxy-Authorization: Basic ");
                headers.extend_from_slice(&basic_auth(&login, pw.as_deref()));
                headers.extend_from_slice(b"\r\n");
            }
        }
        let use_digest = matches!(
            host.read_slot(this, "_use_digest").map(|v| v.deref()),
            Some(Value::Bool(true))
        );
        if let (Some(login), false) = (slot_str(host, this, "_login"), use_digest) {
            has_authorization = true;
            let pw = slot_str(host, this, "_password");
            headers.extend_from_slice(b"Authorization: Basic ");
            headers.extend_from_slice(&basic_auth(&login, pw.as_deref()));
            headers.extend_from_slice(b"\r\n");
        }
    }
    let ctx = context.unwrap_or_else(|| host.context_create());
    if host
        .context_get_option(&ctx, "http", "protocol_version")
        .is_none()
    {
        host.context_set_option(&ctx, "http", "protocol_version", Value::Float(1.1));
        headers.extend_from_slice(b"Connection: close\r\n");
    }
    if !headers.is_empty() {
        crate::http::http_context_headers(
            host,
            &ctx,
            has_authorization,
            has_proxy_authorization,
            false,
            &mut headers,
        );
        host.context_set_option(&ctx, "http", "header", Value::from(headers.as_slice()));
    }
    ctx
}

/// `get_sdl`: loads (or fetches from the memory cache) the WSDL at `uri`.
/// Errors carry the `"WSDL"` fault code.
pub fn get_sdl(
    host: &mut dyn SoapHost,
    client: Option<&Obj>,
    uri: &str,
    cache_wsdl: i64,
) -> SoapResult<Rc<Sdl>> {
    let mut cache_wsdl = cache_wsdl;
    let mut uri = uri.to_owned();
    if !(uri.contains(':') || uri.starts_with('/')) {
        match host.realpath(&uri) {
            Some(p) => uri = p,
            None => cache_wsdl = WSDL_CACHE_NONE,
        }
    }
    let now = host.now();
    let ttl = ini_long(host, "soap.wsdl_cache_ttl");
    if cache_wsdl & WSDL_CACHE_MEMORY != 0 {
        let hit = globals::with(|g| match g.mem_cache.get(&uri) {
            Some((t, sdl)) if *t >= now - ttl => Some(Rc::clone(sdl)),
            Some(_) => {
                g.mem_cache.remove(&uri);
                None
            }
            None => None,
        });
        if let Some(sdl) = hit {
            return Ok(sdl);
        }
    }
    let context = Some(wsdl_context(host, client, &uri));
    let old = globals::set_error_code(Some("WSDL"));
    let result = {
        let keep_headers = match client.and_then(|c| host.read_slot(c, "_keep_headers")) {
            Some(Value::Int(k)) => k,
            _ => 0,
        };
        let mut loader = HostLoader {
            host,
            context,
            source: uri.clone(),
            keep_headers,
        };
        load_wsdl(&uri, &mut loader)
    };
    globals::set_error_code(old);
    let sdl = Rc::new(result.map_err(|e| match e {
        SoapError::Fatal(m) => SoapError::Coded("WSDL", m),
        other => other,
    })?);
    if cache_wsdl & WSDL_CACHE_MEMORY != 0 {
        let limit = ini_long(host, "soap.wsdl_cache_limit");
        globals::with(|g| {
            if limit > 0 && usize::try_from(limit).is_ok_and(|l| l <= g.mem_cache.len()) {
                let oldest = g
                    .mem_cache
                    .iter()
                    .filter(|(_, (t, _))| *t < now)
                    .min_by_key(|(_, (t, _))| *t)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => {
                        g.mem_cache.remove(&k);
                    }
                    None => return,
                }
            }
            g.mem_cache.insert(uri.clone(), (now, Rc::clone(&sdl)));
        });
    }
    Ok(sdl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(same_origin(
            "http://a.example/x.wsdl",
            "http://a.example:80/y.xsd"
        ));
        assert!(same_origin(
            "https://a.example:443/x",
            "https://a.example/y"
        ));
        assert!(!same_origin("http://a.example/x", "http://b.example/y"));
        assert!(same_origin("/local/file.wsdl", "http://b.example/y"));
    }

    #[test]
    fn redaction() {
        let h = b"X-A: 1\r\nAuthorization: Basic x\r\nCookie: c=1\r\nX-B: 2";
        assert_eq!(redact_headers(h, 0).unwrap(), b"X-A: 1\r\nX-B: 2".to_vec());
        assert_eq!(
            redact_headers(h, WSDL_HEADER_KEEP_COOKIES).unwrap(),
            b"X-A: 1\r\nCookie: c=1\r\nX-B: 2".to_vec()
        );
        assert!(redact_headers(b"X-Authorization: 1", 0).is_none());
    }
}
