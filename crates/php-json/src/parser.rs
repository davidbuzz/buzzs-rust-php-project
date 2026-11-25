// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The JSON parser: an iterative port of the bison grammar in
//! `ext/json/json_parser.y`.
//!
//! The parser reproduces the observable behaviour of the generated LALR(1)
//! parser exactly:
//!
//! * tokens are requested from the scanner only when bison would need a
//!   lookahead, so the reported error location (the start of the last scanned
//!   token) and the error code (scanner error vs. parser error) match;
//! * semantic actions run at the same points: the depth check right after
//!   `[`/`{` (`PHP_JSON_DEPTH_INC`), the object creation when the first member
//!   is reduced (which matters for object handle numbers), the property-name
//!   check on each member, the state-mismatch errors for `[}` / `{]`;
//! * bison's state stack is limited to `YYMAXDEPTH` (10000) entries: the
//!   parser counts the stack size like the generated code and fails with a
//!   syntax error ("memory exhausted") when it would overflow, which limits
//!   nesting to 4998 arrays or 2499 objects regardless of `$depth`.

use crate::error::{ErrorCode, JsonError};
use crate::scanner::{Scanner, Token};
use php_runtime::{RuntimeError, ZStr};

/// bison's `YYMAXDEPTH`.
const YYMAXDEPTH: usize = 10_000;

/// Strings shorter than this are not reserved first: the allocator records
/// a `memory_limit` violation of a small allocation itself, and the parser
/// stops at the next value ([`Sink::aborted`]). Only a large allocation can
/// jump from under the limit to over the hard ceiling at once.
const RESERVE_MIN: usize = 4096;

/// `_ZSTR_STRUCT_SIZE(len)` rounded by `ZEND_MM_ALIGNED_SIZE`: the bytes
/// php-src allocates for a `zend_string` of `len` bytes (a 24-byte header,
/// the bytes and the terminating NUL). Saturates instead of overflowing.
pub(crate) fn zend_string_size(len: usize) -> usize {
    len.saturating_add(24 + 1 + 7) & !7
}

/// A failure inside a semantic action.
#[derive(Debug)]
pub(crate) enum ActionError {
    /// A JSON error code (reported at the current location).
    Json(ErrorCode),
    /// An exception thrown by the host.
    Exception(RuntimeError),
}

/// A parse failure.
#[derive(Debug)]
pub(crate) enum ParseError {
    /// A JSON error with location.
    Json(JsonError),
    /// An exception thrown by the host.
    Exception(RuntimeError),
}

/// A scalar token value.
#[derive(Debug)]
pub(crate) enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    Str(Vec<u8>),
}

/// The semantic actions of the grammar (`php_json_parser_methods`).
pub(crate) trait Sink {
    /// A parsed value.
    type Value;
    /// An array under construction.
    type Array;
    /// An object under construction.
    type Object;

    /// A scalar value.
    fn scalar(&mut self, s: Scalar) -> Self::Value;
    /// `elements: %empty`.
    fn empty_array(&mut self) -> Self::Value;
    /// `element: value` (array creation followed by the first append).
    fn array_create(&mut self) -> Self::Array;
    /// `array_append`.
    fn array_append(&mut self, array: &mut Self::Array, value: Self::Value);
    /// The array's value once complete.
    fn array_finish(&mut self, array: Self::Array) -> Self::Value;
    /// `members: %empty`.
    fn empty_object(&mut self) -> Result<Self::Value, ActionError>;
    /// `member: key ':' value` (object creation before the first update).
    fn object_create(&mut self) -> Result<Self::Object, ActionError>;
    /// `object_update`.
    fn object_update(
        &mut self,
        object: &mut Self::Object,
        key: ZStr,
        value: Self::Value,
    ) -> Result<(), ActionError>;
    /// The object's value once complete.
    fn object_finish(&mut self, object: Self::Object) -> Self::Value;
    /// Reserves the `bytes` a string value of `len` bytes takes
    /// (`zend_string_falloc(len, 0, /* fallible */ true)` in php-src's
    /// scanner) against the host's `memory_limit`, before the value is
    /// built. `false` (the limit would be exceeded; the host has recorded
    /// the out-of-memory fatal it raises once the decoder returns) fails the
    /// parse.
    fn reserve_string(&mut self, bytes: usize) -> bool {
        let _ = bytes;
        true
    }
    /// Whether to stop before the next value: the host has a delayed
    /// out-of-memory fatal pending (`ZEND_HAS_OOM()`), which it raises once
    /// the decoder returns. The parse then fails as a syntax error.
    fn aborted(&mut self) -> bool {
        false
    }
}

