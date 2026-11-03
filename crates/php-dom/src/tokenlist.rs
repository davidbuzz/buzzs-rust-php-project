//! `Dom\TokenList` (`token_list.c`) and the ordered-set helpers.

use crate::object::Node;

/// `Dom\TokenList` bound to an element attribute.
#[derive(Debug, Clone)]
pub struct TokenList {
    pub(crate) element: Node,
    pub(crate) attr: &'static [u8],
}

impl TokenList {
    pub(crate) fn new(element: Node, attr: &'static [u8]) -> Self {
        Self { element, attr }
    }
}

const ASCII_WS: &[u8] = b"\x09\x0a\x0c\x0d\x20";

/// `dom_ordered_set_parser`.
pub(crate) fn ordered_set_parse(value: &[u8], to_lowercase: bool) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for tok in value.split(|b| ASCII_WS.contains(b)).filter(|t| !t.is_empty()) {
        let t = if to_lowercase { tok.to_ascii_lowercase() } else { tok.to_vec() };
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// `dom_ordered_set_all_contained`.
pub(crate) fn ordered_set_all_contained(set: &[Vec<u8>], value: &[u8], to_lowercase: bool) -> bool {
    let tokens = ordered_set_parse(value, to_lowercase);
    set.iter().all(|s| tokens.contains(s))
}
