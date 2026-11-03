//! The `SimpleXMLElement` object model (`ext/simplexml/simplexml.c`).
//!
//! A [`Sxe`] is the state of one PHP object: the `(document, node, iter)`
//! triple of `php_sxe_object`. Element and attribute access, casts,
//! property tables and iteration are associated functions taking the
//! object state (`&SxeRef`) and the [`SxeHost`]; they mirror the C object
//! handlers one to one.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use php_libxml::ffi as lx;
use php_libxml::tree::{Document, Node};
use php_runtime::{
    ArrayKey, CastTarget, ErrorLevel, HashTable, Obj, RtResult, RuntimeContext, RuntimeError,
    Value, ZArray, ZStr,
};

use crate::ffi_extra as fx;
use crate::xnode::{c_string, cstr, take_xml_string, xml_free, XNode};

/// What `SimpleXML` needs from the VM besides [`RuntimeContext`]
/// (diagnostics, conversions).
pub trait SxeHost: RuntimeContext {
    /// `php_sxe_object_new` + `ZVAL_OBJ`: instantiates an object of class
    /// `class` (a `SimpleXMLElement` subclass, **without** calling its
    /// constructor) whose `SimpleXML` state is `sxe`.
    fn sxe_wrap(&mut self, class: &ZStr, sxe: SxeRef) -> RtResult<Value>;

    /// The `SimpleXML` state of an object, or `None` when it is not a
    /// `SimpleXMLElement`.
    fn sxe_unwrap(&mut self, obj: &Obj) -> Option<SxeRef>;
}

/// `SXE_ITER_*`: what an object stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IterType {
    /// `SXE_ITER_NONE`: the node itself.
    #[default]
    None,
    /// `SXE_ITER_ELEMENT`: the children of the node named `iter.name`.
    Element,
    /// `SXE_ITER_CHILD`: all children of the node (`children()`).
    Child,
    /// `SXE_ITER_ATTRLIST`: the attributes of the node (`attributes()`).
    AttrList,
}

#[derive(Debug, Clone, Default)]
struct Iter {
    kind: IterType,
    name: Option<Vec<u8>>,
    nsprefix: Option<Vec<u8>>,
    isprefix: bool,
}

/// A persistent `xmlXPathContext` (`sxe->xpath`).
struct XPathCtx(NonNull<fx::xmlXPathContext>);

impl Drop for XPathCtx {
    fn drop(&mut self) {
        // SAFETY: the context is owned by this wrapper; its document is
        // still alive (State drops `xpath` before `document`).
        unsafe { fx::xmlXPathFreeContext(self.0.as_ptr()) };
    }
}

#[derive(Default)]
struct State {
    // Field order matters: the XPath context references the document.
    xpath: Option<XPathCtx>,
    data: Option<Value>,
    tmp: Option<Value>,
    node: Option<XNode>,
    document: Option<Document>,
    iter: Iter,
    /// The document belongs to the modern DOM (`Dom\XMLDocument`, PHP's
    /// `PHP_LIBXML_CLASS_MODERN`); inherited by derived objects.
    modern: bool,
}

/// The state of one `SimpleXMLElement` object.
pub struct Sxe {
    class: ZStr,
    state: RefCell<State>,
}

/// Shared handle to an object's state.
pub type SxeRef = Rc<Sxe>;

impl std::fmt::Debug for Sxe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.state.borrow();
        f.debug_struct("Sxe")
            .field("class", &self.class)
            .field("node", &s.node)
            .field("iter", &s.iter)
            .finish_non_exhaustive()
    }
}

/// How a property/dimension is fetched (`BP_VAR_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// `BP_VAR_R` (also `FUNC_ARG` in read context and `UNSET`).
    Read,
    /// `BP_VAR_W`.
    Write,
    /// `BP_VAR_RW`.
    ReadWrite,
    /// `BP_VAR_IS` (`isset()`/`??`).
    IsSet,
}

/// `has_property`/`has_dimension`'s `check_empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// `isset()` (0).
    Isset,
    /// `!empty()` (1).
    NotEmpty,
    /// `property_exists()` (2).
    Exists,
}

/// Result of `get_property_ptr_ptr` (`sxe_property_get_adr`).
#[derive(Debug, Clone)]
pub enum PropertyPtr {
    /// `NULL`: the element exists; the VM falls back to
    /// `read_property`/`write_property`.
    Fallback,
    /// The newly created element (kept in `sxe->tmp`): the VM operates on
    /// this value.
    Value(Value),
    /// `&EG(error_zval)` (a diagnostic was raised).
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Member {
    Long(i64),
    Str(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Elements,
    Attribs,
}

/// Whether a `memory_limit` violation is pending (PHP's delayed OOM):
/// loops building large results stop early instead of running into the
/// hard limit.
fn out_of_memory() -> bool {
    php_memlimit::is_pending()
}

/// `"SimpleXMLElement is not properly initialized"`.
pub(crate) fn not_initialized() -> RuntimeError {
    RuntimeError::Error("SimpleXMLElement is not properly initialized".into())
}

fn warn(host: &mut dyn SxeHost, msg: &str) -> RtResult<()> {
    host.diagnostic(ErrorLevel::Warning, msg)
}

/// `match_ns`.
fn match_ns(node: &XNode, name: Option<&[u8]>, prefix: bool) -> bool {
    let ns = node.ns();
    if name.is_none() && ns.as_ref().is_none_or(|n| n.prefix.is_none()) {
        return true;
    }
    if let Some(ns) = ns {
        let have = if prefix { ns.prefix } else { Some(ns.href) };
        return have.as_deref() == name.map(cstr);
    }
    false
}

impl Iter {
    fn match_ns(&self, node: &XNode) -> bool {
        match_ns(node, self.nsprefix.as_deref(), self.isprefix)
    }

    fn name_is(&self, node: &XNode) -> bool {
        self.name.as_deref().is_some_and(|n| node.name_is(n))
    }

    /// `php_sxe_iterator_fetch` (without storing data).
    fn fetch(&self, mut node: Option<XNode>) -> Option<XNode> {
        while let Some(n) = node {
            let hit = match self.kind {
                IterType::AttrList => {
                    n.is_attribute()
                        && (self.name.is_none() || self.name_is(&n))
                        && self.match_ns(&n)
                }
                IterType::Element if self.name.is_some() => {
                    n.is_element() && self.name_is(&n) && self.match_ns(&n)
                }
                _ => n.is_element() && self.match_ns(&n),
            };
            if hit {
                return Some(n);
            }
            node = n.next();
        }
        None
    }

    /// `sxe_get_element_by_offset`.
    fn element_by_offset(&self, offset: i64, node: XNode) -> (Option<XNode>, i64) {
        if self.kind == IterType::None {
            return if offset == 0 {
                (Some(node), 0)
            } else {
                (None, 0)
            };
        }
        let mut idx = 0;
        let mut cur = Some(node);
        while let Some(n) = cur.clone() {
            if !(offset < 0 || idx <= offset) {
                break;
            }
            if n.is_element()
                && self.match_ns(&n)
                && (self.kind == IterType::Child
                    || (self.kind == IterType::Element && self.name_is(&n)))
            {
                if idx == offset {
                    break;
                }
                idx += 1;
            }
            cur = n.next();
        }
        (cur, idx)
    }

    /// `sxe_find_element_by_name`.
    fn find_element_by_name(&self, mut node: Option<XNode>, name: &[u8]) -> Option<XNode> {
        while let Some(n) = node {
            if n.is_element() && self.match_ns(&n) && n.name_is(name) {
                return Some(n);
            }
            node = n.next();
        }
        None
    }

    /// The attribute filter of the prop/dim handlers.
    fn attr_matches(&self, attr: &XNode, test: bool) -> bool {
        (!test || self.name_is(attr)) && self.match_ns(attr)
    }
}

fn add_prop(rv: &mut HashTable, name: &[u8], value: Value) {
    let key = ArrayKey::Str(ZStr::new(name.to_vec()));
    match rv.get_mut(&key) {
        Some(Value::Array(arr)) => {
            let _ = arr.make_mut().push(value);
        }
        Some(existing) => {
            let old = std::mem::replace(existing, Value::Null);
            *existing = Value::Array(ZArray::from_values([old, value]));
        }
        None => {
            rv.insert(key, value);
        }
    }
}

impl Sxe {
    /// A fresh, uninitialised object state of class `class` (what
    /// `new Foo` creates before the constructor runs).
    pub fn new(class: impl Into<ZStr>) -> SxeRef {
        Rc::new(Sxe {
            class: class.into(),
            state: RefCell::new(State::default()),
        })
    }

    /// The class the object was created as (`zo.ce`).
    pub fn class(&self) -> &ZStr {
        &self.class
    }

    /// The node the object refers to (`sxe->node`), if any.
    pub fn node(&self) -> Option<XNode> {
        self.state.borrow().node.clone()
    }

    /// The document (`sxe->document`), if any.
    pub fn document(&self) -> Option<Document> {
        self.state.borrow().document.clone()
    }

    /// Marks the object's document as a modern-DOM document (set by the
    /// binding after `simplexml_import_dom()` of a `Dom\Node`).
    pub fn set_modern_document(&self, modern: bool) {
        self.state.borrow_mut().modern = modern;
    }

    /// Whether the object's document belongs to the modern DOM
    /// ([`Sxe::set_modern_document`]).
    pub fn is_modern_document(&self) -> bool {
        self.state.borrow().modern
    }

    /// The iteration mode (`sxe->iter.type`).
    pub fn iter_type(&self) -> IterType {
        self.state.borrow().iter.kind
    }

    fn iter_state(&self) -> Iter {
        self.state.borrow().iter.clone()
    }

    /// `GET_NODE`.
    fn get_node(&self) -> RtResult<XNode> {
        self.node().ok_or_else(not_initialized)
    }

    /// `php_sxe_reset_iterator_no_clear_iter_data(sxe, 0)`.
    fn reset_first(&self) -> RtResult<Option<XNode>> {
        let node = self.get_node()?;
        let iter = self.iter_state();
        let start = match iter.kind {
            IterType::AttrList => node.properties(),
            _ => node.children(),
        };
        Ok(iter.fetch(start))
    }

    /// `php_sxe_get_first_node_non_destructive`.
    fn first_node(&self, node: Option<XNode>) -> RtResult<Option<XNode>> {
        if self.iter_type() == IterType::None {
            Ok(node)
        } else {
            self.reset_first()
        }
    }

    fn with_iter_kind<T>(&self, kind: IterType, f: impl FnOnce(&Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.state.borrow_mut().iter.kind, kind);
        let out = f(self);
        self.state.borrow_mut().iter.kind = saved;
        out
    }
}

/// `node_as_zval`: a new object of the same class for `node`.
fn wrap(
    this: &Sxe,
    host: &mut dyn SxeHost,
    node: Option<XNode>,
    kind: IterType,
    name: Option<&[u8]>,
    nsprefix: Option<&[u8]>,
    isprefix: bool,
) -> RtResult<Value> {
    let document = this
        .document()
        .or_else(|| node.as_ref().map(|n| n.document().clone()));
    let mut iter = Iter {
        kind,
        name: name.map(|n| cstr(n).to_vec()),
        ..Iter::default()
    };
    if let Some(p) = nsprefix.filter(|p| !p.is_empty()) {
        iter.nsprefix = Some(p.to_vec());
        iter.isprefix = isprefix;
    }
    let sxe = Rc::new(Sxe {
        class: this.class.clone(),
        state: RefCell::new(State {
            node,
            document,
            iter,
            modern: this.state.borrow().modern,
            ..State::default()
        }),
    });
    host.sxe_wrap(&this.class, sxe)
}

/// The state behind a value holding a `SimpleXML` object.
fn state_of(host: &mut dyn SxeHost, v: &Value) -> Option<SxeRef> {
    match v {
        Value::Object(o) => host.sxe_unwrap(o),
        Value::Reference(r) => {
            let inner = r.get();
            state_of(host, &inner)
        }
        _ => None,
    }
}

fn member_for_read(host: &mut dyn SxeHost, v: &Value) -> RtResult<Member> {
    match v {
        Value::Int(i) => Ok(Member::Long(*i)),
        Value::Str(s) => Ok(Member::Str(s.as_bytes().to_vec())),
        Value::Reference(r) => member_for_read(host, &r.get()),
        other => Ok(Member::Str(other.to_zstr(host)?.as_bytes().to_vec())),
    }
}

/// `php_trim(str, NULL, 0, 3)`: strips `" \t\n\r\v\0"` on both sides.
fn php_trim(s: &[u8]) -> &[u8] {
    let ws = |b: &u8| matches!(*b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0);
    let start = s.iter().position(|b| !ws(b)).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !ws(b)).map_or(start, |e| e + 1);
    &s[start..end]
}

/// Implementation of the property/dimension handlers.
impl Sxe {
    /// `read_property`.
    pub fn read_property(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        name: &[u8],
        mode: FetchMode,
    ) -> RtResult<Value> {
        Self::prop_dim_read(
            this,
            host,
            Some(Member::Str(name.to_vec())),
            Access::Elements,
            mode,
        )
    }

