// Derived from php-src ext/dom/token_list.c. Similarity metric to original: 6%
//   Copyright © The PHP Group and Contributors. Authors: Nora Dossche <ndossche@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `Dom\TokenList` (`token_list.c`) and the ordered-set helpers of
//! `infra.c`.
//!
//! The token set mirrors the Zend hash table PHP uses: deleted entries
//! leave tombstones so that iterator positions behave identically.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::lists::Offset;
use crate::object::Node;
use crate::value::DomValue;

const ASCII_WS: &[u8] = b"\x09\x0a\x0c\x0d\x20";

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_ws(b: &u8) -> bool {
    ASCII_WS.contains(b)
}

/// `dom_ordered_set_parser` (on the NUL-terminated view of `value`).
pub(crate) fn ordered_set_parse(value: &[u8], to_lowercase: bool) -> Vec<Vec<u8>> {
    let value = &value[..value.iter().position(|&b| b == 0).unwrap_or(value.len())];
    let mut out: Vec<Vec<u8>> = Vec::new();
    for tok in value.split(is_ws).filter(|t| !t.is_empty()) {
        let t = if to_lowercase {
            tok.to_ascii_lowercase()
        } else {
            tok.to_vec()
        };
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

#[derive(Debug, Default, Clone)]
struct TokenSet {
    buckets: Vec<Option<Vec<u8>>>,
}

impl TokenSet {
    fn parse(value: &[u8]) -> Self {
        Self {
            buckets: ordered_set_parse(value, false)
                .into_iter()
                .map(Some)
                .collect(),
        }
    }
    fn find(&self, t: &[u8]) -> Option<usize> {
        self.buckets.iter().position(|b| b.as_deref() == Some(t))
    }
    fn add(&mut self, t: &[u8]) {
        if self.find(t).is_none() {
            self.buckets.push(Some(t.to_vec()));
        }
    }
    fn count(&self) -> usize {
        self.buckets.iter().filter(|b| b.is_some()).count()
    }
    fn valid_pos(&self, mut pos: usize) -> usize {
        while pos < self.buckets.len() && self.buckets[pos].is_none() {
            pos += 1;
        }
        pos
    }
    fn move_forward(&self, pos: usize) -> usize {
        let idx = self.valid_pos(pos);
        if idx >= self.buckets.len() {
            return self.buckets.len();
        }
        self.valid_pos(idx + 1)
    }
    fn nth(&self, n: usize) -> Option<&[u8]> {
        self.buckets.iter().flatten().nth(n).map(Vec::as_slice)
    }
    fn serialize(&self) -> Vec<u8> {
        let toks: Vec<&[u8]> = self.buckets.iter().flatten().map(Vec::as_slice).collect();
        toks.join(&b' ')
    }
}

#[derive(Debug)]
struct Inner {
    element: Node,
    set: RefCell<TokenSet>,
    cached: RefCell<Option<Vec<u8>>>,
    cache_tag: Cell<u64>,
}

/// `Dom\TokenList` bound to the `class` attribute of an element.
#[derive(Debug, Clone)]
pub struct TokenList(Rc<Inner>);

impl PartialEq for TokenList {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}

impl TokenList {
    /// `dom_token_list_ctor`.
    pub(crate) fn new(element: Node, _attr: &'static [u8]) -> Self {
        let t = TokenList(Rc::new(Inner {
            element,
            set: RefCell::new(TokenSet::default()),
            cached: RefCell::new(None),
            cache_tag: Cell::new(0),
        }));
        let v = t.class_value();
        if let Some(v) = &v {
            *t.0.set.borrow_mut() = TokenSet::parse(v);
        }
        *t.0.cached.borrow_mut() = v;
        t
    }

    /// The element this list reflects.
    pub fn element(&self) -> &Node {
        &self.0.element
    }

    /// `dom_token_list_get_class_value`: `None` without attribute or
    /// without children.
    fn class_value(&self) -> Option<Vec<u8>> {
        let n = self.0.element.n()?;
        let attr = crate::element::has_ns_prop(n, b"class", None)?;
        attr.children()?;
        Some(crate::tree::attr_value(attr))
    }

    fn invalidate_tag(&self) {
        self.0.cache_tag.set(self.0.cache_tag.get().wrapping_add(1));
    }

    /// `dom_token_list_ensure_set_up_to_date`.
    fn ensure_up_to_date(&self) {
        let v = self.class_value();
        let same = crate::raw::str_eq(v.as_deref(), self.0.cached.borrow().as_deref());
        if !same {
            self.invalidate_tag();
            *self.0.set.borrow_mut() = v.as_deref().map(TokenSet::parse).unwrap_or_default();
            *self.0.cached.borrow_mut() = v;
        }
    }

    /// `dom_token_list_update`.
    fn update(&self) {
        self.invalidate_tag();
        if let Some(d) = self.0.element.doc_ref() {
            d.invalidate();
        }
        let Some(n) = self.0.element.n() else { return };
        let attr = crate::element::has_ns_prop(n, b"class", None);
        if attr.is_none() && self.0.set.borrow().count() == 0 {
            return;
        }
        let value = self.0.set.borrow().serialize();
        crate::tree::set_attribute_value(n, std::ptr::null_mut(), b"class", &value);
        *self.0.cached.borrow_mut() = Some(value);
    }

    /// `$length` / `count()`.
    pub fn length(&self) -> i64 {
        self.ensure_up_to_date();
        i64::try_from(self.0.set.borrow().count()).unwrap_or(i64::MAX)
    }

    /// `$value`.
    pub fn value(&self) -> Vec<u8> {
        self.class_value().unwrap_or_default()
    }

    /// `$value = $v`.
    pub fn set_value(&self, v: &[u8]) -> DomResult<()> {
        if v.contains(&0) {
            return Err(DomError::Value(
                "Value must not contain any null bytes".into(),
            ));
        }
        if let Some(d) = self.0.element.doc_ref() {
            d.invalidate();
        }
        if let Some(n) = self.0.element.n() {
            crate::tree::set_attribute_value(n, std::ptr::null_mut(), b"class", v);
        }
        Ok(())
    }

    /// `item($index)`.
    pub fn item(&self, index: i64) -> Option<Vec<u8>> {
        self.ensure_up_to_date();
        let set = self.0.set.borrow();
        if index < 0 {
            return None;
        }
        set.nth(usize::try_from(index).ok()?).map(<[u8]>::to_vec)
    }

    /// `contains($token)`.
    pub fn contains(&self, token: &[u8]) -> bool {
        self.ensure_up_to_date();
        self.0.set.borrow().find(token).is_some()
    }

    fn validate_token(t: &[u8]) -> DomResult<()> {
        if t.is_empty() {
            return Err(DomError::dom_msg(
                C::Syntax,
                "The empty string is not a valid token",
            ));
        }
        if t.iter().any(is_ws) {
            return Err(DomError::dom_msg(
                C::InvalidCharacter,
                "The token must not contain any ASCII whitespace",
            ));
        }
        Ok(())
    }

    fn validate_varargs(tokens: &[DomValue]) -> DomResult<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let arg = u32::try_from(i + 1).unwrap_or(u32::MAX);
            let DomValue::Str(s) = t else {
                return Err(DomError::arg_type(
                    arg,
                    format!("must be of type string, {} given", t.type_name()),
                ));
            };
            if s.contains(&0) {
                return Err(DomError::arg_value(arg, "must not contain any null bytes"));
            }
            Self::validate_token(s)?;
            out.push(s.clone());
        }
        Ok(out)
    }

    /// `add(...$tokens)`.
    pub fn add(&self, tokens: &[DomValue]) -> DomResult<()> {
        let toks = Self::validate_varargs(tokens)?;
        self.ensure_up_to_date();
        for t in &toks {
            self.0.set.borrow_mut().add(t);
        }
        self.update();
        Ok(())
    }

    /// `remove(...$tokens)`.
    pub fn remove(&self, tokens: &[DomValue]) -> DomResult<()> {
        let toks = Self::validate_varargs(tokens)?;
        self.ensure_up_to_date();
        for t in &toks {
            let mut set = self.0.set.borrow_mut();
            if let Some(i) = set.find(t) {
                set.buckets[i] = None;
            }
        }
        self.update();
        Ok(())
    }

    /// `toggle($token, $force)`.
    pub fn toggle(&self, token: &[u8], force: Option<bool>) -> DomResult<bool> {
        Self::validate_token(token)?;
        self.ensure_up_to_date();
        let found = self.0.set.borrow().find(token);
        if let Some(i) = found {
            if force != Some(true) {
                self.0.set.borrow_mut().buckets[i] = None;
                self.update();
                return Ok(false);
            }
            return Ok(true);
        }
        if force != Some(false) {
            self.0.set.borrow_mut().add(token);
            self.update();
            return Ok(true);
        }
        Ok(false)
    }

    /// `replace($token, $newToken)`.
    pub fn replace(&self, token: &[u8], new_token: &[u8]) -> DomResult<bool> {
        Self::validate_token(token)?;
        Self::validate_token(new_token)?;
        self.ensure_up_to_date();
        let found = self.0.set.borrow().find(token);
        let Some(i) = found else { return Ok(false) };
        {
            let mut set = self.0.set.borrow_mut();
            if set.find(new_token).is_some_and(|j| j != i) {
                set.buckets[i] = None;
            } else {
                set.buckets[i] = Some(new_token.to_vec());
            }
        }
        self.update();
        Ok(true)
    }

    /// `supports($token)`: always a `TypeError` for `classList`.
    pub fn supports(&self, _token: &[u8]) -> DomResult<bool> {
        Err(DomError::Type(
            "Attribute \"class\" does not define any supported tokens".into(),
        ))
    }

    /// `foreach`.
    #[allow(clippy::iter_not_returning_iterator)]
    pub fn iter(&self) -> TokenIter {
        self.ensure_up_to_date();
        let pos = self.0.set.borrow().valid_pos(0);
        TokenIter {
            list: self.clone(),
            pos,
            index: 0,
            cache_tag: self.0.cache_tag.get(),
        }
    }

    fn offset_index(o: &Offset) -> Result<i64, ()> {
        match o {
            Offset::Int(i) => Ok(*i),
            Offset::Float(f) => Ok(crate::lists::dval_to_lval_safe(*f)),
            Offset::Bool(b) => Ok(i64::from(*b)),
            Offset::Str(s) => crate::lists::handle_numeric(s).ok_or(()),
            Offset::Null | Offset::Array(_) | Offset::Other(_) => Err(()),
        }
    }

    fn illegal(o: &Offset, isset: bool) -> DomError {
        let ty = match o {
            Offset::Null => "null".to_string(),
            Offset::Str(_) => "string".into(),
            Offset::Array(_) => "array".into(),
            Offset::Other(t) => t.clone(),
            _ => "int".into(),
        };
        if isset {
            DomError::Type(format!(
                "Cannot access offset of type {ty} in isset or empty"
            ))
        } else {
            DomError::Type(format!(
                "Cannot access offset of type {ty} on Dom\\TokenList"
            ))
        }
    }

    /// `$list[$i]`.
    pub fn offset_get(&self, o: &Offset) -> DomResult<Option<Vec<u8>>> {
        match Self::offset_index(o) {
            Ok(i) => Ok(self.item(i)),
            Err(()) => Err(Self::illegal(o, false)),
        }
    }

    /// `isset($list[$i])`.
    pub fn offset_exists(&self, o: &Offset) -> DomResult<bool> {
        match Self::offset_index(o) {
            Ok(i) => {
                self.ensure_up_to_date();
                Ok(i >= 0 && i < i64::try_from(self.0.set.borrow().count()).unwrap_or(i64::MAX))
            }
            Err(()) => Err(Self::illegal(o, true)),
        }
    }
}

