// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! curl's verbose log, sent to a PHP stream with `CURLOPT_STDERR`, can be
//! read while the transfer runs: php-src hands libcurl the stream's own
//! `FILE`, which the stream's next seek flushes. An HTTP client that
//! collects the log from a progress or write callback found it empty
//! until the transfer had ended. Expected output from php8.5.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn the_verbose_log_is_readable_during_the_transfer() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("curl-verbose-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("ok.php"), "<?php echo 'ok';").unwrap();
    std::fs::write(
        root.join("client.php"),
        r"<?php
$buf = fopen('php://temp', 'w+');
$ch = curl_init('http://127.0.0.1:' . $argv[1] . '/ok.php');
curl_setopt($ch, CURLOPT_STDERR, $buf);
curl_setopt($ch, CURLOPT_VERBOSE, true);
$seen = null;
curl_setopt($ch, CURLOPT_WRITEFUNCTION, function ($ch, $data) use ($buf, &$seen) {
    rewind($buf);
    $seen = str_contains(stream_get_contents($buf), '> GET /ok.php HTTP/1.1');
    return strlen($data);
});
curl_exec($ch);
var_dump($seen);
rewind($buf);
ftruncate($buf, 0);
curl_exec($ch);
rewind($buf);
var_dump(substr_count(stream_get_contents($buf), '> GET /ok.php HTTP/1.1'));
",
    )
    .unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-t"])
        .arg(&root)
        .args(["-S", "127.0.0.1:0"])
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn php -S");
    let mut lines = BufReader::new(server.stderr.take().unwrap()).lines();
    let banner = lines.next().unwrap().unwrap();
    let port = banner
        .rsplit(':')
        .next()
        .and_then(|s| s.strip_suffix(") started"))
        .unwrap_or_else(|| panic!("bad banner {banner}"))
        .to_string();
    std::thread::spawn(move || for _ in lines {});
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "client.php", &port])
        .current_dir(&root)
        .output()
        .expect("run the client");
    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bool(true)\nint(1)\n");
}
