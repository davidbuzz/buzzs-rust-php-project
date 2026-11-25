// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The possible-root buffers for references and arrays
//! (`gc_check_possible_root`, Zend/zend_gc.c), consumed by the VM's cycle
//! collector.
//!
//! When a [`PhpRef`] handle is dropped while other handles remain, the
//! reference may now be part of an unreachable cycle (`$a[] = &$a;
//! unset($a);`). Its address is buffered weakly (at most once) so that the
//! collector can start from it. Nothing is recorded until the collector
//! turns recording on.
//!
//! Arrays are possible roots too, as every non-persistent `zend_array` is
//! collectable in php-src: the VM's release sites hand an array released to
//! a non-zero count to [`array_released`]. The buffer cannot hold a weak
//! handle of an array (a weak count makes `Rc::get_mut` fail, so a write
//! to the buffered array would copy it: copy-on-write would change), nor a
//! strong one (a count). It holds the table's address, and the table's
//! header holds the slot ([`HashTable::gc_root_mark`], php-src's
//! `GC_INFO`). The entry stays valid because the table leaves the buffer
//! when it is freed (`HashTable::drop` → [`array_freed`],
//! `GC_REMOVE_FROM_BUFFER`) or its contents leave the allocation; the slot
//! is then free for the next root, which takes it without a threshold
//! check, as php-src's unused list hands it out.

use crate::array::{HashTable, ZArray};
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

    /// Empties the buffer, clearing every entry's buffered bit; the slots
    /// of the dead entries are gone with them.
    fn drain(&mut self) -> Vec<WeakRef> {
        forget_freed_slots();
        let entries = std::mem::take(&mut self.entries);
        for w in &entries {
            w.set_buffered(false);
        }
        entries
    }
}

/// The buffer lets go of its entries at thread exit: their bits go first.
impl Drop for Buf {
    fn drop(&mut self) {
        for w in &self.entries {
            w.set_buffered(false);
        }
    }
}

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static NOTIFY: Cell<Option<fn()>> = const { Cell::new(None) };
    static BUF: RefCell<Buf> = RefCell::new(Buf::default());
    /// Slots of references freed while buffered since the buffer was last
    /// emptied: php-src takes a freed root out of the buffer
    /// (`GC_REMOVE_FROM_BUFFER`) and the next root reuses its slot
    /// (`GC_FETCH_UNUSED`), which hands out no new slot, so it neither
    /// moves nor checks the threshold. Each one lets the next reference
    /// be buffered without [`NOTIFY`].
    static FREED: Cell<usize> = const { Cell::new(0) };
    static ARRAYS: RefCell<ArrBuf> = RefCell::new(ArrBuf::default());
    /// Runs a buffer's growth outside `memory_get_usage()`, as php-src's
    /// root buffer is allocated persistently (set by the VM; without it
    /// the growth is counted).
    static PERSISTENT: Cell<Option<fn(&mut dyn FnMut())>> = const { Cell::new(None) };
}

/// Sets the function a root buffer's growth runs in (the VM's
/// `php_memlimit::persistent`, which leaves it out of
/// `memory_get_usage()`, as php-src allocates its root buffer with
/// `pemalloc`).
pub fn set_persistent_scope(f: Option<fn(&mut dyn FnMut())>) {
    let _ = PERSISTENT.try_with(|p| p.set(f));
}

/// `v.push(x)`, growing `v` in the [`PERSISTENT`] scope.
#[inline]
fn push_persistent<T>(v: &mut Vec<T>, x: T) {
    if v.len() == v.capacity() {
        reserve_persistent(v);
    }
    v.push(x);
}

/// Grows `v` (amortized doubling) in the [`PERSISTENT`] scope.
#[cold]
#[inline(never)]
fn reserve_persistent<T>(v: &mut Vec<T>) {
    match PERSISTENT.try_with(Cell::get).ok().flatten() {
        Some(scope) => scope(&mut || v.reserve(1)),
        None => v.reserve(1),
    }
}

