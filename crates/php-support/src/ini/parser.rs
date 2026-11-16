// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! LALR(1) driver for `Zend/zend_ini_parser.y`, using the tables Bison
//! generates for that grammar (see `tables.rs`) and Bison's
//! `parse.error verbose` message construction, so syntax errors read
//! exactly like PHP's (`syntax error, unexpected '=', expecting …`).

use super::scanner::{Scanner, Tok, TokVal};
use super::tables::{
    YYCHECK, YYDEFACT, YYDEFGOTO, YYFINAL, YYLAST, YYNTOKENS, YYPACT, YYPACT_NINF, YYPGOTO, YYR1,
    YYR2, YYTABLE, YYTNAME, YYTRANSLATE,
};
use super::{IniHooks, IniParserCallback, IniScalar, ScannerMode};
use crate::numeric::{atoi, scan_numeric, NumericValue};

/// Parser semantic value (`zval` with the `INI_ZVAL_IS_NUMBER` extra flag).
#[derive(Debug, Clone, PartialEq)]
enum Sem {
    None,
    Str(Vec<u8>, bool),
    Long(i64),
    Double(f64),
    Bool(bool),
    Null,
}

impl Sem {
    fn from_tok(v: TokVal) -> Self {
        match v {
            TokVal::Str(s) => Self::Str(s, false),
            TokVal::Bool(b) => Self::Bool(b),
            TokVal::Null => Self::Null,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Str(s, _) => s.clone(),
            Self::Long(l) => l.to_string().into_bytes(),
            Self::Double(d) => {
                crate::fmt::format_double(*d, 14, crate::fmt::DoubleMode::Plain).into_bytes()
            }
            Self::Bool(true) => b"1".to_vec(),
            Self::Bool(false) | Self::Null | Self::None => Vec::new(),
        }
    }

    fn flag(&self) -> bool {
        matches!(self, Self::Str(_, true))
    }

    fn into_scalar(self) -> IniScalar {
        match self {
            Self::Str(s, _) => IniScalar::Str(s),
            Self::Long(l) => IniScalar::Long(l),
            Self::Double(d) => IniScalar::Double(d),
            Self::Bool(b) => IniScalar::Bool(b),
            Self::Null => IniScalar::Null,
            Self::None => IniScalar::Str(Vec::new()),
        }
    }
}

/// Splits a token into its grammar symbol number and semantic value.
fn symbol(tok: Tok) -> (i32, Sem) {
    let (sym, val) = match tok {
        Tok::End => (0, None),
        Tok::Section => (3, None),
        Tok::Raw(v) => (4, Some(v)),
        Tok::Constant(v) => (5, Some(v)),
        Tok::Number(v) => (6, Some(v)),
        Tok::String(v) => (7, Some(v)),
        Tok::Whitespace(v) => (8, Some(v)),
        Tok::Label(v) => (9, Some(v)),
        Tok::Offset(v) => (10, Some(v)),
        Tok::DollarCurly => (11, None),
        Tok::VarName(v) => (12, Some(v)),
        Tok::QuotedString(s) => (13, Some(TokVal::Str(s))),
        Tok::Fallback => (14, None),
        Tok::BoolTrue(v) => (15, Some(v)),
        Tok::BoolFalse(v) => (16, Some(v)),
        Tok::NullNull(v) => (17, Some(v)),
        Tok::EndOfLine => (18, None),
        Tok::Char(c) => (i32::from(YYTRANSLATE[usize::from(c)]), None),
    };
    (sym, val.map_or(Sem::None, Sem::from_tok))
}

/// Bison's `yytnamerr()`: strips the double quotes of `"end of file"`.
fn tname(sym: usize) -> String {
    let name = YYTNAME[sym];
    if let Some(inner) = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        if !inner.contains(['\'', ',', '\\']) {
            return inner.to_string();
        }
    }
    name.to_string()
}

