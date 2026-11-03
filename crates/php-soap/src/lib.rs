//! PHP's ext/soap for phprust.
//!
//! * [`xml`] — the XML tree ext/soap works on, with libxml2-compatible
//!   serialization; documents are parsed by the system libxml2.
//! * [`zhash`] — Zend-like ordered maps used by the SDL model.
//! * [`consts`] — namespaces, type codes and PHP constants.

pub mod consts;
pub mod xml;
pub mod zhash;
pub mod encoders;
pub mod error;
pub mod schema;
pub mod sdl;
pub mod wsdl;
pub mod sdl_print;
pub mod charset;
pub mod encoding;
pub mod host;
pub mod state;
