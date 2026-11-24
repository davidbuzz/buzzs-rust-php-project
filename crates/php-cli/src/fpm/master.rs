// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The master process (ref: `sapi/fpm/fpm/fpm.c` `fpm_run()`,
//! `fpm_children.c`, `fpm_process_ctl.c`, `fpm_events.c`, `fpm_stdio.c`,
//! `fpm_request.c` `fpm_request_check_timed_out()`): the workers of each
//! pool (`pm = static`, `dynamic`, `ondemand`), the signals (SIGTERM and
//! SIGINT terminate, SIGQUIT finishes, SIGUSR1 reopens the logs, SIGUSR2
//! reloads), the workers' captured output and the request timeouts.

use super::conf::{Conf, Pm, Pool};
use super::log::{self, Level};
use super::os::{self, Exit, Fork};
use super::scoreboard::{Scoreboard, Stage};
use super::sockets::Listening;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// `FPM_EXIT_OK`.
pub const EXIT_OK: i32 = 0;
/// `FPM_EXIT_SOFTWARE`.
pub const EXIT_SOFTWARE: i32 = 70;
/// `FPM_EXIT_CONFIG`.
pub const EXIT_CONFIG: i32 = 78;

/// `FPM_STDIO_CMD_FLUSH` (with its NUL): a worker writes it to standard
/// error after each request, ending the line the master relays.
const CMD_FLUSH: &[u8; 6] = b"\0fscf\0";

/// `struct fpm_child_s`.
struct Child {
    pid: i32,
    slot: usize,
    started: Instant,
    /// Stopped by the process management (`idle_kill`): not respawned.
    idle_kill: bool,
    /// The read ends of the worker's standard output and error
    /// (`catch_workers_output`).
    fd_stdout: Option<RawFd>,
    fd_stderr: Option<RawFd>,
    /// `child->log_stream`: what the worker said, as log lines.
    log_stream: Option<log::Stream>,
    /// `child->slow_logged`: the start of the request already reported as
    /// slow (0: none).
    slow_logged: u64,
}

impl Child {
    fn fds(&self) -> impl Iterator<Item = (RawFd, bool)> + '_ {
        self.fd_stdout
            .map(|fd| (fd, true))
            .into_iter()
            .chain(self.fd_stderr.map(|fd| (fd, false)))
    }
}

/// A pool at run time (`struct fpm_worker_pool_s`).
pub struct PoolRt {
    pub conf: Pool,
    pub listening: Listening,
    pub scoreboard: Scoreboard,
    children: Vec<Child>,
    /// `idle_spawn_rate`.
    idle_spawn_rate: i32,
    /// `warn_max_children`.
    warned_max_children: bool,
    /// The last time the process management looked at it.
    last_check: Instant,
}

impl PoolRt {
    /// A pool with its socket and scoreboard.
    pub fn new(conf: Pool, listening: Listening, scoreboard: Scoreboard) -> PoolRt {
        PoolRt {
            conf,
            listening,
            scoreboard,
            children: Vec::new(),
            idle_spawn_rate: 1,
            warned_max_children: false,
            last_check: Instant::now(),
        }
    }
}

/// `enum fpm_pctl_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Reloading,
    Terminating,
    Finishing,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Normal => "normal",
            State::Reloading => "reloading",
            State::Terminating => "terminating",
            State::Finishing => "finishing",
        }
    }
}

/// Who returns from [`Master::run`].
pub enum Role {
    /// A worker of `pool`, using scoreboard slot `slot`.
    Child { pool: usize, slot: usize },
    /// The master is done: exit with this status.
    Exit(i32),
    /// Re-execute the binary (`fpm_pctl_exec()`).
    Reload,
}

/// The master's state.
pub struct Master {
    pub conf: Conf,
    pub pools: Vec<PoolRt>,
    signals: RawFd,
    state: State,
    signal_sent: i32,
    deadline: Option<Instant>,
    /// `fpm_postponed_child_free()`: buried workers' streams, closed (their
    /// unfinished line logged) a second later.
    postponed: Vec<(Instant, log::Stream)>,
}

