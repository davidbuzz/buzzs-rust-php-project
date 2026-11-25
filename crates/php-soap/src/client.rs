// Derived from php-src ext/soap/soap.c. Similarity metric to original: 22%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `SoapClient` (`soap.c`): construction, calls, the request/response
//! cycle and the small accessor methods. All state lives in the private
//! properties of the object (read and written through
//! [`SoapHost::read_slot`]/[`SoapHost::write_slot`]) plus the typemap kept
//! in [`SoapObject::Client`].

use std::rc::Rc;

use php_runtime::array::ArrayKey;
use php_runtime::{ErrorLevel, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};

use crate::cache::{default_cache_mode, get_sdl};
use crate::charset::Charset;
use crate::consts::*;
use crate::encoding::{CtxSettings, Typemap};
use crate::error::{SoapError, SoapResult};
use crate::fault::{add_soap_fault, FaultSpec};
use crate::globals::{self, ErrorObject};
use crate::host::{MethodCall, SoapHost};
use crate::packet::{parse_packet_soap, serialize_function_call, NonWsdlStyle, PacketResult};
use crate::sdl::{BindingType, Sdl};
use crate::state::SoapObject;
use crate::typemap::create_typemap;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn opt(options: Option<&ZArray>, key: &str) -> Option<Value> {
    options
        .and_then(|o| o.get_str(key.as_bytes()))
        .map(Value::deref)
}

fn slot(host: &mut dyn SoapHost, this: &Obj, name: &str) -> Value {
    host.read_slot(this, name)
        .map(|v| v.deref())
        .unwrap_or_default()
}

fn exceptions_enabled(host: &mut dyn SoapHost, this: &Obj) -> bool {
    !matches!(slot(host, this, "_exceptions"), Value::Bool(false))
}

/// The client error handler: converts an aborted operation according to
/// `soap_real_error_handler` (client branch).
fn convert_error(host: &mut dyn SoapHost, this: &Obj, e: SoapError) -> RuntimeError {
    let (code, msg) = match e {
        SoapError::Runtime(r) => return r,
        SoapError::Fatal(m) => (globals::error_code().unwrap_or("Client"), m),
        SoapError::Coded(c, m) => (c, m),
        SoapError::ServerFault(c, m) => {
            return match add_soap_fault(host, this, &FaultSpec::en(&c, &m)) {
                Ok(f) => RuntimeError::Thrown(Value::Object(f)),
                Err(e) => e,
            }
        }
    };
    if exceptions_enabled(host, this) {
        match add_soap_fault(host, this, &FaultSpec::en(code, &msg)) {
            Ok(f) => RuntimeError::Thrown(Value::Object(f)),
            Err(e) => e,
        }
    } else {
        RuntimeError::Fatal(msg)
    }
}

/// Runs `body` inside `SOAP_CLIENT_BEGIN_CODE`/`END_CODE`.
fn client_scope<T>(
    host: &mut dyn SoapHost,
    this: &Obj,
    body: impl FnOnce(&mut dyn SoapHost) -> SoapResult<T>,
) -> RtResult<T> {
    let exceptions = exceptions_enabled(host, this);
    let saved = globals::begin("Client", ErrorObject::Client(this.clone(), exceptions));
    let r = body(host);
    let r = r.map_err(|e| convert_error(host, this, e));
    globals::end(saved);
    r
}

fn docref_fatal(method: &str, msg: &str) -> SoapError {
    SoapError::Fatal(format!("SoapClient::{method}(): {msg}"))
}

/// Whether a class map has only string keys.
fn class_map_has_only_string_keys(a: &ZArray) -> bool {
    a.keys().all(|k| matches!(k, ArrayKey::Str(_)))
}

/// `zend_zval_type_name`.
pub fn zend_type_name(v: &Value) -> &'static str {
    match v.deref() {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Resource(_) => "resource",
        Value::Reference(_) => "reference",
    }
}

