//! `DOMDocument`, `Dom\Document`, `Dom\XMLDocument` (`document.c`,
//! `xml_document.c`).

use std::ptr;

use crate::error::{throw, warning, DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::lists::NodeList;
use crate::object::{ClassType, DocInner, DocRef, Handlers, Node};
use crate::parse::{self, ParseFlags, ParseOutcome};
use crate::raw::{c_len, cs, Ns, N};
use crate::tree::{self, NodeOrString};

/// `dom_copy_document_ref`: copies properties, class map and class type.
pub(crate) fn copy_document_ref(src: &DocRef, dst: &DocRef) {
    let props = src.props();
    dst.with_props(|p| {
        p.format_output = props.format_output;
        p.validate_on_parse = props.validate_on_parse;
        p.resolve_externals = props.resolve_externals;
        p.preserve_white_space = props.preserve_white_space;
        p.substitute_entities = props.substitute_entities;
        p.strict_error_checking = props.strict_error_checking;
        p.recover = props.recover;
        if props.classmap.is_some() {
            p.classmap.clone_from(&props.classmap);
        }
    });
    dst.class_type.set(src.class_type.get());
    dst.handlers.set(src.handlers.get());
}

/// `dom_set_xml_class`.
pub(crate) fn set_xml_class(doc: &DocRef) {
    doc.class_type.set(ClassType::Modern);
    doc.handlers.set(Handlers::ModernXml);
}

/// `dom_mark_namespaces_as_attributes_too`.
pub(crate) fn mark_namespaces_as_attributes_too(mapper: &mut crate::ns::NsMapper, doc: N) {
    let mut node = doc.children();
    while let Some(n) = node {
        if n.is_element() {
            crate::ns::mark_attribute_list(mapper, n);
        }
        node = tree::next_in_tree_order(n, None);
    }
}

/// `dom_document_convert_to_modern`.
pub(crate) fn convert_to_modern(doc: &DocRef) {
    let pd = crate::ns::PrivateData::create();
    mark_namespaces_as_attributes_too(&mut pd.mapper.borrow_mut(), doc.n());
    *doc.private.borrow_mut() = Some(pd);
}

fn transfer_ref_single(n: N, new_doc: &DocRef) {
    if let Some(obj) = Node::existing(n) {
        obj.set_doc_ref(Some(new_doc.clone()));
    }
}

fn transfer_ref_single_aux(n: N, new_doc: &DocRef) {
    transfer_ref_single(n, new_doc);
    if n.is_element() {
        for a in n.attr_iter() {
            transfer_ref_single(a, new_doc);
            for c in a.child_iter() {
                transfer_ref_single(c, new_doc);
            }
        }
    }
}

/// `php_dom_transfer_document_ref`.
fn transfer_document_ref(node: N, new_doc: &DocRef) {
    transfer_ref_single_aux(node, new_doc);
    let mut cur = node.children();
    while let Some(c) = cur {
        transfer_ref_single_aux(c, new_doc);
        cur = tree::next_in_tree_order(c, Some(node));
    }
}

fn copy_dicted_string(
    src_dict: *mut ffi::xmlDict,
    dst_dict: *mut ffi::xmlDict,
    s: *const u8,
) -> *const u8 {
    if s.is_null() {
        return s;
    }
    // SAFETY: dictionaries are live; s is a live string.
    unsafe {
        if ffi::xmlDictOwns(src_dict, s) == 1 {
            if dst_dict.is_null() {
                return ffi::xmlStrdup(s);
            }
            return ffi::xmlDictLookup(dst_dict, s, -1);
        }
    }
    s
}

fn fixup_name_and_content(src: *mut ffi::xmlDoc, dst: *mut ffi::xmlDoc, node: N) {
    if node.t() == ffi::XML_ENTITY_REF_NODE {
        node.set_children(None);
    }
    // SAFETY: live documents and node.
    unsafe {
        if !src.is_null() && !(*src).dict.is_null() {
            let dst_dict = if dst.is_null() {
                ptr::null_mut()
            } else {
                (*dst).dict
            };
            (*node.p()).name = copy_dicted_string((*src).dict, dst_dict, (*node.p()).name);
            (*node.p()).content =
                copy_dicted_string((*src).dict, ptr::null_mut(), (*node.p()).content).cast_mut();
        }
    }
}

fn fixup_name_and_content_outer(src: *mut ffi::xmlDoc, dst: *mut ffi::xmlDoc, node: N) {
    fixup_name_and_content(src, dst, node);
    if node.is_element() {
        for a in node.attr_iter() {
            fixup_name_and_content(src, dst, a);
            for c in a.child_iter() {
                fixup_name_and_content(src, dst, c);
            }
        }
    }
    if node.is_element() || node.t() == ffi::XML_ATTRIBUTE_NODE {
        for c in node.child_iter() {
            fixup_name_and_content_outer(src, dst, c);
        }
    }
}

/// `php_dom_adopt_node` given the new document's reference object.
pub(crate) fn adopt_node_raw(
    nodep: N,
    new_doc_ref: Option<&DocRef>,
    new_document: *mut ffi::xmlDoc,
) -> bool {
    let old_doc = nodep.doc_ptr();
    if let Some(old) = DocInner::lookup(old_doc) {
        old.invalidate();
    }
    if old_doc == new_document {
        tree::unlink(nodep);
    } else {
        if let Some(d) = new_doc_ref {
            d.invalidate();
        }
        if new_doc_ref.is_some_and(|d| d.is_modern()) {
            tree::unlink(nodep);
            crate::node::set_tree_doc(nodep, new_document);
            if let Some(pd) = new_doc_ref.and_then(|d| d.private_data()) {
                crate::ns::reconcile_modern(&mut pd.mapper.borrow_mut(), nodep);
            }
        } else {
            // SAFETY: live node and documents.
            let ret = unsafe {
                ffi::xmlDOMWrapAdoptNode(
                    ptr::null_mut(),
                    old_doc,
                    nodep.p(),
                    new_document,
                    ptr::null_mut(),
                    0,
                )
            };
            if ret != 0 {
                return false;
            }
        }
        fixup_name_and_content_outer(old_doc, new_document, nodep);
        if let Some(d) = new_doc_ref {
            transfer_document_ref(nodep, d);
        }
    }
    true
}

/// An options value as PHP's `zend_long`.
fn exceeds_int(v: i64) -> bool {
    v > i64::from(i32::MAX) || v < i64::from(i32::MIN)
}

impl Node {
    fn doc_node(&self) -> DomResult<N> {
        self.node()
    }

    /// `new DOMDocument($version, $encoding)`.
    pub fn construct_document(&self, version: &[u8], encoding: &[u8]) -> DomResult<()> {
        let v = cs(version);
        // SAFETY: valid C string.
        let docp = unsafe { ffi::xmlNewDoc(v.as_ptr().cast()) };
        let d = N::from_doc(docp).ok_or_else(|| DomError::dom(C::InvalidState))?;
        if !encoding.is_empty() {
            d.d().encoding = crate::raw::xml_strdup(encoding);
        }
        let doc = DocInner::new(docp);
        // Detach from a previous document (constructor called twice).
        if let Some(old) = self.n() {
            if old.private() == std::rc::Rc::as_ptr(&self.0).cast_mut().cast() {
                old.set_private(ptr::null_mut());
            }
        }
        self.rebind(d, doc);
        Ok(())
    }

    // ---------------- properties ----------------

    /// `$doctype`.
    pub fn doctype(&self) -> DomResult<Option<Node>> {
        let d = self.prop_node()?;
        // SAFETY: live document.
        Ok(N::from_dtd(unsafe { ffi::xmlGetIntSubset(d.as_doc()) }).map(Node::wrap))
    }

    /// `$documentElement`.
    pub fn document_element(&self) -> DomResult<Option<Node>> {
        let d = self.prop_node()?;
        // SAFETY: live document.
        Ok(N::new(unsafe { ffi::xmlDocGetRootElement(d.as_doc()) }).map(Node::wrap))
    }

    /// `$encoding` / `$xmlEncoding` / `$charset` / ….
    pub fn encoding(&self) -> DomResult<Option<Vec<u8>>> {
        let d = self.prop_node()?;
        Ok(crate::raw::opt_cstr(d.d().encoding).map(<[u8]>::to_vec))
    }

    /// `$actualEncoding` (deprecated).
    pub fn actual_encoding(&self) -> DomResult<Option<Vec<u8>>> {
        crate::error::deprecated("Property DOMDocument::$actualEncoding is deprecated");
        self.encoding()
    }

    /// `$encoding = $value`.
    pub fn set_encoding(&self, value: Option<&[u8]>) -> DomResult<()> {
        let d = self.prop_node()?;
        let invalid = || DomError::Value("Invalid document encoding".into());
        let Some(v) = value else {
            return Err(invalid());
        };
        let c = cs(v);
        // SAFETY: handler lookup and release.
        unsafe {
            let h = ffi::xmlFindCharEncodingHandler(c.as_ptr());
            if h.is_null() {
                return Err(invalid());
            }
            ffi::xmlCharEncCloseFunc(h);
            let doc = d.d();
            if !doc.encoding.is_null() {
                crate::raw::xml_free(doc.encoding.cast_mut().cast());
            }
            doc.encoding = ffi::xmlStrdup(c.as_ptr().cast());
        }
        Ok(())
    }

    /// `$characterSet = $value` (`Dom\Document`).
    pub fn set_character_set(&self, value: &[u8]) -> DomResult<()> {
        crate::html::set_document_encoding(self, value)
    }

    /// `$standalone` / `$xmlStandalone`.
    pub fn standalone(&self) -> DomResult<bool> {
        Ok(self.prop_node()?.d().standalone > 0)
    }

    /// `$standalone = $value`.
    pub fn set_standalone(&self, value: bool) -> DomResult<()> {
        self.prop_node()?.d().standalone = i32::from(value);
        Ok(())
    }

    /// `$version` / `$xmlVersion`.
    pub fn version(&self) -> DomResult<Option<Vec<u8>>> {
        Ok(crate::raw::opt_cstr(self.prop_node()?.d().version).map(<[u8]>::to_vec))
    }

    /// `$version = $value`.
    pub fn set_version(&self, value: Option<&[u8]>) -> DomResult<()> {
        let d = self.prop_node()?;
        let v = value.unwrap_or_default();
        if self.is_modern() && v != b"1.0" && v != b"1.1" {
            return Err(DomError::Value("Invalid XML version".into()));
        }
        let doc = d.d();
        if !doc.version.is_null() {
            crate::raw::xml_free(doc.version.cast_mut().cast());
        }
        doc.version = crate::raw::xml_strdup(v);
        Ok(())
    }

    /// `$documentURI` / `$URL`.
    pub fn document_uri(&self) -> DomResult<Option<Vec<u8>>> {
        let d = self.prop_node()?;
        match crate::raw::opt_cstr(d.d().URL) {
            Some(u) => Ok(Some(u.to_vec())),
            None if self.is_modern() => Ok(Some(b"about:blank".to_vec())),
            None => Ok(None),
        }
    }

    /// `$documentURI = $value`.
    pub fn set_document_uri(&self, value: Option<&[u8]>) -> DomResult<()> {
        let d = self.prop_node()?;
        let doc = d.d();
        if !doc.URL.is_null() {
            crate::raw::xml_free(doc.URL.cast_mut().cast());
        }
        doc.URL = crate::raw::xml_strdup(value.unwrap_or_default());
        Ok(())
    }

    /// `$config` (deprecated, always null).
    pub fn config() {
        crate::error::deprecated("Property DOMDocument::$config is deprecated");
    }

    /// Reads one of the boolean document properties.
    pub fn doc_flag(&self, flag: DocFlag) -> bool {
        let p = self.doc_ref().map(|d| d.props()).unwrap_or_default();
        match flag {
            DocFlag::FormatOutput => p.format_output,
            DocFlag::ValidateOnParse => p.validate_on_parse,
            DocFlag::ResolveExternals => p.resolve_externals,
            DocFlag::PreserveWhiteSpace => p.preserve_white_space,
            DocFlag::Recover => p.recover,
            DocFlag::SubstituteEntities => p.substitute_entities,
            DocFlag::StrictErrorChecking => p.strict_error_checking,
        }
    }

    /// Writes one of the boolean document properties.
    pub fn set_doc_flag(&self, flag: DocFlag, value: bool) {
        if let Some(d) = self.doc_ref() {
            d.with_props(|p| match flag {
                DocFlag::FormatOutput => p.format_output = value,
                DocFlag::ValidateOnParse => p.validate_on_parse = value,
                DocFlag::ResolveExternals => p.resolve_externals = value,
                DocFlag::PreserveWhiteSpace => p.preserve_white_space = value,
                DocFlag::Recover => p.recover = value,
                DocFlag::SubstituteEntities => p.substitute_entities = value,
                DocFlag::StrictErrorChecking => p.strict_error_checking = value,
            });
        }
    }

    fn strict_doc(&self) -> bool {
        self.doc_ref().is_none_or(|d| d.strict())
    }

    // ---------------- factories ----------------

    /// `createElement($localName, $value)`.
    pub fn create_element(&self, name: &[u8], value: Option<&[u8]>) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if !tree::valid_name(name) {
            return Err(throw(
                C::InvalidCharacter,
                if self.is_modern() {
                    true
                } else {
                    self.strict_doc()
                },
            ));
        }
        let node = if self.is_modern() && docp.t() == ffi::XML_HTML_DOCUMENT_NODE {
            let pd = self.private_data_or_err()?;
            let html_ns = pd.mapper.borrow_mut().ensure_html_ns();
            let lower = cs(&name.to_ascii_lowercase());
            // SAFETY: live document.
            N::new(unsafe {
                ffi::xmlNewDocRawNode(docp.as_doc(), html_ns, lower.as_ptr().cast(), ptr::null())
            })
        } else {
            let n = cs(name);
            let v = value.map(cs);
            // SAFETY: live document.
            N::new(unsafe {
                ffi::xmlNewDocNode(
                    docp.as_doc(),
                    ptr::null_mut(),
                    n.as_ptr().cast(),
                    v.as_ref().map_or(ptr::null(), |v| v.as_ptr().cast()),
                )
            })
        };
        let node = node.ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(node, self.doc_ref()))
    }

    pub(crate) fn private_data_or_err(&self) -> DomResult<std::rc::Rc<crate::ns::PrivateData>> {
        self.doc_ref()
            .and_then(|d| d.private_data())
            .ok_or_else(|| DomError::dom(C::InvalidState))
    }

    /// `createDocumentFragment()`.
    pub fn create_document_fragment(&self) -> DomResult<Node> {
        let docp = self.doc_node()?;
        // SAFETY: live document.
        let n = N::new(unsafe { ffi::xmlNewDocFragment(docp.as_doc()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createTextNode($data)`.
    pub fn create_text_node(&self, data: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        let c = cs(data);
        // SAFETY: live document.
        let n = N::new(unsafe { ffi::xmlNewDocText(docp.as_doc(), c.as_ptr().cast()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createComment($data)`.
    pub fn create_comment(&self, data: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        let c = cs(data);
        // SAFETY: live document.
        let n = N::new(unsafe { ffi::xmlNewDocComment(docp.as_doc(), c.as_ptr().cast()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createCDATASection($data)`.
    pub fn create_cdata_section(&self, data: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if self.is_modern() {
            if docp.t() == ffi::XML_HTML_DOCUMENT_NODE {
                return Err(DomError::dom_msg(
                    C::NotSupported,
                    "This operation is not supported for HTML documents",
                ));
            }
            if data.windows(3).any(|w| w == b"]]>") {
                return Err(DomError::dom_msg(
                    C::InvalidCharacter,
                    "Invalid character sequence \"]]>\" in CDATA section",
                ));
            }
        }
        // SAFETY: live document; data valid for its length.
        let n = N::new(unsafe {
            ffi::xmlNewCDataBlock(docp.as_doc(), data.as_ptr(), c_len(data.len()))
        })
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createProcessingInstruction($target, $data)`.
    pub fn create_processing_instruction(
        &self,
        target: &[u8],
        data: Option<&[u8]>,
    ) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if !tree::valid_name(target) {
            return Err(throw(C::InvalidCharacter, self.strict_doc()));
        }
        if self.is_modern() && data.is_some_and(|d| d.windows(2).any(|w| w == b"?>")) {
            return Err(DomError::dom_msg(
                C::InvalidCharacter,
                "Invalid character sequence \"?>\" in processing instruction",
            ));
        }
        let t = cs(target);
        let d = data.map(cs);
        // SAFETY: live document.
        let n = N::new(unsafe {
            ffi::xmlNewDocPI(
                docp.as_doc(),
                t.as_ptr().cast(),
                d.as_ref().map_or(ptr::null(), |d| d.as_ptr().cast()),
            )
        })
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createAttribute($localName)`.
    pub fn create_attribute(&self, name: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if !tree::valid_name(name) {
            return Err(throw(C::InvalidCharacter, self.strict_doc()));
        }
        let nm = if docp.t() == ffi::XML_HTML_DOCUMENT_NODE && self.is_modern() {
            name.to_ascii_lowercase()
        } else {
            name.to_vec()
        };
        let c = cs(&nm);
        // SAFETY: live document.
        let n = N::from_attr(unsafe {
            ffi::xmlNewDocProp(docp.as_doc(), c.as_ptr().cast(), ptr::null())
        })
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `createEntityReference($name)`.
    pub fn create_entity_reference(&self, name: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if !tree::valid_name(name) {
            return Err(throw(C::InvalidCharacter, self.strict_doc()));
        }
        let c = cs(name);
        // SAFETY: live document.
        let n = N::new(unsafe { ffi::xmlNewReference(docp.as_doc(), c.as_ptr().cast()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap_with_doc(n, self.doc_ref()))
    }

    /// `importNode($node, $deep)` / `importLegacyNode()`.
    pub fn import_node(&self, node: &Node, deep: bool) -> DomResult<Node> {
        let docp = self.doc_node()?;
        let nodep = node.node()?;
        let modern = self.is_modern();
        if nodep.is_doc() {
            if modern {
                return Err(DomError::dom(C::NotSupported));
            }
            warning("Cannot import: Node Type Not Supported");
            return Err(DomError::Silent);
        }
        if nodep.doc_ptr() == docp.as_doc() {
            return Ok(Node::wrap_with_doc(nodep, self.doc_ref()));
        }
        if modern {
            let pd = self.private_data_or_err()?;
            let ret = crate::node::clone_node_raw(
                Some(&mut pd.mapper.borrow_mut()),
                nodep,
                docp.as_doc(),
                deep,
            )
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
            return Ok(Node::wrap_with_doc(ret, self.doc_ref()));
        }
        let ret = crate::node::clone_node_raw(None, nodep, docp.as_doc(), deep)
            .ok_or(DomError::Silent)?;
        if ret.t() == ffi::XML_ATTRIBUTE_NODE {
            if let (Some(nns), None) = (nodep.ns(), ret.ns()) {
                // SAFETY: live document.
                let root = N::new(unsafe { ffi::xmlDocGetRootElement(docp.as_doc()) });
                // SAFETY: live document; root may be NULL.
                let mut nsptr = unsafe {
                    ffi::xmlSearchNsByHref(docp.as_doc(), crate::raw::raw(root), nns.href_ptr())
                };
                if Ns::new(nsptr).is_none_or(|x| x.prefix_ptr().is_null()) {
                    nsptr = if let Some(r) = root {
                        tree::get_ns(r, nns.href(), nns.prefix()).unwrap_or(ptr::null_mut())
                    } else {
                        // dom_get_ns(NULL, ...): xmlNewNs(NULL, ...) creates a free-standing ns.
                        let p = nns.prefix();
                        let bad = p.is_some_and(|p| {
                            (p == b"xml" && nns.href() != ffi::XML_XML_NAMESPACE)
                                || (p == b"xmlns" && nns.href() != crate::ns::XMLNS_NS)
                                || (nns.href() == crate::ns::XMLNS_NS && p != b"xmlns")
                        });
                        if bad {
                            ptr::null_mut()
                        } else {
                            // SAFETY: free-standing namespace record.
                            let ns = unsafe {
                                ffi::xmlNewNs(ptr::null_mut(), nns.href_ptr(), nns.prefix_ptr())
                            };
                            if !ns.is_null() {
                                crate::object::set_old_ns(docp.as_doc(), ns);
                            }
                            ns
                        }
                    };
                }
                ret.set_ns(nsptr);
            }
        }
        Ok(Node::wrap_with_doc(ret, self.doc_ref()))
    }

    /// `createElementNS($namespace, $qualifiedName, $value)`.
    pub fn create_element_ns(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
        value: Option<&[u8]>,
    ) -> DomResult<Node> {
        let docp = self.doc_node()?;
        if self.is_modern() {
            let (local, prefix) =
                tree::validate_and_extract(uri, name).map_err(|c| throw(c, self.strict_doc()))?;
            let pd = self.private_data_or_err()?;
            let ns = pd.mapper.borrow_mut().get_ns(prefix.as_deref(), uri);
            let l = cs(&local);
            // SAFETY: live document; interned (or duplicated) local name.
            let n = unsafe {
                let dict = docp.d().dict;
                let interned = if dict.is_null() {
                    ptr::null()
                } else {
                    ffi::xmlDictLookup(dict, l.as_ptr().cast(), -1)
                };
                if interned.is_null() {
                    ffi::xmlNewDocNode(docp.as_doc(), ns, l.as_ptr().cast(), ptr::null())
                } else {
                    ffi::xmlNewDocRawNode(docp.as_doc(), ns, interned, ptr::null())
                }
            };
            let n = N::new(n).ok_or_else(|| DomError::dom(C::InvalidState))?;
            return Ok(Node::wrap_with_doc(n, self.doc_ref()));
        }
        let uri_len = uri.map_or(0, <[u8]>::len);
        let mut errorcode = None;
        let mut nodep = None;
        match tree::check_qname(name, uri_len) {
            Err(e) => errorcode = Some(e),
            Ok((local, prefix)) => {
                if tree::valid_name(&local) {
                    let l = cs(&local);
                    let v = value.map(cs);
                    // SAFETY: live document.
                    let n = N::new(unsafe {
                        ffi::xmlNewDocNode(
                            docp.as_doc(),
                            ptr::null_mut(),
                            l.as_ptr().cast(),
                            v.as_ref().map_or(ptr::null(), |v| v.as_ptr().cast()),
                        )
                    })
                    .ok_or_else(|| DomError::dom(C::InvalidState))?;
                    if let Some(u) = uri {
                        let uc = cs(u);
                        // SAFETY: live node.
                        let mut nsptr = unsafe {
                            ffi::xmlSearchNsByHref(n.doc_ptr(), n.p(), uc.as_ptr().cast())
                        };
                        if nsptr.is_null() {
                            match tree::get_ns(n, u, prefix.as_deref()) {
                                Ok(p) => nsptr = p,
                                Err(()) => errorcode = Some(C::Namespace),
                            }
                        }
                        n.set_ns(nsptr);
                    }
                    nodep = Some(n);
                } else {
                    errorcode = Some(C::InvalidCharacter);
                }
            }
        }
        if let Some(e) = errorcode {
            if let Some(n) = nodep {
                tree::free_node(n);
            }
            return Err(throw(e, self.strict_doc()));
        }
        Ok(Node::wrap_with_doc(
            nodep.expect("created above"),
            self.doc_ref(),
        ))
    }

    /// `createAttributeNS($namespace, $qualifiedName)`.
    pub fn create_attribute_ns(&self, uri: Option<&[u8]>, name: &[u8]) -> DomResult<Node> {
        let docp = self.doc_node()?;
        let modern = self.is_modern();
        // SAFETY: live document.
        let root = N::new(unsafe { ffi::xmlDocGetRootElement(docp.as_doc()) });
        if root.is_none() && !modern {
            warning("Document Missing Root Element");
            return Err(DomError::Silent);
        }
        let (local, prefix) = match tree::validate_and_extract(uri, name) {
            Ok(v) => v,
            Err(e) => {
                let code = if modern { e } else { C::Namespace };
                return Err(throw(code, self.strict_doc()));
            }
        };
        let l = cs(&local);
        // SAFETY: live document.
        let nodep = N::from_attr(unsafe {
            ffi::xmlNewDocProp(docp.as_doc(), l.as_ptr().cast(), ptr::null())
        })
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
        let mut errorcode = None;
        if let Some(u) = uri.filter(|u| !u.is_empty()) {
            let nsptr = if modern {
                let pd = self.private_data_or_err()?;
                let p = pd.mapper.borrow_mut().get_ns(prefix.as_deref(), Some(u));
                p
            } else {
                let uc = cs(u);
                // SAFETY: live document; root may be NULL.
                let mut nsptr = unsafe {
                    ffi::xmlSearchNsByHref(docp.as_doc(), crate::raw::raw(root), uc.as_ptr().cast())
                };
                if name == b"xmlns" || prefix.as_deref() == Some(b"xml") {
                    if nsptr.is_null() {
                        let p = prefix.as_deref().map(cs);
                        // SAFETY: free-standing namespace moved to oldNs.
                        nsptr = unsafe {
                            ffi::xmlNewNs(
                                ptr::null_mut(),
                                uc.as_ptr().cast(),
                                p.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
                            )
                        };
                        crate::object::set_old_ns(docp.as_doc(), nsptr);
                    }
                } else if Ns::new(nsptr).is_none_or(|x| x.prefix_ptr().is_null()) {
                    let pfx = prefix.clone().unwrap_or_else(|| b"default".to_vec());
                    nsptr = tree::get_ns_unchecked(root.expect("checked above"), u, Some(&pfx));
                    if nsptr.is_null() {
                        errorcode = Some(C::Namespace);
                    }
                }
                nsptr
            };
            nodep.set_ns(nsptr);
        }
        if let Some(e) = errorcode {
            // SAFETY: fresh handle-less attribute.
            unsafe { ffi::xmlFreeProp(nodep.attr()) };
            return Err(throw(e, self.strict_doc()));
        }
        Ok(Node::wrap_with_doc(nodep, self.doc_ref()))
    }

    /// `getElementById($elementId)`.
    pub fn get_element_by_id(&self, id: &[u8]) -> DomResult<Option<Node>> {
        let docp = self.doc_node()?;
        let modified = self.doc_ref().is_none_or(|d| d.modified_since_parsing());
        if !modified {
            let c = cs(id);
            // SAFETY: live document.
            let attr = N::from_attr(unsafe { ffi::xmlGetID(docp.as_doc(), c.as_ptr().cast()) });
            if let Some(p) = attr.and_then(N::parent) {
                return Ok(Some(Node::wrap(p)));
            }
            return Ok(None);
        }
        let mut node = docp.children();
        while let Some(n) = node {
            if n.is_element() {
                for a in n.attr_iter() {
                    if a.atype() == ffi::XML_ATTRIBUTE_ID && tree::attr_value(a) == id {
                        return Ok(Some(Node::wrap(n)));
                    }
                }
            }
            node = tree::next_in_tree_order(n, Some(docp));
        }
        Ok(None)
    }

    /// `adoptNode($node)`.
    pub fn adopt_node(&self, node: &Node) -> DomResult<Node> {
        let nodep = node.node()?;
        if matches!(
            nodep.t(),
            ffi::XML_DOCUMENT_NODE
                | ffi::XML_HTML_DOCUMENT_NODE
                | ffi::XML_DOCUMENT_TYPE_NODE
                | ffi::XML_DTD_NODE
                | ffi::XML_ENTITY_NODE
                | ffi::XML_NOTATION_NODE
        ) {
            return Err(throw(
                C::NotSupported,
                node.doc_ref().is_none_or(|d| d.strict()),
            ));
        }
        let docp = self.doc_node()?;
        if !adopt_node_raw(nodep, self.doc_ref().as_ref(), docp.as_doc()) {
            if self.is_modern() {
                return Err(DomError::dom(C::InvalidState));
            }
            return Err(DomError::Silent);
        }
        Ok(node.clone())
    }

    /// `normalizeDocument()`.
    pub fn normalize_document(&self) -> DomResult<()> {
        let d = self.doc_node()?;
        let _scope = crate::stack::StackScope::enter();
        crate::node::normalize_legacy(d);
        crate::stack::take_error().map_or(Ok(()), Err)
    }

    fn parse_flags(&self, options: i64) -> ParseFlags {
        let p = self.doc_ref().map(|d| d.props()).unwrap_or_default();
        ParseFlags {
            validate: p.validate_on_parse,
            resolve_externals: p.resolve_externals,
            keep_blanks: p.preserve_white_space,
            substitute_entities: p.substitute_entities,
            recover: p.recover || (options & i64::from(ffi::XML_PARSE_RECOVER)) != 0,
        }
    }

    /// `php_dom_finish_loading_document`.
    fn finish_loading(&self, newdoc: *mut ffi::xmlDoc) -> bool {
        let Some(nd) = N::from_doc(newdoc) else {
            return false;
        };
        let mut class_type = ClassType::Legacy;
        let mut old_nr = 0;
        let mut props = None;
        if let Some(old) = self.doc_ref() {
            class_type = old.class_type.get();
            old_nr = old.modification_nr.get();
            props = old.props.borrow_mut().take();
        }
        let doc = DocInner::new(newdoc);
        *doc.props.borrow_mut() = props;
        doc.class_type.set(class_type);
        if let Some(old) = self.n() {
            if old.private() == std::rc::Rc::as_ptr(&self.0).cast_mut().cast() {
                old.set_private(ptr::null_mut());
            }
        }
        self.rebind(nd, doc.clone());
        if old_nr != 0 {
            doc.modification_nr.set(old_nr);
            doc.invalidate();
        }
        true
    }

    fn parse_document(&self, from_file: bool, source: &[u8], options: i64) -> DomResult<bool> {
        if source.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        if source.len() > i32::MAX as usize {
            warning("Input string is too long");
            return Ok(false);
        }
        if exceeds_int(options) {
            warning("Invalid options");
            return Ok(false);
        }
        if from_file && source.contains(&0) {
            return Err(DomError::arg_value(1, "must not contain any null bytes"));
        }
        #[allow(clippy::cast_possible_truncation)]
        let opts = options as i32;
        let flags = self.parse_flags(options);
        let newdoc = match parse::document_parser(from_file, source, opts, flags, None) {
            ParseOutcome::Doc(d) => d,
            ParseOutcome::Failed | ParseOutcome::Malformed => ptr::null_mut(),
        };
        Ok(self.finish_loading(newdoc))
    }

    /// `load($filename, $options)`.
    pub fn load(&self, filename: &[u8], options: i64) -> DomResult<bool> {
        self.parse_document(true, filename, options)
    }

    /// `loadXML($source, $options)`.
    pub fn load_xml(&self, source: &[u8], options: i64) -> DomResult<bool> {
        self.parse_document(false, source, options)
    }

    /// `loadHTML($source, $options)` / `loadHTMLFile()`.
    pub fn load_html(&self, source: &[u8], options: i64, from_file: bool) -> DomResult<bool> {
        if source.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        if exceeds_int(options) {
            warning("Invalid options");
            return Ok(false);
        }
        if from_file {
            if source.contains(&0) {
                return Err(DomError::arg_value(1, "must not contain any null bytes"));
            }
        } else if source.len() > i32::MAX as usize {
            warning("Input string is too long");
            return Ok(false);
        }
        #[allow(clippy::cast_possible_truncation)]
        let doc = parse::load_html(from_file, source, options as i32);
        Ok(self.finish_loading(doc))
    }

    /// `save($filename, $options)`: bytes written or `None` (false).
    pub fn save(&self, filename: &[u8], options: i64) -> DomResult<Option<i64>> {
        if filename.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        if filename.contains(&0) {
            return Err(DomError::arg_value(1, "must not contain any null bytes"));
        }
        let docp = self.doc_node()?;
        let format = self.doc_flag(DocFlag::FormatOutput);
        let noempty = options & i64::from(crate::constants::LIBXML_SAVE_NOEMPTYTAG) != 0;
        let handlers = self
            .doc_ref()
            .map_or(Handlers::Default, |d| d.handlers.get());
        let bytes = crate::save::with_no_empty_tags(noempty, || {
            crate::save::dump_doc_to_file(handlers, filename, docp, format)
        })?;
        Ok(if bytes == -1 { None } else { Some(bytes) })
    }

    /// `saveXML($node, $options)` / `saveXml()`.
    pub fn save_xml(&self, node: Option<&Node>, options: i64) -> DomResult<Option<Vec<u8>>> {
        let docp = self.doc_node()?;
        let format = self.doc_flag(DocFlag::FormatOutput);
        let handlers = self
            .doc_ref()
            .map_or(Handlers::Default, |d| d.handlers.get());
        let noempty = options & i64::from(crate::constants::LIBXML_SAVE_NOEMPTYTAG) != 0;
        let res = if let Some(node) = node {
            let n = node.node()?;
            if n.doc_ptr() != docp.as_doc() {
                return Err(throw(C::WrongDocument, self.strict_doc()));
            }
            crate::save::with_no_empty_tags(noempty, || {
                crate::save::dump_node_to_str(handlers, docp, n, format)
            })?
        } else {
            let mut converted = ffi::XML_SAVE_AS_XML;
            #[allow(clippy::cast_possible_truncation)]
            if options as i32 & ffi::XML_SAVE_NO_DECL != 0 {
                converted |= ffi::XML_SAVE_NO_DECL;
            }
            if format {
                converted |= ffi::XML_SAVE_FORMAT;
            }
            crate::save::with_no_empty_tags(noempty, || {
                crate::save::dump_doc_to_str(handlers, docp, converted)
            })?
        };
        if let Some(s) = res {
            Ok(Some(s))
        } else {
            warning("Could not save document");
            Ok(None)
        }
    }

    /// `xinclude($options)`: number of substitutions; legacy returns
    /// `None` (false) when zero.
    pub fn xinclude(&self, flags: i64) -> DomResult<Option<i64>> {
        if exceeds_int(flags) {
            if self.is_modern() {
                return Err(DomError::arg_value(1, "is too large"));
            }
            warning("Invalid flags");
            return Ok(None);
        }
        let docp = self.doc_node()?;
        strip_xinclude_fallback_references(docp);
        #[allow(clippy::cast_possible_truncation)]
        let flags = flags as i32 | ffi::XML_PARSE_NOXINCNODE;
        // SAFETY: live document.
        let err = parse::with_generic_errors(|| unsafe {
            ffi::xmlXIncludeProcessFlags(docp.as_doc(), flags)
        });
        if let Some(d) = self.doc_ref() {
            d.invalidate();
        }
        if self.is_modern() {
            if err < 0 {
                return Err(DomError::dom(C::InvalidModification));
            }
            return Ok(Some(i64::from(err)));
        }
        Ok(if err != 0 { Some(i64::from(err)) } else { None })
    }

    #[allow(clippy::unused_self)]
    fn with_ns_guard<R>(&self, docp: N, f: impl FnOnce() -> R) -> R {
        let active = crate::lists::follow_spec_node(docp);
        let mut links = crate::ns::NsLinks::default();
        if active {
            // SAFETY: live document.
            if let Some(root) = N::new(unsafe { ffi::xmlDocGetRootElement(docp.as_doc()) }) {
                links.relink(root);
            }
        }
        let r = f();
        if active {
            links.unlink();
        }
        r
    }

    /// `validate()`.
    pub fn validate(&self) -> DomResult<bool> {
        let docp = self.doc_node()?;
        let ok = parse::with_generic_errors(|| {
            // SAFETY: fresh validation context; freed below.
            unsafe {
                let cvp = ffi::xmlNewValidCtxt();
                let r = self.with_ns_guard(docp, || ffi::xmlValidateDocument(cvp, docp.as_doc()));
                ffi::xmlFreeValidCtxt(cvp);
                r
            }
        });
        Ok(ok != 0)
    }

    /// `schemaValidate($filename, $flags)` / `schemaValidateSource()`.
    pub fn schema_validate(&self, source: &[u8], flags: i64, from_file: bool) -> DomResult<bool> {
        if source.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        let docp = self.doc_node()?;
        let scope = parse::ErrorScope::new();
        // SAFETY: schema parser/validator objects are created and freed here.
        unsafe {
            let parser = if from_file {
                if source.contains(&0) {
                    return Err(DomError::arg_value(1, "must not contain any null bytes"));
                }
                let Some(file) = parse::valid_file_path(source) else {
                    drop(scope);
                    warning("Invalid Schema file source");
                    return Ok(false);
                };
                let c = cs(&file);
                ffi::xmlSchemaNewParserCtxt(c.as_ptr())
            } else {
                ffi::xmlSchemaNewMemParserCtxt(source.as_ptr().cast(), c_len(source.len()))
            };
            let sptr = ffi::xmlSchemaParse(parser);
            ffi::xmlSchemaFreeParserCtxt(parser);
            if sptr.is_null() {
                drop(scope);
                warning("Invalid Schema");
                return Ok(false);
            }
            let vptr = ffi::xmlSchemaNewValidCtxt(sptr);
            if vptr.is_null() {
                ffi::xmlSchemaFree(sptr);
                return Err(DomError::Error("Invalid Schema Validation Context".into()));
            }
            let mut opts = 0;
            if flags & i64::from(ffi::XML_SCHEMA_VAL_VC_I_CREATE) != 0 {
                opts |= ffi::XML_SCHEMA_VAL_VC_I_CREATE;
            }
            ffi::xmlSchemaSetValidOptions(vptr, opts);
            let valid = self.with_ns_guard(docp, || ffi::xmlSchemaValidateDoc(vptr, docp.as_doc()));
            ffi::xmlSchemaFree(sptr);
            ffi::xmlSchemaFreeValidCtxt(vptr);
            drop(scope);
            Ok(valid == 0)
        }
    }

    /// `relaxNGValidate($filename)` / `relaxNGValidateSource()`.
    pub fn relaxng_validate(&self, source: &[u8], from_file: bool) -> DomResult<bool> {
        if source.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        let docp = self.doc_node()?;
        let scope = parse::ErrorScope::new();
        // SAFETY: RelaxNG parser/validator objects are created and freed here.
        unsafe {
            let parser = if from_file {
                if source.contains(&0) {
                    return Err(DomError::arg_value(1, "must not contain any null bytes"));
                }
                let Some(file) = parse::valid_file_path(source) else {
                    drop(scope);
                    warning("Invalid RelaxNG file source");
                    return Ok(false);
                };
                let c = cs(&file);
                ffi::xmlRelaxNGNewParserCtxt(c.as_ptr())
            } else {
                ffi::xmlRelaxNGNewMemParserCtxt(source.as_ptr().cast(), c_len(source.len()))
            };
            let sptr = ffi::xmlRelaxNGParse(parser);
            ffi::xmlRelaxNGFreeParserCtxt(parser);
            if sptr.is_null() {
                drop(scope);
                warning("Invalid RelaxNG");
                return Ok(false);
            }
            let vptr = ffi::xmlRelaxNGNewValidCtxt(sptr);
            if vptr.is_null() {
                ffi::xmlRelaxNGFree(sptr);
                return Err(DomError::Error("Invalid RelaxNG Validation Context".into()));
            }
            let valid =
                self.with_ns_guard(docp, || ffi::xmlRelaxNGValidateDoc(vptr, docp.as_doc()));
            ffi::xmlRelaxNGFree(sptr);
            ffi::xmlRelaxNGFreeValidCtxt(vptr);
            drop(scope);
            Ok(valid == 0)
        }
    }

    /// `saveHTMLFile($filename)`.
    pub fn save_html_file(&self, filename: &[u8]) -> DomResult<Option<i64>> {
        if filename.is_empty() {
            return Err(DomError::arg_value(1, "must not be empty"));
        }
        let docp = self.doc_node()?;
        let format = self.doc_flag(DocFlag::FormatOutput);
        let bytes = crate::save::html_save_file(docp, filename, format);
        Ok(if bytes == -1 { None } else { Some(bytes) })
    }

    /// `saveHTML($node)` (legacy).
    pub fn save_html(&self, node: Option<&Node>) -> DomResult<Option<Vec<u8>>> {
        let docp = self.doc_node()?;
        if let Some(d) = self.doc_ref() {
            d.with_props(|_| ());
        }
        let format = self.doc_flag(DocFlag::FormatOutput);
        if let Some(node) = node {
            let n = node.node()?;
            if n.doc_ptr() != docp.as_doc() {
                return Err(throw(C::WrongDocument, self.strict_doc()));
            }
            return Ok(crate::save::html_dump_node(docp, n, format));
        }
        Ok(crate::save::html_dump_doc(docp, format))
    }

    /// `registerNodeClass($baseClass, $extendedClass)`. The embedder
    /// checks the class relationship (`host.is_subclass_of`).
    pub fn register_node_class(
        &self,
        base: &str,
        extended: Option<&str>,
        host: &dyn crate::host::DomHost,
    ) -> DomResult<()> {
        let modern = self.family_is_modern();
        let root = if modern { "Dom\\Node" } else { "DOMNode" };
        if !host.is_subclass_of(base, root) {
            return Err(DomError::arg_type(
                1,
                format!("must be a class name derived from {root}, \"{base}\" given"),
            ));
        }
        if host.is_abstract(base) {
            return Err(DomError::arg_value(1, "must not be an abstract class"));
        }
        let base_canon = host
            .canonical_name(base)
            .unwrap_or_else(|| base.to_string());
        match extended {
            None => {
                if let Some(d) = self.doc_ref() {
                    d.with_props(|p| {
                        if let Some(map) = p.classmap.as_mut() {
                            map.retain(|(b, _)| !b.eq_ignore_ascii_case(&base_canon));
                        }
                    });
                }
                Ok(())
            }
            Some(ext) => {
                if !host.is_subclass_of(ext, base) {
                    let ext_canon = host.canonical_name(ext).unwrap_or_else(|| ext.to_string());
                    return Err(DomError::ArgType {
                        arg: 2,
                        message: format!("must be a class name derived from {base_canon} or null, {ext_canon} given"),
                    });
                }
                if host.is_abstract(ext) {
                    return Err(DomError::arg_value(2, "must not be an abstract class"));
                }
                let ext_canon = host.canonical_name(ext).unwrap_or_else(|| ext.to_string());
                self.node()?;
                if let Some(d) = self.doc_ref() {
                    d.with_props(|p| {
                        let map = p.classmap.get_or_insert_with(Vec::new);
                        if let Some(e) = map
                            .iter_mut()
                            .find(|(b, _)| b.eq_ignore_ascii_case(&base_canon))
                        {
                            e.1 = ext_canon;
                        } else {
                            map.push((base_canon, ext_canon));
                        }
                    });
                }
                Ok(())
            }
        }
    }

    /// `getElementsByTagName()` on a document.
    pub fn doc_get_elements_by_tag_name(&self, name: &[u8]) -> DomResult<NodeList> {
        self.get_elements_by_tag_name(name)
    }

    /// `append()` etc. on documents and fragments share the element code.
    pub fn doc_replace_children(&self, nodes: &[NodeOrString]) -> DomResult<()> {
        self.replace_children(nodes)
    }
}

/// The boolean `DOMDocument` properties stored on the document reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFlag {
    /// `formatOutput`.
    FormatOutput,
    /// `validateOnParse`.
    ValidateOnParse,
    /// `resolveExternals`.
    ResolveExternals,
    /// `preserveWhiteSpace`.
    PreserveWhiteSpace,
    /// `recover`.
    Recover,
    /// `substituteEntities`.
    SubstituteEntities,
    /// `strictErrorChecking`.
    StrictErrorChecking,
}

const XINCLUDE_NS: &[u8] = b"http://www.w3.org/2001/XInclude";
const XINCLUDE_OLD_NS: &[u8] = b"http://www.w3.org/2003/XInclude";

fn strip_references_for_attributes(basep: N) {
    let attrs: Vec<N> = basep.attr_iter().collect();
    for prop in attrs {
        crate::object::free_resource(prop);
        let kids: Vec<N> = prop.child_iter().collect();
        for c in kids {
            crate::object::free_resource(c);
        }
    }
}

fn strip_references(basep: N) {
    crate::object::free_resource(basep);
    strip_references_for_attributes(basep);
    let mut cur = basep.children();
    while let Some(c) = cur {
        crate::object::free_resource(c);
        if c.is_element() {
            strip_references_for_attributes(c);
        }
        cur = tree::next_in_tree_order(c, Some(basep));
    }
}

/// `dom_xinclude_strip_fallback_references`.
fn strip_xinclude_fallback_references(basep: N) {
    let mut cur = basep.children();
    while let Some(c) = cur {
        if c.is_element() && c.name() == b"include" {
            if let Some(ns) = c.ns() {
                if ns.href() == XINCLUDE_NS || ns.href() == XINCLUDE_OLD_NS {
                    strip_references(c);
                }
            }
        }
        cur = tree::next_in_tree_order(c, Some(basep));
    }
}

/// Creates a modern document object for a fresh libxml2 document.
pub(crate) fn instantiate_modern(doc: *mut ffi::xmlDoc) -> (Node, DocRef) {
    let d = DocInner::new(doc);
    set_xml_class(&d);
    let n = N::from_doc(doc).expect("non-null document");
    let obj = Node::wrap_with_doc(n, Some(d.clone()));
    (obj, d)
}

/// `Dom\XMLDocument::createEmpty($version, $encoding)`.
pub fn xml_document_create_empty(version: &[u8], encoding: &[u8]) -> DomResult<Node> {
    php_libxml::raw::ensure_init();
    let e = cs(encoding);
    // SAFETY: handler lookup and release.
    unsafe {
        let h = ffi::xmlFindCharEncodingHandler(e.as_ptr());
        if h.is_null() {
            return Err(DomError::arg_value(2, "is not a valid document encoding"));
        }
        ffi::xmlCharEncCloseFunc(h);
    }
    let v = cs(version);
    // SAFETY: valid C string.
    let doc = unsafe { ffi::xmlNewDoc(v.as_ptr().cast()) };
    let dn = N::from_doc(doc).ok_or_else(|| DomError::dom(C::InvalidState))?;
    dn.d().encoding = crate::raw::xml_strdup(encoding);
    let (obj, d) = instantiate_modern(doc);
    *d.private.borrow_mut() = Some(crate::ns::PrivateData::create());
    Ok(obj)
}

const VALID_XML_OPTIONS: i64 = (ffi::XML_PARSE_RECOVER
    | ffi::XML_PARSE_NOENT
    | ffi::XML_PARSE_DTDLOAD
    | ffi::XML_PARSE_DTDATTR
    | ffi::XML_PARSE_DTDVALID
    | ffi::XML_PARSE_NOERROR
    | ffi::XML_PARSE_NOWARNING
    | ffi::XML_PARSE_NOBLANKS
    | ffi::XML_PARSE_NSCLEAN
    | ffi::XML_PARSE_NOCDATA
    | ffi::XML_PARSE_NONET
    | ffi::XML_PARSE_PEDANTIC
    | ffi::XML_PARSE_COMPACT
    | ffi::XML_PARSE_HUGE
    | ffi::XML_PARSE_BIG_LINES) as i64;

/// `Dom\XMLDocument::createFromString()` / `createFromFile()`.
pub fn xml_document_create_from(
    source: &[u8],
    options: i64,
    override_encoding: Option<&[u8]>,
    from_file: bool,
) -> DomResult<Node> {
    php_libxml::raw::ensure_init();
    if source.is_empty() {
        return Err(DomError::arg_value(1, "must not be empty"));
    }
    if source.len() > i32::MAX as usize {
        return Err(DomError::arg_value(1, "is too long"));
    }
    if from_file && source.windows(3).any(|w| w == b"%00") {
        return Err(DomError::arg_value(
            1,
            "must not contain percent-encoded NUL bytes",
        ));
    }
    if from_file && source.contains(&0) {
        return Err(DomError::arg_value(1, "must not contain any null bytes"));
    }
    if options & !VALID_XML_OPTIONS != 0 {
        return Err(DomError::arg_value(
            2,
            "contains invalid flags (allowed flags: LIBXML_RECOVER, LIBXML_NOENT, LIBXML_DTDLOAD, LIBXML_DTDATTR, LIBXML_DTDVALID, LIBXML_NOERROR, LIBXML_NOWARNING, LIBXML_NOBLANKS, LIBXML_NSCLEAN, LIBXML_NOCDATA, LIBXML_NONET, LIBXML_PEDANTIC, LIBXML_COMPACT, LIBXML_PARSEHUGE, LIBXML_BIGLINES)",
        ));
    }
    #[allow(clippy::cast_possible_truncation)]
    let mut opts = options as i32;
    if let Some(enc) = override_encoding {
        let e = cs(enc);
        // SAFETY: handler lookup and release.
        unsafe {
            let h = ffi::xmlFindCharEncodingHandler(e.as_ptr());
            if h.is_null() {
                return Err(DomError::arg_value(3, "must be a valid document encoding"));
            }
            ffi::xmlCharEncCloseFunc(h);
        }
        opts |= 1 << 21; // XML_PARSE_IGNORE_ENC
    }
    let flags = ParseFlags {
        keep_blanks: true,
        recover: opts & ffi::XML_PARSE_RECOVER != 0,
        ..ParseFlags::default()
    };
    let doc = match parse::document_parser(from_file, source, opts, flags, override_encoding) {
        ParseOutcome::Doc(d) => d,
        ParseOutcome::Malformed => {
            return Err(DomError::dom_msg(
                C::Syntax,
                "XML fragment is not well-formed",
            ));
        }
        ParseOutcome::Failed => {
            if from_file {
                return Err(DomError::Exception(format!(
                    "Cannot open file '{}'",
                    String::from_utf8_lossy(source)
                )));
            }
            return Err(DomError::dom(C::InvalidState));
        }
    };
    let dn = N::from_doc(doc).expect("non-null document");
    if dn.d().encoding.is_null() {
        dn.d().encoding = crate::raw::xml_strdup(override_encoding.unwrap_or(b"UTF-8"));
    }
    if from_file {
        let url = dn.d().URL;
        if let Some(u) = crate::raw::opt_cstr(url) {
            let is_stream = u.windows(3).position(|w| w == b"://").is_some_and(|p| {
                u[..p]
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
            }) || u.starts_with(b"data:");
            if !is_stream && !u.starts_with(b"file:/") {
                let mut v = b"file://".to_vec();
                v.extend_from_slice(u);
                crate::raw::xml_free(url.cast_mut().cast());
                dn.d().URL = crate::raw::xml_strdup(&v);
            }
        }
    }
    let (obj, d) = instantiate_modern(doc);
    convert_to_modern(&d);
    Ok(obj)
}
