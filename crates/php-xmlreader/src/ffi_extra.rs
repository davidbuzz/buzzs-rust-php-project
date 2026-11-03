//! libxml2 declarations that `php-libxml`'s [`php_libxml::ffi`] does not
//! (yet) provide: the `xmlTextReader` API, Relax-NG schema loading and a few
//! helpers. Declarations only; see `reader.rs` for the safe wrappers.
//!
//! Candidates for upstreaming into `php_libxml::ffi`.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{c_char, c_int, c_void};

use php_libxml::ffi::{
    xmlChar, xmlInputCloseCallback, xmlInputReadCallback, xmlNode, xmlParserInputBuffer,
};

/// Opaque `xmlTextReader`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlTextReader {
    _opaque: [u8; 0],
}

/// Opaque `xmlRelaxNG` (a compiled schema).
#[repr(C)]
#[derive(Debug)]
pub struct xmlRelaxNG {
    _opaque: [u8; 0],
}

/// Opaque `xmlRelaxNGParserCtxt`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlRelaxNGParserCtxt {
    _opaque: [u8; 0],
}

#[link(name = "xml2")]
unsafe extern "C" {
    // Construction / destruction.
    pub fn xmlNewTextReader(
        input: *mut xmlParserInputBuffer,
        uri: *const c_char,
    ) -> *mut xmlTextReader;
    pub fn xmlTextReaderSetup(
        reader: *mut xmlTextReader,
        input: *mut xmlParserInputBuffer,
        url: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> c_int;
    pub fn xmlReaderForFile(
        filename: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlTextReader;
    pub fn xmlReaderForIO(
        ioread: xmlInputReadCallback,
        ioclose: xmlInputCloseCallback,
        ioctx: *mut c_void,
        url: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlTextReader;
    pub fn xmlFreeTextReader(reader: *mut xmlTextReader);
    pub fn xmlParserInputBufferCreateMem(
        mem: *const c_char,
        size: c_int,
        enc: c_int,
    ) -> *mut xmlParserInputBuffer;
    pub fn xmlCanonicPath(path: *const xmlChar) -> *mut xmlChar;

    // Cursor movement.
    pub fn xmlTextReaderRead(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderNext(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderMoveToAttribute(reader: *mut xmlTextReader, name: *const xmlChar) -> c_int;
    pub fn xmlTextReaderMoveToAttributeNo(reader: *mut xmlTextReader, no: c_int) -> c_int;
    pub fn xmlTextReaderMoveToAttributeNs(
        reader: *mut xmlTextReader,
        local: *const xmlChar,
        ns: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextReaderMoveToElement(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderMoveToFirstAttribute(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderMoveToNextAttribute(reader: *mut xmlTextReader) -> c_int;

    // Content.
    pub fn xmlTextReaderReadInnerXml(reader: *mut xmlTextReader) -> *mut xmlChar;
    pub fn xmlTextReaderReadOuterXml(reader: *mut xmlTextReader) -> *mut xmlChar;
    pub fn xmlTextReaderReadString(reader: *mut xmlTextReader) -> *mut xmlChar;
    pub fn xmlTextReaderGetAttribute(
        reader: *mut xmlTextReader,
        name: *const xmlChar,
    ) -> *mut xmlChar;
    pub fn xmlTextReaderGetAttributeNo(reader: *mut xmlTextReader, no: c_int) -> *mut xmlChar;
    pub fn xmlTextReaderGetAttributeNs(
        reader: *mut xmlTextReader,
        local: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlChar;
    pub fn xmlTextReaderLookupNamespace(
        reader: *mut xmlTextReader,
        prefix: *const xmlChar,
    ) -> *mut xmlChar;
    pub fn xmlTextReaderExpand(reader: *mut xmlTextReader) -> *mut xmlNode;

    // Properties.
    pub fn xmlTextReaderAttributeCount(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderDepth(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderHasAttributes(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderHasValue(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderIsDefault(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderIsEmptyElement(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderNodeType(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderConstBaseUri(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstLocalName(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstName(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstNamespaceUri(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstPrefix(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstValue(reader: *mut xmlTextReader) -> *const xmlChar;
    pub fn xmlTextReaderConstXmlLang(reader: *mut xmlTextReader) -> *const xmlChar;

    // Parser properties and validation.
    pub fn xmlTextReaderSetParserProp(
        reader: *mut xmlTextReader,
        prop: c_int,
        value: c_int,
    ) -> c_int;
    pub fn xmlTextReaderGetParserProp(reader: *mut xmlTextReader, prop: c_int) -> c_int;
    pub fn xmlTextReaderIsValid(reader: *mut xmlTextReader) -> c_int;
    pub fn xmlTextReaderSchemaValidate(reader: *mut xmlTextReader, xsd: *const c_char) -> c_int;
    pub fn xmlTextReaderRelaxNGSetSchema(
        reader: *mut xmlTextReader,
        schema: *mut xmlRelaxNG,
    ) -> c_int;
    pub fn xmlRelaxNGNewParserCtxt(url: *const c_char) -> *mut xmlRelaxNGParserCtxt;
    pub fn xmlRelaxNGNewMemParserCtxt(
        buffer: *const c_char,
        size: c_int,
    ) -> *mut xmlRelaxNGParserCtxt;
    pub fn xmlRelaxNGParse(ctxt: *mut xmlRelaxNGParserCtxt) -> *mut xmlRelaxNG;
    pub fn xmlRelaxNGFreeParserCtxt(ctxt: *mut xmlRelaxNGParserCtxt);
    pub fn xmlRelaxNGFree(schema: *mut xmlRelaxNG);

    // Tree helpers used by expand().
    pub fn xmlAddChild(parent: *mut xmlNode, cur: *mut xmlNode) -> *mut xmlNode;
}
