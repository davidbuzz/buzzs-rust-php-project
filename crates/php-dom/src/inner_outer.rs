//! `innerHTML` / `outerHTML` (`inner_outer_html_mixin.c`).

use crate::error::{DomError, DomResult};
use crate::object::Node;
use crate::raw::N;

/// `dom_parse_fragment`.
pub(crate) fn parse_fragment(_this: &Node, _context: N, _input: &[u8]) -> DomResult<N> {
    Err(DomError::Error("fragment parsing is not available".into()))
}
