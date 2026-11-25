// Derived from php-src ext/soap/soap.c. Similarity metric to original: 11%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `SoapServer` (`soap.c`): construction, handler registration and the
//! request handling state machine (`SoapServer::handle`), including fault
//! responses and the SOAP error handler's server branch.

use std::cell::RefCell;
use std::rc::Rc;

use php_runtime::array::ArrayKey;
use php_runtime::{ErrorLevel, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};

use crate::cache::{default_cache_mode, get_sdl};
use crate::charset::Charset;
use crate::client::zend_type_name;
use crate::consts::*;
use crate::encoding::{CtxSettings, Typemap};
use crate::error::{SoapError, SoapResult};
use crate::fault::{add_soap_fault, set_soap_fault, FaultSpec};
use crate::globals::{self, ErrorObject};
use crate::host::{MethodCall, SoapHost};
use crate::packet::{deserialize_function_call, serialize_response_call, SoapHeaderEntry};
use crate::sdl::{Function, Sdl};
use crate::state::SoapObject;
use crate::typemap::create_typemap;
use crate::xml::Doc;
use crate::zhash::ZHash;

/// How requests are dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceType {
    /// `addFunction` (`SOAP_FUNCTIONS`).
    Functions,
    /// `setClass` (`SOAP_CLASS`).
    Class,
    /// `setObject` (`SOAP_OBJECT`).
    Object,
}

/// `soapService`.
#[derive(Debug, Clone)]
pub struct Service {
    /// The WSDL.
    pub sdl: Option<Rc<Sdl>>,
    /// Functions added with `addFunction` (lower-case name → name).
    pub functions: Option<ZHash<ZStr>>,
    /// `SOAP_FUNCTIONS_ALL`.
    pub functions_all: bool,
    /// Class of `setClass`.
    pub class: Option<ZStr>,
    /// Constructor arguments of `setClass`.
    pub class_args: Vec<Value>,
    /// `SOAP_PERSISTENCE_*`.
    pub persistence: i64,
    /// Object of `setObject`.
    pub object: Option<Obj>,
    /// Dispatch mode.
    pub service_type: ServiceType,
    /// `typemap`.
    pub typemap: Option<Rc<Typemap>>,
    /// SOAP version.
    pub version: i64,
    /// `actor`.
    pub actor: Option<Vec<u8>>,
    /// `uri`.
    pub uri: Option<Vec<u8>>,
    /// `encoding`.
    pub charset: Option<Charset>,
    /// `classmap`.
    pub class_map: Option<ZArray>,
    /// `features`.
    pub features: i64,
    /// `send_errors`.
    pub send_errors: bool,
    /// `trace`.
    pub trace: bool,
    /// Last response body (with `trace`).
    pub last_response: Option<Vec<u8>>,
    /// Headers of the request being handled (`soap_headers_ptr`).
    pub soap_headers: Option<Rc<RefCell<Vec<SoapHeaderEntry>>>>,
}

impl Service {
    fn new() -> Self {
        Self {
            sdl: None,
            functions: Some(ZHash::new()),
            functions_all: false,
            class: None,
            class_args: Vec::new(),
            persistence: SOAP_PERSISTENCE_REQUEST,
            object: None,
            service_type: ServiceType::Functions,
            typemap: None,
            version: SOAP_1_1,
            actor: None,
            uri: None,
            charset: None,
            class_map: None,
            features: 0,
            send_errors: true,
            trace: false,
            last_response: None,
            soap_headers: None,
        }
    }

    fn settings(&self, version: i64) -> CtxSettings {
        CtxSettings {
            sdl: self.sdl.clone(),
            typemap: self.typemap.clone(),
            class_map: self.class_map.clone(),
            features: self.features,
            soap_version: version,
            charset: self.charset,
        }
    }
}

fn state(host: &mut dyn SoapHost, this: &Obj) -> Option<Rc<SoapObject>> {
    host.soap_state(this)
        .filter(|s| matches!(**s, SoapObject::Server(_)))
}

fn with_service<R>(
    host: &mut dyn SoapHost,
    this: &Obj,
    f: impl FnOnce(&mut Service) -> R,
) -> RtResult<R> {
    let st = state(host, this)
        .ok_or_else(|| RuntimeError::Error("Cannot fetch SoapServer object".into()))?;
    let SoapObject::Server(cell) = &*st else {
        return Err(RuntimeError::Error("Cannot fetch SoapServer object".into()));
    };
    let mut b = cell.borrow_mut();
    let svc = b
        .as_mut()
        .ok_or_else(|| RuntimeError::Error("Cannot fetch SoapServer object".into()))?;
    Ok(f(svc))
}

