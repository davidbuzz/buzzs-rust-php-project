// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Save handlers (`ps_module`): the [`SaveModule`]/[`SaveHandler`] traits
//! for native storage modules, the module reference stored in
//! `PS(mod)`/`PS(default_mod)` and the dispatch functions
//! (`PS(mod)->s_open(&PS(mod_data), …)` and friends).

use crate::host::SessionHost;
use crate::state::create_default_id;
use crate::user;
use std::fmt;
use std::rc::Rc;

/// A native storage module (the `ps_module` vtable's `s_open`): opening it
/// produces the module data ([`SaveHandler`]) stored in `PS(mod_data)`.
pub trait SaveModule: fmt::Debug {
    /// `s_name`, e.g. `"files"`.
    fn name(&self) -> &str;

    /// `PS_OPEN_FUNC`: parses `save_path` and returns the opened handler,
    /// or `None` (`FAILURE`, after emitting any diagnostics).
    fn open(
        &self,
        host: &mut dyn SessionHost,
        save_path: &[u8],
        session_name: &[u8],
    ) -> Option<Box<dyn SaveHandler>>;
}

/// The data of an opened native module (`PS(mod_data)`) and the remaining
/// `ps_module` operations. Boolean results are `SUCCESS`/`FAILURE`.
pub trait SaveHandler: fmt::Debug {
    /// `PS_CLOSE_FUNC`: releases resources (the handler is dropped after).
    fn close(&mut self, host: &mut dyn SessionHost) -> bool;

    /// `PS_READ_FUNC`: the stored data (`""` for a new session), or `None`.
    fn read(&mut self, host: &mut dyn SessionHost, key: &[u8], maxlifetime: i64)
        -> Option<Vec<u8>>;

    /// `PS_WRITE_FUNC`.
    fn write(
        &mut self,
        host: &mut dyn SessionHost,
        key: &[u8],
        val: &[u8],
        maxlifetime: i64,
    ) -> bool;

    /// `PS_DESTROY_FUNC`.
    fn destroy(&mut self, host: &mut dyn SessionHost, key: &[u8]) -> bool;

    /// `PS_GC_FUNC`: number of deleted sessions, `-1` on failure.
    fn gc(&mut self, host: &mut dyn SessionHost, maxlifetime: i64) -> i64;

    /// `PS_CREATE_SID_FUNC` (the default is `php_session_create_id`).
    fn create_sid(&mut self, host: &mut dyn SessionHost) -> Option<Vec<u8>> {
        create_default_id(host)
    }

    /// `PS_VALIDATE_SID_FUNC`: whether a session with this ID exists (the
    /// default `php_session_validate_sid` accepts everything).
    fn validate_sid(&mut self, host: &mut dyn SessionHost, key: &[u8]) -> bool {
        let _ = (host, key);
        true
    }

    /// `PS_UPDATE_TIMESTAMP_FUNC` (the default is the dummy
    /// `php_session_update_timestamp`).
    fn update_timestamp(
        &mut self,
        host: &mut dyn SessionHost,
        key: &[u8],
        val: &[u8],
        maxlifetime: i64,
    ) -> bool {
        let _ = (host, key, val, maxlifetime);
        true
    }

    /// Whether `s_update_timestamp` is a real implementation (lazy writes
    /// only use it then).
    fn has_update_timestamp(&self) -> bool {
        true
    }
}

/// A registered module: the built-in user module or a native one.
#[derive(Debug, Clone)]
pub enum ModRef {
    /// `ps_mod_user` (`session_set_save_handler`).
    User,
    /// A native module such as `files`.
    Native(Rc<dyn SaveModule>),
}

