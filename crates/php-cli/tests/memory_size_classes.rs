// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `memory_get_usage()` moves in php-src's size classes (`Zend/zend_alloc.c`:
//! 30 small bins, 4 KiB pages, page-rounded huge blocks), not byte by byte,
//! and the memory-limit fatal reports what php-src's allocator tried to
//! allocate. Run through the `php` binary: the counting allocator is only
//! installed there. Expected output from `/usr/bin/php8.5 -n`.

use std::process::{Command, Output};

fn php(memory_limit: &str, code: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-d"])
        .arg(format!("memory_limit={memory_limit}"))
        .args(["-r", code])
        .output()
        .expect("spawn php")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn usage_steps_only_when_a_string_changes_size_class() {
    // php8.5: int(188) int(12) int(0). Our blocks are not php-src's (a
    // string's header differs), so the bin boundaries fall at other lengths;
    // what must hold is that usage stays put within a class (most of the
    // 199 steps) and only moves by whole classes (multiples of 8).
    let out = php(
        "1G",
        r"
$base = memory_get_usage(); $prev = -1; $same = 0; $odd = 0; $moves = 0;
for ($n = 1; $n <= 200; $n++) {
    $s = str_repeat('a', $n);
    $d = memory_get_usage() - $base;
    if ($d === $prev) { $same++; } else { $moves++; }
    if ($d % 8 !== 0) { $odd++; }
    $prev = $d;
    unset($s);
}
var_dump($same >= 185, $moves <= 15, $odd);
",
    );
    assert_eq!(stdout(&out), "bool(true)\nbool(true)\nint(0)\n");
}

#[test]
fn a_loop_whose_identifier_grows_a_character_leaves_no_trace() {
    // An event loop's "no garbage after N iterations" check: the id grows
    // from two characters to three on the way, inside one size class.
    // php8.5: string(3) "dkk" int(0).
    let out = php(
        "1G",
        r"
$id = 'a'; $m0 = 0;
for ($i = 0; $i < 3000; $i++) {
    $id = str_increment($id);
    $o = ['id' => $id, 'n' => $i];
    unset($o);
    if ($i === 100) { $m0 = memory_get_usage(); }
}
var_dump($id, memory_get_usage() - $m0);
",
    );
    assert_eq!(stdout(&out), "string(3) \"dkk\"\nint(0)\n");
}

#[test]
fn limit_errors_report_php_src_s_tried_sizes() {
    // A huge block: its size rounded up to pages.
    let out = php("4M", r#"$s = str_repeat("a", 5 * 1024 * 1024);"#);
    assert!(
        stdout(&out).contains(
            "Fatal error: Allowed memory size of 4194304 bytes exhausted \
             (tried to allocate 5246976 bytes) in Command line code on line 1\n"
        ),
        "{}",
        stdout(&out)
    );
    // A reservation before reading: the zend_string, rounded up to pages.
    let out = php(
        "8M",
        r#"$fp = fopen("php://memory", "w+"); fwrite($fp, "abc"); rewind($fp); fread($fp, 100 << 20);"#,
    );
    assert!(
        stdout(&out).contains(
            "Fatal error: Allowed memory size of 8388608 bytes exhausted \
             (tried to allocate 104861696 bytes) in Command line code on line 1\n"
        ),
        "{}",
        stdout(&out)
    );
    // Small blocks: the 2 MiB chunk the next one would need.
    let out = php(
        "4M",
        r#"$a = []; for ($i = 0; ; $i++) { $a[] = str_repeat("x", 100); }"#,
    );
    assert!(
        stdout(&out).contains(
            "Fatal error: Allowed memory size of 4194304 bytes exhausted \
             (tried to allocate 2097152 bytes) in Command line code on line 1\n"
        ),
        "{}",
        stdout(&out)
    );
}
