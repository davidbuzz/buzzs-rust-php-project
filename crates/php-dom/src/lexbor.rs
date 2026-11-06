//! Safe wrappers over lexbor (compiled from php-src's vendored copy) and
//! the C glue in `csrc/glue.c`: the HTML5 parser, fragment parser,
//! encodings, CSS selector engine and read-only lexbor DOM accessors.

use std::alloc::Layout;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::sync::Once;

use crate::ffi::{xmlNode, xmlNs};
use crate::ns::{
    ns_is_fast, HTML_TOKEN, MATHML_TOKEN, SVG_TOKEN, XLINK_TOKEN, XMLNS_TOKEN, XML_TOKEN,
};
use crate::raw::N;

#[repr(C)]
struct LxbEncodingData {
    _private: [u8; 0],
}
#[repr(C)]
struct LxbHtmlParser {
    _private: [u8; 0],
}
#[repr(C)]
struct LxbHtmlDocument {
    _private: [u8; 0],
}
#[repr(C)]
struct LxbNodeRaw {
    _private: [u8; 0],
}
#[repr(C)]
struct LxbAttrRaw {
    _private: [u8; 0],
}
#[repr(C)]
struct CssQueryRaw {
    _private: [u8; 0],
}
/// `lexbor_array_obj_t`.
#[repr(C)]
struct LxbArrayObj {
    _private: [u8; 0],
}

type NsIsFastFn = unsafe extern "C" fn(*const xmlNode, c_int) -> bool;
type NsHtmlDocFn = unsafe extern "C" fn(*const xmlNode) -> bool;
type HtmlErrorFn = unsafe extern "C" fn(*mut c_void, bool, *const c_char, usize, usize, usize);
type CssCb = unsafe extern "C" fn(*const xmlNode, *mut c_void) -> c_int;
type WriteFn = unsafe extern "C" fn(*mut c_void, *const c_char, usize);

type LxbMallocFn = unsafe extern "C" fn(usize) -> *mut c_void;
type LxbReallocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type LxbCallocFn = unsafe extern "C" fn(usize, usize) -> *mut c_void;
type LxbFreeFn = unsafe extern "C" fn(*mut c_void);

