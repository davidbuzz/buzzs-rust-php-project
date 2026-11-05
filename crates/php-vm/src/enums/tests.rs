//! Script-level tests of enums and callable resolution.

use crate::builtins::classbuilder::ClassBuilder;
use crate::{Vm, VmOptions};
use php_runtime::Value;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

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

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

const CATCH: &str = "catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), \"\\n\"; }";

#[test]
fn from_and_try_from() {
    let out = run(&format!(
        "<?php enum Foo: int {{ case A = 1; case B = 2; }} enum S: string {{ case A = 'a'; }}\n\
         var_dump(Foo::from(2), Foo::tryFrom(3), Foo::from('1'), S::tryFrom('b'));\n\
         try {{ Foo::from(3); }} {CATCH}\n\
         try {{ S::from('x'); }} {CATCH}\n\
         try {{ Foo::from('x'); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "enum(Foo::B)\nNULL\nenum(Foo::A)\nNULL\n\
         ValueError: 3 is not a valid backing value for enum Foo\n\
         ValueError: \"x\" is not a valid backing value for enum S\n\
         TypeError: Foo::from(): Argument #1 ($value) must be of type int, string given\n"
    );
}

#[test]
fn duplicate_and_mismatched_backing_values() {
    let out = run(&format!(
        "<?php enum D: int {{ case A = 0; case B = 0; }} enum M: int {{ case A = 'a'; }}\n\
         try {{ var_dump(D::A); }} {CATCH}\n\
         try {{ var_dump(D::tryFrom(1)); }} {CATCH}\n\
         try {{ var_dump(M::A); }} {CATCH}\n\
         try {{ var_dump(M::A); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "Error: Duplicate value in enum D for cases A and B\n\
         Error: Duplicate value in enum D for cases A and B\n\
         TypeError: Enum case type string does not match enum backing type int\n\
         TypeError: Enum case type string does not match enum backing type int\n"
    );
}

#[test]
fn internal_backed_enum_cases_are_materialized_individually() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    ClassBuilder::new("InternalBackedEnum")
        .backed_enum("string")
        .case("First", Some(Value::Str("same".into())))
        .case("Second", Some(Value::Str("same".into())))
        .register(&mut vm);
    let script = concat!(
        "<?php echo InternalBackedEnum::First->name, '|', ",
        "InternalBackedEnum::First->value, '|', InternalBackedEnum::Second->name, '|', ",
        "InternalBackedEnum::Second->value;"
    );
    vm.run_script(script.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    assert_eq!(String::from_utf8_lossy(&out), "First|same|Second|same");
}

#[test]
fn cases_and_identity() {
    let out = run("<?php enum E { case X; case Y; }\n\
         var_dump(E::cases(), E::X === E::cases()[0], E::X == E::Y, E::X instanceof UnitEnum);");
    assert_eq!(
        out,
        "array(2) {\n  [0]=>\n  enum(E::X)\n  [1]=>\n  enum(E::Y)\n}\nbool(true)\nbool(false)\nbool(true)\n"
    );
}

#[test]
fn callable_resolution_errors() {
    let out = run(
        "<?php abstract class A { abstract static function a(); protected function p() {}\n\
         static function t() { var_dump(is_callable('self::p'), is_callable('parent::x')); } }\n\
         class B extends A { static function a() {} }\n\
         foreach (['A::a', [new B, 'p'], 'B::x', 'Nope::x', 'self::x', [1], 42] as $c) {\n\
         try { Closure::fromCallable($c); } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; } }\n\
         A::t();",
    );
    assert_eq!(
        out,
        "Failed to create closure from callable: cannot call abstract method A::a()\n\
         Failed to create closure from callable: cannot access protected method B::p()\n\
         Failed to create closure from callable: class B does not have a method \"x\"\n\
         Failed to create closure from callable: class \"Nope\" not found\n\
         Failed to create closure from callable: cannot access \"self\" when no class scope is active\n\
         Failed to create closure from callable: array callback must have exactly two members\n\
         Failed to create closure from callable: no array or string given\n\
         \nDeprecated: Use of \"self\" in callables is deprecated in t.php on line 2\n\
         bool(false)\nbool(false)\n"
    );
}
