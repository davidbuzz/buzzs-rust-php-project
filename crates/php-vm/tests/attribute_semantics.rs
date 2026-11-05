//! Attribute regressions (Zend/tests/attributes): `#[\Override]`,
//! `#[\NoDiscard]`, attribute instantiation.
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

/// Runs `source` as `t.php`, returning the exit status and the output.
fn run_status(source: &str) -> (i32, Vec<u8>) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, output)
}

fn run(source: &str) -> String {
    let (status, output) = run_status(source);
    let output = String::from_utf8_lossy(&output).into_owned();
    assert_eq!(status, 0, "{output}");
    output
}

fn fatal(source: &str) -> String {
    let (status, output) = run_status(source);
    let output = String::from_utf8_lossy(&output).into_owned();
    assert_ne!(status, 0, "{output}");
    output
}

#[test]
fn override_on_constructor_needs_an_abstract_prototype() {
    // override/021: a concrete parent constructor is no prototype.
    let out = fatal(
        "<?php\nclass P { public function __construct() {} }\nclass C extends P {\n    #[\\Override]\n    public function __construct() {}\n}\necho \"Done\";\n",
    );
    assert!(
        out.contains(
            "C::__construct() has #[\\Override] attribute, but no matching parent method exists"
        ),
        "{out}"
    );
    // An abstract parent constructor (or an interface one) is.
    let out = run(
        "<?php\nabstract class P { abstract public function __construct(); }\nclass C extends P {\n    #[\\Override]\n    public function __construct() {}\n}\ninterface I { public function __construct(); }\nclass D implements I {\n    #[\\Override]\n    public function __construct() {}\n}\necho \"Done\";\n",
    );
    assert_eq!(out, "Done");
}

#[test]
fn override_on_trait_constructor_needs_an_abstract_prototype() {
    // override/gh12189_4.
    let out = fatal(
        "<?php\nclass A { public function __construct() {} }\ntrait T {\n    #[\\Override]\n    public function __construct() {}\n}\nclass D extends A { use T; }\necho \"Done\";\n",
    );
    assert!(
        out.contains(
            "D::__construct() has #[\\Override] attribute, but no matching parent method exists"
        ),
        "{out}"
    );
}

#[test]
fn override_on_static_property() {
    // override/properties_17.
    let out = fatal(
        "<?php\nclass C {\n    #[\\Override]\n    public static mixed $c;\n}\necho \"Done\";\n",
    );
    assert!(
        out.contains("C::$c has #[\\Override] attribute, but no matching parent property exists"),
        "{out}"
    );
    let out = run(
        "<?php\nclass P { public static mixed $c; }\nclass C extends P {\n    #[\\Override]\n    public static mixed $c;\n}\necho \"Done\";\n",
    );
    assert_eq!(out, "Done");
}

#[test]
fn override_on_anonymous_class_constant_prints_the_whole_name() {
    // override/constants/anon_failure: `%pS` keeps the NUL-separated suffix.
    let (status, out) =
        run_status("<?php\nnew class () {\n    #[\\Override]\n    public const C = 'C';\n};\n");
    assert_ne!(status, 0);
    let needle = b"class@anonymous\0t.php:";
    assert!(
        out.windows(needle.len()).any(|w| w == needle),
        "{}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn nodiscard_through_direct_call_user_func() {
    // nodiscard/001.
    let out = run(
        "<?php\n#[\\NoDiscard]\nfunction test(): int { return 0; }\ncall_user_func(\"test\");\n$x = call_user_func(\"test\");\n(void) call_user_func(\"test\");\n",
    );
    assert_eq!(
        out.matches("The return value of function test() should either be used")
            .count(),
        1,
        "{out}"
    );
}

#[test]
fn nodiscard_attribute_on_hook_fails_on_new_instance() {
    // delayed_target_validation/validator_NoDiscard.
    let out = run(
        "<?php\nclass D {\n    public string $h {\n        #[DelayedTargetValidation]\n        #[NoDiscard]\n        get => 'x';\n    }\n}\n$r = new ReflectionProperty('D', 'h')->getHook(PropertyHookType::Get);\ntry { $r->getAttributes()[1]->newInstance(); } catch (Error $e) { echo $e->getMessage(); }\n",
    );
    assert_eq!(out, "#[\\NoDiscard] is not supported for property hooks");
}

#[test]
fn anonymous_class_attribute_self_is_the_anonymous_class() {
    // 013_class_scope.
    let out = run(
        "<?php\nclass C3 {\n    public static function foo() {\n        return new #[A1(self::class)] class() {};\n    }\n}\n$ref = new ReflectionObject(C3::foo());\nvar_dump($ref->getAttributes()[0]->getArguments()[0] === $ref->getName());\n",
    );
    assert_eq!(out, "bool(true)\n");
}

#[test]
fn attribute_constructor_errors_name_the_attribute_site() {
    // 005_objects, 030_strict_types.
    let src = "<?php\n#[Attribute]\nclass A { public function __construct(public int $v) {} }\n#[A(\"42\")]\nclass W {}\ntry { (new ReflectionClass('W'))->getAttributes()[0]->newInstance(); echo \"ok\\n\"; } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; }\n#[A]\nclass X {}\ntry { (new ReflectionClass('X'))->getAttributes()[0]->newInstance(); } catch (ArgumentCountError $e) { echo $e->getMessage(), \"\\n\"; }\n";
    assert_eq!(
        run(src),
        "ok\nToo few arguments to function A::__construct(), 0 passed in t.php on line 7 and exactly 1 expected\n"
    );
    let strict = src.replacen("<?php\n", "<?php declare(strict_types=1);\n", 1);
    assert_eq!(
        run(&strict),
        "A::__construct(): Argument #1 ($v) must be of type int, string given, called in t.php on line 4\nToo few arguments to function A::__construct(), 0 passed in t.php on line 7 and exactly 1 expected\n"
    );
}
