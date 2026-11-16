// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A hand-written recursive-descent parser (with precedence climbing for
//! binary operators) for the grammar of `Zend/zend_language_parser.y`.
//!
//! Syntax errors reproduce bison's messages: the parser mirrors the LALR
//! automaton closely enough at each error site to name the same unexpected
//! token and the same short list of expected tokens.

mod decl;
mod expr;
mod stmt;

use crate::ast::{Diagnostic, DiagnosticLevel, Ident, Name, NameKind, Program, Span};
use crate::error::{syntax_error_message, ParseError};
use php_lexer::{
    LexError, LexMode, Lexer, LexerOptions, ScriptEncodingOptions, TokenKind, TokenValue,
    WarningKind,
};
use std::borrow::Cow;
use std::collections::VecDeque;

/// Parser configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ParseOptions {
    /// Whether `<?` opens PHP code (`short_open_tag`).
    pub short_open_tag: bool,
    /// Start in PHP mode instead of inline HTML (used by `eval()`).
    pub start_in_scripting: bool,
    /// Skip a leading `#!` line (CLI primary script).
    pub skip_shebang: bool,
    /// Skip the compile-time checks of [`crate::validate`] and only report
    /// errors the grammar itself raises.
    pub syntax_only: bool,
    /// Record the tokens accepted as identifiers in
    /// [`Program::identifier_tokens`](crate::ast::Program::identifier_tokens).
    pub record_identifier_tokens: bool,
    /// Which thread [`crate::parse`] parses on.
    pub stack: crate::StackPolicy,
    /// Enable Zend multibyte script filtering and `declare(encoding=...)`.
    /// [`crate::parse`] uses default encoding settings; hosts can supply INI
    /// settings through [`crate::parse_with_encoding`].
    pub multibyte: bool,
}

/// Result type of parser functions.
pub(crate) type PResult<T> = Result<T, ParseError>;

/// A token with its semantic value.
///
/// Kept small (80 bytes rather than 104) because every token is moved into
/// and out of the lookahead buffer: the source text is a borrowed slice, and
/// the rare payloads (a preceding doc comment, text re-encoded by a script
/// encoding filter) live out of line in [`TokExtra`].
#[derive(Clone, Debug)]
pub(crate) struct Tok<'a> {
    pub kind: TokenKind,
    pub end_line: u32,
    pub span: Span,
    pub value: Option<TokenValue>,
    /// The token's text when it is borrowed from the input (otherwise
    /// empty, with the owned text in `extra`).
    text: &'a [u8],
    extra: Option<Box<TokExtra>>,
}

/// Rarely present parts of a [`Tok`].
#[derive(Clone, Debug)]
struct TokExtra {
    /// Text that could not be borrowed from the input (filtered or
    /// re-encoded by the script encoding).
    text: Option<Vec<u8>>,
    /// Doc comment scanned in the trivia before this token.
    doc: Option<Vec<u8>>,
}

impl<'a> Tok<'a> {
    /// A token without semantic value, doc comment or owned text.
    fn plain(kind: TokenKind, span: Span, end_line: u32, text: &'a [u8]) -> Self {
        Tok {
            kind,
            end_line,
            span,
            value: None,
            text,
            extra: None,
        }
    }

    /// The source text of the token.
    #[inline]
    pub fn text(&self) -> &[u8] {
        match &self.extra {
            Some(extra) => extra.text.as_deref().unwrap_or(self.text),
            None => self.text,
        }
    }

    /// Takes the doc comment scanned before this token.
    fn take_doc(&mut self) -> Option<Vec<u8>> {
        self.extra.as_mut().and_then(|extra| extra.doc.take())
    }
}

