//! Hand-written declarations for the system libxslt 1.1.x / libexslt, the
//! libxml2 XPath functions the crate needs that `php-libxml` does not
//! declare, a few libc functions, and the C glue compiled by `build.rs`
//! (`csrc/glue.c`).
//!
//! libxslt structures are opaque here: every field access goes through a
//! glue accessor, so no layout is replicated. Node and document pointers
//! use `php-libxml`'s (layout-checked) `xmlNode`/`xmlDoc` declarations.
//!
//! Everything in here is a raw declaration; the safe surface is built in
//! the private `raw` module.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

use std::ffi::{c_char, c_double, c_int, c_void};

pub use php_libxml::ffi::{
    xmlChar, xmlDoc, xmlDocCopyNode, xmlFree, xmlFreeDoc, xmlFreeNode, xmlFreeProp, xmlGetNodePath,
    xmlNode, xmlNodeGetContent, xmlNs,
};

/// Opaque `xsltStylesheet`.
#[repr(C)]
#[derive(Debug)]
pub struct xsltStylesheet {
    _opaque: [u8; 0],
}

/// Opaque `xsltTransformContext`.
#[repr(C)]
#[derive(Debug)]
pub struct xsltTransformContext {
    _opaque: [u8; 0],
}

/// Opaque `xsltSecurityPrefs`.
#[repr(C)]
#[derive(Debug)]
pub struct xsltSecurityPrefs {
    _opaque: [u8; 0],
}

/// Opaque `xmlXPathParserContext`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathParserContext {
    _opaque: [u8; 0],
}

/// Opaque `xmlXPathObject`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlXPathObject {
    _opaque: [u8; 0],
}

/// Opaque `xmlNodeSet`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNodeSet {
    _opaque: [u8; 0],
}

/// Opaque C `FILE`.
#[repr(C)]
#[derive(Debug)]
pub struct FILE {
    _opaque: [u8; 0],
}

/// `xmlXPathFunction`.
pub type xmlXPathFunction = Option<unsafe extern "C" fn(*mut xmlXPathParserContext, c_int)>;

/// `xsltSecurityCheck`.
pub type xsltSecurityCheck = Option<
    unsafe extern "C" fn(*mut xsltSecurityPrefs, *mut xsltTransformContext, *const c_char) -> c_int,
>;

// xmlXPathObjectType
pub const XPATH_UNDEFINED: c_int = 0;
pub const XPATH_NODESET: c_int = 1;
pub const XPATH_BOOLEAN: c_int = 2;
pub const XPATH_NUMBER: c_int = 3;
pub const XPATH_STRING: c_int = 4;

// xsltSecurityOption
pub const XSLT_SECPREF_READ_FILE: c_int = 1;
pub const XSLT_SECPREF_WRITE_FILE: c_int = 2;
pub const XSLT_SECPREF_CREATE_DIRECTORY: c_int = 3;
pub const XSLT_SECPREF_READ_NETWORK: c_int = 4;
pub const XSLT_SECPREF_WRITE_NETWORK: c_int = 5;

// libxslt and libexslt are bound lazily (see `build.rs`): the generated
// stubs define the functions below (and those `csrc/glue.c` calls) and load
// the libraries on the first call, keeping them out of every start-up.
include!(concat!(env!("OUT_DIR"), "/lazy_xslt.rs"));
include!(concat!(env!("OUT_DIR"), "/lazy_exslt.rs"));

/// Reads libxslt's `int` global `name` (`xsltMaxDepth`, ...), which a
/// lazily bound library cannot export to a Rust `extern static`; loads
/// libxslt if needed. 0 if the library lacks the symbol.
fn xslt_int_global(name: &std::ffi::CStr) -> c_int {
    let p = lazy_xslt_sym(name).cast::<c_int>();
    if p.is_null() {
        0
    } else {
        // SAFETY: `name` is an initialised `int` global of libxslt, which
        // stays loaded for the life of the process.
        unsafe { p.read() }
    }
}

