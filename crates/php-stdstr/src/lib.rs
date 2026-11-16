// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Byte-exact ports of the pure algorithms behind PHP's `ext/standard`
//! string, URL, encoding and math functions.
//!
//! Everything here works on plain Rust data (`&[u8]`, `Vec<u8>`, `i64`,
//! `f64`, small structs) so that it does not depend on the VM's value
//! representation.  The VM is responsible for argument parsing and type
//! juggling (each function documents which PHP function it backs and what
//! coercions the VM has to perform first) and for turning the returned
//! [`StdError`]s into thrown exceptions and the pushed [`Notice`]s into
//! emitted diagnostics.
//!
//! Strings are byte strings, exactly like PHP's `zend_string`.

pub mod error;

pub mod analysis;
pub mod base64;
pub mod compare;
pub mod csv;
pub mod html;
mod html_tables;
pub mod math;
pub mod pack;
pub mod quoted_printable;
pub mod scanf;
pub mod search;
pub mod slashes;
pub mod strip_tags;
pub mod transform;
pub mod url;
pub mod uuencode;
pub mod versioning;

pub use error::{ErrorKind, Level, Notice, Notices, StdError, StdResult};