/// One slot of the array root buffer.
#[derive(Clone, Copy)]
struct ArrSlot {
    /// The buffered table ([`ZArray::table_ptr`]); null while unused.
    table: *const HashTable,
    /// 0 while the entry is in the buffer; otherwise the [`Stash`] it was
    /// set aside in.
    stash: u32,
}

impl ArrSlot {
    const UNUSED: ArrSlot = ArrSlot {
        table: std::ptr::null(),
        stash: 0,
    };
}

/// The array root buffer: slot `i` is the one whose table's mark is
/// `i + 1`. Freed slots are reused last-in first-out (`GC_FETCH_UNUSED`),
/// and a collection takes the entries in slot order.
#[derive(Default)]
struct ArrBuf {
    slots: Vec<ArrSlot>,
    /// Unused slots below `slots.len()`, the last freed on top.
    unused: Vec<u32>,
    /// Entries in the buffer (not set aside).
    live: usize,
    /// Occupied slots (in the buffer or set aside).
    used: usize,
    /// The last [`Stash`] number handed out.
    last_stash: u32,
}

impl ArrBuf {
    /// Buffers the table at `table`; its mark, or `None` when the buffer
    /// cannot number another slot.
    fn insert(&mut self, table: *const HashTable) -> Option<u32> {
        let i = match self.unused.pop() {
            Some(i) => i,
            None => {
                let i = u32::try_from(self.slots.len()).ok()?;
                if i >= HashTable::GC_IMMUTABLE - 1 {
                    return None;
                }
                push_persistent(&mut self.slots, ArrSlot::UNUSED);
                i
            }
        };
        self.slots[i as usize] = ArrSlot { table, stash: 0 };
        self.live += 1;
        self.used += 1;
        Some(i + 1)
    }

    /// Frees slot `i` (its entry was taken or its table freed); whether
    /// the entry was in the buffer (not set aside).
    fn release(&mut self, i: usize) -> bool {
        let in_buffer = self.slots[i].stash == 0;
        self.slots[i] = ArrSlot::UNUSED;
        self.used -= 1;
        if in_buffer {
            self.live -= 1;
        }
        if self.used == 0 {
            // Nothing left: the slots are numbered from the start again
            // (php-src compacts its buffer when a run empties it).
            self.slots.clear();
            self.unused.clear();
        } else {
            #[allow(clippy::cast_possible_truncation)] // slots are numbered by u32
            push_persistent(&mut self.unused, i as u32);
        }
        in_buffer
    }
}

/// `gc_check_possible_root` of an array one of whose handles is being
/// released while others remain (the VM calls it at the release sites
/// that check, never at the `_nogc` ones): buffers it unless it is
/// buffered already or immutable (its mark, tested by the caller too) or
/// nothing is recorded.
#[inline(never)]
pub fn array_released(a: &ZArray) {
    if a.gc_root_mark() != 0 || !RECORDING.try_with(Cell::get).unwrap_or(false) {
        return;
    }
    // As for a reference: a slot freed by a root that died while buffered
    // is reused; otherwise the collector may run (and empty the buffer)
    // before the new root is added (`gc_possible_root_when_full`).
    if !reuse_freed_slot() {
        if let Some(f) = NOTIFY.try_with(Cell::get).ok().flatten() {
            f();
        }
        if a.gc_root_mark() != 0 {
            return;
        }
    }
    let table = a.table_ptr();
    let _ = ARRAYS.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            if let Some(mark) = b.insert(table) {
                a.set_gc_root_mark(mark);
            }
        }
    });
}

/// `GC_REMOVE_FROM_BUFFER` of the table `t`, whose mark is `mark` (not
/// 0): its slot is freed (the next root reuses it), unless the mark is
/// [`HashTable::GC_IMMUTABLE`] or the slot holds another table (`t` is a
/// copy of a buffered table's bits). Called as `t` is dropped or its
/// contents leave the allocation; the caller clears the mark if `t` lives
/// on. Nothing here drops a value.
#[cold]
#[inline(never)]
pub(crate) fn array_freed(t: &HashTable, mark: u32) {
    if mark == HashTable::GC_IMMUTABLE {
        return;
    }
    let p: *const HashTable = t;
    let freed = ARRAYS
        .try_with(|b| {
            let Ok(mut b) = b.try_borrow_mut() else {
                return false;
            };
            let i = (mark - 1) as usize;
            if !b.slots.get(i).is_some_and(|s| std::ptr::eq(s.table, p)) {
                return false;
            }
            b.release(i)
        })
        .unwrap_or(false);
    if freed {
        buffered_freed();
    }
}

