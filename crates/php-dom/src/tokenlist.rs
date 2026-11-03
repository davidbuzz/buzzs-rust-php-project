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

use crate::error::{DomError, DomResult};
use crate::lists::Offset;
use crate::value::DomValue;

/// Iterator over a token list.
#[derive(Debug)]
pub struct TokenIter {
    tokens: Vec<Vec<u8>>,
    pos: usize,
}

impl TokenIter {
    /// Valid position?
    pub fn valid(&self) -> bool {
        self.pos < self.tokens.len()
    }
    /// Current token.
    pub fn current(&self) -> Option<Vec<u8>> {
        self.tokens.get(self.pos).cloned()
    }
    /// Current key.
    pub fn key(&self) -> i64 {
        i64::try_from(self.pos).unwrap_or(i64::MAX)
    }
    /// Advance.
    pub fn move_forward(&mut self) {
        self.pos += 1;
    }
}

impl TokenList {
    /// `$length`.
    pub fn length(&self) -> i64 {
        0
    }
    /// `$value`.
    pub fn value(&self) -> Vec<u8> {
        Vec::new()
    }
    /// `$value = $v`.
    pub fn set_value(&self, _v: &[u8]) -> DomResult<()> {
        Ok(())
    }
    /// Iteration.
    pub fn iter(&self) -> TokenIter {
        TokenIter { tokens: Vec::new(), pos: 0 }
    }
    /// `$list[$i]`.
    pub fn offset_get(&self, _o: &Offset) -> DomResult<Option<Vec<u8>>> {
        Ok(None)
    }
    /// `isset($list[$i])`.
    pub fn offset_exists(&self, _o: &Offset) -> DomResult<bool> {
        Ok(false)
    }
}

/// Method dispatch for `Dom\TokenList`.
pub(crate) fn call(_t: &TokenList, _func: &str, _m: &str, _args: &[DomValue]) -> DomResult<DomValue> {
    Err(DomError::Error("Dom\\TokenList is not available yet".into()))
}
