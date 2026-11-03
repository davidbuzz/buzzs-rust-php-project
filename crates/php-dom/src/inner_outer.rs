//! `innerHTML` / `outerHTML` (`inner_outer_html_mixin.c`).

use crate::error::{DomError, DomResult};
use crate::object::Node;
use crate::raw::N;

/// `dom_parse_fragment`.
pub(crate) fn parse_fragment(_this: &Node, _context: N, _input: &[u8]) -> DomResult<N> {
    Err(DomError::Error("fragment parsing is not available".into()))
}

/// `$innerHTML`.
pub(crate) fn inner_html(_n: &Node) -> DomResult<Vec<u8>> {
    Err(DomError::Error("innerHTML is not available yet".into()))
}
/// `$outerHTML`.
pub(crate) fn outer_html(_n: &Node) -> DomResult<Vec<u8>> {
    Err(DomError::Error("outerHTML is not available yet".into()))
}
/// `$innerHTML = $v`.
pub(crate) fn set_inner_html(_n: &Node, _v: &[u8]) -> DomResult<()> {
    Err(DomError::Error("innerHTML is not available yet".into()))
}
/// `$outerHTML = $v`.
pub(crate) fn set_outer_html(_n: &Node, _v: &[u8]) -> DomResult<()> {
    Err(DomError::Error("outerHTML is not available yet".into()))
}
