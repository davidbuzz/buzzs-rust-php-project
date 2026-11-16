// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Port of php-src `openssl_error_string_basic_openssl3.phpt`.

mod common;
use common::*;
use php_openssl::{CertArg, CsrArg, KeyArg, OpenSsl, PhpArray, PhpValue};

/// Checks that every expected code appears among the queued errors (and
/// drains the queue), like the phpt's `expect_openssl_errors`.
fn expect(o: &mut OpenSsl, name: &str, codes: &[&str]) {
    let errs = errors(o);
    for c in codes {
        assert!(
            errs.iter().any(|e| e.contains(&format!(":{c}:"))),
            "{name}: no error code {c} in {errs:?}"
        );
    }
    let _ = o.take_diagnostics();
}

#[test]
fn error_queue_phpt() {
    let mut o = OpenSsl::new();
    let output_file = tmp("errq_output.tmp");
    let invalid_read = format!("file://{}", fixture("invalid_file_for_read_operation.txt"));
    let invalid_write = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let crt = fixture_url("cert.crt");
    let csr = fixture_url("cert.csr");
    let public_key = fixture_url("public.key");
    let private_key = fixture_url("private_rsa_1024.key");
    let with_pass = fixture_url("private_rsa_2048_pass_php.key");
    let mut options = PhpArray::new();
    options.set("config", PhpValue::str(fixture("openssl.cnf")));

    let _ = o.pkey_export_to_file(
        &KeyArg::Str(invalid_read.as_bytes()),
        output_file.as_bytes(),
        None,
        Some(&options),
    );
    expect(&mut o, "pkey_export_to_file opening", &["10000080"]);
    let _ = o.pkey_export_to_file(
        &KeyArg::Str(&csr),
        output_file.as_bytes(),
        None,
        Some(&options),
    );
    expect(&mut o, "pkey_export_to_file pem", &["1E08010C"]);
    let _ = o.pkey_export_to_file(
        &KeyArg::Str(&private_key),
        invalid_write.as_bytes(),
        None,
        Some(&options),
    );
    expect(&mut o, "pkey_export_to_file write", &["10080002"]);
    let _ = o.pkey_export(&KeyArg::Str(&with_pass), Some(b"wrong pwd"), Some(&options));
    expect(&mut o, "pkey_export", &["1C800064", "04800065"]);
    let _ = o.private_encrypt(b"data", &KeyArg::Str(&private_key), 1000);
    expect(&mut o, "private_encrypt", &["1C8000A5"]);
    let _ = o.private_decrypt(b"data", &KeyArg::Str(&private_key), 4, None);
    expect(&mut o, "private_decrypt", &["02000079"]);
    let _ = o.public_encrypt(b"data", &KeyArg::Str(&public_key), 1000, None);
    let _ = o.public_decrypt(b"data", &KeyArg::Str(&public_key), 4);
    expect(&mut o, "public padding", &["1C8000A5"]);

    let _ = o.x509_export_to_file(
        CertArg::Str(invalid_read.as_bytes()),
        output_file.as_bytes(),
        true,
    );
    expect(&mut o, "x509_export_to_file open", &["10000080"]);
    let _ = o.x509_export_to_file(CertArg::Str(&csr), output_file.as_bytes(), true);
    expect(&mut o, "x509_export_to_file pem", &["0480006C"]);
    let _ = o.x509_export_to_file(CertArg::Str(&crt), invalid_write.as_bytes(), true);
    expect(&mut o, "x509_export_to_file write", &["10080002"]);
    let _ = o.x509_checkpurpose(CertArg::Str(&crt), 1000, &[], None);
    expect(&mut o, "x509_checkpurpose purpose", &["05800079"]);

    let _ = o.csr_get_subject(CsrArg::Str(invalid_read.as_bytes()), true);
    expect(&mut o, "csr_get_subject open", &["10000080"]);
    let _ = o.csr_get_subject(CsrArg::Str(&crt), true);
    expect(&mut o, "csr_get_subject pem", &["0480006C"]);
}

/// The default config file is chosen on first use (bug #79881: creating
/// the state must not allocate it) and stays the same afterwards.
#[test]
fn default_config_file_is_stable() {
    let o = OpenSsl::new();
    let conf = o.default_config_file();
    let from_env =
        std::env::var_os("OPENSSL_CONF").is_some() || std::env::var_os("SSLEAY_CONF").is_some();
    assert!(from_env || conf.ends_with(b"/openssl.cnf"), "{conf:?}");
    assert_eq!(o.default_config_file(), conf);
}
