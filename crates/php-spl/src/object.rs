// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! SPL object state: one [`SplState`] variant per family of SPL classes,
//! stored by the VM next to the object's properties (the C `*_object`
//! structs that embed `zend_object`).

use crate::host::{missing_state, SplHost};
use php_runtime::{Obj, RtResult, RuntimeError};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// The internal state of one SPL object.
#[derive(Debug)]
#[non_exhaustive]
// The states live behind one `Rc` per object; boxing the larger variants
// would only add an indirection to every access.
#[allow(clippy::large_enum_variant)]
pub enum SplState {
    /// `SplFixedArray`.
    FixedArray(crate::fixedarray::FixedArray),
    /// `SplHeap`, `SplMinHeap`, `SplMaxHeap`, `SplPriorityQueue`.
    Heap(crate::heap::Heap),
    /// `SplDoublyLinkedList`, `SplQueue`, `SplStack`.
    DlList(crate::dllist::DlList),
    /// `SplObjectStorage`, `MultipleIterator`.
    Storage(crate::observer::ObjectStorage),
    /// `ArrayObject`, `ArrayIterator`, `RecursiveArrayIterator`.
    Array(crate::array::ArrayObject),
    /// `IteratorIterator` and all classes built on `spl_dual_it_object`.
    DualIt(crate::iterators::DualIt),
    /// `RecursiveIteratorIterator`, `RecursiveTreeIterator`.
    RecursiveIt(crate::iterators::RecursiveIt),
    /// `SplFileInfo` and its subclasses.
    File(crate::directory::FileState),
}

/// A shared, interior-mutable SPL state cell. SPL code borrows it only for
/// the duration of a state manipulation, never across calls into the host.
pub struct SplObject {
    base: &'static str,
    state: RefCell<SplState>,
}

impl fmt::Debug for SplObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.try_borrow() {
            Ok(s) => write!(f, "SplObject({}, {:?})", self.base, &*s),
            Err(_) => write!(f, "SplObject({}, <borrowed>)", self.base),
        }
    }
}

fn reentrant() -> RuntimeError {
    RuntimeError::Error("SPL object state is already in use".to_string())
}

impl SplObject {
    /// Wraps a state.
    #[must_use]
    pub fn new(base: &'static str, state: SplState) -> Rc<Self> {
        Rc::new(SplObject {
            base,
            state: RefCell::new(state),
        })
    }

    /// The internal SPL class this state was created for (the nearest SPL
    /// ancestor of the object's class).
    #[must_use]
    pub fn base(&self) -> &'static str {
        self.base
    }

    /// Runs `f` on the state.
    pub fn with<R>(&self, f: impl FnOnce(&mut SplState) -> R) -> RtResult<R> {
        let mut b = self.state.try_borrow_mut().map_err(|_| reentrant())?;
        Ok(f(&mut b))
    }
}

macro_rules! accessor {
    ($name:ident, $var:ident, $ty:ty) => {
        impl SplObject {
            /// Runs `f` on the state of this variant (error for other variants).
            pub fn $name<R>(&self, f: impl FnOnce(&mut $ty) -> R) -> RtResult<R> {
                let mut b = self.state.try_borrow_mut().map_err(|_| reentrant())?;
                match &mut *b {
                    SplState::$var(x) => Ok(f(x)),
                    _ => Err(RuntimeError::Error(format!(
                        "Internal error: {} has no {} state",
                        self.base,
                        stringify!($var)
                    ))),
                }
            }
        }
    };
}

accessor!(fixed, FixedArray, crate::fixedarray::FixedArray);
accessor!(heap, Heap, crate::heap::Heap);
accessor!(dllist, DlList, crate::dllist::DlList);
accessor!(storage, Storage, crate::observer::ObjectStorage);
accessor!(array, Array, crate::array::ArrayObject);
accessor!(dual, DualIt, crate::iterators::DualIt);
accessor!(recursive, RecursiveIt, crate::iterators::RecursiveIt);
thread_local! {
    /// See [`set_file_access_hook`].
    static FILE_ACCESS_HOOK: std::cell::Cell<Option<fn(&mut crate::directory::FileState)>> =
        const { std::cell::Cell::new(None) };
}

