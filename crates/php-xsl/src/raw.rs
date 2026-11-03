//! The unsafe boundary of the crate.
//!
//! Every `unsafe` block lives here (or in the declarations of
//! [`crate::ffi`]); the types exported to the rest of the crate encode
//! libxslt's ownership rules:
//!
//! * [`Stylesheet`] owns an `xsltStylesheet` (and through it the private
//!   copy of the stylesheet document).
//! * [`Transform`] owns an `xsltTransformContext` plus its security
//!   preferences, and registers itself as *active* so that the XPath
//!   extension functions only ever dereference contexts created here.
//! * [`XPathCall`] / [`XPathValue`] wrap one invocation of an extension
//!   function: popping arguments, building node arguments
//!   (`xsl_proxy_factory`) and pushing the result.
//!
//! libxslt/libxml2 report errors through C-variadic callbacks; those are
//! defined in `csrc/glue.c`, which forwards each formatted message to
//! [`php_xsl_rs_error_sink`]. Panics raised by Rust code running inside a
//! libxslt callback are caught at the boundary, stashed and resumed once
//! control is back in Rust ([`resume_panic`]).

use std::any::Any;
use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::marker::PhantomData;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::Once;

use crate::callbacks::PHP_NAMESPACE;
use crate::document::ResultDocument;
use crate::ffi;
use crate::host::{NodeArg, NodeRef, OwnedNode};

const XSLT_NAMESPACE: &[u8] = b"http://www.w3.org/1999/XSL/Transform";

static GLOBAL_INIT: Once = Once::new();

thread_local! {
    static PENDING_PANIC: RefCell<Option<Box<dyn Any + Send>>> = const { RefCell::new(None) };
    /// Transformation contexts created by this crate on this thread.
    static ACTIVE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Initialises libxml2 (through `php-libxml`) and, once per process,
/// registers EXSLT, the `php:function`/`php:functionString` module
/// functions and PHP's libxslt error handler (`PHP_MINIT_FUNCTION(xsl)`).
pub(crate) fn ensure_init() {
    php_libxml::raw::ensure_init();
    GLOBAL_INIT.call_once(|| {
        // SAFETY: plain registration calls with static NUL-terminated
        // strings and `extern "C"` functions valid for the whole process.
        unsafe {
            ffi::exsltRegisterAll();
            ffi::xsltRegisterExtModuleFunction(
                c"functionString".as_ptr().cast(),
                c"http://php.net/xsl".as_ptr().cast(),
                Some(ext_function_string),
            );
            ffi::xsltRegisterExtModuleFunction(
                c"function".as_ptr().cast(),
                c"http://php.net/xsl".as_ptr().cast(),
                Some(ext_function_object),
            );
            ffi::php_xsl_glue_install_xslt_error_handler();
        }
    });
    debug_assert_eq!(PHP_NAMESPACE, b"http://php.net/xsl");
}

// ---------------------------------------------------------------------------
// Panics and error messages
// ---------------------------------------------------------------------------

fn stash_panic(payload: Box<dyn Any + Send>) {
    PENDING_PANIC.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            *p = Some(payload);
        }
    });
}

fn panic_pending() -> bool {
    PENDING_PANIC.with(|p| p.borrow().is_some())
}

/// Re-raises a panic caught inside a libxslt callback, if any.
pub(crate) fn resume_panic() {
    if let Some(payload) = PENDING_PANIC.with(|p| p.borrow_mut().take()) {
        resume_unwind(payload);
    }
}

/// `xsl_libxslt_error_handler`'s rewriting of libxslt option names into
/// PHP property names (only for messages passed through `"%s"`).
pub(crate) fn replace_option_names(msg: Vec<u8>) -> Vec<u8> {
    const REPLACEMENTS: [(&[u8], &[u8]); 2] = [
        (b"xsltMaxDepth (--maxdepth)", b"$maxTemplateDepth"),
        (b"maxTemplateVars (--maxvars)", b"$maxTemplateVars"),
    ];
    for (search, replace) in REPLACEMENTS {
        if let Some(pos) = msg.windows(search.len()).position(|w| w == search) {
            let mut out = Vec::with_capacity(msg.len());
            out.extend_from_slice(&msg[..pos]);
            out.extend_from_slice(replace);
            out.extend_from_slice(&msg[pos + search.len()..]);
            return out;
        }
    }
    msg
}

/// Reports a message fragment the way `php_libxml_ctx_error(NULL, …)` /
/// `php_libxml_error_handler` do: buffered until a fragment ends in a
/// newline, then stored in the libxml error list or displayed as a plain
/// warning.
pub(crate) fn report_fragment(msg: &[u8]) {
    if panic_pending() {
        return;
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
        php_libxml::error::ctx_error(None, msg);
    })) {
        stash_panic(payload);
    }
}