extern "C" {
    fn lexbor_memory_setup(
        new_malloc: LxbMallocFn,
        new_realloc: LxbReallocFn,
        new_calloc: LxbCallocFn,
        new_free: LxbFreeFn,
    ) -> c_uint;
    fn phpdom_glue_init(ns_is_fast: NsIsFastFn, ns_html_doc: NsHtmlDocFn);
    fn phpdom_glue_take_exception(code: *mut c_int) -> *const c_char;

    fn phpdom_css_compile(
        selectors: *const c_char,
        len: usize,
        match_first: bool,
        quirks: bool,
        err: *mut c_char,
        err_cap: usize,
        err_len: *mut usize,
    ) -> *mut CssQueryRaw;
    fn phpdom_css_find(
        q: *mut CssQueryRaw,
        root: *const xmlNode,
        cb: CssCb,
        ctx: *mut c_void,
    ) -> c_int;
    fn phpdom_css_match_node(
        q: *mut CssQueryRaw,
        node: *const xmlNode,
        cb: CssCb,
        ctx: *mut c_void,
    ) -> c_int;
    fn phpdom_css_free(q: *mut CssQueryRaw);

    fn phpdom_encoding_by_name(name: *const c_char, len: usize) -> *const LxbEncodingData;
    fn phpdom_encoding_name(data: *const LxbEncodingData) -> *const c_char;
    fn phpdom_encoding_is_utf8(data: *const LxbEncodingData) -> bool;
    fn phpdom_encode_from_utf8(
        data: *const LxbEncodingData,
        input: *const c_char,
        len: usize,
        write: WriteFn,
        ctx: *mut c_void,
    );

    fn phpdom_html_parser_new(
        override_encoding: *const LxbEncodingData,
        report_errors: bool,
        html_no_implied: bool,
        error_fn: Option<HtmlErrorFn>,
        error_app: *mut c_void,
    ) -> *mut LxbHtmlParser;
    fn phpdom_html_parser_free(p: *mut LxbHtmlParser);
    fn phpdom_html_parse_string(p: *mut LxbHtmlParser, source: *const c_char, len: usize) -> bool;
    fn phpdom_html_parse_file_chunk(p: *mut LxbHtmlParser, data: *const c_char, len: usize)
        -> bool;
    fn phpdom_html_parser_set_encoding(p: *mut LxbHtmlParser, encoding: *const LxbEncodingData);
    fn phpdom_html_parse_file_finish(p: *mut LxbHtmlParser) -> bool;
    fn phpdom_html_parser_document_last_child(p: *mut LxbHtmlParser) -> *mut LxbNodeRaw;
    fn phpdom_html_parser_encoding_name(p: *mut LxbHtmlParser) -> *const c_char;
    fn phpdom_html_parser_observations(
        p: *mut LxbHtmlParser,
        html: *mut bool,
        head: *mut bool,
        body: *mut bool,
        quirks: *mut c_int,
    );

    fn phpdom_html_fragment_parse(
        quirks_mode: c_int,
        context_name: *const c_char,
        context_name_len: usize,
        ns_uri: *const c_char,
        ns_uri_len: usize,
        input: *const c_char,
        input_len: usize,
        root_out: *mut *mut LxbNodeRaw,
    ) -> *mut LxbHtmlDocument;
    fn phpdom_html_document_destroy(document: *mut LxbHtmlDocument);

    fn phpdom_lxb_ns_ids(out: *mut usize);
    fn phpdom_lxb_ns_xmlns() -> usize;
    fn phpdom_lxb_node_type(node: *const LxbNodeRaw) -> c_int;
    fn phpdom_lxb_node_prev(node: *const LxbNodeRaw) -> *mut LxbNodeRaw;
    fn phpdom_lxb_node_last_child(node: *const LxbNodeRaw) -> *mut LxbNodeRaw;
    fn phpdom_lxb_node_line(node: *const LxbNodeRaw) -> usize;
    fn phpdom_lxb_node_ns(node: *const LxbNodeRaw) -> usize;
    fn phpdom_lxb_element_qname(node: *mut LxbNodeRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_is_template(node: *mut LxbNodeRaw) -> bool;
    fn phpdom_lxb_template_content_last_child(
        node: *mut LxbNodeRaw,
        has_content: *mut bool,
    ) -> *mut LxbNodeRaw;
    fn phpdom_lxb_first_attr(node: *mut LxbNodeRaw) -> *mut LxbAttrRaw;
    fn phpdom_lxb_attr_next(attr: *const LxbAttrRaw) -> *mut LxbAttrRaw;
    fn phpdom_lxb_attr_qname(attr: *mut LxbAttrRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_attr_has_prefix(attr: *const LxbAttrRaw) -> bool;
    fn phpdom_lxb_attr_ns(attr: *const LxbAttrRaw) -> usize;
    fn phpdom_lxb_attr_value(attr: *mut LxbAttrRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_chardata(node: *mut LxbNodeRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_doctype_name(node: *mut LxbNodeRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_doctype_public_id(node: *mut LxbNodeRaw, len: *mut usize) -> *const u8;
    fn phpdom_lxb_doctype_system_id(node: *mut LxbNodeRaw, len: *mut usize) -> *const u8;

    fn phpdom_work_list_create() -> *mut LxbArrayObj;
    fn phpdom_work_list_push(
        list: *mut LxbArrayObj,
        node: *const LxbNodeRaw,
        current_active_namespace: usize,
        lxml_parent: *mut c_void,
        lxml_ns: *mut c_void,
        in_template: bool,
    ) -> bool;
    fn phpdom_work_list_pop(list: *mut LxbArrayObj, out: *mut WorkItemRaw) -> bool;
    fn phpdom_work_list_destroy(list: *mut LxbArrayObj);
}

unsafe extern "C" fn cb_ns_is_fast(node: *const xmlNode, token: c_int) -> bool {
    crate::parse::guard(false, || {
        let Some(n) = N::new(node.cast_mut()) else {
            return false;
        };
        let t = match token {
            1 => &HTML_TOKEN,
            2 => &MATHML_TOKEN,
            3 => &SVG_TOKEN,
            4 => &XLINK_TOKEN,
            5 => &XML_TOKEN,
            6 => &XMLNS_TOKEN,
            _ => return false,
        };
        ns_is_fast(n, t)
    })
}

unsafe extern "C" fn cb_ns_html_doc(node: *const xmlNode) -> bool {
    crate::parse::guard(false, || {
        N::new(node.cast_mut()).is_some_and(crate::ns::is_html_and_document_is_html)
    })
}

// ---------------------------------------------------------------------
// lexbor's allocator (ref: ext/lexbor/php_lexbor.c `PHP_MINIT(lexbor)`)
// ---------------------------------------------------------------------

/// Size of the hidden header in front of every lexbor block: the block's
/// total size (needed to rebuild its `Layout` on `realloc`/`free`), padded
/// to `malloc`'s alignment.
const LXB_HDR: usize = 16;

fn lxb_layout(total: usize) -> Option<Layout> {
    Layout::from_size_align(total, LXB_HDR).ok()
}

/// Allocates `size` bytes behind a size header from the Rust global
/// allocator.
#[allow(clippy::cast_ptr_alignment)] // the block is LXB_HDR (16)-aligned
unsafe fn lxb_alloc(size: usize, zeroed: bool) -> *mut c_void {
    let Some(layout) = size.checked_add(LXB_HDR).and_then(lxb_layout) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size (at least the header).
    let base = unsafe {
        if zeroed {
            std::alloc::alloc_zeroed(layout)
        } else {
            std::alloc::alloc(layout)
        }
    };
    if base.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `base` is valid for `layout.size() >= LXB_HDR` bytes and
    // 16-aligned, so the header write and the offset stay in bounds.
    unsafe {
        base.cast::<usize>().write(layout.size());
        base.add(LXB_HDR).cast()
    }
}

unsafe extern "C" fn lxb_malloc(size: usize) -> *mut c_void {
    // SAFETY: plain allocation.
    unsafe { lxb_alloc(size, false) }
}

unsafe extern "C" fn lxb_calloc(num: usize, size: usize) -> *mut c_void {
    match num.checked_mul(size) {
        // SAFETY: plain allocation.
        Some(n) => unsafe { lxb_alloc(n, true) },
        None => ptr::null_mut(),
    }
}

#[allow(clippy::cast_ptr_alignment)] // blocks are LXB_HDR (16)-aligned
unsafe extern "C" fn lxb_realloc(dst: *mut c_void, size: usize) -> *mut c_void {
    if dst.is_null() {
        // SAFETY: plain allocation.
        return unsafe { lxb_alloc(size, false) };
    }
    let Some(new_total) = size
        .checked_add(LXB_HDR)
        .filter(|&t| lxb_layout(t).is_some())
    else {
        return ptr::null_mut();
    };
    // SAFETY: lexbor only passes blocks from `lxb_alloc`/`lxb_realloc`, which
    // carry their total size in the header in front of them; the new size
    // forms a valid layout (checked above).
    unsafe {
        let base = dst.cast::<u8>().sub(LXB_HDR);
        let Some(old) = lxb_layout(base.cast::<usize>().read()) else {
            return ptr::null_mut();
        };
        let p = std::alloc::realloc(base, old, new_total);
        if p.is_null() {
            return ptr::null_mut();
        }
        p.cast::<usize>().write(new_total);
        p.add(LXB_HDR).cast()
    }
}

#[allow(clippy::cast_ptr_alignment)] // blocks are LXB_HDR (16)-aligned
unsafe extern "C" fn lxb_free(dst: *mut c_void) {
    if dst.is_null() {
        return;
    }
    // SAFETY: as in `lxb_realloc`: the block came from `lxb_alloc`.
    unsafe {
        let base = dst.cast::<u8>().sub(LXB_HDR);
        if let Some(layout) = lxb_layout(base.cast::<usize>().read()) {
            std::alloc::dealloc(base, layout);
        }
    }
}

/// Routes lexbor's allocations through the Rust global allocator (php-src
/// sets `femalloc`/`efree` with `lexbor_memory_setup`), so the lexbor tree
/// counts towards `memory_limit` and a huge document raises PHP's
/// "Allowed memory size" fatal (gh25118) instead of growing unaccounted.
/// Unlike php-src the blocks are not fallible: a soft-limit violation is
/// recorded and raised after the method returns, and only the hard
/// ceiling fails an allocation. Then registers the namespace callbacks
/// used by the selector engine. Runs before the first lexbor allocation:
/// every entry point that creates lexbor objects calls it first.
fn init() {
    static INIT: Once = Once::new();
    // SAFETY: the callbacks are plain functions valid for the program's
    // lifetime, and no lexbor block exists yet (so none is freed by a
    // different allocator than the one that made it).
    INIT.call_once(|| unsafe {
        let _ = lexbor_memory_setup(lxb_malloc, lxb_realloc, lxb_calloc, lxb_free);
        phpdom_glue_init(cb_ns_is_fast, cb_ns_html_doc);
    });
}

/// The `DOMException` thrown from inside the selector engine, if any.
pub(crate) fn take_exception() -> Option<(c_int, &'static str)> {
    let mut code = 0;
    // SAFETY: returns a static string literal or NULL.
    let p = unsafe { phpdom_glue_take_exception(&raw mut code) };
    if p.is_null() {
        return None;
    }
    // SAFETY: NUL-terminated static string from selectors.c.
    let s = unsafe { CStr::from_ptr(p) };
    Some((code, s.to_str().unwrap_or("")))
}

unsafe fn slice<'a>(p: *const u8, len: usize) -> &'a [u8] {
    if p.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: caller guarantees `p` is valid for `len` bytes.
        unsafe { std::slice::from_raw_parts(p, len) }
    }
}

// ---------------------------------------------------------------------
// Encodings
// ---------------------------------------------------------------------

/// A lexbor encoding (`lxb_encoding_data_t`, static data).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Encoding(NonNull<LxbEncodingData>);

impl Encoding {
    /// `lxb_encoding_data_by_name`.
    pub(crate) fn by_name(name: &[u8]) -> Option<Encoding> {
        // SAFETY: valid buffer; returns static data or NULL.
        NonNull::new(
            unsafe { phpdom_encoding_by_name(name.as_ptr().cast(), name.len()) }.cast_mut(),
        )
        .map(Encoding)
    }

    /// The canonical name.
    pub(crate) fn name(self) -> &'static [u8] {
        // SAFETY: static NUL-terminated name.
        unsafe { CStr::from_ptr(phpdom_encoding_name(self.0.as_ptr())) }.to_bytes()
    }

    /// Converts serializer output (UTF-8) to this encoding (`dom_common_save`).
    pub(crate) fn encode_from_utf8(self, input: &[u8]) -> Vec<u8> {
        unsafe extern "C" fn write(ctx: *mut c_void, buf: *const c_char, len: usize) {
            // SAFETY: ctx is the Vec below; buf valid for len.
            unsafe {
                let v = &mut *ctx.cast::<Vec<u8>>();
                v.extend_from_slice(slice(buf.cast(), len));
            }
        }
        let mut out: Vec<u8> = Vec::with_capacity(input.len());
        // SAFETY: valid input; the callback only appends to `out`.
        unsafe {
            phpdom_encode_from_utf8(
                self.0.as_ptr(),
                input.as_ptr().cast(),
                input.len(),
                write,
                (&raw mut out).cast(),
            );
        }
        out
    }

    /// Whether this is UTF-8.
    #[allow(dead_code)]
    pub(crate) fn is_utf8(self) -> bool {
        // SAFETY: static encoding data.
        unsafe { phpdom_encoding_is_utf8(self.0.as_ptr()) }
    }
}

// ---------------------------------------------------------------------
// lexbor DOM (read-only)
// ---------------------------------------------------------------------

/// Node type constants (`LXB_DOM_NODE_TYPE_*`).
pub(crate) const LXB_ELEMENT: c_int = 1;
pub(crate) const LXB_TEXT: c_int = 3;
pub(crate) const LXB_COMMENT: c_int = 8;
pub(crate) const LXB_DOCUMENT_TYPE: c_int = 10;

/// lexbor namespace ids.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NsIds {
    pub(crate) undef: usize,
    pub(crate) html: usize,
    pub(crate) svg: usize,
    pub(crate) math: usize,
    pub(crate) xlink: usize,
    pub(crate) xml: usize,
    pub(crate) xmlns: usize,
}

pub(crate) fn ns_ids() -> NsIds {
    let mut out = [0usize; 6];
    // SAFETY: writes six values.
    unsafe { phpdom_lxb_ns_ids(out.as_mut_ptr()) };
    NsIds {
        undef: out[0],
        html: out[1],
        svg: out[2],
        math: out[3],
        xlink: out[4],
        xml: out[5],
        // SAFETY: constant accessor.
        xmlns: unsafe { phpdom_lxb_ns_xmlns() },
    }
}

/// A lexbor node, valid while the owning lexbor document `'a` lives.
#[derive(Clone, Copy)]
pub(crate) struct LxbNode<'a>(NonNull<LxbNodeRaw>, PhantomData<&'a ()>);

/// A lexbor attribute.
#[derive(Clone, Copy)]
pub(crate) struct LxbAttr<'a>(NonNull<LxbAttrRaw>, PhantomData<&'a ()>);

