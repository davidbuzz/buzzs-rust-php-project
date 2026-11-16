// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! SOAP messages: building requests (`serialize_function_call`), parsing
//! responses (`parse_packet_soap`), parsing requests on the server
//! (`deserialize_function_call`) and building responses/faults
//! (`serialize_response_call`).

use php_runtime::array::ArrayKey;
use php_runtime::{Obj, Value, ZArray, ZStr};

use crate::consts::*;
use crate::encoders::{conv, EncRef};
use crate::encoding::{is_class, EncodeCtx};
use crate::error::{server_fault, SoapError, SoapResult};
use crate::fault::{add_soap_fault, FaultSpec};
use crate::sdl::*;
use crate::xml::{Doc, NodeId, Ns};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn env_ns(version: i64) -> (&'static str, &'static str) {
    if version == SOAP_1_2 {
        (SOAP_1_2_ENV_NAMESPACE, SOAP_1_2_ENV_NS_PREFIX)
    } else {
        (SOAP_1_1_ENV_NAMESPACE, SOAP_1_1_ENV_NS_PREFIX)
    }
}

/// `get_param`.
#[must_use]
pub fn get_param<'f>(
    function: Option<&'f Function>,
    name: Option<&[u8]>,
    index: usize,
    response: bool,
) -> Option<&'f Param> {
    let f = function?;
    let params = if response {
        f.response_params.as_ref()
    } else {
        f.request_params.as_ref()
    }?;
    match name {
        Some(n) => params
            .iter()
            .find(|p| p.name.as_deref().is_some_and(|pn| pn.as_bytes() == n)),
        None => params.get(index),
    }
}

/// Builds a new SOAP envelope; returns `(doc, envelope, env ns)`.
fn new_envelope(version: i64) -> (Doc, NodeId, Ns) {
    let mut doc = Doc::new_utf8();
    let env = doc.new_element(b"Envelope");
    let (href, prefix) = env_ns(version);
    let ns = doc
        .new_ns(env, href.as_bytes(), Some(prefix.as_bytes()))
        .unwrap_or_else(|| Ns::new(Some(prefix.as_bytes()), href.as_bytes()));
    doc.set_ns(env, Some(ns.clone()));
    doc.append_top(env);
    (doc, env, ns)
}

/// `set_soap_header_attributes`.
fn set_soap_header_attributes(
    ctx: &mut EncodeCtx<'_>,
    doc: &mut Doc,
    h: NodeId,
    header: &Obj,
    version: i64,
) {
    if matches!(
        ctx.host
            .read_slot(header, "mustUnderstand")
            .map(|v| v.deref()),
        Some(Value::Bool(true))
    ) {
        if version == SOAP_1_1 {
            doc.set_prop(h, b"SOAP-ENV:mustUnderstand", b"1");
        } else {
            doc.set_prop(h, b"env:mustUnderstand", b"true");
        }
    }
    match ctx.host.read_slot(header, "actor").map(|v| v.deref()) {
        Some(Value::Str(s)) => {
            if version == SOAP_1_1 {
                doc.set_prop(h, b"SOAP-ENV:actor", s.as_bytes());
            } else {
                doc.set_prop(h, b"env:role", s.as_bytes());
            }
        }
        Some(Value::Int(i)) => {
            if version == SOAP_1_1 {
                if i == SOAP_ACTOR_NEXT {
                    doc.set_prop(h, b"SOAP-ENV:actor", SOAP_1_1_ACTOR_NEXT.as_bytes());
                }
            } else {
                let role = match i {
                    SOAP_ACTOR_NEXT => Some(SOAP_1_2_ACTOR_NEXT),
                    SOAP_ACTOR_NONE => Some(SOAP_1_2_ACTOR_NONE),
                    SOAP_ACTOR_UNLIMATERECEIVER => Some(SOAP_1_2_ACTOR_UNLIMATERECEIVER),
                    _ => None,
                };
                if let Some(r) = role {
                    doc.set_prop(h, b"env:role", r.as_bytes());
                }
            }
        }
        _ => {}
    }
}

/// `serialize_zval`.
fn serialize_zval(
    ctx: &mut EncodeCtx<'_>,
    doc: &mut Doc,
    val: Option<&Value>,
    param: Option<&Param>,
    param_name: &[u8],
    style: i64,
    parent: NodeId,
) -> SoapResult<Option<NodeId>> {
    let mut enc = None;
    let mut defval = None;
    if let Some(p) = param {
        enc = p.encode;
        if val.is_none() {
            if let (Some(e), Some(sdl)) = (p.element, ctx.sdl.clone()) {
                let el = sdl.ty(e);
                if let Some(f) = &el.fixed {
                    defval = Some(Value::from(f.as_str()));
                } else if let (Some(d), false) = (&el.def, el.nillable) {
                    defval = Some(Value::from(d.as_str()));
                }
            }
        }
    }
    let data = val.cloned().or(defval).unwrap_or(Value::Null);
    let x = ctx.master_to_xml(doc, enc, &data, style, parent)?;
    if let Some(x) = x {
        if doc.name(x) == b"BOGUS" {
            doc.set_name(x, param_name);
        }
    }
    Ok(x)
}

/// `serialize_parameter`.
fn serialize_parameter(
    ctx: &mut EncodeCtx<'_>,
    doc: &mut Doc,
    param: Option<&Param>,
    val: Option<&Value>,
    index: usize,
    name: Option<&[u8]>,
    style: i64,
    parent: NodeId,
) -> SoapResult<Option<NodeId>> {
    let mut val = val.map(Value::deref);
    let mut name: Option<Vec<u8>> = name.map(<[u8]>::to_vec);
    if let Some(Value::Object(o)) = &val {
        if is_class(o, "SoapParam") {
            let pname = ctx.host.read_slot(o, "param_name").map(|v| v.deref());
            let pdata = ctx.host.read_slot(o, "param_data");
            if let (Some(Value::Str(n)), Some(d)) = (pname, pdata) {
                val = Some(d.deref());
                name = Some(n.as_bytes().to_vec());
            }
        }
    }
    let param_name = match param.and_then(|p| p.name.clone()) {
        Some(n) => n.into_bytes(),
        None => name.unwrap_or_else(|| format!("param{index}").into_bytes()),
    };
    serialize_zval(ctx, doc, val.as_ref(), param, &param_name, style, parent)
}

/// How a call without WSDL is encoded (the `style`/`use` options).
#[derive(Debug, Clone, Copy, Default)]
pub struct NonWsdlStyle {
    /// `SoapClient::$style`.
    pub style: Option<i64>,
    /// `SoapClient::$use`.
    pub use_: Option<i64>,
}

