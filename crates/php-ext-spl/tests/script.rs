//! SPL property-table reference boundaries exercised through the VM.

use php_ext_spl::host::{std_properties, BridgedHost, VmHost};
use php_runtime::{ArrayKey, Obj, PhpRef, Value, ZStr};
use php_spl::SplHost;
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

fn vm() -> (Vm, Sink) {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("error_reporting".into(), "8191".into())],
    };
    let vm = Vm::with_extensions(&options, Box::new(sink.clone()), &[&php_ext_spl::Spl]);
    (vm, sink)
}

fn output(sink: &Sink) -> String {
    String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output")
}

fn run(code: &str) -> String {
    let (mut vm, sink) = vm();
    assert_eq!(vm.run_script(code.as_bytes(), "test.php"), 0);
    output(&sink)
}

#[test]
fn array_object_uses_configured_iterator_aggregate() {
    let out = run(r#"<?php
        class MyIterator extends ArrayIterator {
            function __construct() {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
            }
            function rewind(): void {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
                parent::rewind();
            }
            function valid(): bool {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
                return parent::valid();
            }
            function current(): mixed {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
                return parent::current();
            }
            function next(): void {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
                parent::next();
            }
            function key(): string|int|null {
                echo "   In " . __METHOD__ . "(" . implode(',', func_get_args()) . ")\n";
                return parent::key();
            }
        }

        $ao = new ArrayObject(['a' => 1, 'b' => 2, 'c' => 3], 0, MyIterator::class);
        var_dump($ao->getIteratorClass());
        var_dump($ao->getIterator());
        foreach ($ao as $key => $value) {
            echo "  $key=>$value\n";
        }

        var_dump($ao->setIteratorClass(ArrayIterator::class));
        var_dump($ao->getIteratorClass());
        var_dump($ao->getIterator());
        foreach ($ao as $key => $value) {
            echo "$key=>$value\n";
        }
        ?>"#);
    assert!(out.contains("string(10) \"MyIterator\"\n"));
    assert!(out.contains("[\"storage\":\"ArrayIterator\":private]=>\n"));
    assert!(out.contains("object(ArrayObject)#"));
    assert!(out.contains("[\"a\"]=>\n      int(1)\n"));
    assert!(!out.contains("In MyIterator::__construct()"));
    assert!(out.contains("In MyIterator::rewind()\n"));
    assert!(out.contains("In MyIterator::valid()\n"));
    assert!(out.contains("In MyIterator::current()\n"));
    assert!(out.contains("In MyIterator::key()\n"));
    assert!(out.contains("In MyIterator::next()\n"));
    assert!(out.contains("NULL\nstring(13) \"ArrayIterator\"\n"));
    assert!(out.contains("object(ArrayIterator)#"));
    assert!(out.contains("a=>1\nb=>2\nc=>3\n"));
}

fn declare(vm: &mut Vm, code: &str, class: &[u8]) -> Obj {
    vm.run_main(code.as_bytes(), &ZStr::from("test.php"))
        .expect("class declarations");
    let cls = vm.find_class(class).expect("class");
    vm.new_object(&cls).expect("object")
}

fn key(name: &str) -> ArrayKey {
    ArrayKey::Str(ZStr::from(name))
}

#[test]
fn repeated_typed_iteration_preserves_alias_and_rejects_arrays() {
    let out = run(r#"<?php
        class A implements IteratorAggregate {
            public string $foo = "bar";
            public function getIterator(): Traversable { return new ArrayIterator($this); }
        }
        $obj = new A;
        foreach ($obj as &$v) { $v = 42; }
        var_dump($obj->foo);
        $held =& $v;
        foreach ($obj as &$v) {
            try { $v = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        }
        try { $held = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $v = 43;
        var_dump($held, $obj->foo);
        "#);
    assert_eq!(
        out,
        concat!(
            "string(2) \"42\"\n",
            "Cannot assign array to reference held by property A::$foo of type string\n",
            "Cannot assign array to reference held by property A::$foo of type string\n",
            "string(2) \"43\"\nstring(2) \"43\"\n",
        )
    );
}

#[test]
fn readonly_reference_acquisition_does_not_mutate_property_or_loop_variable() {
    let out = run(r#"<?php
        class A { public function __construct(public readonly string $foo = "bar") {} }
        $obj = new A;
        $it = new ArrayIterator($obj);
        $v = "untouched";
        for ($i = 0; $i < 2; ++$i) {
            try { foreach ($it as &$v) {} }
            catch (Error $e) { echo $e->getMessage(), "\n"; }
        }
        var_dump($v, $obj->foo);
        foreach ($it as $copy) { var_dump($copy); }
        "#);
    assert_eq!(
        out,
        concat!(
            "Cannot acquire reference to readonly property A::$foo\n",
            "Cannot acquire reference to readonly property A::$foo\n",
            "string(9) \"untouched\"\nstring(3) \"bar\"\nstring(3) \"bar\"\n",
        )
    );
}

#[test]
fn native_array_storage_remains_untyped() {
    let out = run(r#"<?php
        foreach ([new ArrayIterator(["foo" => "bar"]), new ArrayObject(["foo" => "bar"])] as $it) {
            foreach ($it as &$v) { $v = 42; }
            var_dump($it["foo"]);
            foreach ($it as &$v) { $v = []; }
            var_dump($it["foo"]);
            unset($it["foo"]);
            $it[] = "ok";
            foreach ($it as $value) { var_dump($value); }
        }
        "#);
    assert_eq!(
        out,
        "int(42)\narray(0) {\n}\nstring(2) \"ok\"\nint(42)\narray(0) {\n}\nstring(2) \"ok\"\n"
    );
}

#[test]
fn mangled_private_and_protected_references_bind_exact_slots_through_bridge() {
    let (mut vm, sink) = vm();
    let obj = declare(
        &mut vm,
        "<?php class P { private string $same = \"parent\"; }
        class C extends P { private int $same = 7; private string $other = \"other\";
            protected string $protected = \"protected\"; }",
        b"C",
    );
    let names = ["\0P\0same", "\0C\0same", "\0C\0other", "\0*\0protected"];
    let mut props = std_properties(&obj);
    let refs: Vec<_> = names
        .iter()
        .map(|name| PhpRef::new(props.get(&key(name)).expect("property").clone()))
        .collect();
    for (name, r) in names.iter().zip(&refs) {
        props
            .make_mut()
            .insert(key(name), Value::Reference(r.clone()));
    }
    php_ext_streams::bridge::run(&mut vm, || {
        BridgedHost::new().set_std_properties(&obj, props)
    })
    .expect("bridge")
    .expect("reference writeback");
    let stored = std_properties(&obj);
    for (i, (name, r)) in names.iter().zip(&refs).enumerate() {
        assert!(
            matches!(stored.get(&key(name)), Some(Value::Reference(actual)) if PhpRef::ptr_eq(actual, r))
        );
        let mut globals = vm.globals.borrow_mut();
        let slot = globals.find_or_add(format!("r{i}").as_bytes()) as usize;
        globals.slots[slot] = Some(Value::Reference(r.clone()));
    }
    vm.run_main(
        br#"<?php
        foreach (["r0", "r1", "r2", "r3"] as $name) {
            try { $$name = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        }"#,
        &ZStr::from("test.php"),
    )
    .expect("reference assignments");
    assert_eq!(
        output(&sink),
        concat!(
            "Cannot assign array to reference held by property P::$same of type string\n",
            "Cannot assign array to reference held by property C::$same of type int\n",
            "Cannot assign array to reference held by property C::$other of type string\n",
            "Cannot assign array to reference held by property C::$protected of type string\n",
        )
    );
}

#[test]
fn failed_binding_keeps_storage_and_incoming_reference_unchanged() {
    let (mut vm, _) = vm();
    let obj = declare(
        &mut vm,
        "<?php class A { public string $foo = \"bar\"; }",
        b"A",
    );
    let mut props = std_properties(&obj);
    let r = PhpRef::new(Value::empty_array());
    props
        .make_mut()
        .insert(key("foo"), Value::Reference(r.clone()));
    assert!(VmHost::new(&mut vm)
        .set_std_properties(&obj, props)
        .is_err());
    assert!(
        matches!(std_properties(&obj).get(&key("foo")), Some(Value::Str(s)) if s.as_bytes() == b"bar")
    );
    assert!(matches!(r.get(), Value::Array(a) if a.is_empty()));
}

#[test]
fn later_binding_failure_rolls_back_storage_and_coercion() {
    for declaration in [
        "public int $second = 7;",
        "public readonly string $second; public function __construct() { $this->second = \"bar\"; }",
    ] {
        let (mut vm, _) = vm();
        vm.run_main(
            format!("<?php class A {{ public string $first = \"original\"; {declaration} }}").as_bytes(),
            &ZStr::from("test.php"),
        )
        .expect("class declaration");
        let cls = vm.find_class(b"A").expect("class");
        let obj = vm.new_object_with_args(&cls, Vec::new()).expect("object");
        let before = std_properties(&obj);
        let mut props = before.clone();
        let r = PhpRef::new(Value::Int(42));
        for name in ["first", "second"] {
            props.make_mut().insert(key(name), Value::Reference(r.clone()));
        }
        assert!(VmHost::new(&mut vm).set_std_properties(&obj, props).is_err());
        let after = std_properties(&obj);
        for name in ["first", "second"] {
            match (after.get(&key(name)), before.get(&key(name))) {
                (Some(Value::Str(actual)), Some(Value::Str(expected))) => {
                    assert_eq!(actual, expected);
                }
                (Some(Value::Int(actual)), Some(Value::Int(expected))) => {
                    assert_eq!(actual, expected);
                }
                values => panic!("property changed: {values:?}"),
            }
        }
        assert!(matches!(r.get(), Value::Int(42)));
        {
            let mut globals = vm.globals.borrow_mut();
            let slot = globals.find_or_add(b"r") as usize;
            globals.slots[slot] = Some(Value::Reference(r.clone()));
        }
        vm.run_main(b"<?php $r = [];", &ZStr::from("test.php"))
            .expect("rolled-back reference has no live type sources");
        assert!(matches!(r.get(), Value::Array(a) if a.is_empty()));
    }
}

#[test]
fn autoloader_registered_by_a_running_loader_is_called_for_the_same_class() {
    let out = run(r#"<?php
        function inner_autoload($name) {
            if ($name == 'A') {
                class A { function __construct() { echo "okey, "; } }
            }
        }
        spl_autoload_register(function ($name) {
            if ($name == 'A') {
                spl_autoload_register('inner_autoload');
            } else {
                spl_autoload_unregister('inner_autoload');
            }
        });
        new A();
        try {
            new B();
        } catch (Error $e) {
            echo $e->getMessage(), "\n";
        }
        echo count(spl_autoload_functions()), "\n";
    "#);
    assert_eq!(out, "okey, Class \"B\" not found\n1\n");
}

#[test]
fn self_callables_register_the_class_of_the_registering_scope() {
    let out = run(r#"<?php
        class First {
            static function init() { spl_autoload_register(['self', 'load']); }
            static function load($c) { echo "First::load($c)\n"; }
        }
        class Second {
            static function init() { spl_autoload_register('self::load'); }
            static function load($c) { echo "Second::load($c)\n"; }
        }
        First::init();
        Second::init();
        foreach (spl_autoload_functions() as $f) {
            echo implode('::', $f), "\n";
        }
        var_dump(class_exists('Missing'));
    "#);
    assert_eq!(
        out,
        "First::load\nSecond::load\nFirst::load(Missing)\nSecond::load(Missing)\nbool(false)\n"
    );
}

#[test]
fn foreach_over_spl_objects_numbers_handles_like_zend() {
    // `ArrayObject::getIterator()` creates an ArrayIterator and the
    // `foreach` iterator takes one more handle; a non-Traversable SPL
    // class iterates its properties.
    let out = run(r#"<?php
        $a = new ArrayObject([1]);
        foreach ($a as $v) { echo spl_object_id(new stdClass), ","; }
        echo spl_object_id(new stdClass), "\n";
        class F extends SplFileInfo { public $p = 1; }
        foreach (new F('x') as $k => $v) { echo $k, "=", $v, "\n"; }
    "#);
    assert_eq!(out, "4,3\np=1\n");
}

#[test]
fn array_object_element_references_match_zend() {
    // A compound assignment leaves no visible reference in a wrapped
    // object's property table, and `$ao[$k] = &$v` binds the element.
    let out = run(r#"<?php
$o = new stdClass;
$ao = @new ArrayObject($o);
$ao['x'] = 1;
$ao['x'] += 1;
$ao['y'] = 5;
$ao['y']++;
var_dump($o, (array) $ao);
$ao = new ArrayObject;
foreach ([1, 2] as $i => $var) { $ao[$i] = &$var; }
var_dump($ao->getArrayCopy());
"#);
    assert_eq!(
        out,
        r#"object(stdClass)#1 (2) {
  ["x"]=>
  int(2)
  ["y"]=>
  int(6)
}
array(2) {
  ["x"]=>
  int(2)
  ["y"]=>
  int(6)
}
array(2) {
  [0]=>
  &int(2)
  [1]=>
  &int(2)
}
"#
    );
}

#[test]
fn unserialize_members_load_like_object_properties_load() {
    // GH-9708: declared members are type-checked strictly; undeclared
    // ones are dynamic properties with the PHP 8.2 deprecation.
    let out = run(r#"<?php
error_reporting(E_ALL);
set_error_handler(function ($n, $m) { echo $m, "\n"; return true; });
class Foo extends ArrayObject { public int $a = 5; }
try {
    unserialize('O:3:"Foo":4:{i:0;i:0;i:1;a:0:{}i:2;a:1:{s:1:"a";s:2:"10";}i:3;N;}');
} catch (TypeError $e) {
    echo $e->getMessage(), "\n";
}
$f = unserialize('O:13:"SplFixedArray":2:{i:0;i:1;s:3:"foo";s:3:"bar";}');
var_dump($f->foo);
"#);
    assert_eq!(
        out,
        "Cannot assign string to property Foo::$a of type int\n\
         Creation of dynamic property SplFixedArray::$foo is deprecated\n\
         string(3) \"bar\"\n"
    );
}

#[test]
fn autoloader_is_called_as_resolved_in_the_registering_scope() {
    // `[$this, 'autoload']` registered from a scope that cannot see the
    // protected method resolves to the `__call` trampoline, even when the
    // autoload is triggered from inside the class.
    let out = run(r#"<?php
        class Registrator {
            static function call($callable, array $args) {
                return call_user_func_array($callable, [$args]);
            }
        }
        class Loader {
            function __construct() {
                Registrator::call('spl_autoload_register', [$this, 'autoload']);
            }
            function __call($method, $args) { echo "__call($method, {$args[0]})\n"; }
            protected function autoload($class) { echo "autoload($class)\n"; }
            function probe() { return class_exists('Inner'); }
        }
        $l = new Loader();
        var_dump(class_exists('Outer'));
        var_dump($l->probe());
    "#);
    assert_eq!(
        out,
        "__call(autoload, Outer)\nbool(false)\n__call(autoload, Inner)\nbool(false)\n"
    );
}
fn array_iterator_initializes_lazy_storage() {
    // GH-15833: reading a lazy object's property table initializes it
    // (a proxy reads its real instance; a failing initializer throws).
    let out = run(r#"<?php
class C { public int $a = 1; }
$r = new ReflectionClass(C::class);
$it = new RecursiveArrayIterator($r->newLazyProxy(fn ($o) => new C()));
var_dump($it->current());
$it->next();
var_dump($it->current());
$it = new RecursiveArrayIterator($r->newLazyProxy(function ($o) {
    static $n = 0;
    throw new Error('nope ' . $n++);
}));
for ($i = 0; $i < 2; $i++) {
    try { $it->current(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
"#);
    assert_eq!(out, "int(1)\nNULL\nnope 0\nnope 1\n");
}

#[test]
fn autoload_callbacks_resolve_at_registration() {
    // `self` resolves (with its deprecation) and a private method stays
    // callable from the registering scope; a loader registered by another
    // runs in the same lookup (bug #71202).
    let out = run(r#"<?php
error_reporting(E_ALL);
set_error_handler(function ($n, $m) { echo $m, "\n"; return true; });
class First {
    public static function init() { spl_autoload_register(['self', 'load']); }
    public static function load($c) {}
}
First::init();
foreach (spl_autoload_functions() as $f) { echo implode("::", $f), "\n"; }
spl_autoload_unregister(['First', 'load']);
class L {
    function __construct() { spl_autoload_register([$this, 'load']); }
    private function load($c) {
        echo "load $c\n";
        if ($c === 'A') { spl_autoload_register(function ($c) { echo "inner $c\n"; }); }
    }
}
new L;
var_dump(class_exists('A'));
"#);
    assert_eq!(
        out,
        "Use of \"self\" in callables is deprecated\nFirst::load\nload A\ninner A\nbool(false)\n"
    );
}

#[test]
fn internal_class_constants_list_inherited_first() {
    // Bug #75242: an internal class's table holds the parent's constants
    // before its own.
    let out = run(r#"<?php
foreach ((new ReflectionClass('RecursiveArrayIterator'))->getConstants() as $k => $v) {
    echo $k, "=", $v, "\n";
}
"#);
    assert_eq!(
        out,
        "STD_PROP_LIST=1\nARRAY_AS_PROPS=2\nCHILD_ARRAYS_ONLY=4\n"
    );
}

#[test]
fn fixed_array_export_and_zval_dump_match_zend() {
    // GH-8044: var_export() buffers its output (the circular-reference
    // warning comes first) and debug_zval_dump() counts the references of
    // the table `get_properties_for` builds.
    let out = run(r#"<?php
set_error_handler(function ($n, $m) { echo "W:$m\n"; return true; });
call_user_func(function () {
    $x = new SplFixedArray(1);
    $x[0] = $x;
    echo "start\n";
    var_export($x); echo "\n";
    debug_zval_dump($x);
});
"#);
    assert_eq!(
        out,
        "start\nW:var_export does not handle circular references\n\
         \\SplFixedArray::__set_state(array(\n   0 => NULL,\n))\n\
         object(SplFixedArray)#3 (1) refcount(4){\n  [0]=>\n  *RECURSION*\n}\n"
    );
}

#[test]
fn object_storage_cycle_counts_only_objects() {
    // Bug #53071: the collected cycle is the three objects.
    let out = run(r#"<?php
class C { public $member; }
function scope() {
    $a = new C();
    $b = new SplObjectStorage();
    $c = new C();
    $c->member = $a;
    $b->offsetSet($c);
    $a->member = $b;
}
scope();
var_dump(gc_collect_cycles());
"#);
    assert_eq!(out, "int(3)\n");
}

#[test]
fn sorting_object_storage_reorders_its_properties() {
    // `ArrayObject::asort()`/`ksort()` over an object sort its property
    // table, declared properties included; the order shows in every
    // listing, survives `clone`, and later additions go last.
    let out = run(r#"<?php
class C { public $p1 = 'x'; public $p2 = 'z'; private $p3 = 'a'; public $p4 = 'y'; }
function keys($a) { return str_replace("\0", '|', implode(',', array_keys($a))); }
$c = new C;
$c->dyn = 'b';
$ao = @new ArrayObject($c);
$ao->asort();
foreach ($c as $k => $v) echo "$k=$v ";
echo "\n", keys(get_object_vars($c)), "\n", keys((array)$c), "\n";
echo str_replace("\0", '|', serialize($c)), "\n";
$ao->ksort();
echo keys((array)$c), "\n";
$c->p1 = 'NEW'; unset($c->dyn); $c->dyn = 'b2'; $c->n = 1;
echo keys((array)$c), "\n";
$d = clone $c;
echo keys(get_object_vars($d)), "\n";
$e = new C;
echo keys(get_object_vars($e)), "\n";
"#);
    assert_eq!(
        out,
        "dyn=b p1=x p4=y p2=z \n\
         dyn,p1,p4,p2\n\
         |C|p3,dyn,p1,p4,p2\n\
         O:1:\"C\":5:{s:5:\"|C|p3\";s:1:\"a\";s:3:\"dyn\";s:1:\"b\";s:2:\"p1\";s:1:\"x\";\
         s:2:\"p4\";s:1:\"y\";s:2:\"p2\";s:1:\"z\";}\n\
         |C|p3,dyn,p1,p2,p4\n\
         |C|p3,p1,p2,p4,dyn,n\n\
         p1,p2,p4,dyn,n\n\
         p1,p2,p4\n"
    );
}

#[test]
fn writes_through_array_object_keep_declaration_order() {
    // Unsetting and re-adding a declared property through the
    // ArrayObject is no reorder: it stays in its declared place.
    let out = run(r#"<?php
class C { public $a = 1; public $b = 2; public $c = 3; }
$o = new C;
$ao = @new ArrayObject($o);
unset($ao['a']);
$ao['x'] = 9;
$ao['a'] = 4;
echo implode(',', array_keys(get_object_vars($o))), "\n";
"#);
    assert_eq!(out, "a,b,c,x\n");
}

#[test]
fn constructor_types_are_reflected_but_parsed_by_the_handler() {
    // Bug #71412: Reflection shows the arginfo types; the handler's own
    // `zend_parse_parameters` messages still report bad arguments.
    let out = run(r#"<?php
foreach ((new ReflectionMethod('ArrayIterator', '__construct'))->getParameters() as $p) {
    echo $p->getType(), " $", $p->getName(), "\n";
}
try { new ArrayIterator("x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { new ArrayIterator([], "y"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump((new ReflectionMethod('ArrayObject', 'uasort'))->getParameters()[0]->isPromoted());
"#);
    assert_eq!(
        out,
        "object|array $array\nint $flags\n\
         ArrayIterator::__construct(): Argument #1 ($array) must be of type array, string given\n\
         ArrayIterator::__construct(): Argument #2 ($flags) must be of type int, string given\n\
         bool(false)\n"
    );
}