impl<'a> LxbNode<'a> {
    fn new(p: *mut LxbNodeRaw) -> Option<LxbNode<'a>> {
        NonNull::new(p).map(|p| LxbNode(p, PhantomData))
    }
    fn p(self) -> *mut LxbNodeRaw {
        self.0.as_ptr()
    }
    pub(crate) fn node_type(self) -> c_int {
        // SAFETY: live node (lifetime 'a).
        unsafe { phpdom_lxb_node_type(self.p()) }
    }
    pub(crate) fn prev(self) -> Option<LxbNode<'a>> {
        // SAFETY: live node.
        LxbNode::new(unsafe { phpdom_lxb_node_prev(self.p()) })
    }
    pub(crate) fn last_child(self) -> Option<LxbNode<'a>> {
        // SAFETY: live node.
        LxbNode::new(unsafe { phpdom_lxb_node_last_child(self.p()) })
    }
    pub(crate) fn line(self) -> usize {
        // SAFETY: live node.
        unsafe { phpdom_lxb_node_line(self.p()) }
    }
    pub(crate) fn ns(self) -> usize {
        // SAFETY: live node.
        unsafe { phpdom_lxb_node_ns(self.p()) }
    }
    /// Element qualified name.
    pub(crate) fn qname(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live element node; the name lives as long as the document.
        unsafe { slice(phpdom_lxb_element_qname(self.p(), &raw mut len), len) }
    }
    pub(crate) fn is_template(self) -> bool {
        // SAFETY: live node.
        unsafe { phpdom_lxb_is_template(self.p()) }
    }
    /// The template contents' last child: `Err(())` when the template has no
    /// contents fragment.
    pub(crate) fn template_content_last_child(self) -> Result<Option<LxbNode<'a>>, ()> {
        let mut has = false;
        // SAFETY: live template element.
        let p = unsafe { phpdom_lxb_template_content_last_child(self.p(), &raw mut has) };
        if has {
            Ok(LxbNode::new(p))
        } else {
            Err(())
        }
    }
    pub(crate) fn first_attr(self) -> Option<LxbAttr<'a>> {
        // SAFETY: live element.
        NonNull::new(unsafe { phpdom_lxb_first_attr(self.p()) }).map(|p| LxbAttr(p, PhantomData))
    }
    /// Text/comment data.
    pub(crate) fn chardata(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live character data node.
        unsafe { slice(phpdom_lxb_chardata(self.p(), &raw mut len), len) }
    }
    pub(crate) fn doctype_name(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live doctype node.
        unsafe { slice(phpdom_lxb_doctype_name(self.p(), &raw mut len), len) }
    }
    pub(crate) fn doctype_public_id(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live doctype node.
        unsafe { slice(phpdom_lxb_doctype_public_id(self.p(), &raw mut len), len) }
    }
    pub(crate) fn doctype_system_id(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live doctype node.
        unsafe { slice(phpdom_lxb_doctype_system_id(self.p(), &raw mut len), len) }
    }
}

