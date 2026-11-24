// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The scoreboard (ref: `sapi/fpm/fpm/fpm_scoreboard.c`,
//! `fpm_request.c`): shared memory where each worker records what it is
//! doing and the master (process management, status page) reads it.
//!
//! The memory is an array of 64-bit atomics mapped before the workers
//! are forked ([`super::os::shm_alloc`]); every field is one or more
//! words, strings are stored eight bytes to a word behind a per-slot spin
//! lock (`fpm_spinlock`), as php-src guards its slots.

use std::sync::atomic::{AtomicU64, Ordering};

/// `enum fpm_request_stage_e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Creating = 0,
    Accepting = 1,
    ReadingHeaders = 2,
    Info = 3,
    Executing = 4,
    End = 5,
    Finished = 6,
}

impl Stage {
    fn from_u64(v: u64) -> Stage {
        match v {
            1 => Stage::Accepting,
            2 => Stage::ReadingHeaders,
            3 => Stage::Info,
            4 => Stage::Executing,
            5 => Stage::End,
            6 => Stage::Finished,
            _ => Stage::Creating,
        }
    }

    /// `requests_stages[]`.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Creating => "Creating",
            Stage::Accepting => "Idle",
            Stage::ReadingHeaders => "Reading headers",
            Stage::Info => "Getting request information",
            Stage::Executing => "Running",
            Stage::End => "Ending",
            Stage::Finished => "Finishing",
        }
    }

    /// Whether the worker counts as idle (`Idle`, the stages before it).
    pub fn idle(self) -> bool {
        matches!(self, Stage::Creating | Stage::Accepting)
    }
}

// Pool header words.
const H_START: usize = 0;
const H_REQUESTS: usize = 1;
const H_MAX_CHILDREN_REACHED: usize = 2;
const H_LQ_MAX: usize = 3;
const H_ACTIVE_MAX: usize = 4;
const H_SLOW: usize = 5;
const H_LQ: usize = 6;
const H_LQ_LEN: usize = 7;
const H_IDLE: usize = 8;
const H_ACTIVE: usize = 9;
const H_MEMORY_PEAK: usize = 10;
const HEADER: usize = 16;

// Slot words.
const S_LOCK: usize = 0;
const S_PID: usize = 1;
const S_START_EPOCH: usize = 2;
const S_REQUESTS: usize = 3;
const S_STAGE: usize = 4;
const S_ACCEPTED_US: usize = 5;
const S_DURATION_US: usize = 6;
const S_CONTENT_LENGTH: usize = 7;
const S_MEMORY: usize = 8;
const S_CPU_US: usize = 9;
const S_ACCEPTED_EPOCH: usize = 10;
/// Set by the master when the request is slow: the worker writes its
/// backtrace to the slowlog (what C's `fpm_php_trace()` reads by ptrace).
const S_TRACE: usize = 11;
/// Strings: (first word, words).
const S_METHOD: (usize, usize) = (16, 2);
const S_URI: (usize, usize) = (18, 64);
const S_QUERY: (usize, usize) = (82, 256);
const S_SCRIPT: (usize, usize) = (338, 64);
const S_USER: (usize, usize) = (402, 4);
const SLOT: usize = 406;

/// One pool's part of the shared memory.
#[derive(Debug, Clone, Copy)]
pub struct Scoreboard {
    mem: &'static [AtomicU64],
    nprocs: usize,
}

/// What a worker's slot says.
#[derive(Debug, Clone, Default)]
pub struct Proc {
    pub pid: i32,
    pub start_epoch: i64,
    pub requests: u64,
    pub stage: Option<Stage>,
    /// `accepted` in microseconds since the epoch.
    pub accepted_us: u64,
    pub accepted_epoch: i64,
    pub duration_us: u64,
    pub content_length: u64,
    pub memory: u64,
    pub cpu_us: u64,
    pub method: String,
    pub uri: String,
    pub query: String,
    pub script: String,
    pub user: String,
}

