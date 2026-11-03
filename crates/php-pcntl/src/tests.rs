//! Tests.  Anything that forks, waits for arbitrary children, changes
//! signal dispositions/masks or process attributes runs in an isolated
//! subprocess (the test binary re-executed with a single `iso_*` test), so
//! the multi-threaded cargo harness is never disturbed.  Deterministic
//! behaviour is compared with `/usr/bin/php` where available.

#![allow(clippy::format_collect)]

use super::*;
use std::process::Command;

const ORACLE: &str = "/usr/bin/php";
const ISOLATED: &str = "PHP_PCNTL_ISOLATED";

fn php(code: &str) -> Option<String> {
    if !std::path::Path::new(ORACLE).exists() {
        return None;
    }
    let out = Command::new(ORACLE)
        .args(["-d", "memory_limit=1G", "-r", code])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs the ignored test `name` alone in a fresh process.
fn isolated(name: &str) {
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args([
            name,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ISOLATED, "1")
        .output()
        .expect("spawn isolated test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "isolated {name} failed:\n{stdout}\n{stderr}"
    );
}

/// True when running as the isolated child; ignored tests return early
/// otherwise (e.g. under a plain `cargo test -- --ignored`).
fn in_isolation() -> bool {
    std::env::var_os(ISOLATED).is_some()
}

fn is_root() -> bool {
    sys::getuid() == 0
}

type P = Pcntl<u32>;

fn ve(m: &str) -> PcntlError {
    PcntlError::ValueError(m.into())
}

fn ints(v: &[i64]) -> Vec<SignalArg<'static>> {
    v.iter().map(|&i| SignalArg::Int(i)).collect()
}

/// Forks a child that only runs `child` (async-signal-safe work) and then
/// `_exit(code)`.
fn spawn_child(child: fn(), code: i32) -> i64 {
    let mut p = P::new();
    let pid = p.fork().unwrap();
    if pid == 0 {
        child();
        sys::exit_now(code);
    }
    pid
}

// ------------------------------------------------------------- pure tests

#[test]
fn constants_match_oracle() {
    let ours = constants();
    let Some(out) = php(
        "foreach (get_defined_constants(true)['pcntl'] as $k => $v) echo $k, '=', $v, \"\\n\";",
    ) else {
        return;
    };
    let mut seen = 0;
    for line in out.lines() {
        let (k, v) = line.split_once('=').unwrap();
        let ours_v = ours.iter().find(|c| c.0 == k).map(|c| c.1);
        assert_eq!(ours_v, Some(v.parse().unwrap()), "{k}");
        seen += 1;
    }
    assert!(seen > 100);
    // Master-only (8.4+) additions.
    for (k, v) in [
        ("WEXITED", 4),
        ("WSTOPPED", 2),
        ("WNOWAIT", 0x0100_0000),
        ("P_ALL", 0),
        ("P_PID", 1),
        ("P_PGID", 2),
        ("P_PIDFD", 3),
    ] {
        assert_eq!(ours.iter().find(|c| c.0 == k).map(|c| c.1), Some(v), "{k}");
    }
    let mut names: Vec<_> = ours.iter().map(|c| c.0).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), ours.len(), "duplicate constant");
}

#[test]
fn status_macros_match_oracle() {
    let vals: [i64; 12] = [
        0,
        0x2a00,
        0x7f,
        0x137f,
        0xffff,
        9,
        0x8b,
        0x1_0009,
        -1,
        1 << 40,
        0x0f00,
        0x7f7f,
    ];
    let ours: String = vals
        .iter()
        .map(|&s| {
            format!(
                "{}{}{}{}:{}:{}:{}\n",
                u8::from(wifexited(s)),
                u8::from(wifstopped(s)),
                u8::from(wifsignaled(s)),
                u8::from(wifcontinued(s)),
                wexitstatus(s),
                wtermsig(s),
                wstopsig(s)
            )
        })
        .collect();
    let list = vals.map(|v| v.to_string()).join(",");
    let Some(theirs) = php(&format!(
        "foreach ([{list}] as $s) echo (int)pcntl_wifexited($s),(int)pcntl_wifstopped($s),(int)pcntl_wifsignaled($s),(int)pcntl_wifcontinued($s),':',pcntl_wexitstatus($s),':',pcntl_wtermsig($s),':',pcntl_wstopsig($s),\"\\n\";"
    )) else {
        return;
    };
    assert_eq!(ours, theirs);
}

