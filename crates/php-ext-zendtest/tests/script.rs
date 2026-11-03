//! End-to-end tests: PHP scripts run on a VM with the `zend_test`
//! extension.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = php_ext_zendtest::ZendTest;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn plain_functions() {
    let out = run(r#"echo zend_test_zend_ini_parse_quantity("1K"), '|',
        strlen(zend_test_zstr_init_literal()), '|',
        zend_get_current_func_name(), '|',
        ZendTestNS2\namespaced_func() ? 'y' : 'n', '|', ZendTestNS2\ZendSubNS\ZEND_CONSTANT_A;"#);
    assert_eq!(out, "1024|7|main|y|namespaced");
}

#[test]
fn deprecations() {
    let out = run("zend_test_deprecated(); echo ZEND_TEST_DEPRECATED;");
    assert_eq!(
        out,
        "\nDeprecated: Function zend_test_deprecated() is deprecated in test.php on line 1\n\
         \nDeprecated: Constant ZEND_TEST_DEPRECATED is deprecated in test.php on line 1\n42"
    );
}

#[test]
fn zpp_probes() {
    let out = run(r#"echo zend_class_name("stdclass"), '|';
        try { zend_class_name(42); } catch (TypeError $e) { echo $e->getMessage(), '|'; }
        try { zend_resource_or_null(1.5); } catch (TypeError $e) { echo $e->getMessage(), '|'; }
        echo zend_obj_stdclass_or_int("15"), '|', zend_obj_or_class_name(new stdClass);"#);
    assert_eq!(
        out,
        "stdClass|zend_class_name(): Argument #1 ($param) must be a valid class name, int given|\
         zend_resource_or_null(): Argument #1 ($param) must be of type resource or null, float given|\
         15|stdClass"
    );
}

#[test]
fn enums_and_classes() {
    let out = run(
        r#"function sizeof_cases(array $a): int { $n = 0; foreach ($a as $_) { $n++; } return $n; }
        echo ZendTestStringEnum::from("Test2")->name, '|', ZendTestIntEnum::Baz->value, '|',
        sizeof_cases(ZendTestUnitEnum::cases()), '|', _ZendTestClass::is_object(), '|',
        (new _ZendTestClass)->intProp, '|', _ZendTestClassWithTraits::ZEND_TRAIT_CONST2, '|',
        (new _ZendTestClassWithTraits)->traitMethod(), '|', _ZendTestClassWithTraits::$staticTraitProp, '|',
        (new _ZendTestMagicCall)->foo(1)[0];"#,
    );
    assert_eq!(out, "Bar|-1|2|42|123|321|789|999|foo");
}

#[test]
fn weakmap() {
    let out = run(
        r#"function n(array $a): int { $n = 0; foreach ($a as $_) { $n++; } return $n; }
        $a = new stdClass; $b = new stdClass;
        var_dump(zend_weakmap_attach($a, 1), zend_weakmap_attach($a, 2), zend_weakmap_attach($b, 3));
        unset($a); echo n(zend_weakmap_dump()), zend_weakmap_remove($b) ? 'r' : '-', n(zend_weakmap_dump());"#,
    );
    assert_eq!(out, "bool(true)\nbool(false)\nbool(true)\n1r0");
}

#[test]
fn object_handlers() {
    let out = run(r#"$a = new DoOperationNoCast(5); $b = $a * 3 + 1;
        var_dump($b instanceof DoOperationNoCast);
        $d = new DimensionHandlersNoArrayAccess; isset($d['k']); var_dump($d->has, $d->offset);"#);
    assert_eq!(out, "bool(true)\nbool(true)\nstring(1) \"k\"\n");
}

#[test]
fn ini_directives() {
    let out = run(
        r#"var_dump(ini_get("zend_test.not_empty_str_test"), ini_set("zend_test.not_empty_str_test", ""),
        ini_set("zend_test.str_test", "abc"), zend_test_zend_ini_str());"#,
    );
    assert_eq!(
        out,
        "string(3) \"val\"\nbool(false)\nstring(0) \"\"\nstring(3) \"abc\"\n"
    );
}
