//! The `Throwable` hierarchy (`Zend/zend_exceptions.c`) and backtraces
//! (`zend_fetch_debug_backtrace`).

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class, InternalKind};
use crate::func::{flags, Args, Func};
use crate::object;
use crate::opcode::FetchMode;
use crate::vm::{FrameKind, Throw, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, Value, ZArray, ZStr};
use std::fmt::Write as _;
use std::rc::Rc;

const PUB: u32 = flags::PUBLIC;
const PROT: u32 = flags::PROTECTED;
const PRIV: u32 = flags::PRIVATE;

/// `zend_exception_set_previous`: appends `prev` at the end of `exc`'s
/// `previous` chain unless that would create a cycle.
pub(crate) fn chain_previous(exc: &Obj, prev: &Obj) {
    const MAX_DEPTH: usize = 100_000;
    if Obj::ptr_eq(exc, prev) {
        return;
    }
    let previous_of = |o: &Obj| match get_base_prop(o, "previous") {
        Some(Value::Object(n)) => Some(n),
        _ => None,
    };
    let mut ex = exc.clone();
    for _ in 0..MAX_DEPTH {
        // `ex` already reachable from `prev`: linking would form a cycle.
        let mut ancestor = previous_of(prev);
        let mut depth = 0;
        while let Some(a) = ancestor {
            if Obj::ptr_eq(&a, &ex) || depth > MAX_DEPTH {
                return;
            }
            depth += 1;
            ancestor = previous_of(&a);
        }
        if let Some(Value::Object(n)) = get_base_prop(&ex, "previous") {
            if Obj::ptr_eq(&n, prev) {
                return;
            }
            ex = n;
        } else {
            set_base_prop(&ex, "previous", Value::Object(prev.clone()));
            return;
        }
    }
}

fn base_slot(o: &Obj, name: &str) -> Option<usize> {
    let cls = object::class_of(o);
    cls.props
        .iter()
        .find(|p| {
            p.name.as_bytes() == name.as_bytes()
                && (p.class.as_bytes() == b"Exception"
                    || p.class.as_bytes() == b"Error"
                    || p.class.as_bytes() == b"ErrorException")
        })
        .or_else(|| {
            cls.props
                .iter()
                .find(|p| p.name.as_bytes() == name.as_bytes())
        })
        .map(|p| p.slot as usize)
}

