// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `json_encode()`/`json_decode()` under a low `memory_limit` end with
//! php-src's "Allowed memory size ... exhausted" fatal, never with the hard
//! limit's kill, and what fits under the limit in php-src fits here too.
//! Run through the `php` binary: the counting allocator is only installed
//! there. Expected output from `/usr/bin/php8.5 -n -d memory_limit=16M -d
//! fatal_error_backtraces=0`.
//!
//! Where the "tried to allocate" figure depends on how usage is counted
//! (php-src limits its real usage, chunks included), only the message is
//! checked; the figures of single reservations are php-src's own.

use std::process::{Command, Output};

fn php(code: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=16M",
            "-d",
            "fatal_error_backtraces=0",
            "-r",
            code,
        ])
        .output()
        .expect("spawn php")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const FATAL: &str =
    "\nFatal error: Allowed memory size of 16777216 bytes exhausted (tried to allocate ";

/// The script ended with the memory-limit fatal (exit status 255) before
/// `not reached`; returns the "tried to allocate" figure.
fn tried(out: &Output) -> u64 {
    let s = stdout(out);
    assert_eq!(out.status.code(), Some(255), "{s}");
    assert!(!s.contains("not reached"), "{s}");
    let rest = s
        .strip_prefix(FATAL)
        .unwrap_or_else(|| panic!("not the memory-limit fatal: {s:?}"));
    let (n, tail) = rest.split_once(' ').expect("figure");
    assert_eq!(tail, "bytes) in Command line code on line 1\n");
    n.parse().expect("number")
}

/// json_decode_oom_utf8_substitute.phpt: 10 MiB of invalid UTF-8 with
/// JSON_INVALID_UTF8_SUBSTITUTE becomes a 30 MiB string. It used to grow by
/// doubling past the 32M hard limit and be killed (signal 9).
#[test]
fn substituted_utf8_beyond_the_limit_is_the_fatal() {
    let out = php(
        r#"$n = 10 << 20; $j = str_repeat("\x80", $n); $j[0] = '"'; $j[$n - 1] = '"'; json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); echo "not reached";"#,
    );
    // php8.5: 31461376 (the page-rounded zend_string); an accounting of
    // exact bytes reports the zend_string itself, 31457304.
    assert!(matches!(tried(&out), 31_461_376 | 31_457_304));
}

/// What php-src holds under the limit is held here: a 9 MB result of
/// 3 MB of invalid UTF-8 (php8.5: int(8999994)), built in one exactly sized
/// allocation instead of a doubled buffer.
#[test]
fn substituted_utf8_within_the_limit_decodes() {
    let out = php(
        r#"$j = str_repeat("\x80", 3000000); $j[0] = $j[2999999] = '"'; $r = json_decode($j, false, 512, JSON_INVALID_UTF8_SUBSTITUTE); var_dump(strlen($r));"#,
    );
    assert_eq!(stdout(&out), "int(8999994)\n");
}

#[test]
fn large_strings_are_reserved_before_they_are_copied() {
    // The encoder's pre-allocation of the string and its quotes; php8.5
    // reports the page-rounded smart_str block: 9441280.
    let out = php(r#"json_encode(str_repeat("a", 9 << 20)); echo "not reached";"#);
    assert_eq!(tried(&out), 9_441_280);
    // A decoded string (php8.5: 7344128, the page-rounded zend_string).
    let out = php(r#"json_decode('"' . str_repeat("a", 7 << 20) . '"'); echo "not reached";"#);
    assert!(matches!(tried(&out), 7_344_128 | 7_340_064));
}

#[test]
fn growing_output_ends_with_the_fatal() {
    // json_encode_oom_escape.phpt and json_encode_oom_partial_output.phpt
    // at 16M (php8.5: tried 10489856 and 6295552 bytes).
    for code in [
        r#"json_encode(str_repeat("\x01", 2 << 20)); echo "not reached";"#,
        r#"json_encode(array_fill(0, 200, str_repeat("\x01", 20000)), JSON_PARTIAL_OUTPUT_ON_ERROR); echo "not reached";"#,
        r#"json_encode(array_fill(0, 400000, [1, 2, 3]), JSON_PRETTY_PRINT); echo "not reached";"#,
    ] {
        let n = tried(&php(code));
        // The figure is a whole smart_str block, as in php-src.
        assert_eq!(n % 4096, 0, "{code}: {n}");
    }
}

/// `smart_str_extract` trims the result to its length, so the room the
/// output grew into is not kept with it (php8.5: int(6291458) int(6295576)).
#[test]
fn encoded_result_is_trimmed() {
    let out = php(
        r#"$s = str_repeat("\u{e9}", 1 << 20); $m = memory_get_usage(); $r = json_encode($s); var_dump(strlen($r), memory_get_usage() - $m < 6400000);"#,
    );
    assert_eq!(stdout(&out), "int(6291458)\nbool(true)\n");
}
