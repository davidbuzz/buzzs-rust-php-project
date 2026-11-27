// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP's Standard PHP Library (`ext/spl`) for phprust.
//!
//! The crate implements the internal state and the methods of every SPL
//! class, the SPL object handlers, the exception hierarchy and the `spl_*`
//! / `iterator_*` / `class_*` functions, on top of `php-runtime` values.
//! User code (overridden methods, callbacks, `__toString`, foreign
//! iterators) is reached through the [`SplHost`] trait, which the VM
//! implements once.
//!
//! # Integration
//!
//! * Register the classes described by [`classes::class_table`] (names,
//!   parents, interfaces, constants, method signatures).
//! * When instantiating a class whose nearest SPL ancestor is `base`, store
//!   [`object::create_state`]`(base)` with the object and return it from
//!   [`SplHost::spl_state`].
//! * Route every call of an internal SPL method to [`call_method`] with the
//!   declaring class, and every SPL function to [`call_function`].
//! * Consult [`handlers`] for dimension access, `count()`, property tables,
//!   debug info, comparison, cloning and native `foreach` iteration.
//!
//! # Re-entrancy
//!
//! State lives in an [`object::SplObject`] (a `RefCell`) that SPL borrows
//! only briefly; it never holds a borrow while calling into the host, so
//! user callbacks may use the same object again (PHP's own guards such as
//! "Heap cannot be changed when it is already being modified." apply).

pub mod args;
pub mod array;
pub mod classes;
pub mod directory;
pub mod dllist;
pub mod exceptions;
pub mod fixedarray;
pub mod functions;
pub mod handlers;
pub mod heap;
pub mod host;
pub mod iter;
pub mod iterators;
pub mod object;
pub mod observer;
pub mod serial;
pub mod util;

pub use host::{ClassInfo, HostIterator, MethodIterator, NativeOp, ObjectIterator, SplHost};
pub use object::{create_state, SplObject, SplState};
pub use util::DimKind;

use php_runtime::{Obj, RtResult, RuntimeError, Value};

/// Calls the internal implementation of `class::method` (the declaring SPL
/// class and the method name, any case). `this` is `None` for static
/// methods.
pub fn call_method(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> RtResult<Value> {
    // Lower-cased in buffers on the stack: this runs for every SPL method
    // call (each step of a `foreach` over an SPL iterator makes several),
    // and two `String`s a call were two allocations.
    let mut mbuf = [0u8; 64];
    let mut cbuf = [0u8; 64];
    let m_owned;
    let m: &str = if let Some(m) = lower_in(&mut mbuf, method) {
        m
    } else {
        m_owned = method.to_ascii_lowercase();
        &m_owned
    };
    let c_owned;
    let lc_class: &str = if let Some(c) = lower_in(&mut cbuf, class) {
        c
    } else {
        c_owned = class.to_ascii_lowercase();
        &c_owned
    };
    let r = match lc_class {
        "splfixedarray" => fixedarray::call(host, m, this, args),
        "splheap" => heap::call(host, "SplHeap", m, this, args),
        "splminheap" => heap::call(host, "SplMinHeap", m, this, args),
        "splmaxheap" => heap::call(host, "SplMaxHeap", m, this, args),
        "splpriorityqueue" => heap::call(host, "SplPriorityQueue", m, this, args),
        "spldoublylinkedlist" => dllist::call(host, "SplDoublyLinkedList", m, this, args),
        "splqueue" => dllist::call(host, "SplQueue", m, this, args),
        "splobjectstorage" => observer::call(host, "SplObjectStorage", m, this, args),
        "multipleiterator" => observer::call(host, "MultipleIterator", m, this, args),
        "arrayobject" => array::call(host, "ArrayObject", m, this, args),
        "arrayiterator" => array::call(host, "ArrayIterator", m, this, args),
        "recursivearrayiterator" => array::call(host, "RecursiveArrayIterator", m, this, args),
        lc => match classes::canonical_name(lc) {
            Some(c) if directory::is_file_class(c) => directory::call(host, c, m, this, args),
            Some(c) => iterators::call(host, c, m, this, args),
            None => None,
        },
    };
    r.unwrap_or_else(|| {
        Err(RuntimeError::Error(format!(
            "Call to undefined method {class}::{method}()"
        )))
    })
}

/// [`call_method`] of a method `php-ext-spl` registered: `class` is the
/// declaring SPL class's canonical name and `method` the method's name as
/// declared, so neither is looked up again. (Every step of a `foreach`
/// over an SPL iterator calls several: the lookups by name were most of
/// each call.)
pub fn call_method_canonical(
    host: &mut dyn SplHost,
    class: &'static str,
    method: &'static str,
    this: Option<&Obj>,
    args: &[Value],
) -> RtResult<Value> {
    let lowered;
    let m: &str = if method.bytes().any(|b| b.is_ascii_uppercase()) {
        lowered = method.to_ascii_lowercase();
        &lowered
    } else {
        method
    };
    let r = match class {
        "SplFixedArray" => fixedarray::call(host, m, this, args),
        "SplHeap" | "SplMinHeap" | "SplMaxHeap" | "SplPriorityQueue" => {
            heap::call(host, class, m, this, args)
        }
        "SplDoublyLinkedList" | "SplQueue" => dllist::call(host, class, m, this, args),
        "SplObjectStorage" | "MultipleIterator" => observer::call(host, class, m, this, args),
        "ArrayObject" | "ArrayIterator" | "RecursiveArrayIterator" => {
            array::call(host, class, m, this, args)
        }
        c if directory::is_file_class(c) => directory::call(host, c, m, this, args),
        c => Some(iterators::call_named(host, c, m, method, this, args)),
    };
    r.unwrap_or_else(|| {
        Err(RuntimeError::Error(format!(
            "Call to undefined method {class}::{method}()"
        )))
    })
}

/// `s` lower-cased into `buf`, or `None` when it does not fit.
fn lower_in<'b>(buf: &'b mut [u8; 64], s: &str) -> Option<&'b str> {
    let out = buf.get_mut(..s.len())?;
    out.copy_from_slice(s.as_bytes());
    out.make_ascii_lowercase();
    // ASCII lower-casing keeps UTF-8 valid.
    std::str::from_utf8(out).ok()
}

/// Calls an SPL function (`spl_object_id`, `iterator_to_array`, ...).
/// Returns `None` for names that are not SPL functions.
pub fn call_function(
    host: &mut dyn SplHost,
    name: &str,
    args: &[Value],
) -> Option<RtResult<Value>> {
    functions::call(host, &name.to_ascii_lowercase(), args)
}

#[cfg(test)]
mod tests {
    use super::lower_in;

    /// The stack lower-casing `call_method` uses: names that fit come back
    /// lower-cased, longer ones are left to the allocating path.
    #[test]
    fn lower_in_lower_cases_names_that_fit() {
        let mut buf = [0u8; 64];
        assert_eq!(lower_in(&mut buf, "FilterIterator"), Some("filteriterator"));
        assert_eq!(
            lower_in(&mut buf, "getInnerIterator"),
            Some("getinneriterator")
        );
        assert_eq!(lower_in(&mut buf, ""), Some(""));
        let long = "A".repeat(65);
        assert_eq!(lower_in(&mut buf, &long), None);
        assert_eq!(
            lower_in(&mut buf, &"B".repeat(64)),
            Some("b".repeat(64).as_str())
        );
    }
}
