//! `DOMXPath` / `Dom\XPath` (`xpath.c`, `xpath_callbacks.c`) and the fake
//! namespace-declaration nodes (`DOMNameSpaceNode`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::ptr;
use std::rc::Rc;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::host::{DomHost, HostCallable, XPathArg, XPathReturn};
use crate::lists::{new_list, MapKind, NodeList};
use crate::object::{DocRef, Node};
use crate::raw::{cs, opt_cstr, N, Ns};
use crate::value::DomValue;

/// `php_dom_create_fake_namespace_decl`.
pub(crate) fn create_fake_namespace_decl(parent: N, original: Ns, parent_obj: &Node) -> Node {
    let n = fake_ns_node_ptr(Some(parent), original, ptr::null_mut());
    Node::wrap_namespace(n, Some(parent_obj.clone()))
}

fn fake_ns_node_ptr(parent: Option<N>, original: Ns, fallback_doc: *mut ffi::xmlDoc) -> N {
    let doc = parent.map_or(fallback_doc, N::doc_ptr);
    // SAFETY: fresh allocations; the node is owned by its handle and freed
    // through node_free (XML_NAMESPACE_DECL branch).
    unsafe {
        let curns = ffi::xmlNewNs(ptr::null_mut(), original.href_ptr(), ptr::null());
        let attrp = if original.prefix_ptr().is_null() {
            ffi::xmlNewDocNode(doc, ptr::null_mut(), b"xmlns\0".as_ptr(), original.href_ptr())
        } else {
            (*curns).prefix = ffi::xmlStrdup(original.prefix_ptr());
            ffi::xmlNewDocNode(doc, ptr::null_mut(), original.prefix_ptr(), original.href_ptr())
        };
        let n = N::new(attrp).expect("allocation");
        n.set_type(ffi::XML_NAMESPACE_DECL);
        n.set_parent(parent);
        n.set_ns(curns);
        n
    }
}

/// Clones a `DOMNameSpaceNode` object (`dom_object_namespace_node_clone_obj`).
pub fn clone_namespace_node(node: &Node) -> Option<Node> {
    let n = node.n()?;
    let parent_obj = node.0.ns_parent.borrow().clone();
    let parent = parent_obj.as_ref().and_then(Node::n);
    let original = n.ns()?;
    let fake = fake_ns_node_ptr(parent, original, n.doc_ptr());
    let obj = Node::wrap_namespace(fake, parent_obj);
    Some(obj)
}

/// Registration mode of a callback namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    None,
    All,
    Set,
}

#[derive(Default)]
struct CallbackNs {
    functions: Vec<(Vec<u8>, HostCallable)>,
    mode: Mode,
}

impl CallbackNs {
    fn insert(&mut self, name: Vec<u8>, c: HostCallable) {
        if let Some(e) = self.functions.iter_mut().find(|(n, _)| *n == name) {
            e.1 = c;
        } else {
            self.functions.push((name, c));
        }
    }
}

#[derive(Default)]
struct Callbacks {
    php_ns: Option<CallbackNs>,
    namespaces: Vec<(Vec<u8>, CallbackNs)>,
}

impl Callbacks {
    fn ensure_ns(&mut self, ns: Option<&[u8]>) -> &mut CallbackNs {
        match ns {
            None => self.php_ns.get_or_insert_with(CallbackNs::default),
            Some(n) => {
                let pos = match self.namespaces.iter().position(|(k, _)| k == n) {
                    Some(p) => p,
                    None => {
                        self.namespaces.push((n.to_vec(), CallbackNs::default()));
                        self.namespaces.len() - 1
                    }
                };
                &mut self.namespaces[pos].1
            }
        }
    }
}

/// One entry of the `registerPhpFunctions()` restriction array.
#[derive(Clone)]
pub struct CallbackSpec {
    /// The array key (`None` for list entries).
    pub key: Option<Vec<u8>>,
    /// The entry's string value (used as name for list entries).
    pub value_string: Option<Vec<u8>>,
    /// The resolved callable, or the `zend_is_callable` error text.
    pub callable: Result<HostCallable, String>,
}

