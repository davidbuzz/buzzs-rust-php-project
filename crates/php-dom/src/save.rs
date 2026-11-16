// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Serialization through libxml2 (`saveXML`, `saveHTML`, `C14N`, …) and
//! dispatch to the spec serializers for `Dom\*` documents.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::node::C14nXPath;
use crate::object::{Handlers, Node};
use crate::raw::{cs, N};

pub(crate) unsafe extern "C" fn write_vec(
    ctx: *mut c_void,
    buf: *const c_char,
    len: c_int,
) -> c_int {
    crate::parse::guard(-1, || {
        // SAFETY: ctx is the `Vec<u8>` passed at creation; buf is valid for len.
        unsafe {
            let v = &mut *ctx.cast::<Vec<u8>>();
            v.extend_from_slice(std::slice::from_raw_parts(
                buf.cast::<u8>(),
                usize::try_from(len).unwrap_or(0),
            ));
        }
        len
    })
}

/// An output buffer collecting into a `Vec<u8>`.
pub(crate) struct VecOutput {
    pub(crate) buf: *mut ffi::xmlOutputBuffer,
    #[allow(clippy::box_collection)]
    data: Box<Vec<u8>>,
}

impl VecOutput {
    pub(crate) fn new(encoder: *mut ffi::xmlCharEncodingHandler) -> Option<Self> {
        let mut data = Box::new(Vec::new());
        // SAFETY: the context pointer stays valid as long as `data` lives,
        // which outlives the buffer (closed in `finish`).
        let buf = unsafe {
            ffi::xmlOutputBufferCreateIO(Some(write_vec), None, (&raw mut *data).cast(), encoder)
        };
        if buf.is_null() {
            return None;
        }
        Some(Self { buf, data })
    }

    /// Whether libxml2 reported an output error.
    pub(crate) fn error(&self) -> c_int {
        // SAFETY: live output buffer.
        unsafe { (*self.buf.cast::<ffi::xmlOutputBufferFields>()).error }
    }

    /// Flushes and closes; `None` on error.
    pub(crate) fn finish(self) -> Option<Vec<u8>> {
        // SAFETY: live buffer, closed exactly once.
        let ok = unsafe {
            let f = ffi::xmlOutputBufferFlush(self.buf);
            ffi::xmlOutputBufferClose(self.buf);
            f >= 0
        };
        let data = *self.data;
        if ok {
            Some(data)
        } else {
            None
        }
    }
}

/// Temporarily sets libxml2's `xmlSaveNoEmptyTags`.
pub(crate) fn with_no_empty_tags<R>(on: bool, f: impl FnOnce() -> R) -> R {
    // SAFETY: per-thread global.
    let slot = unsafe { ffi::__xmlSaveNoEmptyTags() };
    // SAFETY: valid pointer to the global.
    let old = unsafe { *slot };
    // SAFETY: as above.
    unsafe { *slot = i32::from(on) };
    let r = f();
    // SAFETY: as above.
    unsafe { *slot = old };
    r
}

/// `php_libxml_default_dump_doc_to_str`.
fn default_dump_doc_to_str(doc: N, options: c_int, encoding: *const c_char) -> Option<Vec<u8>> {
    let mut data: Box<Vec<u8>> = Box::default();
    crate::parse::with_generic_errors(|| {
        // SAFETY: save context writing into `data`.
        unsafe {
            let ctxt = ffi::xmlSaveToIO(
                Some(write_vec),
                None,
                (&raw mut *data).cast(),
                encoding,
                options,
            );
            if ctxt.is_null() {
                return None;
            }
            let mut status = ffi::xmlSaveDoc(ctxt, doc.as_doc());
            status |= std::ffi::c_long::from(ffi::xmlSaveClose(ctxt));
            if status < 0 {
                return None;
            }
            Some(())
        }
    })?;
    Some(*data)
}

