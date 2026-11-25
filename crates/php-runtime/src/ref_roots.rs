// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The possible-root buffer for references (`gc_check_possible_root` for
//! `zend_reference`), consumed by the VM's cycle collector.
//!
//! When a [`PhpRef`] handle is dropped while other handles remain, the
//! reference may now be part of an unreachable cycle (`$a[] = &$a;
//! unset($a);`). Its address is buffered weakly (at most once) so that the
//! collector can start from it. Nothing is recorded until the collector
//! turns recording on.

use crate::reference::{PhpRef, WeakRef};
use crate::Value;
use std::cell::{Cell, RefCell};

/// The buffered references, oldest first, held weakly. Each one has its
/// buffered bit set ([`PhpRef::is_buffered`], php-src's `GC_INFO`) for as
/// long as its entry is here, so "is it buffered already" is one load and
/// test in the reference's own header, never a lookup. The entry's weak
/// handle keeps the allocation (and so the bit and the address) until the
/// buffer lets go of it, which clears the bit first.
#[derive(Default)]
struct Buf {
    entries: Vec<WeakRef>,
    /// Dead entries are dropped when `entries` grows past this (keeps the
    /// buffer bounded while the collector does not run).
    compact_at: usize,
}

impl Buf {
    fn compact(&mut self) {
        self.entries.retain(|w| {
            let alive = w.strong_count() > 0;
            if !alive {
                w.set_buffered(false);
            }
            alive
        });
        self.compact_at = self.entries.len() * 2;
    }

    /// Empties the buffer, clearing every entry's buffered bit.
    fn drain(&mut self) -> Vec<WeakRef> {
        let entries = std::mem::take(&mut self.entries);
        for w in &entries {
            w.set_buffered(false);
        }
        entries
    }
}

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static NOTIFY: Cell<Option<fn()>> = const { Cell::new(None) };
    static BUF: RefCell<Buf> = RefCell::new(Buf::default());
}

/// Called by `PhpRef::drop` when a handle is released and others remain.
/// Costs one thread-local load while recording is off, and one more load
/// of the reference's header while it is buffered already.
#[inline]
pub(crate) fn released(r: &PhpRef) {
    if r.is_buffered() || !RECORDING.try_with(Cell::get).unwrap_or(false) {
        return;
    }
    buffer(r);
}

/// [`released`] for a reference not buffered yet: buffers it if it holds
/// an array or object (only those can close a cycle, `Z_COLLECTABLE`).
#[inline(never)]
fn buffer(r: &PhpRef) {
    if !r
        .try_borrow()
        .is_some_and(|v| matches!(*v, Value::Array(_) | Value::Object(_)))
    {
        return;
    }
    // The collector may run (and empty the buffer) before the new root is
    // added, like `gc_possible_root_when_full`.
    if let Some(f) = NOTIFY.try_with(Cell::get).ok().flatten() {
        f();
    }
    if r.is_buffered() {
        return;
    }
    let w = r.downgrade();
    let _ = BUF.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            if b.entries.len() >= b.compact_at.max(4096) {
                b.compact();
            }
            w.set_buffered(true);
            b.entries.push(w);
        }
    });
}

/// `gc_check_possible_root` for a reference a live temporary or loop
/// variable holds after a collection (`zend_gc_check_root_tmpvars`):
/// buffers it like a release of one of its handles would.
pub fn check_possible_root(r: &PhpRef) {
    released(r);
}

/// Takes the buffered references that are still alive and empties the
/// buffer.
#[must_use]
pub fn take() -> Vec<Value> {
    let entries = BUF
        .try_with(|b| {
            b.try_borrow_mut()
                .map(|mut b| b.drain())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|w| w.upgrade())
        .map(Value::Reference)
        .collect()
}

/// Number of buffered references still alive.
#[must_use]
pub fn len() -> usize {
    BUF.try_with(|b| {
        b.try_borrow().map_or(0, |b| {
            b.entries.iter().filter(|w| w.strong_count() > 0).count()
        })
    })
    .unwrap_or(0)
}

/// Buffered references set aside by [`stash`], held weakly: the roots a
/// due collection will look at, kept apart from roots buffered after it
/// became due (`gc_possible_root_when_full` collects before buffering).
#[derive(Default)]
pub struct Stash(Vec<WeakRef>);

impl Stash {
    /// Number of set-aside references still alive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.iter().filter(|w| w.strong_count() > 0).count()
    }

    /// Whether no set-aside reference is alive.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for Stash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stash({})", self.0.len())
    }
}

