// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/spl` for the phprust VM.
//!
//! The SPL classes, their object handlers and the `spl_*`/`iterator_*`
//! functions are implemented by the `php-spl` library over
//! [`php_spl::SplHost`]; this crate implements that host trait over the
//! [`Vm`] ([`host::VmHost`]), registers every class of
//! [`php_spl::classes::class_table`] generically (one native handler
//! routes all methods to [`php_spl::call_method`]) and installs the SPL
//! object handlers as [`ClassHooks`].

pub mod dispatch;
pub mod files;
pub mod funcs;
pub mod hooks;
pub mod host;
pub mod iter;

use php_vm::ext::{Extension, InfoItem, ModuleBuilder};
use php_vm::Vm;

/// The `SPL` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spl;

impl Extension for Spl {
    fn name(&self) -> &'static str {
        "SPL"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        files::install();
        iter::register_internal_iterator(m.vm());
        dispatch::register_classes(m.vm());
        funcs::register(m);
        m.info(info);
        // Ahead of the stream layer's shutdown hook (which closes all
        // resources; it was registered earlier, and the hooks run in
        // reverse): see `files::shutdown`.
        m.on_request_shutdown(files::shutdown);
        // `PHP_RINIT(spl)`: the autoloader registry is the request's (a
        // server process runs many requests on one thread).
        // The class cache of the `create_object` hook too (class addresses
        // are reused from one request to the next).
        m.on_request_startup(|_| {
            php_spl::functions::reset_request();
            hooks::reset_bases();
            host::reset_request();
            files::request_startup();
        });
    }
}

/// `PHP_MINFO_FUNCTION(spl)`: the interface and class lists in
/// `spl_classes()` order.
fn info(_vm: &mut Vm) -> Vec<InfoItem> {
    let php_runtime::Value::Array(all) = php_spl::functions::spl_classes() else {
        return Vec::new();
    };
    let (mut interfaces, mut classes) = (Vec::new(), Vec::new());
    for name in all.values() {
        let php_runtime::Value::Str(n) = name else {
            continue;
        };
        let n = n.to_string_lossy();
        let is_interface = php_spl::classes::find_class(&n)
            .is_some_and(|c| c.kind == php_spl::classes::ClassKind::Interface);
        if is_interface {
            interfaces.push(n);
        } else {
            classes.push(n);
        }
    }
    let row = |k: &str, v: String| InfoItem::Row(vec![k.to_string(), v]);
    vec![
        InfoItem::TableStart,
        row("SPL support", "enabled".to_string()),
        row("Interfaces", interfaces.join(", ")),
        row("Classes", classes.join(", ")),
        InfoItem::TableEnd,
    ]
}
