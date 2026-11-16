// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `get_open_basedir()` (zend_test) returns `PG(open_basedir)`: the
//! directories a runtime `ini_set()` expanded (GH-10469).

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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let zend_test = php_ext_zendtest::ZendTest;
    let exts: [&dyn Extension; 2] = [&streams, &zend_test];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn runtime_open_basedir_is_expanded() {
    let real = std::fs::canonicalize(std::env::temp_dir()).expect("temp dir");
    let dir = real.to_string_lossy();
    let out = run(&format!(
        r#"chdir('{dir}');
        ini_set('open_basedir', '{dir}');
        ini_set('open_basedir', ini_get('open_basedir') . ':./sub/..');
        echo ini_get('open_basedir'), '|', get_open_basedir();"#
    ));
    assert_eq!(out, format!("{dir}:./sub/..|{dir}:{dir}"));
}

#[test]
fn unset_open_basedir_is_null() {
    assert_eq!(run("var_dump(get_open_basedir());"), "NULL\n");
}
