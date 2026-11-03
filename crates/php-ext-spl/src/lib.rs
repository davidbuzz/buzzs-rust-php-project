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
        iter::register_internal_iterator(m.vm());
        dispatch::register_classes(m.vm());
        funcs::register(m);
        m.info(info);
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
