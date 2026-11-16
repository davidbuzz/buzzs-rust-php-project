// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Just-in-time auto-globals (`CG(auto_globals)`).
//!
//! ref: Zend/zend_compile.c (`zend_register_auto_global`,
//! `zend_is_auto_global`, `zend_activate_auto_globals`) and
//! main/php_variables.c (`php_startup_auto_globals`,
//! `php_auto_globals_create_*`).
//!
//! With `auto_globals_jit` php-src creates `$_SERVER`, `$_ENV` and
//! `$_REQUEST` only when a script that is being compiled names them (or an
//! extension asks for them through `zend_is_auto_global()`): until then
//! they are absent from `$GLOBALS`. The SAPI arms a creation callback per
//! name ([`Vm::arm_auto_global`]); the compiler notes every auto-global
//! name it resolves, and the VM runs the armed callbacks right after each
//! compilation, in the order the names were first seen.

use crate::Vm;
use php_runtime::{Value, ZArray};
use std::cell::RefCell;

/// `zend_auto_global_callback`: creates the auto-global `name` in the
/// global symbol table.
pub type AutoGlobalCallback = fn(&mut Vm, &[u8]);

/// The auto-global names, in `CG(auto_globals)` registration order
/// (`php_startup_auto_globals`, then ext/session's `$_SESSION`).
pub const AUTO_GLOBAL_NAMES: [&[u8]; 8] = [
    b"_GET",
    b"_POST",
    b"_COOKIE",
    b"_SERVER",
    b"_ENV",
    b"_REQUEST",
    b"_FILES",
    b"_SESSION",
];

/// The index of `$_SESSION` in [`AUTO_GLOBAL_NAMES`].
const SESSION: usize = 7;

/// The index of an auto-global name in [`AUTO_GLOBAL_NAMES`].
pub fn index(name: &[u8]) -> Option<usize> {
    AUTO_GLOBAL_NAMES.iter().position(|n| *n == name)
}

/// Returns true if `name` is an auto-global name.
#[inline]
#[allow(dead_code)]
pub fn is_auto_global_name(name: &[u8]) -> bool {
    index(name).is_some()
}

thread_local! {
    /// Auto-globals named by code compiled since the VM last activated
    /// them, in first-seen order (indices into [`AUTO_GLOBAL_NAMES`]).
    static COMPILED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Resets compiled auto-globals state for a new request.
pub(crate) fn reset() {
    COMPILED.with(|c| c.borrow_mut().clear());
}

/// `zend_is_auto_global()` as the compiler calls it: whether `name` is an
/// auto-global, remembering it so that its armed callback runs once the
/// compilation finishes ([`Vm::activate_compiled_auto_globals`]).
pub(crate) fn note_compiled(name: &[u8]) -> bool {
    let Some(i) = index(name) else {
        return false;
    };
    let i = u8::try_from(i).unwrap_or(u8::MAX);
    COMPILED.with(|c| {
        let mut c = c.borrow_mut();
        if !c.contains(&i) {
            c.push(i);
        }
    });
    true
}

/// The armed (not yet created) auto-globals of a request.
pub struct AutoGlobals {
    /// `zend_auto_global.armed` with its callback, per
    /// [`AUTO_GLOBAL_NAMES`] entry.
    armed: [Option<AutoGlobalCallback>; AUTO_GLOBAL_NAMES.len()],
    /// Direct cached slot index in `vm.globals` per auto-global.
    pub(crate) cached_slots: [Option<u32>; AUTO_GLOBAL_NAMES.len()],
    /// Pre-initialized array for `$_GET`.
    pub(crate) get_array: Option<Value>,
}

impl Default for AutoGlobals {
    fn default() -> Self {
        Self {
            armed: [None; AUTO_GLOBAL_NAMES.len()],
            cached_slots: [None; AUTO_GLOBAL_NAMES.len()],
            get_array: Some(Value::Array(ZArray::new())),
        }
    }
}

impl std::fmt::Debug for AutoGlobals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let armed: Vec<String> = AUTO_GLOBAL_NAMES
            .iter()
            .zip(&self.armed)
            .filter(|(_, cb)| cb.is_some())
            .map(|(n, _)| String::from_utf8_lossy(n).into_owned())
            .collect();
        f.debug_struct("AutoGlobals")
            .field("armed", &armed)
            .field("cached_slots", &self.cached_slots)
            .finish()
    }
}

impl Vm {
    /// Arms the JIT auto-global `name` (`zend_register_auto_global` with
    /// `jit`): `callback` creates it the first time compiled code names it
    /// or [`Vm::is_auto_global`] asks for it. Returns false when `name` is
    /// not an auto-global.
    pub fn arm_auto_global(&mut self, name: &[u8], callback: AutoGlobalCallback) -> bool {
        let Some(i) = index(name) else {
            return false;
        };
        self.ext.auto_globals.armed[i] = Some(callback);
        true
    }