/// An open container on the parser stack.
enum Frame<S: Sink> {
    /// An array; `None` until the first element is reduced.
    Array(Option<S::Array>),
    /// An object; `None` until the first member is reduced. `key` is the key
    /// of the member whose value is being parsed.
    Object(Option<S::Object>, ZStr),
}

/// The parser state (`php_json_parser`).
pub(crate) struct Parser<'a, S: Sink> {
    scanner: Scanner<'a>,
    sink: S,
    depth: i64,
    max_depth: i64,
    /// Number of entries on bison's state stack.
    stack_size: usize,
}

impl<'a, S: Sink> Parser<'a, S> {
    /// `php_json_parser_init_ex`. `max_depth == 0` disables the depth check.
    pub(crate) fn new(
        input: &'a [u8],
        options: i64,
        max_depth: i64,
        sink: S,
        validate: bool,
    ) -> Self {
        Parser {
            scanner: Scanner::new(input, options, validate),
            sink,
            depth: 1,
            max_depth,
            stack_size: 1,
        }
    }

    fn error(&self, code: ErrorCode) -> ParseError {
        ParseError::Json(JsonError::at(
            code,
            self.scanner.line(),
            self.scanner.column(),
        ))
    }

    fn action_error(&self, e: ActionError) -> ParseError {
        match e {
            ActionError::Json(code) => self.error(code),
            ActionError::Exception(ex) => ParseError::Exception(ex),
        }
    }

    /// The error for an unexpected lookahead token: a scanner error keeps its
    /// code, anything else is a syntax error (`php_json_yyerror`).
    fn unexpected(&self, tok: &Token) -> ParseError {
        match tok {
            Token::Error(code) => self.error(*code),
            _ => self.error(ErrorCode::Syntax),
        }
    }

    /// Pushes `n` states on bison's stack (`yynewstate`), failing like
    /// `yyexhaustedlab` when the stack would reach `YYMAXDEPTH` entries.
    fn push(&mut self, n: usize) -> Result<(), ParseError> {
        for _ in 0..n {
            self.stack_size += 1;
            if self.stack_size >= YYMAXDEPTH {
                return Err(self.error(ErrorCode::Syntax));
            }
        }
        Ok(())
    }

    /// `PHP_JSON_DEPTH_INC`.
    fn depth_inc(&mut self) -> Result<(), ParseError> {
        if self.max_depth != 0 && self.depth >= self.max_depth {
            return Err(self.error(ErrorCode::Depth));
        }
        self.depth += 1;
        Ok(())
    }

    /// The next token (`php_json_yylex`). A string is measured by the
    /// scanner's first pass, its memory reserved through the sink, and only
    /// then built, in one exactly sized allocation; a failed reservation is
    /// a scanner error (php-src returns `PHP_JSON_T_ERROR` with the
    /// out-of-memory error pending).
    fn scan(&mut self) -> Token {
        match self.scanner.scan() {
            Token::StrLen(len) => {
                if len >= RESERVE_MIN && !self.sink.reserve_string(zend_string_size(len)) {
                    return Token::Error(ErrorCode::Syntax);
                }
                Token::Str(self.scanner.string_value(len))
            }
            tok => tok,
        }
    }

