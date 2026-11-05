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

/// The index of an auto-global name in [`AUTO_GLOBAL_NAMES`].
fn index(name: &[u8]) -> Option<usize> {
    AUTO_GLOBAL_NAMES.iter().position(|n| *n == name)
}

thread_local! {
    /// Auto-globals named by code compiled since the VM last activated
    /// them, in first-seen order (indices into [`AUTO_GLOBAL_NAMES`]).
    static COMPILED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
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
#[derive(Default)]
pub struct AutoGlobals {
    /// `zend_auto_global.armed` with its callback, per
    /// [`AUTO_GLOBAL_NAMES`] entry.
    armed: [Option<AutoGlobalCallback>; AUTO_GLOBAL_NAMES.len()],
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
        true
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