/// The sink of the C error handlers (`csrc/glue.c`).
///
/// # Safety
/// `msg` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn php_xsl_rs_error_sink(replace: c_int, msg: *const c_char, len: usize) {
    if msg.is_null() {
        return;
    }
    // SAFETY: guaranteed by the caller (the glue passes a formatted buffer).
    let bytes = unsafe { std::slice::from_raw_parts(msg.cast::<u8>(), len) }.to_vec();
    let bytes = if replace != 0 {
        replace_option_names(bytes)
    } else {
        bytes
    };
    report_fragment(&bytes);
}

/// Routes libxml2's generic error channel (`xmlGenericError`, used e.g.
/// by the XPath engine for "function not found") to PHP's handler while
/// alive, restoring the previous handler on drop.
pub(crate) struct GenericErrorScope {
    saved: [*mut c_void; 2],
}

impl GenericErrorScope {
    pub(crate) fn new() -> Self {
        let mut saved = [std::ptr::null_mut(); 2];
        // SAFETY: saved has room for the two pointers the glue stores.
        unsafe { ffi::php_xsl_glue_push_libxml_generic(saved.as_mut_ptr()) };
        Self { saved }
    }
}

impl Drop for GenericErrorScope {
    fn drop(&mut self) {
        // SAFETY: restores exactly what `new` saved on this thread.
        unsafe { ffi::php_xsl_glue_pop_libxml_generic(self.saved.as_mut_ptr()) };
    }
}

/// `PHP_LIBXML_SANITIZE_GLOBALS(parse)` plus XSL's parser overrides, undone
/// on drop (`PHP_LIBXML_RESTORE_GLOBALS(parse)`).
struct ParseGlobalsScope {
    saved: [c_int; 6],
}

impl ParseGlobalsScope {
    fn new() -> Self {
        let mut saved = [0; 6];
        // SAFETY: saved has room for the six values the glue stores.
        unsafe { ffi::php_xsl_glue_sanitize_globals(saved.as_mut_ptr()) };
        Self { saved }
    }
}

impl Drop for ParseGlobalsScope {
    fn drop(&mut self) {
        // SAFETY: restores what `new` saved.
        unsafe { ffi::php_xsl_glue_restore_globals(self.saved.as_ptr()) };
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A C string from PHP bytes, truncated at the first NUL (what handing a
/// `char *` to libxslt does).
pub(crate) fn c_string(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Copies a possibly-NULL C string.
///
/// # Safety
/// `p` must be NULL or a NUL-terminated string valid for reads.
unsafe fn opt_bytes(p: *const u8) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(p.cast()) }.to_bytes().to_vec())
    }
}

/// Copies and releases a libxml2-allocated string.
///
/// # Safety
/// `p` must be NULL or a string allocated by libxml2 and not used again.
unsafe fn take_xml_string(p: *mut u8) -> Option<Vec<u8>> {
    // SAFETY: p is NULL or a valid C string.
    let bytes = unsafe { opt_bytes(p) }?;
    // SAFETY: xmlFree is initialised by libxml2; p is released once.
    unsafe {
        if let Some(free) = ffi::xmlFree {
            free(p.cast());
        }
    }
    Some(bytes)
}

/// `xmlValidateNCName(name, 0) == 0`.
pub(crate) fn is_ncname(name: &[u8]) -> bool {
    ensure_init();
    let c = c_string(name);
    // SAFETY: c is a valid C string for the call.
    unsafe { ffi::xmlValidateNCName(c.as_ptr().cast(), 0) == 0 }
}

/// Compile-time and runtime library versions.
pub(crate) mod version {
    use super::{ffi, CStr};

    /// `LIBXSLT_VERSION`.
    pub fn libxslt() -> i64 {
        // SAFETY: returns a compile-time constant.
        i64::from(unsafe { ffi::php_xsl_glue_libxslt_version() })
    }

    /// `LIBXSLT_DOTTED_VERSION`.
    pub fn libxslt_dotted() -> String {
        // SAFETY: returns a static NUL-terminated string.
        unsafe { CStr::from_ptr(ffi::php_xsl_glue_libxslt_dotted_version()) }
            .to_string_lossy()
            .into_owned()
    }

    /// `LIBEXSLT_VERSION`.
    pub fn libexslt() -> i64 {
        // SAFETY: returns a compile-time constant.
        i64::from(unsafe { ffi::php_xsl_glue_libexslt_version() })
    }

