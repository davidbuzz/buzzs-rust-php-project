//! The remaining "misc" of PHP's `ext/standard` that is not covered by the
//! string/array/stream crates.
//!
//! * [`crypt`] — `crypt()` with every bundled algorithm and the
//!   `password_*` API (bcrypt, Argon2i/Argon2id).
//! * [`exec`] — `escapeshellarg`/`escapeshellcmd`, `exec`/`system`/
//!   `passthru`/`shell_exec`, `proc_nice`.
//! * [`process`] — `proc_open`/`proc_close`/`proc_get_status`/
//!   `proc_terminate`.
//! * [`query`] — `http_build_query`, `parse_str` and the SAPI's request
//!   variable registration (`php_register_variable_ex`).
//! * [`highlight`] — `highlight_string`/`highlight_file` HTML and
//!   `php_strip_whitespace`.
//! * [`locale`] — `setlocale`, `localeconv`, `nl_langinfo`.
//!
//! All `unsafe` code is confined to [`sys`].

pub mod crypt;
pub mod exec;
pub mod highlight;
pub mod locale;
pub mod process;
pub mod query;
pub mod sys;
