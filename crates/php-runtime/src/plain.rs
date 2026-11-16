// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A minimal object implementation (class name + property table) used by
//! [`crate::CollectingContext`] so that object-related runtime features
//! (`(object)` casts, `unserialize`, `json_decode`, dumps) can be exercised
//! without a VM.

use crate::array::ZArray;
use crate::string::ZStr;
use crate::value::{Obj, PhpObject};
use std::any::Any;
use std::cell::{Cell, RefCell};

thread_local! {
    static NEXT_HANDLE: Cell<u32> = const { Cell::new(1) };
}

/// A plain object: a class name, a property table (mangled string keys) and
/// an optional `Serializable` payload.
#[derive(Debug)]
pub struct PlainObject {
    handle: u32,
    class: ZStr,
    props: RefCell<ZArray>,
    custom: RefCell<Option<Vec<u8>>>,
}

impl PlainObject {
    /// Creates a new object handle with the next free handle number.
    #[must_use]
    pub fn create(class: impl Into<ZStr>, props: ZArray) -> Obj {
        let handle = NEXT_HANDLE.with(|c| {
            let h = c.get();
            c.set(h.wrapping_add(1));
            h
        });
        Obj::new(PlainObject {
            handle,
            class: class.into(),
            props: RefCell::new(props),
            custom: RefCell::new(None),
        })
    }

    /// A copy of the property table.
    #[must_use]
    pub fn props(&self) -> ZArray {
        self.props.borrow().clone()
    }

    /// Replaces the property table.
    pub fn set_props(&self, props: ZArray) {
        *self.props.borrow_mut() = props;
    }

    /// Writes one property (verbatim string key).
    pub fn write(&self, name: &ZStr, value: crate::Value) {
        self.props
            .borrow_mut()
            .make_mut()
            .insert_str_raw(name.clone(), value);
    }

    /// The `Serializable` payload, if this object came from a `C:` record.
    #[must_use]
    pub fn custom_data(&self) -> Option<Vec<u8>> {
        self.custom.borrow().clone()
    }

    /// Sets the `Serializable` payload.
    pub fn set_custom_data(&self, data: Option<Vec<u8>>) {
        *self.custom.borrow_mut() = data;
    }
}

impl PhpObject for PlainObject {
    fn handle(&self) -> u32 {
        self.handle
    }
    fn class_name(&self) -> ZStr {
        self.class.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