    /// `read_dimension` (`None` = `$sxe[]`).
    pub fn read_dimension(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        offset: Option<&Value>,
        mode: FetchMode,
    ) -> RtResult<Value> {
        let member = match offset {
            Some(v) => Some(member_for_read(host, v)?),
            None => None,
        };
        Self::prop_dim_read(this, host, member, Access::Attribs, mode)
    }

    #[allow(clippy::too_many_lines)]
    fn prop_dim_read(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        member: Option<Member>,
        mut access: Access,
        mode: FetchMode,
    ) -> RtResult<Value> {
        let iter = this.iter_state();
        match &member {
            None => {
                if iter.kind == IterType::AttrList {
                    return Err(RuntimeError::Error(
                        "Cannot create unnamed attribute".into(),
                    ));
                }
                access = Access::Elements;
            }
            Some(Member::Long(_)) if iter.kind != IterType::AttrList => access = Access::Elements,
            _ => {}
        }
        let mut node = Some(this.get_node()?);
        let mut attr = None;
        let mut test = false;
        if iter.kind == IterType::AttrList {
            access = Access::Attribs;
            node = this.first_node(node)?;
            attr.clone_from(&node);
            test = iter.name.is_some();
        } else if iter.kind != IterType::Child {
            node = this.first_node(node)?;
            attr = node.as_ref().and_then(XNode::properties);
            if member.is_none() && node.as_ref().is_some_and(XNode::parent_is_document) {
                return Err(RuntimeError::Error(
                    "Cannot create unnamed attribute".into(),
                ));
            }
        }
        let Some(node) = node else {
            return Ok(Value::Null);
        };
        let nsprefix = iter.nsprefix.clone();
        let ns = nsprefix.as_deref();
        if access == Access::Attribs {
            let long = matches!(member, Some(Member::Long(_)));
            if !long || iter.kind == IterType::AttrList {
                let mut idx = 0;
                while let Some(a) = attr {
                    match &member {
                        Some(Member::Long(n)) => {
                            if idx > *n {
                                break;
                            }
                            if iter.attr_matches(&a, test) {
                                if idx == *n {
                                    return wrap(
                                        this,
                                        host,
                                        Some(a),
                                        IterType::None,
                                        None,
                                        ns,
                                        iter.isprefix,
                                    );
                                }
                                idx += 1;
                            }
                        }
                        Some(Member::Str(name)) => {
                            if iter.attr_matches(&a, test) && a.name_is(name) {
                                return wrap(
                                    this,
                                    host,
                                    Some(a),
                                    IterType::None,
                                    None,
                                    ns,
                                    iter.isprefix,
                                );
                            }
                        }
                        None => break,
                    }
                    attr = a.next();
                }
            }
            return Ok(Value::Null);
        }
        match member {
            None | Some(Member::Long(_)) => {
                let offset = match member {
                    Some(Member::Long(n)) => Some(n),
                    _ => None,
                };
                let mynode = node.clone();
                let mut cur = Some(node);
                let mut cnt = 0;
                let mut appendable = true;
                if iter.kind == IterType::Child {
                    cur = this.first_node(cur)?;
                }
                if iter.kind == IterType::None {
                    if offset.is_some_and(|n| n != 0) {
                        cur = None;
                        appendable = false;
                    }
                } else if let Some(n) = offset {
                    match cur {
                        Some(c) => {
                            let (found, count) = iter.element_by_offset(n, c);
                            cur = found;
                            cnt = count;
                        }
                        None => cur = None,
                    }
                } else {
                    cur = None;
                }
                if let Some(c) = cur {
                    return wrap(this, host, Some(c), IterType::None, None, ns, iter.isprefix);
                }
                if matches!(mode, FetchMode::Write | FetchMode::ReadWrite) {
                    if let Some(n) = offset {
                        if n < 0 || cnt < n {
                            warn(
                                host,
                                &format!(
                                    "Cannot add element {} number {n} when only {cnt} such elements exist",
                                    String::from_utf8_lossy(&mynode.name().unwrap_or_default())
                                ),
                            )?;
                        }
                    }
                    if appendable && offset.is_none_or(|n| n >= 0) {
                        let created = mynode.parent().and_then(|p| {
                            p.new_text_child(
                                mynode.ns_ptr(),
                                &mynode.name().unwrap_or_default(),
                                None,
                            )
                        });
                        if created.is_some() {
                            return wrap(
                                this,
                                host,
                                created,
                                IterType::None,
                                None,
                                ns,
                                iter.isprefix,
                            );
                        }
                    }
                }
                Ok(Value::Null)
            }
            Some(Member::Str(name)) => {
                if mode != FetchMode::IsSet
                    || iter.find_element_by_name(node.children(), &name).is_some()
                {
                    return wrap(
                        this,
                        host,
                        Some(node),
                        IterType::Element,
                        Some(&name),
                        ns,
                        iter.isprefix,
                    );
                }
                Ok(Value::Null)
            }
        }
    }

    /// `write_property`. Returns `false` when PHP returns `&EG(error_zval)`
    /// (after a warning).
    pub fn write_property(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        name: &[u8],
        value: &Value,
    ) -> RtResult<bool> {
        Self::prop_dim_write(
            this,
            host,
            Some(&Member::Str(name.to_vec())),
            Some(value),
            Access::Elements,
        )
        .map(|(ok, _)| ok)
    }