/// An iterator over a token list (`dom_token_list_it`).
#[derive(Debug)]
pub struct TokenIter {
    list: TokenList,
    pos: usize,
    index: i64,
    cache_tag: u64,
}

impl TokenIter {
    /// Valid position?
    pub fn valid(&mut self) -> bool {
        self.list.ensure_up_to_date();
        let set = self.list.0.set.borrow();
        self.pos = set.valid_pos(self.pos);
        self.pos < set.buckets.len()
    }

    /// Current token.
    pub fn current(&self) -> Option<Vec<u8>> {
        self.list.ensure_up_to_date();
        let set = self.list.0.set.borrow();
        set.buckets.get(set.valid_pos(self.pos)).cloned().flatten()
    }

    /// Current key (`dom_token_list_it_get_current_key`): after the set
    /// changed, the position's index in the new set; the iterator then
    /// takes the list's cache tag over (`php_libxml_is_cache_tag_stale`),
    /// so later moves count from there.
    pub fn key(&mut self) -> i64 {
        self.list.ensure_up_to_date();
        let tag = self.list.0.cache_tag.get();
        if tag != self.cache_tag {
            self.cache_tag = tag;
            let set = self.list.0.set.borrow();
            let mut i = 0;
            let mut p = set.valid_pos(0);
            while p != self.pos && p < set.buckets.len() {
                i += 1;
                p = set.move_forward(p);
            }
            self.index = i;
        }
        self.index
    }

