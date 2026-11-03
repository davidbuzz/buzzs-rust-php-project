//! The `Throwable` hierarchy (`Zend/zend_exceptions.c`) and backtraces
//! (`zend_fetch_debug_backtrace`).

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, InternalKind};
use crate::func::{flags, Args, Func};
use crate::object;
use crate::vm::{FrameKind, Throw, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, Value, ZArray, ZStr};

const PUB: u32 = flags::PUBLIC;
const PROT: u32 = flags::PROTECTED;
const PRIV: u32 = flags::PRIVATE;

/// Sets `prev` as the innermost `previous` of `exc`'s chain.
pub(crate) fn chain_previous(exc: &Obj, prev: &Obj) {
    if Obj::ptr_eq(exc, prev) {
        return;
    }
    // Walk to the end of exc's chain.
    let mut cur = exc.clone();
    for _ in 0..10000 {
        let next = get_base_prop(&cur, "previous");
        match next {
            Some(Value::Object(n)) => {
                if Obj::ptr_eq(&n, prev) {
                    return;
                }
                cur = n;
            }
            _ => {
                set_base_prop(&cur, "previous", Value::Object(prev.clone()));
                return;
            }
        }
    }
}

fn base_slot(o: &Obj, name: &str) -> Option<usize> {
    let cls = object::class_of(o);
    cls.props
        .iter()
        .find(|p| {
            p.name.as_bytes() == name.as_bytes()
                && (p.class.as_bytes() == b"Exception" || p.class.as_bytes() == b"Error" || p.class.as_bytes() == b"ErrorException")
        })
        .or_else(|| cls.props.iter().find(|p| p.name.as_bytes() == name.as_bytes()))
        .map(|p| p.slot as usize)
}

/// Reads a property of the exception base class.
pub(crate) fn get_base_prop(o: &Obj, name: &str) -> Option<Value> {
    let slot = base_slot(o, name)?;
    object::data(o).slots.borrow().get(slot).cloned().flatten().map(|v| v.deref())
}

/// Writes a property of the exception base class.
pub(crate) fn set_base_prop(o: &Obj, name: &str, v: Value) {
    if let Some(slot) = base_slot(o, name) {
        let d = object::data(o);
        let mut s = d.slots.borrow_mut();
        if slot < s.len() {
            s[slot] = Some(v);
        }
    }
}

impl Vm {
    /// Reads a base exception property.
    pub(crate) fn exc_prop(&self, o: &Obj, name: &str) -> Value {
        get_base_prop(o, name).unwrap_or_default()
    }

    /// Writes a base exception property.
    pub(crate) fn set_exc_prop(&self, o: &Obj, name: &str, v: Value) {
        set_base_prop(o, name, v);
    }

    /// Creates an exception of an internal (or user) class.
    pub fn create_exception(&mut self, class: &str, msg: &str, code: i64) -> VmResult<Obj> {
        let cls = self
            .find_class(class.as_bytes())
            .or_else(|| self.find_class(b"Error"))
            .expect("Error class");
        let o = self.instantiate_unchecked(&cls)?;
        set_base_prop(&o, "message", Value::Str(ZStr::from(msg)));
        if code != 0 {
            set_base_prop(&o, "code", Value::Int(code));
        }
        Ok(o)
    }

    /// Throws an exception of the given class.
    pub(crate) fn throw_error(&mut self, class: &str, msg: &str) -> Throw {
        self.make_throw(class, msg)
    }

    /// Initializes file/line/trace of a new throwable.
    pub(crate) fn init_throwable(&mut self, o: &Obj) -> VmResult<()> {
        let (file, line) = self.current_location();
        let ignore_args = self.ini.get_bool("zend.exception_ignore_args");
        let trace = self.backtrace(false, ignore_args, false, 0);
        set_base_prop(o, "file", Value::Str(file));
        set_base_prop(o, "line", Value::Int(i64::from(line)));
        set_base_prop(o, "trace", Value::Array(trace));
        Ok(())
    }

