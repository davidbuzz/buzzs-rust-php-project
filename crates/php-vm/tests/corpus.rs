// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The differential corpus: every `tests/corpus/<name>.php` runs in a fresh
//! VM and its output must equal `<name>.out` beside it, the output of
//! `/usr/bin/php8.5 -n <name>.php` run in that directory.
//!
//! The scripts exercise what most php-src `.phpt` files do not: code inside
//! functions and methods, each called several times, so that the tape, its
//! call-site and property caches, the prepared calls, the quick builtin
//! paths and the trace JIT take over from the second call on. Every script
//! prints the returned values and the state a call may have changed.
//! `tests/corpus/open/` holds reduced scripts of known differences; they
//! are not run here.
mod common;

use php_vm::VmOptions;
use std::cell::RefCell;
use std::io::Write;
use std::path::Path;
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

fn run(source: &[u8], name: &str) -> Vec<u8> {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let _status = vm.run_script(source, name);
    drop(vm);
    sink.0.take()
}

/// The first line where `actual` and `expected` differ, with both lines.
fn first_difference(actual: &str, expected: &str) -> String {
    let (a, e): (Vec<&str>, Vec<&str>) = (actual.lines().collect(), expected.lines().collect());
    let at = (0..a.len().max(e.len()))
        .find(|&i| a.get(i) != e.get(i))
        .unwrap_or(0);
    format!(
        "line {}:\n  expected: {:?}\n  actual:   {:?}",
        at + 1,
        e.get(at).copied().unwrap_or("<end>"),
        a.get(at).copied().unwrap_or("<end>")
    )
}

#[test]
fn corpus_matches_php() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut scripts: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/corpus exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "php"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no scripts in {}", dir.display());
    let mut failures = Vec::new();
    for path in &scripts {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("UTF-8 file name");
        let source = std::fs::read(path).expect("readable script");
        let expected = std::fs::read(path.with_extension("out"))
            .unwrap_or_else(|_| panic!("{name}: no .out file beside it"));
        let actual = run(&source, name);
        if actual != expected {
            let (a, e) = (
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected),
            );
            failures.push(format!("{name}: {}", first_difference(&a, &e)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} corpus scripts differ from php8.5:\n{}",
        failures.len(),
        scripts.len(),
        failures.join("\n")
    );
}
