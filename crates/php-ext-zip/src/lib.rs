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
//! * The closing destructor is the VM's native `destruct` hook, which the
//!   VM skips when a subclass declares `__destruct()` (PHP runs both): such
//!   an archive is closed only when the object is freed, without callbacks.
//! * `php_zip::ZipHost` cannot ask whether a PHP frame is executing, so
//!   exceptions the library itself raises during a shutdown destructor are
//!   not bailouts there; [`host`] holds back the library's warnings in that
//!   case instead.

pub mod class;
mod host;
pub mod procedural;

use std::rc::Rc;

use php_vm::ext::{Extension, InfoItem, ModuleBuilder};
use php_vm::Vm;

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
        m.info(info);
    }
}

/// `PHP_MINFO_FUNCTION(zip)`.
fn info(_vm: &mut Vm) -> Vec<InfoItem> {
    let mut items = vec![InfoItem::TableStart];
    items.extend(
        php_zip::consts::info_rows()
            .into_iter()
            .map(|(k, v)| InfoItem::Row(vec![k.to_string(), v])),
    );
    items.push(InfoItem::TableEnd);
    items
}
