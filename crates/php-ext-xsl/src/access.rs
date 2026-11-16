// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `DomAccess`: how the XML object models (DOM, SimpleXML) hand their
//! documents to `XSLTProcessor`.
//!
//! In PHP, `ext/xsl` reaches the libxml2 tree behind a `DOMDocument`,
//! `Dom\Document`, any DOM node or a `SimpleXMLElement` through
//! `php_libxml_import_node()` (every libxml-based extension registers an
//! exporter for its class hierarchy), and builds DOM objects for
//! callback arguments / `transformToDoc()` results with
//! `php_dom_create_object()`.
//!
//! phprust's equivalent is the [`DomAccess`] trait: a binding crate
//! (`php-ext-dom`, `php-ext-simplexml`) implements it once and registers a
//! `'static` instance with [`register`] from its `Extension::register`
//! (startup). The registry is process-wide and idempotent per
//! [`DomAccess::name`]; registration order between extensions does not
//! matter.
//!
//! # Contract for the DOM binding (`php-ext-dom`)
//!
//! * [`DomAccess::name`] must return `"dom"`: the result of
//!   `transformToDoc()` without `$returnClass` is always created by the
//!   provider with that name (PHP always returns a `DOMDocument` then,
//!   even for SimpleXML input).
//! * [`DomAccess::root_classes`]: `["DOMNode", "Dom\\Node"]`.
//! * [`DomAccess::import`] answers `Ok(None)` for objects that are not
//!   DOM nodes; otherwise [`ImportedNode::Document`] for document objects,
//!   [`ImportedNode::Node`] for other attached nodes and
//!   [`ImportedNode::Detached`] for nodes without a document. The
//!   document is usually passed as [`XmlSource::raw`]: the `xmlDoc`
//!   pointer plus a keep-alive handle that prevents the document from
//!   being freed while the processor uses it (the processor may read it,
//!   copy it, and during a transformation hand pointers to its nodes back
//!   to [`DomAccess::node_value`]). Set `modern` for `Dom\` documents.
//! * [`DomAccess::owner_document`] returns the PHP object of a node's
//!   owner document (`php_dom_create_object(nodep->doc)`), used by
//!   `importStylesheet()` to clone the document when its class has a
//!   `__clone()` method; [`DomAccess::document_shared`] answers whether a
//!   *cloned* document object's tree is referenced by any other PHP
//!   object (`clone_lxml_obj->document->refcount > 1`), which makes
//!   `importStylesheet()` fail with "must not have its clone retained by
//!   __clone()". (The clone itself is made by this crate: a new instance,
//!   the class's native `clone` hook, then `__clone()`.)
//! * [`DomAccess::wrap_document`] adopts a result tree
//!   (`ResultDocument::into_raw()`): with `class == None` it creates a
//!   `DOMDocument`; otherwise an instance of `class` (already checked to
//!   derive from the input's hierarchy root) wrapping the tree, without
//!   calling a constructor.
//! * [`DomAccess::node_value`] creates the PHP object for a node-set
//!   member passed to a `php:function()` callback (`xsl_proxy_factory`):
//!   [`NodeArg::Node`] is a node of the transformation's main document
//!   (the input, or its private copy [`CallbackContext::document_copy`]
//!   when the stylesheet uses `xsl:key` or `$cloneDocument` is set; keep
//!   a clone of that `Document` alive for as long as the PHP object
//!   lives), [`NodeArg::Copy`] a detached copy the provider takes over
//!   with `OwnedNode::into_raw()`, [`NodeArg::Namespace`] a
//!   `DOMNameSpaceNode`.
//! * [`DomAccess::returned_node`] maps a callback's return value back to
//!   a node when it is a DOM node of the right family
//!   (`modern` selects `Dom\Node` vs `DOMNode`); the node must stay alive
//!   until [`DomAccess::transformation_finished`].
//!
//! # Contract for the SimpleXML binding (`php-ext-simplexml`)
//!
//! Name `"simplexml"`, root classes `["SimpleXMLElement"]`; `import`
//! returns [`ImportedNode::Node`] with [`XmlSource::Node`] (the element's
//! `php_libxml::tree::Node`), and `wrap_document` wraps
//! `ResultDocument::into_document()` in an instance of `class` (the
//! `$returnClass`, a `SimpleXMLElement` subclass) positioned on the
//! *document node* (PHP attaches the object to `newdocp` itself, so
//! `getName()` is `""` and the root element appears as a child). Callback node-sets are always DOM objects, so the SimpleXML
//! provider leaves `node_value`/`returned_node` at their defaults.

use std::any::Any;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{PoisonError, RwLock};

use php_libxml::tree::{Document, Node};
use php_runtime::{Obj, Value};
use php_vm::class::Class;
use php_vm::{Vm, VmResult};
use php_xsl::{CallbackContext, NodeArg, RawDocument, ResultDocument, ReturnedNode, XmlDocument};

/// The libxml2 document behind a PHP object.
pub enum XmlSource {
    /// A `php-libxml` document handle.
    Document(Document),
    /// A `php-libxml` node handle (its document is used).
    Node(Node),
    /// Another object model's document (see [`XmlSource::raw`]).
    Raw {
        /// The document.
        doc: RawDocument,
        /// Keeps the document alive while the source exists.
        keepalive: Rc<dyn Any>,
    },
}

