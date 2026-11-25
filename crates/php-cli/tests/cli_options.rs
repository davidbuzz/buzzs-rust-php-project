// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Command-line SAPI behaviour found by the differential harness
//! (tests/differential/clidiff.py); expected values from php8.5.

use std::path::PathBuf;
use std::process::{Command, Output};

fn php_in(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_php"))
        .current_dir(dir)
        .args(["-n", "-d", "memory_limit=1G"])
        .args(args)
        .output()
        .expect("spawn php")
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-cli-options-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn missing_auto_prepend_file_is_a_fatal_error() {
    let d = temp_dir("prepend");
    std::fs::write(d.join("a.php"), "<?php echo 1;").unwrap();
    let out = php_in(
        &d,
        &[
            "-d",
            "include_path=.",
            "-d",
            "auto_prepend_file=nope.php",
            "a.php",
        ],
    );
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\nWarning: Unknown: Failed to open stream: No such file or directory in Unknown on line 0\n\
         \nFatal error: Failed opening required 'nope.php' (include_path='.') in Unknown on line 0\n\
         Stack trace:\n#0 {main}\n"
    );
    // A missing auto_append_file fails the same way, after the script.
    let out = php_in(
        &d,
        &[
            "-d",
            "include_path=.",
            "-d",
            "auto_append_file=nope.php",
            "a.php",
        ],
    );
    assert_eq!(out.status.code(), Some(255));
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .starts_with("1\nWarning: Unknown: Failed to open stream"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn refused_session_ini_restore_keeps_the_value() {
    let d = temp_dir("session-restore");
    let out = php_in(
        &d,
        &[
            "-r",
            "ini_set('session.name', 'abc'); echo \"x\\n\"; ini_restore('session.name'); \
             var_dump(ini_get('session.name'), ini_get_all('session')['session.name']['global_value']);",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "x\n\nWarning: ini_restore(): Session ini settings cannot be changed after headers \
         have already been sent (sent from Command line code on line 1) in Command line code \
         on line 1\nstring(3) \"abc\"\nstring(9) \"PHPSESSID\"\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ini_restore_cannot_unset_open_basedir() {
    // OnUpdateBaseDir refuses an empty value at runtime, and a refused
    // restore leaves the entry modified (zend_restore_ini_entry_cb).
    let d = temp_dir("basedir");
    let out = php_in(
        &d,
        &[
            "-r",
            "var_dump(ini_set('open_basedir', '/tmp')); ini_restore('open_basedir'); \
             var_dump(ini_get('open_basedir'), ini_get_all()['open_basedir']['global_value']);",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "string(0) \"\"\nstring(4) \"/tmp\"\nstring(4) \"/tmp\"\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn rejected_startup_timezone_keeps_the_default() {
    let d = temp_dir("timezone");
    for value in ["Nope/Zone", ""] {
        let out = php_in(
            &d,
            &[
                "-d",
                &format!("date.timezone={value}"),
                "-r",
                "var_dump(ini_get('date.timezone'), date_default_timezone_get(), \
                 ini_get_all('date')['date.timezone']['global_value'], get_cfg_var('date.timezone'));",
            ],
        );
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!(
                "\nWarning: PHP Startup: Invalid date.timezone value '{value}', using 'UTC' instead \
                 in Unknown on line 0\nstring(3) \"UTC\"\nstring(3) \"UTC\"\nstring(3) \"UTC\"\n\
                 string({}) \"{value}\"\n",
                value.len()
            )
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ini_restore_keeps_entries_without_on_modify() {
    // zend_restore_ini_entry_cb() fails at runtime for entries C declares
    // with a NULL on_modify (php8.5).
    let d = temp_dir("restore-null");
    let out = php_in(
        &d,
        &[
            "-r",
            "foreach (['SMTP', 'highlight.string', 'date.default_latitude', \
             'precision', 'gd.jpeg_ignore_warning'] as $n) { ini_set($n, '7'); \
             ini_restore($n); echo $n, '=', ini_get($n), \"\\n\"; }",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "SMTP=7\nhighlight.string=7\ndate.default_latitude=7\nprecision=14\n\
         gd.jpeg_ignore_warning=7\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ini_set_report_memleaks_off_is_deprecated() {
    let d = temp_dir("memleaks");
    let out = php_in(
        &d,
        &[
            "-r",
            "ini_set('report_memleaks', '1'); var_dump(ini_set('report_memleaks', '0'));",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\nDeprecated: ini_set(): Directive 'report_memleaks' is deprecated in Command line \
         code on line 1\nstring(1) \"1\"\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn rejected_startup_session_values_keep_the_default() {
    let d = temp_dir("session-startup");
    let out = php_in(
        &d,
        &[
            "-d",
            "session.gc_divisor=-1",
            "-r",
            "$a = ini_get_all('session')['session.gc_divisor']; \
             var_dump(ini_get('session.gc_divisor'), $a['global_value'], $a['local_value']);",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\nWarning: PHP Startup: session.gc_divisor must be greater than 0 in Unknown on line 0\n\
         string(3) \"100\"\nstring(3) \"100\"\nstring(3) \"100\"\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn script_encoding_without_multibyte_keeps_the_default() {
    let d = temp_dir("script-encoding");
    let read = "var_dump(ini_get('zend.script_encoding'), \
                ini_get_all()['zend.script_encoding']['global_value']);";
    let out = php_in(&d, &["-d", "zend.script_encoding=UTF-8", "-r", read]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "string(0) \"\"\nNULL\n"
    );
    let out = php_in(
        &d,
        &[
            "-d",
            "zend.multibyte=1",
            "-d",
            "zend.script_encoding=UTF-8",
            "-r",
            read,
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "string(5) \"UTF-8\"\nstring(5) \"UTF-8\"\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn invalid_startup_mail_cr_lf_mode_is_fatal_but_runs() {
    let d = temp_dir("crlf");
    let out = php_in(
        &d,
        &[
            "-d",
            "mail.cr_lf_mode=abc",
            "-r",
            "var_dump(ini_get('mail.cr_lf_mode'));",
        ],
    );
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\nFatal error: PHP Startup: Invalid value \"abc\" for mail.cr_lf_mode. Must be one of: \
         \"crlf\", \"lf\", \"mixed\", \"os\" in Unknown on line 0\nstring(4) \"crlf\"\n"
    );
    let out = php_in(&d, &["-d", "mail.cr_lf_mode=", "-r", "echo 1;"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"1");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unknown_openssl_libctx_is_fatal_at_startup() {
    let d = temp_dir("libctx");
    let out = php_in(
        &d,
        &[
            "-d",
            "openssl.libctx=abc",
            "-r",
            "var_dump(ini_get('openssl.libctx'));",
        ],
    );
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\nFatal error: PHP Startup: OpenSSL libctx \"abc\" cannot be found in Unknown on line 0\n\
         Stack trace:\n#0 {main}\nstring(6) \"custom\"\n"
    );
    let out = php_in(
        &d,
        &[
            "-d",
            "openssl.libctx=default",
            "-r",
            "echo ini_get('openssl.libctx');",
        ],
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"default");
    let _ = std::fs::remove_dir_all(&d);
}
