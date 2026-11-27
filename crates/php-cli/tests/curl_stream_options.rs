// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `CURLOPT_FILE`, `CURLOPT_WRITEHEADER`, `CURLOPT_INFILE` and
//! `CURLOPT_STDERR` take any stream, not only one with a descriptor:
//! php-src casts the stream to a `FILE*`, which `fopencookie()` provides
//! for a stream in memory, a user-space stream and a filtered one, without
//! asking the stream (`stream_cast()` is not called). They were refused
//! with "Cannot represent a stream of type MEMORY as a STDIO FILE*" and
//! the body went to the output. Expected output from php8.5, except the
//! wording of the three errors at the end, which is php-src master's.

use std::path::Path;
use std::process::Command;

const CLIENT: &str = r#"<?php
class W {
    public $context;
    public static $buf = '';
    public static $calls = [];
    function stream_open($path, $mode, $options, &$opened) { return true; }
    function stream_write($data) { self::$buf .= $data; return strlen($data); }
    function stream_read($n) { return ''; }
    function stream_eof() { return true; }
    function stream_tell() { return strlen(self::$buf); }
    function stream_seek($offset, $whence) { return true; }
    function stream_flush() { return true; }
    function stream_stat() { return []; }
    function stream_close() {}
    function stream_cast($as) { self::$calls[] = 'cast'; return false; }
}
stream_wrapper_register('wr', 'W');
$file = __DIR__ . '/body.txt';
file_put_contents($file, 'hello');
$streams = [
    'memory' => fn() => fopen('php://memory', 'w+'),
    'user' => fn() => fopen('wr://x', 'w+'),
    'filtered' => function () {
        $h = fopen('php://memory', 'w+');
        stream_filter_append($h, 'string.toupper', STREAM_FILTER_WRITE);
        return $h;
    },
];
foreach ($streams as $name => $open) {
    foreach (['CURLOPT_FILE', 'CURLOPT_WRITEHEADER', 'CURLOPT_STDERR'] as $opt) {
        W::$buf = '';
        $h = $open();
        $ch = curl_init("file://$file");
        curl_setopt($ch, CURLOPT_VERBOSE, true);
        if ($opt !== 'CURLOPT_STDERR') {
            // The log of the other two goes nowhere.
            curl_setopt($ch, CURLOPT_STDERR, fopen('/dev/null', 'w'));
        }
        if ($opt !== 'CURLOPT_FILE') {
            curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        }
        echo "$name $opt: ";
        var_dump(curl_setopt($ch, constant($opt), $h));
        curl_exec($ch);
        rewind($h);
        $got = $name === 'user' ? W::$buf : stream_get_contents($h);
        if ($opt === 'CURLOPT_FILE') {
            var_dump($got);
        } elseif ($opt === 'CURLOPT_WRITEHEADER') {
            var_dump(strtolower(substr($got, 0, 19)));
        } else {
            var_dump(stripos($got, 'closing connection') !== false);
        }
        unset($ch);
        fclose($h);
    }
}
var_dump(W::$calls);

// CURLOPT_INFILE: an upload read from a stream in memory.
$in = fopen('php://memory', 'w+');
fwrite($in, 'payload');
rewind($in);
$out = __DIR__ . '/uploaded.txt';
$ch = curl_init("file://$out");
var_dump(curl_setopt($ch, CURLOPT_INFILE, $in));
curl_setopt($ch, CURLOPT_UPLOAD, true);
curl_setopt($ch, CURLOPT_INFILESIZE, 7);
var_dump(curl_exec($ch), file_get_contents($out));

// A stream opened for reading is still refused for the three it is written to.
$ro = fopen('php://memory', 'r');
foreach (['CURLOPT_FILE', 'CURLOPT_WRITEHEADER', 'CURLOPT_STDERR'] as $opt) {
    try {
        curl_setopt($ch, constant($opt), $ro);
    } catch (ValueError $e) {
        echo $e->getMessage(), "\n";
    }
}
"#;

const EXPECTED: &str = "memory CURLOPT_FILE: bool(true)\n\
string(5) \"hello\"\n\
memory CURLOPT_WRITEHEADER: bool(true)\n\
string(19) \"content-length: 5\r\n\
\"\n\
memory CURLOPT_STDERR: bool(true)\n\
bool(true)\n\
user CURLOPT_FILE: bool(true)\n\
string(5) \"hello\"\n\
user CURLOPT_WRITEHEADER: bool(true)\n\
string(19) \"content-length: 5\r\n\
\"\n\
user CURLOPT_STDERR: bool(true)\n\
bool(true)\n\
filtered CURLOPT_FILE: bool(true)\n\
string(5) \"HELLO\"\n\
filtered CURLOPT_WRITEHEADER: bool(true)\n\
string(19) \"content-length: 5\r\n\
\"\n\
filtered CURLOPT_STDERR: bool(true)\n\
bool(true)\n\
array(0) {\n\
}\n\
bool(true)\n\
bool(true)\n\
string(7) \"payload\"\n\
curl_setopt(): The file handle provided for CURLOPT_FILE must be writable\n\
curl_setopt(): The file handle provided for CURLOPT_WRITEHEADER must be writable\n\
curl_setopt(): The file handle provided for CURLOPT_STDERR must be writable\n\
";

#[test]
fn streams_without_a_descriptor_are_taken() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("curl-stream-options-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("client.php"), CLIENT).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "client.php"])
        .current_dir(&root)
        .output()
        .expect("run the client");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        EXPECTED,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
