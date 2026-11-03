//! libxml2 declarations used by this crate that `php_libxml::ffi` does not
//! provide (yet). Structure layouts were checked against the libxml2
//! 2.9.14 headers with a C program (LP64); see the `layout` assertions.
//!
//! These are raw declarations only; candidates for upstreaming into
//! `php-libxml`.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::upper_case_acronyms,
    clippy::pub_underscore_fields
)]

use std::ffi::{c_char, c_int, c_long, c_uint, c_void};

pub use php_libxml::ffi::{
    xmlAttr, xmlChar, xmlDoc, xmlDtd, xmlEntity, xmlError, xmlNode, xmlNs, xmlOutputBuffer,
    xmlParserCtxt, xmlParserInput, xmlSAXHandler, xmlStructuredErrorFunc, XML_ATTRIBUTE_DECL,
    XML_ATTRIBUTE_NODE, XML_CDATA_SECTION_NODE, XML_COMMENT_NODE, XML_DOCUMENT_FRAG_NODE,
    XML_DOCUMENT_NODE, XML_DOCUMENT_TYPE_NODE, XML_DTD_NODE, XML_ELEMENT_DECL, XML_ELEMENT_NODE,
    XML_ENTITY_DECL, XML_ENTITY_NODE, XML_ENTITY_REF_NODE, XML_HTML_DOCUMENT_NODE,
    XML_INTERNAL_PREDEFINED_ENTITY, XML_LOCAL_NAMESPACE, XML_NAMESPACE_DECL, XML_NOTATION_NODE,
    XML_PI_NODE, XML_TEXT_NODE,
};

/// Opaque hash table.
pub type xmlHashTable = c_void;
/// Opaque dictionary.
pub type xmlDict = c_void;

/// `xmlNodeSet`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNodeSet {
    pub nodeNr: c_int,
    pub nodeMax: c_int,
    pub nodeTab: *mut *mut xmlNode,
}

/// `xmlXPathObject`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathObject {
    pub type_: c_int,
    pub nodesetval: *mut xmlNodeSet,
    pub boolval: c_int,
    pub floatval: f64,
    pub stringval: *mut xmlChar,
    pub user: *mut c_void,
    pub index: c_int,
    pub user2: *mut c_void,
    pub index2: c_int,
}

pub type xmlXPathFunction = Option<unsafe extern "C" fn(*mut xmlXPathParserContext, c_int)>;
pub type xmlXPathFuncLookupFunc =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, *const xmlChar) -> xmlXPathFunction>;

/// `xmlXPathContext` (only the fields we touch are typed precisely).
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathContext {
    pub doc: *mut xmlDoc,
    pub node: *mut xmlNode,
    pub nb_variables_unused: c_int,
    pub max_variables_unused: c_int,
    pub varHash: *mut c_void,
    pub nb_types: c_int,
    pub max_types: c_int,
    pub types: *mut c_void,
    pub nb_funcs_unused: c_int,
    pub max_funcs_unused: c_int,
    pub funcHash: *mut c_void,
    pub nb_axis: c_int,
    pub max_axis: c_int,
    pub axis: *mut c_void,
    pub namespaces: *mut *mut xmlNs,
    pub nsNr: c_int,
    pub user: *mut c_void,
    pub contextSize: c_int,
    pub proximityPosition: c_int,
    pub xptr: c_int,
    pub here: *mut xmlNode,
    pub origin: *mut xmlNode,
    pub nsHash: *mut c_void,
    pub varLookupFunc: *mut c_void,
    pub varLookupData: *mut c_void,
    pub extra: *mut c_void,
    pub function: *const xmlChar,
    pub functionURI: *const xmlChar,
    pub funcLookupFunc: xmlXPathFuncLookupFunc,
    pub funcLookupData: *mut c_void,
    pub tmpNsList: *mut *mut xmlNs,
    pub tmpNsNr: c_int,
    pub userData: *mut c_void,
    pub error: xmlStructuredErrorFunc,
    pub lastError: xmlError,
    pub debugNode: *mut xmlNode,
    pub dict: *mut xmlDict,
    pub flags: c_int,
    pub cache: *mut c_void,
    pub opLimit: std::ffi::c_ulong,
    pub opCount: std::ffi::c_ulong,
    pub depth: c_int,
}

/// `xmlXPathParserContext`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathParserContext {
    pub cur: *const xmlChar,
    pub base: *const xmlChar,
    pub error: c_int,
    pub context: *mut xmlXPathContext,
    pub value: *mut xmlXPathObject,
    pub valueNr: c_int,
    pub valueMax: c_int,
    pub valueTab: *mut *mut xmlXPathObject,
    pub comp: *mut c_void,
    pub xptr: c_int,
    pub ancestor: *mut xmlNode,
    pub valueFrame: c_int,
}