/// `serialize_function_call`.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn serialize_function_call(
    ctx: &mut EncodeCtx<'_>,
    function: Option<&Function>,
    function_name: Option<&[u8]>,
    uri: Option<&[u8]>,
    args: &[Value],
    version: i64,
    soap_headers: Option<&[Value]>,
    opts: NonWsdlStyle,
) -> SoapResult<Doc> {
    ctx.reset_ns();
    let (mut doc, envelope, env_nsp) = new_envelope(version);
    let head = soap_headers.map(|_| doc.new_child(envelope, Some(env_nsp.clone()), b"Header"));
    let body = doc.new_child(envelope, Some(env_nsp.clone()), b"Body");
    let sdl = ctx.sdl.clone();
    let soap_fn = function.filter(|f| {
        sdl.as_deref()
            .is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap)
    });
    let mut method = None;
    let style;
    let mut use_;
    let mut hdrs = None;
    if let Some(fnb) = soap_fn.and_then(|f| f.soap.as_ref()) {
        let f = soap_fn.unwrap_or_else(|| unreachable!());
        hdrs = fnb.input.headers.as_ref();
        style = fnb.style;
        use_ = fnb.input.use_;
        if style == SOAP_RPC {
            let ns = match &fnb.input.ns {
                Some(n) => ctx.encode_add_ns(&mut doc, body, n.as_bytes()),
                None => None,
            };
            let name = f.request_name.as_deref().unwrap_or(&f.name);
            method = Some(doc.new_child(body, ns, name.as_bytes()));
        }
    } else {
        style = opts.style.unwrap_or(SOAP_RPC);
        if style == SOAP_RPC {
            let ns = match uri {
                Some(u) => ctx.encode_add_ns(&mut doc, body, u),
                None => None,
            };
            let name: Option<Vec<u8>> = function_name
                .map(<[u8]>::to_vec)
                .or_else(|| {
                    function
                        .and_then(|f| f.request_name.clone())
                        .map(String::into_bytes)
                })
                .or_else(|| function.map(|f| f.name.clone().into_bytes()));
            method = Some(match name {
                Some(n) => doc.new_child(body, ns, &n),
                None => body,
            });
        } else {
            method = Some(body);
        }
        use_ = if opts.use_ == Some(SOAP_LITERAL) {
            SOAP_LITERAL
        } else {
            SOAP_ENCODED
        };
    }
    let is_soap = soap_fn.is_some();
    let n_req = function
        .and_then(|f| f.request_params.as_ref())
        .map_or(0, Vec::len);
    for i in 0..args.len().max(n_req) {
        let parameter = get_param(function, None, i, false);
        let arg = args.get(i);
        if i >= args.len() && function.and_then(|f| f.request_params.as_ref()).is_none() {
            break;
        }
        if style == SOAP_RPC {
            if let Some(m) = method {
                serialize_parameter(ctx, &mut doc, parameter, arg, i, None, use_, m)?;
            }
        } else if style == SOAP_DOCUMENT {
            let param = serialize_parameter(ctx, &mut doc, parameter, arg, i, None, use_, body)?;
            if let (true, Some(p), Some(param)) = (is_soap, parameter, param) {
                if let (Some(e), Some(sdl)) = (p.element, sdl.as_deref()) {
                    let el = sdl.ty(e);
                    let ns = match &el.namens {
                        Some(n) => ctx.encode_add_ns(&mut doc, param, n.as_bytes()),
                        None => None,
                    };
                    doc.set_name(param, el.name_str().as_bytes());
                    doc.set_ns(param, ns);
                }
            }
        }
    }
    if let (Some(head), Some(headers)) = (head, soap_headers) {
        for header in headers {
            let Value::Object(h) = header.deref() else {
                continue;
            };
            if !ctx.host.instance_of(&h, "SoapHeader") {
                continue;
            }
            let name = ctx.host.read_slot(&h, "name").map(|v| v.deref());
            let ns = ctx.host.read_slot(&h, "namespace").map(|v| v.deref());
            let (Some(Value::Str(name)), Some(Value::Str(ns))) = (name, ns) else {
                continue;
            };
            let mut hdr_use = SOAP_LITERAL;
            let mut enc = None;
            if let Some(hdrs) = hdrs {
                let key = format!("{}:{}", lossy(ns.as_bytes()), lossy(name.as_bytes()));
                if let Some(hdr) = hdrs.get(&key) {
                    hdr_use = hdr.use_;
                    enc = hdr.encode;
                    if hdr_use == SOAP_ENCODED {
                        use_ = SOAP_ENCODED;
                    }
                }
            }
            let data = ctx.host.read_slot(&h, "data").map(|v| v.deref());
            let node = match data {
                Some(d) if !matches!(d, Value::Null) => {
                    let x = ctx.master_to_xml(&mut doc, enc, &d, hdr_use, head)?;
                    if let Some(x) = x {
                        doc.set_name(x, name.as_bytes());
                    }
                    x
                }
                _ => {
                    let x = doc.new_element(name.as_bytes());
                    Some(doc.append_child(head, x))
                }
            };
            if let Some(node) = node {
                let nsp = ctx.encode_add_ns(&mut doc, node, ns.as_bytes());
                doc.set_ns(node, nsp);
                set_soap_header_attributes(ctx, &mut doc, node, &h, version);
            }
        }
    }
    if use_ == SOAP_ENCODED {
        doc.new_ns(envelope, XSD_NAMESPACE.as_bytes(), Some(b"xsd"));
        if version == SOAP_1_1 {
            doc.new_ns(
                envelope,
                SOAP_1_1_ENC_NAMESPACE.as_bytes(),
                Some(b"SOAP-ENC"),
            );
            doc.set_ns_prop(
                envelope,
                Some(env_nsp),
                b"encodingStyle",
                SOAP_1_1_ENC_NAMESPACE.as_bytes(),
            );
        } else {
            doc.new_ns(envelope, SOAP_1_2_ENC_NAMESPACE.as_bytes(), Some(b"enc"));
            if let Some(m) = method {
                doc.set_ns_prop(
                    m,
                    Some(env_nsp),
                    b"encodingStyle",
                    SOAP_1_2_ENC_NAMESPACE.as_bytes(),
                );
            }
        }
    }
    ctx.finish();
    if let Some(e) = ctx.take_pending_error() {
        return Err(SoapError::Runtime(e));
    }
    Ok(doc)
}

/// Result of [`parse_packet_soap`].
#[derive(Debug, Clone)]
pub enum PacketResult {
    /// The decoded return value and the output headers.
    Ok(Value, ZArray),
    /// A fault was stored in `__soap_fault`.
    Fault,
}

fn fault(ctx: &mut EncodeCtx<'_>, this: &Obj, code: &str, msg: &str) -> SoapResult<PacketResult> {
    add_soap_fault(&mut *ctx.host, this, &FaultSpec::en(code, msg))?;
    Ok(PacketResult::Fault)
}