    /// `write_dimension` (`None` = `$sxe[] = ...`).
    pub fn write_dimension(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        offset: Option<&Value>,
        value: &Value,
    ) -> RtResult<bool> {
        let member = match offset {
            None => None,
            Some(v) => Some(Self::member_for_write(host, v)?),
        };
        Self::prop_dim_write(this, host, member.as_ref(), Some(value), Access::Attribs)
            .map(|(ok, _)| ok)
    }

    fn member_for_write(host: &mut dyn SxeHost, v: &Value) -> RtResult<Member> {
        Ok(match v {
            Value::Int(i) => Member::Long(*i),
            Value::Str(s) => Member::Str(s.as_bytes().to_vec()),
            Value::Reference(r) => return Self::member_for_write(host, &r.get()),
            other => Member::Str(php_trim(other.to_zstr(host)?.as_bytes()).to_vec()),
        })
    }

    /// The string a written value is converted to (`zval_get_string` for
    /// scalars, the `SimpleXML` cast for `SimpleXMLElement` objects).
    fn value_string(host: &mut dyn SxeHost, value: &Value, access: Access) -> RtResult<Vec<u8>> {
        match value {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => {
                Ok(value.to_zstr(host)?.as_bytes().to_vec())
            }
            Value::Reference(r) => Self::value_string(host, &r.get(), access),
            Value::Object(o) if o.class_name().as_bytes() == b"SimpleXMLElement" => {
                match host.sxe_unwrap(o) {
                    Some(s) => Ok(Self::cast_contents(&s)?.unwrap_or_default()),
                    None => Ok(Vec::new()),
                }
            }
            other => Err(RuntimeError::TypeError(format!(
                "It's not possible to assign a complex type to {}, {} given",
                if access == Access::Attribs {
                    "attributes"
                } else {
                    "properties"
                },
                other.value_name()
            ))),
        }
    }

    /// `sxe_prop_dim_write`. Returns `(ok, newnode)`.
    #[allow(clippy::too_many_lines)]
    fn prop_dim_write(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        member: Option<&Member>,
        value: Option<&Value>,
        mut access: Access,
    ) -> RtResult<(bool, Option<XNode>)> {
        let iter = this.iter_state();
        match member {
            None => {
                if iter.kind == IterType::AttrList {
                    return Err(RuntimeError::Error(
                        "Cannot append to an attribute list".into(),
                    ));
                }
                access = Access::Elements;
            }
            Some(Member::Long(_)) => {
                if iter.kind != IterType::AttrList {
                    access = Access::Elements;
                }
            }
            Some(Member::Str(s)) => {
                if s.is_empty() {
                    return Err(RuntimeError::ValueError(format!(
                        "Cannot create {} with an empty name",
                        if access == Access::Attribs {
                            "attribute"
                        } else {
                            "element"
                        }
                    )));
                }
            }
        }
        let start = this.get_node()?;
        let mut node = Some(start.clone());
        let mut attr = None;
        let mut test = false;
        if iter.kind == IterType::AttrList {
            access = Access::Attribs;
            attr = this.first_node(node.clone())?;
            test = iter.name.is_some();
        } else if iter.kind != IterType::Child {
            let mynode = start.clone();
            node = this.first_node(node)?;
            attr = node.as_ref().and_then(XNode::properties);
            if member.is_none() && node.as_ref().is_some_and(XNode::parent_is_document) {
                return Err(RuntimeError::ValueError(
                    "Cannot append to an attribute list".into(),
                ));
            }
            if access == Access::Attribs && node.is_none() && iter.kind == IterType::Element {
                node = mynode.new_child(
                    mynode.ns_ptr(),
                    iter.name.as_deref().unwrap_or_default(),
                    None,
                );
                attr = node.as_ref().and_then(XNode::properties);
            }
        }
        let mynode = node.clone();
        let value_str = match value {
            Some(v) => Some(Self::value_string(host, v, access)?),
            None => None,
        };
        let Some(node) = node else {
            return Ok((true, None));
        };
        let offset = match member {
            Some(Member::Long(n)) => Some(*n),
            _ => None,
        };
        let mut counter = 0;
        let mut is_attr = false;
        let mut nodendx = 0;
        let mut cnt = 0;
        let mut newnode: Option<XNode> = None;
        // `node` after the element search (the C loop variable).
        let mut loop_node = Some(node.clone());
        if access == Access::Attribs {
            while let Some(a) = attr.clone() {
                match member {
                    Some(Member::Long(n)) => {
                        if nodendx > *n {
                            break;
                        }
                        if iter.attr_matches(&a, test) {
                            if nodendx == *n {
                                is_attr = true;
                                counter += 1;
                                break;
                            }
                            nodendx += 1;
                        }
                    }
                    Some(Member::Str(name)) => {
                        if iter.attr_matches(&a, test) && a.name_is(name) {
                            is_attr = true;
                            counter += 1;
                            break;
                        }
                    }
                    None => break,
                }
                attr = a.next();
            }
        } else if let Some(Member::Str(name)) = member {
            let mut c = node.children();
            while let Some(child) = c {
                if !child.is_text() && child.name_is(name) && iter.match_ns(&child) {
                    newnode = Some(child.clone());
                    counter += 1;
                }
                c = child.next();
            }
            loop_node = None;
        } else {
            if node.is_attribute() {
                return Err(RuntimeError::Error(
                    "Cannot create duplicate attribute".into(),
                ));
            }
            if iter.kind == IterType::None {
                if let Some(n) = offset.filter(|n| *n != 0) {
                    warn(
                        host,
                        &format!(
                            "Cannot add element {} number {n} when only 0 such elements exist",
                            String::from_utf8_lossy(&node.name().unwrap_or_default())
                        ),
                    )?;
                    return Ok((false, None));
                }
                newnode = Some(node.clone());
                counter += 1;
            } else if let Some(n) = offset {
                let (found, count) = iter.element_by_offset(n, node.clone());
                cnt = count;
                if found.is_some() {
                    counter += 1;
                }
                newnode = found;
            }
        }
        let mut ok = true;
        if counter == 1 {
            if is_attr {
                newnode.clone_from(&attr);
            }
            if let (Some(v), Some(target)) = (&value_str, &newnode) {
                let mut c = target.children();
                while let Some(child) = c {
                    c = child.next();
                    child.unlink();
                }
                target.set_value(v);
            }
        } else if counter > 1 {
            warn(
                host,
                "Cannot assign to an array of nodes (duplicate subnodes or attr detected)",
            )?;
            ok = false;
        } else if access == Access::Elements {
            let mynode = mynode.unwrap_or_else(|| node.clone());
            if loop_node.is_none() {
                newnode = match member {
                    Some(Member::Str(name)) => {
                        mynode.new_text_child(std::ptr::null_mut(), name, value_str.as_deref())
                    }
                    _ => mynode.parent().and_then(|p| {
                        p.new_text_child(
                            mynode.ns_ptr(),
                            &mynode.name().unwrap_or_default(),
                            value_str.as_deref(),
                        )
                    }),
                };
            } else {
                if let Some(n) = offset {
                    if n < 0 || cnt < n {
                        warn(
                                host,
                                &format!(
                                    "Cannot add element {} number {n} when only {cnt} such elements exist",
                                    String::from_utf8_lossy(&mynode.name().unwrap_or_default())
                                ),
                            )?;
                    }
                }
                if offset.is_some_and(|n| n < 0) {
                    ok = false;
                } else {
                    newnode = mynode.parent().and_then(|p| {
                        p.new_text_child(
                            mynode.ns_ptr(),
                            &mynode.name().unwrap_or_default(),
                            value_str.as_deref(),
                        )
                    });
                }
            }
        } else if let Some(n) = offset {
            warn(
                host,
                &format!("Cannot change attribute number {n} when only {nodendx} attributes exist"),
            )?;
        } else if let Some(Member::Str(name)) = member {
            newnode = node.new_prop(std::ptr::null_mut(), name, value_str.as_deref());
        }
        Ok((ok, newnode))
    }

    /// `get_property_ptr_ptr` (`sxe_property_get_adr`).
    pub fn property_ptr(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        name: &[u8],
    ) -> RtResult<PropertyPtr> {
        let node = this.get_node()?;
        if Self::element_by_name(this, &node, name)?.is_some() {
            return Ok(PropertyPtr::Fallback);
        }
        let (ok, newnode) = Self::prop_dim_write(
            this,
            host,
            Some(&Member::Str(name.to_vec())),
            None,
            Access::Elements,
        )?;
        if !ok {
            return Ok(PropertyPtr::Error);
        }
        let iter = this.iter_state();
        let v = wrap(
            this,
            host,
            newnode,
            IterType::None,
            None,
            iter.nsprefix.as_deref(),
            iter.isprefix,
        )?;
        this.state.borrow_mut().tmp = Some(v.clone());
        Ok(PropertyPtr::Value(v))
    }

    /// `sxe_get_element_by_name`.
    fn element_by_name(this: &SxeRef, node: &XNode, name: &[u8]) -> RtResult<Option<XNode>> {
        let iter = this.iter_state();
        let mut orgnode = Some(node.clone());
        let mut cur = Some(node.clone());
        if iter.kind != IterType::AttrList {
            let kind = if iter.kind == IterType::None {
                IterType::Child
            } else {
                iter.kind
            };
            cur = this.with_iter_kind(kind, |s| s.first_node(cur))?;
        }
        if iter.kind == IterType::Element {
            orgnode = iter.find_element_by_name(cur, iter.name.as_deref().unwrap_or_default());
            let Some(o) = &orgnode else { return Ok(None) };
            cur = o.children();
        }
        while let Some(n) = cur {
            if n.is_element() && iter.match_ns(&n) && n.name_is(name) {
                return Ok(orgnode);
            }
            cur = n.next();
        }
        Ok(None)
    }

