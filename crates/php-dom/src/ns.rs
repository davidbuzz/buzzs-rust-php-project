//! Namespace compatibility layer of the spec-compliant `Dom\*` API
//! (`namespace_compat.c`, `private_data.c`).
//!
//! Modern documents do not keep namespace declarations in `nsDef`; every
//! element/attribute namespace points at an `xmlNs` owned by the
//! document's [`NsMapper`], and declarations are ordinary `xmlns`
//! attributes.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::ptr;

use crate::ffi;
use crate::object::{set_old_ns, NS_TAG_HOOK};
use crate::raw::{cs, Ns, N};

/// `DOM_XHTML_NS_URI`.
pub const XHTML_NS: &[u8] = b"http://www.w3.org/1999/xhtml";
/// `DOM_MATHML_NS_URI`.
pub const MATHML_NS: &[u8] = b"http://www.w3.org/1998/Math/MathML";
/// `DOM_SVG_NS_URI`.
pub const SVG_NS: &[u8] = b"http://www.w3.org/2000/svg";
/// `DOM_XLINK_NS_URI`.
pub const XLINK_NS: &[u8] = b"http://www.w3.org/1999/xlink";
/// `DOM_XML_NS_URI`.
pub const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";
/// `DOM_XMLNS_NS_URI`.
pub const XMLNS_NS: &[u8] = b"http://www.w3.org/2000/xmlns/";

/// A magic token stored in `xmlNs._private` to cache "this is namespace X".
#[repr(C, align(8))]
#[derive(Debug)]
pub(crate) struct Token(&'static [u8]);

pub(crate) static HTML_TOKEN: Token = Token(XHTML_NS);
pub(crate) static MATHML_TOKEN: Token = Token(MATHML_NS);
pub(crate) static SVG_TOKEN: Token = Token(SVG_NS);
pub(crate) static XLINK_TOKEN: Token = Token(XLINK_NS);
pub(crate) static XML_TOKEN: Token = Token(XML_NS);
pub(crate) static XMLNS_TOKEN: Token = Token(XMLNS_NS);

impl Token {
    pub(crate) fn addr(&'static self) -> *mut c_void {
        ptr::from_ref(self).cast_mut().cast()
    }
}

/// `php_dom_ns_is_fast_ex`.
pub(crate) fn ns_is_fast_ex(ns: Ns, token: &'static Token) -> bool {
    let p = ns.private();
    if p == token.addr() {
        return true;
    } else if !p.is_null() && (p as usize) & 1 == 0 {
        return false;
    }
    if ns.href() == token.0 {
        if p.is_null() {
            ns.set_private(token.addr());
        }
        return true;
    }
    false
}

/// `php_dom_ns_is_fast`.
pub(crate) fn ns_is_fast(n: N, token: &'static Token) -> bool {
    n.ns().is_some_and(|ns| ns_is_fast_ex(ns, token))
}

/// `php_dom_ns_is_html_and_document_is_html`.
pub(crate) fn is_html_and_document_is_html(n: N) -> bool {
    n.doc()
        .is_some_and(|d| d.t() == ffi::XML_HTML_DOCUMENT_NODE)
        && ns_is_fast(n, &HTML_TOKEN)
}

struct OwnedNs {
    ns: ffi::xmlNs,
    _prefix: Option<CString>,
    _href: CString,
}

enum Entry {
    Owned(Box<OwnedNs>),
    Unowned(*mut ffi::xmlNs),
}

impl Entry {
    fn ptr(&mut self) -> *mut ffi::xmlNs {
        match self {
            Entry::Owned(o) => &raw mut o.ns,
            Entry::Unowned(p) => *p,
        }
    }
}

/// `php_dom_libxml_ns_mapper`.
#[derive(Default)]
pub(crate) struct NsMapper {
    html_ns: Option<*mut ffi::xmlNs>,
    prefixless_xmlns_ns: Option<*mut ffi::xmlNs>,
    uri_to_prefix: HashMap<Vec<u8>, HashMap<Vec<u8>, Entry>>,
}

impl std::fmt::Debug for NsMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NsMapper")
            .field("uris", &self.uri_to_prefix.len())
            .finish_non_exhaustive()
    }
}

