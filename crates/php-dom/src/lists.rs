//! Live node collections: `DOMNodeList`, `DOMNamedNodeMap`,
//! `Dom\NodeList`, `Dom\NamedNodeMap`, `Dom\DtdNamedNodeMap` and
//! `Dom\HTMLCollection` (`obj_map.c`, `nodelist.c`, `namednodemap.c`,
//! `html_collection.c`, `dom_iterators.c`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::error::{DomError, DomResult};
use crate::ffi;
use crate::object::Node;
use crate::raw::{cs, N};
use crate::tree::{first_child_of_container, next_in_tree_order};

/// What a collection enumerates (`php_dom_obj_map_handler`).
#[derive(Debug, Clone)]
pub(crate) enum MapKind {
    /// `php_dom_obj_map_attributes`.
    Attributes,
    /// `php_dom_obj_map_by_tag_name` (`ns == None` matches any namespace).
    ByTagName {
        ns: Option<Vec<u8>>,
        local: Vec<u8>,
        local_lower: Vec<u8>,
    },
    /// `php_dom_obj_map_by_class_name`.
    ByClassName { classes: Vec<Vec<u8>> },
    /// `php_dom_obj_map_child_nodes`.
    ChildNodes,
    /// `php_dom_obj_map_child_elements`.
    ChildElements,
    /// `php_dom_obj_map_nodeset`: a fixed list.
    NodeSet(Vec<Node>),
    /// `php_dom_obj_map_entities`.
    Entities,
    /// `php_dom_obj_map_notations`.
    Notations,
    /// `php_dom_obj_map_noop`.
    Noop,
}

impl MapKind {
    fn use_cache(&self) -> bool {
        matches!(
            self,
            Self::ByTagName { .. } | Self::ByClassName { .. } | Self::ChildNodes | Self::ChildElements
        )
    }
    fn nameless(&self) -> bool {
        !matches!(self, Self::Attributes | Self::Entities | Self::Notations)
    }
}

/// The collection state (`dom_nnodemap_object`).
pub(crate) struct ObjMap {
    class: &'static str,
    base: Option<Node>,
    kind: MapKind,
    cached_length: Cell<Option<i64>>,
    modification_nr: Cell<u64>,
    cached_obj: RefCell<Option<(Node, i64)>>,
}

impl std::fmt::Debug for ObjMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjMap")
            .field("class", &self.class)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// A node list (`DOMNodeList`, `Dom\NodeList` or `Dom\HTMLCollection`).
#[derive(Debug, Clone)]
pub struct NodeList(pub(crate) Rc<ObjMap>);

/// A named node map (`DOMNamedNodeMap`, `Dom\NamedNodeMap`,
/// `Dom\DtdNamedNodeMap`).
#[derive(Debug, Clone)]
pub struct NamedNodeMap(pub(crate) Rc<ObjMap>);

impl PartialEq for NodeList {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}
impl PartialEq for NamedNodeMap {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}

/// `php_dom_follow_spec_node`.
pub(crate) fn follow_spec_node(n: N) -> bool {
    Node::existing(n).is_some_and(|o| o.is_modern())
}

/// `dom_get_elements_by_tag_name_ns_raw`.
pub(crate) fn elements_by_tag_name_ns_raw(
    basep: N,
    start: Option<N>,
    ns: Option<&[u8]>,
    local: &[u8],
    local_lower: &[u8],
    cur: &mut i64,
    index: i64,
) -> Option<N> {
    let mut nodep = start?;
    let local_match_any = local == b"*";
    let ns_match_any = ns.is_none_or(|n| n == b"*");
    let match_qname = ns.is_none() && follow_spec_node(basep);
    while *cur <= index {
        if nodep.is_element() {
            let name_ok = local_match_any || {
                if match_qname {
                    let use_lower = nodep.doc().is_some_and(|d| d.t() == ffi::XML_HTML_DOCUMENT_NODE)
                        && crate::ns::ns_is_fast(nodep, &crate::ns::HTML_TOKEN);
                    crate::tree::match_qualified_name(if use_lower { local_lower } else { local }, nodep)
                } else {
                    nodep.name() == local
                }
            };
            if name_ok {
                let ns_ok = ns_match_any
                    || (ns == Some(b"") && nodep.ns().is_none())
                    || nodep.ns().is_some_and(|x| Some(x.href()) == ns);
                if ns_ok {
                    if *cur == index {
                        return Some(nodep);
                    }
                    *cur += 1;
                }
            }
        }
        nodep = next_in_tree_order(nodep, Some(basep))?;
    }
    None
}

