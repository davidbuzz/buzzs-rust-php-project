// Derived from php-src ext/dom/html5_parser.c. Similarity metric to original: 19%
//   Copyright © The PHP Group and Contributors. Authors: Nora Dossche <ndossche@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The lexbor → libxml2 bridge (`html5_parser.c`): converts a parsed
//! lexbor tree into libxml2 nodes using the `Dom\*` namespace model.

use std::ffi::{c_int, c_ushort};
use std::mem::size_of;
use std::ptr;

use crate::ffi;
use crate::lexbor::{
    ns_ids, LxbNode, NsIds, WorkItem, WorkList, LXB_COMMENT, LXB_DOCUMENT_TYPE, LXB_ELEMENT,
    LXB_TEXT,
};
use crate::ns::{
    PrivateData, MATHML_NS, MATHML_TOKEN, SVG_NS, SVG_TOKEN, XLINK_NS, XLINK_TOKEN, XMLNS_NS,
    XMLNS_TOKEN, XML_NS, XML_TOKEN,
};
use crate::raw::{Ns, N};

/// `lexbor_libxml2_bridge_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BridgeStatus {
    Ok,
    Overflow,
    Oom,
}

impl BridgeStatus {
    /// `dom_lexbor_libxml2_bridge_status_code_to_string`.
    pub(crate) fn message(self) -> &'static str {
        match self {
            BridgeStatus::Overflow => "string length overflow",
            BridgeStatus::Oom => "out of memory",
            BridgeStatus::Ok => "unknown error",
        }
    }
}

/// libxml2 reserves 2 pointer-sized words for interned strings.
const LXML_INTERNED_STRINGS_SIZE: usize = size_of::<*mut ()>() * 2;

/// Whether the push of a work item left the conversion out of memory:
/// php-src's `lexbor_array_obj_push_wo_cls` fails when growing the list
/// would exceed `memory_limit` (a fallible allocation); here the
/// allocation succeeds and records the violation, which the VM raises as
/// the fatal once the method returns, so the bridge stops at the same
/// point.
fn push_failed(pushed: bool) -> bool {
    !pushed || php_memlimit::is_pending()
}

fn sanitize_line_nr(line: usize) -> c_ushort {
    c_ushort::try_from(line).unwrap_or(c_ushort::MAX)
}

fn xml_malloc(size: usize) -> *mut std::ffi::c_void {
    // SAFETY: libxml2's allocator global (returns suitably aligned memory).
    unsafe { ffi::xmlMalloc.map_or(ptr::null_mut(), |m| m(size)) }
}

/// `lexbor_libxml2_bridge_new_text_node_fast`.
fn new_text_node_fast(doc: *mut ffi::xmlDoc, data: &[u8], compact: bool) -> *mut ffi::xmlNode {
    if compact && data.len() < LXML_INTERNED_STRINGS_SIZE {
        // See xmlSAX2TextNode(): the content lives inside the node (in the
        // `properties`/`nsDef` words), which xmlFreeNode() knows about.
        let p = xml_malloc(size_of::<ffi::xmlNode>()).cast::<ffi::xmlNode>();
        if p.is_null() {
            return p;
        }
        // SAFETY: fresh allocation of the right size; zeroed then filled.
        unsafe {
            ptr::write_bytes(p, 0, 1);
            (*p).name = ffi::xmlStringText.as_ptr();
            (*p).type_ = ffi::XML_TEXT_NODE;
            (*p).doc = doc;
            let content = (&raw mut (*p).properties).cast::<u8>();
            ptr::copy_nonoverlapping(data.as_ptr(), content, data.len());
            (*p).content = content;
        }
        p
    } else {
        // SAFETY: valid buffer; length checked by callers.
        unsafe {
            ffi::xmlNewDocTextLen(
                doc,
                data.as_ptr(),
                c_int::try_from(data.len()).unwrap_or(c_int::MAX),
            )
        }
    }
}

fn set_token(ns: *mut ffi::xmlNs, token: &'static crate::ns::Token) {
    if let Some(n) = Ns::new(ns) {
        n.set_private(token.addr());
    }
}

