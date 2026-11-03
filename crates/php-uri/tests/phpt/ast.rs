//! AST and parser for the PHP subset.

use super::lexer::{Part, Tok};

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<String>,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Var(String),
    Str(Vec<u8>),
    Interp(Vec<Part>),
    Int(i64),
    Null,
    Bool(bool),
    Array(Vec<(Option<Expr>, Expr)>),
    New(String, Vec<Arg>),
    Call(String, Vec<Arg>),
    MethodCall(Box<Expr>, String, Vec<Arg>),
    StaticCall(String, String, Vec<Arg>),
    ClassConst(String, String),
    DynClass(Box<Expr>),
    Prop(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    Push(Box<Expr>, Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    PostInc(String),
    Clone(Box<Expr>),
    InstanceOf(Box<Expr>, String),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Closure(String, Box<Expr>),
    ArrayCast(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Catch {
    pub types: Vec<String>,
    pub var: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Echo(Vec<Expr>),
    Try(Vec<Stmt>, Vec<Catch>, Option<Vec<Stmt>>),
    Foreach(Expr, Option<String>, String, Vec<Stmt>),
    For(Option<Expr>, Option<Expr>, Option<Expr>, Vec<Stmt>),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    Block(Vec<Stmt>),
}

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    aliases: std::collections::HashMap<String, String>,
}

type R<T> = Result<T, String>;

pub fn normalize_class(name: &str) -> String {
    name.trim_start_matches('\\').to_string()
}

impl Parser {
    pub fn new(toks: Vec<Tok>) -> Self {
        Parser {
            toks,
            pos: 0,
            aliases: std::collections::HashMap::new(),
        }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).unwrap_or(&Tok::End)
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) if *q == p)
    }

    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Ident(q) if q.eq_ignore_ascii_case(s))
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(format!("expected {p}, got {:?}", self.peek()))
        }
    }

    pub fn program(&mut self) -> R<Vec<Stmt>> {
        let mut out = Vec::new();
        while *self.peek() != Tok::End {
            if self.is_ident("use") {
                self.next();
                let Tok::Ident(full) = self.next() else {
                    return Err("use".into());
                };
                let full = normalize_class(&full);
                let alias = full.rsplit('\\').next().unwrap_or(&full).to_string();
                self.aliases.insert(alias, full);
                self.expect(";")?;
                continue;
            }
            out.push(self.stmt()?);
        }
        Ok(out)
    }

    fn class(&self, name: &str) -> String {
        let n = normalize_class(name);
        self.aliases.get(&n).cloned().unwrap_or(n)
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        if self.eat("{") {
            let mut out = Vec::new();
            while !self.eat("}") {
                if *self.peek() == Tok::End {
                    return Err("unterminated block".into());
                }
                out.push(self.stmt()?);
            }
            Ok(out)
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    fn stmt(&mut self) -> R<Stmt> {
        if let Tok::Inline(b) = self.peek().clone() {
            self.next();
            return Ok(Stmt::Echo(vec![Expr::Str(b)]));
        }
        if self.eat(";") {
            return Ok(Stmt::Block(Vec::new()));
        }
        if self.is_punct("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.is_ident("echo") || self.is_ident("print") {
            self.next();
            let mut items = vec![self.expr()?];
            while self.eat(",") {
                items.push(self.expr()?);
            }
            self.expect(";")?;
            return Ok(Stmt::Echo(items));
        }
        if self.is_ident("try") {
            self.next();
            let body = self.block()?;
            let mut catches = Vec::new();
            let mut finally = None;
            loop {
                if self.is_ident("catch") {
                    self.next();
                    self.expect("(")?;
                    let mut types = Vec::new();
                    loop {
                        match self.next() {
                            Tok::Ident(n) => types.push(self.class(&n)),
                            t => return Err(format!("catch type {t:?}")),
                        }
                        if !self.eat("|") {
                            break;
                        }
                    }
                    let var = match self.peek().clone() {
                        Tok::Var(v) => {
                            self.next();
                            Some(v)
                        }
                        _ => None,
                    };
                    self.expect(")")?;
                    let body = self.block()?;
                    catches.push(Catch { types, var, body });
                } else if self.is_ident("finally") {
                    self.next();
                    finally = Some(self.block()?);
                } else {
                    break;
                }
            }
            return Ok(Stmt::Try(body, catches, finally));
        }
        if self.is_ident("foreach") {
            self.next();
            self.expect("(")?;
            let subject = self.expr()?;
            if !self.is_ident("as") {
                return Err("expected as".into());
            }
            self.next();
            let first = match self.next() {
                Tok::Var(v) => v,
                t => return Err(format!("foreach var {t:?}")),
            };
            let (k, v) = if self.eat("=>") {
                match self.next() {
                    Tok::Var(v) => (Some(first), v),
                    t => return Err(format!("foreach var {t:?}")),
                }
            } else {
                (None, first)
            };
            self.expect(")")?;
            let body = self.block()?;
            return Ok(Stmt::Foreach(subject, k, v, body));
        }
        if self.is_ident("for") {
            self.next();
            self.expect("(")?;
            let init = if self.is_punct(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(";")?;
            let cond = if self.is_punct(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(";")?;
            let step = if self.is_punct(")") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(")")?;
            let body = self.block()?;
            return Ok(Stmt::For(init, cond, step, body));
        }
        if self.is_ident("if") {
            self.next();
            self.expect("(")?;
            let cond = self.expr()?;
            self.expect(")")?;
            let then = self.block()?;
            let els = if self.is_ident("else") {
                self.next();
                self.block()?
            } else {
                Vec::new()
            };
            return Ok(Stmt::If(cond, then, els));
        }
        if self.is_ident("class")
            || self.is_ident("readonly")
            || self.is_ident("function")
            || self.is_ident("final")
            || self.is_ident("abstract")
            || self.is_ident("enum")
            || self.is_ident("namespace")
            || self.is_ident("use")
            || self.is_ident("declare")
        {
            return Err("unsupported declaration".into());
        }
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    pub fn expr(&mut self) -> R<Expr> {
        self.assignment()
    }

    fn assignment(&mut self) -> R<Expr> {
        let lhs = self.ternary()?;
        if self.is_punct("=") {
            self.next();
            let rhs = self.assignment()?;
            if let Expr::Index(base, idx) = &lhs {
                if matches!(**idx, Expr::Null) {
                    return Ok(Expr::Push(base.clone(), Box::new(rhs)));
                }
            }
            return Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn ternary(&mut self) -> R<Expr> {
        let c = self.binary(0)?;
        if self.eat("?") {
            let a = self.assignment()?;
            self.expect(":")?;
            let b = self.assignment()?;
            return Ok(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let (op, prec) = match self.peek() {
                Tok::Punct(p @ "||") => (*p, 1),
                Tok::Punct(p @ "&&") => (*p, 2),
                Tok::Punct(p @ ("===" | "!==" | "==" | "!=")) => (*p, 3),
                Tok::Punct(p @ ("<" | ">" | "<=" | ">=")) => (*p, 4),
                Tok::Punct(p @ ("." | "+" | "-")) => (*p, 5),
                Tok::Punct(p @ ("*" | "/")) => (*p, 6),
                Tok::Ident(i) if i.eq_ignore_ascii_case("instanceof") => ("instanceof", 7),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.next();
            if op == "instanceof" {
                let cls = match self.next() {
                    Tok::Ident(c) => self.class(&c),
                    t => return Err(format!("instanceof {t:?}")),
                };
                lhs = Expr::InstanceOf(Box::new(lhs), cls);
                continue;
            }
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> R<Expr> {
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.is_ident("clone") {
            self.next();
            return Ok(Expr::Clone(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn args(&mut self) -> R<Vec<Arg>> {
        self.expect("(")?;
        let mut out = Vec::new();
        while !self.eat(")") {
            if self.is_punct("...") {
                return Err("spread".into());
            }
            let name = match (self.peek().clone(), self.peek_at(1).clone()) {
                (Tok::Ident(n), Tok::Punct(":")) if !matches!(self.peek_at(2), Tok::Punct(":")) => {
                    self.next();
                    self.next();
                    Some(n)
                }
                _ => None,
            };
            let expr = self.expr()?;
            out.push(Arg { name, expr });
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        Ok(out)
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat("->") || self.eat("?->") {
                let name = match self.next() {
                    Tok::Ident(n) => n,
                    t => return Err(format!("member {t:?}")),
                };
                if self.is_punct("(") {
                    let a = self.args()?;
                    e = Expr::MethodCall(Box::new(e), name, a);
                } else {
                    e = Expr::Prop(Box::new(e), name);
                }
            } else if self.eat("::") {
                match self.next() {
                    Tok::Ident(n) if n == "class" => e = Expr::DynClass(Box::new(e)),
                    t => return Err(format!("dynamic static {t:?}")),
                }
            } else if self.eat("[") {
                if self.eat("]") {
                    e = Expr::Index(Box::new(e), Box::new(Expr::Null));
                } else {
                    let idx = self.expr()?;
                    self.expect("]")?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
            } else if self.is_punct("++") {
                self.next();
                match e {
                    Expr::Var(v) => e = Expr::PostInc(v),
                    _ => return Err("++".into()),
                }
            } else {
                break;
            }
        }
        Ok(e)
    }

    /// `fn ([Type] $x)[: Type] => expr`.
    fn arrow_fn(&mut self) -> R<Expr> {
        self.expect("(")?;
        if let Tok::Ident(_) = self.peek() {
            self.next();
        }
        let param = match self.next() {
            Tok::Var(v) => v,
            t => return Err(format!("fn param {t:?}")),
        };
        self.expect(")")?;
        if self.eat(":") {
            self.next();
        }
        self.expect("=>")?;
        let body = self.expr()?;
        Ok(Expr::Closure(param, Box::new(body)))
    }

    fn primary(&mut self) -> R<Expr> {
        match self.next() {
            Tok::Var(v) => Ok(Expr::Var(v)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Interp(p) => Ok(Expr::Interp(p)),
            Tok::Int(i) => Ok(Expr::Int(i)),
            Tok::Punct("(") => {
                if matches!(self.peek(), Tok::Ident(i) if i == "array")
                    && matches!(self.peek_at(1), Tok::Punct(")"))
                {
                    self.next();
                    self.next();
                    return Ok(Expr::ArrayCast(Box::new(self.unary()?)));
                }
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                let mut items = Vec::new();
                while !self.eat("]") {
                    let first = self.expr()?;
                    if self.eat("=>") {
                        let v = self.expr()?;
                        items.push((Some(first), v));
                    } else {
                        items.push((None, first));
                    }
                    if !self.eat(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                Ok(Expr::Array(items))
            }
            Tok::Ident(id) => {
                let lower = id.to_ascii_lowercase();
                match lower.as_str() {
                    "null" => return Ok(Expr::Null),
                    "true" => return Ok(Expr::Bool(true)),
                    "false" => return Ok(Expr::Bool(false)),
                    "new" => {
                        let cls = match self.next() {
                            Tok::Ident(c) => self.class(&c),
                            t => return Err(format!("new {t:?}")),
                        };
                        let a = if self.is_punct("(") {
                            self.args()?
                        } else {
                            Vec::new()
                        };
                        return Ok(Expr::New(cls, a));
                    }
                    "static" if matches!(self.peek(), Tok::Ident(f) if f == "fn") => {
                        self.next();
                        return self.arrow_fn();
                    }
                    "fn" => return self.arrow_fn(),
                    "php_int_max" => return Ok(Expr::Int(i64::MAX)),
                    "php_int_min" => return Ok(Expr::Int(i64::MIN)),
                    "php_int_size" => return Ok(Expr::Int(8)),
                    "function" | "static" | "match" | "list" | "isset" | "empty" => {
                        return Err(format!("unsupported {id}"));
                    }
                    _ => {}
                }
                if self.eat("::") {
                    let cls = self.class(&id);
                    let member = match self.next() {
                        Tok::Ident(m) => m,
                        t => return Err(format!("static member {t:?}")),
                    };
                    if self.is_punct("(") {
                        let a = self.args()?;
                        return Ok(Expr::StaticCall(cls, member, a));
                    }
                    return Ok(Expr::ClassConst(cls, member));
                }
                if self.is_punct("(") {
                    let a = self.args()?;
                    return Ok(Expr::Call(lower, a));
                }
                Err(format!("constant {id}"))
            }
            t => Err(format!("unexpected token {t:?}")),
        }
    }
}