/// Checks the attributes of Envelope/Body/Header (shared by client and
/// server); returns the fault message.
fn check_env_attrs(
    doc: &Doc,
    node: NodeId,
    version: i64,
    what: &str,
    body: bool,
) -> Option<&'static str> {
    for a in doc.attrs(node) {
        match &a.ns {
            None if (!body || version == SOAP_1_2) => {
                return Some(match what {
                    "Envelope" => {
                        "A SOAP Envelope element cannot have non Namespace qualified attributes"
                    }
                    "Body" => "A SOAP Body element cannot have non Namespace qualified attributes",
                    _ => "A SOAP Header element cannot have non Namespace qualified attributes",
                });
            }
            Some(ns)
                if a.name == b"encodingStyle" && ns.href == SOAP_1_2_ENV_NAMESPACE.as_bytes() =>
            {
                if version == SOAP_1_2 {
                    return Some(match what {
                        "Envelope" => "encodingStyle cannot be specified on the Envelope",
                        "Body" => "encodingStyle cannot be specified on the Body",
                        _ => "encodingStyle cannot be specified on the Header",
                    });
                } else if a.value != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                    return Some("Unknown data encoding style");
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_non_elements(doc: &Doc, mut trav: Option<NodeId>) -> Option<NodeId> {
    while let Some(t) = trav {
        if doc.is_element(t) {
            return Some(t);
        }
        trav = doc.next(t);
    }
    None
}

/// `parse_packet_soap`: decodes a response for `function`.
#[allow(clippy::too_many_lines)]
pub fn parse_packet_soap(
    ctx: &mut EncodeCtx<'_>,
    this: &Obj,
    buffer: &[u8],
    function: Option<&Function>,
) -> SoapResult<PacketResult> {
    if buffer.is_empty() {
        return Ok(PacketResult::Ok(Value::Null, ZArray::new()));
    }
    let Ok(response) = Doc::parse(buffer) else {
        return fault(ctx, this, "Client", "looks like we got no XML document");
    };
    if response.has_dtd() {
        return fault(ctx, this, "Client", "DTD are not supported by SOAP");
    }
    let mut env = None;
    let mut envelope_ns = SOAP_1_1_ENV_NAMESPACE;
    let mut version = SOAP_1_1;
    for t in response.top() {
        if !response.is_element(t) {
            continue;
        }
        if env.is_none() && response.node_is(t, Some("Envelope"), Some(SOAP_1_1_ENV_NAMESPACE)) {
            env = Some(t);
        } else if env.is_none()
            && response.node_is(t, Some("Envelope"), Some(SOAP_1_2_ENV_NAMESPACE))
        {
            env = Some(t);
            envelope_ns = SOAP_1_2_ENV_NAMESPACE;
            version = SOAP_1_2;
        } else {
            return fault(ctx, this, "VersionMismatch", "Wrong Version");
        }
    }
    let Some(env) = env else {
        return fault(
            ctx,
            this,
            "Client",
            "looks like we got XML without \"Envelope\" element",
        );
    };
    if let Some(msg) = check_env_attrs(&response, env, version, "Envelope", false) {
        return fault(ctx, this, "Client", msg);
    }
    let mut trav = skip_non_elements(&response, response.first_child(env));
    let mut head = None;
    if let Some(t) = trav.filter(|&t| response.node_is(t, Some("Header"), Some(envelope_ns))) {
        head = Some(t);
        trav = response.next(t);
    }
    trav = skip_non_elements(&response, trav);
    let mut body = None;
    if let Some(t) = trav.filter(|&t| response.node_is(t, Some("Body"), Some(envelope_ns))) {
        body = Some(t);
        trav = response.next(t);
    }
    trav = skip_non_elements(&response, trav);
    let Some(body) = body else {
        return fault(
            ctx,
            this,
            "Client",
            "Body must be present in a SOAP envelope",
        );
    };
    if let Some(msg) = check_env_attrs(&response, body, version, "Body", true) {
        return fault(ctx, this, "Client", msg);
    }
    if trav.is_some() && version == SOAP_1_2 {
        return fault(
            ctx,
            this,
            "Client",
            "A SOAP 1.2 envelope can contain only Header and Body",
        );
    }
    if let Some(h) = head {
        if let Some(msg) = check_env_attrs(&response, h, version, "Header", false) {
            return fault(ctx, this, "Client", msg);
        }
    }

    if let Some(f) = response.get_node(response.first_child(body), "Fault", Some(envelope_ns)) {
        let mut spec = FaultSpec {
            lang: Vec::new(),
            ..FaultSpec::default()
        };
        let string_enc = conv(IS_STRING);
        let as_string = |ctx: &mut EncodeCtx<'_>, n: NodeId| -> SoapResult<Vec<u8>> {
            let v = ctx.master_to_zval(Some(string_enc), &response, n)?;
            Ok(v.to_zstr(&mut *ctx.host)?.as_bytes().to_vec())
        };
        if version == SOAP_1_1 {
            if let Some(t) = response.get_node(response.first_child(f), "faultcode", None) {
                if let Some(c) = response.first_child(t) {
                    spec.code = Some(response.content(c).to_vec());
                }
            }
            if let Some(t) = response.get_node(response.first_child(f), "faultstring", None) {
                if response.first_child(t).is_some() {
                    spec.string = Some(as_string(ctx, t)?);
                }
            }
            if let Some(t) = response.get_node(response.first_child(f), "faultactor", None) {
                if response.first_child(t).is_some() {
                    spec.actor = Some(as_string(ctx, t)?);
                }
            }
            if let Some(t) = response.get_node(response.first_child(f), "detail", None) {
                spec.detail = Some(ctx.master_to_zval(None, &response, t)?);
            }
        } else {
            if let Some(t) = response.get_node(response.first_child(f), "Code", None) {
                if response.first_child(t).is_some() {
                    if let Some(v) = response.get_node(response.first_child(t), "Value", None) {
                        if let Some(c) = response.first_child(v) {
                            spec.code = Some(response.content(c).to_vec());
                        }
                    }
                }
            }
            if let Some(t) = response.get_node(response.first_child(f), "Reason", None) {
                if response.first_child(t).is_some() {
                    if let Some(tx) = response.get_node(response.first_child(t), "Text", None) {
                        if response.first_child(tx).is_some() {
                            spec.string = Some(as_string(ctx, tx)?);
                            if let Some(l) = response.attr_value(tx, "lang", Some(XML_NAMESPACE)) {
                                spec.lang = l.to_vec();
                            }
                        }
                    }
                }
            }
            if let Some(t) = response.get_node(response.first_child(f), "Detail", None) {
                spec.detail = Some(ctx.master_to_zval(None, &response, t)?);
            }
        }
        add_soap_fault(&mut *ctx.host, this, &spec)?;
        return Ok(PacketResult::Fault);
    }

    let mut ret = ZArray::new();
    let mut param_count = 0;
    let mut hdrs = None;
    let resp = skip_non_elements(&response, response.first_child(body));
    let sdl = ctx.sdl.clone();
    if let Some(resp) = resp {
        let soap_fn = function.filter(|f| {
            f.soap.is_some()
                && sdl
                    .as_deref()
                    .is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap)
        });
        if let (Some(f), Some(fnb), Some(sdl)) = (
            soap_fn,
            soap_fn.and_then(|f| f.soap.as_ref()),
            sdl.as_deref(),
        ) {
            hdrs = fnb.output.headers.as_ref();
            if let Some(params) = &f.response_params {
                let res_count = params.len();
                for param in params {
                    let (name, ns): (String, Option<String>) = if fnb.style == SOAP_DOCUMENT {
                        match param.element {
                            Some(e) => (sdl.ty(e).name_str().to_owned(), sdl.ty(e).namens.clone()),
                            None => (param.name.clone().unwrap_or_default(), None),
                        }
                    } else {
                        (f.response_name.clone().unwrap_or_default(), None)
                    };
                    let mut cur = response.get_node(Some(resp), &name, ns.as_deref());
                    if cur.is_none() {
                        cur = response.get_node(Some(resp), &name, None);
                    }
                    if cur.is_none() && fnb.style == SOAP_RPC {
                        cur = Some(resp);
                    }
                    let mut val = None;
                    if let Some(c) = cur {
                        if fnb.style == SOAP_DOCUMENT {
                            val = Some(c);
                        } else {
                            let pname = param.name.clone().unwrap_or_default();
                            val = response.get_node(response.first_child(c), &pname, None);
                            if res_count == 1 {
                                if val.is_none() {
                                    val =
                                        response.get_node(response.first_child(c), "return", None);
                                }
                                if val.is_none() {
                                    val =
                                        response.get_node(response.first_child(c), "result", None);
                                }
                                if val.is_none() {
                                    if let Some(only) = response.first_child(c) {
                                        if response.next(only).is_none() {
                                            val = Some(only);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let tmp = match val {
                        None => Value::Null,
                        Some(v) => ctx.master_to_zval(param.encode, &response, v)?,
                    };
                    let key = param.name.clone().unwrap_or_default();
                    ret.make_mut()
                        .insert(ArrayKey::from_bytes(key.as_bytes()), tmp);
                    param_count += 1;
                }
            }
        } else {
            let mut val = response.first_child(resp);
            while let Some(v0) = val {
                let Some(v) = skip_non_elements(&response, Some(v0)) else {
                    break;
                };
                if !response.node_is(v, Some("result"), Some(RPC_SOAP12_NAMESPACE)) {
                    let tmp = ctx.master_to_zval(None, &response, v)?;
                    let name = response.name(v).to_vec();
                    if name.is_empty() {
                        let _ = ret.make_mut().push(tmp);
                    } else if let Some(existing) = ret
                        .make_mut()
                        .get_mut(&ArrayKey::Str(ZStr::from(name.as_slice())))
                    {
                        if let Value::Array(a) = existing {
                            let _ = a.make_mut().push(tmp);
                        }
                    } else if response
                        .next(v)
                        .is_some_and(|n| response.get_node(Some(n), &lossy(&name), None).is_some())
                    {
                        ret.make_mut().insert(
                            ArrayKey::from_bytes(&name),
                            Value::Array(ZArray::from_values([tmp])),
                        );
                    } else {
                        ret.make_mut().insert(ArrayKey::from_bytes(&name), tmp);
                    }
                    param_count += 1;
                }
                val = response.next(v);
            }
        }
    }
    let ret = match param_count {
        0 => Value::Null,
        1 => ret.values().next().cloned().unwrap_or(Value::Null),
        _ => Value::Array(ret),
    };
    let mut out_headers = ZArray::new();
    if let Some(h) = head {
        for t in response.children(h) {
            if !response.is_element(t) {
                continue;
            }
            let mut enc: Option<EncRef> = None;
            if let Some(hdrs) = hdrs {
                let mut key = String::new();
                if let Some(href) = response.ns_href(t) {
                    key.push_str(&lossy(href));
                    key.push(':');
                }
                key.push_str(&lossy(response.name(t)));
                if let Some(hdr) = hdrs.get(&key) {
                    enc = hdr.encode;
                }
            }
            let v = ctx.master_to_zval(enc, &response, t)?;
            out_headers
                .make_mut()
                .insert(ArrayKey::from_bytes(response.name(t)), v);
        }
    }
    Ok(PacketResult::Ok(ret, out_headers))
}

/// A header of an incoming request (`soapHeader`).
#[derive(Debug, Clone)]
pub struct SoapHeaderEntry {
    /// Index of the matching function (header handled by a method).
    pub function: Option<usize>,
    /// The header binding (`h->hdr`): `(input headers key)`.
    pub hdr: Option<HeaderBinding>,
    /// Name of the method to call (`function_name`); `None` for headers
    /// added with `SoapServer::addSoapHeader`.
    pub function_name: Option<Vec<u8>>,
    /// `mustUnderstand`.
    pub must_understand: bool,
    /// Decoded parameters.
    pub parameters: Vec<Value>,
    /// The handler's return value (or the added `SoapHeader`).
    pub retval: Value,
}

/// A decoded request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Index of the function in the SDL.
    pub function: Option<usize>,
    /// The function to call.
    pub function_name: Vec<u8>,
    /// Decoded parameters.
    pub params: Vec<Value>,
    /// SOAP version of the envelope.
    pub version: i64,
    /// Headers.
    pub headers: Vec<SoapHeaderEntry>,
}

fn function_index(sdl: &Sdl, f: &Function) -> Option<usize> {
    sdl.function_list.iter().position(|x| std::ptr::eq(x, f))
}

/// `get_doc_function`.
fn get_doc_function<'s>(
    sdl: Option<&'s Sdl>,
    doc: &Doc,
    params: Option<NodeId>,
) -> Option<&'s Function> {
    let sdl = sdl?;
    for f in sdl.functions() {
        let Some(fnb) = f
            .soap
            .as_ref()
            .filter(|_| sdl.binding_of(f).binding_type == BindingType::Soap)
        else {
            continue;
        };
        if fnb.style != SOAP_DOCUMENT {
            continue;
        }
        let req = f.request_params.as_ref();
        match params {
            None => {
                if req.is_none_or(Vec::is_empty) {
                    return Some(f);
                }
            }
            Some(p) => {
                let Some(req) = req.filter(|r| !r.is_empty()) else {
                    continue;
                };
                let mut ok = true;
                let mut node = Some(p);
                for param in req {
                    let Some(n) = node else {
                        ok = false;
                        break;
                    };
                    if let Some(e) = param.element {
                        let el = sdl.ty(e);
                        if el.name_str().as_bytes() != doc.name(n) {
                            ok = false;
                            break;
                        }
                        match (&el.namens, doc.ns_href(n)) {
                            (Some(a), Some(b)) => {
                                if a.as_bytes() != b {
                                    ok = false;
                                    break;
                                }
                            }
                            (None, None) => {}
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    } else if param.name.as_deref().unwrap_or("").as_bytes() != doc.name(n) {
                        ok = false;
                        break;
                    }
                    node = doc.next(n);
                }
                if ok {
                    return Some(f);
                }
            }
        }
    }
    None
}

/// `find_function`.
fn find_function<'s>(
    sdl: Option<&'s Sdl>,
    doc: &Doc,
    func: NodeId,
) -> (Option<&'s Function>, Vec<u8>) {
    let mut function = sdl.and_then(|s| s.get_function(doc.name(func)));
    if let (Some(f), Some(s)) = (function, sdl) {
        if s.binding_of(f).binding_type == BindingType::Soap {
            if let Some(fnb) = &f.soap {
                if fnb.style == SOAP_DOCUMENT
                    && (doc.first_child(func).is_some()
                        || f.request_params.as_ref().is_some_and(|p| !p.is_empty()))
                {
                    function = None;
                }
            }
        }
    }
    if sdl.is_some() && function.is_none() {
        function = get_doc_function(sdl, doc, Some(func));
    }
    let name = match function {
        Some(f) => f.name.clone().into_bytes(),
        None => doc.name(func).to_vec(),
    };
    (function, name)
}

/// `find_function_using_soap_action`.
fn find_function_using_soap_action<'s>(
    sdl: Option<&'s Sdl>,
    soap_action: &[u8],
) -> Option<&'s Function> {
    let sdl = sdl?;
    let mut action = soap_action;
    if action.first() == Some(&b'"') {
        if action.len() < 2 || action.last() != Some(&b'"') {
            return None;
        }
        action = &action[1..action.len() - 1];
    }
    if action.is_empty() {
        return None;
    }
    sdl.functions().find(|f| {
        sdl.binding_of(f).binding_type == BindingType::Soap
            && f.soap
                .as_ref()
                .and_then(|b| b.soap_action.as_ref())
                .is_some_and(|a| a.as_bytes() == action)
    })
}

/// `deserialize_parameters`.
fn deserialize_parameters(
    ctx: &mut EncodeCtx<'_>,
    doc: &Doc,
    params: Option<NodeId>,
    function: Option<&Function>,
) -> SoapResult<Vec<Value>> {
    let sdl = ctx.sdl.clone();
    if let Some(f) = function {
        let Some(req) = &f.request_params else {
            return Ok(Vec::new());
        };
        let use_names = req.iter().any(|p| {
            p.name
                .as_deref()
                .is_some_and(|n| doc.get_node(params, n, None).is_some())
        });
        if use_names {
            let mut out = Vec::with_capacity(req.len());
            for p in req {
                let n = p.name.as_deref().unwrap_or("");
                out.push(match doc.get_node(params, n, None) {
                    None => Value::Null,
                    Some(v) => ctx.master_to_zval(p.encode, doc, v)?,
                });
            }
            return Ok(out);
        }
    }
    let mut out = Vec::new();
    let mut num = 0;
    if let Some(p) = params {
        num = doc
            .siblings_from(Some(p))
            .filter(|&t| doc.is_element(t))
            .count();
        let doc_style_no_params = num == 1
            && function.is_some_and(|f| {
                sdl.as_deref()
                    .is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap)
                    && f.soap.as_ref().is_some_and(|b| b.style == SOAP_DOCUMENT)
                    && f.request_params.as_ref().is_none_or(Vec::is_empty)
                    && doc.name(p) == f.name.as_bytes()
            });
        if doc_style_no_params {
            num = 0;
        } else if num > 0 {
            let mut cur = 0;
            for t in doc.siblings_from(Some(p)) {
                if cur >= num {
                    break;
                }
                if !doc.is_element(t) {
                    continue;
                }
                let mut enc = None;
                if let Some(f) = function {
                    match f.request_params.as_ref().and_then(|r| r.get(cur)) {
                        Some(param) => enc = param.encode,
                        None => return Err(server_fault("Client", "Error cannot find parameter")),
                    }
                }
                out.push(ctx.master_to_zval(enc, doc, t)?);
                cur += 1;
            }
        }
    }
    if num > out.len() {
        return Err(server_fault("Client", "Missing parameter"));
    }
    Ok(out)
}

/// `deserialize_function_call`.
#[allow(clippy::too_many_lines)]
pub fn deserialize_function_call(
    ctx: &mut EncodeCtx<'_>,
    request: &Doc,
    actor: Option<&[u8]>,
    soap_action: Option<&[u8]>,
) -> SoapResult<Request> {
    ctx.reset_ns();
    let sdl = ctx.sdl.clone();
    let sdl_ref = sdl.as_deref();
    let mut env = None;
    let mut version = SOAP_1_1;
    let mut envelope_ns = SOAP_1_1_ENV_NAMESPACE;
    for t in request.top() {
        if !request.is_element(t) {
            continue;
        }
        if request.node_is(t, Some("Envelope"), Some(SOAP_1_1_ENV_NAMESPACE)) {
            crate::globals::set_soap_version(SOAP_1_1);
            ctx.soap_version = SOAP_1_1;
            env = Some(t);
            break;
        }
        if request.node_is(t, Some("Envelope"), Some(SOAP_1_2_ENV_NAMESPACE)) {
            version = SOAP_1_2;
            envelope_ns = SOAP_1_2_ENV_NAMESPACE;
            crate::globals::set_soap_version(SOAP_1_2);
            ctx.soap_version = SOAP_1_2;
            env = Some(t);
            break;
        }
        return Err(server_fault("VersionMismatch", "Wrong Version"));
    }
    let Some(env) = env else {
        return Err(server_fault(
            "Client",
            "looks like we got XML without \"Envelope\" element",
        ));
    };
    for a in request.attrs(env) {
        match &a.ns {
            None => {
                return Err(server_fault(
                    "Client",
                    "A SOAP Envelope element cannot have non Namespace qualified attributes",
                ))
            }
            Some(ns)
                if a.name == b"encodingStyle" && ns.href == SOAP_1_2_ENV_NAMESPACE.as_bytes() =>
            {
                if version == SOAP_1_2 {
                    return Err(server_fault(
                        "Client",
                        "encodingStyle cannot be specified on the Envelope",
                    ));
                } else if a.value != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                    return Err(server_fault("Client", "Unknown data encoding style"));
                }
            }
            _ => {}
        }
    }
    let mut trav = skip_non_elements(request, request.first_child(env));
    let mut head = None;
    if let Some(t) = trav.filter(|&t| request.node_is(t, Some("Header"), Some(envelope_ns))) {
        head = Some(t);
        trav = request.next(t);
    }
    trav = skip_non_elements(request, trav);
    let mut body = None;
    if let Some(t) = trav.filter(|&t| request.node_is(t, Some("Body"), Some(envelope_ns))) {
        body = Some(t);
        trav = request.next(t);
    }
    trav = skip_non_elements(request, trav);
    let Some(body) = body else {
        return Err(server_fault(
            "Client",
            "Body must be present in a SOAP envelope",
        ));
    };
    for a in request.attrs(body) {
        match &a.ns {
            None if version == SOAP_1_2 => {
                return Err(server_fault(
                    "Client",
                    "A SOAP Body element cannot have non Namespace qualified attributes",
                ));
            }
            Some(ns)
                if a.name == b"encodingStyle" && ns.href == SOAP_1_2_ENV_NAMESPACE.as_bytes() =>
            {
                if version == SOAP_1_2 {
                    return Err(server_fault(
                        "Client",
                        "encodingStyle cannot be specified on the Body",
                    ));
                } else if a.value != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                    return Err(server_fault("Client", "Unknown data encoding style"));
                }
            }
            _ => {}
        }
    }
    if trav.is_some() && version == SOAP_1_2 {
        return Err(server_fault(
            "Client",
            "A SOAP 1.2 envelope can contain only Header and Body",
        ));
    }
    let func = skip_non_elements(request, request.first_child(body));
    let mut function: Option<&Function> = None;
    let mut function_name = Vec::new();
    if let Some(a) = soap_action {
        if let Some(f) = find_function_using_soap_action(sdl_ref, a) {
            function_name = f.name.clone().into_bytes();
            function = Some(f);
        }
    }
    match func {
        None => {
            if function.is_none() {
                match get_doc_function(sdl_ref, request, None) {
                    Some(f) => {
                        function_name = f.name.clone().into_bytes();
                        function = Some(f);
                    }
                    None => {
                        return Err(server_fault(
                            "Client",
                            "looks like we got \"Body\" without function call",
                        ))
                    }
                }
            }
        }
        Some(fnode) => {
            if version == SOAP_1_1 {
                if let Some(v) =
                    request.attr_value(fnode, "encodingStyle", Some(SOAP_1_1_ENV_NAMESPACE))
                {
                    if v != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                        return Err(server_fault("Client", "Unknown Data Encoding Style"));
                    }
                }
            } else if let Some(v) =
                request.attr_value(fnode, "encodingStyle", Some(SOAP_1_2_ENV_NAMESPACE))
            {
                if v != SOAP_1_2_ENC_NAMESPACE.as_bytes() {
                    return Err(server_fault(
                        "DataEncodingUnknown",
                        "Unknown Data Encoding Style",
                    ));
                }
            }
            if function.is_none() {
                let (f, name) = find_function(sdl_ref, request, fnode);
                function = f;
                function_name = name;
            }
            if sdl_ref.is_some() && function.is_none() {
                if version == SOAP_1_2 {
                    return Err(server_fault(
                        "rpc:ProcedureNotPresent",
                        "Procedure not present",
                    ));
                }
                return Err(SoapError::Fatal(format!(
                    "Procedure '{}' not present",
                    lossy(request.name(fnode))
                )));
            }
        }
    }

    let mut headers = Vec::new();
    if let Some(head) = head {
        for a in request.attrs(head) {
            match &a.ns {
                None => {
                    return Err(server_fault(
                        "Client",
                        "A SOAP Header element cannot have non Namespace qualified attributes",
                    ))
                }
                Some(ns)
                    if a.name == b"encodingStyle"
                        && ns.href == SOAP_1_2_ENV_NAMESPACE.as_bytes() =>
                {
                    if version == SOAP_1_2 {
                        return Err(server_fault(
                            "Client",
                            "encodingStyle cannot be specified on the Header",
                        ));
                    } else if a.value != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                        return Err(server_fault("Client", "Unknown data encoding style"));
                    }
                }
                _ => {}
            }
        }
        for hdr_func in request.children(head) {
            if !request.is_element(hdr_func) {
                continue;
            }
            if version == SOAP_1_1 {
                if let Some(v) =
                    request.attr_value(hdr_func, "encodingStyle", Some(SOAP_1_1_ENV_NAMESPACE))
                {
                    if v != SOAP_1_1_ENC_NAMESPACE.as_bytes() {
                        return Err(server_fault("Client", "Unknown Data Encoding Style"));
                    }
                }
                if let Some(v) = request.attr_value(hdr_func, "actor", Some(envelope_ns)) {
                    if v != SOAP_1_1_ACTOR_NEXT.as_bytes() && actor != Some(v) {
                        continue;
                    }
                }
            } else {
                if let Some(v) =
                    request.attr_value(hdr_func, "encodingStyle", Some(SOAP_1_2_ENV_NAMESPACE))
                {
                    if v != SOAP_1_2_ENC_NAMESPACE.as_bytes() {
                        return Err(server_fault(
                            "DataEncodingUnknown",
                            "Unknown Data Encoding Style",
                        ));
                    }
                }
                if let Some(v) = request.attr_value(hdr_func, "role", Some(envelope_ns)) {
                    if v != SOAP_1_2_ACTOR_UNLIMATERECEIVER.as_bytes()
                        && v != SOAP_1_2_ACTOR_NEXT.as_bytes()
                        && actor != Some(v)
                    {
                        continue;
                    }
                }
            }
            let mut must_understand = false;
            if let Some(v) = request.attr_value(hdr_func, "mustUnderstand", Some(envelope_ns)) {
                match v {
                    b"1" | b"true" => must_understand = true,
                    b"0" | b"false" => {}
                    _ => {
                        return Err(server_fault(
                            "Client",
                            "mustUnderstand value is not boolean",
                        ))
                    }
                }
            }
            let (hfunction, hname) = find_function(sdl_ref, request, hdr_func);
            let mut hdr = None;
            if hfunction.is_none() {
                if let (Some(_), Some(f)) = (sdl_ref, function) {
                    if let Some(fnb) = f.soap.as_ref().filter(|_| {
                        sdl_ref.is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap)
                    }) {
                        if let Some(ih) = &fnb.input.headers {
                            let mut key = String::new();
                            if let Some(href) = request.ns_href(hdr_func) {
                                key.push_str(&lossy(href));
                                key.push(':');
                            }
                            key.push_str(&lossy(&hname));
                            hdr = ih.get(&key).cloned();
                        }
                    }
                }
            }
            let parameters = if let Some(h) = &hdr {
                vec![ctx.master_to_zval(h.encode, request, hdr_func)?]
            } else {
                let mut start = Some(hdr_func);
                if let Some(hf) = hfunction {
                    if sdl_ref.is_some_and(|s| s.binding_of(hf).binding_type == BindingType::Soap)
                        && hf.soap.as_ref().is_some_and(|b| b.style == SOAP_RPC)
                    {
                        start = request.first_child(hdr_func);
                    }
                }
                deserialize_parameters(ctx, request, start, hfunction)?
            };
            headers.push(SoapHeaderEntry {
                function: hfunction.and_then(|f| sdl_ref.and_then(|s| function_index(s, f))),
                hdr,
                function_name: Some(hname),
                must_understand,
                parameters,
                retval: Value::Null,
            });
        }
    }

    let mut params_start = func;
    let rpc_like = match function {
        Some(f) if sdl_ref.is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap) => {
            f.soap.as_ref().is_some_and(|b| b.style == SOAP_RPC)
        }
        _ => true,
    };
    if rpc_like {
        params_start = func.and_then(|f| request.first_child(f));
    }
    let params = deserialize_parameters(ctx, request, params_start, function)?;
    ctx.finish();
    Ok(Request {
        function: function.and_then(|f| sdl_ref.and_then(|s| function_index(s, f))),
        function_name,
        params,
        version,
        headers,
    })
}

