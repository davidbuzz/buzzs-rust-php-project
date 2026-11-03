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
use php_lexer::{LexError, LexMode, Lexer, LexerOptions, TokenKind, TokenValue, WarningKind};
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
}

/// Result type of parser functions.
pub(crate) type PResult<T> = Result<T, ParseError>;

/// A token with its semantic value.
#[derive(Clone, Debug)]
pub(crate) struct Tok {
    pub kind: TokenKind,
    pub span: Span,
    pub end_line: u32,
    pub value: Option<TokenValue>,
    /// Doc comment scanned in the trivia before this token.
    doc: Option<Vec<u8>>,
}

/// The parser state.
pub(crate) struct Parser<'a> {
    pub src: &'a [u8],
    lexer: Lexer<'a>,
    buf: VecDeque<Tok>,
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
    /// Current recursion depth (bounded like bison's stack).
    pub depth: u32,
}

/// Bison's `YYMAXDEPTH`, approximated as a recursion limit.
const MAX_DEPTH: u32 = 9990;

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], opts: &ParseOptions) -> Self {
        let lexer = Lexer::with_options(
            src,
            LexerOptions {
                mode: LexMode::Parser,
                short_tags: opts.short_open_tag,
                start_in_scripting: opts.start_in_scripting,
                skip_shebang: opts.skip_shebang,
            },
        );
        Self {
            src,
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
            depth: 0,
        }
    }

    // ---- token stream ----------------------------------------------------

    fn lex_one(&mut self) -> Tok {
        let mut doc = None;
        loop {
            if self.lexer_stopped {
                let len = self.src.len();
                let line = self.lexer.line();
                return Tok {
                    kind: TokenKind::End,
                    span: Span::new(len, len, line),
                    end_line: line,
                    value: None,
                    doc,
                };
            }
            let t = self.lexer.next_token_or_end();
            self.collect_lexer_warnings();
            match t.kind {
                TokenKind::Whitespace | TokenKind::Comment | TokenKind::OpenTag => continue,
                TokenKind::DocComment => {
                    doc = Some(t.text(self.src).to_vec());
                    continue;
                }
                TokenKind::Error => {
                    if self.lex_error.is_none() {
                        self.lex_error = self.lexer.error().cloned();
                    }
                }
                _ => {}
            }
            let value = self.lexer.take_value();
            return Tok {
                kind: t.kind,
                span: t.span,
                end_line: t.end_line,
                value,
                doc,
            };
        }
    }

    fn collect_lexer_warnings(&mut self) {
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

    fn fill(&mut self, n: usize) {
        while self.buf.len() <= n {
            let t = self.lex_one();
            self.buf.push_back(t);
        }
    }

    /// Makes sure the current token is scanned and its doc-comment effects
    /// (`CG(doc_comment)` updates) are applied.
    fn front(&mut self) -> &Tok {
        self.fill(0);
        if !self.front_applied {
            self.front_applied = true;
            let tok = &mut self.buf[0];
            if let Some(d) = tok.doc.take() {
                self.doc_comment = Some(d);
            }
            if tok.kind == TokenKind::RBrace {
                self.doc_comment = None;
            }
        }
        &self.buf[0]
    }

    /// The kind of the current token.
    pub fn peek(&mut self) -> TokenKind {
        self.front().kind
    }

    /// The current token.
    pub fn cur(&mut self) -> &Tok {
        self.front()
    }

    /// The span of the current token.
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
    pub fn bump(&mut self) -> Tok {
        self.front();
        let tok = self.buf.pop_front().unwrap_or_else(|| Tok {
            kind: TokenKind::End,
            span: Span::default(),
            end_line: 0,
            value: None,
            doc: None,
        });
        self.front_applied = false;
        self.prev_span = tok.span;
        self.prev_end_line = tok.end_line;
        tok
    }

    /// Consumes the current token if it is of kind `k`.
    pub fn eat(&mut self, k: TokenKind) -> Option<Tok> {
        if self.peek() == k {
            Some(self.bump())
        } else {
            None
        }
    }


    /// Consumes a token of kind `k`, or fails with a syntax error listing
    /// `expected` (which should include `k`).
    pub fn expect(&mut self, k: TokenKind, expected: &[TokenKind]) -> PResult<Tok> {
        if self.peek() == k {
            Ok(self.bump())
        } else {
            Err(self.unexpected(expected))
        }
    }

    /// Consumes a token of kind `k`, failing with `expecting "k"`.
    pub fn expect1(&mut self, k: TokenKind) -> PResult<Tok> {
        self.expect(k, &[k])
    }

    /// The source text of a token.
    pub fn text(&self, tok: &Tok) -> &'a [u8] {
        &self.src[tok.span.start..tok.span.end]
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
        let text = self.text(&tok);
        ParseError::parse(syntax_error_message(tok.kind, text, expected), tok.end_line)
    }

    /// A compile error at the current line (`CG(zend_lineno)` after the
    /// last scanned token).
    pub fn compile_error(&mut self, msg: impl Into<String>) -> ParseError {
        ParseError::compile(msg, self.prev_end_line)
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
    pub fn ident_of(&self, tok: &Tok) -> Ident {
        Ident::new(self.text(tok).to_vec(), tok.span)
    }

    /// Builds a [`Name`] from a name token.
    pub fn name_of(&self, tok: &Tok) -> Name {
        let text = self.text(tok);
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
        if self.depth > MAX_DEPTH {
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

/// Whether a token can be used as `identifier` (T_STRING or semi-reserved).
pub(crate) fn is_identifier_token(k: TokenKind) -> bool {
    k == TokenKind::String || k.is_semi_reserved()
}

/// Parses a complete file.
pub fn parse_program(src: &[u8], opts: &ParseOptions) -> Result<Program, ParseError> {
    let mut p = Parser::new(src, opts);
    match p.parse_top_level() {
        Ok((statements, halt_compiler_offset)) => Ok(Program {
            statements,
            halt_compiler_offset,
            warnings: p.warnings,
        }),
        Err(mut e) => {
            e.warnings = std::mem::take(&mut p.warnings);
            Err(e)
        }
    }
}
