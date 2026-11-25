// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The VM's stack on the main thread, shared by `php`, `php-cgi` and
//! `php-fpm` (moved from the `php` binary's `main.rs`).

use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;

/// Stack the VM needs for deep recursion in user code.
pub(crate) const VM_STACK: u64 = 256 << 20;

/// Makes sure the main thread may grow its stack to [`VM_STACK`], so the
/// VM runs on the main thread: process-directed signals then reach it
/// (pcntl), the process stays single-threaded (`unshare()`, `fork()`
/// children exit with the script's status, php-fpm's and php-cgi's
/// workers fork from a single-threaded master) — as in PHP.
///
/// A too small `RLIMIT_STACK` is raised. Linux checks the limit each
/// time the stack grows, so raising it is enough when the addresses
/// below the stack are still free. The kernel only keeps the old limit's
/// worth of room free when it loads the program, but with address-space
/// randomisation the gap is almost always much larger (see
/// [`main_stack_room`]). If the room is too small, the binary is
/// re-executed once. That costs a second dynamic link and runtime
/// start-up, about a fifth of `php -r 'echo 1;'`. The raised limit is
/// inherited, which ends the recursion, and the kernel then reserves room
/// for it. Returns false when neither works; the VM then runs on a
/// dedicated thread with a large stack instead.
pub(crate) fn ensure(argv: &[Vec<u8>]) -> bool {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the struct we own.
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &raw mut rl) } != 0 {
        return false;
    }
    if rl.rlim_cur == libc::RLIM_INFINITY || rl.rlim_cur >= VM_STACK {
        return true;
    }
    if rl.rlim_max != libc::RLIM_INFINITY && rl.rlim_max < VM_STACK {
        return false;
    }
    let raised = libc::rlimit {
        rlim_cur: VM_STACK,
        rlim_max: rl.rlim_max,
    };
    // SAFETY: setrlimit only reads the struct we pass.
    if unsafe { libc::setrlimit(libc::RLIMIT_STACK, &raw const raised) } != 0 {
        return false;
    }
    let sp = approx_stack_pointer();
    let roomy = std::fs::read("/proc/self/maps")
        .ok()
        .and_then(|maps| main_stack_room(&maps, sp))
        .filter(|&(_, room)| room >= VM_STACK + STACK_GUARD_ALLOWANCE);
    if let Some((top, _)) = roomy {
        hand_stack_to_parser(top);
        return true;
    }
    let mut cmd = std::process::Command::new("/proc/self/exe");
    if let Some(a0) = argv.first() {
        cmd.arg0(std::ffi::OsStr::from_bytes(a0));
    }
    cmd.args(argv.iter().skip(1).map(|a| std::ffi::OsStr::from_bytes(a)));
    // Only returns on failure.
    let _ = cmd.exec();
    // SAFETY: as above; restores the original limit.
    unsafe {
        libc::setrlimit(libc::RLIMIT_STACK, &raw const rl);
    }
    false
}

/// Room kept free below a stack grown to [`VM_STACK`]. The kernel will
/// not grow a stack to within `stack_guard_gap` (1 MiB by default) of the
/// mapping below it.
const STACK_GUARD_ALLOWANCE: u64 = 16 << 20;

/// An address inside the calling thread's stack.
#[inline(never)]
fn approx_stack_pointer() -> u64 {
    let marker = 0u8;
    let addr = std::hint::black_box(std::ptr::addr_of!(marker)) as usize;
    u64::try_from(addr).unwrap_or(0)
}

/// Hands the main stack's bounds to the parser, which would otherwise probe
/// them itself on the first parse (`pthread_getattr_np` reads and parses
/// `/proc/self/maps` again: about 0.9 M instructions per `php` run). `top`
/// is the end of the stack mapping; with the limit raised to [`VM_STACK`]
/// the stack may grow down to `top - VM_STACK`, which is what the probe
/// computes from the limit.
fn hand_stack_to_parser(top: u64) {
    if let (Ok(low), Ok(high)) = (
        usize::try_from(top.saturating_sub(VM_STACK)),
        usize::try_from(top),
    ) {
        // False only when the bounds do not contain the stack pointer,
        // which the mapping that contains it rules out.
        let _ = php_parser::set_thread_stack(low, high);
    }
}

/// The top of the stack mapping that contains `sp` in the
/// `/proc/self/maps` listing `maps`, and how far that stack may grow
/// down: from its top to the end of the next lower mapping. `None` when
/// no mapping contains `sp` or the listing does not parse.
///
/// The kernel places shared libraries and anonymous mappings below
/// `mmap_base`. When it loads the program, it keeps only the stack limit
/// of that moment (at least 128 MiB) plus the stack's random offset free
/// below `mmap_base`, so this is checked rather than assumed. With
/// address-space randomisation `mmap_base` itself sits a random amount of
/// up to terabytes lower, so the room is almost always far larger than
/// [`VM_STACK`].
fn main_stack_room(maps: &[u8], sp: u64) -> Option<(u64, u64)> {
    let (top, below) = stack_mapping(maps, sp)?;
    Some((top, top.checked_sub(below)?))
}