/// `dom_nodelist_iter_start_first_child`.
pub(crate) fn iter_start_first_child(n: N) -> Option<N> {
    if n.t() == ffi::XML_ENTITY_REF_NODE {
        crate::node::entity_reference_sync(n);
    }
    n.children()
}

fn hash_size(ht: *mut ffi::xmlHashTable) -> i64 {
    if ht.is_null() {
        0
    } else {
        // SAFETY: live DTD hash table.
        i64::from(unsafe { ffi::xmlHashSize(ht) })
    }
}

struct HashIter {
    cur: i64,
    index: i64,
    node: *mut std::ffi::c_void,
}

unsafe extern "C" fn item_hash_scanner(payload: *mut std::ffi::c_void, data: *mut std::ffi::c_void, _name: *const u8) {
    // SAFETY: data is the HashIter passed to xmlHashScan.
    let it = unsafe { &mut *data.cast::<HashIter>() };
    if it.cur < it.index {
        it.cur += 1;
    } else if it.node.is_null() {
        it.node = payload;
    }
}

/// `php_dom_libxml_hash_iter`.
pub(crate) fn hash_iter(ht: *mut ffi::xmlHashTable, index: i64) -> DomResult<*mut std::ffi::c_void> {
    if index < 0 {
        return Ok(std::ptr::null_mut());
    }
    if index > i64::from(i32::MAX) {
        return Err(DomError::Value(format!("must be between 0 and {}", i32::MAX)));
    }
    let size = hash_size(ht);
    if size > 0 && index < size {
        let mut it = HashIter {
            cur: 0,
            index,
            node: std::ptr::null_mut(),
        };
        // SAFETY: live hash table; the scanner only writes into `it`.
        unsafe { ffi::xmlHashScan(ht, Some(item_hash_scanner), (&raw mut it).cast()) };
        Ok(it.node)
    } else {
        Ok(std::ptr::null_mut())
    }
}

/// `create_notation`: a fake notation node (entity layout).
pub(crate) fn create_notation(dtd: Option<N>, name: *const u8, public: *const u8, system: *const u8) -> Option<N> {
    // SAFETY: allocation with libxml2's allocator, zeroed; strings
    // duplicated (xmlStrdup(NULL) returns NULL).
    unsafe {
        let malloc = ffi::xmlMalloc?;
        let p = malloc(std::mem::size_of::<ffi::xmlEntity>()).cast::<ffi::xmlEntity>();
        if p.is_null() {
            return None;
        }
        std::ptr::write_bytes(p, 0, 1);
        (*p).type_ = ffi::XML_NOTATION_NODE;
        (*p).name = ffi::xmlStrdup(name);
        (*p).ExternalID = ffi::xmlStrdup(public);
        (*p).SystemID = ffi::xmlStrdup(system);
        if let Some(dtd) = dtd {
            (*p).parent = dtd.as_dtd();
            (*p).doc = dtd.doc_ptr();
        }
        N::new(p.cast())
    }
}

fn notation_node(dtd: Option<N>, p: *mut std::ffi::c_void) -> Option<N> {
    if p.is_null() {
        return None;
    }
    let nota = p.cast::<ffi::xmlNotation>();
    // SAFETY: p is an xmlNotation from the DTD's notation table.
    unsafe { create_notation(dtd, (*nota).name, (*nota).PublicID, (*nota).SystemID) }
}

impl ObjMap {
    pub(crate) fn new(class: &'static str, base: Option<Node>, kind: MapKind) -> ObjMap {
        ObjMap {
            class,
            base,
            kind,
            cached_length: Cell::new(None),
            modification_nr: Cell::new(0),
            cached_obj: RefCell::new(None),
        }
    }