fn service_snapshot(host: &mut dyn SoapHost, this: &Obj) -> RtResult<Service> {
    with_service(host, this, |s| s.clone())
}

fn opt(options: Option<&ZArray>, key: &str) -> Option<Value> {
    options
        .and_then(|o| o.get_str(key.as_bytes()))
        .map(Value::deref)
}

fn type_err(msg: &str) -> RuntimeError {
    RuntimeError::TypeError(format!(
        "SoapServer::__construct(): Argument #2 ($options) {msg}"
    ))
}

/// `SoapServer::__construct(?string $wsdl, array $options = [])`.
#[allow(clippy::too_many_lines)]
pub fn construct(
    host: &mut dyn SoapHost,
    this: &Obj,
    wsdl: Option<&[u8]>,
    options: Option<&ZArray>,
) -> RtResult<()> {
    let mut service = Service::new();
    let mut cache_wsdl = default_cache_mode(host);
    let mut typemap_ht = None;
    let mut version = SOAP_1_1;
    let uri_required = || {
        RuntimeError::ValueError(
            "SoapServer::__construct(): Argument #2 ($options) \"uri\" option is required when argument #1 ($wsdl) is null".into(),
        )
    };
    if let Some(options) = options {
        if let Some(v) = opt(Some(options), "soap_version") {
            let Value::Int(n) = v else {
                return Err(type_err(&format!(
                    "\"soap_version\" option must be of type int, {} given",
                    zend_type_name(&v)
                )));
            };
            if n != SOAP_1_1 && n != SOAP_1_2 {
                return Err(RuntimeError::ValueError(
                    "SoapServer::__construct(): Argument #2 ($options) \"soap_version\" option must be SOAP_1_1 or SOAP_1_2".into(),
                ));
            }
            version = n;
        }
        match opt(Some(options), "uri") {
            Some(Value::Str(s)) => service.uri = Some(s.as_bytes().to_vec()),
            Some(other) => {
                return Err(type_err(&format!(
                    "\"uri\" option must be of type string, {} given",
                    zend_type_name(&other)
                )))
            }
            None => {
                if wsdl.is_none() {
                    return Err(uri_required());
                }
            }
        }
        if let Some(v) = opt(Some(options), "actor") {
            let Value::Str(s) = v else {
                return Err(type_err(&format!(
                    "\"actor\" option must be of type string, {} given",
                    zend_type_name(&v)
                )));
            };
            service.actor = Some(s.as_bytes().to_vec());
        }
        if let Some(v) = opt(Some(options), "encoding") {
            let Value::Str(s) = v else {
                return Err(type_err(&format!(
                    "\"encoding\" option must be of type string, {} given",
                    zend_type_name(&v)
                )));
            };
            match Charset::find(s.as_bytes()) {
                Some(c) => service.charset = Some(c),
                None => {
                    return Err(RuntimeError::ValueError(format!(
                        "SoapServer::__construct(): Argument #2 ($options) \"encoding\" option must be a valid encoding, \"{}\" given",
                        String::from_utf8_lossy(s.as_bytes())
                    )))
                }
            }
        }
        if let Some(v) = opt(Some(options), "classmap") {
            let Value::Array(a) = v else {
                return Err(type_err(&format!(
                    "\"classmap\" option must be of type array, {} given",
                    zend_type_name(&v)
                )));
            };
            if !a.keys().all(|k| matches!(k, ArrayKey::Str(_))) {
                return Err(RuntimeError::ValueError(
                    "SoapServer::__construct(): Argument #2 ($options) \"classmap\" option must be an associative array".into(),
                ));
            }
            service.class_map = Some(a);
        }
        if let Some(v) = opt(Some(options), "typemap") {
            let Value::Array(a) = v else {
                return Err(type_err(&format!(
                    "\"typemap\" option must be of type array, {} given",
                    zend_type_name(&v)
                )));
            };
            if !a.is_empty() {
                typemap_ht = Some(a);
            }
        }
        if let Some(v) = opt(Some(options), "features") {
            let Value::Int(f) = v else {
                return Err(type_err(&format!(
                    "\"features\" option must be of type int, {} given",
                    zend_type_name(&v)
                )));
            };
            service.features = f;
        }
        if let Some(v) = opt(Some(options), "cache_wsdl") {
            let Value::Int(c) = v else {
                return Err(type_err(&format!(
                    "\"cache_wsdl\" option must be of type int, {} given",
                    zend_type_name(&v)
                )));
            };
            cache_wsdl = c;
        }
        if let Some(v) = opt(Some(options), "send_errors") {
            service.send_errors = match v {
                Value::Bool(b) => b,
                Value::Int(i) => i != 0,
                other => {
                    return Err(type_err(&format!(
                        "\"send_errors\" option must be of type bool, {} given",
                        zend_type_name(&other)
                    )))
                }
            };
        }
        if let Some(v) = opt(Some(options), "trace") {
            service.trace = match v {
                Value::Bool(b) => b,
                Value::Int(i) => i != 0,
                other => {
                    return Err(type_err(&format!(
                        "\"trace\" option must be of type bool, {} given",
                        zend_type_name(&other)
                    )))
                }
            };
        }
    } else if wsdl.is_none() {
        return Err(uri_required());
    }
    service.version = version;
    let saved = globals::begin("Server", ErrorObject::Server(this.clone()));
    let r = (|| -> SoapResult<()> {
        if let Some(w) = wsdl {
            let sdl = get_sdl(host, None, &String::from_utf8_lossy(w), cache_wsdl)?;
            if service.uri.is_none() {
                service.uri = Some(
                    sdl.target_ns
                        .clone()
                        .unwrap_or_else(|| "http://unknown-uri/".to_owned())
                        .into_bytes(),
                );
            }
            service.sdl = Some(sdl);
        }
        if let Some(tm) = &typemap_ht {
            service.typemap = create_typemap(host, service.sdl.as_deref(), tm)?.map(Rc::new);
        }
        Ok(())
    })();
    let r = match r {
        Ok(()) => {
            if let Some(st) = state(host, this) {
                if let SoapObject::Server(cell) = &*st {
                    *cell.borrow_mut() = Some(service);
                }
            }
            Ok(())
        }
        Err(e) => Err(server_error(host, this, e, None, None)),
    };
    globals::end(saved);
    r
}

