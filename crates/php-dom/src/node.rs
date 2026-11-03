//! `DOMNode` / `Dom\Node` (`node.c`).

use std::ptr;

use crate::error::{throw, throw_msg, DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::lists::{new_list, new_map, MapKind, NamedNodeMap, NodeList};
use crate::ns::{self, ns_is_fast_ex, XMLNS_TOKEN};
use crate::object::{free_resource, remove_all_children, DocRef, Node};
use crate::raw::{cs, str_eq, take, Ns, N};
use crate::tree::{self, children_valid, is_read_only, Ctx};

/// `DOCUMENT_POSITION_*` constants.
pub mod position {
    /// `DOCUMENT_POSITION_DISCONNECTED`.
    pub const DISCONNECTED: i64 = 0x01;
    /// `DOCUMENT_POSITION_PRECEDING`.
    pub const PRECEDING: i64 = 0x02;
    /// `DOCUMENT_POSITION_FOLLOWING`.
    pub const FOLLOWING: i64 = 0x04;
    /// `DOCUMENT_POSITION_CONTAINS`.
    pub const CONTAINS: i64 = 0x08;
    /// `DOCUMENT_POSITION_CONTAINED_BY`.
    pub const CONTAINED_BY: i64 = 0x10;
    /// `DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC`.
    pub const IMPLEMENTATION_SPECIFIC: i64 = 0x20;
}

/// `dom_entity_reference_fetch_and_sync_declaration`.
pub(crate) fn entity_reference_sync(reference: N) -> Option<N> {
    // SAFETY: live entity reference node.
    let entity = unsafe { ffi::xmlGetDocEntity(reference.doc_ptr(), reference.name_ptr()) };
    let e = N::new(entity.cast());
    reference.set_children(e);
    reference.set_last(e);
    // SAFETY: live node; content points into the entity (not owned).
    unsafe {
        (*reference.p()).content = if entity.is_null() {
            ptr::null_mut()
        } else {
            (*entity).content
        };
    }
    e
}

/// `dom_set_document_ref_obj_single`: false when the object already had a
/// different document reference.
fn set_doc_ref_single(n: N, doc: &DocRef) -> bool {
    let Some(obj) = Node::existing(n) else {
        return true;
    };
    if obj.doc_ref().is_none() {
        obj.set_doc_ref(Some(doc.clone()));
        return true;
    }
    false
}

/// `dom_set_document_ref_pointers_attr`.
pub(crate) fn set_document_ref_pointers_attr(attr: N, doc: &DocRef) {
    set_doc_ref_single(attr, doc);
    for c in attr.child_iter() {
        set_doc_ref_single(c, doc);
    }
}

fn set_document_ref_pointers_node(n: N, doc: &DocRef) -> bool {
    if !set_doc_ref_single(n, doc) {
        return false;
    }
    if n.is_element() {
        for a in n.attr_iter() {
            set_document_ref_pointers_attr(a, doc);
        }
    }
    true
}

/// `dom_set_document_ref_pointers`.
pub(crate) fn set_document_ref_pointers(n: N, doc: Option<&DocRef>) {
    let Some(doc) = doc else { return };
    if !set_document_ref_pointers_node(n, doc) {
        return;
    }
    let base = n;
    let mut cur = n.children();
    while let Some(c) = cur {
        if !set_document_ref_pointers_node(c, doc) {
            break;
        }
        cur = tree::next_in_tree_order(c, Some(base));
    }
}

/// `xmlSetTreeDoc`.
pub(crate) fn set_tree_doc(n: N, doc: *mut ffi::xmlDoc) {
    // SAFETY: live subtree.
    unsafe { ffi::xmlSetTreeDoc(n.p(), doc) };
}

/// `dom_insert_fragment`.
fn insert_fragment(parent: N, prevsib: Option<N>, nextsib: Option<N>, fragment: N) -> Option<N> {
    let newchild = fragment.children();
    if let (Some(first), Some(flast)) = (newchild, fragment.last()) {
        match prevsib {
            None => parent.set_children(Some(first)),
            Some(p) => p.set_next(Some(first)),
        }
        first.set_prev(prevsib);
        match nextsib {
            None => parent.set_last(Some(flast)),
            Some(n) => {
                flast.set_next(Some(n));
                n.set_prev(Some(flast));
            }
        }
        let mut cur = Some(first);
        while let Some(c) = cur {
            c.set_parent(Some(parent));
            if c == flast {
                break;
            }
            cur = c.next();
        }
        fragment.set_children(None);
        fragment.set_last(None);
    }
    newchild
}

/// `dom_node_check_legacy_insertion_validity`.
fn check_legacy_insertion_validity(
    parent: N,
    child: N,
    strict: bool,
    warn_empty_fragment: bool,
) -> DomResult<()> {
    if is_read_only(parent) || child.parent().is_some_and(is_read_only) {
        return Err(throw(C::NoModificationAllowed, strict));
    }
    if !tree::hierarchy_ok(parent, child) {
        return Err(throw(C::HierarchyRequest, strict));
    }
    if child.doc_ptr() != parent.doc_ptr() && !child.doc_ptr().is_null() {
        return Err(throw(C::WrongDocument, strict));
    }
    if warn_empty_fragment && child.t() == ffi::XML_DOCUMENT_FRAG_NODE && child.children().is_none()
    {
        crate::error::warning("Document Fragment is empty");
        return Err(DomError::Silent);
    }
    if parent.t() == ffi::XML_ATTRIBUTE_NODE
        && child.t() != ffi::XML_TEXT_NODE
        && child.t() != ffi::XML_ENTITY_REF_NODE
    {
        return Err(throw(C::HierarchyRequest, strict));
    }
    if child.t() == ffi::XML_ATTRIBUTE_NODE && !parent.is_element() {
        return Err(throw(C::HierarchyRequest, strict));
    }
    if child.is_doc() {
        return Err(throw(C::HierarchyRequest, strict));
    }
    Ok(())
}

fn has_ns_prop_for(parent: N, child: N) -> Option<N> {
    let href = child.ns().map(Ns::href);
    crate::element::has_ns_prop(parent, child.name(), href)
}

/// `xmlAddChild` / `xmlAddPrevSibling` wrappers.
fn add_child(parent: N, child: N) -> Option<N> {
    // SAFETY: live nodes; callers avoid the text-merging cases.
    N::new(unsafe { ffi::xmlAddChild(parent.p(), child.p()) })
}
fn add_prev_sibling(cur: N, elem: N) -> Option<N> {
    // SAFETY: live nodes; callers avoid the text-merging cases.
    N::new(unsafe { ffi::xmlAddPrevSibling(cur.p(), elem.p()) })
}

impl Node {
    fn ctx_doc(&self) -> Option<DocRef> {
        self.doc_ref()
    }

    fn strict(&self) -> bool {
        self.doc_ref().is_none_or(|d| d.strict())
    }

    fn invalidate(&self) {
        if let Some(d) = self.doc_ref() {
            d.invalidate();
        }
    }

    // -----------------------------------------------------------------
    // Properties
    // -----------------------------------------------------------------

    /// `$nodeName`.
    pub fn node_name(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(match n.t() {
            ffi::XML_ELEMENT_NODE => {
                let upper = self.is_modern() && ns::is_html_and_document_is_html(n);
                tree::qualified_name(n, upper)
            }
            ffi::XML_ATTRIBUTE_NODE => tree::qualified_name(n, false),
            ffi::XML_NAMESPACE_DECL => match n.ns().and_then(Ns::prefix) {
                Some(p) => {
                    let mut v = b"xmlns:".to_vec();
                    v.extend_from_slice(p);
                    v
                }
                None => n.name().to_vec(),
            },
            ffi::XML_DOCUMENT_TYPE_NODE
            | ffi::XML_DTD_NODE
            | ffi::XML_PI_NODE
            | ffi::XML_ENTITY_DECL
            | ffi::XML_ENTITY_REF_NODE
            | ffi::XML_NOTATION_NODE => n.name().to_vec(),
            ffi::XML_CDATA_SECTION_NODE => b"#cdata-section".to_vec(),
            ffi::XML_COMMENT_NODE => b"#comment".to_vec(),
            ffi::XML_HTML_DOCUMENT_NODE | ffi::XML_DOCUMENT_NODE => b"#document".to_vec(),
            ffi::XML_DOCUMENT_FRAG_NODE => b"#document-fragment".to_vec(),
            ffi::XML_TEXT_NODE => b"#text".to_vec(),
            _ => Vec::new(),
        })
    }

    /// `$nodeValue`.
    pub fn node_value(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        Ok(match n.t() {
            ffi::XML_ELEMENT_NODE if self.is_modern() => None,
            ffi::XML_ELEMENT_NODE
            | ffi::XML_ATTRIBUTE_NODE
            | ffi::XML_TEXT_NODE
            | ffi::XML_COMMENT_NODE
            | ffi::XML_CDATA_SECTION_NODE
            | ffi::XML_PI_NODE => tree::get_content(n),
            ffi::XML_NAMESPACE_DECL => n.children().and_then(tree::node_get_content),
            _ => None,
        })
    }

    /// `$nodeValue = $value`.
    pub fn set_node_value(&self, value: Option<&[u8]>) -> DomResult<()> {
        let n = self.prop_node()?;
        let s = value.unwrap_or_default();
        let t = n.t();
        let mut fallthrough_element = t == ffi::XML_ELEMENT_NODE;
        let mut set_content = matches!(
            t,
            ffi::XML_ELEMENT_NODE
                | ffi::XML_TEXT_NODE
                | ffi::XML_COMMENT_NODE
                | ffi::XML_CDATA_SECTION_NODE
                | ffi::XML_PI_NODE
        );
        if t == ffi::XML_ATTRIBUTE_NODE {
            crate::attr::value_will_change(self, n);
            if self.is_modern() {
                remove_all_children(n);
                // SAFETY: s is valid for its length.
                let text = unsafe { ffi::xmlNewTextLen(s.as_ptr(), crate::raw::c_len(s.len())) };
                if let Some(text) = N::new(text) {
                    add_child(n, text);
                }
                set_content = false;
            } else {
                fallthrough_element = true;
                set_content = true;
            }
        }
        if fallthrough_element {
            remove_all_children(n);
        }
        if set_content {
            // SAFETY: live node, s valid for its length.
            unsafe { ffi::xmlNodeSetContentLen(n.p(), s.as_ptr(), crate::raw::c_len(s.len())) };
        }
        self.invalidate();
        Ok(())
    }

    /// `$nodeType`.
    pub fn node_type(&self) -> DomResult<i64> {
        let n = self.prop_node()?;
        Ok(if n.t() == ffi::XML_DTD_NODE {
            i64::from(ffi::XML_DOCUMENT_TYPE_NODE)
        } else {
            i64::from(n.t())
        })
    }

    fn parent_of(&self, n: N) -> Option<N> {
        if n.t() == ffi::XML_NAMESPACE_DECL {
            return self.0.ns_parent.borrow().as_ref().and_then(Node::n);
        }
        n.parent()
    }

    /// `$parentNode`.
    pub fn parent_node(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        Ok(self.parent_of(n).map(Node::wrap))
    }

    /// `$parentElement`.
    pub fn parent_element(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        Ok(self.parent_of(n).filter(|p| p.is_element()).map(Node::wrap))
    }

    /// `$childNodes`.
    pub fn child_nodes(&self) -> DomResult<NodeList> {
        let n = self.prop_node()?;
        if n.t() == ffi::XML_ENTITY_REF_NODE {
            entity_reference_sync(n);
        }
        let class = if self.is_modern() {
            "Dom\\NodeList"
        } else {
            "DOMNodeList"
        };
        Ok(new_list(class, Some(self.clone()), MapKind::ChildNodes))
    }

    /// `$firstChild`.
    pub fn first_child(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        if n.t() == ffi::XML_ENTITY_REF_NODE {
            return Ok(entity_reference_sync(n).map(Node::wrap));
        }
        Ok(if children_valid(n) {
            n.children().map(Node::wrap)
        } else {
            None
        })
    }

    /// `$lastChild`.
    pub fn last_child(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        if n.t() == ffi::XML_ENTITY_REF_NODE {
            return Ok(entity_reference_sync(n).map(Node::wrap));
        }
        Ok(if children_valid(n) {
            n.last().map(Node::wrap)
        } else {
            None
        })
    }

    /// `$previousSibling`.
    pub fn previous_sibling(&self) -> DomResult<Option<Node>> {
        Ok(self.prop_node()?.prev().map(Node::wrap))
    }

    /// `$nextSibling`.
    pub fn next_sibling(&self) -> DomResult<Option<Node>> {
        Ok(self.prop_node()?.next().map(Node::wrap))
    }

    /// `$previousElementSibling`.
    pub fn previous_element_sibling(&self) -> DomResult<Option<Node>> {
        let mut cur = self.prop_node()?.prev();
        while let Some(c) = cur {
            if c.is_element() {
                return Ok(Some(Node::wrap(c)));
            }
            cur = c.prev();
        }
        Ok(None)
    }

    /// `$nextElementSibling`.
    pub fn next_element_sibling(&self) -> DomResult<Option<Node>> {
        let mut cur = self.prop_node()?.next();
        while let Some(c) = cur {
            if c.is_element() {
                return Ok(Some(Node::wrap(c)));
            }
            cur = c.next();
        }
        Ok(None)
    }

    /// `$attributes`.
    pub fn attributes(&self) -> DomResult<Option<NamedNodeMap>> {
        let n = self.prop_node()?;
        if n.is_element() {
            let class = if self.is_modern() {
                "Dom\\NamedNodeMap"
            } else {
                "DOMNamedNodeMap"
            };
            Ok(Some(new_map(
                class,
                Some(self.clone()),
                MapKind::Attributes,
            )))
        } else {
            Ok(None)
        }
    }

    /// `$isConnected`.
    pub fn is_connected(&self) -> DomResult<bool> {
        let n = self.prop_node()?;
        if n.t() == ffi::XML_NAMESPACE_DECL {
            return Ok(self.parent_of(n).is_some_and(tree::is_connected));
        }
        Ok(tree::is_connected(n))
    }

    /// `$ownerDocument`. `Ok(None)` for documents; `Err(Silent)` when the
    /// node has no document (the property is then uninitialized).
    pub fn owner_document(&self) -> DomResult<Option<Node>> {
        let n = self.prop_node()?;
        if n.is_doc() {
            return Ok(None);
        }
        let doc = n.doc().ok_or(DomError::Silent)?;
        Ok(Some(Node::wrap(doc)))
    }

    /// `$namespaceURI`.
    pub fn namespace_uri(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        Ok(match n.t() {
            ffi::XML_ELEMENT_NODE | ffi::XML_ATTRIBUTE_NODE | ffi::XML_NAMESPACE_DECL => {
                n.ns().map(|ns| ns.href().to_vec())
            }
            _ => None,
        })
    }

    /// `$prefix` (legacy: `""` when none).
    pub fn prefix(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(match n.t() {
            ffi::XML_ELEMENT_NODE | ffi::XML_ATTRIBUTE_NODE | ffi::XML_NAMESPACE_DECL => {
                n.ns().and_then(Ns::prefix).unwrap_or_default().to_vec()
            }
            _ => Vec::new(),
        })
    }

    /// `$prefix` of `Dom\Element` / `Dom\Attr` (null when none).
    pub fn prefix_modern(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        Ok(n.ns().and_then(Ns::prefix).map(<[u8]>::to_vec))
    }

    /// `$prefix = $value` (legacy).
    pub fn set_prefix(&self, value: &[u8]) -> DomResult<()> {
        let n = self.prop_node()?;
        // The C code works on the NUL-terminated view of the string.
        let value = &value[..value.iter().position(|&b| b == 0).unwrap_or(value.len())];
        let t = n.t();
        if t != ffi::XML_ELEMENT_NODE && t != ffi::XML_ATTRIBUTE_NODE {
            return Ok(());
        }
        let nsnode = if t == ffi::XML_ELEMENT_NODE {
            Some(n)
        } else {
            // SAFETY: n.doc_ptr() may be NULL (returns NULL).
            n.parent()
                .or_else(|| N::new(unsafe { ffi::xmlDocGetRootElement(n.doc_ptr()) }))
        };
        let prefix: Option<&[u8]> = if value.is_empty() { None } else { Some(value) };
        let Some(nns) = n.ns() else { return Ok(()) };
        let Some(nsnode) = nsnode else { return Ok(()) };
        if str_eq(nns.prefix(), prefix) {
            return Ok(());
        }
        let uri = nns.href_ptr();
        let uri_b = nns.href();
        if uri.is_null()
            || (value == b"xml" && uri_b != ffi::XML_XML_NAMESPACE)
            || (t == ffi::XML_ATTRIBUTE_NODE && value == b"xmlns" && uri_b != ns::XMLNS_NS)
            || (t == ffi::XML_ATTRIBUTE_NODE && n.name() == b"xmlns")
        {
            return Err(throw(C::Namespace, self.strict()));
        }
        let mut found = None;
        let mut cur = nsnode.ns_def();
        while let Some(c) = cur {
            if str_eq(prefix, c.prefix()) && c.href() == uri_b {
                found = Some(c.p());
                break;
            }
            cur = c.next();
        }
        let ns = if let Some(f) = found {
            f
        } else {
            let p = prefix.map(cs);
            // SAFETY: live element; uri valid.
            let created = unsafe {
                ffi::xmlNewNs(
                    nsnode.p(),
                    uri,
                    p.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
                )
            };
            if created.is_null() {
                return Err(throw(C::Namespace, true));
            }
            created
        };
        // SAFETY: live node, ns live.
        unsafe { ffi::xmlSetNs(n.p(), ns) };
        Ok(())
    }

    /// `$localName`.
    pub fn local_name(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        Ok(match n.t() {
            ffi::XML_ELEMENT_NODE | ffi::XML_ATTRIBUTE_NODE | ffi::XML_NAMESPACE_DECL => {
                Some(n.name().to_vec())
            }
            _ => None,
        })
    }

    /// `$baseURI` (legacy, nullable).
    pub fn base_uri(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        // SAFETY: live node.
        Ok(take(unsafe { ffi::xmlNodeGetBase(n.doc_ptr(), n.p()) }))
    }

    /// `$baseURI` of `Dom\Node`.
    pub fn base_uri_modern(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        // SAFETY: live node.
        if let Some(b) = take(unsafe { ffi::xmlNodeGetBase(n.doc_ptr(), n.p()) }) {
            return Ok(b);
        }
        if let Some(doc) = n.doc() {
            let url = doc.d().URL;
            if !url.is_null() {
                return Ok(crate::raw::cstr(url).to_vec());
            }
        }
        Ok(b"about:blank".to_vec())
    }

    fn skip_text_content(&self, n: N) -> bool {
        self.is_modern()
            && !matches!(
                n.t(),
                ffi::XML_DOCUMENT_FRAG_NODE
                    | ffi::XML_ELEMENT_NODE
                    | ffi::XML_ATTRIBUTE_NODE
                    | ffi::XML_TEXT_NODE
                    | ffi::XML_CDATA_SECTION_NODE
                    | ffi::XML_COMMENT_NODE
                    | ffi::XML_PI_NODE
            )
    }

    /// `$textContent` (`None` = null, modern documents/doctypes).
    pub fn text_content(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        if n.t() == ffi::XML_ENTITY_REF_NODE {
            entity_reference_sync(n);
        }
        if self.skip_text_content(n) {
            return Ok(None);
        }
        Ok(Some(tree::get_content(n).unwrap_or_default()))
    }

    /// `$textContent = $value`.
    pub fn set_text_content(&self, value: Option<&[u8]>) -> DomResult<()> {
        let n = self.prop_node()?;
        self.invalidate();
        let s = value.unwrap_or_default();
        let t = n.t();
        if t == ffi::XML_DOCUMENT_FRAG_NODE
            || t == ffi::XML_ELEMENT_NODE
            || t == ffi::XML_ATTRIBUTE_NODE
        {
            remove_all_children(n);
            // SAFETY: valid string/length; doc may be NULL.
            let text = unsafe {
                ffi::xmlNewDocTextLen(n.doc_ptr(), s.as_ptr(), crate::raw::c_len(s.len()))
            };
            if let Some(text) = N::new(text) {
                add_child(n, text);
            }
        } else {
            let c = cs(s);
            // SAFETY: live node.
            unsafe { ffi::xmlNodeSetContent(n.p(), c.as_ptr().cast()) };
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Methods
    // -----------------------------------------------------------------

    /// `insertBefore($node, $child)`.
    pub fn insert_before(&self, node: &Node, child: Option<&Node>) -> DomResult<Node> {
        let parentp = self.node()?;
        let childp = node.node()?;
        if self.family_is_modern() {
            if tree::pre_insert_parent_invalid(parentp) {
                return Err(DomError::dom(C::HierarchyRequest));
            }
            let refp = match child {
                Some(r) => Some(r.node()?),
                None => None,
            };
            let doc = self.ctx_doc();
            let ctx = Ctx { doc: doc.as_ref() };
            ctx.invalidate();
            tree::pre_insert(ctx, childp, parentp, refp)?;
            return Ok(Node::wrap(childp));
        }
        self.insert_before_legacy(parentp, childp, child)
    }

    fn insert_before_legacy(
        &self,
        parentp: N,
        mut child: N,
        refnode: Option<&Node>,
    ) -> DomResult<Node> {
        if !children_valid(parentp) {
            return Err(DomError::Silent);
        }
        let strict = self.strict();
        check_legacy_insertion_validity(parentp, child, strict, true)?;
        let mut refp = None;
        if let Some(r) = refnode {
            let rp = r.node()?;
            if rp.parent() != Some(parentp) {
                return Err(throw(C::NotFound, strict));
            }
            refp = Some(if rp == child {
                match child.next() {
                    Some(n) => n,
                    None => {
                        // refp becomes NULL: append path.
                        return self.insert_before_append(parentp, child, strict);
                    }
                }
            } else {
                rp
            });
        }
        if child.doc_ptr().is_null() && !parentp.doc_ptr().is_null() {
            set_tree_doc(child, parentp.doc_ptr());
            set_document_ref_pointers(child, self.doc_ref().as_ref());
        }
        self.invalidate();
        let Some(refp) = refp else {
            return self.insert_before_append_linked(parentp, child, strict);
        };
        if child.parent().is_some() {
            tree::unlink(child);
        }
        let new_child;
        if child.t() == ffi::XML_TEXT_NODE
            && (refp.t() == ffi::XML_TEXT_NODE
                || refp.prev().is_some_and(|p| p.t() == ffi::XML_TEXT_NODE))
        {
            new_child = child;
            new_child.set_parent(refp.parent());
            new_child.set_next(Some(refp));
            new_child.set_prev(refp.prev());
            refp.set_prev(Some(new_child));
            if let Some(p) = new_child.prev() {
                p.set_next(Some(new_child));
            }
            if let Some(par) = new_child.parent() {
                if par.children() == Some(refp) {
                    par.set_children(Some(new_child));
                }
            }
        } else if child.t() == ffi::XML_ATTRIBUTE_NODE {
            if let Some(lastattr) = refp.parent().and_then(|p| has_ns_prop_for(p, child)) {
                if lastattr.t() != ffi::XML_ATTRIBUTE_DECL {
                    if lastattr == child {
                        return Ok(Node::wrap(child));
                    }
                    tree::unlink(lastattr);
                    free_resource(lastattr);
                }
            }
            new_child = add_prev_sibling(refp, child).ok_or_else(|| {
                DomError::Error("Cannot add newnode as the previous sibling of refnode".into())
            })?;
        } else if child.t() == ffi::XML_DOCUMENT_FRAG_NODE {
            let last = child.last();
            let first = insert_fragment(parentp, refp.prev(), Some(refp), child);
            if let (Some(f), Some(l)) = (first, last) {
                tree::reconcile_ns_list(parentp.doc_ptr(), f, l);
            }
            child = first.unwrap_or(child);
            return Ok(Node::wrap(child));
        } else {
            new_child = add_prev_sibling(refp, child).ok_or_else(|| {
                DomError::Error("Cannot add newnode as the previous sibling of refnode".into())
            })?;
            tree::reconcile_ns(parentp.doc_ptr(), new_child);
        }
        Ok(Node::wrap(new_child))
    }

    fn insert_before_append(&self, parentp: N, child: N, strict: bool) -> DomResult<Node> {
        if child.doc_ptr().is_null() && !parentp.doc_ptr().is_null() {
            set_tree_doc(child, parentp.doc_ptr());
            set_document_ref_pointers(child, self.doc_ref().as_ref());
        }
        self.invalidate();
        self.insert_before_append_linked(parentp, child, strict)
    }

    #[allow(clippy::unused_self)]
    fn insert_before_append_linked(
        &self,
        parentp: N,
        mut child: N,
        _strict: bool,
    ) -> DomResult<Node> {
        if child.parent().is_some() {
            tree::unlink(child);
        }
        let new_child;
        if child.t() == ffi::XML_TEXT_NODE
            && parentp.last().is_some_and(|l| l.t() == ffi::XML_TEXT_NODE)
        {
            child.set_parent(Some(parentp));
            new_child = child;
            if parentp.children().is_none() {
                parentp.set_children(Some(child));
                parentp.set_last(Some(child));
            } else {
                let last = parentp.last().expect("checked above");
                last.set_next(Some(new_child));
                new_child.set_prev(Some(last));
                parentp.set_last(Some(new_child));
            }
        } else if child.t() == ffi::XML_ATTRIBUTE_NODE {
            if let Some(lastattr) = has_ns_prop_for(parentp, child) {
                if lastattr.t() != ffi::XML_ATTRIBUTE_DECL {
                    if lastattr == child {
                        return Ok(Node::wrap(child));
                    }
                    tree::unlink(lastattr);
                    free_resource(lastattr);
                }
            }
            new_child = add_child(parentp, child).ok_or_else(|| {
                DomError::Error("Cannot add newnode as the previous sibling of refnode".into())
            })?;
        } else if child.t() == ffi::XML_DOCUMENT_FRAG_NODE {
            let last = child.last();
            let first = insert_fragment(parentp, parentp.last(), None, child);
            if let (Some(f), Some(l)) = (first, last) {
                tree::reconcile_ns_list(parentp.doc_ptr(), f, l);
            }
            child = first.unwrap_or(child);
            return Ok(Node::wrap(child));
        } else {
            new_child = add_child(parentp, child).ok_or_else(|| {
                DomError::Error("Cannot add newnode as the previous sibling of refnode".into())
            })?;
            tree::reconcile_ns(parentp.doc_ptr(), new_child);
        }
        Ok(Node::wrap(new_child))
    }

    /// `replaceChild($node, $child)`.
    pub fn replace_child(&self, node: &Node, child: &Node) -> DomResult<Node> {
        let nodep = self.node()?;
        let mut newchild = node.node()?;
        let oldchild = child.node()?;
        let modern = self.family_is_modern();
        let strict = self.strict();
        if newchild.doc_ptr() != nodep.doc_ptr() && !newchild.doc_ptr().is_null() {
            return Err(throw(C::WrongDocument, strict));
        }
        if modern {
            replace_node_validity_checks(nodep, newchild, oldchild)?;
        } else {
            if !children_valid(nodep) || nodep.children().is_none() {
                return Err(DomError::Silent);
            }
            check_legacy_insertion_validity(nodep, newchild, strict, false)?;
            if (oldchild.t() == ffi::XML_ATTRIBUTE_NODE)
                != (newchild.t() == ffi::XML_ATTRIBUTE_NODE)
            {
                return Err(throw(C::HierarchyRequest, strict));
            }
            if oldchild.parent() != Some(nodep) {
                return Err(throw(C::NotFound, strict));
            }
        }
        if newchild.doc_ptr().is_null() && !nodep.doc_ptr().is_null() {
            set_tree_doc(newchild, nodep.doc_ptr());
            set_document_ref_pointers(newchild, self.doc_ref().as_ref());
        }
        if newchild.t() == ffi::XML_DOCUMENT_FRAG_NODE {
            let prevsib = oldchild.prev();
            let nextsib = oldchild.next();
            tree::unlink(oldchild);
            let last = newchild.last();
            let first = insert_fragment(nodep, prevsib, nextsib, newchild);
            if let (Some(f), Some(l), false) = (first, last, modern) {
                tree::reconcile_ns_list(nodep.doc_ptr(), f, l);
            }
            if let Some(f) = first {
                newchild = f;
            }
            let _ = newchild;
        } else if oldchild != newchild {
            // SAFETY: doc may be NULL.
            let int_subset = unsafe { ffi::xmlGetIntSubset(nodep.doc_ptr()) };
            let replacedoctype = int_subset.cast::<ffi::xmlNode>() == oldchild.p();
            // SAFETY: live nodes.
            unsafe { ffi::xmlReplaceNode(oldchild.p(), newchild.p()) };
            if !modern {
                tree::reconcile_ns(nodep.doc_ptr(), newchild);
            }
            if replacedoctype {
                if let Some(doc) = nodep.doc() {
                    doc.d().intSubset = newchild.as_dtd();
                }
            }
        }
        self.invalidate();
        Ok(Node::wrap(oldchild))
    }

    /// `removeChild($child)`.
    pub fn remove_child(&self, child: &Node) -> DomResult<Node> {
        let nodep = self.node()?;
        let childp = child.node()?;
        let strict = self.strict();
        if nodep.children().is_none() || childp.parent() != Some(nodep) {
            return Err(throw(C::NotFound, strict));
        }
        if is_read_only(nodep) || childp.parent().is_some_and(is_read_only) {
            return Err(throw(C::NoModificationAllowed, strict));
        }
        tree::unlink(childp);
        self.invalidate();
        Ok(Node::wrap(childp))
    }

    /// `appendChild($node)`.
    pub fn append_child(&self, node: &Node) -> DomResult<Node> {
        let nodep = self.node()?;
        let child = node.node()?;
        if self.family_is_modern() {
            if tree::pre_insert_parent_invalid(nodep) {
                return Err(DomError::dom(C::HierarchyRequest));
            }
            let doc = self.ctx_doc();
            let ctx = Ctx { doc: doc.as_ref() };
            ctx.invalidate();
            tree::node_append(ctx, child, nodep)?;
            return Ok(Node::wrap(child));
        }
        self.append_child_legacy(nodep, child)
    }

    fn append_child_legacy(&self, nodep: N, mut child: N) -> DomResult<Node> {
        if !children_valid(nodep) {
            return Err(DomError::Silent);
        }
        let strict = self.strict();
        check_legacy_insertion_validity(nodep, child, strict, true)?;
        if child.doc_ptr().is_null() && !nodep.doc_ptr().is_null() {
            set_tree_doc(child, nodep.doc_ptr());
            set_document_ref_pointers(child, self.doc_ref().as_ref());
        }
        if child.parent().is_some() {
            tree::unlink(child);
        }
        let new_child;
        if child.t() == ffi::XML_TEXT_NODE
            && nodep.last().is_some_and(|l| l.t() == ffi::XML_TEXT_NODE)
        {
            child.set_parent(Some(nodep));
            new_child = child;
            if nodep.children().is_none() {
                nodep.set_children(Some(child));
                nodep.set_last(Some(child));
            } else {
                let last = nodep.last().expect("checked above");
                last.set_next(Some(new_child));
                new_child.set_prev(Some(last));
                nodep.set_last(Some(new_child));
            }
        } else if child.t() == ffi::XML_ATTRIBUTE_NODE {
            if let Some(lastattr) = has_ns_prop_for(nodep, child) {
                if lastattr.t() != ffi::XML_ATTRIBUTE_DECL && lastattr != child {
                    tree::unlink(lastattr);
                    free_resource(lastattr);
                }
            }
            new_child = add_child(nodep, child).ok_or_else(|| throw(C::InvalidState, strict))?;
            ns::reconcile_attribute_namespace_after_insertion(new_child);
        } else if child.t() == ffi::XML_DOCUMENT_FRAG_NODE {
            let last = child.last();
            let first = insert_fragment(nodep, nodep.last(), None, child);
            if let (Some(f), Some(l)) = (first, last) {
                tree::reconcile_ns_list(nodep.doc_ptr(), f, l);
            }
            child = first.unwrap_or(child);
            self.invalidate();
            return Ok(Node::wrap(child));
        } else if child.t() == ffi::XML_DTD_NODE {
            if let Some(doc) = nodep.doc() {
                if !doc.d().intSubset.is_null() {
                    return Err(throw_msg(
                        C::HierarchyRequest,
                        "A document may only contain one document type",
                        strict,
                    ));
                }
            }
            new_child = add_child(nodep, child).ok_or_else(|| throw(C::InvalidState, strict))?;
            if let Some(doc) = nodep.doc() {
                doc.d().intSubset = new_child.as_dtd();
            }
        } else {
            new_child = add_child(nodep, child).ok_or_else(|| throw(C::InvalidState, strict))?;
            tree::reconcile_ns(nodep.doc_ptr(), new_child);
        }
        self.invalidate();
        Ok(Node::wrap(new_child))
    }

    /// `hasChildNodes()`.
    pub fn has_child_nodes(&self) -> DomResult<bool> {
        let n = self.node()?;
        Ok(children_valid(n) && n.children().is_some())
    }

    /// `cloneNode($deep)`; `Err(Silent)` means PHP returns false.
    pub fn clone_node(&self, deep: bool) -> DomResult<Node> {
        let n = self.node()?;
        let clone_document = n.is_doc();
        let mut private = None;
        if self.is_modern() {
            private = Some(if clone_document {
                crate::ns::PrivateData::create()
            } else {
                self.doc_ref()
                    .and_then(|d| d.private_data())
                    .unwrap_or_else(crate::ns::PrivateData::create)
            });
        }
        let cloned = {
            let mut mapper = private.as_ref().map(|p| p.mapper.borrow_mut());
            clone_node_raw(mapper.as_deref_mut(), n, n.doc_ptr(), deep)
        };
        let Some(node) = cloned else {
            if clone_document {
                if let Some(p) = private {
                    p.destroy();
                }
            }
            return Err(DomError::Silent);
        };
        if clone_document {
            let doc = crate::object::DocInner::new(node.as_doc());
            if let Some(src) = self.doc_ref() {
                crate::document::copy_document_ref(&src, &doc);
            }
            *doc.private.borrow_mut() = private;
            let obj = Node::wrap_with_doc(node, Some(doc));
            return Ok(obj);
        }
        if node.t() == ffi::XML_ATTRIBUTE_NODE && n.ns().is_some() && node.ns().is_none() {
            node.set_ns(n.ns_ptr());
        }
        Ok(Node::wrap_with_doc(node, self.doc_ref()))
    }

    /// `normalize()`.
    pub fn normalize(&self) -> DomResult<()> {
        let n = self.node()?;
        let _scope = crate::stack::StackScope::enter();
        if self.is_modern() {
            normalize_modern(n);
        } else {
            normalize_legacy(n);
        }
        crate::stack::take_error().map_or(Ok(()), Err)
    }

    /// `isSupported($feature, $version)`.
    pub fn is_supported(feature: &[u8], version: &[u8]) -> bool {
        has_feature(feature, version)
    }

    /// `hasAttributes()`.
    pub fn has_attributes(&self) -> DomResult<bool> {
        let n = self.node()?;
        Ok(n.is_element() && n.properties().is_some())
    }

    /// `isSameNode($other)`.
    pub fn is_same_node(&self, other: Option<&Node>) -> DomResult<bool> {
        let n = self.node()?;
        let Some(other) = other else { return Ok(false) };
        Ok(n == other.node()?)
    }

    /// `isEqualNode($other)`.
    pub fn is_equal_node(&self, other: Option<&Node>) -> DomResult<bool> {
        let Some(other) = other else { return Ok(false) };
        let otherp = other.node()?;
        let nodep = self.node()?;
        if nodep == otherp {
            return Ok(true);
        }
        let _scope = crate::stack::StackScope::enter();
        let r = is_equal_node(nodep, otherp, self.family_is_modern());
        crate::stack::take_error().map_or(Ok(r), Err)
    }

    /// `lookupPrefix($namespace)`.
    pub fn lookup_prefix(&self, uri: Option<&[u8]>) -> DomResult<Option<Vec<u8>>> {
        let n = self.node()?;
        let uri = uri.unwrap_or_default();
        if uri.is_empty() {
            return Ok(None);
        }
        let lookupp = match n.t() {
            ffi::XML_ELEMENT_NODE => Some(n),
            // SAFETY: live document.
            ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE => {
                N::new(unsafe { ffi::xmlDocGetRootElement(n.as_doc()) })
            }
            ffi::XML_ENTITY_NODE
            | ffi::XML_NOTATION_NODE
            | ffi::XML_DOCUMENT_FRAG_NODE
            | ffi::XML_DOCUMENT_TYPE_NODE
            | ffi::XML_DTD_NODE => return Ok(None),
            _ => n.parent(),
        };
        let Some(lookupp) = lookupp else {
            return Ok(None);
        };
        if self.family_is_modern() {
            return Ok(locate_a_namespace_prefix(lookupp, uri).map(<[u8]>::to_vec));
        }
        let u = cs(uri);
        // SAFETY: live node.
        let nsptr = Ns::new(unsafe {
            ffi::xmlSearchNsByHref(lookupp.doc_ptr(), lookupp.p(), u.as_ptr().cast())
        });
        Ok(nsptr.and_then(Ns::prefix).map(<[u8]>::to_vec))
    }

    /// `isDefaultNamespace($namespace)`.
    pub fn is_default_namespace(&self, uri: Option<&[u8]>) -> DomResult<bool> {
        let mut n = self.node()?;
        if self.family_is_modern() {
            let uri = uri.filter(|u| !u.is_empty());
            let ns_uri = locate_a_namespace(n, None);
            return Ok(str_eq(uri, ns_uri.as_deref()));
        }
        let uri = uri.unwrap_or_default();
        if !uri.is_empty() {
            if n.is_doc() {
                // SAFETY: live document.
                match N::new(unsafe { ffi::xmlDocGetRootElement(n.as_doc()) }) {
                    Some(r) => n = r,
                    None => return Ok(false),
                }
            }
            // SAFETY: live node.
            let nsptr = Ns::new(unsafe { ffi::xmlSearchNs(n.doc_ptr(), n.p(), ptr::null()) });
            if nsptr.is_some_and(|ns| ns.href() == uri) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `lookupNamespaceURI($prefix)`.
    pub fn lookup_namespace_uri(&self, prefix: Option<&[u8]>) -> DomResult<Option<Vec<u8>>> {
        let mut n = self.node()?;
        if self.is_modern() {
            let prefix = prefix.filter(|p| !p.is_empty());
            return Ok(locate_a_namespace(n, prefix));
        }
        if n.is_doc() {
            // SAFETY: live document.
            match N::new(unsafe { ffi::xmlDocGetRootElement(n.as_doc()) }) {
                Some(r) => n = r,
                None => return Ok(None),
            }
        }
        let p = prefix.map(cs);
        // SAFETY: live node.
        let nsptr = Ns::new(unsafe {
            ffi::xmlSearchNs(
                n.doc_ptr(),
                n.p(),
                p.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
            )
        });
        Ok(nsptr
            .filter(|ns| !ns.href_ptr().is_null())
            .map(|ns| ns.href().to_vec()))
    }

    /// `getNodePath()`.
    pub fn get_node_path(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.node()?;
        // SAFETY: live node.
        let v = take(unsafe { ffi::xmlGetNodePath(n.p()) });
        if v.is_none() && self.family_is_modern() {
            return Err(DomError::dom(C::InvalidState));
        }
        Ok(v)
    }

    /// `getLineNo()`.
    pub fn get_line_no(&self) -> DomResult<i64> {
        let n = self.node()?;
        // SAFETY: live node.
        Ok(unsafe { ffi::xmlGetLineNo(n.p()) })
    }

    /// `contains($other)`.
    pub fn contains(&self, other: Option<&Node>) -> DomResult<bool> {
        let Some(other) = other else { return Ok(false) };
        let mut otherp = other.node()?;
        let thisp = self.node()?;
        loop {
            if otherp == thisp {
                return Ok(true);
            }
            match otherp.parent() {
                Some(p) => otherp = p,
                None => return Ok(false),
            }
        }
    }

    /// `getRootNode()`.
    pub fn get_root_node(&self) -> DomResult<Node> {
        let mut n = self.node()?;
        while let Some(p) = n.parent() {
            n = p;
        }
        Ok(Node::wrap(n))
    }

    /// `compareDocumentPosition($other)`.
    #[allow(clippy::too_many_lines)]
    pub fn compare_document_position(&self, other: &Node) -> DomResult<i64> {
        use position::{
            CONTAINED_BY, CONTAINS, DISCONNECTED, FOLLOWING, IMPLEMENTATION_SPECIFIC, PRECEDING,
        };
        let this = self.node()?;
        let otherp = other.node()?;
        if this == otherp {
            return Ok(0);
        }
        let mut node1 = Some(otherp);
        let mut node2 = Some(this);
        let mut attr1 = None;
        let mut attr2 = None;
        if otherp.t() == ffi::XML_ATTRIBUTE_NODE {
            attr1 = Some(otherp);
            node1 = otherp.parent();
        }
        if this.t() == ffi::XML_ATTRIBUTE_NODE {
            attr2 = Some(this);
            node2 = this.parent();
            if let (Some(a1), Some(n1), Some(n2)) = (attr1, node1, node2) {
                if n1 == n2 {
                    for attr in n2.attr_iter() {
                        if is_equal_attr(attr, a1) {
                            return Ok(IMPLEMENTATION_SPECIFIC | PRECEDING);
                        } else if is_equal_attr(attr, this) {
                            return Ok(IMPLEMENTATION_SPECIFIC | FOLLOWING);
                        }
                    }
                }
            }
        }
        let disconnected = |n1: Option<N>, n2: Option<N>| -> i64 {
            let ordering = if n1 == n2 {
                if other.object_id() < self.object_id() {
                    PRECEDING
                } else {
                    FOLLOWING
                }
            } else if raw_addr(n1) < raw_addr(n2) {
                PRECEDING
            } else {
                FOLLOWING
            };
            DISCONNECTED | IMPLEMENTATION_SPECIFIC | ordering
        };
        let (Some(mut n1), Some(mut n2)) = (node1, node2) else {
            return Ok(disconnected(node1, node2));
        };
        let mut n2_anc_of_n1 = false;
        let mut d1 = 0usize;
        let mut r1 = n1;
        while let Some(p) = r1.parent() {
            r1 = p;
            if r1 == n2 {
                n2_anc_of_n1 = true;
            }
            d1 += 1;
        }
        let mut n1_anc_of_n2 = false;
        let mut d2 = 0usize;
        let mut r2 = n2;
        while let Some(p) = r2.parent() {
            r2 = p;
            if r2 == n1 {
                n1_anc_of_n2 = true;
            }
            d2 += 1;
        }
        if r1 != r2 {
            return Ok(disconnected(Some(n1), Some(n2)));
        }
        if (n1_anc_of_n2 && attr1.is_none()) || (n1 == n2 && attr2.is_some()) {
            return Ok(CONTAINS | PRECEDING);
        }
        if (n2_anc_of_n1 && attr2.is_none()) || (n1 == n2 && attr1.is_some()) {
            return Ok(CONTAINED_BY | FOLLOWING);
        }
        if n1_anc_of_n2 {
            return Ok(PRECEDING);
        } else if n2_anc_of_n1 {
            return Ok(FOLLOWING);
        }
        while d1 > d2 {
            n1 = n1.parent().expect("depth computed above");
            d1 -= 1;
        }
        while d2 > d1 {
            n2 = n2.parent().expect("depth computed above");
            d2 -= 1;
        }
        while n1.parent() != n2.parent() {
            n1 = n1.parent().expect("common root");
            n2 = n2.parent().expect("common root");
        }
        let mut cur = n1.next();
        while let Some(c) = cur {
            if c == n2 {
                return Ok(PRECEDING);
            }
            cur = c.next();
        }
        Ok(FOLLOWING)
    }

    /// `C14N()`.
    pub fn c14n(
        &self,
        exclusive: bool,
        with_comments: bool,
        xpath: Option<&C14nXPath>,
        ns_prefixes: Option<&[Vec<u8>]>,
    ) -> DomResult<Option<Vec<u8>>> {
        match crate::save::canonicalize(self, None, exclusive, with_comments, xpath, ns_prefixes)? {
            crate::save::C14nOut::Bytes(b) => Ok(Some(b)),
            crate::save::C14nOut::Written(_) | crate::save::C14nOut::Failed => Ok(None),
        }
    }

    /// `C14NFile()`: bytes written or `None` (false).
    pub fn c14n_file(
        &self,
        uri: &[u8],
        exclusive: bool,
        with_comments: bool,
        xpath: Option<&C14nXPath>,
        ns_prefixes: Option<&[Vec<u8>]>,
    ) -> DomResult<Option<i64>> {
        match crate::save::canonicalize(
            self,
            Some(uri),
            exclusive,
            with_comments,
            xpath,
            ns_prefixes,
        )? {
            crate::save::C14nOut::Written(n) => Ok(Some(n)),
            _ => Ok(None),
        }
    }

    /// `__sleep()` / `serialize()`: always throws.
    pub fn sleep(class: &str) -> DomError {
        DomError::Error(format!(
            "Serialization of '{class}' is not allowed, unless serialization methods are implemented in a subclass"
        ))
    }

    /// `__wakeup()`: always throws.
    pub fn wakeup(class: &str) -> DomError {
        DomError::Error(format!(
            "Unserialization of '{class}' is not allowed, unless unserialization methods are implemented in a subclass"
        ))
    }
}

/// The `$xpath` argument of `C14N()`: `["query" => ..., "namespaces" => [...]]`.
#[derive(Debug, Clone, Default)]
pub struct C14nXPath {
    /// The `query` key (`None` when missing; `Some(Err(type))` when not a
    /// string).
    pub query: Option<Result<Vec<u8>, String>>,
    /// `namespaces` prefix → URI pairs (string keys only).
    pub namespaces: Vec<(Vec<u8>, Vec<u8>)>,
}

fn raw_addr(n: Option<N>) -> usize {
    n.map_or(0, |n| n.p() as usize)
}

/// `dom_has_feature`.
pub(crate) fn has_feature(feature: &[u8], version: &[u8]) -> bool {
    if (version == b"1.0" || version == b"2.0" || version.is_empty())
        && (feature.eq_ignore_ascii_case(b"XML")
            || (feature.eq_ignore_ascii_case(b"Core") && version == b"1.0"))
    {
        return true;
    }
    false
}

/// `dom_replace_node_validity_checks`.
fn replace_node_validity_checks(parent: N, node: N, child: N) -> DomResult<()> {
    if tree::pre_insert_parent_invalid(parent) {
        return Err(DomError::dom(C::HierarchyRequest));
    }
    if !tree::hierarchy_ok(parent, node) {
        return Err(DomError::dom(C::HierarchyRequest));
    }
    if child.parent() != Some(parent) {
        return Err(DomError::dom(C::NotFound));
    }
    if !matches!(
        node.t(),
        ffi::XML_DOCUMENT_FRAG_NODE
            | ffi::XML_DTD_NODE
            | ffi::XML_ELEMENT_NODE
            | ffi::XML_TEXT_NODE
            | ffi::XML_CDATA_SECTION_NODE
            | ffi::XML_COMMENT_NODE
            | ffi::XML_PI_NODE
    ) {
        return Err(DomError::dom(C::HierarchyRequest));
    }
    let parent_is_document = parent.is_doc();
    if parent_is_document && node.is_text_like() {
        return Err(DomError::dom_msg(
            C::HierarchyRequest,
            "Cannot insert text as a child of a document",
        ));
    }
    if !parent_is_document && node.t() == ffi::XML_DTD_NODE {
        return Err(DomError::dom_msg(
            C::HierarchyRequest,
            "Cannot insert a document type into anything other than a document",
        ));
    }
    if parent_is_document {
        match node.t() {
            ffi::XML_DOCUMENT_FRAG_NODE => tree::fragment_check_replace(parent, node, child)?,
            ffi::XML_ELEMENT_NODE => {
                // SAFETY: live document.
                let root = N::new(unsafe { ffi::xmlDocGetRootElement(parent.as_doc()) });
                if root != Some(child) {
                    return Err(DomError::dom_msg(
                        C::HierarchyRequest,
                        "Cannot have more than one element child in a document",
                    ));
                }
                if tree::has_following_sibling_of_type(child, ffi::XML_DTD_NODE) {
                    return Err(DomError::dom_msg(
                        C::HierarchyRequest,
                        "Document types must be the first child in a document",
                    ));
                }
            }
            ffi::XML_DTD_NODE => {
                let doc = parent.d();
                if doc.intSubset.cast::<ffi::xmlNode>() != child.p()
                    || tree::has_preceding_sibling_of_type(child, ffi::XML_ELEMENT_NODE)
                {
                    return Err(DomError::dom_msg(
                        C::HierarchyRequest,
                        "Document types must be the first child in a document",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_empty_node(n: N) -> bool {
    n.content().is_empty()
}

fn free_node_if_unreferenced(n: N) {
    if n.private().is_null() {
        tree::free_node(n);
    }
}

fn merge_adjacent_exclusive_text_nodes(node: N) {
    let mut nextp = node.next();
    while let Some(nx) = nextp {
        if nx.t() != ffi::XML_TEXT_NODE {
            break;
        }
        let newnext = nx.next();
        if !nx.content_ptr().is_null() {
            // SAFETY: live text nodes.
            unsafe { ffi::xmlNodeAddContent(node.p(), nx.content_ptr()) };
        }
        tree::unlink(nx);
        free_node_if_unreferenced(nx);
        nextp = newnext;
    }
}

/// `php_dom_normalize_legacy`.
pub(crate) fn normalize_legacy(nodep: N) {
    if crate::stack::check(crate::stack::STACK_OVERFLOW_MESSAGE) {
        return;
    }
    let mut child = nodep.children();
    while let Some(c) = child {
        match c.t() {
            ffi::XML_TEXT_NODE => {
                merge_adjacent_exclusive_text_nodes(c);
                if is_empty_node(c) {
                    let next = c.next();
                    tree::unlink(c);
                    free_node_if_unreferenced(c);
                    child = next;
                    continue;
                }
            }
            ffi::XML_ELEMENT_NODE => {
                normalize_legacy(c);
                for attr in c.attr_iter() {
                    normalize_legacy(attr);
                }
            }
            _ => {}
        }
        child = c.next();
    }
}

/// `php_dom_normalize_modern`.
pub(crate) fn normalize_modern(this: N) {
    if crate::stack::check(crate::stack::STACK_OVERFLOW_MESSAGE) {
        return;
    }
    let mut node = this.children();
    while let Some(n) = node {
        if n.t() == ffi::XML_TEXT_NODE {
            if is_empty_node(n) {
                let next = n.next();
                tree::unlink(n);
                free_node_if_unreferenced(n);
                node = next;
                continue;
            }
            merge_adjacent_exclusive_text_nodes(n);
        } else if n.is_element() {
            normalize_modern(n);
        }
        node = n.next();
    }
}

fn content_equal(a: N, b: N) -> bool {
    let ca = tree::node_get_content(a);
    let cb = tree::node_get_content(b);
    ca == cb
}

fn ns_uri_equal(a: N, b: N) -> bool {
    str_eq(a.ns().map(Ns::href), b.ns().map(Ns::href))
}

fn ns_prefix_equal(a: N, b: N) -> bool {
    str_eq(a.ns().and_then(Ns::prefix), b.ns().and_then(Ns::prefix))
}

/// `php_dom_is_equal_attr`.
pub(crate) fn is_equal_attr(a: N, b: N) -> bool {
    a.name() == b.name() && ns_uri_equal(a, b) && content_equal(a, b)
}

fn list_eq_ordered(a: Option<N>, b: Option<N>, spec: bool) -> bool {
    let la: Vec<N> = crate::raw::Siblings::from(a).collect();
    let lb: Vec<N> = crate::raw::Siblings::from(b).collect();
    la.len() == lb.len() && la.iter().zip(&lb).all(|(x, y)| is_equal_node(*x, *y, spec))
}

fn list_eq_unordered(a: Option<N>, b: Option<N>, spec: bool) -> bool {
    let la: Vec<N> = crate::raw::Siblings::from(a).collect();
    let lb: Vec<N> = crate::raw::Siblings::from(b).collect();
    la.len() == lb.len()
        && la
            .iter()
            .all(|x| lb.iter().any(|y| is_equal_node(*x, *y, spec)))
}

fn ns_list(mut cur: Option<Ns>) -> Vec<Ns> {
    let mut v = Vec::new();
    while let Some(c) = cur {
        v.push(c);
        cur = c.next();
    }
    v
}

/// `php_dom_node_is_equal_node`.
pub(crate) fn is_equal_node(a: N, b: N, spec: bool) -> bool {
    if crate::stack::check("Maximum call stack size reached.") {
        return false;
    }
    if a.t() != b.t() {
        return false;
    }
    match a.t() {
        ffi::XML_ELEMENT_NODE => {
            a.name() == b.name()
                && ns_prefix_equal(a, b)
                && ns_uri_equal(a, b)
                && list_eq_unordered(a.properties(), b.properties(), spec)
                && (spec || {
                    let la = ns_list(a.ns_def());
                    let lb = ns_list(b.ns_def());
                    la.len() == lb.len()
                        && la.iter().all(|x| {
                            lb.iter()
                                .any(|y| str_eq(x.prefix(), y.prefix()) && x.href() == y.href())
                        })
                })
                && list_eq_ordered(a.children(), b.children(), spec)
        }
        ffi::XML_DTD_NODE => {
            let (da, db) = (a.dtd(), b.dtd());
            str_eq(crate::raw::opt_cstr(da.name), crate::raw::opt_cstr(db.name))
                && str_eq(
                    crate::raw::opt_cstr(da.ExternalID),
                    crate::raw::opt_cstr(db.ExternalID),
                )
                && str_eq(
                    crate::raw::opt_cstr(da.SystemID),
                    crate::raw::opt_cstr(db.SystemID),
                )
        }
        ffi::XML_PI_NODE => {
            a.name() == b.name()
                && str_eq(
                    crate::raw::opt_cstr(a.content_ptr()),
                    crate::raw::opt_cstr(b.content_ptr()),
                )
        }
        ffi::XML_TEXT_NODE | ffi::XML_COMMENT_NODE | ffi::XML_CDATA_SECTION_NODE => str_eq(
            crate::raw::opt_cstr(a.content_ptr()),
            crate::raw::opt_cstr(b.content_ptr()),
        ),
        ffi::XML_ATTRIBUTE_NODE => is_equal_attr(a, b),
        ffi::XML_ENTITY_REF_NODE => a.name() == b.name(),
        ffi::XML_ENTITY_DECL | ffi::XML_NOTATION_NODE | ffi::XML_ENTITY_NODE => {
            let (ea, eb) = (a.ent(), b.ent());
            ea.etype == eb.etype
                && a.name() == b.name()
                && str_eq(
                    crate::raw::opt_cstr(ea.ExternalID),
                    crate::raw::opt_cstr(eb.ExternalID),
                )
                && str_eq(
                    crate::raw::opt_cstr(ea.SystemID),
                    crate::raw::opt_cstr(eb.SystemID),
                )
                && content_equal(a, b)
        }
        ffi::XML_NAMESPACE_DECL => {
            let (na, nb) = (a.ns(), b.ns());
            match (na, nb) {
                (Some(x), Some(y)) => str_eq(x.prefix(), y.prefix()) && x.href() == y.href(),
                _ => false,
            }
        }
        ffi::XML_DOCUMENT_FRAG_NODE | ffi::XML_HTML_DOCUMENT_NODE | ffi::XML_DOCUMENT_NODE => {
            list_eq_ordered(a.children(), b.children(), spec)
        }
        _ => false,
    }
}

/// `dom_locate_a_namespace_prefix`.
fn locate_a_namespace_prefix(mut elem: N, uri: &[u8]) -> Option<&'static [u8]> {
    loop {
        if let Some(ns) = elem.ns() {
            if let Some(p) = ns.prefix() {
                if ns.href() == uri {
                    return Some(p);
                }
            }
        }
        for attr in elem.attr_iter() {
            if let (Some(ans), Some(c)) = (attr.ns(), attr.children()) {
                if ans.prefix() == Some(b"xmlns")
                    && c.content() == uri
                    && !c.content_ptr().is_null()
                {
                    return Some(attr.name());
                }
            }
        }
        match elem.parent() {
            Some(p) if p.is_element() => elem = p,
            _ => return None,
        }
    }
}

/// `dom_locate_a_namespace`.
pub(crate) fn locate_a_namespace(node: N, prefix: Option<&[u8]>) -> Option<Vec<u8>> {
    match node.t() {
        ffi::XML_ELEMENT_NODE => {
            if let Some(p) = prefix {
                if p.eq_ignore_ascii_case(b"xml") {
                    return Some(ns::XML_NS.to_vec());
                }
                if p.eq_ignore_ascii_case(b"xmlns") {
                    return Some(ns::XMLNS_NS.to_vec());
                }
            }
            let mut n = node;
            loop {
                if let Some(ns) = n.ns() {
                    if str_eq(ns.prefix(), prefix) {
                        return Some(ns.href().to_vec());
                    }
                }
                for attr in n.attr_iter() {
                    let Some(ans) = attr.ns() else { continue };
                    if !ns_is_fast_ex(ans, &XMLNS_TOKEN) {
                        continue;
                    }
                    let matches = match prefix {
                        Some(p) => ans.prefix() == Some(b"xmlns") && attr.name() == p,
                        None => ans.prefix().is_none() && attr.name() == b"xmlns",
                    };
                    if matches {
                        return match attr.children() {
                            Some(c) if !c.content().is_empty() => Some(c.content().to_vec()),
                            _ => None,
                        };
                    }
                }
                match n.parent() {
                    Some(p) if p.is_element() => n = p,
                    _ => return None,
                }
            }
        }
        ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE => {
            // SAFETY: live document.
            let root = N::new(unsafe { ffi::xmlDocGetRootElement(node.as_doc()) })?;
            locate_a_namespace(root, prefix)
        }
        ffi::XML_DTD_NODE | ffi::XML_DOCUMENT_FRAG_NODE => None,
        _ => match node.parent() {
            Some(p) if p.is_element() => locate_a_namespace(p, prefix),
            _ => None,
        },
    }
}

/// `dom_clone_container_helper`.
fn clone_container_helper(
    mapper: &mut crate::ns::NsMapper,
    src: N,
    dst_doc: *mut ffi::xmlDoc,
) -> Option<N> {
    // SAFETY: live source node; dst_doc may be NULL.
    let clone = N::new(unsafe { ffi::xmlDocCopyNode(src.p(), dst_doc, 0) })?;
    clone.set_ns(src.ns_ptr());
    if src.is_element() {
        let mut last_added_attr: Option<N> = None;
        if let Some(first) = src.ns_def() {
            let mut cur = Some(first);
            while let Some(ns) = cur {
                crate::ns::mark_attribute(mapper, clone, ns);
                cur = ns.next();
            }
            let mut l = clone.properties();
            while let Some(a) = l {
                last_added_attr = Some(a);
                l = a.next();
            }
        }
        for attr in src.attr_iter() {
            // SAFETY: live attribute.
            let Some(new_attr) = N::new(unsafe { ffi::xmlDocCopyNode(attr.p(), dst_doc, 0) })
            else {
                tree::free_node(clone);
                return None;
            };
            match last_added_attr {
                None => clone.set_properties(Some(new_attr)),
                Some(l) => {
                    new_attr.set_prev(Some(l));
                    l.set_next(Some(new_attr));
                }
            }
            new_attr.set_parent(Some(clone));
            last_added_attr = Some(new_attr);
            new_attr.set_ns(attr.ns_ptr());
        }
    }
    Some(clone)
}

/// `dom_clone_helper`.
fn clone_helper(
    mapper: &mut crate::ns::NsMapper,
    src_node: N,
    mut dst_doc: *mut ffi::xmlDoc,
    recursive: bool,
) -> Option<N> {
    let outer = clone_container_helper(mapper, src_node, dst_doc)?;
    if !recursive
        || !matches!(
            src_node.t(),
            ffi::XML_ELEMENT_NODE
                | ffi::XML_DOCUMENT_FRAG_NODE
                | ffi::XML_DOCUMENT_NODE
                | ffi::XML_HTML_DOCUMENT_NODE
        )
    {
        return Some(outer);
    }
    if src_node.is_doc() {
        dst_doc = outer.as_doc();
        let original_subset = src_node.d().intSubset;
        if !original_subset.is_null() {
            // SAFETY: live DTD.
            let copy = unsafe { ffi::xmlCopyDtd(original_subset) };
            if copy.is_null() {
                tree::free_node(outer);
                return None;
            }
            outer.d().intSubset = copy;
            // SAFETY: fresh DTD copy owned by the new document.
            unsafe {
                (*copy).parent = dst_doc;
                ffi::xmlSetTreeDoc(copy.cast(), dst_doc);
            }
            outer.set_children(N::from_dtd(copy));
            outer.set_last(N::from_dtd(copy));
        }
    }
    let mut cloned_parent = outer;
    let base = src_node;
    let mut src = src_node.children();
    while let Some(s) = src {
        let cloned = if s.is_element() {
            clone_container_helper(mapper, s, dst_doc)
        } else if s.t() == ffi::XML_DTD_NODE {
            None
        } else {
            // SAFETY: live node.
            N::new(unsafe { ffi::xmlDocCopyNode(s.p(), dst_doc, 1) })
        };
        if let Some(c) = cloned {
            if cloned_parent.children().is_none() {
                cloned_parent.set_children(Some(c));
            } else {
                let last = cloned_parent.last().expect("non-empty list");
                c.set_prev(Some(last));
                last.set_next(Some(c));
            }
            c.set_parent(Some(cloned_parent));
            cloned_parent.set_last(Some(c));
        }
        if s.is_element() && s.children().is_some() {
            if let Some(c) = cloned {
                cloned_parent = c;
            }
            src = s.children();
        } else if let Some(nx) = s.next() {
            src = Some(nx);
        } else {
            let mut up = s;
            loop {
                up = up.parent()?;
                if up == base {
                    return Some(outer);
                }
                cloned_parent = cloned_parent.parent()?;
                if up.next().is_some() {
                    break;
                }
            }
            src = up.next();
        }
    }
    Some(outer)
}

/// `dom_clone_node`.
pub(crate) fn clone_node_raw(
    mapper: Option<&mut crate::ns::NsMapper>,
    node: N,
    doc: *mut ffi::xmlDoc,
    recursive: bool,
) -> Option<N> {
    if node.t() == ffi::XML_DTD_NODE {
        // SAFETY: live DTD.
        let dtd = unsafe { ffi::xmlCopyDtd(node.as_dtd()) };
        let d = N::from_dtd(dtd)?;
        set_tree_doc(d, doc);
        return Some(d);
    }
    if let Some(mapper) = mapper {
        let clone = clone_helper(mapper, node, doc, recursive)?;
        if clone.doc_ptr() != node.doc_ptr() {
            if matches!(
                clone.t(),
                ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE | ffi::XML_DOCUMENT_FRAG_NODE
            ) {
                let mut c = clone.children();
                while let Some(ch) = c {
                    crate::ns::reconcile_modern(mapper, ch);
                    c = ch.next();
                }
            } else {
                crate::ns::reconcile_modern(mapper, clone);
            }
        }
        Some(clone)
    } else {
        let extended = if !recursive && node.is_element() {
            2
        } else {
            i32::from(recursive)
        };
        // SAFETY: live node.
        N::new(unsafe { ffi::xmlDocCopyNode(node.p(), doc, extended) })
    }
}

impl Node {
    /// `clone $node` (`dom_objects_store_clone_obj`): a deep copy of the
    /// node with the same PHP class. User properties are the embedder's.
    pub fn clone_object(&self) -> DomResult<Node> {
        if let Some(n) = self.n() {
            if n.t() == ffi::XML_NAMESPACE_DECL {
                let c = crate::xpath::clone_namespace_node(self)
                    .ok_or_else(|| DomError::dom(C::InvalidState))?;
                c.set_class_name(self.class_name());
                return Ok(c);
            }
        }
        let Some(n) = self.n() else {
            let c = Node::new_unbound(self.class_name(), self.builtin_class());
            return Ok(c);
        };
        let mut private = None;
        if self.is_modern() {
            private = Some(if n.is_doc() {
                crate::ns::PrivateData::create()
            } else {
                self.doc_ref()
                    .and_then(|d| d.private_data())
                    .unwrap_or_else(crate::ns::PrivateData::create)
            });
        }
        let cloned = {
            let mut mapper = private.as_ref().map(|p| p.mapper.borrow_mut());
            clone_node_raw(mapper.as_deref_mut(), n, n.doc_ptr(), true)
        };
        let Some(c) = cloned else {
            return Ok(Node::new_unbound(self.class_name(), self.builtin_class()));
        };
        let obj = if c.doc_ptr() == n.doc_ptr() && !c.is_doc() {
            Node::wrap_with_doc(c, self.doc_ref())
        } else {
            let doc = crate::object::DocInner::new(c.doc_ptr());
            if let Some(src) = self.doc_ref() {
                crate::document::copy_document_ref(&src, &doc);
            }
            if c.is_doc() {
                *doc.private.borrow_mut() = private;
            }
            Node::wrap_with_doc(c, Some(doc))
        };
        obj.set_class_name(self.class_name());
        obj.set_builtin_class(self.builtin_class());
        obj.set_family_modern(self.family_is_modern());
        Ok(obj)
    }
}