/// `php_libxml_default_dump_node_to_str`.
fn default_dump_node_to_str(
    doc: N,
    node: N,
    format: bool,
    encoding: *const c_char,
) -> Option<Vec<u8>> {
    let out = VecOutput::new(ptr::null_mut())?;
    crate::parse::with_generic_errors(|| {
        // SAFETY: live nodes and output buffer.
        unsafe {
            ffi::xmlNodeDumpOutput(
                out.buf,
                doc.as_doc(),
                node.p(),
                0,
                c_int::from(format),
                encoding,
            );
        };
    });
    out.finish()
}

/// `xmlNodeDumpOutput` into a fresh buffer (used for internal subsets).
pub(crate) fn node_dump_output(
    doc: *mut ffi::xmlDoc,
    node: N,
    level: c_int,
    format: bool,
    encoding: Option<&[u8]>,
) -> Vec<u8> {
    let Some(out) = VecOutput::new(ptr::null_mut()) else {
        return Vec::new();
    };
    let e = encoding.map(cs);
    // SAFETY: live node and output buffer.
    unsafe {
        ffi::xmlNodeDumpOutput(
            out.buf,
            doc,
            node.p(),
            level,
            c_int::from(format),
            e.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
        );
    }
    out.finish().unwrap_or_default()
}

fn doc_encoding(doc: N) -> *const c_char {
    doc.d().encoding.cast()
}

/// `handlers->dump_doc_to_str`.
pub(crate) fn dump_doc_to_str(
    handlers: Handlers,
    doc: N,
    options: c_int,
) -> DomResult<Option<Vec<u8>>> {
    match handlers {
        Handlers::Default => Ok(default_dump_doc_to_str(doc, options, doc_encoding(doc))),
        Handlers::ModernXml => crate::xml_serializer::dump_node_to_str_ex(
            doc,
            options,
            options & ffi::XML_SAVE_FORMAT != 0,
            crate::raw::opt_cstr(doc.d().encoding),
        ),
    }
}

/// `handlers->dump_node_to_str`.
pub(crate) fn dump_node_to_str(
    handlers: Handlers,
    doc: N,
    node: N,
    format: bool,
) -> DomResult<Option<Vec<u8>>> {
    match handlers {
        Handlers::Default => Ok(default_dump_node_to_str(
            doc,
            node,
            format,
            doc_encoding(doc),
        )),
        Handlers::ModernXml => crate::xml_serializer::dump_node_to_str_ex(
            node,
            0,
            format,
            crate::raw::opt_cstr(doc.d().encoding),
        ),
    }
}

/// `handlers->dump_doc_to_file`.
pub(crate) fn dump_doc_to_file(
    handlers: Handlers,
    filename: &[u8],
    doc: N,
    format: bool,
) -> DomResult<i64> {
    match handlers {
        Handlers::Default => {
            let f = cs(filename);
            Ok(crate::parse::with_generic_errors(|| {
                // SAFETY: live document.
                i64::from(unsafe {
                    ffi::xmlSaveFormatFileEnc(
                        f.as_ptr(),
                        doc.as_doc(),
                        doc_encoding(doc),
                        c_int::from(format),
                    )
                })
            }))
        }
        Handlers::ModernXml => crate::xml_serializer::dump_node_to_file(filename, doc, doc, format),
    }
}

/// `saveHTML($node)` for one node.
pub(crate) fn html_dump_node(doc: N, node: N, format: bool) -> Option<Vec<u8>> {
    let Some(out) = VecOutput::new(ptr::null_mut()) else {
        crate::error::warning("Could not fetch output buffer");
        return None;
    };
    crate::parse::with_generic_errors(|| {
        // SAFETY: live nodes and output buffer.
        unsafe {
            if node.t() == ffi::XML_DOCUMENT_FRAG_NODE {
                for c in node.child_iter() {
                    ffi::htmlNodeDumpFormatOutput(
                        out.buf,
                        doc.as_doc(),
                        c.p(),
                        ptr::null(),
                        c_int::from(format),
                    );
                    if out.error() != 0 {
                        break;
                    }
                }
            } else {
                ffi::htmlNodeDumpFormatOutput(
                    out.buf,
                    doc.as_doc(),
                    node.p(),
                    ptr::null(),
                    c_int::from(format),
                );
            }
        }
    });
    if out.error() != 0 {
        crate::error::warning("Error dumping HTML node");
        // SAFETY: closing the buffer.
        unsafe { ffi::xmlOutputBufferClose(out.buf) };
        std::mem::forget(out);
        return None;
    }
    out.finish()
}

