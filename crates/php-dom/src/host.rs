//! Call-backs into the embedding PHP engine.

use std::rc::Rc;

use crate::error::DomError;
use crate::object::Node;

/// An opaque, already-validated PHP callable owned by the embedder.
pub type HostCallable = Rc<dyn std::any::Any>;

/// An argument passed to an XPath PHP function.
#[derive(Debug, Clone)]
pub enum XPathArg {
    /// `string`.
    Str(Vec<u8>),
    /// `bool`.
    Bool(bool),
    /// `float`.
    Float(f64),
    /// A node-set (array of node objects).
    Nodes(Vec<Node>),
}

/// The value returned by an XPath PHP function.
#[derive(Debug, Clone)]
pub enum XPathReturn {
    /// No value (the call produced nothing, e.g. after an exception).
    Undef,
    /// A DOM node of the right family.
    Node(Node),
    /// `true`/`false`.
    Bool(bool),
    /// Any other object (not a DOM node of the right family): a
    /// `TypeError` is raised.
    NonNodeObject,
    /// Any other value, already converted with `(string)`.
    Str(Vec<u8>),
}

/// The engine services the DOM extension needs.
pub trait DomHost {
    /// `instanceof`: whether class `class` (case-insensitive) is `base` or a
    /// subclass of it. Unknown classes return false.
    fn is_subclass_of(&self, class: &str, base: &str) -> bool;

    /// Whether the class is abstract.
    fn is_abstract(&self, class: &str) -> bool {
        let _ = class;
        false
    }

    /// The declared spelling of a class name, if the class exists.
    fn canonical_name(&self, class: &str) -> Option<String> {
        Some(class.to_string())
    }

    /// Calls a registered callable with XPath arguments.
    fn call(&self, callable: &HostCallable, args: Vec<XPathArg>) -> Result<XPathReturn, DomError>;

    /// Calls a PHP function by name (`registerPhpFunctions()` without a
    /// restriction list).
    fn call_by_name(&self, name: &[u8], args: Vec<XPathArg>) -> Result<XPathReturn, DomError>;

    /// Whether `node` is an instance of the DOM node class of the given
    /// family (always true for nodes produced by this crate).
    fn is_dom_node(&self, node: &Node, modern: bool) -> bool {
        node.family_is_modern() == modern
    }
}

/// A host with no PHP callbacks (useful for tests and tools).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHost;

impl DomHost for NoHost {
    fn is_subclass_of(&self, class: &str, base: &str) -> bool {
        class.eq_ignore_ascii_case(base)
    }

    fn call(&self, _callable: &HostCallable, _args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        Ok(XPathReturn::Undef)
    }

    fn call_by_name(&self, name: &[u8], _args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        Err(DomError::Error(format!(
            "Invalid callback {}, function \"{}\" not found or invalid function name",
            String::from_utf8_lossy(name),
            String::from_utf8_lossy(name)
        )))
    }
}