impl std::fmt::Debug for CallbackSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackSpec")
            .field("key", &self.key)
            .field("value_string", &self.value_string)
            .finish_non_exhaustive()
    }
}

/// The argument of `registerPhpFunctions()`.
#[derive(Debug, Clone)]
pub enum Restrict {
    /// `null`: allow every function.
    All,
    /// A single function name plus its resolution.
    Name(Vec<u8>, Result<HostCallable, String>),
    /// An array of names / callables.
    Array(Vec<CallbackSpec>),
}

impl std::fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callbacks").finish_non_exhaustive()
    }
}

struct XPathInner {
    ctx: Cell<*mut ffi::xmlXPathContext>,
    doc: RefCell<Option<DocRef>>,
    modern: bool,
    register_node_ns: Cell<bool>,
    callbacks: RefCell<Callbacks>,
    node_list: RefCell<Vec<Node>>,
    evaluation_depth: Cell<u32>,
}

impl Drop for XPathInner {
    fn drop(&mut self) {
        let ctx = self.ctx.get();
        if !ctx.is_null() {
            // SAFETY: the context is owned by this object.
            unsafe { ffi::xmlXPathFreeContext(ctx) };
        }
    }
}

/// A `DOMXPath` / `Dom\XPath` object.
#[derive(Clone)]
pub struct XPath(Rc<XPathInner>);

impl std::fmt::Debug for XPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XPath").field("modern", &self.0.modern).finish_non_exhaustive()
    }
}

/// What `evaluate()`/`query()` returned.
#[derive(Debug, Clone)]
pub enum XPathResult {
    /// A node list.
    NodeList(NodeList),
    /// A boolean.
    Bool(bool),
    /// A number.
    Float(f64),
    /// A string.
    Str(Vec<u8>),
    /// `null` (unsupported result type).
    Null,
    /// `false` (legacy failure, a warning was emitted).
    False,
}

impl XPathResult {
    /// As a generic value.
    pub fn into_value(self) -> DomValue {
        match self {
            Self::NodeList(l) => DomValue::NodeList(l),
            Self::Bool(b) => DomValue::Bool(b),
            Self::Float(f) => DomValue::Float(f),
            Self::Str(s) => DomValue::Str(s),
            Self::Null => DomValue::Null,
            Self::False => DomValue::Bool(false),
        }
    }
}

struct EvalCtx<'a> {
    xpath: &'a XPath,
    host: &'a dyn DomHost,
    error: Option<DomError>,
}

thread_local! {
    static EVAL: RefCell<Vec<*mut c_void>> = const { RefCell::new(Vec::new()) };
}

const PHP_XPATH_NS: &[u8] = b"http://php.net/xpath";

impl XPath {
    /// `new DOMXPath($document, $registerNodeNS)` / `new Dom\XPath()`.
    pub fn new(document: &Node, register_node_ns: bool) -> DomResult<XPath> {
        let docp = document.node()?;
        // SAFETY: live document; the context is owned by the new object.
        let ctx = unsafe { ffi::xmlXPathNewContext(docp.as_doc()) };
        if ctx.is_null() {
            return Err(DomError::dom(C::InvalidState));
        }
        // SAFETY: live context.
        unsafe {
            ffi::xmlXPathRegisterFuncNS(ctx, b"functionString\0".as_ptr(), b"http://php.net/xpath\0".as_ptr(), Some(ext_function_string_php));
            ffi::xmlXPathRegisterFuncNS(ctx, b"function\0".as_ptr(), b"http://php.net/xpath\0".as_ptr(), Some(ext_function_object_php));
        }
        Ok(XPath(Rc::new(XPathInner {
            ctx: Cell::new(ctx),
            doc: RefCell::new(document.doc_ref()),
            modern: document.family_is_modern(),
            register_node_ns: Cell::new(register_node_ns),
            callbacks: RefCell::new(Callbacks::default()),
            node_list: RefCell::new(Vec::new()),
            evaluation_depth: Cell::new(0),
        })))
    }