    fn base_node(&self) -> Option<N> {
        self.base.as_ref().and_then(Node::n)
    }

    fn doc_nr(&self, n: N) -> Option<u64> {
        let obj = Node::existing(n)?;
        let doc = obj.doc_ref()?;
        Some(doc.modification_nr.get())
    }

    fn is_stale(&self, n: N) -> bool {
        self.doc_nr(n) != Some(self.modification_nr.get())
    }

    fn mark_up_to_date(&self, n: N) {
        if let Some(nr) = self.doc_nr(n) {
            self.modification_nr.set(nr);
        }
    }

    fn reset_cache(&self) {
        *self.cached_obj.borrow_mut() = None;
        self.cached_length.set(None);
    }

    fn ht(&self) -> *mut ffi::xmlHashTable {
        match (self.base_node(), &self.kind) {
            (Some(b), MapKind::Entities) => b.dtd().entities,
            (Some(b), MapKind::Notations) => b.dtd().notations,
            _ => std::ptr::null_mut(),
        }
    }

    fn matches_class_name(&self, classes: &[Vec<u8>], n: N) -> bool {
        if !n.is_element() {
            return false;
        }
        let name = cs(b"class");
        // SAFETY: live element.
        let attr = N::from_attr(unsafe { ffi::xmlHasNsProp(n.p(), name.as_ptr().cast(), std::ptr::null()) });
        let Some(attr) = attr else { return false };
        let value = crate::tree::attr_value(attr);
        let quirks = self
            .base
            .as_ref()
            .and_then(Node::doc_ref)
            .is_some_and(|d| d.quirks_mode.get() == crate::object::QuirksMode::Quirks);
        crate::tokenlist::ordered_set_all_contained(classes, &value, quirks)
    }

    /// The handler's raw length.
    fn raw_length(&self) -> i64 {
        let Some(nodep) = self.base_node() else {
            return match &self.kind {
                MapKind::NodeSet(v) => i64::try_from(v.len()).unwrap_or(i64::MAX),
                _ => 0,
            };
        };
        match &self.kind {
            MapKind::Entities | MapKind::Notations => hash_size(self.ht()),
            MapKind::NodeSet(v) => i64::try_from(v.len()).unwrap_or(i64::MAX),
            MapKind::Attributes => {
                i64::try_from(nodep.attr_iter().count()).unwrap_or(i64::MAX)
            }
            MapKind::ChildNodes => {
                let mut count = 0;
                let mut c = iter_start_first_child(nodep);
                while let Some(x) = c {
                    count += 1;
                    c = x.next();
                }
                count
            }
            MapKind::ChildElements => {
                let mut count = 0;
                let mut c = iter_start_first_child(nodep);
                while let Some(x) = c {
                    if x.is_element() {
                        count += 1;
                    }
                    c = x.next();
                }
                count
            }
            MapKind::ByTagName { ns, local, local_lower } => {
                let mut count = 0;
                elements_by_tag_name_ns_raw(
                    nodep,
                    first_child_of_container(nodep),
                    ns.as_deref(),
                    local,
                    local_lower,
                    &mut count,
                    i64::MAX - 1,
                );
                count
            }
            MapKind::ByClassName { classes } => {
                let mut count = 0;
                let mut c = first_child_of_container(nodep);
                while let Some(x) = c {
                    if self.matches_class_name(classes, x) {
                        count += 1;
                    }
                    c = next_in_tree_order(x, Some(nodep));
                }
                count
            }
            MapKind::Noop => 0,
        }
    }

    /// `php_dom_get_nodelist_length` / `php_dom_get_namednodemap_length`.
    pub(crate) fn length(&self) -> i64 {
        if self.kind.use_cache() {
            let Some(nodep) = self.base_node() else {
                return 0;
            };
            if self.is_stale(nodep) {
                self.mark_up_to_date(nodep);
                self.reset_cache();
            } else if let Some(l) = self.cached_length.get() {
                return l;
            }
            let count = self.raw_length();
            self.cached_length.set(Some(count));
            count
        } else {
            self.raw_length()
        }
    }

