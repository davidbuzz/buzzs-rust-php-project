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
pub mod fault;
pub mod globals;
pub mod packet;
pub mod cache;
pub mod client;
pub mod http;
pub mod typemap;

/// `zend_zval_type_name` of a value (used in error messages).
#[must_use]
pub fn client_type_name(v: &php_runtime::Value) -> &'static str {
    client::zend_type_name(v)
}

/// The PHP version reported in the default `User-Agent`.
pub const PHP_VERSION: &str = "8.5.0-dev";
