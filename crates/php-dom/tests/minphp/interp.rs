//! The evaluator.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use php_dom::dispatch::{self, DomObject, PropRead};
use php_dom::lists::Offset;
use php_dom::{ArrayKey, DomError, DomHost, DomValue, HostCallable, Node, XPathArg, XPathReturn};
use php_parser::ast::*;

use super::output::{print_r, to_bytes, var_dump, var_export};
use super::value::{new_obj, Arr, Callable, Key, Obj, ObjKind, UserFunc, V};

pub enum Flow {
    Break(u32),
    Continue(u32),
    Return(V),
    Throw(V),
    Exit,
}

pub type R<T> = Result<T, Flow>;

#[derive(Default)]
struct Frame {
    vars: HashMap<Vec<u8>, V>,
    statics: Vec<(String, Vec<u8>)>,
    this: Option<V>,
    class: Option<String>,
    static_class: Option<String>,
    func: String,
}

pub struct UserClass {
    pub name: String,
    pub parent: Option<String>,
    pub interfaces: Vec<String>,
    pub methods: HashMap<String, (Rc<UserFunc>, bool)>,
    pub props: Vec<(Vec<u8>, Option<Expr>, bool)>,
    pub consts: Vec<(Vec<u8>, Expr)>,
    pub is_abstract: bool,
}

pub struct Interp {
    pub out: RefCell<Vec<u8>>,
    pub file: String,
    pub dir: String,
    frames: RefCell<Vec<Frame>>,
    functions: RefCell<HashMap<Vec<u8>, Rc<UserFunc>>>,
    classes: RefCell<HashMap<String, Rc<UserClass>>>,
    constants: RefCell<HashMap<Vec<u8>, V>>,
    uses: RefCell<HashMap<String, String>>,
    namespace: RefCell<String>,
    pub line: Cell<u32>,
    pub cur_fn: RefCell<String>,
    silence: Cell<u32>,
    enum_cases: RefCell<HashMap<String, Rc<Obj>>>,
    shutdown_fns: RefCell<Vec<V>>,
    ob_stack: RefCell<Vec<Vec<u8>>>,
    statics: RefCell<HashMap<(String, Vec<u8>), V>>,
    list_objs: RefCell<HashMap<usize, Weak<Obj>>>,
}

thread_local! {
    static CURRENT: RefCell<Option<Weak<Interp>>> = const { RefCell::new(None) };
}

fn cur() -> Option<Rc<Interp>> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(Weak::upgrade))
}

fn lc(s: &str) -> String {
    s.to_ascii_lowercase()
}

const BUILTIN_CLASSES: &[(&str, Option<&str>, &[&str])] = &[
    ("Throwable", None, &["Stringable"]),
    ("Stringable", None, &[]),
    ("Traversable", None, &[]),
    ("Iterator", None, &["Traversable"]),
    ("IteratorAggregate", None, &["Traversable"]),
    ("Countable", None, &[]),
    ("ArrayAccess", None, &[]),
    ("UnitEnum", None, &[]),
    ("BackedEnum", None, &["UnitEnum"]),
    ("Exception", None, &["Throwable"]),
    ("Error", None, &["Throwable"]),
    ("TypeError", Some("Error"), &[]),
    ("ValueError", Some("Error"), &[]),
    ("ArgumentCountError", Some("TypeError"), &[]),
    ("ArithmeticError", Some("Error"), &[]),
    ("DivisionByZeroError", Some("ArithmeticError"), &[]),
    ("RuntimeException", Some("Exception"), &[]),
    ("LogicException", Some("Exception"), &[]),
    ("InvalidArgumentException", Some("LogicException"), &[]),
    ("UnexpectedValueException", Some("RuntimeException"), &[]),
    ("stdClass", None, &[]),
    ("Closure", None, &[]),
    ("InternalIterator", None, &["Iterator"]),
    ("ArrayIterator", None, &["Iterator", "Countable"]),
];

impl Interp {
    pub fn new(file: &str) -> Rc<Interp> {
        let dir = std::path::Path::new(file)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let i = Rc::new(Interp {
            out: RefCell::new(Vec::new()),
            file: file.to_string(),
            dir,
            frames: RefCell::new(vec![Frame { func: String::new(), ..Default::default() }]),
            functions: RefCell::new(HashMap::new()),
            classes: RefCell::new(HashMap::new()),
            constants: RefCell::new(HashMap::new()),
            uses: RefCell::new(HashMap::new()),
            namespace: RefCell::new(String::new()),
            line: Cell::new(1),
            cur_fn: RefCell::new(String::new()),
            silence: Cell::new(0),
            enum_cases: RefCell::new(HashMap::new()),
            shutdown_fns: RefCell::new(Vec::new()),
            ob_stack: RefCell::new(Vec::new()),
            statics: RefCell::new(HashMap::new()),
            list_objs: RefCell::new(HashMap::new()),
        });
        CURRENT.with(|c| *c.borrow_mut() = Some(Rc::downgrade(&i)));
        php_dom::error::set_diagnostic_handler(Some(Box::new(|d| {
            if let Some(i) = cur() {
                let label = match d.level {
                    php_dom::error::Level::Warning => "Warning",
                    php_dom::error::Level::Notice => "Notice",
                    php_dom::error::Level::Deprecated => "Deprecated",
                };
                if d.level == php_dom::error::Level::Deprecated {
                    i.emit_raw(label, d.message.as_bytes());
                } else {
                    i.emit_diag(label, d.message.as_bytes());
                }
            }
        })));
        php_libxml::error::set_diagnostic_handler(Some(Box::new(|d| {
            if let Some(i) = cur() {
                let label = match d.level {
                    php_libxml::error::DiagnosticLevel::Warning => "Warning",
                    php_libxml::error::DiagnosticLevel::Notice => "Notice",
                };
                i.emit_diag(label, &d.message);
            }
        })));
        i
    }

    fn emit_raw(&self, label: &str, msg: &[u8]) {
        if self.silence.get() > 0 {
            return;
        }
        let mut o = self.out.borrow_mut();
        o.extend_from_slice(format!("\n{label}: ").as_bytes());
        o.extend_from_slice(msg);
        o.extend_from_slice(format!(" in {} on line {}\n", self.file, self.line.get()).as_bytes());
    }

    pub fn emit_diag(&self, label: &str, msg: &[u8]) {
        if self.silence.get() > 0 {
            return;
        }
        let f = self.cur_fn.borrow().clone();
        let mut o = self.out.borrow_mut();
        o.extend_from_slice(format!("\n{label}: ").as_bytes());
        if !f.is_empty() {
            o.extend_from_slice(format!("{f}(): ").as_bytes());
        }
        o.extend_from_slice(msg);
        o.extend_from_slice(format!(" in {} on line {}\n", self.file, self.line.get()).as_bytes());
    }

    pub fn echo(&self, b: &[u8]) {
        self.out.borrow_mut().extend_from_slice(b);
    }

    // ------------------------------------------------------------------
    // Running
    // ------------------------------------------------------------------

    pub fn run(self: &Rc<Self>, src: &[u8]) {
        let opts = php_parser::ParseOptions::default();
        let prog = match php_parser::parse(src, &opts) {
            Ok(p) => p,
            Err(e) => {
                self.echo(format!("\nPARSE ERROR: {e:?}\n").as_bytes());
                return;
            }
        };
        self.hoist(&prog.statements);
        let r = self.exec_block(&prog.statements);
        match r {
            Err(Flow::Throw(e)) => self.uncaught(&e),
            Err(Flow::Exit) | Ok(()) | Err(_) => {}
        }
        let fns = std::mem::take(&mut *self.shutdown_fns.borrow_mut());
        for f in fns {
            let _ = self.call_value(&f, vec![]);
        }
    }

    fn uncaught(&self, e: &V) {
        let V::Obj(o) = e else { return };
        let msg = o.prop(b"message").map(|m| to_bytes(&m)).unwrap_or_default();
        let line = o.prop(b"line").map(|l| to_bytes(&l)).unwrap_or_default();
        let trace = o.prop(b"__trace").map(|l| to_bytes(&l)).unwrap_or_default();
        let mut out = self.out.borrow_mut();
        out.extend_from_slice(format!("\nFatal error: Uncaught {}: ", o.class.borrow()).as_bytes());
        out.extend_from_slice(&msg);
        out.extend_from_slice(format!(" in {}:{}\nStack trace:\n", self.file, String::from_utf8_lossy(&line)).as_bytes());
        out.extend_from_slice(&trace);
        out.extend_from_slice(format!("#{} {{main}}\n  thrown in {} on line {}\n", trace.iter().filter(|&&b| b == b'\n').count(), self.file, String::from_utf8_lossy(&line)).as_bytes());
    }

    fn hoist(&self, stmts: &[Stmt]) {
        for s in stmts {
            match &s.kind {
                StmtKind::Function(f) => self.declare_function(f),
                StmtKind::Class(c) => self.declare_class(c),
                StmtKind::Namespace { body: Some(b), .. } => self.hoist(b),
                _ => {}
            }
        }
    }

    fn declare_function(&self, f: &FunctionDecl) {
        self.functions.borrow_mut().insert(
            f.name.name.to_ascii_lowercase(),
            Rc::new(UserFunc {
                name: f.name.name.clone(),
                params: f.params.clone(),
                body: f.body.clone(),
                is_static: false,
            }),
        );
    }

    fn declare_class(&self, c: &ClassDecl) {
        let Some(name) = &c.name else { return };
        let name = String::from_utf8_lossy(&name.name).into_owned();
        let parent = c.extends.as_ref().map(|n| self.resolve_class_name(n));
        let interfaces = c.implements.iter().map(|n| self.resolve_class_name(n)).collect();
        let mut methods = HashMap::new();
        let mut props = Vec::new();
        let mut consts = Vec::new();
        for m in &c.members {
            match &m.kind {
                ClassMemberKind::Method(md) => {
                    if let Some(body) = &md.body {
                        methods.insert(
                            lc(&String::from_utf8_lossy(&md.name.name)),
                            (
                                Rc::new(UserFunc {
                                    name: md.name.name.clone(),
                                    params: md.params.clone(),
                                    body: body.clone(),
                                    is_static: md.modifiers.contains(Modifiers::STATIC),
                                }),
                                md.modifiers.contains(Modifiers::STATIC),
                            ),
                        );
                    }
                }
                ClassMemberKind::Property(pg) => {
                    for p in &pg.props {
                        props.push((p.name.name.clone(), p.default.clone(), pg.modifiers.contains(Modifiers::STATIC)));
                    }
                }
                ClassMemberKind::Const(cg) => {
                    for cc in &cg.consts {
                        consts.push((cc.name.name.clone(), cc.value.clone()));
                    }
                }
                _ => {}
            }
            // promoted constructor params
            if let ClassMemberKind::Method(md) = &m.kind {
                if md.name.name.eq_ignore_ascii_case(b"__construct") {
                    for p in &md.params {
                        if p.modifiers.0 != 0 {
                            props.push((p.name.name.clone(), None, false));
                        }
                    }
                }
            }
        }
        let uc = UserClass {
            name: name.clone(),
            parent,
            interfaces,
            methods,
            props,
            consts,
            is_abstract: c.modifiers.contains(Modifiers::ABSTRACT),
        };
        self.classes.borrow_mut().insert(lc(&name), Rc::new(uc));
    }

    fn resolve_class_name(&self, n: &Name) -> String {
        let s = String::from_utf8_lossy(&n.name).into_owned();
        if n.kind == NameKind::FullyQualified {
            return s.trim_start_matches('\\').to_string();
        }
        let first = s.split('\\').next().unwrap_or("").to_string();
        if let Some(full) = self.uses.borrow().get(&lc(&first)) {
            let rest = &s[first.len()..];
            return format!("{full}{rest}");
        }
        let ns = self.namespace.borrow().clone();
        if !ns.is_empty() && n.kind != NameKind::FullyQualified {
            let cand = format!("{ns}\\{s}");
            if self.class_exists(&cand) {
                return cand;
            }
        }
        s
    }

    pub fn class_exists(&self, name: &str) -> bool {
        self.classes.borrow().contains_key(&lc(name))
            || php_dom::classes::find_class(name).is_some()
            || BUILTIN_CLASSES.iter().any(|(c, _, _)| c.eq_ignore_ascii_case(name))
    }

    fn canonical_class(&self, name: &str) -> String {
        if let Some(c) = self.classes.borrow().get(&lc(name)) {
            return c.name.clone();
        }
        if let Some(c) = php_dom::classes::find_class(name) {
            return c.name.to_string();
        }
        if let Some((c, _, _)) = BUILTIN_CLASSES.iter().find(|(c, _, _)| c.eq_ignore_ascii_case(name)) {
            return (*c).to_string();
        }
        name.to_string()
    }

    /// Direct parents/interfaces of a class.
    fn class_supers(&self, name: &str) -> Vec<String> {
        if let Some(c) = self.classes.borrow().get(&lc(name)) {
            let mut v: Vec<String> = c.interfaces.clone();
            if let Some(p) = &c.parent {
                v.push(p.clone());
            }
            return v;
        }
        if let Some(c) = php_dom::classes::find_class(name) {
            let mut v: Vec<String> = c.interfaces.iter().map(|s| (*s).to_string()).collect();
            if let Some(p) = c.parent {
                v.push(p.to_string());
            }
            if c.name == "DOMException" {
                v.push("Exception".into());
            }
            return v;
        }
        if let Some((_, p, ifs)) = BUILTIN_CLASSES.iter().find(|(c, _, _)| c.eq_ignore_ascii_case(name)) {
            let mut v: Vec<String> = ifs.iter().map(|s| (*s).to_string()).collect();
            if let Some(p) = p {
                v.push((*p).to_string());
            }
            return v;
        }
        Vec::new()
    }

    pub fn is_subclass(&self, class: &str, base: &str) -> bool {
        if class.eq_ignore_ascii_case(base) {
            return true;
        }
        self.class_supers(class).iter().any(|s| self.is_subclass(s, base))
    }

