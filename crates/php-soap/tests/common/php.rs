// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A tiny PHP interpreter for running the ext/soap `.phpt` scripts
//! against the extension through [`TestHost`]. It covers the subset of
//! PHP these scripts use (classes with promoted constructor parameters,
//! functions, arrays, strings with interpolation, heredocs, try/catch,
//! include, output buffering and a handful of builtins).

#![allow(dead_code, clippy::too_many_lines, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use php_runtime::array::ArrayKey;
use php_runtime::{ops, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};
use php_soap::host::SoapHost;

use super::host::{CliServer, MethodFn, TestHost, UserClass};

// ---- lexer --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum T {
    Html(Vec<u8>),
    Var(String),
    Id(String),
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    IStr(Vec<Part>),
    Op(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Lit(Vec<u8>),
    Code(Vec<T>),
}

const OPS: &[&str] = &[
    "<<=", ">>=", "**=", "...", "<=>", "===", "!==", "??=", "?->", "->", "=>", "::", "==", "!=",
    "<>", "<=", ">=", "&&", "||", "++", "--", ".=", "+=", "-=", "*=", "/=", "%=", "??", "<<", ">>",
    "**", ";", ",", "(", ")", "{", "}", "[", "]", "=", ".", "+", "-", "*", "/", "%", "!", "<", ">",
    "?", ":", "@", "&", "|", "^", "~",
];

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    out: Vec<T>,
}

fn is_id_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || c == b'\\'
}

fn is_id(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || c == b'\\'
}

impl<'a> Lexer<'a> {
    fn lex(src: &'a [u8]) -> Vec<T> {
        let mut l = Lexer {
            s: src,
            i: 0,
            out: Vec::new(),
        };
        l.run();
        l.out
    }

    fn run(&mut self) {
        // inline HTML until <?php
        loop {
            let rest = &self.s[self.i..];
            let open = find(rest, b"<?php").or_else(|| find(rest, b"<?="));
            if let Some(p) = open {
                if p > 0 {
                    self.out.push(T::Html(rest[..p].to_vec()));
                }
                let is_echo = rest[p..].starts_with(b"<?=");
                self.i += p + if is_echo { 3 } else { 5 };
                if is_echo {
                    self.out.push(T::Id("echo".into()));
                }
                if !self.code() {
                    return;
                }
            } else {
                if !rest.is_empty() {
                    self.out.push(T::Html(rest.to_vec()));
                }
                return;
            }
        }
    }

