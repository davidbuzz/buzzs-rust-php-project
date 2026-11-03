//! `SplFileInfo` family streams as PHP resources, and the VM lent to the
//! stream layer while SPL file methods run.
//!
//! PHP registers the stream of an `SplFileObject` (and the directory
//! stream of a `DirectoryIterator`/`GlobIterator`) as an ordinary stream
//! resource flagged `PHP_STREAM_FLAG_NO_FCLOSE`: `get_resources()` lists
//! it and `fclose()` refuses it. `php-spl` keeps the streams in its object
//! state; this module parks them in `php-ext-streams` resources between
//! SPL calls ("check in", when the outermost SPL entry returns) and moves
//! them back into the state whenever SPL code accesses it ("check out",
//! through `php_spl::object::set_file_access_hook`).
//!
//! User stream wrappers run PHP code from inside the stream layer, which
//! reaches the VM through `php_ext_streams::bridge`. SPL file methods
//! therefore run inside [`bridge::run`] with a [`BridgedHost`], which
//! borrows the VM from the bridge only for the duration of each host call.

use crate::host::BridgedHost;
use php_ext_streams::{bridge, res};
use php_runtime::{RtResult, Value};
use php_spl::directory::FileState;
use php_spl::{SplHost, SplObject};
use php_streams::stream::{flags, Stream};
use php_vm::ext::VmResult;
use php_vm::Vm;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

thread_local! {
    /// Nesting depth of SPL entries (method calls, hooks, iterator steps).
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Whether a stream was checked out during the current entry.
    static DIRTY: Cell<bool> = const { Cell::new(false) };
    /// Set at request shutdown: streams stay in their objects.
    static UNPARKED: Cell<bool> = const { Cell::new(false) };
    /// The live `SplFileInfo`-family states.
    static STATES: RefCell<Vec<Weak<SplObject>>> = const { RefCell::new(Vec::new()) };
}

/// Installs the check-out hook (at module startup).
pub fn install() {
    php_spl::object::set_file_access_hook(Some(on_access));
}

/// Request shutdown, run before the stream layer closes the remaining
/// resources: PHP flushes the output buffers (whose handlers may still
/// write to an `SplFileObject`) before it frees resources, so the streams
/// go back into their objects for good and are closed with them.
pub fn shutdown(_vm: &mut Vm) {
    UNPARKED.with(|u| u.set(true));
    let states: Vec<Rc<SplObject>> =
        STATES.with(|s| s.borrow().iter().filter_map(Weak::upgrade).collect());
    for st in states {
        let _ = st.file(|f| {
            check_out(&mut f.stream, f.stream_res.as_ref());
            check_out(&mut f.dirp, f.dir_res.as_ref());
        });
    }
}

/// Records a new `SplFileInfo`-family state (from the `create`/`clone`
/// hooks).
pub fn track(st: &Rc<SplObject>) {
    if !is_file_state(st) {
        return;
    }
    STATES.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() >= 64 && s.len().is_power_of_two() {
            s.retain(|w| w.strong_count() > 0);
        }
        s.push(Rc::downgrade(st));
    });
}

/// One SPL entry; the outermost one checks the streams back in on drop.
#[derive(Debug)]
pub struct Entry(());

/// Starts an SPL entry.
#[must_use]
pub fn enter() -> Entry {
    DEPTH.with(|d| d.set(d.get() + 1));
    Entry(())
}

impl Drop for Entry {
    fn drop(&mut self) {
        let depth = DEPTH.with(|d| {
            let n = d.get().saturating_sub(1);
            d.set(n);
            n
        });
        if depth == 0 && DIRTY.with(|d| d.replace(false)) && !UNPARKED.with(Cell::get) {
            let states: Vec<Rc<SplObject>> = STATES.with(|s| {
                let mut s = s.borrow_mut();
                s.retain(|w| w.strong_count() > 0);
                s.iter().filter_map(Weak::upgrade).collect()
            });
            for st in states {
                // The access hook is inactive at depth 0.
                let _ = st.file(|f| {
                    f.stream_res = park(f.stream.take(), f.stream_res.take());
                    f.dir_res = park(f.dirp.take(), f.dir_res.take());
                });
            }
        }
    }
}

/// The file access hook: inside an SPL entry, moves parked streams back
/// into the state.
fn on_access(f: &mut FileState) {
    if DEPTH.with(Cell::get) == 0 {
        return;
    }
    DIRTY.with(|d| d.set(true));
    check_out(&mut f.stream, f.stream_res.as_ref());
    check_out(&mut f.dirp, f.dir_res.as_ref());
}

fn check_out(slot: &mut Option<Stream>, resource: Option<&Value>) {
    if slot.is_some() {
        return;
    }
    if let Some(Value::Resource(r)) = resource {
        if let Some(cell) = res::cell_of(r) {
            *slot = cell.take();
        }
    }
}

/// Moves a stream into its resource (creating the resource on first use).
/// Without a checked-out stream the resource stays while it still holds
/// its stream; one SPL released (checked out, then dropped) goes away.
fn park(stream: Option<Stream>, resource: Option<Value>) -> Option<Value> {
    let Some(mut stream) = stream else {
        return resource.filter(|r| match r {
            Value::Resource(r) => res::cell_of(r).is_some_and(|c| !c.is_closed()),
            _ => false,
        });
    };
    if let Some(Value::Resource(r)) = &resource {
        if let Some(cell) = res::cell_of(r) {
            drop(cell.replace_stream(stream));
            return resource;
        }
    }
    stream.add_flags(flags::NO_FCLOSE);
    Some(res::new_stream(stream, None))
}

/// Runs a `php-spl` call on a file object with the VM lent to the stream
/// layer (user wrappers), propagating wrapper exceptions and warnings.
pub fn run_bridged<R>(vm: &mut Vm, f: impl FnOnce(&mut dyn SplHost) -> RtResult<R>) -> VmResult<R> {
    let r = bridge::run(vm, || {
        let mut host = BridgedHost::new();
        f(&mut host)
    });
    match r {
        Err(t) => Err(t),
        Ok(r) => crate::iter::finish(vm, r),
    }
}

/// Whether `st` is the state of an `SplFileInfo`-family object.
#[must_use]
pub fn is_file_state(st: &SplObject) -> bool {
    st.with(|s| matches!(s, php_spl::SplState::File(_)))
        .unwrap_or(false)
}