/// `xmlNotation`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNotation {
    pub name: *const xmlChar,
    pub PublicID: *const xmlChar,
    pub SystemID: *const xmlChar,
}

/// `xmlAttribute` (attribute declaration).
#[repr(C)]
#[derive(Debug)]
pub struct xmlAttribute {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlDtd,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub nexth: *mut xmlAttribute,
    pub atype: c_int,
    pub def: c_int,
    pub defaultValue: *const xmlChar,
    pub tree: *mut c_void,
    pub prefix: *const xmlChar,
    pub elem: *const xmlChar,
}

/// `xmlOutputBuffer` with its public fields.
#[repr(C)]
#[derive(Debug)]
pub struct xmlOutputBufferFields {
    pub context: *mut c_void,
    pub writecallback: *mut c_void,
    pub closecallback: *mut c_void,
    pub encoder: *mut c_void,
    pub buffer: *mut c_void,
    pub conv: *mut c_void,
    pub written: c_int,
    pub error: c_int,
}

/// Opaque handles.
#[repr(C)]
#[derive(Debug)]
pub struct xmlValidCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlSchemaParserCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlSchema {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlSchemaValidCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlRelaxNGParserCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlRelaxNG {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlRelaxNGValidCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlDOMWrapCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlBuffer {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlCharEncodingHandler {
    pub name: *mut c_char,
}

pub type xmlHashScanner = Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *const xmlChar)>;
pub type xmlOutputWriteCallback =
    Option<unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int>;
pub type xmlOutputCloseCallback = Option<unsafe extern "C" fn(*mut c_void) -> c_int>;
pub type xmlMallocFunc = Option<unsafe extern "C" fn(usize) -> *mut c_void>;
pub type xmlFreeFunc = Option<unsafe extern "C" fn(*mut c_void)>;

// xmlXPathObjectType
pub const XPATH_UNDEFINED: c_int = 0;
pub const XPATH_NODESET: c_int = 1;
pub const XPATH_BOOLEAN: c_int = 2;
pub const XPATH_NUMBER: c_int = 3;
pub const XPATH_STRING: c_int = 4;
pub const XPATH_POINT: c_int = 5;
pub const XPATH_RANGE: c_int = 6;
pub const XPATH_LOCATIONSET: c_int = 7;
pub const XPATH_USERS: c_int = 8;
pub const XPATH_XSLT_TREE: c_int = 9;

// xmlXPathError
pub const XPATH_INVALID_TYPE: c_int = 11;
pub const XPATH_UNKNOWN_FUNC_ERROR: c_int = 13;
pub const XPATH_INVALID_ARITY: c_int = 12;

// xmlAttributeType
pub const XML_ATTRIBUTE_CDATA: c_int = 1;
pub const XML_ATTRIBUTE_ID: c_int = 2;
pub const XML_ATTRIBUTE_IDREF: c_int = 3;
pub const XML_ATTRIBUTE_IDREFS: c_int = 4;
pub const XML_ATTRIBUTE_ENTITY: c_int = 5;
pub const XML_ATTRIBUTE_ENTITIES: c_int = 6;
pub const XML_ATTRIBUTE_NMTOKEN: c_int = 7;
pub const XML_ATTRIBUTE_NMTOKENS: c_int = 8;
pub const XML_ATTRIBUTE_ENUMERATION: c_int = 9;
pub const XML_ATTRIBUTE_NOTATION: c_int = 10;

// Entity types.
pub const XML_INTERNAL_GENERAL_ENTITY: c_int = 1;
pub const XML_EXTERNAL_GENERAL_PARSED_ENTITY: c_int = 2;
pub const XML_EXTERNAL_GENERAL_UNPARSED_ENTITY: c_int = 3;
pub const XML_INTERNAL_PARAMETER_ENTITY: c_int = 4;
pub const XML_EXTERNAL_PARAMETER_ENTITY: c_int = 5;

// Parser options.
pub use php_libxml::ffi::{
    XML_PARSE_DTDATTR, XML_PARSE_DTDLOAD, XML_PARSE_DTDVALID, XML_PARSE_HUGE, XML_PARSE_NOBLANKS,
    XML_PARSE_NOCDATA, XML_PARSE_NOENT, XML_PARSE_NOERROR, XML_PARSE_NONET, XML_PARSE_NOWARNING,
    XML_PARSE_NOXINCNODE, XML_PARSE_NSCLEAN, XML_PARSE_RECOVER,
};
pub const XML_PARSE_BIG_LINES: c_int = 1 << 22;
pub const XML_PARSE_COMPACT: c_int = 1 << 16;
pub const XML_PARSE_OLD10: c_int = 1 << 17;
pub const XML_PARSE_NOBASEFIX: c_int = 1 << 18;
pub const XML_PARSE_PEDANTIC: c_int = 1 << 7;

pub const HTML_PARSE_RECOVER: c_int = 1 << 0;
pub const HTML_PARSE_NODEFDTD: c_int = 1 << 2;
pub const HTML_PARSE_NOERROR: c_int = 1 << 5;
pub const HTML_PARSE_NOWARNING: c_int = 1 << 6;
pub const HTML_PARSE_NOIMPLIED: c_int = 1 << 13;

// Save options.
pub const XML_SAVE_FORMAT: c_int = 1 << 0;
pub const XML_SAVE_NO_DECL: c_int = 1 << 1;
pub const XML_SAVE_NO_EMPTY: c_int = 1 << 2;
pub const XML_SAVE_NO_XHTML: c_int = 1 << 3;
pub const XML_SAVE_XHTML: c_int = 1 << 4;
pub const XML_SAVE_AS_XML: c_int = 1 << 5;
pub const XML_SAVE_AS_HTML: c_int = 1 << 6;

pub const XML_XML_NAMESPACE: &[u8] = b"http://www.w3.org/XML/1998/namespace";

// Schema validity options
pub const XML_SCHEMA_VAL_VC_I_CREATE: c_int = 1;

// c14n modes
pub const XML_C14N_1_0: c_int = 0;
pub const XML_C14N_EXCLUSIVE_1_0: c_int = 1;

#[link(name = "xml2")]
extern "C" {
    pub static xmlMalloc: xmlMallocFunc;
    pub static xmlFree: xmlFreeFunc;
    pub static xmlStringText: [xmlChar; 5];
    pub fn __xmlStructuredError() -> *mut xmlStructuredErrorFunc;
    pub fn __xmlStructuredErrorContext() -> *mut *mut c_void;
    pub fn __xmlSaveNoEmptyTags() -> *mut c_int;
    pub fn __xmlIndentTreeOutput() -> *mut c_int;
    pub fn __xmlTreeIndentString() -> *mut *const c_char;
    pub fn __xmlKeepBlanksDefaultValue() -> *mut c_int;
    pub fn xmlGetLastError() -> *mut xmlError;

    // strings
    pub fn xmlStrdup(s: *const xmlChar) -> *mut xmlChar;
    pub fn xmlStrndup(s: *const xmlChar, len: c_int) -> *mut xmlChar;
    pub fn xmlStrEqual(a: *const xmlChar, b: *const xmlChar) -> c_int;
    pub fn xmlStrlen(s: *const xmlChar) -> c_int;
    pub fn xmlStrcat(a: *mut xmlChar, b: *const xmlChar) -> *mut xmlChar;
    pub fn xmlSplitQName3(name: *const xmlChar, len: *mut c_int) -> *const xmlChar;
    pub fn xmlSplitQName2(name: *const xmlChar, prefix: *mut *mut xmlChar) -> *mut xmlChar;
    pub fn xmlValidateQName(value: *const xmlChar, space: c_int) -> c_int;
    pub fn xmlValidateName(value: *const xmlChar, space: c_int) -> c_int;
    pub fn xmlValidateNCName(value: *const xmlChar, space: c_int) -> c_int;
    pub fn xmlGetUTF8Char(utf: *const u8, len: *mut c_int) -> c_int;
    pub fn xmlCheckUTF8(utf: *const u8) -> c_int;
    pub fn xmlUTF8Strlen(utf: *const xmlChar) -> c_int;
    pub fn xmlUTF8Strsub(utf: *const xmlChar, start: c_int, len: c_int) -> *mut xmlChar;
    pub fn xmlUTF8Strndup(utf: *const xmlChar, len: c_int) -> *mut xmlChar;
    pub fn xmlTextConcat(node: *mut xmlNode, content: *const xmlChar, len: c_int) -> c_int;
    pub fn xmlDictCreate() -> *mut xmlDict;
    pub fn xmlDictFree(dict: *mut xmlDict);
    pub fn xmlDictOwns(dict: *mut xmlDict, s: *const xmlChar) -> c_int;
    pub fn xmlDictLookup(dict: *mut xmlDict, name: *const xmlChar, len: c_int) -> *const xmlChar;
    pub fn xmlDictReference(dict: *mut xmlDict) -> c_int;

    // hash
    pub fn xmlHashLookup(table: *mut xmlHashTable, name: *const xmlChar) -> *mut c_void;
    pub fn xmlHashScan(table: *mut xmlHashTable, f: xmlHashScanner, data: *mut c_void);
    pub fn xmlHashSize(table: *mut xmlHashTable) -> c_int;
    pub fn xmlHashRemoveEntry(
        table: *mut xmlHashTable,
        name: *const xmlChar,
        f: Option<unsafe extern "C" fn(*mut c_void, *const xmlChar)>,
    ) -> c_int;

    // tree construction
    pub fn xmlNewDoc(version: *const xmlChar) -> *mut xmlDoc;
    pub fn xmlFreeDoc(doc: *mut xmlDoc);
    pub fn xmlCopyDoc(doc: *mut xmlDoc, recursive: c_int) -> *mut xmlDoc;
    pub fn xmlNewNode(ns: *mut xmlNs, name: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewDocNode(
        doc: *mut xmlDoc,
        ns: *mut xmlNs,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewDocRawNode(
        doc: *mut xmlDoc,
        ns: *mut xmlNs,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewText(content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewTextLen(content: *const xmlChar, len: c_int) -> *mut xmlNode;
    pub fn xmlNewDocText(doc: *const xmlDoc, content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewDocTextLen(doc: *mut xmlDoc, content: *const xmlChar, len: c_int) -> *mut xmlNode;
    pub fn xmlNewDocComment(doc: *mut xmlDoc, content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewComment(content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewCDataBlock(doc: *mut xmlDoc, content: *const xmlChar, len: c_int) -> *mut xmlNode;
    pub fn xmlNewDocPI(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewPI(name: *const xmlChar, content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewReference(doc: *const xmlDoc, name: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewCharRef(doc: *mut xmlDoc, name: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewDocFragment(doc: *mut xmlDoc) -> *mut xmlNode;
    pub fn xmlNewDocProp(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
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
    pub fn xmlNewDtd(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        external_id: *const xmlChar,
        system_id: *const xmlChar,
    ) -> *mut xmlDtd;
    pub fn xmlCreateIntSubset(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        external_id: *const xmlChar,
        system_id: *const xmlChar,
    ) -> *mut xmlDtd;
    pub fn xmlGetIntSubset(doc: *const xmlDoc) -> *mut xmlDtd;
    pub fn xmlCopyDtd(dtd: *mut xmlDtd) -> *mut xmlDtd;
    pub fn xmlFreeDtd(dtd: *mut xmlDtd);
    pub fn xmlNewNs(node: *mut xmlNode, href: *const xmlChar, prefix: *const xmlChar)
        -> *mut xmlNs;
    pub fn xmlFreeNs(ns: *mut xmlNs);
    pub fn xmlFreeNsList(ns: *mut xmlNs);
    pub fn xmlSearchNs(doc: *mut xmlDoc, node: *mut xmlNode, prefix: *const xmlChar) -> *mut xmlNs;
    pub fn xmlSearchNsByHref(
        doc: *mut xmlDoc,
        node: *mut xmlNode,
        href: *const xmlChar,
    ) -> *mut xmlNs;
    pub fn xmlGetNsList(doc: *const xmlDoc, node: *const xmlNode) -> *mut *mut xmlNs;
    pub fn xmlSetNs(node: *mut xmlNode, ns: *mut xmlNs);
    pub fn xmlReconciliateNs(doc: *mut xmlDoc, tree: *mut xmlNode) -> c_int;

    // tree manipulation
    pub fn xmlAddChild(parent: *mut xmlNode, cur: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlAddChildList(parent: *mut xmlNode, cur: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlAddPrevSibling(cur: *mut xmlNode, elem: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlAddNextSibling(cur: *mut xmlNode, elem: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlReplaceNode(old: *mut xmlNode, cur: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlUnlinkNode(node: *mut xmlNode);
    pub fn xmlFreeNode(node: *mut xmlNode);
    pub fn xmlFreeNodeList(node: *mut xmlNode);
    pub fn xmlFreeProp(attr: *mut xmlAttr);
    pub fn xmlRemoveProp(attr: *mut xmlAttr) -> c_int;
    pub fn xmlDocGetRootElement(doc: *const xmlDoc) -> *mut xmlNode;
    pub fn xmlDocSetRootElement(doc: *mut xmlDoc, root: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlSetTreeDoc(tree: *mut xmlNode, doc: *mut xmlDoc);
    pub fn xmlSetListDoc(list: *mut xmlNode, doc: *mut xmlDoc);
    pub fn xmlDocCopyNode(node: *mut xmlNode, doc: *mut xmlDoc, extended: c_int) -> *mut xmlNode;
    pub fn xmlDocCopyNodeList(doc: *mut xmlDoc, node: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlCopyProp(target: *mut xmlNode, cur: *mut xmlAttr) -> *mut xmlAttr;
    pub fn xmlTextMerge(first: *mut xmlNode, second: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlNodeSetName(node: *mut xmlNode, name: *const xmlChar);
    pub fn xmlNodeGetContent(node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlNodeSetContent(node: *mut xmlNode, content: *const xmlChar);
    pub fn xmlNodeSetContentLen(node: *mut xmlNode, content: *const xmlChar, len: c_int);
    pub fn xmlNodeAddContent(node: *mut xmlNode, content: *const xmlChar);
    pub fn xmlNodeAddContentLen(node: *mut xmlNode, content: *const xmlChar, len: c_int);
    pub fn xmlNodeListGetString(
        doc: *mut xmlDoc,
        list: *const xmlNode,
        inline: c_int,
    ) -> *mut xmlChar;
    pub fn xmlStringGetNodeList(doc: *const xmlDoc, value: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNodeGetBase(doc: *const xmlDoc, node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlGetNodePath(node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlGetLineNo(node: *const xmlNode) -> c_long;
    pub fn xmlIsBlankNode(node: *const xmlNode) -> c_int;
    pub fn xmlHasProp(node: *const xmlNode, name: *const xmlChar) -> *mut xmlAttr;
    pub fn xmlHasNsProp(
        node: *const xmlNode,
        name: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlGetProp(node: *const xmlNode, name: *const xmlChar) -> *mut xmlChar;
    pub fn xmlGetNsProp(
        node: *const xmlNode,
        name: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlChar;
    pub fn xmlGetNoNsProp(node: *const xmlNode, name: *const xmlChar) -> *mut xmlChar;
    pub fn xmlSetProp(
        node: *mut xmlNode,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlSetNsProp(
        node: *mut xmlNode,
        ns: *mut xmlNs,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlUnsetNsProp(node: *mut xmlNode, ns: *mut xmlNs, name: *const xmlChar) -> c_int;
    pub fn xmlEncodeEntitiesReentrant(doc: *mut xmlDoc, input: *const xmlChar) -> *mut xmlChar;
    pub fn xmlEncodeSpecialChars(doc: *const xmlDoc, input: *const xmlChar) -> *mut xmlChar;

    // DOM wrap
    pub fn xmlDOMWrapNewCtxt() -> *mut xmlDOMWrapCtxt;
    pub fn xmlDOMWrapFreeCtxt(ctxt: *mut xmlDOMWrapCtxt);
    pub fn xmlDOMWrapAdoptNode(
        ctxt: *mut xmlDOMWrapCtxt,
        source_doc: *mut xmlDoc,
        node: *mut xmlNode,
        dest_doc: *mut xmlDoc,
        dest_parent: *mut xmlNode,
        options: c_int,
    ) -> c_int;

    // ids / entities / dtd
    pub fn xmlGetID(doc: *mut xmlDoc, id: *const xmlChar) -> *mut xmlAttr;
    pub fn xmlAddID(
        ctxt: *mut xmlValidCtxt,
        doc: *mut xmlDoc,
        value: *const xmlChar,
        attr: *mut xmlAttr,
    ) -> *mut c_void;
    pub fn xmlRemoveID(doc: *mut xmlDoc, attr: *mut xmlAttr) -> c_int;
    pub fn xmlIsID(doc: *mut xmlDoc, elem: *mut xmlNode, attr: *mut xmlAttr) -> c_int;
    pub fn xmlGetDocEntity(doc: *const xmlDoc, name: *const xmlChar) -> *mut xmlEntity;
    pub fn xmlGetPredefinedEntity(name: *const xmlChar) -> *mut xmlEntity;
    pub fn xmlGetDtdEntity(doc: *mut xmlDoc, name: *const xmlChar) -> *mut xmlEntity;

    // parsing
    pub fn xmlCreateMemoryParserCtxt(buffer: *const c_char, size: c_int) -> *mut xmlParserCtxt;
    pub fn xmlCreateFileParserCtxt(filename: *const c_char) -> *mut xmlParserCtxt;
    pub fn xmlCreateURLParserCtxt(filename: *const c_char, options: c_int) -> *mut xmlParserCtxt;
    pub fn xmlCtxtUseOptions(ctxt: *mut xmlParserCtxt, options: c_int) -> c_int;
    pub fn xmlParseDocument(ctxt: *mut xmlParserCtxt) -> c_int;
    pub fn xmlFreeParserCtxt(ctxt: *mut xmlParserCtxt);
    pub fn xmlSwitchToEncoding(
        ctxt: *mut xmlParserCtxt,
        handler: *mut xmlCharEncodingHandler,
    ) -> c_int;
    pub fn xmlParseBalancedChunkMemory(
        doc: *mut xmlDoc,
        sax: *mut xmlSAXHandler,
        user_data: *mut c_void,
        depth: c_int,
        string: *const xmlChar,
        lst: *mut *mut xmlNode,
    ) -> c_int;
    pub fn xmlParseInNodeContext(
        node: *mut xmlNode,
        data: *const c_char,
        datalen: c_int,
        options: c_int,
        lst: *mut *mut xmlNode,
    ) -> c_int;
    pub fn xmlCanonicPath(path: *const xmlChar) -> *mut xmlChar;
    pub fn xmlBuildURI(uri: *const xmlChar, base: *const xmlChar) -> *mut xmlChar;
    pub fn xmlPathToURI(path: *const xmlChar) -> *mut xmlChar;
    pub fn xmlCreateURI() -> *mut c_void;
    pub fn xmlFreeURI(uri: *mut c_void);
    pub fn xmlParseURIReference(uri: *mut c_void, s: *const c_char) -> c_int;
    pub fn xmlURIEscapeStr(s: *const xmlChar, list: *const xmlChar) -> *mut xmlChar;
    pub fn xmlFindCharEncodingHandler(name: *const c_char) -> *mut xmlCharEncodingHandler;
    pub fn xmlCharEncCloseFunc(handler: *mut xmlCharEncodingHandler) -> c_int;
    pub fn xmlParseCharEncoding(name: *const c_char) -> c_int;
    pub fn xmlGetCharEncodingName(enc: c_int) -> *const c_char;

    // HTML
    pub fn htmlCreateMemoryParserCtxt(buffer: *const c_char, size: c_int) -> *mut xmlParserCtxt;
    pub fn htmlCreateFileParserCtxt(
        filename: *const c_char,
        encoding: *const c_char,
    ) -> *mut xmlParserCtxt;
    pub fn htmlCtxtUseOptions(ctxt: *mut xmlParserCtxt, options: c_int) -> c_int;
    pub fn htmlParseDocument(ctxt: *mut xmlParserCtxt) -> c_int;
    pub fn htmlFreeParserCtxt(ctxt: *mut xmlParserCtxt);
    pub fn htmlNewDoc(uri: *const xmlChar, external_id: *const xmlChar) -> *mut xmlDoc;
    pub fn htmlNewDocNoDtD(uri: *const xmlChar, external_id: *const xmlChar) -> *mut xmlDoc;
    pub fn htmlDocDumpMemoryFormat(
        doc: *mut xmlDoc,
        mem: *mut *mut xmlChar,
        size: *mut c_int,
        format: c_int,
    );
    pub fn htmlNodeDumpFormatOutput(
        buf: *mut xmlOutputBuffer,
        doc: *mut xmlDoc,
        cur: *mut xmlNode,
        encoding: *const c_char,
        format: c_int,
    );
    pub fn htmlDocContentDumpFormatOutput(
        buf: *mut xmlOutputBuffer,
        cur: *mut xmlDoc,
        encoding: *const c_char,
        format: c_int,
    );
    pub fn htmlSaveFileFormat(
        filename: *const c_char,
        cur: *mut xmlDoc,
        encoding: *const c_char,
        format: c_int,
    ) -> c_int;
    pub fn htmlSetMetaEncoding(doc: *mut xmlDoc, encoding: *const xmlChar) -> c_int;
    pub fn htmlGetMetaEncoding(doc: *mut xmlDoc) -> *const xmlChar;
    pub fn htmlIsBooleanAttr(name: *const xmlChar) -> c_int;

    // output
    pub fn xmlOutputBufferCreateIO(
        iowrite: xmlOutputWriteCallback,
        ioclose: xmlOutputCloseCallback,
        ioctx: *mut c_void,
        encoder: *mut xmlCharEncodingHandler,
    ) -> *mut xmlOutputBuffer;
    pub fn xmlOutputBufferCreateFilename(
        uri: *const c_char,
        encoder: *mut xmlCharEncodingHandler,
        compression: c_int,
    ) -> *mut xmlOutputBuffer;
    pub fn xmlOutputBufferClose(out: *mut xmlOutputBuffer) -> c_int;
    pub fn xmlOutputBufferFlush(out: *mut xmlOutputBuffer) -> c_int;
    pub fn xmlOutputBufferWrite(out: *mut xmlOutputBuffer, len: c_int, buf: *const c_char)
        -> c_int;
    pub fn xmlNodeDumpOutput(
        buf: *mut xmlOutputBuffer,
        doc: *mut xmlDoc,
        cur: *mut xmlNode,
        level: c_int,
        format: c_int,
        encoding: *const c_char,
    );
    pub fn xmlSaveToIO(
        iowrite: xmlOutputWriteCallback,
        ioclose: xmlOutputCloseCallback,
        ioctx: *mut c_void,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut c_void;
    pub fn xmlSaveDoc(ctxt: *mut c_void, doc: *mut xmlDoc) -> c_long;
    pub fn xmlSaveTree(ctxt: *mut c_void, node: *mut xmlNode) -> c_long;
    pub fn xmlSaveClose(ctxt: *mut c_void) -> c_int;
    pub fn xmlSaveFlush(ctxt: *mut c_void) -> c_int;
    pub fn xmlSaveFormatFileEnc(
        filename: *const c_char,
        cur: *mut xmlDoc,
        encoding: *const c_char,
        format: c_int,
    ) -> c_int;
    pub fn xmlSaveFormatFileTo(
        buf: *mut xmlOutputBuffer,
        cur: *mut xmlDoc,
        encoding: *const c_char,
        format: c_int,
    ) -> c_int;
    pub fn xmlBufferCreate() -> *mut xmlBuffer;
    pub fn xmlBufferFree(buf: *mut xmlBuffer);
    pub fn xmlBufferContent(buf: *const xmlBuffer) -> *const xmlChar;
    pub fn xmlBufferLength(buf: *const xmlBuffer) -> c_int;
    pub fn xmlNodeDump(
        buf: *mut xmlBuffer,
        doc: *mut xmlDoc,
        cur: *mut xmlNode,
        level: c_int,
        format: c_int,
    ) -> c_int;
    pub fn xmlC14NDocSaveTo(
        doc: *mut xmlDoc,
        nodes: *mut xmlNodeSet,
        mode: c_int,
        inclusive_ns_prefixes: *mut *mut xmlChar,
        with_comments: c_int,
        buf: *mut xmlOutputBuffer,
    ) -> c_int;

    pub fn xmlC14NExecute(
        doc: *mut xmlDoc,
        cb: Option<unsafe extern "C" fn(*mut c_void, *mut xmlNode, *mut xmlNode) -> c_int>,
        user_data: *mut c_void,
        mode: c_int,
        inclusive_ns_prefixes: *mut *mut xmlChar,
        with_comments: c_int,
        buf: *mut xmlOutputBuffer,
    ) -> c_int;
    pub fn xmlAllocOutputBuffer(encoder: *mut xmlCharEncodingHandler) -> *mut xmlOutputBuffer;
    pub fn xmlOutputBufferGetContent(out: *mut xmlOutputBuffer) -> *const xmlChar;
    pub fn xmlOutputBufferGetSize(out: *mut xmlOutputBuffer) -> usize;

    // XPath
    pub fn xmlXPathNewContext(doc: *mut xmlDoc) -> *mut xmlXPathContext;
    pub fn xmlXPathFreeContext(ctxt: *mut xmlXPathContext);
    pub fn xmlXPathRegisterNs(
        ctxt: *mut xmlXPathContext,
        prefix: *const xmlChar,
        uri: *const xmlChar,
    ) -> c_int;
    pub fn xmlXPathRegisterFuncNS(
        ctxt: *mut xmlXPathContext,
        name: *const xmlChar,
        uri: *const xmlChar,
        f: xmlXPathFunction,
    ) -> c_int;
    pub fn xmlXPathRegisterFuncLookup(
        ctxt: *mut xmlXPathContext,
        f: xmlXPathFuncLookupFunc,
        data: *mut c_void,
    );
    pub fn xmlXPathEval(expr: *const xmlChar, ctxt: *mut xmlXPathContext) -> *mut xmlXPathObject;
    pub fn xmlXPathEvalExpression(
        expr: *const xmlChar,
        ctxt: *mut xmlXPathContext,
    ) -> *mut xmlXPathObject;
    pub fn xmlXPathFreeObject(obj: *mut xmlXPathObject);
    pub fn xmlXPathFreeNodeSet(obj: *mut xmlNodeSet);
    pub fn xmlXPathNodeSetCreate(val: *mut xmlNode) -> *mut xmlNodeSet;
    pub fn xmlXPathNodeSetAdd(cur: *mut xmlNodeSet, val: *mut xmlNode) -> c_int;
    pub fn xmlXPathNodeSetAddUnique(cur: *mut xmlNodeSet, val: *mut xmlNode) -> c_int;
    pub fn xmlXPathNewNodeSet(val: *mut xmlNode) -> *mut xmlXPathObject;
    pub fn xmlXPathWrapNodeSet(val: *mut xmlNodeSet) -> *mut xmlXPathObject;
    pub fn xmlXPathNewString(val: *const xmlChar) -> *mut xmlXPathObject;
    pub fn xmlXPathNewFloat(val: f64) -> *mut xmlXPathObject;
    pub fn xmlXPathNewBoolean(val: c_int) -> *mut xmlXPathObject;
    pub fn xmlXPathCastToString(val: *mut xmlXPathObject) -> *mut xmlChar;
    pub fn xmlXPathCastNodeToString(node: *mut xmlNode) -> *mut xmlChar;
    pub fn valuePop(ctxt: *mut xmlXPathParserContext) -> *mut xmlXPathObject;
    pub fn valuePush(ctxt: *mut xmlXPathParserContext, value: *mut xmlXPathObject) -> c_int;
    pub fn xmlXPathErr(ctxt: *mut xmlXPathParserContext, error: c_int);
    pub fn xmlXPathRegisteredNsCleanup(ctxt: *mut xmlXPathContext);

    // validation
    pub fn xmlNewValidCtxt() -> *mut xmlValidCtxt;
    pub fn xmlFreeValidCtxt(ctxt: *mut xmlValidCtxt);
    pub fn xmlValidateDocument(ctxt: *mut xmlValidCtxt, doc: *mut xmlDoc) -> c_int;
    pub fn xmlSchemaNewParserCtxt(url: *const c_char) -> *mut xmlSchemaParserCtxt;
    pub fn xmlSchemaNewMemParserCtxt(
        buffer: *const c_char,
        size: c_int,
    ) -> *mut xmlSchemaParserCtxt;
    pub fn xmlSchemaParse(ctxt: *mut xmlSchemaParserCtxt) -> *mut xmlSchema;
    pub fn xmlSchemaFreeParserCtxt(ctxt: *mut xmlSchemaParserCtxt);
    pub fn xmlSchemaFree(schema: *mut xmlSchema);
    pub fn xmlSchemaNewValidCtxt(schema: *mut xmlSchema) -> *mut xmlSchemaValidCtxt;
    pub fn xmlSchemaFreeValidCtxt(ctxt: *mut xmlSchemaValidCtxt);
    pub fn xmlSchemaSetValidOptions(ctxt: *mut xmlSchemaValidCtxt, options: c_int) -> c_int;
    pub fn xmlSchemaValidateDoc(ctxt: *mut xmlSchemaValidCtxt, doc: *mut xmlDoc) -> c_int;
    pub fn xmlRelaxNGNewParserCtxt(url: *const c_char) -> *mut xmlRelaxNGParserCtxt;
    pub fn xmlRelaxNGNewMemParserCtxt(
        buffer: *const c_char,
        size: c_int,
    ) -> *mut xmlRelaxNGParserCtxt;
    pub fn xmlRelaxNGParse(ctxt: *mut xmlRelaxNGParserCtxt) -> *mut xmlRelaxNG;
    pub fn xmlRelaxNGFreeParserCtxt(ctxt: *mut xmlRelaxNGParserCtxt);
    pub fn xmlRelaxNGFree(schema: *mut xmlRelaxNG);
    pub fn xmlRelaxNGNewValidCtxt(schema: *mut xmlRelaxNG) -> *mut xmlRelaxNGValidCtxt;
    pub fn xmlRelaxNGFreeValidCtxt(ctxt: *mut xmlRelaxNGValidCtxt);
    pub fn xmlRelaxNGValidateDoc(ctxt: *mut xmlRelaxNGValidCtxt, doc: *mut xmlDoc) -> c_int;
    pub fn xmlXIncludeProcessFlags(doc: *mut xmlDoc, flags: c_int) -> c_int;
}

/// Compile-time layout checks (values from a C program built against the
/// libxml2 2.9.14 headers).
#[cfg(target_pointer_width = "64")]
mod layout {
    use super::{
        xmlAttribute, xmlNodeSet, xmlNotation, xmlOutputBufferFields, xmlXPathContext,
        xmlXPathObject, xmlXPathParserContext,
    };
    use std::mem::{offset_of, size_of};

    const _: () = assert!(size_of::<xmlXPathContext>() == 376);
    const _: () = assert!(offset_of!(xmlXPathContext, namespaces) == 80);
    const _: () = assert!(offset_of!(xmlXPathContext, nsNr) == 88);
    const _: () = assert!(offset_of!(xmlXPathContext, contextSize) == 104);
    const _: () = assert!(offset_of!(xmlXPathContext, function) == 168);
    const _: () = assert!(offset_of!(xmlXPathContext, funcLookupFunc) == 184);
    const _: () = assert!(offset_of!(xmlXPathContext, userData) == 216);
    const _: () = assert!(offset_of!(xmlXPathContext, lastError) == 232);
    const _: () = assert!(offset_of!(xmlXPathContext, dict) == 328);
    const _: () = assert!(offset_of!(xmlXPathContext, depth) == 368);
    const _: () = assert!(size_of::<xmlXPathObject>() == 72);
    const _: () = assert!(offset_of!(xmlXPathObject, stringval) == 32);
    const _: () = assert!(size_of::<xmlNodeSet>() == 16);
    const _: () = assert!(size_of::<xmlXPathParserContext>() == 88);
    const _: () = assert!(offset_of!(xmlXPathParserContext, context) == 24);
    const _: () = assert!(offset_of!(xmlXPathParserContext, valueTab) == 48);
    const _: () = assert!(size_of::<xmlNotation>() == 24);
    const _: () = assert!(size_of::<xmlAttribute>() == 120);
    const _: () = assert!(offset_of!(xmlAttribute, defaultValue) == 88);
    const _: () = assert!(offset_of!(xmlAttribute, elem) == 112);
    const _: () = assert!(size_of::<xmlOutputBufferFields>() == 56);
    const _: () = assert!(offset_of!(xmlOutputBufferFields, error) == 52);
}

/// Unused marker to keep `c_uint` imported for future declarations.
pub type Unused = c_uint;