impl NsMapper {
    /// `php_dom_libxml_ns_mapper_get_ns`.
    pub fn get_ns(&mut self, prefix: Option<&[u8]>, uri: Option<&[u8]>) -> *mut ffi::xmlNs {
        let uri = uri.unwrap_or_default();
        let prefix = prefix.unwrap_or_default();
        if prefix.is_empty() && uri.is_empty() {
            return ptr::null_mut();
        }
        let map = self.uri_to_prefix.entry(uri.to_vec()).or_default();
        if let Some(e) = map.get_mut(prefix) {
            return e.ptr();
        }
        let href = cs(uri);
        let pfx = if prefix.is_empty() {
            None
        } else {
            Some(cs(prefix))
        };
        // SAFETY: an all-zero xmlNs is a valid empty namespace record.
        let mut ns: ffi::xmlNs = unsafe { std::mem::zeroed() };
        ns.type_ = ffi::XML_LOCAL_NAMESPACE;
        ns.href = href.as_ptr().cast();
        ns.prefix = pfx.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast());
        let mut owned = Box::new(OwnedNs {
            ns,
            _prefix: pfx,
            _href: href,
        });
        let p = &raw mut owned.ns;
        map.insert(prefix.to_vec(), Entry::Owned(owned));
        p
    }

    /// `php_dom_libxml_ns_mapper_get_ns_raw_strings_nullsafe`.
    pub fn get_ns_nullsafe(
        &mut self,
        prefix: Option<&[u8]>,
        uri: Option<&[u8]>,
    ) -> *mut ffi::xmlNs {
        self.get_ns(
            Some(prefix.unwrap_or_default()),
            Some(uri.unwrap_or_default()),
        )
    }

    fn ensure_cached(&mut self, which: bool, uri: &[u8], token: &'static Token) -> *mut ffi::xmlNs {
        let slot = if which {
            self.html_ns
        } else {
            self.prefixless_xmlns_ns
        };
        if let Some(p) = slot {
            return p;
        }
        let p = self.get_ns(None, Some(uri));
        if let Some(ns) = Ns::new(p) {
            ns.set_private(token.addr());
        }
        if which {
            self.html_ns = Some(p);
        } else {
            self.prefixless_xmlns_ns = Some(p);
        }
        p
    }

    /// `php_dom_libxml_ns_mapper_ensure_html_ns`.
    pub fn ensure_html_ns(&mut self) -> *mut ffi::xmlNs {
        self.ensure_cached(true, XHTML_NS, &HTML_TOKEN)
    }

    /// `php_dom_libxml_ns_mapper_ensure_prefixless_xmlns_ns`.
    pub fn ensure_prefixless_xmlns_ns(&mut self) -> *mut ffi::xmlNs {
        self.ensure_cached(false, XMLNS_NS, &XMLNS_TOKEN)
    }

    /// `php_dom_libxml_ns_mapper_store_and_normalize_parsed_ns`.
    fn store_parsed(&mut self, ns: Ns) {
        let map = self.uri_to_prefix.entry(ns.href().to_vec()).or_default();
        let prefix = ns.prefix().unwrap_or_default().to_vec();
        map.entry(prefix).or_insert(Entry::Unowned(ns.p()));
    }
}

/// `php_dom_private_data`.
#[derive(Debug, Default)]
pub struct PrivateData {
    pub(crate) mapper: RefCell<NsMapper>,
    templates: RefCell<HashMap<usize, N>>,
}

