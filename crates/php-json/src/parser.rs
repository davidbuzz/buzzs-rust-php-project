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
    pub(crate) fn new(input: &'a [u8], options: i64, max_depth: i64, sink: S, validate: bool) -> Self {
        Parser {
            scanner: Scanner::new(input, options, validate),
            sink,
            depth: 1,
            max_depth,
            stack_size: 1,
        }
    }

    fn error(&self, code: ErrorCode) -> ParseError {
        ParseError::Json(JsonError::at(code, self.scanner.line(), self.scanner.column()))
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

    /// Runs `php_json_yyparse` and returns the parsed value.
    pub(crate) fn parse(mut self) -> Result<S::Value, ParseError> {
        let mut stack: Vec<Frame<S>> = Vec::new();
        let mut tok = self.scanner.scan();
        'value: loop {
            // `tok` is the lookahead at a position expecting a value.
            let mut value = match tok {
                Token::LBracket => {
                    self.push(1)?; // shift '['
                    self.depth_inc()?; // $@ action
                    self.push(1)?; // $@ goto
                    tok = self.scanner.scan();
                    if matches!(tok, Token::RBracket | Token::RBrace) {
                        // elements: %empty; shift array_end; reduce.
                        self.push(2)?;
                        if tok == Token::RBrace {
                            return Err(self.error(ErrorCode::StateMismatch));
                        }
                        self.stack_size -= 3;
                        self.depth -= 1;
                        self.sink.empty_array()
                    } else {
                        stack.push(Frame::Array(None));
                        continue 'value;
                    }
                }
                Token::LBrace => {
                    self.push(1)?;
                    self.depth_inc()?;
                    self.push(1)?;
                    tok = self.scanner.scan();
                    match tok {
                        Token::RBrace | Token::RBracket => {
                            // members: %empty (creates the object); shift.
                            self.push(1)?;
                            let v = self
                                .sink
                                .empty_object()
                                .map_err(|e| self.action_error(e))?;
                            self.push(1)?;
                            if tok == Token::RBracket {
                                return Err(self.error(ErrorCode::StateMismatch));
                            }
                            self.stack_size -= 3;
                            self.depth -= 1;
                            v
                        }
                        Token::Str(key) => {
                            self.push(1)?; // shift key (reduced in place)
                            let key = ZStr::from(key);
                            tok = self.scanner.scan();
                            if tok != Token::Colon {
                                return Err(self.unexpected(&tok));
                            }
                            self.push(1)?;
                            stack.push(Frame::Object(None, key));
                            tok = self.scanner.scan();
                            continue 'value;
                        }
                        other => return Err(self.unexpected(&other)),
                    }
                }
                Token::Null => self.scalar(Scalar::Null)?,
                Token::True => self.scalar(Scalar::Bool(true))?,
                Token::False => self.scalar(Scalar::Bool(false))?,
                Token::Int(i) => self.scalar(Scalar::Int(i))?,
                Token::Double(d) => self.scalar(Scalar::Double(d))?,
                Token::Str(s) => self.scalar(Scalar::Str(s))?,
                other => return Err(self.unexpected(&other)),
            };
            // A complete value has been reduced; attach it to its container.
            loop {
                match stack.last_mut() {
                    None => {
                        tok = self.scanner.scan();
                        if tok != Token::Eoi {
                            return Err(self.unexpected(&tok));
                        }
                        self.push(1)?;
                        return Ok(value);
                    }
                    Some(Frame::Array(arr)) => {
                        match arr {
                            None => {
                                // element: value
                                let mut a = self.sink.array_create();
                                self.sink.array_append(&mut a, value);
                                *arr = Some(a);
                            }
                            Some(a) => {
                                // element: element ',' value
                                self.stack_size -= 2;
                                self.sink.array_append(a, value);
                            }
                        }
                        tok = self.scanner.scan();
                        match tok {
                            Token::Comma => {
                                self.push(1)?;
                                tok = self.scanner.scan();
                                continue 'value;
                            }
                            Token::RBracket | Token::RBrace => {
                                // elements: element; shift array_end.
                                self.push(1)?;
                                if tok == Token::RBrace {
                                    return Err(self.error(ErrorCode::StateMismatch));
                                }
                                self.stack_size -= 3;
                                self.depth -= 1;
                                let Some(Frame::Array(Some(a))) = stack.pop() else {
                                    unreachable!("array frame without array");
                                };
                                value = self.sink.array_finish(a);
                            }
                            other => return Err(self.unexpected(&other)),
                        }
                    }
                    Some(Frame::Object(obj, key)) => {
                        let key = std::mem::take(key);
                        let first = obj.is_none();
                        if first {
                            // member: key ':' value
                            self.stack_size -= 2;
                            let o = self
                                .sink
                                .object_create()
                                .map_err(|e| self.action_error(e))?;
                            *obj = Some(o);
                        } else {
                            // member: member ',' key ':' value
                            self.stack_size -= 4;
                        }
                        let Some(o) = obj.as_mut() else {
                            unreachable!("object frame without object");
                        };
                        if let Err(e) = self.sink.object_update(o, key, value) {
                            return Err(self.action_error(e));
                        }
                        tok = self.scanner.scan();
                        match tok {
                            Token::Comma => {
                                self.push(1)?;
                                tok = self.scanner.scan();
                                let Token::Str(k) = tok else {
                                    return Err(self.unexpected(&tok));
                                };
                                self.push(1)?;
                                tok = self.scanner.scan();
                                if tok != Token::Colon {
                                    return Err(self.unexpected(&tok));
                                }
                                self.push(1)?;
                                if let Some(Frame::Object(_, key)) = stack.last_mut() {
                                    *key = ZStr::from(k);
                                }
                                tok = self.scanner.scan();
                                continue 'value;
                            }
                            Token::RBrace | Token::RBracket => {
                                // members: member; shift object_end.
                                self.push(1)?;
                                if tok == Token::RBracket {
                                    return Err(self.error(ErrorCode::StateMismatch));
                                }
                                self.stack_size -= 3;
                                self.depth -= 1;
                                let Some(Frame::Object(Some(o), _)) = stack.pop() else {
                                    unreachable!("object frame without object");
                                };
                                value = self.sink.object_finish(o);
                            }
                            other => return Err(self.unexpected(&other)),
                        }
                    }
                }
            }
        }
    }

    /// Shifts a scalar token and reduces it to a value.
    fn scalar(&mut self, s: Scalar) -> Result<S::Value, ParseError> {
        self.push(1)?;
        Ok(self.sink.scalar(s))
    }
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
        let objects2 =
            |n: usize| format!("{}1{}", "{\"b\":1,\"a\":".repeat(n), "}".repeat(n));
        assert_eq!(check(&objects2(1666), 100_000), Ok(()));
        assert_eq!(check(&objects2(1667), 100_000).unwrap_err().code(), 4);
    }
}
