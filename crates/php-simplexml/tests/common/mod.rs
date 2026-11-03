//! A tiny PHP-ish harness for porting `ext/simplexml/tests/*.phpt`: an
//! object store with PHP's handle reuse, a [`RuntimeContext`]/[`SxeHost`]
//! implementation, output capture (including diagnostics formatted like
//! PHP prints them) and an `EXPECTF` matcher.

#![allow(dead_code, clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use php_runtime::{
    dump, CastTarget, ErrorLevel, Obj, ObjectDump, ObjectKind, PhpObject, PropertyPurpose,
    PropertySlot, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use php_simplexml::{CheckKind, FetchMode, Sxe, SxeHost, SxeRef};

thread_local! {
    static NEXT: Cell<u32> = const { Cell::new(1) };
    static FREE: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

fn alloc_handle() -> u32 {
    if let Some(h) = FREE.with(|f| f.borrow_mut().pop()) {
        return h;
    }
    NEXT.with(|n| {
        let h = n.get();
        n.set(h + 1);
        h
    })
}

/// A PHP object of the test VM.
pub struct TObj {
    handle: u32,
    class: ZStr,
    /// SimpleXML state (`None` for plain objects).
    pub sxe: Option<SxeRef>,
}

impl Drop for TObj {
    fn drop(&mut self) {
        let h = self.handle;
        let _ = FREE.try_with(|f| f.borrow_mut().push(h));
    }
}

impl PhpObject for TObj {
    fn handle(&self) -> u32 {
        self.handle
    }
    fn class_name(&self) -> ZStr {
        self.class.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn new_object(class: &str, sxe: Option<SxeRef>) -> Value {
    Value::Object(Obj::new(TObj {
        handle: alloc_handle(),
        class: ZStr::from(class),
        sxe,
    }))
}

/// Output buffer shared with the libxml diagnostic handler.
pub type Out = Rc<RefCell<Vec<u8>>>;

/// The host.
pub struct Host {
    pub out: Out,
    pub func: Rc<RefCell<String>>,
    pub file: String,
}

fn diag_line(out: &Out, func: &str, file: &str, label: &str, msg: &[u8]) {
    let mut o = out.borrow_mut();
    o.extend_from_slice(format!("\n{label}: {func}(): ").as_bytes());
    o.extend_from_slice(msg);
    o.extend_from_slice(format!(" in {file} on line 1\n").as_bytes());
}

impl RuntimeContext for Host {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        let func = self.func.borrow().clone();
        diag_line(&self.out, &func, &self.file, level.label(), message.as_bytes());
        Ok(())
    }

    fn object_properties(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<Option<ZArray>> {
        match self.sxe_unwrap(obj) {
            Some(s) => Sxe::properties(&s, self, purpose == PropertyPurpose::Debug).map(Some),
            None => Ok(Some(ZArray::new())),
        }
    }

    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        let props = self.object_properties(obj, purpose)?;
        Ok(ObjectDump {
            kind: ObjectKind::Regular { lazy_prefix: "" },
            properties: props.map(|t| {
                t.iter()
                    .map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone())))
                    .collect()
            }),
        })
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        match self.sxe_unwrap(obj) {
            Some(s) => Sxe::cast(&s, self, target),
            None => Ok(None),
        }
    }

    fn compare_objects(&mut self, op1: &Value, op2: &Value) -> RtResult<i32> {
        if let (Value::Object(a), Value::Object(b)) = (op1, op2) {
            if Obj::ptr_eq(a, b) {
                return Ok(0);
            }
            if let (Some(x), Some(y)) = (self.sxe_unwrap(a), self.sxe_unwrap(b)) {
                return Sxe::compare(&x, &y);
            }
            return Ok(1);
        }
        // zend_std_compare_objects: object vs scalar.
        let (obj, other, lhs) = match (op1, op2) {
            (Value::Object(o), v) => (o.clone(), v.clone(), true),
            (v, Value::Object(o)) => (o.clone(), v.clone(), false),
            _ => return Ok(1),
        };
        let target = match other {
            Value::Bool(_) => CastTarget::Bool,
            Value::Int(_) => CastTarget::Int,
            Value::Float(_) => CastTarget::Float,
            Value::Null => CastTarget::Null,
            _ => CastTarget::String,
        };
        let Some(casted) = self.cast_object(&obj, target)? else {
            return Ok(if lhs { 1 } else { -1 });
        };
        if lhs {
            php_runtime::ops::compare(&casted, &other, self)
        } else {
            php_runtime::ops::compare(&other, &casted, self)
        }
    }

    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        match self.sxe_unwrap(obj) {
            Some(s) => Sxe::count(&s).map(Some),
            None => Ok(None),
        }
    }
}

