// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The generic native method handler: every DOM method is registered with
//! its stub signature (the VM performs argument parsing) and dispatched
//! by name to `php_dom::dispatch`.

use std::rc::Rc;

use php_dom::classes::{find_method, MethodInfo};
use php_dom::dispatch::{self, DomObject};
use php_dom::{ArrayKey as DomKey, DomValue};
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, VmResult};
use php_vm::object::{class_of, display_name};
use php_vm::Vm;

use crate::errors::{check, Ctx};
use crate::hooks::{builtin_of, builtin_of_class};
use crate::host::VmHost;
use crate::run;
use crate::state::{bytes, dom_object, set_dom_object, to_dom, to_value};

/// A method declaration: declaring class and method.
type MethodDecl = Option<(&'static str, &'static MethodInfo)>;

/// What the handler needs to know about one registered native method,
/// computed once per `Builtin` (keyed by its address).
#[derive(Clone)]
struct MethodMeta {
    /// The method name as registered.
    name: Rc<str>,
    /// The stub declaration.
    decl: MethodDecl,
    /// Whether the call may reach PHP code or the stream layer (I/O,
    /// `XPath` callbacks, class checks) and must lend the VM.
    needs_vm: bool,
}

thread_local! {
    /// `Builtin` address → metadata.
    static METHODS: std::cell::RefCell<std::collections::HashMap<usize, MethodMeta>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Method names (lowercase substrings) whose implementation may perform
/// I/O, parse with external entities or call back into PHP.
const NEEDS_VM: &[&str] = &[
    "load",
    "save",
    "validate",
    "xinclude",
    "c14n",
    "evaluate",
    "query",
    "registernodeclass",
    "createfrom",
    "schema",
    "relaxng",
    "adjacenthtml",
    "registerphp",
    "__construct",
];

/// The cached metadata of the native method being called.
fn method_meta(a: &Args, builtin: &'static str) -> MethodMeta {
    let key = a.func.as_ref().map_or(0, |f| Rc::as_ptr(f) as usize);
    if key != 0 {
        if let Some(m) = METHODS.with(|m| m.borrow().get(&key).cloned()) {
            return m;
        }
    }
    let name = a.func_name();
    let lc = name.to_ascii_lowercase();
    let meta = MethodMeta {
        decl: find_method(builtin, &name).map(|(c, m)| (c.name, m)),
        needs_vm: NEEDS_VM.iter().any(|n| lc.contains(n)),
        name: Rc::from(name),
    };
    if key != 0 {
        METHODS.with(|m| m.borrow_mut().insert(key, meta.clone()));
    }
    meta
}

/// The error context of `builtin::method` called on an object of `class`.
fn method_ctx(builtin: &'static str, method: &str, decl: MethodDecl, class: String) -> Ctx {
    match decl {
        Some((c, m)) => Ctx {
            func: format!("{c}::{}", m.name),
            params: m.params.iter().map(|p| p.name).collect(),
            variadic_from: m.params.iter().position(|p| p.variadic),
            class,
        },
        None => Ctx {
            func: format!("{builtin}::{method}"),
            class,
            ..Ctx::default()
        },
    }
}

/// Converts the passed arguments (trailing omitted optional arguments are
/// left out so php-dom can tell them from explicit values).
fn convert_args(vm: &mut Vm, a: &Args, meta: &MethodMeta) -> VmResult<Vec<DomValue>> {
    let params = meta.decl.map_or(&[][..], |(_, m)| m.params);
    let method = &*meta.name;
    let n = a
        .passed
        .iter()
        .rposition(|&p| p)
        .map_or(0, |i| i + 1)
        .min(a.values.len())
        .max(if params.iter().any(|p| p.variadic) {
            a.values.len()
        } else {
            0
        });
    let reg_fns = method.eq_ignore_ascii_case("registerPhpFunctions");
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = a.values[i].deref();
        let p = params
            .get(i)
            .or_else(|| params.last().filter(|p| p.variadic));
        let is_callable = p.is_some_and(|p| p.ty == Some("callable"));
        let dv = if is_callable && !v.is_null() {
            DomValue::Callable(Rc::new(v.clone()))
        } else if reg_fns {
            match &v {
                Value::Array(arr) => {
                    let mut items = Vec::with_capacity(arr.len());
                    for (k, e) in arr.iter() {
                        let e = e.deref();
                        let (key, ev) = match k {
                            php_runtime::ArrayKey::Int(i) => {
                                // List entries are function names
                                // (`zval_try_get_string`).
                                let s = vm.value_to_string(&e)?;
                                (DomKey::Int(*i), DomValue::Str(s.as_bytes().to_vec()))
                            }
                            php_runtime::ArrayKey::Str(s) => {
                                let ev = match &e {
                                    Value::Str(s) => DomValue::Str(s.as_bytes().to_vec()),
                                    _ if vm.is_callable(&e) => {
                                        DomValue::Callable(Rc::new(e.clone()))
                                    }
                                    Value::Array(_) | Value::Object(_) => {
                                        DomValue::Other(vm.callable_error_message(&e))
                                    }
                                    other => to_dom(vm, other),
                                };
                                (DomKey::Str(s.as_bytes().to_vec()), ev)
                            }
                        };
                        items.push((key, ev));
                    }
                    DomValue::Array(items)
                }
                other => to_dom(vm, other),
            }
        } else {
            to_dom(vm, &v)
        };
        out.push(dv);
    }
    Ok(out)
}