/// `SoapClient::__construct(?string $wsdl, array $options = [])`.
#[allow(clippy::too_many_lines)]
pub fn construct(
    host: &mut dyn SoapHost,
    this: &Obj,
    wsdl: Option<&[u8]>,
    options: Option<&ZArray>,
) -> RtResult<()> {
    if let Some(cm) = opt(options, "classmap") {
        match &cm {
            Value::Array(a) => {
                if !class_map_has_only_string_keys(a) {
                    return Err(RuntimeError::ValueError(
                        "SoapClient::__construct(): Argument #2 ($options) \"classmap\" option must be an associative array".into(),
                    ));
                }
            }
            other => {
                return Err(RuntimeError::TypeError(format!(
                    "SoapClient::__construct(): Argument #2 ($options) \"classmap\" option must be of type array, {} given",
                    zend_type_name(other)
                )))
            }
        }
    }
    let wsdl = wsdl.map(<[u8]>::to_vec);
    client_scope(host, this, |host| {
        let mut cache_wsdl = default_cache_mode(host);
        let mut soap_version = SOAP_1_1;
        let mut typemap_ht = None;
        if let Some(options) = options {
            if wsdl.is_none() {
                match opt(Some(options), "uri") {
                    Some(Value::Str(s)) => host.write_slot(this, "uri", Value::Str(s)),
                    _ => {
                        return Err(docref_fatal(
                            "__construct",
                            "'uri' option is required in nonWSDL mode",
                        ))
                    }
                }
                if let Some(Value::Int(s)) = opt(Some(options), "style") {
                    if s == SOAP_RPC || s == SOAP_DOCUMENT {
                        host.write_slot(this, "style", Value::Int(s));
                    }
                }
                if let Some(Value::Int(u)) = opt(Some(options), "use") {
                    if u == SOAP_LITERAL || u == SOAP_ENCODED {
                        host.write_slot(this, "use", Value::Int(u));
                    }
                }
            }
            match opt(Some(options), "location") {
                Some(Value::Str(s)) => host.write_slot(this, "location", Value::Str(s)),
                _ => {
                    if wsdl.is_none() {
                        return Err(docref_fatal(
                            "__construct",
                            "'location' option is required in nonWSDL mode",
                        ));
                    }
                }
            }
            if let Some(Value::Int(v)) = opt(Some(options), "soap_version") {
                if v == SOAP_1_1 || v == SOAP_1_2 {
                    soap_version = v;
                }
            }
            if let Some(Value::Str(login)) = opt(Some(options), "login") {
                host.write_slot(this, "_login", Value::Str(login));
                if let Some(Value::Str(p)) = opt(Some(options), "password") {
                    host.write_slot(this, "_password", Value::Str(p));
                }
                if let Some(Value::Int(SOAP_AUTHENTICATION_DIGEST)) =
                    opt(Some(options), "authentication")
                {
                    host.write_slot(this, "_use_digest", Value::Bool(true));
                }
            }
            if let Some(Value::Str(ph)) = opt(Some(options), "proxy_host") {
                host.write_slot(this, "_proxy_host", Value::Str(ph));
                if let Some(pp) = opt(Some(options), "proxy_port") {
                    let port = pp.to_long(host)?;
                    host.write_slot(this, "_proxy_port", Value::Int(port));
                }
                if let Some(Value::Str(pl)) = opt(Some(options), "proxy_login") {
                    host.write_slot(this, "_proxy_login", Value::Str(pl));
                    if let Some(Value::Str(pw)) = opt(Some(options), "proxy_password") {
                        host.write_slot(this, "_proxy_password", Value::Str(pw));
                    }
                }
            }
            if matches!(
                opt(Some(options), "trace"),
                Some(Value::Bool(true) | Value::Int(1))
            ) {
                host.write_slot(this, "trace", Value::Bool(true));
            }
            if matches!(
                opt(Some(options), "exceptions"),
                Some(Value::Bool(false) | Value::Int(0))
            ) {
                host.write_slot(this, "_exceptions", Value::Bool(false));
                globals::with(|g| {
                    if let Some(ErrorObject::Client(_, e)) = &mut g.error_object {
                        *e = false;
                    }
                });
            }
            if let Some(Value::Int(c)) = opt(Some(options), "compression") {
                let all = [
                    "gzinflate",
                    "gzdeflate",
                    "gzuncompress",
                    "gzcompress",
                    "gzencode",
                ]
                .iter()
                .all(|f| host.function_exists(f.as_bytes()).is_some());
                if all {
                    host.write_slot(this, "compression", Value::Int(c));
                }
            }
            if let Some(Value::Str(enc)) = opt(Some(options), "encoding") {
                if Charset::find(enc.as_bytes()).is_none() {
                    return Err(docref_fatal(
                        "__construct",
                        &format!("Invalid 'encoding' option - '{}'", lossy(enc.as_bytes())),
                    ));
                }
                host.write_slot(this, "_encoding", Value::Str(enc));
            }
            if let Some(Value::Array(cm)) = opt(Some(options), "classmap") {
                host.write_slot(this, "_classmap", Value::Array(cm));
            }
            let context = match opt(Some(options), "stream_context") {
                Some(Value::Resource(r)) => Value::Resource(r),
                _ => host.context_create(),
            };
            if let Some(Value::Str(lc)) = opt(Some(options), "local_cert") {
                host.context_set_option(&context, "ssl", "local_cert", Value::Str(lc));
                if let Some(Value::Str(pp)) = opt(Some(options), "passphrase") {
                    host.context_set_option(&context, "ssl", "passphrase", Value::Str(pp));
                }
            }
            if let Some(Value::Array(tm)) = opt(Some(options), "typemap") {
                if !tm.is_empty() {
                    typemap_ht = Some(tm);
                }
            }
            if let Some(Value::Int(f)) = opt(Some(options), "features") {
                host.write_slot(this, "_features", Value::Int(f));
            }
            if let Some(ct) = opt(Some(options), "connection_timeout") {
                let l = ct.to_long(host)?;
                if l > 0 {
                    host.write_slot(this, "_connection_timeout", Value::Int(l));
                }
            }
            if !matches!(context, Value::Null) {
                host.write_slot(this, "_stream_context", context);
            }
            if let Some(Value::Int(c)) = opt(Some(options), "cache_wsdl") {
                cache_wsdl = c;
            }
            if let Some(Value::Str(ua)) = opt(Some(options), "user_agent") {
                host.write_slot(this, "_user_agent", Value::Str(ua));
            }
            if matches!(
                opt(Some(options), "keep_alive"),
                Some(Value::Bool(false) | Value::Int(0))
            ) {
                host.write_slot(this, "_keep_alive", Value::Bool(false));
            }
            if matches!(
                opt(Some(options), "follow_cross_origin_redirects"),
                Some(Value::Bool(true) | Value::Int(1))
            ) {
                host.write_slot(this, "_follow_cross_origin_redirects", Value::Bool(true));
            }
            if let Some(Value::Int(m)) = opt(Some(options), "ssl_method") {
                host.write_slot(this, "_ssl_method", Value::Int(m));
                host.diagnostic(
                    ErrorLevel::Deprecated,
                    "SoapClient::__construct(): The \"ssl_method\" option is deprecated. Use \"ssl\" stream context options instead",
                )?;
            }
            if let Some(Value::Array(kh)) = opt(Some(options), "keep_headers") {
                let mut keep = 0;
                for v in kh.values() {
                    if let Value::Str(s) = v.deref() {
                        let b = s.as_bytes();
                        if b.eq_ignore_ascii_case(b"authorization") {
                            keep |= WSDL_HEADER_KEEP_AUTHORIZATION;
                        } else if b.eq_ignore_ascii_case(b"proxy-authorization") {
                            keep |= WSDL_HEADER_KEEP_PROXY_AUTHORIZATION;
                        } else if b.eq_ignore_ascii_case(b"cookie") {
                            keep |= WSDL_HEADER_KEEP_COOKIES;
                        }
                    }
                }
                host.write_slot(this, "_keep_headers", Value::Int(keep));
            }
        } else if wsdl.is_none() {
            return Err(docref_fatal(
                "__construct",
                "'location' and 'uri' options are required in nonWSDL mode",
            ));
        }
        host.write_slot(this, "_soap_version", Value::Int(soap_version));
        let mut sdl = None;
        if let Some(w) = &wsdl {
            let old = globals::soap_version();
            globals::set_soap_version(soap_version);
            let r = get_sdl(host, Some(this), &lossy(w), cache_wsdl);
            globals::set_soap_version(old);
            let s = r?;
            let obj = host.create_object(&ZStr::from("Soap\\Sdl"))?;
            if let Some(state) = host.soap_state(&obj) {
                if let SoapObject::Sdl(cell) = &*state {
                    *cell.borrow_mut() = Some(Rc::clone(&s));
                }
            }
            host.write_slot(this, "sdl", Value::Object(obj));
            sdl = Some(s);
        }
        if let Some(tm) = typemap_ht {
            let t = create_typemap(host, sdl.as_deref(), &tm)?;
            if let Some(state) = host.soap_state(this) {
                if let SoapObject::Client(cell) = &*state {
                    *cell.borrow_mut() = t.map(Rc::new);
                }
            }
        }
        Ok(())
    })
}