    /// `has_property`.
    pub fn has_property(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        name: &[u8],
        check: CheckKind,
    ) -> RtResult<bool> {
        let _ = host;
        Self::prop_dim_exists(this, &Member::Str(name.to_vec()), check, Access::Elements)
    }

    /// `has_dimension`.
    pub fn has_dimension(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        offset: &Value,
        check: CheckKind,
    ) -> RtResult<bool> {
        let member = member_for_read(host, offset)?;
        Self::prop_dim_exists(this, &member, check, Access::Attribs)
    }

    fn prop_dim_exists(
        this: &SxeRef,
        member: &Member,
        check: CheckKind,
        mut access: Access,
    ) -> RtResult<bool> {
        let iter = this.iter_state();
        let mut node = Some(this.get_node()?);
        if matches!(member, Member::Long(_)) && iter.kind != IterType::AttrList {
            access = Access::Elements;
            if iter.kind == IterType::Child {
                node = this.first_node(node)?;
            }
        }
        let mut attr = None;
        let mut test = false;
        if iter.kind == IterType::AttrList {
            access = Access::Attribs;
            node = this.first_node(node)?;
            attr.clone_from(&node);
            test = iter.name.is_some();
        } else if iter.kind != IterType::Child {
            node = this.first_node(node)?;
            attr = node.as_ref().and_then(XNode::properties);
        }
        let Some(node) = node else { return Ok(false) };
        let empty_text = |v: Option<Vec<u8>>| v.is_none_or(|c| c.is_empty() || c == b"0");
        if access == Access::Attribs {
            let mut found = None;
            let mut idx = 0;
            while let Some(a) = attr {
                match member {
                    Member::Long(n) => {
                        if idx > *n {
                            break;
                        }
                        if iter.attr_matches(&a, test) {
                            if idx == *n {
                                found = Some(a);
                                break;
                            }
                            idx += 1;
                        }
                    }
                    Member::Str(name) => {
                        if iter.attr_matches(&a, test) && a.name_is(name) {
                            found = Some(a);
                            break;
                        }
                    }
                }
                attr = a.next();
            }
            let Some(a) = found else { return Ok(false) };
            if check == CheckKind::NotEmpty
                && empty_text(a.children().and_then(|c| c.raw_content()))
            {
                return Ok(false);
            }
            return Ok(true);
        }
        let found = match member {
            Member::Long(n) => {
                let mut start = Some(node);
                if iter.kind == IterType::Child {
                    start = this.first_node(start)?;
                }
                start.and_then(|s| iter.element_by_offset(*n, s).0)
            }
            Member::Str(name) => iter.find_element_by_name(node.children(), name),
        };
        let Some(n) = found else { return Ok(false) };
        if check == CheckKind::NotEmpty {
            match n.children() {
                None => return Ok(false),
                Some(c) if c.is_text() && !c.has_next() && empty_text(c.raw_content()) => {
                    return Ok(false)
                }
                _ => {}
            }
        }
        Ok(true)
    }

    /// `unset_property`.
    pub fn unset_property(this: &SxeRef, host: &mut dyn SxeHost, name: &[u8]) -> RtResult<()> {
        let _ = host;
        Self::prop_dim_delete(this, &Member::Str(name.to_vec()), Access::Elements)
    }

    /// `unset_dimension`.
    pub fn unset_dimension(this: &SxeRef, host: &mut dyn SxeHost, offset: &Value) -> RtResult<()> {
        let member = member_for_read(host, offset)?;
        Self::prop_dim_delete(this, &member, Access::Attribs)
    }

    fn prop_dim_delete(this: &SxeRef, member: &Member, mut access: Access) -> RtResult<()> {
        let iter = this.iter_state();
        let mut node = Some(this.get_node()?);
        if matches!(member, Member::Long(_)) && iter.kind != IterType::AttrList {
            access = Access::Elements;
            if iter.kind == IterType::Child {
                node = this.first_node(node)?;
            }
        }
        let mut attr = None;
        let mut test = false;
        if iter.kind == IterType::AttrList {
            access = Access::Attribs;
            node = this.first_node(node)?;
            attr.clone_from(&node);
            test = iter.name.is_some();
        } else if iter.kind != IterType::Child {
            node = this.first_node(node)?;
            attr = node.as_ref().and_then(XNode::properties);
        }
        let Some(node) = node else { return Ok(()) };
        if access == Access::Attribs {
            let mut idx = 0;
            while let Some(a) = attr {
                let next = a.next();
                match member {
                    Member::Long(n) => {
                        if idx > *n {
                            break;
                        }
                        if iter.attr_matches(&a, test) {
                            if idx == *n {
                                a.unlink();
                                break;
                            }
                            idx += 1;
                        }
                    }
                    Member::Str(name) => {
                        if iter.attr_matches(&a, test) && a.name_is(name) {
                            a.unlink();
                            break;
                        }
                    }
                }
                attr = next;
            }
            return Ok(());
        }
        match member {
            Member::Long(n) => {
                let mut start = Some(node);
                if iter.kind == IterType::Child {
                    start = this.first_node(start)?;
                }
                if let Some(found) = start.and_then(|s| iter.element_by_offset(*n, s).0) {
                    found.unlink();
                }
            }
            Member::Str(name) => {
                let mut c = node.children();
                while let Some(child) = c {
                    c = child.next();
                    if !child.is_text() && child.name_is(name) && iter.match_ns(&child) {
                        child.unlink();
                    }
                }
            }
        }
        Ok(())
    }
}

/// Casts, property tables, comparison, counting.
impl Sxe {
    /// The text a string cast yields (`sxe_object_cast_ex` before the type
    /// conversion); `None` is PHP's NULL contents.
    fn cast_contents(this: &Sxe) -> RtResult<Option<Vec<u8>>> {
        if this.iter_type() != IterType::None {
            let node = this.first_node(None)?;
            return Ok(node
                .and_then(|n| n.children())
                .and_then(|c| c.list_get_string()));
        }
        if this.node().is_none() {
            let root = this.document().and_then(|d| d.root()).map(XNode::from_node);
            if root.is_some() {
                this.state.borrow_mut().node = root;
            }
        }
        let Some(node) = this.node() else {
            return Ok(None);
        };
        if let Some(c) = node.children() {
            return Ok(c.list_get_string());
        }
        if matches!(node.kind(), lx::XML_COMMENT_NODE | lx::XML_PI_NODE) {
            return Ok(node.raw_content());
        }
        Ok(None)
    }

    /// `cast_object` (`sxe_object_cast_ex`). `Ok(None)` is `FAILURE`
    /// (unsupported target). The VM checks for a user `__toString`
    /// override first (`sxe_object_cast`).
    pub fn cast(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        target: CastTarget,
    ) -> RtResult<Option<Value>> {
        if target == CastTarget::Bool {
            let node = this.first_node(None)?;
            if node.is_some() {
                return Ok(Some(Value::Bool(true)));
            }
            return Ok(Some(Value::Bool(!Self::is_empty(this)?)));
        }
        let contents = Self::cast_contents(this)?;
        let base = match contents {
            Some(c) => Value::Str(ZStr::new(c)),
            None => Value::Null,
        };
        Ok(match target {
            CastTarget::String => Some(Value::Str(base.to_zstr(host)?)),
            CastTarget::Int => Some(Value::Int(base.to_long(host)?)),
            CastTarget::Float => Some(Value::Float(base.to_double(host)?)),
            CastTarget::Number => Some(match base {
                Value::Null => Value::Int(0),
                other => other.to_number(host)?,
            }),
            _ => None,
        })
    }

    /// `SimpleXMLElement::__toString()`.
    pub fn to_string(this: &SxeRef, host: &mut dyn SxeHost) -> RtResult<ZStr> {
        match Self::cast(this, host, CastTarget::String)? {
            Some(Value::Str(s)) => Ok(s),
            _ => Ok(ZStr::empty()),
        }
    }

    /// `sxe_prop_is_empty` (used by the bool cast).
    pub fn is_empty(this: &SxeRef) -> RtResult<bool> {
        let iter = this.iter_state();
        let Some(start) = this.node() else {
            return Err(not_initialized());
        };
        let mut node = Some(start);
        if iter.kind == IterType::Element {
            node = this.first_node(node)?;
        }
        if let Some(n) = &node {
            if n.kind() != lx::XML_ENTITY_DECL {
                let test = iter.name.is_some() && iter.kind == IterType::AttrList;
                let mut attr = n.properties();
                while let Some(a) = attr {
                    if iter.attr_matches(&a, test) {
                        return Ok(false);
                    }
                    attr = a.next();
                }
            }
        }
        let node = this.first_node(this.node())?;
        let Some(node) = node else { return Ok(true) };
        if iter.kind == IterType::AttrList {
            return Ok(true);
        }
        if node.is_attribute() {
            return Ok(false);
        }
        let mut use_iter = false;
        let mut cur = if iter.kind == IterType::Child {
            Some(node)
        } else {
            let first = node.children();
            let simple = iter.kind == IterType::None
                || first.is_none()
                || !node.has_parent()
                || first
                    .as_ref()
                    .is_some_and(|c| c.has_next() || c.has_children())
                || node.parent_has_single_child();
            if simple {
                first
            } else {
                use_iter = true;
                this.reset_first()?
            }
        };
        while let Some(n) = cur {
            let skip = if n.has_children() || n.has_prev() || n.has_next() {
                n.is_text()
            } else if n.is_text() {
                if n.raw_content().is_some_and(|c| !c.is_empty()) {
                    return Ok(false);
                }
                true
            } else {
                false
            };
            let ns_ok = !n.is_element() || iter.match_ns(&n);
            if !skip && ns_ok && n.name().is_some() {
                return Ok(false);
            }
            cur = if use_iter {
                iter.fetch(n.next())
            } else {
                n.next()
            };
        }
        Ok(true)
    }