/// `Rc::make_mut` moved the buffered table at `old` to `new` (whose mark
/// is `mark`): its entry follows it.
#[cold]
#[inline(never)]
pub(crate) fn array_moved(old: *const HashTable, new: &HashTable, mark: u32) {
    if mark == HashTable::GC_IMMUTABLE {
        return;
    }
    let _ = ARRAYS.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            if let Some(s) = b.slots.get_mut((mark - 1) as usize) {
                if std::ptr::eq(s.table, old) {
                    s.table = new;
                }
            }
        }
    });
}

/// Takes the arrays in the buffer (not those set aside), in slot order,
/// as strong handles; their marks are cleared and their slots freed.
fn take_arrays() -> Vec<ZArray> {
    ARRAYS
        .try_with(|b| {
            let Ok(mut b) = b.try_borrow_mut() else {
                return Vec::new();
            };
            let mut out = Vec::with_capacity(b.live);
            // (`release` empties the slots when it frees the last one.)
            for i in 0..b.slots.len() {
                let Some(&s) = b.slots.get(i) else {
                    break;
                };
                if s.table.is_null() || s.stash != 0 {
                    continue;
                }
                // SAFETY: an occupied slot's table is alive (see
                // `ZArray::from_buffered`): it leaves the buffer before it
                // is freed or moved. Nothing is dropped while the buffer is
                // borrowed, so none leaves meanwhile.
                let a = unsafe { ZArray::from_buffered(s.table) };
                a.set_gc_root_mark(0);
                out.push(a);
                b.release(i);
            }
            out
        })
        .unwrap_or_default()
}

/// Forgets every buffered array, also those set aside, clearing their
/// marks (the request's end: `gc_reset`). Takes no handle.
pub fn clear_arrays() {
    let _ = ARRAYS.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            for s in &b.slots {
                if !s.table.is_null() {
                    // SAFETY: an occupied slot's table is alive (see
                    // `ZArray::from_buffered`); only its mark, a `Cell`, is
                    // written.
                    unsafe { (*s.table).set_gc_root_mark(0) };
                }
            }
            let last_stash = b.last_stash;
            *b = ArrBuf {
                last_stash,
                ..ArrBuf::default()
            };
        }
    });
}

/// Number of arrays in the buffer (not set aside).
fn arrays_len() -> usize {
    ARRAYS
        .try_with(|b| b.try_borrow().map_or(0, |b| b.live))
        .unwrap_or(0)
}

/// Number of arrays set aside in stash `n`.
fn stashed_arrays(n: u32) -> usize {
    ARRAYS
        .try_with(|b| {
            b.try_borrow().map_or(0, |b| {
                b.slots
                    .iter()
                    .filter(|s| !s.table.is_null() && s.stash == n)
                    .count()
            })
        })
        .unwrap_or(0)
}

/// Sets every array in the buffer aside in a new stash; its number.
fn stash_arrays() -> u32 {
    ARRAYS
        .try_with(|b| {
            let Ok(mut b) = b.try_borrow_mut() else {
                return 0;
            };
            if b.live == 0 {
                return 0;
            }
            b.last_stash = b.last_stash.wrapping_add(1).max(1);
            let n = b.last_stash;
            for s in &mut b.slots {
                if !s.table.is_null() && s.stash == 0 {
                    s.stash = n;
                }
            }
            b.live = 0;
            n
        })
        .unwrap_or(0)
}

/// Puts the arrays set aside in stash `n` back in the buffer.
fn unstash_arrays(n: u32) {
    if n == 0 {
        return;
    }
    let _ = ARRAYS.try_with(|b| {
        if let Ok(mut b) = b.try_borrow_mut() {
            let mut back = 0;
            for s in &mut b.slots {
                if !s.table.is_null() && s.stash == n {
                    s.stash = 0;
                    back += 1;
                }
            }
            b.live += back;
        }
    });
}

