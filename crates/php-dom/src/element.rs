// Derived from php-src ext/dom/element.c. Similarity metric to original: 23%
//   Copyright © The PHP Group and Contributors.
//   Authors: Christian Stocker <chregu@php.net>
//            Rob Richards <rrichards@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `DOMElement` / `Dom\Element` / `Dom\HTMLElement` (`element.c`).

use std::ptr;

use crate::error::{throw, DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::lists::{new_list, MapKind, NodeList};
use crate::node::set_document_ref_pointers_attr;
use crate::ns::{self, ns_is_fast, ns_is_fast_ex, HTML_TOKEN, XMLNS_TOKEN};
use crate::object::{node_list_unlink, remove_all_children, set_old_ns, DocRef, Node};
use crate::raw::{c_len, cs, str_eq, take, Ns, N};
use crate::tree::{self, NodeOrString};
use crate::value::{DomValue, NamespaceInfo};

/// `xmlHasNsProp(node, name, ns)`.
pub(crate) fn has_ns_prop(node: N, name: &[u8], ns: Option<&[u8]>) -> Option<N> {
    let n = cs(name);
    let u = ns.map(cs);
    // SAFETY: live node; strings valid.
    N::from_attr(unsafe {
        ffi::xmlHasNsProp(
            node.p(),
            n.as_ptr().cast(),
            u.as_ref().map_or(ptr::null(), |u| u.as_ptr().cast()),
        )
    })
}

/// `xmlHasProp(node, name)`.
pub(crate) fn has_prop(node: N, name: &[u8]) -> Option<N> {
    let n = cs(name);
    // SAFETY: live node.
    N::from_attr(unsafe { ffi::xmlHasProp(node.p(), n.as_ptr().cast()) })
}

/// The position argument of `insertAdjacent*()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjacentPosition {
    /// `beforebegin`.
    BeforeBegin,
    /// `afterbegin`.
    AfterBegin,
    /// `beforeend`.
    BeforeEnd,
    /// `afterend`.
    AfterEnd,
}

impl AdjacentPosition {
    /// `dom_adjacent_position_str_to_enum` (case-insensitive).
    pub fn parse(s: &[u8]) -> Option<Self> {
        let l = s.to_ascii_lowercase();
        match &l[..] {
            b"beforebegin" => Some(Self::BeforeBegin),
            b"afterbegin" => Some(Self::AfterBegin),
            b"beforeend" => Some(Self::BeforeEnd),
            b"afterend" => Some(Self::AfterEnd),
            _ => None,
        }
    }

    /// The enum case name (`Dom\AdjacentPosition::*`).
    pub fn case_name(self) -> &'static str {
        match self {
            Self::BeforeBegin => "BeforeBegin",
            Self::AfterBegin => "AfterBegin",
            Self::BeforeEnd => "BeforeEnd",
            Self::AfterEnd => "AfterEnd",
        }
    }

    /// The backing value.
    pub fn value(self) -> &'static str {
        match self {
            Self::BeforeBegin => "beforebegin",
            Self::AfterBegin => "afterbegin",
            Self::BeforeEnd => "beforeend",
            Self::AfterEnd => "afterend",
        }
    }
}

/// What `dom_get_attribute_or_nsdecl` found.
#[derive(Debug, Clone, Copy)]
enum AttrOrNs {
    Attr(N),
    Ns(Ns),
}

impl AttrOrNs {
    fn t(self) -> i32 {
        match self {
            Self::Attr(n) => n.t(),
            Self::Ns(_) => ffi::XML_NAMESPACE_DECL,
        }
    }
}

/// `dom_check_register_attribute_id`.
pub(crate) fn check_register_attribute_id(attr: N, doc: Option<&DocRef>) {
    if let Some(d) = doc {
        d.mark_modified_since_parsing();
    }
    if attr.atype() != ffi::XML_ATTRIBUTE_ID
        && attr
            .doc()
            .is_some_and(|d| d.t() == ffi::XML_HTML_DOCUMENT_NODE)
        && attr.ns().is_none()
        && attr.name() == b"id"
    {
        attr.set_atype(ffi::XML_ATTRIBUTE_ID);
    }
}

/// `php_set_attribute_id`.
fn set_attribute_id(attr: N, is_id: bool, doc: Option<&DocRef>) {
    if is_id && attr.atype() != ffi::XML_ATTRIBUTE_ID {
        attr.set_atype(ffi::XML_ATTRIBUTE_ID);
    } else if !is_id && attr.atype() == ffi::XML_ATTRIBUTE_ID {
        // SAFETY: live attribute.
        unsafe { ffi::xmlRemoveID(attr.doc_ptr(), attr.attr()) };
        attr.set_atype(0);
    }
    if let Some(d) = doc {
        d.mark_modified_since_parsing();
    }
}

/// `dom_create_attribute`.
fn create_attribute(node: N, name: &[u8], value: &[u8]) -> Option<AttrOrNs> {
    let v = cs(value);
    if name == b"xmlns" {
        // SAFETY: live element.
        let ns = unsafe { ffi::xmlNewNs(node.p(), v.as_ptr().cast(), ptr::null()) };
        Ns::new(ns).map(AttrOrNs::Ns)
    } else {
        let n = cs(name);
        // SAFETY: live element.
        N::from_attr(unsafe { ffi::xmlSetProp(node.p(), n.as_ptr().cast(), v.as_ptr().cast()) })
            .map(AttrOrNs::Attr)
    }
}

/// `dom_deep_ns_redef`.
fn deep_ns_redef(node: N, ns_to_redefine: Ns) {
    let mut worklist: Vec<(N, *mut ffi::xmlNs)> = vec![(node, ptr::null_mut())];
    while let Some((cur, mut defined)) = worklist.pop() {
        if cur.ns_ptr() == ns_to_redefine.p() {
            if defined.is_null() {
                // SAFETY: live element; ns strings valid.
                defined = unsafe {
                    ffi::xmlNewNs(
                        cur.p(),
                        ns_to_redefine.href_ptr(),
                        ns_to_redefine.prefix_ptr(),
                    )
                };
            }
            cur.set_ns(defined);
        }
        for attr in cur.attr_iter() {
            if attr.ns_ptr() == ns_to_redefine.p() {
                if defined.is_null() {
                    // SAFETY: as above.
                    defined = unsafe {
                        ffi::xmlNewNs(
                            cur.p(),
                            ns_to_redefine.href_ptr(),
                            ns_to_redefine.prefix_ptr(),
                        )
                    };
                }
                attr.set_ns(defined);
            }
        }
        for child in cur.child_iter() {
            if child.is_element() {
                worklist.push((child, defined));
            }
        }
    }
}

