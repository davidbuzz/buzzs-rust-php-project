// Derived from php-src Zend/zend_language_scanner.l. Similarity metric to original: 10%
//   Copyright © Zend Technologies Ltd., a subsidiary company of Perforce Software, Inc., and Contributors.
//   Authors: Marcus Boerger <helly@php.net>
//            Nuno Lopes <nlopess@php.net>
//            Scott MacVicar <scottmac@php.net>
//            Flex version authors:
//            Andi Gutmans <andi@php.net>
//            Zeev Suraski <zeev@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! Literal value processing: numeric literals, escape sequences and heredoc
//! indentation removal, following `Zend/zend_language_scanner.l`.

use crate::token::TokenValue;

/// A non-fatal diagnostic raised while scanning (PHP's `E_COMPILE_WARNING` /
/// `E_DEPRECATED` from the scanner).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexWarning {
    /// Severity of the diagnostic.
    pub kind: WarningKind,
    /// Message text, without the "Warning: " prefix.
    pub message: String,
    /// Line on which the diagnostic is reported.
    pub line: u32,
}

/// Severity of a [`LexWarning`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningKind {
    /// `E_COMPILE_WARNING`.
    CompileWarning,
    /// `E_DEPRECATED`.
    Deprecated,
}

/// Whether `c` may start a label (`[a-zA-Z_\x80-\xff]`).
pub const fn is_label_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

/// Whether `c` may continue a label (`[a-zA-Z0-9_\x80-\xff]`).
pub const fn is_label_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Counts newlines the way `HANDLE_NEWLINES` does: every `\n`, and every `\r`
/// that is not followed by `\n` (looking past the end of `range` into `src`).
pub fn count_newlines(src: &[u8], start: usize, end: usize) -> u32 {
    let s = &src[start..end];
    // Counting `\n` bytes is a branch-free (vectorisable) pass; only a
    // run with a `\r` needs the per-byte rule for a lone `\r`.
    let lf = s.iter().filter(|&&c| c == b'\n').count();
    let mut n = u32::try_from(lf).unwrap_or(u32::MAX);
    if s.contains(&b'\r') {
        for (k, &c) in s.iter().enumerate() {
            // A `\r` not followed by `\n` (looking past `end`, into the
            // rest of `src`) is a line break of its own.
            if c == b'\r' && src.get(start + k + 1) != Some(&b'\n') {
                n = n.saturating_add(1);
            }
        }
    }
    n
}

fn strip_underscores(digits: &[u8]) -> Vec<u8> {
    digits.iter().copied().filter(|&c| c != b'_').collect()
}

/// Overflowing prefixed literal to float, rounding step by step like
/// `zend_hex_strtod` / `zend_oct_strtod` / `zend_bin_strtod`
/// (`Zend/zend_strtod.c`). The octal and binary loops compute
/// `value * base + c - '0'`, i.e. add the ASCII code before subtracting
/// `'0'`, which rounds differently once `value` exceeds 2^53.
fn accumulate_float(digits: &[u8], radix: u32) -> f64 {
    let mut value = 0f64;
    for &c in digits {
        let Some(d) = char::from(c).to_digit(radix) else {
            break;
        };
        value = if radix == 16 {
            value * 16.0 + f64::from(d)
        } else {
            (value * f64::from(radix) + f64::from(c)) - f64::from(b'0')
        };
    }
    value
}

fn parse_i64_radix(digits: &[u8], radix: u32) -> Option<i64> {
    let mut value: i64 = 0;
    for &c in digits {
        let d = char::from(c).to_digit(radix)?;
        value = value
            .checked_mul(i64::from(radix))?
            .checked_add(i64::from(d))?;
    }
    Some(value)
}

/// Parses a decimal float literal (`DNUM` / `EXPONENT_DNUM`), underscores
/// allowed.
pub fn parse_float_literal(text: &[u8]) -> f64 {
    let s = strip_underscores(text);
    let s = String::from_utf8_lossy(&s);
    let mut s = s.into_owned();
    if s.starts_with('.') {
        s.insert(0, '0');
    }
    if let Some(pos) = s.find(['e', 'E']) {
        if s[..pos].ends_with('.') {
            s.insert(pos, '0');
        }
    } else if s.ends_with('.') {
        s.push('0');
    }
    s.parse::<f64>().unwrap_or(0.0)
}

