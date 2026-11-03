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
        .or_else(|| INI_ENTRIES.iter().find(|(n, _)| *n == name).map(|(_, d)| d.as_bytes().to_vec()))
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
}

impl ResourceLoader for HostLoader<'_> {
    fn load(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.host.load_resource(url, self.context.as_ref())
    }
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
fn wsdl_context(host: &mut dyn SoapHost, client: Option<&Obj>, uri: &str) -> Option<Value> {
    let mut headers: Vec<u8> = Vec::new();
    let mut context: Option<Value> = None;
    if let Some(this) = client {
        if let Some(Value::Resource(r)) = host.read_slot(this, "_stream_context").map(|v| v.deref()) {
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
                let pw = slot_str(host, this, "_proxy_password");
                headers.extend_from_slice(b"Proxy-Authorization: Basic ");
                headers.extend_from_slice(&basic_auth(&login, pw.as_deref()));
                headers.extend_from_slice(b"\r\n");
            }
        }
        let use_digest = matches!(host.read_slot(this, "_use_digest").map(|v| v.deref()), Some(Value::Bool(true)));
        if let (Some(login), false) = (slot_str(host, this, "_login"), use_digest) {
            let pw = slot_str(host, this, "_password");
            headers.extend_from_slice(b"Authorization: Basic ");
            headers.extend_from_slice(&basic_auth(&login, pw.as_deref()));
            headers.extend_from_slice(b"\r\n");
        }
    }
    let ctx = context.unwrap_or_else(|| host.context_create());
    if host.context_get_option(&ctx, "http", "protocol_version").is_none() {
        host.context_set_option(&ctx, "http", "protocol_version", Value::Float(1.1));
        headers.extend_from_slice(b"Connection: close\r\n");
    }
    if !headers.is_empty() {
        if let Some(existing) = host.context_get_option(&ctx, "http", "header") {
            if let Value::Str(s) = existing.deref() {
                let mut h = headers.clone();
                h.extend_from_slice(s.as_bytes());
                headers = h;
            }
        }
        host.context_set_option(&ctx, "http", "header", Value::from(headers.as_slice()));
    }
    Some(ctx)
}

/// `get_sdl`: loads (or fetches from the memory cache) the WSDL at `uri`.
/// Errors carry the `"WSDL"` fault code.
pub fn get_sdl(host: &mut dyn SoapHost, client: Option<&Obj>, uri: &str, cache_wsdl: i64) -> SoapResult<Rc<Sdl>> {
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
    let context = wsdl_context(host, client, &uri);
    let old = globals::set_error_code(Some("WSDL"));
    let result = {
        let mut loader = HostLoader { host, context };
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
