//! Minimal hand-written bindings to the system libxml2 (2.9.x ABI).
//!
//! Only the functions, structures and constants this crate (and the future
//! DOM/SimpleXML/XMLReader/XSL crates built on it) actually use are
//! declared. Structure layouts mirror `parser.h`, `tree.h`, `entities.h` and
//! `xmlerror.h` of libxml2 2.9.14; the offsets that the crate relies on are
//! checked at compile time (see the `layout` assertions at the bottom), and
//! were obtained from a C program compiled against the system headers.
//!
//! Everything in here is a raw declaration: no logic, no safe wrappers.
//! The safe surface lives in [`crate::raw`] and above.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]
#![allow(clippy::upper_case_acronyms, clippy::pub_underscore_fields)]

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_ushort, c_void};

/// libxml2's byte type (`unsigned char`); strings are UTF-8 internally.
pub type xmlChar = u8;

// ---------------------------------------------------------------------------
// Callback types
// ---------------------------------------------------------------------------

/// `void (*)(void *userData, xmlErrorPtr error)` (non-const in 2.9.x).
pub type xmlStructuredErrorFunc = Option<unsafe extern "C" fn(*mut c_void, *mut xmlError)>;
/// `void (*)(void *ctx, const char *msg, ...)` — only ever stored as NULL by us.
pub type xmlGenericErrorFunc = Option<unsafe extern "C" fn(*mut c_void, *const c_char, ...)>;
pub type xmlExternalEntityLoader = Option<
    unsafe extern "C" fn(*const c_char, *const c_char, *mut xmlParserCtxt) -> *mut xmlParserInput,
>;
pub type xmlInputReadCallback =
    Option<unsafe extern "C" fn(*mut c_void, *mut c_char, c_int) -> c_int>;
pub type xmlInputCloseCallback = Option<unsafe extern "C" fn(*mut c_void) -> c_int>;
pub type xmlOutputWriteCallback =
    Option<unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int>;
pub type xmlOutputCloseCallback = Option<unsafe extern "C" fn(*mut c_void) -> c_int>;
pub type xmlParserInputBufferCreateFilenameFunc =
    Option<unsafe extern "C" fn(*const c_char, c_int) -> *mut xmlParserInputBuffer>;
pub type xmlOutputBufferCreateFilenameFunc = Option<
    unsafe extern "C" fn(*const c_char, *mut xmlCharEncodingHandler, c_int) -> *mut xmlOutputBuffer,
>;
pub type xmlFreeFunc = Option<unsafe extern "C" fn(*mut c_void)>;

// SAX callbacks used by the expat-compatibility layer.
pub type getEntitySAXFunc =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar) -> *mut xmlEntity>;
pub type notationDeclSAXFunc =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, *const xmlChar, *const xmlChar)>;
pub type unparsedEntityDeclSAXFunc = Option<
    unsafe extern "C" fn(
        *mut c_void,
        *const xmlChar,
        *const xmlChar,
        *const xmlChar,
        *const xmlChar,
    ),
>;
pub type startElementSAXFunc =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, *mut *const xmlChar)>;
pub type endElementSAXFunc = Option<unsafe extern "C" fn(*mut c_void, *const xmlChar)>;
pub type charactersSAXFunc = Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, c_int)>;
pub type processingInstructionSAXFunc =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, *const xmlChar)>;
pub type commentSAXFunc = Option<unsafe extern "C" fn(*mut c_void, *const xmlChar)>;
pub type startElementNsSAX2Func = Option<
    unsafe extern "C" fn(
        *mut c_void,
        *const xmlChar,
        *const xmlChar,
        *const xmlChar,
        c_int,
        *mut *const xmlChar,
        c_int,
        c_int,
        *mut *const xmlChar,
    ),
>;
pub type endElementNsSAX2Func =
    Option<unsafe extern "C" fn(*mut c_void, *const xmlChar, *const xmlChar, *const xmlChar)>;
/// Any SAX slot we never fill: typed as an opaque nullable function pointer.
pub type unusedSAXFunc = Option<unsafe extern "C" fn()>;

// ---------------------------------------------------------------------------
// Structures
// ---------------------------------------------------------------------------

/// `xmlError` (88 bytes on LP64).
#[repr(C)]
#[derive(Debug)]
pub struct xmlError {
    pub domain: c_int,
    pub code: c_int,
    pub message: *mut c_char,
    pub level: c_int,
    pub file: *mut c_char,
    pub line: c_int,
    pub str1: *mut c_char,
    pub str2: *mut c_char,
    pub str3: *mut c_char,
    pub int1: c_int,
    /// Column number (named `int2` for ABI reasons).
    pub int2: c_int,
    pub ctxt: *mut c_void,
    pub node: *mut c_void,
}