    /// `dom_obj_map_get_start_point`.
    fn start_point(&self, basep: N, index: i64) -> (Option<N>, i64) {
        let cached = self.cached_obj.borrow().clone();
        if let Some((obj, cidx)) = cached {
            if index >= cidx && !self.is_stale(basep) {
                if let Some(n) = obj.n() {
                    return (Some(n), index - cidx);
                }
                self.reset_cache();
            }
        }
        (None, index)
    }

    fn cache_obj(&self, item: &Node, n: N, index: i64) {
        if self.is_stale(n) {
            self.mark_up_to_date(n);
            self.reset_cache();
        }
        *self.cached_obj.borrow_mut() = Some((item.clone(), index));
    }

    fn wrap(n: Option<N>) -> Option<Node> {
        n.map(Node::wrap)
    }

    /// `handler->get_item`.
    pub(crate) fn item(&self, index: i64) -> DomResult<Option<Node>> {
        if let MapKind::NodeSet(v) = &self.kind {
            return Ok(usize::try_from(index).ok().and_then(|i| v.get(i).cloned()));
        }
        let Some(nodep) = self.base_node() else {
            return Ok(None);
        };
        match &self.kind {
            MapKind::Entities => {
                let p = if self.ht().is_null() {
                    std::ptr::null_mut()
                } else {
                    hash_iter(self.ht(), index)?
                };
                Ok(Self::wrap(N::new(p.cast())))
            }
            MapKind::Notations => {
                let p = if self.ht().is_null() {
                    std::ptr::null_mut()
                } else {
                    hash_iter(self.ht(), index)?
                };
                Ok(Self::wrap(notation_node(Some(nodep), p)))
            }
            MapKind::Attributes => {
                if index < 0 {
                    return Ok(None);
                }
                let item = usize::try_from(index)
                    .ok()
                    .and_then(|i| nodep.attr_iter().nth(i));
                Ok(Self::wrap(item))
            }
            MapKind::ChildNodes => {
                if index < 0 {
                    return Ok(None);
                }
                let (start, mut rel) = self.start_point(nodep, index);
                let mut item = match start {
                    Some(s) => Some(s),
                    None => iter_start_first_child(nodep),
                };
                while rel > 0 {
                    let Some(i) = item else { break };
                    item = i.next();
                    rel -= 1;
                }
                Ok(self.finish_item(item, index))
            }
            MapKind::ChildElements => {
                if index < 0 {
                    return Ok(None);
                }
                let (start, mut rel) = self.start_point(nodep, index);
                let mut item = match start {
                    Some(s) => Some(s),
                    None => {
                        let mut c = nodep.children();
                        while let Some(x) = c {
                            if x.is_element() {
                                break;
                            }
                            c = x.next();
                        }
                        c
                    }
                };
                while rel > 0 {
                    let Some(mut i) = item else { break };
                    loop {
                        match i.next() {
                            Some(nx) => {
                                i = nx;
                                if i.is_element() {
                                    item = Some(i);
                                    break;
                                }
                            }
                            None => {
                                item = None;
                                break;
                            }
                        }
                    }
                    rel -= 1;
                }
                let item = item.filter(|i| i.is_element());
                Ok(self.finish_item(item, index))
            }
            MapKind::ByTagName { ns, local, local_lower } => {
                if index < 0 {
                    return Ok(None);
                }
                let (start, rel) = self.start_point(nodep, index);
                let s = start.or_else(|| first_child_of_container(nodep));
                let mut count = 0;
                let item = elements_by_tag_name_ns_raw(nodep, s, ns.as_deref(), local, local_lower, &mut count, rel);
                Ok(self.finish_item(item, index))
            }
            MapKind::ByClassName { classes } => {
                if index < 0 {
                    return Ok(None);
                }
                let (start, mut rel) = self.start_point(nodep, index);
                let mut item = match start {
                    Some(s) => Some(s),
                    None => {
                        let mut c = first_child_of_container(nodep);
                        while let Some(x) = c {
                            if self.matches_class_name(classes, x) {
                                break;
                            }
                            c = next_in_tree_order(x, Some(nodep));
                        }
                        c
                    }
                };
                while rel > 0 {
                    let Some(i) = item else { break };
                    let mut c = next_in_tree_order(i, Some(nodep));
                    while let Some(x) = c {
                        if self.matches_class_name(classes, x) {
                            break;
                        }
                        c = next_in_tree_order(x, Some(nodep));
                    }
                    item = c;
                    rel -= 1;
                }
                Ok(self.finish_item(item, index))
            }
            MapKind::NodeSet(_) | MapKind::Noop => Ok(None),
        }
    }

