//! Native stack management for the recursive parser.
//!
//! PHP's bison parser accepts nesting up to depth 10000 before failing with
//! `memory exhausted`. The recursive-descent parser needs native stack for
//! that, so [`crate::parse`] checks how much stack the calling thread has
//! left while it parses ([`StackPolicy::Auto`]): ordinary code is parsed on
//! the calling thread, and only if the stack runs low is the parse restarted
//! on a helper thread with a large stack.

use std::cell::Cell;

/// Where [`crate::parse`] runs the parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StackPolicy {
    /// Parse on the calling thread while it has enough stack left; restart
    /// on a helper thread with a 512 MB stack only if the code nests too
    /// deeply for the remaining stack.
    #[default]
    Auto,
    /// Always parse on the calling thread. If its stack runs low, parsing
    /// fails with `memory exhausted` (possibly earlier than PHP would).
    CurrentThread,
    /// Always parse on a helper thread with a stack of this many bytes.
    Spawn(usize),
}

/// Stack size of the helper thread used by [`StackPolicy::Auto`].
pub(crate) const HELPER_STACK_SIZE: usize = 512 * 1024 * 1024;

/// Stack kept free below the limit: enough for the deepest frames between
/// two depth checks (debug builds included).
const MARGIN: usize = 1024 * 1024;

/// Cached result of probing the current thread's stack.
#[derive(Clone, Copy)]
enum Probe {
    NotYet,
    Unknown,
    Low(usize),
}

thread_local! {
    static STACK_LOW: Cell<Probe> = const { Cell::new(Probe::NotYet) };
}

/// The approximate current stack pointer.
#[inline]
pub(crate) fn current_sp() -> usize {
    let marker = 0u8;
    std::ptr::addr_of!(marker) as usize
}

/// Tells the parser the calling thread's stack is `[low, high)`, so its
/// first parse on this thread does not probe the bounds itself (for the
/// main thread `pthread_getattr_np` reads and parses `/proc/self/maps`:
/// about 0.9 M instructions). A host that has just read the stack mapping
/// to raise `RLIMIT_STACK` passes the mapping's end and `end - limit`,
/// which is what the probe would find.
///
/// The bounds must contain the current stack pointer: otherwise they are
/// ignored, the call returns `false` and the parser probes as usual.
pub fn set_thread_stack(low: usize, high: usize) -> bool {
    let sp = current_sp();
    if sp <= low || sp >= high {
        return false;
    }
    STACK_LOW.with(|c| c.set(Probe::Low(low)));
    true
}

/// The address below which the parser must stop recursing on this thread,
/// or `None` if the thread's stack bounds are unknown.
pub(crate) fn stack_limit() -> Option<usize> {
    let low = STACK_LOW.with(|c| {
        let probe = match c.get() {
            Probe::NotYet => {
                let p = probe_stack_low().map_or(Probe::Unknown, Probe::Low);
                c.set(p);
                p
            }
            p => p,
        };
        match probe {
            Probe::Low(low) => Some(low),
            Probe::NotYet | Probe::Unknown => None,
        }
    })?;
    low.checked_add(MARGIN)
}

/// Lowest address of the current thread's stack.
#[cfg(target_os = "linux")]
fn probe_stack_low() -> Option<usize> {
    let mut attr = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
    let mut addr: *mut libc::c_void = std::ptr::null_mut();
    let mut size: libc::size_t = 0;
    // SAFETY: `pthread_getattr_np` initialises `attr` for the calling
    // thread when it returns 0; only then is it read (by
    // `pthread_attr_getstack`, with valid out-pointers) and destroyed
    // exactly once. No pointer escapes the block.
    let ok = unsafe {
        if libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) != 0 {
            return None;
        }
        let r = libc::pthread_attr_getstack(attr.as_ptr(), &raw mut addr, &raw mut size);
        libc::pthread_attr_destroy(attr.as_mut_ptr());
        r == 0
    };
    if !ok || addr.is_null() || size == 0 {
        return None;
    }
    let low = addr as usize;
    // Sanity check: we must currently be inside [low, low + size).
    let sp = current_sp();
    (sp > low && sp - low < size).then_some(low)
}

/// Lowest address of the current thread's stack (unknown on this OS).
#[cfg(not(target_os = "linux"))]
fn probe_stack_low() -> Option<usize> {
    None
}

/// Whether the stack is below `limit` (i.e. nearly exhausted).
#[inline]
pub(crate) fn exhausted(limit: Option<usize>) -> bool {
    limit.is_some_and(|l| current_sp() < l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn probe_finds_current_stack() {
        let limit = stack_limit().expect("stack bounds on linux");
        assert!(current_sp() > limit);
        assert!(!exhausted(Some(limit)));
        let h = std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(|| {
                let l = stack_limit().unwrap();
                current_sp() - l
            })
            .unwrap();
        let room = h.join().unwrap();
        assert!(room > 2 * 1024 * 1024 && room < 4 * 1024 * 1024, "{room}");
    }

    #[test]
    fn given_bounds_replace_the_probe_on_their_thread() {
        const MIB: usize = 1024 * 1024;
        let h = std::thread::Builder::new()
            .stack_size(4 * MIB)
            .spawn(|| {
                // `set_thread_stack` checks its own frame's stack pointer,
                // a little below this one: the bounds below keep a margin
                // far larger than the frames in between.
                let sp = current_sp();
                let not_probed = || STACK_LOW.with(|c| matches!(c.get(), Probe::NotYet));
                // Bounds that do not contain the stack pointer (wholly
                // above it, wholly below it, or empty) are ignored.
                assert!(!set_thread_stack(sp + MIB, sp + 2 * MIB));
                assert!(!set_thread_stack(sp - 2 * MIB, sp - MIB));
                assert!(!set_thread_stack(sp + MIB, sp - MIB));
                assert!(not_probed());
                // Given bounds are used instead of the probe.
                let low = sp - 3 * MIB;
                assert!(set_thread_stack(low, sp + MIB));
                assert_eq!(stack_limit(), Some(low + MARGIN));
                assert!(!exhausted(stack_limit()));
                // A stack said to end less than MARGIN below the current
                // frame counts as nearly exhausted.
                let low = sp - MARGIN / 2;
                assert!(set_thread_stack(low, sp + MIB));
                assert_eq!(stack_limit(), Some(low + MARGIN));
                assert!(exhausted(stack_limit()));
            })
            .unwrap();
        h.join().unwrap();
    }
}