#[test]
fn strerror_matches_oracle() {
    let codes = [0i64, 1, 2, 13, 22, 133, 134, 1234, -1, 1 << 33];
    let ours: Vec<String> = codes.iter().map(|&c| strerror(c)).collect();
    let list = codes.map(|v| v.to_string()).join(",");
    let Some(out) = php(&format!(
        "foreach ([{list}] as $c) echo pcntl_strerror($c), \"\\n\";"
    )) else {
        return;
    };
    assert_eq!(ours, out.lines().collect::<Vec<_>>());
}

#[test]
fn signal_argument_errors() {
    let mut p = P::new();
    assert_eq!(num_signals(), 65);
    assert_eq!(
        p.signal(65, SignalHandler::Ignore, None),
        Err(ve("Argument #1 ($signal) must be less than 65"))
    );
    assert_eq!(
        p.signal(0, SignalHandler::Ignore, None),
        Err(ve(
            "Argument #1 ($signal) must be greater than or equal to 1"
        ))
    );
    assert_eq!(
        int_handler::<u32>(5),
        Err(ve("Argument #2 ($handler) must be either SIG_DFL or SIG_IGN when an integer value is given"))
    );
    assert_eq!(int_handler::<u32>(0), Ok(SignalHandler::Default));
    assert_eq!(int_handler::<u32>(1), Ok(SignalHandler::Ignore));
    assert_eq!(
        p.reject_handler("string"),
        PcntlError::TypeError(
            "Argument #2 ($handler) must be of type callable|int, string given".into()
        )
    );
    assert_eq!(p.last_error(), 22);
    assert_eq!(
        p.signal_get_handler(65).unwrap_err(),
        ve("Argument #1 ($signal) must be between 1 and 64")
    );
    assert_eq!(
        p.signal_get_handler(0).unwrap_err(),
        ve("Argument #1 ($signal) must be between 1 and 64")
    );
    assert_eq!(p.signal_get_handler(10).unwrap().as_int(), Some(SIG_DFL));
}

#[test]
fn signal_set_errors() {
    // pcntl_signal_functions_invalid_signals.phpt, pcntl_sigprocmask_errors.phpt,
    // pcntl_sigtimedwait_errors.phpt, pcntl_sigwaitinfo_errors.phpt
    let mut p = P::new();
    let s = [SignalArg::Invalid("string")];
    assert_eq!(
        p.sigprocmask(0, &s).unwrap_err(),
        PcntlError::TypeError(
            "Argument #2 ($signals) signals must be of type int, string given".into()
        )
    );
    for bad in [0, -1, 1024] {
        assert_eq!(
            p.sigprocmask(0, &ints(&[bad])).unwrap_err(),
            ve("Argument #2 ($signals) signals must be between 1 and 64")
        );
        assert_eq!(
            p.sigwaitinfo(&ints(&[bad])).unwrap_err(),
            ve("Argument #1 ($signals) signals must be between 1 and 64")
        );
        assert_eq!(
            p.sigtimedwait(&ints(&[bad]), 2, 0).unwrap_err(),
            ve("Argument #1 ($signals) signals must be between 1 and 64")
        );
    }
    assert_eq!(
        p.sigprocmask(0, &[]).unwrap_err(),
        ve("Argument #2 ($signals) must not be empty")
    );
    assert_eq!(
        p.sigprocmask(-1, &ints(&[15])).unwrap_err(),
        ve("Argument #1 ($mode) must be one of SIG_BLOCK, SIG_UNBLOCK, or SIG_SETMASK")
    );
    assert_eq!(
        p.sigwaitinfo(&[]).unwrap_err(),
        ve("Argument #1 ($signals) must not be empty")
    );
    assert_eq!(
        p.sigwaitinfo(&s).unwrap_err(),
        PcntlError::TypeError(
            "Argument #1 ($signals) signals must be of type int, string given".into()
        )
    );
    assert_eq!(
        p.sigtimedwait(&[], 2, 0).unwrap_err(),
        ve("Argument #1 ($signals) must not be empty")
    );
    // Element order: the first bad element decides.
    assert_eq!(
        p.sigwaitinfo(&[SignalArg::Int(0), SignalArg::Invalid("array")])
            .unwrap_err(),
        ve("Argument #1 ($signals) signals must be between 1 and 64")
    );
    let t = ints(&[15]);
    assert_eq!(
        p.sigtimedwait(&t, -1, 0).unwrap_err(),
        ve("Argument #3 ($seconds) must be greater than or equal to 0")
    );
    for ns in [-1, i64::MAX, 1_000_000_000] {
        assert_eq!(
            p.sigtimedwait(&t, i64::from(ns != 1_000_000_000), ns)
                .unwrap_err(),
            ve("Argument #4 ($nanoseconds) must be between 0 and 1e9")
        );
    }
    assert_eq!(
        p.sigtimedwait(&t, 0, 0).unwrap_err(),
        ve("pcntl_sigtimedwait(): At least one of argument #3 ($seconds) or argument #4 ($nanoseconds) must be greater than 0")
    );
}