/// `dom_remove_attribute`.
fn remove_attribute_raw(thisp: N, attrp: AttrOrNs, doc: Option<&DocRef>) -> bool {
    match attrp {
        AttrOrNs::Attr(a) if a.t() == ffi::XML_ATTRIBUTE_NODE => {
            if Node::existing(a).is_none() {
                node_list_unlink(a.children());
                tree::unlink(a);
                // SAFETY: handle-less unlinked attribute.
                unsafe { ffi::xmlFreeProp(a.attr()) };
            } else {
                tree::unlink(a);
            }
        }
        AttrOrNs::Ns(ns) => {
            let first = thisp.ns_def();
            if first == Some(ns) {
                thisp.set_ns_def(ns.next().map_or(ptr::null_mut(), Ns::p));
            } else if let Some(mut prev) = first {
                let mut cur = prev.next();
                while let Some(c) = cur {
                    if c == ns {
                        prev.set_next(c.next().map_or(ptr::null_mut(), Ns::p));
                        break;
                    }
                    prev = c;
                    cur = c.next();
                }
            } else {
                return true;
            }
            ns.set_next(ptr::null_mut());
            set_old_ns(thisp.doc_ptr(), ns.p());
            deep_ns_redef(thisp, ns);
        }
        AttrOrNs::Attr(_) => return false,
    }
    if let Some(d) = doc {
        d.invalidate();
    }
    true
}

fn remove_eliminated_ns_single(node: N, ns: *mut ffi::xmlNs) {
    if node.ns_ptr() == ns {
        node.set_ns(ptr::null_mut());
    }
    for attr in node.attr_iter() {
        if attr.ns_ptr() == ns {
            attr.set_ns(ptr::null_mut());
        }
    }
}

fn eliminate_ns(nodep: N, nsptr: Ns) {
    // SAFETY: the namespace record is owned by nodep's nsDef list.
    unsafe {
        let p = nsptr.p();
        if !(*p).href.is_null() {
            crate::raw::xml_free((*p).href.cast_mut().cast());
            (*p).href = ptr::null();
        }
        if !(*p).prefix.is_null() {
            crate::raw::xml_free((*p).prefix.cast_mut().cast());
            (*p).prefix = ptr::null();
        }
    }
    let mut cur = nodep.ns_def();
    if cur == Some(nsptr) {
        nodep.set_ns_def(nsptr.next().map_or(ptr::null_mut(), Ns::p));
    } else {
        while let Some(c) = cur {
            if c.next() == Some(nsptr) {
                c.set_next(nsptr.next().map_or(ptr::null_mut(), Ns::p));
                break;
            }
            cur = c.next();
        }
    }
    nsptr.set_next(ptr::null_mut());
    set_old_ns(nodep.doc_ptr(), nsptr.p());
    remove_eliminated_ns_single(nodep, nsptr.p());
    let mut c = nodep.children();
    while let Some(x) = c {
        if x.is_element() {
            remove_eliminated_ns_single(x, nsptr.p());
        }
        c = tree::next_in_tree_order(x, Some(nodep));
    }
}

/// `php_dom_adopt_node`.
pub(crate) fn adopt_node(node: N, intern: &Node, new_doc: *mut ffi::xmlDoc) -> bool {
    crate::document::adopt_node_raw(node, intern.doc_ref().as_ref(), new_doc)
}

impl Node {
    fn get_attribute_or_nsdecl(&self, elem: N, name: &[u8]) -> Option<AttrOrNs> {
        if self.is_modern() {
            return tree::get_attribute_node(elem, name).map(AttrOrNs::Attr);
        }
        let c = cs(name);
        let mut len: i32 = 0;
        // SAFETY: valid C string; result points into it.
        let nqname = unsafe { ffi::xmlSplitQName3(c.as_ptr().cast(), &raw mut len) };
        if let Some(nq) = crate::raw::opt_cstr(nqname) {
            let len = usize::try_from(len).unwrap_or(0);
            let nq = nq.to_vec();
            if len == 5 && name.starts_with(b"xmlns:") {
                let mut ns = elem.ns_def();
                while let Some(x) = ns {
                    if x.prefix() == Some(&nq[..]) {
                        return Some(AttrOrNs::Ns(x));
                    }
                    ns = x.next();
                }
                return None;
            }
            let prefix = cs(&name[..len]);
            // SAFETY: live element.
            let ns = Ns::new(unsafe {
                ffi::xmlSearchNs(elem.doc_ptr(), elem.p(), prefix.as_ptr().cast())
            });
            if let Some(ns) = ns {
                return has_ns_prop(elem, &nq, Some(ns.href())).map(AttrOrNs::Attr);
            }
        } else if name == b"xmlns" {
            let mut ns = elem.ns_def();
            while let Some(x) = ns {
                if x.prefix_ptr().is_null() {
                    return Some(AttrOrNs::Ns(x));
                }
                ns = x.next();
            }
            return None;
        }
        has_ns_prop(elem, name, None).map(AttrOrNs::Attr)
    }

