// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Mapping [`DomError`]s to PHP exceptions.

use php_dom::DomError;
use php_runtime::Value;
use php_vm::ext::{Throw, VmResult};
use php_vm::Vm;

/// Where an error was raised: the function and its parameters (for
/// `Argument #N ($name)` prefixes) and the receiver's class (for
/// "Couldn't fetch").
#[derive(Debug, Clone, Default)]
pub(crate) struct Ctx {
    /// `Class::method` (or a function name).
    pub func: String,
    /// Parameter names.
    pub params: Vec<&'static str>,
    /// Index of the variadic parameter.
    pub variadic_from: Option<usize>,
    /// Class of `$this`.
    pub class: String,
}

impl Ctx {
    /// The `func(): Argument #N ($name) ` prefix.
    fn arg_prefix(&self, arg: u32) -> String {
        let idx = arg.saturating_sub(1) as usize;
        if self.variadic_from.is_some_and(|v| idx >= v) || self.func.is_empty() {
            return format!("{}(): Argument #{arg} ", self.func);
        }
        let p = self.params.get(idx).copied().unwrap_or("");
        format!("{}(): Argument #{arg} (${p}) ", self.func)
    }
}

/// Builds the exception for `e` (`Silent` yields `None`).
pub(crate) fn throw(vm: &mut Vm, e: DomError, ctx: Option<&Ctx>) -> Option<Throw> {
    let def = Ctx::default();
    let ctx = ctx.unwrap_or(&def);
    Some(match e {
        DomError::Silent => return None,
        DomError::Dom { code, message } => {
            let t = vm.throw("DOMException", &message);
            if let Throw::Exception(o) = &t {
                vm.set_property_raw(o, "code", Value::Int(code.code()));
            }
            t
        }
        DomError::Type(m) => vm.throw("TypeError", &m),
        DomError::Value(m) => vm.throw("ValueError", &m),
        DomError::Error(m) => vm.throw("Error", &m),
        DomError::Exception(m) => vm.throw("Exception", &m),
        DomError::ArgType { arg, message } => {
            vm.throw("TypeError", &format!("{}{message}", ctx.arg_prefix(arg)))
        }
        DomError::ArgValue { arg, message } => {
            vm.throw("ValueError", &format!("{}{message}", ctx.arg_prefix(arg)))
        }
        DomError::CouldntFetch => {
            let class = if ctx.class.is_empty() {
                "DOMNode"
            } else {
                ctx.class.as_str()
            };
            vm.throw("Error", &format!("Couldn't fetch {class}"))
        }
        DomError::Host(b) => match b.downcast::<Throw>() {
            Ok(t) => *t,
            Err(_) => vm.throw("Error", "Unknown DOM host error"),
        },
    })
}

/// Converts a library result: errors become exceptions, `Silent` becomes
/// `Ok(None)`.
pub(crate) fn check<T>(
    vm: &mut Vm,
    r: Result<T, DomError>,
    ctx: Option<&Ctx>,
) -> VmResult<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) => match throw(vm, e, ctx) {
            Some(t) => Err(t),
            None => Ok(None),
        },
    }
}