/// The stack mapping that contains `sp` in the `/proc/self/maps` listing
/// `maps`: its end, and the end of the next lower mapping (0 without one).
/// `None` when no mapping contains `sp` or the listing does not parse.
///
/// The listing is read from its end: the stack is the last of the
/// process's own mappings, followed only by `[vvar]`, `[vdso]` and
/// `[vsyscall]`, so a few lines are parsed instead of the four hundred a
/// `php` process has.
fn stack_mapping(maps: &[u8], sp: u64) -> Option<(u64, u64)> {
    let mut ranges = maps
        .rsplit(|&c| c == b'\n')
        .map(|line| line.split(|&c| c == b' ').next().unwrap_or_default())
        .filter(|range| !range.is_empty());
    let bounds = |range: &[u8]| -> Option<(u64, u64)> {
        let dash = range.iter().position(|&c| c == b'-')?;
        Some((parse_hex(&range[..dash])?, parse_hex(&range[dash + 1..])?))
    };
    loop {
        let (start, end) = bounds(ranges.next()?)?;
        if start <= sp && sp < end {
            let below = match ranges.next() {
                Some(range) => bounds(range)?.1,
                None => 0,
            };
            return Some((end, below));
        }
    }
}

/// A hexadecimal number without prefix, as `/proc/self/maps` prints
/// addresses.
fn parse_hex(digits: &[u8]) -> Option<u64> {
    u64::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPS: &[u8] = b"\
5a0000000000-5a0000001000 r--p 00000000 08:01 1234 /usr/bin/php
7f0000000000-7f0000010000 r-xp 00000000 08:01 99 /usr/lib/libc.so.6
7f0000010000-7f0000020000 rw-p 00000000 00:00 0
7ffff0000000-7ffff0021000 rw-p 00000000 00:00 0                          [stack]
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0                  [vsyscall]
";

    #[test]
    fn stack_room_reaches_the_next_lower_mapping() {
        let (top, room) = main_stack_room(MAPS, 0x7fff_f001_0000).unwrap();
        assert_eq!(top, 0x7fff_f002_1000);
        assert_eq!(room, 0x7fff_f002_1000 - 0x7f00_0002_0000);
        assert!(room >= VM_STACK + STACK_GUARD_ALLOWANCE);
    }

    #[test]
    fn stack_room_is_small_when_a_mapping_sits_close_below() {
        let maps = b"\
7ffff0000000-7ffff0001000 rw-p 00000000 00:00 0
7ffff8000000-7ffff8021000 rw-p 00000000 00:00 0 [stack]
";
        let (top, room) = main_stack_room(maps, 0x7fff_f801_0000).unwrap();
        assert_eq!(top, 0x7fff_f802_1000);
        assert_eq!(room, 0x7fff_f802_1000 - 0x7fff_f000_1000);
        assert!(room < VM_STACK + STACK_GUARD_ALLOWANCE);
    }

    #[test]
    fn stack_room_needs_a_mapping_that_contains_sp() {
        assert_eq!(main_stack_room(MAPS, 0x1000), None);
        assert_eq!(main_stack_room(b"garbage line\n", 0x1000), None);
        assert_eq!(main_stack_room(b"", 0x1000), None);
    }

    #[test]
    fn stack_mapping_reports_the_top_and_the_mapping_below() {
        assert_eq!(
            stack_mapping(MAPS, 0x7fff_f001_0000),
            Some((0x7fff_f002_1000, 0x7f00_0002_0000))
        );
        // The lowest mapping has nothing below it.
        assert_eq!(
            stack_mapping(MAPS, 0x5a00_0000_0800),
            Some((0x5a00_0000_1000, 0))
        );
        // Without a trailing newline, and with the stack line last.
        let maps = b"7f0000000000-7f0000010000 r-xp 00000000 08:01 99 /usr/lib/libc.so.6\n\
7ffff0000000-7ffff0021000 rw-p 00000000 00:00 0 [stack]";
        assert_eq!(
            stack_mapping(maps, 0x7fff_f000_0000),
            Some((0x7fff_f002_1000, 0x7f00_0001_0000))
        );
        assert_eq!(stack_mapping(maps, 0x7fff_f002_1000), None);
        assert_eq!(
            stack_mapping(b"7ffff0000000+7ffff0021000 rw-p\n", 0x7fff_f000_0000),
            None
        );
    }

    #[test]
    fn hex_addresses_parse() {
        assert_eq!(parse_hex(b"0"), Some(0));
        assert_eq!(parse_hex(b"7ffff0021000"), Some(0x7fff_f002_1000));
        assert_eq!(parse_hex(b"ffffffffff600000"), Some(0xffff_ffff_ff60_0000));
        assert_eq!(parse_hex(b"ABCdef"), Some(0xabc_def));
        assert_eq!(parse_hex(b""), None);
        assert_eq!(parse_hex(b"0x10"), None);
        assert_eq!(parse_hex(b"12g"), None);
        assert_eq!(parse_hex(b"10000000000000000"), None);
    }

    #[test]
    fn stack_room_of_this_process_parses() {
        let maps = std::fs::read("/proc/self/maps").unwrap();
        // Test threads run on their own stacks, which are mappings too.
        assert!(main_stack_room(&maps, approx_stack_pointer()).is_some());
    }
}
