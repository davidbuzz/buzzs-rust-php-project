//! `ext/session` bindings for the phprust VM.
//!
//! The session semantics live in the `php-session` library; this crate
//! implements its [`php_session::SessionHost`] over the VM ([`host`]) and
//! registers:
//!
//! * the 23 `session_*()` functions ([`funcs`]);
//! * `SessionHandler` and the three session interfaces, and the
//!   `PHP_SESSION_*` constants ([`classes`]);
//! * the `session.*` INI directives, kept in sync between the library and
//!   the VM registry, with `ini_set()`/`ini_alter()`/`ini_restore()`
//!   wrapped so the library's on-modify handlers run ([`ini`]);
//! * `MINIT`+`RINIT` at registration (the VM has no request-startup hook
//!   and runs one request per process), `RSHUTDOWN` through
//!   `on_request_shutdown` ([`shutdown`]).
//!
//! `$_SESSION` is a compiled superglobal in the VM; the module binds the
//! global slot to the session array by reference, so writes from any scope
//! reach the session.

mod classes;
mod funcs;
mod headers;
mod host;
mod ini;
mod shutdown;

use php_runtime::{ArrayKey, PropertyPurpose, RuntimeContext, Value};
use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::object::class_of;
use php_vm::Vm;

use crate::host::{set_phase, with_host, Phase};

/// The `session` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Session;

impl Extension for Session {
    fn name(&self) -> &'static str {
        "session"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        ini::normalize_config(m.vm());
        ini::register(m);
        funcs::register(m);
        classes::register(m);
        ini::install_hooks(m);
        m.on_request_shutdown(shutdown::request_shutdown);
        startup(m.vm());
    }
}

/// `MINIT` (configuration values) followed by `RINIT`
/// (`session.auto_start`).
fn startup(vm: &mut Vm) {
    set_phase(Phase::Startup);
    let config: Vec<(&str, Option<Vec<u8>>)> = php_session::ini::DIRECTIVES
        .iter()
        .map(|d| (d.name, ini::config_value(vm, d.name)))
        .collect();
    let lookup = move |name: &str| {
        config
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| v.clone())
    };
    let err = with_host(vm, |h| {
        php_session::module_startup(h, &lookup);
        let r = php_session::request_startup(h);
        let late = h.state().take_exception();
        r.err().or(late)
    });
    // An exception during startup cannot be caught by PHP code; it is
    // dropped like PHP drops exceptions thrown before the script starts.
    drop(err);
    set_phase(Phase::Running);
}

/// `zend_get_callable_name()` with closures named after their function
/// (`{closure:/path/file.php:12}`).
pub(crate) fn callable_name(vm: &mut Vm, callable: &Value) -> String {
    if let Value::Object(o) = callable.deref() {
        if class_of(&o).lc_name.as_bytes() == b"closure" {
            if let Ok(Some(t)) = vm.object_properties(&o, PropertyPurpose::Debug) {
                for key in ["name", "function"] {
                    if let Some(Value::Str(s)) = t.get(&ArrayKey::from(key)) {
                        return s.to_string_lossy();
                    }
                }
            }
        }
    }
    php_session::user::callable_name(callable)
}