thread_local! {
    static LIVE_PRIVATE: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

impl PrivateData {
    /// `php_dom_private_data_create`.
    pub(crate) fn create() -> std::rc::Rc<PrivateData> {
        let rc = std::rc::Rc::new(PrivateData::default());
        LIVE_PRIVATE.with(|l| l.borrow_mut().insert(std::rc::Rc::as_ptr(&rc) as usize));
        rc
    }

    /// `php_dom_private_data_destroy` (template fragments).
    pub(crate) fn destroy(&self) {
        LIVE_PRIVATE.with(|l| l.borrow_mut().remove(&(ptr::from_ref(self) as usize)));
        let fragments: Vec<N> = self
            .templates
            .borrow_mut()
            .drain()
            .map(|(_, n)| n)
            .collect();
        for f in fragments {
            // SAFETY: template fragments are owned by the private data and
            // never exposed with handles to userland.
            unsafe { ffi::xmlFreeNode(f.p()) };
        }
    }

    /// `php_dom_retrieve_templated_content`.
    pub(crate) fn retrieve_template(&self, template: N) -> Option<N> {
        self.templates
            .borrow()
            .get(&(template.p() as usize))
            .copied()
    }

    /// `php_dom_add_templated_content`.
    pub(crate) fn add_template(&self, template: N, fragment: N) {
        self.templates
            .borrow_mut()
            .insert(template.p() as usize, fragment);
    }

    /// `php_dom_ensure_templated_content`.
    pub(crate) fn ensure_template(&self, template: N) -> Option<N> {
        if let Some(f) = self.retrieve_template(template) {
            return Some(f);
        }
        // SAFETY: template is a live element of a live document.
        let frag = N::new(unsafe { ffi::xmlNewDocFragment(template.doc_ptr()) })?;
        frag.set_parent(Some(template));
        self.add_element_ns_hook(template);
        self.add_template(template, frag);
        Some(frag)
    }

    /// `php_dom_remove_templated_content`.
    pub(crate) fn remove_template(&self, template: N) {
        let frag = self.templates.borrow_mut().remove(&(template.p() as usize));
        if let Some(frag) = frag {
            self.remove_templates_in_subtree(frag);
            // SAFETY: the fragment is owned by us and now unregistered.
            unsafe { ffi::xmlFreeNode(frag.p()) };
        }
    }

    /// `php_dom_remove_templated_content_in_subtree`.
    pub(crate) fn remove_templates_in_subtree(&self, base: N) {
        if self.templates.borrow().is_empty() {
            return;
        }
        let mut cur = base.children();
        while let Some(c) = cur {
            if c.is_element() {
                self.remove_template(c);
            }
            cur = crate::tree::next_in_tree_order(c, Some(base));
        }
    }

    /// `php_dom_get_template_count`.
    pub(crate) fn template_count(&self) -> usize {
        self.templates.borrow().len()
    }

    /// `dom_add_element_ns_hook`.
    pub(crate) fn add_element_ns_hook(&self, element: N) {
        let Some(ens) = element.ns() else { return };
        let ns = crate::object::alloc_ns();
        if ns.is_null() {
            return;
        }
        // SAFETY: ns is a fresh zeroed record; ownership goes to oldNs.
        unsafe {
            (*ns).prefix = if ens.prefix_ptr().is_null() {
                ptr::null()
            } else {
                ffi::xmlStrdup(ens.prefix_ptr())
            };
            (*ns).href = ffi::xmlStrdup(ens.href_ptr());
            (*ns).type_ = ffi::XML_LOCAL_NAMESPACE;
            (*ns)._private = ((ptr::from_ref(self) as usize) | NS_TAG_HOOK) as *mut c_void;
        }
        element.set_ns(ns);
        set_old_ns(element.doc_ptr(), ns);
    }
}

/// Invoked by `node_free` for elements whose namespace carries the hook tag.
pub(crate) fn run_ns_hook(private_addr: usize, node: N) {
    let live = LIVE_PRIVATE.with(|l| l.borrow().contains(&private_addr));
    if live {
        // SAFETY: the address is registered as a live PrivateData (it is
        // removed from the registry in `destroy`, before deallocation).
        let pd = unsafe { &*(private_addr as *const PrivateData) };
        pd.remove_template(node);
    }
}

/// `php_dom_ns_compat_mark_attribute`.
pub(crate) fn mark_attribute(mapper: &mut NsMapper, node: N, ns: Ns) -> Option<N> {
    let (xmlns_ns, name): (*mut ffi::xmlNs, *const u8) = if ns.prefix_ptr().is_null() {
        (
            mapper.ensure_prefixless_xmlns_ns(),
            c"xmlns".as_ptr().cast(),
        )
    } else {
        (
            mapper.get_ns(Some(b"xmlns"), Some(XMLNS_NS)),
            ns.prefix_ptr(),
        )
    };
    // SAFETY: node is a live element; strings are valid C strings.
    N::from_attr(unsafe { ffi::xmlSetNsProp(node.p(), xmlns_ns, name, ns.href_ptr()) })
}

/// `php_dom_ns_compat_mark_attribute_list`.
pub(crate) fn mark_attribute_list(mapper: &mut NsMapper, node: N) {
    let Some(mut ns) = node.ns_def() else { return };
    let first_original = node.properties();
    let mut first_ns_attr: Option<N> = None;
    let mut last_ns_attr: Option<N> = None;
    loop {
        let next = ns.next();
        node.set_ns_def(next.map_or(ptr::null_mut(), Ns::p));
        ns.set_next(ptr::null_mut());
        set_old_ns(node.doc_ptr(), ns.p());
        if let Some(added) = mark_attribute(mapper, node, ns) {
            if first_ns_attr.is_none() {
                first_ns_attr = Some(added);
            }
            last_ns_attr = Some(added);
        }
        mapper.store_parsed(ns);
        match next {
            Some(n) => ns = n,
            None => break,
        }
    }
    if let (Some(first_ns), Some(last_ns), Some(first_orig)) =
        (first_ns_attr, last_ns_attr, first_original)
    {
        if let Some(last_original) = first_ns.prev() {
            last_original.set_next(None);
            first_ns.set_prev(None);
            last_ns.set_next(Some(first_orig));
            first_orig.set_prev(Some(last_ns));
            node.set_properties(Some(first_ns));
        }
    }
}

/// `php_dom_reconcile_attribute_namespace_after_insertion`.
pub(crate) fn reconcile_attribute_namespace_after_insertion(attr: N) {
    let Some(ans) = attr.ns() else { return };
    let Some(node) = attr.parent() else { return };
    // SAFETY: live nodes and namespace records.
    unsafe {
        let matching = ffi::xmlSearchNs(node.doc_ptr(), node.p(), ans.prefix_ptr());
        if !matching.is_null() && ffi::xmlStrEqual((*matching).href, ans.href_ptr()) != 0 {
            attr.set_ns(matching);
        } else if !ans.prefix_ptr().is_null() {
            ffi::xmlReconciliateNs(node.doc_ptr(), node.p());
        }
    }
}

struct ReconcileCtx<'a> {
    map: HashMap<usize, *mut ffi::xmlNs>,
    last_src: *mut ffi::xmlNs,
    last_dst: *mut ffi::xmlNs,
    mapper: &'a mut NsMapper,
}