#[test]
fn alarm_range() {
    let mut p = P::new();
    let msg = ve(&format!(
        "Argument #1 ($seconds) must be between 0 and {}",
        u32::MAX
    ));
    for v in [-1, i64::MIN, i64::MAX, i64::from(u32::MAX) + 1] {
        assert_eq!(p.alarm(v).unwrap_err(), msg);
    }
}

#[test]
fn exec_argument_errors() {
    let mut p = P::new();
    assert_eq!(
        exec_args_not_list(),
        ve("Argument #2 ($args) must be a list array")
    );
    let args = vec![b"-n".to_vec(), b"value\0null\0byte".to_vec()];
    assert_eq!(
        p.exec(b"cmd", Some(&args), None),
        ve("Argument #2 ($args) individual argument must not contain null bytes")
    );
    let env = vec![
        (b"var1".to_vec(), b"value1".to_vec()),
        (b"var2".to_vec(), b"v\0b".to_vec()),
    ];
    assert_eq!(
        p.exec(b"cmd", Some(&[b"-n".to_vec()]), Some(&env)),
        ve("Argument #3 ($env_vars) value for environment variable must not contain null bytes")
    );
    let env = vec![
        (b"var1".to_vec(), b"value1".to_vec()),
        (b"key\0null".to_vec(), b"v".to_vec()),
    ];
    assert_eq!(
        p.exec(b"cmd", Some(&[]), Some(&env)),
        ve("Argument #3 ($env_vars) name for environment variable must not contain null bytes")
    );
    assert_eq!(
        p.exec(b"c\0md", None, None),
        ve("Argument #1 ($path) must not contain any null bytes")
    );
    // A missing binary: exec fails and returns (no fork needed).
    assert_eq!(
        p.exec(b"/nonexistent/phprust-pcntl", None, None),
        PcntlError::Warning("Error has occurred: (errno 2) No such file or directory".into())
    );
    assert_eq!(p.last_error(), 2);
    let e = p.exec(b"/", None, Some(&[]));
    assert_eq!(
        e,
        PcntlError::Warning("Error has occurred: (errno 13) Permission denied".into())
    );
}

