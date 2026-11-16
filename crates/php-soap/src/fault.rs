// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `SoapFault` construction (`set_soap_fault`, `add_soap_fault`, the
//! `SoapFault` methods).

use php_runtime::{Obj, RtResult, RuntimeError, Value, ZStr};

use crate::consts::*;
use crate::globals;
use crate::host::SoapHost;

/// Fields of a fault to create.
#[derive(Debug, Clone, Default)]
pub struct FaultSpec {
    /// `faultcodens` given explicitly.
    pub code_ns: Option<Vec<u8>>,
    /// `faultcode`.
    pub code: Option<Vec<u8>>,
    /// `faultstring`.
    pub string: Option<Vec<u8>>,
    /// `faultactor`.
    pub actor: Option<Vec<u8>>,
    /// `detail`.
    pub detail: Option<Value>,
    /// `_name`.
    pub name: Option<Vec<u8>>,
    /// `lang`.
    pub lang: Vec<u8>,
}

impl FaultSpec {
    /// A fault with code and string and `lang = "en"` (`add_soap_fault_en`).
    #[must_use]
    pub fn en(code: &str, string: &str) -> Self {
        Self {
            code: Some(code.as_bytes().to_vec()),
            string: Some(string.as_bytes().to_vec()),
            lang: b"en".to_vec(),
            ..Self::default()
        }
    }
}

/// `set_soap_fault`: fills (creating when `obj` is `None`) a `SoapFault`.
pub fn set_soap_fault(host: &mut dyn SoapHost, obj: Option<Obj>, f: &FaultSpec) -> RtResult<Obj> {
    let obj = match obj {
        Some(o) => o,
        None => host.create_object(&ZStr::from("SoapFault"))?,
    };
    let string = Value::from(f.string.as_deref().unwrap_or(b""));
    host.write_slot(&obj, "faultstring", string.clone());
    host.write_slot(&obj, "message", string);
    if let Some(code) = &f.code {
        let version = globals::soap_version();
        let set = |host: &mut dyn SoapHost, c: &[u8], ns: Option<&str>| {
            host.write_slot(&obj, "faultcode", Value::from(c));
            if let Some(ns) = ns {
                host.write_slot(&obj, "faultcodens", Value::from(ns));
            }
        };
        if let Some(ns) = &f.code_ns {
            host.write_slot(&obj, "faultcode", Value::from(code.as_slice()));
            host.write_slot(&obj, "faultcodens", Value::from(ns.as_slice()));
        } else if version == SOAP_1_1 {
            let ns = matches!(
                code.as_slice(),
                b"Client" | b"Server" | b"VersionMismatch" | b"MustUnderstand"
            )
            .then_some(SOAP_1_1_ENV_NAMESPACE);
            set(host, code, ns);
        } else if version == SOAP_1_2 {
            match code.as_slice() {
                b"Client" => set(host, b"Sender", Some(SOAP_1_2_ENV_NAMESPACE)),
                b"Server" => set(host, b"Receiver", Some(SOAP_1_2_ENV_NAMESPACE)),
                b"VersionMismatch" | b"MustUnderstand" | b"DataEncodingUnknown" => {
                    set(host, code, Some(SOAP_1_2_ENV_NAMESPACE));
                }
                _ => set(host, code, None),
            }
        }
    }
    if let Some(actor) = &f.actor {
        host.write_slot(&obj, "faultactor", Value::from(actor.as_slice()));
    }
    if let Some(detail) = &f.detail {
        host.write_slot(&obj, "detail", detail.clone());
    }
    if let Some(name) = &f.name {
        host.write_slot(&obj, "_name", Value::from(name.as_slice()));
    }
    host.write_slot(&obj, "lang", Value::from(f.lang.as_slice()));
    Ok(obj)
}

/// `add_soap_fault_ex`: creates a fault and stores it in the
/// `__soap_fault` property of a client or server.
pub fn add_soap_fault(host: &mut dyn SoapHost, this: &Obj, f: &FaultSpec) -> RtResult<Obj> {
    let fault = set_soap_fault(host, None, f)?;
    host.write_slot(this, "__soap_fault", Value::Object(fault.clone()));
    Ok(fault)
}