/// `xmlParserInput`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlParserInput {
    pub buf: *mut xmlParserInputBuffer,
    pub filename: *const c_char,
    pub directory: *const c_char,
    pub base: *const xmlChar,
    pub cur: *const xmlChar,
    pub end: *const xmlChar,
    pub length: c_int,
    pub line: c_int,
    pub col: c_int,
    pub consumed: c_ulong,
    pub free: Option<unsafe extern "C" fn(*mut xmlChar)>,
    pub encoding: *const xmlChar,
    pub version: *const xmlChar,
    pub standalone: c_int,
    pub id: c_int,
    pub parentConsumed: c_ulong,
    pub entity: *mut xmlEntity,
}

/// `xmlSAXHandler` (SAX2 layout, 256 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct xmlSAXHandler {
    pub internalSubset: unusedSAXFunc,
    pub isStandalone: unusedSAXFunc,
    pub hasInternalSubset: unusedSAXFunc,
    pub hasExternalSubset: unusedSAXFunc,
    pub resolveEntity: unusedSAXFunc,
    pub getEntity: getEntitySAXFunc,
    pub entityDecl: unusedSAXFunc,
    pub notationDecl: notationDeclSAXFunc,
    pub attributeDecl: unusedSAXFunc,
    pub elementDecl: unusedSAXFunc,
    pub unparsedEntityDecl: unparsedEntityDeclSAXFunc,
    pub setDocumentLocator: unusedSAXFunc,
    pub startDocument: unusedSAXFunc,
    pub endDocument: unusedSAXFunc,
    pub startElement: startElementSAXFunc,
    pub endElement: endElementSAXFunc,
    pub reference: unusedSAXFunc,
    pub characters: charactersSAXFunc,
    pub ignorableWhitespace: charactersSAXFunc,
    pub processingInstruction: processingInstructionSAXFunc,
    pub comment: commentSAXFunc,
    pub warning: xmlGenericErrorFunc,
    pub error: xmlGenericErrorFunc,
    pub fatalError: xmlGenericErrorFunc,
    pub getParameterEntity: unusedSAXFunc,
    pub cdataBlock: charactersSAXFunc,
    pub externalSubset: unusedSAXFunc,
    pub initialized: c_uint,
    pub _private: *mut c_void,
    pub startElementNs: startElementNsSAX2Func,
    pub endElementNs: endElementNsSAX2Func,
    pub serror: xmlStructuredErrorFunc,
}

/// `xmlParserCtxt` (752 bytes on LP64). Opaque sub-structures that we never
/// touch (`node_seq`, `vctxt`) are represented as correctly sized blobs.
#[repr(C)]
#[derive(Debug)]
pub struct xmlParserCtxt {
    pub sax: *mut xmlSAXHandler,
    pub userData: *mut c_void,
    pub myDoc: *mut xmlDoc,
    pub wellFormed: c_int,
    pub replaceEntities: c_int,
    pub version: *const xmlChar,
    pub encoding: *const xmlChar,
    pub standalone: c_int,
    pub html: c_int,
    pub input: *mut xmlParserInput,
    pub inputNr: c_int,
    pub inputMax: c_int,
    pub inputTab: *mut *mut xmlParserInput,
    pub node: *mut xmlNode,
    pub nodeNr: c_int,
    pub nodeMax: c_int,
    pub nodeTab: *mut *mut xmlNode,
    pub record_info: c_int,
    pub node_seq: [u64; 3],
    pub errNo: c_int,
    pub hasExternalSubset: c_int,
    pub hasPErefs: c_int,
    pub external: c_int,
    pub valid: c_int,
    pub validate: c_int,
    pub vctxt: [u64; 14],
    pub instate: c_int,
    pub token: c_int,
    pub directory: *mut c_char,
    pub name: *const xmlChar,
    pub nameNr: c_int,
    pub nameMax: c_int,
    pub nameTab: *mut *const xmlChar,
    pub nbChars: c_long,
    pub checkIndex: c_long,
    pub keepBlanks: c_int,
    pub disableSAX: c_int,
    pub inSubset: c_int,
    pub intSubName: *const xmlChar,
    pub extSubURI: *mut xmlChar,
    pub extSubSystem: *mut xmlChar,
    pub space: *mut c_int,
    pub spaceNr: c_int,
    pub spaceMax: c_int,
    pub spaceTab: *mut c_int,
    pub depth: c_int,
    pub entity: *mut xmlParserInput,
    pub charset: c_int,
    pub nodelen: c_int,
    pub nodemem: c_int,
    pub pedantic: c_int,
    pub _private: *mut c_void,
    pub loadsubset: c_int,
    pub linenumbers: c_int,
    pub catalogs: *mut c_void,
    pub recovery: c_int,
    pub progressive: c_int,
    pub dict: *mut c_void,
    pub atts: *mut *const xmlChar,
    pub maxatts: c_int,
    pub docdict: c_int,
    pub str_xml: *const xmlChar,
    pub str_xmlns: *const xmlChar,
    pub str_xml_ns: *const xmlChar,
    pub sax2: c_int,
    pub nsNr: c_int,
    pub nsMax: c_int,
    pub nsTab: *mut *const xmlChar,
    pub attallocs: *mut c_int,
    pub pushTab: *mut c_void,
    pub attsDefault: *mut c_void,
    pub attsSpecial: *mut c_void,
    pub nsWellFormed: c_int,
    pub options: c_int,
    pub dictNames: c_int,
    pub freeElemsNr: c_int,
    pub freeElems: *mut xmlNode,
    pub freeAttrsNr: c_int,
    pub freeAttrs: *mut xmlAttr,
    pub lastError: xmlError,
    pub parseMode: c_int,
    pub nbentities: c_ulong,
    pub sizeentities: c_ulong,
    pub nodeInfo: *mut c_void,
    pub nodeInfoNr: c_int,
    pub nodeInfoMax: c_int,
    pub nodeInfoTab: *mut c_void,
    pub input_id: c_int,
    pub sizeentcopy: c_ulong,
}