/// `lexbor_libxml2_bridge_convert`.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn convert(
    start_node: Option<LxbNode<'_>>,
    doc: *mut ffi::xmlDoc,
    root: N,
    compact_text_nodes: bool,
    create_default_ns: bool,
    in_template_contents: bool,
    private: &PrivateData,
) -> BridgeStatus {
    let ids: NsIds = ns_ids();
    let mut mapper = private.mapper.borrow_mut();
    let html_ns = mapper.ensure_html_ns();
    let mut xlink_ns: *mut ffi::xmlNs = ptr::null_mut();
    let mut xml_ns: *mut ffi::xmlNs = ptr::null_mut();
    let mut prefixed_xmlns_ns: *mut ffi::xmlNs = ptr::null_mut();

    let Some(mut work) = WorkList::new() else {
        return BridgeStatus::Oom;
    };
    let mut cur = start_node;
    while let Some(n) = cur {
        let pushed = work.push(WorkItem {
            node: n,
            current_active_namespace: ids.undef,
            lxml_parent: root,
            lxml_ns: ptr::null_mut(),
            in_template: in_template_contents,
        });
        if push_failed(pushed) {
            return BridgeStatus::Oom;
        }
        cur = n.prev();
    }

    let mut templates: Vec<(N, N)> = Vec::new();
    let mut status = BridgeStatus::Ok;
    while let Some(item) = work.pop() {
        let node = item.node;
        let lxml_parent = item.lxml_parent;
        let in_template = item.in_template;
        match node.node_type() {
            LXB_ELEMENT => {
                let name = crate::raw::cs(node.qname());
                // SAFETY: valid document and name.
                let el = unsafe {
                    ffi::xmlNewDocNode(doc, ptr::null_mut(), name.as_ptr().cast(), ptr::null())
                };
                let Some(el) = N::new(el) else {
                    status = BridgeStatus::Oom;
                    break;
                };
                // SAFETY: both nodes are live.
                unsafe { ffi::xmlAddChild(lxml_parent.p(), el.p()) };
                // SAFETY: live node.
                unsafe { (*el.p()).line = sanitize_line_nr(node.line()) };

                let entering_namespace = node.ns();
                let mut current_lxml_ns = item.lxml_ns;
                if create_default_ns && entering_namespace != item.current_active_namespace {
                    if entering_namespace == ids.html {
                        current_lxml_ns = html_ns;
                    } else {
                        let (token, href): (&'static crate::ns::Token, &[u8]) =
                            if entering_namespace == ids.svg {
                                (&SVG_TOKEN, SVG_NS)
                            } else if entering_namespace == ids.math {
                                (&MATHML_TOKEN, MATHML_NS)
                            } else {
                                (&crate::ns::HTML_TOKEN, crate::ns::XHTML_NS)
                            };
                        current_lxml_ns = mapper.get_ns(None, Some(href));
                        set_token(current_lxml_ns, token);
                    }
                }
                el.set_ns(current_lxml_ns);

                let mut child_parent = el;
                let mut child_node = node.last_child();
                let mut child_in_template = in_template;
                if node.is_template() {
                    child_in_template = true;
                    if create_default_ns {
                        // SAFETY: valid document.
                        let Some(frag) = N::new(unsafe { ffi::xmlNewDocFragment(doc) }) else {
                            status = BridgeStatus::Oom;
                            break;
                        };
                        frag.set_parent(Some(el));
                        templates.push((el, frag));
                        child_parent = frag;
                    }
                    if let Ok(last) = node.template_content_last_child() {
                        child_node = last;
                    }
                }

                while let Some(c) = child_node {
                    let pushed = work.push(WorkItem {
                        node: c,
                        current_active_namespace: entering_namespace,
                        lxml_parent: child_parent,
                        lxml_ns: current_lxml_ns,
                        in_template: child_in_template,
                    });
                    if push_failed(pushed) {
                        status = BridgeStatus::Oom;
                        break;
                    }
                    child_node = c.prev();
                }
                if status != BridgeStatus::Ok {
                    break;
                }

                let mut last_added: *mut ffi::xmlAttr = ptr::null_mut();
                let mut attr = node.first_attr();
                while let Some(a) = attr {
                    attr = a.next();
                    let mut local_name = a.qname();
                    if a.has_prefix() {
                        if let Some(pos) = local_name.iter().position(|&c| c == b':') {
                            local_name = &local_name[pos + 1..];
                        }
                    }
                    let value = a.value();
                    if local_name.len() >= c_int::MAX as usize || value.len() >= c_int::MAX as usize
                    {
                        status = BridgeStatus::Overflow;
                        break;
                    }
                    let la = xml_malloc(size_of::<ffi::xmlAttr>()).cast::<ffi::xmlAttr>();
                    if la.is_null() {
                        status = BridgeStatus::Oom;
                        break;
                    }
                    // SAFETY: fresh allocation, zeroed and filled; strings
                    // interned in the document dictionary.
                    unsafe {
                        ptr::write_bytes(la, 0, 1);
                        (*la).type_ = ffi::XML_ATTRIBUTE_NODE;
                        (*la).parent = el.p();
                        (*la).name = ffi::xmlDictLookup(
                            (*doc).dict,
                            local_name.as_ptr(),
                            c_int::try_from(local_name.len()).unwrap_or(0),
                        );
                        (*la).doc = doc;
                    }
                    let text = new_text_node_fast(doc, value, true);
                    if text.is_null() {
                        // SAFETY: our own attribute.
                        unsafe { ffi::xmlFreeProp(la) };
                        status = BridgeStatus::Oom;
                        break;
                    }
                    // SAFETY: link text under attribute.
                    unsafe {
                        (*la).children = text;
                        (*la).last = text;
                        (*text).parent = la.cast();
                    }

                    let ans = a.ns();
                    let mut ns_ptr: *mut ffi::xmlNs = ptr::null_mut();
                    if ans == ids.xmlns && (a.has_prefix() || local_name == b"xmlns") {
                        if local_name == b"xmlns" {
                            ns_ptr = mapper.ensure_prefixless_xmlns_ns();
                        } else {
                            if prefixed_xmlns_ns.is_null() {
                                prefixed_xmlns_ns =
                                    mapper.get_ns_nullsafe(Some(b"xmlns"), Some(XMLNS_NS));
                            }
                            ns_ptr = prefixed_xmlns_ns;
                        }
                        set_token(ns_ptr, &XMLNS_TOKEN);
                    } else if a.has_prefix() && ans == ids.xlink {
                        if xlink_ns.is_null() {
                            xlink_ns = mapper.get_ns_nullsafe(Some(b"xlink"), Some(XLINK_NS));
                            set_token(xlink_ns, &XLINK_TOKEN);
                        }
                        ns_ptr = xlink_ns;
                    } else if a.has_prefix() && ans == ids.xml {
                        if xml_ns.is_null() {
                            xml_ns = mapper.get_ns_nullsafe(Some(b"xml"), Some(XML_NS));
                            set_token(xml_ns, &XML_TOKEN);
                        }
                        ns_ptr = xml_ns;
                    }
                    // SAFETY: linking our attribute into the element's list.
                    unsafe {
                        (*la).ns = ns_ptr;
                        if last_added.is_null() {
                            (*el.p()).properties = la;
                        } else {
                            (*last_added).next = la;
                            (*la).prev = last_added;
                        }
                    }
                    last_added = la;

                    if local_name == b"id" && ns_ptr.is_null() {
                        if !in_template {
                            let v = crate::raw::cs(value);
                            // SAFETY: live document and attribute; a duplicate
                            // ID is simply not registered.
                            unsafe { ffi::xmlAddID(ptr::null_mut(), doc, v.as_ptr().cast(), la) };
                        }
                        // Even a duplicate ID attribute is marked as an ID.
                        // SAFETY: our attribute.
                        unsafe { (*la).atype = ffi::XML_ATTRIBUTE_ID };
                    }
                }
                if status != BridgeStatus::Ok {
                    break;
                }
            }
            LXB_TEXT => {
                let data = node.chardata();
                if data.len() >= c_int::MAX as usize {
                    status = BridgeStatus::Overflow;
                    break;
                }
                let text = new_text_node_fast(doc, data, compact_text_nodes);
                if text.is_null() {
                    status = BridgeStatus::Oom;
                    break;
                }
                // SAFETY: live nodes.
                unsafe {
                    ffi::xmlAddChild(lxml_parent.p(), text);
                    let line = node.line();
                    if line >= usize::from(c_ushort::MAX) {
                        (*text).line = c_ushort::MAX;
                        (*text).psvi = line as *mut std::ffi::c_void;
                    } else {
                        (*text).line = sanitize_line_nr(line);
                    }
                }
            }
            LXB_DOCUMENT_TYPE => {
                let name = crate::raw::cs(node.doctype_name());
                let public_id = node.doctype_public_id();
                let system_id = node.doctype_system_id();
                let p = (!public_id.is_empty()).then(|| crate::raw::cs(public_id));
                let s = (!system_id.is_empty()).then(|| crate::raw::cs(system_id));
                // SAFETY: valid document and strings.
                let dtd = unsafe {
                    ffi::xmlCreateIntSubset(
                        doc,
                        name.as_ptr().cast(),
                        p.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast()),
                        s.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast()),
                    )
                };
                if dtd.is_null() {
                    status = BridgeStatus::Oom;
                    break;
                }
            }
            LXB_COMMENT => {
                let c = crate::raw::cs(node.chardata());
                // SAFETY: valid document and string.
                let comment = unsafe { ffi::xmlNewDocComment(doc, c.as_ptr().cast()) };
                if comment.is_null() {
                    status = BridgeStatus::Oom;
                    break;
                }
                // SAFETY: live nodes.
                unsafe {
                    ffi::xmlAddChild(lxml_parent.p(), comment);
                    (*comment).line = sanitize_line_nr(node.line());
                }
            }
            _ => {}
        }
    }
    drop(mapper);
    for (el, frag) in templates {
        private.add_element_ns_hook(el);
        private.add_template(el, frag);
    }
    status
}

