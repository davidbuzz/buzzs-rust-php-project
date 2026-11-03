//! The PHP streams layer for phprust.
//!
//! A port of `main/streams/` and the file-related parts of `ext/standard`
//! (`file.c`, `filestat.c`, `dir.c`, `filters.c`, `user_filters.c`,
//! `streamsfuncs.c`, `php_fopen_wrapper.c`) plus `main/fopen_wrappers.c`,
//! `main/php_open_temporary_file.c` and `main/php_glob.c`.
//!
//! The crate does not depend on the VM's value model: PHP values crossing the
//! boundary (context options, filter parameters, user wrapper arguments and
//! return values) use the small [`value::Value`] type, user-space wrappers and
//! filters are traits the VM implements, and warnings are reported through
//! [`diag`].

pub mod builtin;
pub mod context;
pub mod diag;
pub mod file;
pub mod filestat;
pub mod filter;
pub mod fnmatch;
pub mod glob;
pub mod glob_wrapper;
pub mod memory;
pub mod mode;
pub mod open;
pub mod pathname;
pub mod paths;
pub mod php_wrapper;
pub mod plain;
pub mod stat;
pub mod state;
pub mod stream;
pub mod sys;
pub mod temp;
#[cfg(test)]
mod test_util;
pub mod transports;
pub mod user;
pub mod util;
pub mod value;
pub mod wrapper;
