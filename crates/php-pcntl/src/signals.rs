// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The pending-signal queue (`PCNTL_G(head)`/`tail`/`spares`).
//!
//! PHP's C signal handler only appends a record to a preallocated list;
//! PHP callables run later, when the engine dispatches the queue (at ticks,
//! on `pcntl_signal_dispatch()`, or at the next VM interrupt check when
//! `pcntl_async_signals(true)`).  Here the handler writes into a fixed pool
//! of slots using only atomics (async-signal-safe, no allocation), and
//! [`drain`] collects ready slots in arrival order.

use crate::sys::RawSigInfo;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};

/// Number of records (PHP preallocates `num_signals` spares; signals
/// arriving while all are in use are dropped).
pub const CAPACITY: usize = 65;

const FREE: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const READING: u8 = 3;

struct Slot {
    state: AtomicU8,
    seq: AtomicU64,
    signo: AtomicI32,
    words: [AtomicU64; 16],
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(FREE),
            seq: AtomicU64::new(0),
            signo: AtomicI32::new(0),
            words: [const { AtomicU64::new(0) }; 16],
        }
    }
}

static SLOTS: [Slot; CAPACITY] = [const { Slot::new() }; CAPACITY];
static SEQ: AtomicU64 = AtomicU64::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);
static ASYNC: AtomicBool = AtomicBool::new(false);

/// Records a delivered signal.  Called from the signal handler, so it uses
/// nothing but lock-free atomics.
pub(crate) fn enqueue(signo: i32, info: &RawSigInfo) {
    for slot in &SLOTS {
        if slot
            .state
            .compare_exchange(FREE, WRITING, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            slot.signo.store(signo, Ordering::Relaxed);
            for (dst, &w) in slot.words.iter().zip(info.0.iter()) {
                dst.store(w, Ordering::Relaxed);
            }
            slot.seq
                .store(SEQ.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
            slot.state.store(READY, Ordering::Release);
            PENDING.store(true, Ordering::Release);
            return;
        }
    }
    // Out of records: the signal is forgotten, as in PHP.
}

/// Takes every recorded signal, oldest first.
pub(crate) fn drain() -> Vec<(i32, RawSigInfo)> {
    let mut out = Vec::new();
    for slot in &SLOTS {
        if slot
            .state
            .compare_exchange(READY, READING, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            let mut words = [0u64; 16];
            for (w, src) in words.iter_mut().zip(slot.words.iter()) {
                *w = src.load(Ordering::Relaxed);
            }
            let seq = slot.seq.load(Ordering::Relaxed);
            let signo = slot.signo.load(Ordering::Relaxed);
            slot.state.store(FREE, Ordering::Release);
            out.push((seq, signo, RawSigInfo(words)));
        }
    }
    out.sort_by_key(|e| e.0);
    out.into_iter().map(|(_, s, i)| (s, i)).collect()
}

/// Whether some recorded signal has not been drained yet.
pub(crate) fn any_ready() -> bool {
    SLOTS
        .iter()
        .any(|s| s.state.load(Ordering::Acquire) == READY)
}

/// Whether a signal has been recorded since the queue was last emptied
/// (`PCNTL_G(pending_signals)`).  Cheap enough to poll at every tick.
pub fn signals_pending() -> bool {
    PENDING.load(Ordering::Acquire)
}

pub(crate) fn clear_pending() {
    PENDING.store(false, Ordering::Release);
}

pub(crate) fn set_pending() {
    PENDING.store(true, Ordering::Release);
}

/// Whether the VM should dispatch at its next interrupt check: a signal is
/// pending and `pcntl_async_signals(true)` is in effect (PHP sets
/// `EG(vm_interrupt)` in that case).
pub fn async_interrupt_pending() -> bool {
    ASYNC.load(Ordering::Relaxed) && signals_pending()
}

pub(crate) fn set_async(on: bool) {
    ASYNC.store(on, Ordering::Relaxed);
}