/// `SoapServer::setPersistence(int $mode)`.
pub fn set_persistence(host: &mut dyn SoapHost, this: &Obj, mode: i64) -> RtResult<()> {
    let session = host.session_available();
    with_service(host, this, |s| {
        if s.service_type == ServiceType::Class {
            if mode == SOAP_PERSISTENCE_SESSION || mode == SOAP_PERSISTENCE_REQUEST {
                if mode == SOAP_PERSISTENCE_SESSION && !session {
                    return Err(RuntimeError::Error(
                        "SoapServer::setPersistence(): Session persistence cannot be enabled because the session module is not enabled".into(),
                    ));
                }
                s.persistence = mode;
                Ok(())
            } else {
                Err(RuntimeError::ValueError(
                    "SoapServer::setPersistence(): Argument #1 ($mode) must be either SOAP_PERSISTENCE_SESSION or SOAP_PERSISTENCE_REQUEST when the SOAP server is used in class mode".into(),
                ))
            }
        } else {
            Err(RuntimeError::Error(
                "SoapServer::setPersistence(): Persistence cannot be set when the SOAP server is used in function mode".into(),
            ))
        }
    })?
}

/// `SoapServer::setClass(string $class, mixed ...$args)` (`class` already
/// resolved by the engine).
pub fn set_class(
    host: &mut dyn SoapHost,
    this: &Obj,
    class: ZStr,
    args: Vec<Value>,
) -> RtResult<()> {
    with_service(host, this, |s| {
        s.service_type = ServiceType::Class;
        s.class = Some(class);
        s.class_args = args;
        s.persistence = SOAP_PERSISTENCE_REQUEST;
    })
}

/// `SoapServer::setObject(object $object)`.
pub fn set_object(host: &mut dyn SoapHost, this: &Obj, object: Obj) -> RtResult<()> {
    with_service(host, this, |s| {
        s.service_type = ServiceType::Object;
        s.object = Some(object);
    })
}

/// `SoapServer::getFunctions()`.
pub fn get_functions(host: &mut dyn SoapHost, this: &Obj) -> RtResult<Value> {
    let s = service_snapshot(host, this)?;
    let names: Vec<ZStr> = match s.service_type {
        ServiceType::Object => {
            let class = s.object.as_ref().map(Obj::class_name).unwrap_or_default();
            host.class_methods(&class)
                .into_iter()
                .filter(|(_, p)| *p)
                .map(|(n, _)| n)
                .collect()
        }
        ServiceType::Class => {
            let class = s.class.clone().unwrap_or_default();
            host.class_methods(&class)
                .into_iter()
                .filter(|(_, p)| *p)
                .map(|(n, _)| n)
                .collect()
        }
        ServiceType::Functions => {
            if s.functions_all {
                host.defined_functions()
            } else {
                s.functions
                    .map(|f| f.values().cloned().collect())
                    .unwrap_or_default()
            }
        }
    };
    Ok(Value::Array(ZArray::from_values(
        names.into_iter().map(Value::Str),
    )))
}