    /// Re-running `__construct()` on an existing object.
    pub fn reconstruct(&self, class: &str) -> DomResult<()> {
        if self.0.evaluation_depth.get() > 0 {
            return Err(DomError::Error(format!(
                "Cannot call {class}::__construct() while an XPath evaluation is in progress"
            )));
        }
        Ok(())
    }

    /// The PHP class name.
    pub fn class_name(&self) -> &'static str {
        if self.0.modern {
            "Dom\\XPath"
        } else {
            "DOMXPath"
        }
    }

    /// `$document`.
    pub fn document(&self) -> DomResult<Node> {
        let ctx = self.0.ctx.get();
        // SAFETY: live context.
        let doc = if ctx.is_null() { ptr::null_mut() } else { unsafe { (*ctx).doc } };
        let d = N::from_doc(doc).ok_or_else(|| DomError::dom(C::InvalidState))?;
        Ok(Node::wrap(d))
    }

    /// `$registerNodeNamespaces`.
    pub fn register_node_namespaces(&self) -> bool {
        self.0.register_node_ns.get()
    }

    /// `$registerNodeNamespaces = $value`.
    pub fn set_register_node_namespaces(&self, v: bool) {
        self.0.register_node_ns.set(v);
    }

    /// `registerNamespace($prefix, $namespace)`.
    pub fn register_namespace(&self, prefix: &[u8], uri: &[u8]) -> DomResult<bool> {
        let ctx = self.0.ctx.get();
        if ctx.is_null() {
            return Err(DomError::Error("Invalid XPath Context".into()));
        }
        let p = cs(prefix);
        let u = cs(uri);
        // SAFETY: live context.
        Ok(unsafe { ffi::xmlXPathRegisterNs(ctx, p.as_ptr().cast(), u.as_ptr().cast()) } == 0)
    }

    /// `query()` / `evaluate()`.
    pub fn eval(
        &self,
        expr: &[u8],
        context: Option<&Node>,
        register_node_ns: Option<bool>,
        evaluate: bool,
        host: &dyn DomHost,
    ) -> DomResult<XPathResult> {
        let modern = self.0.modern;
        let register_node_ns = register_node_ns.unwrap_or_else(|| self.0.register_node_ns.get());
        let ctxp = self.0.ctx.get();
        if ctxp.is_null() {
            return Err(DomError::Error("Invalid XPath Context".into()));
        }
        // SAFETY: live context.
        let docp = unsafe { (*ctxp).doc };
        if docp.is_null() {
            if modern {
                return Err(DomError::Error("Invalid XPath Document Pointer".into()));
            }
            crate::error::warning("Invalid XPath Document Pointer");
            return Ok(XPathResult::False);
        }
        let mut nodep = match context {
            Some(c) => Some(c.node()?),
            None => None,
        };
        if nodep.is_none() {
            // SAFETY: live document.
            nodep = N::new(unsafe { ffi::xmlDocGetRootElement(docp) });
        }
        if let Some(n) = nodep {
            if n.doc_ptr() != docp {
                return Err(DomError::Error("Node from wrong document".into()));
            }
        }
        // SAFETY: live context; saved fields restored below.
        let (old_node, old_ns, old_nr) = unsafe { ((*ctxp).node, (*ctxp).namespaces, (*ctxp).nsNr) };
        let mut in_scope: Vec<*mut ffi::xmlNs> = Vec::new();
        // SAFETY: live context.
        unsafe { (*ctxp).node = crate::raw::raw(nodep) };
        if register_node_ns {
            if let Some(n) = nodep {
                in_scope = if modern {
                    match self.0.doc.borrow().as_ref().and_then(|d| d.private_data()) {
                        Some(pd) => crate::ns::in_scope_ns(&mut pd.mapper.borrow_mut(), n, false),
                        None => Vec::new(),
                    }
                } else {
                    crate::ns::in_scope_ns_legacy(n)
                };
                // SAFETY: the vector outlives the evaluation.
                unsafe {
                    (*ctxp).namespaces = in_scope.as_mut_ptr();
                    (*ctxp).nsNr = c_int::try_from(in_scope.len()).unwrap_or(0);
                }
            }
        }
        let e = cs(expr);
        let mut eval_ctx = EvalCtx {
            xpath: self,
            host,
            error: None,
        };
        EVAL.with(|s| s.borrow_mut().push((&raw mut eval_ctx).cast()));
        self.0.evaluation_depth.set(self.0.evaluation_depth.get() + 1);
        // SAFETY: live context; userData is only read by our callbacks
        // during this call.
        let obj = crate::parse::with_generic_errors(|| unsafe {
            (*ctxp).userData = (&raw mut eval_ctx).cast();
            let o = ffi::xmlXPathEvalExpression(e.as_ptr().cast(), ctxp);
            (*ctxp).userData = ptr::null_mut();
            o
        });
        self.0.evaluation_depth.set(self.0.evaluation_depth.get() - 1);
        EVAL.with(|s| s.borrow_mut().pop());
        // SAFETY: restoring context fields.
        unsafe {
            (*ctxp).node = old_node;
            (*ctxp).namespaces = old_ns;
            (*ctxp).nsNr = old_nr;
        }
        drop(in_scope);
        crate::parse::resume_panic();
        if let Some(err) = eval_ctx.error.take() {
            if !obj.is_null() {
                // SAFETY: owned result.
                unsafe { ffi::xmlXPathFreeObject(obj) };
            }
            return Err(err);
        }
        if obj.is_null() {
            if modern {
                return Err(DomError::Error("Could not evaluate XPath expression".into()));
            }
            return Ok(XPathResult::False);
        }
        // SAFETY: obj is a valid result object, freed at the end.
        let result = unsafe {
            let t = if evaluate { (*obj).type_ } else { ffi::XPATH_NODESET };
            match t {
                ffi::XPATH_NODESET => {
                    let mut items = Vec::new();
                    let set = (*obj).nodesetval;
                    let mut err = None;
                    if (*obj).type_ == ffi::XPATH_NODESET && !set.is_null() && (*set).nodeNr > 0 {
                        for i in 0..usize::try_from((*set).nodeNr).unwrap_or(0) {
                            let node = *(*set).nodeTab.add(i);
                            let Some(n) = N::new(node) else { continue };
                            if n.t() == ffi::XML_NAMESPACE_DECL {
                                if modern {
                                    err = Some(DomError::dom_msg(
                                        C::NotSupported,
                                        "The namespace axis is not well-defined in the living DOM specification. Use Dom\\Element::getInScopeNamespaces() or Dom\\Element::getDescendantNamespaces() instead.",
                                    ));
                                    break;
                                }
                                let original = Ns::new(node.cast()).expect("non-null");
                                let nsparent = N::new(n.private().cast());
                                let parent_obj = nsparent.map(Node::wrap);
                                let fake = fake_ns_node_ptr(nsparent, original, ptr::null_mut());
                                items.push(Node::wrap_namespace(fake, parent_obj));
                            } else {
                                items.push(Node::wrap(n));
                            }
                        }
                    }
                    if let Some(e) = err {
                        ffi::xmlXPathFreeObject(obj);
                        return Err(e);
                    }
                    let class = if modern { "Dom\\NodeList" } else { "DOMNodeList" };
                    XPathResult::NodeList(new_list(class, None, MapKind::NodeSet(items)))
                }
                ffi::XPATH_BOOLEAN => XPathResult::Bool((*obj).boolval != 0),
                ffi::XPATH_NUMBER => XPathResult::Float((*obj).floatval),
                ffi::XPATH_STRING => XPathResult::Str(opt_cstr((*obj).stringval).unwrap_or_default().to_vec()),
                _ => XPathResult::Null,
            }
        };
        // SAFETY: owned result.
        unsafe { ffi::xmlXPathFreeObject(obj) };
        Ok(result)
    }

    /// `registerPhpFunctions($restrict)`.
    pub fn register_php_functions(&self, restrict: Restrict) -> DomResult<()> {
        let mut cbs = self.0.callbacks.borrow_mut();
        let ns = cbs.ensure_ns(None);
        match restrict {
            Restrict::Array(items) => {
                for item in items {
                    let callable = item.callable.map_err(|e| {
                        DomError::arg_type(1, format!("must be an array with valid callbacks as values, {e}"))
                    })?;
                    let name = match item.key {
                        Some(k) => k,
                        None => item.value_string.unwrap_or_default(),
                    };
                    if name.is_empty() || name.contains(&0) {
                        return Err(DomError::arg_value(1, "must be an array containing valid callback names"));
                    }
                    ns.insert(name, callable);
                }
                ns.mode = Mode::Set;
            }
            Restrict::Name(name, callable) => {
                if name.is_empty() || name.contains(&0) {
                    return Err(DomError::arg_value(1, "must be a valid callback name"));
                }
                let callable = callable.map_err(|e| DomError::arg_type(1, format!("must be a callable, {e}")))?;
                ns.insert(name, callable);
                ns.mode = Mode::Set;
            }
            Restrict::All => ns.mode = Mode::All,
        }
        Ok(())
    }

    /// `registerPhpFunctionNS($namespaceURI, $name, $callable)`.
    pub fn register_php_function_ns(&self, namespace: &[u8], name: &[u8], callable: HostCallable) -> DomResult<()> {
        if namespace == PHP_XPATH_NS {
            return Err(DomError::arg_value(
                1,
                "must not be \"http://php.net/xpath\" because it is reserved by PHP",
            ));
        }
        if name.is_empty() || name.contains(&0) || !crate::tree::valid_ncname(name) {
            return Err(DomError::arg_value(2, "must be a valid callback name"));
        }
        let mut cbs = self.0.callbacks.borrow_mut();
        let ns = cbs.ensure_ns(Some(namespace));
        ns.insert(name.to_vec(), callable);
        ns.mode = Mode::Set;
        let ctx = self.0.ctx.get();
        let n = cs(name);
        let u = cs(namespace);
        // SAFETY: live context.
        unsafe { ffi::xmlXPathRegisterFuncNS(ctx, n.as_ptr().cast(), u.as_ptr().cast(), Some(ext_function_trampoline)) };
        Ok(())
    }

    /// `DOMXPath::quote($str)`.
    pub fn quote(input: &[u8]) -> Vec<u8> {
        if !input.contains(&b'\'') {
            let mut v = Vec::with_capacity(input.len() + 2);
            v.push(b'\'');
            v.extend_from_slice(input);
            v.push(b'\'');
            return v;
        }
        if !input.contains(&b'"') {
            let mut v = Vec::with_capacity(input.len() + 2);
            v.push(b'"');
            v.extend_from_slice(input);
            v.push(b'"');
            return v;
        }
        let mut out = b"concat(".to_vec();
        let mut pos = 0;
        while pos < input.len() {
            let rest = &input[pos..];
            let ds = rest.iter().position(|&b| b == b'\'').unwrap_or(rest.len());
            let dd = rest.iter().position(|&b| b == b'"').unwrap_or(rest.len());
            let n = ds.max(dd);
            let q = if ds > dd { b'\'' } else { b'"' };
            out.push(q);
            out.extend_from_slice(&rest[..n]);
            out.push(q);
            pos += n;
            out.push(b',');
        }
        if let Some(last) = out.last_mut() {
            *last = b')';
        }
        out
    }
}

