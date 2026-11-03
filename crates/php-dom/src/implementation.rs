//! `DOMImplementation` / `Dom\Implementation` (`domimplementation.c`).

use std::ptr;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::object::{DocInner, Node};
use crate::raw::{cs, N};

/// A `DOMImplementation` (`modern == false`) or `Dom\Implementation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    /// Whether this is `Dom\Implementation`.
    pub modern: bool,
    token: std::rc::Rc<()>,
}

impl Implementation {
    /// A new implementation object.
    pub fn new(modern: bool) -> Self {
        Implementation {
            modern,
            token: std::rc::Rc::new(()),
        }
    }

    /// A stable identity: `Dom\Document::$implementation` returns the same
    /// object on every read.
    pub fn identity(&self) -> usize {
        std::rc::Rc::as_ptr(&self.token) as usize
    }

    /// The PHP class.
    pub fn class_name(&self) -> &'static str {
        if self.modern {
            "Dom\\Implementation"
        } else {
            "DOMImplementation"
        }
    }

    /// `hasFeature($feature, $version)`.
    pub fn has_feature(feature: &[u8], version: &[u8]) -> bool {
        crate::node::has_feature(feature, version)
    }

    /// `createDocumentType($qualifiedName, $publicId, $systemId)`; legacy
    /// `Err(Silent)` = false.
    pub fn create_document_type(
        &self,
        name: &[u8],
        public_id: &[u8],
        system_id: &[u8],
    ) -> DomResult<Node> {
        php_libxml::raw::ensure_init();
        let pch1 = (!public_id.is_empty()).then(|| cs(public_id));
        let pch2 = (!system_id.is_empty()).then(|| cs(system_id));
        let localname;
        if self.modern {
            if !crate::tree::valid_qname(name) {
                return Err(DomError::dom(C::Namespace));
            }
            localname = cs(name);
        } else {
            if name.is_empty() {
                return Err(DomError::arg_value(1, "must not be empty"));
            }
            if name.windows(3).any(|w| w == b"%00") {
                crate::error::warning("URI must not contain percent-encoded NUL bytes");
                return Err(DomError::Silent);
            }
            let n = cs(name);
            // SAFETY: URI parse of a valid C string; freed below.
            let opaque = unsafe {
                let uri = php_libxml::ffi::xmlParseURI(n.as_ptr());

                if uri.is_null() {
                    None
                } else {
                    let o = crate::raw::opt_cstr((*uri).opaque.cast()).map(<[u8]>::to_vec);
                    php_libxml::ffi::xmlFreeURI(uri);
                    o
                }
            };
            match opaque {
                Some(o) => {
                    if o.contains(&b':') {
                        return Err(DomError::dom(C::Namespace));
                    }
                    localname = cs(&o);
                }
                None => localname = n,
            }
        }
        // SAFETY: valid strings; the DTD is detached (doc NULL).
        let dtd = unsafe {
            ffi::xmlCreateIntSubset(
                ptr::null_mut(),
                localname.as_ptr().cast(),
                pch1.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast()),
                pch2.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast()),
            )
        };
        let d = N::from_dtd(dtd).ok_or_else(|| DomError::dom(C::InvalidState))?;
        let node = Node::wrap_with_doc(d, None);
        if self.modern {
            node.set_class_name("Dom\\DocumentType");
            node.set_builtin_class("Dom\\DocumentType");
            node.set_family_modern(true);
        }
        Ok(node)
    }

    /// `createDocument($namespace, $qualifiedName, $doctype)`.
    pub fn create_document(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
        doctype: Option<&Node>,
    ) -> DomResult<Node> {
        php_libxml::raw::ensure_init();
        if self.modern {
            return self.create_document_modern(uri, name, doctype);
        }
        let mut dt = None;
        if let Some(d) = doctype {
            let dn = d.node()?;
            if dn.t() == ffi::XML_DOCUMENT_TYPE_NODE {
                return Err(DomError::arg_value(3, "is an invalid DocumentType object"));
            }
            if !dn.doc_ptr().is_null() {
                return Err(DomError::dom(C::WrongDocument));
            }
            dt = Some(dn);
        }
        let mut localname = None;
        let mut nsptr: *mut ffi::xmlNs = ptr::null_mut();
        if !name.is_empty() {
            let (local, prefix) = crate::tree::check_qname(name, 1).map_err(DomError::dom)?;
            if let Some(u) = uri.filter(|u| !u.is_empty()) {
                let uc = cs(u);
                let pc = prefix.as_deref().map(cs);
                // SAFETY: free-standing namespace (owned by the element below).
                nsptr = unsafe {
                    ffi::xmlNewNs(
                        ptr::null_mut(),
                        uc.as_ptr().cast(),
                        pc.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
                    )
                };
                if nsptr.is_null() {
                    return Err(DomError::dom(C::Namespace));
                }
            }
            localname = Some(local);
        }
        // SAFETY: fresh document.
        let docp = unsafe { ffi::xmlNewDoc(ptr::null()) };
        let dn = N::from_doc(docp).ok_or_else(|| DomError::dom(C::InvalidState))?;
        if let Some(d) = dt {
            dn.d().intSubset = d.as_dtd();
            d.dtd().parent = docp;
            d.set_doc_raw(docp);
            dn.set_children(Some(d));
            dn.set_last(Some(d));
        }
        if let Some(l) = localname {
            let lc = cs(&l);
            // SAFETY: live document.
            let nodep =
                N::new(unsafe { ffi::xmlNewDocNode(docp, nsptr, lc.as_ptr().cast(), ptr::null()) })
                    .ok_or_else(|| DomError::dom(C::InvalidState))?;
            nodep.set_ns_def(nsptr);
            // SAFETY: live document and detached element.
            unsafe { ffi::xmlDocSetRootElement(docp, nodep.p()) };
        }
        let doc = DocInner::new(docp);
        let obj = Node::wrap_with_doc(dn, Some(doc.clone()));
        if let Some(d) = doctype {
            d.set_doc_ref(Some(doc));
        }
        Ok(obj)
    }

    #[allow(clippy::unused_self)]
    fn create_document_modern(
        &self,
        uri: Option<&[u8]>,
        name: &[u8],
        doctype: Option<&Node>,
    ) -> DomResult<Node> {
        let dt = match doctype {
            Some(d) => Some(d.node()?),
            None => None,
        };
        let pd = crate::ns::PrivateData::create();
        // SAFETY: fresh document.
        let docp = unsafe { ffi::xmlNewDoc(c"1.0".as_ptr().cast()) };
        let dn = N::from_doc(docp).ok_or_else(|| DomError::dom(C::InvalidState))?;
        dn.d().encoding = crate::raw::xml_strdup(b"UTF-8");
        let mut element = None;
        if !name.is_empty() {
            match crate::tree::validate_and_extract(uri, name) {
                Ok((local, prefix)) => {
                    let ns = pd.mapper.borrow_mut().get_ns(prefix.as_deref(), uri);
                    let lc = cs(&local);
                    // SAFETY: live document.
                    element = N::new(unsafe {
                        ffi::xmlNewDocNode(docp, ns, lc.as_ptr().cast(), ptr::null())
                    });
                }
                Err(e) => {
                    // SAFETY: freeing our fresh document.
                    unsafe { ffi::xmlFreeDoc(docp) };
                    pd.destroy();
                    return Err(DomError::dom(e));
                }
            }
        }
        let (obj, doc) = crate::document::instantiate_modern(docp);
        *doc.private.borrow_mut() = Some(pd);
        if let (Some(d), Some(dobj)) = (dt, doctype) {
            crate::element::adopt_node(d, &obj, docp);
            // SAFETY: live document and doctype.
            unsafe { ffi::xmlAddChild(docp.cast(), d.p()) };
            d.set_doc_raw(docp);
            dn.d().intSubset = d.as_dtd();
            let _ = dobj;
        }
        if let Some(e) = element {
            // SAFETY: live document and element.
            unsafe { ffi::xmlAddChild(docp.cast(), e.p()) };
        }
        Ok(obj)
    }

    /// `Dom\Implementation::createHTMLDocument($title)`.
    pub fn create_html_document(&self, title: Option<&[u8]>) -> DomResult<Node> {
        php_libxml::raw::ensure_init();
        let doc = crate::html::create_html_doc().ok_or_else(|| DomError::dom(C::InvalidState))?;
        doc.d().encoding = crate::raw::xml_strdup(b"UTF-8");
        let docp = doc.as_doc();
        // SAFETY: live document; all created nodes are attached to it.
        unsafe {
            ffi::xmlCreateIntSubset(docp, c"html".as_ptr().cast(), ptr::null(), ptr::null());
        }
        let pd = crate::ns::PrivateData::create();
        let html_ns = pd.mapper.borrow_mut().ensure_html_ns();
        // SAFETY: live document.
        unsafe {
            let html = ffi::xmlNewDocRawNode(docp, html_ns, c"html".as_ptr().cast(), ptr::null());
            ffi::xmlAddChild(docp.cast(), html);
            let head = ffi::xmlNewDocRawNode(docp, html_ns, c"head".as_ptr().cast(), ptr::null());
            ffi::xmlAddChild(html, head);
            if let Some(t) = title {
                let tc = cs(t);
                let te = ffi::xmlNewDocRawNode(
                    docp,
                    html_ns,
                    c"title".as_ptr().cast(),
                    tc.as_ptr().cast(),
                );
                ffi::xmlAddChild(head, te);
            }
            let body = ffi::xmlNewDocRawNode(docp, html_ns, c"body".as_ptr().cast(), ptr::null());
            ffi::xmlAddChild(html, body);
        }
        let (obj, d) = crate::document::instantiate_modern(docp);
        *d.private.borrow_mut() = Some(pd);
        Ok(obj)
    }
}

impl Node {
    /// `$implementation`.
    pub fn implementation(&self) -> Implementation {
        if !self.family_is_modern() {
            return Implementation::new(false);
        }
        self.0
            .extra
            .borrow_mut()
            .implementation
            .get_or_insert_with(|| Implementation::new(true))
            .clone()
    }
}