#[test]
fn cpu_affinity_argument_errors() {
    // pcntl_cpuaffinity.phpt (error half)
    let mut p = P::new();
    let max = sys::nprocessors_conf();
    assert_eq!(
        p.setcpuaffinity(None, &[]).unwrap_err(),
        ve("Argument #2 ($cpu_ids) must not be empty")
    );
    assert_eq!(
        p.setcpuaffinity(None, &[CpuArg::Str(b"def"), CpuArg::Str(b"cpuid")])
            .unwrap_err(),
        ve("Argument #2 ($cpu_ids) cpu id invalid value (def)")
    );
    assert_eq!(
        p.setcpuaffinity(None, &[CpuArg::Int(i64::MAX)])
            .unwrap_err(),
        ve(&format!(
            "Argument #2 ($cpu_ids) cpu id must be between 0 and {} ({})",
            max - 1,
            i64::MAX
        ))
    );
    assert_eq!(
        p.setcpuaffinity(None, &[CpuArg::Int(-1024), CpuArg::Int(64)])
            .unwrap_err(),
        ve(&format!(
            "Argument #2 ($cpu_ids) cpu id must be between 0 and {} (-1024)",
            max - 1
        ))
    );
    assert_eq!(
        p.setcpuaffinity(None, &[CpuArg::Int(0), CpuArg::Invalid("array")])
            .unwrap_err(),
        PcntlError::TypeError(
            "Argument #2 ($cpu_ids) value must be of type int|string, array given".into()
        )
    );
    assert_eq!(
        p.getcpuaffinity(Some(-1024)).unwrap_err(),
        ve("Argument #1 ($process_id) invalid process (-1024)")
    );
    assert_eq!(p.last_error(), i64::from(libc::ESRCH));
    let cpus = p.getcpuaffinity(None).unwrap();
    assert!(!cpus.is_empty());
    assert!(getcpu() >= 0);
}

