use php_runtime::{ArrayKey, PropertyPurpose, RuntimeContext, Value, ZStr};
use php_vm::ext::{Extension, ModuleBuilder};
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
fn reference_returns_verify_and_coerce_without_separating_bound_variables() {
    assert_eq!(run(r#"<?php
function value(&$v): string { return $v; }
function &local(int $v): string { return $v; }
function &bound(int &$v): string { return $v; }
$v = 123;
var_dump(value($v), $v, local($v), $v, bound($v), $v);
class S {
    public $v = "text";
    public function &__toString(): string { return $this->v; }
}
echo (string) new S, "\n";
"#), "string(3) \"123\"\nint(123)\nstring(3) \"123\"\nint(123)\nstring(3) \"123\"\nstring(3) \"123\"\ntext\n");
}

#[test]
fn array_access_incdec_writes_through_returned_typed_reference() {
    assert_eq!(run(r#"<?php
class A implements ArrayAccess {
    public int $value = 1;
    function &offsetGet($key): mixed { return $this->value; }
    function offsetSet($key, $value): void { echo "set\n"; }
    function offsetExists($key): bool { return true; }
    function offsetUnset($key): void {}
}
$a = new A;
var_dump($a[0]++, $a->value);
$a[0] += 3;
var_dump($a->value);
$a->value = PHP_INT_MAX;
try { ++$a[0]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($a->value === PHP_INT_MAX);
"#), "int(1)\nint(2)\nset\nint(2)\nCannot increment a reference held by property A::$value of type int past its maximal value\nbool(true)\n");
}

#[test]
fn folded_initializers_reject_weak_coercion_but_accept_widening() {
    assert_eq!(
        run("<?php class A {public float $v = 1; const float V = 2;} var_dump((new A)->v, A::V);"),
        "float(1)\nfloat(2)\n"
    );
    assert!(run("<?php class A {public int $v = \"1\";}")
        .contains("Cannot use string as default value for property A::$v of type int"));
    assert!(run("<?php class A {public int $v = null;}")
        .contains("Use the nullable type ?int to allow null default value"));
    assert!(run("<?php class A {const int V = \"1\";}")
        .contains("Cannot use string as value for class constant A::V of type int"));
}

#[test]
fn constant_covariance_waits_for_runtime_class_dependencies() {
    assert_eq!(
        run(r#"<?php
class S implements Stringable { function __toString(): string { return ""; } }
class A { const object V = VALUE; }
class B extends A { const S V = VALUE; }
define("VALUE", new S);
echo B::V instanceof S ? "ok\n" : "bad\n";
"#),
        "ok\n"
    );
    assert!(
        run("<?php class A {const int V = 1;} class B extends A {const string V = \"x\";}")
            .contains("Type of B::V must be compatible with A::V of type int")
    );
    assert!(
        run("<?php interface A {const int V = 1;} class B implements A {const V = 1;}")
            .contains("Type of B::V must be compatible with A::V of type int")
    );
}

#[test]
fn cloning_preserves_explicitly_unset_typed_property_state() {
    assert_eq!(
        run(r#"<?php
class A {
    public int $v;
    function __set($name, $value) { echo "set\n"; }
}
$a = new A;
unset($a->v);
$b = clone $a;
$b->v = 42;
$c = clone new A;
$c->v = 42;
var_dump($c->v);
"#),
        "set\nint(42)\n"
    );
}

#[test]
fn numeric_parent_hook_call_is_checked_after_constant_folding() {
    assert_eq!(
        run("<?php class A {} class B extends A {public $v {get {return parent::${1+1}::get();}}}"),
        ""
    );
    let output =
        run("<?php class A {} class B extends A {public $v {get {return parent::${0}::get();}}}");
    assert!(
        output.contains("Must not use parent::$0::get() in a different property ($v)"),
        "{output}"
    );
}

#[test]
fn unserialization_rejects_virtual_storage_without_rejecting_backed_hooks() {
    assert_eq!(run(r#"<?php
set_error_handler(function($level, $message) { echo $message, "\n"; });
class A { public $v { get => 1; set {} } }
var_dump(unserialize('O:1:"A":1:{s:1:"v";i:42;}'));
class B { public $v = 0; }
class C extends B { public $v { get => parent::$v::get(); } }
$c = unserialize('O:1:"C":1:{s:1:"v";i:42;}');
var_dump($c->v);
"#), "unserialize(): Cannot unserialize value for virtual property A::$v\nunserialize(): Error at offset 19 of 25 bytes\nbool(false)\nint(42)\n");
}

#[test]
fn proxy_reset_releases_instance_before_publishing_new_lazy_state() {
    assert_eq!(
        run(r#"<?php
class A {
    public $proxy;
    function __destruct() { echo (new ReflectionClass(A::class))->isUninitializedLazyObject($this->proxy) ? "lazy\n" : "plain\n"; }
}
$r = new ReflectionClass(A::class);
$p = $r->newLazyProxy(function($proxy) { $a = new A; $a->proxy = $proxy; return $a; });
$r->initializeLazyObject($p);
$r->resetAsLazyProxy($p, function() { return new A; });
"#),
        "plain\n"
    );
}

#[test]
fn gh20085_lazy_foreach_rebinding_propagates_uncaught_error() {
    let output = run(r#"<?php
class C {
    public int $a;
    public function __construct() { $this->a = 1; }
}
$obj = new C;
$reflector = new ReflectionClass(C::class);
foreach ($obj as &$value) {
    $obj = $reflector->newLazyGhost(function ($obj) { throw new Error; });
}
echo !obj;
"#);
    assert!(
        output.contains("Fatal error: Uncaught Error in t.php:"),
        "{output}"
    );
    assert!(output.contains("Stack trace:"), "{output}");
    assert!(output.contains("thrown in t.php on line"), "{output}");
    assert!(!output.contains("Undefined constant"), "{output}");
}

#[test]
fn lazy_foreach_rebinding_exceptions_leave_ghosts_and_proxies_retryable() {
    assert_eq!(
        run(r#"<?php
class C { public int $a = 1; }
$r = new ReflectionClass(C::class);
foreach ([false, true] as $proxy) {
    $o = new C;
    $alias =& $o;
    $attempts = 0;
    $initialize = function ($obj) use (&$attempts) {
        echo "init:", ++$attempts, "\n";
        throw new Error("lazy failure");
    };
    $steps = 0;
    try {
        foreach ($o as &$value) {
            echo "body:", ++$steps, ":", $value, "\n";
            if ($steps > 1) break;
            $alias = $proxy ? $r->newLazyProxy($initialize) : $r->newLazyGhost($initialize);
        }
        echo "missed\n";
    } catch (Error $e) { echo $e->getMessage(), "\n"; }
    unset($value);
    var_dump($r->isUninitializedLazyObject($o));
    try {
        foreach ($o as &$value) { echo "unexpected\n"; break; }
    } catch (Error $e) { echo $e->getMessage(), "\n"; }
    unset($value);
    foreach ([7] as &$value) { echo "after:", $value, "\n"; }
    unset($value, $alias);
}
"#),
        "body:1:1\ninit:1\nlazy failure\nbool(true)\ninit:2\nlazy failure\nafter:7\n\
         body:1:1\ninit:1\nlazy failure\nbool(true)\ninit:2\nlazy failure\nafter:7\n"
    );
}

#[test]
fn lazy_foreach_rebinding_restarts_at_the_resolved_property_table() {
    assert_eq!(
        run(r#"<?php
class C { public int $a = 1; public int $b = 2; }
$r = new ReflectionClass(C::class);
foreach ([false, true] as $proxy) {
    $o = new C;
    $steps = 0;
    foreach ($o as $key => &$value) {
        echo $key, ":", $value, "\n";
        if (++$steps === 1) {
            $o = $proxy
                ? $r->newLazyProxy(function ($obj) { echo "init\n"; $real = new C; $real->a = 3; $real->b = 4; return $real; })
                : $r->newLazyGhost(function ($obj) { echo "init\n"; $obj->a = 3; $obj->b = 4; });
        }
        if ($steps > 3) break;
        $value += 10;
    }
    unset($value);
    var_dump($o->a, $o->b);
}
"#),
        "a:1\ninit\na:3\nb:4\nint(13)\nint(14)\n\
         a:1\ninit\na:3\nb:4\nint(13)\nint(14)\n"
    );
}

#[test]
fn property_foreach_rebinding_refreshes_keys_without_changing_value_iteration() {
    assert_eq!(
        run(r#"<?php
class C { public int $a = 1; }
$o = new C;
$alias =& $o;
$steps = 0;
foreach ($o as $key => &$value) {
    echo $key, ":", $value, "\n";
    if (++$steps === 1) $alias = (object) ["b" => 2, "c" => 3];
    if ($steps > 3) break;
    $value += 10;
}
unset($value, $alias);
var_dump($o->b, $o->c);
$r = new ReflectionClass(C::class);
$o = new C;
$steps = 0;
foreach ($o as $value) {
    echo "value:", $value, "\n";
    if (++$steps === 1) {
        $o = $r->newLazyGhost(function ($obj) { throw new Error("must not initialize"); });
    }
    if ($steps > 1) break;
}
var_dump($r->isUninitializedLazyObject($o));
"#),
        "a:1\nb:2\nc:3\nint(12)\nint(13)\nvalue:1\nbool(true)\n"
    );
}

#[test]
fn property_foreach_rebinding_does_not_retain_the_previous_object() {
    assert_eq!(
        run(r#"<?php
class C {
    public int $a = 1;
    function __destruct() { echo "released\n"; }
}
$o = new C;
$steps = 0;
foreach ($o as &$value) {
    echo "body\n";
    if (++$steps === 1) $o = (object) [];
    if ($steps > 1) break;
    echo "rebound\n";
}
unset($value);
echo "done\n";
"#),
        "body\nreleased\nrebound\ndone\n"
    );
}

#[test]
fn non_jit_lazy_dynamic_compound_assignment_preserves_initializer_order() {
    assert_eq!(
        run(r##"<?php
#[AllowDynamicProperties]
class C {
    public int $b = 1;
    public function __construct() {
        var_dump(__METHOD__);
        $this->a = 1;
        $this->b = 3;
    }
}
function test(string $name, object $obj, ReflectionClass $reflector) {
    echo "# ", $name, ":\n";
    var_dump($reflector->isUninitializedLazyObject($obj));
    $obj->a += 1;
    var_dump($obj->a, $obj->b, $reflector->isUninitializedLazyObject($obj));
    var_dump($obj->a += 1, $obj->a, $obj->b);
}
$reflector = new ReflectionClass(C::class);
$obj = $reflector->newLazyGhost(function ($obj) {
    var_dump("initializer");
    $obj->__construct();
});
test("Ghost", $obj, $reflector);
$obj = $reflector->newLazyProxy(function ($obj) {
    var_dump("initializer");
    return new C();
});
test("Proxy", $obj, $reflector);
"##),
        "# Ghost:\n\
         bool(true)\n\
         string(11) \"initializer\"\n\
         string(14) \"C::__construct\"\n\
         int(2)\n\
         int(3)\n\
         bool(false)\n\
         int(3)\n\
         int(3)\n\
         int(3)\n\
         # Proxy:\n\
         bool(true)\n\
         string(11) \"initializer\"\n\
         string(14) \"C::__construct\"\n\
         int(2)\n\
         int(3)\n\
         bool(false)\n\
         int(3)\n\
         int(3)\n\
         int(3)\n"
    );
}

#[test]
fn lazy_initialization_detects_released_objects_and_chains_errors() {
    assert_eq!(
        run(r#"<?php
class C { public $s; }
$r = new ReflectionClass(C::class);

$o = $r->newLazyGhost(function ($obj) { global $o; $o = null; });
try { $o->s = new stdClass; } catch (Error $e) { echo $e->getMessage(), "\n"; }

$o = $r->newLazyProxy(function ($obj) { global $o; $o = null; return new C; });
try { $o->s = new stdClass; } catch (Error $e) { echo $e->getMessage(), "\n"; }

$o = $r->newLazyGhost(function ($obj) { global $o; $o = null; return new stdClass; });
try { $o->s = new stdClass; } catch (Error $e) {
    do { echo $e->getMessage(), "\n"; } while ($e = $e->getPrevious());
}
"#),
        "Lazy object was released during initialization\n\
         Lazy object was released during initialization\n\
         Lazy object was released during initialization\n\
         Lazy object initializer must return NULL or no value\n"
    );
}

#[test]
fn temporary_lazy_objects_can_initialize_before_cloning() {
    assert_eq!(
        run(r#"<?php
class C { public $value; function __clone() { echo "cloned\n"; } }
$r = new ReflectionClass(C::class);
clone $r->newLazyGhost(function ($o) {});
clone $r->newLazyProxy(fn() => new C);
"#),
        "cloned\ncloned\n"
    );
}

#[test]
fn lazy_hook_fetch_does_not_initialize_when_the_hook_ignores_state() {
    assert_eq!(
        run(r#"<?php
class C {
    public $value { get => 42; }
    public int $state = 1;
}
$r = new ReflectionClass(C::class);
$o = $r->newLazyGhost(function ($obj) { echo "initialized\n"; });
echo $o->value, "\n";
var_dump($r->isUninitializedLazyObject($o));
"#),
        "42\nbool(true)\n"
    );
}

#[test]
fn constructing_an_object_resolves_unevaluated_class_constants() {
    assert_eq!(
        run(r#"<?php
class A { const self V = VALUE; }
try { define("VALUE", new A); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "Undefined constant \"VALUE\"\n"
    );
}

#[test]
fn invalid_reference_return_reports_type_error_before_reference_notice() {
    assert_eq!(
        run(r#"<?php
set_error_handler(function($level, $message) { echo "notice: ", $message, "\n"; });
function &invalid(): array { return null; }
try { invalid(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#),
        "invalid(): Return value must be of type array, null returned\n"
    );
}

struct PropertyTables;

impl Extension for PropertyTables {
    fn name(&self) -> &'static str {
        "property_tables_test"
    }

    fn register(&self, module: &mut ModuleBuilder<'_>) {
        module.function("json_table(object $o): array", |vm, args| {
            let Value::Object(o) = args.at(0) else {
                panic!("object argument")
            };
            vm.object_properties(&o, PropertyPurpose::Json)
                .map(|table| Value::Array(table.expect("property table")))
                .map_err(|e| vm.throw_runtime_error(e))
        });
        module.function("export_table(object $o): array", |vm, args| {
            let Value::Object(o) = args.at(0) else {
                panic!("object argument")
            };
            vm.object_properties(&o, PropertyPurpose::VarExport)
                .map(|table| Value::Array(table.expect("property table")))
                .map_err(|e| vm.throw_runtime_error(e))
        });
        module.function(
            "prepare_untyped_table(object $o, string $key, mixed &$v): bool",
            |vm, args| {
                let Value::Object(o) = args.at(0) else {
                    panic!("object argument")
                };
                let Value::Str(key) = args.at(1) else {
                    panic!("table key")
                };
                let r = args.reference(2).expect("incoming reference");
                vm.prepare_property_table_reference(&o, &ArrayKey::Str(key), &r)?;
                assert!(r.meta().is_none());
                Ok(Value::Bool(true))
            },
        );
        module.function(
            "bind_table(object $o, mixed $key, mixed &$v): bool",
            |vm, args| {
                let Value::Object(o) = args.at(0) else {
                    panic!("object argument")
                };
                let key = match args.at(1) {
                    Value::Int(i) => ArrayKey::Int(i),
                    Value::Str(s) => ArrayKey::Str(s),
                    _ => panic!("table key"),
                };
                let r = args.reference(2).expect("incoming reference");
                let target = vm.prepare_property_table_reference(&o, &key, &r)?;
                // Emulate the native table writer, immediately on the resolved target.
                let name = key.to_zstr();
                let name = name.as_bytes().rsplit(|b| *b == 0).next().expect("name");
                let name = ZStr::from(name.to_vec()).to_string_lossy();
                vm.set_property_raw(&target, &name, Value::Reference(r.clone()));
                let table = vm
                    .object_properties(&target, PropertyPurpose::ArrayCast)
                    .map_err(|e| vm.throw_runtime_error(e))?
                    .expect("property table");
                let stored = table
                    .get(&key)
                    .or_else(|| table.get(&ArrayKey::Str(ZStr::from(name))));
                let Some(Value::Reference(stored)) = stored else {
                    panic!("stored reference")
                };
                assert!(php_runtime::PhpRef::ptr_eq(stored, &r));
                Ok(Value::Bool(true))
            },
        );
    }
}

fn run_property_tables(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(
        &VmOptions::default(),
        Box::new(sink.clone()),
        &[&PropertyTables],
    );
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn json_table_skips_nonpublic_hooks_including_inside_declaring_scope() {
    assert_eq!(run_property_tables(r#"<?php
#[AllowDynamicProperties]
class A {
    private int $secret = 7 { get { echo "private\n"; throw new Exception("private hook"); return $this->secret; } }
    protected $hidden { get { echo "protected\n"; throw new Exception("protected hook"); } }
    public int $backed = 3 { get { echo "public\n"; return $this->backed + 1; } }
    public $virtual { get => 5; }
    public int $uninitialized;
    function table() { return json_table($this); }
}
$a = new A;
$a->dynamic = 6;
$t = json_table($a);
var_dump($t["backed"], $t["virtual"], $t["dynamic"], isset($t["uninitialized"]), isset($t["\0A\0secret"]), isset($t["\0*\0hidden"]));
$t = $a->table();
var_dump($t["backed"]);
try { export_table($a); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
"#), "public\nint(4)\nint(5)\nint(6)\nbool(false)\nbool(false)\nbool(false)\npublic\nint(4)\nprivate\nprivate hook\n");
}

#[test]
fn json_table_projects_lazy_proxy_hooks_and_dynamic_properties() {
    assert_eq!(
        run_property_tables(
            r#"<?php
#[AllowDynamicProperties]
class A {
    private $hidden { get { throw new Exception("hidden"); } }
    public $value { get { echo "get\n"; return (fn() => 42)(); } }
    public int $uninitialized;
}
$r = new ReflectionClass(A::class);
$p = $r->newLazyProxy(function() { echo "init\n"; $a = new A; $a->dynamic = 8; return $a; });
$t = json_table($p);
var_dump($t["value"], $t["dynamic"], isset($t["uninitialized"]));
"#
        ),
        "init\nget\nint(42)\nint(8)\nbool(false)\n"
    );
}

#[test]
fn native_table_reference_preserves_identity_and_checks_typed_writes() {
    assert_eq!(run_property_tables(r#"<?php
class A { public int $foo = 1; }
$a = new A;
$v = "2";
var_dump(bind_table($a, "foo", $v), $v, $a->foo);
$v = "3";
var_dump($v, $a->foo);
try { $v = "bad"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($v, $a->foo);
$bad = [];
try { bind_table($a, "foo", $bad); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$v = 4;
var_dump($a->foo);
"#), "bool(true)\nint(2)\nint(2)\nint(3)\nint(3)\nCannot assign string to reference held by property A::$foo of type int\nint(3)\nint(3)\nCannot assign array to property A::$foo of type int\nint(4)\n");
}

#[test]
fn native_table_reference_rejects_readonly_before_installing_reference() {
    assert_eq!(run_property_tables(r#"<?php
class A { public readonly int $foo; function __construct() { $this->foo = 1; } }
$a = new A;
$v = "2";
try { bind_table($a, "foo", $v); } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump($v, $a->foo);
$v = "untyped";
var_dump($v, $a->foo);
"#), "Cannot acquire reference to readonly property A::$foo\nstring(1) \"2\"\nint(1)\nstring(7) \"untyped\"\nint(1)\n");
}

#[test]
fn native_table_reference_uses_mangled_slots_and_resolves_lazy_targets() {
    assert_eq!(run_property_tables(r#"<?php
class A {
    private int $foo = 1;
    protected int $bar = 2;
    function values() { var_dump($this->foo, $this->bar); }
}
$r = new ReflectionClass(A::class);
$p = $r->newLazyProxy(function() { echo "init\n"; return new A; });
$v = 3;
$w = 4;
bind_table($p, "\0A\0foo", $v);
bind_table($p, "\0*\0bar", $w);
$v = "5";
$w = "6";
$p->values();
try { $v = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
#[AllowDynamicProperties]
class B { public static int $foo = 1; public $virtual { get => 1; } }
$b = new B;
$x = 1;
prepare_untyped_table($b, "foo", $x);
prepare_untyped_table($b, "virtual", $x);
bind_table($b, 123, $x);
$x = [];
var_dump($b->{"123"});
"#), "init\nint(5)\nint(6)\nCannot assign array to reference held by property A::$foo of type int\narray(0) {\n}\n");
}

#[test]
fn typed_plain_reads_preserve_values_and_checked_writes() {
    assert_eq!(run(r#"<?php
class PlainTyped { public int $v = 1; public ?int $nullable = null; }
$o = new PlainTyped;
for ($i = 0; $i < 20; $i++) { $o->v += 1; }
var_dump($o->v, $o->v ?? 99, $o->nullable, $o->nullable ?? 99);
$copy = $o->v; $copy = 0; var_dump($o->v);
$o->v = "22"; var_dump($o->v);
try { $o->v = []; } catch (TypeError $e) { echo "checked\n"; }
$r = &$o->v; $r = 23; var_dump($o->v, $o->v ?? 99);
$copy = $o->v; $copy = 0; var_dump($r);
try { $r = []; } catch (TypeError $e) { echo "reference-checked\n"; }
var_dump($o->v);
"#), "int(21)\nint(21)\nNULL\nint(99)\nint(21)\nint(22)\nchecked\nint(23)\nint(23)\nint(23)\nreference-checked\nint(23)\n");
}

#[test]
fn typed_plain_reads_keep_uninitialized_and_unset_boundaries() {
    assert_eq!(run(r#"<?php
class PlainTyped { public int $v; }
$o = new PlainTyped;
var_dump($o->v ?? 99);
try { $o->v; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$o->v = 7; var_dump($o->v);
unset($o->v); var_dump($o->v ?? 99);
try { $o->v; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$o->v = 8; var_dump($o->v);
class MagicTyped {
    public int $v;
    function __get($name) { echo "get\n"; return "9"; }
    function __isset($name) { echo "isset\n"; return true; }
}
$m = new MagicTyped;
try { $m->v; } catch (Error $e) { echo "uninitialized\n"; }
$m->v = 10; var_dump($m->v);
unset($m->v); var_dump($m->v);
"#), "int(99)\nTyped property PlainTyped::$v must not be accessed before initialization\nint(7)\nint(99)\nTyped property PlainTyped::$v must not be accessed before initialization\nint(8)\nuninitialized\nint(10)\nget\nint(9)\n");
}

#[test]
fn typed_reads_keep_lazy_ghost_proxy_and_hook_dispatch() {
    assert_eq!(run(r#"<?php
class PlainTyped { public int $v = 1; public int $other = 2; }
$r = new ReflectionClass(PlainTyped::class);
$g = $r->newLazyGhost(function($o) { echo "ghost\n"; $o->v = 3; });
var_dump($g->v, $g->v);
$p = $r->newLazyProxy(function() { echo "proxy\n"; $o = new PlainTyped; $o->v = 4; return $o; });
var_dump($p->v, $p->v);
$g = $r->newLazyGhost(function($o) { echo "skipped-ghost\n"; $o->other = 6; });
(new ReflectionProperty(PlainTyped::class, "v"))->setRawValueWithoutLazyInitialization($g, 5);
var_dump($g->v, $r->isUninitializedLazyObject($g));
var_dump($g->other, $g->v);
class HookedTyped { public int $v = 7 { get { echo "hook\n"; return $this->v; } } }
var_dump((new HookedTyped)->v);
class ChildTyped extends PlainTyped { private int $hidden = 8; }
$c = new ChildTyped; var_dump($c->v);
try { $c->hidden; } catch (Error $e) { echo "private\n"; }
class ReadonlyTyped { public function __construct(public readonly int $v = 9) {} }
$o = new ReadonlyTyped; var_dump($o->v);
try { $o->v = 10; } catch (Error $e) { echo "readonly\n"; }
"#), "ghost\nint(3)\nint(3)\nproxy\nint(4)\nint(4)\nint(5)\nbool(true)\nskipped-ghost\nint(6)\nint(5)\nhook\nint(7)\nint(1)\nprivate\nint(9)\nreadonly\n");
}

#[test]
fn typed_initialized_reads_keep_native_property_handlers() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    php_vm::ext::ClassBuilder::new("NativeTyped")
        .prop(
            "v",
            php_vm::func::flags::PUBLIC,
            Some("int"),
            Some(Value::Int(1)),
        )
        .hooks(php_vm::ext::ClassHooks {
            read_property: Some(|_, _, name| {
                assert_eq!(name.as_bytes(), b"v");
                Ok(Some(Value::Int(42)))
            }),
            ..php_vm::ext::ClassHooks::default()
        })
        .register(&mut vm);
    assert_eq!(
        vm.run_script(
            b"<?php $o = new NativeTyped; var_dump($o->v, $o->v ?? 99);",
            "t.php"
        ),
        0
    );
    drop(vm);
    assert_eq!(sink.0.borrow().as_slice(), b"int(42)\nint(42)\n");
}

#[test]
fn native_get_object_vars_filters_visibility_and_reads_property_hooks() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    php_vm::ext::ClassBuilder::new("NativeVars")
        .prop(
            "visible",
            php_vm::func::flags::PUBLIC,
            None,
            Some(Value::Int(1)),
        )
        .prop(
            "hidden",
            php_vm::func::flags::PRIVATE,
            None,
            Some(Value::Int(2)),
        )
        .prop(
            "guarded",
            php_vm::func::flags::PROTECTED,
            None,
            Some(Value::Int(3)),
        )
        .hooks(php_vm::ext::ClassHooks {
            properties_for: Some(|_, _, purpose| {
                assert_eq!(purpose, PropertyPurpose::GetObjectVars);
                let mut table = php_runtime::ZArray::new();
                for (key, value) in [
                    (&b"visible"[..], 1),
                    (&b"\0NativeVars\0hidden"[..], 2),
                    (&b"\0*\0guarded"[..], 3),
                    (&b"\0ChildVars\0secret"[..], 4),
                    (&b"native"[..], 5),
                    (&b"12"[..], 6),
                    (&b"computed"[..], 0),
                ] {
                    table
                        .make_mut()
                        .insert(ArrayKey::Str(ZStr::from(key)), Value::Int(value));
                }
                table.make_mut().insert(ArrayKey::Int(7), Value::Int(7));
                Ok(Some(table))
            }),
            get_properties: Some(|_, _| panic!("properties_for must take precedence")),
            ..php_vm::ext::ClassHooks::default()
        })
        .register(&mut vm);
    assert_eq!(
        vm.run_script(
            br#"<?php
class ChildVars extends NativeVars {
    private $secret = 4;
    public int $computed { get { echo "get\n"; return 8; } }
    function snapshot() {
        foreach (get_object_vars($this) as $key => $value) echo $key, ':', $value, "\n";
    }
}
$o = new ChildVars;
foreach (get_object_vars($o) as $key => $value) echo $key, ':', $value, "\n";
$o->snapshot();
class ThrowingVars extends NativeVars {
    public int $computed { get { throw new Error('getter failure'); } }
}
try { get_object_vars(new ThrowingVars); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
            "t.php",
        ),
        0
    );
    drop(vm);
    assert_eq!(
        sink.0.borrow().as_slice(),
        b"get\nvisible:1\nnative:5\n12:6\ncomputed:8\n7:7\n\
          get\nvisible:1\nguarded:3\nsecret:4\nnative:5\n12:6\ncomputed:8\n7:7\n\
          getter failure\n"
    );
}

#[test]
fn native_get_object_vars_preserves_handler_exceptions_and_fallback() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    php_vm::ext::ClassBuilder::new("FailingVars")
        .hooks(php_vm::ext::ClassHooks {
            properties_for: Some(|vm, _, purpose| {
                assert_eq!(purpose, PropertyPurpose::GetObjectVars);
                Err(vm.throw("Error", "native table failure"))
            }),
            get_properties: Some(|_, _| panic!("must not fall back after an exception")),
            ..php_vm::ext::ClassHooks::default()
        })
        .register(&mut vm);
    php_vm::ext::ClassBuilder::new("FallbackVars")
        .hooks(php_vm::ext::ClassHooks {
            properties_for: Some(|_, _, purpose| {
                assert_eq!(purpose, PropertyPurpose::GetObjectVars);
                Ok(None)
            }),
            get_properties: Some(|_, _| {
                Ok(Some(php_runtime::ZArray::from_values([Value::Int(42)])))
            }),
            ..php_vm::ext::ClassHooks::default()
        })
        .register(&mut vm);
    php_vm::ext::ClassBuilder::new("EmptyVars")
        .hooks(php_vm::ext::ClassHooks {
            properties_for: Some(|_, _, purpose| {
                assert_eq!(purpose, PropertyPurpose::GetObjectVars);
                Ok(Some(php_runtime::ZArray::new()))
            }),
            get_properties: Some(|_, _| panic!("an empty native table is not fallback")),
            ..php_vm::ext::ClassHooks::default()
        })
        .register(&mut vm);
    assert_eq!(
        vm.run_script(
            br#"<?php
try { get_object_vars(new FailingVars); } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(get_object_vars(new FallbackVars), get_object_vars(new EmptyVars));
"#,
            "t.php",
        ),
        0
    );
    drop(vm);
    assert_eq!(
        sink.0.borrow().as_slice(),
        b"native table failure\narray(1) {\n  [0]=>\n  int(42)\n}\narray(0) {\n}\n"
    );
}

#[test]
fn array_access_read_modify_write_fetches_each_container_once() {
    assert_eq!(
        run(r#"<?php
class A implements ArrayAccess {
    function offsetGet($k): mixed { echo "get($k)\n"; return 1; }
    function offsetSet($k, $v): void { echo "set($k,$v)\n"; }
    function offsetExists($k): bool { return true; }
    function offsetUnset($k): void {}
}
class P implements ArrayAccess {
    function offsetGet($k): mixed { echo "P::get($k)\n"; return new A; }
    function offsetSet($k, $v): void { echo "P::set\n"; }
    function offsetExists($k): bool { return true; }
    function offsetUnset($k): void {}
}
$a = new A;
var_dump($a['x']++);
var_dump(--$a['y']);
$a['z'] .= 'b';
$p = new P;
$p[0]['n'] .= 'c';
$t = 1;
try { $a['r'] = &$t; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "get(x)\n\nNotice: Indirect modification of overloaded element of A has no effect in t.php on line 15\nint(1)\n\
get(y)\n\nNotice: Indirect modification of overloaded element of A has no effect in t.php on line 16\nint(0)\n\
get(z)\nset(z,1b)\nP::get(0)\nget(n)\nset(n,1c)\nget(r)\n\
\nNotice: Indirect modification of overloaded element of A has no effect in t.php on line 21\n\
Cannot assign by reference to an array dimension of an object\n"
    );
}

#[test]
fn clone_destructor_and_static_isset_follow_zend_checks() {
    assert_eq!(
        run(r#"<?php
foreach ([0, null, []] as $v) {
    try { clone $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
class Q { private function __clone() {} }
try { clone new Q; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$ref = 'r';
$with = ['x' => &$ref];
try { clone(new stdClass, $with); } catch (Error $e) { echo $e->getMessage(), "\n"; }
unset($ref);
$c = clone(new stdClass, $with);
var_dump($c->x);
class R { protected function __destruct() {} }
$r = new R;
try { $r = null; } catch (Error $e) { echo $e->getMessage(), "\n"; }
class S {}
class T { private static $q = 1; }
var_dump(isset(S::$p), empty(S::$p), isset(T::$q));
$keep = new R;
"#),
        "clone(): Argument #1 ($object) must be of type object, int given\n\
clone(): Argument #1 ($object) must be of type object, null given\n\
clone(): Argument #1 ($object) must be of type object, array given\n\
Call to private method Q::__clone() from global scope\n\
Cannot assign by reference when cloning with updated properties\n\
string(1) \"r\"\n\
Call to protected R::__destruct() from global scope\n\
bool(false)\nbool(true)\nbool(false)\n\
\nWarning: Call to protected R::__destruct() from global scope during shutdown ignored in Unknown on line 0\n"
    );
}

/// `ZEND_AST_PROP` in `zend_ast_evaluate_ex`: constant expressions (global
/// constants and parameter defaults included) only fetch properties of
/// enum cases, after converting the name; closure bodies are not affected.
#[test]
fn constant_expression_property_fetch_requires_enum() {
    let out = run(r#"<?php
class A { public $prop = 42; }
class B {}
enum E: int { case X = 1; }
function f($p = (new A)->prop) {}
function h($p = (new A)->{new B}) {}
function g($c = static function () { return (new A)->prop; }) { return $c(); }
foreach (['f', 'f', 'h'] as $fn) {
    try { $fn(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
const V = E::X->value;
const N = (null)?->prop;
echo V, g(), N === null ? "null" : "set", "\n";
const P = (new A)?->prop;
echo "unreachable\n";
"#);
    assert!(
        out.starts_with(
            "Fetching properties on non-enums in constant expressions is not allowed\n\
             Fetching properties on non-enums in constant expressions is not allowed\n\
             Object of class B could not be converted to string\n\
             142null\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "Uncaught Error: Fetching properties on non-enums in constant expressions is not allowed in t.php:14"
        ),
        "{out}"
    );
    assert!(!out.contains("unreachable"), "{out}");
}

/// `zend_compile_prop_decl` sets `active_property_info_name` while it
/// compiles each property's default and attributes, so `__PROPERTY__`
/// names the property there too (GH-17222).
#[test]
fn property_magic_constant_in_defaults_and_attributes() {
    let out = run(r#"<?php
#[Attribute]
class MyAttr { public function __construct(public string $msg) {} }
class Foo {
    #[MyAttr(__PROPERTY__)]
    public $a = __PROPERTY__, $b = __PROPERTY__;
    public static $s = __PROPERTY__;
    public $c = [__PROPERTY__];
}
$f = new Foo;
echo $f->a, $f->b, Foo::$s, $f->c[0], "\n";
$r = new ReflectionClass(Foo::class);
foreach (['a', 'b'] as $p) {
    echo $r->getProperty($p)->getAttributes()[0]->getArguments()[0];
}
echo "\n";
"#);
    assert_eq!(out, "absc\nab\n");
}

/// Instantiation evaluates static property initializers as well
/// (`zend_update_class_constants`); a failed one is retried (GH-10356).
#[test]
fn instantiation_evaluates_static_property_initializers() {
    let out = run(r#"<?php
class Foo {
    public static $foo = [
        'foo' => DOES_NOT_EXIST,
    ];
}
for ($i = 0; $i < 2; $i++) {
    try { new Foo(); } catch (Error $e) { echo $e->getMessage(), " @", $e->getLine(), "\n"; }
}
define('DOES_NOT_EXIST', 1);
new Foo();
echo Foo::$foo['foo'], "\n";
"#);
    assert_eq!(
        out,
        "Undefined constant \"DOES_NOT_EXIST\" @4\nUndefined constant \"DOES_NOT_EXIST\" @4\n1\n"
    );
}

#[test]
fn promoted_by_reference_property_binds_the_argument() {
    // ctor_promotion_by_ref: `ZEND_ASSIGN_OBJ_REF`.
    let out = run(r#"<?php
class Ary { public function __construct(public array &$array) {} }
$array = [];
$ary = new Ary($array);
$array[] = 42;
var_dump($ary->array);
"#);
    assert_eq!(out, "array(1) {\n  [0]=>\n  int(42)\n}\n");
}

#[test]
fn promoted_property_cannot_be_callable() {
    // ctor_promotion_callable_type (the parameter's line, as in master).
    let out = run("<?php\nclass T {\n    public function __construct(\n        public ?callable $c\n    ) {}\n}\n");
    assert_eq!(
        out,
        "\nFatal error: Property T::$c cannot have type ?callable in t.php on line 4\n"
    );
}

#[test]
fn readonly_object_incdec_reports_the_write_after_the_type_error() {
    // readonly_containing_object: the overloaded inc/dec path writes back
    // even when the increment failed.
    let out = run(r#"<?php
class Test { public function __construct(public readonly object $prop) {} }
$t = new Test(new stdClass);
try { $t->prop++; } catch (Error $e) {
    $p = $e->getPrevious();
    echo $e::class, ": ", $e->getMessage(), " <- ", $p::class, ": ", $p->getMessage(), "\n";
}
try { $t->prop += 1; } catch (Throwable $e) { echo $e::class, ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "Error: Cannot modify readonly property Test::$prop <- TypeError: Cannot increment stdClass\nTypeError: Unsupported operand types: stdClass + int\n"
    );
}

#[test]
fn readonly_class_rejects_unserialized_dynamic_properties() {
    // readonly_class_unserialize_error (GH-9186).
    let out = run(r#"<?php
readonly class R {}
try { unserialize('O:1:"R":1:{s:1:"x";b:1;}'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(out, "Cannot create dynamic property R::$x\n");
}

#[test]
fn public_set_readonly_property_leaves_interface_set_abstract() {
    // readonly_with_default_interface_get_set.
    let out = run("<?php\ninterface I {\n    public int $prop { get; set; }\n}\nclass C implements I {\n    public public(set) readonly int $prop = 42;\n}\n");
    assert!(
        out.contains("Fatal error: Class C contains 1 abstract method and must therefore be declared abstract or implement the remaining method (I::$prop::set) in t.php on line"),
        "{out}"
    );
#[test]
fn non_object_property_and_string_offset_errors_follow_the_opcode() {
    assert_eq!(run(r#"<?php
$t = true;
$s = "ab";
foreach ([
    fn() => $t->p = 1,
    fn() => $t->p += 1,
    fn() => $t->p++,
    fn() => $t->p[0] = 1,
    fn() => $s[0]++,
    fn() => $s[0] .= "x",
    fn() => $s[0][0] = "x",
    fn() => $s[0]->p = "x",
] as $f) {
    try { $f(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
try { $u->p = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(isset($u));
try { $u->p--; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(isset($u));
"#), "Attempt to assign property \"p\" on true
Attempt to assign property \"p\" on true
Attempt to increment/decrement property \"p\" on true
Attempt to modify property \"p\" on true
Cannot increment/decrement string offsets
Cannot use assign-op operators with string offsets
Cannot use string offset as an array
Cannot use string offset as an object
Attempt to assign property \"p\" on null
bool(false)

Warning: Undefined variable $u in t.php on line 18
Attempt to increment/decrement property \"p\" on null
bool(false)
");
}

#[test]
fn undefined_assigned_value_warns_after_property_fetches() {
    // bug43201: ZEND_ASSIGN_DIM reads OP_DATA after FETCH_OBJ_W's notice,
    // but before zend_assign_to_object_dim calls offsetSet.
    assert_eq!(run(r#"<?php
class Foo { function __get($k) { return null; } }
class AA implements ArrayAccess {
    function offsetGet($o): mixed { echo "get\n"; return null; }
    function offsetSet($o, $v): void { echo "set\n"; }
    function offsetExists($o): bool { return false; }
    function offsetUnset($o): void {}
}
$c = new Foo();
$c->arr[0]["k2"] = $ref;
$o = new stdClass;
$o->aa = new AA;
$o->aa[1] = $ref2;
echo "ok\n";
"#), "
Notice: Indirect modification of overloaded property Foo::$arr has no effect in t.php on line 10

Warning: Undefined variable $ref in t.php on line 10

Warning: Undefined variable $ref2 in t.php on line 13
set
ok
");
}
