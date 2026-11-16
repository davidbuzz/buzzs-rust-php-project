// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Process-directed signal scenarios (`ext/pcntl/tests/002.phpt` and
//! friends).  They need a single-threaded process — any other thread with
//! the signal unblocked would receive it — so this test target runs
//! without the libtest harness, on the main thread only.  Signals are sent
//! from outside with `kill(1)` so they carry `SI_USER`.

use php_pcntl::{Pcntl, PcntlError, SignalArg, SignalHandler};
use std::os::unix::fs::MetadataExt;
use std::process::Command;

const SIGTERM: i64 = 15;
const SIGKILL: i64 = 9;
const SIGUSR1: i64 = 10;
const SIGCHLD: i64 = 17;
const SIG_BLOCK: i64 = 0;
const SIG_UNBLOCK: i64 = 1;
const SI_USER: i64 = 0;
const CLD_KILLED: i64 = 2;

fn ints(v: &[i64]) -> Vec<SignalArg<'static>> {
    v.iter().map(|&i| SignalArg::Int(i)).collect()
}

fn send(sig: &str, pid: u32) {
    let st = Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()
        .expect("run kill(1)");
    assert!(st.success());
}

fn our_uid() -> i64 {
    i64::from(std::fs::metadata("/proc/self").expect("procfs").uid())
}

fn sigwaitinfo_user_and_child() {
    let mut p: Pcntl<u8> = Pcntl::new();
    p.sigprocmask(SIG_BLOCK, &ints(&[SIGCHLD, SIGTERM]))
        .unwrap();
    let old = p.sigprocmask(SIG_UNBLOCK, &ints(&[2])).unwrap();
    assert!(old.contains(&SIGCHLD) && old.contains(&SIGTERM));

    send("-TERM", std::process::id());
    let (signo, info) = p.sigwaitinfo(&ints(&[SIGTERM])).unwrap();
    assert_eq!((signo, info.signo()), (SIGTERM, SIGTERM));
    assert_eq!(info.code(), SI_USER);

    // kill(1) was our child too: drop its (blocked, pending) SIGCHLD so it
    // cannot merge with the one we wait for.
    let _ = p.sigtimedwait(&ints(&[SIGCHLD]), 0, 1_000_000);

    p.signal(SIGCHLD, SignalHandler::Callable(1), None).unwrap();
    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn sleep");
    let pid = i64::from(child.id());
    child.kill().expect("SIGKILL the child");
    let (signo, info) = p.sigwaitinfo(&ints(&[SIGCHLD])).unwrap();
    assert_eq!(signo, SIGCHLD);
    assert_eq!(info.signo(), SIGCHLD);
    assert_eq!(info.code(), CLD_KILLED);
    assert_eq!(info.uid(), our_uid());
    assert_eq!(info.pid(), pid);
    assert_eq!(info.status(), SIGKILL);
    let entries = info.entries(SIGCHLD).unwrap();
    assert_eq!(entries.len(), 8);
    let r = p.waitpid(pid, 0, 0, false);
    assert_eq!(r.pid, pid);
    assert!(php_pcntl::wifsignaled(r.status));
    let _ = child.try_wait();
    // Nothing was queued for the handler: sigwaitinfo consumed it.
    assert!(!p.has_pending());
    p.request_shutdown();

    send("-TERM", std::process::id());
    let (signo, info) = p.sigtimedwait(&ints(&[SIGTERM]), 5, 0).unwrap();
    assert_eq!((signo, info.code()), (SIGTERM, SI_USER));
    assert_eq!(
        p.sigtimedwait(&ints(&[SIGTERM]), 0, 1_000_000).unwrap_err(),
        PcntlError::Failed
    );
    p.sigprocmask(SIG_UNBLOCK, &ints(&[SIGCHLD, SIGTERM]))
        .unwrap();
}

fn handler_receives_external_signal() {
    let mut p: Pcntl<&str> = Pcntl::new();
    p.signal(SIGUSR1, SignalHandler::Callable("handler"), None)
        .unwrap();
    send("-USR1", std::process::id());
    // kill(1) has exited, so the signal has been delivered.
    for _ in 0..1000 {
        if p.has_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let mut batch = p.begin_dispatch().expect("pending signal");
    let sig = batch.next().unwrap();
    assert_eq!(sig.signo, SIGUSR1);
    assert_eq!(sig.info.code(), SI_USER);
    assert_eq!(sig.info.uid(), our_uid());
    assert_eq!(p.handler(sig.signo), &SignalHandler::Callable("handler"));
    assert!(batch.next().is_none());
    p.end_dispatch(batch);
    assert!(!p.has_pending());
    p.request_shutdown();
}

fn main() {
    sigwaitinfo_user_and_child();
    println!("sigwaitinfo_user_and_child ... ok");
    handler_receives_external_signal();
    println!("handler_receives_external_signal ... ok");
}
