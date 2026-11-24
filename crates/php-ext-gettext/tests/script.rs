// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `gettext` extension.
//! Expected outputs come from `/usr/bin/php8.5` (php-src master messages).

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
    let ext = php_ext_gettext::Gettext;
    let exts: [&dyn Extension; 1] = [&ext];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn untranslated_messages_pass_through() {
    assert_eq!(
        run("echo gettext('hello'), '|', _('x'), '|', ngettext('a', 'b', 1), ngettext('a', 'b', 2), '|', dgettext('d', 'm');"),
        "hello|x|ab|m"
    );
}

#[test]
fn value_errors_carry_the_function_name() {
    assert_eq!(
        run("try { textdomain(''); } catch (ValueError $e) { echo $e->getMessage(), \"\\n\"; }
             try { dcgettext('d', 'm', 6); } catch (ValueError $e) { echo $e->getMessage(), \"\\n\"; }
             try { textdomain('0'); } catch (ValueError $e) { echo $e->getMessage(), \"\\n\"; }
             try { bindtextdomain('', 'foo'); } catch (ValueError $e) { echo $e->getMessage(); }"),
        "textdomain(): Argument #1 ($domain) must not be empty\n\
         dcgettext(): Argument #3 ($category) cannot be LC_ALL\n\
         textdomain(): Argument #1 ($domain) cannot be zero\n\
         bindtextdomain(): Argument #1 ($domain) must not be empty"
    );
}

#[test]
fn bindtextdomain_returns_false_for_missing_directory() {
    assert_eq!(
        run("var_dump(bindtextdomain('messages', '/this/does/not/exist'));"),
        "bool(false)\n"
    );
}

#[test]
fn textdomain_query_and_set() {
    assert_eq!(
        run("echo textdomain('foo'), '|', textdomain(null);"),
        "foo|foo"
    );
}