/// `SoapFault::__construct`.
#[allow(clippy::too_many_arguments)]
pub fn construct(
    host: &mut dyn SoapHost,
    this: &Obj,
    code: &Value,
    string: &[u8],
    actor: Option<&[u8]>,
    details: Option<&Value>,
    name: Option<&[u8]>,
    header_fault: Option<&Value>,
) -> RtResult<()> {
    let mut code_ns = None;
    let mut fault_code: Option<Vec<u8>> = None;
    if let Value::Object(_)
    | Value::Int(_)
    | Value::Float(_)
    | Value::Bool(_)
    | Value::Resource(_) = code.deref()
    {
        let given = match code.deref() {
            Value::Object(o) => o.class_name().to_string_lossy(),
            other => crate::client_type_name(&other).to_owned(),
        };
        return Err(RuntimeError::TypeError(format!(
            "SoapFault::__construct(): Argument #1 ($code) must be of type array|string|null, {given} given"
        )));
    }
    match code.deref() {
        Value::Str(s) => fault_code = Some(s.as_bytes().to_vec()),
        Value::Array(a) if a.len() == 2 => {
            if let (Some(Value::Str(ns)), Some(Value::Str(c))) = (
                a.get_int(0).map(Value::deref),
                a.get_int(1).map(Value::deref),
            ) {
                code_ns = Some(ns.as_bytes().to_vec());
                fault_code = Some(c.as_bytes().to_vec());
            }
        }
        _ => {}
    }
    let given = !matches!(code.deref(), Value::Null);
    if given && fault_code.as_ref().is_none_or(Vec::is_empty) {
        return Err(RuntimeError::ValueError(
            "SoapFault::__construct(): Argument #1 ($code) is not a valid fault code".into(),
        ));
    }
    // soap_fault_dtor_properties
    for (slot, v) in [
        ("faultstring", Value::from("")),
        ("faultcode", Value::Null),
        ("faultcodens", Value::Null),
        ("faultactor", Value::Null),
        ("detail", Value::Null),
        ("_name", Value::Null),
        ("headerfault", Value::Null),
        ("lang", Value::from("")),
    ] {
        host.write_slot(this, slot, v);
    }
    let spec = FaultSpec {
        code_ns,
        code: fault_code,
        string: Some(string.to_vec()),
        actor: actor.map(<[u8]>::to_vec),
        detail: details.filter(|d| !matches!(d, Value::Null)).cloned(),
        name: name.filter(|n| !n.is_empty()).map(<[u8]>::to_vec),
        lang: b"en".to_vec(),
    };
    set_soap_fault(host, Some(this.clone()), &spec)?;
    if let Some(hf) = header_fault.filter(|h| !matches!(h, Value::Null)) {
        host.write_slot(this, "headerfault", hf.clone());
    }
    Ok(())
}

/// `SoapFault::__toString`.
pub fn to_string(host: &mut dyn SoapHost, this: &Obj) -> RtResult<Vec<u8>> {
    let code = host.read_property(this, b"faultcode")?.unwrap_or_default();
    let string = host
        .read_property(this, b"faultstring")?
        .unwrap_or_default();
    let file = host.read_slot(this, "file").unwrap_or_default();
    let line = host.read_slot(this, "line").unwrap_or_default();
    let trace = match host.call_method(this, b"getTraceAsString", &[])? {
        crate::host::MethodCall::Returned(v) => v,
        crate::host::MethodCall::NotCallable(_) => Value::from(""),
    };
    let code = code.to_zstr(host)?;
    let string = string.to_zstr(host)?;
    let file = file.to_zstr(host)?;
    let line = line.to_long(host)?;
    let trace = trace.to_zstr(host)?;
    let mut out = Vec::new();
    out.extend_from_slice(b"SoapFault exception: [");
    out.extend_from_slice(code.as_bytes());
    out.extend_from_slice(b"] ");
    out.extend_from_slice(string.as_bytes());
    out.extend_from_slice(b" in ");
    out.extend_from_slice(file.as_bytes());
    out.extend_from_slice(format!(":{line}\nStack trace:\n").as_bytes());
    if trace.is_empty() {
        out.extend_from_slice(b"#0 {main}\n");
    } else {
        out.extend_from_slice(trace.as_bytes());
    }
    Ok(out)
}
