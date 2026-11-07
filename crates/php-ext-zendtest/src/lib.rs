//! `ext/zend_test` for the phprust VM: the engine's own test extension
//! (the functions, classes, enums, attributes, INI settings and observer
//! hooks that php-src's `.phpt` tests rely on through `--EXTENSIONS--
//! zend_test`). Ref: `ext/zend_test/{test.c,test.stub.php,observer.c,
//! fiber.c,iterators.c,object_handlers.c}`.
//!
//! Native function handlers share the `NativeFn` signature, so the ones
//! that cannot fail still return `VmResult`.
#![allow(clippy::unnecessary_wraps)]
//!
//! Layout:
//! * [`functions`] — the plain functions of `test.stub.php`;
//! * [`zpp`] — the `zend_<type>[_or_null][_slow_zpp]` argument-parsing
//!   probes (`Zend/tests/zpp`);
//! * [`classes`] — classes, interfaces, traits, enums and attribute
//!   classes (plus [`patch`], which edits a registered class in place for
//!   what [`php_vm::ext::ClassBuilder`] cannot express);
//! * [`iterators`], [`object_handlers`] — the auxiliary stubs;
//! * [`ini`] — the `zend_test.*` directives;
//! * [`mm`] — the custom ZendMM heap handlers (`zend_mm_custom_handlers.c`);
//! * [`post`] — `zend_test_handle_post_data()` (the SAPI POST handlers);
//! * [`observer`] — observer printer and user opcode handlers (`observer.c`).

mod classes;
mod compile;
mod functions;
mod ini;
mod iterators;
mod mm;
mod object_handlers;
#[cfg(feature = "observer")]
mod observer;
mod patch;
mod post;
mod stack;
mod state;
mod uri;
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
        mm::register(m);
        functions::register(m);
        zpp::register(m);
        classes::register(m);
        // fiber.c: `_ZendTestFiber` (its context switching is the VM's).
        php_vm::ext::register_zend_test_fiber(m.vm());
        iterators::register(m);
        object_handlers::register(m);
        uri::register(m);
        #[cfg(feature = "observer")]
        observer::register(m);
        m.info(info);
        m.on_request_startup(state::request_startup);
        m.on_request_shutdown(state::request_shutdown);
    }
}
