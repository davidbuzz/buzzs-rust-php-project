//! Builtin functions and classes.

pub mod classbuilder;
mod core;
mod dump;
pub(crate) mod headers;
pub(crate) mod info;
mod serial;
pub(crate) use self::core::enum_methods;
mod array;
mod classes;
mod math;
mod output;
pub mod sig;
mod string;
pub(crate) mod util;
mod var;

use crate::func::{flags, Builtin, BuiltinFn, Func};
use crate::vm::Vm;
use php_runtime::{Value, ZStr};
use std::collections::HashMap;
use std::rc::Rc;

/// Per-request state of builtin extensions.
#[derive(Debug, Default)]
pub struct ExtState {
    /// `__COMPILER_HALT_OFFSET__` per file.
    pub halt_offsets: HashMap<ZStr, i64>,
    /// Random number generator globals (`mt_rand`, `rand`, `lcg_value`).
    pub random: Option<php_random::RandomGlobals>,
}

/// Registers a builtin function from a stub signature.
pub(crate) fn reg(vm: &mut Vm, sig: &str, f: BuiltinFn) -> ZStr {
    let s = sig::parse_sig(sig);
    let mut fl = 0;
    if s.variadic {
        fl |= flags::VARIADIC;
    }
    if s.by_ref_return {
        fl |= flags::RETURN_REF;
    }
    let lc = ZStr::from(s.name.to_ascii_lowercase());
    let b = Builtin {
        name: ZStr::from(s.name.as_str()),
        params: s.params,
        defaults: s.defaults,
        required: s.required,
        return_type: s.ret,
        flags: fl,
        handler: f,
    };
    vm.functions.insert(lc.clone(), Func::Builtin(Rc::new(b)));
    lc
}

/// Registers an alias for an existing function.
pub(crate) fn alias(vm: &mut Vm, alias: &str, target: &str) {
    if let Some(Func::Builtin(b)) = vm.functions.get(&ZStr::from(target)).cloned() {
        let nb = Builtin {
            name: ZStr::from(alias),
            params: b.params.clone(),
            defaults: b.defaults.clone(),
            required: b.required,
            return_type: b.return_type.clone(),
            flags: b.flags,
            handler: b.handler,
        };
        vm.functions.insert(
            ZStr::from(alias.to_ascii_lowercase()),
            Func::Builtin(Rc::new(nb)),
        );
    }
}

/// Registers every builtin function and class.
pub(crate) fn register_all(vm: &mut Vm) {
    classes::register(vm);
    crate::weakref::register(vm);
    core::register(vm);
    crate::gc::register(vm);
    headers::register(vm);
    var::register(vm);
    output::register(vm);
    string::register(vm);
    array::register(vm);
    math::register(vm);
    info::register(vm);
}

/// Defines a constant.
pub(crate) fn constant(vm: &mut Vm, name: &str, v: Value) {
    vm.constants.insert(ZStr::from(name), v);
}

/// The current time formatted like PHP's error log (`d-M-Y H:i:s UTC`).
pub(crate) fn time_now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = secs / 86400;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(i64::try_from(days).unwrap_or(0));
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{:02}-{}-{} {:02}:{:02}:{:02} UTC",
        d,
        MONTHS[usize::try_from(m - 1).unwrap_or(0)],
        y,
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
