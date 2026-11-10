//! Glue between the VM and the [`urlscan`] rewriter: the
//! `session.trans_sid_*`/`url_rewriter.*` directives (with their master
//! deprecations), the `URL-Rewriter` output handler and the
//! `output_add_rewrite_var()`/`output_reset_rewrite_vars()` functions.

use std::cell::RefCell;

use php_runtime::{ArrayKey, Value};
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_support::output::{self, flags, StartHandler};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::host::docref;
use crate::urlscan::{self, Kind, DEFAULT_SESSION_TAGS};

/// The directives owned by the rewriter.
pub(crate) const DIRECTIVES: [&str; 4] = [
    "session.trans_sid_tags",
    "session.trans_sid_hosts",
    "url_rewriter.tags",
    "url_rewriter.hosts",
];

thread_local! {
    /// The values of the session directives at startup (restored at the
    /// end of the request).
    static STARTUP: RefCell<Vec<(&'static str, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    /// Directives changed at runtime (restored by `zend_ini_deactivate`).
    static MODIFIED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn set_modified(name: &str, on: bool) {
    MODIFIED.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|n| n != name);
        if on {
            m.push(name.to_string());
        }
    });
}

/// Registers the directives the VM does not know yet and the functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let defaults = [
        ("session.trans_sid_tags", DEFAULT_SESSION_TAGS),
        ("session.trans_sid_hosts", ""),
        ("url_rewriter.tags", "form="),
        ("url_rewriter.hosts", ""),
    ];
    for (name, default) in defaults {
        if !m.vm().ini.contains(name) {
            m.ini(IniDirective::new(
                name,
                Some(default),
                Access::ALL,
                ModifyHandler::String,
                // Registered by ext/standard's URL scanner in PHP.
                "standard",
            ));
        }
    }
    for (name, hook) in DIRECTIVES.into_iter().zip(HOOKS) {
        m.ini_on_modify(name, hook);
    }
    m.function_static(
        "output_add_rewrite_var(string $name, string $value): bool",
        output_add_rewrite_var,
    )
    .function_static(
        "output_reset_rewrite_vars(): bool",
        output_reset_rewrite_vars,
    );
    m.output_handler_hook("URL-Rewriter", refresh_env);
}

/// The deprecation `OnUpdateSessionTags`/`OnUpdateSessionHosts` raise for
/// `value`, if any.
fn deprecation(name: &str, value: &[u8]) -> Option<&'static str> {
    match name {
        "session.trans_sid_tags" if !value.starts_with(DEFAULT_SESSION_TAGS.as_bytes()) => {
            Some("Usage of session.trans_sid_tags INI setting is deprecated")
        }
        "session.trans_sid_hosts" if !value.is_empty() => {
            Some("Usage of session.trans_sid_hosts INI setting is deprecated")
        }
        _ => None,
    }
}

/// Applies a value to the scanner configuration.
fn apply(name: &str, value: &[u8]) {
    match name {
        "session.trans_sid_tags" => urlscan::set_tags(Kind::Session, value),
        "session.trans_sid_hosts" => urlscan::set_hosts(Kind::Session, value),
        "url_rewriter.tags" => urlscan::set_tags(Kind::Output, value),
        "url_rewriter.hosts" => urlscan::set_hosts(Kind::Output, value),
        _ => {}
    }
}

/// Runs the on-modify handler of a rewriter directive: the deprecation
/// (prefixed like `php_error_docref`), then the table update.
pub(crate) fn on_modify(vm: &mut Vm, name: &str, value: &[u8]) -> VmResult<()> {
    if let Some(msg) = deprecation(name, value) {
        docref(vm, 8192, msg.as_bytes())?;
    }
    apply(name, value);
    Ok(())
}

/// `MINIT` of the rewriter: applies the configured values.
pub(crate) fn startup(vm: &mut Vm) {
    let mut startup = Vec::new();
    for name in DIRECTIVES {
        let v = vm.ini.raw(name).map(<[u8]>::to_vec).unwrap_or_default();
        let _ = on_modify(vm, name, &v);
        startup.push((name, v));
    }
    STARTUP.with(|s| *s.borrow_mut() = startup);
}

/// Request end: the rewriter state is dropped and directives changed at
/// runtime are restored (running their handlers, as `zend_ini_deactivate`).
pub(crate) fn request_shutdown(vm: &mut Vm) {
    urlscan::request_shutdown();
    let startup = STARTUP.with(|s| s.borrow().clone());
    let modified = MODIFIED.with(|m| std::mem::take(&mut *m.borrow_mut()));
    for (name, orig) in startup {
        if modified.iter().any(|m| m == name) {
            let _ = on_modify(vm, name, &orig);
        }
    }
}

/// Refreshes `arg_separator.output` and `$_SERVER['HTTP_HOST']` before
/// adding a variable or processing buffered output.
fn refresh_env(vm: &mut Vm) {
    let sep = vm
        .ini_string("arg_separator.output")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| b"&".to_vec());
    let host = {
        let mut g = vm.globals.borrow_mut();
        g.find(b"_SERVER").and_then(|i| {
            match g
                .slots
                .get(i as usize)
                .and_then(Option::as_ref)
                .map(Value::deref)
            {
                Some(Value::Array(a)) => {
                    match a.get(&ArrayKey::from("HTTP_HOST")).map(Value::deref) {
                        Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                        _ => None,
                    }
                }
                _ => None,
            }
        })
    };
    urlscan::set_env(sep, host);
}

/// `php_url_scanner_add_var_impl()` including the handler start.
fn add_var(vm: &mut Vm, kind: Kind, name: &[u8], value: &[u8]) {
    refresh_env(vm);
    if urlscan::add_var(kind, name, value, true) {
        output::start(
            vm,
            StartHandler::Internal {
                name: b"URL-Rewriter".to_vec(),
                func: urlscan::output_handler(kind),
            },
            0,
            flags::STDFLAGS,
        );
    }
}

/// `php_url_scanner_reset_session_var()` + `php_url_scanner_add_session_var()`.
pub(crate) fn session_var(vm: &mut Vm, name: &[u8], id: &[u8]) {
    refresh_env(vm);
    urlscan::reset_var(Kind::Session, name, true);
    add_var(vm, Kind::Session, name, id);
}

fn output_add_rewrite_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (name, value) = (a.str(0), a.str(1));
    add_var(vm, Kind::Output, name.as_bytes(), value.as_bytes());
    if let Some(t) = vm.pending_throw.take() {
        return Err(t);
    }
    Ok(Value::Bool(true))
}

#[allow(clippy::unnecessary_wraps)] // the native function signature
fn output_reset_rewrite_vars(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    urlscan::reset_vars(Kind::Output);
    Ok(Value::Bool(true))
}

/// The modify hook of `DIRECTIVES[I]`: the handler (deprecation, table
/// update) runs before `ini_set()` stores the value, and again with the
/// restored value after `ini_restore()`.
fn modify_hook<const I: usize>(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    let name = DIRECTIVES[I];
    set_modified(name, !crate::ini::restoring());
    on_modify(vm, name, value)?;
    Ok(true)
}

/// The hooks, in [`DIRECTIVES`] order.
const HOOKS: [php_vm::ext::IniModifyFn; 4] = [
    modify_hook::<0>,
    modify_hook::<1>,
    modify_hook::<2>,
    modify_hook::<3>,
];
