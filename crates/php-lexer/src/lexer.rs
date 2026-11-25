// Derived from php-src Zend/zend_language_scanner.l. Similarity metric to original: 6%
//   Copyright © Zend Technologies Ltd., a subsidiary company of Perforce Software, Inc., and Contributors.
//   Authors: Marcus Boerger <helly@php.net>
//            Nuno Lopes <nlopess@php.net>
//            Scott MacVicar <scottmac@php.net>
//            Flex version authors:
//            Andi Gutmans <andi@php.net>
//            Zeev Suraski <zeev@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The scanner state machine, a hand-written port of
//! `Zend/zend_language_scanner.l`.

use crate::encoding::{Input, ScriptEncoding, ScriptEncodingOptions};
use crate::literal::{
    count_newlines, is_label_char, is_label_start, parse_float_literal, parse_lnum,
    parse_prefixed_int, strip_indentation, unescape_double_quoted, unescape_single_quoted,
    LexWarning, WarningKind,
};
use crate::token::{Span, Token, TokenKind, TokenValue};
use std::borrow::Cow;
use std::fmt::Write as _;

/// Which flavour of scanning to perform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LexMode {
    /// `token_get_all()` semantics: every token is returned, errors never
    /// abort scanning, `?>` is `T_CLOSE_TAG`.
    #[default]
    Tokenizer,
    /// Compiler semantics: `?>` becomes `;`, `<?=` becomes `T_ECHO`, token
    /// values are computed and scanner errors produce a `T_ERROR` token (see
    /// [`Lexer::error`]).  Whitespace, comments and open tags are still
    /// returned so that callers can see doc comments; they are trivia.
    Parser,
}

/// Scanner configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexerOptions {
    /// Tokenizer or parser semantics.
    pub mode: LexMode,
    /// Whether `<?` opens PHP code (`short_open_tag` ini setting).
    pub short_tags: bool,
    /// Start in scripting state (as `eval()` does) instead of inline HTML.
    pub start_in_scripting: bool,
    /// Skip a leading `#!` line (CLI primary script behaviour).
    pub skip_shebang: bool,
}

impl Default for LexerOptions {
    fn default() -> Self {
        Self {
            mode: LexMode::Tokenizer,
            short_tags: false,
            start_in_scripting: false,
            skip_shebang: false,
        }
    }
}

/// A fatal scanner error (PHP throws a `ParseError` with this message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    /// Message text, e.g. `Unterminated comment starting line 2`.
    pub message: String,
    /// Line PHP reports the error on.
    pub line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Initial,
    InScripting,
    LookingForProperty,
    DoubleQuotes,
    Backquote,
    Heredoc,
    Nowdoc,
    EndHeredoc,
    LookingForVarname,
    VarOffset,
    Shebang,
}

#[derive(Clone, Copy, Debug)]
struct HeredocLabel {
    start: usize,
    len: usize,
    indentation: usize,
    indentation_uses_spaces: bool,
}

const USING_SPACES: u8 = 1;
const USING_TABS: u8 = 2;

/// The PHP scanner.
///
/// Iterating yields every token up to (but excluding) the end of input. In
/// [`LexMode::Parser`] a scanner error yields a single [`TokenKind::Error`]
/// token, after which iteration stops.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Lexer<'a> {
    src: Input<'a>,
    encoding: Option<ScriptEncoding<'a>>,
    pos: usize,
    tok_start: usize,
    line: u32,
    increment_line: bool,
    state: State,
    state_stack: Vec<State>,
    heredoc_stack: Vec<HeredocLabel>,
    nesting: Vec<(u8, u32)>,
    opts: LexerOptions,
    value: Option<TokenValue>,
    error: Option<LexError>,
    warnings: Vec<LexWarning>,
    heredoc_scan_ahead: bool,
    heredoc_indentation: usize,
    heredoc_indentation_uses_spaces: bool,
    done: bool,
    /// See [`Lexer::set_skip_trivia`].
    skip_trivia: bool,
}

/// [`is_label_char`] (`[a-zA-Z0-9_\x80-\xff]`) as a lookup table.
static LABEL_CHAR: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = matches!(i, 0x30..=0x39 | 0x41..=0x5a | 0x5f | 0x61..=0x7a | 0x80..);
        i += 1;
    }
    t
};

/// Case-insensitive keyword table for labels in scripting state.
///
/// Dispatches on the label length first, then compares the lowercased bytes
/// as a fixed-size array (which compiles to one or two integer compares per
/// candidate) instead of testing every keyword in turn. Labels that cannot
/// be keywords (too short or long, or not starting with a keyword's first
/// letter) are rejected before lowercasing.
fn keyword(label: &[u8]) -> Option<TokenKind> {
    use TokenKind as T;
    let len = label.len();
    if !(2..=KEYWORD_MAX_LEN).contains(&len) || !KEYWORD_FIRST[usize::from(label[0])] {
        return None;
    }
    let mut buf = [0u8; KEYWORD_MAX_LEN];
    buf[..len].copy_from_slice(label);
    buf.make_ascii_lowercase();
    // Matches the first `$n` bytes of `buf` against `$n`-byte keywords.
    macro_rules! arms {
        ($n:literal: $($kw:literal => $kind:ident),+ $(,)?) => {{
            let mut a = [0u8; $n];
            a.copy_from_slice(&buf[..$n]);
            match &a {
                $($kw => Some(T::$kind),)+
                _ => None,
            }
        }};
    }
    match len {
        2 => arms!(2: b"fn" => Fn, b"if" => If, b"do" => Do, b"as" => As, b"or" => LogicalOr),
        3 => {
            arms!(3: b"die" => Exit, b"try" => Try, b"for" => For, b"new" => New, b"var" => Var, b"use" => Use, b"and" => LogicalAnd, b"xor" => LogicalXor)
        }
        4 => {
            arms!(4: b"exit" => Exit, b"else" => Else, b"case" => Case, b"goto" => Goto, b"echo" => Echo, b"eval" => Eval, b"list" => List)
        }
        5 => {
            arms!(5: b"const" => Const, b"yield" => Yield, b"catch" => Catch, b"throw" => Throw, b"endif" => EndIf, b"while" => While, b"match" => Match, b"break" => Break, b"print" => Print, b"class" => Class, b"trait" => Trait, b"clone" => Clone, b"isset" => Isset, b"empty" => Empty, b"final" => Final, b"unset" => Unset, b"array" => Array)
        }
        6 => {
            arms!(6: b"return" => Return, b"elseif" => ElseIf, b"endfor" => EndFor, b"switch" => Switch, b"global" => Global, b"static" => Static, b"public" => Public)
        }
        7 => {
            arms!(7: b"finally" => Finally, b"foreach" => Foreach, b"declare" => Declare, b"default" => Default, b"extends" => Extends, b"include" => Include, b"require" => Require, b"private" => Private, b"__dir__" => Dir)
        }
        8 => {
            arms!(8: b"function" => Function, b"endwhile" => EndWhile, b"continue" => Continue, b"abstract" => Abstract, b"readonly" => Readonly, b"callable" => Callable, b"__line__" => Line, b"__file__" => File)
        }
        9 => {
            arms!(9: b"endswitch" => EndSwitch, b"interface" => Interface, b"namespace" => Namespace, b"insteadof" => Insteadof, b"protected" => Protected, b"__class__" => ClassC, b"__trait__" => TraitC)
        }
        10 => {
            arms!(10: b"endforeach" => EndForeach, b"enddeclare" => EndDeclare, b"instanceof" => Instanceof, b"implements" => Implements, b"__method__" => MethodC)
        }
        12 => {
            arms!(12: b"include_once" => IncludeOnce, b"require_once" => RequireOnce, b"__function__" => FuncC, b"__property__" => PropertyC)
        }
        13 => arms!(13: b"__namespace__" => NsC),
        15 => arms!(15: b"__halt_compiler" => HaltCompiler),
        _ => None,
    }
}