    /// `LIBEXSLT_DOTTED_VERSION`.
    pub fn libexslt_dotted() -> String {
        // SAFETY: returns a static NUL-terminated string.
        unsafe { CStr::from_ptr(ffi::php_xsl_glue_libexslt_dotted_version()) }
            .to_string_lossy()
            .into_owned()
    }

    /// `xsltLibxsltVersion` (the loaded library).
    pub fn runtime_libxslt() -> i64 {
        // SAFETY: reading an initialised constant global.
        i64::from(unsafe { ffi::xsltLibxsltVersion })
    }

    /// `xsltLibxmlVersion`.
    pub fn runtime_libxml() -> i64 {
        // SAFETY: reading an initialised constant global.
        i64::from(unsafe { ffi::xsltLibxmlVersion })
    }

    /// `xsltMaxDepth` (default of `$maxTemplateDepth`).
    pub fn max_depth() -> i64 {
        // SAFETY: reading a global int nobody writes concurrently.
        i64::from(unsafe { ffi::xsltMaxDepth })
    }

    /// `xsltMaxVars` (default of `$maxTemplateVars`).
    pub fn max_vars() -> i64 {
        // SAFETY: reading a global int nobody writes concurrently.
        i64::from(unsafe { ffi::xsltMaxVars })
    }
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

/// # Safety
/// `node` must be a live node.
pub(crate) unsafe fn node_type(node: NonNull<ffi::xmlNode>) -> i32 {
    // SAFETY: forwarded.
    unsafe { ffi::php_xsl_glue_node_type(node.as_ptr()) }
}

/// # Safety
/// `node` must be a live node.
pub(crate) unsafe fn node_name(node: NonNull<ffi::xmlNode>) -> Vec<u8> {
    // SAFETY: forwarded; the name is NULL or a valid string.
    unsafe { opt_bytes(ffi::php_xsl_glue_node_name(node.as_ptr())) }.unwrap_or_default()
}

/// # Safety
/// `node` must be a live node.
pub(crate) unsafe fn node_content(node: NonNull<ffi::xmlNode>) -> Vec<u8> {
    // SAFETY: forwarded; the result is a fresh allocation.
    unsafe { take_xml_string(ffi::xmlNodeGetContent(node.as_ptr())) }.unwrap_or_default()
}

/// # Safety
/// `node` must be a live node.
pub(crate) unsafe fn node_path(node: NonNull<ffi::xmlNode>) -> Option<Vec<u8>> {
    // SAFETY: forwarded; the result is a fresh allocation.
    unsafe { take_xml_string(ffi::xmlGetNodePath(node.as_ptr())) }
}

/// # Safety
/// `node` must be a live node.
pub(crate) unsafe fn node_doc(node: NonNull<ffi::xmlNode>) -> *mut ffi::xmlDoc {
    // SAFETY: forwarded.
    unsafe { ffi::php_xsl_glue_node_doc(node.as_ptr()) }
}

/// Frees a detached node of any kind.
///
/// # Safety
/// `node` must be a live, parentless node exclusively owned by the caller.
pub(crate) unsafe fn free_detached(node: NonNull<ffi::xmlNode>) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match ffi::php_xsl_glue_node_type(node.as_ptr()) {
            php_libxml::ffi::XML_ATTRIBUTE_NODE => ffi::xmlFreeProp(node.as_ptr().cast()),
            php_libxml::ffi::XML_DOCUMENT_NODE | php_libxml::ffi::XML_HTML_DOCUMENT_NODE => {
                ffi::xmlFreeDoc(node.as_ptr().cast());
            }
            php_libxml::ffi::XML_NAMESPACE_DECL => {}
            _ => ffi::xmlFreeNode(node.as_ptr()),
        }
    }
}

// ---------------------------------------------------------------------------
// Stylesheets
// ---------------------------------------------------------------------------

/// A compiled stylesheet (`xsl_object::ptr`).
#[derive(Debug)]
pub(crate) struct Stylesheet {
    ptr: NonNull<ffi::xsltStylesheet>,
    uses_keys: bool,
}