    /// Builds a backtrace array (`zend_fetch_debug_backtrace`).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn backtrace(&mut self, skip_last: bool, ignore_args: bool, provide_object: bool, limit: usize) -> ZArray {
        let mut out = HashTable::new();
        let n = self.frames.len();
        if n == 0 {
            return ZArray::new();
        }
        let mut idx = n - 1;
        if skip_last {
            if idx == 0 {
                return ZArray::new();
            }
            idx -= 1;
        }
        let mut count = 0;
        loop {
            if limit != 0 && count >= limit {
                break;
            }
            let f = &self.frames[idx];
            if idx == 0 {
                break;
            }
            if f.kind == FrameKind::ConstExpr {
                if idx == 0 {
                    break;
                }
                idx -= 1;
                continue;
            }
            let prev = &self.frames[idx - 1];
            let mut entry = HashTable::new();
            // File/line of the call site.
            if prev.kind != FrameKind::Builtin {
                entry.insert(ArrayKey::Str(ZStr::from("file")), Value::Str(prev.code.filename.clone()));
                entry.insert(ArrayKey::Str(ZStr::from("line")), Value::Int(i64::from(prev.line())));
            }
            match f.kind {
                FrameKind::Include | FrameKind::Eval => {
                    let fname = if f.kind == FrameKind::Eval {
                        "eval"
                    } else {
                        f.builtin_name.as_ref().map_or("include", |n| match n.as_bytes() {
                            b"require" => "require",
                            b"include_once" => "include_once",
                            b"require_once" => "require_once",
                            _ => "include",
                        })
                    };
                    if f.kind == FrameKind::Include && !ignore_args {
                        entry.insert(
                            ArrayKey::Str(ZStr::from("args")),
                            Value::Array(ZArray::from_values([Value::Str(f.code.filename.clone())])),
                        );
                    }
                    entry.insert(ArrayKey::Str(ZStr::from("function")), Value::Str(ZStr::from(fname)));
                    // Move "function" before "args" as PHP does.
                    let mut e2 = HashTable::new();
                    for k in ["file", "line", "function", "args"] {
                        if let Some(v) = entry.get(&ArrayKey::Str(ZStr::from(k))) {
                            e2.insert(ArrayKey::Str(ZStr::from(k)), v.clone());
                        }
                    }
                    entry = e2;
                }
                FrameKind::Main => break,
                _ => {
                    let Some(func) = &f.func else {
                        idx -= 1;
                        continue;
                    };
                    let fname = match &f.magic_name {
                        Some(m) => m.clone(),
                        None => func.name().clone(),
                    };
                    entry.insert(ArrayKey::Str(ZStr::from("function")), Value::Str(fname));
                    let scope_name = match func {
                        Func::User(u) if u.is_closure() => f.scope.as_ref().map(|s| s.name.clone()),
                        _ => f.scope.as_ref().map(|s| s.name.clone()),
                    };
                    if let Some(this) = &f.this {
                        let cn = scope_name.unwrap_or_else(|| object::class_of(this).name.clone());
                        entry.insert(ArrayKey::Str(ZStr::from("class")), Value::Str(cn));
                        if provide_object {
                            entry.insert(ArrayKey::Str(ZStr::from("object")), Value::Object(this.clone()));
                        }
                        entry.insert(ArrayKey::Str(ZStr::from("type")), Value::Str(ZStr::from("->")));
                    } else if let Some(cn) = scope_name {
                        entry.insert(ArrayKey::Str(ZStr::from("class")), Value::Str(cn));
                        entry.insert(ArrayKey::Str(ZStr::from("type")), Value::Str(ZStr::from("::")));
                    }
                    if !ignore_args {
                        let args = self.frame_args(idx);
                        entry.insert(ArrayKey::Str(ZStr::from("args")), Value::Array(args));
                    }
                }
            }
            let _ = out.push(Value::Array(ZArray::from_table(entry)));
            count += 1;
            idx -= 1;
        }
        ZArray::from_table(out)
    }

    /// The arguments of frame `idx` (current values for user functions).
    pub(crate) fn frame_args(&self, idx: usize) -> ZArray {
        let f = &self.frames[idx];
        let mut t = HashTable::new();
        match f.kind {
            FrameKind::Builtin => {
                for a in f.args.iter().flatten() {
                    let _ = t.push(a.deref());
                }
            }
            _ => {
                if let Some(uf) = &f.ufunc {
                    if f.magic_name.is_some() {
                        // __call frames show the original args.
                        if let Some(Some(Value::Array(a))) = f.args.get(1) {
                            for (_, v) in a.iter() {
                                let _ = t.push(v.deref());
                            }
                        }
                        return ZArray::from_table(t);
                    }
                    let vars = f.vars.borrow();
                    let nparams = uf.params.len();
                    let passed = f.args.len();
                    for (i, p) in uf.params.iter().enumerate() {
                        if p.variadic {
                            if let Some(Some(Value::Array(a))) = vars.slots.get(f.cv_idx(u32::try_from(i).unwrap_or(0))) {
                                for (k, v) in a.iter() {
                                    match k {
                                        ArrayKey::Int(_) => {
                                            let _ = t.push(v.deref());
                                        }
                                        ArrayKey::Str(_) => {
                                            t.insert(k.clone(), v.deref());
                                        }
                                    }
                                }
                            }
                            break;
                        }
                        if i >= passed && i >= nparams {
                            break;
                        }
                        if i >= passed {
                            break;
                        }
                        let v = vars
                            .slots
                            .get(f.cv_idx(u32::try_from(i).unwrap_or(0)))
                            .cloned()
                            .flatten()
                            .map(|v| v.deref())
                            .unwrap_or_default();
                        let v = if p.sensitive {
                            Value::Str(ZStr::from("[SensitiveParameterValue]"))
                        } else {
                            v
                        };
                        let _ = t.push(v);
                    }
                    // Extra args beyond the declared parameters.
                    if !uf.params.last().is_some_and(|p| p.variadic) {
                        for a in f.args.iter().skip(nparams).flatten() {
                            let _ = t.push(a.deref());
                        }
                    }
                }
            }
        }
        ZArray::from_table(t)
    }

    /// `zend_trace_to_string`.
    pub(crate) fn trace_to_string(&mut self, trace: &ZArray, include_main: bool) -> String {
        let mut s = String::new();
        let mut num = 0;
        for (_, frame) in trace.iter() {
            let Value::Array(fr) = frame.deref() else { continue };
            s.push_str(&format!("#{num} "));
            num += 1;
            let get = |k: &str| fr.get(&ArrayKey::Str(ZStr::from(k))).map(Value::deref);
            match get("file") {
                Some(Value::Str(file)) => {
                    let line = get("line").and_then(|l| l.as_int()).unwrap_or(0);
                    s.push_str(&format!("{}({line}): ", file.to_string_lossy()));
                }
                _ => s.push_str("[internal function]: "),
            }
            if let Some(Value::Str(c)) = get("class") {
                s.push_str(&object::display_name(&c).to_string_lossy());
            }
            if let Some(Value::Str(t)) = get("type") {
                s.push_str(&t.to_string_lossy());
            }
            if let Some(Value::Str(f)) = get("function") {
                s.push_str(&f.to_string_lossy());
            }
            s.push('(');
            if let Some(Value::Array(args)) = get("args") {
                let mut first = true;
                for (k, a) in args.iter() {
                    if !first {
                        s.push_str(", ");
                    }
                    first = false;
                    if let ArrayKey::Str(name) = k {
                        s.push_str(&name.to_string_lossy());
                        s.push_str(": ");
                    }
                    s.push_str(&self.trace_arg(&a.deref()));
                }
            }
            s.push_str(")\n");
        }
        if include_main {
            s.push_str(&format!("#{num} {{main}}"));
        }
        s
    }

    fn trace_arg(&mut self, v: &Value) -> String {
        let maxlen = usize::try_from(self.ini.get_long("zend.exception_string_param_max_len")).unwrap_or(15);
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(true) => "true".into(),
            Value::Bool(false) => "false".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                let mut s = php_runtime::float::zend_gcvt(*f, i32::try_from(self.precision).unwrap_or(14).max(1), b'.', b'E');
                if f.is_finite() && !s.contains('.') {
                    s.push_str(".0");
                }
                s
            }
            Value::Str(st) => {
                let b = st.as_bytes();
                let n = b.len().min(maxlen);
                let mut s = String::from("'");
                s.push_str(&escape_bytes(&b[..n]));
                if b.len() > maxlen {
                    s.push_str("...");
                }
                s.push('\'');
                s
            }
            Value::Array(_) => "Array".into(),
            Value::Object(o) => {
                let cls = object::class_of(o);
                if cls.is_enum() {
                    let case = self.enum_case_name(o).map(|c| c.to_string_lossy()).unwrap_or_default();
                    return format!("{}::{case}", cls.name);
                }
                format!("Object({})", object::display_name(&cls.name))
            }
            Value::Resource(r) => format!("Resource id #{}", r.handle()),
            Value::Reference(r) => self.trace_arg(&r.get()),
        }
    }

    /// `UnhandledMatchError` for a subject.
    pub(crate) fn match_error(&mut self, v: &Value) -> Throw {
        let desc = match v {
            Value::Str(s) => {
                let b = s.as_bytes();
                let n = b.len().min(15);
                let mut d = String::from("'");
                d.push_str(&escape_bytes(&b[..n]));
                if b.len() > 15 {
                    d.push_str("...");
                }
                d.push('\'');
                d
            }
            Value::Object(o) => format!("of type {}", object::display_name(&object::class_of(o).name)),
            Value::Array(_) => "of type array".into(),
            Value::Resource(_) => "of type resource".into(),
            other => self.trace_arg(other),
        };
        self.make_throw("UnhandledMatchError", &format!("Unhandled match case {desc}"))
    }
}