    fn finish_item(&self, item: Option<N>, index: i64) -> Option<Node> {
        let n = item?;
        let obj = Node::wrap(n);
        self.cache_obj(&obj, n, index);
        Some(obj)
    }

    /// `handler->get_ns_named_item`.
    pub(crate) fn named_item(&self, named: &[u8], ns: Option<&[u8]>) -> Option<Node> {
        let nodep = self.base_node()?;
        match &self.kind {
            MapKind::Entities => {
                let ht = self.ht();
                if ht.is_null() {
                    return None;
                }
                let c = cs(named);
                // SAFETY: live hash table.
                let p = unsafe { ffi::xmlHashLookup(ht, c.as_ptr().cast()) };
                Self::wrap(N::new(p.cast()))
            }
            MapKind::Notations => {
                let ht = self.ht();
                if ht.is_null() {
                    return None;
                }
                let c = cs(named);
                // SAFETY: live hash table.
                let p = unsafe { ffi::xmlHashLookup(ht, c.as_ptr().cast()) };
                Self::wrap(notation_node(Some(nodep), p))
            }
            MapKind::Attributes => self.prop_named(nodep, named, ns).map(Node::wrap),
            _ => None,
        }
    }

    fn prop_named(&self, nodep: N, named: &[u8], ns: Option<&[u8]>) -> Option<N> {
        let c = cs(named);
        match ns {
            Some(ns) => {
                let u = cs(ns);
                // SAFETY: live element.
                let item = N::from_attr(unsafe { ffi::xmlHasNsProp(nodep.p(), c.as_ptr().cast(), u.as_ptr().cast()) });
                item.filter(|i| i.t() != ffi::XML_ATTRIBUTE_DECL)
            }
            None => {
                if self.base.as_ref().is_some_and(Node::is_modern) {
                    crate::tree::get_attribute_node(nodep, named)
                } else {
                    // SAFETY: live element.
                    N::from_attr(unsafe { ffi::xmlHasProp(nodep.p(), c.as_ptr().cast()) })
                }
            }
        }
    }

    /// `handler->has_ns_named_item`.
    pub(crate) fn has_named_item(&self, named: &[u8], ns: Option<&[u8]>) -> bool {
        let Some(nodep) = self.base_node() else {
            return false;
        };
        match &self.kind {
            MapKind::Entities | MapKind::Notations => {
                let ht = self.ht();
                if ht.is_null() {
                    return false;
                }
                let c = cs(named);
                // SAFETY: live hash table.
                !unsafe { ffi::xmlHashLookup(ht, c.as_ptr().cast()) }.is_null()
            }
            MapKind::Attributes => self.prop_named(nodep, named, ns).is_some(),
            _ => false,
        }
    }

    /// `HTMLCollection::namedItem` candidate iteration.
    fn collection_named_item(&self, key: &[u8]) -> Option<Node> {
        if key.is_empty() {
            return None;
        }
        let basep = self.base_node()?;
        basep.children()?;
        let mut candidates: Vec<N> = Vec::new();
        match &self.kind {
            MapKind::ChildElements => {
                candidates.extend(basep.child_iter().filter(|c| c.is_element()));
            }
            MapKind::ByTagName { ns, local, local_lower } => {
                let mut start = basep.children();
                let mut cur = 0;
                let mut next = 0;
                loop {
                    let found = elements_by_tag_name_ns_raw(basep, start, ns.as_deref(), local, local_lower, &mut cur, next);
                    let Some(f) = found else { break };
                    candidates.push(f);
                    next = cur + 1;
                    start = Some(f);
                }
            }
            MapKind::ByClassName { classes } => {
                let mut c = first_child_of_container(basep);
                while let Some(x) = c {
                    if self.matches_class_name(classes, x) {
                        candidates.push(x);
                    }
                    c = next_in_tree_order(x, Some(basep));
                }
            }
            _ => return None,
        }
        for cand in candidates {
            if let Some(attr) = crate::element::has_ns_prop(cand, b"id", None) {
                if crate::tree::attr_value(attr) == key {
                    return Some(Node::wrap(cand));
                }
            }
            if crate::ns::ns_is_fast(cand, &crate::ns::HTML_TOKEN) {
                if let Some(attr) = crate::element::has_ns_prop(cand, b"name", None) {
                    if crate::tree::attr_value(attr) == key {
                        return Some(Node::wrap(cand));
                    }
                }
            }
        }
        None
    }
}

