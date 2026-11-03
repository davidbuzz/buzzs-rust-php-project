//! `ext/phar` for the phprust VM.
//!
//! The archive formats, the registry, the `phar://` wrapper and every
//! method's semantics live in the VM-independent `php-phar` library; this
//! crate binds them to the VM:
//!
//! * [`classes`] registers `PharException`, `Phar`, `PharData` and
//!   `PharFileInfo` (the latter three on top of the SPL classes from
//!   `php-ext-spl`, whose constructors they call for the
//!   `RecursiveDirectoryIterator` / `SplFileInfo` parent state);
//! * [`state`] keeps the native `PharObject` / `PharFileInfo` of each
//!   object in a side table (the object's internal slot belongs to SPL);
//! * [`archive`] / [`fileinfo`] / [`statics`] implement the methods;
//! * [`ini`] mirrors `phar.readonly` / `phar.require_hash` /
//!   `phar.cache_list` into the library;
//! * [`intercept`] implements `Phar::interceptFileFuncs()` by wrapping the
//!   file functions;
//! * [`openssl`] provides OpenSSL signatures through `php-openssl`;
//! * [`web`] is the `Phar::webPhar()` host.

pub mod archive;
pub mod classes;
pub mod fileinfo;
pub mod ini;
pub mod intercept;
pub mod openssl;
pub mod state;
pub mod statics;
pub mod util;
pub mod web;

use std::rc::Rc;

use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::Vm;

/// The `Phar` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Phar;

fn request_shutdown(_vm: &mut Vm) {
    state::release_all();
    php_phar::registry::reset_request();
}

impl Extension for Phar {
    fn name(&self) -> &'static str {
        "Phar"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        php_phar::registry::set_compression_support(true, true);
        php_phar::signature::set_openssl_hook(Some(Rc::new(openssl::Hook)));
        let _ =
            php_streams::wrapper::register_global(b"phar", Rc::new(php_phar::wrapper::PharWrapper));
        ini::register(m);
        classes::register(m);
        intercept::install(m);
        m.on_request_shutdown(request_shutdown);
    }
}
