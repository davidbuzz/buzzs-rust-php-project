//! libxml2 declarations that `php_libxml::ffi` does not (yet) provide:
//! the `XPath` evaluator, the tree builders `SimpleXML` uses
//! (`xmlNewTextChild`, `xmlNewChild`, `xmlNewProp`, …) and the plain
//! `xmlRead*` entry points. Declarations only.
//!
//! Candidates for upstreaming into `php_libxml::ffi`.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{c_char, c_int, c_void};

use php_libxml::ffi::{xmlAttr, xmlChar, xmlDoc, xmlNode, xmlNs, xmlOutputBuffer};

/// The leading fields of `xmlXPathContext` that `SimpleXML` writes (offsets
/// checked against the 2.9.14 headers: `node` at 8, `namespaces` at 80,
/// `nsNr` at 88). Only ever used through a pointer to a context allocated
/// by `xmlXPathNewContext`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathContext {
    pub doc: *mut xmlDoc,
    pub node: *mut xmlNode,
    _unused: [u64; 8],
    pub namespaces: *mut *mut xmlNs,
    pub nsNr: c_int,
}

/// `xmlNodeSet`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNodeSet {
    pub nodeNr: c_int,
    pub nodeMax: c_int,
    pub nodeTab: *mut *mut xmlNode,
}

/// The leading fields of `xmlXPathObject`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathObject {
    pub type_: c_int,
    pub nodesetval: *mut xmlNodeSet,
    pub boolval: c_int,
    pub floatval: f64,
    pub stringval: *mut xmlChar,
}

/// `XPATH_NODESET`.
pub const XPATH_NODESET: c_int = 1;
/// `XPATH_BOOLEAN`.
pub const XPATH_BOOLEAN: c_int = 2;
/// `XPATH_NUMBER`.
pub const XPATH_NUMBER: c_int = 3;
/// `XPATH_STRING`.
pub const XPATH_STRING: c_int = 4;

const _: () = {
    assert!(std::mem::offset_of!(xmlXPathContext, node) == 8);
    assert!(std::mem::offset_of!(xmlXPathContext, namespaces) == 80);
    assert!(std::mem::offset_of!(xmlXPathContext, nsNr) == 88);
    assert!(std::mem::offset_of!(xmlXPathObject, nodesetval) == 8);
    assert!(std::mem::offset_of!(xmlNodeSet, nodeTab) == 8);
};

#[link(name = "xml2")]
unsafe extern "C" {
    // Parsing.
    pub fn xmlReadMemory(
        buffer: *const c_char,
        size: c_int,
        url: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc;
    pub fn xmlReadFile(
        filename: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc;
    pub fn xmlCopyDoc(doc: *mut xmlDoc, recursive: c_int) -> *mut xmlDoc;

    // Tree builders.
    pub fn xmlNewTextChild(
        parent: *mut xmlNode,
        ns: *mut xmlNs,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewChild(
        parent: *mut xmlNode,
        ns: *mut xmlNs,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewProp(
        node: *mut xmlNode,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlNewNsProp(
        node: *mut xmlNode,
        ns: *mut xmlNs,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlEncodeEntitiesReentrant(doc: *mut xmlDoc, input: *const xmlChar) -> *mut xmlChar;
    pub fn xmlNodeListGetString(
        doc: *mut xmlDoc,
        list: *const xmlNode,
        in_line: c_int,
    ) -> *mut xmlChar;
    pub fn xmlSplitQName2(name: *const xmlChar, prefix: *mut *mut xmlChar) -> *mut xmlChar;
    pub fn xmlGetNsList(doc: *const xmlDoc, node: *const xmlNode) -> *mut *mut xmlNs;

    // XPath.
    pub fn xmlXPathNewContext(doc: *mut xmlDoc) -> *mut xmlXPathContext;
    pub fn xmlXPathFreeContext(ctxt: *mut xmlXPathContext);
    pub fn xmlXPathRegisterNs(
        ctxt: *mut xmlXPathContext,
        prefix: *const xmlChar,
        ns_uri: *const xmlChar,
    ) -> c_int;
    pub fn xmlXPathEval(expr: *const xmlChar, ctxt: *mut xmlXPathContext) -> *mut xmlXPathObject;
    pub fn xmlXPathFreeObject(obj: *mut xmlXPathObject);

    // Output.
    pub fn xmlSaveFormatFileEnc(
        filename: *const c_char,
        doc: *mut xmlDoc,
        encoding: *const c_char,
        format: c_int,
    ) -> c_int;
    pub fn xmlOutputBufferCreateFilename(
        uri: *const c_char,
        encoder: *mut c_void,
        compression: c_int,
    ) -> *mut xmlOutputBuffer;
}
