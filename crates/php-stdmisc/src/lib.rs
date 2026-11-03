//! The remaining "misc" of PHP's `ext/standard` that is not covered by the
//! string/array/stream crates.
//!
//! * [`crypt`] — `crypt()` with every bundled algorithm and the
//!   `password_*` API (bcrypt, Argon2i/Argon2id).
//! * [`exec`] — `escapeshellarg`/`escapeshellcmd`, `exec`/`system`/
//!   `passthru`/`shell_exec`, `proc_nice`.
//! * [`process`] — `proc_open`/`proc_close`/`proc_get_status`/
//!   `proc_terminate`.
//!
//! All `unsafe` code is confined to [`sys`].

pub mod crypt;
pub mod exec;
pub mod locale;
pub mod process;
pub mod sys;
