//! PHP's `ext/intl` for phprust, bound to the system ICU through its C API.
//!
//! The crate exposes plain Rust types mirroring the PHP classes
//! (`NumberFormatter`, `IntlDateFormatter`, `Collator`, …) and functions;
//! the VM wraps them into PHP objects.  Errors follow php-src's intl error
//! model, see [`error`].
#![deny(unsafe_code)]

pub mod error;
pub mod ffi;
pub mod ustr;

pub mod calendar;
pub mod common;
pub mod numfmt;
pub mod sys;
pub mod timezone;

pub use error::{Error, ErrorState, IntlError, IntlSettings, Notice, Result, Scope, UErrorCode};