/// `xmlNs`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNs {
    pub next: *mut xmlNs,
    pub type_: c_int,
    pub href: *const xmlChar,
    pub prefix: *const xmlChar,
    pub _private: *mut c_void,
    pub context: *mut xmlDoc,
}

/// `xmlNode` (also the common prefix of every tree node type).
#[repr(C)]
#[derive(Debug)]
pub struct xmlNode {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub ns: *mut xmlNs,
    pub content: *mut xmlChar,
    pub properties: *mut xmlAttr,
    pub nsDef: *mut xmlNs,
    pub psvi: *mut c_void,
    pub line: c_ushort,
    pub extra: c_ushort,
}

/// `xmlAttr`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlAttr {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlAttr,
    pub prev: *mut xmlAttr,
    pub doc: *mut xmlDoc,
    pub ns: *mut xmlNs,
    pub atype: c_int,
    pub psvi: *mut c_void,
}

/// `xmlDoc`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlDoc {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *mut c_char,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub compression: c_int,
    pub standalone: c_int,
    pub intSubset: *mut xmlDtd,
    pub extSubset: *mut xmlDtd,
    pub oldNs: *mut xmlNs,
    pub version: *const xmlChar,
    pub encoding: *const xmlChar,
    pub ids: *mut c_void,
    pub refs: *mut c_void,
    pub URL: *const xmlChar,
    pub charset: c_int,
    pub dict: *mut c_void,
    pub psvi: *mut c_void,
    pub parseFlags: c_int,
    pub properties: c_int,
}

/// `xmlDtd`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlDtd {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlDoc,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub notations: *mut c_void,
    pub elements: *mut c_void,
    pub attributes: *mut c_void,
    pub entities: *mut c_void,
    pub ExternalID: *const xmlChar,
    pub SystemID: *const xmlChar,
    pub pentities: *mut c_void,
}

/// `xmlEntity`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlEntity {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlDtd,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub orig: *mut xmlChar,
    pub content: *mut xmlChar,
    pub length: c_int,
    pub etype: c_int,
    pub ExternalID: *const xmlChar,
    pub SystemID: *const xmlChar,
    pub nexte: *mut xmlEntity,
    pub URI: *const xmlChar,
    pub owner: c_int,
    pub checked: c_int,
    pub flags: c_int,
    pub expandedSize: c_ulong,
}

/// `xmlURI`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlURI {
    pub scheme: *mut c_char,
    pub opaque: *mut c_char,
    pub authority: *mut c_char,
    pub server: *mut c_char,
    pub user: *mut c_char,
    pub port: c_int,
    pub path: *mut c_char,
    pub query: *mut c_char,
    pub fragment: *mut c_char,
    pub cleanup: c_int,
    pub query_raw: *mut c_char,
}

