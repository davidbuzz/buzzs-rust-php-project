// Derived from php-src ext/dom/xml_serializer.c. Similarity metric to original: 16%
//   Copyright © The PHP Group and Contributors. Authors: Nora Dossche <ndossche@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The spec-compliant XML serializer (`xml_serializer.c`) used by the
//! `Dom\XMLDocument` save handlers and the XML branch of `innerHTML`.
//!
//! Implements <https://w3c.github.io/DOM-Parsing/#dfn-xml-serialization>
//! including php-src's fixes for the DOM-Parsing spec issues 28, 29, 38,
//! 43, 44, 45, 47, 50, 52, 59 and 71.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::rc::Rc;

use crate::error::{DomError, DomExceptionCode, DomResult};
use crate::ffi;
use crate::ns::{
    ns_is_fast, ns_is_fast_ex, PrivateData, HTML_TOKEN, XMLNS_NS, XMLNS_TOKEN, XML_NS, XML_TOKEN,
};
use crate::object::DocInner;
use crate::raw::{cs, N};
use crate::save::write_vec;
use crate::stack::{overflowed, StackScope, STACK_OVERFLOW_MESSAGE};

/// Why serialization stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fail {
    /// An output error or a well-formedness violation (`-1`).
    Failed,
    /// `zend_call_stack_overflowed()` (an `Error` was thrown).
    Stack,
}

type Res = Result<(), Fail>;

/// `dom_xml_str_equals_treat_nulls_as_empty`.
fn eq_empty(a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
    a.unwrap_or_default() == b.unwrap_or_default()
}

/// Namespace URI → candidate prefixes.
type PrefixLists = HashMap<Vec<u8>, Rc<Vec<Vec<u8>>>>;

/// `dom_xml_ns_prefix_map`: namespace → list of candidate prefixes, copy
/// on write.
#[derive(Clone, Default)]
struct NsPrefixMap(Rc<PrefixLists>);

impl NsPrefixMap {
    /// <https://w3c.github.io/DOM-Parsing/#dfn-add>.
    fn add(&mut self, prefix: &[u8], ns: Option<&[u8]>) {
        let ns = ns.unwrap_or_default();
        let map = Rc::make_mut(&mut self.0);
        let list = map.entry(ns.to_vec()).or_default();
        Rc::make_mut(list).push(prefix.to_vec());
    }

    fn candidates(&self, ns: &[u8]) -> Option<&Rc<Vec<Vec<u8>>>> {
        self.0.get(ns)
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-found>.
    fn found(&self, prefix: &[u8], ns: &[u8]) -> bool {
        self.candidates(ns)
            .is_some_and(|l| l.iter().any(|p| eq_empty(Some(p), Some(prefix))))
    }
}

/// `dom_xml_local_prefix_map`.
#[derive(Default)]
struct LocalPrefixMap(HashMap<Vec<u8>, Option<Vec<u8>>>);

impl LocalPrefixMap {
    fn add(&mut self, prefix: &[u8], ns: Option<&[u8]>) {
        self.0
            .entry(prefix.to_vec())
            .or_insert_with(|| ns.map(<[u8]>::to_vec));
    }

    fn find(&self, prefix: &[u8]) -> Option<&[u8]> {
        self.0.get(prefix).and_then(|v| v.as_deref())
    }

    fn conflicts(&self, prefix: &[u8], ns: &[u8]) -> bool {
        self.find(prefix)
            .is_some_and(|r| !eq_empty(Some(r), Some(ns)))
    }

    fn contains(&self, prefix: &[u8]) -> bool {
        self.find(prefix).is_some()
    }
}

/// `dom_get_attribute_value`.
fn attribute_value(attr: N) -> &'static [u8] {
    attr.children().map_or(b"".as_slice(), N::content)
}

fn ns_href(n: N) -> Option<&'static [u8]> {
    n.ns().map(crate::raw::Ns::href)
}

fn ns_prefix(n: N) -> Option<&'static [u8]> {
    n.ns().and_then(crate::raw::Ns::prefix)
}

