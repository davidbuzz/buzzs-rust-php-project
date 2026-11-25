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