/// Evaluation mode for node-set arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodesetMode {
    ToString,
    ToNodeset,
}

fn current_eval<'a>(ctxt: *mut ffi::xmlXPathParserContext) -> Option<&'a mut EvalCtx<'a>> {
    // SAFETY: ctxt is a live parser context; its context's userData is the
    // EvalCtx installed by `eval` for the duration of the evaluation.
    unsafe {
        let c = (*ctxt).context;
        if c.is_null() || (*c).userData.is_null() {
            return None;
        }
        Some(&mut *(*c).userData.cast::<EvalCtx<'a>>())
    }
}

fn clean_argument_stack(ctxt: *mut ffi::xmlXPathParserContext, n: c_int) {
    for _ in 0..n {
        // SAFETY: popping values pushed by libxml2.
        unsafe { ffi::xmlXPathFreeObject(ffi::valuePop(ctxt)) };
    }
}

fn fetch_args(ctxt: *mut ffi::xmlXPathParserContext, count: usize, mode: NodesetMode) -> Vec<XPathArg> {
    let mut params = vec![XPathArg::Str(Vec::new()); count];
    for slot in params.iter_mut().rev() {
        // SAFETY: libxml2 pushed `count` values for this call.
        let obj = unsafe { ffi::valuePop(ctxt) };
        if obj.is_null() {
            continue;
        }
        // SAFETY: obj is a valid XPath object owned by us now.
        unsafe {
            *slot = match (*obj).type_ {
                ffi::XPATH_STRING => XPathArg::Str(opt_cstr((*obj).stringval).unwrap_or_default().to_vec()),
                ffi::XPATH_BOOLEAN => XPathArg::Bool((*obj).boolval != 0),
                ffi::XPATH_NUMBER => XPathArg::Float((*obj).floatval),
                ffi::XPATH_NODESET => {
                    if mode == NodesetMode::ToString {
                        XPathArg::Str(crate::raw::take(ffi::xmlXPathCastToString(obj)).unwrap_or_default())
                    } else {
                        let set = (*obj).nodesetval;
                        let mut nodes = Vec::new();
                        if !set.is_null() {
                            for j in 0..usize::try_from((*set).nodeNr).unwrap_or(0) {
                                let node = *(*set).nodeTab.add(j);
                                let Some(n) = N::new(node) else { continue };
                                if n.t() == ffi::XML_NAMESPACE_DECL {
                                    let original = Ns::new(node.cast()).expect("non-null");
                                    let parent = N::new(n.private().cast());
                                    let parent_obj = parent.map(Node::wrap);
                                    let fake = fake_ns_node_ptr(parent, original, ptr::null_mut());
                                    nodes.push(Node::wrap_namespace(fake, parent_obj));
                                } else {
                                    nodes.push(Node::wrap(n));
                                }
                            }
                        }
                        XPathArg::Nodes(nodes)
                    }
                }
                _ => XPathArg::Str(crate::raw::take(ffi::xmlXPathCastToString(obj)).unwrap_or_default()),
            };
            ffi::xmlXPathFreeObject(obj);
        }
    }
    params
}