/// `saveHTML()` of the whole document.
pub(crate) fn html_dump_doc(doc: N, format: bool) -> Option<Vec<u8>> {
    let mut mem: *mut u8 = ptr::null_mut();
    let mut size: c_int = 0;
    crate::parse::with_generic_errors(|| {
        // SAFETY: live document; mem receives a libxml2 allocation.
        unsafe {
            ffi::htmlDocDumpMemoryFormat(
                doc.as_doc(),
                &raw mut mem,
                &raw mut size,
                c_int::from(format),
            );
        };
    });
    if size == 0 || mem.is_null() {
        crate::raw::xml_free(mem.cast());
        return None;
    }
    // SAFETY: mem holds `size` bytes.
    let v = unsafe { std::slice::from_raw_parts(mem, usize::try_from(size).unwrap_or(0)) }.to_vec();
    crate::raw::xml_free(mem.cast());
    Some(v)
}

/// `saveHTMLFile()`.
pub(crate) fn html_save_file(doc: N, filename: &[u8], format: bool) -> i64 {
    let f = cs(filename);
    crate::parse::with_generic_errors(|| {
        // SAFETY: live document.
        unsafe {
            let enc = ffi::htmlGetMetaEncoding(doc.as_doc());
            i64::from(ffi::htmlSaveFileFormat(
                f.as_ptr(),
                doc.as_doc(),
                enc.cast(),
                c_int::from(format),
            ))
        }
    })
}

/// The outcome of [`canonicalize`].
#[derive(Debug)]
pub(crate) enum C14nOut {
    Bytes(Vec<u8>),
    Written(i64),
    Failed,
}

unsafe extern "C" fn c14n_parent_lookup(
    user: *mut c_void,
    node: *mut ffi::xmlNode,
    parent: *mut ffi::xmlNode,
) -> c_int {
    let root = user.cast::<ffi::xmlNode>();
    if node == root {
        return 1;
    }
    let mut n = parent;
    while !n.is_null() {
        if n == root {
            return 1;
        }
        // SAFETY: walking parents of live nodes.
        n = unsafe { (*n).parent };
    }
    0
}