    /// `get_base_node_value`.
    fn base_node_value(
        this: &Sxe,
        host: &mut dyn SxeHost,
        node: XNode,
        iter: &Iter,
    ) -> RtResult<Value> {
        if let Some(c) = node.children() {
            if c.is_text() && !c.is_blank() {
                return Ok(Value::Str(ZStr::new(
                    c.list_get_string().unwrap_or_default(),
                )));
            }
        }
        wrap(
            this,
            host,
            Some(node),
            IterType::None,
            None,
            iter.nsprefix.as_deref(),
            iter.isprefix,
        )
    }

    /// `sxe_get_prop_hash`: the property table used by `var_dump()`/
    /// `print_r()`/`var_export()` (`is_debug`, i.e. `__debugInfo()`) and by
    /// `(array)`, `get_object_vars()`, `json_encode()`, `foreach` over
    /// properties (`!is_debug`, the `get_properties` handler).
    #[allow(clippy::too_many_lines)]
    pub fn properties(this: &SxeRef, host: &mut dyn SxeHost, is_debug: bool) -> RtResult<ZArray> {
        let mut rv = HashTable::new();
        let iter = this.iter_state();
        let Some(start) = this.node() else {
            return Err(not_initialized());
        };
        if is_debug || iter.kind != IterType::Child {
            let mut node = Some(start.clone());
            if iter.kind == IterType::Element {
                node = this.first_node(node)?;
            }
            if let Some(n) = node.filter(|n| n.kind() != lx::XML_ENTITY_DECL) {
                let test = iter.name.is_some() && iter.kind == IterType::AttrList;
                let mut attrs: Option<HashTable> = None;
                let mut attr = n.properties();
                while let Some(a) = attr {
                    if iter.attr_matches(&a, test) {
                        let value = a
                            .children()
                            .and_then(|c| c.list_get_string())
                            .unwrap_or_default();
                        let name = a.name().unwrap_or_default();
                        attrs
                            .get_or_insert_with(HashTable::new)
                            .insert(ArrayKey::from_bytes(&name), Value::Str(ZStr::new(value)));
                    }
                    attr = a.next();
                }
                if let Some(t) = attrs {
                    add_prop(&mut rv, b"@attributes", Value::Array(ZArray::from_table(t)));
                }
            }
        }
        let node = this.first_node(Some(start))?;
        let Some(node) = node else {
            return Ok(ZArray::from_table(rv));
        };
        if iter.kind == IterType::AttrList {
            return Ok(ZArray::from_table(rv));
        }
        let mut use_iter = false;
        let mut cur = if node.is_attribute() {
            let s = node
                .children()
                .and_then(|c| c.list_get_string())
                .unwrap_or_default();
            let _ = rv.push(Value::Str(ZStr::new(s)));
            None
        } else if iter.kind == IterType::Child {
            Some(node)
        } else {
            let first = node.children();
            let simple = iter.kind == IterType::None
                || first.is_none()
                || !node.has_parent()
                || !node.has_next()
                || first
                    .as_ref()
                    .is_some_and(|c| c.has_next() || c.has_children())
                || node.parent_has_single_child();
            if simple {
                first
            } else {
                use_iter = true;
                this.reset_first()?
            }
        };
        while let Some(n) = cur {
            if out_of_memory() {
                // Stop building: the VM raises the pending "Allowed memory
                // size" fatal error at its next check.
                break;
            }
            let mut process = true;
            if n.has_children() || n.has_prev() || n.has_next() || n.is_blank() {
                if n.is_text() {
                    process = false;
                }
            } else if n.is_text() {
                if n.raw_content().is_some_and(|c| !c.is_empty()) {
                    let s = n.list_get_string().unwrap_or_default();
                    let _ = rv.push(Value::Str(ZStr::new(s)));
                }
                process = false;
            }
            if process && n.is_element() && !iter.match_ns(&n) {
                process = false;
            }
            if process {
                if let Some(name) = n.name() {
                    let value = Self::base_node_value(this, host, n.clone(), &iter)?;
                    if use_iter {
                        let _ = rv.push(value);
                    } else {
                        add_prop(&mut rv, &name, value);
                    }
                }
            }
            if n.kind() == lx::XML_ENTITY_DECL {
                break;
            }
            cur = if use_iter {
                iter.fetch(n.next())
            } else {
                n.next()
            };
        }
        Ok(ZArray::from_table(rv))
    }

    /// `compare` (`sxe_objects_compare`) for two `SimpleXML` objects:
    /// `0` (equal) or `1` (`ZEND_UNCOMPARABLE`).
    pub fn compare(a: &SxeRef, b: &SxeRef) -> RtResult<i32> {
        if Rc::ptr_eq(a, b) {
            return Ok(0);
        }
        match (a.node(), b.node()) {
            (Some(na), Some(nb)) => {
                let same_ptr = na == nb;
                let n1 = a.first_node(Some(na))?;
                let n2 = b.first_node(Some(nb))?;
                let equal = match (&n1, &n2) {
                    (Some(x), Some(y)) => x == y,
                    (None, None) => same_ptr,
                    _ => false,
                };
                Ok(i32::from(!equal))
            }
            (None, None) => match (a.document(), b.document()) {
                (Some(d1), Some(d2)) if d1 == d2 => Ok(0),
                _ => Ok(1),
            },
            _ => Ok(1),
        }
    }

    /// `SimpleXMLElement::count()` / the `count_elements` handler (the VM
    /// calls a user `count()` override instead when there is one).
    pub fn count(this: &SxeRef) -> RtResult<i64> {
        let iter = this.iter_state();
        let mut n = 0;
        let mut node = this.reset_first()?;
        while let Some(cur) = node {
            n += 1;
            node = iter.fetch(cur.next());
        }
        Ok(n)
    }

    /// `clone_obj` (`sxe_object_clone`): a root element clones the whole
    /// document, any other node is deep-copied (detached) into the same
    /// document.
    pub fn clone_object(this: &SxeRef) -> RtResult<SxeRef> {
        let st = this.state.borrow();
        let node = st.node.clone();
        let is_root = node.as_ref().is_some_and(XNode::parent_is_document);
        let mut out = State {
            iter: Iter {
                kind: st.iter.kind,
                name: st.iter.name.clone(),
                nsprefix: st.iter.nsprefix.clone(),
                isprefix: st.iter.isprefix,
            },
            ..State::default()
        };
        drop(st);
        if is_root {
            let doc = this.document().ok_or_else(not_initialized)?;
            // SAFETY: xmlCopyDoc returns a fresh document we take ownership of.
            let copy = unsafe { fx::xmlCopyDoc(doc.as_ptr(), 1) };
            let copy = NonNull::new(copy).ok_or_else(not_initialized)?;
            // SAFETY: the copy is exclusively ours.
            let copy = unsafe { Document::from_raw(copy) };
            out.node = copy.root().map(XNode::from_node);
            out.document = Some(copy);
        } else {
            out.document = this.document();
            out.modern = this.state.borrow().modern;
            if let (Some(n), Some(doc)) = (node, &out.document) {
                out.node = n
                    .node()
                    .and_then(|pin| doc.import(pin, true).ok())
                    .map(XNode::from_node);
            }
        }
        Ok(Rc::new(Sxe {
            class: this.class.clone(),
            state: RefCell::new(out),
        }))
    }
}

/// Parsing and construction.
impl Sxe {
    fn read_memory(data: &[u8], options: i32) -> Option<Document> {
        php_libxml::raw::ensure_init();
        let len = i32::try_from(data.len()).ok()?;
        // SAFETY: data is valid for len bytes; the document is owned by us.
        let doc = unsafe {
            fx::xmlReadMemory(
                data.as_ptr().cast(),
                len,
                std::ptr::null(),
                std::ptr::null(),
                options,
            )
        };
        // SAFETY: a fresh document nothing else references.
        NonNull::new(doc).map(|d| unsafe { Document::from_raw(d) })
    }

    fn read_file(path: &[u8], options: i32) -> Option<Document> {
        php_libxml::raw::ensure_init();
        let p = c_string(path);
        // SAFETY: valid C string; the document is owned by us.
        let doc = unsafe { fx::xmlReadFile(p.as_ptr(), std::ptr::null(), options) };
        // SAFETY: a fresh document nothing else references.
        NonNull::new(doc).map(|d| unsafe { Document::from_raw(d) })
    }