/// `serialize_response_call2`; returns the use and the method node.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn serialize_response_call2(
    ctx: &mut EncodeCtx<'_>,
    doc: &mut Doc,
    body: NodeId,
    function: Option<&Function>,
    function_name: Option<&[u8]>,
    uri: Option<&[u8]>,
    ret: Option<&Value>,
    version: i64,
    main: bool,
) -> SoapResult<(i64, Option<NodeId>)> {
    let sdl = ctx.sdl.clone();
    let sdl_ref = sdl.as_deref();
    let mut method = None;
    let style;
    let use_;
    let soap_fnb = function.and_then(|f| {
        f.soap
            .as_ref()
            .filter(|_| sdl_ref.is_some_and(|s| s.binding_of(f).binding_type == BindingType::Soap))
    });
    let is_soap_fn = soap_fnb.is_some();
    if let (Some(f), Some(fnb)) = (function, soap_fnb) {
        style = fnb.style;
        use_ = fnb.output.use_;
        if style == SOAP_RPC {
            let ns = match &fnb.output.ns {
                Some(n) => ctx.encode_add_ns(doc, body, n.as_bytes()),
                None => None,
            };
            if let Some(rn) = &f.response_name {
                method = Some(doc.new_child(body, ns, rn.as_bytes()));
            } else if f.response_params.is_some() {
                method = Some(doc.new_child(body, ns, f.name.as_bytes()));
            }
        }
    } else {
        style = if main { SOAP_RPC } else { SOAP_DOCUMENT };
        use_ = if main { SOAP_ENCODED } else { SOAP_LITERAL };
        if style == SOAP_RPC {
            let ns = match uri {
                Some(u) => ctx.encode_add_ns(doc, body, u),
                None => None,
            };
            method = Some(doc.new_child(body, ns, function_name.unwrap_or(b"")));
        }
    }
    let param_count = match function {
        Some(f) => f.response_params.as_ref().map_or(0, Vec::len),
        None => 1,
    };
    if param_count == 1 {
        let parameter = get_param(function, None, 0, true);
        if style == SOAP_RPC {
            if let Some(m) = method {
                if main && version == SOAP_1_2 {
                    let rpc_ns = doc.new_ns(
                        body,
                        RPC_SOAP12_NAMESPACE.as_bytes(),
                        Some(RPC_SOAP12_NS_PREFIX.as_bytes()),
                    );
                    let rpc_result = doc.new_child(m, rpc_ns, b"result");
                    let param =
                        serialize_parameter(ctx, doc, parameter, ret, 0, Some(b"return"), use_, m)?;
                    if let Some(p) = param {
                        let name = doc.name(p).to_vec();
                        doc.set_content(rpc_result, &name);
                    }
                } else {
                    serialize_parameter(ctx, doc, parameter, ret, 0, Some(b"return"), use_, m)?;
                }
            }
        } else {
            let param =
                serialize_parameter(ctx, doc, parameter, ret, 0, Some(b"return"), use_, body)?;
            if let Some(param) = param {
                if is_soap_fn {
                    if let (Some(p), Some(sdl)) = (parameter, sdl_ref) {
                        if let Some(e) = p.element {
                            let el = sdl.ty(e);
                            let ns = match &el.namens {
                                Some(n) => ctx.encode_add_ns(doc, param, n.as_bytes()),
                                None => None,
                            };
                            doc.set_name(param, el.name_str().as_bytes());
                            doc.set_ns(param, ns);
                        }
                    }
                } else if doc.name(param) == b"return" {
                    let ns = match uri {
                        Some(u) => ctx.encode_add_ns(doc, param, u),
                        None => None,
                    };
                    doc.set_name(param, function_name.unwrap_or(b""));
                    doc.set_ns(param, ns);
                }
            }
        }
    } else if param_count > 1 {
        if let Some(Value::Array(a)) = ret.map(Value::deref) {
            for (i, (k, data)) in a.iter().enumerate() {
                let (pname, pidx) = match k {
                    ArrayKey::Str(s) => (Some(s.as_bytes().to_vec()), i),
                    ArrayKey::Int(n) => (None, usize::try_from(*n).unwrap_or(usize::MAX)),
                };
                let parameter = get_param(function, pname.as_deref(), pidx, true);
                if style == SOAP_RPC {
                    if let Some(m) = method {
                        serialize_parameter(
                            ctx,
                            doc,
                            parameter,
                            Some(data),
                            i,
                            pname.as_deref(),
                            use_,
                            m,
                        )?;
                    }
                } else {
                    let param = serialize_parameter(
                        ctx,
                        doc,
                        parameter,
                        Some(data),
                        i,
                        pname.as_deref(),
                        use_,
                        body,
                    )?;
                    if let (true, Some(p), Some(param), Some(sdl)) =
                        (is_soap_fn, parameter, param, sdl_ref)
                    {
                        if let Some(e) = p.element {
                            let el = sdl.ty(e);
                            let ns = match &el.namens {
                                Some(n) => ctx.encode_add_ns(doc, param, n.as_bytes()),
                                None => None,
                            };
                            doc.set_name(param, el.name_str().as_bytes());
                            doc.set_ns(param, ns);
                        }
                    }
                }
            }
        }
    }
    if use_ == SOAP_ENCODED && version == SOAP_1_2 {
        if let Some(m) = method {
            let body_ns = doc.ns(body).cloned();
            doc.set_ns_prop(
                m,
                body_ns,
                b"encodingStyle",
                SOAP_1_2_ENC_NAMESPACE.as_bytes(),
            );
        }
    }
    Ok((use_, method))
}