fn idx(v: i32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

/// Bison's `yysyntax_error()` for the given state and lookahead.
fn syntax_error(state: i32, lookahead: Option<i32>) -> String {
    let Some(tok) = lookahead else {
        return "syntax error".to_string();
    };
    let mut expected: Vec<usize> = Vec::new();
    let mut too_many = false;
    let n = i32::from(YYPACT[idx(state)]);
    if n != YYPACT_NINF {
        let begin = if n < 0 { -n } else { 0 };
        let end = (YYLAST - n + 1).min(YYNTOKENS);
        for x in begin..end {
            if i32::from(YYCHECK[idx(x + n)]) == x && x != 1 {
                if expected.len() == 4 {
                    too_many = true;
                    break;
                }
                expected.push(idx(x));
            }
        }
    }
    let mut msg = format!("syntax error, unexpected {}", tname(idx(tok)));
    if !too_many {
        for (i, e) in expected.iter().enumerate() {
            msg.push_str(if i == 0 { ", expecting " } else { " or " });
            msg.push_str(&tname(*e));
        }
    }
    msg
}

pub(super) struct Parser<'a, 'h> {
    scanner: Scanner<'a>,
    mode: ScannerMode,
    hooks: &'h mut dyn IniHooks,
}

/// A syntax error: the message (`syntax error, …`) and the line number.
pub(super) struct SyntaxError {
    pub message: String,
    pub lineno: u32,
}

impl<'a, 'h> Parser<'a, 'h> {
    pub(super) fn new(input: &'a [u8], mode: ScannerMode, hooks: &'h mut dyn IniHooks) -> Self {
        Self {
            scanner: Scanner::new(input, mode),
            mode,
            hooks,
        }
    }

    /// `get_int_val()`.
    fn int_val(v: &Sem) -> i32 {
        match v {
            #[allow(clippy::cast_possible_truncation)]
            Sem::Long(l) => *l as i32,
            Sem::Str(s, _) => atoi(s),
            _ => 0,
        }
    }

    /// `zend_ini_do_op()`.
    fn do_op(&self, op: u8, a: &Sem, b: Option<&Sem>) -> Sem {
        let i1 = Self::int_val(a);
        let i2 = b.map_or(0, Self::int_val);
        let r = match op {
            b'|' => i1 | i2,
            b'&' => i1 & i2,
            b'^' => i1 ^ i2,
            b'~' => !i1,
            b'!' => i32::from(i1 == 0),
            _ => 0,
        };
        if self.mode == ScannerMode::Typed {
            Sem::Long(i64::from(r))
        } else {
            Sem::Str(r.to_string().into_bytes(), false)
        }
    }

    /// `zend_ini_add_string()`: result keeps the flag of `a`.
    fn add_string(a: &Sem, b: &Sem) -> Sem {
        let mut s = a.bytes();
        s.extend_from_slice(&b.bytes());
        Sem::Str(s, a.flag())
    }

    /// `zend_ini_get_constant()`.
    fn get_constant(&mut self, name: Sem) -> Sem {
        let n = name.bytes();
        if !n.contains(&b':') {
            // zend_get_constant() resolves the special constants itself.
            if n.eq_ignore_ascii_case(b"true") {
                return Sem::Str(b"1".to_vec(), false);
            }
            if n.eq_ignore_ascii_case(b"false") || n.eq_ignore_ascii_case(b"null") {
                return Sem::Str(Vec::new(), false);
            }
            if let Some(v) = self.hooks.constant(&n) {
                return Sem::Str(v, false);
            }
        }
        name
    }

    /// `zend_ini_get_var()`.
    fn get_var(&mut self, name: &Sem, fallback: Option<&Sem>) -> Sem {
        if let Some(v) = self.hooks.variable(&name.bytes()) {
            return Sem::Str(v, false);
        }
        match fallback {
            Some(f) => {
                let mut b = f.bytes();
                if let Some(p) = b.iter().position(|&c| c == 0) {
                    b.truncate(p);
                }
                Sem::Str(b, false)
            }
            None => Sem::Str(Vec::new(), false),
        }
    }

    /// `normalize_value()` (typed mode only).
    fn normalize(&self, v: Sem) -> Sem {
        if self.mode != ScannerMode::Typed {
            return v;
        }
        if let Sem::Str(s, true) = &v {
            if let Some(scan) = scan_numeric(s, false) {
                match scan.value {
                    NumericValue::Long(l) => return Sem::Long(l),
                    NumericValue::Double(d) if !scan.overflow => return Sem::Double(d),
                    NumericValue::Double(_) => {}
                }
            }
        }
        v
    }

    /// Runs the parser, invoking `cb` for every entry.
    #[allow(clippy::too_many_lines)]
    pub(super) fn parse(&mut self, cb: &mut dyn IniParserCallback) -> Result<(), SyntaxError> {
        let mut states: Vec<i32> = vec![0];
        let mut vals: Vec<Sem> = vec![Sem::None];
        let mut lookahead: Option<(i32, Sem)> = None;
        let mut state: i32 = 0;
        loop {
            if state == YYFINAL {
                return Ok(());
            }
            let mut n = i32::from(YYPACT[idx(state)]);
            let mut rule: Option<i32> = None;
            if n != YYPACT_NINF {
                if lookahead.is_none() {
                    lookahead = Some(symbol(self.scanner.lex()));
                }
                let sym = lookahead.as_ref().map_or(0, |l| l.0);
                n += sym;
                if (0..=YYLAST).contains(&n) && i32::from(YYCHECK[idx(n)]) == sym {
                    let t = i32::from(YYTABLE[idx(n)]);
                    if t > 0 {
                        // Shift.
                        let (_, v) = lookahead.take().unwrap_or((0, Sem::None));
                        states.push(t);
                        vals.push(v);
                        state = t;
                        continue;
                    }
                    rule = Some(-t);
                }
            }
            let r = match rule {
                Some(r) => r,
                None => i32::from(YYDEFACT[idx(state)]),
            };
            if r == 0 {
                return Err(SyntaxError {
                    message: syntax_error(state, lookahead.map(|l| l.0)),
                    lineno: self.scanner.lineno,
                });
            }
            let len = usize::from(YYR2[idx(r)]);
            let args: Vec<Sem> = vals.split_off(vals.len() - len);
            states.truncate(states.len() - len);
            let result = self.reduce(r - 1, args, cb);
            let top = *states.last().unwrap_or(&0);
            let lhs = i32::from(YYR1[idx(r)]) - YYNTOKENS;
            let i = i32::from(YYPGOTO[idx(lhs)]) + top;
            state = if (0..=YYLAST).contains(&i) && i32::from(YYCHECK[idx(i)]) == top {
                i32::from(YYTABLE[idx(i)])
            } else {
                i32::from(YYDEFGOTO[idx(lhs)])
            };
            states.push(state);
            vals.push(result);
        }
    }

    /// Semantic actions; `rule` uses the numbering of Bison's report.
    fn reduce(&mut self, rule: i32, args: Vec<Sem>, cb: &mut dyn IniParserCallback) -> Sem {
        let mut a: Vec<Sem> = args;
        let mut take = |i: usize| std::mem::replace(&mut a[i], Sem::None);
        let empty = || Sem::Str(Vec::new(), false);
        match rule {
            3 => {
                cb.section(&take(1).bytes());
                Sem::None
            }
            4 => {
                let key = take(0).bytes();
                cb.entry(&key, Some(take(2).into_scalar()));
                Sem::None
            }
            5 => {
                let key = take(0).bytes();
                let offset = take(1).bytes();
                cb.pop_entry(&key, take(4).into_scalar(), &offset);
                Sem::None
            }
            6 => {
                cb.entry(&take(0).bytes(), None);
                Sem::None
            }
            10 => {
                let v = take(0);
                self.normalize(v)
            }
            // Pass-through rules ($$ = $1).
            8 | 11..=13 | 15 | 20 | 21 | 26 | 27 | 32 | 41 | 43..=47 | 49 | 51 | 52 => take(0),
            9 | 14 | 16 | 19 | 42 => empty(),
            22 | 28 | 38 => take(1),
            17 | 18 | 23 | 24 | 29 | 30 => {
                let x = take(0);
                let y = take(1);
                Self::add_string(&x, &y)
            }
            25 | 31 => {
                let x = take(0);
                let y = take(2);
                Self::add_string(&x, &y)
            }
            33..=35 => {
                let op = [b'|', b'&', b'^'][usize::try_from(rule - 33).unwrap_or(0)];
                let x = take(0);
                let y = take(2);
                self.do_op(op, &x, Some(&y))
            }
            36 | 37 => {
                let x = take(1);
                self.do_op(if rule == 36 { b'~' } else { b'!' }, &x, None)
            }
            39 => {
                let name = take(1);
                self.get_var(&name, None)
            }
            40 => {
                let name = take(1);
                let fb = take(3);
                self.get_var(&name, Some(&fb))
            }
            48 => {
                let name = take(0);
                self.get_constant(name)
            }
            50 => match take(0) {
                Sem::Str(s, _) => Sem::Str(s, true),
                other => other,
            },
            _ => Sem::None,
        }
    }
}