impl SxeHost for Host {
    fn sxe_wrap(&mut self, class: &ZStr, sxe: SxeRef) -> RtResult<Value> {
        Ok(new_object(&class.to_string_lossy(), Some(sxe)))
    }
    fn sxe_unwrap(&mut self, obj: &Obj) -> Option<SxeRef> {
        obj.downcast_ref::<TObj>().and_then(|t| t.sxe.clone())
    }
}

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/simplexml/tests")
}

pub fn fixture(name: &str) -> String {
    fixtures().join(name).to_string_lossy().into_owned()
}

/// A test "script".
pub struct T {
    pub h: Host,
    name: String,
}

impl T {
    pub fn new(name: &str) -> T {
        NEXT.with(|n| n.set(1));
        FREE.with(|f| f.borrow_mut().clear());
        php_libxml::error::reset_request_state();
        php_libxml::error::take_diagnostics();
        let out: Out = Rc::new(RefCell::new(Vec::new()));
        let func = Rc::new(RefCell::new("main".to_string()));
        let file = format!("/t/{name}.php");
        let (o, f, fl) = (out.clone(), func.clone(), file.clone());
        php_libxml::error::set_diagnostic_handler(Some(Box::new(move |d| {
            let label = match d.level {
                php_libxml::error::DiagnosticLevel::Warning => "Warning",
                php_libxml::error::DiagnosticLevel::Notice => "Notice",
            };
            diag_line(&o, &f.borrow(), &fl, label, &d.message);
        })));
        T {
            h: Host { out, func, file },
            name: name.to_string(),
        }
    }

    /// Sets the function name used as diagnostic prefix.
    pub fn func(&mut self, f: &str) -> &mut Self {
        *self.h.func.borrow_mut() = f.to_string();
        self
    }

