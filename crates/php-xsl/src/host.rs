//! The services the embedding PHP engine provides to a transformation.
//!
//! During `transformTo*()`, libxslt evaluates `php:function()`,
//! `php:functionString()` and functions registered with
//! `registerPHPFunctionNS()`; the processor marshals their XPath
//! arguments into [`CallbackArg`]s and asks the [`XslHost`] to call the
//! PHP callable, then converts the returned [`CallbackReturn`] back into an
//! XPath value (`php_dom_xpath_callbacks_call_php_ns` and friends).

use std::marker::PhantomData;
use std::ptr::NonNull;

use php_libxml::tree::Node;

use crate::error::XslError;
use crate::ffi;
use crate::raw;

/// Information about the callback being invoked.
#[derive(Debug, Clone, Copy)]
pub struct CallbackContext<'a> {
    /// `None` for `php:function`/`php:functionString`, else the namespace
    /// URI of a `registerPHPFunctionNS()` function.
    pub namespace: Option<&'a [u8]>,
    /// The function name (for `php:function` the first XPath argument).
    pub function: &'a [u8],
    /// Whether the main document is a `Dom\` (modern) document: a returned
    /// node must then be a `Dom\Node`, otherwise a `DOMNode`
    /// (`dom_get_node_ce(php_dom_follow_spec_node(doc))`).
    pub modern: bool,
    /// The transformation's private copy of the input document, when the
    /// stylesheet uses `xsl:key` or `$cloneDocument` is set. Attached nodes
    /// handed to the host then belong to this document: keep a clone of it
    /// alive for as long as such nodes are referenced.
    pub document_copy: Option<&'a php_libxml::tree::Document>,
}

/// One argument of a PHP callback.
#[derive(Debug)]
pub enum CallbackArg<'a> {
    /// An XPath string (also node-sets in `php:functionString`, numbers…
    /// cast to string for other object types).
    Str(Vec<u8>),
    /// An XPath boolean.
    Bool(bool),
    /// An XPath number.
    Float(f64),
    /// A node-set (a PHP array of DOM nodes, possibly empty).
    Nodes(Vec<NodeArg<'a>>),
}

/// A node of a node-set argument (`xsl_proxy_factory`).
#[derive(Debug)]
pub enum NodeArg<'a> {
    /// A node of the main document (the transformation input, or its
    /// private copy — see [`CallbackContext::document_copy`]).
    Node(NodeRef<'a>),
    /// A node from another document (`document()`, result tree fragments
    /// …), deep-copied into the main document as a detached node, as PHP
    /// does to avoid handing out nodes libxslt frees.
    Copy(OwnedNode<'a>),
    /// An XPath namespace node: a `DOMNameSpaceNode` for the declaration
    /// `prefix`/`href` on the element `parent`.
    Namespace {
        /// The element carrying the declaration.
        parent: Box<NodeArg<'a>>,
        /// The prefix (`None` for the default namespace).
        prefix: Option<Vec<u8>>,
        /// The namespace URI.
        href: Vec<u8>,
    },
}

/// A borrowed node handed to the host during one callback.
///
/// The pointer is valid while the callback runs and the node is not
/// removed from its document; hosts convert it into their own node
/// objects before running user code.
#[derive(Debug, Clone, Copy)]
pub struct NodeRef<'a> {
    pub(crate) ptr: NonNull<ffi::xmlNode>,
    pub(crate) _scope: PhantomData<&'a ffi::xmlNode>,
}

impl NodeRef<'_> {
    /// The node pointer.
    pub fn as_ptr(&self) -> *mut ffi::xmlNode {
        self.ptr.as_ptr()
    }

    /// The node type (`xmlElementType`).
    ///
    /// # Safety
    /// The node must still be alive (see the type documentation).
    pub unsafe fn node_type(&self) -> i32 {
        // SAFETY: forwarded to the caller.
        unsafe { raw::node_type(self.ptr) }
    }

    /// The node name (`node->name`; empty for nameless nodes).
    ///
    /// # Safety
    /// The node must still be alive.
    pub unsafe fn name(&self) -> Vec<u8> {
        // SAFETY: forwarded to the caller.
        unsafe { raw::node_name(self.ptr) }
    }

    /// `xmlNodeGetContent` (the DOM `textContent`/attribute `value`).
    ///
    /// # Safety
    /// The node must still be alive.
    pub unsafe fn content(&self) -> Vec<u8> {
        // SAFETY: forwarded to the caller.
        unsafe { raw::node_content(self.ptr) }
    }

    /// `xmlGetNodePath` (`DOMNode::getNodePath()`).
    ///
    /// # Safety
    /// The node must still be alive.
    pub unsafe fn path(&self) -> Option<Vec<u8>> {
        // SAFETY: forwarded to the caller.
        unsafe { raw::node_path(self.ptr) }
    }

    /// The node's document.
    ///
    /// # Safety
    /// The node must still be alive.
    pub unsafe fn document_ptr(&self) -> *mut ffi::xmlDoc {
        // SAFETY: forwarded to the caller.
        unsafe { raw::node_doc(self.ptr) }
    }
}

