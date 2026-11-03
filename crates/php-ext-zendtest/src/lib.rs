//! `ext/zend_test` for the phprust VM: the engine's own test extension
//! (the functions, classes, enums, attributes, INI settings and observer
//! hooks that php-src's `.phpt` tests rely on through `--EXTENSIONS--
//! zend_test`). Ref: ext/zend_test/{test.c,test.stub.php,observer.c,
//! fiber.c,iterators.c,object_handlers.c}.

use php_vm::ext::{Extension, ModuleBuilder};

/// The `zend_test` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZendTest;

impl Extension for ZendTest {
    fn name(&self) -> &'static str {
        "zend_test"
    }

    fn register(&self, _m: &mut ModuleBuilder<'_>) {}
}