impl std::fmt::Debug for XmlSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(d) => f.debug_tuple("Document").field(d).finish(),
            Self::Node(n) => f.debug_tuple("Node").field(n).finish(),
            Self::Raw { doc, .. } => f
                .debug_struct("Raw")
                .field("doc", doc)
                .finish_non_exhaustive(),
        }
    }
}

impl XmlSource {
    /// Wraps another object model's `xmlDoc`.
    ///
    /// # Safety
    /// `ptr` must point to a live `xmlDoc` that is not freed while
    /// `keepalive` (or a clone of it) exists.
    pub unsafe fn raw(
        ptr: NonNull<php_xsl::ffi::xmlDoc>,
        modern: bool,
        keepalive: Rc<dyn Any>,
    ) -> Self {
        // SAFETY: forwarded to the caller.
        let doc = unsafe { RawDocument::new(ptr, modern) };
        Self::Raw { doc, keepalive }
    }

    fn inner(&self) -> &dyn XmlDocument {
        match self {
            Self::Document(d) => d,
            Self::Node(n) => n,
            Self::Raw { doc, .. } => doc,
        }
    }
}

// SAFETY: each variant keeps its document alive for as long as it exists:
// `Document`/`Node` handles by reference counting, `Raw` by the contract of
// `XmlSource::raw`.
unsafe impl XmlDocument for XmlSource {
    fn xml_doc(&self) -> NonNull<php_xsl::ffi::xmlDoc> {
        self.inner().xml_doc()
    }

    fn is_modern(&self) -> bool {
        self.inner().is_modern()
    }
}

/// What [`DomAccess::import`] found behind an object
/// (`php_libxml_import_node`).
#[derive(Debug)]
pub enum ImportedNode {
    /// The object wraps the document node itself.
    Document(XmlSource),
    /// The object wraps another node of a document.
    Node(XmlSource),
    /// The object wraps a node that belongs to no document.
    Detached,
}

impl ImportedNode {
    /// The document, unless detached.
    pub fn into_source(self) -> Option<XmlSource> {
        match self {
            Self::Document(s) | Self::Node(s) => Some(s),
            Self::Detached => None,
        }
    }
}

/// A provider of XML documents for `XSLTProcessor` (see the module
/// documentation for the contract).
pub trait DomAccess: Sync {
    /// The provider's name (`"dom"`, `"simplexml"`).
    fn name(&self) -> &'static str;

    /// The roots of the class hierarchies whose objects this provider
    /// imports (`["DOMNode", "Dom\\Node"]`, `["SimpleXMLElement"]`).
    fn root_classes(&self) -> &'static [&'static str];

    /// `php_libxml_import_node`: `Ok(None)` if `obj` is not one of this
    /// provider's objects.
    fn import(&self, vm: &mut Vm, obj: &Obj) -> VmResult<Option<ImportedNode>>;

    /// The PHP object of the owner document of the node object `obj`
    /// (only asked for [`ImportedNode::Node`] results). `None` means the
    /// node object itself is cloned instead.
    fn owner_document(&self, vm: &mut Vm, obj: &Obj) -> VmResult<Option<Obj>> {
        let _ = (vm, obj);
        Ok(None)
    }

    /// Whether the tree of the freshly cloned object `clone` is
    /// referenced by any other PHP object.
    fn document_shared(&self, clone: &Obj) -> bool {
        let _ = clone;
        false
    }

    /// Creates the PHP object for a `transformToDoc()` result: a default
    /// document object (`class == None`, DOM provider only) or an
    /// instance of `class`.
    fn wrap_document(
        &self,
        vm: &mut Vm,
        doc: ResultDocument,
        class: Option<&Rc<Class>>,
    ) -> VmResult<Value>;

    /// The PHP value of a node-set member passed to a callback; `input` is
    /// the transformation input. `Ok(None)` if unsupported.
    fn node_value(
        &self,
        vm: &mut Vm,
        input: &XmlSource,
        node: NodeArg<'_>,
        ctx: &CallbackContext<'_>,
    ) -> VmResult<Option<Value>> {
        let _ = (vm, input, node, ctx);
        Ok(None)
    }

    /// The node behind a callback's return value, if it is a node object
    /// of the family selected by `modern`.
    fn returned_node(&self, vm: &mut Vm, value: &Obj, modern: bool) -> Option<ReturnedNode> {
        let _ = (vm, value, modern);
        None
    }

    /// The transformation is over: objects created by
    /// [`DomAccess::node_value`] may release their transformation-scoped
    /// state.
    fn transformation_finished(&self, vm: &mut Vm) {
        let _ = vm;
    }
}

static PROVIDERS: RwLock<Vec<&'static dyn DomAccess>> = RwLock::new(Vec::new());

/// Registers a provider (replacing a previous one with the same name).
pub fn register(provider: &'static dyn DomAccess) {
    let mut list = PROVIDERS.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = list.iter_mut().find(|p| p.name() == provider.name()) {
        *slot = provider;
    } else {
        list.push(provider);
    }
}

/// The registered providers, in registration order.
pub fn providers() -> Vec<&'static dyn DomAccess> {
    PROVIDERS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The provider named `name`.
pub fn provider(name: &str) -> Option<&'static dyn DomAccess> {
    providers().into_iter().find(|p| p.name() == name)
}

/// `php_libxml_import_node`: the first provider that recognises `obj`.
pub fn import(vm: &mut Vm, obj: &Obj) -> VmResult<Option<(&'static dyn DomAccess, ImportedNode)>> {
    for p in providers() {
        if let Some(found) = p.import(vm, obj)? {
            return Ok(Some((p, found)));
        }
    }
    Ok(None)
}
