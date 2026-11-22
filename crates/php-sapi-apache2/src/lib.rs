// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `apache2handler` SAPI: phprust as an Apache httpd 2.4 module
//! (`mod_php` replacement), built as `libphprust_apache2.so`.
//!
//! * `csrc/mod_phprust.c` — the small C shim declaring `php_module`, the
//!   `php_value`/`php_flag`/`php_admin_value`/`php_admin_flag`/`PHPINIDir`
//!   /`PhpRustAllowThreads` directives and the hooks; it reaches Rust
//!   through [`ffi`].
//! * [`handler`] — `php_handler()`: filtering, the request lock and
//!   thread, the `max_execution_time` watchdog, error-log capture.
//! * [`request`] — one request's lifecycle around the [`php_vm::Vm`].
//! * [`vars`] — `$_GET`/`$_POST`/`$_COOKIE`/`$_FILES`/`$_SERVER`/`$_ENV`/
//!   `$_REQUEST`; [`multipart`] — the rfc1867 upload parser.
//! * [`headers`] — `sapi_header_op()` with the Apache header handler;
//!   [`cookie`] — `setcookie()` formatting.
//! * [`functions`] — the PHP functions the SAPI provides (`header()`,
//!   `flush()`, `apache_*()`, ...).
//! * [`config`] — `php.ini` loading and SAPI INI defaults.
//! * [`backend`] — the server abstraction (real: [`ffi::ApacheRequest`],
//!   tests: [`backend::MockBackend`]).
//!
//! See `README.md` for the Apache configuration.

// The request heap and `memory_limit` accounting, as in the `php`,
// `php-cgi` and `php-fpm` binaries: the module's Rust allocations go
// through it (Apache's own pools and libraries are not affected). Without
// it nothing was counted, so `memory_get_usage()` stayed at its start
// value and `memory_limit` held only where a builtin checks a size before
// allocating.
php_memlimit::install_global_allocator!();

pub mod backend;
pub mod config;
pub mod cookie;
pub mod extensions;
pub mod ffi;
pub mod functions;
pub mod handler;
pub mod headers;
pub mod multipart;
pub mod request;
pub mod vars;