/// The parser state.
#[allow(clippy::struct_excessive_bools)] // parser state flags
pub(crate) struct Parser<'a> {
    lexer: Lexer<'a>,
    buf: VecDeque<Tok<'a>>,
    /// Whether the doc-comment effects of `buf[0]` have been applied.
    front_applied: bool,
    lex_error: Option<LexError>,
    lexer_stopped: bool,
    /// Span of the last consumed token.
    pub prev_span: Span,
    /// Line counter after the last consumed token.
    pub prev_end_line: u32,
    /// `CG(doc_comment)`
    pub doc_comment: Option<Vec<u8>>,
    /// One entry per function body being parsed: whether it contains
    /// `yield`.
    pub generator_stack: Vec<bool>,
    pub warnings: Vec<Diagnostic>,
    /// `Some` when identifier tokens are recorded.
    pub identifier_tokens: Option<Vec<usize>>,
    /// See [`Program::foreach_ref_keys`].
    pub foreach_ref_keys: Vec<usize>,
    /// Current recursion depth (bounded like bison's stack).
    pub depth: u32,
    /// Native stack address below which recursion must stop.
    pub stack_limit: Option<usize>,
    /// Set when parsing stopped because the native stack ran low (rather
    /// than at bison's depth limit).
    pub stack_exhausted: bool,
    /// [`ParseOptions::multibyte`].
    multibyte: bool,
    encoding_precision: usize,
    /// Start offset of a keyword that began a call argument (see
    /// `parse_argument`).
    pub colon_hint: Option<usize>,
}

/// Bison's `YYMAXDEPTH`, approximated as a recursion limit.
const MAX_DEPTH: u32 = 9990;

impl<'a> Parser<'a> {
    #[allow(clippy::trivially_copy_pass_by_ref)] // `ParseOptions` may grow
    pub fn new(src: &'a [u8], opts: &ParseOptions, encoding: &ScriptEncodingOptions<'_>) -> Self {
        let lexer_opts = LexerOptions {
            mode: LexMode::Parser,
            short_tags: opts.short_open_tag,
            start_in_scripting: opts.start_in_scripting,
            skip_shebang: opts.skip_shebang,
        };
        let mut lexer = if opts.multibyte {
            Lexer::with_script_encoding(src, lexer_opts, encoding)
        } else {
            Lexer::with_options(src, lexer_opts)
        };
        // Whitespace and ordinary comments are discarded below anyway;
        // the lexer skips them without building tokens.
        lexer.set_skip_trivia(true);
        Self {
            lexer,
            buf: VecDeque::new(),
            front_applied: false,
            lex_error: None,
            lexer_stopped: false,
            prev_span: Span::new(0, 0, 1),
            prev_end_line: 1,
            doc_comment: None,
            generator_stack: Vec::new(),
            warnings: Vec::new(),
            identifier_tokens: opts.record_identifier_tokens.then(Vec::new),
            foreach_ref_keys: Vec::new(),
            depth: 0,
            stack_limit: None,
            stack_exhausted: false,
            colon_hint: None,
            multibyte: opts.multibyte,
            encoding_precision: encoding.precision,
        }
    }

    // ---- token stream ----------------------------------------------------