impl<'a> LxbAttr<'a> {
    fn p(self) -> *mut LxbAttrRaw {
        self.0.as_ptr()
    }
    pub(crate) fn next(self) -> Option<LxbAttr<'a>> {
        // SAFETY: live attribute.
        NonNull::new(unsafe { phpdom_lxb_attr_next(self.p()) }).map(|p| LxbAttr(p, PhantomData))
    }
    pub(crate) fn qname(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live attribute.
        unsafe { slice(phpdom_lxb_attr_qname(self.p(), &raw mut len), len) }
    }
    pub(crate) fn has_prefix(self) -> bool {
        // SAFETY: live attribute.
        unsafe { phpdom_lxb_attr_has_prefix(self.p()) }
    }
    pub(crate) fn ns(self) -> usize {
        // SAFETY: live attribute.
        unsafe { phpdom_lxb_attr_ns(self.p()) }
    }
    pub(crate) fn value(self) -> &'a [u8] {
        let mut len = 0;
        // SAFETY: live attribute.
        unsafe { slice(phpdom_lxb_attr_value(self.p(), &raw mut len), len) }
    }
}

// ---------------------------------------------------------------------
// Document parser
// ---------------------------------------------------------------------

/// Receives parse errors: `(is_tree, code, line, column, len)`.
pub(crate) type ErrorSink<'e> = Box<dyn FnMut(bool, &str, usize, usize, usize) + 'e>;