/// <https://w3c.github.io/DOM-Parsing/#dfn-recording-the-namespace-information>.
fn record_namespace_information(
    map: &mut NsPrefixMap,
    local: &mut LocalPrefixMap,
    element: N,
) -> Option<&'static [u8]> {
    let mut default_namespace_attr_value = None;
    for attr in element.attr_iter() {
        let Some(ans) = attr.ns() else { continue };
        if !ns_is_fast_ex(ans, &XMLNS_TOKEN) {
            continue;
        }
        if ans.prefix().is_none() {
            default_namespace_attr_value = Some(attribute_value(attr));
            continue;
        }
        let prefix_definition = attr.name();
        let namespace_definition = attribute_value(attr);
        if namespace_definition == XML_NS {
            continue;
        }
        if map.found(prefix_definition, namespace_definition) {
            continue;
        }
        let def = (!namespace_definition.is_empty()).then_some(namespace_definition);
        map.add(prefix_definition, def);
        local.add(prefix_definition, Some(def.unwrap_or_default()));
    }
    default_namespace_attr_value
}

/// <https://w3c.github.io/DOM-Parsing/#dfn-retrieving-a-preferred-prefix-string>.
fn retrieve_preferred_prefix(
    map: &NsPrefixMap,
    local: &LocalPrefixMap,
    preferred: Option<&[u8]>,
    ns: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let ns = ns.unwrap_or_default();
    let list = map.candidates(ns)?;
    let mut last_non_conflicting: Option<&Vec<u8>> = None;
    // Reverse so that the "nearest" ns gets priority (DOM-Parsing #45).
    for prefix in list.iter().rev() {
        if !local.conflicts(prefix, ns) {
            if eq_empty(preferred, Some(prefix)) {
                return Some(prefix.clone());
            }
            if last_non_conflicting.is_none() {
                last_non_conflicting = Some(prefix);
            }
        }
    }
    last_non_conflicting.cloned()
}

/// <https://w3c.github.io/DOM-Parsing/#dfn-generating-a-prefix>.
fn generate_prefix(
    map: &mut NsPrefixMap,
    local: &mut LocalPrefixMap,
    ns: Option<&[u8]>,
    index: &mut u32,
) -> Vec<u8> {
    let generated = loop {
        let candidate = format!("ns{index}").into_bytes();
        *index = index.wrapping_add(1);
        // DOM-Parsing #44.
        if !local.contains(&candidate) {
            break candidate;
        }
    };
    map.add(&generated, ns);
    local.add(&generated, ns);
    generated
}

/// `dom_xml_check_char_production`.
fn check_char_production(content: &[u8]) -> Res {
    let mut rest = content;
    while !rest.is_empty() {
        let len = rest.len().min(4);
        let mut buf = [0u8; 5];
        buf[..len].copy_from_slice(&rest[..len]);
        let mut l = c_int::try_from(len).unwrap_or(4);
        // SAFETY: `buf` is NUL-terminated and `l` bounds the read.
        let c = unsafe { ffi::xmlGetUTF8Char(buf.as_ptr(), &raw mut l) };
        let is_char = u32::try_from(c)
            .is_ok_and(|c| matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x1_0000..=0x10_FFFF));
        if !is_char || l <= 0 {
            return Err(Fail::Failed);
        }
        rest = &rest[usize::try_from(l).unwrap_or(1).min(rest.len())..];
    }
    Ok(())
}

/// `xmlValidateNCName(name, 0) == 0`.
fn valid_ncname(name: &[u8]) -> bool {
    let c = cs(name);
    // SAFETY: NUL-terminated string.
    unsafe { ffi::xmlValidateNCName(c.as_ptr().cast(), 0) == 0 }
}

/// HTML void elements serialized as `<x />` (step 14).
const VOID_ELEMENTS: &[&[u8]] = &[
    b"area",
    b"base",
    b"basefont",
    b"bgsound",
    b"br",
    b"col",
    b"embed",
    b"frame",
    b"hr",
    b"img",
    b"input",
    b"keygen",
    b"link",
    b"menuitem",
    b"meta",
    b"param",
    b"source",
    b"track",
    b"wbr",
];

