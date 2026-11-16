// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! CSS selectors: `querySelector()`, `querySelectorAll()`, `matches()`,
//! `closest()` (`parentnode/css_selectors.c`) over php-src's adapted lexbor
//! selector engine.

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::lexbor::{take_exception, CssCompileError, CssQuery};
use crate::lists::{new_list, MapKind, NodeList};
use crate::object::{Node, QuirksMode};
use crate::raw::N;

fn quirks(n: &Node) -> bool {
    n.doc_ref()
        .is_some_and(|d| d.quirks_mode.get() != QuirksMode::NoQuirks)
}

/// `dom_parse_selector`.
fn parse(n: &Node, selectors: &[u8]) -> DomResult<CssQuery> {
    CssQuery::compile(selectors, true, quirks(n)).map_err(|e| match e {
        CssCompileError::Message(m) => DomError::dom_msg(
            C::Syntax,
            format!("Invalid selector ({})", String::from_utf8_lossy(&m)),
        ),
        CssCompileError::Unknown => DomError::dom_msg(C::Syntax, "Invalid selector"),
    })
}

/// Turns an exception raised inside the engine, or an execution failure,
/// into the PHP error.
fn check(status: Result<(), ()>) -> DomResult<()> {
    if let Some((code, msg)) = take_exception() {
        let code = if code == 9 {
            C::NotSupported
        } else {
            C::Syntax
        };
        return Err(DomError::dom_msg(code, msg));
    }
    status.map_err(|()| DomError::arg_value(1, "contains an unsupported selector"))
}

/// `querySelector()`.
pub(crate) fn query_selector(n: &Node, selectors: &[u8]) -> DomResult<Option<Node>> {
    let thisp = n.node()?;
    let q = parse(n, selectors)?;
    let mut result: Option<N> = None;
    let st = q.find(thisp, &mut |node| {
        result = Some(node);
        true
    });
    check(st)?;
    Ok(result.map(Node::wrap))
}

/// `querySelectorAll()`.
pub(crate) fn query_selector_all(n: &Node, selectors: &[u8]) -> DomResult<NodeList> {
    let thisp = n.node()?;
    let q = parse(n, selectors)?;
    let mut found: Vec<N> = Vec::new();
    let st = q.find(thisp, &mut |node| {
        found.push(node);
        false
    });
    check(st)?;
    let nodes = found.into_iter().map(Node::wrap).collect();
    Ok(new_list("Dom\\NodeList", None, MapKind::NodeSet(nodes)))
}

fn matches_node(q: &CssQuery, node: N) -> DomResult<bool> {
    let mut result = false;
    let st = q.match_node(node, &mut |m| {
        if m == node {
            result = true;
            true
        } else {
            false
        }
    });
    check(st)?;
    Ok(result)
}

/// `matches()`.
pub(crate) fn matches(n: &Node, selectors: &[u8]) -> DomResult<bool> {
    let thisp = n.node()?;
    let q = parse(n, selectors)?;
    matches_node(&q, thisp)
}

/// `closest()`.
pub(crate) fn closest(n: &Node, selectors: &[u8]) -> DomResult<Option<Node>> {
    let thisp = n.node()?;
    let q = parse(n, selectors)?;
    let mut current = Some(thisp);
    while let Some(c) = current {
        if matches_node(&q, c)? {
            return Ok(Some(Node::wrap(c)));
        }
        current = c.parent();
    }
    Ok(None)
}