    fn lex_one(&mut self) -> Tok<'a> {
        let mut doc = None;
        loop {
            if self.lexer_stopped {
                let len = self.lexer.source().len();
                let line = self.lexer.line();
                let mut tok = Tok::plain(TokenKind::End, Span::new(len, len, line), line, b"");
                if doc.is_some() {
                    tok.extra = Some(Box::new(TokExtra { text: None, doc }));
                }
                return tok;
            }
            let t = self.lexer.next_token_or_end();
            self.collect_lexer_warnings();
            match t.kind {
                // Not returned with `set_skip_trivia`; kept for safety.
                TokenKind::Whitespace | TokenKind::Comment | TokenKind::OpenTag => continue,
                TokenKind::DocComment => {
                    doc = Some(self.lexer.token_text(t).into_owned());
                    continue;
                }
                TokenKind::Error if self.lex_error.is_none() => {
                    self.lex_error = self.lexer.error().cloned();
                }
                _ => {}
            }
            let mut tok = Tok::plain(t.kind, t.span, t.end_line, b"");
            tok.value = self.lexer.take_value();
            match self.lexer.token_text(t) {
                Cow::Borrowed(text) if doc.is_none() => tok.text = text,
                Cow::Borrowed(text) => {
                    tok.text = text;
                    tok.extra = Some(Box::new(TokExtra { text: None, doc }));
                }
                Cow::Owned(text) => {
                    tok.extra = Some(Box::new(TokExtra {
                        text: Some(text),
                        doc,
                    }));
                }
            }
            return tok;
        }
    }

    fn collect_lexer_warnings(&mut self) {
        if self.lexer.warnings().is_empty() {
            return;
        }
        for w in self.lexer.take_warnings() {
            self.warnings.push(Diagnostic {
                level: match w.kind {
                    WarningKind::CompileWarning => DiagnosticLevel::CompileWarning,
                    WarningKind::Deprecated => DiagnosticLevel::Deprecated,
                },
                message: w.message,
                line: w.line,
            });
        }
    }

    #[inline(always)]
    fn fill(&mut self, n: usize) {
        if self.buf.len() <= n {
            self.fill_slow(n);
        }
    }

    #[inline(never)]
    fn fill_slow(&mut self, n: usize) {
        while self.buf.len() <= n {
            let t = self.lex_one();
            self.buf.push_back(t);
        }
    }

    /// Makes sure the current token is scanned and its doc-comment effects
    /// (`CG(doc_comment)` updates) are applied.
    ///
    /// Called for every `peek`/`eat`, so the common case (front token
    /// already scanned and applied) is an inlined flag test; the rest lives
    /// in [`Self::front_slow`].
    #[inline(always)]
    fn front(&mut self) -> &Tok<'a> {
        if !self.front_applied || self.buf.is_empty() {
            self.front_slow();
        }
        &self.buf[0]
    }

    #[inline(never)]
    fn front_slow(&mut self) {
        self.fill(0);
        if !self.front_applied {
            self.front_applied = true;
            let tok = &mut self.buf[0];
            if let Some(d) = tok.take_doc() {
                self.doc_comment = Some(d);
            }
            if tok.kind == TokenKind::RBrace {
                self.doc_comment = None;
            }
        }
    }

    /// The kind of the current token.
    #[inline]
    pub fn peek(&mut self) -> TokenKind {
        self.front().kind
    }

    /// The current token.
    #[inline]
    pub fn cur(&mut self) -> &Tok<'a> {
        self.front()
    }

    /// The span of the current token.
    #[inline]
    pub fn cur_span(&mut self) -> Span {
        self.front().span
    }

    /// The kind of the `n`th token after the current one (0 = current).
    pub fn peek_nth(&mut self, n: usize) -> TokenKind {
        self.front();
        self.fill(n);
        self.buf[n].kind
    }

    /// Consumes the current token.
    pub fn bump(&mut self) -> Tok<'a> {
        self.front();
        let tok = self
            .buf
            .pop_front()
            .unwrap_or_else(|| Tok::plain(TokenKind::End, Span::default(), 0, b""));
        self.front_applied = false;
        self.prev_span = tok.span;
        self.prev_end_line = tok.end_line;
        tok
    }

    /// Consumes the current token without returning it.
    ///
    /// Cheaper than discarding [`Self::bump`]'s result: the token is
    /// dropped in the buffer instead of being moved out to the caller.
    #[inline]
    pub fn advance(&mut self) {
        let tok = self.front();
        let (span, end_line) = (tok.span, tok.end_line);
        self.prev_span = span;
        self.prev_end_line = end_line;
        self.front_applied = false;
        self.buf.pop_front();
    }

    /// Consumes the current token and returns its span.
    #[inline]
    pub fn bump_span(&mut self) -> Span {
        self.advance();
        self.prev_span
    }

    /// Consumes the current token if it is of kind `k`, returning whether
    /// it did (`eat(k).is_some()` without moving the token out).
    #[inline]
    pub fn eat_kind(&mut self, k: TokenKind) -> bool {
        if self.peek() == k {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Like [`Self::expect1`] but discards the consumed token.
    #[inline]
    pub fn expect1_skip(&mut self, k: TokenKind) -> PResult<()> {
        if self.peek() == k {
            self.advance();
            Ok(())
        } else {
            Err(self.unexpected(&[k]))
        }
    }

    /// Consumes a token of kind `k`, or fails with a syntax error listing
    /// `expected` (which should include `k`).
    pub fn expect(&mut self, k: TokenKind, expected: &[TokenKind]) -> PResult<Tok<'a>> {
        if self.peek() == k {
            Ok(self.bump())
        } else {
            Err(self.unexpected(expected))
        }
    }

    /// Consumes a token of kind `k`, failing with `expecting "k"`.
    pub fn expect1(&mut self, k: TokenKind) -> PResult<Tok<'a>> {
        self.expect(k, &[k])
    }

    /// Stops the lexer (after `__halt_compiler();`).
    pub fn stop_lexing(&mut self) {
        self.lexer.stop();
        self.lexer_stopped = true;
        self.buf.clear();
        self.front_applied = false;
    }

    // ---- errors ----------------------------------------------------------

    /// A syntax error at the current token, with bison's expected-token
    /// list (only printed when it has 1 to 4 entries).
    pub fn unexpected(&mut self, expected: &[TokenKind]) -> ParseError {
        self.front();
        let tok = self.buf[0].clone();
        if tok.kind == TokenKind::Error {
            if let Some(e) = &self.lex_error {
                return ParseError::parse(e.message.clone(), e.line);
            }
        }
        let text = &self.lexer.source()[tok.span.start..tok.span.end];
        let expected = if self.colon_hint.is_some_and(|s| s == self.prev_span.start) {
            &[TokenKind::Colon][..]
        } else {
            expected
        };
        ParseError::parse(syntax_error_message(tok.kind, text, expected), tok.end_line)
    }

    // ---- helpers ---------------------------------------------------------

    /// Span from `start` to the end of the last consumed token.
    pub fn span_from(&self, start: Span) -> Span {
        Span::new(start.start, self.prev_span.end.max(start.start), start.line)
    }

    /// Takes the pending doc comment (`backup_doc_comment`).
    pub fn take_doc_comment(&mut self) -> Option<Vec<u8>> {
        self.doc_comment.take()
    }

    /// The identifier text of a token.
    pub fn ident_of(&self, tok: &Tok<'a>) -> Ident {
        Ident::new(tok.text().to_vec(), tok.span)
    }

    /// The identifier of a token used as `identifier` (`zend_lex_tstring`):
    /// `<?=` scans as `echo` but cannot be used as a name.
    pub fn ident_checked(&mut self, tok: &Tok<'a>) -> PResult<Ident> {
        if tok.text() == b"<?=" {
            let line = self.cur().end_line;
            return Err(ParseError::parse(
                "Cannot use \"<?=\" as an identifier",
                line,
            ));
        }
        if tok.kind != TokenKind::String {
            self.record_identifier(tok.span.start);
        }
        Ok(self.ident_of(tok))
    }

    /// Records a keyword token accepted as an identifier.
    pub fn record_identifier(&mut self, offset: usize) {
        if let Some(v) = &mut self.identifier_tokens {
            v.push(offset);
        }
    }

    /// Builds a [`Name`] from a name token.
    pub fn name_of(&self, tok: &Tok<'a>) -> Name {
        let text = tok.text();
        let (kind, name) = match tok.kind {
            TokenKind::NameFullyQualified => (NameKind::FullyQualified, &text[1..]),
            TokenKind::NameRelative => (NameKind::Relative, &text[b"namespace\\".len()..]),
            TokenKind::NameQualified => (NameKind::Qualified, text),
            _ => (NameKind::Unqualified, text),
        };
        Name {
            kind,
            name: name.to_vec(),
            span: tok.span,
        }
    }

    /// Enters one level of nesting, failing like bison when its stack is
    /// exhausted. The caller must decrement `depth` afterwards.
    pub fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        let low_stack = crate::stack::exhausted(self.stack_limit);
        if self.depth > MAX_DEPTH || low_stack {
            self.stack_exhausted |= low_stack && self.depth <= MAX_DEPTH;
            let line = self.cur().end_line;
            self.depth -= 1;
            return Err(ParseError::parse("memory exhausted", line));
        }
        Ok(())
    }

    /// Marks the innermost function as a generator.
    pub fn mark_generator(&mut self) {
        if let Some(top) = self.generator_stack.last_mut() {
            *top = true;
        }
    }
}

