// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A tiny evaluator for the PHP subset, wired to the `php_uri::api` layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use php_uri::api::{DebugValue, Rfc3986Uri, Rfc3986UriBuilder, WhatWgUrl, WhatWgUrlBuilder};
use php_uri::whatwg::UrlPercentEncodingMode;
use php_uri::{UriComparisonMode, UriError, UrlValidationError, UrlValidationErrorType};

use super::ast::{normalize_class, Arg, Catch, Expr, Stmt};
use super::lexer::Part;

#[derive(Debug, Clone)]
pub enum Key {
    Int(i64),
    Str(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Str(Vec<u8>),
    Arr(Vec<(Key, V)>),
    Obj(Rc<RefCell<Obj>>),
    Closure(String, Rc<Expr>),
}

#[derive(Debug)]
pub struct Obj {
    pub id: usize,
    pub kind: Kind,
}

#[derive(Debug)]
pub enum Kind {
    Rfc(Option<Rfc3986Uri>),
    Wh(Option<WhatWgUrl>),
    RfcBuilder(Rfc3986UriBuilder),
    WhBuilder(WhatWgUrlBuilder),
    Exc {
        class: String,
        message: Vec<u8>,
        errors: Option<V>,
        previous: Option<V>,
    },
    ValErr {
        context: Vec<u8>,
        ty: V,
        failure: bool,
    },
    Enum {
        class: String,
        case: String,
    },
}

pub enum Flow {
    Throw(V),
    Unsupported(String),
}

type R<T> = Result<T, Flow>;

fn unsupported<T>(m: impl Into<String>) -> R<T> {
    Err(Flow::Unsupported(m.into()))
}

pub struct Interp {
    pub out: Vec<u8>,
    vars: HashMap<String, V>,
    next_id: usize,
    enums: HashMap<(String, String), V>,
}

const RFC: &str = "Uri\\Rfc3986\\Uri";
const WH: &str = "Uri\\WhatWg\\Url";

fn parents(class: &str) -> Vec<&'static str> {
    match class {
        "Uri\\WhatWg\\InvalidUrlException" => vec![
            "Uri\\WhatWg\\InvalidUrlException",
            "Uri\\InvalidUriException",
            "Uri\\UriException",
            "Exception",
            "Throwable",
        ],
        "Uri\\InvalidUriException" => vec![
            "Uri\\InvalidUriException",
            "Uri\\UriException",
            "Exception",
            "Throwable",
        ],
        "Uri\\UriException" => vec!["Uri\\UriException", "Exception", "Throwable"],
        "Uri\\UriError" => vec!["Uri\\UriError", "Error", "Throwable"],
        "Exception" => vec!["Exception", "Throwable"],
        "Error" => vec!["Error", "Throwable"],
        "ValueError" => vec!["ValueError", "Error", "Throwable"],
        "TypeError" => vec!["TypeError", "Error", "Throwable"],
        "ArgumentCountError" => vec!["ArgumentCountError", "TypeError", "Error", "Throwable"],
        _ => vec![],
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            out: Vec::new(),
            vars: HashMap::new(),
            next_id: 1,
            enums: HashMap::new(),
        }
    }

    fn obj(&mut self, kind: Kind) -> V {
        let id = self.next_id;
        self.next_id += 1;
        V::Obj(Rc::new(RefCell::new(Obj { id, kind })))
    }

    fn enum_case(&mut self, class: &str, case: &str) -> V {
        let key = (class.to_string(), case.to_string());
        if let Some(v) = self.enums.get(&key) {
            return v.clone();
        }
        let v = self.obj(Kind::Enum {
            class: class.to_string(),
            case: case.to_string(),
        });
        self.enums.insert(key, v.clone());
        v
    }

    fn exc(&mut self, class: &str, message: &str) -> Flow {
        let v = self.obj(Kind::Exc {
            class: class.to_string(),
            message: message.as_bytes().to_vec(),
            errors: None,
            previous: None,
        });
        Flow::Throw(v)
    }

    fn val_err(&mut self, e: &UrlValidationError) -> V {
        // PHP creates the error object before fetching the enum case.
        let id = self.next_id;
        self.next_id += 1;
        let ty = self.enum_case("Uri\\WhatWg\\UrlValidationErrorType", e.error_type.name());
        V::Obj(Rc::new(RefCell::new(Obj {
            id,
            kind: Kind::ValErr {
                context: e.context.to_vec(),
                ty,
                failure: e.failure,
            },
        })))
    }

    fn errors_array(&mut self, errs: &[UrlValidationError]) -> V {
        let mut arr = Vec::new();
        for (i, e) in errs.iter().enumerate() {
            let v = self.val_err(e);
            arr.push((Key::Int(i as i64), v));
        }
        V::Arr(arr)
    }

    fn uri_error(&mut self, e: UriError) -> Flow {
        let errors = if e.class == php_uri::ErrorClass::InvalidUrlException {
            Some(self.errors_array(&e.errors))
        } else {
            None
        };
        let v = self.obj(Kind::Exc {
            class: e.class.class_name().to_string(),
            message: e.message.into_bytes(),
            errors,
            previous: None,
        });
        Flow::Throw(v)
    }

    pub fn run(&mut self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        match s {
            Stmt::Expr(e) => {
                self.eval(e)?;
            }
            Stmt::Echo(items) => {
                for e in items {
                    let v = self.eval(e)?;
                    let s = self.to_str(&v)?;
                    self.out.extend_from_slice(&s);
                }
            }
            Stmt::Block(b) => self.run(b)?,
            Stmt::If(c, a, b) => {
                let v = self.eval(c)?;
                if truthy(&v) {
                    self.run(a)?;
                } else {
                    self.run(b)?;
                }
            }
            Stmt::For(init, cond, step, body) => {
                if let Some(i) = init {
                    self.eval(i)?;
                }
                loop {
                    if let Some(c) = cond {
                        let v = self.eval(c)?;
                        if !truthy(&v) {
                            break;
                        }
                    }
                    self.run(body)?;
                    if let Some(s) = step {
                        self.eval(s)?;
                    }
                }
            }
            Stmt::Foreach(subject, k, v, body) => {
                let arr = match self.eval(subject)? {
                    V::Arr(a) => a,
                    _ => return unsupported("foreach non-array"),
                };
                for (key, val) in arr {
                    if let Some(k) = k {
                        let kv = match key {
                            Key::Int(i) => V::Int(i),
                            Key::Str(s) => V::Str(s),
                        };
                        self.vars.insert(k.clone(), kv);
                    }
                    self.vars.insert(v.clone(), val);
                    self.run(body)?;
                }
            }
            Stmt::Try(body, catches, finally) => {
                let r = self.run(body);
                let r = match r {
                    Err(Flow::Throw(ex)) => self.handle_catch(ex, catches),
                    other => other,
                };
                if let Some(f) = finally {
                    self.run(f)?;
                }
                r?;
            }
        }
        Ok(())
    }

    fn handle_catch(&mut self, ex: V, catches: &[Catch]) -> R<()> {
        let class = self.class_of(&ex);
        for c in catches {
            let ps = parents(&class);
            if c.types
                .iter()
                .any(|t| ps.contains(&t.as_str()) || *t == class)
            {
                if let Some(var) = &c.var {
                    self.vars.insert(var.clone(), ex.clone());
                }
                return self.run(&c.body);
            }
        }
        Err(Flow::Throw(ex))
    }

    fn class_of(&self, v: &V) -> String {
        match v {
            V::Obj(o) => match &o.borrow().kind {
                Kind::Rfc(_) => RFC.into(),
                Kind::Wh(_) => WH.into(),
                Kind::RfcBuilder(_) => "Uri\\Rfc3986\\UriBuilder".into(),
                Kind::WhBuilder(_) => "Uri\\WhatWg\\UrlBuilder".into(),
                Kind::Exc { class, .. } => class.clone(),
                Kind::ValErr { .. } => "Uri\\WhatWg\\UrlValidationError".into(),
                Kind::Enum { class, .. } => class.clone(),
            },
            _ => String::new(),
        }
    }

    fn to_str(&mut self, v: &V) -> R<Vec<u8>> {
        Ok(match v {
            V::Null => Vec::new(),
            V::Bool(b) => {
                if *b {
                    b"1".to_vec()
                } else {
                    Vec::new()
                }
            }
            V::Int(i) => i.to_string().into_bytes(),
            V::Str(s) => s.clone(),
            V::Arr(_) => b"Array".to_vec(),
            V::Obj(_) | V::Closure(..) => return unsupported("object to string"),
        })
    }

    fn eval_args(&mut self, args: &[Arg], params: &[&str]) -> R<Vec<Option<V>>> {
        let mut out: Vec<Option<V>> = vec![None; params.len().max(args.len())];
        for (i, a) in args.iter().enumerate() {
            let idx = match &a.name {
                Some(n) => match params.iter().position(|p| p == n) {
                    Some(i) => i,
                    None => return unsupported(format!("named arg {n}")),
                },
                None => i,
            };
            if idx >= out.len() {
                out.resize(idx + 1, None);
            }
            out[idx] = Some(self.eval(&a.expr)?);
        }
        Ok(out)
    }

    fn has_ref(args: &[Arg], params: &[&str], param: usize) -> bool {
        args.iter().enumerate().any(|(i, a)| match &a.name {
            Some(n) => params.iter().position(|p| p == n) == Some(param),
            None => i == param,
        })
    }

    /// Converts validation errors and writes them to a by-reference argument.
    fn write_errors(
        &mut self,
        args: &[Arg],
        params: &[&str],
        param: usize,
        errs: &[UrlValidationError],
    ) -> R<()> {
        if !Self::has_ref(args, params, param) {
            return Ok(());
        }
        let ev = self.errors_array(errs);
        self.write_ref(args, params, param, ev)
    }

    /// Writes a by-reference argument back to its variable.
    fn write_ref(&mut self, args: &[Arg], params: &[&str], param: usize, value: V) -> R<()> {
        for (i, a) in args.iter().enumerate() {
            let idx = match &a.name {
                Some(n) => params.iter().position(|p| p == n).unwrap_or(usize::MAX),
                None => i,
            };
            if idx == param {
                return match &a.expr {
                    Expr::Var(v) => {
                        self.vars.insert(v.clone(), value);
                        Ok(())
                    }
                    _ => unsupported("by-ref to non-variable"),
                };
            }
        }
        Ok(())
    }

    fn eval(&mut self, e: &Expr) -> R<V> {
        match e {
            Expr::Var(v) => Ok(self.vars.get(v).cloned().unwrap_or(V::Null)),
            Expr::Str(s) => Ok(V::Str(s.clone())),
            Expr::Interp(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        Part::Lit(l) => out.extend_from_slice(l),
                        Part::Var(v) => {
                            let val = self.vars.get(v).cloned().unwrap_or(V::Null);
                            out.extend(self.to_str(&val)?);
                        }
                    }
                }
                Ok(V::Str(out))
            }
            Expr::Int(i) => Ok(V::Int(*i)),
            Expr::Null => Ok(V::Null),
            Expr::Bool(b) => Ok(V::Bool(*b)),
            Expr::Array(items) => {
                let mut arr: Vec<(Key, V)> = Vec::new();
                let mut next = 0i64;
                for (k, v) in items {
                    let val = self.eval(v)?;
                    let key = match k {
                        Some(k) => match self.eval(k)? {
                            V::Int(i) => {
                                next = next.max(i + 1);
                                Key::Int(i)
                            }
                            V::Str(s) => Key::Str(s),
                            _ => return unsupported("array key"),
                        },
                        None => {
                            next += 1;
                            Key::Int(next - 1)
                        }
                    };
                    arr.push((key, val));
                }
                Ok(V::Arr(arr))
            }
            Expr::Assign(target, value) => {
                let v = self.eval(value)?;
                match &**target {
                    Expr::Var(name) => {
                        self.vars.insert(name.clone(), v.clone());
                        Ok(v)
                    }
                    Expr::Prop(obj, name) => {
                        let o = self.eval(obj)?;
                        self.set_prop(&o, name, v.clone())?;
                        Ok(v)
                    }
                    _ => unsupported("assignment target"),
                }
            }
            Expr::Push(base, value) => {
                let v = self.eval(value)?;
                let Expr::Var(name) = &**base else {
                    return unsupported("push target");
                };
                let mut arr = match self.vars.remove(name) {
                    Some(V::Arr(a)) => a,
                    None | Some(V::Null) => Vec::new(),
                    _ => return unsupported("push to non-array"),
                };
                let next = arr
                    .iter()
                    .filter_map(|(k, _)| {
                        if let Key::Int(i) = k {
                            Some(*i + 1)
                        } else {
                            None
                        }
                    })
                    .max()
                    .unwrap_or(0);
                arr.push((Key::Int(next), v.clone()));
                self.vars.insert(name.clone(), V::Arr(arr));
                Ok(v)
            }
            Expr::PostInc(name) => {
                let cur = self.vars.get(name).cloned().unwrap_or(V::Null);
                let i = match cur {
                    V::Int(i) => i,
                    V::Null => 0,
                    _ => return unsupported("++"),
                };
                self.vars.insert(name.clone(), V::Int(i + 1));
                Ok(V::Int(i))
            }
            Expr::Not(e) => {
                let v = self.eval(e)?;
                Ok(V::Bool(!truthy(&v)))
            }
            Expr::Neg(e) => match self.eval(e)? {
                V::Int(i) => Ok(V::Int(-i)),
                _ => unsupported("neg"),
            },
            Expr::Ternary(c, a, b) => {
                let v = self.eval(c)?;
                if truthy(&v) {
                    self.eval(a)
                } else {
                    self.eval(b)
                }
            }
            Expr::Bin(op, a, b) => {
                let x = self.eval(a)?;
                if *op == "&&" {
                    return if truthy(&x) {
                        let y = self.eval(b)?;
                        Ok(V::Bool(truthy(&y)))
                    } else {
                        Ok(V::Bool(false))
                    };
                }
                if *op == "||" {
                    return if truthy(&x) {
                        Ok(V::Bool(true))
                    } else {
                        let y = self.eval(b)?;
                        Ok(V::Bool(truthy(&y)))
                    };
                }
                let y = self.eval(b)?;
                match *op {
                    "." => {
                        let mut s = self.to_str(&x)?;
                        s.extend(self.to_str(&y)?);
                        Ok(V::Str(s))
                    }
                    "===" => Ok(V::Bool(identical(&x, &y))),
                    "!==" => Ok(V::Bool(!identical(&x, &y))),
                    "==" => Ok(V::Bool(loose_eq(&x, &y))),
                    "!=" => Ok(V::Bool(!loose_eq(&x, &y))),
                    "+" | "-" | "*" | "/" | "<" | ">" | "<=" | ">=" => {
                        let (V::Int(p), V::Int(q)) = (&x, &y) else {
                            return unsupported("arith on non-int");
                        };
                        let (p, q) = (*p, *q);
                        Ok(match *op {
                            "+" => V::Int(p + q),
                            "-" => V::Int(p - q),
                            "*" => V::Int(p * q),
                            "/" => V::Int(p / q),
                            "<" => V::Bool(p < q),
                            ">" => V::Bool(p > q),
                            "<=" => V::Bool(p <= q),
                            _ => V::Bool(p >= q),
                        })
                    }
                    _ => unsupported(format!("op {op}")),
                }
            }
            Expr::InstanceOf(e, cls) => {
                let v = self.eval(e)?;
                let c = self.class_of(&v);
                Ok(V::Bool(c == *cls || parents(&c).contains(&cls.as_str())))
            }
            Expr::Clone(e) => {
                let v = self.eval(e)?;
                let V::Obj(o) = v else {
                    return unsupported("clone non-object");
                };
                let kind = match &o.borrow().kind {
                    Kind::Rfc(u) => Kind::Rfc(u.clone()),
                    Kind::Wh(u) => Kind::Wh(u.clone()),
                    Kind::RfcBuilder(b) => Kind::RfcBuilder(b.clone()),
                    Kind::WhBuilder(b) => Kind::WhBuilder(b.clone()),
                    _ => return unsupported("clone kind"),
                };
                Ok(self.obj(kind))
            }
            Expr::DynClass(e) => {
                let v = self.eval(e)?;
                Ok(V::Str(self.class_of(&v).into_bytes()))
            }
            Expr::ClassConst(cls, member) => self.class_const(cls, member),
            Expr::Prop(obj, name) => {
                let o = self.eval(obj)?;
                self.get_prop(&o, name)
            }
            Expr::Index(base, idx) => {
                let b = self.eval(base)?;
                let i = self.eval(idx)?;
                match (b, i) {
                    (V::Arr(a), V::Int(n)) => Ok(a
                        .into_iter()
                        .find(|(k, _)| matches!(k, Key::Int(m) if *m == n))
                        .map_or(V::Null, |(_, v)| v)),
                    (V::Arr(a), V::Str(s)) => Ok(a
                        .into_iter()
                        .find(|(k, _)| matches!(k, Key::Str(m) if *m == s))
                        .map_or(V::Null, |(_, v)| v)),
                    _ => unsupported("index"),
                }
            }
            Expr::Closure(p, body) => Ok(V::Closure(p.clone(), Rc::new((**body).clone()))),
            Expr::ArrayCast(e) => match self.eval(e)? {
                V::Arr(a) => Ok(V::Arr(a)),
                V::Obj(_) => Ok(V::Arr(Vec::new())),
                _ => unsupported("array cast"),
            },
            Expr::New(cls, args) => self.new_obj(cls, args),
            Expr::Call(name, args) => self.call(name, args),
            Expr::StaticCall(cls, m, args) => self.static_call(cls, m, args),
            Expr::MethodCall(obj, m, args) => {
                let o = self.eval(obj)?;
                self.method(&o, m, args)
            }
        }
    }

    fn class_const(&mut self, cls: &str, member: &str) -> R<V> {
        if member == "class" {
            return Ok(V::Str(cls.as_bytes().to_vec()));
        }
        let valid = match cls {
            "Uri\\UriComparisonMode" => ["IncludeFragment", "ExcludeFragment"].contains(&member),
            "Uri\\WhatWg\\UrlValidationErrorType" => {
                UrlValidationErrorType::from_name(member).is_some()
            }
            "Uri\\WhatWg\\UrlPercentEncodingMode" => {
                UrlPercentEncodingMode::from_name(member).is_some()
            }
            "Uri\\Rfc3986\\UriType" | "Uri\\Rfc3986\\UriHostType" | "Uri\\WhatWg\\UrlHostType" => {
                true
            }
            _ => false,
        };
        if !valid {
            return unsupported(format!("constant {cls}::{member}"));
        }
        Ok(self.enum_case(cls, member))
    }

    fn get_prop(&mut self, o: &V, name: &str) -> R<V> {
        let V::Obj(o) = o else {
            return unsupported("prop of non-object");
        };
        let b = o.borrow();
        match (&b.kind, name) {
            (
                Kind::Exc {
                    errors: Some(e), ..
                },
                "errors",
            ) => Ok(e.clone()),
            (Kind::Exc { message, .. }, "message") => Ok(V::Str(message.clone())),
            (Kind::ValErr { context, .. }, "context") => Ok(V::Str(context.clone())),
            (Kind::ValErr { ty, .. }, "type") => Ok(ty.clone()),
            (Kind::ValErr { failure, .. }, "failure") => Ok(V::Bool(*failure)),
            (Kind::Enum { case, .. }, "name") => Ok(V::Str(case.as_bytes().to_vec())),
            _ => unsupported(format!("property {name}")),
        }
    }

    fn set_prop(&mut self, o: &V, name: &str, _v: V) -> R<()> {
        let class = self.class_of(o);
        match class.as_str() {
            "Uri\\WhatWg\\UrlValidationError" | "Uri\\WhatWg\\InvalidUrlException" => Err(self
                .exc(
                    "Error",
                    &format!("Cannot modify readonly property {class}::${name}"),
                )),
            _ => unsupported("set property"),
        }
    }

    fn opt_str(&mut self, v: Option<&V>) -> R<Option<Vec<u8>>> {
        match v {
            None | Some(V::Null) => Ok(None),
            Some(V::Str(s)) => Ok(Some(s.clone())),
            Some(V::Int(i)) => Ok(Some(i.to_string().into_bytes())),
            _ => unsupported("string arg"),
        }
    }

    fn req_str(&mut self, v: Option<&V>) -> R<Vec<u8>> {
        match v {
            Some(V::Str(s)) => Ok(s.clone()),
            Some(V::Int(i)) => Ok(i.to_string().into_bytes()),
            _ => unsupported("required string arg"),
        }
    }

    fn opt_int(&mut self, v: Option<&V>) -> R<Option<i64>> {
        match v {
            None | Some(V::Null) => Ok(None),
            Some(V::Int(i)) => Ok(Some(*i)),
            _ => unsupported("int arg"),
        }
    }

    fn rfc_of(&self, v: Option<&V>) -> R<Option<Rfc3986Uri>> {
        match v {
            None | Some(V::Null) => Ok(None),
            Some(V::Obj(o)) => match &o.borrow().kind {
                Kind::Rfc(Some(u)) => Ok(Some(u.clone())),
                _ => unsupported("expected Rfc3986 Uri"),
            },
            _ => unsupported("expected object"),
        }
    }

    fn wh_of(&self, v: Option<&V>) -> R<Option<WhatWgUrl>> {
        match v {
            None | Some(V::Null) => Ok(None),
            Some(V::Obj(o)) => match &o.borrow().kind {
                Kind::Wh(Some(u)) => Ok(Some(u.clone())),
                _ => unsupported("expected WhatWg Url"),
            },
            _ => unsupported("expected object"),
        }
    }

    fn mode_of(&self, v: Option<&V>) -> R<UriComparisonMode> {
        match v {
            None => Ok(UriComparisonMode::ExcludeFragment),
            Some(V::Obj(o)) => match &o.borrow().kind {
                Kind::Enum { case, .. } if case == "IncludeFragment" => {
                    Ok(UriComparisonMode::IncludeFragment)
                }
                Kind::Enum { case, .. } if case == "ExcludeFragment" => {
                    Ok(UriComparisonMode::ExcludeFragment)
                }
                _ => unsupported("mode"),
            },
            _ => unsupported("mode"),
        }
    }

    fn new_obj(&mut self, cls: &str, args: &[Arg]) -> R<V> {
        match cls {
            "Uri\\Rfc3986\\Uri" => {
                let a = self.eval_args(args, &["uri", "baseUrl"])?;
                let s = self.req_str(a.first().and_then(Option::as_ref))?;
                let base = self.rfc_of(a.get(1).and_then(Option::as_ref))?;
                match Rfc3986Uri::new(&s, base.as_ref()) {
                    Ok(u) => Ok(self.obj(Kind::Rfc(Some(u)))),
                    Err(e) => Err(self.uri_error(e)),
                }
            }
            "Uri\\WhatWg\\Url" => {
                let params = ["uri", "baseUrl", "softErrors"];
                let a = self.eval_args(args, &params)?;
                let s = self.req_str(a.first().and_then(Option::as_ref))?;
                let base = self.wh_of(a.get(1).and_then(Option::as_ref))?;
                match WhatWgUrl::new(&s, base.as_ref()) {
                    Ok((u, errs)) => {
                        self.write_errors(args, &params, 2, &errs)?;
                        Ok(self.obj(Kind::Wh(Some(u))))
                    }
                    Err(e) => {
                        self.write_ref(args, &params, 2, V::Arr(Vec::new()))?;
                        Err(self.uri_error(e))
                    }
                }
            }
            "Uri\\Rfc3986\\UriBuilder" => Ok(self.obj(Kind::RfcBuilder(Rfc3986UriBuilder::new()))),
            "Uri\\WhatWg\\UrlBuilder" => Ok(self.obj(Kind::WhBuilder(WhatWgUrlBuilder::new()))),
            "Uri\\WhatWg\\UrlValidationError" => {
                let a = self.eval_args(args, &["context", "type", "failure"])?;
                let context = self.req_str(a.first().and_then(Option::as_ref))?;
                let ty = a.get(1).cloned().flatten().unwrap_or(V::Null);
                let failure = match a.get(2).cloned().flatten() {
                    Some(V::Bool(b)) => b,
                    _ => return unsupported("failure arg"),
                };
                Ok(self.obj(Kind::ValErr {
                    context,
                    ty,
                    failure,
                }))
            }
            "Uri\\WhatWg\\InvalidUrlException"
            | "Exception"
            | "Uri\\InvalidUriException"
            | "Uri\\UriException"
            | "Uri\\UriError"
            | "Error" => {
                let a = self.eval_args(args, &["message", "errors", "code", "previous"])?;
                let message = self
                    .opt_str(a.first().and_then(Option::as_ref))?
                    .unwrap_or_default();
                let errors = if cls == "Uri\\WhatWg\\InvalidUrlException" {
                    let e = a.get(1).cloned().flatten().unwrap_or(V::Arr(Vec::new()));
                    if let V::Arr(items) = &e {
                        let is_list = items.iter().enumerate().all(|(i, (k, v))| {
                            matches!(k, Key::Int(n) if *n == i as i64)
                                && self.class_of(v) == "Uri\\WhatWg\\UrlValidationError"
                        });
                        if !is_list {
                            return Err(self.uri_error(
                                php_uri::api::invalid_url_exception_errors_value_error(),
                            ));
                        }
                    }
                    Some(e)
                } else {
                    None
                };
                let previous = a.get(3).cloned().flatten();
                Ok(self.obj(Kind::Exc {
                    class: cls.to_string(),
                    message,
                    errors,
                    previous,
                }))
            }
            _ => unsupported(format!("new {cls}")),
        }
    }

    fn call(&mut self, name: &str, args: &[Arg]) -> R<V> {
        let a = self.eval_args(args, &[])?;
        let arg = |i: usize| a.get(i).cloned().flatten();
        match name {
            "var_dump" => {
                for v in a.iter().flatten() {
                    let mut buf = Vec::new();
                    self.dump(v, 0, &mut buf);
                    self.out.extend(buf);
                }
                Ok(V::Null)
            }
            "die" | "exit" => {
                if let Some(V::Str(m)) = arg(0) {
                    self.out.extend_from_slice(&m);
                }
                unsupported("die")
            }
            "str_repeat" => {
                let (Some(V::Str(s)), Some(V::Int(n))) = (arg(0), arg(1)) else {
                    return unsupported("str_repeat");
                };
                Ok(V::Str(s.repeat(usize::try_from(n).unwrap_or(0))))
            }
            "array_map" => {
                let (Some(V::Closure(p, body)), Some(V::Arr(items))) = (arg(0), arg(1)) else {
                    return unsupported("array_map");
                };
                let mut out = Vec::new();
                for (k, v) in items {
                    let saved = self.vars.insert(p.clone(), v);
                    let r = self.eval(&body);
                    match saved {
                        Some(old) => {
                            self.vars.insert(p.clone(), old);
                        }
                        None => {
                            self.vars.remove(&p);
                        }
                    }
                    out.push((k, r?));
                }
                Ok(V::Arr(out))
            }
            "substr" => {
                let (Some(V::Str(st)), Some(V::Int(start))) = (arg(0), arg(1)) else {
                    return unsupported("substr");
                };
                let len = st.len() as i64;
                let from = if start < 0 {
                    (len + start).max(0)
                } else {
                    start.min(len)
                };
                let to = match arg(2) {
                    Some(V::Int(l)) if l >= 0 => (from + l).min(len),
                    Some(V::Int(l)) => (len + l).max(from),
                    _ => len,
                };
                Ok(V::Str(st[from as usize..to as usize].to_vec()))
            }
            "strlen" => match arg(0) {
                Some(V::Str(s)) => Ok(V::Int(s.len() as i64)),
                _ => unsupported("strlen"),
            },
            "substr_count" => match (arg(0), arg(1)) {
                (Some(V::Str(h)), Some(V::Str(n))) if !n.is_empty() => {
                    let mut c = 0;
                    let mut i = 0;
                    while i + n.len() <= h.len() {
                        if h[i..].starts_with(&n) {
                            c += 1;
                            i += n.len();
                        } else {
                            i += 1;
                        }
                    }
                    Ok(V::Int(c))
                }
                _ => unsupported("substr_count"),
            },
            "count" => match arg(0) {
                Some(V::Arr(x)) => Ok(V::Int(x.len() as i64)),
                _ => unsupported("count"),
            },
            "json_encode" => match arg(0) {
                Some(V::Obj(_)) => Ok(V::Str(b"{}".to_vec())),
                _ => unsupported("json_encode"),
            },
            "serialize" => match arg(0) {
                Some(v @ V::Obj(_)) => self.serialize(&v),
                _ => unsupported("serialize"),
            },
            "unserialize" => match arg(0) {
                Some(V::Str(s)) => self.unserialize(&s),
                _ => unsupported("unserialize"),
            },
            "uri\\whatwg\\url_percent_encode" => {
                let a = self.eval_args(args, &["input", "mode"])?;
                let input = self.req_str(a.first().and_then(Option::as_ref))?;
                let mode = match a.get(1).cloned().flatten() {
                    Some(V::Obj(o)) => match &o.borrow().kind {
                        Kind::Enum { case, .. } => UrlPercentEncodingMode::from_name(case),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(mode) = mode else {
                    return unsupported("percent mode");
                };
                Ok(V::Str(php_uri::api::whatwg::url_percent_encode(
                    &input, mode,
                )))
            }
            _ => unsupported(format!("function {name}")),
        }
    }

    fn serialize(&mut self, v: &V) -> R<V> {
        let V::Obj(o) = v else { unreachable!() };
        let (class, s) = match &o.borrow().kind {
            Kind::Rfc(Some(u)) => (RFC, u.serialize()),
            Kind::Wh(Some(u)) => (WH, u.serialize()),
            _ => return unsupported("serialize kind"),
        };
        let mut out = format!(
            "O:{}:\"{}\":2:{{i:0;a:1:{{s:3:\"uri\";s:{}:\"",
            class.len(),
            class,
            s.len()
        )
        .into_bytes();
        out.extend_from_slice(&s);
        out.extend_from_slice(b"\";}i:1;a:0:{}}");
        Ok(V::Str(out))
    }

    fn unserialize(&mut self, s: &[u8]) -> R<V> {
        let mut p = SerParser { s, i: 0 };
        let Some((class, props)) = p.object() else {
            return unsupported("unserialize format");
        };
        if p.i != s.len() {
            return unsupported("trailing data");
        }
        let class = class.as_str();
        if class != RFC && class != WH {
            return unsupported("unserialize class");
        }
        // Mirror uri_unserialize().
        let err = php_uri::api::invalid_serialization_data(class);
        let check = || -> Option<Vec<u8>> {
            if props.len() != 2 {
                return None;
            }
            let first = props.iter().find(|(k, _)| matches!(k, Key::Int(0)))?;
            let V::Arr(first) = &first.1 else { return None };
            if first.len() != 1 {
                return None;
            }
            let uri = first
                .iter()
                .find(|(k, _)| matches!(k, Key::Str(n) if n == b"uri"))?;
            let V::Str(uri) = &uri.1 else { return None };
            Some(uri.clone())
        };
        let Some(uri) = check() else {
            return Err(self.uri_error(err));
        };
        let kind = if class == RFC {
            match Rfc3986Uri::unserialize(&uri) {
                Ok(u) => Kind::Rfc(Some(u)),
                Err(e) => return Err(self.uri_error(e)),
            }
        } else {
            match WhatWgUrl::unserialize(&uri) {
                Ok(u) => Kind::Wh(Some(u)),
                Err(e) => return Err(self.uri_error(e)),
            }
        };
        let second_ok = props
            .iter()
            .find(|(k, _)| matches!(k, Key::Int(1)))
            .is_some_and(|(_, v)| matches!(v, V::Arr(a) if a.is_empty()));
        if !second_ok {
            return Err(self.uri_error(err));
        }
        Ok(self.obj(kind))
    }

    fn static_call(&mut self, cls: &str, m: &str, args: &[Arg]) -> R<V> {
        match (cls, m) {
            ("Uri\\Rfc3986\\Uri", "parse") => {
                let a = self.eval_args(args, &["uri", "baseUrl"])?;
                let s = self.req_str(a.first().and_then(Option::as_ref))?;
                let base = self.rfc_of(a.get(1).and_then(Option::as_ref))?;
                Ok(match Rfc3986Uri::parse(&s, base.as_ref()) {
                    Some(u) => self.obj(Kind::Rfc(Some(u))),
                    None => V::Null,
                })
            }
            ("Uri\\WhatWg\\Url", "parse") => {
                let params = ["uri", "baseUrl", "errors"];
                let a = self.eval_args(args, &params)?;
                let s = self.req_str(a.first().and_then(Option::as_ref))?;
                let base = self.wh_of(a.get(1).and_then(Option::as_ref))?;
                match WhatWgUrl::parse(&s, base.as_ref()) {
                    Ok((u, errs)) => {
                        self.write_errors(args, &params, 2, &errs)?;
                        Ok(self.obj(Kind::Wh(Some(u))))
                    }
                    Err(f) => {
                        self.write_errors(args, &params, 2, &f.errors)?;
                        Ok(V::Null)
                    }
                }
            }
            (c, "cases") if c.ends_with("Type") || c.ends_with("Mode") => {
                let names: Vec<&str> = match c {
                    "Uri\\WhatWg\\UrlValidationErrorType" => UrlValidationErrorType::ALL
                        .iter()
                        .map(|t| t.name())
                        .collect(),
                    "Uri\\UriComparisonMode" => vec!["IncludeFragment", "ExcludeFragment"],
                    _ => return unsupported("cases"),
                };
                let mut arr = Vec::new();
                for (i, n) in names.into_iter().enumerate() {
                    let v = self.enum_case(c, n);
                    arr.push((Key::Int(i as i64), v));
                }
                Ok(V::Arr(arr))
            }
            _ => unsupported(format!("static {cls}::{m}")),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn method(&mut self, o: &V, m: &str, args: &[Arg]) -> R<V> {
        let V::Obj(obj) = o else {
            return unsupported(format!("method {m} on non-object"));
        };
        let kind_tag = match &obj.borrow().kind {
            Kind::Rfc(_) => 1,
            Kind::Wh(_) => 2,
            Kind::RfcBuilder(_) => 3,
            Kind::WhBuilder(_) => 4,
            Kind::Exc { .. } => 5,
            _ => 0,
        };
        let s = |v: Option<Vec<u8>>| v.map_or(V::Null, V::Str);
        match kind_tag {
            1 => {
                let uri = match &obj.borrow().kind {
                    Kind::Rfc(Some(u)) => u.clone(),
                    _ => return unsupported("uninitialized uri"),
                };
                let a = self.eval_args(args, &["uri", "comparisonMode"])?;
                let a0 = a.first().cloned().flatten();
                let wrap = |me: &mut Self, r: Result<Rfc3986Uri, UriError>| match r {
                    Ok(u) => Ok(me.obj(Kind::Rfc(Some(u)))),
                    Err(e) => Err(me.uri_error(e)),
                };
                match m {
                    "getUriType" => {
                        let n = uri.uri_type().name();
                        Ok(self.enum_case("Uri\\Rfc3986\\UriType", n))
                    }
                    "getScheme" => Ok(s(uri.scheme())),
                    "getRawScheme" => Ok(s(uri.raw_scheme())),
                    "getUserInfo" => Ok(s(uri.user_info())),
                    "getRawUserInfo" => Ok(s(uri.raw_user_info())),
                    "getUsername" => Ok(s(uri.username())),
                    "getRawUsername" => Ok(s(uri.raw_username())),
                    "getPassword" => Ok(s(uri.password())),
                    "getRawPassword" => Ok(s(uri.raw_password())),
                    "getHost" => Ok(s(uri.host())),
                    "getRawHost" => Ok(s(uri.raw_host())),
                    "getHostType" => Ok(match uri.host_type() {
                        Some(t) => self.enum_case("Uri\\Rfc3986\\UriHostType", t.name()),
                        None => V::Null,
                    }),
                    "getPort" => Ok(uri.port().map_or(V::Null, V::Int)),
                    "getPath" => Ok(V::Str(uri.path())),
                    "getRawPath" => Ok(V::Str(uri.raw_path())),
                    "getQuery" => Ok(s(uri.query())),
                    "getRawQuery" => Ok(s(uri.raw_query())),
                    "getFragment" => Ok(s(uri.fragment())),
                    "getRawFragment" => Ok(s(uri.raw_fragment())),
                    "withScheme" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, uri.with_scheme(v.as_deref()))
                    }
                    "withUserInfo" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, uri.with_user_info(v.as_deref()))
                    }
                    "withHost" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, uri.with_host(v.as_deref()))
                    }
                    "withPort" => {
                        let v = self.opt_int(a0.as_ref())?;
                        wrap(self, uri.with_port(v))
                    }
                    "withPath" => {
                        let v = self.req_str(a0.as_ref())?;
                        wrap(self, uri.with_path(&v))
                    }
                    "withQuery" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, uri.with_query(v.as_deref()))
                    }
                    "withFragment" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, uri.with_fragment(v.as_deref()))
                    }
                    "equals" => {
                        let other = self.rfc_of(a0.as_ref())?;
                        let Some(other) = other else {
                            return unsupported("equals null");
                        };
                        let mode = self.mode_of(a.get(1).and_then(Option::as_ref))?;
                        Ok(V::Bool(uri.equals(&other, mode)))
                    }
                    "toString" => Ok(V::Str(uri.to_string_normalized())),
                    "toRawString" => Ok(V::Str(uri.to_raw_string())),
                    "resolve" => {
                        let v = self.req_str(a0.as_ref())?;
                        wrap(self, uri.resolve(&v))
                    }
                    "__serialize" => {
                        let inner =
                            V::Arr(vec![(Key::Str(b"uri".to_vec()), V::Str(uri.serialize()))]);
                        Ok(V::Arr(vec![
                            (Key::Int(0), inner),
                            (Key::Int(1), V::Arr(Vec::new())),
                        ]))
                    }
                    "__unserialize" => {
                        Err(self.uri_error(php_uri::api::invalid_serialization_data(RFC)))
                    }
                    "__construct" => {
                        Err(self.uri_error(php_uri::api::cannot_modify_readonly_object(RFC)))
                    }
                    "__debugInfo" => Ok(self.debug_array(&uri.debug_info())),
                    _ => unsupported(format!("Rfc method {m}")),
                }
            }
            2 => {
                let url = match &obj.borrow().kind {
                    Kind::Wh(Some(u)) => u.clone(),
                    _ => return unsupported("uninitialized url"),
                };
                let a = self.eval_args(args, &["url", "comparisonMode"])?;
                let a0 = a.first().cloned().flatten();
                let wrap = |me: &mut Self, r: Result<WhatWgUrl, UriError>| match r {
                    Ok(u) => Ok(me.obj(Kind::Wh(Some(u)))),
                    Err(e) => Err(me.uri_error(e)),
                };
                match m {
                    "getScheme" => Ok(V::Str(url.scheme())),
                    "isSpecialScheme" => Ok(V::Bool(url.is_special_scheme())),
                    "getUsername" => Ok(s(url.username())),
                    "getPassword" => Ok(s(url.password())),
                    "getAsciiHost" => Ok(s(url.ascii_host())),
                    "getUnicodeHost" => Ok(s(url.unicode_host())),
                    "getHostType" => Ok(match url.host_type() {
                        Some(t) => self.enum_case("Uri\\WhatWg\\UrlHostType", t.name()),
                        None => V::Null,
                    }),
                    "getPort" => Ok(url.port().map_or(V::Null, V::Int)),
                    "getPath" => Ok(V::Str(url.path())),
                    "getQuery" => Ok(s(url.query())),
                    "getFragment" => Ok(s(url.fragment())),
                    "withScheme" => {
                        let v = self.req_str(a0.as_ref())?;
                        wrap(self, url.with_scheme(&v))
                    }
                    "withUsername" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, url.with_username(v.as_deref()))
                    }
                    "withPassword" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, url.with_password(v.as_deref()))
                    }
                    "withHost" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, url.with_host(v.as_deref()))
                    }
                    "withPort" => {
                        let v = self.opt_int(a0.as_ref())?;
                        wrap(self, url.with_port(v))
                    }
                    "withPath" => {
                        let v = self.req_str(a0.as_ref())?;
                        wrap(self, url.with_path(&v))
                    }
                    "withQuery" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, url.with_query(v.as_deref()))
                    }
                    "withFragment" => {
                        let v = self.opt_str(a0.as_ref())?;
                        wrap(self, url.with_fragment(v.as_deref()))
                    }
                    "equals" => {
                        let other = self.wh_of(a0.as_ref())?;
                        let Some(other) = other else {
                            return unsupported("equals null");
                        };
                        let mode = self.mode_of(a.get(1).and_then(Option::as_ref))?;
                        Ok(V::Bool(url.equals(&other, mode)))
                    }
                    "toAsciiString" => Ok(V::Str(url.to_ascii_string())),
                    "toUnicodeString" => Ok(V::Str(url.to_unicode_string())),
                    "resolve" => {
                        let params = ["uri", "softErrors"];
                        let a = self.eval_args(args, &params)?;
                        let v = self.req_str(a.first().and_then(Option::as_ref))?;
                        match url.resolve(&v) {
                            Ok((u, errs)) => {
                                self.write_errors(args, &params, 1, &errs)?;
                                Ok(self.obj(Kind::Wh(Some(u))))
                            }
                            Err(e) => {
                                self.write_ref(args, &params, 1, V::Arr(Vec::new()))?;
                                Err(self.uri_error(e))
                            }
                        }
                    }
                    "__serialize" => {
                        let inner =
                            V::Arr(vec![(Key::Str(b"uri".to_vec()), V::Str(url.serialize()))]);
                        Ok(V::Arr(vec![
                            (Key::Int(0), inner),
                            (Key::Int(1), V::Arr(Vec::new())),
                        ]))
                    }
                    "__unserialize" => {
                        Err(self.uri_error(php_uri::api::invalid_serialization_data(WH)))
                    }
                    "__construct" => {
                        Err(self.uri_error(php_uri::api::cannot_modify_readonly_object(WH)))
                    }
                    "__debugInfo" => Ok(self.debug_array(&url.debug_info())),
                    _ => unsupported(format!("WhatWg method {m}")),
                }
            }
            3 => {
                let a = self.eval_args(args, &["value"])?;
                let a0 = a.first().cloned().flatten();
                if m == "build" {
                    let base = self.rfc_of(a0.as_ref())?;
                    let b = match &obj.borrow().kind {
                        Kind::RfcBuilder(b) => b.clone(),
                        _ => unreachable!(),
                    };
                    return match b.build(base.as_ref()) {
                        Ok(u) => Ok(self.obj(Kind::Rfc(Some(u)))),
                        Err(e) => Err(self.uri_error(e)),
                    };
                }
                let r = {
                    let sv = self.opt_str(a0.as_ref());
                    let iv = self.opt_int(a0.as_ref());
                    let mut borrow = obj.borrow_mut();
                    let Kind::RfcBuilder(b) = &mut borrow.kind else {
                        unreachable!()
                    };
                    match m {
                        "reset" => {
                            b.reset();
                            Ok(())
                        }
                        "setScheme" => b.set_scheme(sv?.as_deref()),
                        "setUserInfo" => b.set_user_info(sv?.as_deref()),
                        "setHost" => b.set_host(sv?.as_deref()),
                        "setPort" => b.set_port(iv?),
                        "setPath" => match sv? {
                            Some(p) => b.set_path(&p),
                            None => return unsupported("setPath(null)"),
                        },
                        "setQuery" => b.set_query(sv?.as_deref()),
                        "setFragment" => b.set_fragment(sv?.as_deref()),
                        _ => return unsupported(format!("RfcBuilder {m}")),
                    }
                };
                match r {
                    Ok(()) => Ok(o.clone()),
                    Err(e) => Err(self.uri_error(e)),
                }
            }
            4 => {
                if m == "build" {
                    let params = ["baseUrl", "softErrors"];
                    let a = self.eval_args(args, &params)?;
                    let base = self.wh_of(a.first().and_then(Option::as_ref))?;
                    let b = match &obj.borrow().kind {
                        Kind::WhBuilder(b) => b.clone(),
                        _ => unreachable!(),
                    };
                    return match b.build(base.as_ref()) {
                        Ok((u, errs)) => {
                            self.write_errors(args, &params, 1, &errs)?;
                            Ok(self.obj(Kind::Wh(Some(u))))
                        }
                        Err(e) => {
                            self.write_ref(args, &params, 1, V::Arr(Vec::new()))?;
                            Err(self.uri_error(e))
                        }
                    };
                }
                let a = self.eval_args(args, &["value"])?;
                let a0 = a.first().cloned().flatten();
                let r = {
                    let sv = self.opt_str(a0.as_ref());
                    let iv = self.opt_int(a0.as_ref());
                    let mut borrow = obj.borrow_mut();
                    let Kind::WhBuilder(b) = &mut borrow.kind else {
                        unreachable!()
                    };
                    match m {
                        "reset" => {
                            b.reset();
                            Ok(())
                        }
                        "setScheme" => match sv? {
                            Some(v) => b.set_scheme(&v),
                            None => return unsupported("setScheme(null)"),
                        },
                        "setUsername" => {
                            b.set_username(sv?.as_deref());
                            Ok(())
                        }
                        "setPassword" => {
                            b.set_password(sv?.as_deref());
                            Ok(())
                        }
                        "setHost" => b.set_host(sv?.as_deref()),
                        "setPort" => b.set_port(iv?),
                        "setPath" => match sv? {
                            Some(p) => {
                                b.set_path(&p);
                                Ok(())
                            }
                            None => return unsupported("setPath(null)"),
                        },
                        "setQuery" => {
                            b.set_query(sv?.as_deref());
                            Ok(())
                        }
                        "setFragment" => {
                            b.set_fragment(sv?.as_deref());
                            Ok(())
                        }
                        _ => return unsupported(format!("WhBuilder {m}")),
                    }
                };
                match r {
                    Ok(()) => Ok(o.clone()),
                    Err(e) => Err(self.uri_error(e)),
                }
            }
            5 => {
                let b = obj.borrow();
                let Kind::Exc {
                    message, previous, ..
                } = &b.kind
                else {
                    unreachable!()
                };
                match m {
                    "getMessage" => Ok(V::Str(message.clone())),
                    "getCode" => Ok(V::Int(0)),
                    "getPrevious" => Ok(previous.clone().unwrap_or(V::Null)),
                    _ => unsupported(format!("exception {m}")),
                }
            }
            _ => unsupported(format!("method {m}")),
        }
    }

    fn debug_array(&mut self, info: &[(&'static str, DebugValue)]) -> V {
        V::Arr(
            info.iter()
                .map(|(k, v)| {
                    (
                        Key::Str(k.as_bytes().to_vec()),
                        match v {
                            DebugValue::Null => V::Null,
                            DebugValue::Str(s) => V::Str(s.clone()),
                            DebugValue::Int(i) => V::Int(*i),
                        },
                    )
                })
                .collect(),
        )
    }

    fn dump(&self, v: &V, indent: usize, out: &mut Vec<u8>) {
        let pad = " ".repeat(indent);
        out.extend_from_slice(pad.as_bytes());
        self.dump_inline(v, indent, out);
    }

    fn dump_props(&self, props: &[(String, V)], indent: usize, out: &mut Vec<u8>) {
        for (k, v) in props {
            out.extend_from_slice(format!("{}[{}]=>\n", " ".repeat(indent + 2), k).as_bytes());
            self.dump(v, indent + 2, out);
        }
        out.extend_from_slice(format!("{}}}\n", " ".repeat(indent)).as_bytes());
    }

    fn dump_inline(&self, v: &V, indent: usize, out: &mut Vec<u8>) {
        match v {
            V::Null => out.extend_from_slice(b"NULL\n"),
            V::Bool(b) => out.extend_from_slice(format!("bool({b})\n").as_bytes()),
            V::Int(i) => out.extend_from_slice(format!("int({i})\n").as_bytes()),
            V::Str(s) => {
                out.extend_from_slice(format!("string({}) \"", s.len()).as_bytes());
                out.extend_from_slice(s);
                out.extend_from_slice(b"\"\n");
            }
            V::Arr(a) => {
                out.extend_from_slice(format!("array({}) {{\n", a.len()).as_bytes());
                let props: Vec<(String, V)> = a
                    .iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Key::Int(i) => i.to_string(),
                            Key::Str(s) => format!("\"{}\"", String::from_utf8_lossy(s)),
                        };
                        (key, v.clone())
                    })
                    .collect();
                self.dump_props(&props, indent, out);
            }
            V::Closure(..) => out.extend_from_slice(b"object(Closure)#0 (0) {\n}\n"),
            V::Obj(o) => {
                let b = o.borrow();
                let id = b.id;
                let q = |s: &str| format!("\"{s}\"");
                let (class, props): (String, Vec<(String, V)>) = match &b.kind {
                    Kind::Enum { class, case } => {
                        out.extend_from_slice(format!("enum({class}::{case})\n").as_bytes());
                        return;
                    }
                    Kind::Rfc(u) => (
                        RFC.into(),
                        u.as_ref()
                            .map(|u| debug_props(&u.debug_info()))
                            .unwrap_or_default(),
                    ),
                    Kind::Wh(u) => (
                        WH.into(),
                        u.as_ref()
                            .map(|u| debug_props(&u.debug_info()))
                            .unwrap_or_default(),
                    ),
                    Kind::RfcBuilder(bd) => {
                        let c = "Uri\\Rfc3986\\UriBuilder";
                        let p = |n: &str| format!("\"{n}\":\"{c}\":private");
                        let s = |v: &Option<Vec<u8>>| v.clone().map_or(V::Null, V::Str);
                        (
                            c.into(),
                            vec![
                                (p("scheme"), s(&bd.scheme)),
                                (p("userinfo"), s(&bd.userinfo)),
                                (p("host"), s(&bd.host)),
                                (p("port"), bd.port.map_or(V::Null, V::Int)),
                                (p("path"), V::Str(bd.path.clone())),
                                (p("query"), s(&bd.query)),
                                (p("fragment"), s(&bd.fragment)),
                            ],
                        )
                    }
                    Kind::WhBuilder(bd) => {
                        let c = "Uri\\WhatWg\\UrlBuilder";
                        let p = |n: &str| format!("\"{n}\":\"{c}\":private");
                        let s = |v: &Option<Vec<u8>>| v.clone().map_or(V::Null, V::Str);
                        (
                            c.into(),
                            vec![
                                (p("scheme"), V::Str(bd.scheme.clone())),
                                (p("username"), s(&bd.username)),
                                (p("password"), s(&bd.password)),
                                (p("host"), s(&bd.host)),
                                (p("port"), bd.port.map_or(V::Null, V::Int)),
                                (p("path"), V::Str(bd.path.clone())),
                                (p("query"), s(&bd.query)),
                                (p("fragment"), s(&bd.fragment)),
                            ],
                        )
                    }
                    Kind::ValErr {
                        context,
                        ty,
                        failure,
                    } => (
                        "Uri\\WhatWg\\UrlValidationError".into(),
                        vec![
                            (q("context"), V::Str(context.clone())),
                            (q("type"), ty.clone()),
                            (q("failure"), V::Bool(*failure)),
                        ],
                    ),
                    Kind::Exc {
                        class,
                        message,
                        errors,
                        ..
                    } => {
                        let mut props = vec![
                            ("\"message\":protected".to_string(), V::Str(message.clone())),
                            (
                                "\"string\":\"Exception\":private".to_string(),
                                V::Str(Vec::new()),
                            ),
                            ("\"code\":protected".to_string(), V::Int(0)),
                        ];
                        if let Some(e) = errors {
                            props.push((q("errors"), e.clone()));
                        }
                        (class.clone(), props)
                    }
                };
                out.extend_from_slice(
                    format!("object({class})#{id} ({}) {{\n", props.len()).as_bytes(),
                );
                self.dump_props(&props, indent, out);
            }
        }
    }
}

