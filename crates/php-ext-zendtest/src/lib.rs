//! `ext/zend_test` for the phprust VM: the engine's own test extension
//! (the functions, classes, enums, attributes, INI settings and observer
//! hooks that php-src's `.phpt` tests rely on through `--EXTENSIONS--
//! zend_test`). Ref: ext/zend_test/{test.c,test.stub.php,observer.c,
//! fiber.c,iterators.c,object_handlers.c}.
//!
//! Layout:
//! * [`functions`] — the plain functions of `test.stub.php`;
//! * [`zpp`] — the `zend_<type>[_or_null][_slow_zpp]` argument-parsing
//!   probes (`Zend/tests/zpp`);
//! * [`classes`] — classes, interfaces, traits, enums and attribute
//!   classes (plus [`patch`], which edits a registered class in place for
//!   what [`php_vm::ext::ClassBuilder`] cannot express);
//! * [`iterators`], [`object_handlers`] — the auxiliary stubs;
//! * [`ini`] — the `zend_test.*` directives.

mod classes;
mod compile;
mod functions;
mod ini;
mod iterators;
mod object_handlers;
#[cfg(feature = "observer")]
mod observer;
mod patch;
mod stack;
mod state;
mod util;
mod zpp;

use php_vm::ext::{Extension, InfoItem, ModuleBuilder};
use php_vm::Vm;

/// The `zend_test` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZendTest;

/// `PHP_MINFO(zend_test)`.
fn info(_vm: &mut Vm) -> Vec<InfoItem> {
    vec![
        InfoItem::TableStart,
        InfoItem::Row(vec!["zend_test extension".into(), "enabled".into()]),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

impl Extension for ZendTest {
    fn name(&self) -> &'static str {
        "zend_test"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        ini::register(m);
        functions::register(m);
        zpp::register(m);
        classes::register(m);
        iterators::register(m);
        object_handlers::register(m);
        #[cfg(feature = "observer")]
        observer::register(m);
        m.info(info);
        m.on_request_startup(state::request_startup);
        m.on_request_shutdown(state::request_shutdown);
    }
}
