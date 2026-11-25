// Derived from php-src ext/soap/php_sdl.c. Similarity metric to original: 24%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! WSDL 1.1 loading (`load_wsdl`/`load_wsdl_ex` of `php_sdl.c`).

use std::collections::HashSet;
use std::rc::Rc;

use crate::bail;
use crate::consts::*;
use crate::error::SoapResult;
use crate::schema::{location_uri, prepare_schema};
use crate::sdl::*;
use crate::xml::{Doc, NodeId, ParseFailure};
use crate::zhash::ZHash;

/// Fetches WSDL/XSD documents (the PHP stream layer behind libxml2's
/// `xmlCreateFileParserCtxt`).
pub trait ResourceLoader {
    /// Returns the bytes at `url`, or the libxml2-style error message
    /// (e.g. `failed to load external entity "…"\n`).
    fn load(&mut self, url: &str) -> Result<Vec<u8>, String>;
}

/// A node in one of the loaded documents.
#[derive(Debug, Clone)]
pub struct NodeRef {
    /// The document.
    pub doc: Rc<Doc>,
    /// The node.
    pub node: NodeId,
}

/// `sdlCtx`.
pub struct LoadCtx<'a> {
    /// The SDL under construction.
    pub sdl: Sdl,
    /// Loaded documents keyed by URL.
    pub docs: ZHash<Rc<Doc>>,
    messages: ZHash<NodeRef>,
    bindings: ZHash<NodeRef>,
    port_types: ZHash<NodeRef>,
    services: ZHash<NodeRef>,
    /// Global attributes (`ctx->attributes`).
    pub attributes: Option<ZHash<SdlAttribute>>,
    /// Attribute groups (`ctx->attributeGroups`).
    pub attribute_groups: Option<ZHash<TypeId>>,
    /// Types being fixed up (`fixupInProgress`).
    pub fixup_in_progress: HashSet<TypeId>,
    loader: &'a mut dyn ResourceLoader,
    last_error: Option<String>,
}