fn reconcile_single(ctx: &mut ReconcileCtx<'_>, node: N) {
    let ns = node.ns_ptr();
    if ns == ctx.last_src {
        node.set_ns(ctx.last_dst);
        return;
    }
    match ctx.map.get(&(ns as usize)) {
        None => {
            let Some(old) = Ns::new(ns) else { return };
            let new_ns = ctx.mapper.get_ns_nullsafe(old.prefix(), Some(old.href()));
            ctx.map.insert(ns as usize, new_ns);
            ctx.last_src = ns;
            ctx.last_dst = new_ns;
            node.set_ns(new_ns);
        }
        Some(&new_ns) => {
            if ns != new_ns {
                node.set_ns(new_ns);
            }
        }
    }
}

fn reconcile_single_element(ctx: &mut ReconcileCtx<'_>, node: N) {
    if node.ns().is_some() {
        reconcile_single(ctx, node);
    }
    for attr in node.attr_iter() {
        if attr.ns().is_some() {
            reconcile_single(ctx, attr);
        }
    }
}

/// `php_dom_libxml_reconcile_modern`.
pub(crate) fn reconcile_modern(mapper: &mut NsMapper, node: N) {
    if node.t() == ffi::XML_ATTRIBUTE_NODE {
        if let Some(ns) = node.ns() {
            node.set_ns(mapper.get_ns_nullsafe(ns.prefix(), Some(ns.href())));
        }
        return;
    }
    if !node.is_element()
        || (node.children().is_none()
            && node.properties().is_none()
            && node.ns_ptr() == node.ns_def().map_or(ptr::null_mut(), Ns::p))
    {
        return;
    }
    let mut ctx = ReconcileCtx {
        map: HashMap::new(),
        last_src: ptr::null_mut(),
        last_dst: ptr::null_mut(),
        mapper,
    };
    reconcile_single_element(&mut ctx, node);
    let base = node;
    let mut cur = node.children();
    while let Some(c) = cur {
        if c.is_element() {
            reconcile_single_element(&mut ctx, c);
        }
        cur = crate::tree::next_in_tree_order(c, Some(base));
    }
}

