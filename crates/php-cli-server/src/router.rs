// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Observing the router script's return value.
//!
//! `php_cli_server_dispatch_router` runs the router as the primary script
//! and serves the requested file itself when the router returns `false`.
//! The VM's public `run_main` does not hand back the top-level return value,
//! so the router source is rewritten before compilation: every `return
//! <expr>` outside function bodies becomes `return
//! \__cli_server_router_return(<expr>)`, a native function that records
//! the value and returns it unchanged.  The rewrite only inserts text inside
//! lines, so line numbers, `__FILE__` and stack traces are unaffected.
//! Sources without a top-level `return <expr>` run unchanged.

use php_lexer::{token_get_all, TokenInfo, TokenKind};

/// The recording function's name.
pub const RETURN_HOOK: &str = "__cli_server_router_return";

fn is_trivia(t: &TokenInfo<'_>) -> bool {
    matches!(
        t.kind,
        TokenKind::Whitespace | TokenKind::Comment | TokenKind::DocComment
    )
}

fn ch(t: &TokenInfo<'_>) -> Option<u8> {
    t.kind.as_char()
}

fn opens(t: &TokenInfo<'_>) -> bool {
    matches!(ch(t), Some(b'(' | b'[' | b'{'))
        || matches!(
            t.kind,
            TokenKind::CurlyOpen | TokenKind::DollarOpenCurlyBraces
        )
}

fn closes(t: &TokenInfo<'_>) -> bool {
    matches!(ch(t), Some(b')' | b']' | b'}'))
}

/// Rewrites the top-level `return <expr>` statements of `src`; `None` when
/// there are none.
pub fn rewrite(src: &[u8]) -> Option<Vec<u8>> {
    let toks = token_get_all(src);
    // Inserts (byte offset, text), in increasing offset order.
    let mut inserts: Vec<(usize, String)> = Vec::new();
    // For each open `{`: whether it opened a function body.
    let mut braces: Vec<bool> = Vec::new();
    let mut pending_function = false;
    let mut paren_depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        match t.kind {
            TokenKind::Function => pending_function = true,
            TokenKind::CurlyOpen | TokenKind::DollarOpenCurlyBraces => braces.push(false),
            TokenKind::Return if !braces.iter().any(|&f| f) => {
                let mut j = i + 1;
                while j < toks.len() && is_trivia(&toks[j]) {
                    j += 1;
                }
                let bare = j >= toks.len()
                    || ch(&toks[j]) == Some(b';')
                    || toks[j].kind == TokenKind::CloseTag;
                if !bare {
                    // Find the end of the expression.
                    let mut depth = 0usize;
                    let mut k = j;
                    while k < toks.len() {
                        let tk = &toks[k];
                        if opens(tk) {
                            depth += 1;
                        } else if closes(tk) {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        } else if depth == 0
                            && (ch(tk) == Some(b';') || tk.kind == TokenKind::CloseTag)
                        {
                            break;
                        }
                        k += 1;
                    }
                    let end = toks.get(k).map_or(src.len(), |e| e.pos);
                    // Trailing trivia stays outside the call.
                    let mut last = k;
                    while last > j && is_trivia(&toks[last - 1]) {
                        last -= 1;
                    }
                    let close_at = if last < k { toks[last].pos } else { end };
                    inserts.push((toks[j].pos, format!("\\{RETURN_HOOK}(")));
                    inserts.push((close_at, ")".to_string()));
                }
            }
            _ => match ch(t) {
                Some(b'(') => paren_depth += 1,
                Some(b')') => paren_depth = paren_depth.saturating_sub(1),
                Some(b'{') => {
                    braces.push(pending_function);
                    pending_function = false;
                }
                Some(b'}') => {
                    braces.pop();
                }
                Some(b';') if paren_depth == 0 => pending_function = false,
                _ => {}
            },
        }
        i += 1;
    }
    if inserts.is_empty() {
        return None;
    }
    inserts.sort_by_key(|(p, _)| *p);
    let mut out = Vec::with_capacity(src.len() + inserts.len() * 32);
    let mut last = 0;
    for (pos, text) in inserts {
        out.extend_from_slice(&src[last..pos]);
        out.extend_from_slice(text.as_bytes());
        last = pos;
    }
    out.extend_from_slice(&src[last..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw(s: &str) -> Option<String> {
        rewrite(s.as_bytes()).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn top_level_returns() {
        assert_eq!(
            rw("<?php return false;").as_deref(),
            Some("<?php return \\__cli_server_router_return(false);")
        );
        assert_eq!(
            rw("<?php if ($x) { return f(1, [2]); } echo 1;").as_deref(),
            Some("<?php if ($x) { return \\__cli_server_router_return(f(1, [2])); } echo 1;")
        );
        assert_eq!(
            rw("<?php return FALSE ?>").as_deref(),
            Some("<?php return \\__cli_server_router_return(FALSE) ?>")
        );
    }

    #[test]
    fn function_returns_untouched() {
        assert_eq!(rw("<?php function f() { return 1; } f();"), None);
        assert_eq!(rw("<?php $f = function () use ($a) { return 1; };"), None);
        assert_eq!(
            rw("<?php class A { function f(): int { return 1; } } return \"{$a}\";").as_deref(),
            Some("<?php class A { function f(): int { return 1; } } return \\__cli_server_router_return(\"{$a}\");")
        );
        assert_eq!(rw("<?php return;"), None);
        assert_eq!(rw("<?php echo 1;"), None);
    }

    #[test]
    fn multiline_keeps_lines() {
        let src = "<?php\nif (1)\n  return\n    false\n  ;\n";
        let out = rw(src).unwrap();
        assert_eq!(out.matches('\n').count(), src.matches('\n').count());
    }
}