    /// The built-in DOM class a class derives from.
    fn dom_builtin(&self, class: &str) -> Option<&'static str> {
        if let Some(c) = php_dom::classes::find_class(class) {
            if c.kind == php_dom::classes::ClassKind::Class {
                return Some(c.name);
            }
        }
        let parent = self.classes.borrow().get(&lc(class)).and_then(|c| c.parent.clone());
        parent.and_then(|p| self.dom_builtin(&p))
    }

    fn find_user_method(&self, class: &str, method: &str) -> Option<(String, Rc<UserFunc>, bool)> {
        let mut cur = Some(class.to_string());
        while let Some(c) = cur {
            let cls = self.classes.borrow().get(&lc(&c)).cloned();
            let Some(cls) = cls else { return None };
            if let Some((f, st)) = cls.methods.get(&lc(method)) {
                return Some((cls.name.clone(), f.clone(), *st));
            }
            cur = cls.parent.clone();
        }
        None
    }

    // ------------------------------------------------------------------
    // Variables
    // ------------------------------------------------------------------

    fn static_key(&self, name: &[u8]) -> Option<(String, Vec<u8>)> {
        let f = self.frames.borrow();
        let fr = f.last()?;
        fr.statics.iter().find(|(_, n)| n == name).map(|(k, n)| (k.clone(), n.clone()))
    }

    fn get_var(&self, name: &[u8]) -> V {
        if name == b"this" {
            return self.frames.borrow().last().and_then(|f| f.this.clone()).unwrap_or(V::Null);
        }
        if name == b"GLOBALS" {
            let mut a = Arr::default();
            for (k, v) in &self.frames.borrow()[0].vars {
                a.set(Key::Str(k.clone()), v.clone());
            }
            return V::arr(a);
        }
        if let Some(k) = self.static_key(name) {
            return self.statics.borrow().get(&k).cloned().unwrap_or(V::Null);
        }
        let f = self.frames.borrow();
        f.last().and_then(|fr| fr.vars.get(name).cloned()).unwrap_or(V::Null)
    }

    fn var_exists(&self, name: &[u8]) -> bool {
        if name == b"this" {
            return self.frames.borrow().last().is_some_and(|f| f.this.is_some());
        }
        if name == b"GLOBALS" || self.static_key(name).is_some() {
            return true;
        }
        self.frames.borrow().last().is_some_and(|f| f.vars.contains_key(name))
    }

    fn set_var(&self, name: &[u8], v: V) {
        if let Some(k) = self.static_key(name) {
            self.statics.borrow_mut().insert(k, v);
            return;
        }
        let mut f = self.frames.borrow_mut();
        if let Some(fr) = f.last_mut() {
            fr.vars.insert(name.to_vec(), v);
        }
    }

    fn unset_var(&self, name: &[u8]) {
        let mut f = self.frames.borrow_mut();
        if let Some(fr) = f.last_mut() {
            fr.vars.remove(name);
        }
    }

    fn var_name(&self, vn: &VarName) -> R<Vec<u8>> {
        match vn {
            VarName::Name(i) => Ok(i.name.clone()),
            VarName::Expr(e) => Ok(to_bytes(&self.eval(e)?)),
        }
    }

    fn member_name(&self, m: &MemberName) -> R<Vec<u8>> {
        match m {
            MemberName::Ident(i) => Ok(i.name.clone()),
            MemberName::Expr(e) => Ok(to_bytes(&self.eval(e)?)),
        }
    }

    // ------------------------------------------------------------------
    // Exceptions
    // ------------------------------------------------------------------

    pub fn make_exception(&self, class: &str, msg: &[u8], code: i64) -> V {
        let o = new_obj(class, ObjKind::Plain);
        o.set_prop(b"message", V::Str(msg.to_vec()));
        o.set_prop(b"code", V::Int(code));
        o.set_prop(b"file", V::Str(self.file.as_bytes().to_vec()));
        o.set_prop(b"line", V::Int(i64::from(self.line.get())));
        V::Obj(o)
    }

    fn throw_err(&self, class: &str, msg: &str) -> Flow {
        Flow::Throw(self.make_exception(class, msg.as_bytes(), 0))
    }

    fn dom_err(&self, e: DomError, func: &str, params: &[&str]) -> Flow {
        let variadic_from = php_dom::classes::find_method(func.split("::").next().unwrap_or(""), func.split("::").nth(1).unwrap_or(""))
            .and_then(|(_, m)| m.params.iter().position(|p| p.variadic));
        let arg_prefix = |arg: u32| {
            let idx = arg as usize - 1;
            if variadic_from.is_some_and(|v| idx >= v) {
                return format!("{func}(): Argument #{arg} ");
            }
            let pname = params.get(idx).copied().unwrap_or("");
            format!("{func}(): Argument #{arg} (${pname}) ")
        };
        let v = match e {
            DomError::Dom { code, message } => self.make_exception("DOMException", message.as_bytes(), code.code()),
            DomError::Type(m) => self.make_exception("TypeError", m.as_bytes(), 0),
            DomError::Value(m) => self.make_exception("ValueError", m.as_bytes(), 0),
            DomError::Error(m) => self.make_exception("Error", m.as_bytes(), 0),
            DomError::Exception(m) => self.make_exception("Exception", m.as_bytes(), 0),
            DomError::ArgType { arg, message } => {
                self.make_exception("TypeError", format!("{}{message}", arg_prefix(arg)).as_bytes(), 0)
            }
            DomError::ArgValue { arg, message } => {
                self.make_exception("ValueError", format!("{}{message}", arg_prefix(arg)).as_bytes(), 0)
            }
            DomError::CouldntFetch => self.make_exception("Error", b"Couldn't fetch DOMNode", 0),
            DomError::Host(b) => match b.downcast::<V>() {
                Ok(v) => *v,
                Err(_) => self.make_exception("Error", b"host error", 0),
            },
            DomError::Silent => return Flow::Return(V::Bool(false)),
        };
        Flow::Throw(v)
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    pub fn exec_block(&self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            self.exec(s)?;
        }
        Ok(())
    }

    fn exec(&self, s: &Stmt) -> R<()> {
        self.line.set(s.span.line);
        match &s.kind {
            StmtKind::Block(b) => self.exec_block(b),
            StmtKind::Echo(es) => {
                for e in es {
                    let v = self.eval(e)?;
                    let b = self.to_str(&v)?;
                    self.echo(&b);
                }
                Ok(())
            }
            StmtKind::InlineHtml(h) => {
                self.echo(h);
                Ok(())
            }
            StmtKind::Expr(e) => {
                self.eval(e)?;
                Ok(())
            }
            StmtKind::If { branches, else_branch } => {
                for b in branches {
                    if self.eval(&b.cond)?.truthy() {
                        return self.exec(&b.body);
                    }
                }
                if let Some(e) = else_branch {
                    self.exec(e)?;
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                while self.eval(cond)?.truthy() {
                    match self.exec(body) {
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                return Err(Flow::Break(n - 1));
                            }
                            break;
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                return Err(Flow::Continue(n - 1));
                            }
                        }
                        other => other?,
                    }
                }
                Ok(())
            }
            StmtKind::DoWhile { body, cond } => {
                loop {
                    match self.exec(body) {
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                return Err(Flow::Break(n - 1));
                            }
                            break;
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                return Err(Flow::Continue(n - 1));
                            }
                        }
                        other => other?,
                    }
                    if !self.eval(cond)?.truthy() {
                        break;
                    }
                }
                Ok(())
            }
            StmtKind::For { init, cond, step, body } => {
                for e in init {
                    self.eval(e)?;
                }
                loop {
                    let mut ok = true;
                    for (i, c) in cond.iter().enumerate() {
                        let v = self.eval(c)?;
                        if i == cond.len() - 1 {
                            ok = v.truthy();
                        }
                    }
                    if !ok {
                        break;
                    }
                    match self.exec(body) {
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                return Err(Flow::Break(n - 1));
                            }
                            break;
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                return Err(Flow::Continue(n - 1));
                            }
                        }
                        other => other?,
                    }
                    for e in step {
                        self.eval(e)?;
                    }
                }
                Ok(())
            }
            StmtKind::Foreach { subject, key, value, body, .. } => self.exec_foreach(subject, key.as_ref(), value, body),
            StmtKind::Switch { subject, cases } => {
                let v = self.eval(subject)?;
                let mut matched = false;
                for c in cases {
                    if !matched {
                        match &c.cond {
                            None => matched = true,
                            Some(cond) => {
                                let cv = self.eval(cond)?;
                                matched = loose_eq(&v, &cv);
                            }
                        }
                    }
                    if matched {
                        match self.exec_block(&c.body) {
                            Err(Flow::Break(n)) => {
                                if n > 1 {
                                    return Err(Flow::Break(n - 1));
                                }
                                return Ok(());
                            }
                            Err(Flow::Continue(n)) => {
                                if n > 1 {
                                    return Err(Flow::Continue(n - 1));
                                }
                                return Ok(());
                            }
                            other => other?,
                        }
                    }
                }
                if !matched {
                    if let Some(d) = cases.iter().position(|c| c.cond.is_none()) {
                        for c in &cases[d..] {
                            match self.exec_block(&c.body) {
                                Err(Flow::Break(_)) => return Ok(()),
                                other => other?,
                            }
                        }
                    }
                }
                Ok(())
            }
            StmtKind::Break(n) => Err(Flow::Break(self.level(n.as_ref())?)),
            StmtKind::Continue(n) => Err(Flow::Continue(self.level(n.as_ref())?)),
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => V::Null,
                };
                Err(Flow::Return(v))
            }
            StmtKind::Global(vars) => {
                for v in vars {
                    if let ExprKind::Var(VarName::Name(n)) = &v.kind {
                        let g = self.frames.borrow()[0].vars.get(&n.name).cloned().unwrap_or(V::Null);
                        self.set_var(&n.name, g);
                    }
                }
                Ok(())
            }
            StmtKind::Static(vars) => {
                let fkey = format!("{}#{}", self.frames.borrow().last().map(|f| f.func.clone()).unwrap_or_default(), s.span.start);
                for sv in vars {
                    let key = (fkey.clone(), sv.name.name.clone());
                    let exists = self.statics.borrow().contains_key(&key);
                    if !exists {
                        let v = match &sv.default {
                            Some(d) => self.eval(d)?,
                            None => V::Null,
                        };
                        self.statics.borrow_mut().insert(key.clone(), v);
                    }
                    if let Some(f) = self.frames.borrow_mut().last_mut() {
                        f.statics.push(key);
                    }
                }
                Ok(())
            }
            StmtKind::Unset(es) => {
                for e in es {
                    self.unset(e)?;
                }
                Ok(())
            }
            StmtKind::Declare { body, .. } => {
                if let Some(b) = body {
                    self.exec(b)?;
                }
                Ok(())
            }
            StmtKind::Nop | StmtKind::Label(_) | StmtKind::HaltCompiler { .. } => Ok(()),
            StmtKind::Try { body, catches, finally } => {
                let r = self.exec_block(body);
                let r = match r {
                    Err(Flow::Throw(ex)) => {
                        let mut handled = None;
                        for c in catches {
                            let cls = ex_class(&ex);
                            if c.types.iter().any(|t| self.is_subclass(&cls, &self.resolve_class_name(t))) {
                                if let Some(v) = &c.var {
                                    self.set_var(&v.name, ex.clone());
                                }
                                handled = Some(self.exec_block(&c.body));
                                break;
                            }
                        }
                        handled.unwrap_or(Err(Flow::Throw(ex)))
                    }
                    other => other,
                };
                if let Some(f) = finally {
                    self.exec_block(f)?;
                }
                r
            }
            StmtKind::Function(_) | StmtKind::Class(_) => Ok(()),
            StmtKind::Const { consts, .. } => {
                for c in consts {
                    let v = self.eval(&c.value)?;
                    self.constants.borrow_mut().insert(c.name.name.clone(), v);
                }
                Ok(())
            }
            StmtKind::Namespace { name, body } => {
                *self.namespace.borrow_mut() = name.as_ref().map(|n| String::from_utf8_lossy(&n.name).into_owned()).unwrap_or_default();
                if let Some(b) = body {
                    self.exec_block(b)?;
                }
                Ok(())
            }
            StmtKind::Use { uses, .. } => {
                for u in uses {
                    let full = String::from_utf8_lossy(&u.name.name).trim_start_matches('\\').to_string();
                    let alias = match &u.alias {
                        Some(a) => String::from_utf8_lossy(&a.name).into_owned(),
                        None => full.rsplit('\\').next().unwrap_or(&full).to_string(),
                    };
                    self.uses.borrow_mut().insert(lc(&alias), full);
                }
                Ok(())
            }
            StmtKind::GroupUse { prefix, uses, .. } => {
                let p = String::from_utf8_lossy(&prefix.name).trim_start_matches('\\').to_string();
                for u in uses {
                    let full = format!("{p}\\{}", String::from_utf8_lossy(&u.name.name));
                    let alias = match &u.alias {
                        Some(a) => String::from_utf8_lossy(&a.name).into_owned(),
                        None => full.rsplit('\\').next().unwrap_or(&full).to_string(),
                    };
                    self.uses.borrow_mut().insert(lc(&alias), full);
                }
                Ok(())
            }
            StmtKind::Goto(_) => Err(self.throw_err("Error", "goto unsupported")),
        }
    }

    fn level(&self, e: Option<&Expr>) -> R<u32> {
        Ok(match e {
            Some(e) => match self.eval(e)? {
                V::Int(i) => u32::try_from(i).unwrap_or(1),
                _ => 1,
            },
            None => 1,
        })
    }

    fn exec_foreach(&self, subject: &Expr, key: Option<&Expr>, value: &Expr, body: &Stmt) -> R<()> {
        let s = self.eval(subject)?;
        let mut run = |k: V, v: V| -> R<bool> {
            if let Some(ke) = key {
                self.assign(ke, k)?;
            }
            if let ExprKind::Array { items, .. } = &value.kind {
                self.destructure(items, &v)?;
            } else {
                self.assign(value, v)?;
            }
            match self.exec(body) {
                Err(Flow::Break(n)) => {
                    if n > 1 {
                        return Err(Flow::Break(n - 1));
                    }
                    Ok(false)
                }
                Err(Flow::Continue(n)) => {
                    if n > 1 {
                        return Err(Flow::Continue(n - 1));
                    }
                    Ok(true)
                }
                Err(e) => Err(e),
                Ok(()) => Ok(true),
            }
        };
        match &s {
            V::Arr(a) => {
                for (k, v) in a.items.clone() {
                    let kv = match k {
                        Key::Int(i) => V::Int(i),
                        Key::Str(s) => V::Str(s),
                    };
                    if !run(kv, v)? {
                        break;
                    }
                }
            }
            V::Obj(o) => {
                let iter = self.get_iterator(o)?;
                loop {
                    let (valid, cur, k) = self.iter_state(&iter)?;
                    if !valid {
                        break;
                    }
                    if !run(k, cur)? {
                        break;
                    }
                    self.iter_next(&iter)?;
                }
            }
            _ => {
                self.emit_raw("Warning", format!("foreach() argument must be of type array|object, {} given", s.type_name()).as_bytes());
            }
        }
        Ok(())
    }

    fn get_iterator(&self, o: &Rc<Obj>) -> R<V> {
        let kind_dom = o.dom();
        if let Some(d) = kind_dom {
            if matches!(d, DomObject::NodeList(_) | DomObject::NamedNodeMap(_) | DomObject::TokenList(_)) {
                let it = dispatch::iterate(&d).map_err(|e| self.dom_err(e, "", &[]))?;
                return Ok(V::Obj(super::value::new_internal("InternalIterator", ObjKind::Iter(it))));
            }
        }
        if matches!(&*o.kind.borrow(), ObjKind::Iter(_) | ObjKind::ArrayIter { .. }) {
            return Ok(V::Obj(o.clone()));
        }
        let cls = o.class.borrow().clone();
        if self.is_subclass(&cls, "IteratorAggregate") {
            let it = self.call_method(&V::Obj(o.clone()), "getIterator", vec![])?;
            if let V::Obj(io) = &it {
                return self.get_iterator(io);
            }
        }
        if self.is_subclass(&cls, "Iterator") {
            self.call_method(&V::Obj(o.clone()), "rewind", vec![])?;
            return Ok(V::Obj(o.clone()));
        }
        // Plain object: iterate properties.
        let mut a = Arr::default();
        for (k, v) in o.props.borrow().iter() {
            a.set(Key::Str(k.clone()), v.clone());
        }
        let arr = Rc::new(a);
        Ok(V::Obj(super::value::new_internal("ArrayIterator", ObjKind::ArrayIter { arr, pos: 0 })))
    }

    pub fn const_by_name(&self, n: &str) -> R<V> {
        let name = Name { kind: NameKind::Unqualified, name: n.as_bytes().to_vec(), span: php_lexer::Span::new(0, 0, 0) };
        self.constant(&name)
    }

    pub fn resolve_path_pub(&self, p: &[u8]) -> String {
        let s = String::from_utf8_lossy(p).into_owned();
        if s.starts_with('/') { s } else { format!("{}/{}", std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(), s) }
    }

    pub fn ob_start(&self) {
        let cur = std::mem::take(&mut *self.out.borrow_mut());
        self.ob_stack.borrow_mut().push(cur);
    }

    pub fn ob_end(&self, end: bool) -> Vec<u8> {
        let contents = self.out.borrow().clone();
        if end {
            if let Some(prev) = self.ob_stack.borrow_mut().pop() {
                *self.out.borrow_mut() = prev;
            }
        }
        contents
    }

    pub fn clone_value(&self, v: &V) -> R<V> {
        let V::Obj(o) = v else {
            return Err(self.throw_err("Error", "__clone method called on non-object"));
        };
        if let Some(d) = o.dom() {
            let c = dispatch::clone_object(&d).map_err(|e| self.dom_err(e, "", &[]))?;
            let cls = o.class.borrow().clone();
            let new = new_obj(&cls, ObjKind::Dom(c.clone()));
            if let DomObject::Node(n) = &c {
                *n.host_data() = Some(Box::new(Rc::downgrade(&new)));
            }
            *new.props.borrow_mut() = o.props.borrow().clone();
            if self.find_user_method(&cls, "__clone").is_some() {
                self.call_method(&V::Obj(new.clone()), "__clone", vec![])?;
            }
            return Ok(V::Obj(new));
        }
        let new = new_obj(&o.class.borrow(), ObjKind::Plain);
        *new.props.borrow_mut() = o.props.borrow().clone();
        Ok(V::Obj(new))
    }

    pub fn get_iter_pub(&self, o: &Rc<Obj>) -> R<V> {
        self.get_iterator(o)
    }
    pub fn iter_state_pub(&self, it: &V) -> R<(bool, V, V)> {
        self.iter_state(it)
    }
    pub fn iter_next_pub(&self, it: &V) -> R<()> {
        self.iter_next(it)
    }

    fn iter_state(&self, it: &V) -> R<(bool, V, V)> {
        let V::Obj(o) = it else { return Ok((false, V::Null, V::Null)) };
        let k = &*o.kind.borrow();
        match k {
            ObjKind::Iter(h) => {
                let mut i = h.0.borrow_mut();
                if !i.valid() {
                    return Ok((false, V::Null, V::Null));
                }
                Ok((true, from_dom(i.current()), from_dom(i.key())))
            }
            ObjKind::ArrayIter { arr, pos } => match arr.items.get(*pos) {
                Some((k, v)) => Ok((true, v.clone(), match k {
                    Key::Int(i) => V::Int(*i),
                    Key::Str(s) => V::Str(s.clone()),
                })),
                None => Ok((false, V::Null, V::Null)),
            },
            _ => {
                let valid = self.call_method(it, "valid", vec![])?.truthy();
                if !valid {
                    return Ok((false, V::Null, V::Null));
                }
                let cur = self.call_method(it, "current", vec![])?;
                let key = self.call_method(it, "key", vec![])?;
                Ok((true, cur, key))
            }
        }
    }

    fn iter_next(&self, it: &V) -> R<()> {
        let V::Obj(o) = it else { return Ok(()) };
        let mut k = o.kind.borrow_mut();
        match &mut *k {
            ObjKind::Iter(h) => {
                let r = h.0.borrow_mut().next();
                drop(k);
                r.map_err(|e| self.dom_err(e, "", &[]))
            }
            ObjKind::ArrayIter { pos, .. } => {
                *pos += 1;
                Ok(())
            }
            _ => {
                drop(k);
                self.call_method(it, "next", vec![])?;
                Ok(())
            }
        }
    }

    fn destructure(&self, items: &[Option<ArrayItem>], v: &V) -> R<()> {
        let V::Arr(a) = v else { return Ok(()) };
        let mut idx = 0i64;
        for it in items.iter() {
            let Some(it) = it else {
                idx += 1;
                continue;
            };
            let k = match &it.key {
                Some(k) => to_key(&self.eval(k)?),
                None => {
                    let k = Key::Int(idx);
                    idx += 1;
                    k
                }
            };
            let val = a.get(&k).cloned().unwrap_or(V::Null);
            self.assign(&it.value, val)?;
        }
        Ok(())
    }

    fn unset(&self, e: &Expr) -> R<()> {
        match &e.kind {
            ExprKind::Var(vn) => {
                let n = self.var_name(vn)?;
                self.unset_var(&n);
            }
            ExprKind::Dim { base, index: Some(index) } => {
                let k = to_key(&self.eval(index)?);
                let bv = self.eval(base)?;
                if let V::Obj(o) = &bv {
                    if let Some(d) = o.dom() {
                        let _ = d;
                        return Err(self.throw_err("Error", &format!("Cannot unset offset in a non-array variable")));
                    }
                }
                self.write_lvalue(base, &mut |slot| {
                    if let V::Arr(a) = slot {
                        Rc::make_mut(a).remove(&k);
                    }
                    Ok(())
                })?;
            }
            ExprKind::Prop { object, name, .. } => {
                let o = self.eval(object)?;
                let n = self.member_name(name)?;
                if let V::Obj(o) = &o {
                    if let Some(d) = o.dom() {
                        if dispatch::is_handled_property(&d, &String::from_utf8_lossy(&n)) {
                            return Err(self.throw_err(
                                "Error",
                                &format!("Cannot unset {}::${}", o.class.borrow(), String::from_utf8_lossy(&n)),
                            ));
                        }
                    }
                    o.props.borrow_mut().retain(|(k, _)| *k != n);
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    pub fn to_str(&self, v: &V) -> R<Vec<u8>> {
        match v {
            V::Obj(o) => {
                let cls = o.class.borrow().clone();
                if self.find_user_method(&cls, "__toString").is_some() {
                    let r = self.call_method(v, "__toString", vec![])?;
                    return Ok(to_bytes(&r));
                }
                if self.is_subclass(&cls, "Throwable") {
                    return Ok(o.prop(b"message").map(|m| to_bytes(&m)).unwrap_or_default());
                }
                if let ObjKind::Enum(..) = &*o.kind.borrow() {
                    return Err(self.throw_err("Error", &format!("Object of class {cls} could not be converted to string")));
                }
                Err(self.throw_err("Error", &format!("Object of class {cls} could not be converted to string")))
            }
            V::Arr(_) => {
                self.emit_raw("Warning", b"Array to string conversion");
                Ok(b"Array".to_vec())
            }
            other => Ok(to_bytes(other)),
        }
    }

    pub fn eval(&self, e: &Expr) -> R<V> {
        match &e.kind {
            ExprKind::Int(i) => Ok(V::Int(*i)),
            ExprKind::Float(f) => Ok(V::Float(*f)),
            ExprKind::String(s) => Ok(V::Str(s.clone())),
            ExprKind::Interpolated(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        InterpPart::Literal(l, _) => out.extend_from_slice(l),
                        InterpPart::Expr { expr, .. } => {
                            let v = self.eval(expr)?;
                            out.extend_from_slice(&self.to_str(&v)?);
                        }
                    }
                }
                Ok(V::Str(out))
            }
            ExprKind::Array { items, .. } => {
                let mut a = Arr::default();
                for it in items.iter().flatten() {
                    if it.unpack {
                        if let V::Arr(src) = self.eval(&it.value)? {
                            for (k, v) in &src.items {
                                match k {
                                    Key::Int(_) => a.push(v.clone()),
                                    Key::Str(_) => a.set(k.clone(), v.clone()),
                                }
                            }
                        }
                        continue;
                    }
                    let v = self.eval(&it.value)?;
                    match &it.key {
                        Some(k) => {
                            let kv = self.eval(k)?;
                            a.set(to_key(&kv), v);
                        }
                        None => a.push(v),
                    }
                }
                Ok(V::arr(a))
            }
            ExprKind::Const(n) => self.constant(n),
            ExprKind::MagicConst(m) => Ok(match m {
                MagicConst::Line => V::Int(i64::from(e.span.line)),
                MagicConst::File => V::str(&self.file),
                MagicConst::Dir => V::str(&self.dir),
                MagicConst::Class => V::Str(self.frames.borrow().last().and_then(|f| f.class.clone()).unwrap_or_default().into_bytes()),
                MagicConst::Function | MagicConst::Method => V::str(&self.frames.borrow().last().map(|f| f.func.clone()).unwrap_or_default()),
                _ => V::str(""),
            }),
            ExprKind::ClassConst { class, name } => {
                let cname = self.class_ref_name(class)?;
                let n = self.member_name(name)?;
                if n == b"class" {
                    return Ok(V::Str(self.canonical_class(&cname).into_bytes()));
                }
                self.class_constant(&cname, &n)
            }
            ExprKind::ClassName(c) => Ok(V::Str(self.class_ref_name(c)?.into_bytes())),
            ExprKind::Var(vn) => {
                let n = self.var_name(vn)?;
                if !self.var_exists(&n) && n != b"GLOBALS" {
                    self.emit_raw("Warning", format!("Undefined variable ${}", String::from_utf8_lossy(&n)).as_bytes());
                }
                Ok(self.get_var(&n))
            }
            ExprKind::Dim { base, index } => {
                let b = self.eval(base)?;
                let Some(index) = index else {
                    if let V::Obj(o) = &b {
                        if o.dom().is_some() {
                            let c = o.class.borrow().clone();
                            let verb = if c.starts_with("Dom\\") { format!("Cannot append to {c}") } else { format!("Cannot access {c} without offset") };
                            return Err(self.throw_err("Error", &verb));
                        }
                    }
                    return Err(self.throw_err("Error", "Cannot use [] for reading"));
                };
                let k = self.eval(index)?;
                self.read_dim(&b, &k)
            }
            ExprKind::Prop { object, name, nullsafe } => {
                let o = self.eval(object)?;
                if *nullsafe && matches!(o, V::Null) {
                    return Ok(V::Null);
                }
                let n = self.member_name(name)?;
                self.read_prop(&o, &n)
            }
            ExprKind::StaticProp { .. } => Ok(V::Null),
            ExprKind::Call { callee, args } => {
                self.line.set(e.span.line);
                self.eval_call(callee, args, e.span.line)
            }
            ExprKind::MethodCall { object, name, args, nullsafe } => {
                let o = self.eval(object)?;
                if *nullsafe && matches!(o, V::Null) {
                    return Ok(V::Null);
                }
                let n = String::from_utf8_lossy(&self.member_name(name)?).into_owned();
                let a = self.eval_args(args)?;
                self.line.set(e.span.line);
                self.call_method_named(&o, &n, a.0, a.1)
            }
            ExprKind::StaticCall { class, name, args } => {
                let n = String::from_utf8_lossy(&self.member_name(name)?).into_owned();
                let a = self.eval_args(args)?;
                self.line.set(e.span.line);
                self.static_call(class, &n, a.0, a.1)
            }
            ExprKind::New { class, args } => {
                let cname = match &**class {
                    NewClass::Name(n) => {
                        if n.is_special("static") || n.is_special("self") {
                            self.frames.borrow().last().and_then(|f| f.class.clone()).unwrap_or_default()
                        } else {
                            self.resolve_class_name(n)
                        }
                    }
                    NewClass::Expr(ex) => {
                        let v = self.eval(ex)?;
                        match v {
                            V::Obj(o) => o.class.borrow().clone(),
                            other => String::from_utf8_lossy(&to_bytes(&other)).into_owned(),
                        }
                    }
                    NewClass::Anonymous(cd) => {
                        let mut cd = cd.clone();
                        let name = format!("class@anonymous{}", e.span.start);
                        cd.name = Some(Ident::new(name.clone().into_bytes(), e.span));
                        self.declare_class(&cd);
                        name
                    }
                };
                let a = self.eval_args(args)?;
                self.line.set(e.span.line);
                self.instantiate(&cname, a.0, a.1)
            }
            ExprKind::Assign { target, value } => {
                if let ExprKind::Array { items, .. } = &target.kind {
                    let v = self.eval(value)?;
                    self.destructure(items, &v)?;
                    return Ok(v);
                }
                let v = self.eval(value)?;
                self.assign(target, v.clone())?;
                Ok(v)
            }
            ExprKind::AssignRef { target, value } => {
                let v = self.eval(value)?;
                self.assign(target, v.clone())?;
                Ok(v)
            }
            ExprKind::AssignOp { op, target, value } => {
                if *op == BinaryOp::Coalesce {
                    let cur = self.eval_quiet(target)?;
                    if !matches!(cur, V::Null) {
                        return Ok(cur);
                    }
                    let v = self.eval(value)?;
                    self.assign(target, v.clone())?;
                    return Ok(v);
                }
                let cur = self.eval(target)?;
                let rhs = self.eval(value)?;
                let v = self.binop(*op, &cur, &rhs)?;
                self.assign(target, v.clone())?;
                Ok(v)
            }
            ExprKind::Binary { op, left, right } => match op {
                BinaryOp::BoolAnd => Ok(V::Bool(self.eval(left)?.truthy() && self.eval(right)?.truthy())),
                BinaryOp::BoolOr => Ok(V::Bool(self.eval(left)?.truthy() || self.eval(right)?.truthy())),
                BinaryOp::Coalesce => {
                    let l = self.eval_quiet(left)?;
                    if matches!(l, V::Null) {
                        self.eval(right)
                    } else {
                        Ok(l)
                    }
                }
                _ => {
                    let l = self.eval(left)?;
                    let r = self.eval(right)?;
                    self.binop(*op, &l, &r)
                }
            },
            ExprKind::Unary { op, expr } => {
                let v = self.eval(expr)?;
                Ok(match op {
                    UnaryOp::Not => V::Bool(!v.truthy()),
                    UnaryOp::Minus => match v {
                        V::Int(i) => V::Int(-i),
                        V::Float(f) => V::Float(-f),
                        other => match to_num(&other) {
                            V::Int(i) => V::Int(-i),
                            V::Float(f) => V::Float(-f),
                            _ => V::Int(0),
                        },
                    },
                    UnaryOp::Plus => to_num(&v),
                    UnaryOp::BitNot => V::Int(!to_int(&v)),
                })
            }
            ExprKind::IncDec { op, var } => {
                let cur = self.eval(var)?;
                let new = match &cur {
                    V::Int(i) => V::Int(if matches!(op, IncDecOp::PreInc | IncDecOp::PostInc) { i + 1 } else { i - 1 }),
                    V::Float(f) => V::Float(if matches!(op, IncDecOp::PreInc | IncDecOp::PostInc) { f + 1.0 } else { f - 1.0 }),
                    V::Null => if matches!(op, IncDecOp::PreInc | IncDecOp::PostInc) { V::Int(1) } else { V::Null },
                    other => {
                        let n = to_int(other);
                        V::Int(if matches!(op, IncDecOp::PreInc | IncDecOp::PostInc) { n + 1 } else { n - 1 })
                    }
                };
                self.assign(var, new.clone())?;
                Ok(if matches!(op, IncDecOp::PreInc | IncDecOp::PreDec) { new } else { cur })
            }
            ExprKind::Cast { kind, expr } => {
                let v = self.eval(expr)?;
                Ok(match kind {
                    CastKind::Int => V::Int(to_int(&v)),
                    CastKind::Float => match to_num(&v) {
                        V::Int(i) => V::Float(i as f64),
                        other => other,
                    },
                    CastKind::String => V::Str(self.to_str(&v)?),
                    CastKind::Bool => V::Bool(v.truthy()),
                    CastKind::Array => match v {
                        V::Arr(_) => v,
                        V::Null => V::arr(Arr::default()),
                        V::Obj(o) => {
                            let mut a = Arr::default();
                            for (k, v) in o.props.borrow().iter() {
                                a.set(Key::Str(k.clone()), v.clone());
                            }
                            V::arr(a)
                        }
                        other => {
                            let mut a = Arr::default();
                            a.push(other);
                            V::arr(a)
                        }
                    },
                    CastKind::Object => match v {
                        V::Arr(a) => {
                            let o = new_obj("stdClass", ObjKind::Plain);
                            for (k, v) in &a.items {
                                let kb = match k {
                                    Key::Int(i) => i.to_string().into_bytes(),
                                    Key::Str(s) => s.clone(),
                                };
                                o.set_prop(&kb, v.clone());
                            }
                            V::Obj(o)
                        }
                        V::Obj(_) => v,
                        _ => V::Obj(new_obj("stdClass", ObjKind::Plain)),
                    },
                    CastKind::Unset => V::Null,
                })
            }
            ExprKind::VoidCast(e) => {
                self.eval(e)?;
                Ok(V::Null)
            }
            ExprKind::Isset(es) => {
                for e in es {
                    if matches!(self.eval_isset(e)?, V::Null) {
                        return Ok(V::Bool(false));
                    }
                }
                Ok(V::Bool(true))
            }
            ExprKind::Empty(e) => Ok(V::Bool(!self.eval_isset(e)?.truthy())),
            ExprKind::Silence(e) => {
                self.silence.set(self.silence.get() + 1);
                let r = self.eval(e);
                self.silence.set(self.silence.get() - 1);
                r
            }
            ExprKind::Print(e) => {
                let v = self.eval(e)?;
                let b = self.to_str(&v)?;
                self.echo(&b);
                Ok(V::Int(1))
            }
            ExprKind::Include { expr, .. } => {
                let p = to_bytes(&self.eval(expr)?);
                let p = String::from_utf8_lossy(&p).into_owned();
                let path = if p.starts_with('/') { p } else { format!("{}/{}", self.dir, p) };
                match std::fs::read(&path) {
                    Ok(src) => {
                        let opts = php_parser::ParseOptions::default();
                        if let Ok(prog) = php_parser::parse(&src, &opts) {
                            self.hoist(&prog.statements);
                            match self.exec_block(&prog.statements) {
                                Err(Flow::Return(v)) => return Ok(v),
                                other => other?,
                            }
                        }
                        Ok(V::Int(1))
                    }
                    Err(_) => Ok(V::Bool(false)),
                }
            }
            ExprKind::Instanceof { expr, class } => {
                let v = self.eval(expr)?;
                let cname = self.class_ref_name(class)?;
                Ok(V::Bool(match &v {
                    V::Obj(o) => {
                        let oc = o.class.borrow().clone();
                        self.is_subclass(&oc, &cname)
                    }
                    _ => false,
                }))
            }
            ExprKind::Conditional { cond, then, else_, .. } => {
                let c = self.eval(cond)?;
                if c.truthy() {
                    match then {
                        Some(t) => self.eval(t),
                        None => Ok(c),
                    }
                } else {
                    self.eval(else_)
                }
            }
            ExprKind::Throw(e) => {
                let v = self.eval(e)?;
                Err(Flow::Throw(v))
            }
            ExprKind::Closure(c) => {
                let mut bound = Vec::new();
                for u in &c.uses {
                    bound.push((u.name.name.clone(), self.get_var(&u.name.name)));
                }
                let (this, scope) = {
                    let f = self.frames.borrow();
                    let fr = f.last();
                    (fr.and_then(|f| f.this.clone()), fr.and_then(|f| f.class.clone()))
                };
                let func = Rc::new(UserFunc { name: b"{closure}".to_vec(), params: c.params.clone(), body: c.body.clone(), is_static: c.is_static });
                Ok(V::Obj(new_obj("Closure", ObjKind::Closure(Rc::new(Callable::Closure { func, bound, this, scope, arrow: None })))))
            }
            ExprKind::ArrowFn(af) => {
                let bound: Vec<(Vec<u8>, V)> = self.frames.borrow().last().map(|f| f.vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
                let (this, scope) = {
                    let f = self.frames.borrow();
                    let fr = f.last();
                    (fr.and_then(|f| f.this.clone()), fr.and_then(|f| f.class.clone()))
                };
                let func = Rc::new(UserFunc { name: b"{closure}".to_vec(), params: af.params.clone(), body: Vec::new(), is_static: af.is_static });
                Ok(V::Obj(new_obj("Closure", ObjKind::Closure(Rc::new(Callable::Closure { func, bound, this, scope, arrow: Some(Rc::new((*af.body).clone())) })))))
            }
            ExprKind::Match { subject, arms } => {
                let v = self.eval(subject)?;
                for arm in arms {
                    match &arm.conds {
                        None => return self.eval(&arm.body),
                        Some(cs) => {
                            for c in cs {
                                if strict_eq(&v, &self.eval(c)?) {
                                    return self.eval(&arm.body);
                                }
                            }
                        }
                    }
                }
                Err(self.throw_err("UnhandledMatchError", "Unhandled match case"))
            }
            _ => Err(self.throw_err("Error", "unsupported expression")),
        }
    }

    fn eval_quiet(&self, e: &Expr) -> R<V> {
        self.silence.set(self.silence.get() + 1);
        let r = self.eval_isset(e);
        self.silence.set(self.silence.get() - 1);
        r
    }

    /// Evaluates for isset()/??: missing things are null, no warnings.
    fn eval_isset(&self, e: &Expr) -> R<V> {
        match &e.kind {
            ExprKind::Var(vn) => {
                let n = self.var_name(vn)?;
                Ok(self.get_var(&n))
            }
            ExprKind::Dim { base, index: Some(i) } => {
                let b = self.eval_isset(base)?;
                let k = self.eval(i)?;
                match &b {
                    V::Arr(a) => Ok(a.get(&to_key(&k)).cloned().unwrap_or(V::Null)),
                    V::Obj(o) => {
                        if let Some(d) = o.dom() {
                            let off = to_offset(&k);
                            let exists = dispatch::offset_exists(&d, &off).map_err(|e| self.dom_err(e, "", &[]))?;
                            if !exists {
                                return Ok(V::Null);
                            }
                            return self.read_dim(&b, &k);
                        }
                        Ok(V::Null)
                    }
                    V::Str(s) => {
                        let i = to_int(&k);
                        Ok(if i >= 0 && (i as usize) < s.len() { V::Str(vec![s[i as usize]]) } else { V::Null })
                    }
                    _ => Ok(V::Null),
                }
            }
            ExprKind::Prop { object, name, .. } => {
                let o = self.eval_isset(object)?;
                let n = self.member_name(name)?;
                match &o {
                    V::Obj(ob) => {
                        if let Some(d) = ob.dom() {
                            let ns = String::from_utf8_lossy(&n).into_owned();
                            if dispatch::is_handled_property(&d, &ns) {
                                return match dispatch::read_property(&d, &ns) {
                                    Ok(PropRead::Value(v)) => Ok(self.from_dom_value(v)),
                                    _ => Ok(V::Null),
                                };
                            }
                        }
                        Ok(ob.prop(&n).unwrap_or(V::Null))
                    }
                    _ => Ok(V::Null),
                }
            }
            _ => self.eval(e),
        }
    }

    fn constant(&self, n: &Name) -> R<V> {
        let raw = String::from_utf8_lossy(&n.name).into_owned();
        let name = raw.trim_start_matches('\\').to_string();
        let short = name.rsplit('\\').next().unwrap_or(&name).to_string();
        match lc(&short).as_str() {
            "true" => return Ok(V::Bool(true)),
            "false" => return Ok(V::Bool(false)),
            "null" => return Ok(V::Null),
            _ => {}
        }
        if let Some(v) = self.constants.borrow().get(name.as_bytes()) {
            return Ok(v.clone());
        }
        if name == "DOM_PHP_ERR" {
            self.emit_raw("Deprecated", b"Constant DOM_PHP_ERR is deprecated since 8.4, as it is no longer used");
            return Ok(V::Int(0));
        }
        if name == "PHP_MAXPATHLEN" {
            return Ok(V::Int(4096));
        }
        let v = match name.as_str() {
            "PHP_EOL" => V::str("\n"),
            "PHP_INT_MAX" => V::Int(i64::MAX),
            "PHP_INT_MIN" => V::Int(i64::MIN),
            "PHP_INT_SIZE" => V::Int(8),
            "PHP_VERSION" => V::str("8.5.0"),
            "PHP_OS" | "PHP_OS_FAMILY" => V::str("Linux"),
            "PHP_VERSION_ID" => V::Int(80500),
            "LIBXML_VERSION" => V::Int(20914),
            "LIBXML_DOTTED_VERSION" => V::str("2.9.14"),
            "E_ALL" => V::Int(32767),
            "E_WARNING" => V::Int(2),
            "E_NOTICE" => V::Int(8),
            "E_DEPRECATED" => V::Int(8192),
            "E_STRICT" => V::Int(2048),
            "INF" => V::Float(f64::INFINITY),
            "NAN" => V::Float(f64::NAN),
            "DIRECTORY_SEPARATOR" => V::str("/"),
            "JSON_PRETTY_PRINT" => V::Int(128),
            "JSON_THROW_ON_ERROR" => V::Int(4_194_304),
            "ENT_QUOTES" => V::Int(3),
            "SORT_STRING" => V::Int(2),
            _ => {
                if let Some(v) = libxml_const(&name) {
                    V::Int(v)
                } else if let Some((_, v)) = php_dom::constants::GLOBAL_CONSTANTS.iter().find(|(n, _)| *n == name) {
                    V::Int(*v)
                } else if let Some((_, v)) = php_dom::constants::DOM_NS_CONSTANTS.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name) || (*n == format!("Dom\\{short}") && name.contains('\\'))) {
                    V::Int(*v)
                } else {
                    return Err(self.throw_err("Error", &format!("Undefined constant \"{name}\"")));
                }
            }
        };
        Ok(v)
    }

    fn class_ref_name(&self, c: &ClassRef) -> R<String> {
        match c {
            ClassRef::Name(n) => {
                if n.is_special("self") || n.is_special("static") {
                    let f = self.frames.borrow();
                    let fr = f.last();
                    let s = if n.is_special("static") { fr.and_then(|f| f.static_class.clone().or_else(|| f.class.clone())) } else { fr.and_then(|f| f.class.clone()) };
                    return Ok(s.unwrap_or_default());
                }
                if n.is_special("parent") {
                    let cls = self.frames.borrow().last().and_then(|f| f.class.clone()).unwrap_or_default();
                    return Ok(self.classes.borrow().get(&lc(&cls)).and_then(|c| c.parent.clone()).unwrap_or_default());
                }
                Ok(self.resolve_class_name(n))
            }
            ClassRef::Expr(e) => {
                let v = self.eval(e)?;
                Ok(match v {
                    V::Obj(o) => o.class.borrow().clone(),
                    other => String::from_utf8_lossy(&to_bytes(&other)).into_owned(),
                })
            }
        }
    }

    fn class_constant(&self, class: &str, name: &[u8]) -> R<V> {
        let cls = self.classes.borrow().get(&lc(class)).cloned();
        if let Some(c) = cls {
            if let Some((_, e)) = c.consts.iter().find(|(n, _)| n == name) {
                return self.eval(e);
            }
            if let Some(p) = &c.parent {
                return self.class_constant(p, name);
            }
        }
        let nm = String::from_utf8_lossy(name).into_owned();
        let mut cur = php_dom::classes::find_class(class);
        while let Some(c) = cur {
            if let Some(k) = c.constants.iter().find(|k| k.name == nm) {
                return Ok(V::Int(k.value));
            }
            if c.kind == php_dom::classes::ClassKind::Enum {
                if let Some((case, _)) = c.enum_cases.iter().find(|(n, _)| *n == nm) {
                    return Ok(self.enum_case(c.name, case));
                }
            }
            cur = c.parent.and_then(php_dom::classes::find_class);
        }
        Err(self.throw_err("Error", &format!("Undefined constant {class}::{nm}")))
    }

    pub fn enum_case(&self, class: &'static str, case: &'static str) -> V {
        let key = format!("{class}::{case}");
        if let Some(o) = self.enum_cases.borrow().get(&key) {
            return V::Obj(o.clone());
        }
        let o = new_obj(class, ObjKind::Enum(class, case));
        o.set_prop(b"name", V::str(case));
        if let Some(c) = php_dom::classes::find_class(class) {
            if let Some((_, val)) = c.enum_cases.iter().find(|(n, _)| *n == case) {
                o.set_prop(b"value", V::str(val));
            }
        }
        self.enum_cases.borrow_mut().insert(key, o.clone());
        V::Obj(o)
    }

    fn read_dim(&self, b: &V, k: &V) -> R<V> {
        match b {
            V::Arr(a) => match a.get(&to_key(k)) {
                Some(v) => Ok(v.clone()),
                None => {
                    let ks = match to_key(k) {
                        Key::Int(i) => i.to_string(),
                        Key::Str(s) => format!("\"{}\"", String::from_utf8_lossy(&s)),
                    };
                    self.emit_raw("Warning", format!("Undefined array key {ks}").as_bytes());
                    Ok(V::Null)
                }
            },
            V::Str(s) => {
                let mut i = to_int(k);
                if i < 0 {
                    i += s.len() as i64;
                }
                if i >= 0 && (i as usize) < s.len() {
                    Ok(V::Str(vec![s[i as usize]]))
                } else {
                    self.emit_raw("Warning", format!("Uninitialized string offset {i}").as_bytes());
                    Ok(V::Str(Vec::new()))
                }
            }
            V::Obj(o) => {
                if let Some(d) = o.dom() {
                    let off = to_offset(k);
                    let r = dispatch::offset_get(&d, &off).map_err(|e| self.dom_err(e, "", &[]))?;
                    return Ok(self.from_dom_value(r));
                }
                Err(self.throw_err("Error", &format!("Cannot use object of type {} as array", o.class.borrow())))
            }
            V::Null => Ok(V::Null),
            _ => Ok(V::Null),
        }
    }

    pub fn read_prop(&self, o: &V, n: &[u8]) -> R<V> {
        let V::Obj(ob) = o else {
            self.emit_raw("Warning", format!("Attempt to read property \"{}\" on {}", String::from_utf8_lossy(n), o.type_name()).as_bytes());
            return Ok(V::Null);
        };
        if let Some(d) = ob.dom() {
            let ns = String::from_utf8_lossy(n).into_owned();
            let saved = self.cur_fn.replace(String::new());
            let r = dispatch::read_property(&d, &ns);
            *self.cur_fn.borrow_mut() = saved;
            match r {
                Ok(PropRead::Value(v)) => return Ok(self.from_dom_value(v)),
                Ok(PropRead::Uninit) => return Ok(V::Null),
                Ok(PropRead::NotHandled) => {}
                Err(e) => return Err(self.dom_err(e, "", &[])),
            }
        }
        if let ObjKind::Enum(..) = &*ob.kind.borrow() {
            return Ok(ob.prop(n).unwrap_or(V::Null));
        }
        match ob.prop(n) {
            Some(v) => Ok(v),
            None => {
                let cls = ob.class.borrow().clone();
                if let Some(_) = self.find_user_method(&cls, "__get") {
                    return self.call_method(o, "__get", vec![V::Str(n.to_vec())]);
                }
                self.emit_raw("Warning", format!("Undefined property: {}::${}", cls, String::from_utf8_lossy(n)).as_bytes());
                Ok(V::Null)
            }
        }
    }

    fn write_prop(&self, ob: &Rc<Obj>, n: &[u8], v: V) -> R<()> {
        if let Some(d) = ob.dom() {
            let ns = String::from_utf8_lossy(n).into_owned();
            if dispatch::is_handled_property(&d, &ns) {
                // Visibility check from the class table.
                let builtin = d.builtin_class();
                if let Some(p) = find_prop(builtin, &ns) {
                    let decl = declaring_class(builtin, &ns).unwrap_or(builtin);
                    if p.set_visibility.is_some() || p.readonly {
                        let in_scope = self.frames.borrow().last().and_then(|f| f.class.clone());
                        let scope = match in_scope {
                            Some(c) => format!("scope {c}"),
                            None => "global scope".into(),
                        };
                        return Err(self.throw_err("Error", &format!("Cannot modify private(set) property {decl}::${ns} from {scope}")));
                    }
                    let typed = self.coerce_prop(&v, p.ty.unwrap_or("mixed"), decl, &ns)?;
                    let dv = self.to_dom_value(&typed);
                    return dispatch::write_property(&d, &ns, &dv).map(|_| ()).map_err(|e| self.dom_err(e, "", &[]));
                }
            }
        }
        ob.set_prop(n, v);
        Ok(())
    }

    fn coerce_prop(&self, v: &V, ty: &str, class: &str, name: &str) -> R<V> {
        let nullable = ty.starts_with('?');
        let base = ty.trim_start_matches('?');
        if matches!(v, V::Null) {
            if nullable || base.contains("null") || base == "mixed" {
                return Ok(V::Null);
            }
            return Err(self.throw_err("TypeError", &format!("Cannot assign null to property {class}::${name} of type {ty}")));
        }
        if let V::Obj(o) = v {
            let cls = o.class.borrow().clone();
            if (base == "string" || base.contains("string")) && self.find_user_method(&cls, "__toString").is_some() {
                let s = self.call_method(v, "__toString", vec![])?;
                return Ok(V::Str(to_bytes(&s)));
            }
        }
        match base {
            "string" => match v {
                V::Arr(_) | V::Obj(_) => Err(self.throw_err("TypeError", &format!("Cannot assign {} to property {class}::${name} of type {ty}", v.type_name()))),
                other => Ok(V::Str(to_bytes(other))),
            },
            "bool" => match v {
                V::Arr(_) | V::Obj(_) => Err(self.throw_err("TypeError", &format!("Cannot assign {} to property {class}::${name} of type {ty}", v.type_name()))),
                other => Ok(V::Bool(other.truthy())),
            },
            _ => Ok(v.clone()),
        }
    }

    fn assign(&self, target: &Expr, v: V) -> R<()> {
        match &target.kind {
            ExprKind::Var(vn) => {
                let n = self.var_name(vn)?;
                self.set_var(&n, v);
                Ok(())
            }
            ExprKind::Prop { object, name, .. } => {
                let o = self.eval(object)?;
                let n = self.member_name(name)?;
                match &o {
                    V::Obj(ob) => self.write_prop(ob, &n, v),
                    _ => Err(self.throw_err("Error", &format!("Attempt to assign property \"{}\" on {}", String::from_utf8_lossy(&n), o.type_name()))),
                }
            }
            ExprKind::Dim { base, index } => {
                let k = match index {
                    Some(i) => Some(to_key(&self.eval(i)?)),
                    None => None,
                };
                let bv = self.eval_quiet(base)?;
                if let V::Obj(o) = &bv {
                    if o.dom().is_some() {
                        return Err(self.throw_err("Error", &format!("Cannot use object of type {} as array", o.class.borrow())));
                    }
                }
                let mut v = Some(v);
                self.write_lvalue(base, &mut |slot| {
                    if matches!(slot, V::Null) {
                        *slot = V::arr(Arr::default());
                    }
                    if let V::Arr(a) = slot {
                        let a = Rc::make_mut(a);
                        match &k {
                            Some(k) => a.set(k.clone(), v.take().unwrap_or(V::Null)),
                            None => a.push(v.take().unwrap_or(V::Null)),
                        }
                    }
                    Ok(())
                })
            }
            ExprKind::Array { items, .. } => self.destructure(items, &v),
            _ => Err(self.throw_err("Error", "Cannot assign to this expression")),
        }
    }

    fn write_lvalue(&self, e: &Expr, f: &mut dyn FnMut(&mut V) -> R<()>) -> R<()> {
        match &e.kind {
            ExprKind::Var(vn) => {
                let n = self.var_name(vn)?;
                let mut v = self.get_var(&n);
                f(&mut v)?;
                self.set_var(&n, v);
                Ok(())
            }
            ExprKind::Dim { base, index } => {
                let k = match index {
                    Some(i) => Some(to_key(&self.eval(i)?)),
                    None => None,
                };
                self.write_lvalue(base, &mut |bv| {
                    if matches!(bv, V::Null) {
                        *bv = V::arr(Arr::default());
                    }
                    if let V::Arr(a) = bv {
                        let a = Rc::make_mut(a);
                        let key = match &k {
                            Some(k) => k.clone(),
                            None => {
                                let kk = Key::Int(a.next);
                                a.push(V::Null);
                                kk
                            }
                        };
                        if a.get(&key).is_none() {
                            a.set(key.clone(), V::Null);
                        }
                        let slot = a.get_mut(&key).expect("inserted");
                        f(slot)?;
                    }
                    Ok(())
                })
            }
            ExprKind::Prop { object, name, .. } => {
                let o = self.eval(object)?;
                let n = self.member_name(name)?;
                if let V::Obj(ob) = &o {
                    let mut v = ob.prop(&n).unwrap_or(V::Null);
                    f(&mut v)?;
                    ob.set_prop(&n, v);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn binop(&self, op: BinaryOp, l: &V, r: &V) -> R<V> {
        Ok(match op {
            BinaryOp::Concat => {
                let mut a = self.to_str(l)?;
                a.extend_from_slice(&self.to_str(r)?);
                V::Str(a)
            }
            BinaryOp::Identical => V::Bool(strict_eq(l, r)),
            BinaryOp::NotIdentical => V::Bool(!strict_eq(l, r)),
            BinaryOp::Equal => V::Bool(loose_eq(l, r)),
            BinaryOp::NotEqual => V::Bool(!loose_eq(l, r)),
            BinaryOp::Less => V::Bool(compare(l, r) < 0),
            BinaryOp::LessEqual => V::Bool(compare(l, r) <= 0),
            BinaryOp::Greater => V::Bool(compare(l, r) > 0),
            BinaryOp::GreaterEqual => V::Bool(compare(l, r) >= 0),
            BinaryOp::Spaceship => V::Int(compare(l, r).into()),
            BinaryOp::BoolXor => V::Bool(l.truthy() ^ r.truthy()),
            BinaryOp::BitOr => V::Int(to_int(l) | to_int(r)),
            BinaryOp::BitAnd => V::Int(to_int(l) & to_int(r)),
            BinaryOp::BitXor => V::Int(to_int(l) ^ to_int(r)),
            BinaryOp::ShiftLeft => V::Int(to_int(l).wrapping_shl(to_int(r) as u32)),
            BinaryOp::ShiftRight => V::Int(to_int(l).wrapping_shr(to_int(r) as u32)),
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => {
                if let (BinaryOp::Add, V::Arr(a), V::Arr(b)) = (op, l, r) {
                    let mut out = (**a).clone();
                    for (k, v) in &b.items {
                        if out.get(k).is_none() {
                            out.set(k.clone(), v.clone());
                        }
                    }
                    return Ok(V::arr(out));
                }
                let (a, b) = (to_num(l), to_num(r));
                match (op, &a, &b) {
                    (BinaryOp::Mod, _, _) => {
                        let d = to_int(&b);
                        if d == 0 {
                            return Err(self.throw_err("DivisionByZeroError", "Modulo by zero"));
                        }
                        V::Int(to_int(&a) % d)
                    }
                    (BinaryOp::Div, _, _) => {
                        let d = to_f(&b);
                        if d == 0.0 {
                            return Err(self.throw_err("DivisionByZeroError", "Division by zero"));
                        }
                        let q = to_f(&a) / d;
                        if let (V::Int(x), V::Int(y)) = (&a, &b) {
                            if x % y == 0 {
                                return Ok(V::Int(x / y));
                            }
                        }
                        V::Float(q)
                    }
                    (_, V::Int(x), V::Int(y)) => match op {
                        BinaryOp::Add => V::Int(x.wrapping_add(*y)),
                        BinaryOp::Sub => V::Int(x.wrapping_sub(*y)),
                        BinaryOp::Mul => V::Int(x.wrapping_mul(*y)),
                        _ => {
                            if *y >= 0 {
                                V::Int(x.wrapping_pow(*y as u32))
                            } else {
                                V::Float((*x as f64).powf(*y as f64))
                            }
                        }
                    },
                    _ => {
                        let (x, y) = (to_f(&a), to_f(&b));
                        V::Float(match op {
                            BinaryOp::Add => x + y,
                            BinaryOp::Sub => x - y,
                            BinaryOp::Mul => x * y,
                            _ => x.powf(y),
                        })
                    }
                }
            }
            BinaryOp::BoolAnd | BinaryOp::BoolOr | BinaryOp::Coalesce => V::Null,
        })
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn sort_array(&self, kind: &str, arr: &Arr, cb: Option<&V>) -> R<Arr> {
        let mut items: Vec<(Key, V)> = arr.items.clone();
        let mut err = None;
        let key_v = |k: &Key| match k {
            Key::Int(i) => V::Int(*i),
            Key::Str(s) => V::Str(s.clone()),
        };
        // Merge sort via sort_by with a fallible comparator.
        items.sort_by(|x, y| {
            if err.is_some() {
                return std::cmp::Ordering::Equal;
            }
            let r = match kind {
                "usort" | "uasort" => cb.map(|f| self.call_value(f, vec![x.1.clone(), y.1.clone()]).map(|v| to_int(&v))),
                "uksort" => cb.map(|f| self.call_value(f, vec![key_v(&x.0), key_v(&y.0)]).map(|v| to_int(&v))),
                "ksort" => Some(Ok(i64::from(compare(&key_v(&x.0), &key_v(&y.0))))),
                "krsort" => Some(Ok(-i64::from(compare(&key_v(&x.0), &key_v(&y.0))))),
                "rsort" | "arsort" => Some(Ok(-i64::from(compare(&x.1, &y.1)))),
                _ => Some(Ok(i64::from(compare(&x.1, &y.1)))),
            };
            match r {
                Some(Ok(v)) => v.cmp(&0),
                Some(Err(e)) => {
                    err = Some(e);
                    std::cmp::Ordering::Equal
                }
                None => std::cmp::Ordering::Equal,
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
        let mut out = Arr::default();
        let keep_keys = matches!(kind, "uasort" | "uksort" | "ksort" | "krsort" | "asort" | "arsort");
        for (k, v) in items {
            if keep_keys {
                out.set(k, v);
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn eval_args(&self, args: &ArgList) -> R<(Vec<V>, Vec<(Vec<u8>, V)>)> {
        let mut pos = Vec::new();
        let mut named = Vec::new();
        for a in &args.args {
            match &a.kind {
                ArgKind::Positional(e) => pos.push(self.eval(e)?),
                ArgKind::Unpack(e) => match self.eval(e)? {
                    V::Arr(arr) => {
                        for (k, v) in &arr.items {
                            match k {
                                Key::Int(_) => pos.push(v.clone()),
                                Key::Str(s) => named.push((s.clone(), v.clone())),
                            }
                        }
                    }
                    V::Obj(o) => {
                        let it = self.get_iterator(&o)?;
                        loop {
                            let (valid, cur, _) = self.iter_state(&it)?;
                            if !valid {
                                break;
                            }
                            pos.push(cur);
                            self.iter_next(&it)?;
                        }
                    }
                    _ => {}
                },
                ArgKind::Named { name, value } => named.push((name.name.clone(), self.eval(value)?)),
                _ => {}
            }
        }
        Ok((pos, named))
    }

    fn eval_call(&self, callee: &Callee, args: &ArgList, line: u32) -> R<V> {
        if args.is_first_class_callable() {
            return match callee {
                Callee::Name(n) => Ok(V::Obj(new_obj("Closure", ObjKind::Closure(Rc::new(Callable::Builtin(String::from_utf8_lossy(&n.name).into_owned())))))),
                Callee::Expr(e) => self.eval(e),
            };
        }
        match callee {
            Callee::Name(n) => {
                let raw = String::from_utf8_lossy(&n.name).into_owned();
                let name = raw.trim_start_matches('\\').rsplit('\\').next().unwrap_or(&raw).to_string();
                // compact()/extract() etc. need raw args; handle isset-like builtins first.
                let (a, named) = self.eval_args(args)?;
                self.line.set(line);
                let uf = self.functions.borrow().get(&name.to_ascii_lowercase().into_bytes()).cloned();
                if let Some(f) = uf {
                    return self.call_user(&f, a, named, None, None, None, &[]);
                }
                let lname = name.to_ascii_lowercase();
                if matches!(lname.as_str(), "usort" | "uasort" | "uksort" | "sort" | "rsort" | "ksort" | "krsort" | "asort" | "arsort") {
                    if let (Some(V::Arr(arr)), Some(first)) = (a.first(), args.args.first()) {
                        if let ArgKind::Positional(target) = &first.kind {
                            let sorted = self.sort_array(&lname, arr, a.get(1))?;
                            self.assign(target, V::arr(sorted))?;
                            return Ok(V::Bool(true));
                        }
                    }
                }
                self.builtin(&name, a, named)
            }
            Callee::Expr(e) => {
                let f = self.eval(e)?;
                let (a, _) = self.eval_args(args)?;
                self.line.set(line);
                self.call_value(&f, a)
            }
        }
    }

    pub fn call_value(&self, f: &V, args: Vec<V>) -> R<V> {
        match f {
            V::Obj(o) => {
                let k = &*o.kind.borrow();
                if let ObjKind::Closure(c) = k {
                    let c = c.clone();
                    return self.call_callable(&c, args);
                }
                if self.find_user_method(&o.class.borrow(), "__invoke").is_some() {
                    return self.call_method(f, "__invoke", args);
                }
                Err(self.throw_err("Error", "Object of type is not callable"))
            }
            V::Str(s) => {
                let name = String::from_utf8_lossy(s).into_owned();
                if let Some((c, m)) = name.split_once("::") {
                    return self.call_static_by_name(c, m, args);
                }
                let uf = self.functions.borrow().get(&name.to_ascii_lowercase().into_bytes()).cloned();
                if let Some(uf) = uf {
                    return self.call_user(&uf, args, vec![], None, None, None, &[]);
                }
                self.builtin(&name, args, vec![])
            }
            V::Arr(a) => {
                let target = a.get(&Key::Int(0)).cloned().unwrap_or(V::Null);
                let m = to_bytes(&a.get(&Key::Int(1)).cloned().unwrap_or(V::Null));
                let m = String::from_utf8_lossy(&m).into_owned();
                match target {
                    V::Obj(_) => self.call_method(&target, &m, args),
                    other => self.call_static_by_name(&String::from_utf8_lossy(&to_bytes(&other)), &m, args),
                }
            }
            _ => Err(self.throw_err("Error", "Value not callable")),
        }
    }

    fn call_callable(&self, c: &Callable, args: Vec<V>) -> R<V> {
        match c {
            Callable::Func(f) => self.call_user(f, args, vec![], None, None, None, &[]),
            Callable::Closure { func, bound, this, scope, arrow } => {
                if let Some(ex) = arrow {
                    // Arrow function: bind params on top of captured vars.
                    let mut fr = Frame { func: "{closure}".into(), this: this.clone(), class: scope.clone(), ..Default::default() };
                    for (k, v) in bound {
                        fr.vars.insert(k.clone(), v.clone());
                    }
                    self.frames.borrow_mut().push(fr);
                    let r = (|| {
                        self.bind_params(&func.params, args, vec![], "{closure}")?;
                        self.eval(ex)
                    })();
                    self.frames.borrow_mut().pop();
                    return r;
                }
                self.call_user(func, args, vec![], this.clone(), scope.clone(), None, bound)
            }
            Callable::Builtin(n) => self.builtin(n, args, vec![]),
            Callable::Method(o, m) => self.call_method(o, &String::from_utf8_lossy(m), args),
            Callable::StaticMethod(c, m) => self.call_static_by_name(c, &String::from_utf8_lossy(m), args),
        }
    }

    fn bind_params(&self, params: &[Param], mut args: Vec<V>, named: Vec<(Vec<u8>, V)>, fname: &str) -> R<()> {
        let nargs = args.len();
        for (i, p) in params.iter().enumerate() {
            if p.variadic {
                let mut a = Arr::default();
                for v in args.drain(i.min(args.len())..) {
                    a.push(v);
                }
                for (k, v) in &named {
                    a.set(Key::Str(k.clone()), v.clone());
                }
                self.set_var(&p.name.name, V::arr(a));
                break;
            }
            let v = if i < nargs {
                args[i].clone()
            } else if let Some((_, v)) = named.iter().find(|(k, _)| *k == p.name.name) {
                v.clone()
            } else if let Some(d) = &p.default {
                self.eval(d)?
            } else {
                return Err(self.throw_err(
                    "ArgumentCountError",
                    &format!("Too few arguments to function {fname}(), {nargs} passed and at least {} expected", params.iter().filter(|p| p.default.is_none() && !p.variadic).count()),
                ));
            };
            self.set_var(&p.name.name, v);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn call_user(&self, f: &UserFunc, args: Vec<V>, named: Vec<(Vec<u8>, V)>, this: Option<V>, class: Option<String>, static_class: Option<String>, bound: &[(Vec<u8>, V)]) -> R<V> {
        if self.frames.borrow().len() > 200 {
            return Err(self.throw_err("Error", "Maximum function nesting level reached"));
        }
        let fname = match &class {
            Some(c) => format!("{c}::{}", String::from_utf8_lossy(&f.name)),
            None => String::from_utf8_lossy(&f.name).into_owned(),
        };
        let mut fr = Frame { func: String::from_utf8_lossy(&f.name).into_owned(), this: this.clone(), class: class.clone(), static_class, ..Default::default() };
        for (k, v) in bound {
            fr.vars.insert(k.clone(), v.clone());
        }
        self.frames.borrow_mut().push(fr);
        let saved_fn = self.cur_fn.replace(String::new());
        let saved_line = self.line.get();
        let r = (|| {
            self.bind_params(&f.params, args, named, &fname)?;
            // Promoted constructor properties.
            if f.name.eq_ignore_ascii_case(b"__construct") {
                if let Some(V::Obj(o)) = &this {
                    for p in &f.params {
                        if p.modifiers.0 != 0 {
                            o.set_prop(&p.name.name, self.get_var(&p.name.name));
                        }
                    }
                }
            }
            self.exec_block(&f.body)
        })();
        self.frames.borrow_mut().pop();
        *self.cur_fn.borrow_mut() = saved_fn;
        self.line.set(saved_line);
        match r {
            Ok(()) => Ok(V::Null),
            Err(Flow::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    fn call_method_named(&self, o: &V, name: &str, args: Vec<V>, named: Vec<(Vec<u8>, V)>) -> R<V> {
        if named.is_empty() {
            return self.call_method(o, name, args);
        }
        // Named args for DOM methods: order by the class table.
        if let V::Obj(ob) = o {
            if let Some(d) = ob.dom() {
                if let Some((_, m)) = php_dom::classes::find_method(d.builtin_class(), name) {
                    let mut a = args;
                    for (i, p) in m.params.iter().enumerate().skip(a.len()) {
                        if let Some((_, v)) = named.iter().find(|(k, _)| k == p.name.as_bytes()) {
                            while a.len() < i {
                                let d = m.params[a.len()].default;
                                a.push(self.default_value(d));
                            }
                            a.push(v.clone());
                        }
                    }
                    return self.call_method(o, name, a);
                }
            }
            let cls = ob.class.borrow().clone();
            if let Some((c, f, _)) = self.find_user_method(&cls, name) {
                return self.call_user(&f, args, named, Some(o.clone()), Some(c), Some(cls), &[]);
            }
        }
        self.call_method(o, name, args)
    }

    fn default_value(&self, d: Option<&str>) -> V {
        match d {
            Some("null") | None => V::Null,
            Some("true") => V::Bool(true),
            Some("false") => V::Bool(false),
            Some(s) if s.starts_with('"') => V::Str(s.trim_matches('"').as_bytes().to_vec()),
            Some(s) => s.parse::<i64>().map_or(V::Null, V::Int),
        }
    }

    pub fn call_method(&self, o: &V, name: &str, args: Vec<V>) -> R<V> {
        let V::Obj(ob) = o else {
            return Err(self.throw_err("Error", &format!("Call to a member function {name}() on {}", o.type_name())));
        };
        let cls = ob.class.borrow().clone();
        if let Some((c, f, _)) = self.find_user_method(&cls, name) {
            return self.call_user(&f, args, vec![], Some(o.clone()), Some(c), Some(cls), &[]);
        }
        if let Some(d) = ob.dom() {
            return self.call_dom(&d, ob, name, args);
        }
        let lname = lc(name);
        {
            let k = &*ob.kind.borrow();
            match k {
                ObjKind::Closure(c) => {
                    let c = c.clone();
                    match lname.as_str() {
                        "__invoke" | "call" => return self.call_callable(&c, args),
                        "bindto" | "bind" => return Ok(o.clone()),
                        _ => {}
                    }
                }
                ObjKind::Iter(h) => {
                    let h = h.clone();
                    let mut i = h.0.borrow_mut();
                    return match lname.as_str() {
                        "current" => Ok(from_dom(i.current())),
                        "key" => Ok(from_dom(i.key())),
                        "next" => {
                            i.next().map_err(|e| self.dom_err(e, "", &[]))?;
                            Ok(V::Null)
                        }
                        "valid" => Ok(V::Bool(i.valid())),
                        "rewind" => Ok(V::Null),
                        _ => Err(self.throw_err("Error", &format!("Call to undefined method InternalIterator::{name}()"))),
                    };
                }
                ObjKind::Enum(_, case) => {
                    let _ = case;
                }
                _ => {}
            }
        }
        if self.is_subclass(&cls, "Throwable") {
            return Ok(match lname.as_str() {
                "getmessage" => ob.prop(b"message").unwrap_or(V::str("")),
                "getcode" => ob.prop(b"code").unwrap_or(V::Int(0)),
                "getline" => ob.prop(b"line").unwrap_or(V::Int(0)),
                "getfile" => ob.prop(b"file").unwrap_or(V::str("")),
                "getprevious" => ob.prop(b"previous").unwrap_or(V::Null),
                "gettraceasstring" => V::str("#0 {main}"),
                "gettrace" => V::arr(Arr::default()),
                "__tostring" => {
                    let msg = ob.prop(b"message").map(|m| to_bytes(&m)).unwrap_or_default();
                    V::Str(format!("{cls}: {} in {}:{}\nStack trace:\n#0 {{main}}", String::from_utf8_lossy(&msg), self.file, to_int(&ob.prop(b"line").unwrap_or(V::Int(0)))).into_bytes())
                }
                _ => return Err(self.throw_err("Error", &format!("Call to undefined method {cls}::{name}()"))),
            });
        }
        Err(self.throw_err("Error", &format!("Call to undefined method {cls}::{name}()")))
    }

    fn call_dom(&self, d: &DomObject, ob: &Rc<Obj>, name: &str, args: Vec<V>) -> R<V> {
        let builtin = d.builtin_class();
        let (decl, params): (String, Vec<&'static str>) = match php_dom::classes::find_method(builtin, name) {
            Some((c, m)) => (format!("{}::{}", c.name, m.name), m.params.iter().map(|p| p.name).collect()),
            None => (format!("{builtin}::{name}"), Vec::new()),
        };
        if lc(name) == "__construct" {
            if let Some(err) = self.arity_error(builtin, name, args.len(), &decl) {
                return Err(err);
            }
            let dargs: Vec<DomValue> = args.iter().map(|a| self.to_dom_value(a)).collect();
            let saved = self.cur_fn.replace(decl.clone());
            let r = dispatch::construct(d, &dargs, self);
            *self.cur_fn.borrow_mut() = saved;
            return match r {
                Ok(Some(newobj)) => {
                    *ob.kind.borrow_mut() = ObjKind::Dom(newobj);
                    Ok(V::Null)
                }
                Ok(None) => Ok(V::Null),
                Err(e) => Err(self.dom_err(e, &decl, &params)),
            };
        }
        if let Some(err) = self.arity_error(builtin, name, args.len(), &decl) {
            return Err(err);
        }
        let dargs: Vec<DomValue> = args.iter().map(|a| self.to_dom_value(a)).collect();
        let saved = self.cur_fn.replace(decl.clone());
        let r = dispatch::call(d, name, &dargs, self);
        *self.cur_fn.borrow_mut() = saved;
        match r {
            Ok(v) => Ok(self.from_dom_value(v)),
            Err(DomError::CouldntFetch) => Err(self.throw_err("Error", &format!("Couldn't fetch {}", ob.class.borrow()))),
            Err(DomError::Silent) => Ok(V::Bool(false)),
            Err(e) => {
                let f = self.dom_err(e, &decl, &params);
                if let Flow::Throw(V::Obj(ex)) = &f {
                    if ex.prop(b"__trace").is_none() {
                        let argstr: Vec<String> = args.iter().map(trace_arg).collect();
                        let callee = decl.replace("::", "->");
                        ex.set_prop(b"__trace", V::Str(format!("#0 {}({}): {}({})\n", self.file, self.line.get(), callee, argstr.join(", ")).into_bytes()));
                    }
                }
                Err(f)
            }
        }
    }

    fn arity_error(&self, class: &str, method: &str, n: usize, decl: &str) -> Option<Flow> {
        let (_, m) = php_dom::classes::find_method(class, method)?;
        let min = m.required_args();
        let variadic = m.params.iter().any(|p| p.variadic);
        let max = m.params.len();
        let (kind, cnt) = if n < min {
            (if min == max && !variadic { "exactly" } else { "at least" }, min)
        } else if !variadic && n > max {
            (if min == max { "exactly" } else { "at most" }, max)
        } else {
            return None;
        };
        let plural = if cnt == 1 { "argument" } else { "arguments" };
        Some(self.throw_err("ArgumentCountError", &format!("{decl}() expects {kind} {cnt} {plural}, {n} given")))
    }

    fn static_call(&self, class: &ClassRef, name: &str, args: Vec<V>, named: Vec<(Vec<u8>, V)>) -> R<V> {
        if let ClassRef::Name(n) = class {
            if n.is_special("parent") {
                let (cur_class, this) = {
                    let f = self.frames.borrow();
                    let fr = f.last();
                    (fr.and_then(|f| f.class.clone()).unwrap_or_default(), fr.and_then(|f| f.this.clone()))
                };
                let parent = self.classes.borrow().get(&lc(&cur_class)).and_then(|c| c.parent.clone()).unwrap_or_default();
                if let Some((c, f, _)) = self.find_user_method(&parent, name) {
                    let sc = this.as_ref().map(|t| t.type_name());
                    return self.call_user(&f, args, named, this, Some(c), sc, &[]);
                }
                if let Some(V::Obj(o)) = &this {
                    if let Some(d) = o.dom() {
                        return self.call_dom(&d, o, name, args);
                    }
                    if self.is_subclass(&parent, "Exception") || self.is_subclass(&parent, "Error") {
                        if lc(name) == "__construct" {
                            if let Some(m) = args.first() {
                                o.set_prop(b"message", m.clone());
                            }
                            if let Some(c) = args.get(1) {
                                o.set_prop(b"code", c.clone());
                            }
                            if let Some(p) = args.get(2) {
                                o.set_prop(b"previous", p.clone());
                            }
                            return Ok(V::Null);
                        }
                        return self.call_method(&V::Obj(o.clone()), name, args);
                    }
                }
                return Ok(V::Null);
            }
        }
        let cname = self.class_ref_name(class)?;
        if let Some((c, f, is_static)) = self.find_user_method(&cname, name) {
            let this = if is_static { None } else { self.frames.borrow().last().and_then(|f| f.this.clone()) };
            return self.call_user(&f, args, named, this, Some(c), Some(cname), &[]);
        }
        let mut args = args;
        if !named.is_empty() {
            let canon = self.canonical_class(&cname);
            let dom_class = self.dom_builtin(&canon).map_or(canon.clone(), str::to_string);
            if let Some((_, m)) = php_dom::classes::find_method(&dom_class, name) {
                for (i, p) in m.params.iter().enumerate().skip(args.len()) {
                    if let Some((_, v)) = named.iter().find(|(k, _)| k == p.name.as_bytes()) {
                        while args.len() < i {
                            let d = m.params[args.len()].default;
                            args.push(self.default_value(d));
                        }
                        args.push(v.clone());
                    }
                }
            }
        }
        self.call_static_by_name(&cname, name, args)
    }

    fn call_static_by_name(&self, class: &str, name: &str, args: Vec<V>) -> R<V> {
        let class = self.canonical_class(class);
        if let Some((c, f, _)) = self.find_user_method(&class, name) {
            return self.call_user(&f, args, vec![], None, Some(c), Some(class.clone()), &[]);
        }
        let mut dom_class = class.clone();
        if let Some(b) = self.dom_builtin(&class) {
            dom_class = b.to_string();
        }
        if let Some(c) = php_dom::classes::find_class(&dom_class) {
            if c.kind == php_dom::classes::ClassKind::Enum {
                match lc(name).as_str() {
                    "cases" => {
                        let mut a = Arr::default();
                        for (case, _) in c.enum_cases {
                            a.push(self.enum_case(c.name, case));
                        }
                        return Ok(V::arr(a));
                    }
                    "from" | "tryfrom" => {
                        let v = to_bytes(args.first().unwrap_or(&V::Null));
                        for (case, val) in c.enum_cases {
                            if val.as_bytes() == v.as_slice() {
                                return Ok(self.enum_case(c.name, case));
                            }
                        }
                        if lc(name) == "tryfrom" {
                            return Ok(V::Null);
                        }
                        return Err(self.throw_err("ValueError", &format!("\"{}\" is not a valid backing value for enum {}", String::from_utf8_lossy(&v), c.name)));
                    }
                    _ => {}
                }
            }
        }
        let (decl, params): (String, Vec<&'static str>) = match php_dom::classes::find_method(&dom_class, name) {
            Some((c, m)) => {
                if !m.is_static {
                    return Err(self.throw_err("Error", &format!("Non-static method {}::{}() cannot be called statically", c.name, m.name)));
                }
                (format!("{}::{}", c.name, m.name), m.params.iter().map(|p| p.name).collect())
            }
            None => (format!("{dom_class}::{name}"), Vec::new()),
        };
        let dargs: Vec<DomValue> = args.iter().map(|a| self.to_dom_value(a)).collect();
        let saved = self.cur_fn.replace(decl.clone());
        let r = dispatch::call_static(&dom_class, name, &dargs, self);
        *self.cur_fn.borrow_mut() = saved;
        match r {
            Ok(v) => Ok(self.from_dom_value(v)),
            Err(DomError::Silent) => Ok(V::Bool(false)),
            Err(e) => {
                let f = self.dom_err(e, &decl, &params);
                if let Flow::Throw(V::Obj(ex)) = &f {
                    if ex.prop(b"__trace").is_none() {
                        let argstr: Vec<String> = args.iter().map(trace_arg).collect();
                        ex.set_prop(b"__trace", V::Str(format!("#0 {}({}): {}({})\n", self.file, self.line.get(), decl, argstr.join(", ")).into_bytes()));
                    }
                }
                Err(f)
            }
        }
    }

    fn instantiate(&self, cname: &str, args: Vec<V>, named: Vec<(Vec<u8>, V)>) -> R<V> {
        let cname = self.canonical_class(cname);
        if !self.class_exists(&cname) {
            return Err(self.throw_err("Error", &format!("Class \"{cname}\" not found")));
        }
        let builtin = self.dom_builtin(&cname);
        let obj = match builtin {
            Some(b) if b == "DOMXPath" || b == "Dom\\XPath" => {
                // Create with a placeholder; the constructor replaces it.
                new_obj(&cname, ObjKind::Plain)
            }
            Some(b) if php_dom::classes::is_subclass(b, "DOMNode") || php_dom::classes::is_subclass(b, "Dom\\Node") || b == "DOMNameSpaceNode" => {
                let n = Node::new_unbound(cname.clone(), b);
                let o = new_obj(&cname, ObjKind::Dom(DomObject::Node(n.clone())));
                *n.host_data() = Some(Box::new(Rc::downgrade(&o)));
                o
            }
            Some(b) if dispatch::instantiate(b).is_some() => new_obj(&cname, ObjKind::Dom(dispatch::instantiate(b).expect("checked"))),
            _ => new_obj(&cname, ObjKind::Plain),
        };
        // default props
        self.init_props(&obj, &cname)?;
        let v = V::Obj(obj.clone());
        if let Some((c, f, _)) = self.find_user_method(&cname, "__construct") {
            if matches!(builtin, Some("DOMXPath" | "Dom\\XPath")) {
                *obj.kind.borrow_mut() = ObjKind::Dom(DomObject::XPath(placeholder_xpath()));
            }
            self.call_user(&f, args, named, Some(v.clone()), Some(c), Some(cname.clone()), &[])?;
            return Ok(v);
        }
        match builtin {
            Some(b) if b == "DOMXPath" || b == "Dom\\XPath" => {
                let doc = match args.first() {
                    Some(V::Obj(o)) => o.dom(),
                    _ => None,
                };
                let Some(DomObject::Node(docn)) = doc else {
                    let given = args.first().map_or("null".into(), V::type_name);
                    let want = if b == "DOMXPath" { "DOMDocument" } else { "Dom\\Document" };
                    return Err(self.throw_err("TypeError", &format!("{b}::__construct(): Argument #1 ($document) must be of type {want}, {given} given")));
                };
                let reg = args.get(1).map_or(true, V::truthy);
                let x = dispatch::new_xpath(&docn, reg).map_err(|e| self.dom_err(e, &format!("{b}::__construct"), &["document", "registerNodeNS"]))?;
                *obj.kind.borrow_mut() = ObjKind::Dom(DomObject::XPath(x));
            }
            Some(b) => {
                if let Some(d) = obj.dom() {
                    if b.starts_with("Dom\\") && php_dom::classes::is_subclass(b, "Dom\\Node") {
                        return Err(self.throw_err("Error", &format!("Cannot directly construct {cname}, use document methods instead")));
                    }
                    let mut a = args;
                    if !named.is_empty() {
                        if let Some((_, m)) = php_dom::classes::find_method(b, "__construct") {
                            for (i, p) in m.params.iter().enumerate().skip(a.len()) {
                                if let Some((_, v)) = named.iter().find(|(k, _)| k == p.name.as_bytes()) {
                                    while a.len() < i {
                                        let d = m.params[a.len()].default;
                                        a.push(self.default_value(d));
                                    }
                                    a.push(v.clone());
                                }
                            }
                        }
                    }
                    self.call_dom(&d, &obj, "__construct", a)?;
                }
            }
            None => {
                if self.is_subclass(&cname, "Throwable") {
                    obj.set_prop(b"message", args.first().cloned().unwrap_or(V::str("")));
                    obj.set_prop(b"code", args.get(1).cloned().unwrap_or(V::Int(0)));
                    obj.set_prop(b"file", V::str(&self.file));
                    obj.set_prop(b"line", V::Int(i64::from(self.line.get())));
                    if let Some(p) = args.get(2) {
                        obj.set_prop(b"previous", p.clone());
                    }
                }
            }
        }
        Ok(v)
    }

    fn init_props(&self, obj: &Rc<Obj>, cname: &str) -> R<()> {
        let mut chain = Vec::new();
        let mut cur = Some(cname.to_string());
        while let Some(c) = cur {
            let cls = self.classes.borrow().get(&lc(&c)).cloned();
            match cls {
                Some(cl) => {
                    cur = cl.parent.clone();
                    chain.push(cl);
                }
                None => break,
            }
        }
        for cl in chain.iter().rev() {
            for (n, d, is_static) in &cl.props {
                if *is_static {
                    continue;
                }
                let v = match d {
                    Some(e) => self.eval(e)?,
                    None => V::Null,
                };
                obj.set_prop(n, v);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // DOM value conversion
    // ------------------------------------------------------------------

    pub fn node_obj(&self, n: Node) -> V {
        {
            let slot = n.host_data();
            if let Some(b) = slot.as_ref() {
                if let Some(w) = b.downcast_ref::<Weak<Obj>>() {
                    if let Some(o) = w.upgrade() {
                        return V::Obj(o);
                    }
                }
            }
        }
        let class = n.class_name();
        let o = new_obj(&class, ObjKind::Dom(DomObject::Node(n.clone())));
        *n.host_data() = Some(Box::new(Rc::downgrade(&o)));
        let _ = self.init_props(&o, &class);
        V::Obj(o)
    }

    pub fn from_dom_value(&self, v: DomValue) -> V {
        match v {
            DomValue::Null => V::Null,
            DomValue::Bool(b) => V::Bool(b),
            DomValue::Int(i) => V::Int(i),
            DomValue::Float(f) => V::Float(f),
            DomValue::Str(s) => V::Str(s),
            DomValue::Node(n) => self.node_obj(n),
            DomValue::NodeList(l) => {
                let keep: Vec<V> = l
                    .fixed_nodes()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|n| self.from_dom_value(DomValue::Node(n)))
                    .collect();
                let id = l.identity();
                if let Some(existing) = self.list_objs.borrow().get(&id).and_then(std::rc::Weak::upgrade) {
                    return V::Obj(existing);
                }
                let o = new_obj(l.class_name(), ObjKind::Dom(DomObject::NodeList(l)));
                *o.keep.borrow_mut() = keep;
                self.list_objs.borrow_mut().insert(id, Rc::downgrade(&o));
                V::Obj(o)
            }
            DomValue::NamedNodeMap(m) => V::Obj(new_obj(m.class_name(), ObjKind::Dom(DomObject::NamedNodeMap(m)))),
            DomValue::Implementation(i) => V::Obj(new_obj(i.class_name(), ObjKind::Dom(DomObject::Implementation(i)))),
            DomValue::TokenList(t) => V::Obj(new_obj("Dom\\TokenList", ObjKind::Dom(DomObject::TokenList(t)))),
            DomValue::XPath(x) => V::Obj(new_obj(x.class_name(), ObjKind::Dom(DomObject::XPath(x)))),
            DomValue::Iterator(i) => V::Obj(new_obj("InternalIterator", ObjKind::Iter(i))),
            DomValue::NamespaceInfo(ni) => {
                let o = new_obj("Dom\\NamespaceInfo", ObjKind::Plain);
                o.set_prop(b"prefix", ni.prefix.map_or(V::Null, V::Str));
                o.set_prop(b"namespaceURI", ni.namespace_uri.map_or(V::Null, V::Str));
                o.set_prop(b"element", self.node_obj(ni.element));
                V::Obj(o)
            }
            DomValue::Array(items) => {
                let mut a = Arr::default();
                for (k, v) in items {
                    let key = match k {
                        ArrayKey::Int(i) => Key::Int(i),
                        ArrayKey::Str(s) => Key::Str(s),
                    };
                    a.set(key, self.from_dom_value(v));
                }
                V::arr(a)
            }
            DomValue::Enum(c, case) => self.enum_case(c, case),
            DomValue::Callable(_) | DomValue::Other(_) => V::Null,
        }
    }

    pub fn to_dom_value(&self, v: &V) -> DomValue {
        match v {
            V::Null => DomValue::Null,
            V::Bool(b) => DomValue::Bool(*b),
            V::Int(i) => DomValue::Int(*i),
            V::Float(f) => DomValue::Float(*f),
            V::Str(s) => DomValue::Str(s.clone()),
            V::Arr(a) => DomValue::Array(
                a.items
                    .iter()
                    .map(|(k, v)| {
                        (
                            match k {
                                Key::Int(i) => ArrayKey::Int(*i),
                                Key::Str(s) => ArrayKey::Str(s.clone()),
                            },
                            self.to_dom_value(v),
                        )
                    })
                    .collect(),
            ),
            V::Obj(o) => {
                if let Some(d) = o.dom() {
                    return match d {
                        DomObject::Node(n) => DomValue::Node(n),
                        DomObject::NodeList(l) => DomValue::NodeList(l),
                        DomObject::NamedNodeMap(m) => DomValue::NamedNodeMap(m),
                        DomObject::Implementation(i) => DomValue::Implementation(i),
                        DomObject::XPath(x) => DomValue::XPath(x),
                        DomObject::TokenList(t) => DomValue::TokenList(t),
                    };
                }
                match &*o.kind.borrow() {
                    ObjKind::Closure(_) => {
                        let hc: HostCallable = Rc::new(HostFn(v.clone()));
                        DomValue::Callable(hc)
                    }
                    ObjKind::Enum(c, case) => DomValue::Enum(c, case),
                    _ => DomValue::Other(o.class.borrow().clone()),
                }
            }
        }
    }
}

struct HostFn(V);

fn placeholder_xpath() -> php_dom::XPath {
    let d = php_dom::document::xml_document_create_empty(b"1.0", b"UTF-8").expect("doc");
    php_dom::XPath::new(&d, true).expect("xpath")
}

fn trace_arg(v: &V) -> String {
    match v {
        V::Null => "NULL".into(),
        V::Bool(b) => if *b { "true".into() } else { "false".into() },
        V::Int(i) => i.to_string(),
        V::Float(f) => super::output::float_repr(*f),
        V::Str(s) => {
            let esc = |b: &[u8]| {
                let mut o = String::new();
                for &c in b {
                    match c {
                        b'\n' => o.push_str("\\n"),
                        b'\r' => o.push_str("\\r"),
                        b'\t' => o.push_str("\\t"),
                        0x0c => o.push_str("\\f"),
                        0x0b => o.push_str("\\v"),
                        b'\\' => o.push_str("\\\\"),
                        0x1b => o.push_str("\\e"),
                        c if !(32..=126).contains(&c) => o.push_str(&format!("\\x{c:02X}")),
                        c => o.push(char::from(c)),
                    }
                }
                o
            };
            if s.len() > 15 {
                format!("'{}...'", esc(&s[..15]))
            } else {
                format!("'{}'", esc(s))
            }
        }
        V::Arr(_) => "Array".into(),
        V::Obj(o) => {
            if let ObjKind::Enum(c, case) = &*o.kind.borrow() {
                return format!("{c}::{case}");
            }
            format!("Object({})", o.class.borrow())
        }
    }
}

fn find_prop(class: &str, name: &str) -> Option<&'static php_dom::classes::PropInfo> {
    let mut cur = php_dom::classes::find_class(class);
    while let Some(c) = cur {
        if let Some(p) = c.properties.iter().find(|p| p.name == name) {
            return Some(p);
        }
        cur = c.parent.and_then(php_dom::classes::find_class);
    }
    None
}

fn declaring_class(class: &str, name: &str) -> Option<&'static str> {
    let mut cur = php_dom::classes::find_class(class);
    let mut last = None;
    while let Some(c) = cur {
        if c.properties.iter().any(|p| p.name == name) {
            last = Some(c.name);
        }
        cur = c.parent.and_then(php_dom::classes::find_class);
    }
    last
}

pub fn from_dom(v: DomValue) -> V {
    match cur() {
        Some(i) => i.from_dom_value(v),
        None => V::Null,
    }
}

pub fn from_dom_plain(v: DomValue) -> V {
    match v {
        DomValue::Null => V::Null,
        DomValue::Bool(b) => V::Bool(b),
        DomValue::Int(i) => V::Int(i),
        DomValue::Float(f) => V::Float(f),
        DomValue::Str(s) => V::Str(s),
        other => from_dom(other),
    }
}

impl DomHost for Interp {
    fn is_subclass_of(&self, class: &str, base: &str) -> bool {
        self.class_exists(class) && self.is_subclass(class, base)
    }

    fn is_abstract(&self, class: &str) -> bool {
        self.classes.borrow().get(&lc(class)).is_some_and(|c| c.is_abstract)
            || php_dom::classes::find_class(class).is_some_and(|c| c.is_abstract)
    }

    fn canonical_name(&self, class: &str) -> Option<String> {
        Some(self.canonical_class(class))
    }

    fn resolve_callable(&self, name: &[u8]) -> Result<HostCallable, String> {
        let n = String::from_utf8_lossy(name).into_owned();
        if let Some((c, m)) = n.split_once("::") {
            if self.find_user_method(c, m).is_some() {
                return Ok(Rc::new(HostFn(V::Str(name.to_vec()))));
            }
            if !self.class_exists(c) {
                return Err(format!("class \"{c}\" not found"));
            }
            return Err(format!("class {c} does not have a method \"{m}\""));
        }
        let exists = self.functions.borrow().contains_key(&n.to_ascii_lowercase().into_bytes()) || is_builtin_fn(&n);
        if exists {
            Ok(Rc::new(HostFn(V::Str(name.to_vec()))))
        } else {
            Err(format!("function \"{n}\" not found or invalid function name"))
        }
    }

    fn call(&self, callable: &HostCallable, args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        let f = match callable.downcast_ref::<HostFn>() {
            Some(h) => h.0.clone(),
            None => match callable.downcast_ref::<Vec<u8>>() {
                Some(n) => V::Str(n.clone()),
                None => return Ok(XPathReturn::Undef),
            },
        };
        let a: Vec<V> = args.into_iter().map(|x| self.xpath_arg(x)).collect();
        match self.call_value(&f, a) {
            Ok(v) => Ok(self.xpath_ret(v)),
            Err(Flow::Throw(e)) => Err(DomError::Host(Box::new(e))),
            Err(_) => Ok(XPathReturn::Undef),
        }
    }

    fn call_by_name(&self, name: &[u8], args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        let a: Vec<V> = args.into_iter().map(|x| self.xpath_arg(x)).collect();
        let n = String::from_utf8_lossy(name).into_owned();
        let exists = self.functions.borrow().contains_key(&n.to_ascii_lowercase().into_bytes()) || n.contains("::") || is_builtin_fn(&n);
        if !exists {
            return Err(DomError::Error(format!("Invalid callback {n}, function \"{n}\" not found or invalid function name")));
        }
        match self.call_value(&V::Str(name.to_vec()), a) {
            Ok(v) => Ok(self.xpath_ret(v)),
            Err(Flow::Throw(e)) => Err(DomError::Host(Box::new(e))),
            Err(_) => Ok(XPathReturn::Undef),
        }
    }
}

impl Interp {
    fn xpath_arg(&self, x: XPathArg) -> V {
        match x {
            XPathArg::Str(s) => V::Str(s),
            XPathArg::Bool(b) => V::Bool(b),
            XPathArg::Float(f) => V::Float(f),
            XPathArg::Nodes(ns) => {
                let mut a = Arr::default();
                for n in ns {
                    a.push(self.node_obj(n));
                }
                V::arr(a)
            }
        }
    }

    fn xpath_ret(&self, v: V) -> XPathReturn {
        match v {
            V::Bool(b) => XPathReturn::Bool(b),
            V::Obj(o) => match o.dom() {
                Some(DomObject::Node(n)) => XPathReturn::Node(n),
                _ => XPathReturn::NonNodeObject,
            },
            V::Null => XPathReturn::Str(Vec::new()),
            other => XPathReturn::Str(to_bytes(&other)),
        }
    }
}

pub fn is_builtin_fn(n: &str) -> bool {
    matches!(
        lc(n).as_str(),
        "strtoupper" | "strtolower" | "trim" | "strlen" | "str_repeat" | "ucfirst" | "var_dump" | "implode" | "count" | "strrev" | "md5" | "sprintf" | "str_replace" | "substr"
    )
}

fn ex_class(v: &V) -> String {
    match v {
        V::Obj(o) => o.class.borrow().clone(),
        _ => String::new(),
    }
}

pub fn to_key(v: &V) -> Key {
    match v {
        V::Int(i) => Key::Int(*i),
        V::Bool(b) => Key::Int(i64::from(*b)),
        V::Float(f) => Key::Int(*f as i64),
        V::Null => Key::Str(Vec::new()),
        V::Str(s) => {
            if let Ok(t) = std::str::from_utf8(s) {
                if let Ok(i) = t.parse::<i64>() {
                    if i.to_string() == t {
                        return Key::Int(i);
                    }
                }
            }
            Key::Str(s.clone())
        }
        other => Key::Str(to_bytes(other)),
    }
}

fn to_offset(v: &V) -> Offset {
    match v {
        V::Int(i) => Offset::Int(*i),
        V::Float(f) => Offset::Float(*f),
        V::Str(s) => Offset::Str(s.clone()),
        V::Null => Offset::Null,
        V::Bool(b) => Offset::Bool(*b),
        V::Arr(a) => Offset::Array(!a.items.is_empty()),
        other => Offset::Other(other.type_name()),
    }
}

pub fn to_num(v: &V) -> V {
    match v {
        V::Int(_) | V::Float(_) => v.clone(),
        V::Bool(b) => V::Int(i64::from(*b)),
        V::Null => V::Int(0),
        V::Str(s) => {
            let t = std::str::from_utf8(s).unwrap_or("").trim();
            if let Ok(i) = t.parse::<i64>() {
                V::Int(i)
            } else if let Ok(f) = t.parse::<f64>() {
                V::Float(f)
            } else {
                let lead: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.').collect();
                lead.parse::<i64>().map(V::Int).unwrap_or(V::Int(0))
            }
        }
        V::Arr(a) => V::Int(i64::from(a.len() > 0)),
        V::Obj(_) => V::Int(1),
    }
}

pub fn to_f_pub(v: &V) -> f64 {
    to_f(v)
}

pub fn to_int(v: &V) -> i64 {
    match to_num(v) {
        V::Int(i) => i,
        V::Float(f) => f as i64,
        _ => 0,
    }
}

fn to_f(v: &V) -> f64 {
    match to_num(v) {
        V::Int(i) => i as f64,
        V::Float(f) => f,
        _ => 0.0,
    }
}

pub fn strict_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Int(x), V::Int(y)) => x == y,
        (V::Float(x), V::Float(y)) => x == y,
        (V::Str(x), V::Str(y)) => x == y,
        (V::Arr(x), V::Arr(y)) => x.items.len() == y.items.len() && x.items.iter().zip(y.items.iter()).all(|((k1, v1), (k2, v2))| k1 == k2 && strict_eq(v1, v2)),
        (V::Obj(x), V::Obj(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn is_numeric(s: &[u8]) -> bool {
    std::str::from_utf8(s).ok().is_some_and(|t| t.trim().parse::<f64>().is_ok())
}

pub fn loose_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), _) => *x == b.truthy(),
        (_, V::Bool(y)) => a.truthy() == *y,
        (V::Null, V::Str(s)) | (V::Str(s), V::Null) => s.is_empty(),
        (V::Null, _) => !b.truthy(),
        (_, V::Null) => !a.truthy(),
        (V::Str(x), V::Str(y)) => {
            if is_numeric(x) && is_numeric(y) {
                to_f(a) == to_f(b)
            } else {
                x == y
            }
        }
        (V::Int(_) | V::Float(_), V::Str(s)) | (V::Str(s), V::Int(_) | V::Float(_)) => {
            if is_numeric(s) {
                to_f(a) == to_f(b)
            } else {
                to_bytes(a) == to_bytes(b)
            }
        }
        (V::Int(_) | V::Float(_), V::Int(_) | V::Float(_)) => to_f(a) == to_f(b),
        (V::Obj(x), V::Obj(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            if let (Some(DomObject::Node(n1)), Some(DomObject::Node(n2))) = (x.dom(), y.dom()) {
                return n1 == n2;
            }
            false
        }
        (V::Arr(x), V::Arr(y)) => x.items.len() == y.items.len() && x.items.iter().all(|(k, v)| y.get(k).is_some_and(|w| loose_eq(v, w))),
        _ => false,
    }
}

pub fn compare(a: &V, b: &V) -> i32 {
    match (a, b) {
        (V::Str(x), V::Str(y)) if !(is_numeric(x) && is_numeric(y)) => match x.cmp(y) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => {
            let (x, y) = (to_f(a), to_f(b));
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
    }
}

fn libxml_const(name: &str) -> Option<i64> {
    use php_dom::constants as k;
    Some(match name {
        "LIBXML_NOENT" => i64::from(k::LIBXML_NOENT),
        "LIBXML_DTDLOAD" => i64::from(k::LIBXML_DTDLOAD),
        "LIBXML_DTDATTR" => i64::from(k::LIBXML_DTDATTR),
        "LIBXML_DTDVALID" => i64::from(k::LIBXML_DTDVALID),
        "LIBXML_NOERROR" => i64::from(k::LIBXML_NOERROR),
        "LIBXML_NOWARNING" => i64::from(k::LIBXML_NOWARNING),
        "LIBXML_NOBLANKS" => i64::from(k::LIBXML_NOBLANKS),
        "LIBXML_XINCLUDE" => i64::from(k::LIBXML_XINCLUDE),
        "LIBXML_NSCLEAN" => i64::from(k::LIBXML_NSCLEAN),
        "LIBXML_NOCDATA" => i64::from(k::LIBXML_NOCDATA),
        "LIBXML_NONET" => i64::from(k::LIBXML_NONET),
        "LIBXML_PEDANTIC" => i64::from(k::LIBXML_PEDANTIC),
        "LIBXML_COMPACT" => i64::from(k::LIBXML_COMPACT),
        "LIBXML_NOXMLDECL" => i64::from(k::LIBXML_NOXMLDECL),
        "LIBXML_PARSEHUGE" => 1 << 19,
        "LIBXML_BIGLINES" => 1 << 22,
        "LIBXML_NOEMPTYTAG" => 4,
        "LIBXML_SCHEMA_CREATE" => 1,
        "LIBXML_HTML_NOIMPLIED" => 1 << 13,
        "LIBXML_HTML_NODEFDTD" => 1 << 2,
        "LIBXML_ERR_NONE" => 0,
        "LIBXML_ERR_WARNING" => 1,
        "LIBXML_ERR_ERROR" => 2,
        "LIBXML_ERR_FATAL" => 3,
        "LIBXML_RECOVER" => 1,
        "LIBXML_NSCLEAN_" => 0,
        _ => return None,
    })
}