/// Creates a collection object.
pub(crate) fn new_list(class: &'static str, base: Option<Node>, kind: MapKind) -> NodeList {
    NodeList(Rc::new(ObjMap::new(class, base, kind)))
}

/// Creates a named node map object.
pub(crate) fn new_map(class: &'static str, base: Option<Node>, kind: MapKind) -> NamedNodeMap {
    NamedNodeMap(Rc::new(ObjMap::new(class, base, kind)))
}

/// An array-access offset for `$list[...]`.
#[derive(Debug, Clone)]
pub enum Offset {
    /// Integer offset.
    Int(i64),
    /// Float offset.
    Float(f64),
    /// String offset.
    Str(Vec<u8>),
    /// `null`.
    Null,
    /// `true`/`false`.
    Bool(bool),
    /// Any other type (the type name for the error message).
    Other(String),
}

/// `ZEND_HANDLE_NUMERIC_STR`: canonical decimal integer strings.
fn handle_numeric(s: &[u8]) -> Option<i64> {
    if s.is_empty() || s.len() > 20 {
        return None;
    }
    let (neg, digits) = match s[0] {
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    if neg && digits == b"0" {
        return None;
    }
    let v: i64 = std::str::from_utf8(s).ok()?.parse().ok()?;
    Some(v)
}

/// `zend_dval_to_lval_safe`/cap-ish conversion.
fn dval_to_lval(d: f64) -> i64 {
    if !d.is_finite() || d >= 9.223_372_036_854_775_807e18 || d < -9.223_372_036_854_775_808e18 {
        0
    } else {
        #[allow(clippy::cast_possible_truncation)]
        let v = d as i64;
        v
    }
}

/// Parses a legacy offset (`dom_nodemap_or_nodelist_process_offset_as_named`):
/// `Err(name)` when it is a non-numeric string.
fn legacy_offset(off: &Offset) -> Result<i64, Vec<u8>> {
    match off {
        Offset::Int(i) => Ok(*i),
        Offset::Float(f) => Ok(dval_to_lval(*f)),
        Offset::Bool(b) => Ok(i64::from(*b)),
        Offset::Null | Offset::Other(_) => Ok(0),
        Offset::Str(s) => match php_numeric(s) {
            Some(Numeric::Int(i)) => Ok(i),
            Some(Numeric::Float(f)) => Ok(if f.is_finite() {
                #[allow(clippy::cast_possible_truncation)]
                let v = f.clamp(i64::MIN as f64, i64::MAX as f64) as i64;
                v
            } else {
                0
            }),
            None => Err(s.clone()),
        },
    }
}

enum Numeric {
    Int(i64),
    Float(f64),
}

/// A small `is_numeric_string` (allow_errors=true semantics are not needed
/// here: only fully numeric strings, with leading/trailing whitespace).
fn php_numeric(s: &[u8]) -> Option<Numeric> {
    let t = std::str::from_utf8(s).ok()?;
    let trimmed = t.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Some(Numeric::Int(i));
    }
    let valid_float = trimmed
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    if valid_float {
        if let Ok(f) = trimmed.parse::<f64>() {
            return Some(Numeric::Float(f));
        }
    }
    None
}

/// An iterator over a collection (`php_dom_iterator`).
#[derive(Debug)]
pub struct CollectionIter {
    map: Rc<ObjMap>,
    index: i64,
    current: Option<Node>,
}

