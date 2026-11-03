//! PHP's `ext/phar` for phprust.
//!
//! A port of `ext/phar` (`phar.c`, `tar.c`, `zip.c`, `util.c`, `stream.c`,
//! `dirstream.c`, `phar_object.c`, `func_interceptors.c`) that does not
//! depend on the VM:
//!
//! * [`archive`] — the in-memory model ([`archive::Archive`],
//!   [`archive::Entry`]) and entry content access.
//! * [`phar_format`], [`tar_format`], [`zip_format`] — readers and writers
//!   for the three container formats, byte-compatible with PHP's output.
//! * [`registry`] — the per-request archive registry (`PHAR_G`): file name
//!   and alias maps, `phar.readonly` / `phar.require_hash` /
//!   `phar.cache_list`, opening, creating and flushing archives.
//! * [`signature`] — MD5/SHA signatures (via `php-hash`) and OpenSSL
//!   signatures through an [`signature::OpenSslHook`].
//! * [`stub`] — the default stub (byte-exact) and stub normalisation.
//! * [`object`] — the `Phar`, `PharData` and `PharFileInfo` methods with
//!   their exact exceptions.
//! * [`wrapper`] — the `phar://` stream and directory wrapper for
//!   `php-streams`.
//! * [`web`] — `Phar::webPhar()` routing, the mime table and
//!   `Phar::mungServer()`.
//! * [`intercept`] — what `Phar::interceptFileFuncs()` replaces.

pub mod archive;
mod bz2;
pub mod compress;
pub mod consts;
pub mod omap;
pub mod phar_format;
pub mod registry;
pub mod signature;
pub mod stub;
pub mod sys;
pub mod tar_format;
pub mod util;
pub mod zip_format;
