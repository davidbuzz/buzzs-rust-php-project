// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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
//! * upload progress: the multipart parser's events, before `RINIT`
//!   ([`upload`]).
//!
//! `$_SESSION` is a compiled superglobal in the VM; the module binds the
//! global slot to the session array by reference, so writes from any scope
//! reach the session.

mod classes;
mod funcs;
mod headers;
mod host;
mod info;
mod ini;
mod rewrite;
mod shutdown;
mod upload;
mod urlscan;

use php_runtime::{ArrayKey, PropertyPurpose, RuntimeContext, Value};
use php_session::SessionHost;
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
        ini::register(m);
        funcs::register(m);
        classes::register(m);
        rewrite::register(m);
        // `SID` is defined per request; registering it here attributes it
        // to this module (`get_defined_constants(true)`), it is removed
        // again at module startup.
        m.constant("SID", Value::string(""));
        m.info(info::module_info);
        m.on_module_startup(module_startup);
        m.on_request_startup(request_startup);
        m.on_request_shutdown(shutdown::request_shutdown);
        m.on_rfc1867(upload::callback);
    }
}

/// `MINIT`: applies the configuration values (`php.ini`/`-d`) to the
/// session directives, then those of the URL rewriter.
fn module_startup(vm: &mut Vm) {
    vm.constants.remove(&php_runtime::ZStr::from("SID"));
    set_phase(Phase::Startup);
    host::note_module_startup();
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
    with_host(vm, |h| {
        php_session::module_startup(h, &lookup);
        h.state().take_exception();
    });
    rewrite::startup(vm);
    vm.pending_throw = None;
    set_phase(Phase::Running);
}

/// `RINIT`: resolves the module/serializer and honours
/// `session.auto_start`.
fn request_startup(vm: &mut Vm) {
    // `SID` is a request constant (php-src registers it without
    // `CONST_PERSISTENT`, so it goes with the request): the one the
    // previous request of a long-running worker defined is gone, and so is
    // its deprecation.
    let sid = php_runtime::ZStr::from("SID");
    vm.constants.remove(&sid);
    vm.deprecated_constants.remove(&sid);
    set_phase(Phase::RequestStartup);
    let err = with_host(vm, |h| {
        let r = php_session::request_startup(h);
        // `PG(modules_activated)`: request startup has completed. Until the
        // request ends, a later `request_parse_body()` must not report to
        // the session's upload progress.
        h.session().set_modules_activated(true);
        let late = h.state().take_exception();
        r.err().or(late)
    });
    // An exception during startup cannot be caught by PHP code; it is
    // dropped like PHP drops exceptions thrown before the script starts.
    drop(err);
    vm.pending_throw = None;
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