/// `dom_canonicalization`.
#[allow(clippy::too_many_lines)]
pub(crate) fn canonicalize(
    this: &Node,
    file: Option<&[u8]>,
    exclusive: bool,
    with_comments: bool,
    xpath: Option<&C14nXPath>,
    ns_prefixes: Option<&[Vec<u8>]>,
) -> DomResult<C14nOut> {
    let nodep = this.node()?;
    let docp = nodep.doc_ptr();
    let modern = crate::lists::follow_spec_node(nodep);
    let mut links = crate::ns::NsLinks::default();
    if modern {
        let mut root = nodep;
        while let Some(p) = root.parent() {
            root = p;
        }
        if !root.is_doc() {
            return Err(DomError::dom_msg(
                C::HierarchyRequest,
                "Canonicalization can only happen on nodes attached to a document.",
            ));
        }
        // SAFETY: live document.
        if let Some(re) = N::new(unsafe { ffi::xmlDocGetRootElement(docp) }) {
            links.relink(re);
        }
    } else if docp.is_null() {
        return Err(DomError::Error(
            "Node must be associated with a document".into(),
        ));
    }
    let arg_base = if file.is_some() { 4 } else { 3 };
    let result = (|| -> DomResult<C14nOut> {
        let mut simple_callback = false;
        let mut ctxp: *mut ffi::xmlXPathContext = ptr::null_mut();
        let mut xpathobjp: *mut ffi::xmlXPathObject = ptr::null_mut();
        let mut nodeset: *mut ffi::xmlNodeSet = ptr::null_mut();
        match xpath {
            None => {
                if !nodep.is_doc() {
                    simple_callback = true;
                }
            }
            Some(x) => {
                let query = match &x.query {
                    None => return Err(DomError::arg_value(arg_base, "must have a \"query\" key")),
                    Some(Err(ty)) => {
                        return Err(DomError::arg_type(
                            arg_base,
                            format!("\"query\" option must be a string, {ty} given"),
                        ))
                    }
                    Some(Ok(q)) => q.clone(),
                };
                // SAFETY: XPath context/object created and freed below.
                unsafe {
                    ctxp = ffi::xmlXPathNewContext(docp);
                    (*ctxp).node = nodep.p();
                    for (prefix, uri) in &x.namespaces {
                        let p = cs(prefix);
                        let u = cs(uri);
                        ffi::xmlXPathRegisterNs(ctxp, p.as_ptr().cast(), u.as_ptr().cast());
                    }
                    let q = cs(&query);
                    xpathobjp = crate::parse::with_generic_errors(|| {
                        ffi::xmlXPathEvalExpression(q.as_ptr().cast(), ctxp)
                    });
                    (*ctxp).node = ptr::null_mut();
                    if !xpathobjp.is_null() && (*xpathobjp).type_ == ffi::XPATH_NODESET {
                        nodeset = (*xpathobjp).nodesetval;
                    } else {
                        if !xpathobjp.is_null() {
                            ffi::xmlXPathFreeObject(xpathobjp);
                        }
                        ffi::xmlXPathFreeContext(ctxp);
                        return Err(DomError::Error(
                            "XPath query did not return a nodeset".into(),
                        ));
                    }
                }
            }
        }
        let mut prefixes: Vec<std::ffi::CString> = Vec::new();
        let mut prefix_ptrs: Vec<*mut u8> = Vec::new();
        let mut use_prefixes = false;
        if let Some(list) = ns_prefixes {
            if exclusive {
                prefixes = list.iter().map(|p| cs(p)).collect();
                prefix_ptrs = prefixes
                    .iter()
                    .map(|c| c.as_ptr().cast_mut().cast())
                    .collect();
                prefix_ptrs.push(ptr::null_mut());
                use_prefixes = true;
            } else {
                crate::error::notice(
                    "Inclusive namespace prefixes only allowed in exclusive mode.",
                );
            }
        }
        // SAFETY: buffers/contexts created and freed in this block.
        let out = unsafe {
            let buf = match file {
                Some(f) => {
                    let c = cs(f);
                    ffi::xmlOutputBufferCreateFilename(c.as_ptr(), ptr::null_mut(), 0)
                }
                None => ffi::xmlAllocOutputBuffer(ptr::null_mut()),
            };
            let mut ret = -1;
            if !buf.is_null() {
                let pp = if use_prefixes {
                    prefix_ptrs.as_mut_ptr()
                } else {
                    ptr::null_mut()
                };
                ret = crate::parse::with_generic_errors(|| {
                    if simple_callback {
                        ffi::xmlC14NExecute(
                            docp,
                            Some(c14n_parent_lookup),
                            nodep.p().cast(),
                            c_int::from(exclusive),
                            pp,
                            c_int::from(with_comments),
                            buf,
                        )
                    } else {
                        ffi::xmlC14NDocSaveTo(
                            docp,
                            nodeset,
                            c_int::from(exclusive),
                            pp,
                            c_int::from(with_comments),
                            buf,
                        )
                    }
                });
            }
            drop(prefixes);
            if !xpathobjp.is_null() {
                ffi::xmlXPathFreeObject(xpathobjp);
            }
            if !ctxp.is_null() {
                ffi::xmlXPathFreeContext(ctxp);
            }
            let mut out = C14nOut::Failed;
            if !buf.is_null() && ret >= 0 && file.is_none() {
                let size = ffi::xmlOutputBufferGetSize(buf);
                let content = ffi::xmlOutputBufferGetContent(buf);
                out = C14nOut::Bytes(if size > 0 {
                    std::slice::from_raw_parts(content, size).to_vec()
                } else {
                    Vec::new()
                });
            }
            if !buf.is_null() {
                let bytes = ffi::xmlOutputBufferClose(buf);
                if file.is_some() && ret >= 0 {
                    out = C14nOut::Written(i64::from(bytes));
                }
            }
            out
        };
        Ok(out)
    })();
    if modern {
        links.unlink();
    }
    result
}
