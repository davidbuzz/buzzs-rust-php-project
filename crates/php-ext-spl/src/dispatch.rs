//! Generic registration of the SPL classes from
//! [`php_spl::classes::class_table`] and the single native method handler
//! that routes every call to [`php_spl::call_method`].

use crate::host::{rt_throw, spl_state_of, VmHost};
use php_runtime::{Value, ZStr};
use php_spl::classes::{ClassDef, ClassKind, MethodDef, MethodFlags};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, VmResult};
use php_vm::func::Func;
use php_vm::Vm;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

/// What a registered native method stands for.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// The declaring SPL class.
    class: &'static str,
    /// The method declaration.
    def: &'static MethodDef,
}

thread_local! {
    /// Native method handlers, keyed by the address of their `Builtin`.
    static METHODS: RefCell<HashMap<usize, Target>> = RefCell::new(HashMap::new());
}

/// The stub signatures of all SPL methods, built once per process (the
/// class builder needs `&'static str` signatures).
fn signatures() -> &'static HashMap<(&'static str, &'static str), String> {
    static SIGS: OnceLock<HashMap<(&'static str, &'static str), String>> = OnceLock::new();
    SIGS.get_or_init(|| {
        let mut m = HashMap::new();
        for c in php_spl::classes::class_table() {
            for d in c.methods {
                m.insert((c.name, d.name), signature(d));
            }
        }
        m
    })
}

/// Renders a value as a literal the VM's stub parser understands.
fn literal(v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let s = format!("{f:?}");
            if s.contains('.') || s.contains('e') {
                s
            } else {
                format!("{s}.0")
            }
        }
        Value::Str(s) => {
            let t = s.to_string_lossy();
            if t.contains('\'') || t.contains('\n') || t.contains('\t') {
                return None;
            }
            format!("'{}'", t.replace('\\', "\\\\"))
        }
        Value::Array(a) if a.is_empty() => "[]".to_string(),
        _ => return None,
    })
}

/// Evaluates a stub default (`null`, `0`, `"r"`, `self::X | Y::Z`,
/// `2 * 1024 * 1024`, `PHP_INT_MAX`, ...).
#[must_use]
pub fn eval_default(src: &str, class: &str) -> Option<Value> {
    let s = src.trim();
    if let Some(pos) = s.find('|') {
        let a = eval_default(&s[..pos], class)?;
        let b = eval_default(&s[pos + 1..], class)?;
        return match (a, b) {
            (Value::Int(x), Value::Int(y)) => Some(Value::Int(x | y)),
            _ => None,
        };
    }
    if let Some(pos) = s.find('*') {
        let a = eval_default(&s[..pos], class)?;
        let b = eval_default(&s[pos + 1..], class)?;
        return match (a, b) {
            (Value::Int(x), Value::Int(y)) => Some(Value::Int(x.saturating_mul(y))),
            _ => None,
        };
    }
    match s.to_ascii_lowercase().as_str() {
        "null" => return Some(Value::Null),
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "[]" => return Some(Value::empty_array()),
        "php_int_max" => return Some(Value::Int(i64::MAX)),
        "sort_regular" => return Some(Value::Int(0)),
        "count_normal" => return Some(Value::Int(0)),
        "preg_split_no_empty" => return Some(Value::Int(1)),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Value::Int(i));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        let inner = &s[1..s.len() - 1];
        let mut out = Vec::new();
        let mut it = inner.bytes().peekable();
        while let Some(c) = it.next() {
            if c == b'\\' {
                match it.next() {
                    Some(b'n') => out.push(b'\n'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'0') => out.push(0),
                    Some(o) => out.push(o),
                    None => out.push(b'\\'),
                }
            } else {
                out.push(c);
            }
        }
        return Some(Value::Str(ZStr::from(out)));
    }
    if let Some((cls, name)) = s.split_once("::") {
        let cls = if cls.eq_ignore_ascii_case("self") || cls.eq_ignore_ascii_case("static") {
            class
        } else {
            cls
        };
        return php_spl::classes::constant(cls, name).map(Value::Int);
    }
    None
}

/// The VM stub signature of a method.
fn signature(d: &MethodDef) -> String {
    let mut s = String::from(d.name);
    s.push('(');
    for (i, p) in d.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        // Callable parameters are checked by `php-spl`, which produces
        // PHP's "must be a valid callback, ..." messages.
        if let Some(t) = p.ty.filter(|t| !t.contains("callable")) {
            s.push_str(t);
            s.push(' ');
        }
        if p.by_ref {
            s.push('&');
        }
        if p.variadic {
            s.push_str("...");
        }
        s.push('$');
        s.push_str(p.name);
        if let Some(def) = p.default {
            s.push_str(" = ");
            match eval_default(def, "").as_ref().and_then(literal) {
                Some(l) => s.push_str(&l),
                None => s.push_str("UNKNOWN"),
            }
        }
    }
    s.push(')');
    if let Some(r) = d.return_type {
        s.push_str(": ");
        s.push_str(r);
    }
    s
}

fn method_flags(d: &MethodDef) -> u32 {
    let mut f = 0;
    if d.flags & MethodFlags::STATIC != 0 {
        f |= acc::STATIC;
    }
    if d.flags & MethodFlags::FINAL != 0 {
        f |= acc::FINAL;
    }
    if d.flags & MethodFlags::PROTECTED != 0 {
        f |= acc::PROTECTED;
    } else if d.flags & MethodFlags::PRIVATE != 0 {
        f |= acc::PRIVATE;
    } else {
        f |= acc::PUBLIC;
    }
    f
}

