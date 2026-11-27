// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `_php_stream_copy_to_mem()` asks the stream for its size before it reads
//! all of it (`file_get_contents()`, `stream_get_contents()` without a
//! length, `file()`): a user wrapper's `stream_stat()` runs. Expected output from
//! `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn reading_a_whole_stream_asks_for_its_size() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stream_read_all_stats.php");
    std::fs::write(&path, SCRIPT).expect("write script");
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .arg("-n")
        .arg(&path)
        .output()
        .expect("spawn php");
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
}

const SCRIPT: &str = r###"<?php
class V { public $context; private $d = "hello world"; private $p = 0;
  function stream_open($p, $m, $o, &$op) { echo "open\n"; return true; }
  function stream_read($n) { echo "read($n)\n"; $r = substr($this->d, $this->p, $n); $this->p += strlen($r); return $r; }
  function stream_eof() { echo "eof\n"; return $this->p >= strlen($this->d); }
  function stream_close() { echo "close\n"; }
}
class V2 extends V { function stream_stat() { echo "stat\n"; return ['size' => 11]; } }
stream_wrapper_register('v2', 'V2');
var_dump(file_get_contents('v2://x'));
$f = fopen('v2://x', 'r'); var_dump(stream_get_contents($f)); fclose($f);
$f = fopen('v2://x', 'r'); var_dump(stream_get_contents($f, 5)); fclose($f);
var_dump(file('v2://x'));
"###;

const EXPECTED: &str = r###"open
stat
read(8192)
eof
read(8192)
eof
close
string(11) "hello world"
open
stat
read(8192)
eof
read(8192)
eof
string(11) "hello world"
close
open
eof
read(8192)
eof
string(5) "hello"
close
open
stat
read(8192)
eof
read(8192)
eof
close
array(1) {
  [0]=>
  string(11) "hello world"
}
"###;
