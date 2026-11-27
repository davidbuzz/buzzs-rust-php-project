// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A script can put a class of its own in place of the `file` wrapper
//! (`stream_wrapper_unregister('file')`, `stream_wrapper_register('file',
//! ...)`): every plain path then goes to it (`php_stream_locate_url_wrapper`),
//! an included file as well (`php_stream_open_for_zend_ex`), with the
//! engine's flags in the options, a read-buffer option and a stat before the
//! read. `include_once` does not open a file that was included before, and
//! without any `file` wrapper an include fails. A library loads compressed
//! data files this way. Expected output from `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn a_class_in_place_of_the_file_wrapper_opens_includes() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("include_replaced_file_wrapper.php");
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
// A class standing in for the `file` wrapper opens what is included.
$dir = sys_get_temp_dir() . '/app-include-' . getmypid();
@mkdir($dir);
file_put_contents("$dir/plain.php", "<?php\nreturn ['plain' => true];\n");
file_put_contents("compress.zlib://$dir/data.php.gz", "<?php\nreturn ['compressed' => true, 'n' => 42];\n");
set_include_path($dir);

class W {
    public $context;
    private $handle;
    private string $path;
    public function stream_open(string $path, string $mode, int $options, ?string &$opened): bool {
        echo 'stream_open(', basename($path), ", $mode, ", $options & ~STREAM_REPORT_ERRORS, ")\n";
        stream_wrapper_restore('file');
        $this->path = $path;
        return false !== $this->handle = @fopen(str_ends_with($path, '.gz') ? 'compress.zlib://' . $path : $path, $mode);
    }
    public function stream_read(int $count): string|false { return fread($this->handle, $count); }
    public function stream_eof(): bool { return feof($this->handle); }
    public function stream_set_option(int $option, int $arg1, int $arg2): bool { echo "stream_set_option($option)\n"; return false; }
    public function stream_stat(): array|false { echo "stream_stat\n"; return stat($this->path); }
    public function stream_close(): void { echo "stream_close\n"; }
}
function replace(): void { stream_wrapper_unregister('file'); stream_wrapper_register('file', W::class); }

replace(); var_dump(require "$dir/data.php.gz");
replace(); var_dump(include "$dir/plain.php");
replace(); var_dump(include_once "$dir/plain.php"); stream_wrapper_restore('file');
replace(); var_dump(include 'plain.php');
replace(); var_dump(@include "$dir/missing.php");
replace(); var_dump(strlen(file_get_contents("$dir/plain.php")));
replace(); $f = fopen("$dir/plain.php", 'r'); var_dump(fgets($f)); fclose($f);
// With no `file` wrapper at all an include fails.
stream_wrapper_unregister('file'); var_dump(@include "$dir/plain.php"); stream_wrapper_restore('file');
unlink("$dir/plain.php"); unlink("$dir/data.php.gz"); rmdir($dir);
"###;

const EXPECTED: &str = r###"stream_open(data.php.gz, rb, 82048)
stream_set_option(2)
stream_stat
stream_close
array(2) {
  ["compressed"]=>
  bool(true)
  ["n"]=>
  int(42)
}
stream_open(plain.php, rb, 82048)
stream_set_option(2)
stream_stat
stream_close
array(1) {
  ["plain"]=>
  bool(true)
}
bool(true)
stream_open(plain.php, rb, 82048)
stream_set_option(2)
stream_stat
stream_close
array(1) {
  ["plain"]=>
  bool(true)
}
stream_open(missing.php, rb, 65665)
bool(false)
stream_open(plain.php, rb, 0)
stream_stat
stream_close
int(32)
stream_open(plain.php, r, 0)
string(6) "<?php
"
stream_close
bool(false)
"###;
