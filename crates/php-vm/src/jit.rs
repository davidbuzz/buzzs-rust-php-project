// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The JIT's switches and counters (ref: `ext/opcache/jit/zend_jit.c`,
//! `zend_jit_config`, `zend_jit_status`).
//!
//! The code generator itself is `exec/tape/jit.rs`: it turns a hot op
//! array's tape into machine code. This module is what the rest of the
//! workspace sees of it: [`configure`] (called by the opcache extension
//! from `opcache.jit` and its companions) and [`stats`]. Both exist without
//! the `jit` cargo feature; nothing is then ever compiled.
//!
//! The state is per thread, like the VM whose op arrays it applies to.
//!
//! The environment variable `PHPRUST_JIT` overrides [`configure`] for the
//! whole process, as a debugging aid: `1` (or `on`) turns the JIT on
//! whatever `opcache.jit` says (the second tier, as `opcache.jit` does),
//! `both` adds the first tier ([`Options::first_tier`]), `all` also makes
//! it compile every op array (see [`Options::everything`]), which is how
//! the test suites exercise the code generator, `tier1` and `all-tier1`
//! are the first tier alone, and `0` (or `off`) turns it off.
//! `PHPRUST_JIT_HOT=n` sets [`Options::hot_loop`] with any of them.

use std::cell::Cell;
use std::sync::OnceLock;

/// What decides whether and when an op array is compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent switches, as the INI settings are
pub struct Options {
    /// `JIT_G(enabled)`.
    pub enabled: bool,
    /// Compile every op array the first time it runs. Without it, only an
    /// op array with a loop that stays in machine code is compiled: code
    /// that leaves for the interpreter on every round (a call, an op with
    /// no specialised instruction) was measured 3-6% slower compiled than
    /// interpreted.
    pub everything: bool,
    /// The first tier: the tape of a loop as machine code. It runs a loop
    /// at about the interpreter's speed (it saves the dispatch and pays a
    /// little for getting in and out), so it is off unless asked for: what
    /// the JIT gains, the second tier gains.
    pub first_tier: bool,
    /// The second tier: a loop is compiled for the types its variables
    /// have when it has proved hot, with their values in registers.
    pub registers: bool,
    /// `opcache.jit_hot_loop`: how many times a loop must be reached
    /// before it is compiled, and again before the second tier looks at
    /// it (0 and 1: the first time). Compiling costs millions of
    /// instructions; a loop of a few rounds is cheaper interpreted.
    pub hot_loop: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            enabled: false,
            everything: false,
            first_tier: false,
            registers: false,
            hot_loop: 64,
        }
    }
}

/// What the JIT has done on this thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Op arrays compiled.
    pub compiled: u64,
    /// Op arrays the code generator gave up on (they stay interpreted).
    pub failed: u64,
    /// Loops compiled by the second tier.
    pub typed_loops: u64,
    /// Bytes of machine code produced.
    pub code_bytes: u64,
}

thread_local! {
    static OPTIONS: Cell<Options> = const {
        Cell::new(Options {
            enabled: false,
            everything: false,
            first_tier: false,
            registers: false,
            hot_loop: 64,
        })
    };
    static STATS: Cell<Stats> = const {
        Cell::new(Stats {
            compiled: 0,
            failed: 0,
            typed_loops: 0,
            code_bytes: 0,
        })
    };
}

/// `PHPRUST_JIT`, read once: the options it forces.
fn forced() -> Option<Options> {
    static FORCED: OnceLock<Option<Options>> = OnceLock::new();
    *FORCED.get_or_init(|| {
        // (enabled, everything, first tier, second tier)
        let (enabled, everything, first_tier, registers) =
            match std::env::var("PHPRUST_JIT").ok()?.trim() {
                "1" | "on" => (true, false, false, true),
                "both" => (true, false, true, true),
                "all" => (true, true, true, true),
                "tier1" => (true, false, true, false),
                "all-tier1" => (true, true, true, false),
                "0" | "off" => (false, false, false, false),
                _ => return None,
            };
        // `PHPRUST_JIT_HOT` sets the threshold; compiling everything is
        // for the tests, which want it at once.
        let hot_loop = std::env::var("PHPRUST_JIT_HOT")
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(if everything { 1 } else { 64 });
        Some(Options {
            enabled,
            everything,
            first_tier,
            registers,
            hot_loop,
        })
    })
}

/// Sets this thread's options. Op arrays that already ran keep what was
/// decided for them when they first did.
pub fn configure(options: Options) {
    OPTIONS.with(|o| o.set(options));
}

/// This thread's options, `PHPRUST_JIT` applied.
#[must_use]
pub fn options() -> Options {
    forced().unwrap_or_else(|| OPTIONS.with(Cell::get))
}

thread_local! {
    /// Whether this thread's VM has observers installed.
    static OBSERVED: Cell<bool> = const { Cell::new(false) };
}

/// Notes that this thread's VM has observers installed (`zend_observer`):
/// they are told of every internal call, so the code may not compute a
/// builtin's result itself.
pub(crate) fn note_observers() {
    OBSERVED.with(|o| o.set(true));
}

/// Whether [`note_observers`] was called on this thread.
#[cfg_attr(not(feature = "jit"), allow(dead_code))]
pub(crate) fn calls_are_observed() -> bool {
    OBSERVED.with(Cell::get)
}

/// Whether this build can compile at all (the `jit` cargo feature).
#[must_use]
pub const fn available() -> bool {
    cfg!(feature = "jit")
}

/// This thread's counters.
#[must_use]
pub fn stats() -> Stats {
    STATS.with(Cell::get)
}

/// Prints this thread's counters to stderr when the environment variable
/// `PHPRUST_JIT_STATS` is set: for a SAPI to call when its script is done,
/// so that a measurement or a fuzzer can tell what the JIT did.
pub fn report_if_asked() {
    if std::env::var_os("PHPRUST_JIT_STATS").is_some() {
        let s = stats();
        eprintln!(
            "jit: {} tapes compiled, {} typed loops, {} failed, {} bytes of code",
            s.compiled, s.typed_loops, s.failed, s.code_bytes
        );
    }
}

/// Updates this thread's counters.
#[cfg_attr(not(feature = "jit"), allow(dead_code))]
pub(crate) fn record(update: impl FnOnce(&mut Stats)) {
    STATS.with(|s| {
        let mut stats = s.get();
        update(&mut stats);
        s.set(stats);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_is_per_thread_and_read_back() {
        let before = options();
        let set = Options {
            enabled: true,
            everything: true,
            first_tier: true,
            registers: true,
            hot_loop: 9,
        };
        configure(set);
        if forced().is_none() {
            assert_eq!(options(), set);
        }
        std::thread::spawn(|| {
            if forced().is_none() {
                assert_eq!(options(), Options::default());
            }
        })
        .join()
        .expect("thread");
        configure(before);
    }

    #[test]
    fn record_accumulates() {
        let before = stats();
        record(|s| s.compiled += 2);
        assert_eq!(stats().compiled, before.compiled + 2);
    }
}