/// A buffered reference was freed (from `release_last`, once per
/// reference): its slot is free for the next root.
#[cold]
pub(crate) fn buffered_freed() {
    let _ = FREED.try_with(|n| n.set(n.get() + 1));
}

/// Takes one freed slot, if there is one (see [`FREED`]).
fn reuse_freed_slot() -> bool {
    FREED
        .try_with(|n| {
            let k = n.get();
            if k > 0 {
                n.set(k - 1);
            }
            k > 0
        })
        .unwrap_or(false)
}

/// The buffer was emptied: no freed slot is left to reuse.
fn forget_freed_slots() {
    let _ = FREED.try_with(|n| n.set(0));
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
    // A slot freed by a reference that died while buffered is reused:
    // nothing new is handed out. Otherwise the collector may run (and
    // empty the buffer) before the new root is added, like
    // `gc_possible_root_when_full`.
    if !reuse_freed_slot() {
        if let Some(f) = NOTIFY.try_with(Cell::get).ok().flatten() {
            f();
        }
        if r.is_buffered() {
            return;
        }
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

/// Takes the buffered references that are still alive, then the buffered
/// arrays, and empties the buffers (arrays set aside by [`stash`] stay).
#[must_use]
pub fn take() -> Vec<Value> {
    let entries = BUF
        .try_with(|b| {
            b.try_borrow_mut()
                .map(|mut b| b.drain())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let mut out: Vec<Value> = entries
        .into_iter()
        .filter_map(|w| w.upgrade())
        .map(Value::Reference)
        .collect();
    out.extend(take_arrays().into_iter().map(Value::Array));
    out
}

/// Number of buffered references still alive and buffered arrays.
#[must_use]
pub fn len() -> usize {
    BUF.try_with(|b| {
        b.try_borrow().map_or(0, |b| {
            b.entries.iter().filter(|w| w.strong_count() > 0).count()
        })
    })
    .unwrap_or(0)
        + arrays_len()
}

/// Buffered references set aside by [`stash`], held weakly: the roots a
/// due collection will look at, kept apart from roots buffered after it
/// became due (`gc_possible_root_when_full` collects before buffering).
/// Arrays set aside stay in their slots, numbered with the stash
/// (`arrays`, 0 for none), so that freeing one still frees its slot.
#[derive(Default)]
pub struct Stash(Vec<WeakRef>, u32);

impl Stash {
    /// Number of set-aside references still alive and arrays.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.iter().filter(|w| w.strong_count() > 0).count()
            + if self.1 == 0 {
                0
            } else {
                stashed_arrays(self.1)
            }
    }

    /// Whether no set-aside reference is alive.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for Stash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stash({}, {})", self.0.len(), self.1)
    }
}

/// Moves every buffered reference out of the buffer, weakly, and sets the
/// buffered arrays aside.
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
        stash_arrays(),
    )
}

