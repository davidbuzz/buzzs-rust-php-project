// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Subprocess execution with a timeout (ref: `run-tests.php` `system_with_timeout()`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::resource::{getrlimit, setrlimit, Resource};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

/// Environment passed to a child; empty values are dropped like PHP's `proc_open()` does.
pub type Env = BTreeMap<OsString, OsString>;

/// Which standard streams are connected to pipes (`--CAPTURE_STDIO--`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    /// Feed stdin from a pipe (otherwise inherited).
    pub stdin: bool,
    /// Capture stdout.
    pub stdout: bool,
    /// Capture stderr (merged into stdout when both are captured).
    pub stderr: bool,
}

impl Capture {
    /// Capture everything, the default.
    pub const ALL: Capture = Capture {
        stdin: true,
        stdout: true,
        stderr: true,
    };
    /// Capture stdout only, stdin is an empty pipe and stderr is discarded (used for SKIPIF and
    /// CLEAN, where run-tests.php never reads the stderr pipe).
    pub const STDOUT_ONLY: Capture = Capture {
        stdin: true,
        stdout: true,
        stderr: false,
    };
}

/// A command to execute.
#[derive(Debug, Clone)]
pub struct Invocation<'a> {
    /// argv; `argv[0]` is the program.
    pub argv: &'a [OsString],
    /// Full environment (the parent environment is not inherited).
    pub env: &'a Env,
    /// Working directory.
    pub cwd: &'a Path,
    /// Data written to stdin, if any.
    pub stdin: Option<&'a [u8]>,
    /// Stream capture mode.
    pub capture: Capture,
    /// Stderr is discarded rather than inherited when not captured.
    pub discard_stderr: bool,
    /// Wall-clock limit.
    pub timeout: Duration,
}

/// The result of running a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Captured bytes, with run-tests.php's timeout / `Termsig=` annotations appended.
    pub data: Vec<u8>,
    /// Whether the process was killed for exceeding the timeout.
    pub timed_out: bool,
}

/// Most output kept per process. A test that prints more is runaway (typically an infinite
/// loop in an early-stage implementation): its process group is killed, the output is truncated
/// and marked, and the runner's memory stays bounded.
pub const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

/// Marker appended when [`OUTPUT_LIMIT`] was hit.
pub const OUTPUT_LIMIT_MARKER: &[u8] =
    b"\n ** ERROR: output exceeded the 16 MiB limit, process killed **\n";

/// Read `source` to EOF into `sink` (up to `limit` bytes; on overflow the process group `pgid`
/// is killed, the flag is set and further data is discarded), then signal `done`.
fn spawn_reader(
    mut source: impl Read + Send + 'static,
    sink: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Sender<()>,
    pgid: Option<Pid>,
    overflow: Arc<std::sync::atomic::AtomicBool>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut data = sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let room = OUTPUT_LIMIT.saturating_sub(data.len());
                    data.extend_from_slice(&buf[..n.min(room)]);
                    if n > room && !overflow.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        if let Some(pgid) = pgid {
                            let _ = killpg(pgid, Signal::SIGKILL);
                        }
                    }
                }
            }
        }
        let _ = done.send(());
    });
}

fn kill_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let pgid = Pid::from_raw(pid);
        let _ = killpg(pgid, Signal::SIGKILL);
    }
    let _ = child.kill();
}

/// Wait for `child` until `deadline`, polling.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    let mut delay = Duration::from_micros(200);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::sleep(delay.min(deadline - now));
        delay = (delay * 2).min(Duration::from_millis(20));
    }
}

/// Set this process's soft `RLIMIT_CORE` to 0 (once). Resource limits are inherited across
/// `fork`/`exec`, so no test process (or anything it spawns) writes a core dump; crashing tests
/// are common while the implementation is young and core handlers such as apport are slow. The
/// hard limit is left untouched. Done in the parent rather than in a `pre_exec` hook, so no
/// `unsafe` is needed.
pub fn disable_core_dumps() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Ok((_, hard)) = getrlimit(Resource::RLIMIT_CORE) {
            let _ = setrlimit(Resource::RLIMIT_CORE, 0, hard);
        }
    });
}