    fn init(&self, doc: Document, ns: &[u8], is_prefix: bool) {
        let mut st = self.state.borrow_mut();
        *st = State {
            iter: Iter {
                kind: st.iter.kind,
                nsprefix: (!ns.is_empty()).then(|| cstr(ns).to_vec()),
                isprefix: is_prefix,
                name: None,
            },
            node: doc.root().map(XNode::from_node),
            document: Some(doc),
            ..State::default()
        };
    }

    /// `SimpleXMLElement::__construct($data, $options, $dataIsURL,
    /// $namespaceOrPrefix, $isPrefix)`.
    pub fn construct(
        this: &SxeRef,
        data: &[u8],
        options: i64,
        data_is_url: bool,
        ns: &[u8],
        is_prefix: bool,
    ) -> RtResult<()> {
        if data.contains(&0) {
            return Err(RuntimeError::ValueError(
                "SimpleXMLElement::__construct(): Argument #1 ($data) must not contain any null bytes"
                    .into(),
            ));
        }
        let exception = |m: &str| RuntimeError::Custom {
            class: ZStr::from("Exception"),
            message: m.to_owned(),
        };
        if i32::try_from(data.len()).is_err() {
            return Err(exception(
                "SimpleXMLElement::__construct(): Argument #1 ($data) is too long",
            ));
        }
        let Ok(opts) = i32::try_from(options) else {
            return Err(exception(
                "SimpleXMLElement::__construct(): Argument #2 ($options) is invalid",
            ));
        };
        let doc = if data_is_url {
            Self::read_file(data, opts)
        } else {
            Self::read_memory(data, opts)
        };
        let doc = doc.ok_or_else(|| exception("String could not be parsed as XML"))?;
        this.init(doc, ns, is_prefix);
        Ok(())
    }

    /// `simplexml_load_string()`: `false` when parsing fails.
    pub fn load_string(
        host: &mut dyn SxeHost,
        data: &[u8],
        class: Option<&ZStr>,
        options: i64,
        ns: &[u8],
        is_prefix: bool,
    ) -> RtResult<Value> {
        if i32::try_from(data.len()).is_err() {
            return Err(RuntimeError::ValueError(
                "simplexml_load_string(): Argument #1 ($data) is too long".into(),
            ));
        }
        let Ok(opts) = i32::try_from(options) else {
            return Err(RuntimeError::ValueError(
                "simplexml_load_string(): Argument #3 ($options) is too large".into(),
            ));
        };
        let Some(doc) = Self::read_memory(data, opts) else {
            return Ok(Value::Bool(false));
        };
        Self::wrap_document(host, doc, class, ns, is_prefix)
    }

    /// `simplexml_load_file()`: `false` when loading fails.
    pub fn load_file(
        host: &mut dyn SxeHost,
        filename: &[u8],
        class: Option<&ZStr>,
        options: i64,
        ns: &[u8],
        is_prefix: bool,
    ) -> RtResult<Value> {
        if filename.contains(&0) {
            return Err(RuntimeError::ValueError(
                "simplexml_load_file(): Argument #1 ($filename) must not contain any null bytes"
                    .into(),
            ));
        }
        let Ok(opts) = i32::try_from(options) else {
            return Err(RuntimeError::ValueError(
                "simplexml_load_file(): Argument #3 ($options) is too large".into(),
            ));
        };
        let Some(doc) = Self::read_file(filename, opts) else {
            return Ok(Value::Bool(false));
        };
        Self::wrap_document(host, doc, class, ns, is_prefix)
    }

    fn wrap_document(
        host: &mut dyn SxeHost,
        doc: Document,
        class: Option<&ZStr>,
        ns: &[u8],
        is_prefix: bool,
    ) -> RtResult<Value> {
        let class = class
            .cloned()
            .unwrap_or_else(|| ZStr::from("SimpleXMLElement"));
        let sxe = Sxe::new(class.clone());
        sxe.init(doc, ns, is_prefix);
        host.sxe_wrap(&class, sxe)
    }

    /// An object of class `class` attached to the document node itself
    /// (what `XSLTProcessor::transformToDoc($doc, $returnClass)` creates:
    /// `getName()` is `""` and the root element is a child).
    pub fn from_document_node(
        host: &mut dyn SxeHost,
        doc: Document,
        class: Option<&ZStr>,
    ) -> RtResult<Value> {
        let class = class
            .cloned()
            .unwrap_or_else(|| ZStr::from("SimpleXMLElement"));
        let sxe = Sxe::new(class.clone());
        {
            let mut st = sxe.state.borrow_mut();
            st.node = Some(XNode::from_node(doc.as_node()));
            st.document = Some(doc);
        }
        host.sxe_wrap(&class, sxe)
    }

    /// `simplexml_import_dom($node, $class)`. `node` is the libxml node of
    /// the DOM object (`None` when the object is not a valid XML node).
    pub fn import_dom(
        host: &mut dyn SxeHost,
        node: Option<Node>,
        class: Option<&ZStr>,
    ) -> RtResult<Value> {
        let Some(node) = node else {
            return Err(RuntimeError::TypeError(
                "simplexml_import_dom(): Argument #1 ($node) must be a valid XML node".into(),
            ));
        };
        let doc = node.document();
        let target = match node.node_type() {
            php_libxml::tree::NodeType::Document | php_libxml::tree::NodeType::HtmlDocument => {
                doc.root()
            }
            _ => Some(node),
        };
        match target {
            Some(n) if n.node_type() == php_libxml::tree::NodeType::Element => {
                let class = class
                    .cloned()
                    .unwrap_or_else(|| ZStr::from("SimpleXMLElement"));
                let sxe = Sxe::new(class.clone());
                {
                    let mut st = sxe.state.borrow_mut();
                    st.document = Some(doc);
                    st.node = Some(XNode::from_node(n));
                }
                host.sxe_wrap(&class, sxe)
            }
            _ => {
                warn(host, "Invalid Nodetype to import")?;
                Ok(Value::Null)
            }
        }
    }

    /// `simplexml_export_node` (the `dom_import_simplexml()` hook): the
    /// node the object currently designates.
    pub fn export_node(this: &SxeRef) -> RtResult<Option<Node>> {
        let node = this.get_node()?;
        Ok(this.first_node(Some(node))?.and_then(|n| n.node().cloned()))
    }
}

/// The public methods of `SimpleXMLElement`.
impl Sxe {
    /// `SimpleXMLElement::xpath($expression)`: an array of elements,
    /// `null` (attribute lists / no node) or `false`.
    pub fn xpath(this: &SxeRef, host: &mut dyn SxeHost, expression: &[u8]) -> RtResult<Value> {
        if this.iter_type() == IterType::AttrList {
            return Ok(Value::Null);
        }
        let node = this.get_node()?;
        let Some(node) = this.first_node(Some(node))? else {
            return Ok(Value::Null);
        };
        let ctx = this.xpath_ctx()?;
        let hits = match eval_xpath(ctx, &node, expression) {
            Ok(hits) => hits,
            Err(None) => return Ok(Value::Bool(false)),
            Err(Some(kind)) => {
                let name = match kind {
                    fx::XPATH_BOOLEAN => "bool",
                    fx::XPATH_NUMBER => "number",
                    fx::XPATH_STRING => "string",
                    _ => "undefined",
                };
                warn(
                    host,
                    &format!("XPath expression must return a node set, {name} returned"),
                )?;
                return Ok(Value::Bool(false));
            }
        };
        // `array_init_size(return_value, nodeset->nodeNr)`: a packed array
        // of 16-byte slots behind a 56-byte header.
        let table = hits.len().saturating_mul(16).saturating_add(56);
        if php_memlimit::try_reserve(table).is_err() {
            // The VM raises "Allowed memory size ... exhausted" after the
            // call.
            return Ok(Value::Null);
        }
        let mut out = HashTable::new();
        for hit in hits {
            if out_of_memory() {
                break;
            }
            let v = match hit {
                Hit::Node(n) => wrap(this, host, Some(n), IterType::None, None, None, false)?,
                Hit::Attr(parent, name, href) => wrap(
                    this,
                    host,
                    Some(parent),
                    IterType::AttrList,
                    Some(&name),
                    href.as_deref(),
                    false,
                )?,
            };
            let _ = out.push(v);
        }
        Ok(Value::Array(ZArray::from_table(out)))
    }

    fn xpath_ctx(&self) -> RtResult<*mut fx::xmlXPathContext> {
        let doc = self.document().ok_or_else(not_initialized)?;
        let mut st = self.state.borrow_mut();
        if st.xpath.is_none() {
            php_libxml::raw::ensure_init();
            // SAFETY: a fresh context for the object's document.
            let ctx = unsafe { fx::xmlXPathNewContext(doc.as_ptr()) };
            st.xpath = Some(XPathCtx(NonNull::new(ctx).ok_or_else(not_initialized)?));
        }
        Ok(st
            .xpath
            .as_ref()
            .map_or(std::ptr::null_mut(), |c| c.0.as_ptr()))
    }

    /// `SimpleXMLElement::registerXPathNamespace($prefix, $namespace)`.
    pub fn register_xpath_namespace(
        this: &SxeRef,
        prefix: &[u8],
        namespace: &[u8],
    ) -> RtResult<bool> {
        if this.document().is_none() {
            return Err(not_initialized());
        }
        let ctx = this.xpath_ctx()?;
        let p = c_string(prefix);
        let n = c_string(namespace);
        // SAFETY: live context; valid C strings (copied by libxml2).
        let ret = unsafe { fx::xmlXPathRegisterNs(ctx, p.as_ptr().cast(), n.as_ptr().cast()) };
        Ok(ret == 0)
    }

