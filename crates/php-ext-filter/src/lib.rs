// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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
//! The storages are the raw superglobals snapshotted at request startup,
//! before `filter.default` is applied to them. The CLI has no GET/POST/
//! COOKIE input, so those storages are uninitialised there (`filter_input()`
//! returns `null`); php-cgi and `php -S` register them first (a JIT `$_ENV`
//! is snapshotted when it is first created).
//!
//! Known gap: `parse_str()` (php-ext-stdmisc) does not apply
//! `filter.default` (use [`php_filter::apply_default_filter`]).

// Native handlers and object hooks must match the VM's fn-pointer
// signatures (`NativeFn`, `ClassHooks`), even when they cannot fail.
#![allow(clippy::unnecessary_wraps)]

mod host;

use std::cell::RefCell;

use host::{rt_throw, VmFilterHost};
use php_filter::constants::CONSTANTS;
use php_filter::{FilterArgs, FilterInputs};
use php_runtime::{RtResult, Value, ZArray, ZStr};
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

/// Replaces a superglobal array.
fn set_superglobal(vm: &Vm, name: &[u8], value: ZArray) {
    let mut g = vm.globals.borrow_mut();
    let i = g.find_or_add(name) as usize;
    if let Some(slot) = g.slots.get_mut(i) {
        *slot = Some(Value::Array(value));
    }
}

/// `php_sapi_filter` over an already registered superglobal: string
/// values are replaced by their `filter.default` result.
fn filter_superglobal(vm: &mut Vm, name: &[u8], filter: i64, flags: i64) -> VmResult<()> {
    let Some(arr) = superglobal(vm, name) else {
        return Ok(());
    };
    let out = filter_array(vm, &arr, filter, flags)?;
    set_superglobal(vm, name, out);
    Ok(())
}

/// `filter.default` over the string leaves of `arr` (`php_sapi_filter`
/// runs on every registered value, nested ones included).
///
/// Once a memory-limit violation is pending (`ZEND_HAS_OOM()`),
/// `php_sapi_filter` returns 0 and request parsing stops: the request ends
/// with the "Allowed memory size" fatal before the script runs, so the
/// remaining values are not registered.
fn filter_array(vm: &mut Vm, arr: &ZArray, filter: i64, flags: i64) -> VmResult<ZArray> {
    let mut out = ZArray::new();
    for (k, v) in arr.iter() {
        let v = match v.deref() {
            Value::Str(raw) if !default_output_fits(raw.as_bytes(), filter) => Value::from(""),
            Value::Str(raw) => run(vm, |h| {
                php_filter::apply_default_filter(h, raw.as_bytes(), filter, flags)
            })?,
            Value::Array(inner) => Value::Array(filter_array(vm, &inner, filter, flags)?),
            other => other,
        };
        out.make_mut().insert(k.clone(), v);
        if php_memlimit::is_pending() {
            break;
        }
    }
    Ok(out)
}

/// `php_filter_encode_url` (`FILTER_SANITIZE_ENCODED`) allocates its
/// output up front with the fallible `zend_string_safe_falloc(len, 3, 0)`:
/// over the soft memory limit the allocation fails and records the
/// delayed OOM (`zend_mm_check_limit`), and the value becomes `""`. The
/// other filters grow their output and are checked after the call.
fn default_output_fits(raw: &[u8], filter: i64) -> bool {
    // `ZEND_MM_ALIGNED_SIZE(_ZSTR_STRUCT_SIZE(0))`: the 24-byte header and
    // the terminating NUL, 8-aligned.
    const ZSTR_OVERHEAD: usize = 32;
    filter != php_filter::constants::FILTER_SANITIZE_ENCODED
        || php_memlimit::try_reserve_n(raw.len(), 3, ZSTR_OVERHEAD).is_ok()
}

/// A request input registered by the SAPI (`IF_G(get_array)` and kin are
/// only initialised once a variable was registered from that source).
fn registered(vm: &Vm, name: &[u8]) -> Option<ZArray> {
    superglobal(vm, name).filter(|a| !a.is_empty())
}