/// `SoapServer::addFunction($functions)`.
pub fn add_function(host: &mut dyn SoapHost, this: &Obj, functions: &Value) -> RtResult<()> {
    let s = service_snapshot(host, this)?;
    match functions.deref() {
        Value::Array(a) => {
            if s.service_type == ServiceType::Functions {
                let mut add = Vec::new();
                for v in a.values() {
                    let Value::Str(name) = v.deref() else {
                        return Err(RuntimeError::TypeError(
                            "SoapServer::addFunction(): Argument #1 ($functions) must contain only strings".into(),
                        ));
                    };
                    match host.function_exists(name.as_bytes()) {
                        Some(canon) => add.push(canon),
                        None => {
                            return Err(RuntimeError::TypeError(format!(
                                "SoapServer::addFunction(): Function \"{}\" not found",
                                name.to_string_lossy()
                            )))
                        }
                    }
                }
                with_service(host, this, |s| {
                    let ft = s.functions.get_or_insert_with(|| {
                        ZHash::new()
                    });
                    if s.functions_all {
                        s.functions_all = false;
                    }
                    for c in add {
                        ft.update(&c.to_string_lossy().to_ascii_lowercase(), c);
                    }
                })?;
            }
            Ok(())
        }
        Value::Str(name) => {
            let Some(canon) = host.function_exists(name.as_bytes()) else {
                return Err(RuntimeError::TypeError(format!(
                    "SoapServer::addFunction(): Argument #1 ($functions) must be a valid function name, function \"{}\" not found",
                    name.to_string_lossy()
                )));
            };
            with_service(host, this, |s| {
                if s.functions.is_none() {
                    s.functions_all = false;
                }
                s.functions
                    .get_or_insert_with(ZHash::new)
                    .update(&canon.to_string_lossy().to_ascii_lowercase(), canon);
            })
        }
        Value::Int(i) => {
            if i == SOAP_FUNCTIONS_ALL {
                host.diagnostic(
                    ErrorLevel::Deprecated,
                    "SoapServer::addFunction(): Enabling all functions via SOAP_FUNCTIONS_ALL is deprecated since 8.4, due to possible security concerns. If all PHP functions should be enabled, the flattened return value of get_defined_functions() can be used",
                )?;
                with_service(host, this, |s| {
                    s.functions = None;
                    s.functions_all = true;
                })
            } else {
                Err(RuntimeError::ValueError(
                    "SoapServer::addFunction(): Argument #1 ($functions) must be SOAP_FUNCTIONS_ALL when an integer is passed".into(),
                ))
            }
        }
        other => Err(RuntimeError::TypeError(format!(
            "SoapServer::addFunction(): Argument #1 ($functions) must be of type array|string|int, {} given",
            zend_type_name(&other)
        ))),
    }
}

/// `soap_server_fault_ex`: writes a fault response.
fn soap_server_fault_ex(
    host: &mut dyn SoapHost,
    service: Option<&Service>,
    function: Option<&Function>,
    fault: &Value,
    hdr: Option<&SoapHeaderEntry>,
) -> RtResult<()> {
    let version = globals::soap_version();
    let settings = service.map_or_else(
        || CtxSettings {
            soap_version: version,
            ..CtxSettings::default()
        },
        |s| s.settings(version),
    );
    let headers: Vec<SoapHeaderEntry> = hdr.cloned().into_iter().collect();
    let doc = {
        let mut ctx = settings.make(host);
        serialize_response_call(&mut ctx, function, None, None, fault, &headers, version)
    };
    let doc = match doc {
        Ok(d) => d,
        Err(SoapError::Runtime(r)) => return Err(r),
        Err(e) => return Err(RuntimeError::Fatal(e.to_string())),
    };
    let buf = doc.map(|d| d.dump()).unwrap_or_default();
    let flash = host
        .server_var("HTTP_USER_AGENT")
        .is_some_and(|a| a == b"Shockwave Flash");
    if !flash {
        host.add_header("HTTP/1.1 500 Internal Server Error");
    }
    if zlib_output_compression(host) {
        host.add_header("Connection: close");
    } else {
        host.add_header(&format!("Content-Length: {}", buf.len()));
    }
    if version == SOAP_1_2 {
        host.add_header("Content-Type: application/soap+xml; charset=utf-8");
    } else {
        host.add_header("Content-Type: text/xml; charset=utf-8");
    }
    host.write_output(&buf);
    Ok(())
}

