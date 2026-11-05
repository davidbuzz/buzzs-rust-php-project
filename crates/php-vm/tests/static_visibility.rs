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

#[test]
fn instance_visibility_precedes_static_storage_lookup() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(
        br#"<?php
class A {
    private $pri;
    protected $pro;
    public $pub;
    static function inside() { return self::$pri; }
}
foreach (['pri', 'pro', 'pub', 'missing'] as $name) {
    try { A::$$name; } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
try { A::inside(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
        "visibility.php",
    );
    assert_eq!(
        sink.0.borrow().as_slice(),
        b"Cannot access private property A::$pri\nCannot access protected property A::$pro\nAccess to undeclared static property A::$pub\nAccess to undeclared static property A::$missing\nAccess to undeclared static property A::$pri\n"
    );
}

/// `isset()`/`empty()`/`??` fetch static properties in `BP_VAR_IS` mode:
/// undeclared or inaccessible ones are quietly unset, while an unknown
/// class still throws (Zend/zend_execute.c
/// `zend_fetch_static_property_address()`).
#[test]
fn isset_on_inaccessible_static_property_is_quiet() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(
        br#"<?php
class A {
    private static $priv = 7;
    public static $arr = [1];
    private $inst;
    static function inside() { var_dump(isset(self::$priv), self::$priv ?? 0); }
}
var_dump(isset(A::$priv), empty(A::$priv), A::$priv ?? 'd');
var_dump(isset(A::$nope), A::$nope[1] ?? 'e', isset(A::$inst), isset(A::$arr[0]));
A::inside();
try { var_dump(isset(B::$x)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
        "isset_static.php",
    );
    let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
    assert_eq!(
        out,
        "bool(false)\nbool(true)\nstring(1) \"d\"\nbool(false)\nstring(1) \"e\"\nbool(false)\nbool(true)\nbool(true)\nint(7)\nClass \"B\" not found\n"
    );
}