/// Moves every buffered reference out of the buffer, weakly.
#[must_use]
pub fn stash() -> Stash {
    let entries = BUF
        .try_with(|b| {
            b.try_borrow_mut()
                .map(|mut b| b.drain())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Stash(
        entries
            .into_iter()
            .filter(|w| w.strong_count() > 0)
            .collect(),
    )
}

/// Puts set-aside references back, ahead of those buffered since (they
/// were buffered first). References freed in the meantime are dropped,
/// and one buffered again is kept once.
pub fn unstash(s: Stash) {
    if s.0.is_empty() {
        return;
    }
    let _ = BUF.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            // Cleared here and set again as each reference is kept: the bit
            // tells a reference kept already from one seen the first time.
            let newer = b.drain();
            let mut entries = Vec::with_capacity(s.0.len() + newer.len());
            for w in s.0.into_iter().chain(newer) {
                if w.strong_count() == 0 || w.is_buffered() {
                    continue;
                }
                w.set_buffered(true);
                entries.push(w);
            }
            b.entries = entries;
            b.compact_at = b.compact_at.max(b.entries.len() * 2);
        }
    });
}

/// Turns recording on or off; returns the previous setting.
pub fn set_recording(on: bool) -> bool {
    RECORDING.try_with(|r| r.replace(on)).unwrap_or(false)
}

