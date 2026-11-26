// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Module versions (`zend_get_module_version()`) as `phpversion()` and
//! `ReflectionExtension::getVersion()` report them, and the module list of
//! `get_loaded_extensions()`. Expected values are php-src master's module
//! entries (`PHP_VERSION` for every bundled module but `dom`, `mysqlnd` and
//! `zend_test`).
use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::{Vm, VmOptions, PHP_VERSION};
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

/// An extension that only has a name.
struct Named(&'static str);

impl Extension for Named {
    fn name(&self) -> &'static str {
        self.0
    }
    fn register(&self, _module: &mut ModuleBuilder<'_>) {}
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let exts = [
        Named("curl"),
        Named("dom"),
        Named("mysqlnd"),
        Named("zend_test"),
        Named("Zend OPcache"),
        // Adds to Core, as the UPLOAD_ERR_* constants do: not listed twice.
        Named("Core"),
    ];
    let refs: Vec<&dyn Extension> = exts.iter().map(|e| e as &dyn Extension).collect();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &refs);
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn phpversion_reports_each_loaded_modules_version() {
    let out = run(r#"<?php
foreach (["core", "standard", "curl", "CURL", "dom", "mysqlnd", "zend_test",
          "Zend OPcache", "opcache", "nope"] as $e) {
    echo $e, "=", var_export(phpversion($e), true), "\n";
}
echo var_export(phpversion(), true), var_export(phpversion(null), true), "\n";
"#);
    let v = PHP_VERSION;
    assert_eq!(
        out,
        format!(
            "core='{v}'\nstandard='{v}'\ncurl='{v}'\nCURL='{v}'\ndom='20031129'\n\
             mysqlnd='mysqlnd {v}'\nzend_test='0.1.0'\nZend OPcache='{v}'\n\
             opcache=false\nnope=false\n'{v}''{v}'\n"
        )
    );
}

#[test]
fn reflection_extension_get_version_matches_phpversion() {
    let out = run(r#"<?php
foreach (["Core", "curl", "dom", "mysqlnd", "zend_test"] as $e) {
    echo $e, "=", (new ReflectionExtension($e))->getVersion(), "\n";
}
"#);
    let v = PHP_VERSION;
    assert_eq!(
        out,
        format!("Core={v}\ncurl={v}\ndom=20031129\nmysqlnd=mysqlnd {v}\nzend_test=0.1.0\n")
    );
}

#[test]
fn a_module_adding_to_core_is_not_listed_again() {
    let out = run(r#"<?php
$core = 0;
foreach (get_loaded_extensions() as $i => $m) {
    if ($m === "Core") { $core++; }
    if ($i >= 8) { echo $m, ","; }
}
echo " ", $core, "\n";
"#);
    assert_eq!(out, "curl,dom,mysqlnd,zend_test,Zend OPcache, 1\n");
}