    pub fn echo(&mut self, s: impl AsRef<[u8]>) {
        self.h.out.borrow_mut().extend_from_slice(s.as_ref());
    }

    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.h.out.borrow()).into_owned()
    }

    pub fn var_dump(&mut self, v: &Value) {
        let mut buf = Vec::new();
        match dump::var_dump(&mut buf, v, &mut self.h) {
            Ok(()) => self.echo(&buf),
            Err(e) => {
                self.echo(&buf);
                self.exception(&e);
            }
        }
    }

    pub fn print_r(&mut self, v: &Value) {
        match dump::print_r(v, &mut self.h) {
            Ok(b) => self.echo(&b),
            Err(e) => self.exception(&e),
        }
    }

    pub fn var_export(&mut self, v: &Value) {
        match dump::var_export(v, &mut self.h) {
            Ok(b) => self.echo(&b),
            Err(e) => self.exception(&e),
        }
    }

    /// Prints `Class: message` like the usual `catch` block.
    pub fn exception(&mut self, e: &RuntimeError) {
        let class = e.exception_class().unwrap_or_else(|| "Error".to_string());
        let msg = e.message().unwrap_or("").to_string();
        self.echo(format!("{class}: {msg}\n"));
    }

    /// Unwraps a result, printing a thrown exception instead.
    pub fn catch<V: Default>(&mut self, r: RtResult<V>) -> V {
        match r {
            Ok(v) => v,
            Err(e) => {
                self.exception(&e);
                V::default()
            }
        }
    }

    pub fn sxe(&mut self, v: &Value) -> SxeRef {
        match v {
            Value::Object(o) => self.h.sxe_unwrap(o).expect("not a SimpleXMLElement"),
            other => panic!("not an object: {}", other.type_name()),
        }
    }

    pub fn load_string(&mut self, xml: &str) -> Value {
        self.load_string_ex(xml, None, 0, "", false)
    }

    pub fn load_string_ex(&mut self, xml: &str, class: Option<&str>, options: i64, ns: &str, is_prefix: bool) -> Value {
        let class = class.map(ZStr::from);
        let r = Sxe::load_string(&mut self.h, xml.as_bytes(), class.as_ref(), options, ns.as_bytes(), is_prefix);
        self.catch(r)
    }

    pub fn load_file(&mut self, path: &str) -> Value {
        let r = Sxe::load_file(&mut self.h, path.as_bytes(), None, 0, b"", false);
        self.catch(r)
    }

    /// `new $class($xml, ...)`.
    pub fn new_sxe_ex(&mut self, class: &str, xml: &str, options: i64, is_url: bool, ns: &str, is_prefix: bool) -> RtResult<Value> {
        let sxe = Sxe::new(class);
        Sxe::construct(&sxe, xml.as_bytes(), options, is_url, ns.as_bytes(), is_prefix)?;
        Ok(new_object(class, Some(sxe)))
    }

    pub fn new_sxe(&mut self, xml: &str) -> Value {
        let r = self.new_sxe_ex("SimpleXMLElement", xml, 0, false, "", false);
        self.catch(r)
    }

    pub fn prop(&mut self, v: &Value, name: &str) -> Value {
        self.prop_mode(v, name, FetchMode::Read)
    }

    pub fn prop_mode(&mut self, v: &Value, name: &str, mode: FetchMode) -> Value {
        let s = self.sxe(v);
        let r = Sxe::read_property(&s, &mut self.h, name.as_bytes(), mode);
        self.catch(r)
    }

    /// `$v->a->b->c` (read).
    pub fn path(&mut self, v: &Value, names: &[&str]) -> Value {
        let mut cur = v.clone();
        for n in names {
            if !matches!(cur, Value::Object(_)) {
                return Value::Null;
            }
            cur = self.prop(&cur, n);
        }
        cur
    }

    pub fn dim(&mut self, v: &Value, key: impl Into<Value>) -> Value {
        self.dim_mode(v, Some(key.into()), FetchMode::Read)
    }

    pub fn dim_mode(&mut self, v: &Value, key: Option<Value>, mode: FetchMode) -> Value {
        let s = self.sxe(v);
        let r = Sxe::read_dimension(&s, &mut self.h, key.as_ref(), mode);
        self.catch(r)
    }

    pub fn set_prop(&mut self, v: &Value, name: &str, value: impl Into<Value>) {
        let s = self.sxe(v);
        let r = Sxe::write_property(&s, &mut self.h, name.as_bytes(), &value.into());
        self.catch(r.map(|_| ()));
    }

    pub fn set_dim(&mut self, v: &Value, key: Option<Value>, value: impl Into<Value>) {
        let s = self.sxe(v);
        let r = Sxe::write_dimension(&s, &mut self.h, key.as_ref(), &value.into());
        self.catch(r.map(|_| ()));
    }

    pub fn unset_prop(&mut self, v: &Value, name: &str) {
        let s = self.sxe(v);
        let r = Sxe::unset_property(&s, &mut self.h, name.as_bytes());
        self.catch(r);
    }

    pub fn unset_dim(&mut self, v: &Value, key: impl Into<Value>) {
        let s = self.sxe(v);
        let r = Sxe::unset_dimension(&s, &mut self.h, &key.into());
        self.catch(r);
    }

    pub fn isset_prop(&mut self, v: &Value, name: &str) -> bool {
        let s = self.sxe(v);
        let r = Sxe::has_property(&s, &mut self.h, name.as_bytes(), CheckKind::Isset);
        self.catch(r)
    }

    pub fn empty_prop(&mut self, v: &Value, name: &str) -> bool {
        let s = self.sxe(v);
        let r = Sxe::has_property(&s, &mut self.h, name.as_bytes(), CheckKind::NotEmpty);
        !self.catch(r)
    }

    pub fn isset_dim(&mut self, v: &Value, key: impl Into<Value>) -> bool {
        let s = self.sxe(v);
        let r = Sxe::has_dimension(&s, &mut self.h, &key.into(), CheckKind::Isset);
        self.catch(r)
    }

    pub fn empty_dim(&mut self, v: &Value, key: impl Into<Value>) -> bool {
        let s = self.sxe(v);
        let r = Sxe::has_dimension(&s, &mut self.h, &key.into(), CheckKind::NotEmpty);
        !self.catch(r)
    }

    /// `(string)$v`.
    pub fn str(&mut self, v: &Value) -> String {
        let r = v.to_zstr(&mut self.h);
        let z = self.catch(r.map(Some)).unwrap_or_default();
        z.to_string_lossy()
    }

    /// `(bool)$v`.
    pub fn bool(&mut self, v: &Value) -> bool {
        let r = v.to_bool(&mut self.h);
        self.catch(r)
    }

    /// `(array)$v` (the `get_properties` table).
    pub fn array(&mut self, v: &Value) -> Value {
        let s = self.sxe(v);
        let r = Sxe::properties(&s, &mut self.h, false).map(Value::Array);
        self.catch(r)
    }

    /// `count($v)`.
    pub fn count(&mut self, v: &Value) -> i64 {
        let s = self.sxe(v);
        let r = Sxe::count(&s);
        self.catch(r)
    }

    /// `foreach ($v as $k => $x) body($k, $x)`.
    pub fn foreach(&mut self, v: &Value, mut body: impl FnMut(&mut T, Value, Value)) {
        let s = self.sxe(v);
        // zend_iterator_init: the foreach iterator takes an object handle.
        let _iterator = new_object("InternalIterator", None);
        let r = Sxe::rewind(&s, &mut self.h);
        self.catch(r);
        while Sxe::valid(&s) {
            let cur = Sxe::iter_current(&s).unwrap_or(Value::Null);
            let key = Sxe::iter_key(&s, &mut self.h);
            body(self, key, cur);
            let r = Sxe::next(&s, &mut self.h);
            self.catch(r);
        }
    }

    pub fn children(&mut self, v: &Value, ns: Option<&str>, is_prefix: bool) -> Value {
        let s = self.sxe(v);
        let r = Sxe::children(&s, &mut self.h, ns.map(str::as_bytes), is_prefix);
        self.catch(r)
    }

    pub fn attributes(&mut self, v: &Value, ns: Option<&str>, is_prefix: bool) -> Value {
        let s = self.sxe(v);
        let r = Sxe::attributes(&s, &mut self.h, ns.map(str::as_bytes), is_prefix);
        self.catch(r)
    }

    pub fn xpath(&mut self, v: &Value, q: &str) -> Value {
        let s = self.sxe(v);
        let r = Sxe::xpath(&s, &mut self.h, q.as_bytes());
        self.catch(r)
    }

    pub fn as_xml(&mut self, v: &Value) -> Value {
        let s = self.sxe(v);
        let r = Sxe::as_xml(&s, None);
        self.catch(r)
    }

    pub fn add_child(&mut self, v: &Value, name: &str, value: Option<&str>, ns: Option<&str>) -> Value {
        let s = self.sxe(v);
        self.func("SimpleXMLElement::addChild");
        let r = Sxe::add_child(&s, &mut self.h, name.as_bytes(), value.map(str::as_bytes), ns.map(str::as_bytes));
        self.func("main");
        self.catch(r)
    }

    pub fn add_attribute(&mut self, v: &Value, name: &str, value: &str, ns: Option<&str>) {
        let s = self.sxe(v);
        self.func("SimpleXMLElement::addAttribute");
        let r = Sxe::add_attribute(&s, &mut self.h, name.as_bytes(), value.as_bytes(), ns.map(str::as_bytes));
        self.func("main");
        self.catch(r);
    }

    pub fn get_name(&mut self, v: &Value) -> String {
        let s = self.sxe(v);
        let r = Sxe::get_name(&s);
        String::from_utf8_lossy(&self.catch(r)).into_owned()
    }

    pub fn clone_obj(&mut self, v: &Value) -> Value {
        let s = self.sxe(v);
        let class = s.class().to_string_lossy();
        let r = Sxe::clone_object(&s);
        match self.catch(r.map(Some)) {
            Some(c) => new_object(&class, Some(c)),
            None => Value::Null,
        }
    }

    /// Compares the output with the phpt's EXPECT/EXPECTF section.
    #[track_caller]
    pub fn expect(&self, phpt: &str) {
        let text = std::fs::read_to_string(fixtures().join(phpt)).expect("phpt");
        let (expect, is_f) = section(&text);
        let got = self.output();
        let got = got.trim().to_string();
        let ok = if is_f {
            fmatch(expect.as_bytes(), got.as_bytes())
        } else {
            got == expect
        };
        assert!(ok, "{phpt}: output mismatch\n--- expected ---\n{expect}\n--- got ---\n{got}\n");
    }

    /// Compares with an explicit expectation (EXPECTF syntax).
    #[track_caller]
    pub fn expect_text(&self, expect: &str) {
        let got = self.output();
        let got = got.trim().to_string();
        let expect = expect.trim();
        assert!(
            fmatch(expect.as_bytes(), got.as_bytes()),
            "output mismatch\n--- expected ---\n{expect}\n--- got ---\n{got}\n"
        );
    }
}

