//! Constructors of the value classes (`SoapParam`, `SoapHeader`,
//! `SoapVar`) and the global functions.

use php_runtime::{Obj, RtResult, RuntimeError, Value};

use crate::consts::*;
use crate::encoders::default_by_type;
use crate::host::SoapHost;

/// `SoapParam::__construct(mixed $data, string $name)`.
pub fn soap_param_construct(host: &mut dyn SoapHost, this: &Obj, data: &Value, name: &[u8]) -> RtResult<()> {
    if name.is_empty() {
        return Err(RuntimeError::ValueError(
            "SoapParam::__construct(): Argument #2 ($name) cannot be empty".into(),
        ));
    }
    host.write_slot(this, "param_name", Value::from(name));
    host.write_slot(this, "param_data", data.clone());
    Ok(())
}

/// The `$actor` argument of `SoapHeader::__construct`.
#[derive(Debug, Clone)]
pub enum Actor {
    /// A role URI.
    Uri(Vec<u8>),
    /// `SOAP_ACTOR_*`.
    Code(i64),
}

/// `SoapHeader::__construct(...)`.
pub fn soap_header_construct(
    host: &mut dyn SoapHost,
    this: &Obj,
    namespace: &[u8],
    name: &[u8],
    data: Option<&Value>,
    must_understand: bool,
    actor: Option<Actor>,
) -> RtResult<()> {
    if namespace.is_empty() {
        return Err(RuntimeError::ValueError(
            "SoapHeader::__construct(): Argument #1 ($namespace) cannot be empty".into(),
        ));
    }
    if name.is_empty() {
        return Err(RuntimeError::ValueError(
            "SoapHeader::__construct(): Argument #2 ($name) cannot be empty".into(),
        ));
    }
    host.write_slot(this, "namespace", Value::from(namespace));
    host.write_slot(this, "name", Value::from(name));
    if let Some(d) = data {
        host.write_slot(this, "data", d.clone());
    }
    host.write_slot(this, "mustUnderstand", Value::Bool(must_understand));
    match actor {
        Some(Actor::Uri(s)) => {
            if s.len() > 2 {
                host.write_slot(this, "actor", Value::from(s.as_slice()));
            } else {
                return Err(RuntimeError::ValueError(
                    "SoapHeader::__construct(): Argument #5 ($actor) must be longer than 2 characters".into(),
                ));
            }
        }
        Some(Actor::Code(c)) => {
            if matches!(c, SOAP_ACTOR_NEXT | SOAP_ACTOR_NONE | SOAP_ACTOR_UNLIMATERECEIVER) {
                host.write_slot(this, "actor", Value::Int(c));
            } else {
                return Err(RuntimeError::ValueError(
                    "SoapHeader::__construct(): Argument #5 ($actor) must be one of SOAP_ACTOR_NEXT, SOAP_ACTOR_NONE, or SOAP_ACTOR_UNLIMATERECEIVER".into(),
                ));
            }
        }
        None => {}
    }
    Ok(())
}

/// `SoapVar::__construct(...)`.
#[allow(clippy::too_many_arguments)]
pub fn soap_var_construct(
    host: &mut dyn SoapHost,
    this: &Obj,
    data: &Value,
    encoding: Option<i64>,
    type_name: Option<&[u8]>,
    type_namespace: Option<&[u8]>,
    node_name: Option<&[u8]>,
    node_namespace: Option<&[u8]>,
) -> RtResult<()> {
    match encoding {
        None => host.write_slot(this, "enc_type", Value::Int(UNKNOWN_TYPE)),
        Some(t) => {
            if default_by_type(t).is_some() {
                host.write_slot(this, "enc_type", Value::Int(t));
            } else {
                return Err(RuntimeError::ValueError(
                    "SoapVar::__construct(): Argument #2 ($encoding) is not a valid encoding".into(),
                ));
            }
        }
    }
    if !matches!(data, Value::Null) {
        host.write_slot(this, "enc_value", data.clone());
    }
    for (slot, v) in [
        ("enc_stype", type_name),
        ("enc_ns", type_namespace),
        ("enc_name", node_name),
        ("enc_namens", node_namespace),
    ] {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            host.write_slot(this, slot, Value::from(v));
        }
    }
    Ok(())
}

/// `is_soap_fault(mixed $object): bool`.
pub fn is_soap_fault(host: &mut dyn SoapHost, v: &Value) -> bool {
    match v.deref() {
        Value::Object(o) => host.instance_of(&o, "SoapFault"),
        _ => false,
    }
}

/// `use_soap_error_handler(bool $enable = true): bool`.
#[must_use]
pub fn use_soap_error_handler(enable: bool) -> bool {
    crate::globals::use_soap_error_handler(enable)
}
