use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn string_callbacks_prefer_compatible_instance_magic_method() {
    assert_eq!(
        run(r#"<?php
class Base {
    private function hidden() {}
    function __call($name, $args) { echo "instance:", $name, ":", get_class($this), "\n"; }
    static function __callStatic($name, $args) { echo "static:", $name, "\n"; }
}
class Child extends Base {
    function test() {
        call_user_func("Base::missing");
        call_user_func(["Base", "missing"]);
        call_user_func([$this, "missing"]);
        Base::missing();
        self::missing();
        call_user_func("Base::hidden");
    }
}
(new Child)->test();
call_user_func("Base::missing");
call_user_func(["Base", "missing"]);
class MagicChild extends Base {
    function __call($name, $args) { echo "override:", $name, "\n"; }
    function test() { call_user_func("Base::missing"); }
}
(new MagicChild)->test();
"#),
        "instance:missing:Child\n\
         instance:missing:Child\n\
         instance:missing:Child\n\
         instance:missing:Child\n\
         instance:missing:Child\n\
         instance:hidden:Child\n\
         static:missing\n\
         static:missing\n\
         override:missing\n"
    );
}

#[test]
fn namespaced_call_user_func_keeps_frame_when_reference_warning_throws() {
    assert_eq!(
        run(r#"<?php
namespace Foo;
function handler($level, $message) { throw new \Exception($message); }
function target(array &$ref) { echo "unreachable\n"; }
set_error_handler(__NAMESPACE__ . "\\handler");
for ($i = 0; $i < 2; $i++) {
    try { call_user_func(__NAMESPACE__ . "\\target", "bad"); }
    catch (\Exception $e) {
        echo $e->getMessage(), "\n";
        foreach ($e->getTrace() as $frame) {
            echo $frame["function"], ": ", isset($frame["file"]) ? "user" : "internal", "\n";
        }
    }
}
"#),
        "Foo\\target(): Argument #1 ($ref) must be passed by reference, value given\n\
         Foo\\handler: internal\n\
         call_user_func: user\n\
         Foo\\target(): Argument #1 ($ref) must be passed by reference, value given\n\
         Foo\\handler: internal\n\
         call_user_func: user\n"
    );
}

#[test]
fn anonymous_readonly_write_error_hides_internal_name_suffix() {
    assert_eq!(
        run(r#"<?php
$readonly = new readonly class {
    public int $field;
    function __construct() { $this->field = 2; }
};
var_dump($readonly->field);
try { $readonly->field = 123; }
catch (Throwable $e) { echo $e::class, ": ", $e->getMessage(), "\n"; }
var_dump($readonly->field);
$mutable = new class { public int $field = 2; };
$mutable->field = 123;
var_dump($mutable->field);
"#),
        "int(2)\n\
         Error: Cannot modify readonly property class@anonymous::$field\n\
         int(2)\n\
         int(123)\n"
    );
}

#[test]
fn namespace_constant_deprecation_uses_class_like_kind_and_declaration_line() {
    assert_eq!(
        run(r#"<?php
class C { const NAMESPACE = 'class'; }
interface I { const Namespace = 'interface'; }
trait T { const namespace = 'trait'; }
enum E { const NAMESPACE = 'enum'; }
echo C::NAMESPACE, " ", I::Namespace, " ", E::NAMESPACE, "\n";
"#),
        "\nDeprecated: Declaring class constant called 'namespace' is deprecated in t.php on line 2\n\
         \nDeprecated: Declaring interface constant called 'namespace' is deprecated in t.php on line 3\n\
         \nDeprecated: Declaring trait constant called 'namespace' is deprecated in t.php on line 4\n\
         \nDeprecated: Declaring enum constant called 'namespace' is deprecated in t.php on line 5\n\
         class interface enum\n"
    );
}

#[test]
fn unset_with_null_offset_is_silent_but_nested_fetch_is_not() {
    assert_eq!(
        run(r#"<?php
$a = ['' => 1, 'k' => ['' => 2, 'x' => 3]];
unset($a[null]);
var_dump(isset($a['']));
$b = ['' => ['x' => 1]];
unset($b[null]['x']);
var_dump($b);
"#),
        "bool(false)\n\
         \nDeprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 6\n\
         array(1) {\n  [\"\"]=>\n  array(0) {\n  }\n}\n"
    );
}

#[test]
fn get_defined_functions_argument_is_deprecated() {
    assert_eq!(
        run(r#"<?php
var_dump(is_array(get_defined_functions()['internal']));
var_dump(is_array(get_defined_functions(false)['user']));
var_dump(is_array(get_defined_functions(exclude_disabled: true)['user']));
"#),
        "bool(true)\n\
         \nDeprecated: get_defined_functions(): The $exclude_disabled parameter has no effect since PHP 8.0 in t.php on line 3\n\
         bool(true)\n\
         \nDeprecated: get_defined_functions(): The $exclude_disabled parameter has no effect since PHP 8.0 in t.php on line 4\n\
         bool(true)\n"
    );
}

#[test]
fn generator_constructor_and_destructor_are_deprecated() {
    // constructor_destructor_return: raised after the parameters
    // (CG(zend_lineno) is the last parameter's line).
    assert_eq!(
        run("<?php\nclass Gen {\n    public function __construct(\n        $a = 1\n    ) { yield ''; }\n    public function __destruct() { yield ''; }\n    public function other() { yield ''; }\n}\n"),
        "\nDeprecated: Making a constructor a Generator is deprecated in t.php on line 4\n\nDeprecated: Making a destructor a Generator is deprecated in t.php on line 6\n"
    );
}
