//! PHP's `ext/mbstring` for phprust: libmbfl's encodings and the `mb_*`
//! string functions, as plain Rust APIs over byte strings.
//!
//! The VM wraps these: encodings are passed by name, results are byte
//! strings / integers, PHP exceptions are reported as [`MbError`] values
//! carrying PHP's exact messages, and warnings/deprecations are pushed to
//! [`MbState::diagnostics`].
//!
//! Ref: `ext/mbstring/` (`mbstring.c`, `php_unicode.c`, `libmbfl/`).

pub mod codecs;
pub mod convert;
pub mod encoding;
pub mod error;
pub mod legacy;
pub mod tables;

pub use convert::{ConvertBuf, ErrorMode, BAD_INPUT};
pub use encoding::{Encoding, EncodingNo};
pub use error::{Diagnostic, Level, MbError, MbResult};
