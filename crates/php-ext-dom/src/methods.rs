//! The generic native method handler: every DOM method is registered with
//! its stub signature (the VM performs argument parsing) and dispatched
//! by name to `php_dom::dispatch`.

use std::rc::Rc;

use php_dom::classes::{find_class, find_method, ClassKind};
use php_dom::dispatch::{self, DomObject};
use php_dom::{ArrayKey as DomKey, DomValue};
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, VmResult};
use php_vm::object::{class_of, display_name};
use php_vm::Vm;

use crate::errors::{check, Ctx};
use crate::hooks::builtin_of;
use crate::host::VmHost;
use crate::run;
use crate::state::{dom_object, set_dom_object, to_dom, to_value};

/// The error context of `builtin::method` called on an object of `class`.
fn method_ctx(builtin: &str, method: &str, class: String) -> Ctx {
    match find_method(builtin, method) {
        Some((c, m)) => Ctx {
            func: format!("{}::{}", c.name, m.name),
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
fn convert_args(vm: &mut Vm, a: &Args, builtin: &str, method: &str) -> VmResult<Vec<DomValue>> {
    let params = find_method(builtin, method).map_or(&[][..], |(_, m)| m.params);
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
        let p = params.get(i).or_else(|| params.last().filter(|p| p.variadic));
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

/// Finds the built-in DOM class of a (possibly user) class.
fn builtin_of_class(cls: &Rc<php_vm::class::Class>) -> Option<&'static str> {
    let mut c = Some(cls.clone());
    while let Some(cl) = c {
        if let Some(ci) = find_class(&display_name(&cl.name).to_string_lossy()) {
            if ci.kind == ClassKind::Class {
                return Some(ci.name);
            }
        }
        c = cl.parent.clone();
    }
    None
}

fn finish(vm: &mut Vm, r: Result<DomValue, php_dom::DomError>, ctx: &Ctx) -> VmResult<Value> {
    match check(vm, r, Some(ctx))? {
        Some(v) => to_value(vm, v),
        None => Ok(Value::Bool(false)),
    }
}

/// Static methods (`createFromString()`, `quote()`, …).
fn static_method(vm: &mut Vm, a: &Args, name: &str) -> VmResult<Value> {
    let builtin = a
        .called
        .as_ref()
        .and_then(builtin_of_class)
        .unwrap_or("DOMNode");
    let ctx = method_ctx(builtin, name, builtin.to_string());
    let dargs = convert_args(vm, a, builtin, name)?;
    let r = run::call(vm, a, || dispatch::call_static(builtin, name, &dargs, &VmHost))?;
    finish(vm, r, &ctx)
}

/// `__construct`.
fn construct(vm: &mut Vm, a: &Args, this: &Obj, builtin: &'static str) -> VmResult<Value> {
    let ctx = method_ctx(builtin, "__construct", display_name(&class_of(this).name).to_string_lossy());
    let dargs = convert_args(vm, a, builtin, "__construct")?;
    match dom_object(this) {
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
    let name = a.func_name();
    let Some(this) = a.this.clone() else {
        return static_method(vm, a, &name);
    };
    let Some(builtin) = builtin_of(&this) else {
        return Err(vm.throw("Error", &format!("Call to undefined method {name}()")));
    };
    if name.eq_ignore_ascii_case("__construct") {
        return construct(vm, a, &this, builtin);
    }
    let class = display_name(&class_of(&this).name).to_string_lossy();
    let ctx = method_ctx(builtin, &name, class.clone());
    let Some(d) = dom_object(&this) else {
        if crate::hooks::is_xpath_class(builtin) {
            return Err(crate::hooks::invalid_state(vm));
        }
        return Err(vm.throw("Error", &format!("Couldn't fetch {class}")));
    };
    if name.eq_ignore_ascii_case("getIterator") {
        let it = crate::hooks::new_iter(Some(d));
        return Ok(Value::Object(vm.new_internal_iterator(it)?));
    }
    let dargs = convert_args(vm, a, builtin, &name)?;
    let r = run::call(vm, a, || dispatch::call(&d, &name, &dargs, &VmHost))?;
    finish(vm, r, &ctx)
}