fn zlib_output_compression(host: &mut dyn SoapHost) -> bool {
    host.ini_get("zlib.output_compression").is_some_and(|v| {
        let s = String::from_utf8_lossy(&v).to_ascii_lowercase();
        !(s.is_empty() || s == "0" || s == "off" || s == "false" || s == "no")
    })
}

/// `soap_server_fault`: writes a fault built from the arguments; the
/// caller then bails out.
fn soap_server_fault(
    host: &mut dyn SoapHost,
    service: Option<&Service>,
    spec: &FaultSpec,
) -> RtResult<()> {
    let fault = set_soap_fault(host, None, spec)?;
    soap_server_fault_ex(host, service, None, &Value::Object(fault), None)
}

/// The server branch of the SOAP error handler, plus direct faults: writes
/// the fault response and returns the bailout error.
fn server_error(
    host: &mut dyn SoapHost,
    this: &Obj,
    e: SoapError,
    service: Option<&Service>,
    _function: Option<&Function>,
) -> RuntimeError {
    let svc_owned;
    let service = if let Some(s) = service {
        Some(s)
    } else {
        svc_owned = service_snapshot(host, this).ok();
        svc_owned.as_ref()
    };
    let r = match e {
        SoapError::Runtime(r) => return r,
        SoapError::ServerFault(code, msg) => {
            soap_server_fault(host, service, &FaultSpec::en(&code, &msg))
        }
        SoapError::Fatal(msg) => fatal_fault(
            host,
            service,
            globals::error_code().unwrap_or("Server"),
            &msg,
        ),
        SoapError::Coded(code, msg) => fatal_fault(host, service, code, &msg),
    };
    match r {
        Ok(()) => host.bailout(),
        Err(e) => e,
    }
}

fn fatal_fault(
    host: &mut dyn SoapHost,
    service: Option<&Service>,
    code: &str,
    msg: &str,
) -> RtResult<()> {
    let send_errors = service.is_none_or(|s| s.send_errors);
    let mut spec = FaultSpec::en(code, msg);
    if send_errors {
        if let Some(out) = host.ob_contents().filter(|o| !o.is_empty()) {
            spec.detail = Some(Value::from(out.as_slice()));
        }
        host.ob_discard();
    } else {
        spec.string = Some(b"Internal Error".to_vec());
    }
    soap_server_fault(host, service, &spec)
}

/// `_soap_server_exception`: turns a pending exception into a fault
/// response (`SoapFault` and Error); other exceptions propagate.
fn server_exception(
    host: &mut dyn SoapHost,
    this: &Obj,
    service: &Service,
    function: Option<&Function>,
    err: RuntimeError,
) -> RtResult<()> {
    if let RuntimeError::Fatal(m) = err {
        return Err(server_error(
            host,
            this,
            SoapError::Fatal(m),
            Some(service),
            function,
        ));
    }
    if host.is_unwind_exit(&err) {
        return Err(err);
    }
    let Some(exc) = host.exception_object(&err) else {
        return Err(err);
    };
    if host.instance_of(&exc, "SoapFault") {
        return soap_server_fault_ex(host, Some(service), function, &Value::Object(exc), None);
    }
    if host.instance_of(&exc, "Error") {
        let msg = if service.send_errors {
            let m = host.read_slot(&exc, "message").unwrap_or_default();
            m.to_zstr(host)?.as_bytes().to_vec()
        } else {
            b"Internal Error".to_vec()
        };
        let fault = add_soap_fault(
            host,
            this,
            &FaultSpec::en("Server", &String::from_utf8_lossy(&msg)),
        )?;
        return soap_server_fault_ex(host, Some(service), function, &Value::Object(fault), None);
    }
    Err(err)
}

