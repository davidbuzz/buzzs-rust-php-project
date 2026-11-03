//! PHP's `ext/exif` (work in progress): tag tables, diagnostics, stream
//! model, encoding conversion, image identification and `getimagesize`.

mod avif;
pub mod diag;
pub mod encoding;
pub mod exif;
pub mod fmt;
pub mod imagesize;
pub mod imagetype;
mod inflate;
pub mod stream;
pub mod tags;