impl ModRef {
    /// `s_name`.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            ModRef::User => "user".to_string(),
            ModRef::Native(m) => m.name().to_string(),
        }
    }

    /// Whether this is the user module.
    #[must_use]
    pub fn is_user(&self) -> bool {
        matches!(self, ModRef::User)
    }

    /// Identity comparison.
    #[must_use]
    pub fn same(&self, other: &ModRef) -> bool {
        match (self, other) {
            (ModRef::User, ModRef::User) => true,
            (ModRef::Native(a), ModRef::Native(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Runs `f` on the opened module data (taken out of the state for the
/// duration of the call so the handler may re-enter the session module).
fn with_data<T>(
    h: &mut dyn SessionHost,
    missing: T,
    f: impl FnOnce(&mut dyn SessionHost, &mut Box<dyn SaveHandler>) -> T,
) -> T {
    let Some(mut data) = h.session().mod_data.take() else {
        return missing;
    };
    let r = f(h, &mut data);
    let s = h.session();
    if s.mod_data.is_none() {
        s.mod_data = Some(data);
    }
    r
}

/// `m->s_open(&PS(mod_data), save_path, session_name)`.
pub fn mod_open(h: &mut dyn SessionHost, m: &ModRef, save_path: &[u8], name: &[u8]) -> bool {
    match m {
        ModRef::User => user::open(h, save_path, name),
        ModRef::Native(module) => match module.open(h, save_path, name) {
            Some(data) => {
                if let Some(mut old) = h.session().mod_data.take() {
                    old.close(h);
                }
                h.session().mod_data = Some(data);
                true
            }
            None => false,
        },
    }
}

/// `m->s_close(&PS(mod_data))`.
pub fn mod_close(h: &mut dyn SessionHost, m: &ModRef) -> bool {
    match m {
        ModRef::User => user::close(h),
        ModRef::Native(_) => match h.session().mod_data.take() {
            Some(mut data) => data.close(h),
            None => true,
        },
    }
}

/// `m->s_read(&PS(mod_data), key, &val, maxlifetime)`.
pub fn mod_read(
    h: &mut dyn SessionHost,
    m: &ModRef,
    key: &[u8],
    maxlifetime: i64,
) -> Option<Vec<u8>> {
    match m {
        ModRef::User => user::read(h, key),
        ModRef::Native(_) => with_data(h, None, |h, d| d.read(h, key, maxlifetime)),
    }
}

/// `m->s_write(&PS(mod_data), key, val, maxlifetime)`.
pub fn mod_write(
    h: &mut dyn SessionHost,
    m: &ModRef,
    key: &[u8],
    val: &[u8],
    maxlifetime: i64,
) -> bool {
    match m {
        ModRef::User => user::write(h, key, val),
        ModRef::Native(_) => with_data(h, false, |h, d| d.write(h, key, val, maxlifetime)),
    }
}

/// `m->s_destroy(&PS(mod_data), key)`.
pub fn mod_destroy(h: &mut dyn SessionHost, m: &ModRef, key: &[u8]) -> bool {
    match m {
        ModRef::User => user::destroy(h, key),
        ModRef::Native(_) => with_data(h, false, |h, d| d.destroy(h, key)),
    }
}

/// `m->s_gc(&PS(mod_data), maxlifetime, &nrdels)`: `nrdels` (`-1` = error).
pub fn mod_gc(h: &mut dyn SessionHost, m: &ModRef, maxlifetime: i64) -> i64 {
    match m {
        ModRef::User => user::gc(h, maxlifetime),
        ModRef::Native(_) => with_data(h, -1, |h, d| d.gc(h, maxlifetime)),
    }
}

/// `m->s_create_sid(&PS(mod_data))`.
pub fn mod_create_sid(h: &mut dyn SessionHost, m: &ModRef) -> Option<Vec<u8>> {
    match m {
        ModRef::User => user::create_sid(h),
        ModRef::Native(_) => {
            if h.session().mod_data.is_none() {
                return create_default_id(h);
            }
            with_data(h, None, |h, d| d.create_sid(h))
        }
    }
}

/// `m->s_validate_sid(&PS(mod_data), key)`.
pub fn mod_validate_sid(h: &mut dyn SessionHost, m: &ModRef, key: &[u8]) -> bool {
    match m {
        ModRef::User => user::validate_sid(h, key),
        ModRef::Native(_) => with_data(h, false, |h, d| d.validate_sid(h, key)),
    }
}

/// `m->s_update_timestamp(&PS(mod_data), key, val, maxlifetime)`.
pub fn mod_update_timestamp(
    h: &mut dyn SessionHost,
    m: &ModRef,
    key: &[u8],
    val: &[u8],
    maxlifetime: i64,
) -> bool {
    match m {
        ModRef::User => user::update_timestamp(h, key, val),
        ModRef::Native(_) => with_data(h, false, |h, d| {
            d.update_timestamp(h, key, val, maxlifetime)
        }),
    }
}

/// Whether the module has a real `s_update_timestamp`.
pub fn mod_has_update_timestamp(h: &mut dyn SessionHost, m: &ModRef) -> bool {
    match m {
        ModRef::User => true,
        ModRef::Native(_) => h
            .session()
            .mod_data
            .as_ref()
            .is_none_or(|d| d.has_update_timestamp()),
    }
}
