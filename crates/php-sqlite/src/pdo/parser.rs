// Derived from php-src ext/pdo/pdo_sql_parser.re. Similarity metric to original: 25%
//   Copyright © The PHP Group and Contributors. Author: George Schlossnagle <george@omniti.com>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The PDO SQL placeholder parser: a port of `ext/pdo/pdo_sql_parser.re`
//! (`pdo_parse_params()` and the default re2c scanner) plus the SQLite
//! driver's scanner (`ext/pdo_sqlite/sqlite_sql_parser.re`).
//!
//! The scanners reproduce re2c's longest-match semantics (ties go to the
//! earlier rule) over a NUL-terminated buffer, including the `YYFILL`
//! behaviour of the C code: a token that would have to read past the
//! terminating NUL (an unterminated `--` or `/* */` comment) ends the scan
//! (`PDO_PARSER_EOI`).
//!
//! [`parse_params`] is independent of the driver: the PDO core passes the
//! statement's placeholder support, its bound parameters (for emulated
//! prepares), the driver quoter and the persistent `bound_param_map`.

use crate::host::Cx;
use php_runtime::{Value, ZStr};

/// `PDO_PARSER_*` token kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// `PDO_PARSER_TEXT`.
    Text,
    /// `PDO_PARSER_BIND` (`:name`).
    Bind,
    /// `PDO_PARSER_BIND_POS` (`?`).
    BindPos,
    /// `PDO_PARSER_ESCAPED_QUESTION` (`??`, only produced by driver scanners
    /// that support it, e.g. pgsql).
    EscapedQuestion,
    /// `PDO_PARSER_CUSTOM_QUOTE` (e.g. pgsql dollar quotes).
    CustomQuote,
    /// `PDO_PARSER_EOI`.
    Eoi,
}

/// A re2c scanner: returns the token starting at `pos` and its end offset.
/// `input` is the query without its terminating NUL (which the scanner
/// treats as present at `input.len()`).
pub trait Scanner {
    /// Scans one token.
    fn scan(&self, input: &[u8], pos: usize) -> (Token, usize);
}

/// The default scanner of `pdo_sql_parser.re`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultScanner;

/// The SQLite driver scanner (`pdo_sqlite_scanner`): also knows backtick
/// and square-bracket identifiers, and its quoted-string rules accept any
/// byte (longest match).
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteScanner;

/// Byte at `i`, with the terminating NUL past the end.
fn at(input: &[u8], i: usize) -> u8 {
    input.get(i).copied().unwrap_or(0)
}

/// `(["]((["]["])|ANYNOEOF\["])*["])`-style rule: a quoted run where the
/// quote is escaped by doubling it and other bytes must not be NUL.
fn quoted_strict(input: &[u8], pos: usize, q: u8) -> Option<usize> {
    if at(input, pos) != q {
        return None;
    }
    let mut i = pos + 1;
    loop {
        match at(input, i) {
            0 => return None,
            c if c == q => {
                if at(input, i + 1) == q {
                    i += 2;
                } else {
                    return Some(i + 1);
                }
            }
            _ => i += 1,
        }
    }
}

/// `(["]((["]["])|ANYNOEOF)*["])` / `("["ANYNOEOF*"]")`: any non-NUL bytes
/// between the delimiters; longest match ends at the last closing
/// delimiter before a NUL.
fn quoted_greedy(input: &[u8], pos: usize, open: u8, close: u8) -> Option<usize> {
    if at(input, pos) != open {
        return None;
    }
    let mut best = None;
    let mut i = pos + 1;
    while at(input, i) != 0 {
        if at(input, i) == close {
            best = Some(i + 1);
        }
        i += 1;
    }
    best
}

/// `MULTICHAR = ([:]{2,}|[?]{2,})`.
fn multichar(input: &[u8], pos: usize) -> Option<usize> {
    let c = at(input, pos);
    if c != b':' && c != b'?' {
        return None;
    }
    let mut i = pos;
    while at(input, i) == c {
        i += 1;
    }
    (i - pos >= 2).then_some(i)
}

/// `BINDCHR = [:][a-zA-Z0-9_]+`.
fn bindchr(input: &[u8], pos: usize) -> Option<usize> {
    if at(input, pos) != b':' {
        return None;
    }
    let mut i = pos + 1;
    while at(input, i).is_ascii_alphanumeric() || at(input, i) == b'_' {
        i += 1;
    }
    (i > pos + 1).then_some(i)
}

