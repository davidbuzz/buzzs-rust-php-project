// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The JSON tokenizer: a hand-written port of the re2c scanner in
//! `ext/json/json_scanner.re`, including its exact error classification.
//!
//! * Whitespace is `[ \t\r\n]`; every `\n` starts a new line (for error
//!   locations).
//! * Numbers follow the scanner grammar exactly (`-?(0|[1-9][0-9]*)`,
//!   optional `.DIGITS`, optional exponent); integers with too many digits
//!   become floats (or strings with `JSON_BIGINT_AS_STRING`).
//! * A NUL byte before the end of input is a control-character error; the end
//!   of input is the `EOI` token.
//! * Outside strings, any other valid UTF-8 character is a syntax error and an
//!   invalid byte is a UTF-8 error.
//! * Strings: control characters are errors, escapes are validated
//!   (`\uXXXX` lone surrogates are UTF-16 errors), invalid UTF-8 is an error
//!   unless `JSON_INVALID_UTF8_IGNORE`/`SUBSTITUTE` is set, in which case each
//!   invalid byte is dropped or replaced by U+FFFD.

use crate::error::ErrorCode;
use crate::flags::{BIGINT_AS_STRING, INVALID_UTF8_IGNORE, INVALID_UTF8_SUBSTITUTE};

/// A token produced by the scanner.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `null`
    Null,
    /// `true`
    True,
    /// `false`
    False,
    /// An integer that fits in `i64`.
    Int(i64),
    /// A float (or a big integer without `JSON_BIGINT_AS_STRING`).
    Double(f64),
    /// A string (or a big integer with `JSON_BIGINT_AS_STRING`). Empty when
    /// the scanner runs in validation mode.
    Str(Vec<u8>),
    /// A valid string whose value is `len` bytes long, not built yet: the
    /// parser turns it into [`Token::Str`] with [`Scanner::string_value`]
    /// once the memory for it is reserved (php-src's scanner allocates the
    /// string with `zend_string_falloc` between its two passes).
    StrLen(usize),
    /// End of input (`PHP_JSON_T_EOI`).
    Eoi,
    /// A scanner error (`PHP_JSON_T_ERROR` with `errcode`).
    Error(ErrorCode),
}

/// `MAX_LENGTH_OF_LONG - 1` on 64-bit platforms.
const INT_MAX_LENGTH: usize = 19;
/// The digits of `-i64::MIN`.
const LONG_MIN_DIGITS: &[u8] = b"9223372036854775808";

/// The scanner state.
#[derive(Debug)]
pub(crate) struct Scanner<'a> {
    input: &'a [u8],
    cursor: usize,
    /// Start of the current (last scanned) token.
    token: usize,
    line: u64,
    line_start: usize,
    options: i64,
    /// Whether string contents are discarded (`json_validate`).
    validate: bool,
    /// The string last returned as [`Token::StrLen`].
    pending: PendingStr,
}

/// Where the string of a [`Token::StrLen`] is: `input[start..end]` between
/// the quotes, and whether its value differs from those bytes.
#[derive(Debug, Clone, Copy, Default)]
struct PendingStr {
    start: usize,
    end: usize,
    copy: bool,
}

/// The output of a pass over a string body: a byte count (the first pass,
/// `STR_P1`) or the value itself (the second, `STR_P2`).
trait StrOut {
    /// Appends literal bytes.
    fn bytes(&mut self, b: &[u8]);
    /// Appends one byte.
    fn byte(&mut self, b: u8);
    /// Appends the UTF-8 encoding of a code point.
    fn code_point(&mut self, cp: u32);
}

impl StrOut for usize {
    #[inline]
    fn bytes(&mut self, b: &[u8]) {
        *self += b.len();
    }
    #[inline]
    fn byte(&mut self, _: u8) {
        *self += 1;
    }
    #[inline]
    fn code_point(&mut self, cp: u32) {
        *self += match cp {
            0..0x80 => 1,
            0x80..0x800 => 2,
            0x800..0x10000 => 3,
            _ => 4,
        };
    }
}

impl StrOut for Vec<u8> {
    #[inline]
    fn bytes(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }
    #[inline]
    fn byte(&mut self, b: u8) {
        self.push(b);
    }
    #[inline]
    fn code_point(&mut self, cp: u32) {
        push_utf8(self, cp);
    }
}

#[inline]
fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