/// `str_purify_filename()`: control and non-ASCII bytes as `.`.
fn purify_filename(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if (b' '..=0x7f).contains(&b) {
                char::from(b)
            } else {
                '.'
            }
        })
        .collect()
}

fn secs_usecs(d: Duration) -> String {
    format!("{}.{:06}", d.as_secs(), d.subsec_micros())
}

impl Master {
    /// The master for `conf`'s pools.
    pub fn new(conf: Conf, pools: Vec<PoolRt>, signals: RawFd) -> Master {
        Master {
            conf,
            pools,
            signals,
            state: State::Normal,
            signal_sent: 0,
            deadline: None,
            postponed: Vec::new(),
        }
    }

    fn running(&self) -> usize {
        self.pools.iter().map(|p| p.children.len()).sum()
    }

    /// `fpm_pctl_can_spawn_children()`.
    fn can_spawn(&self) -> bool {
        self.state == State::Normal
    }

    /// `fpm_children_make()` for one child of pool `pi`: `None` in the
    /// parent (or on failure), the role in the child.
    #[allow(clippy::too_many_lines)]
    fn make_child(&mut self, pi: usize, is_debug: bool) -> Option<Role> {
        let process_max = usize::try_from(self.conf.global.process_max).unwrap_or(0);
        if process_max > 0 && self.running() >= process_max {
            log::warning("fpm_children_make", 0, "The maximum number of processes has been reached. Please review your configuration and consider raising 'process.max'");
            return None;
        }
        let pool = &self.pools[pi];
        let Some(slot) = pool.scoreboard.alloc(0) else {
            log::error(
                "fpm_resources_prepare",
                0,
                &format!(
                    "[pool {}] unable to allocate a scoreboard slot",
                    pool.conf.name
                ),
            );
            return None;
        };
        let mut pipes = Vec::new();
        if pool.conf.catch_workers_output {
            for _ in 0..2 {
                match os::pipe() {
                    Ok(p) => pipes.push(p),
                    Err(e) => {
                        log::syserror(
                            "fpm_stdio_prepare_pipes",
                            0,
                            "failed to prepare the stdout or stderr pipe",
                            &e,
                        );
                        for (r, w) in pipes {
                            os::close(r);
                            os::close(w);
                        }
                        pool.scoreboard.free(slot);
                        return None;
                    }
                }
            }
        }
        // `fpm_signals_child_block()` around the fork.
        os::block_signals(&os::CHILD_BLOCK_SIGNALS);
        let forked = os::fork();
        if !matches!(forked, Ok(Fork::Child)) {
            os::unblock_signals();
        }
        match forked {
            Ok(Fork::Child) => {
                log::set_child();
                // `fpm_stdio_child_use_pipes()`.
                if let [(r1, w1), (r2, w2)] = pipes[..] {
                    os::close(r1);
                    os::close(r2);
                    os::dup2(w1, libc::STDOUT_FILENO);
                    os::dup2(w2, libc::STDERR_FILENO);
                    os::close(w1);
                    os::close(w2);
                } else {
                    // "stdout of parent is always /dev/null".
                    os::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO);
                }
                for (i, p) in self.pools.iter().enumerate() {
                    if i != pi {
                        os::close(p.listening.fd);
                    }
                    for c in &p.children {
                        for (fd, _) in c.fds() {
                            os::close(fd);
                        }
                    }
                }
                os::close(self.signals);
                self.pools[pi].scoreboard.set_pid(slot, os::getpid());
                Some(Role::Child { pool: pi, slot })
            }
            Ok(Fork::Parent(pid)) => {
                let (mut fd_stdout, mut fd_stderr) = (None, None);
                if let [(r1, w1), (r2, w2)] = pipes[..] {
                    os::close(w1);
                    os::close(w2);
                    os::set_nonblocking(r1);
                    os::set_nonblocking(r2);
                    fd_stdout = Some(r1);
                    fd_stderr = Some(r2);
                }
                let pool = &mut self.pools[pi];
                pool.scoreboard.set_pid(slot, pid);
                pool.children.push(Child {
                    pid,
                    slot,
                    started: Instant::now(),
                    idle_kill: false,
                    fd_stdout,
                    fd_stderr,
                    log_stream: None,
                    slow_logged: 0,
                });
                log::log(
                    if is_debug {
                        Level::Debug
                    } else {
                        Level::Notice
                    },
                    "fpm_children_make",
                    0,
                    &format!("[pool {}] child {pid} started", pool.conf.name),
                    None,
                );
                None
            }
            Err(e) => {
                log::syserror("fpm_children_make", 0, "fork() failed", &e);
                for (r, w) in pipes {
                    os::close(r);
                    os::close(w);
                }
                self.pools[pi].scoreboard.free(slot);
                None
            }
        }
    }

    /// `fpm_children_create_initial()` then `fpm_event_loop()`.
    pub fn run(&mut self) -> Role {
        for pi in 0..self.pools.len() {
            let n = match self.pools[pi].conf.pm {
                Pm::Static => self.pools[pi].conf.pm_max_children,
                Pm::Dynamic => self.pools[pi].conf.pm_start_servers,
                _ => 0,
            };
            for _ in 0..n {
                if let Some(r) = self.make_child(pi, false) {
                    return r;
                }
            }
        }
        log::notice("fpm_event_loop", 0, "ready to handle connections");
        self.event_loop()
    }

    fn event_loop(&mut self) -> Role {
        let mut last_tick = Instant::now();
        loop {
            // The descriptors: the signal pipe, the captured streams, and
            // the sockets of `ondemand` pools without an idle worker.
            let mut fds: Vec<(RawFd, i16)> = vec![(self.signals, libc::POLLIN)];
            let mut owners: Vec<(usize, i32, bool)> = Vec::new();
            for (pi, p) in self.pools.iter().enumerate() {
                for c in &p.children {
                    for (fd, is_stdout) in c.fds() {
                        fds.push((fd, libc::POLLIN));
                        owners.push((pi, c.pid, is_stdout));
                    }
                }
            }
            let mut ondemand: Vec<(usize, usize)> = Vec::new();
            if self.can_spawn() {
                for (pi, p) in self.pools.iter().enumerate() {
                    if p.conf.pm == Pm::Ondemand
                        && p.children.len() < usize::try_from(p.conf.pm_max_children).unwrap_or(0)
                        && p.scoreboard.count().0 == 0
                    {
                        ondemand.push((fds.len(), pi));
                        fds.push((p.listening.fd, libc::POLLIN));
                    }
                }
            }
            let mut timeout =
                1000i64 - i64::try_from(last_tick.elapsed().as_millis()).unwrap_or(1000);
            if let Some(d) = self.deadline {
                let left = i64::try_from(d.saturating_duration_since(Instant::now()).as_millis())
                    .unwrap_or(0);
                timeout = timeout.min(left);
            }
            if let Some((d, _)) = self.postponed.first() {
                let left = i64::try_from(d.saturating_duration_since(Instant::now()).as_millis())
                    .unwrap_or(0);
                timeout = timeout.min(left);
            }
            let timeout = i32::try_from(timeout.max(0)).unwrap_or(1000);
            let revents = os::poll(&fds, timeout).unwrap_or_else(|| vec![0; fds.len()]);
            // What workers wrote became readable before their exit was
            // signalled (epoll reports in that order).
            for (i, &(pi, pid, is_stdout)) in owners.iter().enumerate() {
                if revents[i + 1] != 0 {
                    if let Some(ci) = self.pools[pi].children.iter().position(|c| c.pid == pid) {
                        self.child_said(pi, ci, is_stdout);
                    }
                }
            }
            if revents[0] != 0 {
                if let Some(r) = self.got_signals() {
                    return r;
                }
            }
            let now = Instant::now();
            while self.postponed.first().is_some_and(|(d, _)| *d <= now) {
                let (_, mut stream) = self.postponed.remove(0);
                stream.finish();
            }
            for &(i, pi) in &ondemand {
                if revents.get(i).copied().unwrap_or(0) != 0
                    && self.can_spawn()
                    && self.pools[pi].scoreboard.count().0 == 0
                {
                    log::debug(
                        "fpm_pctl_on_socket_accept",
                        0,
                        &format!(
                            "[pool {}] got accept without idle child available .... I forked",
                            self.pools[pi].conf.name
                        ),
                    );
                    if let Some(r) = self.make_child(pi, true) {
                        return r;
                    }
                }
            }
            if let Some(d) = self.deadline {
                if Instant::now() >= d {
                    self.deadline = None;
                    if let Some(r) = self.action_next() {
                        return r;
                    }
                }
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                last_tick = Instant::now();
                if let Some(r) = self.heartbeat() {
                    return r;
                }
            }
        }
    }

    /// `fpm_got_signal()`.
    fn got_signals(&mut self) -> Option<Role> {
        let mut buf = [0u8; 64];
        let n = os::read(self.signals, &mut buf).unwrap_or(0);
        for &c in &buf[..n] {
            let r = match c {
                b'C' => {
                    log::debug("fpm_got_signal", 0, "received SIGCHLD");
                    self.bury()
                }
                b'I' | b'T' => {
                    log::debug(
                        "fpm_got_signal",
                        0,
                        if c == b'I' {
                            "received SIGINT"
                        } else {
                            "received SIGTERM"
                        },
                    );
                    log::notice("fpm_got_signal", 0, "Terminating ...");
                    self.set_state(State::Terminating)
                }
                b'Q' => {
                    log::debug("fpm_got_signal", 0, "received SIGQUIT");
                    log::notice("fpm_got_signal", 0, "Finishing ...");
                    self.set_state(State::Finishing)
                }
                b'1' => {
                    log::debug("fpm_got_signal", 0, "received SIGUSR1");
                    super::reopen_error_log(&self.conf);
                    let pools: Vec<Pool> = self.pools.iter().map(|p| p.conf.clone()).collect();
                    match super::reopen_access_logs(&pools) {
                        Some(0) => {}
                        Some(n) => {
                            // The workers restart on the new descriptors.
                            for _ in 0..n {
                                self.kill_all(libc::SIGQUIT);
                            }
                            log::notice("fpm_got_signal", 0, "access log file re-opened");
                        }
                        None => {
                            log::error("fpm_got_signal", 0, "unable to re-opened access log file");
                        }
                    }
                    super::reopen_done();
                    None
                }
                b'2' => {
                    log::debug("fpm_got_signal", 0, "received SIGUSR2");
                    log::notice("fpm_got_signal", 0, "Reloading in progress ...");
                    self.set_state(State::Reloading)
                }
                _ => None,
            };
            if r.is_some() {
                return r;
            }
        }
        None
    }

    /// `fpm_children_bury()`.
    fn bury(&mut self) -> Option<Role> {
        while let Some((pid, exit)) = os::reap() {
            let found = self.pools.iter().enumerate().find_map(|(pi, p)| {
                p.children
                    .iter()
                    .position(|c| c.pid == pid)
                    .map(|ci| (pi, ci))
            });
            let mut severity = Level::Notice;
            let mut restart = true;
            let how = match exit {
                Exit::Code(code) => {
                    if found.is_some_and(|(pi, ci)| self.pools[pi].children[ci].idle_kill) {
                        restart = false;
                    }
                    if code != EXIT_OK {
                        severity = Level::Warning;
                    }
                    format!("with code {code}")
                }
                Exit::Signal(sig, core) => {
                    if sig == libc::SIGQUIT
                        && found.is_some_and(|(pi, ci)| self.pools[pi].children[ci].idle_kill)
                    {
                        restart = false;
                    }
                    if sig != libc::SIGQUIT {
                        severity = Level::Warning;
                    }
                    format!(
                        "on signal {sig} ({}{})",
                        os::signal_name(sig),
                        if core { " - core dumped" } else { "" }
                    )
                }
                Exit::Stopped => {
                    log::notice(
                        "fpm_children_bury",
                        0,
                        &format!("child {pid} stopped for tracing"),
                    );
                    continue;
                }
            };
            let Some((pi, ci)) = found else {
                log::warning(
                    "fpm_children_bury",
                    0,
                    &format!("unknown child ({pid}) exited {how} - potentially a bug or pre exec child (e.g. s6-notifyoncheck)"),
                );
                continue;
            };
            let child = &self.pools[pi].children[ci];
            let (slot, started) = (child.slot, child.started);
            self.pools[pi].scoreboard.free(slot);
            let lived = secs_usecs(started.elapsed());
            let name = self.pools[pi].conf.name.clone();
            if restart {
                if !self.can_spawn() {
                    severity = Level::Debug;
                }
                log::log(
                    severity,
                    "fpm_children_bury",
                    0,
                    &format!(
                        "[pool {name}] child {pid} exited {how} after {lived} seconds from start"
                    ),
                    None,
                );
            } else {
                log::debug(
                    "fpm_children_bury",
                    0,
                    &format!("[pool {name}] child {pid} has been killed by the process management after {lived} seconds from start"),
                );
            }
            self.child_close(pi, ci);
            if self.state != State::Normal && self.running() == 0 {
                return self.action_last();
            }
            if restart && self.can_spawn() {
                let keep = match self.pools[pi].conf.pm {
                    Pm::Static => true,
                    // A dynamic or ondemand pool replaces what it needs.
                    Pm::Dynamic => {
                        self.pools[pi].children.len()
                            < usize::try_from(self.pools[pi].conf.pm_min_spare_servers).unwrap_or(0)
                    }
                    _ => false,
                };
                if keep {
                    if let Some(r) = self.make_child(pi, false) {
                        return Some(r);
                    }
                }
            }
        }
        None
    }

    /// `fpm_child_close()` in the event loop: what is left in the worker's
    /// pipes is relayed, they are closed, and its stream is closed a second
    /// later (`fpm_postponed_child_free()`).
    fn child_close(&mut self, pi: usize, ci: usize) {
        for is_stdout in [true, false] {
            let fd = {
                let c = &self.pools[pi].children[ci];
                if is_stdout {
                    c.fd_stdout
                } else {
                    c.fd_stderr
                }
            };
            if fd.is_some() {
                self.child_said(pi, ci, is_stdout);
                let c = &mut self.pools[pi].children[ci];
                let slot = if is_stdout {
                    &mut c.fd_stdout
                } else {
                    &mut c.fd_stderr
                };
                if let Some(fd) = slot.take() {
                    os::close(fd);
                }
            }
        }
        let child = self.pools[pi].children.remove(ci);
        if let Some(stream) = child.log_stream {
            self.postponed
                .push((Instant::now() + Duration::from_secs(1), stream));
        }
    }

    /// `fpm_stdio_child_said()`: reads what a worker wrote into its standard
    /// output or error (1023 bytes at a time, until the pipe is empty) and
    /// logs each line as `[pool www] child 12 said into stderr: "..."`; a
    /// line also ends at the worker's `FPM_STDIO_CMD_FLUSH`.
    #[allow(clippy::too_many_lines)]
    fn child_said(&mut self, pi: usize, ci: usize, is_stdout: bool) {
        let pool = &mut self.pools[pi];
        let name = pool.conf.name.clone();
        let decorate = pool.conf.decorate_workers_output;
        let child = &mut pool.children[ci];
        let Some(fd) = (if is_stdout {
            child.fd_stdout
        } else {
            child.fd_stderr
        }) else {
            return;
        };
        let pid = child.pid;
        let which = if is_stdout { "stdout" } else { "stderr" };
        let prefix = format!("[pool {name}] child {pid} said into {which}: ");
        let create = child.log_stream.is_none();
        let s = child.log_stream.get_or_insert_with(|| {
            let mut s = log::Stream::new(Level::Warning, libc::STDERR_FILENO);
            s.set_decorating(decorate);
            s.set_wrapping(true);
            s.set_msg_prefix(prefix.as_bytes());
            s.set_msg_quoting(true);
            s.set_is_stdout(is_stdout);
            s.set_child_pid(pid);
            s
        });
        if !create && (s.is_stdout() != is_stdout || s.child_pid() != pid) {
            s.finish();
            s.set_msg_prefix(prefix.as_bytes());
            s.set_is_stdout(is_stdout);
            s.set_child_pid(pid);
        }
        let mut buf = [0u8; 1023];
        let mut cmd_pos = 0usize;
        let mut read_fail = 0i32;
        'read: loop {
            let n = match os::read_status(fd, &mut buf) {
                Ok(0) => {
                    read_fail = 1;
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    if e != libc::EAGAIN && e != libc::EWOULDBLOCK {
                        read_fail = -e.max(1);
                    }
                    break;
                }
            };
            let buf = &buf[..n];
            let mut start = 0;
            if cmd_pos > 0 {
                let rest = CMD_FLUSH.len() - cmd_pos;
                if rest <= n && buf[..rest] == CMD_FLUSH[cmd_pos..] {
                    s.finish();
                    start = rest;
                } else {
                    s.str(&CMD_FLUSH[..cmd_pos]);
                }
                cmd_pos = 0;
            }
            let mut pos = start;
            while pos < n {
                match buf[pos] {
                    b'\n' => {
                        s.str(&buf[start..pos]);
                        s.finish();
                        start = pos + 1;
                    }
                    0 => {
                        if pos + CMD_FLUSH.len() <= n {
                            if buf[pos..pos + CMD_FLUSH.len()] == CMD_FLUSH[..] {
                                s.str(&buf[start..pos]);
                                s.finish();
                                start = pos + CMD_FLUSH.len();
                                pos = start;
                                continue;
                            }
                        } else if buf[pos..] == CMD_FLUSH[..n - pos] {
                            cmd_pos = n - pos;
                            s.str(&buf[start..pos]);
                            continue 'read;
                        }
                    }
                    _ => {}
                }
                pos += 1;
            }
            if start < pos {
                s.str(&buf[start..pos]);
            }
        }
        if read_fail != 0 {
            if create {
                s.set_msg_suffix(None, Some(b", pipe is closed"));
                s.finish();
            }
            if read_fail < 0 {
                log::syserror(
                    "fpm_stdio_child_said",
                    0,
                    &format!("unable to read what child {pid} said into {which}"),
                    &std::io::Error::from_raw_os_error(-read_fail),
                );
            }
            os::close(fd);
            if is_stdout {
                child.fd_stdout = None;
            } else {
                child.fd_stderr = None;
            }
        }
    }

    /// `fpm_pctl()` with `FPM_PCTL_ACTION_SET`.
    fn set_state(&mut self, new: State) -> Option<Role> {
        if self.state == new {
            return None;
        }
        let allowed = match self.state {
            State::Normal => true,
            State::Reloading => matches!(new, State::Finishing | State::Terminating),
            State::Finishing => new == State::Terminating,
            State::Terminating => false,
        };
        if !allowed {
            log::debug(
                "fpm_pctl",
                0,
                &format!(
                    "not switching to '{}' state, because already in '{}' state",
                    new.name(),
                    self.state.name()
                ),
            );
            return None;
        }
        self.signal_sent = 0;
        self.state = new;
        log::debug(
            "fpm_pctl",
            0,
            &format!("switching to '{}' state", new.name()),
        );
        self.action_next()
    }

    /// `fpm_pctl_action_next()`: the next signal to the workers, with its
    /// timeout (`process_control_timeout` first, then a second).
    fn action_next(&mut self) -> Option<Role> {
        if self.running() == 0 {
            return self.action_last();
        }
        let (sig, timeout) = if self.signal_sent == 0 {
            (
                if self.state == State::Terminating {
                    libc::SIGTERM
                } else {
                    libc::SIGQUIT
                },
                u64::try_from(self.conf.global.process_control_timeout).unwrap_or(0),
            )
        } else if self.signal_sent == libc::SIGQUIT {
            (libc::SIGTERM, 1)
        } else {
            (libc::SIGKILL, 1)
        };
        self.kill_all(sig);
        self.signal_sent = sig;
        self.deadline = Some(Instant::now() + Duration::from_secs(timeout));
        None
    }

    /// `fpm_pctl_action_last()`.
    fn action_last(&mut self) -> Option<Role> {
        match self.state {
            State::Reloading => Some(Role::Reload),
            State::Terminating | State::Finishing => {
                log::notice("fpm_pctl_exit", 0, "exiting, bye-bye!");
                Some(Role::Exit(EXIT_OK))
            }
            State::Normal => None,
        }
    }

    /// `fpm_pctl_kill_all()`.
    fn kill_all(&mut self, sig: i32) {
        let mut alive = 0;
        for p in &mut self.pools {
            for c in &mut p.children {
                log::debug(
                    "fpm_pctl_kill_all",
                    0,
                    &format!(
                        "[pool {}] sending signal {sig} {} to child {}",
                        p.conf.name,
                        os::signal_name(sig),
                        c.pid
                    ),
                );
                if os::kill(c.pid, sig) {
                    alive += 1;
                }
            }
        }
        if alive > 0 {
            log::debug(
                "fpm_pctl_kill_all",
                0,
                &format!("{alive} child(ren) still alive"),
            );
        }
    }

    /// `fpm_pctl_heartbeat()` and `fpm_pctl_perform_idle_server_maintenance()`:
    /// request timeouts, then the spare workers of `dynamic` and the idle
    /// ones of `ondemand` pools.
    fn heartbeat(&mut self) -> Option<Role> {
        self.check_timeouts();
        if !self.can_spawn() {
            return None;
        }
        for pi in 0..self.pools.len() {
            if let Some(r) = self.maintain(pi) {
                return Some(r);
            }
        }
        None
    }

    /// `fpm_request_check_timed_out()` for every worker.
    fn check_timeouts(&mut self) {
        let (secs, usec) = os::now();
        let now = u64::try_from(secs).unwrap_or(0) * 1_000_000 + u64::from(usec);
        for p in &mut self.pools {
            let terminate = u64::try_from(p.conf.request_terminate_timeout).unwrap_or(0);
            let slow = u64::try_from(p.conf.request_slowlog_timeout).unwrap_or(0);
            if terminate == 0 && slow == 0 {
                continue;
            }
            for c in &mut p.children {
                let proc = p.scoreboard.proc(c.slot);
                if c.slow_logged != 0 && c.slow_logged != proc.accepted_us {
                    c.slow_logged = 0;
                }
                let Some(stage) = proc.stage else { continue };
                let running = matches!(stage, Stage::Info | Stage::Executing)
                    || (p.conf.request_terminate_timeout_track_finished
                        && matches!(stage, Stage::End | Stage::Finished));
                if !running || proc.accepted_us == 0 {
                    continue;
                }
                let elapsed = now.saturating_sub(proc.accepted_us);
                let d = Duration::from_micros(elapsed);
                let request = format!(
                    "{} {}{}{}",
                    proc.method,
                    proc.uri,
                    if proc.query.is_empty() { "" } else { "?" },
                    proc.query
                );
                let script = purify_filename(&proc.script);
                if c.slow_logged == 0
                    && slow > 0
                    && stage == Stage::Executing
                    && elapsed >= slow * 1_000_000
                {
                    c.slow_logged = proc.accepted_us;
                    p.scoreboard.request_trace(c.slot);
                    p.scoreboard.slow();
                    log::warning(
                        "fpm_request_check_timed_out",
                        0,
                        &format!(
                            "[pool {}] child {}, script '{script}' (request: \"{request}\") executing too slow ({} sec), logging",
                            p.conf.name,
                            c.pid,
                            secs_usecs(d)
                        ),
                    );
                } else if terminate > 0 && elapsed >= terminate * 1_000_000 {
                    log::warning(
                        "fpm_request_check_timed_out",
                        0,
                        &format!(
                            "[pool {}] child {}, script '{script}' (request: \"{request}\") execution timed out ({} sec), terminating",
                            p.conf.name,
                            c.pid,
                            secs_usecs(d)
                        ),
                    );
                    os::kill(c.pid, libc::SIGTERM);
                }
            }
        }
    }

    /// The process management of one pool.
    #[allow(clippy::too_many_lines)]
    fn maintain(&mut self, pi: usize) -> Option<Role> {
        let p = &mut self.pools[pi];
        p.last_check = Instant::now();
        let (idle, active) = p.scoreboard.count();
        let running = p.children.len();
        let max_children = usize::try_from(p.conf.pm_max_children).unwrap_or(0);
        p.scoreboard.update(idle, active, 0, 0, false);
        match p.conf.pm {
            Pm::Ondemand => {
                // The worker idle longest, if past pm.process_idle_timeout.
                let timeout =
                    u64::try_from(p.conf.pm_process_idle_timeout).unwrap_or(10) * 1_000_000;
                let (secs, usec) = os::now();
                let now = u64::try_from(secs).unwrap_or(0) * 1_000_000 + u64::from(usec);
                let mut oldest: Option<(usize, u64)> = None;
                for (ci, c) in p.children.iter().enumerate() {
                    let pr = p.scoreboard.proc(c.slot);
                    if pr.stage != Some(Stage::Accepting) {
                        continue;
                    }
                    let since = if pr.accepted_us == 0 {
                        u64::try_from(pr.start_epoch).unwrap_or(0) * 1_000_000
                    } else {
                        pr.accepted_us + pr.duration_us
                    };
                    if oldest.is_none_or(|(_, s)| since < s) {
                        oldest = Some((ci, since));
                    }
                }
                if let Some((ci, since)) = oldest {
                    if now.saturating_sub(since) > timeout {
                        let c = &mut p.children[ci];
                        c.idle_kill = true;
                        os::kill(c.pid, libc::SIGQUIT);
                    }
                }
                None
            }
            Pm::Dynamic => {
                let min_spare = u64::try_from(p.conf.pm_min_spare_servers).unwrap_or(0);
                let max_spare = u64::try_from(p.conf.pm_max_spare_servers).unwrap_or(0);
                log::debug(
                    "fpm_pctl_perform_idle_server_maintenance",
                    0,
                    &format!(
                        "[pool {}] currently {active} active children, {idle} spare children, {running} running children. Spawning rate {}",
                        p.conf.name, p.idle_spawn_rate
                    ),
                );
                if idle > max_spare {
                    if let Some(c) = p.children.iter_mut().rev().find(|c| {
                        p.scoreboard.proc(c.slot).stage == Some(Stage::Accepting) && !c.idle_kill
                    }) {
                        c.idle_kill = true;
                        os::kill(c.pid, libc::SIGQUIT);
                    }
                    p.idle_spawn_rate = 1;
                    return None;
                }
                if idle < min_spare {
                    if running >= max_children {
                        if !p.warned_max_children {
                            log::warning(
                                "fpm_pctl_perform_idle_server_maintenance",
                                0,
                                &format!(
                                    "[pool {}] server reached pm.max_children setting ({max_children}), consider raising it",
                                    p.conf.name
                                ),
                            );
                            p.warned_max_children = true;
                            p.scoreboard.update(idle, active, 0, 0, true);
                        }
                        p.idle_spawn_rate = 1;
                        return None;
                    }
                    p.warned_max_children = false;
                    let want = usize::try_from(min_spare - idle).unwrap_or(0);
                    let mut n = usize::try_from(p.idle_spawn_rate).unwrap_or(1).min(want);
                    n = n.min(max_children - running);
                    if n == 0 {
                        n = 1;
                    }
                    if p.idle_spawn_rate >= 8 {
                        log::warning(
                            "fpm_pctl_perform_idle_server_maintenance",
                            0,
                            &format!(
                                "[pool {}] seems busy (you may need to increase pm.start_servers, or pm.min/max_spare_servers), spawning {n} children, there are {idle} idle, and {running} total children",
                                p.conf.name
                            ),
                        );
                    }
                    if p.idle_spawn_rate < p.conf.pm_max_spawn_rate {
                        p.idle_spawn_rate = (p.idle_spawn_rate * 2).min(p.conf.pm_max_spawn_rate);
                    }
                    for _ in 0..n {
                        if let Some(r) = self.make_child(pi, true) {
                            return Some(r);
                        }
                    }
                    log::debug(
                        "fpm_pctl_perform_idle_server_maintenance",
                        0,
                        &format!(
                            "[pool {}] {n} child(ren) have been created dynamically",
                            self.pools[pi].conf.name
                        ),
                    );
                    return None;
                }
                p.idle_spawn_rate = 1;
                None
            }
            _ => None,
        }
    }
}