/// `dom_xml_serialize_ctx`.
struct Ctx {
    ctxt: *mut c_void,
    out: *mut ffi::xmlOutputBuffer,
    private: Option<Rc<PrivateData>>,
}

impl Ctx {
    fn w(&self, bytes: &[u8]) -> Res {
        if bytes.is_empty() {
            return Ok(());
        }
        let Ok(len) = c_int::try_from(bytes.len()) else {
            return Err(Fail::Failed);
        };
        // SAFETY: live output buffer; `bytes` valid for `len`.
        let r =
            unsafe { ffi::xmlOutputBufferWrite(self.out, len, bytes.as_ptr().cast::<c_char>()) };
        if r < 0 {
            Err(Fail::Failed)
        } else {
            Ok(())
        }
    }

    fn flush_out(&self) -> Res {
        // SAFETY: live output buffer.
        if unsafe { ffi::xmlOutputBufferFlush(self.out) } < 0 {
            Err(Fail::Failed)
        } else {
            Ok(())
        }
    }

    fn flush_ctxt(&self) -> Res {
        // SAFETY: live save context.
        if unsafe { ffi::xmlSaveFlush(self.ctxt) } < 0 {
            Err(Fail::Failed)
        } else {
            Ok(())
        }
    }

    fn qname(&self, prefix: Option<&[u8]>, name: &[u8]) -> Res {
        if let Some(p) = prefix {
            self.w(p)?;
            self.w(b":")?;
        }
        self.w(name)
    }

    /// `dom_xml_common_text_serialization`.
    fn text(&self, content: &[u8], attribute_mode: bool) -> Res {
        let mut last = 0;
        for (i, &b) in content.iter().enumerate() {
            let rep: &[u8] = match b {
                b'&' => b"&amp;",
                b'<' => b"&lt;",
                b'>' => b"&gt;",
                b'"' if attribute_mode => b"&quot;",
                b'\t' if attribute_mode => b"&#9;",
                b'\n' if attribute_mode => b"&#10;",
                b'\r' if attribute_mode => b"&#13;",
                _ => continue,
            };
            self.w(&content[last..i])?;
            self.w(rep)?;
            last = i + 1;
        }
        self.w(&content[last..])
    }

    /// `dom_xml_serialize_attribute_node_value`.
    fn attribute_node_value(&self, attr: N) -> Res {
        self.w(attr.name())?;
        self.w(b"=\"")?;
        for child in attr.child_iter() {
            if child.t() == ffi::XML_TEXT_NODE {
                self.text(child.content(), true)?;
            } else if child.t() == ffi::XML_ENTITY_REF_NODE {
                self.w(b"&")?;
                self.text(child.name(), true)?;
                self.w(b";")?;
            }
        }
        self.w(b"\"")
    }