/// `php_dom_get_in_scope_ns`: (prefix, namespace) pairs in scope of `node`
/// for a modern document (first declaration found walking upwards wins).
pub(crate) fn in_scope_ns(
    mapper: &mut NsMapper,
    node: N,
    ignore_elements: bool,
) -> Vec<*mut ffi::xmlNs> {
    let mut seen: Vec<(Vec<u8>, *mut ffi::xmlNs)> = Vec::new();
    let mut add = |prefix: &[u8], ns: *mut ffi::xmlNs| {
        if !seen.iter().any(|(p, _)| p == prefix) {
            seen.push((prefix.to_vec(), ns));
        }
    };
    let mut cur = Some(node);
    while let Some(c) = cur {
        if c.is_element() {
            if !ignore_elements {
                if let Some(ns) = c.ns() {
                    if let Some(prefix) = ns.prefix() {
                        add(prefix, ns.p());
                    }
                }
            }
            for attr in c.attr_iter() {
                if let Some(ans) = attr.ns() {
                    if ans.prefix().is_some() && ns_is_fast_ex(ans, &XMLNS_TOKEN) {
                        if let Some(child) = attr.children() {
                            if !child.content_ptr().is_null() {
                                let prefix = attr.name();
                                let ns = mapper.get_ns(Some(prefix), Some(child.content()));
                                add(prefix, ns);
                            }
                        }
                    }
                }
            }
        }
        cur = c.parent();
    }
    seen.into_iter().map(|(_, ns)| ns).collect()
}

/// `php_dom_get_in_scope_ns_legacy` (`xmlGetNsList`).
pub(crate) fn in_scope_ns_legacy(node: N) -> Vec<*mut ffi::xmlNs> {
    let mut out = Vec::new();
    // SAFETY: node is live; the returned array is NULL-terminated and freed.
    unsafe {
        let list = ffi::xmlGetNsList(node.doc_ptr(), node.p());
        if list.is_null() {
            return out;
        }
        let mut i = 0;
        while !(*list.add(i)).is_null() {
            out.push(*list.add(i));
            i += 1;
        }
        crate::raw::xml_free(list.cast());
    }
    out
}

/// Temporarily materialised `nsDef` entries (`dom_relink_ns_decls`) so
/// that libxml2's C14N / validators can see modern-DOM declarations.
#[derive(Debug, Default)]
pub(crate) struct NsLinks {
    /// element → number of synthetic nsDef entries prepended.
    counts: Vec<(N, usize)>,
    /// element → original `ns` (replaced by a search result).
    saved_ns: Vec<(N, *mut ffi::xmlNs)>,
}

impl NsLinks {
    fn bump(&mut self, node: N) {
        if let Some(e) = self.counts.iter_mut().find(|(n, _)| *n == node) {
            e.1 += 1;
        } else {
            self.counts.push((node, 1));
        }
    }