fn keep_args_alive(ev: &EvalCtx<'_>, params: &[XPathArg]) {
    let mut list = ev.xpath.0.node_list.borrow_mut();
    for p in params {
        if let XPathArg::Nodes(ns) = p {
            list.extend(ns.iter().cloned());
        }
    }
}

fn push_sentinel(ctxt: *mut ffi::xmlXPathParserContext) {
    // SAFETY: pushing a fresh string object.
    unsafe { ffi::valuePush(ctxt, ffi::xmlXPathNewString(b"\0".as_ptr())) };
}

fn dispatch(
    ev: &mut EvalCtx<'_>,
    ns_key: Option<&[u8]>,
    ctxt: *mut ffi::xmlXPathParserContext,
    params: Vec<XPathArg>,
    function_name: &[u8],
) -> Result<(), DomError> {
    let (mode, callable) = {
        let cbs = ev.xpath.0.callbacks.borrow();
        let ns = match ns_key {
            None => cbs.php_ns.as_ref(),
            Some(k) => cbs.namespaces.iter().find(|(n, _)| n == k).map(|(_, v)| v),
        };
        match ns {
            None => (Mode::None, None),
            Some(ns) => (
                ns.mode,
                ns.functions.iter().find(|(n, _)| n == function_name).map(|(_, c)| c.clone()),
            ),
        }
    };
    let ret = match mode {
        Mode::None => return Err(DomError::Error("No callbacks were registered".into())),
        Mode::All => ev.host.call_by_name(function_name, params)?,
        Mode::Set => match callable {
            Some(c) => ev.host.call(&c, params)?,
            None => {
                return Err(DomError::Error(format!(
                    "No callback handler \"{}\" registered",
                    String::from_utf8_lossy(function_name)
                )))
            }
        },
    };
    let modern = ev.xpath.0.modern;
    // SAFETY: pushing fresh XPath objects onto the parser stack.
    unsafe {
        match ret {
            XPathReturn::Undef => {}
            XPathReturn::Node(n) if ev.host.is_dom_node(&n, modern) => {
                let p = crate::raw::raw(n.n());
                ev.xpath.0.node_list.borrow_mut().push(n);
                ffi::valuePush(ctxt, ffi::xmlXPathNewNodeSet(p));
            }
            XPathReturn::Node(_) | XPathReturn::NonNodeObject => {
                return Err(DomError::Type(
                    "Only objects that are instances of DOM nodes can be converted to an XPath expression".into(),
                ))
            }
            XPathReturn::Bool(b) => {
                ffi::valuePush(ctxt, ffi::xmlXPathNewBoolean(c_int::from(b)));
            }
            XPathReturn::Str(s) => {
                let c = cs(&s);
                ffi::valuePush(ctxt, ffi::xmlXPathNewString(c.as_ptr().cast()));
            }
        }
    }
    Ok(())
}