/// Reads a property of the exception base class.
pub(crate) fn get_base_prop(o: &Obj, name: &str) -> Option<Value> {
    let slot = base_slot(o, name)?;
    object::data(o)
        .slots
        .borrow()
        .get(slot)
        .cloned()
        .flatten()
        .map(|v| v.deref())
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
    #[allow(clippy::unused_self)]
    pub(crate) fn exc_prop(&self, o: &Obj, name: &str) -> Value {
        get_base_prop(o, name).unwrap_or_default()
    }

    /// Writes a base exception property.
    #[allow(clippy::unused_self)]
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
    #[allow(dead_code)]
    pub(crate) fn throw_error(&mut self, class: &str, msg: &str) -> Throw {
        self.make_throw(class, msg)
    }

    /// `EG(filename_override)`/`EG(lineno_override)`: while a constant
    /// expression of a class scope is evaluated (and anything it calls),
    /// the executed location is the expression's declaration.
    pub(crate) fn filename_override(&self) -> Option<(ZStr, u32)> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind == FrameKind::ConstExpr && f.scope.is_some())
            .map(|f| (f.code.filename.clone(), f.line()))
    }

    /// The executing user-code location ignoring the override.
    fn executed_location_raw(&self) -> Option<(ZStr, u32)> {
        self.frames
            .iter()
            .rev()
            .find(|f| !matches!(f.kind, FrameKind::Builtin | FrameKind::ConstExpr))
            .map(|f| (f.code.filename.clone(), f.line()))
    }

    /// Initializes file/line/trace of a new throwable.
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn init_throwable(&mut self, o: &Obj) -> VmResult<()> {
        let (file, line) = self
            .filename_override()
            .unwrap_or_else(|| self.current_location());
        let ignore_args = self.ini.get_bool("zend.exception_ignore_args");
        let trace = self.backtrace(false, ignore_args, false, 0);
        set_base_prop(o, "file", Value::Str(file));
        set_base_prop(o, "line", Value::Int(i64::from(line)));
        set_base_prop(o, "trace", Value::Array(trace));
        Ok(())
    }

    /// Builds a backtrace array (`zend_fetch_debug_backtrace`).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn backtrace(
        &mut self,
        skip_last: bool,
        ignore_args: bool,
        provide_object: bool,
        limit: usize,
    ) -> ZArray {
        let mut out = HashTable::new();
        let n = self.frames.len();
        if n == 0 {
            return ZArray::new();
        }
        if let (Some(over), Some((file, line))) =
            (self.filename_override(), self.executed_location_raw())
        {
            if over.0 != file || over.1 != line {
                let mut entry = HashTable::new();
                entry.insert(ArrayKey::Str(ZStr::from("file")), Value::Str(file));
                entry.insert(
                    ArrayKey::Str(ZStr::from("line")),
                    Value::Int(i64::from(line)),
                );
                entry.insert(
                    ArrayKey::Str(ZStr::from("function")),
                    Value::Str(ZStr::from("[constant expression]")),
                );
                let _ = out.push(Value::Array(ZArray::from_table(entry)));
            }
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
            if f.kind == FrameKind::Main
                || (idx == 0 && !matches!(f.kind, FrameKind::Function | FrameKind::Builtin))
            {
                break;
            }
            if f.kind == FrameKind::ConstExpr {
                if idx == 0 {
                    break;
                }
                idx -= 1;
                continue;
            }
            // The calling frame (constant expressions have no frame of
            // their own in php-src). A function called with no caller
            // (exception handler, shutdown function) is a top-level call
            // shown as "[internal function]".
            let prev = (idx > 0).then(|| {
                let mut pidx = idx - 1;
                while pidx > 0 && self.frames[pidx].kind == FrameKind::ConstExpr {
                    pidx -= 1;
                }
                &self.frames[pidx]
            });
            let mut entry = HashTable::new();
            // File/line of the call site.
            if let Some(prev) = prev.filter(|p| p.kind != FrameKind::Builtin) {
                entry.insert(
                    ArrayKey::Str(ZStr::from("file")),
                    Value::Str(prev.code.filename.clone()),
                );
                entry.insert(
                    ArrayKey::Str(ZStr::from("line")),
                    Value::Int(i64::from(prev.line())),
                );
            }
            match f.kind {
                FrameKind::Include | FrameKind::Eval => {
                    let fname = if f.kind == FrameKind::Eval {
                        "eval"
                    } else {
                        f.builtin_name
                            .as_ref()
                            .map_or("include", |n| match n.as_bytes() {
                                b"require" => "require",
                                b"include_once" => "include_once",
                                b"require_once" => "require_once",
                                _ => "include",
                            })
                    };
                    if f.kind == FrameKind::Include && !ignore_args {
                        entry.insert(
                            ArrayKey::Str(ZStr::from("args")),
                            Value::Array(ZArray::from_values([Value::Str(
                                f.code.filename.clone(),
                            )])),
                        );
                    }
                    entry.insert(
                        ArrayKey::Str(ZStr::from("function")),
                        Value::Str(ZStr::from(fname)),
                    );
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
                        if idx == 0 {
                            break;
                        }
                        idx -= 1;
                        continue;
                    };
                    // `__call`/`__callStatic` frames show themselves with
                    // `($name, $args)`, as the trampoline is replaced.
                    let fname = func.name().clone();
                    entry.insert(ArrayKey::Str(ZStr::from("function")), Value::Str(fname));
                    let scope_name = match func {
                        Func::User(u) if u.is_closure() => f.scope.as_ref().map(|s| s.name.clone()),
                        _ => f.scope.as_ref().map(|s| s.name.clone()),
                    };
                    if let Some(this) = &f.this {
                        let cn = scope_name.unwrap_or_else(|| object::class_of(this).name.clone());
                        entry.insert(ArrayKey::Str(ZStr::from("class")), Value::Str(cn));
                        if provide_object {
                            entry.insert(
                                ArrayKey::Str(ZStr::from("object")),
                                Value::Object(this.clone()),
                            );
                        }
                        entry.insert(
                            ArrayKey::Str(ZStr::from("type")),
                            Value::Str(ZStr::from("->")),
                        );
                    } else if let Some(cn) = scope_name {
                        entry.insert(ArrayKey::Str(ZStr::from("class")), Value::Str(cn));
                        entry.insert(
                            ArrayKey::Str(ZStr::from("type")),
                            Value::Str(ZStr::from("::")),
                        );
                    }
                    if !ignore_args {
                        let args = self.frame_args(idx);
                        entry.insert(ArrayKey::Str(ZStr::from("args")), Value::Array(args));
                    }
                }
            }
            let _ = out.push(Value::Array(ZArray::from_table(entry)));
            count += 1;
            if idx == 0 {
                break;
            }
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
                    let vars = f.vars.borrow();
                    let nparams = uf.params.len();
                    let passed = f.args.len();
                    for (i, p) in uf.params.iter().enumerate() {
                        if p.variadic {
                            if let Some(Some(Value::Array(a))) =
                                vars.slots.get(f.cv_idx(u32::try_from(i).unwrap_or(0)))
                            {
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
                        // Not yet received (RECV pending): the passed value.
                        let v = vars
                            .slots
                            .get(f.cv_idx(u32::try_from(i).unwrap_or(0)))
                            .cloned()
                            .flatten()
                            .or_else(|| f.args.get(i).cloned().flatten())
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
        for (k, frame) in trace.iter() {
            let Value::Array(fr) = frame.deref() else {
                let idx = match k {
                    ArrayKey::Int(i) => i.to_string(),
                    ArrayKey::Str(_) => "0".to_string(),
                };
                let _ = self.warning(&format!("Expected array for frame {idx}"));
                continue;
            };
            self.build_trace_string(&mut s, &fr, num);
            num += 1;
        }
        if include_main {
            let _ = write!(s, "#{num} {{main}}");
        }
        s
    }

    /// `build_trace_string`.
    fn build_trace_string(&mut self, s: &mut String, fr: &ZArray, num: usize) {
        let get = |k: &str| fr.get(&ArrayKey::Str(ZStr::from(k))).map(Value::deref);
        let _ = write!(s, "#{num} ");
        match get("file") {
            Some(Value::Str(file)) => {
                let line = match get("line") {
                    Some(Value::Int(l)) => l,
                    Some(_) => {
                        let _ = self.warning("Line is not an int");
                        0
                    }
                    None => 0,
                };
                let _ = write!(s, "{}({line}): ", file.to_string_lossy());
            }
            Some(_) => {
                let _ = self.warning("File name is not a string");
                s.push_str("[unknown file]: ");
            }
            None => s.push_str("[internal function]: "),
        }
        match get("class") {
            Some(Value::Str(c)) => s.push_str(&object::display_name(&c).to_string_lossy()),
            Some(_) => {
                let _ = self.warning("Value for class is not a string");
                s.push_str("[unknown]");
            }
            None => {}
        }
        for key in ["type", "function"] {
            match get(key) {
                Some(Value::Str(t)) => s.push_str(&t.to_string_lossy()),
                Some(_) => {
                    let _ = self.warning(&format!("Value for {key} is not a string"));
                    s.push_str("[unknown]");
                }
                None => {}
            }
        }
        s.push('(');
        match get("args") {
            Some(Value::Array(args)) => {
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
                    let rendered = self.trace_arg(&a.deref());
                    s.push_str(&rendered);
                }
            }
            Some(_) => {
                let _ = self.warning("args element is not an array");
            }
            None => {}
        }
        s.push_str(")\n");
    }

    #[allow(clippy::many_single_char_names)]
    fn trace_arg(&mut self, v: &Value) -> String {
        let maxlen =
            usize::try_from(self.ini.get_long("zend.exception_string_param_max_len")).unwrap_or(15);
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(true) => "true".into(),
            Value::Bool(false) => "false".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                let mut s = php_runtime::float::zend_gcvt(
                    *f,
                    i32::try_from(self.precision).unwrap_or(14).max(1),
                    b'.',
                    b'E',
                );
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
                    let case = self
                        .enum_case_name(o)
                        .map(|c| c.to_string_lossy())
                        .unwrap_or_default();
                    return format!("{}::{case}", cls.name);
                }
                format!("Object({})", object::display_name(&cls.name))
            }
            Value::Resource(r) => format!("Resource id #{}", r.handle()),
            Value::Reference(r) => self.trace_arg(&r.get()),
        }
    }

    /// `UnhandledMatchError` for a subject (`zend_match_unhandled_error`).
    pub(crate) fn match_error(&mut self, v: &Value) -> Throw {
        let v = v.deref();
        let max_len = self.ini.get_long("zend.exception_string_param_max_len");
        let ignore_args = self.ini.get_bool("zend.exception_ignore_args");
        let renderable = match &v {
            Value::Str(_) => max_len != 0,
            Value::Object(o) => object::class_of(o).is_enum(),
            Value::Array(_) | Value::Resource(_) => false,
            _ => true,
        };
        let desc = if ignore_args || !renderable {
            let ty = match &v {
                Value::Object(o) => {
                    object::display_name(&object::class_of(o).name).to_string_lossy()
                }
                Value::Null => "null".into(),
                Value::Bool(_) => "bool".into(),
                Value::Int(_) => "int".into(),
                Value::Float(_) => "float".into(),
                Value::Str(_) => "string".into(),
                Value::Array(_) => "array".into(),
                Value::Resource(_) => "resource".into(),
                Value::Reference(_) => String::new(),
            };
            format!("of type {ty}")
        } else {
            self.trace_arg(&v)
        };
        self.make_throw(
            "UnhandledMatchError",
            &format!("Unhandled match case {desc}"),
        )
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
                _ => {
                    let _ = write!(s, "x{c:02X}");
                }
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

/// Whether any property of `o`'s class has hooks (then the base
/// properties must go through the regular property handlers).
fn has_hooked_props(o: &Obj) -> bool {
    object::class_of(o).props.iter().any(|p| p.hooks.is_some())
}

/// `i_get_exception_base`.
fn exception_base(vm: &Vm, o: &Obj) -> Option<Rc<Class>> {
    if vm.instance_of(o, b"exception") {
        vm.find_class(b"Exception")
    } else {
        vm.find_class(b"Error")
    }
}

/// Runs `f` with the class scope of the innermost user frame replaced by
/// `scope` (`EG(fake_scope)`).
fn with_scope<T>(
    vm: &mut Vm,
    scope: Option<Rc<Class>>,
    f: impl FnOnce(&mut Vm) -> VmResult<T>,
) -> VmResult<T> {
    let Some(idx) = vm
        .frames
        .iter()
        .rposition(|fr| fr.kind != FrameKind::Builtin)
    else {
        return f(vm);
    };
    let saved = std::mem::replace(&mut vm.frames[idx].scope, scope);
    let r = f(vm);
    if let Some(fr) = vm.frames.get_mut(idx) {
        fr.scope = saved;
    }
    r
}

/// `GET_PROPERTY`: reads a base property in the exception base scope.
fn read_prop(vm: &mut Vm, o: &Obj, name: &str) -> VmResult<Value> {
    if !has_hooked_props(o) {
        return Ok(get_base_prop(o, name).unwrap_or_default());
    }
    let base = exception_base(vm, o);
    let key = ZStr::from(name);
    with_scope(vm, base, |vm| vm.read_property(o, &key, FetchMode::Is)).map(|v| v.deref())
}

/// `zend_update_property_num_checked` (`base_scope` selects the exception
/// base class as scope instead of the object's own class).
fn write_prop(vm: &mut Vm, o: &Obj, name: &str, v: Value, base_scope: bool) -> VmResult<()> {
    if !has_hooked_props(o) {
        set_base_prop(o, name, v);
        return Ok(());
    }
    let scope = if base_scope {
        exception_base(vm, o)
    } else {
        Some(object::class_of(o).clone())
    };
    let key = ZStr::from(name);
    with_scope(vm, scope, |vm| vm.write_property(o, &key, v)).map(|_| ())
}

/// `zend_update_exception_properties`.
fn update_exception_properties(
    vm: &mut Vm,
    o: &Obj,
    a: &Args,
    previous_idx: usize,
) -> VmResult<()> {
    if let Some(m) = a.get(0) {
        write_prop(vm, o, "message", m, false)?;
    }
    if let Some(c) = a.get(1) {
        if c.as_int().is_some_and(|c| c != 0) {
            write_prop(vm, o, "code", c, false)?;
        }
    }
    if let Some(p) = a.get(previous_idx) {
        if !p.is_null() {
            write_prop(vm, o, "previous", p, true)?;
        }
    }
    Ok(())
}

fn exc_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    update_exception_properties(vm, &o, a, 2)?;
    Ok(Value::Null)
}

