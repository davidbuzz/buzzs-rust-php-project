//! End-to-end tests: PHP scripts run on a VM with the `bcmath` extension.
//! Expected output comes from the php-src `.phpt` files / `/usr/bin/php`.

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

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = php_ext_bcmath::Bcmath;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn functions_and_scale() {
    assert_eq!(
        run(
            r"echo bcadd('1.5', '2.25', 2), ' ', bcdiv('1', '3', 5), ' ', bcscale(3), ' ', bcmul('2', '3'), ' ', ini_get('bcmath.scale');"
        ),
        "3.75 0.33333 0 6.000 3"
    );
}

#[test]
fn errors() {
    assert_eq!(
        run(
            r"try { bcadd('x', '1'); } catch (ValueError $e) { echo $e->getMessage(); } try { bcdiv('1', '0'); } catch (DivisionByZeroError $e) { echo ' ', $e->getMessage(); }"
        ),
        "bcadd(): Argument #1 ($num1) is not well-formed Division by zero"
    );
}

#[test]
fn number_object() {
    assert_eq!(
        run(
            r"$n = new BcMath\Number('1.25'); $r = $n + '0.5'; echo $r, ' ', $r->scale, ' ', $n->mul(2), ' ', $n <=> 2, ' ', (int) isset($n->value), ' ', serialize($n);"
        ),
        "1.75 2 2.50 -1 1 O:13:\"BcMath\\Number\":1:{s:5:\"value\";s:4:\"1.25\";}"
    );
}

#[test]
fn readonly_properties() {
    assert_eq!(
        run(
            r"$n = new BcMath\Number(1); try { $n->value = 2; } catch (Error $e) { echo $e->getMessage(); }"
        ),
        "Cannot modify readonly property BcMath\\Number::$value"
    );
}

#[test]
fn rounding_mode_enum() {
    assert_eq!(
        run(r"echo bcround('2.5', 0, RoundingMode::HalfEven), ' ', bcround('2.5');"),
        "2 3"
    );
}
