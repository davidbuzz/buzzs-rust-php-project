//! CSS selectors (`querySelector()` & co.).

use crate::error::{DomError, DomResult};
use crate::lists::NodeList;
use crate::object::Node;

fn todo<T>() -> DomResult<T> {
    Err(DomError::Error("CSS selectors are not available yet".into()))
}

/// `querySelector()`.
pub(crate) fn query_selector(_n: &Node, _s: &[u8]) -> DomResult<Option<Node>> {
    todo()
}
/// `querySelectorAll()`.
pub(crate) fn query_selector_all(_n: &Node, _s: &[u8]) -> DomResult<NodeList> {
    todo()
}
/// `matches()`.
pub(crate) fn matches(_n: &Node, _s: &[u8]) -> DomResult<bool> {
    todo()
}
/// `closest()`.
pub(crate) fn closest(_n: &Node, _s: &[u8]) -> DomResult<Option<Node>> {
    todo()
}
