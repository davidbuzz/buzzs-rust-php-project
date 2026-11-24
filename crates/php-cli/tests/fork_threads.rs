// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The process that forks the workers is single-threaded, as in C: php-fpm's
//! master (`fpm_run()`) and php-cgi's parent with `PHP_FCGI_CHILDREN` run
//! the VM on the main thread (`main_stack`), so `fork()` copies one thread
//! and the children start from a consistent state. Only when `RLIMIT_STACK`
//! cannot be raised to the VM's stack does the VM fall back to a second
//! thread; the tests then report themselves skipped.
//!
//! The process that forks also makes the workers' VM before it forks
//! (module startup once, as `fpm_run()` follows the master's startup in C):
//! an idle worker shares it copy-on-write instead of making its own.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PHP_FPM: &str = env!("CARGO_BIN_EXE_php-fpm");
const PHP_CGI: &str = env!("CARGO_BIN_EXE_php-cgi");

/// The VM's stack (`main_stack::VM_STACK`).
const VM_STACK: u64 = 256 << 20;

/// A scratch directory under PHP's temporary directory, removed on drop.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> ScratchDir {
        let base = PathBuf::from(String::from_utf8(php_streams::temp::temp_dir()).unwrap());
        let p = base.join(format!("phprust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        ScratchDir(p)
    }
}