impl CollectionIter {
    fn new(map: Rc<ObjMap>) -> DomResult<Self> {
        let current = map.item(0)?;
        Ok(Self {
            map,
            index: 0,
            current,
        })
    }

    /// Whether the iterator points at an item.
    pub fn valid(&self) -> bool {
        self.current.is_some()
    }

    /// The current item.
    pub fn current(&self) -> Option<Node> {
        self.current.clone()
    }

    /// The current key: the position, or the node name for attribute maps
    /// and DTD maps.
    pub fn key(&self) -> IterKey {
        if self.map.kind.nameless() {
            return IterKey::Int(self.index);
        }
        match self.current.as_ref().and_then(Node::n) {
            Some(n) => {
                let modern = self.current.as_ref().is_some_and(Node::is_modern);
                if n.t() == ffi::XML_ATTRIBUTE_NODE && modern {
                    IterKey::Str(crate::tree::qualified_name(n, false))
                } else {
                    IterKey::Str(n.name().to_vec())
                }
            }
            None => IterKey::Null,
        }
    }

    /// Advances to the next item.
    pub fn move_forward(&mut self) -> DomResult<()> {
        let Some(cur) = self.current.take() else {
            return Ok(());
        };
        self.index += 1;
        if let Some(n) = cur.n() {
            let legacy_linked = !cur.is_modern()
                && matches!(self.map.kind, MapKind::Attributes | MapKind::ChildNodes);
            if legacy_linked {
                self.current = n.next().map(Node::wrap);
            } else {
                self.current = self.map.item(self.index)?;
            }
        }
        Ok(())
    }
}

impl Iterator for CollectionIter {
    type Item = DomResult<Node>;
    fn next(&mut self) -> Option<Self::Item> {
        let cur = self.current.clone()?;
        if let Err(e) = self.move_forward() {
            return Some(Err(e));
        }
        Some(Ok(cur))
    }
}

/// An iteration key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IterKey {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
    /// `null`.
    Null,
}

impl NodeList {
    /// The PHP class (`DOMNodeList`, `Dom\NodeList` or `Dom\HTMLCollection`).
    pub fn class_name(&self) -> &'static str {
        self.0.class
    }

    /// `$length` / `count()`.
    pub fn length(&self) -> i64 {
        self.0.length()
    }

    /// `item($index)`.
    pub fn item(&self, index: i64) -> DomResult<Option<Node>> {
        self.0.item(index)
    }

    /// `foreach` / `getIterator()`.
    pub fn iter(&self) -> DomResult<CollectionIter> {
        CollectionIter::new(self.0.clone())
    }

    /// Collects all items (convenience).
    pub fn to_vec(&self) -> DomResult<Vec<Node>> {
        self.iter()?.collect()
    }

    /// `Dom\HTMLCollection::namedItem($key)`.
    pub fn named_item(&self, key: &[u8]) -> Option<Node> {
        self.0.collection_named_item(key)
    }

    /// `$list[$offset]` (read_dimension); `Err` for illegal offsets
    /// (modern lists).
    pub fn offset_get(&self, offset: &Offset) -> DomResult<Option<Node>> {
        if self.0.class == "DOMNodeList" {
            return match legacy_offset(offset) {
                Ok(i) => self.item(i),
                Err(_) => Ok(None),
            };
        }
        match modern_index(offset) {
            ModernIndex::Long(i) => self.item(i),
            ModernIndex::Str(s) if self.0.class == "Dom\\HTMLCollection" => Ok(self.named_item(&s)),
            _ => Err(illegal_offset(self.0.class, offset)),
        }
    }

    /// `isset($list[$offset])`.
    pub fn offset_exists(&self, offset: &Offset) -> DomResult<bool> {
        if self.0.class == "DOMNodeList" {
            return Ok(match legacy_offset(offset) {
                Ok(i) => i >= 0 && i < self.length(),
                Err(_) => false,
            });
        }
        match modern_index(offset) {
            ModernIndex::Long(i) => Ok(i >= 0 && i < self.length()),
            ModernIndex::Str(s) if self.0.class == "Dom\\HTMLCollection" => Ok(self.named_item(&s).is_some()),
            _ => Err(illegal_offset(self.0.class, offset)),
        }
    }
}