/// The SDL of a client (`FETCH_THIS_SDL`).
pub fn client_sdl(host: &mut dyn SoapHost, this: &Obj) -> Option<Rc<Sdl>> {
    let Value::Object(o) = slot(host, this, "sdl") else {
        return None;
    };
    let state = host.soap_state(&o)?;
    match &*state {
        SoapObject::Sdl(cell) => cell.borrow().clone(),
        _ => None,
    }
}

fn client_typemap(host: &mut dyn SoapHost, this: &Obj) -> Option<Rc<Typemap>> {
    let state = host.soap_state(this)?;
    match &*state {
        SoapObject::Client(cell) => cell.borrow().clone(),
        _ => None,
    }
}

/// `do_request`: hands the request to `__doRequest` (which may be
/// overridden). `Ok(None)` when no usable response was obtained (a fault
/// may have been stored).
#[allow(clippy::too_many_arguments)]
fn do_request(
    host: &mut dyn SoapHost,
    this: &Obj,
    request: &[u8],
    location: &[u8],
    action: Option<&[u8]>,
    version: i64,
    one_way: bool,
) -> SoapResult<Option<Vec<u8>>> {
    let trace = matches!(slot(host, this, "trace"), Value::Bool(true));
    if trace {
        host.write_slot(this, "__last_request", Value::from(request));
    }
    let params = [
        Value::from(request),
        Value::from(location),
        Value::from(action.unwrap_or(b"")),
        Value::Int(version),
        Value::Bool(one_way),
    ];
    let response = match host.call_method(this, b"__doRequest", &params)? {
        MethodCall::Returned(v) => v.deref(),
        MethodCall::NotCallable(_) => Value::Null,
    };
    let mut ret = None;
    if let Value::Str(s) = &response {
        if trace {
            host.write_slot(this, "__last_response", Value::Str(s.clone()));
        }
        ret = Some(s.as_bytes().to_vec());
    } else if !matches!(slot(host, this, "__soap_fault"), Value::Object(_)) {
        add_soap_fault(
            host,
            this,
            &FaultSpec::en(
                "Client",
                "SoapClient::__doRequest() returned non string value",
            ),
        )?;
    }
    if ret.is_some() && matches!(slot(host, this, "__soap_fault"), Value::Object(_)) {
        ret = None;
    }
    Ok(ret)
}