impl Stylesheet {
    /// `importStylesheet`'s core: copies `source` (the clone PHP makes),
    /// restores namespace declarations for modern documents, and compiles
    /// the copy with PHP's parser globals. `None` when libxslt rejects it
    /// (its errors have been reported).
    ///
    /// # Safety
    /// `source` must be a live document.
    pub(crate) unsafe fn compile(source: NonNull<ffi::xmlDoc>, modern: bool) -> Option<Self> {
        ensure_init();
        // SAFETY: source is live; the copy is exclusively ours until it is
        // handed to libxslt (which owns it on success).
        let sheet = unsafe {
            let copy = ffi::xmlCopyDoc(source.as_ptr(), 1);
            if copy.is_null() {
                return None;
            }
            if modern {
                ffi::php_xsl_glue_add_ns_defs(copy);
            }
            let globals = ParseGlobalsScope::new();
            let errors = GenericErrorScope::new();
            let sheet = ffi::xsltParseStylesheetDoc(copy);
            drop(errors);
            drop(globals);
            if sheet.is_null() {
                // libxslt leaves the document alone on failure.
                ffi::xmlFreeDoc(copy);
            }
            sheet
        };
        resume_panic();
        let ptr = NonNull::new(sheet)?;
        // SAFETY: ptr is a freshly compiled stylesheet.
        let uses_keys = unsafe { sheet_uses_keys(ptr) };
        Some(Self { ptr, uses_keys })
    }

    /// Whether a top-level `xsl:key` exists (the input document must then
    /// be cloned before every transformation).
    pub(crate) fn uses_keys(&self) -> bool {
        self.uses_keys
    }

    pub(crate) fn as_ptr(&self) -> *mut ffi::xsltStylesheet {
        self.ptr.as_ptr()
    }
}

/// # Safety
/// `sheet` must be a live stylesheet.
unsafe fn sheet_uses_keys(sheet: NonNull<ffi::xsltStylesheet>) -> bool {
    // SAFETY: the stylesheet document and its tree are live.
    unsafe {
        let doc = ffi::php_xsl_glue_sheet_doc(sheet.as_ptr());
        if doc.is_null() {
            return false;
        }
        let root = php_libxml::ffi::xmlDocGetRootElement(doc);
        if root.is_null() {
            return false;
        }
        let mut node = ffi::php_xsl_glue_node_children(root);
        while !node.is_null() {
            if ffi::php_xsl_glue_node_type(node) == php_libxml::ffi::XML_ELEMENT_NODE
                && opt_bytes(ffi::php_xsl_glue_node_name(node)).as_deref() == Some(b"key")
                && opt_bytes(ffi::php_xsl_glue_node_ns_href(node)).as_deref()
                    == Some(XSLT_NAMESPACE)
            {
                return true;
            }
            node = ffi::php_xsl_glue_node_next(node);
        }
        false
    }
}

impl Drop for Stylesheet {
    fn drop(&mut self) {
        // SAFETY: we own the stylesheet (and its document).
        unsafe { ffi::xsltFreeStylesheet(self.ptr.as_ptr()) };
    }
}

/// Copies a document (`xmlCopyDoc(doc, 1)`) into a reference-counted
/// `php-libxml` document.
///
/// # Safety
/// `doc` must be a live document.
pub(crate) unsafe fn copy_document(
    doc: NonNull<ffi::xmlDoc>,
) -> Option<php_libxml::tree::Document> {
    // SAFETY: doc is live; the copy is fresh and exclusively ours.
    unsafe {
        let copy = NonNull::new(ffi::xmlCopyDoc(doc.as_ptr(), 1))?;
        Some(php_libxml::tree::Document::from_raw(copy))
    }
}

// ---------------------------------------------------------------------------
// Profiling file
// ---------------------------------------------------------------------------

/// The profiling output (`VCWD_FOPEN(profiling, "w")`).
#[derive(Debug)]
pub(crate) struct ProfileFile(NonNull<ffi::FILE>);

impl ProfileFile {
    pub(crate) fn open(path: &[u8]) -> Option<Self> {
        let c = c_string(path);
        // SAFETY: valid C strings; the FILE is closed on drop.
        NonNull::new(unsafe { ffi::fopen(c.as_ptr(), c"w".as_ptr()) }).map(Self)
    }
}

impl Drop for ProfileFile {
    fn drop(&mut self) {
        // SAFETY: the FILE was opened by `open` and is closed once.
        unsafe { ffi::fclose(self.0.as_ptr()) };
    }
}

// ---------------------------------------------------------------------------
// Transformations
// ---------------------------------------------------------------------------

/// The kind of extension function being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallKind {
    /// `php:functionString` (node-sets become strings).
    PhpString,
    /// `php:function` (node-sets become node arrays).
    PhpNodeset,
    /// A `registerPHPFunctionNS()` function.
    Namespaced,
}

