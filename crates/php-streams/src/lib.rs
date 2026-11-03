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

pub mod fnmatch;
pub mod glob;
