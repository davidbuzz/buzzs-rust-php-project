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
    let out = run(
        r#"<?php
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
        ?>"#,
    );
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
