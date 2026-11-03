//! `ext/sysvmsg`, `ext/sysvsem`, `ext/sysvshm` and `ext/shmop` for the
//! phprust VM: binds the `php-sysv` library through the `php_vm::ext`
//! extension API.
//!
//! Each extension is its own [`Extension`] (so `extension_loaded()` sees
//! the four names): [`Sysvmsg`], [`Sysvsem`], [`Sysvshm`] and [`Shmop`].
//! The library handles live as native state inside the opaque final
//! classes `SysvMessageQueue`, `SysvSemaphore`, `SysvSharedMemory` and
//! `Shmop`; dropping the object drops the handle, which performs PHP's
//! `free_obj` work (semaphore auto-release, detaching segments).
//!
//! PHP values cross the IPC boundary serialized: this crate runs the VM's
//! `serialize()`/`unserialize()` engine (with object hooks) around the
//! library's byte APIs.

// Native handlers share one signature (`fn(&mut Vm, &mut Args) ->
// VmResult<Value>`), so infallible ones still return `VmResult`.
#![allow(clippy::unnecessary_wraps)]

mod msg;
mod objects;
mod sem;
mod serial;
mod shm;
mod shmop;

use php_runtime::{Value, ZStr};
use php_sysv::SysvError;
use php_vm::ext::{Args, Extension, ModuleBuilder, Throw, VmResult};
use php_vm::Vm;

/// The `sysvmsg` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sysvmsg;

/// The `sysvsem` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sysvsem;

/// The `sysvshm` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sysvshm;

/// The `shmop` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shmop;

impl Extension for Sysvmsg {
    fn name(&self) -> &'static str {
        "sysvmsg"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.info(msg_info);
        msg::register(m);
    }
}

impl Extension for Sysvsem {
    fn name(&self) -> &'static str {
        "sysvsem"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.info(sem_info);
        sem::register(m);
    }
}

impl Extension for Sysvshm {
    fn name(&self) -> &'static str {
        "sysvshm"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.info(shm_info);
        shm::register(m);
    }
}

impl Extension for Shmop {
    fn name(&self) -> &'static str {
        "shmop"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.info(shmop_info);
        shmop::register(m);
    }
}

/// The running function's name (for exception prefixes).
pub(crate) fn fname(a: &Args) -> String {
    a.func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy())
}

/// A PHP string from bytes.
pub(crate) fn bytes(b: &[u8]) -> Value {
    Value::Str(ZStr::from(b))
}

/// Applies PHP's failure behaviour for a library error: throws, or warns
/// and returns `false`, or silently returns `false`.
pub(crate) fn failure(vm: &mut Vm, a: &Args, e: SysvError) -> VmResult<Value> {
    match e {
        SysvError::Failed(_) => Ok(Value::Bool(false)),
        SysvError::Warning(msg) => {
            vm.php_warning(&msg)?;
            Ok(Value::Bool(false))
        }
        other => Err(throw(vm, a, other)),
    }
}

/// The exception for a throwing library error (`ValueError`/`TypeError`
/// carry the `func(): ` prefix of `zend_argument_*_error()`; plain `Error`s
/// are `zend_throw_error()` texts without one).
pub(crate) fn throw(vm: &mut Vm, a: &Args, e: SysvError) -> Throw {
    match e {
        SysvError::ValueError(m) => vm.throw("ValueError", &format!("{}(): {m}", fname(a))),
        SysvError::TypeError(m) => vm.throw("TypeError", &format!("{}(): {m}", fname(a))),
        SysvError::Error(m) | SysvError::Warning(m) => vm.throw("Error", &m),
        SysvError::Failed(errno) => vm.throw("Error", &php_sysv::strerror(errno)),
    }
}

/// `PHP_MINFO_FUNCTION`.
fn msg_info(_vm: &mut Vm) -> Vec<php_vm::ext::InfoItem> {
    vec![
        php_vm::ext::InfoItem::TableStart,
        php_vm::ext::InfoItem::Row(vec!["sysvmsg support".into(), "enabled".into()]),
        php_vm::ext::InfoItem::TableEnd,
    ]
}

/// `PHP_MINFO_FUNCTION`.
fn sem_info(_vm: &mut Vm) -> Vec<php_vm::ext::InfoItem> {
    vec![
        php_vm::ext::InfoItem::TableStart,
        php_vm::ext::InfoItem::Row(vec!["sysvsem support".into(), "enabled".into()]),
        php_vm::ext::InfoItem::TableEnd,
    ]
}

/// `PHP_MINFO_FUNCTION`.
fn shm_info(_vm: &mut Vm) -> Vec<php_vm::ext::InfoItem> {
    vec![
        php_vm::ext::InfoItem::TableStart,
        php_vm::ext::InfoItem::Row(vec!["sysvshm support".into(), "enabled".into()]),
        php_vm::ext::InfoItem::TableEnd,
    ]
}

/// `PHP_MINFO_FUNCTION`.
fn shmop_info(_vm: &mut Vm) -> Vec<php_vm::ext::InfoItem> {
    vec![
        php_vm::ext::InfoItem::TableStart,
        php_vm::ext::InfoItem::Row(vec!["shmop support".into(), "enabled".into()]),
        php_vm::ext::InfoItem::TableEnd,
    ]
}