/// `COMMENTS`; `Err(())` when the comment runs into the end of the buffer
/// (the scanner then returns EOI through `YYFILL`).
fn comment(input: &[u8], pos: usize) -> Option<Result<usize, ()>> {
    match (at(input, pos), at(input, pos + 1)) {
        (b'-', b'-') => {
            // "--".* : '.' is any byte but '\n', including the final NUL.
            let mut i = pos + 2;
            while i <= input.len() {
                if at(input, i) == b'\n' {
                    return Some(Ok(i));
                }
                i += 1;
            }
            Some(Err(()))
        }
        (b'/', b'*') => {
            // "/*"([^*]+|[*]+[^/*])*[*]*"*/" : scan for the first "*/".
            let mut i = pos + 2;
            while i <= input.len() {
                if at(input, i) == b'*' && i < input.len() {
                    let mut j = i;
                    while at(input, j) == b'*' && j < input.len() {
                        j += 1;
                    }
                    if at(input, j) == b'/' && j < input.len() {
                        return Some(Ok(j + 1));
                    }
                    i = j;
                    continue;
                }
                i += 1;
            }
            Some(Err(()))
        }
        _ => None,
    }
}

/// Generic re2c-style dispatch: `rules` are tried in order; the longest
/// match wins, ties go to the first rule.
fn longest(candidates: &[(Option<usize>, Token)], pos: usize) -> Option<(Token, usize)> {
    let mut best: Option<(Token, usize)> = None;
    for (end, tok) in candidates {
        if let Some(end) = *end {
            if end > pos && best.is_none_or(|(_, b)| end > b) {
                best = Some((*tok, end));
            }
        }
    }
    best
}

fn scan_common(
    input: &[u8],
    pos: usize,
    specials: &[u8],
    quoted: &[(Option<usize>, Token)],
) -> (Token, usize) {
    if pos >= input.len() {
        return (Token::Eoi, pos);
    }
    let cmt = comment(input, pos);
    let mut candidates: Vec<(Option<usize>, Token)> = quoted.to_vec();
    candidates.push((multichar(input, pos), Token::Text));
    candidates.push((bindchr(input, pos), Token::Bind));
    candidates.push(((at(input, pos) == b'?').then_some(pos + 1), Token::BindPos));
    candidates.push((
        specials.contains(&at(input, pos)).then_some(pos + 1),
        Token::Text,
    ));
    match cmt {
        Some(Err(())) => {
            // The comment is the longest possible match and needs input past
            // the terminator: YYFILL ends the scan.
            return (Token::Eoi, input.len());
        }
        Some(Ok(end)) => candidates.push((Some(end), Token::Text)),
        None => {}
    }
    // (ANYNOEOF\SPECIALS)+
    let mut i = pos;
    while i < input.len() && input[i] != 0 && !specials.contains(&input[i]) {
        i += 1;
    }
    candidates.push(((i > pos).then_some(i), Token::Text));
    // An embedded NUL matches no rule; treat it as one byte of text.
    longest(&candidates, pos).unwrap_or((Token::Text, pos + 1))
}

impl Scanner for DefaultScanner {
    fn scan(&self, input: &[u8], pos: usize) -> (Token, usize) {
        let quoted = [
            (quoted_strict(input, pos, b'"'), Token::Text),
            (quoted_strict(input, pos, b'\''), Token::Text),
        ];
        scan_common(input, pos, b":?\"'/-", &quoted)
    }
}

impl Scanner for SqliteScanner {
    fn scan(&self, input: &[u8], pos: usize) -> (Token, usize) {
        let quoted = [
            (quoted_greedy(input, pos, b'"', b'"'), Token::Text),
            (quoted_greedy(input, pos, b'\'', b'\''), Token::Text),
            (quoted_greedy(input, pos, b'`', b'`'), Token::Text),
            (quoted_greedy(input, pos, b'[', b']'), Token::Text),
        ];
        scan_common(input, pos, b":?\"'`/[-", &quoted)
    }
}

/// Splits a query into tokens (until EOI), for diagnostics and tests.
pub fn tokenize(scanner: &dyn Scanner, input: &[u8]) -> Vec<(Token, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let (t, end) = scanner.scan(input, pos);
        if t == Token::Eoi {
            return out;
        }
        out.push((t, input[pos..end.min(input.len())].to_vec()));
        pos = end;
    }
}