    /// `zend_is_auto_global()`: whether `name` is an auto-global, creating
    /// it first if it is still armed.
    pub fn is_auto_global(&mut self, name: &[u8]) -> bool {
        let Some(i) = index(name) else {
            return false;
        };
        if let Some(callback) = self.ext.auto_globals.armed[i].take() {
            callback(self, AUTO_GLOBAL_NAMES[i]);
        }
        if self.ext.auto_globals.cached_slots[i].is_none() {
            let mut g = self.globals.borrow_mut();
            if let Some(slot) = g.find(name) {
                self.ext.auto_globals.cached_slots[i] = Some(slot);
            } else if i == 0 {
                let slot = g.find_or_add(name);
                let arr = self
                    .ext
                    .auto_globals
                    .get_array
                    .clone()
                    .unwrap_or_else(|| Value::Array(ZArray::new()));
                g.slots[slot as usize] = Some(arr);
                self.ext.auto_globals.cached_slots[0] = Some(slot);
            }
        }
        true
    }

    /// Direct read of `$_GET`: hits a direct cached pointer/slot
    /// or pre-initialized array rather than doing string lookups or full jit-initialization on every read.
    #[inline]
    pub fn get_auto_global_get(&mut self) -> Value {
        self.get_auto_global(0)
    }

    /// Direct read of an auto-global by registration index: hits a direct cached
    /// pointer/slot or pre-initialized array.
    #[inline]
    pub fn get_auto_global(&mut self, idx: usize) -> Value {
        if let Some(slot) = self.ext.auto_globals.cached_slots[idx] {
            let g = self.globals.borrow();
            if let Some(slot_val) = g.slots.get(slot as usize) {
                return match slot_val {
                    Some(val) => val.deref(),
                    None => Value::Null,
                };
            }
        }
        self.init_and_get_auto_global(idx)
    }

    /// Initializes and fetches an auto-global by index, caching its slot index
    /// and pre-initializing an array if not yet set in globals.
    #[cold]
    pub fn init_and_get_auto_global(&mut self, idx: usize) -> Value {
        if let Some(slot) = self.ext.auto_globals.cached_slots[idx] {
            let g = self.globals.borrow();
            if let Some(slot_val) = g.slots.get(slot as usize) {
                return match slot_val {
                    Some(val) => val.deref(),
                    None => Value::Null,
                };
            }
        }
        let name = AUTO_GLOBAL_NAMES[idx];
        if let Some(callback) = self.ext.auto_globals.armed[idx].take() {
            callback(self, name);
        }
        let mut g = self.globals.borrow_mut();
        if let Some(slot) = g.find(name) {
            self.ext.auto_globals.cached_slots[idx] = Some(slot);
            return match &g.slots[slot as usize] {
                Some(v) => v.deref(),
                None => Value::Null,
            };
        }
        let slot = g.find_or_add(name);
        if idx == SESSION {
            // ext/session registers `$_SESSION` without creating it
            // (`zend_register_auto_global(..., 0, NULL)`): it exists only
            // once a session starts, and reads before that are undefined.
            self.ext.auto_globals.cached_slots[idx] = Some(slot);
            return Value::Null;
        }
        let arr = if idx == 0 {
            self.ext
                .auto_globals
                .get_array
                .clone()
                .unwrap_or_else(|| Value::Array(ZArray::new()))
        } else {
            Value::Array(ZArray::new())
        };
        g.slots[slot as usize] = Some(arr.clone());
        self.ext.auto_globals.cached_slots[idx] = Some(slot);
        if idx == 0 && self.ext.auto_globals.get_array.is_none() {
            self.ext.auto_globals.get_array = Some(arr.clone());
        }
        arr
    }

    /// Runs the armed callbacks of the auto-globals the last compilation
    /// named (what `zend_auto_global_check` does during compilation).
    pub(crate) fn activate_compiled_auto_globals(&mut self) {
        let seen = COMPILED.with(|c| std::mem::take(&mut *c.borrow_mut()));
        for i in seen {
            if let Some(name) = AUTO_GLOBAL_NAMES.get(usize::from(i)) {
                self.is_auto_global(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_index_in_registration_order() {
        assert_eq!(index(b"_GET"), Some(0));
        assert_eq!(index(b"_SERVER"), Some(3));
        assert_eq!(index(b"_SESSION"), Some(7));
        assert_eq!(index(b"GLOBALS"), None);
        assert_eq!(index(b"_env"), None);
    }

    #[test]
    fn note_compiled_keeps_first_seen_order() {
        COMPILED.with(|c| c.borrow_mut().clear());
        assert!(note_compiled(b"_REQUEST"));
        assert!(note_compiled(b"_ENV"));
        assert!(note_compiled(b"_REQUEST"));
        assert!(!note_compiled(b"argv"));
        let seen = COMPILED.with(|c| std::mem::take(&mut *c.borrow_mut()));
        assert_eq!(seen, vec![5, 4]);
    }
}