/// Whether a token is a name token usable as `name` in the grammar.
pub(crate) fn is_name_token(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::String
            | TokenKind::NameQualified
            | TokenKind::NameFullyQualified
            | TokenKind::NameRelative
    )
}

/// Whether a token can be used as `identifier` (`T_STRING` or semi-reserved).
pub(crate) fn is_identifier_token(k: TokenKind) -> bool {
    k == TokenKind::String || k.is_semi_reserved()
}

/// Parses a complete file, stopping recursion when the native stack falls
/// below `stack_limit`. The error's flag tells whether that happened.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn parse_program_limited(
    src: &[u8],
    opts: &ParseOptions,
    encoding: &ScriptEncodingOptions<'_>,
    stack_limit: Option<usize>,
) -> Result<Program, (ParseError, bool)> {
    let mut p = Parser::new(src, opts, encoding);
    p.stack_limit = stack_limit;
    let r = parse_program_inner(&mut p);
    r.map_err(|e| (e, p.stack_exhausted))
}

fn parse_program_inner(p: &mut Parser<'_>) -> Result<Program, ParseError> {
    match p.parse_top_level() {
        Ok((statements, halt_compiler_offset)) => {
            let mut identifier_tokens = p.identifier_tokens.take().unwrap_or_default();
            identifier_tokens.sort_unstable();
            identifier_tokens.dedup();
            Ok(Program {
                multibyte: p.multibyte,
                statements,
                halt_compiler_offset,
                warnings: std::mem::take(&mut p.warnings),
                identifier_tokens,
                foreach_ref_keys: std::mem::take(&mut p.foreach_ref_keys),
            })
        }
        Err(mut e) => {
            e.warnings = std::mem::take(&mut p.warnings);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tok_tests {
    use super::*;

    #[test]
    fn tok_stays_small() {
        assert!(std::mem::size_of::<Tok<'static>>() <= 80);
    }

    #[test]
    fn tok_text_prefers_owned_text() {
        let span = Span::new(0, 3, 1);
        let mut tok = Tok::plain(TokenKind::String, span, 1, b"abc");
        assert_eq!(tok.text(), b"abc");
        assert_eq!(tok.take_doc(), None);
        tok.extra = Some(Box::new(TokExtra {
            text: None,
            doc: Some(b"/** d */".to_vec()),
        }));
        assert_eq!(tok.text(), b"abc");
        assert_eq!(tok.take_doc().as_deref(), Some(&b"/** d */"[..]));
        assert_eq!(tok.take_doc(), None);
        tok.extra = Some(Box::new(TokExtra {
            text: Some(b"xyz".to_vec()),
            doc: None,
        }));
        assert_eq!(tok.text(), b"xyz");
    }

    #[test]
    fn doc_comments_reach_declarations() {
        let src = b"<?php\n/** A */\nclass A {\n/** m */\npublic function m() {}\n}\n/** f */ function f() {}";
        let mut p = Parser::new(
            src,
            &ParseOptions::default(),
            &ScriptEncodingOptions::default(),
        );
        assert_eq!(p.peek(), TokenKind::Class);
        assert_eq!(p.take_doc_comment().as_deref(), Some(&b"/** A */"[..]));
    }

    #[test]
    fn discarding_consumers_track_the_previous_token() {
        let src = b"<?php // c\n$a /* x */ ;\r\n# y\n( ) ?>";
        let mut p = Parser::new(
            src,
            &ParseOptions::default(),
            &ScriptEncodingOptions::default(),
        );
        assert_eq!(p.peek(), TokenKind::Variable);
        assert_eq!(p.bump_span(), Span::new(11, 13, 2));
        assert!(!p.eat_kind(TokenKind::Comma));
        assert!(p.eat_kind(TokenKind::Semicolon));
        assert_eq!(p.prev_span, Span::new(22, 23, 2));
        assert!(p.expect1_skip(TokenKind::LParen).is_ok());
        assert_eq!(p.prev_span.line, 4);
        p.advance();
        assert_eq!(p.prev_span, Span::new(31, 32, 4));
        let err = p.expect1_skip(TokenKind::LParen).map_err(|e| e.message);
        assert_eq!(
            err,
            Err("syntax error, unexpected token \";\", expecting \"(\"".into())
        );
        assert_eq!(p.bump().kind, TokenKind::Semicolon);
        assert_eq!(p.peek(), TokenKind::End);
    }
}