struct ErrorBox<'e>(ErrorSink<'e>);

unsafe extern "C" fn error_trampoline(
    app: *mut c_void,
    is_tree: bool,
    code: *const c_char,
    line: usize,
    column: usize,
    len: usize,
) {
    crate::parse::guard((), || {
        // SAFETY: `app` is the ErrorBox owned by the HtmlParser; code is a static string.
        unsafe {
            let b = &mut *app.cast::<ErrorBox<'_>>();
            let code = CStr::from_ptr(code).to_str().unwrap_or("unknown error");
            (b.0)(is_tree, code, line, column, len);
        }
    });
}

/// What the tree builder observed (`lexbor_libxml2_bridge_extracted_observations`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Observations {
    pub(crate) has_explicit_html_tag: bool,
    pub(crate) has_explicit_head_tag: bool,
    pub(crate) has_explicit_body_tag: bool,
    /// 0 = no quirks, 1 = limited quirks, 2 = quirks.
    pub(crate) quirks_mode: c_int,
}

/// An in-progress HTML5 document parse.
pub(crate) struct HtmlParser<'e> {
    p: NonNull<LxbHtmlParser>,
    _errors: Option<Box<ErrorBox<'e>>>,
}

impl<'e> HtmlParser<'e> {
    /// `None` on allocation failure.
    pub(crate) fn new(
        override_encoding: Option<Encoding>,
        html_no_implied: bool,
        errors: Option<ErrorSink<'e>>,
    ) -> Option<Self> {
        init();
        let mut boxed = errors.map(|e| Box::new(ErrorBox(e)));
        let app: *mut c_void = boxed
            .as_mut()
            .map_or(ptr::null_mut(), |b| (&raw mut **b).cast());
        // SAFETY: `app` outlives the parser (both owned by Self).
        let p = unsafe {
            phpdom_html_parser_new(
                override_encoding.map_or(ptr::null(), |e| e.0.as_ptr()),
                boxed.is_some(),
                html_no_implied,
                boxed.as_ref().map(|_| error_trampoline as HtmlErrorFn),
                app,
            )
        };
        Some(HtmlParser {
            p: NonNull::new(p)?,
            _errors: boxed,
        })
    }