/// Opaque handles.
#[repr(C)]
#[derive(Debug)]
pub struct xmlParserInputBuffer {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlOutputBuffer {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlCharEncodingHandler {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlTextWriter {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlSaveCtxt {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Debug)]
pub struct xmlBuffer {
    _opaque: [u8; 0],
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const XML_SAX2_MAGIC: c_uint = 0xDEED_BEAF;

// xmlErrorLevel
pub const XML_ERR_NONE: c_int = 0;
pub const XML_ERR_WARNING: c_int = 1;
pub const XML_ERR_ERROR: c_int = 2;
pub const XML_ERR_FATAL: c_int = 3;

// xmlErrorDomain (subset)
pub const XML_FROM_NONE: c_int = 0;
pub const XML_FROM_PARSER: c_int = 1;
pub const XML_FROM_TREE: c_int = 2;
pub const XML_FROM_NAMESPACE: c_int = 3;
pub const XML_FROM_DTD: c_int = 4;
pub const XML_FROM_HTML: c_int = 5;
pub const XML_FROM_IO: c_int = 8;
pub const XML_FROM_VALID: c_int = 23;

// xmlParserErrors (subset)
pub const XML_ERR_OK: c_int = 0;
pub const XML_ERR_INTERNAL_ERROR: c_int = 1;

// xmlParserOption
pub const XML_PARSE_RECOVER: c_int = 1 << 0;
pub const XML_PARSE_NOENT: c_int = 1 << 1;
pub const XML_PARSE_DTDLOAD: c_int = 1 << 2;
pub const XML_PARSE_DTDATTR: c_int = 1 << 3;
pub const XML_PARSE_DTDVALID: c_int = 1 << 4;
pub const XML_PARSE_NOERROR: c_int = 1 << 5;
pub const XML_PARSE_NOWARNING: c_int = 1 << 6;
pub const XML_PARSE_PEDANTIC: c_int = 1 << 7;
pub const XML_PARSE_NOBLANKS: c_int = 1 << 8;
pub const XML_PARSE_SAX1: c_int = 1 << 9;
pub const XML_PARSE_XINCLUDE: c_int = 1 << 10;
pub const XML_PARSE_NONET: c_int = 1 << 11;
pub const XML_PARSE_NODICT: c_int = 1 << 12;
pub const XML_PARSE_NSCLEAN: c_int = 1 << 13;
pub const XML_PARSE_NOCDATA: c_int = 1 << 14;
pub const XML_PARSE_NOXINCNODE: c_int = 1 << 15;
pub const XML_PARSE_COMPACT: c_int = 1 << 16;
pub const XML_PARSE_HUGE: c_int = 1 << 19;
pub const XML_PARSE_OLDSAX: c_int = 1 << 20;
pub const XML_PARSE_IGNORE_ENC: c_int = 1 << 21;
pub const XML_PARSE_BIG_LINES: c_int = 1 << 22;

// xmlSaveOption
pub const XML_SAVE_FORMAT: c_int = 1 << 0;
pub const XML_SAVE_NO_DECL: c_int = 1 << 1;
pub const XML_SAVE_NO_EMPTY: c_int = 1 << 2;
pub const XML_SAVE_AS_XML: c_int = 1 << 5;

// htmlParserOption (subset)
pub const HTML_PARSE_NODEFDTD: c_int = 1 << 2;
pub const HTML_PARSE_NOIMPLIED: c_int = 1 << 13;

// xmlSchemaValidOption
pub const XML_SCHEMA_VAL_VC_I_CREATE: c_int = 1;

// xmlParserInputState (subset)
pub const XML_PARSER_EOF: c_int = -1;
pub const XML_PARSER_CONTENT: c_int = 7;

// xmlEntityType
pub const XML_INTERNAL_GENERAL_ENTITY: c_int = 1;
pub const XML_EXTERNAL_GENERAL_PARSED_ENTITY: c_int = 2;
pub const XML_EXTERNAL_GENERAL_UNPARSED_ENTITY: c_int = 3;
pub const XML_INTERNAL_PARAMETER_ENTITY: c_int = 4;
pub const XML_EXTERNAL_PARAMETER_ENTITY: c_int = 5;
pub const XML_INTERNAL_PREDEFINED_ENTITY: c_int = 6;

// xmlElementType
pub const XML_ELEMENT_NODE: c_int = 1;
pub const XML_ATTRIBUTE_NODE: c_int = 2;
pub const XML_TEXT_NODE: c_int = 3;
pub const XML_CDATA_SECTION_NODE: c_int = 4;
pub const XML_ENTITY_REF_NODE: c_int = 5;
pub const XML_ENTITY_NODE: c_int = 6;
pub const XML_PI_NODE: c_int = 7;
pub const XML_COMMENT_NODE: c_int = 8;
pub const XML_DOCUMENT_NODE: c_int = 9;
pub const XML_DOCUMENT_TYPE_NODE: c_int = 10;
pub const XML_DOCUMENT_FRAG_NODE: c_int = 11;
pub const XML_NOTATION_NODE: c_int = 12;
pub const XML_HTML_DOCUMENT_NODE: c_int = 13;
pub const XML_DTD_NODE: c_int = 14;
pub const XML_ELEMENT_DECL: c_int = 15;
pub const XML_ATTRIBUTE_DECL: c_int = 16;
pub const XML_ENTITY_DECL: c_int = 17;
pub const XML_NAMESPACE_DECL: c_int = 18;
pub const XML_XINCLUDE_START: c_int = 19;
pub const XML_XINCLUDE_END: c_int = 20;

// xmlNsType
pub const XML_LOCAL_NAMESPACE: c_int = XML_NAMESPACE_DECL;

// xmlCharEncoding
pub const XML_CHAR_ENCODING_ERROR: c_int = -1;
pub const XML_CHAR_ENCODING_NONE: c_int = 0;

pub const XML_MAX_DICTIONARY_LIMIT: usize = 10_000_000;

// ---------------------------------------------------------------------------
// Functions and globals
// ---------------------------------------------------------------------------

/// glibc `cookie_io_functions_t` (for `fopencookie`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct cookie_io_functions_t {
    /// Read callback.
    pub read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> isize>,
    /// Write callback.
    pub write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> isize>,
    /// Seek callback.
    pub seek: Option<unsafe extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int>,
    /// Close callback.
    pub close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

/// `_IONBF` for `setvbuf`.
pub const IONBF: c_int = 2;

// libc (glibc): a FILE* whose writes call back into Rust, used as the
// context of libxml2's default generic error handler (which `vfprintf`s to
// it), since Rust cannot define the C-variadic handler itself.
extern "C" {
    pub fn fopencookie(
        cookie: *mut c_void,
        mode: *const c_char,
        funcs: cookie_io_functions_t,
    ) -> *mut c_void;
    pub fn setvbuf(stream: *mut c_void, buf: *mut c_char, mode: c_int, size: usize) -> c_int;
}

#[link(name = "xml2")]
extern "C" {
    // globals
    pub static xmlFree: xmlFreeFunc;
    pub static xmlParserVersion: *const c_char;

    // init / errors
    pub fn xmlInitParser();
    pub fn xmlSetStructuredErrorFunc(ctx: *mut c_void, handler: xmlStructuredErrorFunc);
    pub fn xmlSetGenericErrorFunc(ctx: *mut c_void, handler: xmlGenericErrorFunc);
    pub fn xmlGetLastError() -> *mut xmlError;
    pub fn xmlResetLastError();
    pub fn xmlCtxtGetLastError(ctx: *mut c_void) -> *mut xmlError;

    // entity loading / IO
    pub fn xmlSetExternalEntityLoader(f: xmlExternalEntityLoader);
    pub fn xmlGetExternalEntityLoader() -> xmlExternalEntityLoader;
    pub fn xmlNewInputFromFile(
        ctxt: *mut xmlParserCtxt,
        filename: *const c_char,
    ) -> *mut xmlParserInput;
    pub fn xmlNewIOInputStream(
        ctxt: *mut xmlParserCtxt,
        input: *mut xmlParserInputBuffer,
        enc: c_int,
    ) -> *mut xmlParserInput;
    pub fn xmlParserInputBufferCreateIO(
        ioread: xmlInputReadCallback,
        ioclose: xmlInputCloseCallback,
        ioctx: *mut c_void,
        enc: c_int,
    ) -> *mut xmlParserInputBuffer;
    pub fn xmlFreeParserInputBuffer(buf: *mut xmlParserInputBuffer);
    pub fn xmlParserInputBufferCreateFilenameDefault(
        f: xmlParserInputBufferCreateFilenameFunc,
    ) -> xmlParserInputBufferCreateFilenameFunc;
    pub fn xmlOutputBufferCreateFilenameDefault(
        f: xmlOutputBufferCreateFilenameFunc,
    ) -> xmlOutputBufferCreateFilenameFunc;
    pub fn __xmlParserInputBufferCreateFilename(
        uri: *const c_char,
        enc: c_int,
    ) -> *mut xmlParserInputBuffer;
    pub fn __xmlOutputBufferCreateFilename(
        uri: *const c_char,
        encoder: *mut xmlCharEncodingHandler,
        compression: c_int,
    ) -> *mut xmlOutputBuffer;
    pub fn xmlOutputBufferCreateIO(
        iowrite: xmlOutputWriteCallback,
        ioclose: xmlOutputCloseCallback,
        ioctx: *mut c_void,
        encoder: *mut xmlCharEncodingHandler,
    ) -> *mut xmlOutputBuffer;
    pub fn xmlOutputBufferClose(out: *mut xmlOutputBuffer) -> c_int;
    pub fn xmlOutputBufferFlush(out: *mut xmlOutputBuffer) -> c_int;
    pub fn xmlCharEncCloseFunc(handler: *mut xmlCharEncodingHandler) -> c_int;
    pub fn xmlFindCharEncodingHandler(name: *const c_char) -> *mut xmlCharEncodingHandler;
    pub fn xmlParseCharEncoding(name: *const c_char) -> c_int;

    // URIs
    pub fn xmlParseURI(s: *const c_char) -> *mut xmlURI;
    pub fn xmlCreateURI() -> *mut xmlURI;
    pub fn xmlParseURIReference(uri: *mut xmlURI, s: *const c_char) -> c_int;
    pub fn xmlFreeURI(uri: *mut xmlURI);
    pub fn xmlURIUnescapeString(s: *const c_char, len: c_int, target: *mut c_char) -> *mut c_char;
    pub fn xmlURIEscapeStr(s: *const xmlChar, list: *const xmlChar) -> *mut xmlChar;

    // parser contexts
    pub fn xmlCreatePushParserCtxt(
        sax: *mut xmlSAXHandler,
        user_data: *mut c_void,
        chunk: *const c_char,
        size: c_int,
        filename: *const c_char,
    ) -> *mut xmlParserCtxt;
    pub fn xmlNewParserCtxt() -> *mut xmlParserCtxt;
    pub fn xmlFreeParserCtxt(ctxt: *mut xmlParserCtxt);
    pub fn xmlCtxtUseOptions(ctxt: *mut xmlParserCtxt, options: c_int) -> c_int;
    pub fn xmlParseChunk(
        ctxt: *mut xmlParserCtxt,
        chunk: *const c_char,
        size: c_int,
        terminate: c_int,
    ) -> c_int;
    pub fn xmlStopParser(ctxt: *mut xmlParserCtxt);
    pub fn xmlCtxtReadMemory(
        ctxt: *mut xmlParserCtxt,
        buffer: *const c_char,
        size: c_int,
        url: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc;
    pub fn xmlCtxtReadFile(
        ctxt: *mut xmlParserCtxt,
        filename: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc;
    pub fn xmlDictSetLimit(dict: *mut c_void, limit: usize) -> usize;

    // entities
    pub fn xmlGetPredefinedEntity(name: *const xmlChar) -> *mut xmlEntity;
    pub fn xmlGetDocEntity(doc: *const xmlDoc, name: *const xmlChar) -> *mut xmlEntity;

    // strings
    pub fn xmlStrlen(s: *const xmlChar) -> c_int;
    pub fn xmlValidateName(value: *const xmlChar, space: c_int) -> c_int;

    // tree
    pub fn xmlNewDoc(version: *const xmlChar) -> *mut xmlDoc;
    pub fn xmlFreeDoc(doc: *mut xmlDoc);
    pub fn xmlDocGetRootElement(doc: *const xmlDoc) -> *mut xmlNode;
    pub fn xmlDocSetRootElement(doc: *mut xmlDoc, root: *mut xmlNode) -> *mut xmlNode;
    pub fn xmlNewDocNode(
        doc: *mut xmlDoc,
        ns: *mut xmlNs,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewDocText(doc: *const xmlDoc, content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewDocTextLen(doc: *mut xmlDoc, content: *const xmlChar, len: c_int) -> *mut xmlNode;
    pub fn xmlNewDocComment(doc: *mut xmlDoc, content: *const xmlChar) -> *mut xmlNode;
    pub fn xmlNewCDataBlock(doc: *mut xmlDoc, content: *const xmlChar, len: c_int) -> *mut xmlNode;
    pub fn xmlNewDocPI(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> *mut xmlNode;
    pub fn xmlNewNs(node: *mut xmlNode, href: *const xmlChar, prefix: *const xmlChar)
        -> *mut xmlNs;
    pub fn xmlSearchNs(doc: *mut xmlDoc, node: *mut xmlNode, prefix: *const xmlChar) -> *mut xmlNs;
    pub fn xmlSearchNsByHref(
        doc: *mut xmlDoc,
        node: *mut xmlNode,
        href: *const xmlChar,
    ) -> *mut xmlNs;
    pub fn xmlSetNs(node: *mut xmlNode, ns: *mut xmlNs);
    pub fn xmlReconciliateNs(doc: *mut xmlDoc, tree: *mut xmlNode) -> c_int;
    pub fn xmlUnlinkNode(node: *mut xmlNode);
    pub fn xmlFreeNode(node: *mut xmlNode);
    pub fn xmlFreeProp(attr: *mut xmlAttr);
    pub fn xmlNodeGetContent(node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlNodeSetContent(node: *mut xmlNode, content: *const xmlChar);
    pub fn xmlNodeGetBase(doc: *const xmlDoc, node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlGetNodePath(node: *const xmlNode) -> *mut xmlChar;
    pub fn xmlGetLineNo(node: *const xmlNode) -> c_long;
    pub fn xmlGetProp(node: *const xmlNode, name: *const xmlChar) -> *mut xmlChar;
    pub fn xmlGetNsProp(
        node: *const xmlNode,
        name: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlChar;
    pub fn xmlHasProp(node: *const xmlNode, name: *const xmlChar) -> *mut xmlAttr;
    pub fn xmlHasNsProp(
        node: *const xmlNode,
        name: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlAttr;
    pub fn xmlNewDocProp(
        doc: *mut xmlDoc,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> *mut xmlAttr;
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
    pub fn xmlDocCopyNode(node: *mut xmlNode, doc: *mut xmlDoc, extended: c_int) -> *mut xmlNode;
    pub fn xmlSetTreeDoc(tree: *mut xmlNode, doc: *mut xmlDoc);
    pub fn xmlDocDumpFormatMemoryEnc(
        doc: *mut xmlDoc,
        mem: *mut *mut xmlChar,
        size: *mut c_int,
        encoding: *const c_char,
        format: c_int,
    );
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
    ) -> *mut xmlSaveCtxt;
    pub fn xmlSaveDoc(ctxt: *mut xmlSaveCtxt, doc: *mut xmlDoc) -> c_long;
    pub fn xmlSaveTree(ctxt: *mut xmlSaveCtxt, node: *mut xmlNode) -> c_long;
    pub fn xmlSaveClose(ctxt: *mut xmlSaveCtxt) -> c_int;

    // xmlwriter
    pub fn xmlNewTextWriter(out: *mut xmlOutputBuffer) -> *mut xmlTextWriter;
    pub fn xmlNewTextWriterFilename(uri: *const c_char, compression: c_int) -> *mut xmlTextWriter;
    pub fn xmlFreeTextWriter(writer: *mut xmlTextWriter);
    pub fn xmlTextWriterSetIndent(writer: *mut xmlTextWriter, indent: c_int) -> c_int;
    pub fn xmlTextWriterSetIndentString(writer: *mut xmlTextWriter, s: *const xmlChar) -> c_int;
    pub fn xmlTextWriterStartDocument(
        writer: *mut xmlTextWriter,
        version: *const c_char,
        encoding: *const c_char,
        standalone: *const c_char,
    ) -> c_int;
    pub fn xmlTextWriterEndDocument(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterStartElement(writer: *mut xmlTextWriter, name: *const xmlChar) -> c_int;
    pub fn xmlTextWriterStartElementNS(
        writer: *mut xmlTextWriter,
        prefix: *const xmlChar,
        name: *const xmlChar,
        uri: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterEndElement(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterFullEndElement(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteElement(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterWriteElementNS(
        writer: *mut xmlTextWriter,
        prefix: *const xmlChar,
        name: *const xmlChar,
        uri: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartAttribute(writer: *mut xmlTextWriter, name: *const xmlChar) -> c_int;
    pub fn xmlTextWriterStartAttributeNS(
        writer: *mut xmlTextWriter,
        prefix: *const xmlChar,
        name: *const xmlChar,
        uri: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterEndAttribute(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteAttribute(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterWriteAttributeNS(
        writer: *mut xmlTextWriter,
        prefix: *const xmlChar,
        name: *const xmlChar,
        uri: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartPI(writer: *mut xmlTextWriter, target: *const xmlChar) -> c_int;
    pub fn xmlTextWriterEndPI(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWritePI(
        writer: *mut xmlTextWriter,
        target: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartCDATA(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterEndCDATA(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteCDATA(writer: *mut xmlTextWriter, content: *const xmlChar) -> c_int;
    pub fn xmlTextWriterWriteRaw(writer: *mut xmlTextWriter, content: *const xmlChar) -> c_int;
    pub fn xmlTextWriterWriteString(writer: *mut xmlTextWriter, content: *const xmlChar) -> c_int;
    pub fn xmlTextWriterStartComment(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterEndComment(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteComment(writer: *mut xmlTextWriter, content: *const xmlChar) -> c_int;
    pub fn xmlTextWriterStartDTD(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        pubid: *const xmlChar,
        sysid: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterEndDTD(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteDTD(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        pubid: *const xmlChar,
        sysid: *const xmlChar,
        subset: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartDTDElement(writer: *mut xmlTextWriter, name: *const xmlChar) -> c_int;
    pub fn xmlTextWriterEndDTDElement(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteDTDElement(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartDTDAttlist(writer: *mut xmlTextWriter, name: *const xmlChar) -> c_int;
    pub fn xmlTextWriterEndDTDAttlist(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteDTDAttlist(
        writer: *mut xmlTextWriter,
        name: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterStartDTDEntity(
        writer: *mut xmlTextWriter,
        pe: c_int,
        name: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterEndDTDEntity(writer: *mut xmlTextWriter) -> c_int;
    pub fn xmlTextWriterWriteDTDEntity(
        writer: *mut xmlTextWriter,
        pe: c_int,
        name: *const xmlChar,
        pubid: *const xmlChar,
        sysid: *const xmlChar,
        ndataid: *const xmlChar,
        content: *const xmlChar,
    ) -> c_int;
    pub fn xmlTextWriterFlush(writer: *mut xmlTextWriter) -> c_int;
}

/// Compile-time layout checks against the values produced by a C program
/// compiled with the libxml2 2.9.14 headers (LP64).
#[cfg(target_pointer_width = "64")]
mod layout {
    use super::{
        xmlAttr, xmlDoc, xmlDtd, xmlEntity, xmlError, xmlNode, xmlNs, xmlParserCtxt,
        xmlParserInput, xmlSAXHandler, xmlURI,
    };
    use std::mem::{offset_of, size_of};

    const _: () = assert!(size_of::<xmlParserCtxt>() == 752);
    const _: () = assert!(offset_of!(xmlParserCtxt, myDoc) == 16);
    const _: () = assert!(offset_of!(xmlParserCtxt, wellFormed) == 24);
    const _: () = assert!(offset_of!(xmlParserCtxt, replaceEntities) == 28);
    const _: () = assert!(offset_of!(xmlParserCtxt, input) == 56);
    const _: () = assert!(offset_of!(xmlParserCtxt, inputTab) == 72);
    const _: () = assert!(offset_of!(xmlParserCtxt, node_seq) == 112);
    const _: () = assert!(offset_of!(xmlParserCtxt, errNo) == 136);
    const _: () = assert!(offset_of!(xmlParserCtxt, validate) == 156);
    const _: () = assert!(offset_of!(xmlParserCtxt, vctxt) == 160);
    const _: () = assert!(offset_of!(xmlParserCtxt, instate) == 272);
    const _: () = assert!(offset_of!(xmlParserCtxt, directory) == 280);
    const _: () = assert!(offset_of!(xmlParserCtxt, keepBlanks) == 328);
    const _: () = assert!(offset_of!(xmlParserCtxt, disableSAX) == 332);
    const _: () = assert!(offset_of!(xmlParserCtxt, inSubset) == 336);
    const _: () = assert!(offset_of!(xmlParserCtxt, intSubName) == 344);
    const _: () = assert!(offset_of!(xmlParserCtxt, extSubSystem) == 360);
    const _: () = assert!(offset_of!(xmlParserCtxt, pedantic) == 420);
    const _: () = assert!(offset_of!(xmlParserCtxt, _private) == 424);
    const _: () = assert!(offset_of!(xmlParserCtxt, loadsubset) == 432);
    const _: () = assert!(offset_of!(xmlParserCtxt, linenumbers) == 436);
    const _: () = assert!(offset_of!(xmlParserCtxt, recovery) == 448);
    const _: () = assert!(offset_of!(xmlParserCtxt, dict) == 456);
    const _: () = assert!(offset_of!(xmlParserCtxt, options) == 564);
    const _: () = assert!(offset_of!(xmlParserCtxt, lastError) == 600);
    const _: () = assert!(offset_of!(xmlParserCtxt, sizeentcopy) == 744);

    const _: () = assert!(size_of::<xmlError>() == 88);
    const _: () = assert!(offset_of!(xmlError, file) == 24);
    const _: () = assert!(offset_of!(xmlError, line) == 32);
    const _: () = assert!(offset_of!(xmlError, int2) == 68);
    const _: () = assert!(offset_of!(xmlError, ctxt) == 72);

    const _: () = assert!(size_of::<xmlParserInput>() == 120);
    const _: () = assert!(offset_of!(xmlParserInput, base) == 24);
    const _: () = assert!(offset_of!(xmlParserInput, line) == 52);
    const _: () = assert!(offset_of!(xmlParserInput, consumed) == 64);
    const _: () = assert!(offset_of!(xmlParserInput, entity) == 112);

    const _: () = assert!(size_of::<xmlSAXHandler>() == 256);
    const _: () = assert!(offset_of!(xmlSAXHandler, initialized) == 216);
    const _: () = assert!(offset_of!(xmlSAXHandler, serror) == 248);

    const _: () = assert!(size_of::<xmlNode>() == 120);
    const _: () = assert!(offset_of!(xmlNode, ns) == 72);
    const _: () = assert!(offset_of!(xmlNode, nsDef) == 96);
    const _: () = assert!(offset_of!(xmlNode, line) == 112);
    const _: () = assert!(size_of::<xmlAttr>() == 96);
    const _: () = assert!(offset_of!(xmlAttr, atype) == 80);
    const _: () = assert!(size_of::<xmlDoc>() == 176);
    const _: () = assert!(offset_of!(xmlDoc, intSubset) == 80);
    const _: () = assert!(offset_of!(xmlDoc, oldNs) == 96);
    const _: () = assert!(offset_of!(xmlDoc, URL) == 136);
    const _: () = assert!(offset_of!(xmlDoc, dict) == 152);
    const _: () = assert!(offset_of!(xmlDoc, properties) == 172);
    const _: () = assert!(size_of::<xmlNs>() == 48);
    const _: () = assert!(offset_of!(xmlNs, _private) == 32);
    const _: () = assert!(size_of::<xmlDtd>() == 128);
    const _: () = assert!(offset_of!(xmlDtd, pentities) == 120);
    const _: () = assert!(size_of::<xmlEntity>() == 152);
    const _: () = assert!(offset_of!(xmlEntity, etype) == 92);
    const _: () = assert!(offset_of!(xmlEntity, URI) == 120);
    const _: () = assert!(size_of::<xmlURI>() == 88);
    const _: () = assert!(offset_of!(xmlURI, path) == 48);
}