    /// `dom_xml_serialize_attribute_node` (DOM-Parsing #28).
    fn attribute_node(&self, attr: N) -> Res {
        if let Some(p) = ns_prefix(attr) {
            self.w(p)?;
            self.w(b":")?;
        }
        self.attribute_node_value(attr)
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-xml-serializing-a-comment-node>.
    fn comment(&self, comment: N, well_formed: bool) -> Res {
        let content = comment.content();
        if well_formed && !comment.content_ptr().is_null() {
            check_char_production(content)?;
            if content.windows(2).any(|w| w == b"--") || content.last() == Some(&b'-') {
                return Err(Fail::Failed);
            }
        }
        self.w(b"<!--")?;
        self.w(content)?;
        self.w(b"-->")
    }

    /// <https://w3c.github.io/DOM-Parsing/#xml-serializing-a-processinginstruction-node>.
    fn processing_instruction(&self, pi: N, well_formed: bool) -> Res {
        if well_formed {
            let name = pi.name();
            if name.contains(&b':') || name.eq_ignore_ascii_case(b"xml") {
                return Err(Fail::Failed);
            }
            if !pi.content_ptr().is_null() {
                check_char_production(pi.content())?;
                if pi.content().windows(2).any(|w| w == b"?>") {
                    return Err(Fail::Failed);
                }
            }
        }
        self.w(b"<?")?;
        self.w(pi.name())?;
        self.w(b" ")?;
        self.w(pi.content())?;
        self.w(b"?>")
    }

    /// DOM-Parsing #38.
    fn cdata(&self, cdata: N) -> Res {
        self.w(b"<![CDATA[")?;
        self.w(cdata.content())?;
        self.w(b"]]>")
    }

    fn indents(&self, indent: i32) -> Res {
        self.w(b"\n")?;
        for _ in 0..indent {
            self.w(b"  ")?;
        }
        Ok(())
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-xml-serialization-of-the-attributes>.
    fn attributes(
        &self,
        element: N,
        map: &mut NsPrefixMap,
        local: &mut LocalPrefixMap,
        prefix_index: &mut u32,
        ignore_namespace_definition_attribute: bool,
        well_formed: bool,
    ) -> Res {
        let mut localname_set: HashSet<(Option<&[u8]>, &[u8])> = HashSet::new();
        for attr in element.attr_iter() {
            if well_formed && !localname_set.insert((ns_href(attr), attr.name())) {
                return Err(Fail::Failed);
            }
            let attribute_namespace = ns_href(attr);
            let mut candidate_prefix: Option<Vec<u8>> = None;
            if let (Some(attribute_namespace), Some(ans)) = (attribute_namespace, attr.ns()) {
                candidate_prefix =
                    retrieve_preferred_prefix(map, local, ans.prefix(), Some(attribute_namespace));
                if ns_is_fast_ex(ans, &XMLNS_TOKEN) {
                    let attr_value = attribute_value(attr);
                    if attr_value == XML_NS {
                        continue;
                    }
                    if ignore_namespace_definition_attribute
                        && ans.prefix().is_none()
                        && !eq_empty(ns_href(element), Some(attr_value))
                    {
                        // DOM-Parsing #47.
                        continue;
                    }
                    if ans.prefix().is_some() {
                        let value = local.find(attr.name());
                        if value != Some(attr_value) && map.found(attr.name(), attr_value) {
                            continue;
                        }
                    }
                    if ans.prefix() == Some(b"xmlns".as_slice()) {
                        candidate_prefix = Some(b"xmlns".to_vec());
                    }
                    // 3.5.2.2 and 3.5.2.3.
                    if well_formed
                        && (attr_value == XMLNS_NS
                            || (attr_value.is_empty() && candidate_prefix.is_some()))
                    {
                        return Err(Fail::Failed);
                    }
                } else if candidate_prefix.is_none() {
                    // DOM-Parsing #29.
                    let prefix = match ans.prefix() {
                        Some(p) if !local.contains(p) => {
                            map.add(p, Some(attribute_namespace));
                            local.add(p, Some(attribute_namespace));
                            p.to_vec()
                        }
                        _ => generate_prefix(map, local, Some(attribute_namespace), prefix_index),
                    };
                    self.w(b" xmlns:")?;
                    self.w(&prefix)?;
                    self.w(b"=\"")?;
                    self.text(attribute_namespace, true)?;
                    self.w(b"\"")?;
                    candidate_prefix = Some(prefix);
                }
            }
            self.w(b" ")?;
            if let Some(p) = &candidate_prefix {
                self.w(p)?;
                self.w(b":")?;
            }
            if well_formed
                && (!valid_ncname(attr.name())
                    || (attr.name() == b"xmlns" && attribute_namespace.is_none()))
            {
                return Err(Fail::Failed);
            }
            self.attribute_node_value(attr)?;
        }
        Ok(())
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-xml-serializing-an-element-node>.
    #[allow(clippy::too_many_lines)]
    fn element(
        &self,
        namespace: Option<&[u8]>,
        prefix_map: &NsPrefixMap,
        element: N,
        prefix_index: &mut u32,
        mut indent: i32,
        well_formed: bool,
    ) -> Res {
        if well_formed && !valid_ncname(element.name()) {
            return Err(Fail::Failed);
        }
        // Only format when there are no text/entityref/cdata children.
        let should_format = indent >= 0
            && element.children().is_some()
            && element.child_iter().all(|c| {
                !matches!(
                    c.t(),
                    ffi::XML_TEXT_NODE | ffi::XML_ENTITY_REF_NODE | ffi::XML_CDATA_SECTION_NODE
                )
            });

        self.w(b"<")?;
        let mut qualified_prefix: Option<Vec<u8>> = None;
        let name = element.name();
        let mut skip_end_tag = false;
        let mut ignore_namespace_definition_attribute = false;
        let mut map = prefix_map.clone();
        let mut local = LocalPrefixMap::default();
        let local_default_namespace = record_namespace_information(&mut map, &mut local, element);
        let mut inherited_ns: Option<&[u8]> = namespace;
        let ns = ns_href(element);

        if inherited_ns == ns {
            if local_default_namespace.is_some() {
                ignore_namespace_definition_attribute = true;
            }
            if ns_is_fast(element, &XML_TOKEN) {
                qualified_prefix = Some(b"xml".to_vec());
            }
            self.qname(qualified_prefix.as_deref(), name)?;
        } else {
            let prefix: Option<&[u8]> = ns_prefix(element);
            // DOM-Parsing #52.
            let mut candidate_prefix = if prefix.is_none() && eq_empty(ns, local_default_namespace)
            {
                None
            } else {
                retrieve_preferred_prefix(&map, &local, prefix, ns)
            };
            if prefix == Some(b"xmlns".as_slice()) {
                if well_formed {
                    return Err(Fail::Failed);
                }
                candidate_prefix = prefix.map(<[u8]>::to_vec);
            }
            if let Some(candidate) = candidate_prefix {
                qualified_prefix = Some(candidate);
                if let Some(ldn) = local_default_namespace {
                    if ldn != XML_NS {
                        inherited_ns = (!ldn.is_empty()).then_some(ldn);
                    }
                }
                self.qname(qualified_prefix.as_deref(), name)?;
            } else if let Some(p) = prefix {
                let p = if local.contains(p) {
                    generate_prefix(&mut map, &mut local, ns, prefix_index)
                } else {
                    map.add(p, ns);
                    // Not in the spec, avoids conflicts (default_namespace_move.phpt).
                    local.add(p, ns);
                    p.to_vec()
                };
                self.qname(Some(&p), name)?;
                self.w(b" xmlns:")?;
                self.w(&p)?;
                self.w(b"=\"")?;
                self.text(ns.unwrap_or_default(), true)?;
                self.w(b"\"")?;
                qualified_prefix = Some(p);
                if let Some(ldn) = local_default_namespace {
                    inherited_ns = (!ldn.is_empty()).then_some(ldn);
                }
            } else if local_default_namespace.is_none() || !eq_empty(local_default_namespace, ns) {
                // DOM-Parsing #47.
                ignore_namespace_definition_attribute = true;
                inherited_ns = ns;
                self.qname(None, name)?;
                self.w(b" xmlns=\"")?;
                self.text(ns.unwrap_or_default(), true)?;
                self.w(b"\"")?;
            } else {
                inherited_ns = ns;
                self.qname(None, name)?;
            }
        }

        self.attributes(
            element,
            &mut map,
            &mut local,
            prefix_index,
            ignore_namespace_definition_attribute,
            well_formed,
        )?;

        if element.children().is_none() {
            // SAFETY: per-thread libxml2 global.
            let save_no_empty_tags = unsafe { *ffi::__xmlSaveNoEmptyTags() } != 0;
            if save_no_empty_tags {
                // `<x></x>` style.
            } else if ns_is_fast(element, &HTML_TOKEN) {
                if VOID_ELEMENTS.contains(&name) {
                    self.w(b" /")?;
                    skip_end_tag = true;
                }
            } else {
                self.w(b"/")?;
                skip_end_tag = true;
            }
        }
        self.w(b">")?;

        if !skip_end_tag {
            if should_format {
                indent += 1;
            } else {
                indent = -1;
            }
            let first = if ns_is_fast(element, &HTML_TOKEN) && name == b"template" {
                self.private
                    .as_ref()
                    .and_then(|p| p.retrieve_template(element))
            } else {
                element.children()
            };
            let mut child = first;
            while let Some(c) = child {
                if should_format {
                    self.indents(indent)?;
                }
                self.algorithm(&map, c, inherited_ns, prefix_index, indent, well_formed)?;
                child = c.next();
            }
            if should_format {
                indent -= 1;
                self.indents(indent)?;
            }
            self.w(b"</")?;
            self.qname(qualified_prefix.as_deref(), name)?;
            self.w(b">")?;
        }
        Ok(())
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-xml-serializing-a-document-node>.
    fn document(
        &self,
        map: &NsPrefixMap,
        node: N,
        namespace: Option<&[u8]>,
        prefix_index: &mut u32,
        indent: i32,
        well_formed: bool,
    ) -> Res {
        // DOM-Parsing #50: the XML declaration through libxml2, using a
        // shallow copy of the document without children.
        self.flush_out()?;
        // SAFETY: bitwise copy of a live xmlDoc; libxml2 only reads it (and
        // restores any field it temporarily changes); the copy is never freed.
        let r = unsafe {
            let mut declaration_doc: ffi::xmlDoc = ptr::read(node.as_doc());
            declaration_doc.children = ptr::null_mut();
            declaration_doc.last = ptr::null_mut();
            ffi::xmlSaveDoc(self.ctxt, &raw mut declaration_doc)
        };
        if r < 0 {
            return Err(Fail::Failed);
        }
        self.flush_ctxt()?;
        for child in node.child_iter() {
            self.algorithm(map, child, namespace, prefix_index, indent, well_formed)?;
        }
        Ok(())
    }

    /// <https://w3c.github.io/DOM-Parsing/#dfn-xml-serialization-algorithm>.
    fn algorithm(
        &self,
        map: &NsPrefixMap,
        node: N,
        namespace: Option<&[u8]>,
        prefix_index: &mut u32,
        indent: i32,
        well_formed: bool,
    ) -> Res {
        if overflowed() {
            return Err(Fail::Stack);
        }
        match node.t() {
            ffi::XML_ELEMENT_NODE => {
                self.element(namespace, map, node, prefix_index, indent, well_formed)
            }
            ffi::XML_DOCUMENT_FRAG_NODE => {
                for child in node.child_iter() {
                    self.algorithm(map, child, namespace, prefix_index, indent, well_formed)?;
                }
                Ok(())
            }
            ffi::XML_HTML_DOCUMENT_NODE | ffi::XML_DOCUMENT_NODE => {
                self.document(map, node, namespace, prefix_index, indent, well_formed)
            }
            ffi::XML_TEXT_NODE => {
                if well_formed && !node.content_ptr().is_null() {
                    check_char_production(node.content())?;
                }
                self.text(node.content(), false)
            }
            ffi::XML_COMMENT_NODE => self.comment(node, well_formed),
            ffi::XML_PI_NODE => self.processing_instruction(node, well_formed),
            ffi::XML_CDATA_SECTION_NODE => self.cdata(node),
            ffi::XML_ATTRIBUTE_NODE => self.attribute_node(node),
            t => {
                self.flush_out()?;
                // SAFETY: live save context and node.
                if unsafe { ffi::xmlSaveTree(self.ctxt, node.p()) } < 0 {
                    return Err(Fail::Failed);
                }
                self.flush_ctxt()?;
                if t == ffi::XML_DTD_NODE {
                    return self.w(b"\n");
                }
                Ok(())
            }
        }
    }
}

/// `dom_xml_serialize`.
fn serialize(
    ctxt: *mut c_void,
    out: *mut ffi::xmlOutputBuffer,
    node: N,
    format: bool,
    well_formed: bool,
    private: Option<Rc<PrivateData>>,
) -> Res {
    let _scope = StackScope::enter();
    let mut map = NsPrefixMap::default();
    map.add(b"xml", Some(XML_NS));
    let mut prefix_index = 1u32;
    let ctx = Ctx { ctxt, out, private };
    ctx.algorithm(
        &map,
        node,
        None,
        &mut prefix_index,
        if format { 0 } else { -1 },
        well_formed,
    )
}

fn private_data_of(node: N) -> Option<Rc<PrivateData>> {
    DocInner::lookup(node.doc_ptr()).and_then(|d| d.private_data())
}

/// Serializes into a byte vector through a save context and an output
/// buffer sharing it, as `php_new_dom_dump_node_to_str_ex` does with its
/// `smart_str`.
fn serialize_to_vec(
    options: c_int,
    encoding: Option<&[u8]>,
    body: impl FnOnce(*mut c_void, *mut ffi::xmlOutputBuffer) -> Res,
) -> Result<Vec<u8>, Fail> {
    let mut data: Box<Vec<u8>> = Box::default();
    let enc = encoding.map(cs);
    let enc_ptr = enc.as_ref().map_or(ptr::null(), |e| e.as_ptr());
    let data_ptr: *mut c_void = (&raw mut *data).cast();
    let mut result = Err(Fail::Failed);
    crate::parse::with_generic_errors(|| {
        // SAFETY: the save context and output buffer both write into `data`,
        // which outlives them (they are closed here).
        unsafe {
            let ctxt = ffi::xmlSaveToIO(
                Some(write_vec),
                None,
                data_ptr,
                enc_ptr,
                ffi::XML_SAVE_AS_XML | options,
            );
            if ctxt.is_null() {
                return;
            }
            let handler = ffi::xmlFindCharEncodingHandler(enc_ptr);
            let out = ffi::xmlOutputBufferCreateIO(Some(write_vec), None, data_ptr, handler);
            let mut r = if out.is_null() {
                ffi::xmlCharEncCloseFunc(handler);
                Err(Fail::Failed)
            } else {
                let mut r = body(ctxt, out);
                if ffi::xmlOutputBufferFlush(out) < 0 && r.is_ok() {
                    r = Err(Fail::Failed);
                }
                if ffi::xmlOutputBufferClose(out) < 0 && r.is_ok() {
                    r = Err(Fail::Failed);
                }
                r
            };
            if ffi::xmlSaveClose(ctxt) < 0 && r.is_ok() {
                r = Err(Fail::Failed);
            }
            result = r;
        }
    });
    crate::parse::resume_panic();
    result.map(|()| *data)
}

fn stack_error() -> DomError {
    DomError::Error(STACK_OVERFLOW_MESSAGE.into())
}

/// `php_new_dom_dump_node_to_str_ex`.
pub(crate) fn dump_node_to_str_ex(
    node: N,
    options: i32,
    format: bool,
    encoding: Option<&[u8]>,
) -> DomResult<Option<Vec<u8>>> {
    let private = private_data_of(node);
    match serialize_to_vec(options, encoding, |ctxt, out| {
        serialize(ctxt, out, node, format, false, private)
    }) {
        Ok(v) => Ok(Some(v)),
        Err(Fail::Failed) => Ok(None),
        Err(Fail::Stack) => Err(stack_error()),
    }
}

/// `php_new_dom_dump_node_to_file`: the number of bytes written or `-1`.
pub(crate) fn dump_node_to_file(filename: &[u8], doc: N, node: N, format: bool) -> DomResult<i64> {
    let encoding = crate::raw::opt_cstr(doc.d().encoding);
    let private = private_data_of(node);
    let data = match serialize_to_vec(0, encoding, |ctxt, out| {
        serialize(ctxt, out, node, format, false, private)
    }) {
        Ok(v) => v,
        Err(Fail::Failed) => return Ok(-1),
        Err(Fail::Stack) => return Err(stack_error()),
    };
    let f = cs(filename);
    let written = crate::parse::with_generic_errors(|| {
        // SAFETY: output buffer created, written and closed here.
        unsafe {
            let out = ffi::xmlOutputBufferCreateFilename(f.as_ptr(), ptr::null_mut(), 0);
            if out.is_null() {
                return -1;
            }
            let mut ok = true;
            for chunk in data.chunks(1 << 20) {
                let len = c_int::try_from(chunk.len()).unwrap_or(0);
                if ffi::xmlOutputBufferWrite(out, len, chunk.as_ptr().cast()) < 0 {
                    ok = false;
                    break;
                }
            }
            let closed = ffi::xmlOutputBufferClose(out);
            if ok && closed >= 0 {
                i64::try_from(data.len()).unwrap_or(i64::MAX)
            } else {
                -1
            }
        }
    });
    crate::parse::resume_panic();
    Ok(written)
}

/// The XML branch of `dom_element_html_fragment_serialize` (`innerHTML`
/// of an element of a `Dom\XMLDocument`): the children serialized with
/// the require-well-formed flag.
pub(crate) fn fragment_serialize(node: N, private: Option<&Rc<PrivateData>>) -> DomResult<Vec<u8>> {
    let r = serialize_to_vec(0, Some(b"UTF-8"), |ctxt, out| {
        for child in node.child_iter() {
            serialize(ctxt, out, child, false, true, private.cloned())?;
        }
        Ok(())
    });
    match r {
        Ok(v) => Ok(v),
        Err(Fail::Stack) => Err(stack_error()),
        Err(Fail::Failed) => Err(DomError::dom_msg(
            DomExceptionCode::Syntax,
            "The resulting XML serialization is not well-formed",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_map_copy_on_write() {
        let mut a = NsPrefixMap::default();
        a.add(b"xml", Some(XML_NS));
        let mut b = a.clone();
        b.add(b"x", Some(b"urn:x"));
        assert!(!a.found(b"x", b"urn:x"));
        assert!(b.found(b"x", b"urn:x"));
        assert!(b.found(b"xml", XML_NS));
    }

    #[test]
    fn preferred_prefix_reverse_and_conflicts() {
        let mut map = NsPrefixMap::default();
        map.add(b"a", Some(b"urn:x"));
        map.add(b"b", Some(b"urn:x"));
        let mut local = LocalPrefixMap::default();
        assert_eq!(
            retrieve_preferred_prefix(&map, &local, None, Some(b"urn:x")).as_deref(),
            Some(b"b".as_slice())
        );
        assert_eq!(
            retrieve_preferred_prefix(&map, &local, Some(b"a"), Some(b"urn:x")).as_deref(),
            Some(b"a".as_slice())
        );
        local.add(b"b", Some(b"urn:other"));
        assert_eq!(
            retrieve_preferred_prefix(&map, &local, None, Some(b"urn:x")).as_deref(),
            Some(b"a".as_slice())
        );
        assert_eq!(
            retrieve_preferred_prefix(&map, &local, None, Some(b"urn:none")),
            None
        );
    }

    #[test]
    fn generated_prefixes_skip_local_names() {
        let mut map = NsPrefixMap::default();
        let mut local = LocalPrefixMap::default();
        local.add(b"ns1", Some(b"urn:a"));
        let mut idx = 1;
        assert_eq!(
            generate_prefix(&mut map, &mut local, Some(b"urn:b"), &mut idx),
            b"ns2"
        );
        assert_eq!(idx, 3);
        assert!(map.found(b"ns2", b"urn:b"));
    }

    #[test]
    fn char_production() {
        assert!(check_char_production(b"abc\t\n").is_ok());
        assert!(check_char_production("h\u{e9}llo \u{1F600}".as_bytes()).is_ok());
        assert!(check_char_production(b"a\x01").is_err());
        assert!(check_char_production(b"\xff").is_err());
        assert!(check_char_production("\u{FFFE}".as_bytes()).is_err());
    }

    #[test]
    fn equals_treat_null_as_empty() {
        assert!(eq_empty(None, Some(b"")));
        assert!(eq_empty(None, None));
        assert!(!eq_empty(Some(b"a"), None));
    }
}
