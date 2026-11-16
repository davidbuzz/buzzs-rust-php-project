// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn lint(source: &[u8], settings: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_php"));
    command.args(["-n", "-d", "memory_limit=1G", "-d", "log_errors=0", "-l"]);
    for &(name, value) in settings {
        command.arg("-d").arg(format!("{name}={value}"));
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start CLI lint");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source)
        .expect("write source");
    child.wait_with_output().expect("read lint result")
}

#[test]
fn lint_honors_multibyte_and_internal_encoding() {
    let source = b"<?php declare(encoding='SJIS'); echo \"\x95\x5C\";";
    let enabled = lint(
        source,
        &[("zend.multibyte", "1"), ("internal_encoding", "SJIS")],
    );
    assert!(enabled.status.success(), "{enabled:?}");
    assert!(String::from_utf8_lossy(&enabled.stdout).contains("No syntax errors detected"));
    let disabled = lint(source, &[("zend.multibyte", "0")]);
    assert_eq!(disabled.status.code(), Some(255));
}

#[test]
fn lint_honors_wide_input_and_script_encoding() {
    let source: Vec<u8> = "<?php echo 'é';"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let result = lint(
        &source,
        &[
            ("zend.multibyte", "1"),
            ("zend.detect_unicode", "0"),
            ("zend.script_encoding", "UTF-16LE"),
        ],
    );
    assert!(result.status.success(), "{result:?}");
}

#[test]
fn lint_reports_encoding_warning_with_configured_precision() {
    let result = lint(
        b"<?php declare(encoding=1.2345);",
        &[("zend.multibyte", "1"), ("precision", "3")],
    );
    assert!(result.status.success(), "{result:?}");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Unsupported encoding [1.23]"), "{stdout}");
    assert!(stdout.contains("No syntax errors detected"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn direct_code_uses_eval_internal_encoding_not_script_encoding() {
    use std::os::unix::ffi::OsStrExt;

    let result = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "zend.multibyte=1",
            "-d",
            "internal_encoding=SJIS",
            "-d",
            "zend.script_encoding=UTF-8",
            "-r",
        ])
        .arg(std::ffi::OsStr::from_bytes(b"echo \"\x95\x5C\";"))
        .output()
        .expect("run direct CLI code");
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, b"\x95\x5C");
}