/// The end (just past the `/`) of the first `*/` whose `*` is at or after
/// `from`, or `None` if the comment is unterminated.
///
/// Doc comments put a `*` on every line, so this looks for the rarer `/`,
/// eight bytes at a time, and then checks the byte before it.
fn find_comment_close(src: &[u8], from: usize) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const SLASH: u64 = 0x2f2f_2f2f_2f2f_2f2f; // `/` in every byte
    let mut i = from + 1;
    while i < src.len() {
        if let Some(Ok(chunk)) = src.get(i..i + 8).map(<[u8; 8]>::try_from) {
            // Classic "has zero byte" test on `word ^ "////////"`: the lowest
            // flagged byte is exactly the first `/` (borrows only cause
            // false flags above a real match).
            let x = u64::from_le_bytes(chunk) ^ SLASH;
            let found = x.wrapping_sub(LO) & !x & HI;
            if found == 0 {
                i += 8;
                continue;
            }
            i += found.trailing_zeros() as usize / 8;
        } else if src[i] != b'/' {
            i += 1;
            continue;
        }
        if src[i - 1] == b'*' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Length of the longest keyword (`__halt_compiler`).
const KEYWORD_MAX_LEN: usize = 15;

/// Bytes (either case) that start some keyword.
static KEYWORD_FIRST: [bool; 256] = {
    let mut t = [false; 256];
    let firsts = b"_abcdefgilmnoprstuvwxy";
    let mut i = 0;
    while i < firsts.len() {
        t[firsts[i] as usize] = true;
        t[firsts[i].to_ascii_uppercase() as usize] = true;
        i += 1;
    }
    t
};

impl<'a> Lexer<'a> {
    /// Creates a tokenizer-mode lexer starting in inline HTML.
    pub fn new(src: &'a [u8]) -> Self {
        Self::with_options(src, LexerOptions::default())
    }

    /// Creates a lexer with explicit options.
    pub fn with_options(src: &'a [u8], opts: LexerOptions) -> Self {
        let state = if opts.start_in_scripting {
            State::InScripting
        } else if opts.skip_shebang {
            State::Shebang
        } else {
            State::Initial
        };
        Self {
            src: Input::Borrowed(src),
            encoding: None,
            pos: 0,
            tok_start: 0,
            line: 1,
            increment_line: false,
            state,
            state_stack: Vec::new(),
            heredoc_stack: Vec::new(),
            nesting: Vec::new(),
            opts,
            value: None,
            error: None,
            warnings: Vec::new(),
            heredoc_scan_ahead: false,
            heredoc_indentation: 0,
            heredoc_indentation_uses_spaces: false,
            done: false,
            skip_trivia: false,
        }
    }

    /// Creates a lexer with Zend multibyte input/output filtering enabled.
    ///
    /// Token spans refer to [`Self::source`], not the original input bytes.
    /// The parser must call [`Self::set_script_encoding`] immediately after
    /// an encoding declaration's closing parenthesis, before scanning ahead.
    pub fn with_script_encoding(
        src: &'a [u8],
        opts: LexerOptions,
        encoding: &ScriptEncodingOptions<'_>,
    ) -> Self {
        let mut lexer = Self::with_options(src, opts);
        let (state, input) = ScriptEncoding::new(src, encoding);
        lexer.src = input;
        lexer.encoding = Some(state);
        lexer
    }

    /// The possibly filtered source being scanned.
    pub fn source(&self) -> &[u8] {
        &self.src
    }

    /// Changes the script encoding and, if needed, rescans the original input.
    ///
    /// The cursor and line counter stay at the declaration's closing `)`,
    /// matching Zend. Returns false when multibyte filtering is not enabled.
    pub fn set_script_encoding(&mut self, encoding: &'static php_mbstring::Encoding) -> bool {
        let Some(state) = self.encoding.as_mut() else {
            return false;
        };
        if let Some(input) = state.set_encoding(encoding) {
            self.src = input;
        }
        true
    }

    /// Token bytes after the scanner's output filter, for AST semantic names
    /// and inline HTML. Lexical prefixes (`$`, `\`, `namespace\`) are retained
    /// unconverted so a parser can strip them before storing the semantic name.
    /// Use [`Token::text`] with [`Self::source`] for raw text.
    #[inline]
    pub fn token_text(&self, token: Token) -> Cow<'a, [u8]> {
        // Common case: no script encoding filter, unfiltered input.
        if let (None, Input::Borrowed(src)) = (&self.encoding, &self.src) {
            return Cow::Borrowed(token.text(*src));
        }
        self.token_text_filtered(token)
    }

    fn token_text_filtered(&self, token: Token) -> Cow<'a, [u8]> {
        let text = token.text(self.source());
        let prefix = match token.kind {
            TokenKind::Variable | TokenKind::NameFullyQualified => 1,
            TokenKind::NameRelative => b"namespace\\".len(),
            _ => 0,
        };
        if let Some(converted) = self
            .encoding
            .as_ref()
            .and_then(|state| state.output(&text[prefix..]))
        {
            let mut bytes = text[..prefix].to_vec();
            bytes.extend_from_slice(&converted);
            return Cow::Owned(bytes);
        }
        match &self.src {
            Input::Borrowed(src) => Cow::Borrowed(token.text(*src)),
            Input::Filtered(_) => Cow::Owned(text.to_vec()),
        }
    }

    /// Current byte offset (the end of the last token).
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// Original-script offset of the cursor (after any detected BOM when an
    /// input filter is active, from the start of the input otherwise).
    ///
    /// Used for `__COMPILER_HALT_OFFSET__`; returns `usize::MAX` if no original
    /// prefix maps exactly to the filtered cursor.
    pub fn scanned_file_offset(&self) -> usize {
        self.encoding
            .as_ref()
            .map_or(self.pos, |state| state.original_offset(self.pos))
    }

    /// The current line counter.
    pub fn line(&self) -> u32 {
        self.line
    }

    /// Takes the semantic value of the last token (parser mode only).
    pub fn take_value(&mut self) -> Option<TokenValue> {
        self.value.take()
    }

    /// The scanner error that produced the last `T_ERROR` token (parser mode).
    /// In tokenizer mode no `T_ERROR` is produced; this holds the first error
    /// PHP would have raised (and `token_get_all()` ignores).
    pub fn error(&self) -> Option<&LexError> {
        self.error.as_ref()
    }

    /// Warnings and deprecations raised while scanning. Octal escape overflow
    /// warnings are raised in both modes (as `token_get_all()` does);
    /// non-canonical cast deprecations only in parser mode.
    pub fn warnings(&self) -> &[LexWarning] {
        &self.warnings
    }

    /// Takes the accumulated warnings.
    pub fn take_warnings(&mut self) -> Vec<LexWarning> {
        std::mem::take(&mut self.warnings)
    }

    /// Stops scanning: subsequent calls return `End` (used after
    /// `__halt_compiler();`).
    pub fn stop(&mut self) {
        self.pos = self.src.len();
        self.done = true;
    }

    /// Makes [`Self::next_token`] skip whitespace, ordinary comments and
    /// open tags ([`TokenKind::is_trivia`] except doc comments) instead of
    /// returning them, for a parser that discards them anyway.
    ///
    /// Line numbers, warnings and the following token's span are exactly
    /// what they would be had the skipped tokens been returned and
    /// ignored. Doc comments are still returned. `token_get_all()` and
    /// `PhpToken` must not use this: they need every token.
    pub fn set_skip_trivia(&mut self, skip: bool) {
        self.skip_trivia = skip;
    }

    fn parser_mode(&self) -> bool {
        self.opts.mode == LexMode::Parser
    }

    #[inline]
    fn at(&self, i: usize) -> u8 {
        self.src.get(i).copied().unwrap_or(0)
    }

    #[inline]
    fn cur(&self) -> u8 {
        self.at(self.pos)
    }

    fn push_state(&mut self, s: State) {
        self.state_stack.push(self.state);
        self.state = s;
    }

    fn pop_state(&mut self) {
        if let Some(s) = self.state_stack.pop() {
            self.state = s;
        }
    }

    fn throw(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(LexError {
                message: message.into(),
                line: self.line,
            });
        }
    }

    fn throw_at(&mut self, message: impl Into<String>, line: u32) {
        if self.error.is_none() {
            self.error = Some(LexError {
                message: message.into(),
                line,
            });
        }
    }

    fn warn(&mut self, kind: WarningKind, message: String, line: u32) {
        if !self.heredoc_scan_ahead {
            self.warnings.push(LexWarning {
                kind,
                message,
                line,
            });
        }
    }

    fn enter_nesting(&mut self, c: u8) {
        self.nesting.push((c, self.line));
    }

    fn report_bad_nesting(&mut self, opening: u8, opening_line: u32, closing: Option<u8>) {
        let mut msg = format!("Unclosed '{}'", opening as char);
        if opening_line != self.line {
            let _ = write!(msg, " on line {opening_line}");
        }
        if let Some(c) = closing {
            let _ = write!(msg, " does not match '{}'", c as char);
        }
        self.throw(msg);
    }

    /// Returns `true` on success.
    fn exit_nesting(&mut self, closing: u8) -> bool {
        let Some(&(opening, line)) = self.nesting.last() else {
            self.throw(format!("Unmatched '{}'", closing as char));
            return false;
        };
        if (opening == b'{' && closing != b'}')
            || (opening == b'[' && closing != b']')
            || (opening == b'(' && closing != b')')
        {
            self.report_bad_nesting(opening, line, Some(closing));
            return false;
        }
        self.nesting.pop();
        true
    }

    fn exit_nesting_token(&mut self, c: u8, kind: TokenKind) -> TokenKind {
        if !self.exit_nesting(c) && self.parser_mode() {
            TokenKind::Error
        } else {
            kind
        }
    }

    fn end_token(&mut self) -> TokenKind {
        if let Some(&(opening, line)) = self.nesting.last() {
            self.report_bad_nesting(opening, line, None);
            if self.parser_mode() {
                return TokenKind::Error;
            }
        }
        TokenKind::End
    }

    /// Scans the next token. Returns `None` once the end of input has been
    /// reached (or after a `T_ERROR` token in parser mode).
    pub fn next_token(&mut self) -> Option<Token> {
        if self.done {
            return None;
        }
        let (kind, start_line) = loop {
            if self.increment_line {
                self.line += 1;
                self.increment_line = false;
            }
            self.value = None;
            if self.parser_mode() {
                self.error = None;
            }
            if self.skip_trivia && self.state == State::InScripting {
                self.skip_plain_trivia();
            }
            let start_line = self.line;
            let kind = self.scan();
            if !(self.skip_trivia
                && matches!(
                    kind,
                    TokenKind::Whitespace | TokenKind::Comment | TokenKind::OpenTag
                ))
            {
                break (kind, start_line);
            }
        };
        if let Some(TokenValue::Str(bytes)) = &mut self.value {
            if let Some(converted) = self.encoding.as_ref().and_then(|state| state.output(bytes)) {
                *bytes = converted;
            }
        }
        let start = self.tok_start;
        let end = self.pos;
        if kind == TokenKind::End {
            self.done = true;
            return None;
        }
        if kind == TokenKind::Error {
            self.done = true;
        }
        Some(Token {
            kind,
            span: Span::new(start, end, start_line),
            end_line: self.line,
        })
    }

    /// Scans the next token, returning an `End` token at end of input instead
    /// of `None`.
    pub fn next_token_or_end(&mut self) -> Token {
        let line = self.line;
        match self.next_token() {
            Some(t) => t,
            None => Token {
                kind: TokenKind::End,
                span: Span::new(self.src.len(), self.src.len(), line.max(self.line)),
                end_line: self.line,
            },
        }
    }

    /// Skips the whitespace and ordinary (non-doc, terminated) comments at
    /// the scanner position in scripting state, counting newlines, as if
    /// they had been scanned as tokens and discarded. Stops before anything
    /// else, including `#[`, `/**` and unterminated comments, which the
    /// normal rules handle (the `/**` forms that are ordinary comments then
    /// come back as `Comment` and are skipped by [`Self::next_token`]).
    fn skip_plain_trivia(&mut self) {
        let src = self.src.clone();
        let len = src.len();
        let mut p = self.pos;
        while p < len {
            match src[p] {
                b' ' | b'\t' => p += 1,
                b'\n' => {
                    self.line += 1;
                    p += 1;
                }
                b'\r' => {
                    // A lone `\r` is a line break (see `count_newlines`).
                    if self.at(p + 1) != b'\n' {
                        self.line += 1;
                    }
                    p += 1;
                }
                b'#' if self.at(p + 1) != b'[' => p = self.line_comment_end(p + 1),
                b'/' if self.at(p + 1) == b'/' => p = self.line_comment_end(p + 2),
                b'/' if self.at(p + 1) == b'*' && self.at(p + 2) != b'*' => {
                    // The first `*/` after the opening `/*`.
                    let Some(end) = find_comment_close(&src, p + 2) else {
                        break;
                    };
                    self.handle_newlines(p, end);
                    p = end;
                }
                _ => break,
            }
        }
        self.pos = p;
    }

    /// End of a `#`/`//` comment whose body starts at `q`: the next line
    /// break or `?>` (neither included).
    fn line_comment_end(&self, mut q: usize) -> usize {
        let src: &[u8] = &self.src;
        while q < src.len() {
            match src[q] {
                b'\r' | b'\n' => break,
                b'?' if self.at(q + 1) == b'>' => break,
                _ => q += 1,
            }
        }
        q
    }

    fn handle_newlines(&mut self, start: usize, end: usize) {
        self.line += count_newlines(&self.src, start, end);
    }

    /// Runs the scanner rules; a rule returning `None` restarts scanning
    /// (re2c's `goto restart`), which resets the token start but not the
    /// token's line, exactly like `lex_scan`.
    fn scan(&mut self) -> TokenKind {
        loop {
            self.tok_start = self.pos;
            let r = match self.state {
                State::Shebang => self.scan_shebang(),
                State::Initial => Some(self.scan_initial()),
                State::InScripting => Some(self.scan_scripting()),
                State::LookingForProperty => self.scan_looking_for_property(),
                State::LookingForVarname => self.scan_looking_for_varname(),
                State::VarOffset => Some(self.scan_var_offset()),
                State::DoubleQuotes => Some(self.scan_double_quotes()),
                State::Backquote => Some(self.scan_backquote()),
                State::Heredoc => Some(self.scan_heredoc()),
                State::Nowdoc => Some(self.scan_nowdoc()),
                State::EndHeredoc => Some(self.scan_end_heredoc()),
            };
            if let Some(k) = r {
                return k;
            }
        }
    }

    // ---- SHEBANG -----------------------------------------------------------

    fn scan_shebang(&mut self) -> Option<TokenKind> {
        // <SHEBANG>"#!" .* {NEWLINE}: `.` matches anything but `\n`, so the
        // match ends at the first `\n`, or failing that at the last `\r`.
        self.state = State::Initial;
        if self.cur() == b'#' && self.at(self.pos + 1) == b'!' {
            let rest = &self.src[self.pos..];
            let end = rest
                .iter()
                .position(|&c| c == b'\n')
                .or_else(|| rest.iter().rposition(|&c| c == b'\r'));
            if let Some(nl) = end {
                self.pos += nl + 1;
                self.line += 1;
            }
        }
        None
    }

    // ---- INITIAL -----------------------------------------------------------

    fn scan_initial(&mut self) -> TokenKind {
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        if src[p] == b'<' && self.at(p + 1) == b'?' {
            if self.at(p + 2) == b'=' {
                self.pos = p + 3;
                self.state = State::InScripting;
                return if self.parser_mode() {
                    TokenKind::Echo
                } else {
                    TokenKind::OpenTagWithEcho
                };
            }
            if src.len() >= p + 5 && src[p + 2..p + 5].eq_ignore_ascii_case(b"php") {
                let c = self.at(p + 5);
                if p + 5 < src.len() && matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                    let mut end = p + 6;
                    if c == b'\r' && self.at(p + 6) == b'\n' {
                        end += 1;
                    }
                    self.pos = end;
                    if c == b'\n' || c == b'\r' {
                        self.line += 1;
                    }
                    self.state = State::InScripting;
                    return TokenKind::OpenTag;
                }
                if p + 5 == src.len() {
                    self.pos = p + 5;
                    self.state = State::InScripting;
                    return TokenKind::OpenTag;
                }
                if self.opts.short_tags {
                    self.pos = p + 2;
                    self.state = State::InScripting;
                    return TokenKind::OpenTag;
                }
                self.pos = p + 5;
                return self.inline_html(p);
            }
            if self.opts.short_tags {
                self.pos = p + 2;
                self.state = State::InScripting;
                return TokenKind::OpenTag;
            }
            self.pos = p + 2;
            return self.inline_html(p);
        }
        self.pos = p + 1;
        self.inline_html(p)
    }

    fn inline_html(&mut self, start: usize) -> TokenKind {
        let src = self.src.clone();
        let limit = src.len();
        let mut cur = self.pos;
        loop {
            match src[cur.min(limit)..].iter().position(|&c| c == b'<') {
                Some(i) => cur = cur + i + 1,
                None => {
                    cur = limit;
                }
            }
            if cur >= limit {
                cur = limit;
                break;
            }
            if src[cur] == b'?' {
                let at = |i: usize| src.get(i).copied().unwrap_or(0);
                if self.opts.short_tags
                    || at(cur + 1) == b'='
                    || (cur + 4 <= limit
                        && src[cur + 1..cur + 4].eq_ignore_ascii_case(b"php")
                        && (cur + 4 == limit
                            || matches!(at(cur + 4), b' ' | b'\t' | b'\n' | b'\r')))
                {
                    cur -= 1;
                    break;
                }
            }
        }
        self.pos = cur;
        self.handle_newlines(start, cur);
        TokenKind::InlineHtml
    }

    // ---- shared helpers ----------------------------------------------------

    #[inline]
    fn scan_label_end(&self, from: usize) -> usize {
        let src: &[u8] = &self.src;
        match src.get(from..) {
            Some(rest) => {
                from + rest
                    .iter()
                    .position(|&c| !LABEL_CHAR[usize::from(c)])
                    .unwrap_or(rest.len())
            }
            None => from,
        }
    }

    /// Matches `{WHITESPACE_OR_COMMENTS}` (one or more) starting at `p`,
    /// returning the end position.
    fn match_ws_or_comments(&self, mut p: usize, at_least_one: bool) -> Option<usize> {
        let src = self.src.clone();
        let start = p;
        loop {
            let c = self.at(p);
            if p < src.len() && matches!(c, b' ' | b'\n' | b'\r' | b'\t') {
                p += 1;
                continue;
            }
            if c == b'/' && self.at(p + 1) == b'*' {
                // MULTI_LINE_COMMENT: "/*" ... "*/" without NUL bytes
                let mut q = p + 2;
                let mut ok = false;
                while q < src.len() {
                    if src[q] == 0 {
                        break;
                    }
                    if src[q] == b'*' && self.at(q + 1) == b'/' {
                        ok = true;
                        q += 2;
                        break;
                    }
                    q += 1;
                }
                if ok {
                    p = q;
                    continue;
                }
                break;
            }
            if c == b'/' && self.at(p + 1) == b'/' {
                let mut q = p + 2;
                while q < src.len() && !matches!(src[q], b'\n' | b'\r' | 0) {
                    q += 1;
                }
                if q < src.len() && matches!(src[q], b'\n' | b'\r') {
                    p = q + 1;
                    continue;
                }
                break;
            }
            if c == b'#' && p < src.len() {
                let n = self.at(p + 1);
                if p + 1 < src.len() && (n == b'\n' || n == b'\r') {
                    p += 2;
                    continue;
                }
                if n != b'[' && n != 0 {
                    let mut q = p + 2;
                    while q < src.len() && !matches!(src[q], b'\n' | b'\r' | 0) {
                        q += 1;
                    }
                    if q < src.len() && matches!(src[q], b'\n' | b'\r') {
                        p = q + 1;
                        continue;
                    }
                }
                break;
            }
            break;
        }
        if at_least_one && p == start {
            None
        } else {
            Some(p)
        }
    }

    fn eq_ci(&self, p: usize, lit: &[u8]) -> bool {
        p + lit.len() <= self.src.len() && self.src[p..p + lit.len()].eq_ignore_ascii_case(lit)
    }

    // ---- ST_IN_SCRIPTING ---------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn scan_scripting(&mut self) -> TokenKind {
        use TokenKind as T;
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        let c = src[p];
        let n1 = self.at(p + 1);
        let n2 = self.at(p + 2);
        macro_rules! tok {
            ($len:expr, $kind:expr) => {{
                self.pos = p + $len;
                return $kind;
            }};
        }
        match c {
            b' ' | b'\n' | b'\r' | b'\t' => {
                let mut q = p + 1;
                while q < src.len() && matches!(src[q], b' ' | b'\n' | b'\r' | b'\t') {
                    q += 1;
                }
                self.pos = q;
                self.handle_newlines(p, q);
                T::Whitespace
            }
            b'$' => {
                if is_label_start(n1) && p + 1 < src.len() {
                    let e = self.scan_label_end(p + 1);
                    tok!(e - p, T::Variable)
                }
                tok!(1, T::Dollar)
            }
            b'\\' => {
                if is_label_start(n1) && p + 1 < src.len() {
                    let e = self.scan_qualified_end(p + 1);
                    tok!(e - p, T::NameFullyQualified)
                }
                tok!(1, T::NsSeparator)
            }
            b'0'..=b'9' => self.scan_number(),
            b'.' => {
                if n1.is_ascii_digit() && p + 1 < src.len() {
                    return self.scan_number();
                }
                if n1 == b'.' && n2 == b'.' {
                    tok!(3, T::Ellipsis)
                }
                if n1 == b'=' {
                    tok!(2, T::ConcatEqual)
                }
                tok!(1, T::Dot)
            }
            b'\'' => self.scan_single_quoted(p, 0),
            b'"' => self.scan_double_quoted_start(p, 0),
            b'`' => {
                self.state = State::Backquote;
                tok!(1, T::Backtick)
            }
            b'#' => {
                if n1 == b'[' {
                    self.pos = p + 2;
                    self.enter_nesting(b'[');
                    return T::Attribute;
                }
                self.scan_line_comment(p, 1)
            }
            b'/' => {
                if n1 == b'/' {
                    return self.scan_line_comment(p, 2);
                }
                if n1 == b'*' {
                    return self.scan_block_comment(p);
                }
                if n1 == b'=' {
                    tok!(2, T::DivEqual)
                }
                tok!(1, T::Slash)
            }
            b'?' => {
                if n1 == b'-' && n2 == b'>' {
                    self.pos = p + 3;
                    self.push_state(State::LookingForProperty);
                    return T::NullsafeObjectOperator;
                }
                if n1 == b'?' {
                    if n2 == b'=' {
                        tok!(3, T::CoalesceEqual)
                    }
                    tok!(2, T::Coalesce)
                }
                if n1 == b'>' {
                    let mut e = p + 2;
                    if self.at(e) == b'\n' && e < src.len() {
                        e += 1;
                    } else if self.at(e) == b'\r' && e < src.len() {
                        e += 1;
                        if self.at(e) == b'\n' && e < src.len() {
                            e += 1;
                        }
                    }
                    self.pos = e;
                    self.state = State::Initial;
                    if e > p + 2 {
                        self.increment_line = true;
                    }
                    return if self.parser_mode() {
                        T::Semicolon
                    } else {
                        T::CloseTag
                    };
                }
                tok!(1, T::Question)
            }
            b'=' => {
                if n1 == b'=' {
                    if n2 == b'=' {
                        tok!(3, T::IsIdentical)
                    }
                    tok!(2, T::IsEqual)
                }
                if n1 == b'>' {
                    tok!(2, T::DoubleArrow)
                }
                tok!(1, T::Equals)
            }
            b'!' => {
                if n1 == b'=' {
                    if n2 == b'=' {
                        tok!(3, T::IsNotIdentical)
                    }
                    tok!(2, T::IsNotEqual)
                }
                tok!(1, T::Bang)
            }
            b'<' => {
                if n1 == b'<' && n2 == b'<' {
                    if let Some(k) = self.try_heredoc_start(p, 0) {
                        return k;
                    }
                }
                if n1 == b'=' {
                    if n2 == b'>' {
                        tok!(3, T::Spaceship)
                    }
                    tok!(2, T::IsSmallerOrEqual)
                }
                if n1 == b'<' {
                    if n2 == b'=' {
                        tok!(3, T::SlEqual)
                    }
                    tok!(2, T::Sl)
                }
                if n1 == b'>' {
                    tok!(2, T::IsNotEqual)
                }
                tok!(1, T::Lt)
            }
            b'>' => {
                if n1 == b'>' {
                    if n2 == b'=' {
                        tok!(3, T::SrEqual)
                    }
                    tok!(2, T::Sr)
                }
                if n1 == b'=' {
                    tok!(2, T::IsGreaterOrEqual)
                }
                tok!(1, T::Gt)
            }
            b'+' => {
                if n1 == b'+' {
                    tok!(2, T::Inc)
                }
                if n1 == b'=' {
                    tok!(2, T::PlusEqual)
                }
                tok!(1, T::Plus)
            }
            b'-' => {
                if n1 == b'-' {
                    tok!(2, T::Dec)
                }
                if n1 == b'=' {
                    tok!(2, T::MinusEqual)
                }
                if n1 == b'>' {
                    self.pos = p + 2;
                    self.push_state(State::LookingForProperty);
                    return T::ObjectOperator;
                }
                tok!(1, T::Minus)
            }
            b'*' => {
                if n1 == b'*' {
                    if n2 == b'=' {
                        tok!(3, T::PowEqual)
                    }
                    tok!(2, T::Pow)
                }
                if n1 == b'=' {
                    tok!(2, T::MulEqual)
                }
                tok!(1, T::Star)
            }
            b'%' => {
                if n1 == b'=' {
                    tok!(2, T::ModEqual)
                }
                tok!(1, T::Percent)
            }
            b'&' => {
                if n1 == b'&' {
                    tok!(2, T::BooleanAnd)
                }
                if n1 == b'=' {
                    tok!(2, T::AndEqual)
                }
                if let Some(q) = self.match_ws_or_comments(p + 1, false) {
                    if self.at(q) == b'$' && q < src.len()
                        || (self.at(q) == b'.' && self.at(q + 1) == b'.' && self.at(q + 2) == b'.')
                    {
                        tok!(1, T::AmpersandFollowedByVarOrVararg)
                    }
                }
                tok!(1, T::AmpersandNotFollowedByVarOrVararg)
            }
            b'|' => {
                if n1 == b'|' {
                    tok!(2, T::BooleanOr)
                }
                if n1 == b'=' {
                    tok!(2, T::OrEqual)
                }
                if n1 == b'>' {
                    tok!(2, T::PipeOp)
                }
                tok!(1, T::Pipe)
            }
            b'^' => {
                if n1 == b'=' {
                    tok!(2, T::XorEqual)
                }
                tok!(1, T::Caret)
            }
            b':' => {
                if n1 == b':' {
                    tok!(2, T::PaamayimNekudotayim)
                }
                tok!(1, T::Colon)
            }
            b'(' => {
                if let Some(k) = self.try_cast(p) {
                    return k;
                }
                self.pos = p + 1;
                self.enter_nesting(b'(');
                T::LParen
            }
            b'[' => {
                self.pos = p + 1;
                self.enter_nesting(b'[');
                T::LBracket
            }
            b')' => {
                self.pos = p + 1;
                self.exit_nesting_token(b')', T::RParen)
            }
            b']' => {
                self.pos = p + 1;
                self.exit_nesting_token(b']', T::RBracket)
            }
            b'{' => {
                self.pos = p + 1;
                self.push_state(State::InScripting);
                self.enter_nesting(b'{');
                T::LBrace
            }
            b'}' => {
                self.pos = p + 1;
                if !self.state_stack.is_empty() {
                    self.pop_state();
                }
                self.exit_nesting_token(b'}', T::RBrace)
            }
            b';' => tok!(1, T::Semicolon),
            b',' => tok!(1, T::Comma),
            b'~' => tok!(1, T::Tilde),
            b'@' => tok!(1, T::At),
            c if is_label_start(c) => self.scan_label(p),
            _ => tok!(1, T::BadCharacter),
        }
    }

    fn scan_qualified_end(&self, from: usize) -> usize {
        let mut e = self.scan_label_end(from);
        while self.at(e) == b'\\' && is_label_start(self.at(e + 1)) && e + 1 < self.src.len() {
            e = self.scan_label_end(e + 1);
        }
        e
    }

    fn scan_label(&mut self, p: usize) -> TokenKind {
        use TokenKind as T;
        let src = self.src.clone();
        let label_end = self.scan_label_end(p);
        let label = &src[p..label_end];

        // b-prefixed strings
        if label.len() == 1 && (label[0] == b'b' || label[0] == b'B') {
            match self.at(p + 1) {
                b'\'' if p + 1 < src.len() => return self.scan_single_quoted(p, 1),
                b'"' if p + 1 < src.len() => return self.scan_double_quoted_start(p, 1),
                b'<' if self.at(p + 2) == b'<' && self.at(p + 3) == b'<' => {
                    if let Some(k) = self.try_heredoc_start(p, 1) {
                        return k;
                    }
                }
                _ => {}
            }
        }

        // namespaced names (`scan_qualified_end(p)`, resuming after the
        // first segment that was just scanned)
        let mut qual_end = label_end;
        while self.at(qual_end) == b'\\'
            && is_label_start(self.at(qual_end + 1))
            && qual_end + 1 < src.len()
        {
            qual_end = self.scan_label_end(qual_end + 1);
        }
        if qual_end > label_end {
            self.pos = qual_end;
            return if label.eq_ignore_ascii_case(b"namespace") {
                T::NameRelative
            } else {
                T::NameQualified
            };
        }

        let kw = keyword(label);
        match kw {
            Some(T::Yield) => {
                if let Some(q) = self.match_ws_or_comments(label_end, true) {
                    if self.eq_ci(q, b"from") && !is_label_char(self.at(q + 4)) {
                        self.pos = q + 4;
                        self.handle_newlines(p, self.pos);
                        return T::YieldFrom;
                    }
                }
            }
            Some(T::Public | T::Protected | T::Private) if self.eq_ci(label_end, b"(set)") => {
                self.pos = label_end + 5;
                return match kw {
                    Some(T::Public) => T::PublicSet,
                    Some(T::Protected) => T::ProtectedSet,
                    _ => T::PrivateSet,
                };
            }
            _ => {}
        }
        if kw.is_none() && label.eq_ignore_ascii_case(b"enum") {
            if let Some(q) = self.match_ws_or_comments(label_end, true) {
                self.pos = label_end;
                if self.eq_ci(q, b"extends") || self.eq_ci(q, b"implements") {
                    return T::String;
                }
                if is_label_start(self.at(q)) && q < src.len() && !self.at(q).is_ascii_digit() {
                    return T::Enum;
                }
            }
            self.pos = label_end;
            return T::String;
        }
        self.pos = label_end;
        kw.unwrap_or(T::String)
    }

    fn try_cast(&mut self, p: usize) -> Option<TokenKind> {
        use TokenKind as T;
        let src = self.src.clone();
        let mut q = p + 1;
        while q < src.len() && (src[q] == b' ' || src[q] == b'\t') {
            q += 1;
        }
        let ws = q;
        while q < src.len() && src[q].is_ascii_alphabetic() {
            q += 1;
        }
        let word = &src[ws..q];
        if word.is_empty() || word.len() > 7 {
            return None;
        }
        while q < src.len() && (src[q] == b' ' || src[q] == b'\t') {
            q += 1;
        }
        if self.at(q) != b')' || q >= src.len() {
            return None;
        }
        let mut lower = word.to_vec();
        lower.make_ascii_lowercase();
        let line = self.line;
        let (kind, deprecated) = match &lower[..] {
            b"int" => (T::IntCast, None),
            b"integer" => (T::IntCast, Some(("integer", "int"))),
            b"float" => (T::DoubleCast, None),
            b"double" => (T::DoubleCast, Some(("double", "float"))),
            b"real" => {
                self.pos = q + 1;
                if self.parser_mode() {
                    self.throw("The (real) cast has been removed, use (float) instead");
                    return Some(T::Error);
                }
                return Some(T::DoubleCast);
            }
            b"string" => (T::StringCast, None),
            b"binary" => (T::StringCast, Some(("binary", "string"))),
            b"array" => (T::ArrayCast, None),
            b"object" => (T::ObjectCast, None),
            b"bool" => (T::BoolCast, None),
            b"boolean" => (T::BoolCast, Some(("boolean", "bool"))),
            b"unset" => (T::UnsetCast, None),
            b"void" => (T::VoidCast, None),
            _ => return None,
        };
        self.pos = q + 1;
        if let Some((old, new)) = deprecated.filter(|_| self.parser_mode()) {
            self.warn(
                WarningKind::Deprecated,
                format!("Non-canonical cast ({old}) is deprecated, use the ({new}) cast instead"),
                line,
            );
        }
        Some(kind)
    }

    fn scan_digits(&self, mut p: usize, pred: impl Fn(u8) -> bool) -> usize {
        // [d]+(_[d]+)*
        if !pred(self.at(p)) || p >= self.src.len() {
            return p;
        }
        while p < self.src.len() && pred(self.src[p]) {
            p += 1;
        }
        while self.at(p) == b'_' && p + 1 < self.src.len() && pred(self.src[p + 1]) {
            p += 1;
            while p < self.src.len() && pred(self.src[p]) {
                p += 1;
            }
        }
        p
    }

    fn scan_number(&mut self) -> TokenKind {
        use TokenKind as T;
        let src = self.src.clone();
        let p = self.pos;
        let dec = |c: u8| c.is_ascii_digit();

        // Prefixed integers.
        if src[p] == b'0' {
            let x = self.at(p + 1) | 0x20;
            let (radix, pred): (u32, fn(u8) -> bool) = match x {
                b'x' => (16, |c: u8| c.is_ascii_hexdigit()),
                b'b' => (2, |c: u8| c == b'0' || c == b'1'),
                b'o' => (8, |c: u8| (b'0'..=b'7').contains(&c)),
                _ => (0, |_| false),
            };
            if radix != 0 {
                let e = self.scan_digits(p + 2, pred);
                if e > p + 2 {
                    // Longest match: DNUM/LNUM can only match "0" here.
                    self.pos = e;
                    let v = parse_prefixed_int(&src[p..e], radix);
                    let kind = if matches!(v, TokenValue::Int(_)) {
                        T::LNumber
                    } else {
                        T::DNumber
                    };
                    if self.parser_mode() {
                        self.value = Some(v);
                    }
                    return kind;
                }
            }
        }

        // LNUM / DNUM / EXPONENT_DNUM, longest match.
        let int_end = self.scan_digits(p, dec);
        let mut end = int_end;
        let mut is_float = false;
        // DNUM: LNUM? "." LNUM | LNUM "." LNUM?
        if self.at(end) == b'.' && end < src.len() {
            let frac_end = self.scan_digits(end + 1, dec);
            if frac_end > end + 1 {
                end = frac_end;
                is_float = true;
            } else if int_end > p {
                end += 1;
                is_float = true;
            }
        }
        if end == p {
            // cannot happen: called on a digit or ".digit"
            self.pos = p + 1;
            return T::BadCharacter;
        }
        // exponent
        if matches!(self.at(end), b'e' | b'E') {
            let mut q = end + 1;
            if matches!(self.at(q), b'+' | b'-') {
                q += 1;
            }
            let exp_end = self.scan_digits(q, dec);
            if exp_end > q {
                end = exp_end;
                is_float = true;
            }
        }
        self.pos = end;
        let text = &src[p..end];
        if is_float {
            if self.parser_mode() {
                self.value = Some(TokenValue::Float(parse_float_literal(text)));
            }
            return T::DNumber;
        }
        let r = parse_lnum(text);
        if r.invalid_octal && self.parser_mode() {
            self.throw("Invalid numeric literal");
            return T::Error;
        }
        let kind = if matches!(r.value, TokenValue::Int(_)) {
            T::LNumber
        } else {
            T::DNumber
        };
        if self.parser_mode() {
            self.value = Some(r.value);
        }
        kind
    }

    fn scan_line_comment(&mut self, p: usize, prefix: usize) -> TokenKind {
        self.pos = self.line_comment_end(p + prefix);
        TokenKind::Comment
    }

    fn scan_block_comment(&mut self, p: usize) -> TokenKind {
        let src = self.src.clone();
        let is_doc = self.at(p + 2) == b'*'
            && p + 3 < src.len()
            && matches!(self.at(p + 3), b' ' | b'\n' | b'\r' | b'\t');
        let from = if is_doc { p + 4 } else { p + 2 };
        let q = if let Some(end) = find_comment_close(&src, from) {
            end
        } else {
            let q = src.len();
            let line = self.line;
            self.throw(format!("Unterminated comment starting line {line}"));
            if self.parser_mode() {
                self.pos = q;
                return TokenKind::Error;
            }
            q
        };
        self.pos = q;
        self.handle_newlines(p, q);
        if is_doc {
            TokenKind::DocComment
        } else {
            TokenKind::Comment
        }
    }

    fn scan_single_quoted(&mut self, p: usize, bprefix: usize) -> TokenKind {
        let src = self.src.clone();
        let mut q = p + bprefix + 1;
        loop {
            // Skip to the next quote or backslash in one scan.
            if let Some(k) = src
                .get(q..)
                .and_then(|rest| rest.iter().position(|&c| c == b'\'' || c == b'\\'))
            {
                q += k;
                let c = src[q];
                q += 1;
                if c == b'\'' {
                    break;
                }
                if q < src.len() {
                    q += 1;
                }
            } else {
                // Unterminated: T_ENCAPSED_AND_WHITESPACE with a NULL value.
                self.pos = src.len();
                self.handle_newlines(p, self.pos);
                return TokenKind::EncapsedAndWhitespace;
            }
        }
        self.pos = q;
        self.handle_newlines(p, q);
        if self.parser_mode() {
            let body = &src[p + bprefix + 1..q - 1];
            self.value = Some(TokenValue::Str(unescape_single_quoted(body)));
        }
        TokenKind::ConstantEncapsedString
    }

    /// Unescapes double-quote content, handling line counting, warnings and
    /// errors like `zend_scan_escape_string`. Returns false if an error was
    /// raised.
    fn escape_value(&mut self, start: usize, end: usize, quote: u8) -> bool {
        let line = self.line;
        let text = &self.src[start..end];
        match unescape_double_quoted(text, quote, !self.heredoc_scan_ahead) {
            Ok(u) => {
                for (msg, nl) in u.warnings {
                    self.warn(WarningKind::CompileWarning, msg, line + nl);
                }
                self.handle_newlines(start, end);
                if self.parser_mode() {
                    self.value = Some(TokenValue::Str(u.bytes));
                }
                true
            }
            Err(e) => {
                self.line += e.newlines_before;
                self.throw(e.message);
                false
            }
        }
    }

    fn scan_double_quoted_start(&mut self, p: usize, bprefix: usize) -> TokenKind {
        let src = self.src.clone();
        let body_start = p + bprefix + 1;
        let mut q = body_start;
        while q < src.len() {
            let c = src[q];
            q += 1;
            match c {
                b'"' => {
                    self.pos = q;
                    let ok = self.escape_value(body_start, q - 1, b'"');
                    if ok || !self.parser_mode() {
                        return TokenKind::ConstantEncapsedString;
                    }
                    return TokenKind::Error;
                }
                b'$' if (is_label_start(self.at(q)) || self.at(q) == b'{') => {
                    break;
                }
                b'{' if self.at(q) == b'$' => {
                    break;
                }
                b'\\' if q < src.len() => {
                    q += 1;
                }
                _ => {}
            }
        }
        self.pos = body_start;
        self.state = State::DoubleQuotes;
        TokenKind::DoubleQuote
    }

    #[allow(clippy::too_many_lines)]
    fn try_heredoc_start(&mut self, p: usize, bprefix: usize) -> Option<TokenKind> {
        let src = self.src.clone();
        let mut q = p + bprefix + 3;
        while q < src.len() && (src[q] == b' ' || src[q] == b'\t') {
            q += 1;
        }
        let quote = match self.at(q) {
            b'\'' => Some(b'\''),
            b'"' => Some(b'"'),
            _ => None,
        };
        if quote.is_some() {
            q += 1;
        }
        if !is_label_start(self.at(q)) || q >= src.len() {
            return None;
        }
        let label_start = q;
        let label_end = self.scan_label_end(q);
        q = label_end;
        if let Some(qc) = quote {
            if self.at(q) != qc || q >= src.len() {
                return None;
            }
            q += 1;
        }
        // NEWLINE
        match self.at(q) {
            b'\n' if q < src.len() => q += 1,
            b'\r' if q < src.len() => {
                q += 1;
                if self.at(q) == b'\n' && q < src.len() {
                    q += 1;
                }
            }
            _ => return None,
        }
        self.pos = q;
        self.line += 1;
        let is_heredoc = quote != Some(b'\'');
        self.state = if is_heredoc {
            State::Heredoc
        } else {
            State::Nowdoc
        };
        let label_len = label_end - label_start;
        self.heredoc_stack.push(HeredocLabel {
            start: label_start,
            len: label_len,
            indentation: 0,
            indentation_uses_spaces: false,
        });

        // Check for an ending label on the next line.
        let saved = self.pos;
        let mut cur = self.pos;
        let mut spacing = 0u8;
        let mut indentation = 0usize;
        while cur < src.len() && (src[cur] == b' ' || src[cur] == b'\t') {
            spacing |= if src[cur] == b'\t' {
                USING_TABS
            } else {
                USING_SPACES
            };
            cur += 1;
            indentation += 1;
        }
        if cur == src.len() {
            return Some(TokenKind::StartHeredoc);
        }
        if label_len < src.len() - cur
            && src[cur..cur + label_len] == src[label_start..label_end]
            && !is_label_char(src[cur + label_len])
        {
            if spacing == (USING_SPACES | USING_TABS) {
                self.throw("Invalid indentation - tabs and spaces cannot be mixed");
                if self.parser_mode() {
                    return Some(TokenKind::Error);
                }
            }
            if let Some(top) = self.heredoc_stack.last_mut() {
                top.indentation = indentation;
            }
            self.state = State::EndHeredoc;
            return Some(TokenKind::StartHeredoc);
        }

        if is_heredoc && !self.heredoc_scan_ahead {
            let (indent, uses_spaces, first_token, ahead_line) = self.heredoc_scan_ahead_run();
            let mut error = false;
            if matches!(
                first_token,
                Some(TokenKind::Variable | TokenKind::DollarOpenCurlyBraces | TokenKind::CurlyOpen)
            ) && indent > 0
            {
                self.throw_at(
                    format!(
                        "Invalid body indentation level (expecting an indentation level of at least {indent})"
                    ),
                    ahead_line,
                );
                error = true;
            }
            if let Some(top) = self.heredoc_stack.last_mut() {
                top.indentation = indent;
                top.indentation_uses_spaces = uses_spaces;
            }
            self.increment_line = false;
            if self.parser_mode() && error {
                return Some(TokenKind::Error);
            }
        }
        self.pos = saved;
        Some(TokenKind::StartHeredoc)
    }

    /// Lexes ahead to find the closing marker of the heredoc just started, to
    /// learn its indentation. Returns (indentation, uses spaces, first token,
    /// line at the end of the scan).
    fn heredoc_scan_ahead_run(&mut self) -> (usize, bool, Option<TokenKind>, u32) {
        let mut ahead = Lexer {
            src: self.src.clone(),
            encoding: self.encoding.clone(),
            pos: self.pos,
            tok_start: self.pos,
            line: self.line,
            increment_line: self.increment_line,
            state: self.state,
            state_stack: self.state_stack.clone(),
            heredoc_stack: self.heredoc_stack.clone(),
            nesting: self.nesting.clone(),
            opts: LexerOptions {
                mode: LexMode::Tokenizer,
                ..self.opts
            },
            value: None,
            error: None,
            warnings: Vec::new(),
            heredoc_scan_ahead: true,
            heredoc_indentation: 0,
            heredoc_indentation_uses_spaces: false,
            done: false,
            skip_trivia: false,
        };
        let mut level = 1;
        let mut first = None;
        while level > 0 {
            let tok = ahead.next_token();
            if ahead.error.is_some() {
                break;
            }
            let kind = tok.map_or(TokenKind::End, |t| t.kind);
            if first.is_none() {
                first = Some(kind);
            }
            match kind {
                TokenKind::StartHeredoc => level += 1,
                TokenKind::EndHeredoc => level -= 1,
                TokenKind::End => level = 0,
                _ => {}
            }
        }
        (
            ahead.heredoc_indentation,
            ahead.heredoc_indentation_uses_spaces,
            first,
            ahead.line,
        )
    }

    // ---- ST_LOOKING_FOR_PROPERTY -------------------------------------------

    fn scan_looking_for_property(&mut self) -> Option<TokenKind> {
        let src = self.src.clone();
        let p = self.pos;
        let c = self.at(p);
        if p < src.len() {
            match c {
                b' ' | b'\n' | b'\r' | b'\t' => {
                    let mut q = p + 1;
                    while q < src.len() && matches!(src[q], b' ' | b'\n' | b'\r' | b'\t') {
                        q += 1;
                    }
                    self.pos = q;
                    self.handle_newlines(p, q);
                    return Some(TokenKind::Whitespace);
                }
                b'-' if self.at(p + 1) == b'>' => {
                    self.pos = p + 2;
                    return Some(TokenKind::ObjectOperator);
                }
                b'?' if self.at(p + 1) == b'-' && self.at(p + 2) == b'>' => {
                    self.pos = p + 3;
                    return Some(TokenKind::NullsafeObjectOperator);
                }
                b'#' => return Some(self.scan_line_comment(p, 1)),
                b'/' if self.at(p + 1) == b'/' => return Some(self.scan_line_comment(p, 2)),
                b'/' if self.at(p + 1) == b'*' => return Some(self.scan_block_comment(p)),
                c if is_label_start(c) => {
                    self.pos = self.scan_label_end(p);
                    self.pop_state();
                    return Some(TokenKind::String);
                }
                _ => {}
            }
        }
        self.pop_state();
        None
    }

    // ---- ST_LOOKING_FOR_VARNAME --------------------------------------------

    fn scan_looking_for_varname(&mut self) -> Option<TokenKind> {
        let p = self.pos;
        if is_label_start(self.at(p)) && p < self.src.len() {
            let e = self.scan_label_end(p);
            if matches!(self.at(e), b'[' | b'}') && e < self.src.len() {
                self.pos = e;
                self.pop_state();
                self.push_state(State::InScripting);
                return Some(TokenKind::StringVarname);
            }
        }
        self.pop_state();
        self.push_state(State::InScripting);
        None
    }

    // ---- ST_VAR_OFFSET -----------------------------------------------------

    fn scan_var_offset(&mut self) -> TokenKind {
        use TokenKind as T;
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        let c = src[p];
        match c {
            b'0'..=b'9' => {
                // [0]|([1-9][0-9]*) vs {LNUM}|{HNUM}|{BNUM}|{ONUM}
                let dec = |c: u8| c.is_ascii_digit();
                let simple_end = if c == b'0' {
                    p + 1
                } else {
                    let mut q = p + 1;
                    while q < src.len() && src[q].is_ascii_digit() {
                        q += 1;
                    }
                    q
                };
                let mut long_end = self.scan_digits(p, dec);
                if c == b'0' {
                    let x = self.at(p + 1) | 0x20;
                    let pred: Option<fn(u8) -> bool> = match x {
                        b'x' => Some(|c: u8| c.is_ascii_hexdigit()),
                        b'b' => Some(|c: u8| c == b'0' || c == b'1'),
                        b'o' => Some(|c: u8| (b'0'..=b'7').contains(&c)),
                        _ => None,
                    };
                    if let Some(pred) = pred {
                        let e = self.scan_digits(p + 2, pred);
                        if e > p + 2 {
                            long_end = long_end.max(e);
                        }
                    }
                }
                if simple_end >= long_end {
                    self.pos = simple_end;
                    if self.parser_mode() {
                        let text = &src[p..simple_end];
                        let v = std::str::from_utf8(text)
                            .ok()
                            .and_then(|s| s.parse::<i64>().ok())
                            .map_or_else(|| TokenValue::Str(text.to_vec()), TokenValue::Int);
                        self.value = Some(v);
                    }
                } else {
                    self.pos = long_end;
                    if self.parser_mode() {
                        self.value = Some(TokenValue::Str(src[p..long_end].to_vec()));
                    }
                }
                T::NumString
            }
            b'$' if is_label_start(self.at(p + 1)) && p + 1 < src.len() => {
                self.pos = self.scan_label_end(p + 1);
                T::Variable
            }
            b']' => {
                self.pos = p + 1;
                self.pop_state();
                T::RBracket
            }
            b' ' | b'\n' | b'\r' | b'\t' | b'\\' | b'\'' | b'#' => {
                self.pop_state();
                T::EncapsedAndWhitespace
            }
            c if is_label_start(c) => {
                self.pos = self.scan_label_end(p);
                T::String
            }
            c => {
                self.pos = p + 1;
                if let Some(k) = T::from_char(c) {
                    if b";:,.|^&+-/*=%!~$<>?@[(){}\"`".contains(&c) {
                        return k;
                    }
                }
                T::BadCharacter
            }
        }
    }

    // ---- interpolation shared rules ----------------------------------------

    /// Rules shared by double quotes, backquotes and heredocs. Returns `None`
    /// if none matched.
    fn scan_interpolation_start(&mut self) -> Option<TokenKind> {
        let src = self.src.clone();
        let p = self.pos;
        let c = self.at(p);
        if c == b'$' && p < src.len() {
            let n = self.at(p + 1);
            if is_label_start(n) && p + 1 < src.len() {
                let e = self.scan_label_end(p + 1);
                self.pos = e;
                let next = [self.at(e), self.at(e + 1), self.at(e + 2), self.at(e + 3)];
                let arrow = next[0] == b'-'
                    && next[1] == b'>'
                    && is_label_start(next[2])
                    && e + 2 < src.len();
                let nullsafe = next[0] == b'?'
                    && next[1] == b'-'
                    && next[2] == b'>'
                    && is_label_start(next[3])
                    && e + 3 < src.len();
                if arrow || nullsafe {
                    self.push_state(State::LookingForProperty);
                } else if next[0] == b'[' && e < src.len() {
                    self.push_state(State::VarOffset);
                }
                return Some(TokenKind::Variable);
            }
            if n == b'{' {
                self.pos = p + 2;
                self.push_state(State::LookingForVarname);
                self.enter_nesting(b'{');
                return Some(TokenKind::DollarOpenCurlyBraces);
            }
        }
        if c == b'{' && self.at(p + 1) == b'$' {
            self.pos = p + 1;
            self.push_state(State::InScripting);
            self.enter_nesting(b'{');
            return Some(TokenKind::CurlyOpen);
        }
        None
    }

    fn scan_double_quotes(&mut self) -> TokenKind {
        if let Some(k) = self.scan_interpolation_start() {
            return k;
        }
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        if src[p] == b'"' {
            self.pos = p + 1;
            self.state = State::InScripting;
            return TokenKind::DoubleQuote;
        }
        let mut q = p + 1;
        if src[p] == b'\\' && q < src.len() {
            q += 1;
        }
        while q < src.len() {
            let c = src[q];
            q += 1;
            match c {
                b'"' => {
                    q -= 1;
                    break;
                }
                b'$' if is_label_start(self.at(q)) || self.at(q) == b'{' => {
                    q -= 1;
                    break;
                }
                b'{' if self.at(q) == b'$' => {
                    q -= 1;
                    break;
                }
                b'\\' if q < src.len() => q += 1,
                _ => {}
            }
        }
        self.pos = q;
        let ok = self.escape_value(p, q, b'"');
        if ok || !self.parser_mode() {
            TokenKind::EncapsedAndWhitespace
        } else {
            TokenKind::Error
        }
    }

    fn scan_backquote(&mut self) -> TokenKind {
        if let Some(k) = self.scan_interpolation_start() {
            return k;
        }
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        if src[p] == b'`' {
            self.pos = p + 1;
            self.state = State::InScripting;
            return TokenKind::Backtick;
        }
        let mut q = p + 1;
        if src[p] == b'\\' && q < src.len() {
            q += 1;
        }
        while q < src.len() {
            let c = src[q];
            q += 1;
            match c {
                b'`' => {
                    q -= 1;
                    break;
                }
                b'$' if is_label_start(self.at(q)) || self.at(q) == b'{' => {
                    q -= 1;
                    break;
                }
                b'{' if self.at(q) == b'$' => {
                    q -= 1;
                    break;
                }
                b'\\' if q < src.len() => q += 1,
                _ => {}
            }
        }
        self.pos = q;
        let ok = self.escape_value(p, q, b'`');
        if ok || !self.parser_mode() {
            TokenKind::EncapsedAndWhitespace
        } else {
            TokenKind::Error
        }
    }

    #[allow(clippy::too_many_lines)]
    fn scan_heredoc(&mut self) -> TokenKind {
        if let Some(k) = self.scan_interpolation_start() {
            return k;
        }
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        let Some(label) = self.heredoc_stack.last().copied() else {
            self.pos = src.len();
            return TokenKind::EncapsedAndWhitespace;
        };
        let mut newline = 0usize;
        let mut q = p;
        let mut found_end = false;
        while q < src.len() {
            let c = src[q];
            q += 1;
            match c {
                b'\r' | b'\n' => {
                    if c == b'\r' && self.at(q) == b'\n' && q < src.len() {
                        q += 1;
                    }
                    let mut indentation = 0usize;
                    let mut spacing = 0u8;
                    while q < src.len() && (src[q] == b' ' || src[q] == b'\t') {
                        spacing |= if src[q] == b'\t' {
                            USING_TABS
                        } else {
                            USING_SPACES
                        };
                        q += 1;
                        indentation += 1;
                    }
                    if q == src.len() {
                        self.pos = q;
                        self.handle_newlines(p, q);
                        return TokenKind::EncapsedAndWhitespace;
                    }
                    if is_label_start(src[q])
                        && label.len < src.len() - q
                        && src[q..q + label.len] == src[label.start..label.start + label.len]
                    {
                        if is_label_char(src[q + label.len]) {
                            continue;
                        }
                        if spacing == (USING_SPACES | USING_TABS) {
                            self.throw("Invalid indentation - tabs and spaces cannot be mixed");
                            if self.parser_mode() {
                                self.pos = q;
                                return TokenKind::Error;
                            }
                        }
                        newline = if q >= indentation + 2
                            && src[q - indentation - 2] == b'\r'
                            && src[q - indentation - 1] == b'\n'
                        {
                            2
                        } else {
                            1
                        };
                        self.increment_line = true;
                        if self.heredoc_scan_ahead {
                            self.heredoc_indentation = indentation;
                            self.heredoc_indentation_uses_spaces = spacing == USING_SPACES;
                        } else {
                            q -= indentation;
                        }
                        self.state = State::EndHeredoc;
                        found_end = true;
                        break;
                    }
                }
                b'$' if is_label_start(self.at(q)) || self.at(q) == b'{' => {
                    q -= 1;
                    break;
                }
                b'{' if self.at(q) == b'$' => {
                    q -= 1;
                    break;
                }
                b'\\' if q < src.len() && src[q] != b'\n' && src[q] != b'\r' => {
                    q += 1;
                }
                _ => {}
            }
        }
        let _ = found_end;
        self.pos = q;
        let content_end = q - newline;
        if !self.heredoc_scan_ahead && self.error.is_none() && self.parser_mode() {
            let newline_at_start = p > 0 && matches!(src[p - 1], b'\n' | b'\r');
            let stripped = match strip_indentation(
                &src[p..content_end],
                label.indentation,
                label.indentation_uses_spaces,
                newline_at_start,
                newline != 0,
            ) {
                Ok(s) => s,
                Err(e) => {
                    self.line += e.newlines_before;
                    self.throw(e.message);
                    return TokenKind::Error;
                }
            };
            let line = self.line;
            match unescape_double_quoted(&stripped, 0, true) {
                Ok(u) => {
                    for (msg, nl) in u.warnings {
                        self.warn(WarningKind::CompileWarning, msg, line + nl);
                    }
                    self.line += count_newlines(&stripped, 0, stripped.len());
                    self.value = Some(TokenValue::Str(u.bytes));
                }
                Err(e) => {
                    self.line += e.newlines_before;
                    self.throw(e.message);
                    return TokenKind::Error;
                }
            }
        } else {
            self.handle_newlines(p, content_end);
        }
        TokenKind::EncapsedAndWhitespace
    }

    fn scan_nowdoc(&mut self) -> TokenKind {
        let src = self.src.clone();
        let p = self.pos;
        if p >= src.len() {
            return self.end_token();
        }
        let Some(label) = self.heredoc_stack.last().copied() else {
            self.pos = src.len();
            return TokenKind::EncapsedAndWhitespace;
        };
        let mut newline = 0usize;
        let mut indentation = 0usize;
        let mut spacing: Option<u8> = None;
        let mut q = p;
        while q < src.len() {
            let c = src[q];
            q += 1;
            if c == b'\r' || c == b'\n' {
                if c == b'\r' && self.at(q) == b'\n' && q < src.len() {
                    q += 1;
                }
                indentation = 0;
                let mut sp = 0u8;
                while q < src.len() && (src[q] == b' ' || src[q] == b'\t') {
                    sp |= if src[q] == b'\t' {
                        USING_TABS
                    } else {
                        USING_SPACES
                    };
                    q += 1;
                    indentation += 1;
                }
                spacing = Some(sp);
                if q == src.len() {
                    self.pos = q;
                    self.handle_newlines(p, q);
                    return TokenKind::EncapsedAndWhitespace;
                }
                if is_label_start(src[q])
                    && label.len < src.len() - q
                    && src[q..q + label.len] == src[label.start..label.start + label.len]
                {
                    if is_label_char(src[q + label.len]) {
                        continue;
                    }
                    if sp == (USING_SPACES | USING_TABS) {
                        self.throw("Invalid indentation - tabs and spaces cannot be mixed");
                        if self.parser_mode() {
                            self.pos = q;
                            return TokenKind::Error;
                        }
                    }
                    newline = if q >= indentation + 2
                        && src[q - indentation - 2] == b'\r'
                        && src[q - indentation - 1] == b'\n'
                    {
                        2
                    } else {
                        1
                    };
                    self.increment_line = true;
                    q -= indentation;
                    if let Some(top) = self.heredoc_stack.last_mut() {
                        top.indentation = indentation;
                    }
                    self.state = State::EndHeredoc;
                    break;
                }
            }
        }
        self.pos = q;
        let content_end = q - newline;
        if self.error.is_none() && self.parser_mode() {
            let body = &src[p..content_end];
            if let Some(sp) = spacing {
                let newline_at_start = p > 0 && matches!(src[p - 1], b'\n' | b'\r');
                match strip_indentation(
                    body,
                    indentation,
                    sp == USING_SPACES,
                    newline_at_start,
                    newline != 0,
                ) {
                    Ok(s) => self.value = Some(TokenValue::Str(s)),
                    Err(e) => {
                        self.line += e.newlines_before;
                        self.throw(e.message);
                        return TokenKind::Error;
                    }
                }
            } else {
                self.value = Some(TokenValue::Str(body.to_vec()));
            }
        }
        self.handle_newlines(p, content_end);
        TokenKind::EncapsedAndWhitespace
    }

    fn scan_end_heredoc(&mut self) -> TokenKind {
        let label = self.heredoc_stack.pop();
        let len = label.map_or(0, |l| l.indentation + l.len);
        self.pos = (self.pos + len.max(1)).min(self.src.len());
        self.state = State::InScripting;
        TokenKind::EndHeredoc
    }
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        self.next_token()
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn label_char_table_matches_predicate() {
        for c in 0..=255u8 {
            assert_eq!(LABEL_CHAR[usize::from(c)], is_label_char(c), "{c:#x}");
        }
    }

    #[test]
    fn comment_close_matches_naive_search() {
        fn naive(src: &[u8], from: usize) -> Option<usize> {
            (from..src.len().saturating_sub(1))
                .find(|&k| src[k] == b'*' && src[k + 1] == b'/')
                .map(|k| k + 2)
        }
        let cases: &[&[u8]] = &[
            b"/**/",
            b"/*/",
            b"/* a */",
            b"/** \n * x/y http://a/b\n * z */ tail */",
            b"/* ******** //////// *",
            b"/* unterminated *",
            b"/* 0123456789abcdef*/",
            b"/* 0123456/89abcdef0123*/",
            b"/*\xff\xfe////////*//",
        ];
        for &src in cases {
            for from in 2..=src.len() {
                assert_eq!(
                    find_comment_close(src, from),
                    naive(src, from),
                    "{:?} from {from}",
                    String::from_utf8_lossy(src)
                );
            }
        }
    }

    #[test]
    fn label_ends() {
        let lx = Lexer::new(b"ab_9\xe9x-y");
        assert_eq!(lx.scan_label_end(0), 6);
        assert_eq!(lx.scan_label_end(6), 6);
        assert_eq!(lx.scan_label_end(7), 8);
        assert_eq!(lx.scan_label_end(8), 8);
        assert_eq!(lx.scan_label_end(9), 9);
    }

    /// Every PHP keyword (the `zend_language_scanner.l` list), lowercase.
    const KEYWORDS: &[(&str, TokenKind)] = &[
        ("exit", TokenKind::Exit),
        ("die", TokenKind::Exit),
        ("fn", TokenKind::Fn),
        ("function", TokenKind::Function),
        ("const", TokenKind::Const),
        ("return", TokenKind::Return),
        ("yield", TokenKind::Yield),
        ("try", TokenKind::Try),
        ("catch", TokenKind::Catch),
        ("finally", TokenKind::Finally),
        ("throw", TokenKind::Throw),
        ("if", TokenKind::If),
        ("elseif", TokenKind::ElseIf),
        ("endif", TokenKind::EndIf),
        ("else", TokenKind::Else),
        ("while", TokenKind::While),
        ("endwhile", TokenKind::EndWhile),
        ("do", TokenKind::Do),
        ("for", TokenKind::For),
        ("endfor", TokenKind::EndFor),
        ("foreach", TokenKind::Foreach),
        ("endforeach", TokenKind::EndForeach),
        ("declare", TokenKind::Declare),
        ("enddeclare", TokenKind::EndDeclare),
        ("instanceof", TokenKind::Instanceof),
        ("as", TokenKind::As),
        ("switch", TokenKind::Switch),
        ("match", TokenKind::Match),
        ("endswitch", TokenKind::EndSwitch),
        ("case", TokenKind::Case),
        ("default", TokenKind::Default),
        ("break", TokenKind::Break),
        ("continue", TokenKind::Continue),
        ("goto", TokenKind::Goto),
        ("echo", TokenKind::Echo),
        ("print", TokenKind::Print),
        ("class", TokenKind::Class),
        ("interface", TokenKind::Interface),
        ("trait", TokenKind::Trait),
        ("extends", TokenKind::Extends),
        ("implements", TokenKind::Implements),
        ("new", TokenKind::New),
        ("clone", TokenKind::Clone),
        ("var", TokenKind::Var),
        ("eval", TokenKind::Eval),
        ("include", TokenKind::Include),
        ("include_once", TokenKind::IncludeOnce),
        ("require", TokenKind::Require),
        ("require_once", TokenKind::RequireOnce),
        ("namespace", TokenKind::Namespace),
        ("use", TokenKind::Use),
        ("insteadof", TokenKind::Insteadof),
        ("global", TokenKind::Global),
        ("isset", TokenKind::Isset),
        ("empty", TokenKind::Empty),
        ("__halt_compiler", TokenKind::HaltCompiler),
        ("static", TokenKind::Static),
        ("abstract", TokenKind::Abstract),
        ("final", TokenKind::Final),
        ("private", TokenKind::Private),
        ("protected", TokenKind::Protected),
        ("public", TokenKind::Public),
        ("readonly", TokenKind::Readonly),
        ("unset", TokenKind::Unset),
        ("list", TokenKind::List),
        ("array", TokenKind::Array),
        ("callable", TokenKind::Callable),
        ("or", TokenKind::LogicalOr),
        ("and", TokenKind::LogicalAnd),
        ("xor", TokenKind::LogicalXor),
        ("__class__", TokenKind::ClassC),
        ("__trait__", TokenKind::TraitC),
        ("__function__", TokenKind::FuncC),
        ("__property__", TokenKind::PropertyC),
        ("__method__", TokenKind::MethodC),
        ("__line__", TokenKind::Line),
        ("__file__", TokenKind::File),
        ("__dir__", TokenKind::Dir),
        ("__namespace__", TokenKind::NsC),
    ];

    #[test]
    fn keyword_table_is_exact_and_case_insensitive() {
        assert_eq!(KEYWORDS.len(), 79);
        for &(kw, kind) in KEYWORDS {
            assert_eq!(keyword(kw.as_bytes()), Some(kind), "{kw}");
            assert_eq!(
                keyword(kw.to_ascii_uppercase().as_bytes()),
                Some(kind),
                "{kw}"
            );
            // Mixed case: uppercase every other byte.
            let mixed: Vec<u8> = kw
                .bytes()
                .enumerate()
                .map(|(i, b)| {
                    if i % 2 == 0 {
                        b.to_ascii_uppercase()
                    } else {
                        b
                    }
                })
                .collect();
            assert_eq!(keyword(&mixed), Some(kind), "{kw}");
            // Prefixes and extensions are plain labels.
            assert_eq!(keyword(&kw.as_bytes()[..kw.len() - 1]), None, "{kw}");
            assert_eq!(keyword(format!("{kw}s").as_bytes()), None, "{kw}s");
            assert_eq!(keyword(format!("{kw}_").as_bytes()), None, "{kw}_");
        }
        // Contextual words and non-keywords stay T_STRING.
        for w in [
            "enum",
            "from",
            "mixed",
            "self",
            "parent",
            "true",
            "null",
            "int",
            "a",
            "_",
            "",
            "__halt_compiler_",
            "__COMPILER_HALT_OFFSET__",
            "\u{e9}lse",
            "ifx",
            "zz",
            "ELS\u{c9}",
        ] {
            assert_eq!(keyword(w.as_bytes()), None, "{w}");
        }
        // Non-ASCII bytes are never folded.
        assert_eq!(keyword(b"\xc9cho"), None);
        assert_eq!(keyword(b"ech\xcf"), None);
    }
}