#[inline]
fn hex_val(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'a'..=b'f' => u32::from(c - b'a' + 10),
        b'A'..=b'F' => u32::from(c - b'A' + 10),
        _ => 0,
    }
}

#[inline]
fn is_utf8_tail(c: u8) -> bool {
    (0x80..=0xBF).contains(&c)
}

/// Length of the valid UTF-8 sequence starting at `s[0]` according to the
/// scanner's `UTF8` definition (`UTF8_1` … `UTF8_4`), or `None` if the bytes
/// do not form one.
fn utf8_sequence_len(s: &[u8]) -> Option<usize> {
    let c = *s.first()?;
    let t = |i: usize, lo: u8, hi: u8| s.get(i).is_some_and(|b| (lo..=hi).contains(b));
    let tail = |i: usize| s.get(i).copied().is_some_and(is_utf8_tail);
    match c {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => tail(1).then_some(2),
        0xE0 => (t(1, 0xA0, 0xBF) && tail(2)).then_some(3),
        0xE1..=0xEC | 0xEE..=0xEF => (tail(1) && tail(2)).then_some(3),
        0xED => (t(1, 0x80, 0x9F) && tail(2)).then_some(3),
        0xF0 => (t(1, 0x90, 0xBF) && tail(2) && tail(3)).then_some(4),
        0xF1..=0xF3 => (tail(1) && tail(2) && tail(3)).then_some(4),
        0xF4 => (t(1, 0x80, 0x8F) && tail(2) && tail(3)).then_some(4),
        _ => None,
    }
}