impl std::fmt::Debug for LoadCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadCtx")
            .field("source", &self.sdl.source)
            .finish_non_exhaustive()
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `is_wsdl_element`: whether a node is in the WSDL namespace (or has no
/// namespace); extension elements with `wsdl:required="true"` abort.
fn is_wsdl_element(doc: &Doc, n: NodeId) -> SoapResult<bool> {
    if let Some(ns) = doc.ns(n) {
        if ns.href != WSDL_NAMESPACE.as_bytes() {
            if matches!(
                doc.attr_value(n, "required", Some(WSDL_NAMESPACE)),
                Some(b"1" | b"true")
            ) {
                bail!(
                    "Parsing WSDL: Unknown required WSDL extension '{}'",
                    lossy(&ns.href)
                );
            }
            return Ok(false);
        }
    }
    Ok(true)
}

fn after_colon(s: &[u8]) -> String {
    match s.iter().rposition(|&c| c == b':') {
        Some(p) => lossy(&s[p + 1..]),
        None => lossy(s),
    }
}

fn after_first_colon(s: &[u8]) -> String {
    match s.iter().position(|&c| c == b':') {
        Some(p) => lossy(&s[p + 1..]),
        None => lossy(s),
    }
}

fn encoding_style(value: &[u8]) -> SoapResult<EncodingStyle> {
    if value == SOAP_1_1_ENC_NAMESPACE.as_bytes() {
        Ok(EncodingStyle::Soap11)
    } else if value == SOAP_1_2_ENC_NAMESPACE.as_bytes() {
        Ok(EncodingStyle::Soap12)
    } else {
        bail!("Parsing WSDL: Unknown encodingStyle '{}'", lossy(value));
    }
}

impl<'a> LoadCtx<'a> {
    /// A fresh context for the WSDL at `source`.
    pub fn new(source: &str, loader: &'a mut dyn ResourceLoader) -> Self {
        Self {
            sdl: Sdl {
                source: source.to_owned(),
                ..Sdl::default()
            },
            docs: ZHash::new(),
            messages: ZHash::new(),
            bindings: ZHash::new(),
            port_types: ZHash::new(),
            services: ZHash::new(),
            attributes: None,
            attribute_groups: None,
            fixup_in_progress: HashSet::new(),
            loader,
            last_error: None,
        }
    }

    /// `soap_xmlParseFile`: fetches and parses a document; `None` on
    /// failure (the reason is kept for the error message).
    pub fn fetch_doc(&mut self, url: &str) -> Option<Doc> {
        match self.loader.load(url) {
            Ok(bytes) => match Doc::parse_wsdl(&bytes, url) {
                Ok(d) => Some(d),
                Err(ParseFailure::Malformed(msg)) => {
                    self.last_error = msg;
                    None
                }
                Err(ParseFailure::TooDeep) => {
                    self.last_error = None;
                    None
                }
            },
            Err(msg) => {
                self.last_error = Some(msg);
                None
            }
        }
    }

    /// `load_wsdl_ex`.
    fn load_wsdl_ex(&mut self, struri: &str, include: bool) -> SoapResult<()> {
        if self.docs.contains(struri) {
            return Ok(());
        }
        let Some(mut wsdl) = self.fetch_doc(struri) else {
            match self.last_error.take().filter(|m| !m.is_empty()) {
                Some(msg) => bail!("Parsing WSDL: Couldn't load from '{struri}' : {msg}"),
                None => bail!("Parsing WSDL: Couldn't load from '{struri}'"),
            }
        };
        let root = wsdl.first_top();
        let definitions = wsdl.get_node(root, "definitions", Some(WSDL_NAMESPACE));
        let Some(definitions) = definitions else {
            if include {
                if let Some(schema) = wsdl.get_node(root, "schema", Some(XSD_NAMESPACE)) {
                    prepare_schema(&mut wsdl, schema);
                    let doc = Rc::new(wsdl);
                    self.docs.add(struri, Rc::clone(&doc));
                    return self.load_schema(&doc, schema);
                }
            }
            bail!("Parsing WSDL: Couldn't find <definitions> in '{struri}'");
        };
        // Apply the schema mutations up front (see `prepare_schema`).
        let types: Vec<NodeId> = wsdl
            .children(definitions)
            .filter(|&t| wsdl.node_is(t, Some("types"), None))
            .collect();
        for t in types {
            let schemas: Vec<NodeId> = wsdl
                .children(t)
                .filter(|&s| wsdl.node_is(s, Some("schema"), Some(XSD_NAMESPACE)))
                .collect();
            for s in schemas {
                prepare_schema(&mut wsdl, s);
            }
        }
        let doc = Rc::new(wsdl);
        self.docs.add(struri, Rc::clone(&doc));

        if !include {
            if let Some(tns) = doc.attr_value(definitions, "targetNamespace", None) {
                self.sdl.target_ns = Some(lossy(tns));
            }
        }

        let mut trav = doc.first_child(definitions);
        while let Some(t) = trav {
            trav = doc.next(t);
            if !is_wsdl_element(&doc, t)? {
                continue;
            }
            if doc.node_is(t, Some("types"), None) {
                for t2 in doc.children(t) {
                    if doc.node_is(t2, Some("schema"), Some(XSD_NAMESPACE)) {
                        self.load_schema(&doc, t2)?;
                    } else if is_wsdl_element(&doc, t2)?
                        && !doc.node_is(t2, Some("documentation"), None)
                    {
                        bail!(
                            "Parsing WSDL: Unexpected WSDL element <{}>",
                            lossy(doc.name(t2))
                        );
                    }
                }
            } else if doc.node_is(t, Some("import"), None) {
                if let Some(location) = doc.attr_value(t, "location", None) {
                    let uri = location_uri(&doc, t, location);
                    self.load_wsdl_ex(&uri, true)?;
                }
            } else {
                let (table, what) = if doc.node_is(t, Some("message"), None) {
                    (&mut self.messages, "message")
                } else if doc.node_is(t, Some("portType"), None) {
                    (&mut self.port_types, "portType")
                } else if doc.node_is(t, Some("binding"), None) {
                    (&mut self.bindings, "binding")
                } else if doc.node_is(t, Some("service"), None) {
                    (&mut self.services, "service")
                } else if doc.node_is(t, Some("documentation"), None) {
                    continue;
                } else {
                    bail!(
                        "Parsing WSDL: Unexpected WSDL element <{}>",
                        lossy(doc.name(t))
                    );
                };
                let Some(name) = doc.attr_value(t, "name", None) else {
                    bail!("Parsing WSDL: <{what}> has no name attribute");
                };
                let name = lossy(name);
                let r = NodeRef {
                    doc: Rc::clone(&doc),
                    node: t,
                };
                if !table.add(&name, r) {
                    bail!("Parsing WSDL: <{what}> '{name}' already defined");
                }
            }
        }
        Ok(())
    }

    /// `wsdl_soap_binding_header`.
    fn binding_header(
        &self,
        doc: &Doc,
        header: NodeId,
        soap_ns: &str,
        fault: bool,
    ) -> SoapResult<HeaderBinding> {
        let Some(msg_attr) = doc.attr_value(header, "message", None) else {
            bail!("Parsing WSDL: Missing message attribute for <header>");
        };
        let ctype = after_colon(msg_attr);
        let Some(message) = self.messages.get(&ctype) else {
            bail!(
                "Parsing WSDL: Missing <message> with name '{}'",
                lossy(msg_attr)
            );
        };
        let Some(part_name) = doc.attr_value(header, "part", None) else {
            bail!("Parsing WSDL: Missing part attribute for <header>");
        };
        let mdoc = &message.doc;
        let Some(part) = mdoc.get_node_with_attribute(
            mdoc.first_child(message.node),
            "part",
            Some(WSDL_NAMESPACE),
            "name",
            part_name,
        ) else {
            bail!(
                "Parsing WSDL: Missing part '{}' in <message>",
                lossy(part_name)
            );
        };
        let mut h = HeaderBinding {
            name: lossy(part_name),
            ns: None,
            use_: if doc
                .attr_value(header, "use", None)
                .is_some_and(|u| u == b"encoded")
            {
                SOAP_ENCODED
            } else {
                SOAP_LITERAL
            },
            encoding_style: EncodingStyle::Default,
            element: None,
            encode: None,
            headerfaults: None,
        };
        if let Some(ns) = doc.attr_value(header, "namespace", None) {
            h.ns = Some(lossy(ns));
        }
        if h.use_ == SOAP_ENCODED {
            match doc.attr_value(header, "encodingStyle", None) {
                Some(es) => h.encoding_style = encoding_style(es)?,
                None => bail!("Parsing WSDL: Unspecified encodingStyle"),
            }
        }
        if let Some(t) = mdoc.attr_value(part, "type", None) {
            h.encode = get_encoder_from_prefix(Some(&self.sdl), mdoc, part, t);
        } else if let Some(el) = mdoc.attr_value(part, "element", None) {
            h.element = get_element(&self.sdl, mdoc, part, el);
            if let Some(e) = h.element {
                let et = self.sdl.ty(e);
                h.encode = et.encode;
                if h.ns.is_none() {
                    h.ns.clone_from(&et.namens);
                }
                if let Some(n) = &et.name {
                    h.name.clone_from(n);
                }
            }
        }
        if !fault {
            for t in doc.children(header) {
                if doc.node_is(t, Some("headerfault"), Some(soap_ns)) {
                    let hf = self.binding_header(doc, t, soap_ns, true)?;
                    let key = match &hf.ns {
                        Some(ns) => format!("{ns}:{}", hf.name),
                        None => hf.name.clone(),
                    };
                    h.headerfaults.get_or_insert_with(ZHash::new).add(&key, hf);
                } else if is_wsdl_element(doc, t)? && !doc.node_is(t, Some("documentation"), None) {
                    bail!(
                        "Parsing WSDL: Unexpected WSDL element <{}>",
                        lossy(doc.name(t))
                    );
                }
            }
        }
        Ok(h)
    }

    /// `wsdl_soap_binding_body`.
    fn binding_body(
        &self,
        doc: &Doc,
        node: NodeId,
        soap_ns: &str,
        binding: &mut BodyBinding,
        params: &mut Option<Vec<Param>>,
    ) -> SoapResult<()> {
        for t in doc.children(node) {
            if doc.node_is(t, Some("body"), Some(soap_ns)) {
                binding.use_ = if doc.attr_value(t, "use", None) == Some(b"literal") {
                    SOAP_LITERAL
                } else {
                    SOAP_ENCODED
                };
                if let Some(ns) = doc.attr_value(t, "namespace", None) {
                    binding.ns = Some(lossy(ns));
                }
                if let Some(parts) = doc.attr_value(t, "parts", None) {
                    let mut kept = Vec::new();
                    let list = params.get_or_insert_with(Vec::new);
                    for part in parts.split(|&c| c == b' ').filter(|p| !p.is_empty()) {
                        let pname = lossy(part);
                        let Some(found) = list
                            .iter_mut()
                            .find(|p| p.name.as_deref() == Some(pname.as_str()))
                        else {
                            bail!("Parsing WSDL: Missing part '{pname}' in <message>");
                        };
                        kept.push(found.clone());
                        found.name = None;
                    }
                    *list = kept;
                }
                if binding.use_ == SOAP_ENCODED {
                    match doc.attr_value(t, "encodingStyle", None) {
                        Some(es) => binding.encoding_style = encoding_style(es)?,
                        None => bail!("Parsing WSDL: Unspecified encodingStyle"),
                    }
                }
            } else if doc.node_is(t, Some("header"), Some(soap_ns)) {
                let h = self.binding_header(doc, t, soap_ns, false)?;
                let key = match &h.ns {
                    Some(ns) => format!("{ns}:{}", h.name),
                    None => h.name.clone(),
                };
                binding.headers.get_or_insert_with(ZHash::new).add(&key, h);
            } else if is_wsdl_element(doc, t)? && !doc.node_is(t, Some("documentation"), None) {
                bail!(
                    "Parsing WSDL: Unexpected WSDL element <{}>",
                    lossy(doc.name(t))
                );
            }
        }
        Ok(())
    }

    /// `wsdl_message`.
    fn message(&self, message_name: &[u8]) -> SoapResult<Vec<Param>> {
        let ctype = after_colon(message_name);
        let Some(message) = self.messages.get(&ctype) else {
            bail!(
                "Parsing WSDL: Missing <message> with name '{}'",
                lossy(message_name)
            );
        };
        let doc = &message.doc;
        let mut params = Vec::new();
        for t in doc.children(message.node) {
            if let Some(ns) = doc.ns(t) {
                if ns.href != WSDL_NAMESPACE.as_bytes() {
                    bail!(
                        "Parsing WSDL: Unexpected extensibility element <{}>",
                        lossy(doc.name(t))
                    );
                }
            }
            if doc.node_is(t, Some("documentation"), None) {
                continue;
            }
            if !doc.node_is(t, Some("part"), None) {
                bail!(
                    "Parsing WSDL: Unexpected WSDL element <{}>",
                    lossy(doc.name(t))
                );
            }
            let Some(name) = doc.attr_value(t, "name", None) else {
                bail!(
                    "Parsing WSDL: No name associated with <part> '{}'",
                    lossy(doc.name(message.node))
                );
            };
            let mut p = Param {
                order: 0,
                element: None,
                encode: None,
                name: Some(lossy(name)),
            };
            if let Some(ty) = doc.attr_value(t, "type", None) {
                p.encode = get_encoder_from_prefix(Some(&self.sdl), doc, t, ty);
            } else if let Some(el) = doc.attr_value(t, "element", None) {
                p.element = get_element(&self.sdl, doc, t, el);
                if let Some(e) = p.element {
                    p.encode = self.sdl.ty(e).encode;
                }
            }
            params.push(p);
        }
        Ok(params)
    }

    #[allow(clippy::too_many_lines)]
    fn load_services(&mut self) -> SoapResult<()> {
        let n = self.services.len();
        if n == 0 {
            bail!("Parsing WSDL: Couldn't bind to service");
        }
        let services: Vec<NodeRef> = self.services.values().cloned().collect();
        for (i, service) in services.iter().enumerate() {
            let doc = Rc::clone(&service.doc);
            let mut has_soap_port = false;
            let mut trav = doc.first_child(service.node);
            while let Some(port) = trav {
                trav = doc.next(port);
                if !is_wsdl_element(&doc, port)? || doc.node_is(port, Some("documentation"), None) {
                    continue;
                }
                if !doc.node_is(port, Some("port"), None) {
                    bail!(
                        "Parsing WSDL: Unexpected WSDL element <{}>",
                        lossy(doc.name(port))
                    );
                }
                let Some(binding_attr) = doc.attr_value(port, "binding", None) else {
                    bail!("Parsing WSDL: No binding associated with <port>");
                };
                let mut address = None;
                let mut soap_ns: Option<&str> = None;
                let mut btype = BindingType::Soap;
                for t2 in doc.children(port) {
                    if doc.node_is(t2, Some("address"), None) {
                        if let Some(ns) = doc.ns(t2) {
                            let href = ns.href.as_slice();
                            if href == WSDL_SOAP11_NAMESPACE.as_bytes() {
                                address = Some(t2);
                                soap_ns = Some(WSDL_SOAP11_NAMESPACE);
                                btype = BindingType::Soap;
                            } else if href == WSDL_SOAP12_NAMESPACE.as_bytes() {
                                address = Some(t2);
                                soap_ns = Some(WSDL_SOAP12_NAMESPACE);
                                btype = BindingType::Soap;
                            } else if href == RPC_SOAP12_NAMESPACE.as_bytes() {
                                address = Some(t2);
                                soap_ns = Some(RPC_SOAP12_NAMESPACE);
                                btype = BindingType::Soap;
                            } else if href == WSDL_HTTP11_NAMESPACE.as_bytes()
                                || href == WSDL_HTTP12_NAMESPACE.as_bytes()
                            {
                                address = Some(t2);
                                btype = BindingType::Http;
                            }
                        }
                    }
                    if Some(t2) != address
                        && is_wsdl_element(&doc, t2)?
                        && !doc.node_is(t2, Some("documentation"), None)
                    {
                        bail!(
                            "Parsing WSDL: Unexpected WSDL element <{}>",
                            lossy(doc.name(t2))
                        );
                    }
                }
                if address.is_none() || btype == BindingType::Http {
                    if has_soap_port || trav.is_some() || i < n - 1 {
                        continue;
                    } else if address.is_none() {
                        bail!("Parsing WSDL: No address associated with <port>");
                    }
                }
                has_soap_port = true;
                let address = address.unwrap_or(port);
                let Some(location) = doc.attr_value(address, "location", None) else {
                    bail!("Parsing WSDL: No location associated with <port>");
                };
                let ctype = after_colon(binding_attr);
                let Some(binding) = self.bindings.get(&ctype).cloned() else {
                    bail!("Parsing WSDL: No <binding> element with name '{ctype}'");
                };
                let bdoc = Rc::clone(&binding.doc);
                let soap_ns_str = soap_ns.unwrap_or("");
                let mut soap_binding = None;
                if btype == BindingType::Soap {
                    let mut sb = SoapBinding {
                        style: SOAP_DOCUMENT,
                        transport_http: false,
                    };
                    if let Some(node) =
                        bdoc.get_node(bdoc.first_child(binding.node), "binding", Some(soap_ns_str))
                    {
                        if bdoc.attr_value(node, "style", None) == Some(b"rpc") {
                            sb.style = SOAP_RPC;
                        }
                        if let Some(tr) = bdoc.attr_value(node, "transport", None) {
                            if tr == WSDL_HTTP_TRANSPORT.as_bytes() {
                                sb.transport_http = true;
                            } else {
                                continue;
                            }
                        }
                    }
                    soap_binding = Some(sb);
                }
                let Some(bname) = bdoc.attr_value(binding.node, "name", None) else {
                    bail!("Parsing WSDL: Missing 'name' attribute for <binding>");
                };
                let bname = lossy(bname);
                let Some(btype_attr) = bdoc.attr_value(binding.node, "type", None) else {
                    bail!("Parsing WSDL: Missing 'type' attribute for <binding>");
                };
                let ptname = after_first_colon(btype_attr);
                let Some(port_type) = self.port_types.get(&ptname).cloned() else {
                    bail!("Parsing WSDL: Missing <portType> with name '{bname}'");
                };
                let binding_index = self.sdl.binding_list.len();
                self.sdl.binding_list.push(Binding {
                    name: bname.clone(),
                    location: lossy(location),
                    binding_type: btype,
                    soap: soap_binding,
                });

                let mut t2 = bdoc.first_child(binding.node);
                while let Some(operation) = t2 {
                    t2 = bdoc.next(operation);
                    if (btype == BindingType::Soap
                        && bdoc.node_is(operation, Some("binding"), Some(soap_ns_str)))
                        || !is_wsdl_element(&bdoc, operation)?
                        || bdoc.node_is(operation, Some("documentation"), None)
                    {
                        continue;
                    }
                    if !bdoc.node_is(operation, Some("operation"), None) {
                        bail!(
                            "Parsing WSDL: Unexpected WSDL element <{}>",
                            lossy(bdoc.name(operation))
                        );
                    }
                    let Some(op_name) = bdoc.attr_value(operation, "name", None) else {
                        bail!("Parsing WSDL: Missing 'name' attribute for <operation>");
                    };
                    let op_name = op_name.to_vec();
                    for t3 in bdoc.children(operation) {
                        if btype == BindingType::Soap
                            && bdoc.node_is(t3, Some("operation"), Some(soap_ns_str))
                        {
                            continue;
                        }
                        if is_wsdl_element(&bdoc, t3)?
                            && !bdoc.node_is(t3, Some("input"), None)
                            && !bdoc.node_is(t3, Some("output"), None)
                            && !bdoc.node_is(t3, Some("fault"), None)
                            && !bdoc.node_is(t3, Some("documentation"), None)
                        {
                            bail!(
                                "Parsing WSDL: Unexpected WSDL element <{}>",
                                lossy(bdoc.name(t3))
                            );
                        }
                    }
                    let pdoc = Rc::clone(&port_type.doc);
                    let Some(pt_op) = pdoc.get_node_with_attribute(
                        pdoc.first_child(port_type.node),
                        "operation",
                        Some(WSDL_NAMESPACE),
                        "name",
                        &op_name,
                    ) else {
                        bail!(
                            "Parsing WSDL: Missing <portType>/<operation> with name '{}'",
                            lossy(&op_name)
                        );
                    };
                    let fname = lossy(&op_name);
                    let mut function = Function {
                        name: fname.clone(),
                        request_name: None,
                        response_name: None,
                        request_params: None,
                        response_params: None,
                        binding: binding_index,
                        soap: None,
                        faults: None,
                    };
                    if let (BindingType::Soap, Some(sb)) = (btype, soap_binding) {
                        let mut fb = FunctionBinding {
                            soap_action: None,
                            style: sb.style,
                            input: BodyBinding::default(),
                            output: BodyBinding::default(),
                        };
                        if let Some(sop) = bdoc.get_node(
                            bdoc.first_child(operation),
                            "operation",
                            Some(soap_ns_str),
                        ) {
                            if let Some(a) = bdoc.attr_value(sop, "soapAction", None) {
                                fb.soap_action = Some(lossy(a));
                            }
                            fb.style = match bdoc.attr_value(sop, "style", None) {
                                Some(b"rpc") => SOAP_RPC,
                                Some(_) => SOAP_DOCUMENT,
                                None => sb.style,
                            };
                        }
                        function.soap = Some(fb);
                    }
                    if let Some(input) =
                        pdoc.get_node(pdoc.first_child(pt_op), "input", Some(WSDL_NAMESPACE))
                    {
                        let Some(message) = pdoc.attr_value(input, "message", None) else {
                            bail!("Parsing WSDL: Missing name for <input> of '{fname}'");
                        };
                        function.request_params = Some(self.message(message)?);
                        function.request_name = Some(fname.clone());
                        if btype == BindingType::Soap {
                            if let Some(binput) = bdoc.get_node(
                                bdoc.first_child(operation),
                                "input",
                                Some(WSDL_NAMESPACE),
                            ) {
                                if let Some(fb) = function.soap.as_mut() {
                                    self.binding_body(
                                        &bdoc,
                                        binput,
                                        soap_ns_str,
                                        &mut fb.input,
                                        &mut function.request_params,
                                    )?;
                                }
                            }
                        }
                    }
                    if let Some(output) =
                        pdoc.get_node(pdoc.first_child(pt_op), "output", Some(WSDL_NAMESPACE))
                    {
                        let Some(message) = pdoc.attr_value(output, "message", None) else {
                            bail!("Parsing WSDL: Missing name for <output> of '{fname}'");
                        };
                        function.response_params = Some(self.message(message)?);
                        function.response_name = Some(format!("{fname}Response"));
                        if btype == BindingType::Soap {
                            if let Some(boutput) = bdoc.get_node(
                                bdoc.first_child(operation),
                                "output",
                                Some(WSDL_NAMESPACE),
                            ) {
                                if let Some(fb) = function.soap.as_mut() {
                                    self.binding_body(
                                        &bdoc,
                                        boutput,
                                        soap_ns_str,
                                        &mut fb.output,
                                        &mut function.response_params,
                                    )?;
                                }
                            }
                        }
                    }
                    for fault in pdoc.children(pt_op) {
                        if !pdoc.node_is(fault, Some("fault"), Some(WSDL_NAMESPACE)) {
                            continue;
                        }
                        let Some(fault_name) = pdoc.attr_value(fault, "name", None) else {
                            bail!("Parsing WSDL: Missing name for <fault> of '{fname}'");
                        };
                        let Some(message) = pdoc.attr_value(fault, "message", None) else {
                            bail!("Parsing WSDL: Missing name for <output> of '{fname}'");
                        };
                        let details = self.message(message)?;
                        if details.len() > 1 {
                            bail!(
                                "Parsing WSDL: The fault message '{}' must have a single part",
                                lossy(message)
                            );
                        }
                        let mut f = Fault {
                            name: lossy(fault_name),
                            details,
                            binding: None,
                        };
                        if btype == BindingType::Soap {
                            if let Some(sfault) = bdoc.get_node_with_attribute(
                                bdoc.first_child(operation),
                                "fault",
                                Some(WSDL_NAMESPACE),
                                "name",
                                fault_name,
                            ) {
                                for fnode in bdoc.children(sfault) {
                                    if bdoc.node_is(fnode, Some("fault"), Some(soap_ns_str)) {
                                        let use_ = if bdoc.attr_value(fnode, "use", None)
                                            == Some(b"encoded")
                                        {
                                            SOAP_ENCODED
                                        } else {
                                            SOAP_LITERAL
                                        };
                                        let mut fbind = FaultBinding {
                                            ns: bdoc
                                                .attr_value(fnode, "namespace", None)
                                                .map(lossy),
                                            use_,
                                            encoding_style: EncodingStyle::Default,
                                        };
                                        if use_ == SOAP_ENCODED {
                                            match bdoc.attr_value(fnode, "encodingStyle", None) {
                                                Some(es) => {
                                                    fbind.encoding_style = encoding_style(es)?;
                                                }
                                                None => {
                                                    bail!("Parsing WSDL: Unspecified encodingStyle")
                                                }
                                            }
                                        }
                                        f.binding = Some(fbind);
                                    } else if is_wsdl_element(&bdoc, fnode)?
                                        && !bdoc.node_is(fnode, Some("documentation"), None)
                                    {
                                        bail!(
                                            "Parsing WSDL: Unexpected WSDL element <{}>",
                                            lossy(bdoc.name(fnode))
                                        );
                                    }
                                }
                            }
                        }
                        let fname_key = f.name.clone();
                        if !function
                            .faults
                            .get_or_insert_with(ZHash::new)
                            .add(&fname_key, f)
                        {
                            bail!("Parsing WSDL: <fault> with name '{fname_key}' already defined in '{fname}'");
                        }
                    }
                    let lc = fname.to_ascii_lowercase();
                    let req_differs = function
                        .request_name
                        .as_ref()
                        .is_some_and(|r| *r != function.name);
                    let req_lc = function
                        .request_name
                        .as_ref()
                        .map(|r| r.to_ascii_lowercase());
                    let index = self.sdl.function_list.len();
                    self.sdl.function_list.push(function);
                    if !self.sdl.functions.add(&lc, index) {
                        self.sdl.functions.push(index);
                    }
                    if req_differs {
                        if let Some(r) = req_lc {
                            self.sdl
                                .requests
                                .get_or_insert_with(ZHash::new)
                                .add(&r, index);
                        }
                    }
                }
                if !self.sdl.bindings.add(&bname, binding_index) {
                    self.sdl.bindings.push(binding_index);
                }
            }
        }
        if self.sdl.bindings.is_empty() {
            bail!("Parsing WSDL: Could not find any usable binding services in WSDL.");
        }
        Ok(())
    }
}

/// `load_wsdl`: loads the WSDL at `uri` with all its imports and schemas.
pub fn load_wsdl(uri: &str, loader: &mut dyn ResourceLoader) -> SoapResult<Sdl> {
    let mut ctx = LoadCtx::new(uri, loader);
    ctx.load_wsdl_ex(uri, false)?;
    ctx.schema_pass2()?;
    ctx.load_services()?;
    Ok(ctx.sdl)
}

/// A loader over an in-memory map (tests, fuzzing, embedded WSDLs).
#[derive(Debug, Default, Clone)]
pub struct MemoryLoader {
    /// `url → content`.
    pub files: std::collections::HashMap<String, Vec<u8>>,
}

impl ResourceLoader for MemoryLoader {
    fn load(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.files
            .get(url)
            .cloned()
            .ok_or_else(|| format!("failed to load external entity \"{url}\"\n"))
    }
}