/// Sets the function called before a new root is buffered.
pub fn set_notify(f: Option<fn()>) {
    let _ = NOTIFY.try_with(|n| n.set(f));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_shared_releases_once() {
        set_recording(true);
        let r = PhpRef::new(Value::empty_array());
        let r2 = r.clone();
        let r3 = r.clone();
        drop(r2);
        drop(r3);
        assert_eq!(len(), 1);
        let roots = take();
        assert_eq!(roots.len(), 1);
        assert_eq!(len(), 0);
        drop(roots);
        drop(r);
        set_recording(false);
    }

    #[test]
    fn stash_sets_roots_aside_and_unstash_puts_them_first() {
        set_recording(true);
        drop(take());
        let a = PhpRef::new(Value::empty_array());
        let b = PhpRef::new(Value::empty_array());
        drop(a.clone());
        let s = stash();
        assert_eq!((s.len(), len()), (1, 0));
        drop(b.clone());
        drop(a.clone());
        assert_eq!(len(), 2);
        unstash(s);
        assert_eq!(len(), 2);
        let roots = take();
        assert!(matches!(&roots[0], Value::Reference(r) if PhpRef::ptr_eq(r, &a)));
        assert!(matches!(&roots[1], Value::Reference(r) if PhpRef::ptr_eq(r, &b)));
        drop(roots);
        set_recording(false);
    }

    /// Many references, each released several times, are buffered once
    /// each and in release order (the address index finds every one).
    #[test]
    fn many_references_are_buffered_once_in_order() {
        set_recording(true);
        drop(take());
        let refs: Vec<PhpRef> = (0..300)
            .map(|_| PhpRef::new(Value::empty_array()))
            .collect();
        for _ in 0..3 {
            for r in &refs {
                drop(r.clone());
            }
        }
        assert_eq!(len(), refs.len());
        let roots = take();
        assert_eq!(roots.len(), refs.len());
        assert!(roots
            .iter()
            .zip(&refs)
            .all(|(v, r)| matches!(v, Value::Reference(x) if PhpRef::ptr_eq(x, r))));
        drop(roots);
        set_recording(false);
    }

    /// The buffered bit follows the entry: set while buffered, cleared by
    /// `take`, so the next release buffers the reference again.
    #[test]
    fn take_clears_the_buffered_bit() {
        set_recording(true);
        drop(take());
        let r = PhpRef::new(Value::empty_array());
        assert!(!r.is_buffered());
        drop(r.clone());
        assert!(r.is_buffered());
        let roots = take();
        assert_eq!(roots.len(), 1);
        assert!(!r.is_buffered());
        drop(roots);
        // Dropping the taken root (a handle while `r` remains) buffers again.
        assert!(r.is_buffered());
        assert_eq!(len(), 1);
        drop(take());
        assert!(!r.is_buffered());
        set_recording(false);
    }

    /// A reference freed while buffered leaves a dead entry: not counted,
    /// not taken, and its allocation goes with the entry.
    #[test]
    fn reference_freed_while_buffered() {
        set_recording(true);
        drop(take());
        let a = PhpRef::new(Value::empty_array());
        let b = PhpRef::new(Value::empty_array());
        drop(a.clone());
        drop(b.clone());
        assert_eq!(len(), 2);
        drop(a);
        assert_eq!(len(), 1);
        let s = stash();
        assert_eq!((s.len(), len()), (1, 0));
        assert!(!b.is_buffered());
        unstash(s);
        assert!(b.is_buffered());
        let roots = take();
        assert_eq!(roots.len(), 1);
        assert!(matches!(&roots[0], Value::Reference(r) if PhpRef::ptr_eq(r, &b)));
        drop(roots);
        drop(take());
        drop(b);
        set_recording(false);
    }

    /// A set-aside reference buffered again meanwhile is kept once, in its
    /// first place, by `unstash`.
    #[test]
    fn unstash_keeps_a_rebuffered_reference_once() {
        set_recording(true);
        drop(take());
        let a = PhpRef::new(Value::empty_array());
        let b = PhpRef::new(Value::empty_array());
        drop(a.clone());
        let s = stash();
        drop(b.clone());
        drop(a.clone());
        drop(a.clone());
        assert_eq!(len(), 2);
        unstash(s);
        assert_eq!(len(), 2);
        assert!(a.is_buffered() && b.is_buffered());
        let roots = take();
        assert_eq!(roots.len(), 2);
        assert!(matches!(&roots[0], Value::Reference(r) if PhpRef::ptr_eq(r, &a)));
        assert!(matches!(&roots[1], Value::Reference(r) if PhpRef::ptr_eq(r, &b)));
        drop(roots);
        drop(take());
        set_recording(false);
    }

    /// Compaction drops the dead entries (clearing their bits) and keeps
    /// the live ones buffered, in order.
    #[test]
    fn compaction_keeps_live_entries_buffered() {
        set_recording(true);
        drop(take());
        let keep: Vec<PhpRef> = (0..10).map(|_| PhpRef::new(Value::empty_array())).collect();
        for r in &keep {
            drop(r.clone());
        }
        for _ in 0..5000 {
            let r = PhpRef::new(Value::empty_array());
            drop(r.clone());
        }
        assert_eq!(len(), keep.len());
        assert!(keep.iter().all(PhpRef::is_buffered));
        for r in &keep {
            drop(r.clone());
        }
        let roots = take();
        assert_eq!(roots.len(), keep.len());
        assert!(roots
            .iter()
            .zip(&keep)
            .all(|(v, r)| matches!(v, Value::Reference(x) if PhpRef::ptr_eq(x, r))));
        drop(roots);
        drop(take());
        set_recording(false);
    }

    #[test]
    fn scalars_are_not_collectable() {
        set_recording(true);
        let r = PhpRef::new(Value::Int(1));
        drop(r.clone());
        assert_eq!(len(), 0);
        assert!(!r.is_buffered());
        set_recording(false);
    }

    #[test]
    fn nothing_recorded_when_off() {
        set_recording(false);
        let r = PhpRef::new(Value::Int(1));
        drop(r.clone());
        assert_eq!(len(), 0);
    }
}