/// `smart_str_append_escaped`.
pub(crate) fn escape_bytes(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        if c < 32 || c == b'\\' || c > 126 {
            s.push('\\');
            match c {
                b'\n' => s.push('n'),
                b'\r' => s.push('r'),
                b'\t' => s.push('t'),
                0x0c => s.push('f'),
                0x0b => s.push('v'),
                b'\\' => s.push('\\'),
                0x1b => s.push('e'),
                _ => s.push_str(&format!("x{c:02X}")),
            }
        } else {
            s.push(c as char);
        }
    }
    s
}

// ----------------------------------------------------------------------
// Methods
// ----------------------------------------------------------------------

fn exc_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if let Some(m) = a.get(0) {
        set_base_prop(&o, "message", m);
    }
    if let Some(c) = a.get(1) {
        set_base_prop(&o, "code", c);
    }
    if let Some(p) = a.get(2) {
        if !p.is_null() {
            set_base_prop(&o, "previous", p);
        }
    }
    let _ = vm;
    Ok(Value::Null)
}

fn error_exception_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if let Some(m) = a.get(0) {
        set_base_prop(&o, "message", m);
    }
    if let Some(c) = a.get(1) {
        set_base_prop(&o, "code", c);
    }
    if let Some(s) = a.get(2) {
        set_base_prop(&o, "severity", s);
    }
    if let Some(f) = a.get(3) {
        if !f.is_null() {
            set_base_prop(&o, "file", f);
            // A filename without a line resets the line to 0.
            if a.get(4).is_none_or(|l| l.is_null()) {
                set_base_prop(&o, "line", Value::Int(0));
            }
        }
    }
    if let Some(l) = a.get(4) {
        if !l.is_null() {
            set_base_prop(&o, "line", l);
        }
    }
    if let Some(p) = a.get(5) {
        if !p.is_null() {
            set_base_prop(&o, "previous", p);
        }
    }
    let _ = vm;
    Ok(Value::Null)
}