    /// `new DOMElement($qualifiedName, $value, $namespace)`.
    pub fn construct_element(
        &self,
        name: &[u8],
        value: Option<&[u8]>,
        uri: &[u8],
    ) -> DomResult<()> {
        if !tree::valid_name(name) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        #[allow(clippy::needless_late_init)]
        let nodep;
        if uri.is_empty() {
            if tree::split_qname2(name).is_some() {
                return Err(DomError::dom(C::Namespace));
            }
            let n = cs(name);
            // SAFETY: valid C string.
            nodep = N::new(unsafe {
                ffi::xmlNewDocNode(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    n.as_ptr().cast(),
                    ptr::null(),
                )
            });
        } else {
            let (local, prefix) = tree::check_qname(name, uri.len()).map_err(DomError::dom)?;
            let l = cs(&local);
            // SAFETY: valid C string.
            let created = N::new(unsafe {
                ffi::xmlNewDocNode(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    l.as_ptr().cast(),
                    ptr::null(),
                )
            });
            if let Some(c) = created {
                let Ok(ns) = tree::get_ns(c, uri, prefix.as_deref()) else {
                    tree::free_node(c);
                    return Err(DomError::dom(C::Namespace));
                };
                // SAFETY: live element.
                unsafe { ffi::xmlSetNs(c.p(), ns) };
            }
            nodep = created;
        }
        let nodep = nodep.ok_or_else(|| DomError::dom(C::InvalidState))?;
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            // SAFETY: live node.
            unsafe { ffi::xmlNodeSetContentLen(nodep.p(), v.as_ptr(), c_len(v.len())) };
        }
        self.bind_constructed(nodep);
        Ok(())
    }

    /// `$tagName`.
    pub fn tag_name(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        let upper = self.is_modern() && ns::is_html_and_document_is_html(n);
        Ok(tree::qualified_name(n, upper))
    }

    fn reflected_read(&self, name: &[u8]) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        let c = cs(name);
        // SAFETY: live node.
        Ok(take(unsafe { ffi::xmlGetNoNsProp(n.p(), c.as_ptr().cast()) }).unwrap_or_default())
    }

    fn reflected_write(&self, name: &[u8], value: &[u8]) -> DomResult<Option<N>> {
        let n = self.n().ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.invalidate_doc();
        Ok(tree::set_attribute_value(n, ptr::null_mut(), name, value))
    }

    pub(crate) fn invalidate_doc(&self) {
        if let Some(d) = self.doc_ref() {
            d.invalidate();
        }
    }

    /// `$className`.
    pub fn class_name_attr(&self) -> DomResult<Vec<u8>> {
        self.reflected_read(b"class")
    }

    /// `$className = $value`.
    pub fn set_class_name_attr(&self, value: &[u8]) -> DomResult<()> {
        self.reflected_write(b"class", value).map(|_| ())
    }

    /// `$id`.
    pub fn id(&self) -> DomResult<Vec<u8>> {
        self.reflected_read(b"id")
    }

    /// `$id = $value`.
    pub fn set_id(&self, value: &[u8]) -> DomResult<()> {
        if let Some(attr) = self.reflected_write(b"id", value)? {
            set_attribute_id(attr, true, self.doc_ref().as_ref());
        }
        Ok(())
    }

    /// `$classList` (same object on every read).
    pub fn class_list(&self) -> DomResult<crate::tokenlist::TokenList> {
        let mut extra = self.0.extra.borrow_mut();
        if let Some(t) = &extra.class_list {
            return Ok(t.clone());
        }
        let t = crate::tokenlist::TokenList::new(self.clone(), b"class");
        extra.class_list = Some(t.clone());
        Ok(t)
    }

    /// `$children` (same object on every read).
    pub fn children_collection(&self) -> DomResult<NodeList> {
        let mut extra = self.0.extra.borrow_mut();
        if let Some(c) = &extra.children {
            return Ok(c.clone());
        }
        let c = new_list(
            "Dom\\HTMLCollection",
            Some(self.clone()),
            MapKind::ChildElements,
        );
        extra.children = Some(c.clone());
        Ok(c)
    }

    /// `$firstElementChild`.
    pub fn first_element_child(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        Ok(n.child_iter().find(|c| c.is_element()).map(Node::wrap))
    }

    /// `$lastElementChild`.
    pub fn last_element_child(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        let mut cur = n.last();
        while let Some(c) = cur {
            if c.is_element() {
                return Ok(Some(Node::wrap(c)));
            }
            cur = c.prev();
        }
        Ok(None)
    }

    /// `$childElementCount`.
    pub fn child_element_count(&self) -> DomResult<i64> {
        let n = self.prop_node()?;
        Ok(i64::try_from(n.child_iter().filter(|c| c.is_element()).count()).unwrap_or(i64::MAX))
    }

    /// `getAttribute($name)`: `None` → `""` (legacy) / `null` (modern).
    pub fn get_attribute(&self, name: &[u8]) -> DomResult<Option<Vec<u8>>> {
        let n = self.node()?;
        let value = match self.get_attribute_or_nsdecl(n, name) {
            Some(AttrOrNs::Attr(a)) if a.t() == ffi::XML_ATTRIBUTE_NODE => {
                // SAFETY: live attribute.
                take(unsafe {
                    ffi::xmlNodeListGetString(
                        a.doc_ptr(),
                        a.children().map_or(ptr::null(), |c| c.p().cast_const()),
                        1,
                    )
                })
            }
            Some(AttrOrNs::Ns(ns)) => Some(ns.href().to_vec()),
            Some(AttrOrNs::Attr(decl)) => {
                // SAFETY: attribute declaration layout.
                let dv = unsafe { (*decl.p().cast::<ffi::xmlAttribute>()).defaultValue };
                crate::raw::opt_cstr(dv).map(<[u8]>::to_vec)
            }
            None => None,
        };
        if value.is_none() && !self.is_modern() {
            return Ok(Some(Vec::new()));
        }
        Ok(value)
    }

    /// `getAttributeNames()`.
    pub fn get_attribute_names(&self) -> DomResult<Vec<Vec<u8>>> {
        let n = self.node()?;
        let mut out = Vec::new();
        if !self.is_modern() {
            let mut ns = n.ns_def();
            while let Some(x) = ns {
                match x.prefix() {
                    None => out.push(b"xmlns".to_vec()),
                    Some(p) => {
                        let mut v = b"xmlns:".to_vec();
                        v.extend_from_slice(p);
                        out.push(v);
                    }
                }
                ns = x.next();
            }
        }
        for a in n.attr_iter() {
            out.push(tree::qualified_name(a, false));
        }
        Ok(out)
    }

    /// `setAttribute($name, $value)`. Legacy returns the `DOMAttr`, `true`
    /// (namespace declaration) or `false`; modern returns null.
    pub fn set_attribute(&self, name: &[u8], value: &[u8]) -> DomResult<DomValue> {
        if name.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        if !tree::valid_name(name) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        let nodep = self.node()?;
        if self.is_modern() {
            let processed = if ns::is_html_and_document_is_html(nodep) {
                name.to_ascii_lowercase()
            } else {
                name.to_vec()
            };
            if let Some(attr) = tree::get_attribute_node(nodep, name) {
                crate::attr::value_will_change(self, attr);
                remove_all_children(attr);
                let v = cs(value);
                // SAFETY: live attribute.
                unsafe {
                    let text = ffi::xmlNewDocText(attr.doc_ptr(), v.as_ptr().cast());
                    ffi::xmlAddChild(attr.p(), text);
                }
            } else {
                let p = cs(&processed);
                let v = cs(value);
                // SAFETY: live element.
                let attr = N::from_attr(unsafe {
                    ffi::xmlSetNsProp(
                        nodep.p(),
                        ptr::null_mut(),
                        p.as_ptr().cast(),
                        v.as_ptr().cast(),
                    )
                });
                if let Some(a) = attr {
                    check_register_attribute_id(a, self.doc_ref().as_ref());
                }
            }
            return Ok(DomValue::Null);
        }
        if let Some(found) = self.get_attribute_or_nsdecl(nodep, name) {
            match found.t() {
                ffi::XML_ATTRIBUTE_NODE => {
                    if let AttrOrNs::Attr(a) = found {
                        crate::attr::value_will_change(self, a);
                        node_list_unlink(a.children());
                    }
                }
                ffi::XML_NAMESPACE_DECL => return Ok(DomValue::Bool(false)),
                _ => {}
            }
        }
        match create_attribute(nodep, name, value) {
            None => Err(DomError::arg_value(1, "must be a valid XML attribute")),
            Some(AttrOrNs::Ns(_)) => Ok(DomValue::Bool(true)),
            Some(AttrOrNs::Attr(a)) => Ok(DomValue::Node(Node::wrap(a))),
        }
    }

    /// `removeAttribute($name)`.
    pub fn remove_attribute(&self, name: &[u8]) -> DomResult<bool> {
        let nodep = self.node()?;
        match self.get_attribute_or_nsdecl(nodep, name) {
            None => Ok(false),
            Some(a) => Ok(remove_attribute_raw(nodep, a, self.doc_ref().as_ref())),
        }
    }

    /// `getAttributeNode($name)`: legacy false → `Ok(None)` with
    /// `DomValue::Bool(false)` semantics handled by the dispatcher.
    pub fn get_attribute_node(&self, name: &[u8]) -> DomResult<Option<Node>> {
        let nodep = self.node()?;
        match self.get_attribute_or_nsdecl(nodep, name) {
            None => Ok(None),
            Some(AttrOrNs::Ns(ns)) => Ok(Some(crate::xpath::create_fake_namespace_decl(
                nodep, ns, self,
            ))),
            Some(AttrOrNs::Attr(a)) => Ok(Some(Node::wrap(a))),
        }
    }

    fn set_attribute_node_common(&self, attr: &Node, use_ns: bool) -> DomResult<Option<Node>> {
        let modern = self.family_is_modern();
        let nodep = self.node()?;
        let attrp = attr.node()?;
        if modern {
            if attrp.parent().is_some_and(|p| p != nodep) {
                return Err(DomError::dom(C::InuseAttribute));
            }
            if !attrp.doc_ptr().is_null() && attrp.doc_ptr() != nodep.doc_ptr() {
                adopt_node(attrp, self, nodep.doc_ptr());
            }
        } else if !(attrp.doc_ptr().is_null() || attrp.doc_ptr() == nodep.doc_ptr()) {
            return Err(throw(
                C::WrongDocument,
                self.doc_ref().is_none_or(|d| d.strict()),
            ));
        }
        let nsp = attrp.ns();
        let mut existing = match (use_ns, nsp) {
            (true, Some(ns)) => has_ns_prop(nodep, attrp.name(), Some(ns.href())),
            (_, None) => has_ns_prop(nodep, attrp.name(), None),
            _ => has_prop(nodep, attrp.name()),
        };
        if existing.is_some_and(|e| e.t() == ffi::XML_ATTRIBUTE_DECL) {
            existing = None;
        }
        if let Some(e) = existing {
            if e == attrp && Node::existing(e).is_some() {
                return Ok(None);
            }
            tree::unlink(e);
        }
        if attrp.parent().is_some() {
            tree::unlink(attrp);
        }
        if attrp.doc_ptr().is_null() && !nodep.doc_ptr().is_null() {
            if let Some(doc) = self.doc_ref() {
                set_document_ref_pointers_attr(attrp, &doc);
            }
        }
        // SAFETY: live element and detached attribute (no existing duplicate).
        unsafe { ffi::xmlAddChild(nodep.p(), attrp.p()) };
        if modern {
            check_register_attribute_id(attrp, self.doc_ref().as_ref());
        } else {
            if let Some(d) = self.doc_ref() {
                d.mark_modified_since_parsing();
            }
            ns::reconcile_attribute_namespace_after_insertion(attrp);
        }
        Ok(existing.map(Node::wrap))
    }

    /// `setAttributeNode($attr)`: the replaced attribute or null.
    pub fn set_attribute_node(&self, attr: &Node) -> DomResult<Option<Node>> {
        self.set_attribute_node_common(attr, self.family_is_modern())
    }

    /// `setAttributeNodeNS($attr)`.
    pub fn set_attribute_node_ns(&self, attr: &Node) -> DomResult<Option<Node>> {
        self.set_attribute_node_common(attr, true)
    }

    /// `removeAttributeNode($attr)`.
    pub fn remove_attribute_node(&self, attr: &Node) -> DomResult<Node> {
        let nodep = self.node()?;
        let attrp = attr.node()?;
        if attrp.parent() != Some(nodep) {
            return Err(throw(
                C::NotFound,
                self.doc_ref().is_none_or(|d| d.strict()),
            ));
        }
        self.invalidate_doc();
        tree::unlink(attrp);
        Ok(Node::wrap(attrp))
    }

    /// `getElementsByTagName($name)`.
    pub fn get_elements_by_tag_name(&self, name: &[u8]) -> DomResult<NodeList> {
        if name.len() > i32::MAX as usize {
            return Err(DomError::arg_value(1, "is too long"));
        }
        self.node()?;
        let class = if self.family_is_modern() {
            "Dom\\HTMLCollection"
        } else {
            "DOMNodeList"
        };
        Ok(new_list(
            class,
            Some(self.clone()),
            MapKind::ByTagName {
                ns: None,
                local: name.to_vec(),
                local_lower: name.to_ascii_lowercase(),
            },
        ))
    }

    /// `getElementsByTagNameNS($namespace, $localName)`.
    pub fn get_elements_by_tag_name_ns(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
    ) -> DomResult<NodeList> {
        self.node()?;
        let class = if self.family_is_modern() {
            "Dom\\HTMLCollection"
        } else {
            "DOMNodeList"
        };
        Ok(new_list(
            class,
            Some(self.clone()),
            MapKind::ByTagName {
                ns: Some(uri.unwrap_or_default().to_vec()),
                local: name.to_vec(),
                local_lower: name.to_ascii_lowercase(),
            },
        ))
    }

    /// `getElementsByClassName($classNames)`.
    pub fn get_elements_by_class_name(&self, class_names: &[u8]) -> DomResult<NodeList> {
        self.node()?;
        let quirks = self
            .doc_ref()
            .is_some_and(|d| d.quirks_mode.get() == crate::object::QuirksMode::Quirks);
        let set = crate::tokenlist::ordered_set_parse(class_names, quirks);
        let kind = if set.is_empty() {
            MapKind::Noop
        } else {
            MapKind::ByClassName { classes: set }
        };
        Ok(new_list("Dom\\HTMLCollection", Some(self.clone()), kind))
    }

    fn get_attribute_ns_raw(&self, elemp: N, uri: Option<&[u8]>, name: &[u8]) -> Option<Vec<u8>> {
        let follow = self.is_modern();
        let uri = if follow && uri.is_some_and(<[u8]>::is_empty) {
            None
        } else {
            uri
        };
        let n = cs(name);
        let u = uri.map(cs);
        // SAFETY: live element.
        let strattr = take(unsafe {
            ffi::xmlGetNsProp(
                elemp.p(),
                n.as_ptr().cast(),
                u.as_ref().map_or(ptr::null(), |u| u.as_ptr().cast()),
            )
        });
        if strattr.is_some() {
            return strattr;
        }
        if !follow && uri == Some(ns::XMLNS_NS) {
            return tree::get_nsdecl(elemp, Some(name)).map(|ns| ns.href().to_vec());
        }
        None
    }

    /// `getAttributeNS($namespace, $localName)`.
    pub fn get_attribute_ns(&self, uri: Option<&[u8]>, name: &[u8]) -> DomResult<Option<Vec<u8>>> {
        let elemp = self.node()?;
        let r = self.get_attribute_ns_raw(elemp, uri, name);
        if r.is_none() && !self.is_modern() {
            return Ok(Some(Vec::new()));
        }
        Ok(r)
    }

    /// `setAttributeNS($namespace, $qualifiedName, $value)`.
    pub fn set_attribute_ns(&self, uri: Option<&[u8]>, name: &[u8], value: &[u8]) -> DomResult<()> {
        let elemp = self.node()?;
        if self.is_modern() {
            let (local, prefix) = tree::validate_and_extract(uri, name).map_err(DomError::dom)?;
            let pd = self
                .doc_ref()
                .and_then(|d| d.private_data())
                .ok_or_else(|| DomError::dom(C::InvalidState))?;
            let ns = pd.mapper.borrow_mut().get_ns(prefix.as_deref(), uri);
            match tree::set_attribute_value(elemp, ns, &local, value) {
                None => Err(DomError::dom(C::InvalidState)),
                Some(attr) => {
                    check_register_attribute_id(attr, self.doc_ref().as_ref());
                    Ok(())
                }
            }
        } else {
            self.set_attribute_ns_legacy(elemp, uri.unwrap_or_default(), name, value)
        }
    }

    #[allow(clippy::too_many_lines)]
    fn set_attribute_ns_legacy(
        &self,
        elemp: N,
        uri: &[u8],
        name: &[u8],
        value: &[u8],
    ) -> DomResult<()> {
        if name.is_empty() {
            return Err(DomError::arg_value(2, "must not be empty"));
        }
        let mut stricterror = self.doc_ref().is_none_or(|d| d.strict());
        let mut errorcode: Option<C> = None;
        match tree::check_qname(name, uri.len()) {
            Err(e) => errorcode = Some(e),
            Ok((local, prefix)) => {
                if let Some(d) = self.doc_ref() {
                    d.mark_modified_since_parsing();
                }
                if uri.is_empty() {
                    if tree::valid_name(&local) {
                        if let Some(a) = has_prop(elemp, &local) {
                            if a.t() != ffi::XML_ATTRIBUTE_DECL {
                                node_list_unlink(a.children());
                            }
                        }
                        let l = cs(&local);
                        let v = cs(value);
                        // SAFETY: live element.
                        unsafe { ffi::xmlSetProp(elemp.p(), l.as_ptr().cast(), v.as_ptr().cast()) };
                    } else {
                        errorcode = Some(C::InvalidCharacter);
                        stricterror = true;
                    }
                } else {
                    if let Some(a) = has_ns_prop(elemp, &local, Some(uri)) {
                        if a.t() != ffi::XML_ATTRIBUTE_DECL {
                            node_list_unlink(a.children());
                        }
                    }
                    let is_xmlns = (prefix.as_deref() == Some(b"xmlns")
                        || (prefix.is_none() && local == b"xmlns"))
                        && uri == ns::XMLNS_NS;
                    let mut nsptr: *mut ffi::xmlNs;
                    if is_xmlns {
                        nsptr = if prefix.is_none() {
                            tree::get_nsdecl(elemp, None)
                        } else {
                            tree::get_nsdecl(elemp, Some(&local))
                        }
                        .map_or(ptr::null_mut(), Ns::p);
                    } else {
                        let u = cs(uri);
                        // SAFETY: live element.
                        nsptr = unsafe {
                            ffi::xmlSearchNsByHref(elemp.doc_ptr(), elemp.p(), u.as_ptr().cast())
                        };
                        if let Some(found) = Ns::new(nsptr) {
                            if found.prefix_ptr().is_null() {
                                let mut tmp = found.next();
                                let mut replaced = false;
                                while let Some(t) = tmp {
                                    if !t.prefix_ptr().is_null()
                                        && !t.href_ptr().is_null()
                                        && t.href() == uri
                                    {
                                        nsptr = t.p();
                                        replaced = true;
                                        break;
                                    }
                                    tmp = t.next();
                                }
                                if !replaced {
                                    nsptr =
                                        tree::get_ns_resolve_prefix_conflict(elemp, found.href());
                                }
                            }
                        }
                    }
                    if nsptr.is_null() {
                        if is_xmlns {
                            let v = cs(value);
                            let p = if prefix.is_none() {
                                None
                            } else {
                                Some(cs(&local))
                            };
                            // SAFETY: live element.
                            unsafe {
                                ffi::xmlNewNs(
                                    elemp.p(),
                                    v.as_ptr().cast(),
                                    p.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
                                );
                            }
                        } else {
                            match tree::get_ns(elemp, uri, prefix.as_deref()) {
                                Ok(p) => nsptr = p,
                                Err(()) => errorcode = Some(C::Namespace),
                            }
                        }
                        // SAFETY: live element.
                        unsafe { ffi::xmlReconciliateNs(elemp.doc_ptr(), elemp.p()) };
                    } else if is_xmlns {
                        // SAFETY: nsptr is a live declaration owned by the tree.
                        unsafe {
                            if !(*nsptr).href.is_null() {
                                crate::raw::xml_free((*nsptr).href.cast_mut().cast());
                            }
                            (*nsptr).href = crate::raw::xml_strdup(value);
                        }
                    }
                    if errorcode.is_none() && !is_xmlns {
                        let l = cs(&local);
                        let v = cs(value);
                        // SAFETY: live element.
                        unsafe {
                            ffi::xmlSetNsProp(
                                elemp.p(),
                                nsptr,
                                l.as_ptr().cast(),
                                v.as_ptr().cast(),
                            )
                        };
                    }
                }
            }
        }
        match errorcode {
            Some(e) => Err(throw(e, stricterror)),
            None => Ok(()),
        }
    }

    /// `removeAttributeNS($namespace, $localName)`.
    pub fn remove_attribute_ns(&self, uri: Option<&[u8]>, name: &[u8]) -> DomResult<()> {
        let nodep = self.node()?;
        let follow = self.is_modern();
        let uri = if follow && uri.is_some_and(<[u8]>::is_empty) {
            None
        } else {
            uri
        };
        let attrp = has_ns_prop(nodep, name, uri);
        if !follow {
            if let Some(nsptr) = tree::get_nsdecl(nodep, Some(name)) {
                if str_eq(uri, Some(nsptr.href())) {
                    eliminate_ns(nodep, nsptr);
                    self.invalidate_doc();
                } else {
                    return Ok(());
                }
            }
        }
        if let Some(a) = attrp.filter(|a| a.t() != ffi::XML_ATTRIBUTE_DECL) {
            if Node::existing(a).is_none() {
                node_list_unlink(a.children());
                tree::unlink(a);
                // SAFETY: handle-less unlinked attribute.
                unsafe { ffi::xmlFreeProp(a.attr()) };
            } else {
                tree::unlink(a);
            }
            self.invalidate_doc();
        }
        Ok(())
    }

    /// `getAttributeNodeNS($namespace, $localName)`.
    pub fn get_attribute_node_ns(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
    ) -> DomResult<Option<Node>> {
        let elemp = self.node()?;
        let follow = self.is_modern();
        let uri = if follow && uri.is_some_and(<[u8]>::is_empty) {
            None
        } else {
            uri
        };
        match has_ns_prop(elemp, name, uri) {
            None => {
                if !follow && uri == Some(ns::XMLNS_NS) {
                    if let Some(nsptr) = tree::get_nsdecl(elemp, Some(name)) {
                        return Ok(Some(crate::xpath::create_fake_namespace_decl(
                            elemp, nsptr, self,
                        )));
                    }
                }
                Ok(None)
            }
            Some(a) => Ok(Some(Node::wrap(a))),
        }
    }

    /// `hasAttribute($name)`.
    pub fn has_attribute(&self, name: &[u8]) -> DomResult<bool> {
        let n = self.node()?;
        Ok(self.get_attribute_or_nsdecl(n, name).is_some())
    }

    /// `hasAttributeNS($namespace, $localName)`.
    pub fn has_attribute_ns(&self, uri: Option<&[u8]>, name: &[u8]) -> DomResult<bool> {
        let n = self.node()?;
        Ok(self.get_attribute_ns_raw(n, uri, name).is_some())
    }

    /// `setIdAttribute($name, $isId)`.
    pub fn set_id_attribute(&self, name: &[u8], is_id: bool) -> DomResult<()> {
        let n = self.node()?;
        match has_ns_prop(n, name, None).filter(|a| a.t() != ffi::XML_ATTRIBUTE_DECL) {
            None => Err(throw(
                C::NotFound,
                self.doc_ref().is_none_or(|d| d.strict()),
            )),
            Some(a) => {
                set_attribute_id(a, is_id, self.doc_ref().as_ref());
                Ok(())
            }
        }
    }

    /// `setIdAttributeNS($namespace, $qualifiedName, $isId)`.
    pub fn set_id_attribute_ns(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
        is_id: bool,
    ) -> DomResult<()> {
        let n = self.node()?;
        match has_ns_prop(n, name, Some(uri.unwrap_or_default()))
            .filter(|a| a.t() != ffi::XML_ATTRIBUTE_DECL)
        {
            None => Err(throw(
                C::NotFound,
                self.doc_ref().is_none_or(|d| d.strict()),
            )),
            Some(a) => {
                set_attribute_id(a, is_id, self.doc_ref().as_ref());
                Ok(())
            }
        }
    }

    /// `setIdAttributeNode($attr, $isId)`.
    pub fn set_id_attribute_node(&self, attr: &Node, is_id: bool) -> DomResult<()> {
        let n = self.node()?;
        let a = attr.node()?;
        if a.parent() != Some(n) {
            return Err(throw(
                C::NotFound,
                self.doc_ref().is_none_or(|d| d.strict()),
            ));
        }
        set_attribute_id(a, is_id, self.doc_ref().as_ref());
        Ok(())
    }

    /// `remove()`.
    pub fn remove(&self) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::child_node_remove(self))
    }

    /// `after(...$nodes)`.
    pub fn after(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::child_node_after(self, nodes))
    }

    /// `before(...$nodes)`.
    pub fn before(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::child_node_before(self, nodes))
    }

    /// `append(...$nodes)`.
    pub fn append(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::parent_node_append(self, nodes))
    }

    /// `prepend(...$nodes)`.
    pub fn prepend(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::parent_node_prepend(self, nodes))
    }

    /// `replaceWith(...$nodes)`.
    pub fn replace_with(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::child_replace_with(self, nodes))
    }

    /// `replaceChildren(...$nodes)`.
    pub fn replace_children(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.node()?;
        silent_ok(tree::parent_node_replace_children(self, nodes))
    }

    fn insert_adjacent(
        &self,
        where_: AdjacentPosition,
        thisp: N,
        otherp: N,
    ) -> Result<Option<N>, DomError> {
        let doc = self.doc_ref();
        let ctx = tree::Ctx { doc: doc.as_ref() };
        let (parent, before) = match where_ {
            AdjacentPosition::BeforeBegin => {
                let Some(p) = thisp.parent() else {
                    return Ok(None);
                };
                (p, Some(thisp))
            }
            AdjacentPosition::AfterBegin => (thisp, thisp.children()),
            AdjacentPosition::BeforeEnd => (thisp, None),
            AdjacentPosition::AfterEnd => {
                let Some(p) = thisp.parent() else {
                    return Ok(None);
                };
                (p, thisp.next())
            }
        };
        if !adopt_node(otherp, self, thisp.doc_ptr()) {
            return Err(DomError::Silent);
        }
        tree::pre_insert(ctx, otherp, parent, before)?;
        Ok(Some(otherp))
    }

    /// `insertAdjacentElement($where, $element)` (legacy string positions
    /// are parsed by [`AdjacentPosition::parse`]; an unknown string is a
    /// `SyntaxError`).
    pub fn insert_adjacent_element(
        &self,
        where_: Result<AdjacentPosition, ()>,
        element: &Node,
    ) -> DomResult<Option<Node>> {
        let thisp = self.node()?;
        let Ok(where_) = where_ else {
            return Err(throw(C::Syntax, self.doc_ref().is_none_or(|d| d.strict())));
        };
        let otherp = element.node()?;
        match self.insert_adjacent(where_, thisp, otherp)? {
            None => Ok(None),
            Some(o) => Ok(Some(Node::wrap(o))),
        }
    }

    /// `insertAdjacentText($where, $data)`.
    pub fn insert_adjacent_text(
        &self,
        where_: Result<AdjacentPosition, ()>,
        data: &[u8],
    ) -> DomResult<()> {
        let thisp = self.node()?;
        let Ok(where_) = where_ else {
            return Err(throw(C::Syntax, self.doc_ref().is_none_or(|d| d.strict())));
        };
        if data.len() > i32::MAX as usize {
            return Err(DomError::arg_value(2, "is too long"));
        }
        // SAFETY: valid string/length.
        let otherp = N::new(unsafe {
            ffi::xmlNewDocTextLen(thisp.doc_ptr(), data.as_ptr(), c_len(data.len()))
        })
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
        match self.insert_adjacent(where_, thisp, otherp) {
            Ok(None) | Err(DomError::Silent) => {
                tree::free_node(otherp);
                Ok(())
            }
            Ok(Some(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `insertAdjacentHTML($where, $string)` (`Dom\Element`).
    pub fn insert_adjacent_html(&self, where_: AdjacentPosition, string: &[u8]) -> DomResult<()> {
        let thisp = self.node()?;
        let mut context = match where_ {
            AdjacentPosition::BeforeBegin | AdjacentPosition::AfterEnd => {
                let c = thisp.parent();
                match c {
                    Some(c) if !c.is_doc() => c,
                    _ => return Err(DomError::dom(C::NoModificationAllowed)),
                }
            }
            _ => thisp,
        };
        let mut created = None;
        if !context.is_element()
            || (ns::is_html_and_document_is_html(context) && context.name() == b"html")
        {
            let pd = self
                .doc_ref()
                .and_then(|d| d.private_data())
                .ok_or_else(|| DomError::dom(C::InvalidState))?;
            let html_ns = pd.mapper.borrow_mut().ensure_html_ns();
            // SAFETY: live document.
            let c = N::new(unsafe {
                ffi::xmlNewDocNode(
                    thisp.doc_ptr(),
                    html_ns,
                    c"body".as_ptr().cast(),
                    ptr::null(),
                )
            })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
            created = Some(c);
            context = c;
        }
        let result = (|| {
            let fragment = crate::inner_outer::parse_fragment(self, context, string)?;
            self.invalidate_doc();
            let doc = self.doc_ref();
            let ctx = tree::Ctx { doc: doc.as_ref() };

            match where_ {
                AdjacentPosition::BeforeBegin => {
                    tree::pre_insert(ctx, fragment, thisp.parent().expect("checked"), Some(thisp))
                }
                AdjacentPosition::AfterEnd => tree::pre_insert(
                    ctx,
                    fragment,
                    thisp.parent().expect("checked"),
                    thisp.next(),
                ),
                AdjacentPosition::AfterBegin => {
                    tree::pre_insert(ctx, fragment, thisp, thisp.children())
                }
                AdjacentPosition::BeforeEnd => tree::node_append(ctx, fragment, thisp),
            }
        })();
        if let Some(c) = created {
            tree::free_node(c);
        }
        result
    }

    /// `toggleAttribute($qualifiedName, $force)`.
    pub fn toggle_attribute(&self, qname: &[u8], force: Option<bool>) -> DomResult<bool> {
        let thisp = self.node()?;
        if !tree::valid_name(qname) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        let follow = self.is_modern();
        let mut q = qname.to_vec();
        if thisp
            .doc()
            .is_some_and(|d| d.t() == ffi::XML_HTML_DOCUMENT_NODE)
            && ((!follow && thisp.ns().is_none())
                || thisp.ns().is_some_and(|ns| ns.href() == ns::XHTML_NS))
        {
            q.make_ascii_lowercase();
        }
        let attribute = self.get_attribute_or_nsdecl(thisp, &q);
        let Some(attribute) = attribute else {
            if force.is_none_or(|f| f) {
                if follow {
                    let c = cs(&q);
                    // SAFETY: live element.
                    unsafe {
                        ffi::xmlSetNsProp(
                            thisp.p(),
                            ptr::null_mut(),
                            c.as_ptr().cast(),
                            ptr::null(),
                        )
                    };
                } else {
                    let c = cs(&q);
                    let mut len: i32 = 0;
                    // SAFETY: valid C string.
                    let split = unsafe { ffi::xmlSplitQName3(c.as_ptr().cast(), &raw mut len) };
                    let len = usize::try_from(len).unwrap_or(0);
                    if split.is_null() || !(len == 5 && q.starts_with(b"xmlns:")) {
                        create_attribute(thisp, &q, b"");
                    } else {
                        let p = cs(&q[len + 1..]);
                        // SAFETY: live element.
                        unsafe { ffi::xmlNewNs(thisp.p(), c"".as_ptr().cast(), p.as_ptr().cast()) };
                    }
                }
                return Ok(true);
            }
            return Ok(false);
        };
        if force.is_none_or(|f| !f) {
            return Ok(!remove_attribute_raw(
                thisp,
                attribute,
                self.doc_ref().as_ref(),
            ));
        }
        Ok(true)
    }

    /// `$substitutedNodeValue`.
    pub fn substituted_node_value(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        tree::node_get_content(n).ok_or_else(|| DomError::dom(C::InvalidState))
    }

    /// `$substitutedNodeValue = $value`.
    pub fn set_substituted_node_value(&self, value: &[u8]) -> DomResult<()> {
        let n = self.prop_node()?;
        self.invalidate_doc();
        remove_all_children(n);
        // SAFETY: live node.
        unsafe { ffi::xmlNodeSetContentLen(n.p(), value.as_ptr(), c_len(value.len())) };
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn in_scope_namespace_info(
        &self,
        mapper: &mut ns::NsMapper,
        result: &mut Vec<NamespaceInfo>,
        nodep: N,
    ) {
        let mut table: Vec<(Vec<u8>, *mut ffi::xmlNs)> = Vec::new();
        let mut cur = Some(nodep);
        while let Some(c) = cur {
            if c.is_element() {
                let attrs: Vec<N> = c.attr_iter().collect();
                for attr in attrs.iter().rev() {
                    let Some(ans) = attr.ns() else { continue };
                    if !ns_is_fast_ex(ans, &XMLNS_TOKEN) {
                        continue;
                    }
                    let Some(child) = attr.children() else {
                        continue;
                    };
                    if child.content_ptr().is_null() {
                        continue;
                    }
                    let prefix = if ans.prefix_ptr().is_null() {
                        None
                    } else {
                        Some(attr.name())
                    };
                    let key = prefix.unwrap_or_default().to_vec();
                    let ns = mapper.get_ns_nullsafe(prefix, Some(child.content()));
                    if !table.iter().any(|(k, _)| *k == key) {
                        table.push((key, ns));
                    }
                }
            }
            cur = c.parent();
        }
        for (prefix, ns) in table.into_iter().rev() {
            let href = Ns::new(ns).map(Ns::href).filter(|h| !h.is_empty());
            if prefix.is_empty() && href.is_none() {
                continue;
            }
            result.push(NamespaceInfo {
                prefix: if prefix.is_empty() {
                    None
                } else {
                    Some(prefix)
                },
                namespace_uri: href.map(<[u8]>::to_vec),
                element: Node::wrap(nodep),
            });
        }
    }

    /// `getInScopeNamespaces()`.
    pub fn get_in_scope_namespaces(&self) -> DomResult<Vec<NamespaceInfo>> {
        let n = self.node()?;
        let pd = self
            .doc_ref()
            .and_then(|d| d.private_data())
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        let mut out = Vec::new();
        self.in_scope_namespace_info(&mut pd.mapper.borrow_mut(), &mut out, n);
        Ok(out)
    }

    /// `getDescendantNamespaces()`.
    pub fn get_descendant_namespaces(&self) -> DomResult<Vec<NamespaceInfo>> {
        let n = self.node()?;
        let pd = self
            .doc_ref()
            .and_then(|d| d.private_data())
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        let mut out = Vec::new();
        self.in_scope_namespace_info(&mut pd.mapper.borrow_mut(), &mut out, n);
        let mut cur = n.children();
        while let Some(c) = cur {
            if c.is_element() {
                self.in_scope_namespace_info(&mut pd.mapper.borrow_mut(), &mut out, c);
            }
            cur = tree::next_in_tree_order(c, Some(n));
        }
        Ok(out)
    }

    /// `rename($namespaceURI, $qualifiedName)` (`Dom\Element` and `Dom\Attr`).
    pub fn rename(&self, namespace_uri: Option<&[u8]>, qname: &[u8]) -> DomResult<()> {
        let nodep = self.node()?;
        let (local, prefix) =
            tree::validate_and_extract(namespace_uri, qname).map_err(DomError::dom)?;
        if nodep.t() == ffi::XML_ATTRIBUTE_NODE {
            let uri = namespace_uri.filter(|u| !u.is_empty());
            if let Some(parent) = nodep.parent() {
                if let Some(existing) = has_ns_prop(parent, &local, uri) {
                    if existing != nodep {
                        return Err(DomError::dom_msg(
                            C::InvalidModification,
                            "An attribute with the given name in the given namespace already exists",
                        ));
                    }
                }
            }
        } else {
            let is_html = ns_is_fast(nodep, &HTML_TOKEN);
            let will_be_html = namespace_uri == Some(ns::XHTML_NS);
            if is_html != will_be_html {
                return Err(DomError::dom_msg(
                    C::InvalidModification,
                    if is_html {
                        "It is not possible to move an element out of the HTML namespace because the HTML namespace is tied to the HTMLElement class"
                    } else {
                        "It is not possible to move an element into the HTML namespace because the HTML namespace is tied to the HTMLElement class"
                    },
                ));
            }
            if is_html && nodep.name() == b"template" && local != b"template" {
                return Err(DomError::dom_msg(
                    C::InvalidModification,
                    "It is not possible to rename the template element because it hosts a document fragment",
                ));
            }
        }
        self.invalidate_doc();
        let pd = self
            .doc_ref()
            .and_then(|d| d.private_data())
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        let ns = pd
            .mapper
            .borrow_mut()
            .get_ns(prefix.as_deref(), namespace_uri);
        nodep.set_ns(ns);
        // SAFETY: the old name is freed unless the dictionary owns it; the
        // new one is interned in the document dictionary when possible.
        unsafe {
            let dict = nodep.doc().map_or(ptr::null_mut(), |d| d.d().dict);
            let old = nodep.name_ptr();
            if dict.is_null() || ffi::xmlDictOwns(dict, old) != 1 {
                crate::raw::xml_free(old.cast_mut().cast());
            }
            let l = cs(&local);
            let copy = if dict.is_null() {
                ptr::null()
            } else {
                ffi::xmlDictLookup(dict, l.as_ptr().cast(), -1)
            };
            (*nodep.p()).name = if copy.is_null() {
                crate::raw::xml_strdup(&local)
            } else {
                copy
            };
        }
        Ok(())
    }
}

/// Maps `Err(Silent)` (warning already emitted) to `Ok(())` for void methods.
pub(crate) fn silent_ok(r: DomResult<()>) -> DomResult<()> {
    match r {
        Err(DomError::Silent) => Ok(()),
        other => other,
    }
}
