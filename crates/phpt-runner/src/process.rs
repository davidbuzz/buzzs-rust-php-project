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

fn spawn_reader(
    mut source: impl Read + Send + 'static,
    sink: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&buf[..n]),
            }
        }
        let _ = done.send(());
    });
}

fn kill_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
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
    if let Some(r) = reader {
        spawn_reader(r, Arc::clone(&sink), tx);
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
        let mut data = take(&sink);
        data.extend_from_slice(b"\n ** ERROR: process timed out **\n");
        return Ok(RunOutput {
            data,
            timed_out: true,
        });
    };

    let mut data = take(&sink);
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
}