/// Implemented by the per-transformation session (see `processor.rs`).
pub(crate) trait Dispatcher {
    fn dispatch(&self, call: &mut XPathCall<'_>, kind: CallKind);
}

/// The value stored in `ctxt->_private`: a thin pointer to this.
pub(crate) type DispatchRef<'a> = &'a dyn Dispatcher;

/// XSL security option bits (`XSL_SECPREF_*`) and their libxslt options.
const SECURITY_OPTIONS: [(i64, c_int); 5] = [
    (2, ffi::XSLT_SECPREF_READ_FILE),
    (4, ffi::XSLT_SECPREF_WRITE_FILE),
    (8, ffi::XSLT_SECPREF_CREATE_DIRECTORY),
    (16, ffi::XSLT_SECPREF_READ_NETWORK),
    (32, ffi::XSLT_SECPREF_WRITE_NETWORK),
];

/// A transformation context (`xsltTransformContext`).
pub(crate) struct Transform<'s> {
    ctxt: NonNull<ffi::xsltTransformContext>,
    sec: Option<NonNull<ffi::xsltSecurityPrefs>>,
    style: &'s Stylesheet,
    doc: NonNull<ffi::xmlDoc>,
}

impl<'s> Transform<'s> {
    /// `xsltNewTransformContext(style, doc)`.
    ///
    /// # Safety
    /// `doc` must stay alive for the lifetime of the transform.
    pub(crate) unsafe fn new(style: &'s Stylesheet, doc: NonNull<ffi::xmlDoc>) -> Option<Self> {
        // SAFETY: style and doc are live.
        let ctxt =
            NonNull::new(unsafe { ffi::xsltNewTransformContext(style.as_ptr(), doc.as_ptr()) })?;
        ACTIVE.with(|a| a.borrow_mut().push(ctxt.as_ptr() as usize));
        Some(Self {
            ctxt,
            sec: None,
            style,
            doc,
        })
    }

    /// `xsltQuoteOneUserParam`: whether it succeeded.
    pub(crate) fn quote_param(&self, name: &[u8], value: &[u8]) -> bool {
        let n = c_string(name);
        let v = c_string(value);
        // SAFETY: live context, valid C strings (copied by libxslt).
        let r = unsafe {
            ffi::xsltQuoteOneUserParam(self.ctxt.as_ptr(), n.as_ptr().cast(), v.as_ptr().cast())
        };
        resume_panic();
        r >= 0
    }

    /// Sets `_private`, `xinclude`, `maxTemplateDepth`, `maxTemplateVars`.
    ///
    /// # Safety
    /// `dispatcher` must stay valid until the transform is dropped or
    /// [`Self::apply`] returned, whichever comes first.
    pub(crate) unsafe fn configure(
        &self,
        dispatcher: *const DispatchRef<'_>,
        xinclude: bool,
        max_depth: c_int,
        max_vars: c_int,
    ) {
        // SAFETY: live context; the pointer is only read back by our
        // extension functions while the context is active.
        unsafe {
            ffi::php_xsl_glue_ctxt_configure(
                self.ctxt.as_ptr(),
                dispatcher.cast_mut().cast(),
                c_int::from(xinclude),
                max_depth,
                max_vars,
            );
        }
    }

    /// Installs security preferences forbidding the options in `prefs`
    /// (`XSL_SECPREF_*` bits). Returns false on a libxslt failure.
    pub(crate) fn set_security(&mut self, prefs: i64) -> bool {
        // SAFETY: fresh prefs object owned by self (freed on drop, after the
        // context that references it).
        unsafe {
            let Some(sec) = NonNull::new(ffi::xsltNewSecurityPrefs()) else {
                return false;
            };
            self.sec = Some(sec);
            let mut ok = true;
            for (bit, option) in SECURITY_OPTIONS {
                if prefs & bit != 0
                    && ffi::xsltSetSecurityPrefs(
                        sec.as_ptr(),
                        option,
                        Some(ffi::xsltSecurityForbid),
                    ) != 0
                {
                    ok = false;
                }
            }
            if ffi::xsltSetCtxtSecurityPrefs(sec.as_ptr(), self.ctxt.as_ptr()) != 0 {
                ok = false;
            }
            ok
        }
    }

    /// `xsltRegisterExtFunction(ctxt, name, ns, xsl_ext_function_trampoline)`.
    pub(crate) fn register_function(&self, namespace: &[u8], name: &[u8]) {
        let n = c_string(name);
        let ns = c_string(namespace);
        // SAFETY: live context; libxslt copies the strings.
        unsafe {
            ffi::xsltRegisterExtFunction(
                self.ctxt.as_ptr(),
                n.as_ptr().cast(),
                ns.as_ptr().cast(),
                Some(ext_function_trampoline),
            );
        }
    }