/// `SoapServer::fault(...)`.
#[allow(clippy::too_many_arguments)]
pub fn fault(
    host: &mut dyn SoapHost,
    this: &Obj,
    code: &[u8],
    string: &[u8],
    actor: Option<&[u8]>,
    details: Option<&Value>,
    name: Option<&[u8]>,
    lang: &[u8],
) -> RtResult<()> {
    let saved = globals::begin("Server", ErrorObject::Server(this.clone()));
    let service = match service_snapshot(host, this) {
        Ok(s) => s,
        Err(e) => {
            globals::end(saved);
            return Err(e);
        }
    };
    let spec = FaultSpec {
        code_ns: None,
        code: Some(code.to_vec()),
        string: Some(string.to_vec()),
        actor: actor.map(<[u8]>::to_vec),
        detail: details.cloned(),
        name: name.map(<[u8]>::to_vec),
        lang: lang.to_vec(),
    };
    let r = soap_server_fault(host, Some(&service), &spec);
    globals::end(saved);
    r?;
    Err(host.bailout())
}

/// `SoapServer::addSoapHeader(SoapHeader $header)`.
pub fn add_soap_header(host: &mut dyn SoapHost, this: &Obj, header: Obj) -> RtResult<()> {
    let headers = service_snapshot(host, this)?.soap_headers;
    let Some(headers) = headers else {
        return Err(RuntimeError::Error(
            "SoapServer::addSoapHeader() may be called only during SOAP request processing".into(),
        ));
    };
    headers.borrow_mut().push(SoapHeaderEntry {
        function: None,
        hdr: None,
        function_name: None,
        must_understand: false,
        parameters: Vec::new(),
        retval: Value::Object(header),
    });
    Ok(())
}

/// `SoapServer::__getLastResponse()`.
pub fn get_last_response(host: &mut dyn SoapHost, this: &Obj) -> RtResult<Value> {
    Ok(match service_snapshot(host, this)?.last_response {
        Some(r) => Value::from(r.as_slice()),
        None => Value::Null,
    })
}

/// `SoapServer::handle(?string $request = null)`.
pub fn handle(host: &mut dyn SoapHost, this: &Obj, request: Option<&[u8]>) -> RtResult<()> {
    let saved = globals::begin("Server", ErrorObject::Server(this.clone()));
    let r = handle_inner(host, this, request);
    globals::end(saved);
    r
}

#[allow(clippy::too_many_lines)]
fn handle_inner(host: &mut dyn SoapHost, this: &Obj, request: Option<&[u8]>) -> RtResult<()> {
    let service = service_snapshot(host, this)?;
    globals::set_soap_version(service.version);
    let bail = |host: &mut dyn SoapHost, e: SoapError, f: Option<&Function>| -> RuntimeError {
        server_error(host, this, e, Some(&service), f)
    };

    let (method, query) = host.request_line();
    if method.as_deref() == Some("GET")
        && query
            .as_deref()
            .is_some_and(|q| q.eq_ignore_ascii_case("wsdl"))
    {
        if let Some(sdl) = &service.sdl {
            host.add_header("Content-Type: text/xml; charset=utf-8");
            match host.load_resource(&sdl.source, None) {
                Ok(data) => host.write_output(&data),
                Err(_) => {
                    return Err(bail(
                        host,
                        SoapError::ServerFault("Server".into(), "Couldn't find WSDL".into()),
                        None,
                    ))
                }
            }
            return Ok(());
        }
        return Err(bail(
            host,
            SoapError::ServerFault(
                "Server".into(),
                "WSDL generation is not supported yet".into(),
            ),
            None,
        ));
    }
    if !host.ob_start() {
        return Err(RuntimeError::Fatal(
            "SoapServer::handle(): ob_start failed".into(),
        ));
    }
    let mut soap_action = None;
    let parsed = if let Some(arg) = request {
        Doc::parse_budgeted(arg, &mut |bytes: usize| host.memory_reserve(bytes))
    } else {
        let Some(body) = host.request_body() else {
            return Ok(());
        };
        let mut body = body;
        if let Some(enc) = host.server_var("HTTP_CONTENT_ENCODING") {
            if matches!(enc.as_slice(), b"gzip" | b"x-gzip" | b"deflate") {
                if let Some(Value::Str(s)) =
                    host.call_function(b"zlib_decode", &[Value::from(body.as_slice())])?
                {
                    body = s.as_bytes().to_vec();
                } else {
                    host.diagnostic(
                        ErrorLevel::Warning,
                        "SoapServer::handle(): Can't uncompress compressed request",
                    )?;
                    return Ok(());
                }
            } else {
                host.diagnostic(
                    ErrorLevel::Warning,
                    &format!(
                        "SoapServer::handle(): Request is compressed with unknown compression '{}'",
                        String::from_utf8_lossy(&enc)
                    ),
                )?;
                return Ok(());
            }
        }
        soap_action = host.server_var("HTTP_SOAPACTION").filter(|a| !a.is_empty());
        Doc::parse_budgeted(&body, &mut |bytes: usize| host.memory_reserve(bytes))
    };
    // The memory limit hit while the request tree was built: the SOAP
    // error handler answers with a fault (`soap_real_error_handler`).
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => return Err(bail(host, SoapError::from(e), None)),
    };
    let Ok(doc_request) = parsed else {
        return Err(bail(
            host,
            SoapError::ServerFault("Client".into(), "Bad Request".into()),
            None,
        ));
    };
    if doc_request.has_dtd() {
        if let Some(env) = doc_request.get_node(doc_request.first_top(), "Envelope", None) {
            match doc_request.ns_href(env) {
                Some(h) if h == SOAP_1_1_ENV_NAMESPACE.as_bytes() => {
                    globals::set_soap_version(SOAP_1_1);
                }
                Some(h) if h == SOAP_1_2_ENV_NAMESPACE.as_bytes() => {
                    globals::set_soap_version(SOAP_1_2);
                }
                _ => {}
            }
        }
        return Err(bail(
            host,
            SoapError::ServerFault("Server".into(), "DTD are not supported by SOAP".into()),
            None,
        ));
    }
    let old_features = globals::with(|g| std::mem::replace(&mut g.features, service.features));
    let r = dispatch(host, this, &service, &doc_request, soap_action.as_deref());
    globals::with(|g| g.features = old_features);
    let _ = with_service(host, this, |s| s.soap_headers = None);
    r
}