fn error_exception_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    update_exception_properties(vm, &o, a, 5)?;
    let severity = a.get(2).unwrap_or(Value::Int(1));
    write_prop(vm, &o, "severity", severity, true)?;
    let filename = a.get(3).filter(|f| !f.is_null());
    let has_file = filename.is_some();
    if let Some(f) = filename {
        write_prop(vm, &o, "file", f, false)?;
    }
    match a.get(4).filter(|l| !l.is_null()) {
        Some(l) => write_prop(vm, &o, "line", l, false)?,
        // A filename without a line resets the line to 0.
        None if has_file => write_prop(vm, &o, "line", Value::Int(0), false)?,
        None => {}
    }
    Ok(Value::Null)
}

fn get_message(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = read_prop(vm, &a.this_obj(), "message")?;
    Ok(Value::Str(vm.to_zstr(&v)?))
}
fn get_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    read_prop(vm, &a.this_obj(), "code")
}
fn get_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = read_prop(vm, &a.this_obj(), "file")?;
    Ok(Value::Str(vm.to_zstr(&v)?))
}
fn get_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = read_prop(vm, &a.this_obj(), "line")?;
    Ok(Value::Int(vm.to_long(&v)?))
}
fn get_trace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    read_prop(vm, &a.this_obj(), "trace")
}
fn get_previous(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    read_prop(vm, &a.this_obj(), "previous")
}
fn get_severity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    read_prop(vm, &a.this_obj(), "severity")
}

