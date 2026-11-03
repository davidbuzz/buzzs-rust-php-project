//! PHP's libxml-based extensions on the system libxml2.
//!
//! * [`error`] / [`io`] — `ext/libxml`: the `libxml_*()` functions, the
//!   `LibXMLError` list, external entity loader and stream hooks.
//! * [`tree`] — a safe, reference-counted document/node layer (the shared
//!   base for DOM, `SimpleXML`, `XMLReader` and XSL).
//! * [`xml`] — `ext/xml`: the expat-compatible SAX parser.
//! * [`xmlwriter`] — `ext/xmlwriter`: the `XMLWriter` class.
//! * [`constants`] — the `LIBXML_*` constants.
//!
//! All `unsafe` code is confined to [`ffi`] (declarations) and [`raw`]
//! (sound wrappers); see their module documentation.

pub mod constants;
pub mod error;
pub mod ffi;
pub mod io;
pub mod raw;
pub mod tree;
pub mod xml;
pub mod xmlwriter;