/// A detached node owned by the argument: freed on drop unless the host
/// takes it over with [`OwnedNode::into_raw`].
#[derive(Debug)]
pub struct OwnedNode<'a> {
    pub(crate) ptr: NonNull<ffi::xmlNode>,
    pub(crate) _scope: PhantomData<&'a ffi::xmlNode>,
}

impl<'a> OwnedNode<'a> {
    /// Borrows the node.
    pub fn node(&self) -> NodeRef<'a> {
        NodeRef {
            ptr: self.ptr,
            _scope: PhantomData,
        }
    }

    /// Releases ownership: the host must free the node (`xmlFreeNode`, or
    /// `xmlFreeDoc` if it is a document node) when its wrapper dies, like
    /// any detached DOM node.
    pub fn into_raw(self) -> NonNull<ffi::xmlNode> {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }
}

impl Drop for OwnedNode<'_> {
    fn drop(&mut self) {
        // SAFETY: the node is a detached copy we exclusively own.
        unsafe { raw::free_detached(self.ptr) };
    }
}

/// The value a PHP callback returned (after the host's conversion).
#[derive(Debug)]
pub enum CallbackReturn {
    /// No value (the call threw, or did not run).
    Undef,
    /// A DOM node of the right family (see [`CallbackContext::modern`]):
    /// becomes a one-node node-set.
    Node(ReturnedNode),
    /// `true`/`false`.
    Bool(bool),
    /// Any other object: raises "Only objects that are instances of DOM
    /// nodes can be converted to an XPath expression".
    NonNodeObject,
    /// Any other value, already converted with `(string)`.
    Str(Vec<u8>),
}

/// A node returned by a callback, kept alive until the end of the
/// transformation.
#[derive(Debug)]
pub struct ReturnedNode(pub(crate) Returned);

#[derive(Debug)]
pub(crate) enum Returned {
    Raw(NonNull<ffi::xmlNode>),
    Handle(Node),
}

impl ReturnedNode {
    /// A node of the host's object model.
    ///
    /// # Safety
    /// The node must stay alive until [`XslHost::transformation_finished`]
    /// is called for the running transformation (PHP keeps the returned
    /// object in the callbacks' node list until then).
    pub unsafe fn from_raw(ptr: NonNull<ffi::xmlNode>) -> Self {
        Self(Returned::Raw(ptr))
    }

    /// A `php-libxml` node handle (kept alive by the processor).
    pub fn from_node(node: Node) -> Self {
        Self(Returned::Handle(node))
    }

    pub(crate) fn ptr(&self) -> NonNull<ffi::xmlNode> {
        match &self.0 {
            Returned::Raw(p) => *p,
            Returned::Handle(n) => NonNull::new(n.as_ptr()).expect("a Node handle is never null"),
        }
    }
}

/// The engine services a transformation needs.
///
/// `C` is the embedder's representation of an already-validated callable
/// (`zend_fcall_info_cache`).
pub trait XslHost<C> {
    /// Calls the PHP function named `ctx.function` with `args`
    /// (`registerPHPFunctions()` without restriction: `zend_call_function`
    /// with a string callable). An uncallable name throws (e.g. "Invalid
    /// callback f, function \"f\" not found or invalid function name").
    fn call_by_name(
        &mut self,
        ctx: &CallbackContext<'_>,
        args: Vec<CallbackArg<'_>>,
    ) -> CallbackReturn;

    /// Calls a registered callable (`zend_call_known_fcc`).
    fn call(
        &mut self,
        callable: &C,
        ctx: &CallbackContext<'_>,
        args: Vec<CallbackArg<'_>>,
    ) -> CallbackReturn;

    /// Throws an exception raised by the callback machinery ("No callbacks
    /// were registered", "Handler name must be a string", …).
    fn throw(&mut self, error: XslError);

    /// Whether an exception is pending (`EG(exception)`). While one is,
    /// callables are not invoked (`zend_call_function` returns early).
    fn exception_pending(&self) -> bool;

    /// `php_error_docref(NULL, E_WARNING, message)` from the active method
    /// ("Could not apply parameter …", "Can't set libxslt security
    /// properties …").
    fn warning(&mut self, message: &str);

    /// `php_check_open_basedir(path)` for the profiling file: `false`
    /// skips profiling (the host reports its own warning).
    fn open_basedir_allows(&mut self, path: &[u8]) -> bool {
        let _ = path;
        true
    }

    /// The transformation is over: release the objects created for
    /// callback arguments and return values
    /// (`php_dom_xpath_callbacks_clean_node_list`). Runs before the
    /// processor leaves its "transformation in progress" state.
    fn transformation_finished(&mut self) {}
}