fn sdl_function(service: &Service, idx: Option<usize>) -> Option<&Function> {
    idx.and_then(|i| service.sdl.as_deref().map(|s| &s.function_list[i]))
}

#[allow(clippy::too_many_lines)]
fn dispatch(
    host: &mut dyn SoapHost,
    this: &Obj,
    service: &Service,
    doc: &Doc,
    soap_action: Option<&[u8]>,
) -> RtResult<()> {
    let version = globals::soap_version();
    let req = {
        let mut ctx = service.settings(version).make(host);
        deserialize_function_call(&mut ctx, doc, service.actor.as_deref(), soap_action)
    };
    let req = match req {
        Ok(r) => r,
        Err(SoapError::Runtime(e)) => {
            host.ob_discard();
            return server_exception(host, this, service, None, e);
        }
        Err(e) => return Err(server_error(host, this, e, Some(service), None)),
    };
    let function = sdl_function(service, req.function);
    let headers = Rc::new(RefCell::new(req.headers.clone()));
    with_service(host, this, |s| s.soap_headers = Some(Rc::clone(&headers)))?;

    // Resolve the handler.
    let mut soap_obj: Option<Obj> = None;
    match service.service_type {
        ServiceType::Object => soap_obj.clone_from(&service.object),
        ServiceType::Class => {
            if service.persistence == SOAP_PERSISTENCE_SESSION {
                if let Some(Value::Object(o)) = host.session_get_object()? {
                    let class = service.class.clone().unwrap_or_default();
                    if o.class_name().eq_ignore_ascii_case(class.as_bytes()) {
                        soap_obj = Some(o);
                    } else if o
                        .class_name()
                        .eq_ignore_ascii_case(b"__PHP_Incomplete_Class")
                    {
                        return Err(server_error(
                            host,
                            this,
                            SoapError::ServerFault(
                                "Server".into(),
                                "SoapServer class was deserialized from the session prior to loading the class passed to SoapServer::setClass(). Start the session after loading all classes to resolve this issue.".into(),
                            ),
                            Some(service),
                            function,
                        ));
                    }
                }
            }
            if soap_obj.is_none() {
                let class = service.class.clone().unwrap_or_default();
                match host.instantiate(&class, &service.class_args) {
                    Ok(o) => {
                        if service.persistence == SOAP_PERSISTENCE_SESSION {
                            host.session_set_object(Value::Object(o.clone()));
                        }
                        soap_obj = Some(o);
                    }
                    Err(e) => {
                        host.ob_discard();
                        return server_exception(host, this, service, function, e);
                    }
                }
            }
        }
        ServiceType::Functions => {}
    }
    let lookup_function = |host: &mut dyn SoapHost, name: &[u8]| -> Option<ZStr> {
        if service.functions_all {
            host.function_exists(name)
        } else {
            let lc = String::from_utf8_lossy(name).to_ascii_lowercase();
            service.functions.as_ref().and_then(|f| f.get(&lc).cloned())
        }
    };

    // Process headers.
    let n_headers = headers.borrow().len();
    for i in 0..n_headers {
        let h = headers.borrow()[i].clone();
        let Some(fname) = h.function_name.clone() else {
            continue;
        };
        let result = if let Some(o) = &soap_obj {
            match host.call_method(o, &fname, &h.parameters) {
                Ok(MethodCall::Returned(v)) => Ok(Some(v)),
                Ok(MethodCall::NotCallable(_)) => Ok(None),
                Err(e) => Err(e),
            }
        } else {
            match lookup_function(host, &fname) {
                Some(canon) => host.call_function(canon.as_bytes(), &h.parameters),
                None => Ok(None),
            }
        };
        match result {
            Ok(None) => {
                if h.must_understand {
                    return Err(server_error(
                        host,
                        this,
                        SoapError::ServerFault(
                            "MustUnderstand".into(),
                            "Header not understood".into(),
                        ),
                        Some(service),
                        function,
                    ));
                }
                headers.borrow_mut()[i].retval = Value::Null;
            }
            Ok(Some(v)) => {
                if let Value::Object(o) = &v {
                    if host.instance_of(o, "SoapFault") {
                        host.ob_discard();
                        let entry = headers.borrow()[i].clone();
                        return soap_server_fault_ex(
                            host,
                            Some(service),
                            function,
                            &v,
                            Some(&entry),
                        );
                    }
                }
                headers.borrow_mut()[i].retval = v;
            }
            Err(e) => {
                host.ob_discard();
                return server_exception(host, this, service, function, e);
            }
        }
    }

    // Call the function.
    let fname = req.function_name.clone();
    let retval = if let Some(o) = &soap_obj {
        match host.call_method(o, &fname, &req.params) {
            Ok(MethodCall::Returned(v)) => Ok(v),
            Ok(MethodCall::NotCallable(reason)) => Err(RuntimeError::Error(format!(
                "Cannot call method {}::{}(): {reason}",
                o.class_name().to_string_lossy(),
                String::from_utf8_lossy(&fname)
            ))),
            Err(e) => Err(e),
        }
    } else {
        match lookup_function(host, &fname) {
            Some(canon) => host
                .call_function(canon.as_bytes(), &req.params)
                .map(Option::unwrap_or_default),
            None => Err(RuntimeError::Error(format!(
                "Call to undefined function {}()",
                String::from_utf8_lossy(&fname)
            ))),
        }
    };
    let retval = match retval {
        Ok(v) => v,
        Err(e) => {
            if host.is_unwind_exit(&e) {
                return Err(e);
            }
            host.ob_discard();
            return server_exception(host, this, service, function, e);
        }
    };
    if let Value::Object(o) = &retval {
        if host.instance_of(o, "SoapFault") {
            host.ob_discard();
            return soap_server_fault_ex(host, Some(service), function, &retval, None);
        }
    }
    let response_name = if let Some(n) = function.and_then(|f| f.response_name.clone()) {
        n.into_bytes()
    } else {
        let mut n = fname.clone();
        n.extend_from_slice(b"Response");
        n
    };
    let final_headers = headers.borrow().clone();
    let doc_return = {
        let mut ctx = service.settings(req.version).make(host);
        serialize_response_call(
            &mut ctx,
            function,
            Some(&response_name),
            service.uri.as_deref(),
            &retval,
            &final_headers,
            req.version,
        )
    };
    let doc_return = match doc_return {
        Ok(d) => d,
        Err(SoapError::Runtime(e)) => {
            host.ob_discard();
            return server_exception(host, this, service, function, e);
        }
        Err(e) => return Err(server_error(host, this, e, Some(service), function)),
    };
    host.ob_discard();
    if let Some(d) = doc_return {
        let buf = d.dump();
        if req.version == SOAP_1_2 {
            host.add_header("Content-Type: application/soap+xml; charset=utf-8");
        } else {
            host.add_header("Content-Type: text/xml; charset=utf-8");
        }
        if zlib_output_compression(host) {
            host.add_header("Connection: close");
        } else {
            host.add_header(&format!("Content-Length: {}", buf.len()));
        }
        host.write_output(&buf);
        if service.trace {
            with_service(host, this, |s| s.last_response = Some(buf.clone()))?;
        }
    } else {
        host.add_header("HTTP/1.1 202 Accepted");
        host.add_header("Content-Length: 0");
    }
    Ok(())
}