    /// `xsltApplyStylesheetUser(style, doc, NULL, NULL, profile, ctxt)`.
    pub(crate) fn apply(&self, profile: Option<&ProfileFile>) -> Option<ResultDocument> {
        let errors = GenericErrorScope::new();
        // SAFETY: style, doc and ctxt are live; params are applied
        // separately; the profile FILE is open.
        let result = unsafe {
            ffi::xsltApplyStylesheetUser(
                self.style.as_ptr(),
                self.doc.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null(),
                profile.map_or(std::ptr::null_mut(), |f| f.0.as_ptr()),
                self.ctxt.as_ptr(),
            )
        };
        drop(errors);
        // SAFETY: a fresh result document is exclusively ours.
        let result = NonNull::new(result).map(|p| unsafe { ResultDocument::from_raw(p) });
        resume_panic();
        result
    }
}

impl Drop for Transform<'_> {
    fn drop(&mut self) {
        let addr = self.ctxt.as_ptr() as usize;
        ACTIVE.with(|a| a.borrow_mut().retain(|&c| c != addr));
        // SAFETY: we own the context, then the prefs it referenced.
        unsafe {
            ffi::php_xsl_glue_ctxt_clear_private(self.ctxt.as_ptr());
            ffi::xsltFreeTransformContext(self.ctxt.as_ptr());
            if let Some(sec) = self.sec {
                ffi::xsltFreeSecurityPrefs(sec.as_ptr());
            }
        }
    }
}

/// `xsltSaveResultToString`: the return code and the text (`None` when
/// empty).
pub(crate) fn save_to_string(
    result: &ResultDocument,
    style: &Stylesheet,
) -> (c_int, Option<Vec<u8>>) {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len: c_int = 0;
    // SAFETY: live document and stylesheet; ptr receives a fresh buffer.
    let ret = unsafe {
        ffi::xsltSaveResultToString(&raw mut ptr, &raw mut len, result.as_ptr(), style.as_ptr())
    };
    let text = if ptr.is_null() {
        None
    } else {
        let n = usize::try_from(len).unwrap_or(0);
        // SAFETY: ptr holds len bytes allocated by libxml2, freed once.
        unsafe {
            let bytes = std::slice::from_raw_parts(ptr, n).to_vec();
            if let Some(free) = ffi::xmlFree {
                free(ptr.cast());
            }
            (n > 0).then_some(bytes)
        }
    };
    resume_panic();
    (ret, text)
}

/// `xsltSaveResultToFilename(uri, doc, style, 0)`.
pub(crate) fn save_to_filename(uri: &[u8], result: &ResultDocument, style: &Stylesheet) -> c_int {
    let c = c_string(uri);
    let errors = GenericErrorScope::new();
    // SAFETY: live document and stylesheet, valid C string.
    let ret =
        unsafe { ffi::xsltSaveResultToFilename(c.as_ptr(), result.as_ptr(), style.as_ptr(), 0) };
    drop(errors);
    resume_panic();
    ret
}

// ---------------------------------------------------------------------------
// XPath extension functions
// ---------------------------------------------------------------------------

/// One popped XPath value, freed on drop.
pub(crate) struct XPathValue {
    obj: NonNull<ffi::xmlXPathObject>,
}

/// The classified content of an XPath value.
#[derive(Debug)]
pub(crate) enum ValueKind {
    Str(Vec<u8>),
    Bool(bool),
    Number(f64),
    NodeSet,
    Other,
}

impl XPathValue {
    pub(crate) fn kind(&self) -> ValueKind {
        // SAFETY: obj is a live XPath object.
        unsafe {
            let o = self.obj.as_ptr();
            match ffi::php_xsl_glue_xpobj_type(o) {
                ffi::XPATH_STRING => ValueKind::Str(
                    opt_bytes(ffi::php_xsl_glue_xpobj_stringval(o)).unwrap_or_default(),
                ),
                ffi::XPATH_BOOLEAN => ValueKind::Bool(ffi::php_xsl_glue_xpobj_boolval(o) != 0),
                ffi::XPATH_NUMBER => ValueKind::Number(ffi::php_xsl_glue_xpobj_floatval(o)),
                ffi::XPATH_NODESET => ValueKind::NodeSet,
                _ => ValueKind::Other,
            }
        }
    }

    /// `obj->stringval` (NULL unless the value is a string).
    pub(crate) fn stringval(&self) -> Option<Vec<u8>> {
        // SAFETY: obj is live; stringval is NULL or a valid string.
        unsafe { opt_bytes(ffi::php_xsl_glue_xpobj_stringval(self.obj.as_ptr())) }
    }

    /// `xmlXPathCastToString`.
    pub(crate) fn cast_to_string(&self) -> Vec<u8> {
        // SAFETY: obj is live; the result is a fresh allocation.
        unsafe { take_xml_string(ffi::xmlXPathCastToString(self.obj.as_ptr())) }.unwrap_or_default()
    }
}