    /// Parses a whole string; `false` on failure.
    pub(crate) fn parse_string(&mut self, source: &[u8]) -> bool {
        // SAFETY: live parser and buffer.
        let r = unsafe {
            phpdom_html_parse_string(self.p.as_ptr(), source.as_ptr().cast(), source.len())
        };
        crate::parse::resume_panic();
        r
    }

    /// Feeds one read (at most 4096 bytes) of a file.
    pub(crate) fn feed_file_chunk(&mut self, data: &[u8]) -> bool {
        // SAFETY: live parser and buffer.
        let r = unsafe {
            phpdom_html_parse_file_chunk(self.p.as_ptr(), data.as_ptr().cast(), data.len())
        };
        crate::parse::resume_panic();
        r
    }

    /// Forces the encoding (charset sniffed from the stream).
    pub(crate) fn set_encoding(&mut self, encoding: Encoding) {
        // SAFETY: live parser.
        unsafe { phpdom_html_parser_set_encoding(self.p.as_ptr(), encoding.0.as_ptr()) };
    }

    /// Finishes a file parse.
    pub(crate) fn finish_file(&mut self) -> bool {
        // SAFETY: live parser.
        let r = unsafe { phpdom_html_parse_file_finish(self.p.as_ptr()) };
        crate::parse::resume_panic();
        r
    }

    /// The last child of the lexbor document.
    pub(crate) fn document_last_child(&self) -> Option<LxbNode<'_>> {
        // SAFETY: live parser.
        LxbNode::new(unsafe { phpdom_html_parser_document_last_child(self.p.as_ptr()) })
    }

    /// The decoder's encoding name.
    pub(crate) fn encoding_name(&self) -> Vec<u8> {
        // SAFETY: static NUL-terminated name.
        unsafe { CStr::from_ptr(phpdom_html_parser_encoding_name(self.p.as_ptr())) }
            .to_bytes()
            .to_vec()
    }

    pub(crate) fn observations(&self) -> Observations {
        let mut o = Observations::default();
        // SAFETY: live parser; writes four values.
        unsafe {
            phpdom_html_parser_observations(
                self.p.as_ptr(),
                &raw mut o.has_explicit_html_tag,
                &raw mut o.has_explicit_head_tag,
                &raw mut o.has_explicit_body_tag,
                &raw mut o.quirks_mode,
            );
        }
        o
    }
}

impl Drop for HtmlParser<'_> {
    fn drop(&mut self) {
        // SAFETY: owned parser, freed once.
        unsafe { phpdom_html_parser_free(self.p.as_ptr()) };
    }
}

/// A parsed HTML fragment (owns its lexbor document).
pub(crate) struct Fragment {
    doc: *mut LxbHtmlDocument,
    root: *mut LxbNodeRaw,
}

