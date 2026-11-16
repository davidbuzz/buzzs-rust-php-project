// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! JIT auto-globals (`zend_is_auto_global`, `zend_activate_auto_globals`;
//! ref: Zend/zend_compile.c, main/php_variables.c): an armed `$_ENV` /
//! `$_REQUEST` exists only once compiled code names it. Expected outputs
//! come from the C implementation (`php -n`, auto_globals_jit=1).

use php_runtime::{Value, ZArray, ZStr};
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

/// Creates the auto-global as `['made' => <name>]`.
fn create(vm: &mut Vm, name: &[u8]) {
    let mut arr = ZArray::new();
    arr.make_mut().insert(
        php_runtime::ArrayKey::from("made"),
        Value::Str(ZStr::from(name)),
    );
    let mut g = vm.globals.borrow_mut();
    let i = g.find_or_add(name) as usize;
    g.slots[i] = Some(Value::Array(arr));
}

/// Runs `source` with `$_ENV` and `$_REQUEST` armed (and a plain `$_SERVER`).
fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    {
        let mut g = vm.globals.borrow_mut();
        let i = g.find_or_add(b"_SERVER") as usize;
        g.slots[i] = Some(Value::Array(ZArray::new()));
    }
    assert!(vm.arm_auto_global(b"_ENV", create));
    assert!(vm.arm_auto_global(b"_REQUEST", create));
    assert!(!vm.arm_auto_global(b"argv", create));
    // `array_keys` and `count` are not php-vm builtins.
    let source = format!(
        "{source}\nfunction keys() {{ $o = []; foreach ($GLOBALS as $k => $v) {{ $o[] = $k; }} return implode(',', $o); }}\n\
         function cnt($a) {{ $n = 0; foreach ($a as $_) {{ $n++; }} return $n; }}\n"
    );
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn unreferenced_jit_auto_globals_are_absent() {
    assert_eq!(
        run(r#"<?php
echo keys(), "\n";
$x = "_ENV";
var_dump(isset($$x), isset($GLOBALS["_REQUEST"]));
"#),
        "_SERVER\nbool(false)\nbool(false)\n"
    );
}

#[test]
fn compiled_references_create_them_in_first_seen_order() {
    assert_eq!(
        run(r#"<?php
echo keys(), "\n";
function f() { return $_REQUEST; }
$_ENV['x'] = 1;
echo $_ENV['made'], " ", f()['made'], "\n";
"#),
        "_SERVER,_REQUEST,_ENV\n_ENV _REQUEST\n"
    );
}

#[test]
fn eval_and_constant_var_var_names_create_them() {
    // The whole file compiles first: `${'_REQUEST'}` in g() exists before
    // the first line runs; eval() compiles `$_ENV` later.
    assert_eq!(
        run(r#"<?php
echo cnt($GLOBALS), "\n";
eval('$_ENV;');
echo keys(), "\n";
function g() { return ${'_REQUEST'}['made']; }
echo g(), "\n";
"#),
        "2\n_SERVER,_REQUEST,_ENV\n_REQUEST\n"
    );
}

#[test]
fn is_auto_global_creates_an_armed_global_once() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink));
    assert!(vm.arm_auto_global(b"_ENV", create));
    assert!(vm.is_auto_global(b"_ENV"));
    {
        let mut g = vm.globals.borrow_mut();
        let i = g.find(b"_ENV").expect("created") as usize;
        g.slots[i] = Some(Value::Int(7));
    }
    // Disarmed: asking again does not recreate it.
    assert!(vm.is_auto_global(b"_ENV"));
    assert!(!vm.is_auto_global(b"GLOBALS"));
    let mut g = vm.globals.borrow_mut();
    let i = g.find(b"_ENV").expect("still there") as usize;
    assert!(matches!(g.slots[i], Some(Value::Int(7))));
}