    /// `dom_token_list_it_rewind`: back to the first token of the set as
    /// it stands (the next access brings it up to date), index 0.
    pub fn rewind(&mut self) {
        self.pos = self.list.0.set.borrow().valid_pos(0);
        self.index = 0;
    }

    /// Advance.
    pub fn move_forward(&mut self) {
        self.list.ensure_up_to_date();
        let set = self.list.0.set.borrow();
        let validated = set.valid_pos(self.pos);
        if validated == self.pos {
            self.pos = set.move_forward(self.pos);
        } else {
            self.pos = validated;
        }
        self.index += 1;
    }
}

/// Method dispatch for `Dom\TokenList`.
#[allow(clippy::many_single_char_names)]
pub(crate) fn call(t: &TokenList, _func: &str, m: &str, args: &[DomValue]) -> DomResult<DomValue> {
    let s = |i: usize| -> DomResult<Vec<u8>> {
        match args.get(i) {
            Some(DomValue::Str(s)) => {
                if s.contains(&0) {
                    return Err(DomError::arg_value(
                        u32::try_from(i + 1).unwrap_or(1),
                        "must not contain any null bytes",
                    ));
                }
                Ok(s.clone())
            }
            Some(DomValue::Int(n)) => Ok(n.to_string().into_bytes()),
            Some(other) => Err(DomError::arg_type(
                u32::try_from(i + 1).unwrap_or(1),
                format!("must be of type string, {} given", other.type_name()),
            )),
            None => Err(DomError::Error("Too few arguments".into())),
        }
    };
    Ok(match m {
        "item" => {
            let i = match args.first() {
                Some(DomValue::Int(i)) => *i,
                Some(DomValue::Float(f)) => {
                    #[allow(clippy::cast_possible_truncation)]
                    let v = *f as i64;
                    v
                }
                Some(DomValue::Bool(b)) => i64::from(*b),
                Some(DomValue::Str(s)) => std::str::from_utf8(s)
                    .ok()
                    .and_then(|t| t.trim().parse().ok())
                    .ok_or_else(|| DomError::arg_type(1, "must be of type int, string given"))?,
                Some(other) => {
                    return Err(DomError::arg_type(
                        1,
                        format!("must be of type int, {} given", other.type_name()),
                    ))
                }
                None => return Err(DomError::Error("Too few arguments".into())),
            };
            DomValue::opt_str(t.item(i))
        }
        "contains" => DomValue::Bool(t.contains(&s(0)?)),
        "add" => {
            t.add(args)?;
            DomValue::Null
        }
        "remove" => {
            t.remove(args)?;
            DomValue::Null
        }
        "toggle" => {
            let force = match args.get(1) {
                None | Some(DomValue::Null) => None,
                Some(v) => Some(match v {
                    DomValue::Bool(b) => *b,
                    DomValue::Int(i) => *i != 0,
                    DomValue::Str(s) => !(s.is_empty() || s == b"0"),
                    _ => true,
                }),
            };
            DomValue::Bool(t.toggle(&s(0)?, force)?)
        }
        "replace" => DomValue::Bool(t.replace(&s(0)?, &s(1)?)?),
        "supports" => DomValue::Bool(t.supports(&s(0)?)?),
        "count" => DomValue::Int(t.length()),
        "getiterator" => DomValue::Iterator(crate::dispatch::IterHandle(Rc::new(RefCell::new(
            crate::dispatch::DomIterator::Tokens(t.iter()),
        )))),
        _ => {
            return Err(DomError::Error(format!(
                "Call to undefined method Dom\\TokenList::{m}()"
            )))
        }
    })
}
