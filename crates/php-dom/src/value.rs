//! Values crossing the DOM API boundary in the generic property / method
//! dispatchers.

use crate::lists::{NamedNodeMap, NodeList};
use crate::object::Node;

/// A PHP value produced or consumed by the DOM classes.
#[derive(Debug, Clone)]
pub enum DomValue {
    /// `null`.
    Null,
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `float`.
    Float(f64),
    /// `string`.
    Str(Vec<u8>),
    /// A node object.
    Node(Node),
    /// A node list / HTML collection.
    NodeList(NodeList),
    /// A named node map.
    NamedNodeMap(NamedNodeMap),
    /// A `DOMImplementation` / `Dom\Implementation` object.
    Implementation(crate::implementation::Implementation),
    /// A `Dom\TokenList`.
    TokenList(crate::tokenlist::TokenList),
    /// A `Dom\NamespaceInfo`.
    NamespaceInfo(NamespaceInfo),
    /// A PHP array (list or map).
    Array(Vec<(ArrayKey, DomValue)>),
}

/// An array key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayKey {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(Vec<u8>),
}

/// `Dom\NamespaceInfo` (readonly properties).
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    /// `$prefix`.
    pub prefix: Option<Vec<u8>>,
    /// `$namespaceURI`.
    pub namespace_uri: Option<Vec<u8>>,
    /// `$element`.
    pub element: Node,
}

impl DomValue {
    /// A string value.
    pub fn str(s: impl Into<Vec<u8>>) -> Self {
        Self::Str(s.into())
    }

    /// A nullable string value.
    pub fn opt_str(s: Option<impl Into<Vec<u8>>>) -> Self {
        s.map_or(Self::Null, |s| Self::Str(s.into()))
    }

    /// A nullable node value.
    pub fn opt_node(n: Option<Node>) -> Self {
        n.map_or(Self::Null, Self::Node)
    }

    /// Whether this value is an object.
    pub fn is_object(&self) -> bool {
        matches!(
            self,
            Self::Node(_)
                | Self::NodeList(_)
                | Self::NamedNodeMap(_)
                | Self::Implementation(_)
                | Self::TokenList(_)
                | Self::NamespaceInfo(_)
        )
    }

    /// Builds a list array.
    pub fn list(items: Vec<DomValue>) -> Self {
        Self::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (ArrayKey::Int(i64::try_from(i).unwrap_or(i64::MAX)), v))
                .collect(),
        )
    }

    /// The string payload, if any.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The node payload, if any.
    pub fn as_node(&self) -> Option<&Node> {
        match self {
            Self::Node(n) => Some(n),
            _ => None,
        }
    }
}
