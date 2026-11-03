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
pub mod detect;
pub mod encoding;
pub mod error;
pub mod info;
pub mod legacy;
pub mod numericentity;
pub mod mbregex;
pub mod mime;
pub mod state;
pub mod strcut;
pub mod strings;
pub mod tables;
pub mod unicode;

pub use convert::{ConvertBuf, ErrorMode, BAD_INPUT};
pub use encoding::{Encoding, EncodingNo};
pub use state::{EncodingList, HttpInput, Language, MbState, SubstituteArg, SubstituteChar};
pub use error::{Diagnostic, Level, MbError, MbResult};
