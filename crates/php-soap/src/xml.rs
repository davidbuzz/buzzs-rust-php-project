//! The XML tree ext/soap works on.
//!
//! ext/soap manipulates libxml2 trees directly (`xmlNewDocNode`,
//! `xmlSetNsProp`, `xmlSearchNsByHref`, …) and serializes them with
//! `xmlDocDumpMemory` / `xmlNodeDump`. This module is a small arena tree
//! with exactly those operations and a serializer reproducing libxml2's
//! output byte for byte (attribute/text escaping rules, namespace
//! declarations, empty-element form, the XML declaration).
//!
//! Documents are *parsed* by the system libxml2 (through
//! [`php_libxml::tree::Document`], with the options of `php_xml.c`) and
//! converted into this representation.

use php_libxml::tree::{Document, Node as LxNode, NodeType, ParseOptions, ParserChannels};

use crate::consts::{SOAP_MAX_XML_DEPTH, XML_NAMESPACE};

/// Index of a node inside its [`Doc`].
pub type NodeId = usize;

/// A namespace binding (`xmlNs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ns {
    /// The prefix (`None` for a default namespace).
    pub prefix: Option<Vec<u8>>,
    /// The namespace name.
    pub href: Vec<u8>,
}

impl Ns {
    /// Builds a namespace.
    #[must_use]
    pub fn new(prefix: Option<&[u8]>, href: &[u8]) -> Self {
        Self {
            prefix: prefix.map(<[u8]>::to_vec),
            href: href.to_vec(),
        }
    }

    /// The implicit `xml` namespace.
    #[must_use]
    pub fn xml() -> Self {
        Self::new(Some(b"xml"), XML_NAMESPACE.as_bytes())
    }
}

/// Node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An element.
    Element,
    /// A text node (escaped on output).
    Text,
    /// A text node written verbatim (`xmlStringTextNoenc`).
    TextNoEnc,
    /// A CDATA section.
    CData,
    /// An entity reference (`&name;`).
    EntityRef,
    /// A comment.
    Comment,
    /// A processing instruction.
    Pi,
    /// A document type declaration (kept only to detect DTDs).
    Dtd,
}

/// An attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Local name (may contain a colon when set through `xmlSetProp` with
    /// an undeclared prefix).
    pub name: Vec<u8>,
    /// Namespace.
    pub ns: Option<Ns>,
    /// Value.
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parent {
    None,
    Doc,
    Node(NodeId),
}

#[derive(Debug, Clone)]
struct NodeData {
    kind: Kind,
    name: Vec<u8>,
    ns: Option<Ns>,
    ns_defs: Vec<Ns>,
    attrs: Vec<Attr>,
    content: Vec<u8>,
    parent: Parent,
    first: Option<NodeId>,
    last: Option<NodeId>,
    next: Option<NodeId>,
    prev: Option<NodeId>,
    line: u32,
}

impl NodeData {
    fn new(kind: Kind, name: &[u8]) -> Self {
        Self {
            kind,
            name: name.to_vec(),
            ns: None,
            ns_defs: Vec::new(),
            attrs: Vec::new(),
            content: Vec::new(),
            parent: Parent::None,
            first: None,
            last: None,
            next: None,
            prev: None,
            line: 0,
        }
    }
}

/// Why parsing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseFailure {
    /// libxml2 rejected the document; the message of `xmlGetLastError()`
    /// when there was one.
    Malformed(Option<String>),
    /// The document nests deeper than `SOAP_MAX_XML_DEPTH`.
    TooDeep,
}

/// An XML document.
#[derive(Debug, Clone)]
pub struct Doc {
    uid: u64,
    nodes: Vec<NodeData>,
    first: Option<NodeId>,
    last: Option<NodeId>,
    /// `doc->URL`.
    pub url: Option<String>,
    /// `doc->encoding` (from the XML declaration, or set by the builder).
    pub encoding: Option<Vec<u8>>,
}

/// Iterator over a sibling chain.
#[derive(Debug, Clone)]
pub struct Siblings<'a> {
    doc: &'a Doc,
    cur: Option<NodeId>,
}

impl Iterator for Siblings<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let c = self.cur?;
        self.cur = self.doc.nodes[c].next;
        Some(c)
    }
}

