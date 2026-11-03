//! libcurl binding with ext/curl's semantics (ref: `ext/curl/interface.c`,
//! `multi.c`, `share.c`), independent of the VM.
//!
//! * [`Easy`], [`Multi`], [`Share`] wrap the libcurl handles and keep alive
//!   what libcurl references by pointer (slists, MIME parts, stderr files).
//! * [`Handler`] receives the transfer callbacks (write, header, read,
//!   progress, ...) keyed by [`Easy::key`]; the VM binding implements it.
//! * [`options::option_kind`] classifies `CURLOPT_*` the way
//!   `_php_curl_setopt()` coerces values.
//! * [`consts`] holds every constant ext/curl exposes, generated against
//!   the system libcurl (8.5.0), with the stub's version gates applied.
//!
//! All `unsafe` code is confined to the FFI wrappers.

pub mod consts;
pub mod easy;
mod ffi;
pub mod handler;
pub mod multi;
pub mod options;
pub mod share;
pub mod version;

pub use easy::{Easy, MimePart, MimeSource};
pub use handler::{Handler, NoHandler};
pub use multi::{InfoMsg, Multi};
pub use options::{option_kind, Callback, OptKind};
pub use share::Share;
pub use version::{version_info, VersionInfo};

/// `LIBCURL_VERSION_NUM` of the headers the constants were generated from.
pub const LIBCURL_VERSION_NUM: u32 = 0x08_05_00;
