//! End-to-end tests: PHP scripts run on a VM with the `gmp` extension.
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
    let ext = php_ext_gmp::Gmp;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn functions() {
    assert_eq!(
        run(
            r"echo gmp_strval(gmp_add('0x10', 1)), ' ', gmp_strval(gmp_pow(2, 70)), ' ', gmp_strval(gmp_init('ff', 16), 2), ' ', gmp_cmp(1, '2');"
        ),
        "17 1180591620717411303424 11111111 -1"
    );
}

#[test]
fn operators_and_casts() {
    assert_eq!(
        run(
            r"$a = gmp_init(10); $b = $a ** 3 - 1; $b++; echo $b, ' ', $a << 2, ' ', -$a, ' ', ~$a, ' ', (int) ($a % 3), ' ', var_export($a > 9, true);"
        ),
        "1000 40 -10 -11 1 true"
    );
}

#[test]
fn errors() {
    assert_eq!(
        run(r"try { gmp_add([], 1); } catch (TypeError $e) { echo $e->getMessage(); } try { gmp_init(1) + 'x'; } catch (ValueError $e) { echo ' ', $e->getMessage(); }"),
        "gmp_add(): Argument #1 ($num1) must be of type GMP|string|int, array given Number is not an integer string"
    );
}

#[test]
fn serialization_and_dump() {
    assert_eq!(
        run(r"$s = serialize(gmp_init(42)); echo $s, ' ', unserialize($s); var_dump(gmp_init(-7));"),
        "O:3:\"GMP\":1:{i:0;s:2:\"2a\";} 42object(GMP)#1 (1) {\n  [\"num\"]=>\n  string(2) \"-7\"\n}\n"
    );
}

#[test]
fn seeded_random_is_deterministic() {
    let a = run("gmp_random_seed(100); echo gmp_strval(gmp_random_bits(64));");
    let b = run("gmp_random_seed(100); echo gmp_strval(gmp_random_bits(64));");
    assert_eq!(a, b);
    assert!(!a.is_empty());
}