impl Fragment {
    /// The HTML fragment parsing algorithm with the given context element.
    pub(crate) fn parse(
        quirks_mode: c_int,
        context_name: &[u8],
        ns_uri: &[u8],
        input: &[u8],
    ) -> Fragment {
        init();
        let mut root = ptr::null_mut();
        // SAFETY: valid buffers; the returned document is owned by Fragment.
        let doc = unsafe {
            phpdom_html_fragment_parse(
                quirks_mode,
                context_name.as_ptr().cast(),
                context_name.len(),
                ns_uri.as_ptr().cast(),
                ns_uri.len(),
                input.as_ptr().cast(),
                input.len(),
                &raw mut root,
            )
        };
        Fragment { doc, root }
    }

    /// The fragment root (`None` when parsing failed).
    pub(crate) fn root(&self) -> Option<LxbNode<'_>> {
        LxbNode::new(self.root)
    }
}

impl Drop for Fragment {
    fn drop(&mut self) {
        // SAFETY: owned document (or NULL).
        unsafe { phpdom_html_document_destroy(self.doc) };
    }
}

// ---------------------------------------------------------------------
// The bridge's work list (html5_parser.c `work_list`)
// ---------------------------------------------------------------------

/// `work_list_item` as the C side stores it (`csrc/glue.c`).
#[repr(C)]
struct WorkItemRaw {
    node: *mut LxbNodeRaw,
    current_active_namespace: usize,
    lxml_parent: *mut xmlNode,
    lxml_ns: *mut xmlNs,
    in_template: bool,
}

/// A node waiting to be converted (`work_list_item`).
pub(crate) struct WorkItem<'a> {
    pub(crate) node: LxbNode<'a>,
    pub(crate) current_active_namespace: usize,
    pub(crate) lxml_parent: N,
    pub(crate) lxml_ns: *mut xmlNs,
    pub(crate) in_template: bool,
}

/// The stack of nodes the lexbor → libxml2 bridge still has to convert:
/// php-src's `work_list`, a `lexbor_array_obj_t` in lexbor's allocator.
/// Using lexbor's own array gives the list php-src's memory footprint
/// (lexbor sizes it as `sizeof(uint8_t *)` × items × item size and grows
/// it 128 items at a time), so a huge flat document reaches `memory_limit`
/// while it is converted, as it does in PHP (GH-25118).
pub(crate) struct WorkList<'a>(NonNull<LxbArrayObj>, PhantomData<&'a ()>);

impl<'a> WorkList<'a> {
    /// `lexbor_array_obj_init(&work_list, WORK_LIST_INIT_SIZE, ...)`;
    /// `None` when the allocation failed.
    pub(crate) fn new() -> Option<Self> {
        init();
        // SAFETY: a fresh list from lexbor's allocator, or NULL.
        NonNull::new(unsafe { phpdom_work_list_create() }).map(|p| WorkList(p, PhantomData))
    }

    /// `lexbor_libxml2_bridge_work_list_item_push`: `false` when the list
    /// could not grow.
    #[must_use]
    pub(crate) fn push(&mut self, item: WorkItem<'a>) -> bool {
        // SAFETY: live list; the item's fields are copied into it.
        unsafe {
            phpdom_work_list_push(
                self.0.as_ptr(),
                item.node.p(),
                item.current_active_namespace,
                item.lxml_parent.p().cast(),
                item.lxml_ns.cast(),
                item.in_template,
            )
        }
    }

    /// `lexbor_array_obj_pop`: the most recently pushed item.
    pub(crate) fn pop(&mut self) -> Option<WorkItem<'a>> {
        let mut raw = WorkItemRaw {
            node: ptr::null_mut(),
            current_active_namespace: 0,
            lxml_parent: ptr::null_mut(),
            lxml_ns: ptr::null_mut(),
            in_template: false,
        };
        // SAFETY: live list; `raw` is written only when an item exists.
        if !unsafe { phpdom_work_list_pop(self.0.as_ptr(), &raw mut raw) } {
            return None;
        }
        Some(WorkItem {
            node: LxbNode::new(raw.node)?,
            current_active_namespace: raw.current_active_namespace,
            lxml_parent: N::new(raw.lxml_parent)?,
            lxml_ns: raw.lxml_ns,
            in_template: raw.in_template,
        })
    }
}

impl Drop for WorkList<'_> {
    fn drop(&mut self) {
        // SAFETY: owned list, freed once.
        unsafe { phpdom_work_list_destroy(self.0.as_ptr()) };
    }
}

// ---------------------------------------------------------------------
// CSS selectors
// ---------------------------------------------------------------------