thread_local! {
    static NEXT_DOC_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

impl Default for Doc {
    fn default() -> Self {
        let uid = NEXT_DOC_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        Self {
            uid,
            nodes: Vec::new(),
            first: None,
            last: None,
            url: None,
            encoding: None,
        }
    }
}

impl Doc {
    /// A process-unique identity of this document (used to key
    /// multi-reference maps).
    #[must_use]
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// An empty document (`xmlNewDoc("1.0")`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty document whose `encoding` is `UTF-8` (the SOAP envelopes).
    #[must_use]
    pub fn new_utf8() -> Self {
        Self {
            encoding: Some(b"UTF-8".to_vec()),
            ..Self::default()
        }
    }

    fn alloc(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(data);
        self.nodes.len() - 1
    }

    // ---- accessors -------------------------------------------------------

    /// Node kind.
    #[must_use]
    pub fn kind(&self, n: NodeId) -> Kind {
        self.nodes[n].kind
    }

    /// Whether `n` is an element.
    #[must_use]
    pub fn is_element(&self, n: NodeId) -> bool {
        self.nodes[n].kind == Kind::Element
    }

    /// Local name.
    #[must_use]
    pub fn name(&self, n: NodeId) -> &[u8] {
        &self.nodes[n].name
    }

    /// Namespace of an element.
    #[must_use]
    pub fn ns(&self, n: NodeId) -> Option<&Ns> {
        self.nodes[n].ns.as_ref()
    }

    /// Namespace name of an element (`node->ns->href`).
    #[must_use]
    pub fn ns_href(&self, n: NodeId) -> Option<&[u8]> {
        self.nodes[n].ns.as_ref().map(|ns| ns.href.as_slice())
    }

    /// Namespace declarations made on an element.
    #[must_use]
    pub fn ns_defs(&self, n: NodeId) -> &[Ns] {
        &self.nodes[n].ns_defs
    }

    /// Content of a text-like node.
    #[must_use]
    pub fn content(&self, n: NodeId) -> &[u8] {
        &self.nodes[n].content
    }

    /// Replaces the content of a text-like node.
    pub fn set_text_content(&mut self, n: NodeId, content: &[u8]) {
        self.nodes[n].content = content.to_vec();
    }

    /// Source line (parsed documents).
    #[must_use]
    pub fn line(&self, n: NodeId) -> u32 {
        self.nodes[n].line
    }

    /// Parent element.
    #[must_use]
    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        match self.nodes[n].parent {
            Parent::Node(p) => Some(p),
            _ => None,
        }
    }

    /// First child.
    #[must_use]
    pub fn first_child(&self, n: NodeId) -> Option<NodeId> {
        self.nodes[n].first
    }

    /// Last child.
    #[must_use]
    pub fn last_child(&self, n: NodeId) -> Option<NodeId> {
        self.nodes[n].last
    }

    /// Next sibling.
    #[must_use]
    pub fn next(&self, n: NodeId) -> Option<NodeId> {
        self.nodes[n].next
    }

    /// Previous sibling.
    #[must_use]
    pub fn prev(&self, n: NodeId) -> Option<NodeId> {
        self.nodes[n].prev
    }

    /// Children of `n`.
    #[must_use]
    pub fn children(&self, n: NodeId) -> Siblings<'_> {
        Siblings {
            doc: self,
            cur: self.nodes[n].first,
        }
    }

    /// The chain starting at `start` (`for (trav = start; trav; trav = trav->next)`).
    #[must_use]
    pub fn siblings_from(&self, start: Option<NodeId>) -> Siblings<'_> {
        Siblings {
            doc: self,
            cur: start,
        }
    }

    /// Top-level nodes (`doc->children`).
    #[must_use]
    pub fn top(&self) -> Siblings<'_> {
        Siblings {
            doc: self,
            cur: self.first,
        }
    }

    /// First top-level node.
    #[must_use]
    pub fn first_top(&self) -> Option<NodeId> {
        self.first
    }

    /// The document element.
    #[must_use]
    pub fn root(&self) -> Option<NodeId> {
        self.top().find(|&n| self.is_element(n))
    }

    /// Whether the document has a DTD (`xmlGetIntSubset`).
    #[must_use]
    pub fn has_dtd(&self) -> bool {
        self.top().any(|n| self.kind(n) == Kind::Dtd)
    }

    /// Attributes of an element.
    #[must_use]
    pub fn attrs(&self, n: NodeId) -> &[Attr] {
        &self.nodes[n].attrs
    }

    /// `get_attribute_ex`: attribute `name` in namespace `ns` (no namespace
    /// when `None`).
    #[must_use]
    pub fn attr(&self, n: NodeId, name: &str, ns: Option<&str>) -> Option<&Attr> {
        self.nodes[n].attrs.iter().find(|a| {
            a.name == name.as_bytes()
                && match (ns, &a.ns) {
                    (Some(want), Some(have)) => have.href == want.as_bytes(),
                    (None, None) => true,
                    _ => false,
                }
        })
    }

    /// Value of [`Doc::attr`].
    #[must_use]
    pub fn attr_value(&self, n: NodeId, name: &str, ns: Option<&str>) -> Option<&[u8]> {
        self.attr(n, name, ns).map(|a| a.value.as_slice())
    }

    /// `get_attribute_any_ns`.
    #[must_use]
    pub fn attr_any_ns(&self, n: NodeId, name: &[u8]) -> Option<&Attr> {
        self.nodes[n].attrs.iter().find(|a| a.name == name)
    }

    /// `node_is_equal_ex`: element `name` (any when `None`) whose namespace
    /// (the in-scope default namespace for unprefixed elements) is `ns`.
    #[must_use]
    pub fn node_is(&self, n: NodeId, name: Option<&str>, ns: Option<&str>) -> bool {
        if let Some(name) = name {
            if self.nodes[n].name != name.as_bytes() {
                return false;
            }
        }
        if let Some(ns) = ns {
            return self
                .node_find_ns(n)
                .is_some_and(|found| found.href == ns.as_bytes());
        }
        true
    }

    /// `node_find_ns`: the element namespace or the in-scope default one.
    #[must_use]
    pub fn node_find_ns(&self, n: NodeId) -> Option<Ns> {
        if let Some(ns) = &self.nodes[n].ns {
            return Some(ns.clone());
        }
        self.search_ns(n, None)
    }

    /// `get_node_ex`: first node in the chain starting at `start` matching
    /// [`Doc::node_is`].
    #[must_use]
    pub fn get_node(&self, start: Option<NodeId>, name: &str, ns: Option<&str>) -> Option<NodeId> {
        self.siblings_from(start)
            .find(|&n| self.node_is(n, Some(name), ns))
    }

    /// `get_node_with_attribute_ex`.
    #[must_use]
    pub fn get_node_with_attribute(
        &self,
        start: Option<NodeId>,
        name: &str,
        name_ns: Option<&str>,
        attribute: &str,
        value: &[u8],
    ) -> Option<NodeId> {
        let mut cur = start;
        while let Some(c) = cur {
            let node = self.get_node(Some(c), name, name_ns)?;
            if self.attr_value(node, attribute, None) == Some(value) {
                return Some(node);
            }
            cur = self.next(node);
        }
        None
    }

    /// `get_node_with_attribute_recursive_ex` with any element name.
    #[must_use]
    pub fn find_by_attribute_recursive(
        &self,
        start: Option<NodeId>,
        attribute: &str,
        value: &[u8],
        attr_ns: Option<&str>,
    ) -> Option<NodeId> {
        let mut node = start?;
        let mut depth = 0usize;
        loop {
            if self.attr_value(node, attribute, attr_ns) == Some(value) {
                return Some(node);
            }
            if let Some(child) = self.first_child(node) {
                if !matches!(self.kind(node), Kind::EntityRef | Kind::Dtd) {
                    node = child;
                    depth += 1;
                    continue;
                }
            }
            loop {
                if let Some(next) = self.next(node) {
                    node = next;
                    break;
                }
                if depth == 0 {
                    return None;
                }
                node = match self.nodes[node].parent {
                    Parent::Node(p) => p,
                    _ => return None,
                };
                depth -= 1;
            }
        }
    }

    // ---- namespaces ---------------------------------------------------------

    /// `xmlSearchNs`.
    #[must_use]
    pub fn search_ns(&self, n: NodeId, prefix: Option<&[u8]>) -> Option<Ns> {
        if prefix == Some(b"xml".as_slice()) {
            return Some(Ns::xml());
        }
        let orig = n;
        let mut cur = Some(n);
        while let Some(c) = cur {
            let d = &self.nodes[c];
            if matches!(d.kind, Kind::EntityRef) {
                return None;
            }
            if d.kind == Kind::Element {
                for ns in &d.ns_defs {
                    if ns.prefix.as_deref() == prefix {
                        return Some(ns.clone());
                    }
                }
                if orig != c {
                    if let Some(ns) = &d.ns {
                        if ns.prefix.as_deref() == prefix {
                            return Some(ns.clone());
                        }
                    }
                }
            }
            cur = self.parent(c);
        }
        None
    }

    fn ns_in_scope(&self, orig: NodeId, ancestor: NodeId, prefix: Option<&[u8]>) -> bool {
        let mut cur = Some(orig);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            if self.nodes[c].kind == Kind::Element
                && self.nodes[c]
                    .ns_defs
                    .iter()
                    .any(|ns| ns.prefix.as_deref() == prefix)
            {
                return false;
            }
            cur = self.parent(c);
        }
        true
    }

    /// `xmlSearchNsByHref` (for elements).
    #[must_use]
    pub fn search_ns_by_href(&self, n: NodeId, href: &[u8]) -> Option<Ns> {
        if href == XML_NAMESPACE.as_bytes() {
            return Some(Ns::xml());
        }
        let orig = n;
        let mut cur = Some(n);
        while let Some(c) = cur {
            let d = &self.nodes[c];
            if d.kind == Kind::Element {
                for ns in &d.ns_defs {
                    if ns.href == href && self.ns_in_scope(orig, c, ns.prefix.as_deref()) {
                        return Some(ns.clone());
                    }
                }
                if orig != c {
                    if let Some(ns) = &d.ns {
                        if ns.href == href && self.ns_in_scope(orig, c, ns.prefix.as_deref()) {
                            return Some(ns.clone());
                        }
                    }
                }
            }
            cur = self.parent(c);
        }
        None
    }

    /// `xmlSearchNsPrefixByHref` from `php_encoding.c`: an in-scope
    /// declaration of `href` that has a prefix.
    #[must_use]
    pub fn search_prefixed_ns_by_href(&self, n: NodeId, href: &[u8]) -> Option<Ns> {
        let orig = n;
        let mut cur = Some(n);
        while let Some(c) = cur {
            let d = &self.nodes[c];
            if matches!(d.kind, Kind::EntityRef) {
                return None;
            }
            if d.kind == Kind::Element {
                for ns in &d.ns_defs {
                    if ns.prefix.is_some()
                        && ns.href == href
                        && self.search_ns(c, ns.prefix.as_deref()).as_ref() == Some(ns)
                    {
                        return Some(ns.clone());
                    }
                }
                if orig != c {
                    if let Some(ns) = &d.ns {
                        if ns.prefix.is_some()
                            && ns.href == href
                            && self.search_ns(c, ns.prefix.as_deref()).as_ref() == Some(ns)
                        {
                            return Some(ns.clone());
                        }
                    }
                }
            }
            cur = self.parent(c);
        }
        None
    }

    /// `xmlNewNs(node, href, prefix)`: declares a namespace on an element;
    /// `None` when the prefix is `xml` or already declared there.
    pub fn new_ns(&mut self, n: NodeId, href: &[u8], prefix: Option<&[u8]>) -> Option<Ns> {
        if prefix == Some(b"xml".as_slice()) {
            return None;
        }
        if self.nodes[n]
            .ns_defs
            .iter()
            .any(|ns| ns.prefix.as_deref() == prefix)
        {
            return None;
        }
        let ns = Ns::new(prefix, href);
        self.nodes[n].ns_defs.push(ns.clone());
        Some(ns)
    }

    /// `xmlSetNs`.
    pub fn set_ns(&mut self, n: NodeId, ns: Option<Ns>) {
        self.nodes[n].ns = ns;
    }

    /// `xmlNodeSetName`.
    pub fn set_name(&mut self, n: NodeId, name: &[u8]) {
        self.nodes[n].name = name.to_vec();
    }

    // ---- attributes -------------------------------------------------------------

    /// `xmlSetNsProp`: sets (or replaces in place) attribute `name` in
    /// namespace `ns`.
    pub fn set_ns_prop(&mut self, n: NodeId, ns: Option<Ns>, name: &[u8], value: &[u8]) {
        let attrs = &mut self.nodes[n].attrs;
        let existing = attrs.iter_mut().find(|a| {
            a.name == name
                && match (&a.ns, &ns) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x.href == y.href,
                    _ => false,
                }
        });
        if let Some(a) = existing {
            a.value = value.to_vec();
            if ns.is_some() {
                a.ns = ns;
            }
        } else {
            attrs.push(Attr {
                name: name.to_vec(),
                ns,
                value: value.to_vec(),
            });
        }
    }

    /// `xmlSetProp`: a `prefix:local` name binds to the in-scope prefix
    /// when it is declared.
    pub fn set_prop(&mut self, n: NodeId, name: &[u8], value: &[u8]) {
        if let Some(colon) = name.iter().position(|&c| c == b':') {
            let (prefix, local) = (&name[..colon], &name[colon + 1..]);
            if let Some(ns) = self.search_ns(n, Some(prefix)) {
                self.set_ns_prop(n, Some(ns), local, value);
                return;
            }
        }
        self.set_ns_prop(n, None, name, value);
    }

    /// Removes all attributes named `name` regardless of namespace.
    pub fn remove_attr(&mut self, n: NodeId, name: &[u8]) {
        self.nodes[n].attrs.retain(|a| a.name != name);
    }

    // ---- building -----------------------------------------------------------

    /// `xmlNewDocNode(doc, NULL, name, NULL)`: a detached element.
    pub fn new_element(&mut self, name: &[u8]) -> NodeId {
        self.alloc(NodeData::new(Kind::Element, name))
    }

    /// A detached text node.
    pub fn new_text(&mut self, content: &[u8]) -> NodeId {
        let mut d = NodeData::new(Kind::Text, b"text");
        d.content = content.to_vec();
        self.alloc(d)
    }

    /// A detached text node written verbatim.
    pub fn new_text_noenc(&mut self, content: &[u8]) -> NodeId {
        let mut d = NodeData::new(Kind::TextNoEnc, b"textnoenc");
        d.content = content.to_vec();
        self.alloc(d)
    }

    /// A detached CDATA node.
    pub fn new_cdata(&mut self, content: &[u8]) -> NodeId {
        let mut d = NodeData::new(Kind::CData, b"");
        d.content = content.to_vec();
        self.alloc(d)
    }

    fn new_entity_ref(&mut self, name: &[u8]) -> NodeId {
        self.alloc(NodeData::new(Kind::EntityRef, name))
    }

    /// `xmlNewChild(parent, ns, name, NULL)`.
    pub fn new_child(&mut self, parent: NodeId, ns: Option<Ns>, name: &[u8]) -> NodeId {
        let n = self.new_element(name);
        self.nodes[n].ns = ns;
        self.append_child(parent, n)
    }

    /// `xmlAddChild`: appends `child` (merging adjacent text nodes like
    /// libxml2) and returns the node that holds the content.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> NodeId {
        self.unlink(child);
        if self.nodes[child].kind == Kind::Text {
            if let Some(last) = self.nodes[parent].last {
                if self.nodes[last].kind == Kind::Text {
                    let content = std::mem::take(&mut self.nodes[child].content);
                    self.nodes[last].content.extend_from_slice(&content);
                    return last;
                }
            }
        }
        self.link_last(Parent::Node(parent), child);
        child
    }

    /// Appends a top-level node (`xmlDocSetRootElement` for the first
    /// element).
    pub fn append_top(&mut self, child: NodeId) {
        self.unlink(child);
        self.link_last(Parent::Doc, child);
    }

    fn link_last(&mut self, parent: Parent, child: NodeId) {
        let last = match parent {
            Parent::Node(p) => self.nodes[p].last,
            Parent::Doc => self.last,
            Parent::None => return,
        };
        self.nodes[child].parent = parent;
        self.nodes[child].prev = last;
        self.nodes[child].next = None;
        if let Some(l) = last {
            self.nodes[l].next = Some(child);
        }
        match parent {
            Parent::Node(p) => {
                if self.nodes[p].first.is_none() {
                    self.nodes[p].first = Some(child);
                }
                self.nodes[p].last = Some(child);
            }
            Parent::Doc => {
                if self.first.is_none() {
                    self.first = Some(child);
                }
                self.last = Some(child);
            }
            Parent::None => {}
        }
    }

    /// `xmlUnlinkNode`.
    pub fn unlink(&mut self, n: NodeId) {
        let (parent, prev, next) = {
            let d = &self.nodes[n];
            (d.parent, d.prev, d.next)
        };
        if let Some(p) = prev {
            self.nodes[p].next = next;
        }
        if let Some(x) = next {
            self.nodes[x].prev = prev;
        }
        match parent {
            Parent::Node(p) => {
                if self.nodes[p].first == Some(n) {
                    self.nodes[p].first = next;
                }
                if self.nodes[p].last == Some(n) {
                    self.nodes[p].last = prev;
                }
            }
            Parent::Doc => {
                if self.first == Some(n) {
                    self.first = next;
                }
                if self.last == Some(n) {
                    self.last = prev;
                }
            }
            Parent::None => {}
        }
        let d = &mut self.nodes[n];
        d.parent = Parent::None;
        d.prev = None;
        d.next = None;
    }

    /// Removes all children of `n`.
    pub fn clear_children(&mut self, n: NodeId) {
        while let Some(c) = self.nodes[n].first {
            self.unlink(c);
        }
    }

    /// `xmlNodeSetContent` / `xmlNodeSetContentLen` on an element: replaces
    /// the children with the node list of `content`, where entity and
    /// character references are interpreted (`xmlStringGetNodeList`).
    pub fn set_content(&mut self, n: NodeId, content: &[u8]) {
        if self.nodes[n].kind != Kind::Element {
            self.nodes[n].content = content.to_vec();
            return;
        }
        self.clear_children(n);
        for piece in parse_content(content) {
            let c = match piece {
                ContentPiece::Text(t) => self.new_text(&t),
                ContentPiece::EntityRef(name) => self.new_entity_ref(&name),
            };
            self.append_child(n, c);
        }
    }

    /// Appends a text child holding `content` verbatim (`xmlNewTextLen` +
    /// `xmlAddChild`).
    pub fn add_text(&mut self, n: NodeId, content: &[u8]) -> NodeId {
        let t = self.new_text(content);
        self.append_child(n, t)
    }

    /// `xmlNodeGetContent` for elements: concatenated text of descendants.
    #[must_use]
    pub fn text_content(&self, n: NodeId) -> Vec<u8> {
        let mut out = Vec::new();
        self.collect_text(n, &mut out);
        out
    }

    fn collect_text(&self, n: NodeId, out: &mut Vec<u8>) {
        match self.nodes[n].kind {
            Kind::Text | Kind::TextNoEnc | Kind::CData => {
                out.extend_from_slice(&self.nodes[n].content);
            }
            Kind::Element => {
                for c in self.children(n) {
                    self.collect_text(c, out);
                }
            }
            _ => {}
        }
    }

    /// `xmlDocCopyNode(node, doc, 1)`: deep copy of a node of `src` into
    /// this document (detached). Namespaces used but declared outside the
    /// copied subtree are declared on the copy.
    pub fn import(&mut self, src: &Doc, n: NodeId) -> NodeId {
        let root = self.copy_rec(src, n);
        self.reconcile_copy(src, n, root, root);
        root
    }

    fn copy_rec(&mut self, src: &Doc, n: NodeId) -> NodeId {
        let mut d = src.nodes[n].clone();
        d.parent = Parent::None;
        d.first = None;
        d.last = None;
        d.next = None;
        d.prev = None;
        let id = self.alloc(d);
        let kids: Vec<NodeId> = src.children(n).collect();
        for c in kids {
            let cc = self.copy_rec(src, c);
            self.link_last(Parent::Node(id), cc);
        }
        id
    }

    fn reconcile_copy(&mut self, src: &Doc, src_n: NodeId, dst_n: NodeId, dst_root: NodeId) {
        if self.nodes[dst_n].kind != Kind::Element {
            return;
        }
        if let Some(ns) = self.nodes[dst_n].ns.clone() {
            if ns.prefix.as_deref() != Some(b"xml".as_slice())
                && self.search_ns(dst_n, ns.prefix.as_deref()).is_none()
            {
                if let Some(orig) = src.search_ns(src_n, ns.prefix.as_deref()) {
                    self.new_ns(dst_root, &orig.href, orig.prefix.as_deref());
                }
            }
        }
        let pairs: Vec<(NodeId, NodeId)> = src.children(src_n).zip(self.children(dst_n)).collect();
        for (s, d) in pairs {
            self.reconcile_copy(src, s, d, dst_root);
        }
    }

    // ---- serialization --------------------------------------------------------

    /// `xmlDocDumpMemory`.
    #[must_use]
    pub fn dump(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"<?xml version=\"1.0\"");
        if let Some(enc) = &self.encoding {
            out.extend_from_slice(b" encoding=\"");
            out.extend_from_slice(enc);
            out.push(b'"');
        }
        out.extend_from_slice(b"?>\n");
        let mode = Escaping {
            text_char_refs: self.encoding.is_none(),
            attr_char_refs: self.encoding.is_none(),
        };
        for n in self.top() {
            if self.kind(n) == Kind::Dtd {
                continue;
            }
            self.dump_node_into(n, &mut out, mode);
            out.push(b'\n');
        }
        out
    }

    /// `xmlNodeDump(buf, NULL, node, 0, 0)`.
    #[must_use]
    pub fn dump_node(&self, n: NodeId) -> Vec<u8> {
        let mut out = Vec::new();
        let mode = Escaping {
            text_char_refs: true,
            attr_char_refs: self.encoding.is_none(),
        };
        self.dump_node_into(n, &mut out, mode);
        out
    }

    fn dump_node_into(&self, n: NodeId, out: &mut Vec<u8>, mode: Escaping) {
        let d = &self.nodes[n];
        match d.kind {
            Kind::Element => {
                out.push(b'<');
                write_qname(out, d.ns.as_ref(), &d.name);
                for ns in &d.ns_defs {
                    if ns.prefix.as_deref() == Some(b"xml".as_slice()) {
                        continue;
                    }
                    out.extend_from_slice(b" xmlns");
                    if let Some(p) = &ns.prefix {
                        out.push(b':');
                        out.extend_from_slice(p);
                    }
                    out.push(b'=');
                    write_quoted(out, &ns.href);
                }
                for a in &d.attrs {
                    out.push(b' ');
                    write_qname(out, a.ns.as_ref(), &a.name);
                    out.extend_from_slice(b"=\"");
                    escape_attr(out, &a.value, mode.attr_char_refs);
                    out.push(b'"');
                }
                if d.first.is_none() {
                    out.extend_from_slice(b"/>");
                } else {
                    out.push(b'>');
                    for c in self.children(n) {
                        self.dump_node_into(c, out, mode);
                    }
                    out.extend_from_slice(b"</");
                    write_qname(out, d.ns.as_ref(), &d.name);
                    out.push(b'>');
                }
            }
            Kind::Text => escape_text(out, &d.content, mode.text_char_refs),
            Kind::TextNoEnc => out.extend_from_slice(&d.content),
            Kind::CData => {
                let mut rest: &[u8] = &d.content;
                out.extend_from_slice(b"<![CDATA[");
                while let Some(pos) = find(rest, b"]]>") {
                    out.extend_from_slice(&rest[..pos + 2]);
                    out.extend_from_slice(b"]]><![CDATA[");
                    rest = &rest[pos + 2..];
                }
                out.extend_from_slice(rest);
                out.extend_from_slice(b"]]>");
            }
            Kind::EntityRef => {
                out.push(b'&');
                out.extend_from_slice(&d.name);
                out.push(b';');
            }
            Kind::Comment => {
                out.extend_from_slice(b"<!--");
                out.extend_from_slice(&d.content);
                out.extend_from_slice(b"-->");
            }
            Kind::Pi => {
                out.extend_from_slice(b"<?");
                out.extend_from_slice(&d.name);
                if !d.content.is_empty() {
                    out.push(b' ');
                    out.extend_from_slice(&d.content);
                }
                out.extend_from_slice(b"?>");
            }
            Kind::Dtd => {}
        }
    }

    // ---- parsing ----------------------------------------------------------------

    /// `soap_xmlParseMemory`: parses a SOAP message.
    pub fn parse(data: &[u8]) -> Result<Doc, ParseFailure> {
        Self::parse_with(data, None, false)
    }

    /// `soap_xmlParseFile` over already-fetched bytes: parses a WSDL or
    /// schema document and removes blank text, comments and processing
    /// instructions (`cleanup_xml_node`).
    pub fn parse_wsdl(data: &[u8], url: &str) -> Result<Doc, ParseFailure> {
        Self::parse_with(data, Some(url), true)
    }

    fn parse_with(data: &[u8], url: Option<&str>, cleanup: bool) -> Result<Doc, ParseFailure> {
        let options = ParseOptions {
            options: php_libxml::ffi::XML_PARSE_HUGE
                | php_libxml::ffi::XML_PARSE_NONET
                | php_libxml::ffi::XML_PARSE_NOBLANKS,
            url: url.map(|u| u.as_bytes().to_vec()),
            encoding: None,
            channels: ParserChannels::SILENT,
        };
        php_libxml::raw::errors::reset_last_error();
        let lx = Document::parse_memory(data, &options).map_err(|_| {
            ParseFailure::Malformed(
                php_libxml::raw::errors::last_error()
                    .map(|e| String::from_utf8_lossy(&e.message).into_owned()),
            )
        })?;
        let mut doc = Doc {
            url: url.map(str::to_owned),
            encoding: lx.encoding(),
            ..Doc::default()
        };
        let docnode = lx.as_node();
        // Iterative conversion: (libxml node, parent in our tree, depth).
        let mut stack: Vec<(LxNode, Parent, usize)> = docnode
            .children()
            .into_iter()
            .rev()
            .map(|c| (c, Parent::Doc, 0))
            .collect();
        while let Some((lxn, parent, depth)) = stack.pop() {
            let kind = match lxn.node_type() {
                NodeType::Element => Kind::Element,
                NodeType::Text => Kind::Text,
                NodeType::CData => Kind::CData,
                NodeType::EntityRef => Kind::EntityRef,
                NodeType::ProcessingInstruction => Kind::Pi,
                NodeType::Dtd | NodeType::DocumentType => Kind::Dtd,
                _ => continue, // comments are dropped by the SOAP SAX handler
            };
            if cleanup && !matches!(kind, Kind::Element | Kind::Text | Kind::CData) {
                continue;
            }
            let mut d = NodeData::new(kind, &lxn.name());
            if let Ok(line) = u32::try_from(lxn.line()) {
                d.line = line;
            }
            match kind {
                Kind::Element => {
                    d.ns = lxn.namespace().map(|ns| Ns {
                        prefix: ns.prefix,
                        href: ns.href,
                    });
                    d.ns_defs = lxn
                        .namespace_declarations()
                        .into_iter()
                        .map(|ns| Ns {
                            prefix: ns.prefix,
                            href: ns.href,
                        })
                        .collect();
                    d.attrs = lxn
                        .attributes()
                        .into_iter()
                        .map(|a| Attr {
                            name: a.name(),
                            ns: a.namespace().map(|ns| Ns {
                                prefix: ns.prefix,
                                href: ns.href,
                            }),
                            value: a.content(),
                        })
                        .collect();
                }
                Kind::Text | Kind::CData | Kind::Pi => d.content = lxn.content(),
                _ => {}
            }
            if kind == Kind::Text {
                // Merge with a preceding text node (comments between them
                // were never materialized by the SOAP parser).
                let last = match parent {
                    Parent::Node(p) => doc.nodes[p].last,
                    Parent::Doc => doc.last,
                    Parent::None => None,
                };
                if let Some(l) = last {
                    if doc.nodes[l].kind == Kind::Text {
                        doc.nodes[l].content.extend_from_slice(&d.content);
                        continue;
                    }
                }
                if cleanup && is_blank(&d.content) {
                    continue;
                }
            }
            let id = doc.alloc(d);
            doc.link_last(parent, id);
            if kind == Kind::Element {
                let kids = lxn.children();
                if !kids.is_empty() {
                    if depth + 1 > SOAP_MAX_XML_DEPTH {
                        return Err(ParseFailure::TooDeep);
                    }
                    for c in kids.into_iter().rev() {
                        stack.push((c, Parent::Node(id), depth + 1));
                    }
                }
            }
        }
        if cleanup {
            doc.remove_blank_texts();
        }
        Ok(doc)
    }

    fn remove_blank_texts(&mut self) {
        let all: Vec<NodeId> = (0..self.nodes.len()).collect();
        for n in all {
            if self.nodes[n].kind == Kind::Text
                && self.nodes[n].parent != Parent::None
                && is_blank(&self.nodes[n].content)
            {
                self.unlink(n);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Escaping {
    text_char_refs: bool,
    attr_char_refs: bool,
}

fn is_blank(s: &[u8]) -> bool {
    s.iter().all(|&c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn write_qname(out: &mut Vec<u8>, ns: Option<&Ns>, name: &[u8]) {
    if let Some(p) = ns.and_then(|ns| ns.prefix.as_ref()) {
        out.extend_from_slice(p);
        out.push(b':');
    }
    out.extend_from_slice(name);
}

/// `xmlBufWriteQuotedString` with the href escaping of libxml2 >= 2.13
/// (ext/soap pre-escapes generated namespaces for older versions).
fn write_quoted(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &c in s {
        match c {
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'\r' => out.extend_from_slice(b"&#13;"),
            _ => out.push(c),
        }
    }
    out.push(b'"');
}

/// `xmlSerializeHexCharRef`.
fn hex_char_ref(out: &mut Vec<u8>, val: u32) {
    out.extend_from_slice(format!("&#x{val:X};").as_bytes());
}

/// Decodes one UTF-8 sequence starting at `s[0]` (`>= 0x80`); returns the
/// code point and its length.
fn utf8_at(s: &[u8]) -> Option<(u32, usize)> {
    let c = s[0];
    let (len, init) = if c & 0xe0 == 0xc0 {
        (2, u32::from(c & 0x1f))
    } else if c & 0xf0 == 0xe0 {
        (3, u32::from(c & 0x0f))
    } else if c & 0xf8 == 0xf0 {
        (4, u32::from(c & 0x07))
    } else {
        return None;
    };
    if s.len() < len {
        return None;
    }
    let mut v = init;
    for &b in &s[1..len] {
        if b & 0xc0 != 0x80 {
            return None;
        }
        v = (v << 6) | u32::from(b & 0x3f);
    }
    Some((v, len))
}

/// Text escaping: `xmlEscapeContent` (document dump with an encoding) or
/// `xmlEscapeEntities` (no encoding: non-ASCII and control characters
/// become hexadecimal character references).
fn escape_text(out: &mut Vec<u8>, s: &[u8], char_refs: bool) {
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            b'\r' if !char_refs => out.extend_from_slice(b"&#13;"),
            _ if !char_refs => out.push(c),
            0x20..=0x7f | b'\n' | b'\t' => out.push(c),
            0x80.. => {
                if let Some((v, len)) = utf8_at(&s[i..]) {
                    hex_char_ref(out, v);
                    i += len;
                    continue;
                }
                // Invalid UTF-8: libxml2 stops escaping; emit the byte.
                out.push(c);
            }
            _ => hex_char_ref(out, u32::from(c)),
        }
        i += 1;
    }
}

/// `xmlBufAttrSerializeTxtContent`.
fn escape_attr(out: &mut Vec<u8>, s: &[u8], char_refs: bool) {
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'\n' => out.extend_from_slice(b"&#10;"),
            b'\r' => out.extend_from_slice(b"&#13;"),
            b'\t' => out.extend_from_slice(b"&#9;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            0x80.. if char_refs && i + 1 < s.len() => {
                if let Some((v, len)) = utf8_at(&s[i..]) {
                    hex_char_ref(out, v);
                    i += len;
                    continue;
                }
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
}

enum ContentPiece {
    Text(Vec<u8>),
    EntityRef(Vec<u8>),
}

/// `xmlStringGetNodeList`: splits element content into text and entity
/// references, decoding predefined entities and character references.
fn parse_content(s: &[u8]) -> Vec<ContentPiece> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'&' {
            buf.push(s[i]);
            i += 1;
            continue;
        }
        let Some(end) = s[i..].iter().position(|&c| c == b';').map(|p| p + i) else {
            // Unterminated reference: libxml2 reports an error and stops.
            break;
        };
        let body = &s[i + 1..end];
        if let Some(num) = body.strip_prefix(b"#") {
            let v = if let Some(hex) = num.strip_prefix(b"x") {
                u32::from_str_radix(std::str::from_utf8(hex).unwrap_or("!"), 16).ok()
            } else {
                std::str::from_utf8(num).ok().and_then(|d| d.parse().ok())
            };
            if let Some(ch) = v.and_then(char::from_u32).filter(|&c| c != '\0') {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
            }
        } else {
            match body {
                b"lt" => buf.push(b'<'),
                b"gt" => buf.push(b'>'),
                b"amp" => buf.push(b'&'),
                b"apos" => buf.push(b'\''),
                b"quot" => buf.push(b'"'),
                _ => {
                    if !buf.is_empty() {
                        out.push(ContentPiece::Text(std::mem::take(&mut buf)));
                    }
                    out.push(ContentPiece::EntityRef(body.to_vec()));
                }
            }
        }
        i = end + 1;
    }
    if !buf.is_empty() {
        out.push(ContentPiece::Text(buf));
    }
    out
}

/// `whiteSpace_replace`.
#[must_use]
pub fn whitespace_replace(s: &[u8]) -> Vec<u8> {
    s.iter()
        .map(|&c| if matches!(c, b'\t' | b'\n' | b'\r') { b' ' } else { c })
        .collect()
}

/// `whiteSpace_collapse`.
#[must_use]
pub fn whitespace_collapse(s: &[u8]) -> Vec<u8> {
    let s = whitespace_replace(s);
    let mut out = Vec::with_capacity(s.len());
    let mut iter = s.iter().copied().skip_while(|&c| c == b' ').peekable();
    let mut old = 0u8;
    while let Some(c) = iter.next() {
        if c != b' ' || old != b' ' {
            out.push(c);
        }
        old = c;
    }
    if old == b' ' {
        out.pop();
    }
    out
}

/// `parse_namespace`: splits `prefix:local` at the last colon (a leading
/// colon does not count).
#[must_use]
pub fn parse_qname(s: &[u8]) -> (Option<&[u8]>, &[u8]) {
    match s.iter().rposition(|&c| c == b':') {
        Some(p) if p > 0 => (Some(&s[..p]), &s[p + 1..]),
        _ => (None, s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_dump() {
        let mut d = Doc::new_utf8();
        let env = d.new_element(b"Envelope");
        let ns = d.new_ns(env, b"http://e/", Some(b"E")).unwrap();
        d.set_ns(env, Some(ns.clone()));
        d.append_top(env);
        let body = d.new_child(env, Some(ns), b"Body");
        let p = d.new_element(b"param");
        d.append_child(body, p);
        d.add_text(p, b"a<b>&\r\"\xc3\xa9");
        d.set_prop(p, b"E:attr", b"x\"\n\t<");
        let e = d.new_element(b"empty");
        d.append_child(body, e);
        let e2 = d.new_element(b"emptytext");
        d.append_child(body, e2);
        d.add_text(e2, b"");
        assert_eq!(
            d.dump(),
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<E:Envelope xmlns:E=\"http://e/\"><E:Body><param E:attr=\"x&quot;&#10;&#9;&lt;\">a&lt;b&gt;&amp;&#13;\"\xc3\xa9</param><empty/><emptytext></emptytext></E:Body></E:Envelope>\n".to_vec()
        );
        assert_eq!(d.dump_node(p), b"<param E:attr=\"x&quot;&#10;&#9;&lt;\">a&lt;b&gt;&amp;&#xD;\"&#xE9;</param>".to_vec());
    }

    #[test]
    fn set_content_parses_references() {
        let mut d = Doc::new();
        let e = d.new_element(b"a");
        d.append_top(e);
        d.set_content(e, b"x &lt; &#65;&#x42; &foo; y");
        assert_eq!(d.dump_node(e), b"<a>x &lt; AB &foo; y</a>".to_vec());
        d.set_content(e, b"");
        assert_eq!(d.dump_node(e), b"<a/>".to_vec());
    }

    #[test]
    fn namespace_search() {
        let mut d = Doc::new();
        let r = d.new_element(b"r");
        d.append_top(r);
        d.new_ns(r, b"urn:a", Some(b"a"));
        d.new_ns(r, b"urn:def", None);
        let c = d.new_child(r, None, b"c");
        d.new_ns(c, b"urn:b", Some(b"a"));
        assert_eq!(d.search_ns(c, Some(b"a")).unwrap().href, b"urn:b");
        assert_eq!(d.search_ns(c, None).unwrap().href, b"urn:def");
        assert!(d.search_ns_by_href(c, b"urn:a").is_none(), "shadowed prefix");
        assert_eq!(d.search_ns_by_href(r, b"urn:a").unwrap().prefix.unwrap(), b"a");
        assert_eq!(d.search_ns_by_href(c, XML_NAMESPACE.as_bytes()).unwrap().prefix.unwrap(), b"xml");
        assert!(d.new_ns(r, b"urn:x", Some(b"a")).is_none());
    }

    #[test]
    fn parse_roundtrip_and_lookup() {
        let src = b"<?xml version=\"1.0\"?>\n<s:E xmlns:s=\"urn:s\"><!-- c --><s:B a=\"1\">x<!--y-->z<i id=\"q\"/></s:B>\n  <C/></s:E>";
        let d = Doc::parse(src).unwrap();
        let root = d.root().unwrap();
        assert!(d.node_is(root, Some("E"), Some("urn:s")));
        let b = d.get_node(d.first_child(root), "B", Some("urn:s")).unwrap();
        let t = d.first_child(b).unwrap();
        assert_eq!(d.content(t), b"xz");
        assert_eq!(d.find_by_attribute_recursive(d.first_top(), "id", b"q", None), d.last_child(b));
        assert_eq!(d.dump(), b"<?xml version=\"1.0\"?>\n<s:E xmlns:s=\"urn:s\"><s:B a=\"1\">xz<i id=\"q\"/></s:B><C/></s:E>\n".to_vec());
    }

    #[test]
    fn parse_failures() {
        assert!(matches!(Doc::parse(b"not xml"), Err(ParseFailure::Malformed(_))));
        let mut deep = Vec::new();
        for _ in 0..2100 {
            deep.extend_from_slice(b"<a>");
        }
        for _ in 0..2100 {
            deep.extend_from_slice(b"</a>");
        }
        assert_eq!(Doc::parse(&deep).unwrap_err(), ParseFailure::TooDeep);
        let d = Doc::parse(b"<!DOCTYPE a><a/>").unwrap();
        assert!(d.has_dtd());
    }

    #[test]
    fn whitespace_helpers() {
        assert_eq!(whitespace_collapse(b"  a \t\n b  "), b"a b");
        assert_eq!(whitespace_replace(b"a\tb"), b"a b");
        assert_eq!(parse_qname(b"xsd:int"), (Some(&b"xsd"[..]), &b"int"[..]));
        assert_eq!(parse_qname(b":int"), (None, &b":int"[..]));
    }
}