/// Result of scanning a decimal integer literal (`LNUM`).
#[derive(Clone, Debug, PartialEq)]
pub struct LnumResult {
    /// The value (an integer or, on overflow, a float).
    pub value: TokenValue,
    /// Whether the literal was an invalid octal literal (contains 8 or 9).
    pub invalid_octal: bool,
}

/// Parses a decimal (or legacy octal) integer literal, overflowing to float
/// like the Zend scanner.
pub fn parse_lnum(text: &[u8]) -> LnumResult {
    let is_octal = text.first() == Some(&b'0');
    let mut digits = strip_underscores(text);
    let mut invalid_octal = false;
    if is_octal {
        if let Some(i) = digits.iter().position(|&c| c == b'8' || c == b'9') {
            invalid_octal = true;
            digits.truncate(i);
        }
    }
    let radix = if is_octal { 8 } else { 10 };
    let value = match parse_i64_radix(&digits, radix) {
        Some(v) => TokenValue::Int(v),
        None => {
            if is_octal {
                // zend_oct_strtod
                TokenValue::Float(accumulate_float(&digits, 8))
            } else {
                TokenValue::Float(parse_float_literal(&digits))
            }
        }
    };
    LnumResult {
        value,
        invalid_octal,
    }
}

/// Parses a prefixed integer literal (`0x`, `0o` or `0b`, prefix included),
/// overflowing to float like the Zend scanner.
pub fn parse_prefixed_int(text: &[u8], radix: u32) -> TokenValue {
    let body = &text[2..];
    let start = body
        .iter()
        .position(|&c| c != b'0' && c != b'_')
        .unwrap_or(body.len());
    let digits = strip_underscores(&body[start..]);
    if digits.is_empty() {
        return TokenValue::Int(0);
    }
    match radix {
        16 => {
            if digits.len() < 16 || (digits.len() == 16 && digits[0] <= b'7') {
                TokenValue::Int(parse_i64_radix(&digits, 16).unwrap_or(0))
            } else {
                TokenValue::Float(accumulate_float(&digits, 16))
            }
        }
        2 => {
            if digits.len() < 64 {
                TokenValue::Int(parse_i64_radix(&digits, 2).unwrap_or(0))
            } else {
                TokenValue::Float(accumulate_float(&digits, 2))
            }
        }
        _ => match parse_i64_radix(&digits, radix) {
            Some(v) => TokenValue::Int(v),
            None => TokenValue::Float(accumulate_float(&digits, radix)),
        },
    }
}

/// Processes the contents of a single-quoted string: only `\\` and `\'` are
/// escapes.
pub fn unescape_single_quoted(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    // Copy each run up to the next backslash in one piece.
    while let Some(k) = body[i..].iter().position(|&c| c == b'\\') {
        let bs = i + k;
        out.extend_from_slice(&body[i..bs]);
        match body.get(bs + 1) {
            Some(&n) => {
                if n != b'\\' && n != b'\'' {
                    out.push(b'\\');
                }
                out.push(n);
                i = bs + 2;
            }
            None => {
                // A trailing lone backslash is kept.
                out.push(b'\\');
                return out;
            }
        }
    }
    out.extend_from_slice(&body[i..]);
    out
}

/// Error from [`unescape_double_quoted`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeError {
    /// PHP's error message.
    pub message: &'static str,
    /// Number of newlines that were counted before the error.
    pub newlines_before: u32,
}

/// Output of [`unescape_double_quoted`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unescaped {
    /// The processed bytes.
    pub bytes: Vec<u8>,
    /// Octal overflow warnings: (message, newlines before the escape).
    pub warnings: Vec<(String, u32)>,
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn is_oct(c: u8) -> bool {
    (b'0'..=b'7').contains(&c)
}

