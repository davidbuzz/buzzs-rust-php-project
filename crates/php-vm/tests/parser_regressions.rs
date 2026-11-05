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

/// `ZEND_SEND_UNPACK` raises a `TypeError`; `ZEND_ADD_ARRAY_UNPACK` appends
/// numeric string keys; constant expressions spread arrays only
/// (`zend_ast_add_unpacked_element`); `zend_try_ct_eval_array` rejects a
/// constant non-array spread at compile time.
#[test]
fn unpack_errors_and_numeric_string_keys() {
    let out = run(r#"<?php
function t(...$a) {}
try { t(...null); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
function g() { yield "42" => 1; yield "foo" => 2; yield 7 => 3; }
var_dump([...g()]);
class It implements IteratorAggregate { function getIterator(): Iterator { yield 1; } }
const A = [...[1, 2], ...['k' => 3]];
var_dump(A);
const C = [...new It];
"#);
    assert!(
        out.starts_with(
            "TypeError: Only arrays and Traversables can be unpacked, null given\n\
             array(3) {\n  [0]=>\n  int(1)\n  [\"foo\"]=>\n  int(2)\n  [1]=>\n  int(3)\n}\n\
             array(3) {\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n  [\"k\"]=>\n  int(3)\n}\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "Fatal error: Uncaught Error: Only arrays can be unpacked in constant expression in t.php:9"
        ),
        "{out}"
    );
    let out = run("<?php\necho 1;\nvar_dump([...42]);\n");
    assert!(
        out.contains("Fatal error: Only arrays and Traversables can be unpacked, int given in t.php on line 3"),
        "{out}"
    );
    assert!(!out.starts_with('1'), "{out}");
}

/// `zend_ast_is_short_circuited`: a nullsafe chain passed as an argument,
/// through `X::$p` / `X::m()`, inside `isset`/`empty`/`??` (fetched in
/// `BP_VAR_IS` mode) or iterated by reference behaves as one value; taking a
/// reference of it is a compile error.
#[test]
fn nullsafe_chains_short_circuit_as_one_value() {
    let out = run(r#"<?php
class Foo { public $bar; function n() { echo "n\n"; return null; } }
var_dump(null?->bar()->baz);
var_dump(null?->bar()::$baz);
var_dump(null?->bar()::baz(print("no\n")));
var_dump((new Foo)->n()?->x()::$y);
var_dump(isset($foo?->bar), empty($foo?->bar->baz), $foo?->bar ?? 'd');
$o = new Foo;
foreach ($o?->bar as &$v) {}
$o->bar = [1];
foreach ($o?->bar as &$v) { $v++; }
var_dump($o->bar);
"#);
    assert_eq!(
        out,
        "NULL\nNULL\nNULL\nn\nNULL\nbool(false)\nbool(true)\nstring(1) \"d\"\n\n\
         Warning: foreach() argument must be of type array|object, null given in t.php on line 9\n\
         array(1) {\n  [0]=>\n  int(1)\n}\n"
    );
    for src in [
        "<?php\nfunction &f($o) {\n    return $o?->bar;\n}\n",
        "<?php\nfunction &g($o) {\n    yield $o->y?->y;\n}\n",
        "<?php\n[&$y] = $y?->y->y;\n",
    ] {
        let out = run(src);
        assert!(
            out.contains("Fatal error: Cannot take reference of a nullsafe chain in t.php on line"),
            "{src}: {out}"
        );
    }
}

/// `list()` semantics of `zend_compile_list_assign` / `ZEND_FETCH_LIST`_[RW]:
/// strings are not destructured; a by-value call result notices; foreach
/// with a referencing list iterates by reference; the source is made a
/// reference before any target rebinds it; the result of a referencing
/// list assignment can be passed by reference.
#[test]
fn list_reference_semantics() {
    let out = run(r#"<?php
[$a, $b] = "hi";
var_dump($a);
function no_ref($a) { return $a; }
function &ref_return(&$a) { return $a; }
$x = [1, 2];
[&$v] = no_ref($x);
[[&$w]] = no_ref([[1]]);
[, &$v] = ref_return($x);
$v = 9;
echo $x[1], "\n";
$c = [[1, 2], [3, 4]];
foreach ($c as [&$p, $q]) { $p++; $q++; }
echo $c[0][0], $c[0][1], $c[1][0], "\n";
$a = [1, 2];
$r =& $a;
[&$a, &$b] = $a;
$a++; $b++;
echo $r[0], $r[1], "\n";
function change(&$ref) { $ref = 'changed'; }
$arr = [1];
change([&$e] = $arr);
var_dump($arr);
"#);
    let notice =
        "\nNotice: Attempting to set reference to non referenceable value in t.php on line";
    let string = "\nWarning: Cannot use string as array in t.php on line 2\n";
    assert_eq!(
        out,
        format!(
            "{string}{string}NULL\n{notice} 7\n{notice} 8\n{notice} 8\n9\n224\n23\nstring(7) \"changed\"\n"
        )
    );
}

/// `zend_try_compile_list_assign_from_array`: the values are assigned
/// directly, so a dropped value is released at its assignment, and the
/// pending values are released before a handler runs (`cleanup_live_vars`).
#[test]
fn list_from_array_literal_lifetimes() {
    let out = run(r#"<?php
class D {
    function __construct(public $n) {}
    function __destruct() { echo "destruct $this->n\n"; }
}
final class Boom { function __destruct() { throw new Exception('boom'); } }
function boom() { throw new Exception('boom'); }
$o = new D('o');
[$o, $p] = [new D('new o'), $o];
echo $p->n, "\n";
unset($o, $p);
$b = 0;
try { [$a, $a, $b] = [new Boom, new stdClass, 42]; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($b);
try { [$a, $b] = [new D('first'), boom()]; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { [$arr[boom()], $b] = [new D('v1'), new D('v2')]; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
echo "done\n";
"#);
    assert_eq!(
        out,
        "o\ndestruct new o\ndestruct o\nboom\nint(0)\ndestruct first\nboom\n\
         destruct v1\ndestruct v2\nboom\ndone\n"
    );
}

/// Array literal keys: `zend_try_ct_eval_array` rejects an array key at
/// compile time; at runtime `ZEND_ADD_ARRAY_ELEMENT` throws
/// `Cannot access offset of type T on array`.
#[test]
fn array_literal_offset_type_errors() {
    let out = run("<?php\necho 1;\n$a = [[] => 1];\n");
    assert!(
        out.contains("Fatal error: Illegal offset type in t.php on line 3")
            && !out.starts_with('1'),
        "{out}"
    );
    let out = run(r#"<?php
$k = [];
try { $a = [1, $k => 1]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { $a = [new stdClass => 1]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
const C1 = 1;
try { $a = [C1, [] => 1]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "Cannot access offset of type array on array\n\
         Cannot access offset of type stdClass on array\n\
         Cannot access offset of type array on array\n"
    );
}

/// `ZEND_FETCH_CLASS_CONSTANT` resolves the class before it checks the
/// type of a dynamic constant name (Zend/tests/constants/gh10486.phpt).
#[test]
fn dynamic_class_constant_fetch_resolves_class_first() {
    let out = run(r#"<?php
class A { const X = 1; }
try { y::{5}::y; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { A::{5}; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "Error: Class \"y\" not found\nTypeError: Cannot use value of type int as class constant name\n"
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

/// `zend_is_first_statement(ast, /* allow_nop */ true)`: empty statements
/// (`;`, a bare `?>`) may precede `declare(strict_types=1)`, other code may
/// not (Zend/tests/declare/gh19719.phpt).
#[test]
fn empty_statements_may_precede_strict_types() {
    let out = run(
        "<?php\n// comment\n?>\n<?php\n;\ndeclare(strict_types=1);\nfunction takesInt(int $x) {}\ntry { takesInt('42'); } catch (TypeError $e) { echo \"strict\\n\"; }\n",
    );
    assert_eq!(out, "strict\n");
    let out = run("<?php\necho 1;\ndeclare(strict_types=1);\n");
    assert!(
        out.contains(
            "Fatal error: strict_types declaration must be the very first statement in the script in t.php on line 3"
        ),
        "{out}"
    );
    let out = run("leading html\n<?php\ndeclare(strict_types=1);\n");
    assert!(
        out.contains("Fatal error: strict_types declaration must be the very first statement"),
        "{out}"
    );
}

/// `zend_eval_const_expr` + `zend_compile_const_expr_class_const`: the
/// class of `(expr)::NAME` in a constant expression is folded first
/// (oss-fuzz #59764).
#[test]
fn const_expr_class_operand_is_folded_before_the_dynamic_check() {
    for (expr, msg) in [
        ("[]::{A}", "Class name must be a valid object or a string"),
        ("[]::A", "Illegal class name"),
        ("(1 + 1)::A", "Illegal class name"),
        (
            "$x::A",
            "Dynamic class names are not allowed in compile-time class constant references",
        ),
    ] {
        assert_eq!(
            run(&format!("<?php\nconst B = {expr};\n")),
            format!("\nFatal error: {msg} in t.php on line 2\n"),
            "{expr}"
        );
    }
}