impl Drop for T {
    fn drop(&mut self) {
        php_libxml::error::set_diagnostic_handler(None);
        php_libxml::error::reset_request_state();
    }
}

/// The EXPECT or EXPECTF section (trailing whitespace trimmed).
fn section(text: &str) -> (String, bool) {
    let mut cur: Option<(bool, Vec<&str>)> = None;
    let mut found = None;
    for line in text.lines() {
        if line.starts_with("--") && line.ends_with("--") && line.len() > 4 && line[2..line.len() - 2].chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            if let Some((f, lines)) = cur.take() {
                found = Some((lines.join("\n"), f));
            }
            match &line[2..line.len() - 2] {
                "EXPECT" => cur = Some((false, Vec::new())),
                "EXPECTF" => cur = Some((true, Vec::new())),
                _ => {}
            }
            continue;
        }
        if let Some((_, lines)) = cur.as_mut() {
            lines.push(line);
        }
    }
    if let Some((f, lines)) = cur.take() {
        found = Some((lines.join("\n"), f));
    }
    let (s, f) = found.expect("no EXPECT section");
    (s.trim().to_string(), f)
}

/// `run-tests.php`'s EXPECTF matching (subset: %s %S %a %A %d %i %w %x %f %c %e %%).
pub fn fmatch(pat: &[u8], s: &[u8]) -> bool {
    if pat.is_empty() {
        return s.is_empty();
    }
    if pat[0] == b'%' && pat.len() >= 2 {
        let rest = &pat[2..];
        let class: Option<(fn(u8) -> bool, bool)> = match pat[1] {
            b's' => Some((|c| c != b'\n' && c != b'\r', false)),
            b'S' => Some((|c| c != b'\n' && c != b'\r', true)),
            b'a' => Some((|_| true, false)),
            b'A' => Some((|_| true, true)),
            b'w' => Some((|c| c.is_ascii_whitespace(), true)),
            b'd' => Some((|c| c.is_ascii_digit(), false)),
            b'x' => Some((|c| c.is_ascii_hexdigit(), false)),
            b'i' => Some((|c| c.is_ascii_digit() || c == b'-' || c == b'+', false)),
            b'f' => Some((|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E'), false)),
            b'c' => {
                return !s.is_empty() && fmatch(rest, &s[1..]);
            }
            b'e' => {
                return s.first() == Some(&b'/') && fmatch(rest, &s[1..]);
            }
            b'%' => {
                return s.first() == Some(&b'%') && fmatch(rest, &s[1..]);
            }
            _ => None,
        };
        if let Some((ok, allow_empty)) = class {
            let mut n = 0;
            while n < s.len() && ok(s[n]) {
                n += 1;
            }
            let min = usize::from(!allow_empty);
            let mut k = n;
            loop {
                if k < min {
                    return false;
                }
                if fmatch(rest, &s[k..]) {
                    return true;
                }
                if k == 0 {
                    return false;
                }
                k -= 1;
            }
        }
    }
    !s.is_empty() && s[0] == pat[0] && fmatch(&pat[1..], &s[1..])
}

/// `trim()`.
pub fn trim(s: &str) -> String {
    s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0b')).to_string()
}

pub fn s(v: &str) -> Value {
    Value::Str(ZStr::from(v))
}

impl T {
    /// `$v->name` fetched for writing (`FETCH_OBJ_W`): `get_property_ptr_ptr`
    /// with the `read_property(BP_VAR_W)` fallback.
    pub fn prop_w(&mut self, v: &Value, name: &str) -> Value {
        let s = self.sxe(v);
        let r = Sxe::property_ptr(&s, &mut self.h, name.as_bytes());
        match self.catch(r.map(Some)) {
            Some(php_simplexml::PropertyPtr::Value(x)) => x,
            Some(php_simplexml::PropertyPtr::Fallback) => self.prop_mode(v, name, FetchMode::Write),
            _ => Value::Null,
        }
    }

    /// `$v[$k] .= $x` / `+=` etc.: read (R), operate, write back.
    pub fn dim_op(&mut self, v: &Value, key: Value, op: impl FnOnce(&mut T, Value) -> Value) {
        let cur = self.dim(v, key.clone());
        let n = op(self, cur);
        self.set_dim(v, Some(key), n);
    }

    /// `"..." . $v` (string conversion of any value).
    pub fn cat(&mut self, parts: &[&Value]) -> String {
        let mut out = String::new();
        for p in parts {
            out.push_str(&self.str(p));
        }
        out
    }
}

impl php_json::JsonHost for Host {}

impl T {
    /// `json_encode($v)`.
    pub fn json(&mut self, v: &Value) -> String {
        match php_json::encode(v, 0, 512, &mut self.h) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => "false".into(),
        }
    }
}