    fn alloc_decl(&mut self, node: N) -> *mut ffi::xmlNs {
        let ns = crate::object::alloc_ns();
        if ns.is_null() {
            return ns;
        }
        self.bump(node);
        // SAFETY: fresh zeroed record, linked at the head of nsDef.
        unsafe {
            (*ns).type_ = ffi::XML_LOCAL_NAMESPACE;
            (*ns).next = (*node.p()).nsDef;
        }
        node.set_ns_def(ns);
        ns
    }

    fn add_synthetic(&mut self, node: N, src: Ns) {
        let ns = self.alloc_decl(node);
        if ns.is_null() {
            return;
        }
        // SAFETY: fresh record; strings duplicated.
        unsafe {
            (*ns).href = ffi::xmlStrdup(src.href_ptr());
            (*ns).prefix = if src.prefix_ptr().is_null() {
                ptr::null()
            } else {
                ffi::xmlStrdup(src.prefix_ptr())
            };
        }
    }

    fn add_synthetic_for_attr(&mut self, node: N, src: Ns) {
        let mut cur = node.ns_def();
        while let Some(e) = cur {
            if crate::raw::str_eq(e.prefix(), src.prefix()) {
                return;
            }
            cur = e.next();
        }
        self.add_synthetic(node, src);
    }

    fn relink_element(&mut self, node: N) {
        if !node.is_element() {
            return;
        }
        let attrs: Vec<N> = node.attr_iter().collect();
        for attr in attrs {
            if ns_is_fast(attr, &XMLNS_TOKEN) {
                let ns = self.alloc_decl(node);
                if ns.is_null() {
                    return;
                }
                let value = crate::tree::attr_value(attr);
                // SAFETY: fresh record; attr is live and is unlinked from the
                // property list (re-linked in `unlink`).
                unsafe {
                    (*ns).href = crate::raw::xml_strdup(&value);
                    (*ns).prefix = if attr.ns().and_then(Ns::prefix).is_some() {
                        ffi::xmlStrdup(attr.name_ptr())
                    } else {
                        ptr::null()
                    };
                    (*ns)._private = attr.p().cast();
                }
                match attr.prev() {
                    Some(p) => p.set_next(attr.next()),
                    None => node.set_properties(attr.next()),
                }
                if let Some(nx) = attr.next() {
                    nx.set_prev(attr.prev());
                }
            }
        }
        if let Some(ns) = node.ns() {
            if ns.prefix_ptr().is_null() {
                self.saved_ns.push((node, ns.p()));
                // SAFETY: live element.
                let found = unsafe { ffi::xmlSearchNs(node.doc_ptr(), node.p(), ptr::null()) };
                node.set_ns(found);
            } else {
                self.add_synthetic(node, ns);
            }
        }
        let attrs: Vec<N> = node.attr_iter().collect();
        for attr in attrs {
            if let Some(ans) = attr.ns() {
                if !ns_is_fast(attr, &XMLNS_TOKEN) {
                    self.add_synthetic_for_attr(node, ans);
                }
            }
        }
    }

    /// `dom_relink_ns_decls`.
    pub(crate) fn relink(&mut self, root: N) {
        self.relink_element(root);
        let mut cur = root.children();
        while let Some(c) = cur {
            self.relink_element(c);
            cur = crate::tree::next_in_tree_order(c, Some(root));
        }
    }

    /// `dom_unlink_ns_decls`.
    pub(crate) fn unlink(self) {
        for (node, ns) in self.saved_ns {
            node.set_ns(ns);
        }
        for (node, mut count) in self.counts {
            while count > 0 {
                count -= 1;
                let Some(ns) = node.ns_def() else { break };
                node.set_ns_def(ns.next().map_or(ptr::null_mut(), Ns::p));
                let attr = N::new(ns.private().cast());
                if let Some(attr) = attr {
                    match attr.prev() {
                        Some(p) => p.set_next(Some(attr)),
                        None => node.set_properties(Some(attr)),
                    }
                    if let Some(nx) = attr.next() {
                        nx.set_prev(Some(attr));
                    }
                }
                ns.set_private(ptr::null_mut());
                // SAFETY: the record was allocated by `alloc_decl`.
                unsafe { ffi::xmlFreeNs(ns.p()) };
            }
        }
    }
}
