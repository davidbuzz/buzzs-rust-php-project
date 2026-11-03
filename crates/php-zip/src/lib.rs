//! PHP's `ext/zip` for phprust, bound to the system libzip.
//!
//! * [`ZipArchive`] — the object state and every method of the class
//!   (`open`/`openString`, `add*`, `set*`/`get*`, `stat*`, `extractTo`,
//!   `getFrom*`, `getStream*`, callbacks, the virtual properties);
//! * [`stream`] — entry streams ([`stream::ZipStreamOps`]) and the `zip://`
//!   wrapper ([`ZipWrapper`]) for `php-streams`;
//! * [`procedural`] — the deprecated `zip_open()`/`zip_read()`/
//!   `zip_entry_*()` functions;
//! * [`consts`] — the `ZipArchive::*` constants and `phpinfo()` rows;
//! * [`class`] — method/parameter tables from `php_zip.stub.php`.
//!
//! Exceptions are returned as [`php_runtime::RuntimeError`] with PHP's
//! exact messages; warnings and deprecations go through
//! [`php_streams::diag`] (without the `function(): ` prefix). PHP code
//! (progress/cancel callbacks, `addPattern()` regexes, value conversions)
//! runs through the [`ZipHost`] the VM passes to the methods that need it.
//!
//! All `unsafe` code wraps libzip calls; the raw declarations are in the
//! private `ffi` module.

mod extract;
mod ffi;
mod paths;
mod pattern;
mod source;

pub mod archive;
pub mod class;
pub mod consts;
pub mod error;
pub mod host;
pub mod procedural;
pub mod stream;

pub use archive::{EntryStat, ExtractFiles, OpenResult, ZipArchive, PROPERTY_NAMES};
pub use host::{RegexMatcher, ZipHost};
pub use stream::{ZipStreamOps, ZipWrapper};

/// The libzip version this crate was built against (`LIBZIP_VERSION`).
pub const LIBZIP_HEADERS_VERSION: &str = env!("PHP_ZIP_LIBZIP_VERSION");
