// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `memory_limit` reservations of `json_encode()`/`json_decode()`, run as
//! PHP scripts on a VM with the `json` extension. Its own test binary with
//! a single test: the limit and a pending violation are process-wide.

#![allow(clippy::needless_raw_string_hashes)]

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
    let ext = php_ext_json::Json;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    php_memlimit::clear();
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// A growth or a string larger than `memory_limit` is refused before it is
/// allocated, and the VM raises the fatal after the call. (In this test
/// binary the counting allocator is not installed, so only such single
/// reservations can exceed the limit; crates/php-cli/tests/json_memory_limit.rs
/// covers usage that adds up.) Figures from `php8.5 -n -d memory_limit=16M`.
#[test]
fn reservations_beyond_memory_limit_are_fatal() {
    // The encoder's pre-allocation of the string plus its quotes
    // (`smart_str_falloc(buf, len + 2, 0, true)`), page-rounded.
    // (8M here: this binary counts no usage, so the reservation alone has
    // to pass the limit; php8.5 reports the same figure under 16M, where
    // the string and its encoding add up.)
    let out = run(
        r#"ini_set("memory_limit", "8M"); json_encode(str_repeat("a", 9 << 20)); echo "not reached";"#,
    );
    assert!(
        out.contains(
            "Allowed memory size of 8388608 bytes exhausted (tried to allocate 9441280 bytes)"
        ),
        "{out}"
    );
    assert!(!out.contains("not reached"));

    // json_decode_oom_utf8_substitute.phpt: the 30 MiB value of 10 MiB of
    // invalid UTF-8 is reserved before it is built (php8.5 reports the
    // page-rounded 31461376; an accounting of exact bytes 31457304).
    let out = run(
        r#"ini_set("memory_limit", "16M"); $n = 10 << 20; $j = str_repeat("\x80", $n); $j[0] = '"'; $j[$n - 1] = '"'; json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); echo "not reached";"#,
    );
    assert!(
        out.contains(
            "Allowed memory size of 16777216 bytes exhausted (tried to allocate 31461376 bytes)"
        ) || out.contains(
            "Allowed memory size of 16777216 bytes exhausted (tried to allocate 31457304 bytes)"
        ),
        "{out}"
    );
    assert!(!out.contains("not reached"));

    // Even with JSON_THROW_ON_ERROR it is the fatal, not an exception.
    let out = run(
        r#"ini_set("memory_limit", "16M"); try { json_decode('"' . str_repeat("a", 20 << 20) . '"', false, 512, JSON_THROW_ON_ERROR); } catch (JsonException $e) { echo "caught"; } echo "not reached";"#,
    );
    assert!(
        out.contains("Allowed memory size of 16777216 bytes exhausted"),
        "{out}"
    );
    assert!(
        !out.contains("caught") && !out.contains("not reached"),
        "{out}"
    );

    // Under the limit the same calls succeed.
    let out = run(
        r#"ini_set("memory_limit", "64M"); var_dump(strlen(json_encode(str_repeat("a", 9 << 20)))); $n = 1 << 20; $j = str_repeat("\x80", $n); $j[0] = '"'; $j[$n - 1] = '"'; var_dump(strlen(json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE)));"#,
    );
    assert_eq!(out, "int(9437186)\nint(3145722)\n");
}
