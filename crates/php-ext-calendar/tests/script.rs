// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `calendar` extension.
//! Expected output comes from the php-src `.phpt` files / `/usr/bin/php8.5`.

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
    let ext = php_ext_calendar::Calendar;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn conversions() {
    assert_eq!(
        run(
            r"echo gregoriantojd(10, 11, 1970), ' ', jdtogregorian(2440871), ' ', jddayofweek(2440871, 1), ' ', cal_days_in_month(CAL_GREGORIAN, 2, 2000);"
        ),
        "2440871 10/11/1970 Sunday 29"
    );
}

#[test]
fn cal_from_jd_shape() {
    assert_eq!(
        run(
            r"$a = cal_from_jd(2440871, CAL_JULIAN); echo $a['date'], ' ', $a['dayname'], ' ', $a['monthname'];"
        ),
        "9/28/1970 Sunday September"
    );
}

#[test]
fn value_errors() {
    assert_eq!(
        run(r"try { cal_to_jd(-1, 1, 1, 1); } catch (ValueError $e) { echo $e->getMessage(); }"),
        "cal_to_jd(): Argument #1 ($calendar) must be a valid calendar ID"
    );
    assert_eq!(
        run(r"try { easter_date(1492); } catch (ValueError $e) { echo $e->getMessage(); }"),
        "easter_date(): Argument #1 ($year) must be a year after 1970 (inclusive)"
    );
}

#[test]
fn easter_and_unix() {
    assert_eq!(
        run(
            r"echo easter_days(2000), ' ', jdtounix(2440588), ' ', unixtojd(1000000000) > 2452000 ? 'ok' : 'bad';"
        ),
        "33 0 ok"
    );
}