/// Appends the UTF-8 encoding of a code point (no validation).
#[allow(clippy::cast_possible_truncation)]
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push((0xC0 | (cp >> 6)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else if cp < 0x10000 {
        out.push((0xE0 | (cp >> 12)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else {
        out.push((0xF0 | (cp >> 18)) as u8);
        out.push((0x80 | ((cp >> 12) & 0x3F)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    }
}

impl<'a> Scanner<'a> {
    /// `php_json_scanner_init`.
    pub(crate) fn new(input: &'a [u8], options: i64, validate: bool) -> Self {
        Scanner {
            input,
            cursor: 0,
            token: 0,
            line: 1,
            line_start: 0,
            options,
            validate,
            pending: PendingStr::default(),
        }
    }

    /// The current line (1-based).
    pub(crate) fn line(&self) -> u64 {
        self.line
    }

    /// `php_json_compute_error_column`: the 1-based column of the last token,
    /// counting UTF-8 characters (continuation bytes are folded).
    pub(crate) fn column(&self) -> u64 {
        let start = self.line_start.min(self.token);
        let n = self.input[start..self.token]
            .iter()
            .filter(|&&b| b & 0xC0 != 0x80)
            .count();
        1 + n as u64
    }

    #[inline]
    fn peek(&self, off: usize) -> Option<u8> {
        self.input.get(self.cursor + off).copied()
    }

    /// `php_json_scan`: returns the next token.
    pub(crate) fn scan(&mut self) -> Token {
        loop {
            self.token = self.cursor;
            let Some(c) = self.peek(0) else {
                // The terminating NUL of the zend_string, read past the limit.
                return Token::Eoi;
            };
            match c {
                b' ' | b'\t' | b'\r' => {
                    self.cursor += 1;
                }
                b'\n' => {
                    self.cursor += 1;
                    self.line += 1;
                    self.line_start = self.cursor;
                }
                b'{' => return self.single(Token::LBrace),
                b'}' => return self.single(Token::RBrace),
                b'[' => return self.single(Token::LBracket),
                b']' => return self.single(Token::RBracket),
                b':' => return self.single(Token::Colon),
                b',' => return self.single(Token::Comma),
                b'n' if self.input[self.cursor..].starts_with(b"null") => {
                    self.cursor += 4;
                    return Token::Null;
                }
                b't' if self.input[self.cursor..].starts_with(b"true") => {
                    self.cursor += 4;
                    return Token::True;
                }
                b'f' if self.input[self.cursor..].starts_with(b"false") => {
                    self.cursor += 5;
                    return Token::False;
                }
                b'-' | b'0'..=b'9' => {
                    if let Some(tok) = self.number() {
                        return tok;
                    }
                    // A lone '-' is matched by the UTF8 rule.
                    self.cursor += 1;
                    return Token::Error(ErrorCode::Syntax);
                }
                b'"' => {
                    self.cursor += 1;
                    return self.string();
                }
                0x00..=0x1F => {
                    // A NUL before the end of input or another control char.
                    self.cursor += 1;
                    return Token::Error(ErrorCode::CtrlChar);
                }
                _ => {
                    // Any other valid UTF-8 character is a syntax error, an
                    // invalid byte a UTF-8 error.
                    return if let Some(n) = utf8_sequence_len(&self.input[self.cursor..]) {
                        self.cursor += n;
                        Token::Error(ErrorCode::Syntax)
                    } else {
                        self.cursor += 1;
                        Token::Error(ErrorCode::Utf8)
                    };
                }
            }
        }
    }

    fn single(&mut self, tok: Token) -> Token {
        self.cursor += 1;
        tok
    }

    /// Scans `INT`, `FLOAT` or `EXP` at the cursor. Returns `None` (cursor
    /// untouched) if no number matches (a lone `-`).
    fn number(&mut self) -> Option<Token> {
        let start = self.cursor;
        let s = self.input;
        let mut p = start;
        let negative = s[p] == b'-';
        if negative {
            p += 1;
        }
        match s.get(p) {
            Some(b'0') => p += 1,
            Some(b'1'..=b'9') => {
                while s.get(p).is_some_and(u8::is_ascii_digit) {
                    p += 1;
                }
            }
            _ => return None,
        }
        let mut is_float = false;
        if s.get(p) == Some(&b'.') && s.get(p + 1).is_some_and(u8::is_ascii_digit) {
            p += 2;
            while s.get(p).is_some_and(u8::is_ascii_digit) {
                p += 1;
            }
            is_float = true;
        }
        if matches!(s.get(p), Some(b'e' | b'E')) {
            let mut q = p + 1;
            if matches!(s.get(q), Some(b'+' | b'-')) {
                q += 1;
            }
            if s.get(q).is_some_and(u8::is_ascii_digit) {
                while s.get(q).is_some_and(u8::is_ascii_digit) {
                    q += 1;
                }
                p = q;
                is_float = true;
            }
        }
        self.cursor = p;
        let text = &s[start..p];
        if is_float {
            return Some(Token::Double(parse_double(text)));
        }
        let digits = &text[usize::from(negative)..];
        let bigint = match digits.len().cmp(&INT_MAX_LENGTH) {
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => {
                let cmp = digits.cmp(LONG_MIN_DIGITS);
                !(cmp.is_lt() || (cmp.is_eq() && negative))
            }
            std::cmp::Ordering::Greater => true,
        };
        if !bigint {
            let mut v: i64 = 0;
            for &d in digits {
                v = v * 10 - i64::from(d - b'0');
            }
            // Accumulated negatively so that i64::MIN does not overflow.
            return Some(Token::Int(if negative { v } else { -v }));
        }
        if self.options & BIGINT_AS_STRING != 0 {
            if self.validate {
                return Some(Token::Str(Vec::new()));
            }
            // `ZVAL_STRINGL`: a copy of the digits, reserved like a string.
            self.pending = PendingStr {
                start,
                end: p,
                copy: false,
            };
            Some(Token::StrLen(text.len()))
        } else {
            Some(Token::Double(parse_double(text)))
        }
    }

    /// Scans a string body (the cursor is after the opening quote): the
    /// first pass of php-src's scanner (`STR_P1`), which validates the
    /// string and computes the exact length of its value.
    ///
    /// Returns [`Token::StrLen`]: the parser reserves that many bytes
    /// against `memory_limit` and then calls [`Scanner::string_value`],
    /// which builds the value in one exactly sized allocation (`STR_P2`).
    /// In validation mode no value is built and the token is an empty
    /// [`Token::Str`].
    fn string(&mut self) -> Token {
        let start = self.cursor;
        let mut len = 0usize;
        match self.string_body(&mut len) {
            Ok(copy) => {
                if self.validate {
                    return Token::Str(Vec::new());
                }
                self.pending = PendingStr {
                    start,
                    end: self.cursor - 1,
                    copy,
                };
                Token::StrLen(len)
            }
            Err(code) => Token::Error(code),
        }
    }

    /// The value of the string [`Scanner::string`] just measured as
    /// `len` bytes (`STR_P2`): a copy of the input when it holds neither
    /// escapes nor invalid UTF-8, otherwise the second pass over it.
    pub(crate) fn string_value(&mut self, len: usize) -> Vec<u8> {
        let PendingStr { start, end, copy } = self.pending;
        if !copy {
            return self.input[start..end].to_vec();
        }
        let mut out = Vec::with_capacity(len);
        let resume = self.cursor;
        self.cursor = start;
        let second = self.string_body(&mut out);
        debug_assert!(second.is_ok() && self.cursor == resume && out.len() == len);
        self.cursor = resume;
        out
    }

    /// One pass over a string body, from the cursor to just after the
    /// closing quote, writing the value to `out`. Returns whether the value
    /// differs from the input bytes (escapes or replaced/dropped invalid
    /// UTF-8: php-src's `str_esc || utf8_invalid`), or the error.
    fn string_body<O: StrOut>(&mut self, out: &mut O) -> Result<bool, ErrorCode> {
        let s = self.input;
        let ignore_or_subst = self.options & (INVALID_UTF8_IGNORE | INVALID_UTF8_SUBSTITUTE) != 0;
        let substitute = self.options & INVALID_UTF8_SUBSTITUTE != 0;
        let mut copy = false;
        // Start of the pending run of literal bytes.
        let mut run = self.cursor;
        loop {
            let p = self.cursor;
            let Some(c) = s.get(p).copied() else {
                // EOI inside a string: unterminated.
                return Err(ErrorCode::Syntax);
            };
            match c {
                b'"' => {
                    out.bytes(&s[run..p]);
                    self.cursor = p + 1;
                    return Ok(copy);
                }
                0x00..=0x1F => {
                    self.cursor = p + 1;
                    return Err(ErrorCode::CtrlChar);
                }
                b'\\' => {
                    out.bytes(&s[run..p]);
                    copy = true;
                    match self.escape(p, out) {
                        Ok(next) => {
                            self.cursor = next;
                            run = next;
                        }
                        Err((code, next)) => {
                            self.cursor = next;
                            return Err(code);
                        }
                    }
                }
                0x20..=0x7F => self.cursor = p + 1,
                _ => {
                    if let Some(n) = utf8_sequence_len(&s[p..]) {
                        self.cursor = p + n;
                    } else if ignore_or_subst {
                        out.bytes(&s[run..p]);
                        if substitute {
                            out.bytes("\u{FFFD}".as_bytes());
                        }
                        copy = true;
                        self.cursor = p + 1;
                        run = self.cursor;
                    } else {
                        self.cursor = p + 1;
                        return Err(ErrorCode::Utf8);
                    }
                }
            }
        }
    }

    /// Handles an escape sequence starting with the backslash at `p`.
    /// Returns the position after it, or the error and the cursor position.
    fn escape<O: StrOut>(&self, p: usize, out: &mut O) -> Result<usize, (ErrorCode, usize)> {
        let s = self.input;
        let simple = match s.get(p + 1) {
            Some(b'"') => Some(b'"'),
            Some(b'\\') => Some(b'\\'),
            Some(b'/') => Some(b'/'),
            Some(b'b') => Some(0x08),
            Some(b'f') => Some(0x0C),
            Some(b'n') => Some(b'\n'),
            Some(b'r') => Some(b'\r'),
            Some(b't') => Some(b'\t'),
            _ => None,
        };
        if let Some(b) = simple {
            out.byte(b);
            return Ok(p + 2);
        }
        let ucs2 = |at: usize| -> Option<u32> {
            if s.get(at) == Some(&b'\\') && s.get(at + 1) == Some(&b'u') {
                let h = s.get(at + 2..at + 6)?;
                if h.iter().all(|&c| is_hex(c)) {
                    return Some(h.iter().fold(0, |acc, &c| (acc << 4) | hex_val(c)));
                }
            }
            None
        };
        let Some(cp) = ucs2(p) else {
            // ESCPREF alone.
            return Err((ErrorCode::Syntax, p + 1));
        };
        if (0xD800..0xDC00).contains(&cp) {
            if let Some(lo) = ucs2(p + 6).filter(|lo| (0xDC00..0xE000).contains(lo)) {
                out.code_point(((cp & 0x3FF) << 10) + (lo & 0x3FF) + 0x10000);
                return Ok(p + 12);
            }
            return Err((ErrorCode::Utf16, p + 6));
        }
        if (0xDC00..0xE000).contains(&cp) {
            return Err((ErrorCode::Utf16, p + 6));
        }
        out.code_point(cp);
        Ok(p + 6)
    }
}

/// `zend_strtod` on a token that matches the JSON number grammar.
fn parse_double(text: &[u8]) -> f64 {
    std::str::from_utf8(text)
        .ok()
        .and_then(|t| t.parse::<f64>().ok())
        .unwrap_or(0.0)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn scan_all(input: &[u8], options: i64) -> Vec<Token> {
        let mut s = Scanner::new(input, options, false);
        let mut out = Vec::new();
        loop {
            let t = match s.scan() {
                Token::StrLen(len) => Token::Str(s.string_value(len)),
                t => t,
            };
            let stop = matches!(t, Token::Eoi | Token::Error(_));
            out.push(t);
            if stop {
                return out;
            }
        }
    }

    fn one(input: &[u8]) -> Token {
        scan_all(input, 0).remove(0)
    }

    #[test]
    fn punctuation_and_keywords() {
        assert_eq!(
            scan_all(b" {}[]:, null true false\r\n\t", 0),
            vec![
                Token::LBrace,
                Token::RBrace,
                Token::LBracket,
                Token::RBracket,
                Token::Colon,
                Token::Comma,
                Token::Null,
                Token::True,
                Token::False,
                Token::Eoi
            ]
        );
        assert_eq!(one(b"nul"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"True"), Token::Error(ErrorCode::Syntax));
    }

    #[test]
    fn numbers() {
        assert_eq!(one(b"0"), Token::Int(0));
        assert_eq!(one(b"-0"), Token::Int(0));
        assert_eq!(one(b"-0.0"), Token::Double(-0.0));
        assert!(matches!(one(b"-0.0"), Token::Double(d) if d.is_sign_negative()));
        assert_eq!(one(b"123"), Token::Int(123));
        assert_eq!(one(b"1.5e3"), Token::Double(1500.0));
        assert_eq!(one(b"1E-2"), Token::Double(0.01));
        assert_eq!(one(b"9223372036854775807"), Token::Int(i64::MAX));
        assert_eq!(one(b"-9223372036854775808"), Token::Int(i64::MIN));
        assert_eq!(
            one(b"9223372036854775808"),
            Token::Double(9_223_372_036_854_775_808.0)
        );
        assert_eq!(
            scan_all(b"-9223372036854775809", BIGINT_AS_STRING)[0],
            Token::Str(b"-9223372036854775809".to_vec())
        );
        assert_eq!(
            scan_all(b"01", 0),
            vec![Token::Int(0), Token::Int(1), Token::Eoi]
        );
        assert_eq!(
            scan_all(b"1.e5", 0),
            vec![Token::Int(1), Token::Error(ErrorCode::Syntax)]
        );
        assert_eq!(
            scan_all(b"1e+", 0),
            vec![Token::Int(1), Token::Error(ErrorCode::Syntax)]
        );
        assert_eq!(one(b"-"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"+1"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b".5"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"1e400"), Token::Double(f64::INFINITY));
    }

    #[test]
    fn strings() {
        assert_eq!(one(br#""abc""#), Token::Str(b"abc".to_vec()));
        assert_eq!(one(br#""""#), Token::Str(Vec::new()));
        assert_eq!(
            one(br#""\"\\\/\b\f\n\r\t""#),
            Token::Str(b"\"\\/\x08\x0c\n\r\t".to_vec())
        );
        assert_eq!(
            one(br#""\u0041\u00e9\u20AC\ud83d\ude00""#),
            Token::Str("Aé€😀".as_bytes().to_vec())
        );
        assert_eq!(one(br#""\u0000""#), Token::Str(vec![0]));
        assert_eq!(one(br#""\ud83d""#), Token::Error(ErrorCode::Utf16));
        assert_eq!(one(br#""\ude00""#), Token::Error(ErrorCode::Utf16));
        assert_eq!(one(br#""\ud83d\u0041""#), Token::Error(ErrorCode::Utf16));
        assert_eq!(one(br#""\u12""#), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(br#""\x""#), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"\"abc"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"\"a\nb\""), Token::Error(ErrorCode::CtrlChar));
        assert_eq!(one(b"\"a\x00b\""), Token::Error(ErrorCode::CtrlChar));
        assert_eq!(one(b"\"\xff\""), Token::Error(ErrorCode::Utf8));
        assert_eq!(
            scan_all(b"\"a\xffb\xe0\x80\"", INVALID_UTF8_IGNORE)[0],
            Token::Str(b"ab".to_vec())
        );
        assert_eq!(
            scan_all(b"\"a\xffb\xe0\x80\"", INVALID_UTF8_SUBSTITUTE)[0],
            Token::Str("a\u{FFFD}b\u{FFFD}\u{FFFD}".as_bytes().to_vec())
        );
        assert_eq!(
            scan_all(b"\"\xc3\xa9\xf0\x9f\x98\x80\"", 0)[0],
            Token::Str("é😀".as_bytes().to_vec())
        );
    }

    #[test]
    fn outside_strings() {
        assert_eq!(one(b"\x00"), Token::Error(ErrorCode::CtrlChar));
        assert_eq!(one(b"\x01"), Token::Error(ErrorCode::CtrlChar));
        assert_eq!(one(b"\x7f"), Token::Error(ErrorCode::Syntax));
        assert_eq!(one("é".as_bytes()), Token::Error(ErrorCode::Syntax));
        assert_eq!(one(b"\xff"), Token::Error(ErrorCode::Utf8));
        assert_eq!(one(b""), Token::Eoi);
    }

    #[test]
    fn locations() {
        let mut s = Scanner::new(b"\n  [\r\n  \"\xc3\xa9\" x", 0, false);
        assert_eq!(s.scan(), Token::LBracket);
        assert_eq!((s.line(), s.column()), (2, 3));
        assert_eq!(s.scan(), Token::StrLen(2));
        assert_eq!((s.line(), s.column()), (3, 3));
        assert_eq!(s.scan(), Token::Error(ErrorCode::Syntax));
        assert_eq!((s.line(), s.column()), (3, 7));
    }

    /// The first pass measures exactly the value the second builds, so the
    /// reservation and the allocation are one exactly sized block.
    #[test]
    fn string_length_is_exact() {
        let cases: &[(&[u8], i64)] = &[
            (br#""plain""#, 0),
            (br#""a\"\\\/\b\f\n\r\tz""#, 0),
            (br#""\u0041\u00e9\u20ac\ud83d\ude00""#, 0),
            ("\"\u{e9}\u{1F600}\"".as_bytes(), 0),
            (b"\"a\xffb\xe0\x80c\"", INVALID_UTF8_IGNORE),
            (b"\"a\xffb\xe0\x80c\"", INVALID_UTF8_SUBSTITUTE),
            (b"\"\x80\x80\x80\"", INVALID_UTF8_SUBSTITUTE),
            (b"\"\x80\\n\x80\"", INVALID_UTF8_SUBSTITUTE),
        ];
        for &(input, options) in cases {
            let mut s = Scanner::new(input, options, false);
            let Token::StrLen(len) = s.scan() else {
                panic!("not a string: {input:?}");
            };
            let value = s.string_value(len);
            assert_eq!(value.len(), len, "{input:?}");
            assert_eq!(value.capacity(), len, "{input:?}");
            // The cursor is after the closing quote either way.
            assert_eq!(s.scan(), Token::Eoi, "{input:?}");
        }
        let mut s = Scanner::new(b"\"\x80\x80\"", INVALID_UTF8_SUBSTITUTE, false);
        assert_eq!(s.scan(), Token::StrLen(6));
        let mut s = Scanner::new(b"\"\x80\x80\"", INVALID_UTF8_IGNORE, false);
        assert_eq!(s.scan(), Token::StrLen(0));
        let mut s = Scanner::new(br#""\ud83d\ude00\u20ac\n""#, 0, false);
        assert_eq!(s.scan(), Token::StrLen(4 + 3 + 1));
        // Validation builds nothing.
        let mut s = Scanner::new(br#""a\nb""#, 0, true);
        assert_eq!(s.scan(), Token::Str(Vec::new()));
    }

    #[test]
    fn utf8_table() {
        assert_eq!(utf8_sequence_len(b"\xc2\x80"), Some(2));
        assert_eq!(utf8_sequence_len(b"\xc1\x80"), None);
        assert_eq!(utf8_sequence_len(b"\xe0\x9f\x80"), None);
        assert_eq!(utf8_sequence_len(b"\xed\xa0\x80"), None);
        assert_eq!(utf8_sequence_len(b"\xf4\x90\x80\x80"), None);
        assert_eq!(utf8_sequence_len(b"\xf4\x8f\xbf\xbf"), Some(4));
        assert_eq!(utf8_sequence_len(b"\xf5"), None);
    }
}
