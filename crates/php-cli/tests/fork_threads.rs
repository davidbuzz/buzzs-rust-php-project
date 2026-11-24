// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The process that forks the workers is single-threaded, as in C: php-fpm's
//! master (`fpm_run()`) and php-cgi's parent with `PHP_FCGI_CHILDREN` run
//! the VM on the main thread (`main_stack`), so `fork()` copies one thread
//! and the children start from a consistent state. Only when `RLIMIT_STACK`
//! cannot be raised to the VM's stack does the VM fall back to a second
//! thread; the tests then report themselves skipped.

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
