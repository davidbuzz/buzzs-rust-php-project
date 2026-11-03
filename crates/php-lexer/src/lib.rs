//! A tokenizer for PHP source code, ported from the Zend engine's re2c
//! scanner (`Zend/zend_language_scanner.l`).
//!
//! PHP source is a byte string, so the lexer operates on `&[u8]`.
//!
//! * [`Lexer`] is the scanner. In [`LexMode::Tokenizer`] it behaves like
//!   `token_get_all()`; in [`LexMode::Parser`] it behaves like the compiler's
//!   scanner (token values, `?>` as `;`, scanner errors as `T_ERROR`).
//! * [`token_get_all`] returns the same `(id, text, line)` triples as PHP's
//!   `token_get_all()` without flags.
//!
//! ```
//! use php_lexer::{token_get_all, TokenKind};
//! let toks = token_get_all(b"<?php echo 1;");
//! assert_eq!(toks[1].kind, TokenKind::Echo);
//! assert_eq!(toks[1].text, b"echo");
//! ```

#![warn(missing_docs)]

mod lexer;
mod literal;
mod token;

pub use lexer::{LexError, LexMode, Lexer, LexerOptions};
pub use literal::{
    count_newlines, is_label_char, is_label_start, parse_float_literal, parse_lnum,
    parse_prefixed_int, strip_indentation, unescape_double_quoted, unescape_single_quoted,
    EscapeError, IndentError, LexWarning, LnumResult, Unescaped, WarningKind,
};
pub use token::{Span, Token, TokenKind, TokenValue};

/// One element of a `token_get_all()` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenInfo<'a> {
    /// The token kind; [`TokenKind::id`] gives PHP's numeric id.
    pub kind: TokenKind,
    /// The token text.
    pub text: &'a [u8],
    /// Line on which the token starts.
    pub line: u32,
    /// Byte offset of the token in the source (as `PhpToken::$pos`).
    pub pos: usize,
}

impl TokenInfo<'_> {
    /// Whether PHP represents this token as a bare string rather than an
    /// `[id, text, line]` array (single-character tokens).
    pub fn is_char(&self) -> bool {
        self.kind.as_char().is_some()
    }
}

/// Tokenizes `src` exactly like PHP's `token_get_all($src)` (no flags),
/// including the special handling of `__halt_compiler`.
pub fn token_get_all(src: &[u8]) -> Vec<TokenInfo<'_>> {
    token_get_all_with(src, LexerOptions::default())
}

/// Like [`token_get_all`] with explicit lexer options (the mode is forced to
/// [`LexMode::Tokenizer`]).
pub fn token_get_all_with(src: &[u8], opts: LexerOptions) -> Vec<TokenInfo<'_>> {
    let mut lexer = Lexer::with_options(
        src,
        LexerOptions {
            mode: LexMode::Tokenizer,
            ..opts
        },
    );
    let mut out = Vec::new();
    let mut need_tokens: Option<u32> = None;
    while let Some(tok) = lexer.next_token() {
        out.push(TokenInfo {
            kind: tok.kind,
            text: tok.text(src),
            line: tok.span.line,
            pos: tok.span.start,
        });
        if let Some(n) = need_tokens.as_mut() {
            if !tok.kind.is_trivia() {
                *n -= 1;
                if *n == 0 {
                    let rest = lexer.offset();
                    if rest < src.len() {
                        out.push(TokenInfo {
                            kind: TokenKind::InlineHtml,
                            text: &src[rest..],
                            line: tok.span.line,
                            pos: rest,
                        });
                    }
                    break;
                }
            }
        } else if tok.kind == TokenKind::HaltCompiler {
            need_tokens = Some(3);
        }
    }
    out
}

#[cfg(test)]
mod tests;