/// Resolves escape sequences in double-quoted, backtick and heredoc string
/// content (`zend_scan_escape_string`). `quote` is `b'"'`, `` b'`' `` or 0 for
/// heredocs.
#[allow(clippy::too_many_lines)]
pub fn unescape_double_quoted(
    s: &[u8],
    quote: u8,
    warn_octal: bool,
) -> Result<Unescaped, EscapeError> {
    let at = |i: usize| -> u8 { s.get(i).copied().unwrap_or(0) };
    let mut out = Vec::with_capacity(s.len());
    let mut warnings = Vec::new();
    let mut newlines = 0u32;
    let end = s.len();
    let mut i = 0;
    while i < end {
        if s[i] == b'\\' {
            i += 1;
            if i >= end {
                out.push(b'\\');
                break;
            }
            let c = s[i];
            match c {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'f' => out.push(0x0c),
                b'v' => out.push(0x0b),
                b'e' => out.push(0x1b),
                b'"' | b'`' if c != quote => {
                    out.push(b'\\');
                    out.push(c);
                }
                b'"' | b'`' | b'\\' | b'$' => out.push(c),
                b'x' | b'X' => {
                    if is_hex(at(i + 1)) {
                        i += 1;
                        let mut v = char::from(s[i]).to_digit(16).unwrap_or(0);
                        if is_hex(at(i + 1)) {
                            i += 1;
                            v = v * 16 + char::from(s[i]).to_digit(16).unwrap_or(0);
                        }
                        out.push((v & 0xff) as u8);
                    } else {
                        out.push(b'\\');
                        out.push(c);
                    }
                }
                b'u' => {
                    if at(i + 1) == b'{' {
                        let start = i + 2;
                        let mut j = start;
                        let mut valid = true;
                        while at(j) != b'}' {
                            if !is_hex(at(j)) {
                                valid = false;
                                break;
                            }
                            j += 1;
                        }
                        if j == start {
                            valid = false;
                        }
                        if !valid {
                            return Err(EscapeError {
                                message: "Invalid UTF-8 codepoint escape sequence",
                                newlines_before: newlines,
                            });
                        }
                        let digits = &s[start..j];
                        let cp = digits.iter().try_fold(0u64, |acc, &d| {
                            acc.checked_mul(16)
                                .and_then(|a| a.checked_add(u64::from(char::from(d).to_digit(16)?)))
                        });
                        let cp = match cp {
                            Some(cp) if cp <= 0x10_FFFF => cp,
                            _ => return Err(EscapeError {
                                message:
                                    "Invalid UTF-8 codepoint escape sequence: Codepoint too large",
                                newlines_before: newlines,
                            }),
                        };
                        encode_utf8(cp, &mut out);
                        i = j;
                    } else {
                        out.push(b'\\');
                        out.push(b'u');
                    }
                }
                _ => {
                    if is_oct(c) {
                        let mut buf = vec![c];
                        if is_oct(at(i + 1)) {
                            i += 1;
                            buf.push(s[i]);
                            if is_oct(at(i + 1)) {
                                i += 1;
                                buf.push(s[i]);
                            }
                        }
                        if buf.len() == 3 && buf[0] > b'3' && warn_octal {
                            warnings.push((
                                format!(
                                    "Octal escape sequence overflow \\{} is greater than \\377",
                                    String::from_utf8_lossy(&buf)
                                ),
                                newlines,
                            ));
                        }
                        let v = buf
                            .iter()
                            .fold(0u32, |acc, &d| acc * 8 + u32::from(d - b'0'));
                        out.push((v & 0xff) as u8);
                    } else {
                        out.push(b'\\');
                        out.push(c);
                    }
                }
            }
        } else {
            out.push(s[i]);
        }
        let c = at(i);
        if c == b'\n' || (c == b'\r' && at(i + 1) != b'\n') {
            newlines += 1;
        }
        i += 1;
    }
    Ok(Unescaped {
        bytes: out,
        warnings,
    })
}

fn encode_utf8(cp: u64, out: &mut Vec<u8>) {
    // The casts below are lossless because of the range checks.
    #[allow(clippy::cast_possible_truncation)]
    {
        if cp < 0x80 {
            out.push(cp as u8);
        } else if cp <= 0x7FF {
            out.push(((cp >> 6) + 0xC0) as u8);
            out.push(((cp & 0x3F) + 0x80) as u8);
        } else if cp <= 0xFFFF {
            out.push(((cp >> 12) + 0xE0) as u8);
            out.push((((cp >> 6) & 0x3F) + 0x80) as u8);
            out.push(((cp & 0x3F) + 0x80) as u8);
        } else {
            out.push(((cp >> 18) + 0xF0) as u8);
            out.push((((cp >> 12) & 0x3F) + 0x80) as u8);
            out.push((((cp >> 6) & 0x3F) + 0x80) as u8);
            out.push(((cp & 0x3F) + 0x80) as u8);
        }
    }
}

