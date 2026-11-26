// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `STDIN`, `STDOUT` and `STDERR` are the CLI's alone
//! (`cli_register_file_handles()`, sapi/cli/php_cli.c): php-cgi and php-fpm
//! neither define them nor open a descriptor for them, and `php://fd/N` is
//! refused outside the CLI (main/streams/php_fopen_wrapper.c).
//!
//! Every VM of such a process used to open the three streams on duplicates
//! of descriptors 0 to 2 that nothing closed: three descriptors with every
//! `php-cgi -T` run, and six a pool in the FPM master, kept across each
//! reload, until `ulimit -n` (sapi/fpm/tests/bug80024 with the usual limit
//! of 1024). Expected output from `/usr/bin/php-cgi8.5 -n -q`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PHP: &str = env!("CARGO_BIN_EXE_php");
const PHP_CGI: &str = env!("CARGO_BIN_EXE_php-cgi");
const PHP_FPM: &str = env!("CARGO_BIN_EXE_php-fpm");

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("std-streams-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child process, killed on drop.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stdout_of(bin: &str, args: &[&str], script: &Path) -> String {
    let out = Command::new(bin)
        .args(args)
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// How many descriptors process `pid` has open.
fn open_descriptors(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd")).map_or(0, Iterator::count)
}

#[test]
fn php_cgi_runs_open_no_descriptors_for_the_standard_streams() {
    let dir = Scratch::new("cgi");
    let script = dir.0.join("fds.php");
    std::fs::write(
        &script,
        "<?php echo count(scandir('/proc/self/fd')) - 2, ' ';",
    )
    .expect("script");
    // php8.5: "6 6 6 6 6 6 " (the number itself is the build's; that it
    // stays is the point).
    let out = stdout_of(PHP_CGI, &["-n", "-q", "-T", "6"], &script);
    let counts: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(counts.len(), 6, "{out}");
    assert!(counts.iter().all(|c| *c == counts[0]), "{out}");
}

#[test]
fn only_the_cli_has_the_constants_and_direct_descriptor_access() {
    let dir = Scratch::new("consts");
    let script = dir.0.join("t.php");
    std::fs::write(
        &script,
        "<?php\nvar_dump(defined('STDIN'), defined('STDOUT'), defined('STDERR'));\n\
         $f = @fopen('php://fd/1', 'w'); var_dump($f === false);\n\
         $e = fopen('php://stderr', 'w'); var_dump(is_resource($e)); fclose($e);\n\
         $o = fopen('php://stdout', 'w'); var_dump(is_resource($o)); fclose($o);\n\
         echo \"after\\n\";\n",
    )
    .expect("script");
    assert_eq!(
        stdout_of(PHP_CGI, &["-n", "-q"], &script),
        "bool(false)\nbool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(true)\nafter\n"
    );
    // php8.5 (the CLI): the constants exist and php://fd/1 opens.
    assert_eq!(
        stdout_of(PHP, &["-n"], &script),
        "bool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(true)\nbool(true)\nafter\n"
    );
}

/// The master's descriptors with 24 on-demand pools: the listening
/// sockets and a handful more, the same number after each reload (the
/// re-executed master inherits the sockets and nothing else it does not
/// close).
#[test]
fn fpm_master_keeps_its_descriptors_across_reloads() {
    const POOLS: usize = 24;
    let dir = Scratch::new("fpm");
    let d = dir.0.display();
    let mut conf = format!("[global]\nerror_log = {d}/fpm.log\npid = {d}/fpm.pid\n");
    for i in 0..POOLS {
        conf.push_str(&format!(
            "[pool_{i}]\nlisten = {d}/p{i}.sock\npm = ondemand\npm.max_children = 2\n"
        ));
    }
    let conf_path = dir.0.join("fpm.conf");
    std::fs::write(&conf_path, conf).expect("conf");
    let _fpm = Proc(
        Command::new(PHP_FPM)
            .args(["-n", "-F", "-y"])
            .arg(&conf_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn php-fpm"),
    );
    let log = dir.0.join("fpm.log");
    let ready = |n: usize| {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.matches("ready to handle connections").count() >= n {
                return;
            }
            assert!(Instant::now() < until, "php-fpm not ready ({n}):\n{text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let master = || -> u32 {
        std::fs::read_to_string(dir.0.join("fpm.pid"))
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid")
    };
    ready(1);
    let at_start = open_descriptors(master());
    assert!(
        (POOLS..POOLS + 16).contains(&at_start),
        "{at_start} descriptors for {POOLS} pools"
    );
    for reload in 1..=2 {
        let status = Command::new("kill")
            .args(["-USR2", &master().to_string()])
            .status()
            .expect("kill");
        assert!(status.success());
        ready(1 + reload);
        assert_eq!(
            open_descriptors(master()),
            at_start,
            "after reload {reload}"
        );
    }
}
