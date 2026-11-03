//! `ext/filter` for the phprust VM: binds the `php-filter` library through
//! the `php_vm::ext` extension API.
//!
//! * `filter_var()`, `filter_var_array()`, `filter_input()`,
//!   `filter_input_array()`, `filter_has_var()`, `filter_list()`,
//!   `filter_id()`, the `FILTER_*`/`INPUT_*` constants and the
//!   `Filter\FilterException` / `Filter\FilterFailedException` classes;
//! * the `filter.default` / `filter.default_flags` INI directives.
//!
//! `FILTER_VALIDATE_REGEXP` compiles patterns with `php-pcre` (own cache),
//! `FILTER_CALLBACK` calls back into the VM.
//!
//! # Request inputs
//!
//! The CLI has no GET/POST/COOKIE input, so those storages are
//! uninitialised (`filter_input()` returns `null`). `INPUT_SERVER` and
//! `INPUT_ENV` read a snapshot of `$_SERVER` / `$_ENV` taken the first time
//! a filter function needs them (the VM fills the superglobals after the
//! extensions are registered and offers no request-startup hook).
//!
//! Known gaps: `filter.default` is not applied to the superglobals (no
//! request-startup hook) nor to `parse_str()`, and the
//! `FILTER_SANITIZE_STRING`/`STRIPPED` constants are not reported as
//! deprecated (no deprecated-constant support in the extension API).

mod host;

use std::cell::RefCell;

use host::{rt_throw, VmFilterHost};
use php_filter::constants::CONSTANTS;
use php_filter::{FilterArgs, FilterInputs};
use php_runtime::{RtResult, Value, ZArray};
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, ClassBuilder, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `filter` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter;

/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

thread_local! {
    /// The raw `INPUT_SERVER` / `INPUT_ENV` storages (snapshots).
    static INPUTS: RefCell<Option<FilterInputs>> = const { RefCell::new(None) };
}

/// A superglobal array, if set.
fn superglobal(vm: &Vm, name: &[u8]) -> Option<ZArray> {
    let mut g = vm.globals.borrow_mut();
    let i = g.find(name)?;
    match g.slots.get(i as usize)?.as_ref()?.deref() {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

/// The request inputs (`IF_G(*_array)`).
fn inputs(vm: &Vm) -> FilterInputs {
    INPUTS.with(|i| {
        i.borrow_mut()
            .get_or_insert_with(|| FilterInputs {
                post: None,
                get: None,
                cookie: None,
                env: superglobal(vm, b"_ENV"),
                server: superglobal(vm, b"_SERVER"),
            })
            .clone()
    })
}

/// Runs a library call with the VM as [`php_filter::FilterHost`].
fn run(vm: &mut Vm, f: impl FnOnce(&mut VmFilterHost<'_>) -> RtResult<Value>) -> VmResult<Value> {
    let r = f(&mut VmFilterHost { vm: &mut *vm });
    r.map_err(|e| rt_throw(vm, e))
}

/// The `array|int $options` argument (`default` when not passed).
fn options(a: &Args, n: usize, default: i64) -> FilterArgs {
    match a.get(n) {
        Some(Value::Array(arr)) => FilterArgs::Array(arr),
        Some(Value::Int(i)) => FilterArgs::Long(i),
        _ => FilterArgs::Long(default),
    }
}

fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

fn bool_or(a: &Args, n: usize, default: bool) -> bool {
    if a.has(n) {
        a.bool(n)
    } else {
        default
    }
}

fn filter_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let value = a.at(0);
    let filter = int_or(a, 1, php_filter::constants::FILTER_DEFAULT);
    let opts = options(a, 2, 0);
    run(vm, |h| php_filter::filter_var(h, &value, filter, &opts))
}

fn filter_var_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = a.array(0);
    let opts = options(a, 1, php_filter::constants::FILTER_DEFAULT);
    let add_empty = bool_or(a, 2, true);
    run(vm, |h| {
        php_filter::filter_var_array(h, &arr, &opts, add_empty)
    })
}

fn filter_input(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let inputs = inputs(vm);
    let (ty, name) = (a.int(0), a.str(1));
    let filter = int_or(a, 2, php_filter::constants::FILTER_DEFAULT);
    let opts = options(a, 3, 0);
    run(vm, |h| {
        php_filter::filter_input(h, &inputs, ty, name.as_bytes(), filter, &opts)
    })
}

fn filter_input_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let inputs = inputs(vm);
    let ty = a.int(0);
    let opts = options(a, 1, php_filter::constants::FILTER_DEFAULT);
    let add_empty = bool_or(a, 2, true);
    run(vm, |h| {
        php_filter::filter_input_array(h, &inputs, ty, &opts, add_empty)
    })
}

fn filter_has_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let inputs = inputs(vm);
    let (ty, name) = (a.int(0), a.str(1));
    run(vm, |_| {
        php_filter::filter_has_var(&inputs, ty, name.as_bytes()).map(Value::Bool)
    })
}

fn filter_list(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::from_values(
        php_filter::filter_list().into_iter().map(Value::string),
    )))
}

fn filter_id(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(php_filter::filter_id(a.str(0).as_bytes()).map_or(Value::Bool(false), Value::Int))
}

impl Extension for Filter {
    fn name(&self) -> &'static str {
        "filter"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, value, _) in CONSTANTS {
            m.constant(name, Value::Int(value));
        }
        m.ini(IniDirective::new(
            "filter.default",
            Some("unsafe_raw"),
            Access(Access::SYSTEM.0 | Access::PERDIR.0),
            ModifyHandler::String,
            "filter",
        ))
        .ini(IniDirective::new(
            "filter.default_flags",
            None,
            Access(Access::SYSTEM.0 | Access::PERDIR.0),
            ModifyHandler::None,
            "filter",
        ));
        let default = m.vm().ini_string("filter.default").unwrap_or_default();
        if php_filter::parse_default_filter_ini(&default).1 {
            // UpdateDefaultFilter runs at startup: "in Unknown on line 0".
            let _ = m
                .vm()
                .raise(E_DEPRECATED, "The filter.default ini setting is deprecated");
        }
        ClassBuilder::new(php_filter::FILTER_EXCEPTION_CLASS)
            .extends(m.vm(), "Exception")
            .register(m.vm());
        ClassBuilder::new(php_filter::FILTER_FAILED_EXCEPTION_CLASS)
            .extends(m.vm(), php_filter::FILTER_EXCEPTION_CLASS)
            .register(m.vm());
        m.function("filter_has_var(int $input_type, string $var_name): bool", filter_has_var)
            .function("filter_input(int $type, string $var_name, int $filter = FILTER_DEFAULT, array|int $options = 0): mixed", filter_input)
            .function("filter_var(mixed $value, int $filter = FILTER_DEFAULT, array|int $options = 0): mixed", filter_var)
            .function("filter_input_array(int $type, array|int $options = FILTER_DEFAULT, bool $add_empty = true): array|false|null", filter_input_array)
            .function("filter_var_array(array $array, array|int $options = FILTER_DEFAULT, bool $add_empty = true): array|false", filter_var_array)
            .function("filter_list(): array", filter_list)
            .function("filter_id(string $name): int|false", filter_id);
    }
}