impl Drop for XPathValue {
    fn drop(&mut self) {
        // SAFETY: popped objects belong to us.
        unsafe { ffi::xmlXPathFreeObject(self.obj.as_ptr()) };
    }
}

/// One invocation of an extension function.
pub(crate) struct XPathCall<'c> {
    ctxt: NonNull<ffi::xmlXPathParserContext>,
    main_doc: *mut ffi::xmlDoc,
    nargs: usize,
    _scope: PhantomData<&'c mut ffi::xmlXPathParserContext>,
}

impl<'c> XPathCall<'c> {
    /// Number of arguments on the stack.
    pub(crate) fn nargs(&self) -> usize {
        self.nargs
    }

    /// `valuePop`.
    pub(crate) fn pop(&mut self) -> Option<XPathValue> {
        // SAFETY: live parser context.
        NonNull::new(unsafe { ffi::valuePop(self.ctxt.as_ptr()) }).map(|obj| XPathValue { obj })
    }

    /// `php_dom_xpath_callbacks_clean_argument_stack`.
    pub(crate) fn clean_stack(&mut self, n: usize) {
        for _ in 0..n {
            drop(self.pop());
        }
    }

    fn push(&mut self, obj: *mut ffi::xmlXPathObject) {
        if !obj.is_null() {
            // SAFETY: live context; ownership of obj passes to the stack.
            unsafe { ffi::valuePush(self.ctxt.as_ptr(), obj) };
        }
    }

    pub(crate) fn push_string(&mut self, s: &[u8]) {
        let c = c_string(s);
        // SAFETY: the string is copied by libxml2.
        let obj = unsafe { ffi::xmlXPathNewString(c.as_ptr().cast()) };
        self.push(obj);
    }

    pub(crate) fn push_bool(&mut self, b: bool) {
        // SAFETY: plain constructor.
        let obj = unsafe { ffi::xmlXPathNewBoolean(c_int::from(b)) };
        self.push(obj);
    }

    /// Pushes a one-node node-set. The node must stay alive until the end
    /// of the transformation (guaranteed by `ReturnedNode`'s contract).
    pub(crate) fn push_node(&mut self, node: NonNull<ffi::xmlNode>) {
        // SAFETY: node is live for the rest of the transformation.
        let obj = unsafe { ffi::xmlXPathNewNodeSet(node.as_ptr()) };
        self.push(obj);
    }

    /// `ctxt->context->function`.
    pub(crate) fn function_name(&self) -> Vec<u8> {
        // SAFETY: live context; the name is NULL or a valid string.
        unsafe { opt_bytes(ffi::php_xsl_glue_xpath_function(self.ctxt.as_ptr())) }
            .unwrap_or_default()
    }

    /// `ctxt->context->functionURI`.
    pub(crate) fn function_uri(&self) -> Option<Vec<u8>> {
        // SAFETY: live context; the URI is NULL or a valid string.
        unsafe { opt_bytes(ffi::php_xsl_glue_xpath_function_uri(self.ctxt.as_ptr())) }
    }

    /// `xsl_proxy_factory`: nodes of other documents are deep-copied into
    /// the main document.
    fn proxy(&self, node: NonNull<ffi::xmlNode>) -> Option<NodeArg<'c>> {
        // SAFETY: node is part of a node-set of the running transformation,
        // so it (and its document) is alive.
        unsafe {
            if node_doc(node) == self.main_doc {
                Some(NodeArg::Node(NodeRef {
                    ptr: node,
                    _scope: PhantomData,
                }))
            } else {
                let copy = NonNull::new(ffi::xmlDocCopyNode(node.as_ptr(), self.main_doc, 1))?;
                Some(NodeArg::Copy(OwnedNode {
                    ptr: copy,
                    _scope: PhantomData,
                }))
            }
        }
    }

    /// The nodes of a node-set value as callback arguments.
    pub(crate) fn node_args(&self, value: &XPathValue) -> Vec<NodeArg<'c>> {
        let mut out = Vec::new();
        // SAFETY: the value is a live node-set; its nodes are alive.
        unsafe {
            let o = value.obj.as_ptr();
            let count = ffi::php_xsl_glue_xpobj_node_count(o);
            for i in 0..count {
                let Some(node) = NonNull::new(ffi::php_xsl_glue_xpobj_node(o, i)) else {
                    continue;
                };
                if ffi::php_xsl_glue_node_type(node.as_ptr()) == php_libxml::ffi::XML_NAMESPACE_DECL
                {
                    let ns = node.as_ptr().cast::<ffi::xmlNs>();
                    let parent = ffi::php_xsl_glue_nsnode_parent(ns);
                    let prefix = opt_bytes(ffi::php_xsl_glue_nsnode_prefix(ns));
                    let href = opt_bytes(ffi::php_xsl_glue_nsnode_href(ns)).unwrap_or_default();
                    if let Some(parent) = NonNull::new(parent).and_then(|p| self.proxy(p)) {
                        out.push(NodeArg::Namespace {
                            parent: Box::new(parent),
                            prefix,
                            href,
                        });
                    }
                } else if let Some(arg) = self.proxy(node) {
                    out.push(arg);
                }
            }
        }
        out
    }
}

