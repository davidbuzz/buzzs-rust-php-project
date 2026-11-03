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

/// PHP `var_export()` of a value (bytes, for comparing with oracle dumps).
pub fn var_export(v: &php_openssl::PhpValue) -> Vec<u8> {
    let mut out = Vec::new();
    export(v, 0, &mut out);
    out
}

fn export_str(s: &[u8], out: &mut Vec<u8>) {
    out.push(b'\'');
    for &b in s {
        match b {
            0 => out.extend_from_slice(b"' . \"\\0\" . '"),
            b'\'' => out.extend_from_slice(b"\\'"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(b),
        }
    }
    out.push(b'\'');
}

fn export(v: &php_openssl::PhpValue, indent: usize, out: &mut Vec<u8>) {
    use php_openssl::{Key, PhpValue};
    match v {
        PhpValue::Null => out.extend_from_slice(b"NULL"),
        PhpValue::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        PhpValue::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        PhpValue::Float(f) => out.extend_from_slice(f.to_string().as_bytes()),
        PhpValue::Str(s) => export_str(s, out),
        PhpValue::Object => out.extend_from_slice(b"object"),
        PhpValue::Array(a) => {
            out.extend_from_slice(b"array (\n");
            for (k, v) in a.iter() {
                out.extend(std::iter::repeat_n(b' ', indent + 2));
                match k {
                    Key::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
                    Key::Str(s) => export_str(s, out),
                }
                out.extend_from_slice(b" => ");
                if let PhpValue::Array(_) = v {
                    out.push(b'\n');
                    out.extend(std::iter::repeat_n(b' ', indent + 2));
                }
                export(v, indent + 2, out);
                out.extend_from_slice(b",\n");
            }
            out.extend(std::iter::repeat_n(b' ', indent));
            out.push(b')');
        }
    }
}