/// `PDO_PLACEHOLDER_NONE`.
pub const PLACEHOLDER_NONE: u8 = 0;
/// `PDO_PLACEHOLDER_NAMED`.
pub const PLACEHOLDER_NAMED: u8 = 1;
/// `PDO_PLACEHOLDER_POSITIONAL`.
pub const PLACEHOLDER_POSITIONAL: u8 = 2;

/// `PDO_PARAM_NULL`.
pub const PARAM_NULL: i64 = 0;
/// `PDO_PARAM_INT`.
pub const PARAM_INT: i64 = 1;
/// `PDO_PARAM_STR`.
pub const PARAM_STR: i64 = 2;
/// `PDO_PARAM_LOB`.
pub const PARAM_LOB: i64 = 3;
/// `PDO_PARAM_STMT`.
pub const PARAM_STMT: i64 = 4;
/// `PDO_PARAM_BOOL`.
pub const PARAM_BOOL: i64 = 5;

/// A key of `bound_params` / `bound_param_map`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParamMapKey {
    /// A 0-based position.
    Index(i64),
    /// A name including its leading `:`.
    Name(Vec<u8>),
}

/// A bound parameter as the parser sees it (emulated prepares).
#[derive(Debug, Clone)]
pub struct ParserParam {
    /// The hash key in `stmt->bound_params`.
    pub key: ParamMapKey,
    /// The bound value (may be a reference).
    pub value: Value,
    /// `PDO_PARAM_*` (flags already masked or not; only the exact type is
    /// compared, like the C code does).
    pub param_type: i64,
}

/// What the parser needs from the statement.
#[derive(Debug)]
pub struct ParseContext<'a> {
    /// `stmt->supports_placeholders`.
    pub supports_placeholders: u8,
    /// `stmt->named_rewrite_template` (e.g. `":pdo%d"`, `"$%d"`).
    pub named_rewrite_template: Option<&'a str>,
    /// `stmt->bound_params` (`None` when no parameter was bound).
    pub bound_params: Option<&'a [ParserParam]>,
    /// `stmt->bound_param_map`, kept on the statement between calls.
    pub bound_param_map: &'a mut Vec<(ParamMapKey, Vec<u8>)>,
}

/// A failed `pdo_parse_params()` (`-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// `pdo_raise_impl_error(dbh, stmt, sqlstate, message)`.
    Impl {
        /// The SQLSTATE.
        sqlstate: &'static str,
        /// The supplementary message.
        message: &'static str,
    },
    /// The quoter failed: copy the dbh error code to the statement.
    QuoteFailed,
    /// A conversion threw (the exception is pending in the [`Cx`]).
    Exception,
}

/// The result of a successful parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// `0`: the query can be used as is.
    Unchanged,
    /// `1`: the rewritten query.
    Rewritten(Vec<u8>),
}

#[derive(Debug)]
struct Placeholder {
    pos: usize,
    len: usize,
    quoted: Option<Vec<u8>>,
    /// `None` for an escaped character.
    bindno: Option<u32>,
}

fn find_param<'p>(params: &'p [ParserParam], key: &ParamMapKey) -> Option<&'p ParserParam> {
    params.iter().find(|p| &p.key == key)
}

fn format_template(tmpl: &str, n: u32) -> Vec<u8> {
    tmpl.replacen("%d", &n.to_string(), 1).into_bytes()
}

/// `dbh->methods->quoter`: quotes the bytes for the parameter type
/// (`None` on failure).
pub type Quoter<'q> = dyn FnMut(&mut Cx<'_>, &[u8], i64) -> Option<Vec<u8>> + 'q;

