// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! An included file runs in the global scope: its variables are the
//! globals, and a write through `$GLOBALS[...]` there must not take back
//! what the file assigned to them before (`ZEND_ASSIGN` on a CV of the
//! global scope writes the symbol table's slot itself in php-src).

use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
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

/// A directory for one test, removed again on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "phprust-include-globals-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp dir");
        Dir(std::fs::canonicalize(&base).expect("real temp dir"))
    }

    fn file(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("file");
        path.display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `main` (with `{inc}` replaced by the path of the file `inc`) and
/// returns its output.
fn run(name: &str, inc: &str, main: &str) -> String {
    let dir = Dir::new(name);
    let inc_path = dir.file("inc.php", inc);
    let main = main.replace("{inc}", &inc_path);
    let main_path = dir.file("main.php", &main);
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(main.as_bytes(), &main_path);
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn a_globals_write_in_an_included_file_keeps_its_earlier_assignments() {
    // A reference into `$GLOBALS`, and a plain write to it, after the
    // file assigned a variable; included twice, with and without the
    // variable defined before.
    for (pre, expect) in [
        ("", "1 1\n2 2\n"),
        ("$f = 0;", "1 1\n2 2\n"),
        ("$f = 5;", "6 6\n7 7\n"),
    ] {
        let out = run(
            "ref",
            "<?php\n$f = ($f ?? 0) + 1; $r = &$GLOBALS['x']; $GLOBALS['g'] = $f;\n",
            &format!(
                "<?php\n{pre}\ninclude '{{inc}}'; echo $f, ' ', $g, \"\\n\";\ninclude '{{inc}}'; echo $f, ' ', $g, \"\\n\";\n"
            ),
        );
        assert_eq!(out, expect, "with {pre:?}");
    }
}

#[test]
fn an_included_file_sees_its_own_globals_writes() {
    let out = run(
        "own",
        "<?php\n$a = 1; $GLOBALS['b'] = $a + 1; $c = $b + 1; $GLOBALS['a'] = 10; echo $a, $b, $c, \"\\n\";\n",
        "<?php\ninclude '{inc}'; echo $a, ' ', $b, ' ', $c, \"\\n\";\n",
    );
    assert_eq!(out, "1023\n10 2 3\n");
}
