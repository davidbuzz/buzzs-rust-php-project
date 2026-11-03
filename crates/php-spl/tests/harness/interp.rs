//! The tree-walking evaluator.

use php_parser::ast::*;
use php_runtime::{
    ops, ArrayKey, ErrorLevel, HashTable, Obj, PhpObject, PhpRef, RtResult, RuntimeError, Value,
    ZArray, ZStr,
};
use php_spl::{ObjectIterator, SplObject};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

thread_local! {
    static NEXT_HANDLE: Cell<u32> = const { Cell::new(1) };
    static FREE_HANDLES: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

pub fn reset_handles() {
    NEXT_HANDLE.with(|h| h.set(1));
    FREE_HANDLES.with(|f| f.borrow_mut().clear());
}

fn alloc_handle() -> u32 {
    if let Some(h) = FREE_HANDLES.with(|f| f.borrow_mut().pop()) {
        return h;
    }
    NEXT_HANDLE.with(|h| {
        let v = h.get();
        h.set(v + 1);
        v
    })
}

pub enum Extra {
    None,
    Closure(Rc<ClosureData>),
    Iter(Box<dyn ObjectIterator>),
}

pub struct TObj {
    pub handle: u32,
    pub class: Rc<Class>,
    pub props: RefCell<ZArray>,
    pub spl: RefCell<Option<Rc<SplObject>>>,
    pub extra: RefCell<Extra>,
}

impl Drop for TObj {
    fn drop(&mut self) {
        let h = self.handle;
        let _ = FREE_HANDLES.try_with(|f| f.borrow_mut().push(h));
    }
}

impl std::fmt::Debug for TObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TObj({}#{})", self.class.name, self.handle)
    }
}

