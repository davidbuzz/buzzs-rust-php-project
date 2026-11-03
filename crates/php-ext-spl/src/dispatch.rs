//! Generic registration of the SPL classes from
//! [`php_spl::classes::class_table`] and the single native method handler
//! that routes every call to [`php_spl::call_method`].

use crate::host::{rt_throw, spl_state_of, VmHost};
use php_runtime::{Obj, Value, ZStr};
use php_spl::classes::{ClassDef, ClassKind, MethodDef, MethodFlags};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, VmResult};
use php_vm::func::Func;
use php_vm::opcode::{Op, Operand};
use php_vm::vm::FrameKind;
use php_vm::{Throw, Vm};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::BitOr;
use std::rc::Rc;
use std::sync::OnceLock;

/// What a registered native method stands for.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// The declaring SPL class.
    class: &'static str,
    /// The method declaration.
    def: &'static MethodDef,
    /// Whether the method is `#[\Deprecated]`.
    deprecated: bool,
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
    for (sep, op) in [
        ('|', i64::bitor as fn(i64, i64) -> i64),
        ('*', i64::saturating_mul),
    ] {
        if let Some((lhs, rhs)) = s.split_once(sep) {
            return match (eval_default(lhs, class)?, eval_default(rhs, class)?) {
                (Value::Int(lhs), Value::Int(rhs)) => Some(Value::Int(op(lhs, rhs))),
                _ => None,
            };
        }
    }
    match s.to_ascii_lowercase().as_str() {
        "null" => return Some(Value::Null),
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "[]" => return Some(Value::empty_array()),
        "php_int_max" => return Some(Value::Int(i64::MAX)),
        "sort_regular" | "count_normal" => return Some(Value::Int(0)),
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

/// Whether a declared parameter type is left to `php-spl`'s own checks.
fn loose_type(method: &str, ty: &str) -> bool {
    method.eq_ignore_ascii_case("__construct")
        || ty.contains("callable")
        || matches!(ty, "array|object" | "object|array")
}

/// The VM stub signature of a method.
fn signature(d: &MethodDef) -> String {
    let mut s = String::from(d.name);
    s.push('(');
    for (i, p) in d.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        // Some parameters are checked by `php-spl` itself, which produces
        // the exact `zend_parse_parameters` messages: callables ("must be
        // a valid callback, ..."), `array|object` ("must be of type
        // array, ...") and constructor arguments (constructors are not
        // subject to signature compatibility checks).
        if let Some(t) = p.ty.filter(|t| !loose_type(d.name, t)) {
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
    if matches!(c.name, "IteratorIterator" | "RecursiveIteratorIterator") {
        // `spl_dual_it_get_method` / `spl_recursive_it_get_method` forward
        // calls of unknown methods to the inner iterator; the VM has no
        // `get_method` hook, so this is modelled with `__call`.
        b = b.method("__call(...$arguments)", acc::PUBLIC, forward_call);
    }
    let cls = b.register(vm);
    METHODS.with(|m| {
        let mut m = m.borrow_mut();
        for d in c.methods {
            let lc = d.name.to_ascii_lowercase();
            if let Some(meth) = cls.methods.get(&ZStr::from(lc)) {
                if let Func::Builtin(bi) = &meth.func {
                    m.insert(
                        Rc::as_ptr(bi) as usize,
                        Target {
                            class: c.name,
                            def: d,
                            deprecated: d.deprecated.is_some(),
                        },
                    );
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
fn collect_args(args: &Args, target: Target) -> Vec<Value> {
    let variadic = target.def.params.last().is_some_and(|p| p.variadic);
    let count = if variadic {
        args.values.len()
    } else {
        args.passed.iter().rposition(|&p| p).map_or(0, |i| i + 1)
    };
    (0..count)
        .map(|i| {
            let value = args.values.get(i).cloned().unwrap_or_default();
            if args.passed.get(i).copied().unwrap_or(true) {
                return value;
            }
            target
                .def
                .params
                .get(i)
                .and_then(|p| p.default)
                .and_then(|d| eval_default(d, target.class))
                .unwrap_or(value)
        })
        .collect()
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
    if t.deprecated {
        if let Some(msg) = php_spl::classes::deprecation_message(t.class, t.def.name) {
            vm.raise(8192, &msg)?;
        }
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
    crate::iter::finish(vm, r)
}

/// The VM stub signature of an SPL function.
#[must_use]
pub fn function_signature(d: &MethodDef) -> String {
    signature(d)
}

/// The method name of the call being dispatched to `__call`: the VM does
/// not pass it to native `__call` handlers, so it is recovered from the
/// calling frame's pending `InitMethodCall` (matched by call nesting).
fn pending_method_name(vm: &Vm) -> Option<ZStr> {
    let f = vm
        .frames
        .iter()
        .rev()
        .find(|f| f.kind != FrameKind::Builtin)?;
    let ops = &f.code.ops;
    // The interpreter advances `ip` before executing an instruction.
    let mut i = f.ip.checked_sub(1)?;
    if !matches!(ops.get(i)?, Op::DoCall { .. }) {
        return None;
    }
    let mut depth = 0usize;
    loop {
        match &ops[i] {
            Op::DoCall { .. } | Op::CallableConvert { .. } => depth += 1,
            Op::InitMethodCall { name, .. } => {
                depth -= 1;
                if depth == 0 {
                    return match name {
                        Operand::Lit(n) => match f.code.literals.get(*n as usize) {
                            Some(Value::Str(s)) => Some(s.clone()),
                            _ => None,
                        },
                        _ => None,
                    };
                }
            }
            Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => {
                depth -= 1;
                if depth == 0 {
                    return None;
                }
            }
            _ => {}
        }
        i = i.checked_sub(1)?;
    }
}

/// The fetch type of the dimension access being executed (`BP_VAR_*`),
/// recovered from the current instruction: the VM's `read_dimension`
/// hook does not say whether it serves a read or a write fetch.
#[must_use]
pub fn current_dim_kind(vm: &Vm) -> php_spl::DimKind {
    use php_spl::DimKind;
    let Some(f) = vm
        .frames
        .iter()
        .rev()
        .find(|f| f.kind != FrameKind::Builtin)
    else {
        return DimKind::Read;
    };
    match f.ip.checked_sub(1).and_then(|i| f.code.ops.get(i)) {
        Some(Op::Assign { .. } | Op::AssignRef { .. } | Op::MakeRef { .. }) => DimKind::Write,
        Some(Op::AssignOp { .. } | Op::IncDec { .. }) => DimKind::ReadWrite,
        Some(Op::Unset { .. }) => DimKind::Unset,
        _ => DimKind::Read,
    }
}

/// Whether the instruction being executed is `$a = &$b`.
#[must_use]
pub fn current_op_is_assign_ref(vm: &Vm) -> bool {
    vm.frames
        .iter()
        .rev()
        .find(|f| f.kind != FrameKind::Builtin)
        .and_then(|f| f.ip.checked_sub(1).and_then(|i| f.code.ops.get(i)))
        .is_some_and(|op| matches!(op, Op::AssignRef { .. }))
}

/// Forwards an unknown method call to the inner (current sub-) iterator.
fn forward_call(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(this) = a.this.clone() else {
        return Ok(Value::Null);
    };
    let name = pending_method_name(vm);
    let target = {
        let mut host = VmHost::new(vm);
        php_spl::handlers::method_target(&mut host, &this)
    };
    let target = target.map_err(|e| rt_throw(vm, e))?;
    if let (Some(name), Some(inner)) = (&name, target) {
        let lc = name.to_ascii_lowercase();
        let icls = php_vm::object::class_of(&inner);
        if icls.find_method(lc.as_bytes()).is_some() || icls.magic.call.is_some() {
            let args: Vec<Value> = a.values.iter().map(Value::deref).collect();
            return vm.call_method_by_name(&inner, &name.to_string_lossy(), args);
        }
    }
    let shown = name.map_or_else(|| "__call".to_string(), |n| n.to_string_lossy());
    let t = vm.throw(
        "Error",
        &format!(
            "Call to undefined method {}::{shown}()",
            this.class_name().to_string_lossy()
        ),
    );
    if let Throw::Exception(e) = &t {
        drop_top_trace_frame(e);
    }
    Err(t)
}

/// Removes the innermost frame (our `__call`) from an exception's trace:
/// PHP raises "Call to undefined method" while resolving the call, before
/// any frame is pushed.
fn drop_top_trace_frame(e: &Obj) {
    let cls = php_vm::object::class_of(e).clone();
    let Some(pi) = cls.props.iter().find(|p| p.name.as_bytes() == b"trace") else {
        return;
    };
    let d = php_vm::object::data(e);
    let mut slots = d.slots.borrow_mut();
    if let Some(Some(Value::Array(frames))) = slots.get(pi.slot as usize) {
        let rest: Vec<Value> = frames.values().skip(1).cloned().collect();
        slots[pi.slot as usize] = Some(Value::Array(php_runtime::ZArray::from_values(rest)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(src: &str, class: &str) -> Option<String> {
        eval_default(src, class).as_ref().and_then(literal)
    }

    #[test]
    fn defaults_evaluate() {
        assert_eq!(lit("null", "").as_deref(), Some("null"));
        assert_eq!(lit("2 * 1024 * 1024", "").as_deref(), Some("2097152"));
        assert_eq!(
            lit("self::LEAVES_ONLY", "RecursiveIteratorIterator").as_deref(),
            Some("0")
        );
        assert_eq!(lit("\"\\\"\"", "").as_deref(), Some("'\"'"));
        assert_eq!(lit("\"\\\\\"", "").as_deref(), Some("'\\\\'"));
        assert_eq!(
            lit("PHP_INT_MAX", "").as_deref(),
            Some("9223372036854775807")
        );
    }

    #[test]
    fn signatures_render() {
        let (_, m) = php_spl::classes::find_method("SplFileObject", "fgetcsv").expect("method");
        assert_eq!(
            signature(m),
            "fgetcsv(string $separator = ',', string $enclosure = '\"', string $escape = '\\\\'): array|false"
        );
        let (_, m) =
            php_spl::classes::find_method("CallbackFilterIterator", "__construct").expect("ctor");
        assert_eq!(signature(m), "__construct($iterator, $callback)");
        let (_, m) = php_spl::classes::find_method("ArrayObject", "exchangeArray").expect("method");
        assert_eq!(signature(m), "exchangeArray($array): array");
    }
}