/// `xsltMaxDepth`.
pub fn xslt_max_depth() -> c_int {
    xslt_int_global(c"xsltMaxDepth")
}
/// `xsltMaxVars`.
pub fn xslt_max_vars() -> c_int {
    xslt_int_global(c"xsltMaxVars")
}
/// `xsltLibxsltVersion`.
pub fn xslt_libxslt_version() -> c_int {
    xslt_int_global(c"xsltLibxsltVersion")
}
/// `xsltLibxmlVersion`.
pub fn xslt_libxml_version() -> c_int {
    xslt_int_global(c"xsltLibxmlVersion")
}

extern "C" {
    pub fn xsltParseStylesheetDoc(doc: *mut xmlDoc) -> *mut xsltStylesheet;
    pub fn xsltFreeStylesheet(style: *mut xsltStylesheet);
    pub fn xsltNewTransformContext(
        style: *mut xsltStylesheet,
        doc: *mut xmlDoc,
    ) -> *mut xsltTransformContext;
    pub fn xsltFreeTransformContext(ctxt: *mut xsltTransformContext);
    pub fn xsltApplyStylesheetUser(
        style: *mut xsltStylesheet,
        doc: *mut xmlDoc,
        params: *mut *const c_char,
        output: *const c_char,
        profile: *mut FILE,
        userCtxt: *mut xsltTransformContext,
    ) -> *mut xmlDoc;
    pub fn xsltQuoteOneUserParam(
        ctxt: *mut xsltTransformContext,
        name: *const xmlChar,
        value: *const xmlChar,
    ) -> c_int;

    pub fn xsltNewSecurityPrefs() -> *mut xsltSecurityPrefs;
    pub fn xsltFreeSecurityPrefs(sec: *mut xsltSecurityPrefs);
    pub fn xsltSetSecurityPrefs(
        sec: *mut xsltSecurityPrefs,
        option: c_int,
        func: xsltSecurityCheck,
    ) -> c_int;
    pub fn xsltSetCtxtSecurityPrefs(
        sec: *mut xsltSecurityPrefs,
        ctxt: *mut xsltTransformContext,
    ) -> c_int;
    pub fn xsltSecurityForbid(
        sec: *mut xsltSecurityPrefs,
        ctxt: *mut xsltTransformContext,
        value: *const c_char,
    ) -> c_int;

    pub fn xsltSaveResultToString(
        doc_txt_ptr: *mut *mut xmlChar,
        doc_txt_len: *mut c_int,
        result: *mut xmlDoc,
        style: *mut xsltStylesheet,
    ) -> c_int;
    pub fn xsltSaveResultToFilename(
        uri: *const c_char,
        result: *mut xmlDoc,
        style: *mut xsltStylesheet,
        compression: c_int,
    ) -> c_int;

    pub fn xsltRegisterExtModuleFunction(
        name: *const xmlChar,
        uri: *const xmlChar,
        function: xmlXPathFunction,
    ) -> c_int;
    pub fn xsltRegisterExtFunction(
        ctxt: *mut xsltTransformContext,
        name: *const xmlChar,
        uri: *const xmlChar,
        function: xmlXPathFunction,
    ) -> c_int;
    pub fn xsltXPathGetTransformContext(
        ctxt: *mut xmlXPathParserContext,
    ) -> *mut xsltTransformContext;
}

extern "C" {
    pub fn exsltRegisterAll();
}

#[link(name = "xml2")]
extern "C" {
    pub fn valuePop(ctxt: *mut xmlXPathParserContext) -> *mut xmlXPathObject;
    pub fn valuePush(ctxt: *mut xmlXPathParserContext, value: *mut xmlXPathObject) -> c_int;
    pub fn xmlXPathFreeObject(obj: *mut xmlXPathObject);
    pub fn xmlXPathNewNodeSet(val: *mut xmlNode) -> *mut xmlXPathObject;
    pub fn xmlXPathNewString(val: *const xmlChar) -> *mut xmlXPathObject;
    pub fn xmlXPathNewBoolean(val: c_int) -> *mut xmlXPathObject;
    pub fn xmlXPathCastToString(val: *mut xmlXPathObject) -> *mut xmlChar;
    pub fn xmlValidateNCName(value: *const xmlChar, space: c_int) -> c_int;
    pub fn xmlCopyDoc(doc: *mut xmlDoc, recursive: c_int) -> *mut xmlDoc;
}