fn call_php_ns(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int, mode: NodesetMode) {
    let Some(ev) = current_eval(ctxt) else {
        clean_argument_stack(ctxt, nargs);
        return;
    };
    if ev.error.is_some() {
        clean_argument_stack(ctxt, nargs);
        push_sentinel(ctxt);
        return;
    }
    if nargs == 0 {
        ev.error = Some(DomError::Error("Function name must be passed as the first argument".into()));
        push_sentinel(ctxt);
        return;
    }
    let count = usize::try_from(nargs - 1).unwrap_or(0);
    let params = fetch_args(ctxt, count, mode);
    // SAFETY: the function name is the last value on the stack.
    let obj = unsafe { ffi::valuePop(ctxt) };
    // SAFETY: obj is NULL or a valid object owned by us.
    let name = unsafe { if obj.is_null() { None } else { opt_cstr((*obj).stringval).map(<[u8]>::to_vec) } };
    let result = match name {
        None => Err(DomError::Type("Handler name must be a string".into())),
        Some(name) => {
            keep_args_alive(ev, &params);
            dispatch(ev, None, ctxt, params, &name)
        }
    };
    if !obj.is_null() {
        // SAFETY: owned object.
        unsafe { ffi::xmlXPathFreeObject(obj) };
    }
    if let Err(e) = result {
        ev.error = Some(e);
        push_sentinel(ctxt);
    }
}