/// Installs a function run on an `SplFileInfo`-family state every time SPL
/// code accesses it (before the access). A host that keeps the file
/// streams elsewhere between calls (as PHP stream resources) moves them
/// back into the state here.
pub fn set_file_access_hook(hook: Option<fn(&mut crate::directory::FileState)>) {
    FILE_ACCESS_HOOK.with(|h| h.set(hook));
}

impl SplObject {
    /// Runs `f` on the `SplFileInfo`-family state (error for other
    /// variants), after the file access hook.
    pub fn file<R>(&self, f: impl FnOnce(&mut crate::directory::FileState) -> R) -> RtResult<R> {
        let mut b = self.state.try_borrow_mut().map_err(|_| reentrant())?;
        match &mut *b {
            SplState::File(x) => {
                if let Some(hook) = FILE_ACCESS_HOOK.with(std::cell::Cell::get) {
                    hook(x);
                }
                Ok(f(x))
            }
            _ => Err(RuntimeError::Error(format!(
                "Internal error: {} has no File state",
                self.base
            ))),
        }
    }
}

/// The state of `obj`, or an error if it has none.
pub fn state_of(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Rc<SplObject>> {
    host.spl_state(obj).ok_or_else(|| missing_state(obj))
}

/// The SPL classes that carry internal state, mapped to the canonical
/// spelling (the `create_object` handlers of ext/spl).
const STATEFUL: &[&str] = &[
    "SplFixedArray",
    "SplHeap",
    "SplMinHeap",
    "SplMaxHeap",
    "SplPriorityQueue",
    "SplDoublyLinkedList",
    "SplQueue",
    "SplStack",
    "SplObjectStorage",
    "MultipleIterator",
    "ArrayObject",
    "ArrayIterator",
    "RecursiveArrayIterator",
    "RecursiveIteratorIterator",
    "RecursiveTreeIterator",
    "IteratorIterator",
    "FilterIterator",
    "CallbackFilterIterator",
    "RecursiveFilterIterator",
    "RecursiveCallbackFilterIterator",
    "ParentIterator",
    "LimitIterator",
    "CachingIterator",
    "RecursiveCachingIterator",
    "NoRewindIterator",
    "AppendIterator",
    "InfiniteIterator",
    "RegexIterator",
    "RecursiveRegexIterator",
    "SplFileInfo",
    "DirectoryIterator",
    "FilesystemIterator",
    "RecursiveDirectoryIterator",
    "GlobIterator",
    "SplFileObject",
    "SplTempFileObject",
];

/// Canonical spelling of a stateful SPL class name.
#[must_use]
pub fn canonical_class(name: &str) -> Option<&'static str> {
    STATEFUL
        .iter()
        .copied()
        .find(|c| c.eq_ignore_ascii_case(name))
}

/// The `create_object` handler: creates the internal state for a new
/// object whose nearest SPL ancestor class (or own class) is `base`.
/// Returns `None` for classes without internal state (interfaces,
/// exceptions, `EmptyIterator`).
#[must_use]
pub fn create_state(base: &str) -> Option<Rc<SplObject>> {
    let base = canonical_class(base)?;
    let state = match base {
        "SplFixedArray" => SplState::FixedArray(crate::fixedarray::FixedArray::default()),
        "SplHeap" | "SplMinHeap" | "SplMaxHeap" | "SplPriorityQueue" => {
            SplState::Heap(crate::heap::Heap::new(base))
        }
        "SplDoublyLinkedList" | "SplQueue" | "SplStack" => {
            SplState::DlList(crate::dllist::DlList::new(base))
        }
        "SplObjectStorage" | "MultipleIterator" => {
            SplState::Storage(crate::observer::ObjectStorage::new(base))
        }
        "ArrayObject" | "ArrayIterator" | "RecursiveArrayIterator" => {
            SplState::Array(crate::array::ArrayObject::new(base))
        }
        "RecursiveIteratorIterator" | "RecursiveTreeIterator" => {
            SplState::RecursiveIt(crate::iterators::RecursiveIt::new(base))
        }
        "SplFileInfo"
        | "DirectoryIterator"
        | "FilesystemIterator"
        | "RecursiveDirectoryIterator"
        | "GlobIterator"
        | "SplFileObject"
        | "SplTempFileObject" => SplState::File(crate::directory::FileState::new(base)),
        _ => SplState::DualIt(crate::iterators::DualIt::new(base)),
    };
    Some(SplObject::new(base, state))
}