    /// `SimpleXMLElement::asXML($filename)` / `saveXML()`: the XML string,
    /// or (with a filename) whether writing succeeded; `false` without a
    /// node.
    pub fn as_xml(this: &SxeRef, filename: Option<&[u8]>) -> RtResult<Value> {
        if filename.is_some_and(|f| f.contains(&0)) {
            return Err(RuntimeError::ValueError(
                "SimpleXMLElement::asXML(): Argument #1 ($filename) must not contain any null bytes"
                    .into(),
            ));
        }
        let node = this.get_node()?;
        let Some(node) = this.first_node(Some(node))? else {
            return Ok(Value::Bool(false));
        };
        let doc = node.document().clone();
        let encoding = doc.encoding();
        let whole_doc = node.parent_is_document();
        if let Some(f) = filename {
            php_libxml::raw::ensure_init();
            let path = c_string(f);
            let enc = encoding.as_deref().map(c_string);
            // SAFETY: valid C strings; document and node are live; the
            // output buffer is closed exactly once.
            let bytes = unsafe {
                if whole_doc {
                    i64::from(fx::xmlSaveFormatFileEnc(
                        path.as_ptr(),
                        doc.as_ptr(),
                        enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                        0,
                    ))
                } else {
                    let buf =
                        fx::xmlOutputBufferCreateFilename(path.as_ptr(), std::ptr::null_mut(), 0);
                    if buf.is_null() {
                        -1
                    } else {
                        lx::xmlNodeDumpOutput(
                            buf,
                            doc.as_ptr(),
                            node.ptr(),
                            0,
                            0,
                            std::ptr::null(),
                        );
                        i64::from(lx::xmlOutputBufferClose(buf))
                    }
                }
            };
            return Ok(Value::Bool(bytes != -1));
        }
        let out = if whole_doc {
            doc.save(0, encoding.as_deref())
        } else {
            node.node().map(|n| n.dump(false, encoding.as_deref()))
        };
        Ok(match out {
            Some(s) => Value::Str(ZStr::new(s)),
            None => Value::Bool(false),
        })
    }

    /// `SimpleXMLElement::getNamespaces($recursive)`.
    pub fn get_namespaces(this: &SxeRef, recursive: bool) -> RtResult<ZArray> {
        let mut out = HashTable::new();
        let node = this.get_node()?;
        if let Some(node) = this.first_node(Some(node))? {
            if node.is_element() {
                add_namespaces(&node, recursive, &mut out);
            } else if node.is_attribute() {
                if let Some(ns) = node.ns() {
                    add_ns_name(&mut out, ns.prefix.as_deref(), &ns.href);
                }
            }
        }
        Ok(ZArray::from_table(out))
    }

    /// `SimpleXMLElement::getDocNamespaces($recursive, $fromRoot)`: an
    /// array, or `false` without a root.
    ///
    /// For documents of the modern DOM ([`Sxe::set_modern_document`]),
    /// attributes in the `xmlns` namespace count as declarations too.
    pub fn get_doc_namespaces(this: &SxeRef, recursive: bool, from_root: bool) -> RtResult<Value> {
        let include_xmlns_attributes = this.state.borrow().modern;
        let node = if from_root {
            let doc = this.document().ok_or_else(not_initialized)?;
            doc.root().map(XNode::from_node)
        } else {
            Some(this.get_node()?)
        };
        let Some(node) = node else {
            return Ok(Value::Bool(false));
        };
        let mut out = HashTable::new();
        add_registered_namespaces(&node, recursive, include_xmlns_attributes, &mut out);
        Ok(Value::Array(ZArray::from_table(out)))
    }

    /// `SimpleXMLElement::children($namespaceOrPrefix, $isPrefix)`.
    pub fn children(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        ns: Option<&[u8]>,
        is_prefix: bool,
    ) -> RtResult<Value> {
        if this.iter_type() == IterType::AttrList {
            return Ok(Value::Null);
        }
        let node = this.get_node()?;
        let Some(node) = this.first_node(Some(node))? else {
            return Ok(Value::Null);
        };
        wrap(this, host, Some(node), IterType::Child, None, ns, is_prefix)
    }

    /// `SimpleXMLElement::attributes($namespaceOrPrefix, $isPrefix)`.
    pub fn attributes(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        ns: Option<&[u8]>,
        is_prefix: bool,
    ) -> RtResult<Value> {
        let node = this.get_node()?;
        let Some(node) = this.first_node(Some(node))? else {
            return Ok(Value::Null);
        };
        if this.iter_type() == IterType::AttrList {
            return Ok(Value::Null);
        }
        wrap(
            this,
            host,
            Some(node),
            IterType::AttrList,
            None,
            ns,
            is_prefix,
        )
    }

    /// `SimpleXMLElement::getName()`.
    pub fn get_name(this: &SxeRef) -> RtResult<Vec<u8>> {
        let node = this.get_node()?;
        Ok(this
            .first_node(Some(node))?
            .and_then(|n| n.name())
            .unwrap_or_default())
    }

    /// `SimpleXMLElement::addChild($qualifiedName, $value, $namespace)`.
    pub fn add_child(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        qname: &[u8],
        value: Option<&[u8]>,
        namespace: Option<&[u8]>,
    ) -> RtResult<Value> {
        if qname.is_empty() {
            return Err(RuntimeError::ValueError(
                "SimpleXMLElement::addChild(): Argument #1 ($qualifiedName) must not be empty"
                    .into(),
            ));
        }
        let node = this.get_node()?;
        if this.iter_type() == IterType::AttrList {
            warn(host, "Cannot add element to attributes")?;
            return Ok(Value::Null);
        }
        let Some(node) = this.first_node(Some(node))? else {
            warn(
                host,
                "Cannot add child. Parent is not a permanent member of the XML tree",
            )?;
            return Ok(Value::Null);
        };
        let (local, prefix) = split_qname(qname);
        let Some(newnode) = node.new_child(std::ptr::null_mut(), &local, value) else {
            return Ok(Value::Null);
        };
        if let Some(uri) = namespace {
            if uri.is_empty() {
                newnode.set_ns(std::ptr::null_mut());
                newnode.new_ns(uri, prefix.as_deref());
            } else {
                let mut ns = node.search_ns_by_href(uri);
                if ns.is_null() {
                    ns = newnode.new_ns(uri, prefix.as_deref());
                }
                newnode.set_ns(ns);
            }
        }
        let retprefix = if prefix.is_some() || namespace.is_some() {
            newnode.ns().and_then(|n| n.prefix)
        } else {
            None
        };
        wrap(
            this,
            host,
            Some(newnode),
            IterType::None,
            Some(&local),
            retprefix.as_deref(),
            true,
        )
    }