/// `Exception::getTraceAsString` for `o`.
fn trace_as_string(vm: &mut Vm, o: &Obj) -> VmResult<String> {
    let t = read_prop(vm, o, "trace")?;
    Ok(match t {
        Value::Array(t) => vm.trace_to_string(&t, true),
        _ => "#0 {main}".to_string(),
    })
}

fn get_trace_as_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = trace_as_string(vm, &a.this_obj())?;
    Ok(Value::Str(ZStr::from(s)))
}

/// `Exception::__toString`: the exception and its `previous` chain,
/// innermost first, joined with "Next".
fn exc_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let mut out: Vec<u8> = Vec::new();
    let mut cur = Some(this.clone());
    let mut seen: Vec<usize> = Vec::new();
    while let Some(e) = cur {
        seen.push(e.addr());
        let msg_v = read_prop(vm, &e, "message")?;
        let mut message = vm.to_zstr(&msg_v)?.as_bytes().to_vec();
        let file_v = read_prop(vm, &e, "file")?;
        let file = vm.to_zstr(&file_v)?;
        let line_v = read_prop(vm, &e, "line")?;
        let line = vm.to_long(&line_v)?;
        let trace = trace_as_string(vm, &e)?;
        let trace = if trace.is_empty() {
            "#0 {main}\n".to_string()
        } else {
            trace
        };
        let cls = object::class_of(&e).clone();
        if matches!(cls.lc_name.as_bytes(), b"typeerror" | b"argumentcounterror")
            && message.windows(12).any(|w| w == b", called in ")
        {
            message.extend_from_slice(b" and defined");
        }
        let mut s: Vec<u8> = Vec::new();
        s.extend_from_slice(cls.name.as_bytes());
        if !message.is_empty() {
            s.extend_from_slice(b": ");
            s.extend_from_slice(&message);
        }
        s.extend_from_slice(b" in ");
        s.extend_from_slice(file.as_bytes());
        s.extend_from_slice(format!(":{line}\nStack trace:\n{trace}").as_bytes());
        if !out.is_empty() {
            s.extend_from_slice(b"\n\nNext ");
            s.extend_from_slice(&out);
        }
        out = s;
        cur = match read_prop(vm, &e, "previous")? {
            Value::Object(p) if vm.instance_of(&p, b"throwable") && !seen.contains(&p.addr()) => {
                Some(p)
            }
            _ => None,
        };
    }
    let r = ZStr::from(out);
    set_base_prop(&this, "string", Value::Str(r.clone()));
    Ok(Value::Str(r))
}