/// The pool's counters (`struct fpm_scoreboard_s`).
#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub start_epoch: i64,
    pub requests: u64,
    pub max_children_reached: u64,
    pub lq: u64,
    pub lq_max: u64,
    pub lq_len: u64,
    pub idle: u64,
    pub active: u64,
    pub active_max: u64,
    pub slow: u64,
    pub memory_peak: u64,
}

fn now_us() -> u64 {
    let (s, us) = super::os::now();
    u64::try_from(s).unwrap_or(0) * 1_000_000 + u64::from(us)
}

impl Scoreboard {
    /// `fpm_scoreboard_init_main()` for one pool of `nprocs` slots.
    pub fn new(nprocs: usize) -> Option<Scoreboard> {
        let words = HEADER + nprocs * SLOT;
        let mem = super::os::shm_alloc(words * 8)?;
        let sb = Scoreboard { mem, nprocs };
        sb.mem[H_START].store(
            u64::try_from(super::os::now().0).unwrap_or(0),
            Ordering::Relaxed,
        );
        Some(sb)
    }

    fn word(&self, slot: usize, w: usize) -> &AtomicU64 {
        &self.mem[HEADER + slot * SLOT + w]
    }

    fn lock(&self, slot: usize) {
        let l = self.word(slot, S_LOCK);
        while l
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    fn unlock(&self, slot: usize) {
        self.word(slot, S_LOCK).store(0, Ordering::Release);
    }

    fn put_str(&self, slot: usize, (first, words): (usize, usize), s: &[u8]) {
        let cap = words * 8 - 1;
        let s = &s[..s.len().min(cap)];
        let mut buf = vec![0u8; words * 8];
        buf[..s.len()].copy_from_slice(s);
        for (i, chunk) in buf.chunks(8).enumerate() {
            let mut w = [0u8; 8];
            w.copy_from_slice(chunk);
            self.word(slot, first + i)
                .store(u64::from_ne_bytes(w), Ordering::Relaxed);
        }
    }

    fn get_str(&self, slot: usize, (first, words): (usize, usize)) -> String {
        let mut buf = Vec::with_capacity(words * 8);
        for i in 0..words {
            buf.extend_from_slice(
                &self
                    .word(slot, first + i)
                    .load(Ordering::Relaxed)
                    .to_ne_bytes(),
            );
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// `fpm_scoreboard_proc_alloc()`: a free slot for a new child.
    pub fn alloc(&self, pid: i32) -> Option<usize> {
        for i in 0..self.nprocs {
            if self.word(i, S_PID).load(Ordering::Relaxed) == 0 {
                self.lock(i);
                for w in 1..SLOT {
                    self.word(i, w).store(0, Ordering::Relaxed);
                }
                self.word(i, S_PID)
                    .store(u64::from(pid.unsigned_abs()), Ordering::Relaxed);
                self.word(i, S_START_EPOCH).store(
                    u64::try_from(super::os::now().0).unwrap_or(0),
                    Ordering::Relaxed,
                );
                self.unlock(i);
                return Some(i);
            }
        }
        None
    }

    /// `fpm_scoreboard_proc_free()`.
    pub fn free(&self, slot: usize) {
        self.lock(slot);
        self.word(slot, S_PID).store(0, Ordering::Relaxed);
        self.unlock(slot);
    }

    /// Re-records the child's PID once it is known (the parent's side).
    pub fn set_pid(&self, slot: usize, pid: i32) {
        self.word(slot, S_PID)
            .store(u64::from(pid.unsigned_abs()), Ordering::Relaxed);
    }

    /// `fpm_request_accepting()`.
    pub fn accepting(&self, slot: usize) {
        self.lock(slot);
        let w = self.word(slot, S_STAGE);
        if Stage::from_u64(w.load(Ordering::Relaxed)) != Stage::Accepting {
            // The previous request's duration stays for the status page.
            w.store(Stage::Accepting as u64, Ordering::Relaxed);
        }
        self.unlock(slot);
    }

    /// `fpm_request_reading_headers()`: a connection was accepted.
    pub fn reading_headers(&self, slot: usize) {
        self.lock(slot);
        let now = now_us();
        self.word(slot, S_STAGE)
            .store(Stage::ReadingHeaders as u64, Ordering::Relaxed);
        self.word(slot, S_ACCEPTED_US).store(now, Ordering::Relaxed);
        self.word(slot, S_ACCEPTED_EPOCH)
            .store(now / 1_000_000, Ordering::Relaxed);
        self.word(slot, S_DURATION_US).store(0, Ordering::Relaxed);
        self.word(slot, S_REQUESTS).fetch_add(1, Ordering::Relaxed);
        self.unlock(slot);
        self.mem[H_REQUESTS].fetch_add(1, Ordering::Relaxed);
        // `fpm_scoreboard_update_commit(-1, 1, ...)`: the peak of active
        // workers follows each accepted request.
        let (_, active) = self.count();
        self.mem[H_ACTIVE_MAX].fetch_max(active, Ordering::Relaxed);
    }

    /// `fpm_request_info()`: what the status page shows of the request.
    #[allow(clippy::too_many_arguments)]
    pub fn info(
        &self,
        slot: usize,
        method: &[u8],
        uri: &[u8],
        query: &[u8],
        script: &[u8],
        user: &[u8],
        content_length: u64,
    ) {
        self.lock(slot);
        self.word(slot, S_STAGE)
            .store(Stage::Info as u64, Ordering::Relaxed);
        self.put_str(slot, S_METHOD, method);
        self.put_str(slot, S_URI, uri);
        self.put_str(slot, S_QUERY, query);
        self.put_str(slot, S_SCRIPT, script);
        self.put_str(slot, S_USER, user);
        self.word(slot, S_CONTENT_LENGTH)
            .store(content_length, Ordering::Relaxed);
        self.unlock(slot);
    }

    /// `fpm_request_executing()`.
    pub fn executing(&self, slot: usize) {
        self.word(slot, S_STAGE)
            .store(Stage::Executing as u64, Ordering::Relaxed);
    }

    /// `fpm_request_end()`: the duration and memory of the request.
    pub fn end(&self, slot: usize, memory: u64, cpu_us: u64) {
        self.lock(slot);
        let accepted = self.word(slot, S_ACCEPTED_US).load(Ordering::Relaxed);
        self.word(slot, S_STAGE)
            .store(Stage::Finished as u64, Ordering::Relaxed);
        self.word(slot, S_DURATION_US)
            .store(now_us().saturating_sub(accepted), Ordering::Relaxed);
        self.word(slot, S_MEMORY).store(memory, Ordering::Relaxed);
        self.word(slot, S_CPU_US).store(cpu_us, Ordering::Relaxed);
        self.unlock(slot);
        self.mem[H_MEMORY_PEAK].fetch_max(memory, Ordering::Relaxed);
    }

    /// A worker's slot.
    pub fn proc(&self, slot: usize) -> Proc {
        self.lock(slot);
        let w = |i| self.word(slot, i).load(Ordering::Relaxed);
        let pid = w(S_PID);
        let mut p = Proc {
            pid: i32::try_from(pid).unwrap_or(0),
            start_epoch: i64::try_from(w(S_START_EPOCH)).unwrap_or(0),
            requests: w(S_REQUESTS),
            stage: (pid != 0).then(|| Stage::from_u64(w(S_STAGE))),
            accepted_us: w(S_ACCEPTED_US),
            accepted_epoch: i64::try_from(w(S_ACCEPTED_EPOCH)).unwrap_or(0),
            duration_us: w(S_DURATION_US),
            content_length: w(S_CONTENT_LENGTH),
            memory: w(S_MEMORY),
            cpu_us: w(S_CPU_US),
            ..Proc::default()
        };
        p.method = self.get_str(slot, S_METHOD);
        p.uri = self.get_str(slot, S_URI);
        p.query = self.get_str(slot, S_QUERY);
        p.script = self.get_str(slot, S_SCRIPT);
        p.user = self.get_str(slot, S_USER);
        self.unlock(slot);
        p
    }

    /// Every used slot.
    pub fn procs(&self) -> Vec<Proc> {
        (0..self.nprocs)
            .filter(|&i| self.word(i, S_PID).load(Ordering::Relaxed) != 0)
            .map(|i| self.proc(i))
            .collect()
    }

    /// `fpm_scoreboard_update()` from the master's process management:
    /// the idle and active counts, the listen queue.
    pub fn update(&self, idle: u64, active: u64, lq: u64, lq_len: u64, max_children_reached: bool) {
        self.mem[H_IDLE].store(idle, Ordering::Relaxed);
        self.mem[H_ACTIVE].store(active, Ordering::Relaxed);
        self.mem[H_ACTIVE_MAX].fetch_max(active, Ordering::Relaxed);
        self.mem[H_LQ].store(lq, Ordering::Relaxed);
        self.mem[H_LQ_LEN].store(lq_len, Ordering::Relaxed);
        self.mem[H_LQ_MAX].fetch_max(lq, Ordering::Relaxed);
        if max_children_reached {
            self.mem[H_MAX_CHILDREN_REACHED].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A slow request was logged.
    pub fn slow(&self) {
        self.mem[H_SLOW].fetch_add(1, Ordering::Relaxed);
    }

    /// `fpm_trace_signal()`: asks the worker in `slot` for its backtrace.
    pub fn request_trace(&self, slot: usize) {
        self.word(slot, S_TRACE).store(1, Ordering::Release);
    }

    /// Whether the master asked for a backtrace (and clears the request).
    pub fn take_trace(&self, slot: usize) -> bool {
        self.word(slot, S_TRACE).swap(0, Ordering::AcqRel) != 0
    }

    /// Whether a backtrace request is pending, without taking it.
    pub fn trace_pending(&self, slot: usize) -> bool {
        self.word(slot, S_TRACE).load(Ordering::Acquire) != 0
    }

    /// The pool's counters.
    pub fn totals(&self) -> Totals {
        let h = |i: usize| self.mem[i].load(Ordering::Relaxed);
        Totals {
            start_epoch: i64::try_from(h(H_START)).unwrap_or(0),
            requests: h(H_REQUESTS),
            max_children_reached: h(H_MAX_CHILDREN_REACHED),
            lq: h(H_LQ),
            lq_max: h(H_LQ_MAX),
            lq_len: h(H_LQ_LEN),
            idle: h(H_IDLE),
            active: h(H_ACTIVE),
            active_max: h(H_ACTIVE_MAX),
            slow: h(H_SLOW),
            memory_peak: h(H_MEMORY_PEAK),
        }
    }

    /// Counts the idle and active workers from their slots.
    pub fn count(&self) -> (u64, u64) {
        let mut idle = 0;
        let mut active = 0;
        for i in 0..self.nprocs {
            if self.word(i, S_PID).load(Ordering::Relaxed) == 0 {
                continue;
            }
            if Stage::from_u64(self.word(i, S_STAGE).load(Ordering::Relaxed)).idle() {
                idle += 1;
            } else {
                active += 1;
            }
        }
        (idle, active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_record_requests() {
        let sb = Scoreboard::new(2).expect("shm");
        let s = sb.alloc(42).expect("slot");
        sb.accepting(s);
        assert_eq!(sb.count(), (1, 0));
        sb.reading_headers(s);
        sb.info(s, b"GET", b"/x.php", b"a=1", b"/srv/x.php", b"", 0);
        sb.executing(s);
        assert_eq!(sb.count(), (0, 1));
        let p = sb.proc(s);
        assert_eq!(p.pid, 42);
        assert_eq!(p.uri, "/x.php");
        assert_eq!(p.method, "GET");
        assert_eq!(p.requests, 1);
        assert_eq!(p.stage, Some(Stage::Executing));
        sb.free(s);
        assert!(sb.procs().is_empty());
    }
}