/// `pdo_parse_params()`.
///
/// `quoter` is `dbh->methods->quoter` (`None` when the driver has none):
/// it receives the unquoted bytes and the parameter type and returns
/// `None` on failure. Deprecations raised for escaped question marks in
/// custom quotes go through `cx`.
#[allow(clippy::too_many_lines)]
pub fn parse_params(
    cx: &mut Cx<'_>,
    scanner: &dyn Scanner,
    query: &[u8],
    ctx: &mut ParseContext<'_>,
    mut quoter: Option<&mut Quoter<'_>>,
) -> Result<Parsed, ParseError> {
    let mut placeholders: Vec<Placeholder> = Vec::new();
    let mut bindno: u32 = 0;
    let mut escapes = 0;
    let mut query_type = PLACEHOLDER_NONE;
    let mut custom_quote: Option<(usize, usize)> = None;
    let mut pos = 0;

    // phase 1: look for args
    loop {
        let (t, end) = scanner.scan(query, pos);
        if t == Token::Eoi {
            break;
        }
        let tok = pos;
        pos = end;
        let len = end - tok;
        if let Some((qpos, qlen)) = custom_quote {
            if t == Token::CustomQuote && qlen == len && query[tok..end] == query[qpos..qpos + qlen]
            {
                custom_quote = None;
            } else if t == Token::EscapedQuestion {
                cx.docref(
                    php_runtime::ErrorLevel::Deprecated,
                    "Escaping question marks inside dollar quoted strings is not required anymore and is deprecated",
                );
                placeholders.push(Placeholder {
                    pos: tok,
                    len,
                    quoted: Some(b"?".to_vec()),
                    bindno: None,
                });
                escapes += 1;
            }
            continue;
        }
        if t == Token::CustomQuote {
            custom_quote = Some((tok, len));
            continue;
        }
        if matches!(t, Token::Bind | Token::BindPos | Token::EscapedQuestion) {
            if t == Token::EscapedQuestion && ctx.supports_placeholders == PLACEHOLDER_POSITIONAL {
                // escaped question marks unsupported, treat as text
                continue;
            }
            if t == Token::Bind {
                if tok > 0 && query[tok - 1].is_ascii_alphanumeric() {
                    continue;
                }
                query_type |= PLACEHOLDER_NAMED;
            } else if t == Token::BindPos {
                query_type |= PLACEHOLDER_POSITIONAL;
            }
            if t == Token::EscapedQuestion {
                placeholders.push(Placeholder {
                    pos: tok,
                    len,
                    quoted: Some(b"?".to_vec()),
                    bindno: None,
                });
                escapes += 1;
            } else {
                placeholders.push(Placeholder {
                    pos: tok,
                    len,
                    quoted: None,
                    bindno: Some(bindno),
                });
                bindno += 1;
            }
        }
    }

    // did the query make sense to me?
    if query_type == (PLACEHOLDER_NAMED | PLACEHOLDER_POSITIONAL) {
        return Err(ParseError::Impl {
            sqlstate: "HY093",
            message: "mixed named and positional parameters",
        });
    }

    let params = ctx.bound_params;
    let mut skip_to_safe = false;
    if ctx.supports_placeholders == PLACEHOLDER_NONE {
        if let Some(params) = params {
            if bindno as usize != params.len() {
                // extra bit of validation for instances when same params are
                // bound more than once
                let ok = query_type != PLACEHOLDER_POSITIONAL
                    && bindno as usize > params.len()
                    && placeholders.iter().all(|p| {
                        find_param(
                            params,
                            &ParamMapKey::Name(query[p.pos..p.pos + p.len].to_vec()),
                        )
                        .is_some()
                    });
                if !ok {
                    return Err(ParseError::Impl {
                        sqlstate: "HY093",
                        message: "number of bound variables does not match number of tokens",
                    });
                }
                skip_to_safe = true;
            }
        }
    }

    if placeholders.is_empty() {
        return Ok(Parsed::Unchanged);
    }

    if !skip_to_safe {
        if ctx.supports_placeholders == query_type && ctx.named_rewrite_template.is_none() {
            // query matches native syntax
            if escapes > 0 {
                return Ok(Parsed::Rewritten(rewrite(query, &placeholders)));
            }
            return Ok(Parsed::Unchanged);
        }
        if query_type == PLACEHOLDER_NAMED && ctx.named_rewrite_template.is_some() {
            // pretend the query was positional to fall into the named
            // rewrite case below
            query_type = PLACEHOLDER_POSITIONAL;
        }
    }

    if ctx.supports_placeholders == PLACEHOLDER_NONE {
        // query generation: quote all the values
        if let Some(params) = params {
            for plc in &mut placeholders {
                let Some(bn) = plc.bindno else { continue };
                if query_type == PLACEHOLDER_NONE {
                    continue;
                }
                let key = if query_type == PLACEHOLDER_POSITIONAL {
                    ParamMapKey::Index(i64::from(bn))
                } else {
                    ParamMapKey::Name(query[plc.pos..plc.pos + plc.len].to_vec())
                };
                let Some(param) = find_param(params, &key) else {
                    return Err(ParseError::Impl {
                        sqlstate: "HY093",
                        message: "parameter was not defined",
                    });
                };
                let parameter = param.value.deref();
                if let Some(quoter) = quoter.as_deref_mut() {
                    if param.param_type == PARAM_LOB && matches!(parameter, Value::Resource(_)) {
                        let Some(buf) = cx.host().read_stream(&parameter) else {
                            return Err(ParseError::Impl {
                                sqlstate: "HY105",
                                message: "Expected a stream resource",
                            });
                        };
                        match quoter(cx, &buf, param.param_type) {
                            Some(q) => plc.quoted = Some(q),
                            None => return Err(ParseError::QuoteFailed),
                        }
                    } else {
                        let param_type = if parameter.is_null() {
                            PARAM_NULL
                        } else {
                            param.param_type
                        };
                        plc.quoted = Some(match param_type {
                            PARAM_BOOL => {
                                let r = parameter.to_bool(cx.host());
                                let b = cx.check(r).ok_or(ParseError::Exception)?;
                                if b {
                                    b"1".to_vec()
                                } else {
                                    b"0".to_vec()
                                }
                            }
                            PARAM_INT => {
                                let r = parameter.to_long(cx.host());
                                let l = cx.check(r).ok_or(ParseError::Exception)?;
                                l.to_string().into_bytes()
                            }
                            PARAM_NULL => b"NULL".to_vec(),
                            _ => {
                                let r = parameter.to_zstr(cx.host());
                                let Some(buf) = cx.check(r) else {
                                    return Err(ParseError::QuoteFailed);
                                };
                                match quoter(cx, buf.as_bytes(), param_type) {
                                    Some(q) => q,
                                    None => return Err(ParseError::QuoteFailed),
                                }
                            }
                        });
                    }
                } else {
                    let s = parameter.as_str().cloned().unwrap_or_else(ZStr::empty);
                    plc.quoted = Some(s.as_bytes().to_vec());
                }
            }
        }
        return Ok(Parsed::Rewritten(rewrite(query, &placeholders)));
    }

    if query_type == PLACEHOLDER_POSITIONAL {
        // rewrite ? to :pdoX
        let tmpl = ctx.named_rewrite_template.unwrap_or(":pdo%d");
        let mut bind_no = 1;
        for plc in &mut placeholders {
            let Some(bn) = plc.bindno else { continue };
            let name = query[plc.pos..plc.pos + plc.len].to_vec();
            let existing = if name == b"?" {
                None
            } else {
                ctx.bound_param_map
                    .iter()
                    .find(|(k, _)| *k == ParamMapKey::Name(name.clone()))
                    .map(|(_, v)| v.clone())
            };
            let skip_map = existing.is_some();
            let idx = existing.unwrap_or_else(|| {
                let v = format_template(tmpl, bind_no);
                bind_no += 1;
                v
            });
            if !skip_map && ctx.named_rewrite_template.is_some() {
                map_update(ctx.bound_param_map, ParamMapKey::Name(name), idx.clone());
            }
            map_update(
                ctx.bound_param_map,
                ParamMapKey::Index(i64::from(bn)),
                idx.clone(),
            );
            plc.quoted = Some(idx);
        }
        return Ok(Parsed::Rewritten(rewrite(query, &placeholders)));
    }

    // rewrite :name to ?
    for plc in &mut placeholders {
        let name = query[plc.pos..plc.pos + plc.len].to_vec();
        let bn = plc.bindno.map_or(-1, i64::from);
        map_update(ctx.bound_param_map, ParamMapKey::Index(bn), name);
        plc.quoted = Some(b"?".to_vec());
    }
    Ok(Parsed::Rewritten(rewrite(query, &placeholders)))
}

fn map_update(map: &mut Vec<(ParamMapKey, Vec<u8>)>, key: ParamMapKey, v: Vec<u8>) {
    if let Some(e) = map.iter_mut().find(|(k, _)| *k == key) {
        e.1 = v;
    } else {
        map.push((key, v));
    }
}

/// Builds the output query, replacing each placeholder by its quoted text.
fn rewrite(query: &[u8], placeholders: &[Placeholder]) -> Vec<u8> {
    let mut out = Vec::with_capacity(query.len());
    let mut ptr = 0;
    for plc in placeholders {
        out.extend_from_slice(&query[ptr..plc.pos]);
        match &plc.quoted {
            Some(q) => out.extend_from_slice(q),
            None => out.extend_from_slice(&query[plc.pos..plc.pos + plc.len]),
        }
        ptr = plc.pos + plc.len;
    }
    out.extend_from_slice(&query[ptr..]);
    out
}