unsafe extern "C" fn ext_function_string(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    // SAFETY: called by libxslt with a live parser context.
    unsafe { ext_function(ctxt, nargs, CallKind::PhpString) }
}

unsafe extern "C" fn ext_function_object(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    // SAFETY: called by libxslt with a live parser context.
    unsafe { ext_function(ctxt, nargs, CallKind::PhpNodeset) }
}

unsafe extern "C" fn ext_function_trampoline(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int) {
    // SAFETY: called by libxslt with a live parser context.
    unsafe { ext_function(ctxt, nargs, CallKind::Namespaced) }
}

/// `xsl_ext_function_php` / `xsl_ext_function_trampoline`.
///
/// # Safety
/// `ctxt` must be NULL or a live XPath parser context of a transformation.
unsafe fn ext_function(ctxt: *mut ffi::xmlXPathParserContext, nargs: c_int, kind: CallKind) {
    let Some(ctxt) = NonNull::new(ctxt) else {
        return;
    };
    let n = usize::try_from(nargs).unwrap_or(0);
    // SAFETY: ctxt is live (caller contract).
    let tctxt = unsafe { ffi::xsltXPathGetTransformContext(ctxt.as_ptr()) };
    let mut call = XPathCall {
        ctxt,
        main_doc: std::ptr::null_mut(),
        nargs: n,
        _scope: PhantomData,
    };
    if tctxt.is_null() {
        report_fragment(b"xsltExtFunctionTest: failed to get the transformation context\n");
        call.clean_stack(n);
        return;
    }
    let known = ACTIVE.with(|a| a.borrow().contains(&(tctxt as usize)));
    // SAFETY: tctxt is live; `_private` is only interpreted for contexts
    // this crate created (and configured with a DispatchRef pointer).
    let private = if known {
        unsafe { ffi::php_xsl_glue_ctxt_private(tctxt) }
    } else {
        std::ptr::null_mut()
    };
    if private.is_null() {
        report_fragment(b"xsltExtFunctionTest: failed to get the internal object\n");
        call.clean_stack(n);
        return;
    }
    // SAFETY: tctxt is live.
    call.main_doc = unsafe { ffi::php_xsl_glue_ctxt_main_doc(tctxt) };
    if panic_pending() {
        call.clean_stack(n);
        call.push_string(b"");
        return;
    }
    // SAFETY: `private` was set by `Transform::configure` to a DispatchRef
    // that outlives the running `apply` call.
    let dispatcher: DispatchRef<'_> = unsafe { *private.cast::<DispatchRef<'_>>() };
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| dispatcher.dispatch(&mut call, kind))) {
        stash_panic(payload);
        // SAFETY: tctxt is live; stopping the engine is always allowed.
        unsafe { ffi::php_xsl_glue_ctxt_stop(tctxt) };
        call.push_string(b"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_names_are_rewritten() {
        assert_eq!(
            replace_option_names(b"You can adjust xsltMaxDepth (--maxdepth) in order".to_vec()),
            b"You can adjust $maxTemplateDepth in order"
        );
        assert_eq!(
            replace_option_names(b"adjust maxTemplateVars (--maxvars) now".to_vec()),
            b"adjust $maxTemplateVars now"
        );
        assert_eq!(replace_option_names(b"plain".to_vec()), b"plain");
    }

    #[test]
    fn ncname() {
        assert!(is_ncname(b"test"));
        assert!(!is_ncname(b"x:a"));
        assert!(!is_ncname(b"$$$"));
        assert!(!is_ncname(b""));
    }

    #[test]
    fn versions() {
        assert!(version::libxslt() >= 10100);
        assert_eq!(version::libxslt(), version::runtime_libxslt());
        let dotted = version::libxslt_dotted();
        assert_eq!(dotted.split('.').count(), 3);
        assert!(version::libexslt() > 0);
        assert_eq!(version::max_depth(), 3000);
        assert_eq!(version::max_vars(), 15000);
    }
}