impl Drop for ScratchDir {
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

/// Whether this process may raise `RLIMIT_STACK` to the VM's stack (the
/// hard limit from `/proc/self/limits`, inherited by the servers).
fn stack_can_grow() -> bool {
    let limits = std::fs::read_to_string("/proc/self/limits").unwrap();
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max stack size"))
        .unwrap();
    let hard = line.split_whitespace().nth(4).unwrap();
    hard == "unlimited" || hard.parse::<u64>().is_ok_and(|h| h >= VM_STACK)
}

fn wait_for_socket(child: &mut Child, socket: &Path) {
    let start = Instant::now();
    while UnixStream::connect(socket).is_err() {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("the server ended early: {status}");
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no socket {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The number of threads of `pid`.
fn threads(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .count()
}

/// The children of `pid` (forked by any of its threads), once `n` of
/// them are running.
fn children(pid: u32, n: usize) -> Vec<u32> {
    let start = Instant::now();
    loop {
        let mut kids: Vec<u32> = Vec::new();
        for task in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
            let list =
                std::fs::read_to_string(task.unwrap().path().join("children")).unwrap_or_default();
            kids.extend(list.split_whitespace().map(|c| c.parse::<u32>().unwrap()));
        }
        if kids.len() >= n {
            return kids;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{pid} has {} children, want {n}",
            kids.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn quit(proc: &mut Proc) {
    let pid = libc::pid_t::try_from(proc.0.id()).unwrap();
    // SAFETY: kill(2) on the server we started.
    unsafe { libc::kill(pid, libc::SIGQUIT) };
    let _ = proc.0.wait();
}

#[test]
fn php_fpm_master_forks_its_workers_single_threaded() {
    if !stack_can_grow() {
        eprintln!("SKIPPED: RLIMIT_STACK's hard limit is below the VM's stack");
        return;
    }
    let dir = ScratchDir::new("fpm-threads");
    let socket = dir.0.join("fpm.sock");
    let conf = dir.0.join("php-fpm.conf");
    std::fs::write(
        &conf,
        format!(
            "[global]\nerror_log = {}/fpm.log\n[t]\nlisten = {}\npm = static\npm.max_children = 2\n",
            dir.0.display(),
            socket.display()
        ),
    )
    .unwrap();
    let mut fpm = Proc(
        Command::new(PHP_FPM)
            // -R: the tests may run as root (a container).
            .args(["-n", "-R", "-F", "-y"])
            .arg(&conf)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_socket(&mut fpm.0, &socket);
    let master = fpm.0.id();
    let workers = children(master, 2);
    assert_eq!(threads(master), 1, "php-fpm's master");
    for w in &workers {
        assert_eq!(threads(*w), 1, "php-fpm worker {w}");
    }
    quit(&mut fpm);
}

#[test]
fn php_cgi_parent_forks_its_children_single_threaded() {
    if !stack_can_grow() {
        eprintln!("SKIPPED: RLIMIT_STACK's hard limit is below the VM's stack");
        return;
    }
    let dir = ScratchDir::new("cgi-threads");
    let socket = dir.0.join("cgi.sock");
    let mut cgi = Proc(
        Command::new(PHP_CGI)
            .arg("-n")
            .arg("-b")
            .arg(&socket)
            .env("PHP_FCGI_CHILDREN", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_socket(&mut cgi.0, &socket);
    let parent = cgi.0.id();
    let kids = children(parent, 2);
    assert_eq!(threads(parent), 1, "php-cgi's parent");
    for k in &kids {
        assert_eq!(threads(*k), 1, "php-cgi child {k}");
    }
    quit(&mut cgi);
}

/// A field of `/proc/<pid>/smaps_rollup` in kB (`Private_Dirty`,
/// `Shared_Dirty`).
fn rollup_kb(pid: u32, field: &str) -> u64 {
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap();
    rollup
        .lines()
        .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

/// Checks that the idle worker `pid` shares the VM its parent made: it has
/// written little itself, and shares the parent's written pages (the VM's
/// heap) copy-on-write.
fn assert_shares_parents_vm(what: &str, pid: u32) {
    let private = rollup_kb(pid, "Private_Dirty");
    let shared = rollup_kb(pid, "Shared_Dirty");
    assert!(
        private < IDLE_WORKER_KB,
        "{what} {pid} wrote {private} kB before its first request"
    );
    assert!(
        shared >= SHARED_VM_KB,
        "{what} {pid} shares only {shared} kB written by its parent (no VM made before the fork)"
    );
}

/// What an idle worker may have written: far below one VM (about 14 MB
/// when each worker made its own).
const IDLE_WORKER_KB: u64 = 4 << 10;

/// What a worker shares written by its parent at least: most of one VM.
const SHARED_VM_KB: u64 = 8 << 10;

#[test]
fn php_fpm_workers_share_the_masters_vm() {
    let dir = ScratchDir::new("fpm-shared");
    let socket = dir.0.join("fpm.sock");
    let conf = dir.0.join("php-fpm.conf");
    std::fs::write(
        &conf,
        format!(
            "[global]\nerror_log = {}/fpm.log\n[t]\nlisten = {}\npm = static\npm.max_children = 2\nphp_admin_value[precision] = 7\n",
            dir.0.display(),
            socket.display()
        ),
    )
    .unwrap();
    let mut fpm = Proc(
        Command::new(PHP_FPM)
            // -R: the tests may run as root (a container).
            .args(["-n", "-R", "-F", "-y"])
            .arg(&conf)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_socket(&mut fpm.0, &socket);
    for w in children(fpm.0.id(), 2) {
        assert_shares_parents_vm("php-fpm worker", w);
    }
    quit(&mut fpm);
}

/// One `FastCGI` request for `script` over `socket` (responder, no body);
/// returns the standard output records' content.
fn fastcgi_get(socket: &Path, script: &Path) -> Vec<u8> {
    use std::io::{Read, Write};
    fn record(kind: u8, content: &[u8]) -> Vec<u8> {
        let len = u16::try_from(content.len()).unwrap().to_be_bytes();
        let mut r = vec![1, kind, 0, 1, len[0], len[1], 0, 0];
        r.extend_from_slice(content);
        r
    }
    let mut params = Vec::new();
    let file = script.to_str().unwrap();
    for (k, v) in [
        ("SCRIPT_FILENAME", file),
        ("REQUEST_METHOD", "GET"),
        ("SERVER_PROTOCOL", "HTTP/1.1"),
        ("GATEWAY_INTERFACE", "CGI/1.1"),
    ] {
        params.push(u8::try_from(k.len()).unwrap());
        params.push(u8::try_from(v.len()).unwrap());
        params.extend_from_slice(k.as_bytes());
        params.extend_from_slice(v.as_bytes());
    }
    let mut req = record(1, &[0, 1, 0, 0, 0, 0, 0, 0]);
    req.extend(record(4, &params));
    req.extend(record(4, &[]));
    req.extend(record(5, &[]));
    let mut s = UnixStream::connect(socket).unwrap();
    s.write_all(&req).unwrap();
    let mut out = Vec::new();
    loop {
        let mut h = [0u8; 8];
        if s.read_exact(&mut h).is_err() {
            break;
        }
        let len = usize::from(u16::from_be_bytes([h[4], h[5]]));
        let mut c = vec![0u8; len + usize::from(h[6])];
        s.read_exact(&mut c).unwrap();
        match h[1] {
            6 => out.extend_from_slice(&c[..len]),
            3 => break,
            _ => {}
        }
    }
    out
}

/// php-cgi's children make no VM before their first request either way;
/// after it, the child that served it has written less than it shares
/// with its parent when the parent made the VM (without: about 17 MB
/// private, 5.5 MB shared in a release build).
#[test]
fn php_cgi_children_share_the_parents_vm() {
    let dir = ScratchDir::new("cgi-shared");
    let socket = dir.0.join("cgi.sock");
    let mut cgi = Proc(
        Command::new(PHP_CGI)
            .arg("-n")
            .arg("-b")
            .arg(&socket)
            .env("PHP_FCGI_CHILDREN", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_socket(&mut cgi.0, &socket);
    let kids = children(cgi.0.id(), 2);
    let script = dir.0.join("pid.php");
    std::fs::write(&script, "<?php echo getmypid();").unwrap();
    let out = String::from_utf8(fastcgi_get(&socket, &script)).unwrap();
    let served: u32 = out.rsplit('\n').next().unwrap().trim().parse().unwrap();
    assert!(kids.contains(&served), "{out}");
    let private = rollup_kb(served, "Private_Dirty");
    let shared = rollup_kb(served, "Shared_Dirty");
    assert!(
        private < shared,
        "php-cgi child {served} after its first request: {private} kB private, {shared} kB shared with its parent"
    );
    quit(&mut cgi);
}