/// Why a selector list could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CssCompileError {
    /// The first message of lexbor's CSS log.
    Message(Vec<u8>),
    /// No log message.
    Unknown,
}

/// A compiled selector list plus the matching engine.
pub(crate) struct CssQuery(NonNull<CssQueryRaw>);

struct CbState<'f> {
    f: &'f mut dyn FnMut(N) -> bool,
}

unsafe extern "C" fn css_trampoline(node: *const xmlNode, ctx: *mut c_void) -> c_int {
    crate::parse::guard(1, || {
        // SAFETY: ctx is the CbState passed to find/match.
        let st = unsafe { &mut *ctx.cast::<CbState<'_>>() };
        match N::new(node.cast_mut()) {
            Some(n) => c_int::from((st.f)(n)),
            None => 0,
        }
    })
}

impl CssQuery {
    pub(crate) fn compile(
        selectors: &[u8],
        match_first: bool,
        quirks: bool,
    ) -> Result<CssQuery, CssCompileError> {
        init();
        let mut err = [0u8; 1024];
        let mut err_len = usize::MAX;
        // SAFETY: valid buffers.
        let q = unsafe {
            phpdom_css_compile(
                selectors.as_ptr().cast(),
                selectors.len(),
                match_first,
                quirks,
                err.as_mut_ptr().cast(),
                err.len(),
                &raw mut err_len,
            )
        };
        match NonNull::new(q) {
            Some(q) => Ok(CssQuery(q)),
            None if err_len == usize::MAX => Err(CssCompileError::Unknown),
            None => Err(CssCompileError::Message(
                err[..err_len.min(err.len())].to_vec(),
            )),
        }
    }

    /// `lxb_selectors_find`; `f` returns `true` to stop.  `Err` for an
    /// unsupported selector.
    pub(crate) fn find(&self, root: N, f: &mut dyn FnMut(N) -> bool) -> Result<(), ()> {
        let mut st = CbState { f };
        // SAFETY: live query and node; the callback state outlives the call.
        let r = unsafe {
            phpdom_css_find(
                self.0.as_ptr(),
                root.p(),
                css_trampoline,
                (&raw mut st).cast(),
            )
        };
        crate::parse::resume_panic();
        if r == 0 {
            Ok(())
        } else {
            Err(())
        }
    }

    /// `lxb_selectors_match_node`.
    pub(crate) fn match_node(&self, node: N, f: &mut dyn FnMut(N) -> bool) -> Result<(), ()> {
        let mut st = CbState { f };
        // SAFETY: as in `find`.
        let r = unsafe {
            phpdom_css_match_node(
                self.0.as_ptr(),
                node.p(),
                css_trampoline,
                (&raw mut st).cast(),
            )
        };
        crate::parse::resume_panic();
        if r == 0 {
            Ok(())
        } else {
            Err(())
        }
    }
}

impl Drop for CssQuery {
    fn drop(&mut self) {
        // SAFETY: owned query, freed once.
        unsafe { phpdom_css_free(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{lxb_calloc, lxb_free, lxb_malloc, lxb_realloc};

    #[test]
    fn lexbor_allocator_round_trip() {
        // SAFETY: blocks are used within their sizes and freed once.
        unsafe {
            let p = lxb_malloc(10).cast::<u8>();
            assert!(!p.is_null());
            assert_eq!(p.addr() % 16, 0);
            for i in 0..10 {
                p.add(i).write(u8::try_from(i).unwrap_or(0));
            }
            let q = lxb_realloc(p.cast(), 1000).cast::<u8>();
            assert!(!q.is_null());
            for i in 0..10 {
                assert_eq!(q.add(i).read(), u8::try_from(i).unwrap_or(0));
            }
            let q = lxb_realloc(q.cast(), 4).cast::<u8>();
            assert_eq!(q.add(3).read(), 3);
            lxb_free(q.cast());

            let z = lxb_calloc(8, 8).cast::<u8>();
            assert!((0..64).all(|i| z.add(i).read() == 0));
            lxb_free(z.cast());

            assert!(lxb_calloc(usize::MAX, 2).is_null());
            assert!(lxb_malloc(usize::MAX).is_null());
            let r = lxb_realloc(std::ptr::null_mut(), 3);
            assert!(!r.is_null());
            lxb_free(r);
            lxb_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn html_parse_after_allocator_setup() {
        let doc = crate::html::create_from(
            b"<p>a</p><template><b>x</b></template>",
            0,
            None,
            false,
            &crate::host::NoHost,
        );
        assert!(doc.is_ok());
    }
}