fn exc_get(name: &'static str) -> impl Fn(&mut Vm, &mut Args) -> VmResult<Value> {
    move |_vm, a| Ok(get_base_prop(&a.this_obj(), name).unwrap_or_default())
}

fn get_message(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("message")(vm, a)
}
fn get_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("code")(vm, a)
}
fn get_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("file")(vm, a)
}
fn get_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("line")(vm, a)
}
fn get_trace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("trace")(vm, a)
}
fn get_previous(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("previous")(vm, a)
}
fn get_severity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exc_get("severity")(vm, a)
}

fn get_trace_as_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let t = get_base_prop(&a.this_obj(), "trace").unwrap_or_default();
    let Value::Array(t) = t else {
        return Ok(Value::Str(ZStr::from("#0 {main}")));
    };
    Ok(Value::Str(ZStr::from(vm.trace_to_string(&t, true))))
}

fn exc_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut str = String::new();
    let mut cur = Some(a.this_obj());
    let mut seen: Vec<usize> = Vec::new();
    while let Some(e) = cur {
        if seen.contains(&e.addr()) {
            break;
        }
        seen.push(e.addr());
        let msg_v = get_base_prop(&e, "message").unwrap_or_default();
        let mut message = vm.to_zstr(&msg_v)?.to_string_lossy();
        let file_v = get_base_prop(&e, "file").unwrap_or_default();
        let file = vm.to_zstr(&file_v)?.to_string_lossy();
        let line_v = get_base_prop(&e, "line").unwrap_or_default();
        let line = vm.to_long(&line_v)?;
        let trace = vm.call_method_name(&e, "getTraceAsString", Vec::new())?;
        let trace = match trace {
            Value::Str(s) if !s.is_empty() => s.to_string_lossy(),
            _ => "#0 {main}\n".to_string(),
        };
        let cls = object::class_of(&e).clone();
        if (cls.lc_name.as_bytes() == b"typeerror" || cls.lc_name.as_bytes() == b"argumentcounterror")
            && message.contains(", called in ")
        {
            message.push_str(" and defined");
        }
        let name = object::display_name(&cls.name).to_string_lossy();
        let next = if str.is_empty() {
            String::new()
        } else {
            format!("\n\nNext {str}")
        };
        str = if message.is_empty() {
            format!("{name} in {file}:{line}\nStack trace:\n{trace}{next}")
        } else {
            format!("{name}: {message} in {file}:{line}\nStack trace:\n{trace}{next}")
        };
        cur = match get_base_prop(&e, "previous") {
            Some(Value::Object(p)) if vm.instance_of(&p, b"throwable") => Some(p),
            _ => None,
        };
    }
    let r = ZStr::from(str);
    set_base_prop(&a.this_obj(), "string", Value::Str(r.clone()));
    Ok(Value::Str(r))
}