enum ModernIndex {
    Long(i64),
    Str(Vec<u8>),
    Illegal,
}

fn modern_index(off: &Offset) -> ModernIndex {
    match off {
        Offset::Int(i) => ModernIndex::Long(*i),
        Offset::Float(f) => ModernIndex::Long(dval_to_lval(*f)),
        Offset::Str(s) => match handle_numeric(s) {
            Some(i) => ModernIndex::Long(i),
            None => ModernIndex::Str(s.clone()),
        },
        _ => ModernIndex::Illegal,
    }
}

fn illegal_offset(class: &str, off: &Offset) -> DomError {
    let ty = match off {
        Offset::Null => "null".to_string(),
        Offset::Bool(_) => "bool".to_string(),
        Offset::Str(_) => "string".to_string(),
        Offset::Int(_) => "int".to_string(),
        Offset::Float(_) => "float".to_string(),
        Offset::Other(t) => t.clone(),
    };
    DomError::Type(format!("Cannot access offset of type {ty} on {class}"))
}

impl NamedNodeMap {
    /// The PHP class.
    pub fn class_name(&self) -> &'static str {
        self.0.class
    }

    /// `$length` / `count()`.
    pub fn length(&self) -> i64 {
        self.0.raw_length()
    }

    /// `item($index)` (raises `ValueError` for out-of-range legacy indexes).
    pub fn item(&self, index: i64) -> DomResult<Option<Node>> {
        if self.0.class == "DOMNamedNodeMap" && (index < 0 || index > i64::from(i32::MAX)) {
            return Err(DomError::arg_value(1, format!("must be between 0 and {}", i32::MAX)));
        }
        self.0.item(index)
    }

    /// `getNamedItem($qualifiedName)`.
    pub fn get_named_item(&self, name: &[u8]) -> Option<Node> {
        self.0.named_item(name, None)
    }

    /// `getNamedItemNS($namespace, $localName)`.
    pub fn get_named_item_ns(&self, namespace: Option<&[u8]>, local: &[u8]) -> Option<Node> {
        let ns = match namespace {
            Some(u) if u.is_empty() && self.0.base.as_ref().is_some_and(Node::is_modern) => None,
            Some(u) => Some(u),
            None => None,
        };
        // C passes the uri pointer: NULL only when the PHP arg is null (or
        // the modern empty-string rule above). A NULL uri means "no ns
        // filtering" via xmlHasProp/qualified-name lookup.
        self.0.named_item(local, ns)
    }

    /// `foreach` / `getIterator()`.
    pub fn iter(&self) -> DomResult<CollectionIter> {
        CollectionIter::new(self.0.clone())
    }

    /// `$map[$offset]`.
    pub fn offset_get(&self, offset: &Offset) -> DomResult<Option<Node>> {
        if self.0.class == "DOMNamedNodeMap" {
            return match legacy_offset(offset) {
                Ok(i) => {
                    if i < 0 || i > i64::from(i32::MAX) {
                        return Err(DomError::Value(format!("must be between 0 and {}", i32::MAX)));
                    }
                    self.0.item(i)
                }
                Err(name) => Ok(self.0.named_item(&name, None)),
            };
        }
        match modern_index(offset) {
            ModernIndex::Long(i) => self.0.item(i),
            ModernIndex::Str(s) => Ok(self.0.named_item(&s, None)),
            ModernIndex::Illegal => Err(illegal_offset(self.0.class, offset)),
        }
    }

    /// `isset($map[$offset])`.
    pub fn offset_exists(&self, offset: &Offset) -> DomResult<bool> {
        if self.0.class == "DOMNamedNodeMap" {
            return Ok(match legacy_offset(offset) {
                Ok(i) => i >= 0 && i < self.length(),
                Err(name) => self.0.has_named_item(&name, None),
            });
        }
        match modern_index(offset) {
            ModernIndex::Long(i) => Ok(i >= 0 && i < self.length()),
            ModernIndex::Str(s) => Ok(self.0.has_named_item(&s, None)),
            ModernIndex::Illegal => Err(illegal_offset(self.0.class, offset)),
        }
    }
}