/// `RINIT`: snapshots the raw request input storages and applies
/// `filter.default` to the registered superglobals. The SAPI registers the
/// request variables first (php-cgi, `php -S`); under the CLI GET, POST and
/// COOKIE are empty and their storages stay uninitialised.
fn request_startup(vm: &mut Vm) {
    let raw = FilterInputs {
        post: registered(vm, b"_POST"),
        get: registered(vm, b"_GET"),
        cookie: registered(vm, b"_COOKIE"),
        env: superglobal(vm, b"_ENV"),
        server: superglobal(vm, b"_SERVER"),
    };
    INPUTS.with(|i| *i.borrow_mut() = Some(raw));
    let default = vm.ini_string("filter.default").unwrap_or_default();
    let (filter, deprecated) = php_filter::parse_default_filter_ini(&default);
    if deprecated {
        // UpdateDefaultFilter runs at startup: "in Unknown on line 0".
        let r = vm.error_at(
            E_DEPRECATED,
            "The filter.default ini setting is deprecated",
            &ZStr::from("Unknown"),
            0,
        );
        if r.is_err() {
            return;
        }
    }
    if filter == php_filter::constants::FILTER_UNSAFE_RAW {
        return;
    }
    let flags =
        php_filter::parse_default_flags_ini(vm.ini_string("filter.default_flags").as_deref());
    // `$_REQUEST` is built from the filtered GET/POST/COOKIE values;
    // filtering its raw copies gives the same strings.
    for name in [
        b"_GET".as_slice(),
        b"_POST",
        b"_COOKIE",
        b"_REQUEST",
        b"_SERVER",
        b"_ENV",
    ] {
        if filter_superglobal(vm, name, filter, flags).is_err() || php_memlimit::is_pending() {
            break;
        }
    }
}

/// The request inputs (`IF_G(*_array)`) for `input_type`.
///
/// `php_filter_get_storage`: under `auto_globals_jit`, `INPUT_SERVER` and
/// `INPUT_ENV` first request their auto-global (`zend_is_auto_global`). A
/// JIT `$_ENV` is created after `RINIT`, so its storage is taken when it
/// first exists (php-src falls back to `PG(http_globals)[TRACK_VARS_ENV]`).
fn inputs(vm: &mut Vm, input_type: i64) -> FilterInputs {
    let auto_global: &[u8] = match input_type {
        php_filter::constants::INPUT_ENV => b"_ENV",
        php_filter::constants::INPUT_SERVER => b"_SERVER",
        _ => b"",
    };
    if !auto_global.is_empty() && vm.ini_bool("auto_globals_jit") {
        vm.is_auto_global(auto_global);
    }
    let vm: &Vm = vm;
    INPUTS.with(|i| {
        let mut i = i.borrow_mut();
        let inputs = i.get_or_insert_with(|| FilterInputs {
            post: None,
            get: None,
            cookie: None,
            env: None,
            server: None,
        });
        if inputs.env.is_none() {
            inputs.env = superglobal(vm, b"_ENV");
        }
        if inputs.server.is_none() {
            inputs.server = superglobal(vm, b"_SERVER");
        }
        inputs.clone()
    })
}