    /// Runs `php_json_yyparse` and returns the parsed value.
    pub(crate) fn parse(mut self) -> Result<S::Value, ParseError> {
        let mut stack: Vec<Frame<S>> = Vec::new();
        let mut tok = self.scan();
        loop {
            if self.sink.aborted() {
                return Err(self.error(ErrorCode::Syntax));
            }
            // `tok` is the lookahead at a position expecting a value.
            let mut value = match self.start_value(tok, &mut stack)? {
                Started::Value(v) => v,
                Started::Opened(next) => {
                    tok = next;
                    continue;
                }
            };
            // A complete value has been reduced: attach it to its container,
            // closing containers as long as their end follows.
            loop {
                let attached = match stack.last_mut() {
                    None => return self.finish(value),
                    Some(Frame::Array(arr)) => self.array_element(arr, value)?,
                    Some(Frame::Object(obj, key)) => self.object_member(obj, key, value)?,
                };
                match attached {
                    Attached::Next(next) => {
                        tok = next;
                        break;
                    }
                    Attached::Closed => {
                        value = match stack.pop() {
                            Some(Frame::Array(Some(a))) => self.sink.array_finish(a),
                            Some(Frame::Object(Some(o), _)) => self.sink.object_finish(o),
                            _ => unreachable!("closed an empty frame"),
                        };
                    }
                }
            }
        }
    }

    /// Handles the first token of a value: shifts and reduces a scalar or an
    /// empty container, or opens a container (pushing its frame and
    /// returning the lookahead of its first value).
    fn start_value(
        &mut self,
        tok: Token,
        stack: &mut Vec<Frame<S>>,
    ) -> Result<Started<S::Value>, ParseError> {
        let scalar = match tok {
            Token::LBracket => return self.open_array(stack),
            Token::LBrace => return self.open_object(stack),
            Token::Null => Scalar::Null,
            Token::True => Scalar::Bool(true),
            Token::False => Scalar::Bool(false),
            Token::Int(i) => Scalar::Int(i),
            Token::Double(d) => Scalar::Double(d),
            Token::Str(s) => Scalar::Str(s),
            other => return Err(self.unexpected(&other)),
        };
        self.push(1)?; // shift; reduce `value`
        Ok(Started::Value(self.sink.scalar(scalar)))
    }

    /// `array: '[' $@ elements array_end` up to the first element.
    fn open_array(&mut self, stack: &mut Vec<Frame<S>>) -> Result<Started<S::Value>, ParseError> {
        self.push(1)?; // shift '['
        self.depth_inc()?; // $@ action
        self.push(1)?; // $@ goto
        let tok = self.scan();
        if !matches!(tok, Token::RBracket | Token::RBrace) {
            stack.push(Frame::Array(None));
            return Ok(Started::Opened(tok));
        }
        // elements: %empty; shift array_end; reduce.
        self.push(2)?;
        if tok == Token::RBrace {
            return Err(self.error(ErrorCode::StateMismatch));
        }
        self.close(3);
        Ok(Started::Value(self.sink.empty_array()))
    }

    /// `object: '{' $@ members object_end` up to the first member's value.
    fn open_object(&mut self, stack: &mut Vec<Frame<S>>) -> Result<Started<S::Value>, ParseError> {
        self.push(1)?; // shift '{'
        self.depth_inc()?; // $@ action
        self.push(1)?; // $@ goto
        match self.scan() {
            tok @ (Token::RBrace | Token::RBracket) => {
                // members: %empty (creates the object); shift object_end.
                self.push(1)?;
                let v = self.sink.empty_object().map_err(|e| self.action_error(e))?;
                self.push(1)?;
                if tok == Token::RBracket {
                    return Err(self.error(ErrorCode::StateMismatch));
                }
                self.close(3);
                Ok(Started::Value(v))
            }
            Token::Str(key) => {
                self.push(1)?; // shift key (reduced in place)
                self.colon()?;
                stack.push(Frame::Object(None, ZStr::from(key)));
                Ok(Started::Opened(self.scan()))
            }
            other => Err(self.unexpected(&other)),
        }
    }

    /// Expects and shifts the `':'` after a key.
    fn colon(&mut self) -> Result<(), ParseError> {
        let tok = self.scan();
        if tok != Token::Colon {
            return Err(self.unexpected(&tok));
        }
        self.push(1)
    }