/// `Exception::__wakeup`: drops a `message`/`code` of the wrong type.
#[allow(clippy::unnecessary_wraps)]
fn exc_wakeup(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    for (name, ok) in [
        (
            "message",
            (|v: &Value| matches!(v, Value::Str(_))) as fn(&Value) -> bool,
        ),
        ("code", |v: &Value| matches!(v, Value::Int(_))),
    ] {
        if let Some(v) = get_base_prop(&o, name) {
            if !v.is_null() && !ok(&v) {
                if let Some(slot) = base_slot(&o, name) {
                    if let Some(s) = object::data(&o).slots.borrow_mut().get_mut(slot) {
                        *s = None;
                    }
                }
            }
        }
    }
    Ok(Value::Null)
}

fn exc_clone(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    // Private, but reachable through reflection.
    Err(vm.make_throw("Exception", "Cannot clone object using __clone()"))
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
            .prop(
                "string",
                PRIV,
                Some("string"),
                Some(Value::Str(ZStr::empty())),
            )
            .prop("code", PROT, None, Some(Value::Int(0)))
            .prop(
                "file",
                PROT,
                Some("string"),
                Some(Value::Str(ZStr::empty())),
            )
            .prop("line", PROT, Some("int"), Some(Value::Int(0)))
            .prop(
                "trace",
                PRIV,
                Some("array"),
                Some(Value::Array(ZArray::new())),
            )
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
            .method(
                "getPrevious(): ?Throwable",
                PUB | flags::FINAL,
                get_previous,
            )
            .method(
                "getTraceAsString(): string",
                PUB | flags::FINAL,
                get_trace_as_string,
            )
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