/// Arguments of a call beyond the function name.
#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    /// `location` option of `__soapCall`.
    pub location: Option<Vec<u8>>,
    /// `soapaction` option.
    pub soap_action: Option<Vec<u8>>,
    /// `uri` option.
    pub uri: Option<Vec<u8>>,
    /// Input headers (already validated `SoapHeader` objects).
    pub headers: Option<Vec<Value>>,
    /// Whether output headers are wanted.
    pub want_output_headers: bool,
}

/// `do_soap_call`; returns the result and the output headers.
#[allow(clippy::too_many_lines)]
pub fn do_soap_call(
    host: &mut dyn SoapHost,
    this: &Obj,
    function: &[u8],
    args: &[Value],
    opts: &CallOptions,
) -> RtResult<(Value, ZArray)> {
    client_scope(host, this, |host| {
        if matches!(slot(host, this, "trace"), Value::Bool(true)) {
            host.write_slot(this, "__last_request", Value::Null);
            host.write_slot(this, "__last_response", Value::Null);
        }
        let soap_version = if matches!(slot(host, this, "_soap_version"), Value::Int(SOAP_1_2)) {
            SOAP_1_2
        } else {
            SOAP_1_1
        };
        let location: Option<Vec<u8>> =
            opts.location
                .clone()
                .or_else(|| match slot(host, this, "location") {
                    Value::Str(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                });
        let sdl = client_sdl(host, this);
        let typemap = client_typemap(host, this);
        host.write_slot(this, "__soap_fault", Value::Null);
        globals::set_soap_version(soap_version);
        let charset = match slot(host, this, "_encoding") {
            Value::Str(s) => Charset::find(s.as_bytes()),
            _ => None,
        };
        let class_map = match slot(host, this, "_classmap") {
            Value::Array(a) => Some(a),
            _ => None,
        };
        let features = match slot(host, this, "_features") {
            Value::Int(f) => f,
            _ => 0,
        };
        let style = match slot(host, this, "style") {
            Value::Int(s) => Some(s),
            _ => None,
        };
        let use_ = match slot(host, this, "use") {
            Value::Int(u) => Some(u),
            _ => None,
        };
        let old_features = globals::with(|g| std::mem::replace(&mut g.features, features));
        let settings = CtxSettings {
            sdl: sdl.clone(),
            typemap,
            class_map,
            features,
            soap_version,
            charset,
        };
        let result = (|| -> SoapResult<(Value, ZArray, bool)> {
            let mut ret_ok = false;
            let mut out = (Value::Null, ZArray::new());
            if let Some(sdl) = &sdl {
                match sdl.get_function(function) {
                    Some(f) => {
                        let binding = sdl.binding_of(f);
                        let one_way = f.response_name.is_none()
                            && f.response_params.is_none()
                            && opts.headers.is_none();
                        let loc = location
                            .clone()
                            .unwrap_or_else(|| binding.location.clone().into_bytes());
                        let (request, action) = {
                            let mut ctx = settings.make(host);
                            if binding.binding_type == BindingType::Soap {
                                let fnb = f.soap.as_ref();
                                let uri = fnb.and_then(|b| b.input.ns.clone());
                                let doc = serialize_function_call(
                                    &mut ctx,
                                    Some(f),
                                    None,
                                    uri.as_deref().map(str::as_bytes),
                                    args,
                                    soap_version,
                                    opts.headers.as_deref(),
                                    NonWsdlStyle { style, use_ },
                                )?;
                                (doc.dump(), fnb.and_then(|b| b.soap_action.clone()))
                            } else {
                                let doc = serialize_function_call(
                                    &mut ctx,
                                    Some(f),
                                    None,
                                    sdl.target_ns.as_deref().map(str::as_bytes),
                                    args,
                                    soap_version,
                                    opts.headers.as_deref(),
                                    NonWsdlStyle { style, use_ },
                                )?;
                                (doc.dump(), None)
                            }
                        };
                        let response = do_request(
                            host,
                            this,
                            &request,
                            &loc,
                            action.as_deref().map(str::as_bytes),
                            soap_version,
                            one_way,
                        )?;
                        if let Some(resp) = response {
                            let mut ctx = settings.make(host);
                            ctx.reset_ns();
                            let r = parse_packet_soap(&mut ctx, this, &resp, Some(f));
                            ctx.finish();
                            match r? {
                                PacketResult::Ok(v, h) => {
                                    ret_ok = true;
                                    out = (v, h);
                                }
                                PacketResult::Fault => {}
                            }
                        }
                    }
                    None => {
                        add_soap_fault(
                            host,
                            this,
                            &FaultSpec::en(
                                "Client",
                                &format!(
                                    "Function (\"{}\") is not a valid method for this service",
                                    lossy(function)
                                ),
                            ),
                        )?;
                    }
                }
            } else {
                let uri = match slot(host, this, "uri") {
                    Value::Str(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                };
                if uri.is_none() {
                    add_soap_fault(
                        host,
                        this,
                        &FaultSpec::en("Client", "Error finding \"uri\" property"),
                    )?;
                } else if location.is_none() {
                    add_soap_fault(
                        host,
                        this,
                        &FaultSpec::en("Client", "Error could not find \"location\" property"),
                    )?;
                } else {
                    let call_uri = opts.uri.clone().or(uri).unwrap_or_default();
                    let request = {
                        let mut ctx = settings.make(host);
                        serialize_function_call(
                            &mut ctx,
                            None,
                            Some(function),
                            Some(&call_uri),
                            args,
                            soap_version,
                            opts.headers.as_deref(),
                            NonWsdlStyle { style, use_ },
                        )?
                        .dump()
                    };
                    let action = if let Some(a) = &opts.soap_action {
                        a.clone()
                    } else {
                        let mut a = call_uri.clone();
                        a.push(b'#');
                        a.extend_from_slice(function);
                        a
                    };
                    let response = do_request(
                        host,
                        this,
                        &request,
                        location.as_deref().unwrap_or(b""),
                        Some(&action),
                        soap_version,
                        false,
                    )?;
                    if let Some(resp) = response {
                        let mut ctx = settings.make(host);
                        ctx.reset_ns();
                        let r = parse_packet_soap(&mut ctx, this, &resp, None);
                        ctx.finish();
                        match r? {
                            PacketResult::Ok(v, h) => {
                                ret_ok = true;
                                out = (v, h);
                            }
                            PacketResult::Fault => {}
                        }
                    }
                }
            }
            Ok((out.0, out.1, ret_ok))
        })();
        globals::with(|g| g.features = old_features);
        let (mut value, headers, ok) = result?;
        let fault = slot(host, this, "__soap_fault");
        if !ok {
            value = if matches!(fault, Value::Object(_)) {
                fault
            } else {
                Value::Object(add_soap_fault(
                    host,
                    this,
                    &FaultSpec::en("Client", "Unknown Error"),
                )?)
            };
        } else if matches!(fault, Value::Object(_)) {
            value = fault;
        }
        if let Value::Object(o) = &value {
            if host.instance_of(o, "SoapFault") && exceptions_enabled(host, this) {
                return Err(SoapError::Runtime(RuntimeError::Thrown(value.clone())));
            }
        }
        Ok((value, headers))
    })
}

fn verify_soap_headers(host: &mut dyn SoapHost, a: &ZArray) -> bool {
    a.values().all(|v| match v.deref() {
        Value::Object(o) => host.instance_of(&o, "SoapHeader"),
        _ => false,
    })
}

/// Appends the default headers (`__default_headers`).
fn with_default_headers(
    host: &mut dyn SoapHost,
    this: &Obj,
    headers: Option<Vec<Value>>,
) -> Option<Vec<Value>> {
    match slot(host, this, "__default_headers") {
        Value::Array(d) => match headers {
            Some(mut h) => {
                h.extend(
                    d.values()
                        .map(Value::deref)
                        .filter(|v| matches!(v, Value::Object(_))),
                );
                Some(h)
            }
            None => Some(d.values().map(Value::deref).collect()),
        },
        _ => headers,
    }
}

/// `SoapClient::__call(string $name, array $args)`.
pub fn call(host: &mut dyn SoapHost, this: &Obj, name: &[u8], args: &ZArray) -> RtResult<Value> {
    let headers = with_default_headers(host, this, None);
    let args: Vec<Value> = args.values().map(Value::deref).collect();
    let opts = CallOptions {
        headers,
        ..CallOptions::default()
    };
    do_soap_call(host, this, name, &args, &opts).map(|(v, _)| v)
}

/// `SoapClient::__soapCall(...)`; `output_headers` receives the response
/// headers when given.
pub fn soap_call(
    host: &mut dyn SoapHost,
    this: &Obj,
    name: &[u8],
    args: &ZArray,
    options: Option<&ZArray>,
    input_headers: Option<&Value>,
    output_headers: Option<&mut Value>,
) -> RtResult<Value> {
    let mut opts = CallOptions::default();
    if let Some(Value::Str(s)) = opt(options, "location") {
        opts.location = Some(s.as_bytes().to_vec());
    }
    if let Some(Value::Str(s)) = opt(options, "soapaction") {
        opts.soap_action = Some(s.as_bytes().to_vec());
    }
    if let Some(Value::Str(s)) = opt(options, "uri") {
        opts.uri = Some(s.as_bytes().to_vec());
    }
    let mut out_slot = output_headers;
    if let Some(o) = out_slot.as_deref_mut() {
        *o = Value::Array(ZArray::new());
    }
    let headers = match input_headers.map(Value::deref) {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            if !verify_soap_headers(host, &a) {
                return Err(RuntimeError::Fatal("SoapClient::__soapCall(): Invalid SOAP header".into()));
            }
            Some(a.values().map(Value::deref).collect())
        }
        Some(Value::Object(o)) if host.instance_of(&o, "SoapHeader") => Some(vec![Value::Object(o)]),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "SoapClient::__soapCall(): Argument #4 ($inputHeaders) must be of type SoapHeader|array|null, {} given",
                zend_type_name(&other)
            )))
        }
    };
    opts.headers = with_default_headers(host, this, headers);
    opts.want_output_headers = out_slot.is_some();
    let args: Vec<Value> = args.values().map(Value::deref).collect();
    let (v, h) = do_soap_call(host, this, name, &args, &opts)?;
    if let Some(o) = out_slot {
        *o = Value::Array(h);
    }
    Ok(v)
}

