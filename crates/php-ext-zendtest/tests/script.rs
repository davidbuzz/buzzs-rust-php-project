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

/// Runs `code` with the streams extension loaded (for `fopen()`), under
/// `post_max_size=100K`.
fn run_with_streams(code: &str) -> String {
    let sink = Sink::default();
    let zt = php_ext_zendtest::ZendTest;
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 2] = [&streams, &zt];
    let opts = VmOptions {
        ini: vec![("post_max_size".into(), "100K".into())],
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn handle_post_data_urlencoded_and_post_max_size() {
    let out = run_with_streams(
        r#"
$s = fopen('php://memory', 'w+');
fwrite($s, 'a=hello&b[x]=1');
rewind($s);
zend_test_handle_post_data($s, 'application/x-www-form-urlencoded', 0);
var_dump($_POST['a'], $_POST['b']['x'], $_FILES);
$big = 'aaaaaaaa';
for ($i = 0; $i < 17; $i++) { $big .= $big; }
$s = fopen('php://memory', 'w+');
fwrite($s, 'a=' . $big);
rewind($s);
zend_test_handle_post_data($s, 'application/x-www-form-urlencoded', 0);
var_dump(strlen($_POST['a']));
zend_test_handle_post_data($s, 'application/x-www-form-urlencoded', 200 * 1024);
var_dump($_POST);
"#,
    );
    assert!(
        out.starts_with("string(5) \"hello\"\nstring(1) \"1\"\narray(0) {\n}\n"),
        "{out}"
    );
    assert!(
        out.contains("Warning: zend_test_handle_post_data(): Actual POST length does not match Content-Length, and exceeds 102400 bytes in test.php"),
        "{out}"
    );
    // Seven 16 KiB blocks were read: the variable keeps all but "a=".
    assert!(out.contains("int(114686)\n"), "{out}");
    assert!(
        out.contains("Warning: zend_test_handle_post_data(): POST Content-Length of 204800 bytes exceeds the limit of 102400 bytes in test.php"),
        "{out}"
    );
    assert!(out.ends_with("array(0) {\n}\n"), "{out}");
}

#[test]
fn handle_post_data_multipart_stops_at_post_max_size() {
    let out = run_with_streams(
        r#"
function body($value) {
    $s = fopen('php://memory', 'w+');
    fwrite($s, "--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n");
    fwrite($s, $value);
    fwrite($s, "\r\n--b--\r\n");
    rewind($s);
    return $s;
}
zend_test_handle_post_data(body('hello'), 'multipart/form-data; boundary=b', 0);
var_dump($_POST['a']);
$big = 'aaaaaaaa';
for ($i = 0; $i < 17; $i++) { $big .= $big; }
zend_test_handle_post_data(body($big), 'multipart/form-data; boundary=b', 0);
var_dump(strlen($_POST['a'] ?? '') < 100 * 1024);
"#,
    );
    assert!(out.starts_with("string(5) \"hello\"\n"), "{out}");
    assert!(
        out.contains("\nWarning: Actual POST length does not match Content-Length, and exceeds 102400 bytes in test.php"),
        "{out}"
    );
    assert!(out.ends_with("bool(true)\n"), "{out}");
}

#[test]
fn stub_attributes_and_doc_comments_of_internal_symbols() {
    let out = run(r#"
$a = (new ReflectionFunction('zend_test_attribute_with_named_argument'))->getAttributes()[0];
echo $a->getName(), '|', $a->newInstance()->arg, '|';
$m = new ReflectionMethod(ZendAttributeTest::class, 'testMethod');
echo get_class($m->getAttributes()[0]->newInstance()), '|';
$d = (new ReflectionConstant('ZEND_TEST_ATTRIBUTED_CONSTANT'))->getAttributes()[0]->newInstance();
echo $d->since, '|', strlen((new ReflectionFunction('zend_test_nullable_array_return'))->getDocComment()), '|';
echo (new ReflectionClass(ZendTestPropertyAttribute::class))->isInternal() ? 'internal' : 'user';"#);
    assert_eq!(
        out,
        "ZendTestAttributeWithArguments|foo|ZendTestAttribute|version 1.5|82|internal"
    );
}

#[test]
fn internal_attributes_on_promoted_parameters_follow_their_targets() {
    // attribute-promotion-{parameter,property}-only.phpt
    let out = run(r#"
class AttrTest {
    public function __construct(
        #[ZendTestPropertyAttribute('foo')] public $prop,
        #[ZendTestParameterAttribute('bar')] public $param,
    ) {}
}
$r = new ReflectionClass(AttrTest::class);
foreach ($r->getConstructor()->getParameters() as $p) {
    echo 'param ', $p->getName(), ':';
    foreach ($p->getAttributes() as $a) { echo $a->getName(); }
    echo ' prop ', $p->getName(), ':';
    foreach ($r->getProperty($p->getName())->getAttributes() as $a) { echo $a->getName(); }
    echo ' ';
}"#);
    assert_eq!(
        out,
        "param prop: prop prop:ZendTestPropertyAttribute \
         param param:ZendTestParameterAttribute prop param: "
    );
}

#[test]
fn custom_attribute_validator_runs_at_compile_time() {
    // Zend/tests/attributes/016_custom_attribute_validation.phpt
    let out = run("#[ZendTestAttribute] function foo() {}");
    assert!(
        out.contains("Fatal error: Only classes can be marked with #[ZendTestAttribute]"),
        "{out}"
    );
}

#[test]
fn consumed_args_report_the_moved_arguments() {
    // consumed_args_basic.phpt
    let out = run(r#"
$r = zend_test_call_with_consumed_args(static fn ($a, $b) => $b, [[1], "x"], 1);
echo $r["consumed_args"], '|', $r["retval"], '|';
$r = zend_test_call_with_consumed_args(static fn ($a) => $a, [1], 0);
echo $r["consumed_args"];"#);
    assert_eq!(out, "1|x|0");
}

#[test]
fn throwing_resource_destructor_throws_where_released() {
    // gh17408.phpt
    let out = run(r#"
function test() {
    try { zend_test_create_throwing_resource(); } catch (Exception $e) { echo $e->getMessage(), '|'; }
    $r = zend_test_create_throwing_resource();
}
try { test(); } catch (Exception $e) { echo get_class($e), ':', $e->getLine(); }"#);
    assert_eq!(out, "Throwing resource destructor called|Exception:6");
}

#[test]
fn dnf_argument_errors_name_the_parameter() {
    // internal_dnf_arguments.phpt
    let out = run(r#"
try { zend_test_internal_dnf_arguments(new stdClass); } catch (TypeError $e) { echo $e->getMessage(); }"#);
    assert_eq!(
        out,
        "zend_test_internal_dnf_arguments(): Argument #1 ($arg) must be of type \
         Iterator|(Traversable&Countable), stdClass given"
    );
}

#[test]
fn zend_call_method_on_a_class_uses_it_as_called_scope() {
    // internal-call-internal-static-return.phpt
    let out = run(r#"
class StillReturnsStatic extends _ZendTestClass {}
echo get_class(zend_call_method(_ZendTestClass::class, "returnsStatic")), '|',
    get_class(zend_call_method(StillReturnsStatic::class, "returnsStatic"));"#);
    assert_eq!(out, "_ZendTestClass|StillReturnsStatic");
}

#[test]
fn internal_iterator_calls_and_destruction() {
    // iterators/double-rewind.phpt, gh14741.phpt
    let out = run(r#"
$it = (new \ZendTest\Iterators\TraversableTest())->getIterator();
var_dump($it);
foreach ($it as $k => $v) { if ($k == 1) break; }
try { clone $it; } catch (Error $e) { echo $e->getMessage(), "\n"; }
unset($it);
echo "end\n";"#);
    assert_eq!(
        out,
        "object(InternalIterator)#3 (0) {\n}\n\
         TraversableTest::rewind\nTraversableTest::valid\nTraversableTest::current\n\
         TraversableTest::key\nTraversableTest::next\nTraversableTest::valid\n\
         TraversableTest::current\nTraversableTest::key\n\
         Trying to clone an uncloneable object of class InternalIterator\n\
         TraversableTest::drop\nend\n"
    );
}

#[test]
fn interned_strings_equal_to_extension_names_are_not_marked_utf8() {
    // strings_marked_as_utf8.phpt: "foo" is ZendTestNS2\Foo's property name.
    let out = run(r#"
var_dump(zend_test_is_string_marked_as_valid_utf8("f" . "o" . "o"));
var_dump(zend_test_is_string_marked_as_valid_utf8("f" . "x" . "o"));
$f = "f"; $o = "o";
var_dump(zend_test_is_string_marked_as_valid_utf8($f . $o . $o));"#);
    assert_eq!(out, "bool(false)\nbool(true)\nbool(true)\n");
}

#[test]
fn forbidden_dynamic_calls() {
    // zend_forbid_dynamic_call.phpt
    let out = run(r#"
$o = new ZendTestForbidDynamicCall();
$o->call();
ZendTestForbidDynamicCall::callStatic();
foreach ([[$o, 'call'], [ZendTestForbidDynamicCall::class, 'callStatic']] as $c) {
    try { $c(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
try { call_user_func([$o, 'call']); } catch (Error $e) { echo $e->getMessage(), "\n"; }"#);
    assert_eq!(
        out,
        "Cannot call ZendTestForbidDynamicCall::call() dynamically\n\
         Cannot call ZendTestForbidDynamicCall::callStatic() dynamically\n\
         Cannot call ZendTestForbidDynamicCall::call() dynamically\n"
    );
}
