// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Methods the dual iterators forward to their inner iterator
//! (`spl_dual_it_get_method`, `ext/spl/spl_iterators.c`).

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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("error_reporting".into(), "8191".into())],
    };
    let mut vm = Vm::with_extensions(
        &options,
        Box::new(sink.clone()),
        &[
            &php_ext_spl::Spl,
            &php_ext_stdarr::StdArray,
            &php_ext_stdstr::StdStr,
        ],
    );
    assert_eq!(vm.run_script(code.as_bytes(), "test.php"), 0);
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

#[test]
fn method_exists_sees_inner_iterator_methods() {
    // Bug #76901: `spl_dual_it_get_method` resolves methods of the inner
    // iterator, and `method_exists()` asks `get_method`.
    let out = run(r#"<?php
$it = new IteratorIterator(new ArrayIterator([1, 2, 3]));
var_dump(method_exists($it, 'offsetGet'), method_exists($it, 'nope'));
var_dump(method_exists('IteratorIterator', 'offsetGet'));
foreach ($it as $v) {
    if (method_exists($it, 'offsetGet')) {
        echo $it->offsetGet(0), "\n";
    }
}
"#);
    assert_eq!(out, "bool(true)\nbool(false)\nbool(false)\n1\n1\n1\n");
}