/// `SoapClient::__getFunctions()`.
pub fn get_functions(host: &mut dyn SoapHost, this: &Obj) -> Value {
    match client_sdl(host, this) {
        Some(sdl) => Value::Array(ZArray::from_values(
            crate::sdl_print::get_functions(&sdl)
                .into_iter()
                .map(|s| Value::from(s.as_str())),
        )),
        None => Value::Null,
    }
}

/// `SoapClient::__getTypes()`.
pub fn get_types(host: &mut dyn SoapHost, this: &Obj) -> Value {
    match client_sdl(host, this) {
        Some(sdl) => Value::Array(ZArray::from_values(
            crate::sdl_print::get_types(&sdl)
                .into_iter()
                .map(|s| Value::from(s.as_str())),
        )),
        None => Value::Null,
    }
}

/// `__getLastRequest`, `__getLastResponse`, `__getLastRequestHeaders`,
/// `__getLastResponseHeaders` (`which` is the property name).
pub fn get_last(host: &mut dyn SoapHost, this: &Obj, which: &str) -> Value {
    slot(host, this, which)
}

/// `SoapClient::__setCookie(string $name, ?string $value = null)`.
pub fn set_cookie(host: &mut dyn SoapHost, this: &Obj, name: &[u8], value: Option<&[u8]>) {
    let mut cookies = match slot(host, this, "_cookies") {
        Value::Array(a) => a,
        _ => ZArray::new(),
    };
    let key = ArrayKey::from_bytes(name);
    match value {
        None => {
            cookies.make_mut().remove(&key);
        }
        Some(v) => {
            cookies
                .make_mut()
                .insert(key, Value::Array(ZArray::from_values([Value::from(v)])));
        }
    }
    host.write_slot(this, "_cookies", Value::Array(cookies));
}