#[test]
fn numeric_strings() {
    assert_eq!(handle_numeric(b"0"), Some(0));
    assert_eq!(handle_numeric(b"17"), Some(17));
    assert_eq!(handle_numeric(b"-3"), Some(-3));
    assert_eq!(handle_numeric(b"9223372036854775807"), Some(i64::MAX));
    for bad in [
        &b""[..],
        b"-",
        b"-0",
        b"01",
        b" 1",
        b"1 ",
        b"1.0",
        b"abc",
        b"9223372036854775808",
    ] {
        assert_eq!(
            handle_numeric(bad),
            None,
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn priority_errors() {
    let mut p = P::new();
    assert_eq!(
        p.getpriority(Some(i64::from(i32::MAX)), 0).unwrap_err(),
        PcntlError::Warning("Error 3: No process was located using the given parameters".into())
    );
    assert_eq!(p.last_error(), 3);
    assert_eq!(
        p.getpriority(None, 42).unwrap_err(),
        ve("Argument #2 ($mode) must be one of PRIO_PGRP, PRIO_USER, or PRIO_PROCESS")
    );
    assert_eq!(
        p.setpriority(0, None, 42).unwrap_err(),
        ve("Argument #3 ($mode) must be one of PRIO_PGRP, PRIO_USER, or PRIO_PROCESS")
    );
    assert_eq!(
        p.setpriority(0, Some(i64::from(i32::MAX)), 0).unwrap_err(),
        PcntlError::Warning("Error 3: No process was located using the given parameters".into())
    );
    assert!(p.getpriority(None, 0).is_ok());
}

#[test]
fn siginfo_shapes() {
    let mut raw = sys::RawSigInfo([0; 16]);
    // signo=17 errno=0 code=CLD_EXITED(1); pid=1234 uid=1000 status=42;
    // utime=7 stime=9 (LP64 layout).
    raw.0[0] = 17;
    raw.0[1] = 1;
    raw.0[2] = 1234 + 1000 * (1 << 32);
    raw.0[3] = 42;
    raw.0[4] = 7;
    raw.0[5] = 9;
    let info = SigInfo(raw);
    let e = info.entries(17).unwrap();
    let keys: Vec<_> = e.iter().map(|x| x.0).collect();
    assert_eq!(
        keys,
        ["signo", "errno", "code", "status", "utime", "stime", "pid", "uid"]
    );
    assert_eq!(e[3].1, InfoValue::Int(42));
    assert_eq!(e[4].1, InfoValue::Float(7.0));
    assert_eq!(e[6].1, InfoValue::Int(1234));
    assert_eq!(e[7].1, InfoValue::Int(1000));
    let keys = |s: i64| -> Vec<&str> { info.entries(s).unwrap().iter().map(|x| x.0).collect() };
    assert_eq!(keys(10), ["signo", "errno", "code", "pid", "uid"]);
    assert_eq!(keys(11), ["signo", "errno", "code", "addr"]);
    assert_eq!(keys(29), ["signo", "errno", "code", "band", "fd"]);
    assert_eq!(keys(15), ["signo", "errno", "code"]);
    assert_eq!(keys(40), ["signo", "errno", "code", "pid", "uid"]);
    assert_eq!(info.entries(0), None);
    let ru = ResourceUsage::default();
    assert_eq!(ru.entries().len(), 17);
    assert_eq!(ru.entries()[13].0, "ru_utime.tv_usec");
    assert_eq!(ru.get("ru_stime.tv_sec"), Some(0));
}

// ------------------------------------------------- isolated process tests

macro_rules! isolated_test {
    ($outer:ident, $inner:ident, $body:block) => {
        #[test]
        fn $outer() {
            isolated(concat!("tests::", stringify!($inner)));
        }

        #[test]
        #[ignore = "runs only in an isolated subprocess"]
        fn $inner() {
            if !in_isolation() {
                return;
            }
            $body
        }
    };
}

isolated_test!(fork_and_waitpid, iso_fork_and_waitpid, {
    let mut p = P::new();
    let pid = spawn_child(|| {}, 42);
    assert!(pid > 0);
    let r = p.waitpid(pid, 0, 0, true);
    assert_eq!(r.pid, pid);
    assert!(wifexited(r.status));
    assert_eq!(wexitstatus(r.status), 42);
    let ru = r.resource_usage.expect("rusage");
    assert!(ru.get("ru_maxrss").is_some());
    // No children left: -1, ECHILD, status untouched, empty rusage.
    let r = p.waitpid(-1, 1234, libc::WNOHANG.into(), true);
    assert_eq!((r.pid, r.status, r.resource_usage), (-1, 1234, None));
    assert_eq!(p.last_error(), i64::from(libc::ECHILD));
    // WNOHANG with a live child returns 0 and leaves $status alone.
    let pid = spawn_child(sys::pause, 0);
    let r = p.waitpid(pid, 77, libc::WNOHANG.into(), false);
    assert_eq!((r.pid, r.status), (0, 77));
    sys::kill(i32::try_from(pid).unwrap(), libc::SIGTERM).unwrap();
    let r = p.wait(0, 0, false);
    assert_eq!(r.pid, pid);
    assert!(wifsignaled(r.status));
    assert_eq!(wtermsig(r.status), i64::from(libc::SIGTERM));
});

isolated_test!(stop_continue, iso_stop_continue, {
    // pcntl_wait_rusage1.phpt / pcntl_waitpid_rusage1.phpt
    let mut p = P::new();
    let pid = spawn_child(|| sys::raise(libc::SIGSTOP), 42);
    let r = p.wait(0, libc::WUNTRACED.into(), true);
    assert_eq!(r.pid, pid);
    assert!(wifstopped(r.status));
    assert_eq!(wstopsig(r.status), i64::from(libc::SIGSTOP));
    assert!(r.resource_usage.is_some());
    sys::kill(i32::try_from(pid).unwrap(), libc::SIGCONT).unwrap();
    let r = p.waitpid(pid, 0, (libc::WCONTINUED | libc::WUNTRACED).into(), false);
    if wifcontinued(r.status) {
        let r = p.waitpid(pid, 0, 0, false);
        assert_eq!(wexitstatus(r.status), 42);
    } else {
        assert_eq!(wexitstatus(r.status), 42);
    }
});

isolated_test!(waitid_flow, iso_waitid_flow, {
    // pcntl_waitid.phpt
    let mut p = P::new();
    let pid = spawn_child(
        || {
            sys::raise(libc::SIGSTOP);
            sys::pause();
        },
        42,
    );
    assert_eq!(
        p.waitid(P_PID, Some(pid), 0, false).unwrap_err(),
        PcntlError::Failed
    );
    assert_eq!(p.last_error(), i64::from(libc::EINVAL));
    let (info, _) = p
        .waitid(P_PID, Some(pid), libc::WSTOPPED.into(), false)
        .unwrap();
    assert_eq!(info.code(), i64::from(libc::CLD_STOPPED));
    sys::kill(i32::try_from(pid).unwrap(), libc::SIGCONT).unwrap();
    let (info, _) = p
        .waitid(P_PID, Some(pid), libc::WCONTINUED.into(), false)
        .unwrap();
    assert_eq!(info.code(), i64::from(libc::CLD_CONTINUED));
    sys::kill(i32::try_from(pid).unwrap(), libc::SIGUSR1).unwrap();
    let (info, ru) = p
        .waitid(P_PID, Some(pid), libc::WEXITED.into(), true)
        .unwrap();
    assert!(ru.is_some());
    let e = info.entries(i64::from(libc::SIGCHLD)).unwrap();
    assert_eq!(e[0], ("signo", InfoValue::Int(libc::SIGCHLD.into())));
    assert_eq!(e[2], ("code", InfoValue::Int(libc::CLD_KILLED.into())));
    assert_eq!(e[3], ("status", InfoValue::Int(libc::SIGUSR1.into())));
    assert_eq!(e[6], ("pid", InfoValue::Int(pid)));
    assert_eq!(e[7], ("uid", InfoValue::Int(sys::getuid().into())));
    // WNOHANG with nothing to wait for: ECHILD.
    assert_eq!(
        p.waitid(P_ALL, None, (libc::WEXITED | libc::WNOHANG).into(), false)
            .unwrap_err(),
        PcntlError::Failed
    );
    assert_eq!(p.last_error(), i64::from(libc::ECHILD));
});

isolated_test!(signal_queue_dispatch, iso_signal_queue_dispatch, {
    // pcntl_signal.phpt / pcntl_signal_dispatch.phpt / async_signals.phpt
    let mut p = P::new();
    p.signal(libc::SIGUSR1.into(), SignalHandler::Callable(7), None)
        .unwrap();
    p.signal(
        libc::SIGUSR2.into(),
        SignalHandler::Callable(8),
        Some(false),
    )
    .unwrap();
    assert_eq!(
        p.signal_get_handler(libc::SIGUSR1.into()).unwrap(),
        &SignalHandler::Callable(7)
    );
    assert!(!p.has_pending());
    assert!(p.begin_dispatch().is_none());
    sys::raise(libc::SIGUSR1);
    sys::raise(libc::SIGUSR2);
    sys::raise(libc::SIGUSR1);
    assert!(p.has_pending() && signals_pending());
    assert!(!async_interrupt_pending());
    assert!(!p.async_signals(Some(true)));
    assert!(async_interrupt_pending());
    assert!(p.async_signals(None));
    let mut batch = p.begin_dispatch().unwrap();
    // Re-entrant dispatch is a no-op.
    assert!(p.begin_dispatch().is_none());
    let mut seen = vec![];
    while let Some(sig) = batch.next() {
        let handler = p.handler(sig.signo).clone();
        let e = sig.info.entries(sig.signo).unwrap();
        assert_eq!(e[3], ("pid", InfoValue::Int(sys::getpid().into())));
        assert_eq!(sig.info.code(), i64::from(libc::SI_TKILL));
        seen.push((sig.signo, handler));
    }
    p.end_dispatch(batch);
    assert_eq!(
        seen,
        [
            (10, SignalHandler::Callable(7)),
            (12, SignalHandler::Callable(8)),
            (10, SignalHandler::Callable(7)),
        ]
    );
    assert!(!p.has_pending());

    // A throwing handler: the rest stay queued, in order.
    sys::raise(libc::SIGUSR2);
    sys::raise(libc::SIGUSR1);
    let mut batch = p.begin_dispatch().unwrap();
    assert_eq!(batch.next().unwrap().signo, 12);
    p.end_dispatch(batch); // "threw"
    assert!(p.has_pending());
    let mut batch = p.begin_dispatch().unwrap();
    assert_eq!(batch.next().unwrap().signo, 10);
    assert!(batch.next().is_none());
    p.end_dispatch(batch);
    assert!(!p.has_pending());

    // SIG_IGN / SIG_DFL bookkeeping and request shutdown.
    p.signal(libc::SIGUSR1.into(), SignalHandler::Ignore, None)
        .unwrap();
    assert_eq!(p.signal_get_handler(10).unwrap().as_int(), Some(SIG_IGN));
    sys::raise(libc::SIGUSR1); // ignored
    assert!(!p.has_pending());
    p.request_shutdown();
    assert_eq!(p.signal_get_handler(12).unwrap().as_int(), Some(SIG_DFL));
    assert!(!async_interrupt_pending());
    // Uncatchable signals are fatal at installation (zend_signal).
    assert_eq!(
        p.signal(libc::SIGKILL.into(), SignalHandler::Ignore, None),
        Err(PcntlError::Fatal(
            "Error installing signal handler for 9".into()
        ))
    );
});

isolated_test!(queue_overflow, iso_queue_overflow, {
    let mut p = P::new();
    let rt = i64::from(sys::sigrtmin());
    p.signal(rt, SignalHandler::Callable(1), None).unwrap();
    for _ in 0..(signals::CAPACITY + 10) {
        sys::raise(sys::sigrtmin());
    }
    let mut batch = p.begin_dispatch().unwrap();
    let mut n = 0;
    while let Some(sig) = batch.next() {
        assert_eq!(sig.signo, rt);
        let keys: Vec<_> = sig.info.entries(rt).unwrap().iter().map(|e| e.0).collect();
        assert_eq!(keys, ["signo", "errno", "code", "pid", "uid"]);
        n += 1;
    }
    p.end_dispatch(batch);
    assert_eq!(n, signals::CAPACITY);
});

isolated_test!(sigprocmask_003, iso_sigprocmask_003, {
    let mut p = P::new();
    let (int, term, chld) = (
        i64::from(libc::SIGINT),
        i64::from(libc::SIGTERM),
        i64::from(libc::SIGCHLD),
    );
    let prev = p.sigprocmask(libc::SIG_SETMASK.into(), &[]).unwrap();
    let block = i64::from(libc::SIG_BLOCK);
    assert_eq!(
        p.sigprocmask(block, &ints(&[chld, term])).unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(p.sigprocmask(block, &ints(&[int])).unwrap(), [term, chld]);
    assert_eq!(
        p.sigprocmask(libc::SIG_UNBLOCK.into(), &ints(&[int]))
            .unwrap(),
        [int, term, chld]
    );
    assert_eq!(
        p.sigprocmask(libc::SIG_SETMASK.into(), &ints(&[int]))
            .unwrap(),
        [term, chld]
    );
    assert_eq!(p.sigprocmask(libc::SIG_SETMASK.into(), &[]).unwrap(), [int]);
    assert_eq!(
        p.sigprocmask(libc::SIG_SETMASK.into(), &ints(&prev))
            .unwrap(),
        Vec::<i64>::new()
    );
});

isolated_test!(sigwait_thread_directed, iso_sigwait_thread_directed, {
    // Process-directed variants (pcntl/tests/002.phpt) live in the
    // single-threaded `tests/process_signals.rs`; here only signals aimed
    // at this thread.
    let mut p = P::new();
    let term = i64::from(libc::SIGTERM);
    p.sigprocmask(libc::SIG_BLOCK.into(), &ints(&[term]))
        .unwrap();
    sys::raise(libc::SIGTERM);
    let (signo, info) = p.sigwaitinfo(&ints(&[term])).unwrap();
    assert_eq!((signo, info.signo()), (term, term));
    assert!([libc::SI_TKILL, libc::SI_USER]
        .map(i64::from)
        .contains(&info.code()));
    assert_eq!(info.pid(), i64::from(sys::getpid()));
    // Timeout: silent false, last_error untouched.
    let before = p.last_error();
    assert_eq!(
        p.sigtimedwait(&ints(&[term]), 0, 1000).unwrap_err(),
        PcntlError::Failed
    );
    assert_eq!(p.last_error(), before);
    sys::raise(libc::SIGTERM);
    let (signo, _) = p.sigtimedwait(&ints(&[term]), 1, 0).unwrap();
    assert_eq!(signo, term);
});

isolated_test!(exec_in_child, iso_exec_in_child, {
    let mut p = P::new();
    let pid = p.fork().unwrap();
    if pid == 0 {
        let args = [b"-c".to_vec(), b"exit $CODE".to_vec()];
        let env = [(b"CODE".to_vec(), b"7".to_vec())];
        let _ = p.exec(b"/bin/sh", Some(&args), Some(&env));
        sys::exit_now(99);
    }
    let r = p.waitpid(pid, 0, 0, false);
    assert_eq!(wexitstatus(r.status), 7);
});

isolated_test!(alarm_and_priority, iso_alarm_and_priority, {
    let mut p = P::new();
    assert_eq!(p.alarm(0).unwrap(), 0);
    assert_eq!(p.alarm(100).unwrap(), 0);
    let left = p.alarm(0).unwrap();
    assert!((99..=100).contains(&left));
    let nice = p.getpriority(None, 0).unwrap();
    p.setpriority(nice + 1, None, 0).unwrap();
    assert_eq!(p.getpriority(None, 0).unwrap(), nice + 1);
    if !is_root() {
        assert_eq!(
            p.setpriority(nice, None, 0).unwrap_err(),
            PcntlError::Warning(
                "Error 13: Only a super user may attempt to increase the process priority".into()
            )
        );
        assert_eq!(p.last_error(), 13);
        // Another user's process (init) cannot be reniced.
        assert_eq!(
            p.setpriority(19, Some(1), 0).unwrap_err(),
            PcntlError::Warning("Error 1: A process was located, but neither its effective nor real user ID matched the effective user ID of the caller".into())
        );
    }
});

isolated_test!(namespaces, iso_namespaces, {
    // pcntl_unshare_04.phpt, pcntl_setns_basic.phpt
    let mut p = P::new();
    assert_eq!(
        p.unshare(42).unwrap_err(),
        ve("Argument #1 ($flags) must be a combination of CLONE_* flags, or at least one flag is unsupported by the kernel")
    );
    assert_eq!(p.last_error(), i64::from(libc::EINVAL));
    if !is_root() {
        // Unprivileged user namespaces may allow it.
        if let Err(e) = p.unshare(libc::CLONE_NEWNET.into()) {
            assert_eq!(
                e,
                PcntlError::Warning("Error 1: No privilege to use these flags".into())
            );
        }
    }
    let pid = spawn_child(sys::pause, 0);
    assert_eq!(
        p.setns(Some(pid), 0).unwrap_err(),
        ve("Argument #2 ($nstype) is an invalid nstype (0)")
    );
    assert_eq!(
        p.setns(Some(-5), 0).unwrap_err(),
        ve("Argument #1 ($process_id) is not a valid process (-5)")
    );
    sys::kill(i32::try_from(pid).unwrap(), libc::SIGKILL).unwrap();
    p.waitpid(pid, 0, 0, false);
    assert_eq!(
        p.setns(Some(pid), libc::CLONE_NEWNET.into()).unwrap_err(),
        ve(&format!(
            "Argument #1 ($process_id) is not a valid process ({pid})"
        ))
    );
    if !is_root() {
        assert_eq!(
            p.setns(None, libc::CLONE_NEWNET.into()).unwrap_err(),
            PcntlError::Warning("Error 1: No required capability for this process".into())
        );
    }
});

isolated_test!(cpu_affinity_roundtrip, iso_cpu_affinity_roundtrip, {
    let mut p = P::new();
    let cpus = p.getcpuaffinity(None).unwrap();
    let first = cpus[0];
    p.setcpuaffinity(None, &[CpuArg::Int(first)]).unwrap();
    assert_eq!(p.getcpuaffinity(None).unwrap(), [first]);
    assert_eq!(getcpu(), first);
    let strs: Vec<String> = cpus.iter().map(ToString::to_string).collect();
    let args: Vec<CpuArg<'_>> = strs.iter().map(|s| CpuArg::Str(s.as_bytes())).collect();
    p.setcpuaffinity(None, &args).unwrap();
    assert_eq!(p.getcpuaffinity(None).unwrap(), cpus);
});