/// `lexbor_libxml2_bridge_convert_document`.
pub(crate) fn convert_document(
    last_child: Option<LxbNode<'_>>,
    compact_text_nodes: bool,
    create_default_ns: bool,
    private: &PrivateData,
) -> Result<N, BridgeStatus> {
    let doc = crate::html::create_html_doc().ok_or(BridgeStatus::Oom)?;
    let status = convert(
        last_child,
        doc.as_doc(),
        doc,
        compact_text_nodes,
        create_default_ns,
        false,
        private,
    );
    if status != BridgeStatus::Ok {
        private.remove_templates_in_subtree(doc);
        // SAFETY: our own document, never exposed.
        unsafe { ffi::xmlFreeDoc(doc.as_doc()) };
        return Err(status);
    }
    Ok(doc)
}

/// `lexbor_libxml2_bridge_convert_fragment`.
pub(crate) fn convert_fragment(
    start: Option<LxbNode<'_>>,
    doc: *mut ffi::xmlDoc,
    compact_text_nodes: bool,
    create_default_ns: bool,
    in_template_contents: bool,
    private: &PrivateData,
) -> Result<N, BridgeStatus> {
    // SAFETY: valid document.
    let frag = N::new(unsafe { ffi::xmlNewDocFragment(doc) }).ok_or(BridgeStatus::Oom)?;
    let status = convert(
        start,
        doc,
        frag,
        compact_text_nodes,
        create_default_ns,
        in_template_contents,
        private,
    );
    if status != BridgeStatus::Ok {
        private.remove_templates_in_subtree(frag);
        // SAFETY: our own fragment, never exposed.
        unsafe { ffi::xmlFreeNode(frag.p()) };
        return Err(status);
    }
    Ok(frag)
}