unsafe extern "C" fn ext_function_string_php(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    crate::parse::guard((), || call_php_ns(ctxt, nargs, NodesetMode::ToString));
}

unsafe extern "C" fn ext_function_object_php(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    crate::parse::guard((), || call_php_ns(ctxt, nargs, NodesetMode::ToNodeset));
}

unsafe extern "C" fn ext_function_trampoline(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    crate::parse::guard((), || {
        let Some(ev) = current_eval(ctxt) else {
            clean_argument_stack(ctxt, nargs);
            return;
        };
        let count = usize::try_from(nargs).unwrap_or(0);
        let params = fetch_args(ctxt, count, NodesetMode::ToNodeset);
        if ev.error.is_some() {
            push_sentinel(ctxt);
            return;
        }
        // SAFETY: live parser context; function/functionURI are set by
        // libxml2 for the duration of the call.
        let (ns, name) = unsafe {
            let c = (*ctxt).context;
            (
                opt_cstr((*c).functionURI).unwrap_or_default().to_vec(),
                opt_cstr((*c).function).unwrap_or_default().to_vec(),
            )
        };
        keep_args_alive(ev, &params);
        if let Err(e) = dispatch(ev, Some(&ns), ctxt, params, &name) {
            ev.error = Some(e);
            push_sentinel(ctxt);
        }
    });
}

/// Unused import guard.
#[allow(dead_code)]
fn _unused(_: HashMap<u8, u8>) {}