    /// Reduces a container (`array`/`object`, net `pop` states after the end
    /// token was shifted) and leaves its nesting level.
    fn close(&mut self, pop: usize) {
        self.stack_size -= pop;
        self.depth -= 1;
    }

    /// `start: value PHP_JSON_T_EOI`.
    fn finish(&mut self, value: S::Value) -> Result<S::Value, ParseError> {
        let tok = self.scan();
        if tok != Token::Eoi {
            return Err(self.unexpected(&tok));
        }
        self.push(1)?;
        Ok(value)
    }

    /// Reduces `element` with `value`, then reads what follows it.
    fn array_element(
        &mut self,
        arr: &mut Option<S::Array>,
        value: S::Value,
    ) -> Result<Attached, ParseError> {
        if let Some(a) = arr {
            // element: element ',' value
            self.stack_size -= 2;
            self.sink.array_append(a, value);
        } else {
            // element: value
            let mut a = self.sink.array_create();
            self.sink.array_append(&mut a, value);
            *arr = Some(a);
        }
        match self.scan() {
            Token::Comma => {
                self.push(1)?;
                Ok(Attached::Next(self.scan()))
            }
            tok @ (Token::RBracket | Token::RBrace) => {
                // elements: element; shift array_end.
                self.push(1)?;
                if tok == Token::RBrace {
                    return Err(self.error(ErrorCode::StateMismatch));
                }
                self.close(3);
                Ok(Attached::Closed)
            }
            other => Err(self.unexpected(&other)),
        }
    }

    /// Reduces `member` with `value` (creating the object on the first
    /// member), then reads what follows it.
    fn object_member(
        &mut self,
        obj: &mut Option<S::Object>,
        key: &mut ZStr,
        value: S::Value,
    ) -> Result<Attached, ParseError> {
        let o = if let Some(o) = obj {
            // member: member ',' key ':' value
            self.stack_size -= 4;
            o
        } else {
            // member: key ':' value
            self.stack_size -= 2;
            let o = self
                .sink
                .object_create()
                .map_err(|e| self.action_error(e))?;
            obj.insert(o)
        };
        if let Err(e) = self.sink.object_update(o, std::mem::take(key), value) {
            return Err(self.action_error(e));
        }
        match self.scan() {
            Token::Comma => {
                self.push(1)?;
                let tok = self.scan();
                let Token::Str(k) = tok else {
                    return Err(self.unexpected(&tok));
                };
                self.push(1)?;
                self.colon()?;
                *key = ZStr::from(k);
                Ok(Attached::Next(self.scan()))
            }
            tok @ (Token::RBrace | Token::RBracket) => {
                // members: member; shift object_end.
                self.push(1)?;
                if tok == Token::RBracket {
                    return Err(self.error(ErrorCode::StateMismatch));
                }
                self.close(3);
                Ok(Attached::Closed)
            }
            other => Err(self.unexpected(&other)),
        }
    }
}

/// Outcome of [`Parser::start_value`].
enum Started<V> {
    /// A complete value.
    Value(V),
    /// A container was opened; the lookahead of its first value.
    Opened(Token),
}

/// Outcome of attaching a value to the innermost container.
enum Attached {
    /// A `,` followed: the lookahead of the next value.
    Next(Token),
    /// The container was closed and must be popped.
    Closed,
}

/// The validation sink (`validate_parser_methods`): builds nothing.
#[derive(Debug, Default)]
pub(crate) struct ValidateSink;

impl Sink for ValidateSink {
    type Value = ();
    type Array = ();
    type Object = ();