/// Error from [`strip_indentation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndentError {
    /// PHP's error message.
    pub message: String,
    /// Newlines counted before the error (added to the line number).
    pub newlines_before: u32,
}

fn next_newline(s: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < s.len() {
        if s[i] == b'\r' {
            let len = if s.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
            return Some((i, len));
        } else if s[i] == b'\n' {
            return Some((i, 1));
        }
        i += 1;
    }
    None
}

/// Removes the closing-marker indentation from a heredoc/nowdoc body segment
/// (`strip_multiline_string_indentation`).
pub fn strip_indentation(
    s: &[u8],
    indentation: usize,
    using_spaces: bool,
    newline_at_start: bool,
    newline_at_end: bool,
) -> Result<Vec<u8>, IndentError> {
    let end = s.len();
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut newline_count = 0u32;
    let mut str_pos;
    // `nl` mirrors the C pointer: None == NULL.
    let mut nl: Option<usize>;

    if newline_at_start {
        str_pos = 0;
        nl = Some(0);
    } else {
        match next_newline(s, 0) {
            None => return Ok(s.to_vec()),
            Some((p, len)) => {
                out.extend_from_slice(&s[..p + len]);
                str_pos = p + len;
                nl = Some(p);
                newline_count += 1;
            }
        }
    }

    while str_pos <= end && nl.is_some() {
        let mut newline_len = 0;
        nl = match next_newline(s, str_pos) {
            Some((p, len)) => {
                newline_len = len;
                Some(p)
            }
            None => None,
        };
        if nl.is_none() && newline_at_end {
            nl = Some(end);
        }

        let mut skip = 0;
        while skip < indentation {
            if Some(str_pos) == nl {
                break;
            }
            if str_pos == end || (s[str_pos] != b' ' && s[str_pos] != b'\t') {
                return Err(IndentError {
                    message: format!(
                        "Invalid body indentation level (expecting an indentation level of at least {indentation})"
                    ),
                    newlines_before: newline_count,
                });
            }
            if (!using_spaces && s[str_pos] == b' ') || (using_spaces && s[str_pos] == b'\t') {
                return Err(IndentError {
                    message: "Invalid indentation - tabs and spaces cannot be mixed".to_owned(),
                    newlines_before: newline_count,
                });
            }
            skip += 1;
            str_pos += 1;
        }

        if str_pos == end {
            break;
        }

        let len = match nl {
            Some(p) => p - str_pos + newline_len,
            None => end - str_pos,
        };
        out.extend_from_slice(&s[str_pos..str_pos + len]);
        str_pos += len;
        newline_count += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lnum() {
        assert_eq!(parse_lnum(b"123").value, TokenValue::Int(123));
        assert_eq!(parse_lnum(b"1_000").value, TokenValue::Int(1000));
        assert_eq!(parse_lnum(b"0777").value, TokenValue::Int(511));
        assert_eq!(
            parse_lnum(b"9223372036854775807").value,
            TokenValue::Int(i64::MAX)
        );
        assert_eq!(
            parse_lnum(b"9223372036854775808").value,
            TokenValue::Float(9_223_372_036_854_775_808.0)
        );
        let r = parse_lnum(b"08");
        assert!(r.invalid_octal);
        assert_eq!(r.value, TokenValue::Int(0));
        assert!(matches!(
            parse_lnum(b"0177777777777777777777787").value,
            TokenValue::Float(_)
        ));
    }

    #[test]
    fn prefixed() {
        assert_eq!(parse_prefixed_int(b"0x1A", 16), TokenValue::Int(26));
        assert_eq!(parse_prefixed_int(b"0b101", 2), TokenValue::Int(5));
        assert_eq!(parse_prefixed_int(b"0o17", 8), TokenValue::Int(15));
        assert_eq!(parse_prefixed_int(b"0x0", 16), TokenValue::Int(0));
        assert_eq!(
            parse_prefixed_int(b"0x7FFFFFFFFFFFFFFF", 16),
            TokenValue::Int(i64::MAX)
        );
        assert_eq!(
            parse_prefixed_int(b"0x8000000000000000", 16),
            TokenValue::Float(9_223_372_036_854_775_808.0)
        );
    }

    #[test]
    fn prefixed_overflow_rounds_like_zend_strtod() {
        // zend_bin_strtod adds the ASCII digit then subtracts '0': 64 ones
        // give 2^64 - 2048, not 2^64 (Zend/tests/binary.phpt).
        let ones = [b"0b".as_slice(), &[b'1'; 64]].concat();
        assert_eq!(
            parse_prefixed_int(&ones, 2),
            TokenValue::Float(18_446_744_073_709_549_568.0)
        );
        let sevens = [b"0o".as_slice(), &[b'7'; 25]].concat();
        assert_eq!(
            parse_prefixed_int(&sevens, 8),
            TokenValue::Float(3.777_893_186_295_716e22)
        );
    }

    #[test]
    fn floats() {
        assert!((parse_float_literal(b"1.5") - 1.5).abs() < f64::EPSILON);
        assert!((parse_float_literal(b".5") - 0.5).abs() < f64::EPSILON);
        assert!((parse_float_literal(b"1.") - 1.0).abs() < f64::EPSILON);
        assert!((parse_float_literal(b"1_0.0_1e1_0") - 10.01e10).abs() < 1.0);
        assert!((parse_float_literal(b"1.e3") - 1000.0).abs() < f64::EPSILON);
        assert!(parse_float_literal(b"1e999").is_infinite());
    }

    #[test]
    fn escapes() {
        let r = unescape_double_quoted(br"a\nb\x41\101\u{1F600}\$\q\", b'"', true).unwrap();
        assert_eq!(r.bytes, b"a\nbAA\xF0\x9F\x98\x80$\\q\\".to_vec());
        let r = unescape_double_quoted(br#"\"\`"#, b'`', true).unwrap();
        assert_eq!(r.bytes, br#"\"`"#.to_vec());
        assert!(unescape_double_quoted(br"\u{}", b'"', true).is_err());
        assert!(unescape_double_quoted(br"\u{110000}", b'"', true).is_err());
        let r = unescape_double_quoted(br"\400", b'"', true).unwrap();
        assert_eq!(r.bytes, vec![0]);
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(unescape_single_quoted(br"a\'b\\c\d"), br"a'b\c\d".to_vec());
    }

    #[test]
    fn indentation() {
        assert_eq!(
            strip_indentation(b"  a\n  b", 2, true, true, true).unwrap(),
            b"a\nb".to_vec()
        );
        assert_eq!(
            strip_indentation(b"  a\n\n  b", 2, true, true, true).unwrap(),
            b"a\n\nb".to_vec()
        );
        assert!(strip_indentation(b"  a\n b", 2, true, true, true).is_err());
        assert!(strip_indentation(b" a\n", 1, true, true, false).is_err());
    }
}

#[cfg(test)]
mod scan_helper_tests {
    use super::{count_newlines, unescape_single_quoted};

    #[test]
    fn single_quoted_escapes() {
        assert_eq!(unescape_single_quoted(b"plain"), b"plain");
        assert_eq!(unescape_single_quoted(b""), b"");
        assert_eq!(unescape_single_quoted(br"a\'b\\c\nd"), br"a'b\c\nd");
        assert_eq!(unescape_single_quoted(br"end\"), br"end\");
        assert_eq!(unescape_single_quoted(br"\\\\"), br"\\");
        assert_eq!(unescape_single_quoted(br"\x"), br"\x");
    }

    #[test]
    fn newline_counting() {
        assert_eq!(count_newlines(b"a\nb\nc", 0, 5), 2);
        assert_eq!(count_newlines(b"a\r\nb\rc", 0, 6), 2);
        // A `\r` at the end of the range followed by `\n` past it is not
        // a break of its own.
        assert_eq!(count_newlines(b"a\r\n", 0, 2), 0);
        assert_eq!(count_newlines(b"a\r", 0, 2), 1);
        assert_eq!(count_newlines(b"\n\n", 1, 2), 1);
    }
}
