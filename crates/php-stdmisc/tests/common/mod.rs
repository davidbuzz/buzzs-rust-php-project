//! Shared helpers for the oracle fixture tests.
//!
//! Fixtures live in `tests/fixtures/*.txt` and are produced by the PHP
//! scripts in `tests/gen/` run with `/usr/bin/php` (see the ignored
//! `regenerate_fixtures` test in `tests/regenerate.rs`).

#![allow(dead_code)]

use std::path::PathBuf;

/// Path of a fixture file.
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Reads a fixture file as lines.
pub fn fixture_lines(name: &str) -> Vec<String> {
    let path = fixture_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    text.lines().map(str::to_string).collect()
}

/// Decodes lowercase hex.
pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd hex length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

/// Splits a fixture line into its space-separated fields (empty fields kept).
pub fn fields(line: &str) -> Vec<&str> {
    line.split(' ').collect()
}

/// Shows bytes readably in assertion messages.
pub fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `var_export()` of a value.
pub fn export(v: &php_runtime::Value) -> String {
    let mut ctx = php_runtime::CollectingContext::new();
    String::from_utf8_lossy(&php_runtime::dump::var_export(v, &mut ctx).expect("var_export"))
        .into_owned()
}
