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
pub mod funcs;
pub mod hooks;
pub mod host;
pub mod iter;

use php_vm::ext::{Extension, ModuleBuilder};

/// The `SPL` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spl;

impl Extension for Spl {
    fn name(&self) -> &'static str {
        "SPL"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        iter::register_internal_iterator(m.vm());
        dispatch::register_classes(m.vm());
        funcs::register(m);
    }
}
