//! Shared helpers for integration tests.
#![allow(dead_code)]

use php_openssl::{Level, OpenSsl};

/// Absolute path of a fixture file (copied from php-src ext/openssl/tests).
pub fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// `file://` URL of a fixture.
pub fn fixture_url(name: &str) -> Vec<u8> {
    format!("file://{}", fixture(name)).into_bytes()
}

/// Fixture contents.
pub fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture")
}

/// Drained warning texts.
pub fn warnings(o: &mut OpenSsl) -> Vec<String> {
    o.take_diagnostics()
        .into_iter()
        .filter(|d| d.level == Level::Warning)
        .map(|d| d.message)
        .collect()
}

/// All queued OpenSSL error strings.
pub fn errors(o: &mut OpenSsl) -> Vec<String> {
    std::iter::from_fn(|| o.error_string()).collect()
}

/// Hex decoding for expected values.
pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// A scratch file path unique to this test process.
pub fn tmp(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("php-openssl-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().into_owned()
}