    /// Lexes code until `?>`; returns false at EOF.
    fn code(&mut self) -> bool {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            let rest = &self.s[self.i..];
            if c.is_ascii_whitespace() {
                self.i += 1;
            } else if rest.starts_with(b"?>") {
                self.i += 2;
                if self.s.get(self.i) == Some(&b'\n') {
                    self.i += 1;
                }
                self.out.push(T::Op(";"));
                return true;
            } else if rest.starts_with(b"//") || c == b'#' && !rest.starts_with(b"#[") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    if self.s[self.i..].starts_with(b"?>") {
                        break;
                    }
                    self.i += 1;
                }
            } else if rest.starts_with(b"#[") {
                // attribute: skip to matching ]
                let mut depth = 0;
                while self.i < self.s.len() {
                    match self.s[self.i] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
            } else if rest.starts_with(b"/*") {
                let end = find(&rest[2..], b"*/").map_or(self.s.len(), |p| self.i + 2 + p + 2);
                self.i = end;
            } else if c == b'$' && self.s.get(self.i + 1).is_some_and(|&n| is_id_start(n)) {
                let start = self.i + 1;
                self.i += 1;
                while self.i < self.s.len() && is_id(self.s[self.i]) && self.s[self.i] != b'\\' {
                    self.i += 1;
                }
                self.out.push(T::Var(
                    String::from_utf8_lossy(&self.s[start..self.i]).into_owned(),
                ));
            } else if is_id_start(c) {
                let start = self.i;
                while self.i < self.s.len() && is_id(self.s[self.i]) {
                    self.i += 1;
                }
                let id = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                self.out.push(T::Id(id.trim_start_matches('\\').to_owned()));
            } else if c.is_ascii_digit()
                || (c == b'.' && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit))
            {
                self.number();
            } else if c == b'\'' {
                self.i += 1;
                let mut v = Vec::new();
                while self.i < self.s.len() && self.s[self.i] != b'\'' {
                    if self.s[self.i] == b'\\'
                        && matches!(self.s.get(self.i + 1), Some(b'\'' | b'\\'))
                    {
                        self.i += 1;
                    }
                    v.push(self.s[self.i]);
                    self.i += 1;
                }
                self.i += 1;
                self.out.push(T::Str(v));
            } else if c == b'"' {
                self.i += 1;
                let parts = self.interp(|s, i| s[i] == b'"', true);
                self.i += 1;
                self.out.push(T::IStr(parts));
            } else if rest.starts_with(b"<<<") {
                self.heredoc();
            } else if let Some(op) = OPS.iter().find(|o| rest.starts_with(o.as_bytes())) {
                // casts
                if *op == "(" {
                    if let Some(close) = rest.iter().position(|&x| x == b')') {
                        let inner: String = String::from_utf8_lossy(&rest[1..close])
                            .trim()
                            .to_ascii_lowercase();
                        if [
                            "object", "array", "string", "int", "integer", "float", "double",
                            "bool", "boolean",
                        ]
                        .contains(&inner.as_str())
                        {
                            self.out.push(T::Op("(cast"));
                            self.out.push(T::Id(inner));
                            self.i += close + 1;
                            continue;
                        }
                    }
                }
                self.out.push(T::Op(op));
                self.i += op.len();
            } else {
                self.i += 1;
            }
        }
        false
    }

    fn number(&mut self) {
        let start = self.i;
        let s = self.s;
        if s[self.i] == b'0' && matches!(s.get(self.i + 1), Some(b'x' | b'X')) {
            self.i += 2;
            while self.i < s.len() && (s[self.i].is_ascii_hexdigit() || s[self.i] == b'_') {
                self.i += 1;
            }
            let t: String = String::from_utf8_lossy(&s[start + 2..self.i]).replace('_', "");
            match i64::from_str_radix(&t, 16) {
                Ok(v) => self.out.push(T::Int(v)),
                Err(_) => self.out.push(T::Float(
                    u64::from_str_radix(&t, 16).map_or(0.0, |v| v as f64),
                )),
            }
            return;
        }
        let mut is_float = false;
        while self.i < s.len() {
            let c = s[self.i];
            if c.is_ascii_digit() || c == b'_' {
                self.i += 1;
            } else if c == b'.'
                && !is_float
                && s.get(self.i + 1)
                    .is_none_or(|n| n.is_ascii_digit() || !is_id_start(*n))
            {
                is_float = true;
                self.i += 1;
            } else if (c == b'e' || c == b'E')
                && s.get(self.i + 1)
                    .is_some_and(|n| n.is_ascii_digit() || *n == b'-' || *n == b'+')
            {
                is_float = true;
                self.i += 2;
            } else {
                break;
            }
        }
        let t = String::from_utf8_lossy(&s[start..self.i]).replace('_', "");
        if is_float {
            self.out.push(T::Float(t.parse().unwrap_or(0.0)));
        } else {
            match t.parse::<i64>() {
                Ok(v) => self.out.push(T::Int(v)),
                Err(_) => self.out.push(T::Float(t.parse().unwrap_or(0.0))),
            }
        }
    }

    fn escape(&mut self, out: &mut Vec<u8>) {
        let s = self.s;
        self.i += 1;
        let Some(&c) = s.get(self.i) else {
            return;
        };
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'v' => out.push(0x0b),
            b'e' => out.push(0x1b),
            b'f' => out.push(0x0c),
            b'0'..=b'7' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 && self.i < s.len() && (b'0'..=b'7').contains(&s[self.i]) {
                    v = v * 8 + u32::from(s[self.i] - b'0');
                    self.i += 1;
                    n += 1;
                }
                out.push(u8::try_from(v & 0xff).unwrap_or(0));
                return;
            }
            b'x' if s.get(self.i + 1).is_some_and(u8::is_ascii_hexdigit) => {
                let mut v = 0u32;
                let mut n = 0;
                self.i += 1;
                while n < 2 && self.i < s.len() && s[self.i].is_ascii_hexdigit() {
                    v = v * 16 + (s[self.i] as char).to_digit(16).unwrap_or(0);
                    self.i += 1;
                    n += 1;
                }
                out.push(u8::try_from(v).unwrap_or(0));
                return;
            }
            b'u' if s.get(self.i + 1) == Some(&b'{') => {
                let end = s[self.i..]
                    .iter()
                    .position(|&x| x == b'}')
                    .map_or(s.len(), |p| self.i + p);
                let hex = String::from_utf8_lossy(&s[self.i + 2..end]).into_owned();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
                self.i = end + 1;
                return;
            }
            b'\\' | b'$' | b'"' => out.push(c),
            _ => {
                out.push(b'\\');
                out.push(c);
            }
        }
        self.i += 1;
    }

    /// Lexes an interpolated string body until `end` matches.
    fn interp(&mut self, end: impl Fn(&[u8], usize) -> bool, quotes: bool) -> Vec<Part> {
        let s = self.s;
        let mut parts = Vec::new();
        let mut lit = Vec::new();
        while self.i < s.len() && !end(s, self.i) {
            let c = s[self.i];
            if c == b'\\' {
                if !quotes && s.get(self.i + 1) == Some(&b'"') {
                    lit.push(b'\\');
                    self.i += 1;
                    continue;
                }
                self.escape(&mut lit);
            } else if c == b'$'
                && s.get(self.i + 1)
                    .is_some_and(|&n| is_id_start(n) && n != b'\\')
            {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                let start = self.i;
                self.i += 1;
                while self.i < s.len() && is_id(s[self.i]) && s[self.i] != b'\\' {
                    self.i += 1;
                }
                if s[self.i..].starts_with(b"->")
                    && s.get(self.i + 2).is_some_and(|&n| is_id_start(n))
                {
                    self.i += 2;
                    while self.i < s.len() && is_id(s[self.i]) && s[self.i] != b'\\' {
                        self.i += 1;
                    }
                } else if s.get(self.i) == Some(&b'[') {
                    if let Some(p) = s[self.i..].iter().position(|&x| x == b']') {
                        let inner = &s[self.i + 1..self.i + p];
                        let mut code = s[start..self.i].to_vec();
                        code.push(b'[');
                        if inner.iter().all(|c| c.is_ascii_alphabetic() || *c == b'_') {
                            code.push(b'\'');
                            code.extend_from_slice(inner);
                            code.push(b'\'');
                        } else {
                            code.extend_from_slice(inner);
                        }
                        code.push(b']');
                        self.i += p + 1;
                        parts.push(Part::Code(lex_code(&code)));
                        continue;
                    }
                }
                parts.push(Part::Code(lex_code(&s[start..self.i])));
            } else if c == b'{' && s.get(self.i + 1) == Some(&b'$') {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                let mut depth = 0;
                let start = self.i + 1;
                while self.i < s.len() {
                    match s[self.i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
                parts.push(Part::Code(lex_code(&s[start..self.i])));
                self.i += 1;
            } else {
                lit.push(c);
                self.i += 1;
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        parts
    }

    fn heredoc(&mut self) {
        let s = self.s;
        self.i += 3;
        while s[self.i] == b' ' {
            self.i += 1;
        }
        let nowdoc = s[self.i] == b'\'';
        let quoted = s[self.i] == b'"' || nowdoc;
        if quoted {
            self.i += 1;
        }
        let start = self.i;
        while self.i < s.len() && is_id(s[self.i]) {
            self.i += 1;
        }
        let label = s[start..self.i].to_vec();
        if quoted {
            self.i += 1;
        }
        while self.i < s.len() && s[self.i] != b'\n' {
            self.i += 1;
        }
        self.i += 1;
        // find the closing label: a line consisting of optional whitespace + label
        let body_start = self.i;
        let mut pos = body_start;
        let (body_end, indent, after) = loop {
            let line_end = s[pos..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(s.len(), |p| pos + p);
            let line = &s[pos..line_end];
            let trimmed_start = line
                .iter()
                .position(|&c| c != b' ' && c != b'\t')
                .unwrap_or(line.len());
            if line[trimmed_start..].starts_with(&label)
                && line
                    .get(trimmed_start + label.len())
                    .is_none_or(|c| !is_id(*c))
            {
                break (pos, trimmed_start, pos + trimmed_start + label.len());
            }
            if line_end >= s.len() {
                break (s.len(), 0, s.len());
            }
            pos = line_end + 1;
        };
        let raw_end = if body_end > body_start {
            body_end - 1
        } else {
            body_start
        };
        let body: Vec<u8> = s[body_start..raw_end].to_vec();
        // remove indentation
        let body: Vec<u8> = if indent > 0 {
            let mut out = Vec::new();
            for (n, line) in body.split(|&c| c == b'\n').enumerate() {
                if n > 0 {
                    out.push(b'\n');
                }
                let cut = line
                    .iter()
                    .take(indent)
                    .take_while(|c| **c == b' ' || **c == b'\t')
                    .count();
                out.extend_from_slice(&line[cut..]);
            }
            out
        } else {
            body
        };
        self.i = after;
        if nowdoc {
            self.out.push(T::Str(body));
        } else {
            let mut sub = Lexer {
                s: &body,
                i: 0,
                out: Vec::new(),
            };
            let parts = sub.interp(|_, _| false, false);
            self.out.push(T::IStr(parts));
        }
    }
}

fn lex_code(code: &[u8]) -> Vec<T> {
    let mut l = Lexer {
        s: code,
        i: 0,
        out: Vec::new(),
    };
    l.code();
    l.out
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

// ---- AST ----------------------------------------------------------------------

#[derive(Debug, Clone)]
enum E {
    Lit(Value),
    Interp(Vec<IPart>),
    Var(String),
    VarVar(Box<E>),
    Array(Vec<(Option<E>, E, bool)>),
    Prop(Box<E>, Box<E>),
    SProp(String, String),
    Index(Box<E>, Option<Box<E>>),
    Call(String, Vec<Arg>),
    CallExpr(Box<E>, Vec<Arg>),
    MCall(Box<E>, Box<E>, Vec<Arg>),
    SCall(String, String, Vec<Arg>),
    New(Box<E>, Vec<Arg>),
    Bin(&'static str, Box<E>, Box<E>),
    Not(Box<E>),
    Neg(Box<E>),
    Silence(Box<E>),
    Assign(Box<E>, Box<E>),
    AssignRef(Box<E>, Box<E>),
    AssignOp(&'static str, Box<E>, Box<E>),
    Cast(String, Box<E>),
    Ternary(Box<E>, Option<Box<E>>, Box<E>),
    InstanceOf(Box<E>, String),
    ClassConst(String, String),
    Const(String),
    Isset(Vec<E>),
    Empty(Box<E>),
    IncDec(bool, bool, Box<E>),
    Closure(Rc<Func>),
    List(Vec<Option<E>>),
    Clone(Box<E>),
    Exit(Option<Box<E>>),
    Throw(Box<E>),
    Print(Box<E>),
    Include(Box<E>),
}

#[derive(Debug, Clone)]
enum IPart {
    Lit(Vec<u8>),
    Expr(E),
}

#[derive(Debug, Clone)]
pub struct Arg {
    e: E,
    spread: bool,
    name: Option<String>,
}

#[derive(Debug, Clone)]
struct Param {
    name: String,
    default: Option<E>,
    by_ref: bool,
    variadic: bool,
    promote: bool,
}

#[derive(Debug, Clone)]
pub struct Func {
    name: String,
    params: Vec<Param>,
    body: Vec<S>,
    is_static: bool,
    uses: Vec<(String, bool)>,
}

#[derive(Debug, Clone)]
struct ClassDecl {
    name: String,
    parent: Option<String>,
    props: Vec<(String, Option<E>, bool)>,
    consts: Vec<(String, E)>,
    methods: Vec<Rc<Func>>,
}

#[derive(Debug, Clone)]
enum S {
    Expr(E),
    Echo(Vec<E>),
    Html(Vec<u8>),
    Return(Option<E>),
    If(Vec<(E, Vec<S>)>, Option<Vec<S>>),
    While(E, Vec<S>),
    DoWhile(Vec<S>, E),
    For(Vec<E>, Vec<E>, Vec<E>, Vec<S>),
    Foreach(E, Option<E>, E, bool, Vec<S>),
    Try(
        Vec<S>,
        Vec<(Vec<String>, Option<String>, Vec<S>)>,
        Option<Vec<S>>,
    ),
    Class(Rc<ClassDecl>),
    Func(Rc<Func>),
    Global(Vec<String>),
    Static(Vec<(String, Option<E>)>),
    Unset(Vec<E>),
    Break,
    Continue,
    Block(Vec<S>),
    Nop,
}

// ---- parser -------------------------------------------------------------------

struct Parser {
    t: Vec<T>,
    i: usize,
}

type PResult<X> = Result<X, String>;

impl Parser {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn peek_at(&self, n: usize) -> Option<&T> {
        self.t.get(self.i + n)
    }
    fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), Some(T::Op(x)) if *x == o)
    }
    fn is_id(&self, id: &str) -> bool {
        matches!(self.peek(), Some(T::Id(x)) if x.eq_ignore_ascii_case(id))
    }
    fn eat_op(&mut self, o: &str) -> bool {
        if self.is_op(o) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn eat_id(&mut self, id: &str) -> bool {
        if self.is_id(id) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, o: &str) -> PResult<()> {
        if self.eat_op(o) {
            Ok(())
        } else {
            Err(format!(
                "expected '{o}' at token {} got {:?}",
                self.i,
                self.peek()
            ))
        }
    }
    fn ident(&mut self) -> PResult<String> {
        match self.t.get(self.i).cloned() {
            Some(T::Id(s)) => {
                self.i += 1;
                Ok(s)
            }
            other => Err(format!("expected identifier, got {other:?}")),
        }
    }

    fn program(&mut self) -> PResult<Vec<S>> {
        let mut out = Vec::new();
        while self.peek().is_some() {
            out.push(self.stmt()?);
        }
        Ok(out)
    }

    fn block(&mut self) -> PResult<Vec<S>> {
        if self.eat_op("{") {
            let mut out = Vec::new();
            while !self.is_op("}") {
                if self.peek().is_none() {
                    return Err("unterminated block".into());
                }
                out.push(self.stmt()?);
            }
            self.i += 1;
            Ok(out)
        } else if self.eat_op(":") {
            // alternative syntax
            let mut out = Vec::new();
            while !(self.is_id("endif")
                || self.is_id("endforeach")
                || self.is_id("endwhile")
                || self.is_id("endfor")
                || self.is_id("else")
                || self.is_id("elseif"))
            {
                out.push(self.stmt()?);
            }
            if self.is_id("endif")
                || self.is_id("endforeach")
                || self.is_id("endwhile")
                || self.is_id("endfor")
            {
                self.i += 1;
                self.eat_op(";");
            }
            Ok(out)
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    fn semi(&mut self) {
        self.eat_op(";");
    }

    fn stmt(&mut self) -> PResult<S> {
        match self.peek().cloned() {
            Some(T::Html(h)) => {
                self.i += 1;
                Ok(S::Html(h))
            }
            Some(T::Op(";")) => {
                self.i += 1;
                Ok(S::Nop)
            }
            Some(T::Op("{")) => Ok(S::Block(self.block()?)),
            Some(T::Id(id)) => {
                let lc = id.to_ascii_lowercase();
                match lc.as_str() {
                    "echo" => {
                        self.i += 1;
                        let mut es = vec![self.expr()?];
                        while self.eat_op(",") {
                            es.push(self.expr()?);
                        }
                        self.semi();
                        Ok(S::Echo(es))
                    }
                    "return" => {
                        self.i += 1;
                        if self.eat_op(";") {
                            return Ok(S::Return(None));
                        }
                        let e = self.expr()?;
                        self.semi();
                        Ok(S::Return(Some(e)))
                    }
                    "if" => {
                        self.i += 1;
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        let body = self.block()?;
                        let mut arms = vec![(c, body)];
                        let mut els = None;
                        loop {
                            if self.eat_id("elseif") {
                                self.expect("(")?;
                                let c = self.expr()?;
                                self.expect(")")?;
                                arms.push((c, self.block()?));
                            } else if self.is_id("else")
                                && matches!(self.peek_at(1), Some(T::Id(x)) if x.eq_ignore_ascii_case("if"))
                            {
                                self.i += 2;
                                self.expect("(")?;
                                let c = self.expr()?;
                                self.expect(")")?;
                                arms.push((c, self.block()?));
                            } else if self.eat_id("else") {
                                els = Some(self.block()?);
                                break;
                            } else {
                                break;
                            }
                        }
                        Ok(S::If(arms, els))
                    }
                    "while" => {
                        self.i += 1;
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        Ok(S::While(c, self.block()?))
                    }
                    "do" => {
                        self.i += 1;
                        let b = self.block()?;
                        self.eat_id("while");
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        self.semi();
                        Ok(S::DoWhile(b, c))
                    }
                    "for" => {
                        self.i += 1;
                        self.expect("(")?;
                        let mut parts = [Vec::new(), Vec::new(), Vec::new()];
                        for (n, part) in parts.iter_mut().enumerate() {
                            let close = if n == 2 { ")" } else { ";" };
                            while !self.is_op(close) {
                                part.push(self.expr()?);
                                self.eat_op(",");
                            }
                            self.i += 1;
                        }
                        let [a, b, c] = parts;
                        Ok(S::For(a, b, c, self.block()?))
                    }
                    "foreach" => {
                        self.i += 1;
                        self.expect("(")?;
                        let subject = self.expr()?;
                        if !self.eat_id("as") {
                            return Err("expected as".into());
                        }
                        let mut by_ref = self.eat_op("&");
                        let first = self.expr()?;
                        let (k, v) = if self.eat_op("=>") {
                            by_ref = self.eat_op("&");
                            (Some(first), self.expr()?)
                        } else {
                            (None, first)
                        };
                        self.expect(")")?;
                        Ok(S::Foreach(subject, k, v, by_ref, self.block()?))
                    }
                    "try" => {
                        self.i += 1;
                        let body = self.block()?;
                        let mut catches = Vec::new();
                        let mut fin = None;
                        loop {
                            if self.eat_id("catch") {
                                self.expect("(")?;
                                let mut types = vec![self.ident()?];
                                while self.eat_op("|") {
                                    types.push(self.ident()?);
                                }
                                let var = match self.peek().cloned() {
                                    Some(T::Var(v)) => {
                                        self.i += 1;
                                        Some(v)
                                    }
                                    _ => None,
                                };
                                self.expect(")")?;
                                catches.push((types, var, self.block()?));
                            } else if self.eat_id("finally") {
                                fin = Some(self.block()?);
                            } else {
                                break;
                            }
                        }
                        Ok(S::Try(body, catches, fin))
                    }
                    "abstract" | "final" | "readonly" if matches!(self.peek_at(1), Some(T::Id(x)) if x.eq_ignore_ascii_case("class")) =>
                    {
                        self.i += 1;
                        self.stmt()
                    }
                    "class" | "interface" | "trait" => {
                        self.i += 1;
                        Ok(S::Class(Rc::new(self.class_decl()?)))
                    }
                    "enum" if matches!(self.peek_at(1), Some(T::Id(_))) => {
                        self.i += 1;
                        Ok(S::Class(Rc::new(self.class_decl()?)))
                    }
                    "function"
                        if matches!(self.peek_at(1), Some(T::Id(_)))
                            || (matches!(self.peek_at(1), Some(T::Op("&")))
                                && matches!(self.peek_at(2), Some(T::Id(_)))) =>
                    {
                        self.i += 1;
                        self.eat_op("&");
                        let name = self.ident()?;
                        Ok(S::Func(Rc::new(self.func_rest(name, false)?)))
                    }
                    "const" if matches!(self.peek_at(1), Some(T::Id(_))) => {
                        self.i += 1;
                        let mut defs = Vec::new();
                        loop {
                            let name = self.ident()?;
                            if !self.eat_op("=") {
                                return Err("expected = in const".into());
                            }
                            let value = self.expr()?;
                            let arg = |e| Arg {
                                e,
                                spread: false,
                                name: None,
                            };
                            defs.push(S::Expr(E::Call(
                                "define".into(),
                                vec![arg(E::Lit(Value::from(name.as_str()))), arg(value)],
                            )));
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                        self.semi();
                        Ok(S::Block(defs))
                    }
                    "global" => {
                        self.i += 1;
                        let mut v = Vec::new();
                        while let Some(T::Var(n)) = self.peek().cloned() {
                            self.i += 1;
                            v.push(n);
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                        self.semi();
                        Ok(S::Global(v))
                    }
                    "static" if matches!(self.peek_at(1), Some(T::Var(_))) => {
                        self.i += 1;
                        let mut v = Vec::new();
                        while let Some(T::Var(n)) = self.peek().cloned() {
                            self.i += 1;
                            let d = if self.eat_op("=") {
                                Some(self.expr()?)
                            } else {
                                None
                            };
                            v.push((n, d));
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                        self.semi();
                        Ok(S::Static(v))
                    }
                    "unset" => {
                        self.i += 1;
                        self.expect("(")?;
                        let mut v = Vec::new();
                        while !self.is_op(")") {
                            v.push(self.expr()?);
                            self.eat_op(",");
                        }
                        self.i += 1;
                        self.semi();
                        Ok(S::Unset(v))
                    }
                    "break" => {
                        self.i += 1;
                        if matches!(self.peek(), Some(T::Int(_))) {
                            self.i += 1;
                        }
                        self.semi();
                        Ok(S::Break)
                    }
                    "continue" => {
                        self.i += 1;
                        self.semi();
                        Ok(S::Continue)
                    }
                    "namespace" | "use" | "declare" => {
                        while !self.is_op(";") && !self.is_op("{") && self.peek().is_some() {
                            self.i += 1;
                        }
                        self.semi();
                        Ok(S::Nop)
                    }
                    _ => {
                        let e = self.expr()?;
                        self.semi();
                        Ok(S::Expr(e))
                    }
                }
            }
            _ => {
                let e = self.expr()?;
                self.semi();
                Ok(S::Expr(e))
            }
        }
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect("(")?;
        let mut out = Vec::new();
        while !self.is_op(")") {
            let mut promote = false;
            loop {
                if self.eat_id("public") || self.eat_id("private") || self.eat_id("protected") {
                    promote = true;
                } else if self.eat_id("readonly") {
                } else {
                    break;
                }
            }
            // type
            while !matches!(self.peek(), Some(T::Var(_) | T::Op("&" | "..."))) {
                if self.peek().is_none() {
                    return Err("bad param".into());
                }
                self.i += 1;
            }
            let by_ref = self.eat_op("&");
            let variadic = self.eat_op("...");
            let Some(T::Var(name)) = self.peek().cloned() else {
                return Err("param name".into());
            };
            self.i += 1;
            let default = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            out.push(Param {
                name,
                default,
                by_ref,
                variadic,
                promote,
            });
            self.eat_op(",");
        }
        self.i += 1;
        Ok(out)
    }

    fn func_rest(&mut self, name: String, is_static: bool) -> PResult<Func> {
        let params = self.params()?;
        let mut uses = Vec::new();
        if self.eat_id("use") {
            self.expect("(")?;
            while !self.is_op(")") {
                let r = self.eat_op("&");
                if let Some(T::Var(v)) = self.peek().cloned() {
                    self.i += 1;
                    uses.push((v, r));
                }
                self.eat_op(",");
            }
            self.i += 1;
        }
        if self.eat_op(":") {
            while !self.is_op("{") && !self.is_op(";") && !self.is_op("=>") {
                self.i += 1;
            }
        }
        let body = if self.is_op("{") {
            self.block()?
        } else {
            self.semi();
            Vec::new()
        };
        Ok(Func {
            name,
            params,
            body,
            is_static,
            uses,
        })
    }

    fn class_decl(&mut self) -> PResult<ClassDecl> {
        let name = self.ident()?;
        let mut parent = None;
        if self.eat_op(":") {
            self.ident()?;
        }
        if self.eat_id("extends") {
            parent = Some(self.ident()?);
            while self.eat_op(",") {
                self.ident()?;
            }
        }
        if self.eat_id("implements") {
            self.ident()?;
            while self.eat_op(",") {
                self.ident()?;
            }
        }
        self.expect("{")?;
        let mut c = ClassDecl {
            name,
            parent,
            props: Vec::new(),
            consts: Vec::new(),
            methods: Vec::new(),
        };
        while !self.eat_op("}") {
            let mut is_static = false;
            loop {
                if self.eat_id("public")
                    || self.eat_id("private")
                    || self.eat_id("protected")
                    || self.eat_id("var")
                    || self.eat_id("abstract")
                    || self.eat_id("final")
                    || self.eat_id("readonly")
                {
                } else if self.eat_id("static") {
                    is_static = true;
                } else {
                    break;
                }
            }
            if self.eat_id("use") {
                while !self.eat_op(";") {
                    self.i += 1;
                }
                continue;
            }
            if self.eat_id("const") || self.eat_id("case") {
                let n = self.ident()?;
                let v = if self.eat_op("=") {
                    self.expr()?
                } else {
                    E::Lit(Value::Null)
                };
                self.semi();
                c.consts.push((n, v));
                continue;
            }
            if self.eat_id("function") {
                self.eat_op("&");
                let n = self.ident()?;
                c.methods.push(Rc::new(self.func_rest(n, is_static)?));
                continue;
            }
            // property (maybe typed)
            while !matches!(self.peek(), Some(T::Var(_))) {
                if self.peek().is_none() || self.is_op("}") {
                    break;
                }
                self.i += 1;
            }
            while let Some(T::Var(n)) = self.peek().cloned() {
                self.i += 1;
                let d = if self.eat_op("=") {
                    Some(self.expr()?)
                } else {
                    None
                };
                c.props.push((n, d, is_static));
                if !self.eat_op(",") {
                    break;
                }
            }
            self.semi();
        }
        Ok(c)
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        self.expect("(")?;
        let mut out = Vec::new();
        while !self.is_op(")") {
            if self.is_op("...") && matches!(self.peek_at(1), Some(T::Op(")"))) {
                self.i += 1;
                continue;
            }
            let spread = self.eat_op("...");
            let mut name = None;
            if let (Some(T::Id(n)), Some(T::Op(":"))) =
                (self.peek().cloned(), self.peek_at(1).cloned())
            {
                if !matches!(self.peek_at(2), Some(T::Op(":"))) {
                    name = Some(n);
                    self.i += 2;
                }
            }
            out.push(Arg {
                e: self.expr()?,
                spread,
                name,
            });
            self.eat_op(",");
        }
        self.i += 1;
        Ok(out)
    }

    fn expr(&mut self) -> PResult<E> {
        self.assignment()
    }

    fn assignment(&mut self) -> PResult<E> {
        let lhs = self.ternary()?;
        if self.eat_op("=") {
            if self.eat_op("&") {
                let rhs = self.assignment()?;
                return Ok(E::AssignRef(Box::new(lhs), Box::new(rhs)));
            }
            let rhs = self.assignment()?;
            return Ok(E::Assign(Box::new(lhs), Box::new(rhs)));
        }
        for op in [".=", "+=", "-=", "*=", "/=", "%=", "??="] {
            if self.eat_op(op) {
                let rhs = self.assignment()?;
                return Ok(E::AssignOp(op, Box::new(lhs), Box::new(rhs)));
            }
        }
        Ok(lhs)
    }

    fn ternary(&mut self) -> PResult<E> {
        let c = self.coalesce()?;
        if self.eat_op("?") {
            if self.eat_op(":") {
                let e = self.assignment()?;
                return Ok(E::Ternary(Box::new(c), None, Box::new(e)));
            }
            let a = self.assignment()?;
            self.expect(":")?;
            let b = self.assignment()?;
            return Ok(E::Ternary(Box::new(c), Some(Box::new(a)), Box::new(b)));
        }
        Ok(c)
    }

    fn coalesce(&mut self) -> PResult<E> {
        let l = self.binary(0)?;
        if self.eat_op("??") {
            let r = self.coalesce()?;
            return Ok(E::Bin("??", Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn binary(&mut self, min: u8) -> PResult<E> {
        const LEVELS: &[&[&str]] = &[
            &["||", "or"],
            &["&&", "and"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!=", "===", "!==", "<>", "<=>"],
            &["<", "<=", ">", ">="],
            &["."],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if usize::from(min) >= LEVELS.len() {
            return self.unary();
        }
        let mut l = self.binary(min + 1)?;
        loop {
            let found = LEVELS[usize::from(min)].iter().find(|o| match self.peek() {
                Some(T::Op(x)) => x == *o,
                Some(T::Id(x)) => x.eq_ignore_ascii_case(o),
                _ => false,
            });
            let Some(op) = found else {
                break;
            };
            self.i += 1;
            let r = self.binary(min + 1)?;
            l = E::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> PResult<E> {
        if self.eat_op("!") {
            return Ok(E::Not(Box::new(self.unary()?)));
        }
        if self.eat_op("-") {
            return Ok(E::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op("+") {
            return self.unary();
        }
        if self.eat_op("@") {
            return Ok(E::Silence(Box::new(self.unary()?)));
        }
        if self.eat_op("&") {
            return self.unary();
        }
        if self.eat_op("(cast") {
            let t = self.ident()?;
            return Ok(E::Cast(t, Box::new(self.unary()?)));
        }
        if self.eat_op("++") {
            return Ok(E::IncDec(true, true, Box::new(self.unary()?)));
        }
        if self.eat_op("--") {
            return Ok(E::IncDec(true, false, Box::new(self.unary()?)));
        }
        if self.is_id("new") {
            self.i += 1;
            let class = if let Some(T::Var(_)) = self.peek() {
                self.postfix_noncall()?
            } else if self.is_id("class") {
                self.i += 1;
                let args = if self.is_op("(") {
                    self.args()?
                } else {
                    Vec::new()
                };
                let c = self.class_decl_anon()?;
                return Ok(E::New(Box::new(E::Lit(Value::from(c.name.as_str()))), args)).inspect(
                    |_e| {
                        ANON.with(|a| a.borrow_mut().push(c));
                    },
                );
            } else {
                E::Lit(Value::from(self.ident()?.as_str()))
            };
            let args = if self.is_op("(") {
                self.args()?
            } else {
                Vec::new()
            };
            let e = E::New(Box::new(class), args);
            return self.postfix_ops(e);
        }
        if self.eat_id("clone") {
            return Ok(E::Clone(Box::new(self.unary()?)));
        }
        if self.eat_id("print") {
            return Ok(E::Print(Box::new(self.assignment()?)));
        }
        if self.eat_id("throw") {
            return Ok(E::Throw(Box::new(self.assignment()?)));
        }
        if self.is_id("include")
            || self.is_id("require")
            || self.is_id("include_once")
            || self.is_id("require_once")
        {
            self.i += 1;
            return Ok(E::Include(Box::new(self.assignment()?)));
        }
        let e = self.postfix()?;
        if self.eat_id("instanceof") {
            let c = match self.peek().cloned() {
                Some(T::Id(c)) => {
                    self.i += 1;
                    c
                }
                _ => return Err("instanceof".into()),
            };
            return Ok(E::InstanceOf(Box::new(e), c));
        }
        Ok(e)
    }

    fn class_decl_anon(&mut self) -> PResult<ClassDecl> {
        let n = ANON.with(|a| a.borrow().len());
        let mut parent = None;
        if self.eat_id("extends") {
            parent = Some(self.ident()?);
        }
        if self.eat_id("implements") {
            self.ident()?;
        }
        // reuse class_decl body parsing by injecting a name
        let name = format!("class@anonymous{n}");
        self.t.insert(self.i, T::Id(name));
        let mut c = self.class_decl()?;
        c.parent = parent;
        Ok(c)
    }

    fn postfix_noncall(&mut self) -> PResult<E> {
        let mut e = self.primary()?;
        loop {
            if self.is_op("->") || self.is_op("?->") {
                self.i += 1;
                let name = self.member_name()?;
                e = E::Prop(Box::new(e), Box::new(name));
            } else if self.eat_op("[") {
                let idx = if self.is_op("]") {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                self.expect("]")?;
                e = E::Index(Box::new(e), idx);
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn member_name(&mut self) -> PResult<E> {
        match self.peek().cloned() {
            Some(T::Id(n)) => {
                self.i += 1;
                Ok(E::Lit(Value::from(n.as_str())))
            }
            Some(T::Var(v)) => {
                self.i += 1;
                Ok(E::Var(v))
            }
            Some(T::Op("{")) => {
                self.i += 1;
                let e = self.expr()?;
                self.expect("}")?;
                Ok(e)
            }
            other => Err(format!("member name {other:?}")),
        }
    }

    fn postfix(&mut self) -> PResult<E> {
        let e = self.primary()?;
        self.postfix_ops(e)
    }

    fn postfix_ops(&mut self, mut e: E) -> PResult<E> {
        loop {
            if self.is_op("->") || self.is_op("?->") {
                self.i += 1;
                let name = self.member_name()?;
                if self.is_op("(") {
                    let args = self.args()?;
                    e = E::MCall(Box::new(e), Box::new(name), args);
                } else {
                    e = E::Prop(Box::new(e), Box::new(name));
                }
            } else if self.eat_op("[") {
                let idx = if self.is_op("]") {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                self.expect("]")?;
                e = E::Index(Box::new(e), idx);
            } else if self.eat_op("{") {
                let idx = self.expr()?;
                self.expect("}")?;
                e = E::Index(Box::new(e), Some(Box::new(idx)));
            } else if self.is_op("(")
                && matches!(
                    e,
                    E::Var(_) | E::Index(..) | E::Prop(..) | E::Closure(_) | E::CallExpr(..)
                )
            {
                let args = self.args()?;
                e = E::CallExpr(Box::new(e), args);
            } else if self.is_op("::") && matches!(e, E::Var(_)) {
                self.i += 1;
                let m = self.ident()?;
                if m.eq_ignore_ascii_case("class") {
                    e = E::Call(
                        "get_class".into(),
                        vec![Arg {
                            e,
                            spread: false,
                            name: None,
                        }],
                    );
                } else if self.is_op("(") {
                    let args = self.args()?;
                    e = E::MCall(Box::new(e), Box::new(E::Lit(Value::from(m.as_str()))), args);
                } else {
                    e = E::Lit(Value::Null);
                }
            } else if self.eat_op("++") {
                e = E::IncDec(false, true, Box::new(e));
            } else if self.eat_op("--") {
                e = E::IncDec(false, false, Box::new(e));
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> PResult<E> {
        let Some(t) = self.peek().cloned() else {
            return Err("unexpected end".into());
        };
        self.i += 1;
        Ok(match t {
            T::Var(v) => E::Var(v),
            T::Int(i) => E::Lit(Value::Int(i)),
            T::Float(f) => E::Lit(Value::Float(f)),
            T::Str(s) => E::Lit(Value::from(s.as_slice())),
            T::IStr(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        Part::Lit(l) => out.push(IPart::Lit(l)),
                        Part::Code(toks) => {
                            let mut sub = Parser { t: toks, i: 0 };
                            out.push(IPart::Expr(sub.expr()?));
                        }
                    }
                }
                E::Interp(out)
            }
            T::Op("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                e
            }
            T::Op("[") => {
                self.i -= 1;
                self.array_lit("[", "]")?
            }
            T::Op("$") => {
                if self.eat_op("{") {
                    let e = self.expr()?;
                    self.expect("}")?;
                    E::VarVar(Box::new(e))
                } else {
                    E::VarVar(Box::new(self.primary()?))
                }
            }
            T::Id(id) => {
                let lc = id.to_ascii_lowercase();
                match lc.as_str() {
                    "true" => return Ok(E::Lit(Value::Bool(true))),
                    "false" => return Ok(E::Lit(Value::Bool(false))),
                    "null" => return Ok(E::Lit(Value::Null)),
                    "array" if self.is_op("(") => {
                        return self.array_lit("(", ")");
                    }
                    "list" if self.is_op("(") => {
                        self.expect("(")?;
                        let mut v = Vec::new();
                        while !self.is_op(")") {
                            if self.is_op(",") {
                                v.push(None);
                            } else {
                                v.push(Some(self.expr()?));
                            }
                            self.eat_op(",");
                        }
                        self.i += 1;
                        return Ok(E::List(v));
                    }
                    "isset" => {
                        let args = self.args()?;
                        return Ok(E::Isset(args.into_iter().map(|a| a.e).collect()));
                    }
                    "empty" => {
                        self.expect("(")?;
                        let e = self.expr()?;
                        self.expect(")")?;
                        return Ok(E::Empty(Box::new(e)));
                    }
                    "exit" | "die" => {
                        if self.is_op("(") {
                            self.i += 1;
                            if self.eat_op(")") {
                                return Ok(E::Exit(None));
                            }
                            let e = self.expr()?;
                            self.expect(")")?;
                            return Ok(E::Exit(Some(Box::new(e))));
                        }
                        return Ok(E::Exit(None));
                    }
                    "function" | "fn" => {
                        let is_arrow = lc == "fn";
                        self.eat_op("&");
                        if is_arrow {
                            let params = self.params()?;
                            if self.eat_op(":") {
                                while !self.is_op("=>") {
                                    self.i += 1;
                                }
                            }
                            self.expect("=>")?;
                            let e = self.assignment()?;
                            return Ok(E::Closure(Rc::new(Func {
                                name: "{closure}".into(),
                                params,
                                body: vec![S::Return(Some(e))],
                                is_static: false,
                                uses: vec![("*".into(), false)],
                            })));
                        }
                        return Ok(E::Closure(Rc::new(
                            self.func_rest("{closure}".into(), false)?,
                        )));
                    }
                    "static" if self.is_id("function") || self.is_id("fn") => {
                        return self.primary();
                    }
                    _ => {}
                }
                if self.eat_op("::") {
                    match self.peek().cloned() {
                        Some(T::Var(v)) => {
                            self.i += 1;
                            return Ok(E::SProp(id, v));
                        }
                        Some(T::Id(m)) => {
                            self.i += 1;
                            if self.is_op("(") {
                                let args = self.args()?;
                                return Ok(E::SCall(id, m, args));
                            }
                            return Ok(E::ClassConst(id, m));
                        }
                        other => return Err(format!("after :: {other:?}")),
                    }
                }
                if self.is_op("(") {
                    let args = self.args()?;
                    return Ok(E::Call(id, args));
                }
                E::Const(id)
            }
            other => return Err(format!("unexpected token {other:?}")),
        })
    }

    fn array_lit(&mut self, open: &str, close: &str) -> PResult<E> {
        self.expect(open)?;
        let mut items = Vec::new();
        while !self.is_op(close) {
            if self.eat_op("...") {
                let e = self.expr()?;
                items.push((Some(E::Lit(Value::from("..."))), e, false));
                self.eat_op(",");
                continue;
            }
            let by_ref = self.eat_op("&");
            let v = self.expr()?;
            if self.eat_op("=>") {
                let r = self.eat_op("&");
                let val = self.expr()?;
                items.push((Some(v), val, r));
            } else {
                items.push((None, v, by_ref));
            }
            self.eat_op(",");
        }
        self.i += 1;
        Ok(E::Array(items))
    }
}

thread_local! {
    static ANON: std::cell::RefCell<Vec<ClassDecl>> = const { std::cell::RefCell::new(Vec::new()) };
}

// ---- interpreter -------------------------------------------------------------

/// Control flow.
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

/// A call frame.
pub struct Frame {
    vars: HashMap<String, Value>,
    globals: Vec<String>,
    this: Option<Obj>,
    class: Option<String>,
    is_global: bool,
    file: PathBuf,
}

impl Frame {
    fn global(file: PathBuf) -> Self {
        Self {
            vars: HashMap::new(),
            globals: Vec::new(),
            this: None,
            class: None,
            is_global: true,
            file,
        }
    }
}

fn is_global_var(f: &Frame, n: &str) -> bool {
    f.is_global || f.globals.iter().any(|g| g == n)
}

fn get_var(h: &mut TestHost, f: &Frame, n: &str) -> Value {
    if n == "this" {
        return f.this.clone().map(Value::Object).unwrap_or_default();
    }
    if n == "_SERVER" {
        return Value::Array(ZArray::from_pairs(h.server_vars.iter().map(|(k, v)| {
            (
                ArrayKey::from_bytes(k.as_bytes()),
                Value::from(v.as_slice()),
            )
        })));
    }
    if n == "GLOBALS" {
        return Value::Array(ZArray::from_pairs(
            h.globals
                .iter()
                .map(|(k, v)| (ArrayKey::from_bytes(k.as_bytes()), v.clone())),
        ));
    }
    let v = if is_global_var(f, n) {
        h.globals.get(n).cloned()
    } else {
        f.vars.get(n).cloned()
    };
    v.unwrap_or_default()
}

fn set_var(h: &mut TestHost, f: &mut Frame, n: &str, v: Value) {
    let slot = if is_global_var(f, n) {
        h.globals.entry(n.to_owned()).or_default()
    } else {
        f.vars.entry(n.to_owned()).or_default()
    };
    if let Value::Reference(r) = slot {
        r.set(v.deref());
    } else {
        *slot = v;
    }
}

fn err(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::Error(msg.into())
}

fn to_str(h: &mut TestHost, v: &Value) -> RtResult<Vec<u8>> {
    Ok(v.to_zstr(h)?.as_bytes().to_vec())
}

fn truthy(h: &mut TestHost, v: &Value) -> RtResult<bool> {
    v.to_bool(h)
}

/// Runs a script.
fn builtin_hook(h: &mut TestHost, name: &str, args: &[Value]) -> Option<RtResult<Value>> {
    let mut f = Frame::global(PathBuf::new());
    builtin(h, &mut f, &name.to_ascii_lowercase(), args, &[]).transpose()
}

fn method_hook(h: &mut TestHost, obj: &Obj, name: &str, args: &[Value]) -> Option<RtResult<Value>> {
    let mut refs = Vec::new();
    internal_method(h, obj, name, args, &mut refs).transpose()
}

/// Serves one request of an emulated `php -S` server on its own thread
/// (fresh extension globals, like a separate process).
fn serve_cli(
    server: &CliServer,
    port: u16,
    request: Vec<u8>,
) -> (Vec<u8>, HashMap<String, Vec<u8>>) {
    let server = server.clone();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || serve_cli_request(&server, port, &request))
        .expect("spawn server thread")
        .join()
        .unwrap_or_else(|_| {
            (
                b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n".to_vec(),
                HashMap::new(),
            )
        })
}

fn reason_phrase(code: i64) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn serve_cli_request(
    server: &CliServer,
    port: u16,
    request: &[u8],
) -> (Vec<u8>, HashMap<String, Vec<u8>>) {
    let head_end = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(request.len(), |p| p + 4);
    let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
    let mut lines = head.split('\n').map(|l| l.trim_end_matches('\r'));
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("GET").to_owned();
    let uri = parts.next().unwrap_or("/").to_owned();
    let protocol = parts.next().unwrap_or("HTTP/1.1").to_owned();
    let mut headers = Vec::new();
    for l in lines {
        if let Some((n, v)) = l.split_once(':') {
            headers.push((n.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let mut body = request.get(head_end..).unwrap_or_default().to_vec();
    if let Some((_, len)) = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
    {
        if let Ok(len) = len.parse::<usize>() {
            body.truncate(len);
        }
    }

    php_soap::globals::request_init();
    let mut h = TestHost::new();
    let path = server.doc_root.join(&server.script);
    h.base_dir.clone_from(&server.doc_root);
    h.current_file.clone_from(&path);
    h.script = path.to_string_lossy().into_owned();
    let (path_part, query) = uri.split_once('?').unwrap_or((uri.as_str(), ""));
    for (k, v) in [
        ("REQUEST_METHOD", method.as_str()),
        ("REQUEST_URI", uri.as_str()),
        ("SERVER_PROTOCOL", protocol.as_str()),
        ("QUERY_STRING", query),
        ("PHP_SELF", path_part),
        ("SCRIPT_NAME", path_part),
        ("SERVER_NAME", "localhost"),
        ("DOCUMENT_ROOT", &server.doc_root.to_string_lossy()),
    ] {
        h.server_vars.insert(k.to_owned(), v.as_bytes().to_vec());
    }
    h.server_vars
        .insert("SERVER_PORT".into(), port.to_string().into_bytes());
    for (n, v) in &headers {
        let key = n.to_ascii_uppercase().replace('-', "_");
        let key = if key == "CONTENT_TYPE" || key == "CONTENT_LENGTH" {
            key
        } else {
            format!("HTTP_{key}")
        };
        h.server_vars.insert(key, v.as_bytes().to_vec());
    }
    h.request_headers = headers;
    h.request_body = Some(body);
    let code = format!("<?php {} ?>", server.code);
    if let Err(e) = run_file(&mut h, &path, code.as_bytes()) {
        h.report(&e);
    }
    let out = h.finish();

    let mut status = h.response_code.unwrap_or(200);
    let mut status_line: Option<String> = None;
    let mut sent: Vec<String> = vec![format!("X-Powered-By: PHP/{}", php_soap::PHP_VERSION)];
    for line in std::mem::take(&mut h.headers) {
        if line.starts_with("HTTP/") {
            if let Some(c) = line.split(' ').nth(1).and_then(|c| c.parse().ok()) {
                status = c;
            }
            status_line = Some(line);
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("location") && status != 201 && !(300..400).contains(&status) {
            status = 302;
        }
        if let Some(slot) = sent.iter_mut().find(|l| {
            l.split_once(':')
                .is_some_and(|(n, _)| n.eq_ignore_ascii_case(name))
        }) {
            *slot = line;
        } else {
            sent.push(line);
        }
    }
    if !sent
        .iter()
        .any(|l| l.to_ascii_lowercase().starts_with("content-type:"))
    {
        sent.push("Content-type: text/html; charset=UTF-8".into());
    }
    let mut resp = status_line
        .filter(|_| h.response_code.is_none())
        .unwrap_or_else(|| format!("HTTP/1.1 {status} {}", reason_phrase(status)));
    resp.push_str("\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nConnection: close\r\n");
    for l in sent {
        resp.push_str(&l);
        resp.push_str("\r\n");
    }
    resp.push_str("\r\n");
    let mut resp = resp.into_bytes();
    resp.extend_from_slice(&out);
    (resp, std::mem::take(&mut h.files))
}

pub fn run_file(h: &mut TestHost, path: &Path, code: &[u8]) -> RtResult<()> {
    h.builtin_hook = Some(builtin_hook);
    h.method_hook = Some(method_hook);
    h.net.cli_handler = Some(serve_cli);
    let toks = Lexer::lex(code);
    let mut p = Parser { t: toks, i: 0 };
    let prog = p.program().map_err(|e| err(format!("parse error: {e}")))?;
    let mut frame = Frame::global(path.to_path_buf());
    hoist(h, &prog);
    match exec_block(h, &mut frame, &prog)? {
        Flow::Return(_) | Flow::Normal | Flow::Break | Flow::Continue => Ok(()),
    }
}

/// Registers classes and functions declared at the top level.
fn hoist(h: &mut TestHost, prog: &[S]) {
    for s in prog {
        match s {
            S::Class(c) => declare_class(h, c),
            S::Func(f) => declare_function(h, f),
            _ => {}
        }
    }
}

fn declare_function(h: &mut TestHost, f: &Rc<Func>) {
    h.param_names.insert(
        f.name.to_ascii_lowercase(),
        f.params.iter().map(|p| p.name.clone()).collect(),
    );
    let fr = Rc::clone(f);
    h.add_function(&f.name, move |h, args| {
        call_user(h, &fr, None, None, args, &mut Vec::new())
    });
}

fn declare_class(h: &mut TestHost, c: &Rc<ClassDecl>) {
    if h.classes.contains_key(&c.name.to_ascii_lowercase()) {
        return;
    }
    let mut props = Vec::new();
    for (n, d, is_static) in &c.props {
        if *is_static {
            continue;
        }
        let v = match d {
            Some(e) => {
                let mut fr = Frame::global(PathBuf::new());
                eval(h, &mut fr, e).unwrap_or_default()
            }
            None => Value::Null,
        };
        props.push((n.clone(), v));
    }
    let mut methods: Vec<(String, MethodFn)> = Vec::new();
    for m in &c.methods {
        h.param_names.insert(
            format!(
                "{}::{}",
                c.name.to_ascii_lowercase(),
                m.name.to_ascii_lowercase()
            ),
            m.params.iter().map(|p| p.name.clone()).collect(),
        );
        // promoted constructor parameters are declared properties
        if m.name.eq_ignore_ascii_case("__construct") {
            for p in m.params.iter().filter(|p| p.promote) {
                if !props.iter().any(|(n, _): &(String, Value)| *n == p.name) {
                    props.push((p.name.clone(), Value::Null));
                }
            }
        }
        let mr = Rc::clone(m);
        let cname = c.name.clone();
        methods.push((
            m.name.clone(),
            Rc::new(move |h: &mut TestHost, this: &Obj, args: &[Value]| {
                let this_opt = if mr.is_static {
                    None
                } else {
                    Some(this.clone())
                };
                call_user(h, &mr, this_opt, Some(cname.clone()), args, &mut Vec::new())
            }),
        ));
    }
    for (n, e) in &c.consts {
        let mut fr = Frame::global(PathBuf::new());
        let v = eval(h, &mut fr, e).unwrap_or_default();
        h.globals
            .insert(format!("{}::{}", c.name.to_ascii_lowercase(), n), v);
    }
    h.add_class(UserClass {
        name: c.name.clone(),
        parent: c.parent.clone(),
        props,
        methods,
    });
}

fn call_user(
    h: &mut TestHost,
    f: &Func,
    this: Option<Obj>,
    class: Option<String>,
    args: &[Value],
    out_refs: &mut Vec<(usize, Value)>,
) -> RtResult<Value> {
    if h.depth > 2000 {
        let bytes = h.memory_limit();
        return Err(RuntimeError::Fatal(format!(
            "Allowed memory size of {bytes} bytes exhausted (tried to allocate 65536 bytes)"
        )));
    }
    h.depth += 1;
    let r = call_user_inner(h, f, this, class, args, out_refs);
    h.depth -= 1;
    r
}

fn call_user_inner(
    h: &mut TestHost,
    f: &Func,
    this: Option<Obj>,
    class: Option<String>,
    args: &[Value],
    out_refs: &mut Vec<(usize, Value)>,
) -> RtResult<Value> {
    let mut frame = Frame {
        vars: HashMap::new(),
        globals: Vec::new(),
        this: this.clone(),
        class,
        is_global: false,
        file: PathBuf::new(),
    };
    for (i, p) in f.params.iter().enumerate() {
        if p.variadic {
            let rest: Vec<Value> = args.iter().skip(i).cloned().collect();
            frame
                .vars
                .insert(p.name.clone(), Value::Array(ZArray::from_values(rest)));
            break;
        }
        let v = match args.get(i) {
            Some(v) => v.clone(),
            None => {
                if let Some(d) = &p.default {
                    eval(h, &mut frame, d)?
                } else {
                    let fname = match &frame.class {
                        Some(c) => format!("{c}::{}", f.name),
                        None => f.name.clone(),
                    };
                    let required = f
                        .params
                        .iter()
                        .filter(|p| p.default.is_none() && !p.variadic)
                        .count();
                    return Err(RuntimeError::ArgumentCountError(format!(
                    "Too few arguments to function {fname}(), {} passed and {} {required} expected",
                    args.len(),
                    if required == f.params.len() { "exactly" } else { "at least" }
                )));
                }
            }
        };
        if p.promote {
            if let Some(t) = &this {
                h.write_property(t, p.name.as_bytes(), v.deref())?;
            }
        }
        frame.vars.insert(p.name.clone(), v);
    }
    let r = exec_block(h, &mut frame, &f.body)?;
    for (i, p) in f.params.iter().enumerate() {
        if p.by_ref {
            if let Some(v) = frame.vars.get(&p.name) {
                out_refs.push((i, v.clone()));
            }
        }
    }
    Ok(match r {
        Flow::Return(v) => v,
        _ => Value::Null,
    })
}

fn exec_block(h: &mut TestHost, f: &mut Frame, stmts: &[S]) -> RtResult<Flow> {
    for s in stmts {
        match exec(h, f, s)? {
            Flow::Normal => {}
            other => return Ok(other),
        }
    }
    Ok(Flow::Normal)
}

fn exec(h: &mut TestHost, f: &mut Frame, s: &S) -> RtResult<Flow> {
    match s {
        S::Nop => {}
        S::Html(b) => h.echo(b),
        S::Expr(e) => {
            eval(h, f, e)?;
        }
        S::Echo(es) => {
            for e in es {
                let v = eval(h, f, e)?;
                let s = to_str(h, &v)?;
                h.echo(&s);
            }
        }
        S::Return(e) => {
            let v = match e {
                Some(e) => eval(h, f, e)?,
                None => Value::Null,
            };
            return Ok(Flow::Return(v));
        }
        S::If(arms, els) => {
            for (c, body) in arms {
                let v = eval(h, f, c)?;
                if truthy(h, &v)? {
                    return exec_block(h, f, body);
                }
            }
            if let Some(b) = els {
                return exec_block(h, f, b);
            }
        }
        S::While(c, body) => loop {
            let v = eval(h, f, c)?;
            if !truthy(h, &v)? {
                break;
            }
            match exec_block(h, f, body)? {
                Flow::Break => break,
                Flow::Return(v) => return Ok(Flow::Return(v)),
                _ => {}
            }
        },
        S::DoWhile(body, c) => loop {
            match exec_block(h, f, body)? {
                Flow::Break => break,
                Flow::Return(v) => return Ok(Flow::Return(v)),
                _ => {}
            }
            let v = eval(h, f, c)?;
            if !truthy(h, &v)? {
                break;
            }
        },
        S::For(init, cond, step, body) => {
            for e in init {
                eval(h, f, e)?;
            }
            loop {
                let mut ok = true;
                for e in cond {
                    let v = eval(h, f, e)?;
                    ok = truthy(h, &v)?;
                }
                if !ok {
                    break;
                }
                match exec_block(h, f, body)? {
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    _ => {}
                }
                for e in step {
                    eval(h, f, e)?;
                }
            }
        }
        S::Foreach(subject, k, v, _by_ref, body) => {
            let sv = eval(h, f, subject)?.deref();
            let pairs: Vec<(Value, Value)> = match &sv {
                Value::Array(a) => a.iter().map(|(k, v)| (k.to_value(), v.clone())).collect(),
                Value::Object(o) => h
                    .property_table(o)
                    .iter()
                    .map(|(k, v)| (k.to_value(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            for (kk, vv) in pairs {
                if let Some(k) = k {
                    assign(h, f, k, kk)?;
                }
                assign(h, f, v, vv.deref())?;
                match exec_block(h, f, body)? {
                    Flow::Break => break,
                    Flow::Return(r) => return Ok(Flow::Return(r)),
                    _ => {}
                }
            }
        }
        S::Try(body, catches, fin) => {
            let r = exec_block(h, f, body);
            let r = match r {
                Err(e) if !matches!(e, RuntimeError::Fatal(_)) && !h.is_unwind_exit(&e) => {
                    let exc = h.exception_object(&e);
                    let mut handled = None;
                    if let Some(o) = &exc {
                        let class = o.class_name().to_string_lossy();
                        for (types, var, cbody) in catches {
                            if types.iter().any(|t| {
                                h.is_a(&class, t)
                                    || t.eq_ignore_ascii_case("Throwable")
                                    || (t.eq_ignore_ascii_case("Exception")
                                        && h.is_a(&class, "Exception"))
                            }) {
                                if let Some(var) = var {
                                    set_var(h, f, var, Value::Object(o.clone()));
                                }
                                handled = Some(exec_block(h, f, cbody));
                                break;
                            }
                        }
                    }
                    handled.unwrap_or(Err(e))
                }
                other => other,
            };
            if let Some(fb) = fin {
                exec_block(h, f, fb)?;
            }
            return r;
        }
        S::Class(c) => declare_class(h, c),
        S::Func(fun) => declare_function(h, fun),
        S::Global(vars) => {
            for v in vars {
                f.globals.push(v.clone());
            }
        }
        S::Static(vars) => {
            for (n, d) in vars {
                let key = format!("static::{n}");
                if !h.globals.contains_key(&key) {
                    let v = match d {
                        Some(e) => eval(h, f, e)?,
                        None => Value::Null,
                    };
                    h.globals.insert(key.clone(), v);
                }
                let v = h.globals.get(&key).cloned().unwrap_or_default();
                f.vars.insert(n.clone(), v);
            }
        }
        S::Unset(es) => {
            for e in es {
                match e {
                    E::Var(n) => {
                        if is_global_var(f, n) {
                            h.globals.remove(n);
                        } else {
                            f.vars.remove(n);
                        }
                    }
                    E::Index(base, Some(idx)) => {
                        let mut b = eval(h, f, base)?.deref();
                        let k = eval(h, f, idx)?;
                        if let Value::Array(a) = &mut b {
                            if let Some(key) = k.to_array_key(h)? {
                                a.make_mut().remove(&key);
                            }
                        }
                        assign(h, f, base, b)?;
                    }
                    E::Prop(o, n) => {
                        if let Value::Object(o) = eval(h, f, o)?.deref() {
                            let n = eval(h, f, n)?;
                            let n = to_str(h, &n)?;
                            h.unset_property(&o, &n)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        S::Break => return Ok(Flow::Break),
        S::Continue => return Ok(Flow::Continue),
        S::Block(b) => return exec_block(h, f, b),
    }
    Ok(Flow::Normal)
}

fn assign(h: &mut TestHost, f: &mut Frame, target: &E, v: Value) -> RtResult<()> {
    match target {
        E::Var(n) => {
            set_var(h, f, n, v);
            Ok(())
        }
        E::VarVar(e) => {
            let n = eval(h, f, e)?;
            let n = String::from_utf8_lossy(&to_str(h, &n)?).into_owned();
            set_var(h, f, &n, v);
            Ok(())
        }
        E::Prop(o, n) => {
            let ov = eval(h, f, o)?.deref();
            let name = eval(h, f, n)?;
            let name = to_str(h, &name)?;
            match ov {
                Value::Object(o) => h.write_property(&o, &name, v),
                other => Err(err(format!(
                    "Attempt to assign property \"{}\" on {}",
                    String::from_utf8_lossy(&name),
                    other.type_name()
                ))),
            }
        }
        E::SProp(c, n) => {
            h.globals
                .insert(format!("{}::${n}", c.to_ascii_lowercase()), v);
            Ok(())
        }
        E::Index(base, Some(idx)) if matches!(&**base, E::Var(n) if n == "GLOBALS") => {
            let k = eval(h, f, idx)?;
            let k = String::from_utf8_lossy(&to_str(h, &k)?).into_owned();
            h.globals.insert(k, v);
            Ok(())
        }
        E::Index(base, Some(idx)) if matches!(&**base, E::Var(n) if n == "_SERVER") => {
            let k = eval(h, f, idx)?;
            let k = String::from_utf8_lossy(&to_str(h, &k)?).into_owned();
            let v = to_str(h, &v)?;
            h.server_vars.insert(k, v);
            Ok(())
        }
        E::Index(base, idx) => {
            let mut b = match eval(h, f, base) {
                Ok(b) => b.deref(),
                Err(_) => Value::Null,
            };
            if matches!(b, Value::Null) {
                b = Value::Array(ZArray::new());
            }
            match &mut b {
                Value::Array(a) => {
                    match idx {
                        Some(i) => {
                            let k = eval(h, f, i)?;
                            let key = k.to_array_key(h)?.unwrap_or(ArrayKey::Int(0));
                            a.make_mut().insert(key, v);
                        }
                        None => {
                            let _ = a.make_mut().push(v);
                        }
                    }
                    assign(h, f, base, b)
                }
                Value::Object(_) => Err(err("Cannot use object as array")),
                _ => Err(err("Cannot use a scalar value as an array")),
            }
        }
        E::List(items) => {
            if let Value::Array(a) = v {
                for (i, it) in items.iter().enumerate() {
                    if let Some(it) = it {
                        let val = a
                            .get_int(i64::try_from(i).unwrap_or(0))
                            .cloned()
                            .unwrap_or_default();
                        assign(h, f, it, val)?;
                    }
                }
            }
            Ok(())
        }
        E::Array(items) => {
            if let Value::Array(a) = v {
                for (i, (k, it, _)) in items.iter().enumerate() {
                    let val = match k {
                        Some(k) => {
                            let kv = eval(h, f, k)?;
                            let key = kv.to_array_key(h)?.unwrap_or(ArrayKey::Int(0));
                            a.get(&key).cloned().unwrap_or_default()
                        }
                        None => a
                            .get_int(i64::try_from(i).unwrap_or(0))
                            .cloned()
                            .unwrap_or_default(),
                    };
                    assign(h, f, it, val)?;
                }
            }
            Ok(())
        }
        _ => Err(err("cannot assign")),
    }
}

/// Parameter names (and internal defaults) of a callee for named arguments.
fn callee_params(h: &TestHost, class: Option<&str>, name: &str) -> Vec<(String, Value)> {
    let lc = name.to_ascii_lowercase();
    let mut cur = class.map(str::to_owned);
    while let Some(c) = cur.clone() {
        if let Some(p) = h
            .param_names
            .get(&format!("{}::{lc}", c.to_ascii_lowercase()))
        {
            return p.iter().map(|n| (n.clone(), Value::Null)).collect();
        }
        if let Some(def) = php_soap::classes::find_class(&c) {
            if let Some(m) = def
                .methods
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
            {
                return m
                    .params
                    .iter()
                    .map(|p| {
                        let d = match p.default {
                            Some("\"\"") => Value::from(""),
                            Some("false") => Value::Bool(false),
                            Some("true") => Value::Bool(true),
                            Some("[]") => Value::Array(ZArray::new()),
                            _ => Value::Null,
                        };
                        (p.name.to_owned(), d)
                    })
                    .collect();
            }
        }
        cur = h
            .classes
            .get(&c.to_ascii_lowercase())
            .and_then(|x| x.parent.clone())
            .or_else(|| {
                php_soap::classes::find_class(&c).and_then(|d| d.parent.map(str::to_owned))
            });
    }
    if class.is_none() {
        if let Some(p) = h.param_names.get(&lc) {
            return p.iter().map(|n| (n.clone(), Value::Null)).collect();
        }
    }
    Vec::new()
}

fn eval_args_for(
    h: &mut TestHost,
    f: &mut Frame,
    args: &[Arg],
    class: Option<&str>,
    name: &str,
) -> RtResult<Vec<Value>> {
    if !args.iter().any(|a| a.name.is_some()) {
        return eval_args(h, f, args);
    }
    let params = callee_params(h, class, name);
    let mut out: Vec<Option<Value>> = Vec::new();
    for a in args {
        let v = eval(h, f, &a.e)?;
        match &a.name {
            None => out.push(Some(v)),
            Some(n) => {
                let Some(pos) = params.iter().position(|(p, _)| p == n) else {
                    return Err(err(format!("Unknown named parameter ${n}")));
                };
                if out.len() <= pos {
                    out.resize(pos + 1, None);
                }
                out[pos] = Some(v);
            }
        }
    }
    Ok(out
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            v.unwrap_or_else(|| params.get(i).map(|(_, d)| d.clone()).unwrap_or_default())
        })
        .collect())
}

fn eval_args(h: &mut TestHost, f: &mut Frame, args: &[Arg]) -> RtResult<Vec<Value>> {
    let mut out = Vec::new();
    for a in args {
        let v = eval(h, f, &a.e)?;
        if a.spread {
            if let Value::Array(arr) = v.deref() {
                out.extend(arr.values().cloned());
            }
        } else {
            out.push(v);
        }
    }
    Ok(out)
}

fn class_of_name(h: &mut TestHost, f: &Frame, name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "self" | "static" => f.class.clone().unwrap_or_default(),
        "parent" => {
            let c = f.class.clone().unwrap_or_default();
            h.classes
                .get(&c.to_ascii_lowercase())
                .and_then(|c| c.parent.clone())
                .unwrap_or_default()
        }
        _ => name.to_owned(),
    }
}

fn eval(h: &mut TestHost, f: &mut Frame, e: &E) -> RtResult<Value> {
    Ok(match e {
        E::Lit(v) => v.clone(),
        E::Interp(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p {
                    IPart::Lit(l) => out.extend_from_slice(l),
                    IPart::Expr(e) => {
                        let v = eval(h, f, e)?;
                        out.extend_from_slice(&to_str(h, &v)?);
                    }
                }
            }
            Value::from(out.as_slice())
        }
        E::Var(n) => get_var(h, f, n),
        E::VarVar(e) => {
            let n = eval(h, f, e)?;
            let n = String::from_utf8_lossy(&to_str(h, &n)?).into_owned();
            get_var(h, f, &n)
        }
        E::Const(c) => constant(h, f, c)?,
        E::ClassConst(c, n) => {
            if n.eq_ignore_ascii_case("class") {
                Value::from(class_of_name(h, f, c).as_str())
            } else {
                let cl = class_of_name(h, f, c);
                h.globals
                    .get(&format!("{}::{}", cl.to_ascii_lowercase(), n))
                    .cloned()
                    .unwrap_or_default()
            }
        }
        E::SProp(c, n) => {
            let cl = class_of_name(h, f, c);
            h.globals
                .get(&format!("{}::${n}", cl.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default()
        }
        E::Array(items) => {
            let mut a = ZArray::new();
            for (k, v, _) in items {
                let val = eval(h, f, v)?;
                match k {
                    Some(E::Lit(Value::Str(s))) if s.as_bytes() == b"..." => {
                        if let Value::Array(src) = val.deref() {
                            for (kk, vv) in src.iter() {
                                match kk {
                                    ArrayKey::Int(_) => {
                                        let _ = a.make_mut().push(vv.clone());
                                    }
                                    ArrayKey::Str(_) => {
                                        a.make_mut().insert(kk.clone(), vv.clone());
                                    }
                                }
                            }
                        }
                    }
                    Some(k) => {
                        let kv = eval(h, f, k)?;
                        let key = kv.to_array_key(h)?.unwrap_or(ArrayKey::Int(0));
                        a.make_mut().insert(key, val.deref());
                    }
                    None => {
                        let _ = a.make_mut().push(val.deref());
                    }
                }
            }
            Value::Array(a)
        }
        E::Prop(o, n) => {
            let ov = eval(h, f, o)?.deref();
            let name = eval(h, f, n)?;
            let name = to_str(h, &name)?;
            match ov {
                Value::Object(o) => h.read_property(&o, &name)?.unwrap_or_default(),
                _ => Value::Null,
            }
        }
        E::Index(b, i) => {
            let bv = eval(h, f, b)?.deref();
            let Some(i) = i else {
                return Err(err("Cannot use [] for reading"));
            };
            let k = eval(h, f, i)?;
            match bv {
                Value::Array(a) => match k.to_array_key(h)? {
                    Some(key) => a.get(&key).cloned().unwrap_or_default(),
                    None => Value::Null,
                },
                Value::Str(s) => {
                    let idx = k.to_long(h)?;
                    let idx = if idx < 0 {
                        i64::try_from(s.len()).unwrap_or(0) + idx
                    } else {
                        idx
                    };
                    usize::try_from(idx)
                        .ok()
                        .and_then(|i| s.as_bytes().get(i).copied())
                        .map_or(Value::from(""), |c| Value::from(&[c][..]))
                }
                _ => Value::Null,
            }
        }
        E::Assign(t, v) => {
            let val = eval(h, f, v)?.deref();
            let val = val;
            assign(h, f, t, val.clone())?;
            val
        }
        E::AssignRef(t, v) => {
            let val = eval(h, f, v)?;
            assign(h, f, t, val.clone())?;
            val
        }
        E::AssignOp(op, t, v) => {
            let cur = eval(h, f, t).unwrap_or_default();
            if *op == "??=" {
                if !matches!(cur.deref(), Value::Null) {
                    return Ok(cur);
                }
                let r = eval(h, f, v)?;
                assign(h, f, t, r.clone())?;
                return Ok(r);
            }
            let r = eval(h, f, v)?;
            let nv = binop(h, &op[..op.len() - 1], &cur, &r)?;
            assign(h, f, t, nv.clone())?;
            nv
        }
        E::IncDec(pre, inc, t) => {
            let cur = eval(h, f, t)?;
            let mut n = cur.deref();
            if *inc {
                ops::increment(&mut n, h)?;
            } else {
                ops::decrement(&mut n, h)?;
            }
            assign(h, f, t, n.clone())?;
            if *pre {
                n
            } else {
                cur
            }
        }
        E::Bin(op, l, r) => {
            match *op {
                "&&" | "and" => {
                    let a = eval(h, f, l)?;
                    if !truthy(h, &a)? {
                        return Ok(Value::Bool(false));
                    }
                    let b = eval(h, f, r)?;
                    return Ok(Value::Bool(truthy(h, &b)?));
                }
                "||" | "or" => {
                    let a = eval(h, f, l)?;
                    if truthy(h, &a)? {
                        return Ok(Value::Bool(true));
                    }
                    let b = eval(h, f, r)?;
                    return Ok(Value::Bool(truthy(h, &b)?));
                }
                "??" => {
                    let a = eval(h, f, l).unwrap_or_default();
                    if !matches!(a.deref(), Value::Null) {
                        return Ok(a);
                    }
                    return eval(h, f, r);
                }
                _ => {}
            }
            let a = eval(h, f, l)?;
            let b = eval(h, f, r)?;
            binop(h, op, &a.deref(), &b.deref())?
        }
        E::Not(x) => {
            let v = eval(h, f, x)?;
            Value::Bool(!truthy(h, &v)?)
        }
        E::Neg(x) => {
            let v = eval(h, f, x)?;
            ops::mul(&v, &Value::Int(-1), h)?
        }
        E::Silence(x) => eval(h, f, x).unwrap_or_default(),
        E::Cast(t, x) => {
            let v = eval(h, f, x)?.deref();
            match t.as_str() {
                "string" => Value::from(to_str(h, &v)?.as_slice()),
                "int" | "integer" => Value::Int(v.to_long(h)?),
                "float" | "double" => Value::Float(v.to_double(h)?),
                "bool" | "boolean" => Value::Bool(truthy(h, &v)?),
                "array" => match v {
                    Value::Array(a) => Value::Array(a),
                    Value::Object(o) => Value::Array(h.property_table(&o)),
                    Value::Null => Value::Array(ZArray::new()),
                    other => Value::Array(ZArray::from_values([other])),
                },
                "object" => match v {
                    Value::Object(o) => Value::Object(o),
                    Value::Array(a) => {
                        let o = h.new_object("stdClass");
                        for (k, val) in a.iter() {
                            let name = match k {
                                ArrayKey::Int(i) => i.to_string().into_bytes(),
                                ArrayKey::Str(s) => s.as_bytes().to_vec(),
                            };
                            h.write_property(&o, &name, val.clone())?;
                        }
                        Value::Object(o)
                    }
                    Value::Null => Value::Object(h.new_object("stdClass")),
                    other => {
                        let o = h.new_object("stdClass");
                        h.write_property(&o, b"scalar", other)?;
                        Value::Object(o)
                    }
                },
                _ => v,
            }
        }
        E::Ternary(c, a, b) => {
            let cv = eval(h, f, c)?;
            if truthy(h, &cv)? {
                match a {
                    Some(a) => eval(h, f, a)?,
                    None => cv,
                }
            } else {
                eval(h, f, b)?
            }
        }
        E::InstanceOf(x, c) => {
            let v = eval(h, f, x)?.deref();
            let c = class_of_name(h, f, c);
            Value::Bool(match v {
                Value::Object(o) => h.is_a(&o.class_name().to_string_lossy(), &c),
                _ => false,
            })
        }
        E::Isset(es) => {
            for x in es {
                let v = eval(h, f, x).unwrap_or_default();
                if matches!(v.deref(), Value::Null) {
                    return Ok(Value::Bool(false));
                }
            }
            Value::Bool(true)
        }
        E::Empty(x) => {
            let v = eval(h, f, x).unwrap_or_default();
            Value::Bool(!truthy(h, &v)?)
        }
        E::Closure(func) => {
            let name = format!("{{closure#{}}}", h.functions.len());
            let fr = Rc::clone(func);
            let captured: Vec<(String, Value)> = if func.uses.iter().any(|(n, _)| n == "*") {
                f.vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            } else {
                func.uses
                    .iter()
                    .map(|(n, _)| (n.clone(), get_var(h, f, n)))
                    .collect()
            };
            let this = f.this.clone();
            let class = f.class.clone();
            h.add_function(&name, move |h, args| {
                let mut func2 = (*fr).clone();
                let mut pre = Vec::new();
                for (n, v) in &captured {
                    pre.push(S::Expr(E::Assign(
                        Box::new(E::Var(n.clone())),
                        Box::new(E::Lit(v.clone())),
                    )));
                }
                pre.extend(func2.body);
                func2.body = pre;
                call_user(
                    h,
                    &func2,
                    this.clone(),
                    class.clone(),
                    args,
                    &mut Vec::new(),
                )
            });
            Value::from(name.as_str())
        }
        E::List(_) => Value::Null,
        E::Clone(x) => {
            let v = eval(h, f, x)?.deref();
            match v {
                Value::Object(o) => {
                    let n = h.new_object(&o.class_name().to_string_lossy());
                    let props = h.property_table(&o);
                    *TestHost::props_of(&n).borrow_mut() = props;
                    Value::Object(n)
                }
                other => other,
            }
        }
        E::Exit(x) => {
            if let Some(x) = x {
                let v = eval(h, f, x)?;
                if let Value::Str(s) = v.deref() {
                    h.echo(s.as_bytes());
                }
            }
            return Err(h.bailout());
        }
        E::Throw(x) => {
            let v = eval(h, f, x)?.deref();
            return Err(RuntimeError::Thrown(v));
        }
        E::Print(x) => {
            let v = eval(h, f, x)?;
            let s = to_str(h, &v)?;
            h.echo(&s);
            Value::Int(1)
        }
        E::Include(x) => {
            let v = eval(h, f, x)?;
            let p = String::from_utf8_lossy(&to_str(h, &v)?).into_owned();
            let path = if Path::new(&p).is_absolute() {
                PathBuf::from(&p)
            } else {
                h.base_dir.join(&p)
            };
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(code) = h.include_override.get(name).cloned() {
                    let old = std::mem::replace(&mut h.current_file, path.clone());
                    let r = run_file(h, &path, code.as_bytes());
                    h.current_file = old;
                    r?;
                    return Ok(Value::Int(1));
                }
            }
            let Ok(code) = std::fs::read(&path) else {
                return Err(err(format!("include({p}): Failed to open stream")));
            };
            let old = std::mem::replace(&mut h.current_file, path.clone());
            let toks = Lexer::lex(&code);
            let mut parser = Parser { t: toks, i: 0 };
            let prog = parser
                .program()
                .map_err(|e| err(format!("parse error in {p}: {e}")))?;
            hoist(h, &prog);
            let r = exec_block(h, f, &prog);
            h.current_file = old;
            if let Flow::Return(v) = r? {
                return Ok(v);
            }
            Value::Int(1)
        }
        E::Call(name, args) => {
            let lc = name.to_ascii_lowercase();
            let vals = eval_args_for(h, f, args, None, name)?;
            if let Some(r) = builtin(h, f, &lc, &vals, args)? {
                return Ok(r);
            }
            match h.functions.get(&lc).cloned() {
                Some((_, func)) => func(h, &vals)?,
                None => return Err(err(format!("Call to undefined function {name}()"))),
            }
        }
        E::CallExpr(callee, args) => {
            let c = eval(h, f, callee)?;
            let vals = eval_args(h, f, args)?;
            h.call_callable(&c, &vals)?
        }
        E::New(class, args) => {
            let cv = eval(h, f, class)?;
            let raw = String::from_utf8_lossy(&to_str(h, &cv)?).into_owned();
            let cname = class_of_name(h, f, &raw);
            ANON.with(|a| {
                let decls: Vec<ClassDecl> = a.borrow_mut().drain(..).collect();
                for d in decls {
                    declare_class(h, &Rc::new(d));
                }
            });
            let vals = eval_args_for(h, f, args, Some(&cname), "__construct")?;
            new_object(h, &cname, &vals, args)?
        }
        E::MCall(o, n, args) => {
            let ov = eval(h, f, o)?.deref();
            let name = eval(h, f, n)?;
            let name = String::from_utf8_lossy(&to_str(h, &name)?).into_owned();
            let cls = match &ov {
                Value::Object(o) => Some(o.class_name().to_string_lossy()),
                _ => None,
            };
            let vals = eval_args_for(h, f, args, cls.as_deref(), &name)?;
            let Value::Object(obj) = ov else {
                return Err(err(format!(
                    "Call to a member function {name}() on {}",
                    ov.type_name()
                )));
            };
            let mut refs = Vec::new();
            let r = method_call(h, &obj, &name, &vals, &mut refs)?;
            for (i, v) in refs {
                if let Some(a) = args.get(i) {
                    assign(h, f, &a.e, v)?;
                }
            }
            r
        }
        E::SCall(c, m, args) => {
            let vals = eval_args(h, f, args)?;
            let lc = c.to_ascii_lowercase();
            if lc == "parent" || lc == "self" || lc == "static" {
                let cl = class_of_name(h, f, c);
                let this = f.this.clone();
                match this {
                    Some(t) => {
                        let mut refs = Vec::new();
                        return method_call_from(h, &t, &cl, m, &vals, &mut refs);
                    }
                    None => {
                        return static_call(h, &cl, m, &vals);
                    }
                }
            }
            static_call(h, c, m, &vals)?
        }
    })
}

fn static_call(h: &mut TestHost, class: &str, m: &str, args: &[Value]) -> RtResult<Value> {
    let lc = class.to_ascii_lowercase();
    if let Some(uc) = h.classes.get(&lc).cloned() {
        if let Some((_, mf)) = uc.methods.iter().find(|(n, _)| n.eq_ignore_ascii_case(m)) {
            let dummy = h.new_object(class);
            return mf(h, &dummy, args);
        }
    }
    Err(err(format!("Call to undefined method {class}::{m}()")))
}

fn binop(h: &mut TestHost, op: &str, a: &Value, b: &Value) -> RtResult<Value> {
    Ok(match op {
        "." => ops::concat(a, b, h)?,
        "+" => ops::add(a, b, h)?,
        "-" => ops::sub(a, b, h)?,
        "*" => ops::mul(a, b, h)?,
        "/" => ops::div(a, b, h)?,
        "%" => ops::modulo(a, b, h)?,
        "|" => ops::bit_or(a, b, h)?,
        "&" => ops::bit_and(a, b, h)?,
        "^" => ops::bit_xor(a, b, h)?,
        "==" => Value::Bool(ops::is_equal(a, b, h)?),
        "!=" | "<>" => Value::Bool(!ops::is_equal(a, b, h)?),
        "===" => Value::Bool(ops::is_identical(a, b)?),
        "!==" => Value::Bool(!ops::is_identical(a, b)?),
        "<" => Value::Bool(ops::compare(a, b, h)? < 0),
        "<=" => Value::Bool(ops::compare(a, b, h)? <= 0),
        ">" => Value::Bool(ops::compare(a, b, h)? > 0),
        ">=" => Value::Bool(ops::compare(a, b, h)? >= 0),
        "<=>" => Value::Int(i64::from(ops::compare(a, b, h)?)),
        _ => return Err(err(format!("operator {op}"))),
    })
}

fn constant(h: &mut TestHost, f: &Frame, c: &str) -> RtResult<Value> {
    Ok(match c {
        "__DIR__" => Value::from(
            h.current_file
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .as_str(),
        ),
        "__FILE__" => Value::from(h.current_file.to_string_lossy().as_ref()),
        "__LINE__" => Value::Int(1),
        "__CLASS__" => Value::from(f.class.clone().unwrap_or_default().as_str()),
        "PHP_EOL" => Value::from("\n"),
        "PHP_INT_MAX" => Value::Int(i64::MAX),
        "PHP_INT_MIN" => Value::Int(i64::MIN),
        "PHP_INT_SIZE" => Value::Int(8),
        "PHP_VERSION" => Value::from(php_soap::PHP_VERSION),
        "PHP_OS" | "PHP_OS_FAMILY" => Value::from("Linux"),
        "PHP_SHLIB_SUFFIX" => Value::from("so"),
        "E_ALL" => Value::Int(32767),
        "E_WARNING" => Value::Int(2),
        "E_NOTICE" => Value::Int(8),
        "E_DEPRECATED" => Value::Int(8192),
        "LIBXML_VERSION" => Value::Int(20914),
        "NAN" => Value::Float(f64::NAN),
        "INF" => Value::Float(f64::INFINITY),
        "M_PI" => Value::Float(std::f64::consts::PI),
        _ => {
            if let Some((_, v)) = php_soap::consts::php_constants()
                .into_iter()
                .find(|(n, _)| *n == c)
            {
                return Ok(v);
            }
            if let Some(v) = h.globals.get(&format!("const::{c}")) {
                return Ok(v.clone());
            }
            return Err(err(format!("Undefined constant \"{c}\"")));
        }
    })
}

fn bytes(v: Option<&Value>) -> Option<Vec<u8>> {
    match v.map(Value::deref) {
        Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
        Some(Value::Null) | None => None,
        Some(other) => Some(match other {
            Value::Int(i) => i.to_string().into_bytes(),
            Value::Bool(true) => b"1".to_vec(),
            Value::Bool(false) => Vec::new(),
            Value::Float(d) => php_runtime::float::double_to_string(d, 14).into_bytes(),
            _ => Vec::new(),
        }),
    }
}

/// Instantiates a class (internal constructors for SOAP classes).
pub fn new_object(
    h: &mut TestHost,
    class: &str,
    args: &[Value],
    _exprs: &[Arg],
) -> RtResult<Value> {
    if !h.class_exists(class) {
        return Err(err(format!("Class \"{class}\" not found")));
    }
    let o = h.new_object(class);
    let mut refs = Vec::new();
    match h.find_user_method(class, "__construct") {
        Some(m) => {
            m(h, &o, args)?;
        }
        None => {
            internal_method(h, &o, "__construct", args, &mut refs)?;
        }
    }
    Ok(Value::Object(o))
}

/// Dispatches a method call (user methods first, then internal ones).
pub fn method_call(
    h: &mut TestHost,
    obj: &Obj,
    name: &str,
    args: &[Value],
    refs: &mut Vec<(usize, Value)>,
) -> RtResult<Value> {
    let class = obj.class_name().to_string_lossy();
    method_call_from(h, obj, &class, name, args, refs)
}

fn method_call_from(
    h: &mut TestHost,
    obj: &Obj,
    class: &str,
    name: &str,
    args: &[Value],
    refs: &mut Vec<(usize, Value)>,
) -> RtResult<Value> {
    if let Some(m) = h.find_user_method(class, name) {
        return m(h, obj, args);
    }
    if let Some(v) = internal_method(h, obj, name, args, refs)? {
        Ok(v)
    } else {
        if let Some(m) = h.find_user_method(class, "__call") {
            let a = ZArray::from_values(args.iter().cloned());
            return m(h, obj, &[Value::from(name), Value::Array(a)]);
        }
        Err(err(format!("Call to undefined method {class}::{name}()")))
    }
}

/// Internal methods of the SOAP and exception classes; `None` when the
/// method does not exist.
#[allow(clippy::too_many_lines)]
pub fn internal_method(
    h: &mut TestHost,
    o: &Obj,
    name: &str,
    a: &[Value],
    refs: &mut Vec<(usize, Value)>,
) -> RtResult<Option<Value>> {
    use php_soap::{client, fault, objects, server};
    let class = o.class_name().to_string_lossy();
    let lc = name.to_ascii_lowercase();
    let arg = |i: usize| a.get(i).map(Value::deref);
    if h.is_a(&class, "SoapClient") {
        let r = match lc.as_str() {
            "__construct" => {
                let wsdl = bytes(a.first());
                let opts = match arg(1) {
                    Some(Value::Array(x)) => Some(x),
                    _ => None,
                };
                client::construct(h, o, wsdl.as_deref(), opts.as_ref())?;
                Value::Null
            }
            "__soapcall" => {
                let fname = bytes(a.first()).unwrap_or_default();
                let args = match arg(1) {
                    Some(Value::Array(x)) => x,
                    _ => ZArray::new(),
                };
                let opts = match arg(2) {
                    Some(Value::Array(x)) => Some(x),
                    _ => None,
                };
                let mut out = Value::Null;
                let want = a.len() > 4;
                let r = client::soap_call(
                    h,
                    o,
                    &fname,
                    &args,
                    opts.as_ref(),
                    a.get(3),
                    if want { Some(&mut out) } else { None },
                );
                if want {
                    refs.push((4, out));
                }
                r?
            }
            "__getlastrequest" => client::get_last(h, o, "__last_request"),
            "__getlastresponse" => client::get_last(h, o, "__last_response"),
            "__getlastrequestheaders" => client::get_last(h, o, "__last_request_headers"),
            "__getlastresponseheaders" => client::get_last(h, o, "__last_response_headers"),
            "__getfunctions" => client::get_functions(h, o),
            "__gettypes" => client::get_types(h, o),
            "__dorequest" => {
                let version = arg(3).map_or(Ok(1), |v| v.to_long(h))?;
                let one_way = arg(4).is_some_and(|v| matches!(v, Value::Bool(true)));
                client::do_request_method(
                    h,
                    o,
                    &bytes(a.first()).unwrap_or_default(),
                    &bytes(a.get(1)).unwrap_or_default(),
                    &bytes(a.get(2)).unwrap_or_default(),
                    version,
                    one_way,
                    bytes(a.get(5)).as_deref(),
                )?
            }
            "__setcookie" => {
                client::set_cookie(
                    h,
                    o,
                    &bytes(a.first()).unwrap_or_default(),
                    bytes(a.get(1)).as_deref(),
                );
                Value::Null
            }
            "__getcookies" => client::get_cookies(h, o),
            "__setsoapheaders" => Value::Bool(client::set_soap_headers(h, o, a.first())?),
            "__setlocation" => client::set_location(h, o, bytes(a.first()).as_deref()),
            "__call" => {
                let args = match arg(1) {
                    Some(Value::Array(x)) => x,
                    _ => ZArray::new(),
                };
                client::call(h, o, &bytes(a.first()).unwrap_or_default(), &args)?
            }
            _ => {
                let args = ZArray::from_values(a.iter().cloned());
                client::call(h, o, name.as_bytes(), &args)?
            }
        };
        return Ok(Some(r));
    }
    if h.is_a(&class, "SoapServer") {
        let r = match lc.as_str() {
            "__construct" => {
                let opts = match arg(1) {
                    Some(Value::Array(x)) => Some(x),
                    _ => None,
                };
                server::construct(h, o, bytes(a.first()).as_deref(), opts.as_ref())?;
                Value::Null
            }
            "addfunction" => {
                server::add_function(h, o, a.first().unwrap_or(&Value::Null))?;
                Value::Null
            }
            "setclass" => {
                let cname = bytes(a.first()).unwrap_or_default();
                let cname = String::from_utf8_lossy(&cname).into_owned();
                if !h.class_exists(&cname) {
                    return Err(RuntimeError::TypeError(format!(
                        "SoapServer::setClass(): Argument #1 ($class) must be a valid class name, {cname} given"
                    )));
                }
                let canon = h.canonical(&cname);
                server::set_class(h, o, ZStr::from(canon.as_str()), a[1..].to_vec())?;
                Value::Null
            }
            "setobject" => {
                if let Some(Value::Object(x)) = arg(0) {
                    server::set_object(h, o, x)?;
                }
                Value::Null
            }
            "setpersistence" => {
                let mode = match arg(0) {
                    Some(v) => v.to_long(h)?,
                    None => 0,
                };
                server::set_persistence(h, o, mode)?;
                Value::Null
            }
            "handle" => {
                server::handle(h, o, bytes(a.first()).as_deref())?;
                Value::Null
            }
            "getfunctions" => server::get_functions(h, o)?,
            "fault" => {
                server::fault(
                    h,
                    o,
                    &bytes(a.first()).unwrap_or_default(),
                    &bytes(a.get(1)).unwrap_or_default(),
                    bytes(a.get(2)).as_deref(),
                    a.get(3),
                    bytes(a.get(4)).as_deref(),
                    &bytes(a.get(5)).unwrap_or_default(),
                )?;
                Value::Null
            }
            "addsoapheader" => {
                if let Some(Value::Object(x)) = arg(0) {
                    server::add_soap_header(h, o, x)?;
                }
                Value::Null
            }
            "__getlastresponse" => server::get_last_response(h, o)?,
            _ => return Ok(None),
        };
        return Ok(Some(r));
    }
    if h.is_a(&class, "SoapFault") && lc == "__construct" {
        let actor = bytes(a.get(2));
        let name = bytes(a.get(4));
        fault::construct(
            h,
            o,
            a.first().unwrap_or(&Value::Null),
            &bytes(a.get(1)).unwrap_or_default(),
            actor.as_deref(),
            a.get(3),
            name.as_deref(),
            a.get(5),
        )?;
        return Ok(Some(Value::Null));
    }
    if h.is_a(&class, "SoapFault") && lc == "__tostring" {
        return Ok(Some(Value::from(fault::to_string(h, o)?.as_slice())));
    }
    if h.is_a(&class, "SoapVar") && lc == "__construct" {
        let enc = match arg(1) {
            Some(Value::Null) | None => None,
            Some(v) => Some(v.to_long(h)?),
        };
        objects::soap_var_construct(
            h,
            o,
            a.first().unwrap_or(&Value::Null),
            enc,
            bytes(a.get(2)).as_deref(),
            bytes(a.get(3)).as_deref(),
            bytes(a.get(4)).as_deref(),
            bytes(a.get(5)).as_deref(),
        )?;
        return Ok(Some(Value::Null));
    }
    if h.is_a(&class, "SoapParam") && lc == "__construct" {
        objects::soap_param_construct(
            h,
            o,
            a.first().unwrap_or(&Value::Null),
            &bytes(a.get(1)).unwrap_or_default(),
        )?;
        return Ok(Some(Value::Null));
    }
    if h.is_a(&class, "SoapHeader") && lc == "__construct" {
        let actor = match arg(4) {
            Some(Value::Str(s)) => Some(objects::Actor::Uri(s.as_bytes().to_vec())),
            Some(Value::Int(i)) => Some(objects::Actor::Code(i)),
            _ => None,
        };
        let mu = match arg(3) {
            Some(v) => v.to_bool(h)?,
            None => false,
        };
        objects::soap_header_construct(
            h,
            o,
            &bytes(a.first()).unwrap_or_default(),
            &bytes(a.get(1)).unwrap_or_default(),
            a.get(2),
            mu,
            actor,
        )?;
        return Ok(Some(Value::Null));
    }
    if h.is_a(&class, "Exception") || h.is_a(&class, "Error") {
        let r = match lc.as_str() {
            "__construct" => {
                if let Some(m) = a.first() {
                    h.write_slot(o, "message", m.clone());
                }
                if let Some(c) = a.get(1) {
                    h.write_slot(o, "code", c.clone());
                }
                Value::Null
            }
            "getmessage" => h.read_slot(o, "message").unwrap_or_default(),
            "getcode" => h.read_slot(o, "code").unwrap_or_default(),
            "getfile" => h.read_slot(o, "file").unwrap_or_default(),
            "getline" => h.read_slot(o, "line").unwrap_or_default(),
            "getprevious" => h.read_slot(o, "previous").unwrap_or_default(),
            "gettrace" => Value::Array(ZArray::new()),
            "gettraceasstring" => Value::from("#0 {main}"),
            "__tostring" => {
                let m = h.read_slot(o, "message").unwrap_or_default();
                let m = to_str(h, &m)?;
                Value::from(
                    format!(
                        "{class}: {} in %s:%d\nStack trace:\n#0 {{main}}",
                        String::from_utf8_lossy(&m)
                    )
                    .as_str(),
                )
            }
            _ => return Ok(None),
        };
        return Ok(Some(r));
    }
    if lc == "__construct" {
        return Ok(Some(Value::Null));
    }
    Ok(None)
}

/// Builtin functions; `None` when unknown.
#[allow(clippy::too_many_lines)]
fn builtin(
    h: &mut TestHost,
    f: &mut Frame,
    name: &str,
    a: &[Value],
    exprs: &[Arg],
) -> RtResult<Option<Value>> {
    let arg = |i: usize| a.get(i).map(Value::deref).unwrap_or_default();
    let s = |h: &mut TestHost, i: usize| -> RtResult<Vec<u8>> {
        to_str(h, &a.get(i).map(Value::deref).unwrap_or_default())
    };
    let int =
        |h: &mut TestHost, i: usize| -> RtResult<i64> { a.get(i).map_or(Ok(0), |v| v.to_long(h)) };
    Ok(Some(match name {
        "var_dump" => {
            for v in a {
                h.var_dump(&v.deref());
            }
            Value::Null
        }
        "print_r" => {
            let out = php_runtime::dump::print_r(&arg(0), h)?;
            if a.get(1).is_some_and(|v| matches!(v, Value::Bool(true))) {
                return Ok(Some(Value::from(out.as_slice())));
            }
            h.echo(&out);
            Value::Bool(true)
        }
        "var_export" => {
            let out = php_runtime::dump::var_export(&arg(0), h)?;
            if a.get(1).is_some_and(|v| matches!(v, Value::Bool(true))) {
                return Ok(Some(Value::from(out.as_slice())));
            }
            h.echo(&out);
            Value::Null
        }
        "printf" | "sprintf" => {
            let fmt = s(h, 0)?;
            let mut out = Vec::new();
            let mut ai = 1;
            let mut i = 0;
            while i < fmt.len() {
                if fmt[i] == b'%' && i + 1 < fmt.len() {
                    let mut j = i + 1;
                    while j < fmt.len() && !fmt[j].is_ascii_alphabetic() && fmt[j] != b'%' {
                        j += 1;
                    }
                    let spec = fmt.get(j).copied().unwrap_or(b's');
                    match spec {
                        b'%' => out.push(b'%'),
                        b'd' => out.extend_from_slice(int(h, ai)?.to_string().as_bytes()),
                        _ => out.extend_from_slice(&s(h, ai)?),
                    }
                    if spec != b'%' {
                        ai += 1;
                    }
                    i = j + 1;
                } else {
                    out.push(fmt[i]);
                    i += 1;
                }
            }
            if name == "printf" {
                h.echo(&out);
                Value::Int(i64::try_from(out.len()).unwrap_or(0))
            } else {
                Value::from(out.as_slice())
            }
        }
        "count" | "sizeof" => Value::Int(match arg(0) {
            Value::Array(x) => i64::try_from(x.len()).unwrap_or(0),
            _ => 1,
        }),
        "gzcompress" | "gzencode" | "gzdeflate" | "gzuncompress" | "gzdecode" | "gzinflate"
        | "zlib_decode" => {
            let data = s(h, 0)?;
            let r = match name {
                "gzcompress" => php_zlib::codec::gzcompress(&data, -1, 15),
                "gzencode" => php_zlib::codec::gzencode(&data, -1, 31),
                "gzdeflate" => php_zlib::codec::gzdeflate(&data, -1, -15),
                "gzuncompress" => php_zlib::codec::gzuncompress(&data, 0),
                "gzdecode" => php_zlib::codec::gzdecode(&data, 0),
                "gzinflate" => php_zlib::codec::gzinflate(&data, 0),
                _ => php_zlib::codec::zlib_decode(&data, 0),
            };
            r.map_or(Value::Bool(false), |d| Value::from(d.as_slice()))
        }
        "strrev" => {
            let mut v = s(h, 0)?;
            v.reverse();
            Value::from(v.as_slice())
        }
        "strlen" => Value::Int(i64::try_from(s(h, 0)?.len()).unwrap_or(0)),
        "is_null" => Value::Bool(matches!(arg(0), Value::Null)),
        "is_array" => Value::Bool(matches!(arg(0), Value::Array(_))),
        "is_object" => Value::Bool(matches!(arg(0), Value::Object(_))),
        "is_string" => Value::Bool(matches!(arg(0), Value::Str(_))),
        "is_int" | "is_integer" => Value::Bool(matches!(arg(0), Value::Int(_))),
        "is_float" => Value::Bool(matches!(arg(0), Value::Float(_))),
        "is_soap_fault" => Value::Bool(php_soap::objects::is_soap_fault(h, &arg(0))),
        "use_soap_error_handler" => Value::Bool(php_soap::objects::use_soap_error_handler(
            a.first()
                .is_none_or(|v| matches!(v.deref(), Value::Bool(true) | Value::Int(1))),
        )),
        "gettype" => Value::from(match arg(0) {
            Value::Null => "NULL",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Float(_) => "double",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            _ => "resource",
        }),
        "get_class" => match arg(0) {
            Value::Object(o) => Value::Str(o.class_name()),
            _ => Value::Bool(false),
        },
        "get_parent_class" => match arg(0) {
            Value::Object(o) => {
                let c = o.class_name().to_string_lossy();
                h.classes
                    .get(&c.to_ascii_lowercase())
                    .and_then(|c| c.parent.clone())
                    .map_or(Value::Bool(false), |p| Value::from(p.as_str()))
            }
            _ => Value::Bool(false),
        },
        "method_exists" => {
            let m = s(h, 1)?;
            let class = match arg(0) {
                Value::Object(o) => o.class_name().to_string_lossy(),
                other => String::from_utf8_lossy(&to_str(h, &other)?).into_owned(),
            };
            Value::Bool(
                h.find_user_method(&class, &String::from_utf8_lossy(&m))
                    .is_some(),
            )
        }
        "function_exists" => {
            let n = s(h, 0)?;
            Value::Bool(h.function_exists(&n).is_some())
        }
        "class_exists" => {
            let n = s(h, 0)?;
            Value::Bool(h.class_exists(&String::from_utf8_lossy(&n)))
        }
        "dirname" => {
            let p = s(h, 0)?;
            let p = String::from_utf8_lossy(&p).into_owned();
            Value::from(
                Path::new(&p)
                    .parent()
                    .map_or_else(|| ".".into(), |x| x.to_string_lossy().into_owned())
                    .as_str(),
            )
        }
        "basename" => {
            let p = s(h, 0)?;
            let p = String::from_utf8_lossy(&p).into_owned();
            Value::from(
                Path::new(&p)
                    .file_name()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
            )
        }
        "realpath" => {
            let p = s(h, 0)?;
            let p = String::from_utf8_lossy(&p).into_owned();
            match std::fs::canonicalize(&p) {
                Ok(x) => Value::from(x.to_string_lossy().as_ref()),
                Err(_) => Value::Bool(false),
            }
        }
        "ini_set" => {
            let n = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            let v = s(h, 1)?;
            let old = h.ini.insert(n, v);
            old.map_or(Value::Bool(false), |o| Value::from(o.as_slice()))
        }
        "ini_get" => {
            let n = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            h.ini
                .get(&n)
                .map_or(Value::Bool(false), |v| Value::from(v.as_slice()))
        }
        "header" => {
            let line = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            h.headers.push(line);
            Value::Null
        }
        "http_response_code" => {
            let old = h.response_code;
            if let Some(v) = a.first() {
                h.response_code = Some(v.to_long(h)?);
            }
            old.map_or(Value::Bool(true), Value::Int)
        }
        "getallheaders" => {
            let mut out = ZArray::new();
            for (n, v) in &h.request_headers {
                out.make_mut()
                    .insert(ArrayKey::from_bytes(n.as_bytes()), Value::from(v.as_str()));
            }
            Value::Array(out)
        }
        "stream_context_create" => {
            let ctx = h.context_create();
            if let Value::Array(opts) = arg(0) {
                for (wrapper, inner) in opts.iter() {
                    if let Value::Array(inner) = inner.deref() {
                        for (opt, v) in inner.iter() {
                            h.context_set_option(
                                &ctx,
                                &String::from_utf8_lossy(&wrapper.to_bytes()),
                                &String::from_utf8_lossy(&opt.to_bytes()),
                                v.deref(),
                            );
                        }
                    }
                }
            }
            ctx
        }
        "php_cli_server_start" => {
            let code = match arg(0) {
                Value::Null => String::new(),
                v => String::from_utf8_lossy(&to_str(h, &v)?).into_owned(),
            };
            let script = match a.get(1).map(Value::deref) {
                None => "index.php".to_owned(),
                Some(Value::Null) => "index.php".to_owned(),
                Some(v) => String::from_utf8_lossy(&to_str(h, &v)?).into_owned(),
            };
            let port = 8964 + u16::try_from(h.net.servers.len()).unwrap_or(0);
            let doc_root = h.base_dir.clone();
            h.net.servers.insert(
                port,
                CliServer {
                    code,
                    script,
                    doc_root: doc_root.clone(),
                },
            );
            for (k, v) in [
                ("PHP_CLI_SERVER_HOSTNAME", Value::from("localhost")),
                ("PHP_CLI_SERVER_PORT", Value::Int(i64::from(port))),
                (
                    "PHP_CLI_SERVER_ADDRESS",
                    Value::from(format!("localhost:{port}").as_str()),
                ),
            ] {
                h.globals.entry(format!("const::{k}")).or_insert(v);
            }
            let o = h.new_object("stdClass");
            h.write_property(
                &o,
                b"docRoot",
                Value::from(doc_root.to_string_lossy().as_ref()),
            )?;
            h.write_property(&o, b"port", Value::Int(i64::from(port)))?;
            Value::Object(o)
        }
        "error_reporting"
        | "set_error_handler"
        | "restore_error_handler"
        | "setlocale"
        | "putenv"
        | "date_default_timezone_set"
        | "gc_collect_cycles"
        | "libxml_disable_entity_loader"
        | "libxml_use_internal_errors"
        | "define"
            if name != "define" =>
        {
            Value::Null
        }
        "define" => {
            let n = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            h.globals.insert(format!("const::{n}"), arg(1));
            Value::Bool(true)
        }
        "ob_start" => {
            h.ob_start();
            Value::Bool(true)
        }
        "ob_get_contents" => {
            h.ob.last()
                .map_or(Value::Bool(false), |b| Value::from(b.as_slice()))
        }
        "ob_get_clean" => {
            let v = h.ob.pop();
            v.map_or(Value::Bool(false), |b| Value::from(b.as_slice()))
        }
        "ob_end_clean" => Value::Bool(h.ob.pop().is_some()),
        "ob_flush" => {
            if let Some(b) = h.ob.pop() {
                h.echo(&b);
                h.ob.push(Vec::new());
            }
            Value::Bool(true)
        }
        "ob_clean" => {
            if let Some(b) = h.ob.last_mut() {
                b.clear();
            }
            Value::Bool(true)
        }
        "ob_get_length" => h.ob.last().map_or(Value::Bool(false), |b| {
            Value::Int(i64::try_from(b.len()).unwrap_or(0))
        }),
        "ob_end_flush" | "ob_get_flush" => {
            if let Some(b) = h.ob.pop() {
                h.echo(&b);
            }
            Value::Bool(true)
        }
        "str_repeat" => {
            let x = s(h, 0)?;
            let n = usize::try_from(int(h, 1)?).unwrap_or(0);
            Value::from(x.repeat(n).as_slice())
        }
        "str_replace" => {
            let from = s(h, 0)?;
            let to = s(h, 1)?;
            let subj = s(h, 2)?;
            let mut out = Vec::new();
            let mut i = 0;
            while i < subj.len() {
                if !from.is_empty() && subj[i..].starts_with(&from) {
                    out.extend_from_slice(&to);
                    i += from.len();
                } else {
                    out.push(subj[i]);
                    i += 1;
                }
            }
            Value::from(out.as_slice())
        }
        "substr" => {
            let x = s(h, 0)?;
            let len = i64::try_from(x.len()).unwrap_or(0);
            let mut st = int(h, 1)?;
            if st < 0 {
                st = (len + st).max(0);
            }
            let st = st.min(len);
            let l = match a.get(2) {
                Some(Value::Null) | None => len - st,
                Some(v) => {
                    let l = v.to_long(h)?;
                    if l < 0 {
                        (len - st + l).max(0)
                    } else {
                        l.min(len - st)
                    }
                }
            };
            let (st, l) = (
                usize::try_from(st).unwrap_or(0),
                usize::try_from(l).unwrap_or(0),
            );
            Value::from(&x[st..st + l])
        }
        "strpos" => {
            let hay = s(h, 0)?;
            let n = s(h, 1)?;
            find(&hay, &n).map_or(Value::Bool(false), |p| {
                Value::Int(i64::try_from(p).unwrap_or(0))
            })
        }
        "str_starts_with" => Value::Bool(s(h, 0)?.starts_with(&s(h, 1)?)),
        "str_ends_with" => Value::Bool(s(h, 0)?.ends_with(&s(h, 1)?)),
        "str_contains" => {
            let hay = s(h, 0)?;
            let n = s(h, 1)?;
            Value::Bool(n.is_empty() || find(&hay, &n).is_some())
        }
        "trim" | "rtrim" | "ltrim" => {
            let x = s(h, 0)?;
            let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0 | 0x0b);
            let st = if name == "rtrim" {
                0
            } else {
                x.iter().position(|c| !ws(c)).unwrap_or(x.len())
            };
            let en = if name == "ltrim" {
                x.len()
            } else {
                x.iter().rposition(|c| !ws(c)).map_or(st, |p| p + 1)
            };
            Value::from(&x[st.min(en)..en])
        }
        "strtolower" => Value::from(s(h, 0)?.to_ascii_lowercase().as_slice()),
        "strtoupper" => Value::from(s(h, 0)?.to_ascii_uppercase().as_slice()),
        "implode" | "join" => {
            let (sep, arr) = match (arg(0), arg(1)) {
                (Value::Array(x), sep) => (to_str(h, &sep)?, x),
                (sep, Value::Array(x)) => (to_str(h, &sep)?, x),
                _ => (Vec::new(), ZArray::new()),
            };
            let mut out = Vec::new();
            for (i, v) in arr.values().enumerate() {
                if i > 0 {
                    out.extend_from_slice(&sep);
                }
                out.extend_from_slice(&to_str(h, v)?);
            }
            Value::from(out.as_slice())
        }
        "explode" => {
            let sep = s(h, 0)?;
            let x = s(h, 1)?;
            let mut out = Vec::new();
            let mut rest: &[u8] = &x;
            while let Some(p) = find(rest, &sep) {
                out.push(Value::from(&rest[..p]));
                rest = &rest[p + sep.len()..];
            }
            out.push(Value::from(rest));
            Value::Array(ZArray::from_values(out))
        }
        "array_keys" => match arg(0) {
            Value::Array(x) => Value::Array(ZArray::from_values(x.keys().map(ArrayKey::to_value))),
            _ => Value::Array(ZArray::new()),
        },
        "array_values" => match arg(0) {
            Value::Array(x) => Value::Array(ZArray::from_values(x.values().cloned())),
            _ => Value::Array(ZArray::new()),
        },
        "array_map" => {
            let cb = arg(0);
            let mut out = ZArray::new();
            if let Value::Array(x) = arg(1) {
                for (k, v) in x.iter() {
                    let r = h.call_callable(&cb, std::slice::from_ref(v))?;
                    out.make_mut().insert(k.clone(), r);
                }
            }
            Value::Array(out)
        }
        "in_array" => match arg(1) {
            Value::Array(x) => {
                let needle = arg(0);
                let mut found = false;
                for v in x.values() {
                    if ops::is_equal(v, &needle, h)? {
                        found = true;
                        break;
                    }
                }
                Value::Bool(found)
            }
            _ => Value::Bool(false),
        },
        "array_key_exists" => match arg(1) {
            Value::Array(x) => {
                let k = arg(0).to_array_key(h)?;
                Value::Bool(k.is_some_and(|k| x.contains_key(&k)))
            }
            _ => Value::Bool(false),
        },
        "range" => {
            let (lo, hi) = (int(h, 0)?, int(h, 1)?);
            Value::Array(ZArray::from_values((lo..=hi).map(Value::Int)))
        }
        "base64_encode" => Value::from(php_soap::encoding::base64_encode(&s(h, 0)?).as_slice()),
        "base64_decode" => Value::from(php_soap::encoding::base64_decode(&s(h, 0)?).as_slice()),
        "md5" => Value::from(
            php_hash::md5(&s(h, 0)?)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                .as_str(),
        ),
        "chr" => Value::from(&[u8::try_from(int(h, 0)? & 0xff).unwrap_or(0)][..]),
        "ord" => Value::Int(i64::from(s(h, 0)?.first().copied().unwrap_or(0))),
        "intval" => Value::Int(arg(0).to_long(h)?),
        "strval" => Value::from(s(h, 0)?.as_slice()),
        "floatval" => Value::Float(arg(0).to_double(h)?),
        "serialize" => {
            let out = php_runtime::serialize::serialize(&arg(0), h)?;
            Value::from(out.as_slice())
        }
        "uniqid" => Value::from("5f1a2b3c4d5e6"),
        "time" => Value::Int(h.now),
        "mktime" | "gmmktime" => {
            let v: Vec<i64> = (0..6).map(|i| int(h, i)).collect::<RtResult<_>>()?;
            Value::Int(
                super::days_from_civil(v[5], v[3], v[4]) * 86400 + v[0] * 3600 + v[1] * 60 + v[2],
            )
        }
        "file_get_contents" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            if let Some(d) = h.net.server_files.borrow().get(&p) {
                return Ok(Some(Value::from(d.as_slice())));
            }
            if p.starts_with("http://") {
                let ctx = a.get(2).map(Value::deref);
                return Ok(Some(
                    h.http_get(&p, ctx.as_ref())
                        .map_or(Value::Bool(false), |d| Value::from(d.as_slice())),
                ));
            }
            if p == "php://input" {
                return Ok(Some(Value::from(
                    h.request_body.clone().unwrap_or_default().as_slice(),
                )));
            }
            match h.load_resource(&p, None) {
                Ok(d) => Value::from(d.as_slice()),
                Err(_) => Value::Bool(false),
            }
        }
        "file_put_contents" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            let d = s(h, 1)?;
            let n = d.len();
            h.files.insert(p, d);
            Value::Int(i64::try_from(n).unwrap_or(0))
        }
        "file_exists" | "is_file" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            Value::Bool(h.files.contains_key(&p) || Path::new(&p).exists())
        }
        "tempnam" => {
            let pre = String::from_utf8_lossy(&s(h, 1)?).into_owned();
            let n = format!("/virtual/tmp/{pre}{}", h.files.len());
            h.files.insert(n.clone(), Vec::new());
            Value::from(n.as_str())
        }
        "unlink" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            Value::Bool(h.files.remove(&p).is_some())
        }
        "fopen" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            h.files.insert(p.clone(), Vec::new());
            Value::from(p.as_str())
        }
        "fwrite" | "fputs" => {
            let p = String::from_utf8_lossy(&s(h, 0)?).into_owned();
            let d = s(h, 1)?;
            h.files.entry(p).or_default().extend_from_slice(&d);
            Value::Int(i64::try_from(d.len()).unwrap_or(0))
        }
        "fclose" => Value::Bool(true),
        "php_ini_loaded_file" => Value::Bool(false),
        "extension_loaded" => Value::Bool(!matches!(
            s(h, 0)?.to_ascii_lowercase().as_slice(),
            b"session" | b"pcntl" | b"posix"
        )),
        "getenv" => Value::Bool(false),
        "spl_object_id" => match arg(0) {
            Value::Object(o) => Value::Int(i64::from(o.handle())),
            _ => Value::Int(0),
        },
        "call_user_func" => h.call_callable(&arg(0), &a[1..])?,
        "call_user_func_array" => {
            let args: Vec<Value> = match arg(1) {
                Value::Array(x) => x.values().cloned().collect(),
                _ => Vec::new(),
            };
            h.call_callable(&arg(0), &args)?
        }
        "func_get_args" => Value::Array(ZArray::new()),
        "str_split" => {
            let x = s(h, 0)?;
            let n = usize::try_from(int(h, 1)?.max(1)).unwrap_or(1);
            Value::Array(ZArray::from_values(x.chunks(n).map(Value::from)))
        }
        "simplexml_load_string" => {
            let x = s(h, 0)?;
            match php_soap::xml::Doc::parse(&x) {
                Ok(doc) => match doc.root() {
                    Some(r) => simplexml(h, &doc, r),
                    None => Value::Bool(false),
                },
                Err(_) => Value::Bool(false),
            }
        }
        "json_encode" | "unserialize" => return Ok(None),
        _ => {
            let _ = (f, exprs);
            return Ok(None);
        }
    }))
}

fn simplexml(h: &mut TestHost, doc: &php_soap::xml::Doc, n: usize) -> Value {
    let kids: Vec<usize> = doc.children(n).filter(|&c| doc.is_element(c)).collect();
    if kids.is_empty() {
        return Value::from(doc.text_content(n).as_slice());
    }
    let o = h.new_object("stdClass");
    for c in kids {
        let v = simplexml(h, doc, c);
        let _ = h.write_property(&o, doc.name(c), v);
    }
    Value::Object(o)
}