extern "C" {
    pub fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE;
    pub fn fclose(file: *mut FILE) -> c_int;
}

// The C glue (csrc/glue.c).
extern "C" {
    pub fn php_xsl_glue_install_xslt_error_handler();
    pub fn php_xsl_glue_push_libxml_generic(saved: *mut *mut c_void);
    pub fn php_xsl_glue_pop_libxml_generic(saved: *mut *mut c_void);
    pub fn php_xsl_glue_sanitize_globals(saved: *mut c_int);
    pub fn php_xsl_glue_restore_globals(saved: *const c_int);

    pub fn php_xsl_glue_libxslt_version() -> c_int;
    pub fn php_xsl_glue_libxslt_dotted_version() -> *const c_char;
    pub fn php_xsl_glue_libexslt_version() -> c_int;
    pub fn php_xsl_glue_libexslt_dotted_version() -> *const c_char;

    pub fn php_xsl_glue_sheet_doc(sheet: *mut xsltStylesheet) -> *mut xmlDoc;
    pub fn php_xsl_glue_ctxt_configure(
        ctxt: *mut xsltTransformContext,
        private: *mut c_void,
        xinclude: c_int,
        max_template_depth: c_int,
        max_template_vars: c_int,
    );
    pub fn php_xsl_glue_ctxt_private(ctxt: *mut xsltTransformContext) -> *mut c_void;
    pub fn php_xsl_glue_ctxt_clear_private(ctxt: *mut xsltTransformContext);
    pub fn php_xsl_glue_ctxt_stop(ctxt: *mut xsltTransformContext);
    pub fn php_xsl_glue_ctxt_main_doc(ctxt: *mut xsltTransformContext) -> *mut xmlDoc;

    pub fn php_xsl_glue_xpath_function(ctxt: *mut xmlXPathParserContext) -> *const xmlChar;
    pub fn php_xsl_glue_xpath_function_uri(ctxt: *mut xmlXPathParserContext) -> *const xmlChar;
    pub fn php_xsl_glue_xpath_doc(ctxt: *mut xmlXPathParserContext) -> *mut xmlDoc;

    pub fn php_xsl_glue_xpobj_type(obj: *mut xmlXPathObject) -> c_int;
    pub fn php_xsl_glue_xpobj_stringval(obj: *mut xmlXPathObject) -> *const xmlChar;
    pub fn php_xsl_glue_xpobj_boolval(obj: *mut xmlXPathObject) -> c_int;
    pub fn php_xsl_glue_xpobj_floatval(obj: *mut xmlXPathObject) -> c_double;
    pub fn php_xsl_glue_xpobj_node_count(obj: *mut xmlXPathObject) -> c_int;
    pub fn php_xsl_glue_xpobj_node(obj: *mut xmlXPathObject, i: c_int) -> *mut xmlNode;

    pub fn php_xsl_glue_node_type(node: *mut xmlNode) -> c_int;
    pub fn php_xsl_glue_node_doc(node: *mut xmlNode) -> *mut xmlDoc;
    pub fn php_xsl_glue_node_name(node: *mut xmlNode) -> *const xmlChar;
    pub fn php_xsl_glue_node_children(node: *mut xmlNode) -> *mut xmlNode;
    pub fn php_xsl_glue_node_next(node: *mut xmlNode) -> *mut xmlNode;
    pub fn php_xsl_glue_node_parent(node: *mut xmlNode) -> *mut xmlNode;
    pub fn php_xsl_glue_node_ns_href(node: *mut xmlNode) -> *const xmlChar;
    pub fn php_xsl_glue_nsnode_parent(ns: *mut xmlNs) -> *mut xmlNode;
    pub fn php_xsl_glue_nsnode_prefix(ns: *mut xmlNs) -> *const xmlChar;
    pub fn php_xsl_glue_nsnode_href(ns: *mut xmlNs) -> *const xmlChar;
    pub fn php_xsl_glue_add_ns_defs(doc: *mut xmlDoc);
    pub fn php_xsl_glue_add_ns_defs_from(orig: *mut xmlDoc, copy: *mut xmlDoc);
}