fn exc_wakeup(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn exc_clone(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// Registers the Throwable hierarchy.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("Throwable")
        .flags(cflags::INTERFACE)
        .implements(vm, &["Stringable"])
        .abstract_method("getMessage(): string", PUB)
        .abstract_method("getCode()", PUB)
        .abstract_method("getFile(): string", PUB)
        .abstract_method("getLine(): int", PUB)
        .abstract_method("getTrace(): array", PUB)
        .abstract_method("getPrevious(): ?Throwable", PUB)
        .abstract_method("getTraceAsString(): string", PUB)
        .register(vm);
    for base in ["Exception", "Error"] {
        ClassBuilder::new(base)
            .implements(vm, &["Throwable"])
            .internal(InternalKind::Throwable)
            .prop("message", PROT, None, Some(Value::Str(ZStr::empty())))
            .prop("string", PRIV, Some("string"), Some(Value::Str(ZStr::empty())))
            .prop("code", PROT, None, Some(Value::Int(0)))
            .prop("file", PROT, Some("string"), Some(Value::Str(ZStr::empty())))
            .prop("line", PROT, Some("int"), Some(Value::Int(0)))
            .prop("trace", PRIV, Some("array"), Some(Value::Array(ZArray::new())))
            .prop("previous", PRIV, Some("?Throwable"), Some(Value::Null))
            .method("__clone(): void", PRIV, exc_clone)
            .method(
                "__construct(string $message = \"\", int $code = 0, ?Throwable $previous = null)",
                PUB,
                exc_construct,
            )
            .method("__wakeup(): void", PUB, exc_wakeup)
            .method("getMessage(): string", PUB | flags::FINAL, get_message)
            .method("getCode()", PUB | flags::FINAL, get_code)
            .method("getFile(): string", PUB | flags::FINAL, get_file)
            .method("getLine(): int", PUB | flags::FINAL, get_line)
            .method("getTrace(): array", PUB | flags::FINAL, get_trace)
            .method("getPrevious(): ?Throwable", PUB | flags::FINAL, get_previous)
            .method("getTraceAsString(): string", PUB | flags::FINAL, get_trace_as_string)
            .method("__toString(): string", PUB, exc_to_string)
            .register(vm);
    }
    ClassBuilder::new("ErrorException")
        .extends(vm, "Exception")
        .prop("severity", PROT, Some("int"), Some(Value::Int(1)))
        .method(
            "__construct(string $message = \"\", int $code = 0, int $severity = E_ERROR, ?string $filename = null, ?int $line = null, ?Throwable $previous = null)",
            PUB,
            error_exception_construct,
        )
        .method("getSeverity(): int", PUB | flags::FINAL, get_severity)
        .register(vm);
    for (name, parent) in [
        ("CompileError", "Error"),
        ("ParseError", "CompileError"),
        ("TypeError", "Error"),
        ("ArgumentCountError", "TypeError"),
        ("ValueError", "Error"),
        ("ArithmeticError", "Error"),
        ("DivisionByZeroError", "ArithmeticError"),
        ("UnhandledMatchError", "Error"),
        ("RequestParseBodyException", "Exception"),
    ] {
        ClassBuilder::new(name).extends(vm, parent).register(vm);
    }
}
