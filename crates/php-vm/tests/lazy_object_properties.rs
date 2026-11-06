//! Lazy ghosts and the properties table (`Zend/zend_lazy_objects.c`,
//! `zend_lazy_object_init`): when the table exists before initialization,
//! the initializer's first dynamic-property write separates it from the
//! snapshot (`zend_array_dup`), freezing the declared properties' values
//! in the table the object keeps (`lazy_objects/jit_assign_obj_op_dynamic`).
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

/// Runs `source` as `t.php` and returns its output.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

const CLASS: &str = r#"<?php
#[AllowDynamicProperties]
class C {
    public int $b = 1;
    public function __construct() {
        $this->a = 1;
        $this->b = 3;
    }
}
$r = new ReflectionClass(C::class);
// `json_encode()` of a property array, with the builtins of the bare VM.
function j(array $props): string {
    $out = [];
    foreach ($props as $k => $v) { $out[] = "\"$k\":$v"; }
    return '{' . implode(',', $out) . '}';
}
"#;

/// `var_dump()` of the ghost builds the table; listings after the
/// initialization show `b` as it was when `$this->a = 1` separated it,
/// reads see the slot (`var_export()` lists the object like
/// `json_encode()` does: through its properties table).
#[test]
fn table_built_before_init_keeps_frozen_declared_values() {
    let src = format!(
        "{CLASS}{}",
        r#"
$o = $r->newLazyGhost(function ($o) { $o->__construct(); });
ob_start(); var_dump($o); ob_end_clean();
$o->a += 1;
var_dump($o->b);
echo j(get_object_vars($o)), "\n";
echo j((array) $o), "\n";
echo var_export($o, true), "\n";
$c = clone $o;
$c->b = 7;
echo j(get_object_vars($c)), " ", $c->b, "\n";
"#
    );
    assert_eq!(
        run(&src),
        "int(3)\n{\"b\":1,\"a\":2}\n{\"b\":1,\"a\":2}\n\\C::__set_state(array(\n   'b' => 1,\n   'a' => 2,\n))\n{\"b\":1,\"a\":2} 7\n"
    );
}

/// Without a table before the initialization nothing is frozen.
#[test]
fn no_table_before_init_lists_live_values() {
    let src = format!(
        "{CLASS}{}",
        r#"
$o = $r->newLazyGhost(function ($o) { $o->__construct(); });
$o->a += 1;
echo j(get_object_vars($o)), "\n";
echo j((array) $o), "\n";
"#
    );
    assert_eq!(run(&src), "{\"b\":3,\"a\":2}\n{\"b\":3,\"a\":2}\n");
}

/// A failed initialization restores the snapshot table: later slot
/// changes show again.
#[test]
fn failed_init_drops_frozen_values() {
    let src = format!(
        "{CLASS}{}",
        r#"
$o = $r->newLazyGhost(function ($o) {
    $o->a = 1;
    $o->b = 3;
    throw new Exception('initializer exception');
});
ob_start(); var_dump($o); ob_end_clean();
try {
    $r->initializeLazyObject($o);
} catch (Exception $e) {
    echo $e->getMessage(), "\n";
}
(new ReflectionProperty(C::class, 'b'))->setRawValueWithoutLazyInitialization($o, 5);
echo j((array) $o), "\n";
"#
    );
    assert_eq!(run(&src), "initializer exception\n{\"b\":5}\n");
}

/// The frozen copy holds its values until the object is freed.
#[test]
fn frozen_values_are_released_with_the_object() {
    let src = r#"<?php
class D { public function __destruct() { echo "D gone\n"; } }
#[AllowDynamicProperties]
class E {
    public $d;
    public function __construct() {
        $this->x = 1;
        $this->d = null;
    }
}
$r = new ReflectionClass(E::class);
$o = $r->newLazyGhost(function ($o) {
    $o->d = new D();
    $o->__construct();
});
ob_start(); var_dump($o); ob_end_clean();
$r->initializeLazyObject($o);
echo "init\n";
unset($o);
echo "end\n";
"#;
    assert_eq!(run(src), "init\nD gone\nend\n");
}