    fn scalar(&mut self, _: Scalar) {}
    fn empty_array(&mut self) {}
    fn array_create(&mut self) {}
    fn array_append(&mut self, (): &mut (), (): ()) {}
    fn array_finish(&mut self, (): ()) {}
    fn empty_object(&mut self) -> Result<(), ActionError> {
        Ok(())
    }
    fn object_create(&mut self) -> Result<(), ActionError> {
        Ok(())
    }
    fn object_update(&mut self, (): &mut (), _: ZStr, (): ()) -> Result<(), ActionError> {
        Ok(())
    }
    fn object_finish(&mut self, (): ()) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str, depth: i64) -> Result<(), JsonError> {
        Parser::new(input.as_bytes(), 0, depth, ValidateSink, true)
            .parse()
            .map_err(|e| match e {
                ParseError::Json(j) => j,
                ParseError::Exception(_) => unreachable!(),
            })
    }

    fn err(input: &str) -> (i64, u64, u64) {
        let e = check(input, 512).unwrap_err();
        (e.code(), e.line, e.column)
    }

    #[test]
    fn valid_documents() {
        for s in [
            "1",
            " \"x\" ",
            "[]",
            "{}",
            "[1,[2,[3]],{\"a\":{\"b\":[]}}]",
            "{\"a\":1,\"b\":2,\"a\":3}",
            "\n[\r\n1\t]\n",
            "-0.5e-3",
        ] {
            assert_eq!(check(s, 512), Ok(()), "{s}");
        }
    }

    #[test]
    fn error_codes_and_locations() {
        assert_eq!(err("-"), (4, 1, 1));
        assert_eq!(err("{\"name\": \"value}"), (4, 1, 10));
        assert_eq!(err("{\"name\" \"value\"}"), (4, 1, 9));
        assert_eq!(err("{\"name\": \"val\",}"), (4, 1, 16));
        assert_eq!(err("[1, 2, 3, 4, 5"), (4, 1, 15));
        assert_eq!(err("[1, 2,]"), (4, 1, 7));
        assert_eq!(err("[1,, 2]"), (4, 1, 4));
        assert_eq!(err("{\"test\": \"\n\"}"), (3, 1, 10));
        assert_eq!(err("[1}"), (2, 1, 3));
        assert_eq!(err("{\"a\":1]"), (2, 1, 7));
        assert_eq!(err("[}"), (2, 1, 2));
        assert_eq!(err("{]"), (2, 1, 2));
        assert_eq!(err("{132"), (4, 1, 2));
        assert_eq!(err("\n{\"\\u00FF\"_"), (4, 2, 10));
        assert_eq!(err("\n{\"\u{ff}\"_"), (4, 2, 5));
        assert_eq!(err("\""), (4, 1, 1));
        assert_eq!(err("{\"123\": \""), (4, 1, 9));
        assert_eq!(err("\"\0"), (3, 1, 1));
        assert_eq!(err("[1] x"), (4, 1, 5));
        assert_eq!(err("[1]\0"), (3, 1, 4));
    }

    #[test]
    fn depth() {
        assert_eq!(check("1", 1), Ok(()));
        assert_eq!(check("[]", 1).unwrap_err().code(), 1);
        assert_eq!(check("[[1]]", 2).unwrap_err().code(), 1);
        assert_eq!(check("[[1]]", 3), Ok(()));
        let e = check("[[[1]]]", 2).unwrap_err();
        assert_eq!((e.code(), e.column), (1, 2));
        // The depth check runs before the next token is scanned.
        assert_eq!(check("[\u{1}", 1).unwrap_err().code(), 1);
        assert_eq!(check("[[[[", 0).unwrap_err().code(), 4);
    }

    #[test]
    fn bison_stack_limit() {
        let arrays = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert_eq!(check(&arrays(4998), 100_000), Ok(()));
        assert_eq!(check(&arrays(4999), 100_000).unwrap_err().code(), 4);
        let objects = |n: usize| format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n));
        assert_eq!(check(&objects(2499), 100_000), Ok(()));
        assert_eq!(check(&objects(2500), 100_000).unwrap_err().code(), 4);
        let objects2 = |n: usize| format!("{}1{}", "{\"b\":1,\"a\":".repeat(n), "}".repeat(n));
        assert_eq!(check(&objects2(1666), 100_000), Ok(()));
        assert_eq!(check(&objects2(1667), 100_000).unwrap_err().code(), 4);
    }
}
