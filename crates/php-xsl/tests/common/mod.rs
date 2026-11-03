//! A miniature PHP environment for porting `ext/xsl/tests/*.phpt`.
//!
//! * [`Php`] implements [`XslHost`]: PHP functions are Rust closures
//!   taking [`Value`]s; nodes handed to callbacks become [`TNode`]s.
//! * Output (`echo`, `var_dump`, displayed warnings) goes to one buffer,
//!   so the relative order of callback output and libxslt warnings is the
//!   one PHP produces.
//! * [`expect`] compares the output with the `--EXPECT--`/`--EXPECTF--`
//!   section of the original `.phpt` file.

#![allow(dead_code, clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;

use php_libxml::error::{Diagnostic, DiagnosticLevel};
use php_libxml::ffi::xmlNode;
use php_libxml::tree::{Document, Node, ParseOptions};
use php_xsl::{
    CallbackArg, CallbackContext, CallbackReturn, NodeArg, ReturnedNode, XmlDocument, XmlOutput, XslError,
    XslHost, XsltProcessor,
};

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A detached node copy owned by PHP values.
#[derive(Debug)]
pub struct OwnedCopy(NonNull<xmlNode>);

impl Drop for OwnedCopy {
    fn drop(&mut self) {
        // SAFETY: the copy was released to us with `OwnedNode::into_raw`
        // and nothing else frees it; copies made by the processor are
        // elements/text/attributes, freed with the matching function.
        unsafe {
            let n = self.0.as_ptr();
            match (*n).type_ {
                php_libxml::ffi::XML_ATTRIBUTE_NODE => php_libxml::ffi::xmlFreeProp(n.cast()),
                php_libxml::ffi::XML_DOCUMENT_NODE => php_libxml::ffi::xmlFreeDoc(n.cast()),
                _ => php_libxml::ffi::xmlFreeNode(n),
            }
        }
    }
}

/// A DOM node seen from PHP.
#[derive(Debug, Clone)]
pub enum TNode {
    /// A node of a document kept alive by the test (or a processor copy).
    Raw {
        ptr: NonNull<xmlNode>,
        owner: Option<Rc<OwnedCopy>>,
        keep_doc: Option<Document>,
    },
    /// A `php-libxml` handle.
    Handle(Node),
    /// A `DOMNameSpaceNode`.
    Namespace {
        prefix: Option<Vec<u8>>,
        href: Vec<u8>,
        parent: Box<TNode>,
    },
}

impl TNode {
    pub fn ptr(&self) -> NonNull<xmlNode> {
        match self {
            Self::Raw { ptr, .. } => *ptr,
            Self::Handle(n) => NonNull::new(n.as_ptr()).unwrap(),
            Self::Namespace { parent, .. } => parent.ptr(),
        }
    }

    fn raw(&self) -> &xmlNode {
        // SAFETY: nodes held by test values are alive (owned copies, nodes
        // of documents the test keeps, or handles).
        unsafe { &*self.ptr().as_ptr() }
    }

    /// `textContent` / `value` / `nodeValue`.
    pub fn text(&self) -> Vec<u8> {
        if let Self::Namespace { href, .. } = self {
            return href.clone();
        }
        // SAFETY: see `raw`; the result is a libxml2 allocation.
        unsafe {
            let p = php_libxml::ffi::xmlNodeGetContent(self.ptr().as_ptr());
            if p.is_null() {
                return Vec::new();
            }
            let v = std::ffi::CStr::from_ptr(p.cast()).to_bytes().to_vec();
            if let Some(free) = php_libxml::ffi::xmlFree {
                free(p.cast());
            }
            v
        }
    }

    pub fn set_text(&self, s: &[u8]) {
        let c = std::ffi::CString::new(s).unwrap();
        // SAFETY: live node; content is copied.
        unsafe { php_libxml::ffi::xmlNodeSetContent(self.ptr().as_ptr(), c.as_ptr().cast()) };
    }

    /// `getNodePath()`.
    pub fn path(&self) -> Vec<u8> {
        // SAFETY: see `text`.
        unsafe {
            let p = php_libxml::ffi::xmlGetNodePath(self.ptr().as_ptr());
            if p.is_null() {
                return Vec::new();
            }
            let v = std::ffi::CStr::from_ptr(p.cast()).to_bytes().to_vec();
            if let Some(free) = php_libxml::ffi::xmlFree {
                free(p.cast());
            }
            v
        }
    }