/// `SoapClient::__getCookies()`.
pub fn get_cookies(host: &mut dyn SoapHost, this: &Obj) -> Value {
    slot(host, this, "_cookies")
}

/// `SoapClient::__setSoapHeaders($headers = null)`.
pub fn set_soap_headers(
    host: &mut dyn SoapHost,
    this: &Obj,
    headers: Option<&Value>,
) -> RtResult<bool> {
    match headers.map(Value::deref) {
        None | Some(Value::Null) => host.write_slot(this, "__default_headers", Value::Null),
        Some(Value::Array(a)) => {
            if !verify_soap_headers(host, &a) {
                return Err(RuntimeError::Fatal("SoapClient::__setSoapHeaders(): Invalid SOAP header".into()));
            }
            host.write_slot(this, "__default_headers", Value::Array(a));
        }
        Some(Value::Object(o)) if host.instance_of(&o, "SoapHeader") => {
            host.write_slot(this, "__default_headers", Value::Array(ZArray::from_values([Value::Object(o)])));
        }
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "SoapClient::__setSoapHeaders(): Argument #1 ($headers) must be of type SoapHeader|array|null, {} given",
                zend_type_name(&other)
            )))
        }
    }
    Ok(true)
}

/// `SoapClient::__setLocation(?string $location = null)`: returns the
/// previous location.
pub fn set_location(host: &mut dyn SoapHost, this: &Obj, location: Option<&[u8]>) -> Value {
    let old = slot(host, this, "location");
    match location.filter(|l| !l.is_empty()) {
        Some(l) => host.write_slot(this, "location", Value::from(l)),
        None => host.write_slot(this, "location", Value::Null),
    }
    old
}

/// `SoapClient::__doRequest(...)`: the default HTTP transport.
#[allow(clippy::too_many_arguments)]
pub fn do_request_method(
    host: &mut dyn SoapHost,
    this: &Obj,
    request: &[u8],
    location: &[u8],
    action: &[u8],
    version: i64,
    one_way: bool,
    uri_parser_class: Option<&[u8]>,
) -> RtResult<Value> {
    let features = globals::with(|g| g.features);
    let one_way = one_way && features & SOAP_WAIT_ONE_WAY_CALLS == 0;
    let r = crate::http::make_http_soap_request(
        host,
        this,
        request,
        location,
        action,
        version,
        uri_parser_class,
        !one_way,
    )?;
    Ok(match r {
        Some(body) if !one_way => Value::from(body.as_slice()),
        Some(_) => Value::from(""),
        None => Value::Null,
    })
}