/// Runs a library call with the VM as [`php_filter::FilterHost`].
fn run(vm: &mut Vm, f: impl FnOnce(&mut VmFilterHost<'_>) -> RtResult<Value>) -> VmResult<Value> {
    let r = f(&mut VmFilterHost { vm: &mut *vm });
    r.map_err(|e| rt_throw(vm, e))
}

/// The sanitizing filters whose output can be several times its input.
const EXPANDING: [i64; 3] = [
    php_filter::constants::FILTER_SANITIZE_ENCODED,
    php_filter::constants::FILTER_SANITIZE_SPECIAL_CHARS,
    php_filter::constants::FILTER_SANITIZE_FULL_SPECIAL_CHARS,
];

/// Whether `filter`/`options` (any nesting) select an [`EXPANDING`] filter.
fn uses_expanding(filter: Option<i64>, options: &FilterArgs) -> bool {
    fn scan(v: &Value) -> bool {
        match v.deref() {
            Value::Int(i) => EXPANDING.contains(&i),
            Value::Array(a) => a.values().any(scan),
            _ => false,
        }
    }
    filter.is_some_and(|f| EXPANDING.contains(&f))
        || match options {
            FilterArgs::Long(l) => EXPANDING.contains(l),
            FilterArgs::Array(a) => a.values().any(scan),
        }
}

/// Upper bound of the encoded size of the strings in `v`: every byte that
/// is not ASCII alphanumeric may become a 6-byte entity (`&#255;`).
fn encoded_bound(v: &Value) -> usize {
    match v.deref() {
        Value::Str(s) => s
            .as_bytes()
            .iter()
            .map(|b| if b.is_ascii_alphanumeric() { 1 } else { 6 })
            .sum(),
        Value::Array(a) => a.values().map(encoded_bound).sum(),
        _ => 0,
    }
}

/// Reserves the output of an expanding sanitizer before running it (PHP's
/// `smart_str` growth hits `memory_limit` while building the result; the
/// library would otherwise grow straight through the hard ceiling).
/// `false` means the limit is exceeded: the VM raises the fatal after the
/// call returns.
fn reserve_output(value: &Value, filter: Option<i64>, options: &FilterArgs) -> bool {
    !uses_expanding(filter, options) || php_memlimit::try_reserve(encoded_bound(value)).is_ok()
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
    if !reserve_output(&value, Some(filter), &opts) {
        return Ok(Value::Null);
    }
    run(vm, |h| php_filter::filter_var(h, &value, filter, &opts))
}

fn filter_var_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = a.array(0);
    let opts = options(a, 1, php_filter::constants::FILTER_DEFAULT);
    let add_empty = bool_or(a, 2, true);
    if !reserve_output(&Value::Array(arr.clone()), None, &opts) {
        return Ok(Value::Null);
    }
    run(vm, |h| {
        php_filter::filter_var_array(h, &arr, &opts, add_empty)
    })
}

fn filter_input(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (ty, name) = (a.int(0), a.str(1));
    let inputs = inputs(vm, ty);
    let filter = int_or(a, 2, php_filter::constants::FILTER_DEFAULT);
    let opts = options(a, 3, 0);
    run(vm, |h| {
        php_filter::filter_input(h, &inputs, ty, name.as_bytes(), filter, &opts)
    })
}

fn filter_input_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ty = a.int(0);
    let inputs = inputs(vm, ty);
    let opts = options(a, 1, php_filter::constants::FILTER_DEFAULT);
    let add_empty = bool_or(a, 2, true);
    run(vm, |h| {
        php_filter::filter_input_array(h, &inputs, ty, &opts, add_empty)
    })
}

fn filter_has_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (ty, name) = (a.int(0), a.str(1));
    let inputs = inputs(vm, ty);
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
        for (name, value, deprecation) in CONSTANTS {
            match deprecation {
                Some((since, message)) => {
                    m.constant_deprecated(name, Value::Int(value), since, message)
                }
                None => m.constant(name, Value::Int(value)),
            };
        }
        m.on_request_startup(request_startup);
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
        ClassBuilder::new(php_filter::FILTER_EXCEPTION_CLASS)
            .extends(m.vm(), "Exception")
            .register(m.vm());
        ClassBuilder::new(php_filter::FILTER_FAILED_EXCEPTION_CLASS)
            .extends(m.vm(), php_filter::FILTER_EXCEPTION_CLASS)
            .register(m.vm());
        m.function_static("filter_has_var(int $input_type, string $var_name): bool", filter_has_var)
            .function_static("filter_input(int $type, string $var_name, int $filter = FILTER_DEFAULT, array|int $options = 0): mixed", filter_input)
            .function_static("filter_var(mixed $value, int $filter = FILTER_DEFAULT, array|int $options = 0): mixed", filter_var)
            .function_static("filter_input_array(int $type, array|int $options = FILTER_DEFAULT, bool $add_empty = true): array|false|null", filter_input_array)
            .function_static("filter_var_array(array $array, array|int $options = FILTER_DEFAULT, bool $add_empty = true): array|false", filter_var_array)
            .function_static("filter_list(): array", filter_list)
            .function_static("filter_id(string $name): int|false", filter_id);
    }
}