    /// `nodeName`/`localName`.
    pub fn name(&self) -> Vec<u8> {
        if let Self::Namespace { prefix, .. } = self {
            return prefix.clone().unwrap_or_default();
        }
        let n = self.raw();
        if n.name.is_null() {
            return Vec::new();
        }
        // SAFETY: node names are valid C strings.
        unsafe { std::ffi::CStr::from_ptr(n.name.cast()) }.to_bytes().to_vec()
    }

    /// `parentNode` (for namespace nodes: the element).
    pub fn parent_name(&self) -> Vec<u8> {
        match self {
            Self::Namespace { parent, .. } => parent.name(),
            _ => {
                let p = self.raw().parent;
                if p.is_null() {
                    return Vec::new();
                }
                // SAFETY: the parent is alive with the node.
                unsafe { std::ffi::CStr::from_ptr((*p).name.cast()) }.to_bytes().to_vec()
            }
        }
    }

    pub fn is_namespace(&self) -> bool {
        matches!(self, Self::Namespace { .. })
    }
}

/// A PHP value.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Array(Vec<Value>),
    Node(TNode),
    /// A non-DOM object with an optional `__toString` result.
    Object(&'static str, Option<Vec<u8>>),
}

impl Value {
    pub fn s(s: impl AsRef<[u8]>) -> Self {
        Self::Str(s.as_ref().to_vec())
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            Self::Null | Self::Bool(false) => Vec::new(),
            Self::Bool(true) => b"1".to_vec(),
            Self::Int(i) => i.to_string().into_bytes(),
            Self::Float(f) => php_float(*f).into_bytes(),
            Self::Str(s) => s.clone(),
            Self::Array(_) => b"Array".to_vec(),
            Self::Node(n) => n.text(),
            Self::Object(_, s) => s.clone().unwrap_or_default(),
        }
    }

    pub fn nodes(&self) -> Vec<TNode> {
        match self {
            Self::Array(a) => a
                .iter()
                .filter_map(|v| match v {
                    Self::Node(n) => Some(n.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

pub fn php_float(f: f64) -> String {
    if f.is_nan() {
        "NAN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "INF".into() } else { "-INF".into() }
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        format!("{f}")
    }
}

/// A thrown exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exc {
    pub class: String,
    pub message: String,
}

impl Exc {
    pub fn new(class: &str, message: impl Into<String>) -> Self {
        Self {
            class: class.into(),
            message: message.into(),
        }
    }
}

impl From<XslError> for Exc {
    fn from(e: XslError) -> Self {
        Self::new(e.class().name(), e.message())
    }
}

pub type PhpFn = dyn Fn(&mut Php, Vec<Value>) -> Result<Value, Exc>;
pub type Callable = Rc<PhpFn>;

// ---------------------------------------------------------------------------
// Output and diagnostics
// ---------------------------------------------------------------------------

thread_local! {
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static METHOD: RefCell<String> = const { RefCell::new(String::new()) };
    static EXCEPTION: RefCell<Option<Exc>> = const { RefCell::new(None) };
    static SILENCE: RefCell<bool> = const { RefCell::new(false) };
}

pub fn echo(s: impl AsRef<[u8]>) {
    OUT.with(|o| o.borrow_mut().extend_from_slice(s.as_ref()));
}

pub fn take_output() -> Vec<u8> {
    OUT.with(|o| std::mem::take(&mut *o.borrow_mut()))
}

fn display(level: &str, msg: &[u8]) {
    if SILENCE.with(|s| *s.borrow()) || EXCEPTION.with(|e| e.borrow().is_some()) {
        return;
    }
    let method = METHOD.with(|m| m.borrow().clone());
    let mut line = format!("\n{level}: ");
    if !method.is_empty() {
        line.push_str(&method);
        line.push_str("(): ");
    }
    echo(line);
    echo(msg);
    echo(" in %s on line %d\n");
}

/// Runs `f` as the PHP method `method` (warnings are prefixed with it).
pub fn in_method<R>(method: &str, f: impl FnOnce() -> R) -> R {
    let prev = METHOD.with(|m| std::mem::replace(&mut *m.borrow_mut(), method.to_string()));
    let r = f();
    METHOD.with(|m| *m.borrow_mut() = prev);
    r
}

/// The `@` operator.
pub fn silenced<R>(f: impl FnOnce() -> R) -> R {
    let prev = SILENCE.with(|s| std::mem::replace(&mut *s.borrow_mut(), true));
    let r = f();
    SILENCE.with(|s| *s.borrow_mut() = prev);
    r
}

/// Takes the pending exception (a `catch`).
pub fn take_exception() -> Option<Exc> {
    EXCEPTION.with(|e| e.borrow_mut().take())
}

pub fn throw(e: Exc) {
    EXCEPTION.with(|x| {
        let mut x = x.borrow_mut();
        if x.is_none() {
            *x = Some(e);
        }
    });
}

pub fn exception_pending() -> bool {
    EXCEPTION.with(|e| e.borrow().is_some())
}

/// Resets per-test global state and installs the diagnostic handler and
/// stream hooks resolving relative paths against `base`.
pub fn setup(base: &Path) {
    php_libxml::error::reset_request_state();
    take_output();
    take_exception();
    php_libxml::error::set_diagnostic_handler(Some(Box::new(|d: Diagnostic| {
        let level = match d.level {
            DiagnosticLevel::Warning => "Warning",
            DiagnosticLevel::Notice => "Notice",
        };
        display(level, &d.message);
    })));
    let base_read = base.to_path_buf();
    let base_write = base.to_path_buf();
    php_libxml::io::set_stream_hooks(Some(php_libxml::io::StreamHooks {
        open_read: Some(Box::new(move |path: &[u8]| open_read(&base_read, path))),
        open_write: Some(Box::new(move |path: &[u8]| open_write(&base_write, path))),
    }));
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn strip_file_scheme(s: &str) -> &str {
    s.strip_prefix("file://").unwrap_or(s)
}

fn open_read(base: &Path, path: &[u8]) -> Option<php_libxml::io::InputStream> {
    let s = String::from_utf8_lossy(path).into_owned();
    let reader: Box<dyn Read> = if let Some(rest) = s.strip_prefix("compress.zlib://") {
        let file = resolve(base, strip_file_scheme(rest));
        let out = std::process::Command::new("gzip").arg("-dc").arg(file).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Box::new(std::io::Cursor::new(out.stdout))
    } else {
        let file = resolve(base, strip_file_scheme(&s));
        Box::new(std::fs::File::open(file).ok()?)
    };
    Some(php_libxml::io::InputStream {
        reader,
        charset: None,
    })
}

fn open_write(base: &Path, path: &[u8]) -> Option<Box<dyn std::io::Write>> {
    let s = String::from_utf8_lossy(path).into_owned();
    if s == "php://output" {
        return Some(Box::new(EchoWriter));
    }
    let file = resolve(base, strip_file_scheme(&s));
    Some(Box::new(std::fs::File::create(file).ok()?))
}

struct EchoWriter;

impl std::io::Write for EchoWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        echo(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

/// The PHP engine stand-in.
#[derive(Default)]
pub struct Php {
    functions: HashMap<String, Callable>,
    /// `php_dom_xpath_callbacks::node_list`.
    pub node_list: Vec<Value>,
    pub finished: usize,
}

impl Php {
    pub fn new() -> Self {
        let mut php = Self::default();
        php.define("ucwords", |_, a| Ok(Value::Str(ucwords(&arg(&a, 0)))));
        php.define("ucfirst", |_, a| {
            let mut s = arg(&a, 0);
            if let Some(c) = s.first_mut() {
                c.make_ascii_uppercase();
            }
            Ok(Value::Str(s))
        });
        php.define("strtoupper", |_, a| Ok(Value::Str(arg(&a, 0).to_ascii_uppercase())));
        php.define("strtolower", |_, a| Ok(Value::Str(arg(&a, 0).to_ascii_lowercase())));
        php.define("strrev", |_, a| {
            let mut s = arg(&a, 0);
            s.reverse();
            Ok(Value::Str(s))
        });
        php.define("strpos", |_, _| Ok(Value::Bool(false)));
        php.define("array_key_exists", |_, _| Ok(Value::Bool(false)));
        php.define("var_dump", |_, a| {
            for v in &a {
                var_dump(v);
            }
            Ok(Value::Null)
        });
        php
    }

    pub fn define(&mut self, name: &str, f: impl Fn(&mut Php, Vec<Value>) -> Result<Value, Exc> + 'static) {
        self.functions.insert(name.to_ascii_lowercase(), Rc::new(f));
    }

    /// `zend_is_callable` on a string.
    pub fn resolve(&self, name: &[u8]) -> Result<Callable, String> {
        let n = String::from_utf8_lossy(name).to_ascii_lowercase();
        self.functions.get(&n).cloned().ok_or_else(|| {
            format!(
                "function \"{}\" not found or invalid function name",
                String::from_utf8_lossy(name)
            )
        })
    }

    fn run(&mut self, f: &Callable, args: Vec<Value>) -> Value {
        match f(self, args) {
            Ok(v) => v,
            Err(e) => {
                throw(e);
                Value::Null
            }
        }
    }

    fn convert_args(&mut self, ctx: &CallbackContext<'_>, args: Vec<CallbackArg<'_>>) -> Vec<Value> {
        let keep_doc = ctx.document_copy.cloned();
        args.into_iter()
            .map(|a| match a {
                CallbackArg::Str(s) => Value::Str(s),
                CallbackArg::Bool(b) => Value::Bool(b),
                CallbackArg::Float(f) => Value::Float(f),
                CallbackArg::Nodes(nodes) => {
                    let arr = Value::Array(
                        nodes
                            .into_iter()
                            .map(|n| Value::Node(convert_node(n, keep_doc.as_ref())))
                            .collect(),
                    );
                    self.node_list.push(arr.clone());
                    arr
                }
            })
            .collect()
    }

    fn convert_return(&mut self, v: Value) -> CallbackReturn {
        if exception_pending() {
            return CallbackReturn::Undef;
        }
        match v {
            Value::Node(n) => {
                self.node_list.push(Value::Node(n.clone()));
                let r = match &n {
                    TNode::Handle(h) => ReturnedNode::from_node(h.clone()),
                    // SAFETY: the node is kept alive by `node_list` until
                    // `transformation_finished`.
                    _ => unsafe { ReturnedNode::from_raw(n.ptr()) },
                };
                CallbackReturn::Node(r)
            }
            Value::Bool(b) => CallbackReturn::Bool(b),
            Value::Object(..) => CallbackReturn::NonNodeObject,
            Value::Array(_) => {
                display("Warning", b"Array to string conversion");
                CallbackReturn::Str(b"Array".to_vec())
            }
            other => CallbackReturn::Str(other.as_bytes()),
        }
    }
}

fn convert_node(n: NodeArg<'_>, keep_doc: Option<&Document>) -> TNode {
    match n {
        NodeArg::Node(r) => TNode::Raw {
            ptr: NonNull::new(r.as_ptr()).unwrap(),
            owner: None,
            keep_doc: keep_doc.cloned(),
        },
        NodeArg::Copy(c) => {
            let ptr = c.into_raw();
            TNode::Raw {
                ptr,
                owner: Some(Rc::new(OwnedCopy(ptr))),
                keep_doc: keep_doc.cloned(),
            }
        }
        NodeArg::Namespace { parent, prefix, href } => TNode::Namespace {
            prefix,
            href,
            parent: Box::new(convert_node(*parent, keep_doc)),
        },
    }
}

pub fn arg(args: &[Value], i: usize) -> Vec<u8> {
    args.get(i).map(Value::as_bytes).unwrap_or_default()
}

fn ucwords(s: &[u8]) -> Vec<u8> {
    let mut out = s.to_vec();
    let mut start = true;
    for c in &mut out {
        if start {
            c.make_ascii_uppercase();
        }
        start = matches!(*c, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c);
    }
    out
}

pub fn var_dump(v: &Value) {
    var_dump_indent(v, 0);
}

fn var_dump_indent(v: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match v {
        Value::Null => echo(format!("{pad}NULL\n")),
        Value::Bool(b) => echo(format!("{pad}bool({b})\n")),
        Value::Int(i) => echo(format!("{pad}int({i})\n")),
        Value::Float(f) => echo(format!("{pad}float({})\n", php_float(*f))),
        Value::Str(s) => {
            echo(format!("{pad}string({}) \"", s.len()));
            echo(s);
            echo("\"\n");
        }
        Value::Array(a) => {
            echo(format!("{pad}array({}) {{\n", a.len()));
            for (i, item) in a.iter().enumerate() {
                echo(format!("{pad}  [{i}]=>\n"));
                var_dump_indent(item, indent + 2);
            }
            echo(format!("{pad}}}\n"));
        }
        Value::Node(_) => echo(format!("{pad}object(DOMElement)#1 (0) {{\n{pad}}}\n")),
        Value::Object(c, _) => echo(format!("{pad}object({c})#1 (0) {{\n{pad}}}\n")),
    }
}

impl XslHost<Callable> for Php {
    fn call_by_name(&mut self, ctx: &CallbackContext<'_>, args: Vec<CallbackArg<'_>>) -> CallbackReturn {
        let args = self.convert_args(ctx, args);
        match self.resolve(ctx.function) {
            Ok(f) => {
                let v = self.run(&f, args);
                self.convert_return(v)
            }
            Err(e) => {
                let name = String::from_utf8_lossy(ctx.function).into_owned();
                if let Some((class, _)) = name.split_once("::") {
                    // Autoload hook stand-in.
                    if let Ok(auto) = self.resolve(b"__autoload") {
                        self.run(&auto, vec![Value::s(class)]);
                        if exception_pending() {
                            return CallbackReturn::Undef;
                        }
                    }
                    throw(Exc::new(
                        "Error",
                        format!("Invalid callback {name}, class \"{class}\" not found"),
                    ));
                } else {
                    throw(Exc::new("Error", format!("Invalid callback {name}, {e}")));
                }
                CallbackReturn::Undef
            }
        }
    }

    fn call(&mut self, callable: &Callable, ctx: &CallbackContext<'_>, args: Vec<CallbackArg<'_>>) -> CallbackReturn {
        let args = self.convert_args(ctx, args);
        let v = self.run(callable, args);
        self.convert_return(v)
    }

    fn throw(&mut self, error: XslError) {
        throw(error.into());
    }

    fn exception_pending(&self) -> bool {
        exception_pending()
    }

    fn warning(&mut self, message: &str) {
        display("Warning", message.as_bytes());
    }

    fn transformation_finished(&mut self) {
        self.finished += 1;
        self.node_list.clear();
    }
}

// ---------------------------------------------------------------------------
// Documents and processor helpers
// ---------------------------------------------------------------------------

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/xsl/tests")
}

pub fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// A scratch directory for files the tests write.
pub fn scratch() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("php-xsl-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `DOMDocument::load()`.
pub fn load(path: &Path) -> Document {
    in_method("DOMDocument::load", || {
        Document::parse_file(path.to_string_lossy().as_bytes(), &ParseOptions::default()).expect("load")
    })
}

/// `DOMDocument::loadXML()`.
pub fn load_xml(xml: &str) -> Document {
    in_method("DOMDocument::loadXML", || {
        Document::parse_memory(xml.as_bytes(), &ParseOptions::default()).expect("loadXML")
    })
}

/// `DOMDocument::saveXML()`.
pub fn save_xml(doc: &Document) -> Vec<u8> {
    doc.dump(false, None)
}

pub type Proc = XsltProcessor<Callable>;

pub fn import(proc: &Proc, doc: &dyn XmlDocument) -> bool {
    in_method("XSLTProcessor::importStylesheet", || {
        proc.import_stylesheet(doc).unwrap_or_else(|e| {
            throw(e.into());
            false
        })
    })
}

/// `print $proc->transformToXml($doc)` (false/null print nothing).
pub fn to_xml(proc: &Proc, php: &mut Php, doc: &dyn XmlDocument) -> Option<Vec<u8>> {
    in_method("XSLTProcessor::transformToXml", || match proc.transform_to_xml(doc, php) {
        Ok(XmlOutput::Str(s)) => Some(s),
        Ok(_) => None,
        Err(e) => {
            throw(e.into());
            None
        }
    })
}

pub fn to_xml_raw(proc: &Proc, php: &mut Php, doc: &dyn XmlDocument) -> Result<XmlOutput, XslError> {
    in_method("XSLTProcessor::transformToXml", || proc.transform_to_xml(doc, php))
}

pub fn to_doc(proc: &Proc, php: &mut Php, doc: &dyn XmlDocument) -> Option<Document> {
    in_method("XSLTProcessor::transformToDoc", || match proc.transform_to_doc(doc, php) {
        Ok(d) => d.map(php_xsl::ResultDocument::into_document),
        Err(e) => {
            throw(e.into());
            None
        }
    })
}

/// Sets an attribute of the first `xsl:output` element of a stylesheet
/// (the tests' DOMXPath edits).
pub fn set_output_attr(xsl: &Document, attr: &str, value: &str) {
    let root = xsl.root().unwrap();
    let output = root
        .children()
        .into_iter()
        .find(|n| n.name() == b"output")
        .expect("xsl:output");
    output.set_attribute(attr.as_bytes(), value.as_bytes()).unwrap();
}

/// `var_dump()` of a `transformToXml()` result.
pub fn dump_output(r: &Result<XmlOutput, XslError>) {
    match r {
        Ok(XmlOutput::Str(s)) => var_dump(&Value::Str(s.clone())),
        Ok(XmlOutput::Null) => var_dump(&Value::Null),
        Ok(XmlOutput::False) => var_dump(&Value::Bool(false)),
        Err(_) => {}
    }
}

/// Echoes `$e->getMessage() . "\n"` for a pending exception.
pub fn catch_message() {
    if let Some(e) = take_exception() {
        echo(e.message);
        echo("\n");
    }
}

// ---------------------------------------------------------------------------
// Expectations
// ---------------------------------------------------------------------------

/// Matches run-tests' EXPECTF syntax (`%s`, `%d`, `%i`, `%a`, `%A`,
/// `%w`, `%c`, `%%`).
pub fn expectf_match(pattern: &[u8], text: &[u8]) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == b'%' && p.len() >= 2 {
            let rest = &p[2..];
            let (min, class): (usize, fn(u8) -> bool) = match p[1] {
                b's' => (1, |c| c != b'\n' && c != b'\r'),
                b'S' => (0, |c| c != b'\n' && c != b'\r'),
                b'a' => (1, |_| true),
                b'A' => (0, |_| true),
                b'w' => (0, |c| c.is_ascii_whitespace()),
                b'd' => (1, |c| c.is_ascii_digit()),
                b'i' => (1, |c| c.is_ascii_digit() || c == b'-' || c == b'+'),
                b'x' => (1, |c| c.is_ascii_hexdigit()),
                b'f' => (1, |c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E')),
                b'c' => {
                    return !t.is_empty() && m(rest, &t[1..]);
                }
                b'%' => {
                    return t.first() == Some(&b'%') && m(rest, &t[1..]);
                }
                _ => return t.first() == Some(&b'%') && m(&p[1..], &t[1..]),
            };
            let mut n = 0;
            while n < t.len() && class(t[n]) {
                n += 1;
            }
            // Longest match first (greedy), backtracking.
            let mut k = n;
            loop {
                if k >= min && m(rest, &t[k..]) {
                    return true;
                }
                if k == 0 {
                    return false;
                }
                k -= 1;
            }
        }
        !t.is_empty() && t[0] == p[0] && m(&p[1..], &t[1..])
    }
    m(pattern, text)
}

fn trim(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(*c, b' ' | b'\t' | b'\n' | b'\r' | 0 | 0x0b);
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map_or(start, |e| e + 1);
    &b[start..end]
}

/// Compares the output with the `.phpt`'s expectation.
pub fn expect(phpt: &str, output: &[u8]) {
    let raw_text = std::fs::read(fixture(phpt)).expect("phpt file");
    let (expected, is_f) = match raw_section(&raw_text, "EXPECT") {
        Some(e) => (e, false),
        None => (raw_section(&raw_text, "EXPECTF").expect("expectation"), true),
    };
    let exp = trim(&expected);
    let out = trim(output);
    let ok = if is_f { expectf_match(exp, out) } else { exp == out };
    assert!(
        ok,
        "{phpt}: output mismatch\n--- expected ---\n{}\n--- actual ---\n{}\n",
        String::from_utf8_lossy(exp),
        String::from_utf8_lossy(out)
    );
}

fn raw_section(text: &[u8], name: &str) -> Option<Vec<u8>> {
    let marker = format!("--{name}--\n");
    let m = marker.as_bytes();
    let start = text.windows(m.len()).position(|w| w == m)? + m.len();
    let rest = &text[start..];
    let mut end = rest.len();
    let mut i = 0;
    while i + 3 <= rest.len() {
        if rest[i] == b'\n' && rest[i + 1] == b'-' && rest[i + 2] == b'-' {
            let after = &rest[i + 3..];
            if let Some(pos) = after.windows(2).position(|w| w == b"--") {
                let n = &after[..pos];
                if !n.is_empty() && n.iter().all(|c| c.is_ascii_uppercase() || *c == b'_') {
                    end = i + 1;
                    break;
                }
            }
        }
        i += 1;
    }
    Some(rest[..end].to_vec())
}