fn debug_props(info: &[(&'static str, DebugValue)]) -> Vec<(String, V)> {
    info.iter()
        .map(|(k, v)| {
            (
                format!("\"{k}\""),
                match v {
                    DebugValue::Null => V::Null,
                    DebugValue::Str(s) => V::Str(s.clone()),
                    DebugValue::Int(i) => V::Int(*i),
                },
            )
        })
        .collect()
}

fn truthy(v: &V) -> bool {
    match v {
        V::Null => false,
        V::Bool(b) => *b,
        V::Int(i) => *i != 0,
        V::Str(s) => !(s.is_empty() || s == b"0"),
        V::Arr(a) => !a.is_empty(),
        V::Obj(_) | V::Closure(..) => true,
    }
}

fn identical(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Int(x), V::Int(y)) => x == y,
        (V::Str(x), V::Str(y)) => x == y,
        (V::Arr(x), V::Arr(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((k1, v1), (k2, v2))| key_eq(k1, k2) && identical(v1, v2))
        }
        (V::Obj(x), V::Obj(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn key_eq(a: &Key, b: &Key) -> bool {
    match (a, b) {
        (Key::Int(x), Key::Int(y)) => x == y,
        (Key::Str(x), Key::Str(y)) => x == y,
        _ => false,
    }
}

fn loose_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Null, V::Bool(x)) | (V::Bool(x), V::Null) => !x,
        (V::Str(s), V::Null) | (V::Null, V::Str(s)) => s.is_empty(),
        (V::Arr(x), V::Arr(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .find(|(k2, _)| key_eq(k, k2))
                        .is_some_and(|(_, v2)| loose_eq(v, v2))
                })
        }
        (V::Obj(x), V::Obj(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            match (&x.kind, &y.kind) {
                (
                    Kind::ValErr {
                        context: c1,
                        ty: t1,
                        failure: f1,
                    },
                    Kind::ValErr {
                        context: c2,
                        ty: t2,
                        failure: f2,
                    },
                ) => c1 == c2 && identical(t1, t2) && f1 == f2,
                (Kind::Rfc(u1), Kind::Rfc(u2)) => u1 == u2,
                (Kind::Wh(u1), Kind::Wh(u2)) => u1 == u2,
                _ => false,
            }
        }
        _ => identical(a, b),
    }
}

struct SerParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl SerParser<'_> {
    fn expect(&mut self, b: &[u8]) -> Option<()> {
        if self.s[self.i..].starts_with(b) {
            self.i += b.len();
            Some(())
        } else {
            None
        }
    }

    fn int_until(&mut self, end: u8) -> Option<i64> {
        let start = self.i;
        while self.i < self.s.len() && self.s[self.i] != end {
            self.i += 1;
        }
        let v = std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()?;
        self.i += 1;
        Some(v)
    }

    fn string_body(&mut self) -> Option<Vec<u8>> {
        let len = usize::try_from(self.int_until(b':')?).ok()?;
        self.expect(b"\"")?;
        let v = self.s.get(self.i..self.i + len)?.to_vec();
        self.i += len;
        self.expect(b"\"")?;
        Some(v)
    }

    fn value(&mut self) -> Option<V> {
        let t = *self.s.get(self.i)?;
        self.i += 1;
        match t {
            b'N' => {
                self.expect(b";")?;
                Some(V::Null)
            }
            b'i' => {
                self.expect(b":")?;
                Some(V::Int(self.int_until(b';')?))
            }
            b'b' => {
                self.expect(b":")?;
                Some(V::Bool(self.int_until(b';')? != 0))
            }
            b's' => {
                self.expect(b":")?;
                let v = self.string_body()?;
                self.expect(b";")?;
                Some(V::Str(v))
            }
            b'a' => {
                self.expect(b":")?;
                Some(V::Arr(self.entries()?))
            }
            _ => None,
        }
    }

    fn entries(&mut self) -> Option<Vec<(Key, V)>> {
        let n = self.int_until(b':')?;
        self.expect(b"{")?;
        let mut out = Vec::new();
        for _ in 0..n {
            let k = match self.value()? {
                V::Int(i) => Key::Int(i),
                V::Str(s) => Key::Str(s),
                _ => return None,
            };
            let v = self.value()?;
            out.push((k, v));
        }
        self.expect(b"}")?;
        Some(out)
    }

    fn object(&mut self) -> Option<(String, Vec<(Key, V)>)> {
        self.expect(b"O:")?;
        let name = self.string_body()?;
        self.expect(b":")?;
        let props = self.entries()?;
        Some((normalize_class(&String::from_utf8_lossy(&name)), props))
    }
}

pub fn uncaught(interp: &Interp, ex: &V) -> Vec<u8> {
    let class = interp.class_of(ex);
    let msg = match ex {
        V::Obj(o) => match &o.borrow().kind {
            Kind::Exc { message, .. } => message.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut out = format!("\nFatal error: Uncaught {class}: ").into_bytes();
    out.extend_from_slice(&msg);
    out.extend_from_slice(b" in /t.php:1\nStack trace:\n#0 {main}\n  thrown in /t.php on line 1\n");
    out
}