/// Runs in the child between `fork` and `exec`: if the runner dies (killed, crashed) the child
/// gets SIGKILL instead of being orphaned, and its CPU time is capped slightly above the
/// wall-clock timeout so a runaway test cannot spin forever even when nobody is left to kill it.
fn child_safety_net(parent: u32, cpu_limit_secs: u64) -> std::io::Result<()> {
    nix::sys::prctl::set_pdeathsig(Signal::SIGKILL)?;
    // The parent may have died between fork and prctl.
    if u32::try_from(nix::unistd::getppid().as_raw()).ok() != Some(parent) {
        return Err(std::io::Error::other("runner exited"));
    }
    setrlimit(
        Resource::RLIMIT_CPU,
        cpu_limit_secs,
        cpu_limit_secs.saturating_add(5),
    )?;
    setrlimit(Resource::RLIMIT_CORE, 0, 0)?;
    Ok(())
}

/// Build the command and wire up its standard streams. Returns the merged stdout+stderr pipe
/// reader when both are captured.
fn build_command(inv: &Invocation<'_>) -> std::io::Result<(Command, Option<os_pipe::PipeReader>)> {
    let (program, args) = inv
        .argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("empty argv"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(inv.cwd)
        .env_clear()
        .process_group(0);
    let cpu_limit = inv.timeout.as_secs().saturating_add(10);
    let parent = std::process::id();
    // SAFETY: the closure runs in the forked child before `exec`, where only
    // async-signal-safe operations are allowed. It only performs the raw `prctl`, `getppid`
    // and `setrlimit` system calls through nix's thin wrappers (no allocation, no locks) and
    // converts an `Errno` into `io::Error` (also allocation-free).
    unsafe {
        cmd.pre_exec(move || child_safety_net(parent, cpu_limit));
    }
    for (k, v) in inv.env {
        if !v.is_empty() {
            cmd.env(k, v);
        }
    }
    cmd.stdin(if inv.capture.stdin {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });

    let uncaptured_stderr = || {
        if inv.discard_stderr {
            Stdio::null()
        } else {
            Stdio::inherit()
        }
    };
    let mut merged = None;
    match (inv.capture.stdout, inv.capture.stderr) {
        (true, true) => {
            let (reader, writer) = os_pipe::pipe()?;
            let writer2 = writer.try_clone()?;
            cmd.stdout(writer).stderr(writer2);
            merged = Some(reader);
        }
        (true, false) => {
            cmd.stdout(Stdio::piped()).stderr(uncaptured_stderr());
        }
        (false, true) => {
            cmd.stdout(Stdio::inherit()).stderr(Stdio::piped());
        }
        (false, false) => {
            cmd.stdout(Stdio::inherit()).stderr(uncaptured_stderr());
        }
    }
    Ok((cmd, merged))
}

/// Run a command, returning its captured output (stdout and stderr interleaved when both are
/// captured). Errors only if the process cannot be spawned.
pub fn run(inv: &Invocation<'_>) -> std::io::Result<RunOutput> {
    disable_core_dumps();
    let (mut cmd, merged) = build_command(inv)?;
    let mut reader: Option<Box<dyn Read + Send>> =
        merged.map(|r| Box::new(r) as Box<dyn Read + Send>);
    let mut child = cmd.spawn()?;
    // Close our copies of the pipe write ends so EOF is seen when the child exits.
    drop(cmd);

    if let Some(mut stdin) = child.stdin.take() {
        let data = inv.stdin.map(<[u8]>::to_vec).unwrap_or_default();
        thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }

    if reader.is_none() {
        if let Some(out) = child.stdout.take() {
            reader = Some(Box::new(out));
        } else if let Some(err) = child.stderr.take() {
            reader = Some(Box::new(err));
        }
    }

    let deadline = Instant::now() + inv.timeout;
    let sink = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let reading = reader.is_some();
    let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(r) = reader {
        let pgid = i32::try_from(child.id()).ok().map(Pid::from_raw);
        spawn_reader(r, Arc::clone(&sink), tx, pgid, Arc::clone(&overflow));
    }

    let take = |sink: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(
            &mut *sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    };

    let eof = !reading
        || rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok();
    let status = if eof {
        wait_until(&mut child, deadline)
    } else {
        None
    };
    let Some(status) = status else {
        kill_group(&mut child);
        let _ = child.wait();
        // Give the reader a moment to drain what was written before the kill.
        let _ = rx.recv_timeout(Duration::from_millis(500));
        // Ensure any late-forked or lingering children in the process group are terminated:
        kill_group(&mut child);
        let mut data = take(&sink);
        data.extend_from_slice(b"\n ** ERROR: process timed out **\n");
        return Ok(RunOutput {
            data,
            timed_out: true,
        });
    };

    // Clean up any orphan background child processes remaining in the test's process group.
    kill_group(&mut child);

    let mut data = take(&sink);
    if overflow.load(std::sync::atomic::Ordering::SeqCst) {
        data.extend_from_slice(OUTPUT_LIMIT_MARKER);
        return Ok(RunOutput {
            data,
            timed_out: false,
        });
    }
    if let Some(sig) = status.signal() {
        data.extend_from_slice(format!("\nTermsig={sig}\n").as_bytes());
    } else if let Some(code) = status.code() {
        if code > 128 && code < 160 {
            data.extend_from_slice(format!("\nTermsig={}\n", code - 128).as_bytes());
        }
    }
    Ok(RunOutput {
        data,
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, capture: Capture, stdin: Option<&[u8]>, timeout: Duration) -> RunOutput {
        let argv: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
        let mut env = Env::new();
        env.insert("PATH".into(), std::env::var_os("PATH").unwrap_or_default());
        env.insert("EMPTY".into(), OsString::new());
        run(&Invocation {
            argv: &argv,
            env: &env,
            cwd: Path::new("/"),
            stdin,
            capture,
            discard_stderr: true,
            timeout,
        })
        .unwrap()
    }

    #[test]
    fn merges_stdout_and_stderr_in_order() {
        let out = sh(
            "echo a; echo b >&2; echo c",
            Capture::ALL,
            None,
            Duration::from_secs(10),
        );
        assert_eq!(out.data, b"a\nb\nc\n");
    }

    #[test]
    fn stdout_only_and_stdin() {
        let out = sh(
            "cat; echo err >&2",
            Capture::STDOUT_ONLY,
            Some(b"in"),
            Duration::from_secs(10),
        );
        assert_eq!(out.data, b"in");
    }

    #[test]
    fn empty_env_values_are_unset() {
        let out = sh(
            "echo ${EMPTY-unset}",
            Capture::ALL,
            None,
            Duration::from_secs(10),
        );
        assert_eq!(out.data, b"unset\n");
    }

    #[test]
    fn children_get_cpu_limit() {
        let out = sh("ulimit -t", Capture::ALL, None, Duration::from_secs(10));
        assert_eq!(out.data, b"20\n");
    }

    #[test]
    fn children_cannot_dump_core() {
        let out = sh("ulimit -c", Capture::ALL, None, Duration::from_secs(10));
        assert_eq!(out.data, b"0\n");
    }

    #[test]
    fn runaway_output_is_capped_and_killed() {
        let start = Instant::now();
        let out = sh("yes", Capture::ALL, None, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(20));
        assert!(out.data.ends_with(OUTPUT_LIMIT_MARKER));
        assert_eq!(out.data.len(), OUTPUT_LIMIT + OUTPUT_LIMIT_MARKER.len());
    }

    #[test]
    fn signals_and_timeouts() {
        let out = sh("kill -SEGV $$", Capture::ALL, None, Duration::from_secs(10));
        assert_eq!(out.data, b"\nTermsig=11\n");
        let out = sh(
            "echo x; sleep 30",
            Capture::ALL,
            None,
            Duration::from_millis(300),
        );
        assert!(out.timed_out);
        assert_eq!(out.data, b"x\n\n ** ERROR: process timed out **\n");
    }

    #[test]
    fn timeout_cleans_up_background_children() {
        let out = sh(
            "sleep 30 & sleep 30",
            Capture::ALL,
            None,
            Duration::from_millis(300),
        );
        assert!(out.timed_out);
        assert!(out.data.ends_with(b"** ERROR: process timed out **\n"));
    }
}