    /// `SimpleXMLElement::addAttribute($qualifiedName, $value, $namespace)`.
    pub fn add_attribute(
        this: &SxeRef,
        host: &mut dyn SxeHost,
        qname: &[u8],
        value: &[u8],
        namespace: Option<&[u8]>,
    ) -> RtResult<()> {
        if qname.is_empty() {
            return Err(RuntimeError::ValueError(
                "SimpleXMLElement::addAttribute(): Argument #1 ($qualifiedName) must not be empty"
                    .into(),
            ));
        }
        let node = this.get_node()?;
        let mut node = this.first_node(Some(node))?;
        if let Some(n) = &node {
            if !n.is_element() {
                node = n.parent();
            }
        }
        let Some(node) = node else {
            return warn(host, "Unable to locate parent Element");
        };
        let (local, prefix, split) = match split_qname2(qname) {
            Some((l, p)) => (l, Some(p), true),
            None => (qname.to_vec(), None, false),
        };
        if !split && namespace.is_some_and(|n| !n.is_empty()) {
            return warn(host, "Attribute requires prefix for namespace");
        }
        if node.has_ns_prop(&local, namespace) {
            return warn(host, "Attribute already exists");
        }
        let mut ns = std::ptr::null_mut();
        if let Some(uri) = namespace {
            ns = node.search_ns_by_href(uri);
            if ns.is_null() {
                ns = node.new_ns(uri, prefix.as_deref());
            }
        }
        node.new_prop(ns, &local, Some(value));
        Ok(())
    }
}

/// One `XPath` result as `SimpleXML` exposes it.
enum Hit {
    /// An element, comment or PI (text nodes map to their parent).
    Node(XNode),
    /// An attribute: its element, name and namespace URI.
    Attr(XNode, Vec<u8>, Option<Vec<u8>>),
}

/// Evaluates `expression` with `node` as context node and the node's
/// in-scope namespaces (`ctxt->namespaces`). `Err(None)` when evaluation
/// failed, `Err(Some(type))` for a non-node-set result.
fn eval_xpath(
    ctx: *mut fx::xmlXPathContext,
    node: &XNode,
    expression: &[u8],
) -> Result<Vec<Hit>, Option<i32>> {
    let query = c_string(expression);
    let doc = node.document().clone();
    let mut hits = Vec::new();
    // SAFETY: the context belongs to the node's document; the node and the
    // namespace array are live during the evaluation; every result node is
    // pinned (located) before the result object is freed.
    unsafe {
        (*ctx).node = node.ptr();
        let ns = fx::xmlGetNsList(doc.as_ptr(), node.ptr());
        let mut count = 0;
        if !ns.is_null() {
            while !(*ns.add(count)).is_null() {
                count += 1;
            }
        }
        (*ctx).namespaces = ns;
        (*ctx).nsNr = i32::try_from(count).unwrap_or(0);
        let ret = fx::xmlXPathEval(query.as_ptr().cast(), ctx);
        if !ns.is_null() {
            xml_free(ns.cast());
            (*ctx).namespaces = std::ptr::null_mut();
            (*ctx).nsNr = 0;
        }
        if ret.is_null() {
            return Err(None);
        }
        if (*ret).type_ != fx::XPATH_NODESET {
            let kind = (*ret).type_;
            fx::xmlXPathFreeObject(ret);
            return Err(Some(kind));
        }
        let set = (*ret).nodesetval;
        if !set.is_null() {
            let len = usize::try_from((*set).nodeNr).unwrap_or(0);
            for idx in 0..len {
                let p = *(*set).nodeTab.add(idx);
                match (*p).type_ {
                    lx::XML_TEXT_NODE => {
                        if let Some(parent) = node.locate((*p).parent) {
                            hits.push(Hit::Node(parent));
                        }
                    }
                    lx::XML_ATTRIBUTE_NODE => {
                        let attr = p.cast::<lx::xmlAttr>();
                        let name = crate::xnode::opt_bytes((*attr).name).unwrap_or_default();
                        let href = (*attr)
                            .ns
                            .as_ref()
                            .and_then(|ns| crate::xnode::opt_bytes(ns.href));
                        if let Some(parent) = node.locate((*attr).parent) {
                            hits.push(Hit::Attr(parent, name, href));
                        }
                    }
                    lx::XML_ELEMENT_NODE | lx::XML_PI_NODE | lx::XML_COMMENT_NODE => {
                        if let Some(x) = node.locate(p) {
                            hits.push(Hit::Node(x));
                        }
                    }
                    _ => {}
                }
            }
        }
        fx::xmlXPathFreeObject(ret);
    }
    Ok(hits)
}

/// `xmlSplitQName2`: `(local, prefix)` for `prefix:local`, `None` otherwise.
fn split_qname2(qname: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let q = c_string(qname);
    let mut prefix: *mut u8 = std::ptr::null_mut();
    // SAFETY: valid C string; both results are fresh allocations.
    unsafe {
        let local = take_xml_string(fx::xmlSplitQName2(q.as_ptr().cast(), &raw mut prefix));
        let prefix = take_xml_string(prefix);
        local.map(|l| (l, prefix.unwrap_or_default()))
    }
}

fn split_qname(qname: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    match split_qname2(qname) {
        Some((l, p)) => (l, Some(p)),
        None => (cstr(qname).to_vec(), None),
    }
}

/// `sxe_add_namespace_name_raw`: first binding of a prefix wins.
fn add_ns_name(out: &mut HashTable, prefix: Option<&[u8]>, href: &[u8]) {
    let key = ArrayKey::Str(ZStr::new(prefix.unwrap_or_default().to_vec()));
    if out.get(&key).is_none() {
        out.insert(key, Value::Str(ZStr::new(href.to_vec())));
    }
}

/// `sxe_add_namespaces`.
fn add_namespaces(node: &XNode, recursive: bool, out: &mut HashTable) {
    if let Some(ns) = node.ns() {
        add_ns_name(out, ns.prefix.as_deref(), &ns.href);
    }
    let mut attr = node.properties();
    while let Some(a) = attr {
        if let Some(ns) = a.ns() {
            add_ns_name(out, ns.prefix.as_deref(), &ns.href);
        }
        attr = a.next();
    }
    if recursive {
        let mut c = node.children();
        while let Some(child) = c {
            if child.is_element() {
                add_namespaces(&child, recursive, out);
            }
            c = child.next();
        }
    }
}

/// `sxe_add_registered_namespaces` (legacy documents: `xmlns` attributes
/// are not considered).
fn add_registered_namespaces(
    node: &XNode,
    recursive: bool,
    include_xmlns_attributes: bool,
    out: &mut HashTable,
) {
    const XMLNS: &[u8] = b"http://www.w3.org/2000/xmlns/";
    if !node.is_element() {
        return;
    }
    for (prefix, href) in node.ns_defs() {
        add_ns_name(out, prefix.as_deref(), &href);
    }
    if include_xmlns_attributes {
        // Attributes in the xmlns namespace are declarations too.
        let mut attr = node.properties();
        while let Some(a) = attr {
            if let Some(ns) = a.ns().filter(|ns| ns.href == XMLNS) {
                let prefix = if ns.prefix.is_some() {
                    a.name().unwrap_or_default()
                } else {
                    Vec::new()
                };
                let href = a
                    .children()
                    .and_then(|c| c.list_get_string())
                    .unwrap_or_default();
                add_ns_name(out, Some(&prefix), &href);
            }
            attr = a.next();
        }
    }
    if recursive {
        let mut c = node.children();
        while let Some(child) = c {
            add_registered_namespaces(&child, recursive, include_xmlns_attributes, out);
            c = child.next();
        }
    }
}

/// The iteration protocol (`Iterator`/`RecursiveIterator` methods and the
/// `get_iterator` handler used by `foreach`).
impl Sxe {
    /// `php_sxe_reset_iterator`: `rewind()`.
    pub fn rewind(this: &SxeRef, host: &mut dyn SxeHost) -> RtResult<()> {
        this.state.borrow_mut().data = None;
        let iter = this.iter_state();
        if let Some(node) = this.reset_first()? {
            let v = wrap(
                this,
                host,
                Some(node),
                IterType::None,
                None,
                iter.nsprefix.as_deref(),
                iter.isprefix,
            )?;
            this.state.borrow_mut().data = Some(v);
        }
        Ok(())
    }

    /// `valid()`.
    pub fn valid(this: &SxeRef) -> bool {
        this.state.borrow().data.is_some()
    }

    fn consumed() -> RuntimeError {
        RuntimeError::Error("Iterator not initialized or already consumed".into())
    }

    /// `current()`.
    pub fn current(this: &SxeRef) -> RtResult<Value> {
        this.state.borrow().data.clone().ok_or_else(Self::consumed)
    }

    /// The `foreach` current value (`None` when exhausted).
    pub fn iter_current(this: &SxeRef) -> Option<Value> {
        this.state.borrow().data.clone()
    }

    /// `key()`: the current element's name.
    pub fn key(this: &SxeRef, host: &mut dyn SxeHost) -> RtResult<Vec<u8>> {
        let data = this
            .state
            .borrow()
            .data
            .clone()
            .ok_or_else(Self::consumed)?;
        let inner = state_of(host, &data).ok_or_else(Self::consumed)?;
        let node = inner.node().ok_or_else(Self::consumed)?;
        Ok(node.name().unwrap_or_default())
    }

    /// The `foreach` key: the current element's name or `null`.
    pub fn iter_key(this: &SxeRef, host: &mut dyn SxeHost) -> Value {
        let data = this.state.borrow().data.clone();
        data.and_then(|d| state_of(host, &d))
            .and_then(|s| s.node())
            .map_or(Value::Null, |n| {
                Value::Str(ZStr::new(n.name().unwrap_or_default()))
            })
    }

    /// `php_sxe_move_forward_iterator`: `next()`.
    pub fn next(this: &SxeRef, host: &mut dyn SxeHost) -> RtResult<()> {
        let data = this.state.borrow_mut().data.take();
        let Some(data) = data else { return Ok(()) };
        let node = match state_of(host, &data) {
            Some(inner) => inner.get_node()?,
            None => return Ok(()),
        };
        drop(data);
        let iter = this.iter_state();
        if let Some(n) = iter.fetch(node.next()) {
            let v = wrap(
                this,
                host,
                Some(n),
                IterType::None,
                None,
                iter.nsprefix.as_deref(),
                iter.isprefix,
            )?;
            this.state.borrow_mut().data = Some(v);
        }
        Ok(())
    }

    /// `hasChildren()`.
    pub fn has_children(this: &SxeRef, host: &mut dyn SxeHost) -> RtResult<bool> {
        let data = this.state.borrow().data.clone();
        let Some(data) = data else { return Ok(false) };
        if this.iter_type() == IterType::AttrList {
            return Ok(false);
        }
        let Some(child) = state_of(host, &data) else {
            return Ok(false);
        };
        let node = child.get_node()?;
        let mut c = node.children();
        while let Some(n) = c {
            if n.is_element() {
                return Ok(true);
            }
            c = n.next();
        }
        Ok(false)
    }

    /// `getChildren()`.
    pub fn get_children(this: &SxeRef) -> Value {
        let st = this.state.borrow();
        if st.iter.kind == IterType::AttrList {
            return Value::Null;
        }
        st.data.clone().unwrap_or(Value::Null)
    }

    /// The `get_iterator` handler: `foreach` by reference is an error.
    pub fn get_iterator(by_ref: bool) -> RtResult<()> {
        if by_ref {
            return Err(RuntimeError::Error(
                "An iterator cannot be used with foreach by reference".into(),
            ));
        }
        Ok(())
    }
}
