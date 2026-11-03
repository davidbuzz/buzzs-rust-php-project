//! The remaining "misc" of PHP's `ext/standard` that is not covered by the
//! string/array/stream crates.
//!
//! * [`crypt`] — `crypt()` with every bundled algorithm and the
//!   `password_*` API (bcrypt, Argon2i/Argon2id).
pub mod crypt;
