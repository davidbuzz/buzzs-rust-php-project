// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `json_encode()`/`json_decode()` under a low `memory_limit` end with
//! php-src's "Allowed memory size ... exhausted" fatal, never with the hard
//! limit's kill, and what fits under the limit in php-src fits here too.
//! Run through the `php` binary: the counting allocator is only installed
//! there. Expected output from `/usr/bin/php8.5 -n -d memory_limit=16M -d
//! fatal_error_backtraces=0`; the JSON call is on line 3 of each script
//! (after the `<?php` line and the one building its input), so that a fatal
//! raised while building the input does not pass for it.
//!
//! Where the "tried to allocate" figure depends on how usage is counted
//! (php-src limits its real usage, chunks included), only the message is
//! checked; the figures of single reservations are php-src's own.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Runs `code` (a script without the `<?php` tag) from a file named after
/// `name`.
fn php(name: &str, code: &str) -> Output {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("json_oom_{name}.php"));
    std::fs::write(&path, format!("<?php\n{code}")).expect("write script");
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=16M",
            "-d",
            "fatal_error_backtraces=0",
        ])
        .arg(&path)
        .output()
        .expect("spawn php");
    let _ = std::fs::remove_file(&path);
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const FATAL: &str =
    "\nFatal error: Allowed memory size of 16777216 bytes exhausted (tried to allocate ";

/// The script ended with the memory-limit fatal (exit status 255) on line
/// 3, the JSON call, before `not reached`; returns the "tried to allocate"
/// figure.
fn tried(out: &Output) -> u64 {
    let s = stdout(out);
    assert_eq!(out.status.code(), Some(255), "{s}");
    assert!(!s.contains("not reached"), "{s}");
    let rest = s
        .strip_prefix(FATAL)
        .unwrap_or_else(|| panic!("not the memory-limit fatal: {s:?}"));
    let (n, tail) = rest.split_once(' ').expect("figure");
    assert!(tail.starts_with("bytes) in "), "{s}");
    assert!(tail.ends_with(".php on line 3\n"), "{s}");
    n.parse().expect("number")
}

/// json_decode_oom_utf8_substitute.phpt: 10 MiB of invalid UTF-8 with
/// JSON_INVALID_UTF8_SUBSTITUTE becomes a 30 MiB string. It used to grow by
/// doubling past the 32M hard limit and be killed (signal 9).
#[test]
fn substituted_utf8_beyond_the_limit_is_the_fatal() {
    let out = php(
        "subst10m",
        "$n = 10 * 1024 * 1024; $json = str_repeat(\"\\x80\", $n); $json[0] = '\"'; $json[$n - 1] = '\"';\n\
         json_decode($json, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); echo \"not reached\";",
    );
    // php8.5: 31461376 (the page-rounded zend_string); an accounting of
    // exact bytes reports the zend_string itself, 31457304.
    assert!(matches!(tried(&out), 31_461_376 | 31_457_304));

    // 4 MiB become 12 MiB: php8.5 tried to allocate 12587008 bytes.
    let out = php(
        "subst4m",
        "$j = '\"' . str_repeat(\"\\x80\", 4 << 20) . '\"';\n\
         json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); echo \"not reached\";",
    );
    assert!(matches!(tried(&out), 12_587_008 | 12_582_944));
}

/// What php-src holds under the limit is held here: a 9 MB result of
/// 3 MB of invalid UTF-8 (php8.5: int(8999994)), built in one exactly sized
/// allocation instead of a doubled buffer.
#[test]
fn substituted_utf8_within_the_limit_decodes() {
    let out = php(
        "subst3m",
        "$j = '\"' . str_repeat(\"\\x80\", 2999998) . '\"';\n\
         $r = json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); var_dump(strlen($r));",
    );
    assert_eq!(stdout(&out), "int(8999994)\n");
}

/// The encoder's pre-allocation of a string and its quotes
/// (`smart_str_falloc(buf, len + 2, 0, true)`); php8.5 reports the
/// page-rounded smart_str block: 9441280.
#[test]
fn large_string_is_reserved_before_it_is_encoded() {
    let out = php(
        "enc9m",
        "$s = str_repeat('a', 9 << 20);\njson_encode($s); echo \"not reached\";",
    );
    assert_eq!(tried(&out), 9_441_280);
}

#[test]
fn growing_output_ends_with_the_fatal() {
    // json_encode_oom_escape.phpt and json_encode_oom_partial_output.phpt
    // at 16M (php8.5: tried 10489856 and 6295552 bytes), and pretty
    // printing.
    for (name, code) in [
        (
            "esc",
            "$s = str_repeat(\"\\x01\", 2 << 20);\njson_encode($s); echo \"not reached\";",
        ),
        (
            "partial",
            "$a = array_fill(0, 200, str_repeat(\"\\x01\", 20000));\n\
             json_encode($a, JSON_PARTIAL_OUTPUT_ON_ERROR); echo \"not reached\";",
        ),
        (
            "pretty",
            "$a = array_fill(0, 400000, [1, 2, 3]);\n\
             json_encode($a, JSON_PRETTY_PRINT); echo \"not reached\";",
        ),
    ] {
        let n = tried(&php(name, code));
        // Escaping one string allocates nothing but the output, so the
        // growth that does not fit is what fails: a whole smart_str block,
        // as in php-src. (Encoding arrays makes small allocations too, and
        // one of those may be the one that crosses the limit.)
        if name == "esc" {
            assert_eq!(n % 4096, 0, "{name}: {n}");
        }
    }
}

/// `smart_str_extract` trims the result to its length, so the room the
/// output grew into is not kept with it (php8.5: int(6291458) and a
/// growth of 6295576 bytes).
#[test]
fn encoded_result_is_trimmed() {
    let out = php(
        "trim",
        "$s = str_repeat(\"\\u{e9}\", 1 << 20); $m = memory_get_usage();\n\
         $r = json_encode($s); var_dump(strlen($r), memory_get_usage() - $m < 6400000);",
    );
    assert_eq!(stdout(&out), "int(6291458)\nbool(true)\n");
}