/// Converts a library result; the error context is only built on failure.
fn finish(
    vm: &mut Vm,
    r: Result<DomValue, php_dom::DomError>,
    ctx: impl FnOnce() -> Ctx,
) -> VmResult<Value> {
    match r {
        Ok(v) => to_value(vm, v),
        Err(e) => {
            let ctx = ctx();
            match check::<DomValue>(vm, Err(e), Some(&ctx))? {
                Some(v) => to_value(vm, v),
                None => Ok(Value::Bool(false)),
            }
        }
    }
}

/// Runs a library call, lending the VM only when the method needs it.
fn invoke<R>(vm: &mut Vm, a: &Args, meta: &MethodMeta, f: impl FnOnce() -> R) -> VmResult<R> {
    if meta.needs_vm {
        run::call(vm, a, f)
    } else {
        run::direct(vm, f)
    }
}

/// Static methods (`createFromString()`, `quote()`, …).
fn static_method(vm: &mut Vm, a: &Args) -> VmResult<Value> {
    let builtin = a
        .called
        .as_ref()
        .and_then(builtin_of_class)
        .unwrap_or("DOMNode");
    let meta = method_meta(a, builtin);
    if matches!(builtin, "DOMXPath" | "Dom\\XPath") && meta.name.eq_ignore_ascii_case("quote") {
        if let Some(v) = quote(vm, a)? {
            return Ok(v);
        }
    }
    let dargs = convert_args(vm, a, &meta)?;
    let name = meta.name.clone();
    let r = run::call(vm, a, || {
        dispatch::call_static(builtin, &name, &dargs, &VmHost)
    })?;
    finish(vm, r, || {
        method_ctx(builtin, &name, meta.decl, builtin.to_string())
    })
}

/// `DOMXPath::quote()` without intermediate copies of the (possibly huge)
/// argument: PHP allocates the result with `zend_string_safe_falloc(..,
/// fallible)` / checks `ZEND_HAS_OOM()` while building the `concat()` form,
/// so an oversized result raises the memory-limit fatal before it exists
/// (ref: ext/dom/xpath.c). `None` hands arguments the generic path reports
/// (null bytes, non-strings) back to it.
fn quote(vm: &mut Vm, a: &Args) -> VmResult<Option<Value>> {
    let Some(Value::Str(s)) = a.values.first().map(Value::deref) else {
        return Ok(None);
    };
    let input = s.as_bytes();
    if input.contains(&0) {
        return Ok(None);
    }
    // zend_string header + bytes + NUL.
    let size = php_dom::XPath::quoted_len(input).saturating_add(25);
    if let Err(over) = php_memlimit::try_reserve(size) {
        // Raised here, so the VM's post-call poll does not raise it again.
        let _ = php_memlimit::take();
        return Err(vm.fatal(&over.to_string()));
    }
    Ok(Some(bytes(php_dom::XPath::quote(input))))
}

/// `__construct`.
fn construct(vm: &mut Vm, a: &Args, this: &Obj, builtin: &'static str) -> VmResult<Value> {
    let meta = method_meta(a, builtin);
    let ctx = method_ctx(
        builtin,
        "__construct",
        meta.decl,
        display_name(&class_of(this).name).to_string_lossy(),
    );
    let dargs = convert_args(vm, a, &meta)?;
    match dom_object(this) {
        Some(DomObject::XPath(x)) if x.is_evaluating() => {
            // `%pS` of the object's class: in full, anonymous ones too.
            return Err(vm.throw(
                "Error",
                &format!(
                    "Cannot call {}::__construct() while an XPath evaluation is in progress",
                    class_of(this).name.to_string_lossy()
                ),
            ));
        }
        Some(d) => {
            let r = run::call(vm, a, || dispatch::construct(&d, &dargs, &VmHost))?;
            if let Some(Some(newobj)) = check(vm, r, Some(&ctx))? {
                set_dom_object(this, newobj);
            }
        }
        None if builtin == "DOMXPath" || builtin == "Dom\\XPath" => {
            let Some(DomValue::Node(doc)) = dargs.first().cloned() else {
                return Err(vm.throw("TypeError", "Invalid document"));
            };
            let reg = match dargs.get(1) {
                Some(DomValue::Bool(b)) => *b,
                _ => true,
            };
            let r = run::call(vm, a, || dispatch::new_xpath(&doc, reg))?;
            if let Some(x) = check(vm, r, Some(&ctx))? {
                set_dom_object(this, DomObject::XPath(x));
            }
        }
        None => {}
    }
    Ok(Value::Null)
}

/// The handler of every DOM method.
pub(crate) fn method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(this) = a.this.clone() else {
        return static_method(vm, a);
    };
    let Some(builtin) = builtin_of(&this) else {
        let name = a.func_name();
        return Err(vm.throw("Error", &format!("Call to undefined method {name}()")));
    };
    let meta = method_meta(a, builtin);
    let name = meta.name.clone();
    if name.eq_ignore_ascii_case("__construct") {
        return construct(vm, a, &this, builtin);
    }
    let Some(d) = dom_object(&this) else {
        if crate::hooks::is_xpath_class(builtin) {
            return Err(crate::hooks::invalid_state(vm));
        }
        let class = display_name(&class_of(&this).name).to_string_lossy();
        return Err(vm.throw("Error", &format!("Couldn't fetch {class}")));
    };
    if name.eq_ignore_ascii_case("getIterator") {
        let it = crate::hooks::new_iter(Some(d), Some(this.clone()));
        return Ok(Value::Object(vm.new_internal_iterator(Box::new(it))?));
    }
    let dargs = convert_args(vm, a, &meta)?;
    let r = invoke(vm, a, &meta, || dispatch::call(&d, &name, &dargs, &VmHost))?;
    finish(vm, r, || {
        method_ctx(
            builtin,
            &name,
            meta.decl,
            display_name(&class_of(&this).name).to_string_lossy(),
        )
    })
}
