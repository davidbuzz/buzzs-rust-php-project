//! `ext/libxml`, `ext/xml` and `ext/xmlwriter` for the phprust VM: binds
//! the `php-libxml` library through the `php_vm::ext` extension API.
//!
//! * [`libxml`] — the `libxml_*()` functions, `LibXMLError`, the `LIBXML_*`
//!   constants, the stream I/O hooks and the shared libxml state API used
//!   by every libxml-based binding (DOM, `SimpleXML`, `XMLReader`, XSL, SOAP).
//! * [`xml`] — `ext/xml`: `XMLParser` and the `xml_*()` functions.
//! * [`xmlwriter`] — `ext/xmlwriter`: the `XMLWriter` class and the
//!   procedural `xmlwriter_*()` functions.
//!
//! The single [`Xml`] extension registers all three modules and reports
//! `libxml`, `xml` and `xmlwriter` as loaded extensions.

// Native handlers have the fixed `NativeFn` / hook signatures, so many
// infallible ones still return `VmResult`.
#![allow(clippy::unnecessary_wraps)]

pub mod libxml;
pub mod xml;
pub mod xmlwriter;

use php_vm::ext::{Extension, ModuleBuilder};

/// The `libxml` + `xml` + `xmlwriter` extensions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Xml;

impl Extension for Xml {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        {
            let loaded = &mut m.vm().loaded_extensions;
            let has = |l: &[String], n: &str| l.iter().any(|e| e.eq_ignore_ascii_case(n));
            if !has(loaded, "libxml") {
                let at = loaded
                    .iter()
                    .position(|e| e.eq_ignore_ascii_case("xml"))
                    .unwrap_or(loaded.len());
                loaded.insert(at, "libxml".to_owned());
            }
            if !has(loaded, "xmlwriter") {
                loaded.push("xmlwriter".to_owned());
            }
        }
        libxml::register(m);
        xml::register(m);
        xmlwriter::register(m);
    }
}
