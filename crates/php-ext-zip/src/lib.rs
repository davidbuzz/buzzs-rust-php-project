//! `ext/zip` for the phprust VM: binds the `php-zip` library (the system
//! libzip) through the `php_vm::ext` extension API.
//!
//! * [`class`] — `ZipArchive`: every method, its constants, the virtual
//!   read-only properties and the object handlers;
//! * [`procedural`] — the deprecated `zip_open()`/`zip_read()`/
//!   `zip_entry_*()` functions over "Zip Directory"/"Zip Entry" resources;
//! * [`host`] — the `php_zip::ZipHost` over the VM (progress/cancel
//!   callbacks, option conversions, `addPattern()` regexes) and error
//!   conversion.
//!
//! The `zip://` wrapper is `php_zip::ZipWrapper`, registered with the
//! global `php-streams` wrapper table; `getStream*()` return ordinary
//! `php-ext-streams` stream resources whose ops keep the archive alive.
//!
//! # Known gaps
//!
//! * The VM has no native `dtor_obj` hook, so the closing destructor is
//!   registered as `ZipArchive::__destruct()` (visible to reflection). A
//!   subclass overriding `__destruct()` without calling the parent closes
//!   the archive only when the object is freed, without callbacks.

pub mod class;
mod host;
pub mod procedural;

use std::rc::Rc;

use php_vm::ext::{Extension, ModuleBuilder};

/// The `zip` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Zip;

impl Extension for Zip {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        php_streams::wrapper::register_global(b"zip", Rc::new(php_zip::ZipWrapper));
        procedural::register(m);
        class::register(m);
    }
}