/// Puts set-aside references back, ahead of those buffered since (they
/// were buffered first). References freed in the meantime are dropped,
/// and one buffered again is kept once.
pub fn unstash(s: Stash) {
    // The collector counts the buffer afresh after this ([`len`]): a slot
    // freed before is no longer one to reuse.
    forget_freed_slots();
    unstash_arrays(s.1);
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
        // Taken and kept: the bit is clear until the taken handle goes.
        let roots = take();
        assert!(!r.is_buffered());
        set_recording(false);
        drop(roots);
        assert!(!r.is_buffered());
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

    thread_local! {
        static NOTIFIED: Cell<usize> = const { Cell::new(0) };
    }

    fn count_notify() {
        NOTIFIED.with(|n| n.set(n.get() + 1));
    }

    /// A reference freed while buffered frees its slot: the next reference
    /// buffered takes it without a new slot (no notify, so no threshold
    /// check), as php-src's root buffer reuses a removed root's slot. The
    /// buffer emptied, no freed slot is left.
    #[test]
    fn a_freed_buffered_reference_frees_its_slot() {
        set_recording(true);
        drop(take());
        set_notify(Some(count_notify));
        NOTIFIED.with(|n| n.set(0));
        let a = PhpRef::new(Value::empty_array());
        drop(a.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 1);
        drop(a);
        assert_eq!(len(), 0);
        // Reuses the slot of `a`.
        let b = PhpRef::new(Value::empty_array());
        drop(b.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 1);
        assert_eq!(len(), 1);
        // No freed slot left: a new one.
        let c = PhpRef::new(Value::empty_array());
        drop(c.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 2);
        // Freed slots go with the buffer.
        drop(c);
        // (The taken handle of `b` is kept: releasing it would buffer `b`
        // again, with a notify of its own.)
        let taken = take();
        let d = PhpRef::new(Value::empty_array());
        drop(d.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 3);
        // A reference never buffered frees no slot.
        let e = PhpRef::new(Value::empty_array());
        drop(e);
        let f = PhpRef::new(Value::empty_array());
        drop(f.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 4);
        set_notify(None);
        set_recording(false);
        drop(taken);
        set_notify(None);
        set_recording(false);
        drop(take());
        drop((b, d, f));
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
        let a = ZArray::from_values([Value::Int(1)]);
        array_released(&a.clone());
        assert_eq!((len(), a.gc_root_mark()), (0, 0));
    }

    fn list(n: i64) -> ZArray {
        ZArray::from_values((0..n).map(Value::Int))
    }

    /// Releases one of `a`'s handles while `a` remains, as the VM's
    /// release sites do (`$b = $a; unset($b);`).
    fn release_copy(a: &ZArray) {
        let b = a.clone();
        array_released(&b);
        drop(b);
    }

    /// An array released while still held is buffered once (its mark
    /// set), and a freed buffered array leaves the buffer
    /// (`GC_REMOVE_FROM_BUFFER`): the count goes back.
    #[test]
    fn arrays_are_buffered_once_and_leave_when_freed() {
        set_recording(true);
        drop(take());
        let a = list(3);
        assert_eq!(a.gc_root_mark(), 0);
        release_copy(&a);
        assert_eq!(len(), 1);
        assert_ne!(a.gc_root_mark(), 0);
        release_copy(&a);
        assert_eq!(len(), 1);
        let b = list(2);
        release_copy(&b);
        assert_eq!(len(), 2);
        drop(a);
        assert_eq!(len(), 1);
        drop(b);
        assert_eq!(len(), 0);
        set_recording(false);
    }

    /// The shared empty array is immutable (`zend_empty_array`): never
    /// buffered, and a write separates an unmarked copy.
    #[test]
    fn the_empty_array_is_never_buffered() {
        set_recording(true);
        drop(take());
        let e = ZArray::new();
        assert_eq!(e.gc_root_mark(), HashTable::GC_IMMUTABLE);
        release_copy(&e);
        assert_eq!(len(), 0);
        let mut w = e.clone();
        let _ = w.make_mut().push(Value::Int(1));
        assert_eq!(w.gc_root_mark(), 0);
        release_copy(&w);
        assert_eq!(len(), 1);
        drop(w);
        assert_eq!(len(), 0);
        set_recording(false);
    }

    /// The buffer holds no count: a buffered array whose other handles
    /// went is written in place (copy-on-write unchanged) and keeps its
    /// mark; a separated copy is unmarked.
    #[test]
    fn buffering_leaves_copy_on_write_alone() {
        set_recording(true);
        drop(take());
        let mut a = list(2);
        release_copy(&a);
        let mark = a.gc_root_mark();
        assert_eq!(a.refcount(), 1);
        let p = a.addr();
        assert!(a.get_mut_unshared().is_some());
        let _ = a.make_mut().push(Value::Int(9));
        assert_eq!((a.addr(), a.gc_root_mark()), (p, mark));
        let mut c = a.clone();
        let _ = c.make_mut().push(Value::Int(10));
        assert_eq!(c.gc_root_mark(), 0);
        assert_eq!(len(), 1);
        // A rebuild in place (`array_unshift`) is still the same array.
        a.make_mut().unshift(vec![Value::Int(-1)]);
        assert_eq!((a.gc_root_mark(), len()), (mark, 1));
        drop(c);
        drop(a);
        assert_eq!(len(), 0);
        set_recording(false);
    }

    /// `take` hands the buffered arrays over in slot order, clears their
    /// marks, and holds no count once its handles are dropped.
    #[test]
    fn take_returns_buffered_arrays_and_clears_their_marks() {
        set_recording(true);
        drop(take());
        let a = list(1);
        let b = list(2);
        release_copy(&a);
        release_copy(&b);
        let roots = take();
        assert_eq!(roots.len(), 2);
        assert!(matches!(&roots[0], Value::Array(x) if ZArray::ptr_eq(x, &a)));
        assert!(matches!(&roots[1], Value::Array(x) if ZArray::ptr_eq(x, &b)));
        assert_eq!((a.gc_root_mark(), b.gc_root_mark(), len()), (0, 0, 0));
        assert_eq!(a.refcount(), 2);
        drop(roots);
        assert_eq!((a.refcount(), b.refcount()), (1, 1));
        // Buffered again by the next release.
        release_copy(&a);
        assert_eq!(len(), 1);
        drop(take());
        set_recording(false);
    }

    /// A table taken out of its allocation leaves the buffer first.
    #[test]
    fn into_table_takes_a_buffered_array_out() {
        set_recording(true);
        drop(take());
        let a = list(2);
        release_copy(&a);
        assert_eq!(len(), 1);
        let t = a.into_table();
        assert_eq!((t.gc_root_mark(), len()), (0, 0));
        drop(t);
        assert!(take().is_empty());
        set_recording(false);
    }

    /// Arrays set aside by `stash` are not taken, still leave when freed,
    /// and come back with `unstash`.
    #[test]
    fn stash_sets_arrays_aside() {
        set_recording(true);
        drop(take());
        let a = list(1);
        let b = list(1);
        release_copy(&a);
        release_copy(&b);
        let s = stash();
        assert_eq!((s.len(), len()), (2, 0));
        // Still buffered: a release changes nothing.
        release_copy(&a);
        assert_eq!(len(), 0);
        let c = list(1);
        release_copy(&c);
        assert_eq!(len(), 1);
        drop(b);
        assert_eq!(s.len(), 1);
        assert_eq!(take().len(), 1);
        unstash(s);
        assert_eq!(len(), 1);
        let roots = take();
        assert!(matches!(&roots[0], Value::Array(x) if ZArray::ptr_eq(x, &a)));
        drop(roots);
        drop((a, c));
        set_recording(false);
    }

    /// A freed buffered array frees its slot: the next array (or
    /// reference) buffered takes it without a notify, as for references.
    #[test]
    fn a_freed_buffered_array_frees_its_slot() {
        set_recording(true);
        drop(take());
        set_notify(Some(count_notify));
        NOTIFIED.with(|n| n.set(0));
        let a = list(1);
        release_copy(&a);
        assert_eq!(NOTIFIED.with(Cell::get), 1);
        drop(a);
        let b = list(1);
        release_copy(&b);
        assert_eq!(NOTIFIED.with(Cell::get), 1);
        let r = PhpRef::new(Value::empty_array());
        drop(r.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 2);
        drop(b);
        let r2 = PhpRef::new(Value::empty_array());
        drop(r2.clone());
        assert_eq!(NOTIFIED.with(Cell::get), 2);
        assert_eq!(len(), 2);
        set_notify(None);
        set_recording(false);
        drop(take());
        drop((r, r2));
    }

    /// `clear_arrays` forgets everything, also what was set aside, and
    /// clears the marks.
    #[test]
    fn clear_arrays_forgets_every_array() {
        set_recording(true);
        drop(take());
        let a = list(1);
        let b = list(1);
        release_copy(&a);
        let s = stash();
        release_copy(&b);
        clear_arrays();
        assert_eq!(
            (a.gc_root_mark(), b.gc_root_mark(), len(), s.len()),
            (0, 0, 0, 0)
        );
        unstash(s);
        assert_eq!(len(), 0);
        release_copy(&a);
        assert_eq!(len(), 1);
        drop(a);
        assert_eq!(len(), 0);
        drop(b);
        set_recording(false);
    }
}