/// Registers one SPL class or interface.
fn register_class(vm: &mut Vm, c: &'static ClassDef) {
    let sigs = signatures();
    let mut b = ClassBuilder::new(c.name);
    match c.kind {
        ClassKind::Interface => b = b.flags(class_flags::INTERFACE),
        ClassKind::Abstract => b = b.flags(class_flags::ABSTRACT),
        ClassKind::Class => {}
    }
    if c.is_final {
        b = b.flags(class_flags::FINAL);
    }
    if c.name == "SplFileInfo" {
        b = b.flags(class_flags::NOT_SERIALIZABLE);
    }
    if let Some(p) = c.parent {
        b = b.extends(vm, p);
    }
    b = b.implements(vm, c.interfaces);
    for (n, v) in c.constants {
        b = b.constant(n, Value::Int(*v));
    }
    for p in c.properties {
        let def = p.default.and_then(|d| eval_default(d, c.name));
        b = b.prop(p.name, acc::PUBLIC, p.ty, def);
    }
    for d in c.methods {
        let Some(sig) = sigs.get(&(c.name, d.name)) else {
            continue;
        };
        let fl = method_flags(d);
        if c.kind == ClassKind::Interface || d.flags & MethodFlags::ABSTRACT != 0 {
            b = b.abstract_method(sig, fl);
        } else {
            b = b.method(sig, fl, method_handler);
        }
    }
    if c.kind != ClassKind::Interface {
        b = b.hooks(crate::hooks::class_hooks());
    }
    let cls = b.register(vm);
    METHODS.with(|m| {
        let mut m = m.borrow_mut();
        for d in c.methods {
            let lc = d.name.to_ascii_lowercase();
            if let Some(meth) = cls.methods.get(&ZStr::from(lc)) {
                if let Func::Builtin(bi) = &meth.func {
                    m.insert(Rc::as_ptr(bi) as usize, Target { class: c.name, def: d });
                }
            }
        }
    });
}

/// Registers every SPL class the VM does not provide yet, parents and
/// interfaces first. Returns the names of the classes that were already
/// registered (by the VM or another extension).
pub fn register_classes(vm: &mut Vm) -> Vec<&'static str> {
    let mut skipped = Vec::new();
    let mut pending: Vec<&'static ClassDef> = Vec::new();
    for c in php_spl::classes::class_table() {
        if vm.find_class(c.name.as_bytes()).is_some() {
            skipped.push(c.name);
        } else {
            pending.push(c);
        }
    }
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = Vec::new();
        for c in pending {
            let ready = c
                .parent
                .iter()
                .chain(c.interfaces.iter())
                .all(|n| vm.find_class(n.as_bytes()).is_some());
            if ready {
                register_class(vm, c);
            } else {
                rest.push(c);
            }
        }
        pending = rest;
        if pending.len() == before {
            // Unresolvable dependencies: register anyway.
            for c in pending.drain(..) {
                register_class(vm, c);
            }
        }
    }
    skipped
}

/// The arguments as `php-spl` expects them: only the passed ones, with
/// parameters skipped by named arguments filled with their defaults.
fn collect_args(a: &Args, t: Target) -> Vec<Value> {
    let variadic = t.def.params.last().is_some_and(|p| p.variadic);
    let n = if variadic {
        a.values.len()
    } else {
        a.passed.iter().rposition(|&p| p).map_or(0, |i| i + 1)
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let passed = a.passed.get(i).copied().unwrap_or(true);
        let v = a.values.get(i).cloned().unwrap_or_default();
        if passed {
            out.push(v);
        } else {
            let d = t
                .def
                .params
                .get(i)
                .and_then(|p| p.default)
                .and_then(|d| eval_default(d, t.class));
            out.push(d.unwrap_or(v));
        }
    }
    out
}

/// The native handler of every SPL method.
fn method_handler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(f) = a.func.clone() else {
        return Err(vm.throw("Error", "SPL method called without a function"));
    };
    let key = Rc::as_ptr(&f) as usize;
    let Some(t) = METHODS.with(|m| m.borrow().get(&key).copied()) else {
        return Err(vm.throw(
            "Error",
            &format!("Unknown SPL method {}()", f.name.to_string_lossy()),
        ));
    };
    if let Some(msg) = php_spl::classes::deprecation_message(t.class, t.def.name) {
        vm.raise(8192, &msg)?;
    }
    let args = collect_args(a, t);
    let this = a.this.clone();
    let r = {
        let mut host = VmHost::new(vm);
        let mut r = Ok(());
        if let Some(o) = &this {
            // `get_method` checks do not apply to constructor calls.
            if spl_state_of(o).is_some() && !t.def.name.eq_ignore_ascii_case("__construct") {
                r = php_spl::handlers::before_method_call(&mut host, o);
            }
        }
        r.and_then(|()| php_spl::call_method(&mut host, t.class, t.def.name, this.as_ref(), &args))
    };
    r.map_err(|e| rt_throw(vm, e))
}

/// The VM stub signature of an SPL function.
#[must_use]
pub fn function_signature(d: &MethodDef) -> String {
    signature(d)
}