/// Read a `SoapHeader` object's namespace/name/data.
fn header_parts(ctx: &mut EncodeCtx<'_>, h: &Obj) -> (Option<ZStr>, Option<ZStr>, Option<Value>) {
    let ns = match ctx.host.read_slot(h, "namespace").map(|v| v.deref()) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let name = match ctx.host.read_slot(h, "name").map(|v| v.deref()) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let data = ctx
        .host
        .read_slot(h, "data")
        .map(|v| v.deref())
        .filter(|v| !matches!(v, Value::Null));
    (ns, name, data)
}

/// `serialize_response_call`; `None` for a one-way operation without
/// response.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn serialize_response_call(
    ctx: &mut EncodeCtx<'_>,
    function: Option<&Function>,
    function_name: Option<&[u8]>,
    uri: Option<&[u8]>,
    ret: &Value,
    headers: &[SoapHeaderEntry],
    version: i64,
) -> SoapResult<Option<Doc>> {
    ctx.reset_ns();
    let sdl = ctx.sdl.clone();
    let sdl_ref = sdl.as_deref();
    let (mut doc, envelope, ns) = new_envelope(version);
    let mut use_ = SOAP_LITERAL;
    let mut head = None;
    let body;
    let is_fault = matches!(ret, Value::Object(o) if ctx.host.instance_of(o, "SoapFault"));
    let fn_of = |idx: Option<usize>| idx.and_then(|i| sdl_ref.map(|s| &s.function_list[i]));
    if let (true, Value::Object(fault)) = (is_fault, ret) {
        let headerfault = ctx
            .host
            .read_slot(fault, "headerfault")
            .map(|v| v.deref())
            .filter(|v| !matches!(v, Value::Null));
        if let (Some(first), Some(hf)) = (headers.first(), headerfault) {
            let mut hdr_enc = None;
            let mut hdr_use = SOAP_LITERAL;
            let mut hdr_ret = Some(hf.clone());
            let mut hdr_ns: Option<Vec<u8>> = first
                .hdr
                .as_ref()
                .and_then(|h| h.ns.clone())
                .map(String::into_bytes);
            let mut hdr_name: Option<Vec<u8>> = first.function_name.clone();
            let h = doc.new_child(envelope, Some(ns.clone()), b"Header");
            head = Some(h);
            if let Value::Object(ho) = &hf {
                if ctx.host.instance_of(ho, "SoapHeader") {
                    let (hns, hname, hdata) = header_parts(ctx, ho);
                    let mut key = String::new();
                    if let Some(n) = &hns {
                        key.push_str(&lossy(n.as_bytes()));
                        key.push(':');
                        hdr_ns = Some(n.as_bytes().to_vec());
                    }
                    if let Some(n) = &hname {
                        key.push_str(&lossy(n.as_bytes()));
                        hdr_name = Some(n.as_bytes().to_vec());
                    }
                    if let Some(hb) = first
                        .hdr
                        .as_ref()
                        .and_then(|h| h.headerfaults.as_ref())
                        .and_then(|f| f.get(&key))
                    {
                        hdr_enc = hb.encode;
                        hdr_use = hb.use_;
                    }
                    hdr_ret = hdata;
                }
            }
            if let Some(hfn) = fn_of(first.function) {
                let (u, _) = serialize_response_call2(
                    ctx,
                    &mut doc,
                    h,
                    Some(hfn),
                    first.function_name.as_deref(),
                    uri,
                    hdr_ret.as_ref(),
                    version,
                    false,
                )?;
                // The use is reset to literal for the fault body below.
                let _ = u;
            } else if let Some(x) = ctx.master_to_xml(
                &mut doc,
                hdr_enc,
                &hdr_ret.unwrap_or(Value::Null),
                hdr_use,
                h,
            )? {
                if let Some(n) = &hdr_name {
                    doc.set_name(x, n);
                }
                if let Some(n) = &hdr_ns {
                    let nsp = ctx.encode_add_ns(&mut doc, x, n);
                    doc.set_ns(x, nsp);
                }
            }
        }
        body = doc.new_child(envelope, Some(ns.clone()), b"Body");
        let param = doc.new_child(body, Some(ns.clone()), b"Fault");
        let mut fault_ns: Option<Vec<u8>> =
            match ctx.host.read_slot(fault, "faultcodens").map(|v| v.deref()) {
                Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                _ => None,
            };
        use_ = SOAP_LITERAL;
        let mut sfault: Option<&Fault> = None;
        if let Some(Value::Str(n)) = ctx.host.read_slot(fault, "_name").map(|v| v.deref()) {
            if let Some(f) = function
                .and_then(|f| f.faults.as_ref())
                .and_then(|fs| fs.get(&lossy(n.as_bytes())))
            {
                sfault = Some(f);
                if let Some(fb) = &f.binding {
                    use_ = fb.use_;
                    if fault_ns.is_none() {
                        fault_ns = fb.ns.clone().map(String::into_bytes);
                    }
                }
            }
        } else if let Some(fs) = function
            .and_then(|f| f.faults.as_ref())
            .filter(|fs| fs.len() == 1)
        {
            let f = fs.first();
            sfault = f;
            if let Some(fb) = f.and_then(|f| f.binding.as_ref()) {
                use_ = fb.use_;
                if fault_ns.is_none() {
                    fault_ns = fb.ns.clone().map(String::into_bytes);
                }
            }
        }
        if fault_ns.is_none() {
            if let (Some(f), Some(sdl)) = (sfault, sdl_ref) {
                if f.details.len() == 1 {
                    if let Some(e) = f.details[0].element {
                        fault_ns = sdl.ty(e).namens.clone().map(String::into_bytes);
                    }
                }
            }
        }
        let string_enc = conv(IS_STRING);
        let detail_name: &[u8];
        if version == SOAP_1_1 {
            if let Some(Value::Str(code)) =
                ctx.host.read_slot(fault, "faultcode").map(|v| v.deref())
            {
                let node = doc.add_element(param, b"faultcode");
                let escaped = html_escape(code.as_bytes());
                if let Some(fns) = &fault_ns {
                    let nsp = ctx.encode_add_ns(&mut doc, node, fns);
                    let mut qn = Vec::new();
                    if let Some(p) = nsp.and_then(|n| n.prefix) {
                        qn.extend_from_slice(&p);
                        qn.push(b':');
                    }
                    qn.extend_from_slice(&escaped);
                    doc.set_content(node, &qn);
                } else {
                    doc.set_content(node, &escaped);
                }
            }
            if let Some(s) = ctx
                .host
                .read_slot(fault, "faultstring")
                .map(|v| v.deref())
                .filter(|v| matches!(v, Value::Str(_)))
            {
                if let Some(n) =
                    ctx.master_to_xml(&mut doc, Some(string_enc), &s, SOAP_LITERAL, param)?
                {
                    doc.set_name(n, b"faultstring");
                }
            }
            if let Some(s) = ctx
                .host
                .read_slot(fault, "faultactor")
                .map(|v| v.deref())
                .filter(|v| matches!(v, Value::Str(_)))
            {
                if let Some(n) =
                    ctx.master_to_xml(&mut doc, Some(string_enc), &s, SOAP_LITERAL, param)?
                {
                    doc.set_name(n, b"faultactor");
                }
            }
            detail_name = b"detail";
        } else {
            if let Some(Value::Str(code)) =
                ctx.host.read_slot(fault, "faultcode").map(|v| v.deref())
            {
                let c = doc.new_child(param, Some(ns.clone()), b"Code");
                let node = doc.new_child(c, Some(ns.clone()), b"Value");
                let escaped = html_escape(code.as_bytes());
                if let Some(fns) = &fault_ns {
                    let nsp = ctx.encode_add_ns(&mut doc, node, fns);
                    let mut qn = Vec::new();
                    if let Some(p) = nsp.and_then(|n| n.prefix) {
                        qn.extend_from_slice(&p);
                        qn.push(b':');
                    }
                    qn.extend_from_slice(&escaped);
                    doc.set_content(node, &qn);
                } else {
                    doc.set_content(node, &escaped);
                }
            }
            if let Some(s) = ctx
                .host
                .read_slot(fault, "faultstring")
                .map(|v| v.deref())
                .filter(|v| matches!(v, Value::Str(_)))
            {
                let reason = doc.new_child(param, Some(ns.clone()), b"Reason");
                if let Some(n) =
                    ctx.master_to_xml(&mut doc, Some(string_enc), &s, SOAP_LITERAL, reason)?
                {
                    doc.set_name(n, b"Text");
                    doc.set_ns(n, Some(ns.clone()));
                    let lang = match ctx.host.read_slot(fault, "lang").map(|v| v.deref()) {
                        Some(Value::Str(l)) => l.as_bytes().to_vec(),
                        _ => Vec::new(),
                    };
                    doc.set_ns_prop(n, Some(Ns::xml()), b"lang", &lang);
                }
            }
            detail_name = b"env:Detail";
        }
        let detail = ctx
            .host
            .read_slot(fault, "detail")
            .map(|v| v.deref())
            .filter(|v| !matches!(v, Value::Null));
        if let Some(f) = sfault.filter(|f| f.details.len() == 1) {
            let node = doc.add_element(param, detail_name);
            let sparam = &f.details[0];
            let mut detail = detail;
            if let (Some(Value::Object(o)), Some(e), Some(sdl)) = (&detail, sparam.element, sdl_ref)
            {
                let props = ctx.host.property_table(o);
                if props.len() == 1 {
                    if let Some(v) = props.get_str(sdl.ty(e).name_str().as_bytes()) {
                        detail = Some(v.deref());
                    }
                }
            }
            let x = serialize_parameter(
                ctx,
                &mut doc,
                Some(sparam),
                detail.as_ref(),
                1,
                None,
                use_,
                node,
            )?;
            if let Some(x) = x {
                if let Some(fnb) = function.and_then(|f| f.soap.as_ref()) {
                    if fnb.style == SOAP_RPC && sparam.element.is_none() {
                        if let Some(fbns) = f.binding.as_ref().and_then(|b| b.ns.clone()) {
                            let nsp = ctx.encode_add_ns(&mut doc, x, fbns.as_bytes());
                            doc.set_ns(x, nsp);
                        }
                    } else if let (Some(e), Some(sdl)) = (sparam.element, sdl_ref) {
                        let el = sdl.ty(e);
                        let nsp = match &el.namens {
                            Some(n) => ctx.encode_add_ns(&mut doc, x, n.as_bytes()),
                            None => None,
                        };
                        doc.set_name(x, el.name_str().as_bytes());
                        doc.set_ns(x, nsp);
                    }
                }
                if use_ == SOAP_ENCODED && version == SOAP_1_2 {
                    doc.set_ns_prop(
                        x,
                        Some(ns.clone()),
                        b"encodingStyle",
                        SOAP_1_2_ENC_NAMESPACE.as_bytes(),
                    );
                }
            }
        } else if let Some(d) = detail {
            serialize_zval(ctx, &mut doc, Some(&d), None, detail_name, use_, param)?;
        }
    } else {
        if !headers.is_empty() {
            let h = doc.new_child(envelope, Some(ns.clone()), b"Header");
            head = Some(h);
            for hdr in headers {
                if matches!(hdr.retval, Value::Null) {
                    continue;
                }
                let mut hdr_enc = None;
                let mut hdr_use = SOAP_LITERAL;
                let mut hdr_ret = Some(hdr.retval.clone());
                let mut hdr_ns: Option<Vec<u8>> = hdr
                    .hdr
                    .as_ref()
                    .and_then(|h| h.ns.clone())
                    .map(String::into_bytes);
                let mut hdr_name = hdr.function_name.clone();
                let header_obj = match &hdr.retval {
                    Value::Object(o) if ctx.host.instance_of(o, "SoapHeader") => Some(o.clone()),
                    _ => None,
                };
                if let Some(ho) = &header_obj {
                    let (hns, hname, hdata) = header_parts(ctx, ho);
                    let mut key = String::new();
                    if let Some(n) = &hns {
                        key.push_str(&lossy(n.as_bytes()));
                        key.push(':');
                        hdr_ns = Some(n.as_bytes().to_vec());
                    }
                    if let Some(n) = &hname {
                        key.push_str(&lossy(n.as_bytes()));
                        hdr_name = Some(n.as_bytes().to_vec());
                    }
                    if let Some(fnb) = function.and_then(|f| f.soap.as_ref()) {
                        if let Some(hb) = fnb.output.headers.as_ref().and_then(|hs| hs.get(&key)) {
                            hdr_enc = hb.encode;
                            hdr_use = hb.use_;
                        }
                    }
                    hdr_ret = hdata;
                }
                if let Some(hfn) = fn_of(hdr.function) {
                    let (u, x) = serialize_response_call2(
                        ctx,
                        &mut doc,
                        h,
                        Some(hfn),
                        hdr.function_name.as_deref(),
                        uri,
                        hdr_ret.as_ref(),
                        version,
                        false,
                    )?;
                    if u == SOAP_ENCODED {
                        use_ = SOAP_ENCODED;
                    }
                    if let (Some(ho), Some(x)) = (&header_obj, x) {
                        set_soap_header_attributes(ctx, &mut doc, x, ho, version);
                    }
                } else if let Some(x) = ctx.master_to_xml(
                    &mut doc,
                    hdr_enc,
                    &hdr_ret.unwrap_or(Value::Null),
                    hdr_use,
                    h,
                )? {
                    if let Some(n) = &hdr_name {
                        doc.set_name(x, n);
                    }
                    if let Some(n) = &hdr_ns {
                        let nsp = ctx.encode_add_ns(&mut doc, x, n);
                        doc.set_ns(x, nsp);
                    }
                    if let Some(ho) = &header_obj {
                        set_soap_header_attributes(ctx, &mut doc, x, ho, version);
                    }
                }
            }
            if doc.first_child(h).is_none() {
                doc.unlink(h);
                head = None;
            }
        }
        body = doc.new_child(envelope, Some(ns.clone()), b"Body");
        let (u, _) = serialize_response_call2(
            ctx,
            &mut doc,
            body,
            function,
            function_name,
            uri,
            Some(ret),
            version,
            true,
        )?;
        if u == SOAP_ENCODED {
            use_ = SOAP_ENCODED;
        }
    }
    if use_ == SOAP_ENCODED {
        doc.new_ns(envelope, XSD_NAMESPACE.as_bytes(), Some(b"xsd"));
        if version == SOAP_1_1 {
            doc.new_ns(
                envelope,
                SOAP_1_1_ENC_NAMESPACE.as_bytes(),
                Some(b"SOAP-ENC"),
            );
            doc.set_ns_prop(
                envelope,
                Some(ns.clone()),
                b"encodingStyle",
                SOAP_1_1_ENC_NAMESPACE.as_bytes(),
            );
        } else {
            doc.new_ns(envelope, SOAP_1_2_ENC_NAMESPACE.as_bytes(), Some(b"enc"));
        }
    }
    ctx.finish();
    if let Some(e) = ctx.take_pending_error() {
        return Err(SoapError::Runtime(e));
    }
    if function.is_some_and(|f| f.response_name.is_none())
        && doc.first_child(body).is_none()
        && head.is_none()
    {
        return Ok(None);
    }
    Ok(Some(doc))
}

/// `php_escape_html_entities(str, len, 0, 0, NULL)`: escapes `&`, `<`, `>`;
/// invalid UTF-8 yields an empty string.
fn html_escape(s: &[u8]) -> Vec<u8> {
    if std::str::from_utf8(s).is_err() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        match c {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_string_escaping() {
        assert_eq!(html_escape(b"a<b>&c\"'"), b"a&lt;b&gt;&amp;c\"'");
        assert_eq!(html_escape(b"bad \xff utf8"), b"");
        assert_eq!(env_ns(SOAP_1_1).0, SOAP_1_1_ENV_NAMESPACE);
        assert_eq!(env_ns(SOAP_1_2).0, SOAP_1_2_ENV_NAMESPACE);
    }
}
