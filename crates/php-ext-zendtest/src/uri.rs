//! `zend_test_uri_parser()` (ref: ext/zend_test/test.c): exercises the
//! internal URI parser API of ext/uri (`php_ext_uri::internal_api`).

use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

/// `zend_test_uri_parser(string $uri, string $parser): array`.
fn zend_test_uri_parser(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let uri = a.str(0);
    let parser = a.str(1);
    if !php_ext_uri::internal_api::is_parser(parser.as_bytes()) {
        // `zend_argument_value_error(1, "Unknown parser")`.
        return Err(vm.throw(
            "ValueError",
            "zend_test_uri_parser(): Argument #1 ($uri) Unknown parser",
        ));
    }
    php_ext_uri::internal_api::zend_test_uri_parser(vm, uri.as_bytes(), parser.as_bytes())
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "zend_test_uri_parser(string $uri, string $parser): array",
        zend_test_uri_parser,
    );
}