impl PhpObject for TObj {
    fn handle(&self) -> u32 {
        self.handle
    }
    fn class_name(&self) -> ZStr {
        ZStr::from(self.class.name.as_str())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn tobj(o: &Obj) -> &TObj {
    o.downcast_ref::<TObj>().expect("harness object")
}

// ---------------------------------------------------------------------------
// Classes and functions
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FuncDef {
    pub return_type: Option<Type>,
    pub name: String,
    pub params: Vec<Param>,
    pub body: Rc<Vec<Stmt>>,
    pub arrow: Option<Rc<Expr>>,
    pub by_ref: bool,
    pub is_generator: bool,
    pub statics: Rc<RefCell<HashMap<Vec<u8>, PhpRef>>>,
}

pub struct ClosureData {
    pub func: Rc<FuncDef>,
    pub bound: Vec<(Vec<u8>, Value)>,
    pub this: Option<Obj>,
    pub scope: Option<Rc<Class>>,
    /// A builtin function wrapped as closure (first-class callable syntax).
    pub builtin: Option<String>,
    /// A bound method (`$obj->m(...)`, `Closure::fromCallable`).
    pub method: Option<(Option<Obj>, Rc<Class>, Rc<MethodH>)>,
}

pub type NativeFn = fn(&mut Interp, Option<&Obj>, &[Value]) -> RtResult<Value>;

pub enum MKind {
    User(Rc<FuncDef>),
    Spl,
    Native(NativeFn),
    Abstract,
}

pub struct MethodH {
    pub name: String,
    pub class: String,
    pub kind: MKind,
    pub is_static: bool,
    pub visibility: u8, // 0 public 1 protected 2 private
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CKind {
    Class,
    Abstract,
    Interface,
    Trait,
}

pub struct PropDecl {
    pub name: String,
    pub visibility: u8,
    pub default: Option<Expr>,
    pub is_static: bool,
    pub typed: bool,
}

pub struct Class {
    pub name: String,
    pub parent: Option<Rc<Class>>,
    pub interfaces: Vec<String>,
    pub kind: CKind,
    pub consts: RefCell<Vec<(String, Result<Value, Rc<Expr>>)>>,
    pub statics: RefCell<HashMap<String, Value>>,
    pub props: Vec<PropDecl>,
    pub methods: HashMap<String, Rc<MethodH>>,
    pub spl_base: Option<&'static str>,
    pub internal: bool,
    pub allow_dynamic: bool,
}

impl Class {
    pub fn find_method(self: &Rc<Self>, lc: &str) -> Option<Rc<MethodH>> {
        let mut c = Some(self.clone());
        while let Some(k) = c {
            if let Some(m) = k.methods.get(lc) {
                return Some(m.clone());
            }
            c = k.parent.clone();
        }
        None
    }

    pub fn is_a(self: &Rc<Self>, name: &str) -> bool {
        let mut c = Some(self.clone());
        while let Some(k) = c {
            if k.name.eq_ignore_ascii_case(name) {
                return true;
            }
            if k.interfaces.iter().any(|i| i.eq_ignore_ascii_case(name)) {
                return true;
            }
            if k.internal && php_spl::classes::is_subclass_of(&k.name, name) {
                return true;
            }
            c = k.parent.clone();
        }
        false
    }

    pub fn find_const(self: &Rc<Self>, name: &str) -> Option<(Rc<Class>, Result<Value, Rc<Expr>>)> {
        let mut c = Some(self.clone());
        while let Some(k) = c {
            if let Some((_, v)) = k.consts.borrow().iter().find(|(n, _)| n == name) {
                return Some((k.clone(), v.clone()));
            }
            c = k.parent.clone();
        }
        None
    }

    pub fn all_props(self: &Rc<Self>) -> Vec<(Rc<Class>, usize)> {
        let mut chain = Vec::new();
        let mut c = Some(self.clone());
        while let Some(k) = c {
            chain.push(k.clone());
            c = k.parent.clone();
        }
        let mut out = Vec::new();
        for k in chain.iter().rev() {
            for (i, p) in k.props.iter().enumerate() {
                if !p.is_static {
                    out.push((k.clone(), i));
                }
            }
        }
        out
    }
}

pub fn mangle(class: &str, name: &str, vis: u8) -> ZStr {
    match vis {
        0 => ZStr::from(name),
        1 => {
            let mut v = vec![0, b'*', 0];
            v.extend_from_slice(name.as_bytes());
            ZStr::from(v)
        }
        _ => php_spl::util::private_prop(class, name),
    }
}

// ---------------------------------------------------------------------------
// Interpreter state
// ---------------------------------------------------------------------------

pub enum Flow {
    Normal,
    Return(Value),
    Break(u32),
    Continue(u32),
}

pub struct Frame {
    pub vars: HashMap<Vec<u8>, Value>,
    pub this: Option<Obj>,
    pub scope: Option<Rc<Class>>,
    pub static_scope: Option<Rc<Class>>,
    pub func: String,
    pub args: Vec<Value>,
    pub func_def: Option<Rc<FuncDef>>,
}

impl Frame {
    fn new() -> Frame {
        Frame {
            vars: HashMap::new(),
            this: None,
            scope: None,
            static_scope: None,
            func: String::new(),
            args: Vec::new(),
            func_def: None,
        }
    }
}

pub const EXIT_MARK: &str = "\0exit";

/// One frame of the harness call stack (for exception traces).
#[derive(Clone)]
pub struct TFrame {
    pub internal: bool,
    pub func: String,
    pub class: Option<String>,
    pub ty: &'static str,
    pub args: Vec<Value>,
    pub line: u32,
}

pub struct Interp {
    pub out: Vec<u8>,
    pub file: String,
    pub dir: String,
    pub line: u32,
    pub frames: Vec<Frame>,
    pub classes: HashMap<String, Rc<Class>>,
    pub functions: HashMap<String, Rc<FuncDef>>,
    pub constants: HashMap<String, Value>,
    pub error_handlers: Vec<Value>,
    pub exception_handler: Option<Value>,
    pub error_reporting: i64,
    pub in_error_handler: bool,
    pub ob_stack: Vec<Vec<u8>>,
    pub pending_classes: Vec<Rc<ClassDecl>>,
    pub shutdown: Vec<(Value, Vec<Value>)>,
    pub depth: usize,
    pub tmpfiles: Vec<String>,
    pub pcre: php_pcre::Pcre,
    pub deadline: std::time::Instant,
    pub pending_named: Vec<(String, Value)>,
    pub trace_stack: Vec<TFrame>,
    pub steps: u64,
}

pub fn rt_err(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::Error(msg.into())
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub fn lc(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_lowercase()
}

impl Interp {
    pub fn new(file: &str) -> Interp {
        reset_handles();
        php_spl::functions::reset_request();
        let mut it = Interp {
            out: Vec::new(),
            file: file.to_string(),
            dir: String::from("."),
            line: 0,
            frames: vec![Frame::new()],
            classes: HashMap::new(),
            functions: HashMap::new(),
            constants: HashMap::new(),
            error_handlers: Vec::new(),
            exception_handler: None,
            error_reporting: 32767,
            in_error_handler: false,
            ob_stack: Vec::new(),
            pending_classes: Vec::new(),
            shutdown: Vec::new(),
            depth: 0,
            tmpfiles: Vec::new(),
            pcre: php_pcre::Pcre::new(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
            steps: 0,
            pending_named: Vec::new(),
            trace_stack: Vec::new(),
        };
        crate::harness::builtins::register_classes(&mut it);
        crate::harness::builtins::register_constants(&mut it);
        it
    }

    pub fn apply_ini(&mut self, ini: &[u8]) {
        for line in ini.split(|&c| c == b'\n') {
            let l = s(line);
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == "error_reporting" {
                    let v = v.trim();
                    if let Ok(n) = v.parse::<i64>() {
                        self.error_reporting = n;
                    } else if v == "E_ALL" {
                        self.error_reporting = 32767;
                    }
                }
            }
        }
    }

    pub fn echo(&mut self, b: &[u8]) {
        match self.ob_stack.last_mut() {
            Some(buf) => buf.extend_from_slice(b),
            None => self.out.extend_from_slice(b),
        }
    }

    // -----------------------------------------------------------------------
    // Running programs
    // -----------------------------------------------------------------------

    pub fn run_source(&mut self, src: &[u8]) {
        let program = match php_parser::parse(src, &php_parser::ParseOptions::default()) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!(
                    "\nParse error: {} in {} on line {}\n",
                    e.message, self.file, e.line
                );
                self.out.extend_from_slice(msg.as_bytes());
                return;
            }
        };
        let stmts = Rc::new(program.statements);
        self.hoist(&stmts);
        let r = self.exec_block(&stmts);
        if let Err(e) = r {
            self.uncaught(e);
        }
        let sd = std::mem::take(&mut self.shutdown);
        for (f, args) in sd {
            if let Err(e) = self.call_value(&f, &args) {
                self.uncaught(e);
                break;
            }
        }
        while let Some(b) = self.ob_stack.pop() {
            self.echo(&b);
        }
        for f in std::mem::take(&mut self.tmpfiles) {
            let _ = std::fs::remove_file(f);
        }
    }

    pub fn uncaught(&mut self, e: RuntimeError) {
        if let RuntimeError::Custom { class, .. } = &e {
            if class.as_bytes() == EXIT_MARK.as_bytes() {
                return;
            }
        }
        if let RuntimeError::Fatal(m) = &e {
            let msg = format!(
                "\nFatal error: {m} in {} on line {}\n",
                self.file, self.line
            );
            self.echo(msg.as_bytes());
            return;
        }
        let ex = self.exception_object(e);
        if let Some(h) = self.exception_handler.take() {
            if let Err(e2) = self.call_value(&h, &[Value::Object(ex)]) {
                self.uncaught(e2);
            }
            return;
        }
        let text = match self.exception_to_string(&ex) {
            Ok(t) => t,
            Err(_) => String::from("?"),
        };
        let line = self
            .prop_get_raw(&ex, "line")
            .deref()
            .to_long(self)
            .unwrap_or(0);
        let msg = format!(
            "PHP Fatal error:  Uncaught {text}\n  thrown in {} on line {line}\n",
            self.file
        );
        let _ = msg;
        let msg = format!(
            "\nFatal error: Uncaught {text}\n  thrown in {} on line {line}\n",
            self.file
        );
        self.echo(msg.as_bytes());
    }

    pub fn build_trace(&self) -> Value {
        let mut t = HashTable::new();
        let n = self.trace_stack.len();
        for (i, f) in self.trace_stack.iter().enumerate().rev() {
            let mut fr = HashTable::new();
            let caller_internal = i > 0 && self.trace_stack[i - 1].internal;
            if !caller_internal {
                fr.insert(ArrayKey::from("file"), Value::from(self.file.as_str()));
                fr.insert(ArrayKey::from("line"), Value::Int(i64::from(f.line)));
            }
            let _ = n;
            fr.insert(ArrayKey::from("function"), Value::from(f.func.as_str()));
            if let Some(c) = &f.class {
                fr.insert(ArrayKey::from("class"), Value::from(c.as_str()));
                fr.insert(ArrayKey::from("type"), Value::from(f.ty));
            }
            fr.insert(
                ArrayKey::from("args"),
                Value::Array(ZArray::from_values(f.args.iter().map(Value::deref))),
            );
            let _ = t.push(Value::Array(ZArray::from_table(fr)));
        }
        Value::Array(ZArray::from_table(t))
    }

    pub fn trace_string(&mut self, ex: &Obj) -> String {
        let trace = self.prop_get_raw(ex, "trace");
        let mut out = String::new();
        let mut n = 0;
        if let Value::Array(frames) = trace {
            for f in frames.values() {
                let Value::Array(f) = f.deref() else { continue };
                let g = |k: &str| f.get_str(k.as_bytes()).cloned().unwrap_or_default();
                let file = g("file")
                    .to_zstr(self)
                    .map(|z| z.to_string_lossy())
                    .unwrap_or_default();
                let line = g("line").to_long(self).unwrap_or(0);
                let func = g("function")
                    .to_zstr(self)
                    .map(|z| z.to_string_lossy())
                    .unwrap_or_default();
                let class = match g("class") {
                    Value::Str(c) => format!(
                        "{}{}",
                        c.to_string_lossy(),
                        g("type")
                            .to_zstr(self)
                            .map(|z| z.to_string_lossy())
                            .unwrap_or_default()
                    ),
                    _ => String::new(),
                };
                let mut args = Vec::new();
                if let Value::Array(a) = g("args") {
                    for v in a.values() {
                        args.push(trace_arg(&v.deref()));
                    }
                }
                let loc = if f.get_str(b"file").is_some() {
                    format!("{file}({line})")
                } else {
                    "[internal function]".to_string()
                };
                out.push_str(&format!("#{n} {loc}: {class}{func}({})\n", args.join(", ")));
                n += 1;
            }
        }
        out.push_str(&format!("#{n} {{main}}"));
        out
    }

    pub fn exception_to_string(&mut self, ex: &Obj) -> RtResult<String> {
        let mut parts = Vec::new();
        let mut cur = Some(ex.clone());
        while let Some(e) = cur {
            let msg = self
                .prop_get_raw(&e, "message")
                .to_zstr(self)?
                .to_string_lossy();
            let line = self.prop_get_raw(&e, "line").to_long(self)?;
            let class = e.class_name().to_string_lossy();
            let trace = self.trace_string(&e);
            let m = if msg.is_empty() {
                format!("{class} in {}:{line}\nStack trace:\n{trace}", self.file)
            } else {
                format!(
                    "{class}: {msg} in {}:{line}\nStack trace:\n{trace}",
                    self.file
                )
            };
            parts.push(m);
            cur = match self.prop_get_raw(&e, "previous") {
                Value::Object(p) => Some(p),
                _ => None,
            };
        }
        parts.reverse();
        Ok(parts.join("\n\nNext "))
    }

    pub fn hoist(&mut self, stmts: &Rc<Vec<Stmt>>) {
        for st in stmts.iter() {
            if let StmtKind::Function(f) = &st.kind {
                self.declare_function(f);
            }
        }
        let mut pending: Vec<&ClassDecl> = stmts
            .iter()
            .filter_map(|st| match &st.kind {
                StmtKind::Class(c) => Some(c.as_ref()),
                _ => None,
            })
            .collect();
        loop {
            let before = pending.len();
            pending.retain(|c| {
                let deps_ok = c
                    .extends
                    .iter()
                    .chain(c.implements.iter())
                    .all(|n| self.find_class(&n.name).is_some());
                if deps_ok {
                    let _ = self.declare_class(c);
                    false
                } else {
                    true
                }
            });
            if pending.len() == before {
                break;
            }
        }
    }

    pub fn declare_function(&mut self, f: &FunctionDecl) {
        let def = FuncDef {
            return_type: f.return_type.clone(),
            name: s(&f.name.name),
            params: f.params.clone(),
            body: Rc::new(f.body.clone()),
            arrow: None,
            by_ref: f.by_ref,
            is_generator: f.is_generator,
            statics: Rc::new(RefCell::new(HashMap::new())),
        };
        self.functions.insert(lc(&f.name.name), Rc::new(def));
    }

    pub fn find_class(&self, name: &[u8]) -> Option<Rc<Class>> {
        let n = name.strip_prefix(b"\\").unwrap_or(name);
        self.classes.get(&lc(n)).cloned()
    }

    pub fn lookup_class(&mut self, name: &[u8]) -> RtResult<Rc<Class>> {
        if let Some(c) = self.find_class(name) {
            return Ok(c);
        }
        let n = name.strip_prefix(b"\\").unwrap_or(name);
        php_spl::functions::perform_autoload(self, &ZStr::from(n))?;
        self.find_class(name)
            .ok_or_else(|| rt_err(format!("Class \"{}\" not found", s(n))))
    }

    pub fn declare_class(&mut self, c: &ClassDecl) -> RtResult<Rc<Class>> {
        let name = c
            .name
            .as_ref()
            .map(|n| s(&n.name))
            .unwrap_or_else(|| "class@anonymous".into());
        if let Some(k) = self.classes.get(&name.to_ascii_lowercase()) {
            if c.name.is_some() {
                return Ok(k.clone());
            }
        }
        let parent = match &c.extends {
            Some(n) if c.kind != ClassKind::Interface => Some(self.lookup_class(&n.name)?),
            _ => None,
        };
        let mut interfaces = Vec::new();
        let ifaces: Vec<&Name> = if c.kind == ClassKind::Interface {
            c.extends.iter().chain(c.implements.iter()).collect()
        } else {
            c.implements.iter().collect()
        };
        for n in ifaces {
            let ic = self.lookup_class(&n.name)?;
            interfaces.push(ic.name.clone());
            let mut k = Some(ic.clone());
            while let Some(kk) = k {
                for i in &kk.interfaces {
                    if !interfaces
                        .iter()
                        .any(|x: &String| x.eq_ignore_ascii_case(i))
                    {
                        interfaces.push(i.clone());
                    }
                }
                k = kk.parent.clone();
            }
        }
        let kind = match c.kind {
            ClassKind::Interface => CKind::Interface,
            ClassKind::Trait => CKind::Trait,
            _ if c.modifiers.contains(Modifiers::ABSTRACT) => CKind::Abstract,
            _ => CKind::Class,
        };
        let mut consts = Vec::new();
        let mut props = Vec::new();
        let mut methods = HashMap::new();
        let mut members: Vec<ClassMember> = c.members.clone();
        // traits: copy members
        for m in &c.members {
            if let ClassMemberKind::TraitUse(tu) = &m.kind {
                for t in &tu.traits {
                    if let Some(tc) = self.trait_decls(&t.name) {
                        members.extend(tc.members.iter().cloned());
                    }
                }
            }
        }
        for m in &members {
            match &m.kind {
                ClassMemberKind::Const(g) => {
                    for ce in &g.consts {
                        consts.push((s(&ce.name.name), Err(Rc::new(ce.value.clone()))));
                    }
                }
                ClassMemberKind::Property(g) => {
                    let vis = vis_of(g.modifiers);
                    for p in &g.props {
                        props.push(PropDecl {
                            name: s(&p.name.name),
                            visibility: vis,
                            default: p.default.clone(),
                            is_static: g.modifiers.contains(Modifiers::STATIC),
                            typed: g.ty.is_some(),
                        });
                    }
                }
                ClassMemberKind::Method(md) => {
                    let mname = s(&md.name.name);
                    if mname.eq_ignore_ascii_case("__construct") {
                        for p in &md.params {
                            if !p.modifiers.is_empty() {
                                props.push(PropDecl {
                                    name: s(&p.name.name),
                                    visibility: vis_of(p.modifiers),
                                    default: None,
                                    is_static: false,
                                    typed: p.ty.is_some(),
                                });
                            }
                        }
                    }
                    let kind = match &md.body {
                        Some(b) => MKind::User(Rc::new(FuncDef {
                            return_type: md.return_type.clone(),
                            name: format!("{name}::{mname}"),
                            params: md.params.clone(),
                            body: Rc::new(b.clone()),
                            arrow: None,
                            by_ref: md.by_ref,
                            is_generator: md.is_generator,
                            statics: Rc::new(RefCell::new(HashMap::new())),
                        })),
                        None => MKind::Abstract,
                    };
                    methods.insert(
                        mname.to_ascii_lowercase(),
                        Rc::new(MethodH {
                            name: mname,
                            class: name.clone(),
                            kind,
                            is_static: md.modifiers.contains(Modifiers::STATIC),
                            visibility: vis_of(md.modifiers),
                        }),
                    );
                }
                _ => {}
            }
        }
        let mut statics = HashMap::new();
        let spl_base = parent.as_ref().and_then(|p| p.spl_base);
        let allow_dynamic = c.attributes.iter().any(|g| {
            g.attributes
                .iter()
                .any(|a| lc(&a.name.name).trim_start_matches('\\') == "allowdynamicproperties")
        }) || parent.as_ref().is_some_and(|p| p.allow_dynamic);
        let class = Rc::new(Class {
            name: name.clone(),
            parent,
            interfaces,
            kind,
            consts: RefCell::new(consts),
            statics: RefCell::new(HashMap::new()),
            props,
            methods,
            spl_base,
            internal: false,
            allow_dynamic,
        });
        if c.kind == ClassKind::Trait {
            self.trait_store(&name, c);
        }
        if c.name.is_some() {
            self.classes
                .insert(name.to_ascii_lowercase(), class.clone());
        }
        // static props defaults
        let decls: Vec<(String, Option<Expr>)> = class
            .props
            .iter()
            .filter(|p| p.is_static)
            .map(|p| (p.name.clone(), p.default.clone()))
            .collect();
        self.frames.push(Frame {
            scope: Some(class.clone()),
            static_scope: Some(class.clone()),
            ..Frame::new()
        });
        for (n, d) in decls {
            let v = match d {
                Some(e) => self.eval(&e).unwrap_or_default(),
                None => Value::Null,
            };
            statics.insert(n, v);
        }
        self.frames.pop();
        *class.statics.borrow_mut() = statics;
        Ok(class)
    }

    fn trait_store(&mut self, name: &str, c: &ClassDecl) {
        TRAITS.with(|t| {
            t.borrow_mut()
                .insert(name.to_ascii_lowercase(), Rc::new(c.clone()))
        });
    }

    fn trait_decls(&self, name: &[u8]) -> Option<Rc<ClassDecl>> {
        let n = name.strip_prefix(b"\\").unwrap_or(name);
        TRAITS.with(|t| t.borrow().get(&lc(n)).cloned())
    }

    // -----------------------------------------------------------------------
    // Objects
    // -----------------------------------------------------------------------

    pub fn new_object_raw(&mut self, class: &Rc<Class>) -> RtResult<Obj> {
        if matches!(
            class.kind,
            CKind::Abstract | CKind::Interface | CKind::Trait
        ) {
            let what = match class.kind {
                CKind::Interface => "interface",
                CKind::Trait => "trait",
                _ => "abstract class",
            };
            return Err(rt_err(format!("Cannot instantiate {what} {}", class.name)));
        }
        let spl = class.spl_base.and_then(php_spl::create_state);
        let obj = Obj::new(TObj {
            handle: alloc_handle(),
            class: class.clone(),
            props: RefCell::new(ZArray::new()),
            spl: RefCell::new(spl),
            extra: RefCell::new(Extra::None),
        });
        // default properties
        let mut table = HashTable::new();
        for (k, i) in class.all_props() {
            let p = &k.props[i];
            if p.visibility == 2 && !Rc::ptr_eq(&k, class) {
                // private of a parent: still present, mangled with parent name
            }
            let v = match &p.default {
                Some(e) => {
                    self.frames.push(Frame {
                        scope: Some(k.clone()),
                        static_scope: Some(class.clone()),
                        ..Frame::new()
                    });
                    let v = self.eval(e);
                    self.frames.pop();
                    v?
                }
                None if p.typed => continue,
                None => Value::Null,
            };
            table.insert_str_raw(mangle(&k.name, &p.name, p.visibility), v);
        }
        *tobj(&obj).props.borrow_mut() = ZArray::from_table(table);
        if class.is_a("Throwable") {
            let f = Value::from(self.file.as_str());
            let l = Value::Int(i64::from(self.line));
            self.prop_set_raw(&obj, "file", f);
            self.prop_set_raw(&obj, "line", l);
            let trace = self.build_trace();
            self.prop_set_raw(&obj, "trace", trace);
        }
        Ok(obj)
    }

    pub fn clone_object(&mut self, old: &Obj) -> RtResult<Obj> {
        let t = tobj(old);
        if matches!(&*t.extra.borrow(), Extra::Closure(_)) {
            return Ok(old.clone());
        }
        let class = t.class.clone();
        let obj = Obj::new(TObj {
            handle: alloc_handle(),
            class: class.clone(),
            props: RefCell::new(ZArray::from_table((*t.props.borrow().clone()).clone())),
            spl: RefCell::new(None),
            extra: RefCell::new(Extra::None),
        });
        if t.spl.borrow().is_some() {
            let st = php_spl::handlers::clone_state(self, old, &obj)?;
            *tobj(&obj).spl.borrow_mut() = st;
        }
        if let Some(m) = class.find_method("__clone") {
            self.call_method_h(Some(&obj), &class, &m, &[])?;
        }
        Ok(obj)
    }

    pub fn instantiate(&mut self, class: &Rc<Class>, args: &[Value]) -> RtResult<Obj> {
        let obj = self.new_object_raw(class)?;
        if let Some(m) = class.find_method("__construct") {
            self.call_method_h(Some(&obj), class, &m, args)?;
        } else if !args.is_empty() && false {
        }
        Ok(obj)
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    pub fn emit(&mut self, level: ErrorLevel, msg: &str) -> RtResult<()> {
        let bits = level.bits();
        if !self.error_handlers.is_empty() && !self.in_error_handler {
            let h = self.error_handlers.last().cloned().unwrap_or_default();
            if !h.is_null() {
                self.in_error_handler = true;
                let r = self.call_value(
                    &h,
                    &[
                        Value::Int(bits),
                        Value::from(msg),
                        Value::from(self.file.as_str()),
                        Value::Int(i64::from(self.line)),
                    ],
                );
                self.in_error_handler = false;
                match r {
                    Ok(v) if !matches!(v, Value::Bool(false)) => return Ok(()),
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        if self.error_reporting & bits == 0 {
            return Ok(());
        }
        let text = format!(
            "\n{}: {} in {} on line {}\n",
            level.label(),
            msg,
            self.file,
            self.line
        );
        self.echo(text.as_bytes());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Exceptions
    // -----------------------------------------------------------------------

    /// Converts a runtime error into an exception object.
    pub fn exception_object(&mut self, e: RuntimeError) -> Obj {
        match e {
            RuntimeError::Thrown(Value::Object(o)) => o,
            other => {
                let class = other
                    .exception_class()
                    .unwrap_or_else(|| "Error".to_string());
                let msg = other.message().unwrap_or("").to_string();
                let cls = self
                    .find_class(class.as_bytes())
                    .or_else(|| self.find_class(b"Exception"))
                    .expect("Exception class");
                let o = self.new_object_raw(&cls).expect("exception object");
                self.prop_set_raw(&o, "message", Value::from(msg));
                o
            }
        }
    }

    // -----------------------------------------------------------------------
    // Properties (raw: no visibility checks)
    // -----------------------------------------------------------------------

    pub fn find_prop_key(&self, o: &Obj, name: &str) -> Option<ZStr> {
        let t = tobj(o);
        let props = t.props.borrow();
        if let Some(scope) = self.frames.last().and_then(|f| f.scope.clone()) {
            let k = mangle(&scope.name, name, 2);
            if props.get_str(k.as_bytes()).is_some() {
                return Some(k);
            }
        }
        if props.get_str(name.as_bytes()).is_some() {
            return Some(ZStr::from(name));
        }
        let k = mangle("", name, 1);
        if props.get_str(k.as_bytes()).is_some() {
            return Some(k);
        }
        // private of any class in the chain
        let mut c = Some(t.class.clone());
        while let Some(k) = c {
            let key = mangle(&k.name, name, 2);
            if props.get_str(key.as_bytes()).is_some() {
                return Some(key);
            }
            c = k.parent.clone();
        }
        None
    }

    pub fn prop_get_raw(&self, o: &Obj, name: &str) -> Value {
        match self.find_prop_key(o, name) {
            Some(k) => tobj(o)
                .props
                .borrow()
                .get_str(k.as_bytes())
                .cloned()
                .unwrap_or_default(),
            None => Value::Null,
        }
    }

    pub fn prop_set_raw(&self, o: &Obj, name: &str, v: Value) {
        let key = self
            .find_prop_key(o, name)
            .unwrap_or_else(|| ZStr::from(name));
        let t = tobj(o);
        let mut p = t.props.borrow_mut();
        let slot = p.make_mut();
        match slot.get_mut(&ArrayKey::Str(key.clone())) {
            Some(Value::Reference(r)) => {
                r.set(v);
            }
            _ => {
                slot.insert_str_raw(key, v);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------------

    pub fn exec_block(&mut self, stmts: &[Stmt]) -> RtResult<Flow> {
        for st in stmts {
            match self.exec(st)? {
                Flow::Normal => {}
                f => return Ok(f),
            }
        }
        Ok(Flow::Normal)
    }

    pub fn exec(&mut self, st: &Stmt) -> RtResult<Flow> {
        self.line = st.span.line;
        self.steps += 1;
        if self.steps % 1024 == 0 && std::time::Instant::now() > self.deadline {
            return Err(RuntimeError::Fatal("harness timeout".into()));
        }
        match &st.kind {
            StmtKind::Block(b) => self.exec_block(b),
            StmtKind::Expr(e) => {
                self.eval(e)?;
                Ok(Flow::Normal)
            }
            StmtKind::Echo(es) => {
                for e in es {
                    let v = self.eval(e)?;
                    let z = v.to_zstr(self)?;
                    self.echo(z.as_bytes());
                }
                Ok(Flow::Normal)
            }
            StmtKind::InlineHtml(h) => {
                self.echo(h);
                Ok(Flow::Normal)
            }
            StmtKind::If {
                branches,
                else_branch,
            } => {
                for b in branches {
                    let c = self.eval(&b.cond)?;
                    if c.to_bool(self)? {
                        return self.exec(&b.body);
                    }
                }
                match else_branch {
                    Some(e) => self.exec(e),
                    None => Ok(Flow::Normal),
                }
            }
            StmtKind::While { cond, body } => {
                loop {
                    let c = self.eval(cond)?;
                    if !c.to_bool(self)? {
                        break;
                    }
                    match self.exec(body)? {
                        Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                        Flow::Break(_) => break,
                        Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::DoWhile { body, cond } => {
                loop {
                    match self.exec(body)? {
                        Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                        Flow::Break(_) => break,
                        Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    let c = self.eval(cond)?;
                    if !c.to_bool(self)? {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                for e in init {
                    self.eval(e)?;
                }
                loop {
                    let mut ok = true;
                    for (i, e) in cond.iter().enumerate() {
                        let v = self.eval(e)?;
                        if i == cond.len() - 1 {
                            ok = v.to_bool(self)?;
                        }
                    }
                    if !ok {
                        break;
                    }
                    match self.exec(body)? {
                        Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                        Flow::Break(_) => break,
                        Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    for e in step {
                        self.eval(e)?;
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body,
            } => self.exec_foreach(subject, key.as_ref(), value, *by_ref, body),
            StmtKind::Switch { subject, cases } => {
                let v = self.eval(subject)?;
                let mut matched = None;
                for (i, c) in cases.iter().enumerate() {
                    if let Some(ce) = &c.cond {
                        let cv = self.eval(ce)?;
                        if ops::is_equal(&v, &cv, self)? {
                            matched = Some(i);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    matched = cases.iter().position(|c| c.cond.is_none());
                }
                if let Some(i) = matched {
                    for c in &cases[i..] {
                        match self.exec_block(&c.body)? {
                            Flow::Normal => {}
                            Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                            Flow::Break(_) => return Ok(Flow::Normal),
                            Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                            Flow::Continue(_) => return Ok(Flow::Normal),
                            f => return Ok(f),
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Break(n) => Ok(Flow::Break(self.level(n)?)),
            StmtKind::Continue(n) => Ok(Flow::Continue(self.level(n)?)),
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Global(names) => {
                for n in names {
                    if let ExprKind::Var(VarName::Name(id)) = &n.kind {
                        let r = {
                            let g = &mut self.frames[0].vars;
                            g.entry(id.name.clone()).or_insert(Value::Null).make_ref()
                        };
                        self.frames
                            .last_mut()
                            .unwrap()
                            .vars
                            .insert(id.name.clone(), Value::Reference(r));
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Static(vars) => {
                let statics = self
                    .frames
                    .last()
                    .and_then(|f| f.func_def.as_ref().map(|d| d.statics.clone()));
                for v in vars {
                    let r = match &statics {
                        Some(st) => {
                            let existing = st.borrow().get(&v.name.name).cloned();
                            match existing {
                                Some(r) => r,
                                None => {
                                    let init = match &v.default {
                                        Some(e) => self.eval(e)?,
                                        None => Value::Null,
                                    };
                                    let r = PhpRef::new(init);
                                    st.borrow_mut().insert(v.name.name.clone(), r.clone());
                                    r
                                }
                            }
                        }
                        None => PhpRef::new(match &v.default {
                            Some(e) => self.eval(e)?,
                            None => Value::Null,
                        }),
                    };
                    self.frames
                        .last_mut()
                        .unwrap()
                        .vars
                        .insert(v.name.name.clone(), Value::Reference(r));
                }
                Ok(Flow::Normal)
            }
            StmtKind::Unset(es) => {
                for e in es {
                    self.unset(e)?;
                }
                Ok(Flow::Normal)
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                let r = self.exec_block(body);
                let r = match r {
                    Err(e) if !is_exit(&e) && !matches!(e, RuntimeError::Fatal(_)) => {
                        let ex = self.exception_object(e);
                        let mut handled = None;
                        for c in catches {
                            let hit = c.types.iter().any(|t| {
                                let n = t.name.strip_prefix(b"\\").unwrap_or(&t.name);
                                let n = s(n);
                                tobj(&ex).class.is_a(&n) || (n.eq_ignore_ascii_case("Throwable"))
                            });
                            if hit {
                                if let Some(v) = &c.var {
                                    self.set_var(&v.name, Value::Object(ex.clone()));
                                }
                                handled = Some(self.exec_block(&c.body));
                                break;
                            }
                        }
                        match handled {
                            Some(r) => r,
                            None => Err(RuntimeError::Thrown(Value::Object(ex))),
                        }
                    }
                    other => other,
                };
                if let Some(f) = finally {
                    match self.exec_block(f)? {
                        Flow::Normal => {}
                        fl => return Ok(fl),
                    }
                }
                r
            }
            StmtKind::Function(f) => {
                if !self.functions.contains_key(&lc(&f.name.name)) {
                    self.declare_function(f);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Class(c) => {
                self.declare_class(c)?;
                Ok(Flow::Normal)
            }
            StmtKind::Const { consts, .. } => {
                for c in consts {
                    let v = self.eval(&c.value)?;
                    self.constants.insert(s(&c.name.name), v);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Declare { body, .. } => match body {
                Some(b) => self.exec(b),
                None => Ok(Flow::Normal),
            },
            StmtKind::Namespace { body, .. } => match body {
                Some(b) => {
                    let b = Rc::new(b.clone());
                    self.hoist(&b);
                    self.exec_block(&b)
                }
                None => Ok(Flow::Normal),
            },
            StmtKind::Nop
            | StmtKind::Use { .. }
            | StmtKind::GroupUse { .. }
            | StmtKind::Label(_) => Ok(Flow::Normal),
            StmtKind::HaltCompiler { .. } => Err(RuntimeError::Custom {
                class: ZStr::from(EXIT_MARK),
                message: String::new(),
            }),
            StmtKind::Goto(_) => Err(rt_err("goto unsupported")),
        }
    }

    fn level(&mut self, n: &Option<Expr>) -> RtResult<u32> {
        Ok(match n {
            Some(e) => {
                let v = self.eval(e)?;
                u32::try_from(v.to_long(self)?).unwrap_or(1)
            }
            None => 1,
        })
    }

    fn exec_foreach(
        &mut self,
        subject: &Expr,
        key: Option<&Expr>,
        value: &Expr,
        by_ref: bool,
        body: &Stmt,
    ) -> RtResult<Flow> {
        let subj = if by_ref {
            let r = self.get_ref(subject)?;
            Value::Reference(r)
        } else {
            self.eval(subject)?
        };
        match subj.deref() {
            Value::Array(arr) => {
                if by_ref {
                    let Value::Reference(r) = subj else {
                        unreachable!()
                    };
                    let mut pos = 0u32;
                    loop {
                        let item = {
                            let mut b = r.borrow_mut();
                            let Value::Array(a) = &mut *b else { break };
                            let ht = a.make_mut();
                            let p = ht.valid_pos(pos);
                            if p >= ht.used() {
                                None
                            } else {
                                let (k, _) = ht.at(p).map(|(k, v)| (k.clone(), v.clone())).unwrap();
                                let slot = ht.at_mut(p).unwrap();
                                let rr = slot.make_ref();
                                pos = p + 1;
                                Some((k, rr))
                            }
                        };
                        let Some((k, rr)) = item else { break };
                        self.assign_ref_to(value, rr)?;
                        if let Some(ke) = key {
                            self.assign(ke, k.to_value())?;
                        }
                        match self.exec(body)? {
                            Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                            Flow::Break(_) => break,
                            Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            _ => {}
                        }
                    }
                    return Ok(Flow::Normal);
                }
                for (k, v) in arr.iter() {
                    let v = v.deref();
                    self.assign_foreach_value(value, v)?;
                    if let Some(ke) = key {
                        self.assign(ke, k.to_value())?;
                    }
                    match self.exec(body)? {
                        Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                        Flow::Break(_) => break,
                        Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Value::Object(o) => {
                let t = tobj(&o);
                if !t.class.is_a("Traversable") {
                    // iterate visible properties
                    let props = self.visible_props(&o);
                    for (k, v) in props.iter() {
                        self.assign_foreach_value(value, v.deref())?;
                        if let Some(ke) = key {
                            self.assign(ke, k.to_value())?;
                        }
                        match self.exec(body)? {
                            Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                            Flow::Break(_) => break,
                            Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            _ => {}
                        }
                    }
                    return Ok(Flow::Normal);
                }
                let mut it = php_spl::iter::get_iterator(self, &o, by_ref)?;
                it.rewind(self)?;
                let mut index: i64 = 0;
                loop {
                    if !it.valid(self)? {
                        break;
                    }
                    let Some(v) = it.current(self)? else { break };
                    if by_ref {
                        match v {
                            Value::Reference(r) => self.assign_ref_to(value, r)?,
                            other => self.assign_foreach_value(value, other)?,
                        }
                    } else {
                        self.assign_foreach_value(value, v.deref())?;
                    }
                    if let Some(ke) = key {
                        let k = it.key(self)?.unwrap_or(Value::Int(index));
                        self.assign(ke, k)?;
                    }
                    match self.exec(body)? {
                        Flow::Break(n) if n > 1 => return Ok(Flow::Break(n - 1)),
                        Flow::Break(_) => break,
                        Flow::Continue(n) if n > 1 => return Ok(Flow::Continue(n - 1)),
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    it.next(self)?;
                    index += 1;
                }
                Ok(Flow::Normal)
            }
            other => {
                self.emit(
                    ErrorLevel::Warning,
                    &format!(
                        "foreach() argument must be of type array|object, {} given",
                        other.type_name()
                    ),
                )?;
                Ok(Flow::Normal)
            }
        }
    }

    fn assign_foreach_value(&mut self, target: &Expr, v: Value) -> RtResult<()> {
        if let ExprKind::Array { items, .. } = &target.kind {
            return self.destructure(items, &v);
        }
        self.assign(target, v)?;
        Ok(())
    }

    pub fn visible_props(&self, o: &Obj) -> ZArray {
        let t = tobj(o);
        let props = t.props.borrow().clone();
        let scope = self.frames.last().and_then(|f| f.scope.clone());
        let mut out = HashTable::new();
        for (k, v) in props.iter() {
            let ArrayKey::Str(name) = k else {
                out.insert(k.clone(), v.clone());
                continue;
            };
            let (cls, prop) = php_runtime::dump::unmangle_property_name(name.as_bytes());
            let visible = match cls {
                None => true,
                Some(b"*") => scope
                    .as_ref()
                    .is_some_and(|s| t.class.is_a(&s.name) || s.is_a(&t.class.name)),
                Some(c) => scope.as_ref().is_some_and(|s| s.name.as_bytes() == c),
            };
            if visible {
                out.insert(ArrayKey::from_bytes(prop), v.clone());
            }
        }
        ZArray::from_table(out)
    }

    // -----------------------------------------------------------------------
    // Variables
    // -----------------------------------------------------------------------

    pub fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().unwrap()
    }

    pub fn get_var(&mut self, name: &[u8]) -> RtResult<Value> {
        if name == b"this" {
            return Ok(self
                .frame()
                .this
                .clone()
                .map(Value::Object)
                .unwrap_or_default());
        }
        if name == b"GLOBALS" {
            let g: Vec<(Vec<u8>, Value)> = self.frames[0]
                .vars
                .iter()
                .map(|(k, v)| (k.clone(), v.deref()))
                .collect();
            let mut t = HashTable::new();
            for (k, v) in g {
                t.insert(ArrayKey::from_bytes(&k), v);
            }
            return Ok(Value::Array(ZArray::from_table(t)));
        }
        match self.frame().vars.get(name) {
            Some(v) => Ok(v.deref()),
            None => {
                self.emit(
                    ErrorLevel::Warning,
                    &format!("Undefined variable ${}", s(name)),
                )?;
                Ok(Value::Null)
            }
        }
    }

    pub fn set_var(&mut self, name: &[u8], v: Value) {
        let vars = &mut self.frame().vars;
        match vars.get_mut(name) {
            Some(Value::Reference(r)) => {
                r.set(v);
            }
            Some(slot) => *slot = v,
            None => {
                vars.insert(name.to_vec(), v);
            }
        }
    }

    pub fn var_name(&mut self, v: &VarName) -> RtResult<Vec<u8>> {
        match v {
            VarName::Name(id) => Ok(id.name.clone()),
            VarName::Expr(e) => {
                let v = self.eval(e)?;
                Ok(v.to_zstr(self)?.as_bytes().to_vec())
            }
        }
    }

    pub fn member_name(&mut self, m: &MemberName) -> RtResult<String> {
        match m {
            MemberName::Ident(id) => Ok(s(&id.name)),
            MemberName::Expr(e) => {
                let v = self.eval(e)?;
                Ok(v.to_zstr(self)?.to_string_lossy())
            }
        }
    }

    pub fn resolve_class_ref(&mut self, c: &ClassRef) -> RtResult<Rc<Class>> {
        match c {
            ClassRef::Name(n) => self.resolve_class_name(&n.name),
            ClassRef::Expr(e) => {
                let v = self.eval(e)?;
                match v {
                    Value::Object(o) => Ok(tobj(&o).class.clone()),
                    Value::Str(s) => self.lookup_class(s.as_bytes()),
                    _ => Err(rt_err("Cannot use value as class name")),
                }
            }
        }
    }

    pub fn resolve_class_name(&mut self, n: &[u8]) -> RtResult<Rc<Class>> {
        let l = lc(n);
        match l.as_str() {
            "self" => self
                .frame()
                .scope
                .clone()
                .ok_or_else(|| rt_err("Cannot use \"self\" when no class scope is active")),
            "static" => self
                .frame()
                .static_scope
                .clone()
                .ok_or_else(|| rt_err("Cannot use \"static\" when no class scope is active")),
            "parent" => self
                .frame()
                .scope
                .clone()
                .and_then(|c| c.parent.clone())
                .ok_or_else(|| {
                    rt_err("Cannot use \"parent\" when current class scope has no parent")
                }),
            _ => self.lookup_class(n),
        }
    }
}

thread_local! {
    static TRAITS: RefCell<HashMap<String, Rc<ClassDecl>>> = RefCell::new(HashMap::new());
}

fn vis_of(m: Modifiers) -> u8 {
    if m.contains(Modifiers::PRIVATE) {
        2
    } else if m.contains(Modifiers::PROTECTED) {
        1
    } else {
        0
    }
}

pub fn is_exit(e: &RuntimeError) -> bool {
    matches!(e, RuntimeError::Custom { class, .. } if class.as_bytes() == EXIT_MARK.as_bytes())
}

pub fn trace_arg(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::Int(i) => i.to_string(),
        Value::Float(f) => php_runtime::float::double_to_string(*f, 17),
        Value::Str(s) => {
            let b = s.as_bytes();
            if b.len() > 15 {
                format!("'{}...'", String::from_utf8_lossy(&b[..15]))
            } else {
                format!("'{}'", String::from_utf8_lossy(b))
            }
        }
        Value::Array(_) => "Array".into(),
        Value::Object(o) => format!("Object({})", o.class_name().to_string_lossy()),
        Value::Resource(r) => format!("Resource id #{}", r.handle()),
        Value::Reference(r) => trace_arg(&r.get()),
    }
}
