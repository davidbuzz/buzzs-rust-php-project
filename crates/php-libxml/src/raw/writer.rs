// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `xmlTextWriter` over Rust output sinks.

use std::ffi::{c_int, CString};
use std::io::Write;
use std::ptr::NonNull;

use super::io::output_buffer_from_writer;
use super::{c_string, ensure_init, guard};
use crate::ffi;

/// An owned `xmlTextWriter`. Dropping it flushes and closes the output
/// (`xmlFreeTextWriter`).
pub struct TextWriter {
    ptr: NonNull<ffi::xmlTextWriter>,
}

impl std::fmt::Debug for TextWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextWriter")
            .field("ptr", &self.ptr)
            .finish()
    }
}

fn opt_c(v: Option<&[u8]>) -> Option<CString> {
    v.map(c_string)
}

fn ptr_of(v: Option<&CString>) -> *const u8 {
    v.map_or(std::ptr::null(), |c| c.as_ptr().cast())
}

impl TextWriter {
    /// A writer whose output goes to `sink` (closed when the writer is
    /// dropped).
    pub fn to_sink(sink: Box<dyn Write>) -> Option<Self> {
        ensure_init();
        let out = output_buffer_from_writer(sink, std::ptr::null_mut());
        if out.is_null() {
            return None;
        }
        // SAFETY: `out` is a fresh output buffer; on success the writer owns
        // it, on failure we close it ourselves.
        unsafe {
            let w = ffi::xmlNewTextWriter(out);
            if let Some(ptr) = NonNull::new(w) {
                Some(Self { ptr })
            } else {
                ffi::xmlOutputBufferClose(out);
                guard::resume();
                None
            }
        }
    }

    /// `xmlNewTextWriterFilename(uri, 0)`: libxml2 opens the target
    /// through the installed output factory (stream hooks or plain files).
    pub fn to_uri(uri: &[u8]) -> Option<Self> {
        ensure_init();
        let c = c_string(uri);
        // SAFETY: c is a valid C string; the writer is owned on success.
        let w = unsafe { ffi::xmlNewTextWriterFilename(c.as_ptr(), 0) };
        guard::resume();
        NonNull::new(w).map(|ptr| Self { ptr })
    }

    fn call(&self, f: impl FnOnce(*mut ffi::xmlTextWriter) -> c_int) -> i32 {
        let r = f(self.ptr.as_ptr());
        guard::resume();
        r
    }

    /// `xmlTextWriterSetIndent`.
    pub fn set_indent(&self, indent: bool) -> i32 {
        // SAFETY: the writer is live.
        self.call(|w| unsafe { ffi::xmlTextWriterSetIndent(w, c_int::from(indent)) })
    }

    /// `xmlTextWriterSetIndentString`.
    pub fn set_indent_string(&self, s: &[u8]) -> i32 {
        let c = c_string(s);
        // SAFETY: live writer, valid C string.
        self.call(|w| unsafe { ffi::xmlTextWriterSetIndentString(w, c.as_ptr().cast()) })
    }

    /// `xmlTextWriterStartDocument(version, encoding, standalone)`.
    pub fn start_document(
        &self,
        version: Option<&[u8]>,
        encoding: Option<&[u8]>,
        standalone: Option<&[u8]>,
    ) -> i32 {
        let (v, e, s) = (opt_c(version), opt_c(encoding), opt_c(standalone));
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            ffi::xmlTextWriterStartDocument(
                w,
                ptr_of(v.as_ref()).cast(),
                ptr_of(e.as_ref()).cast(),
                ptr_of(s.as_ref()).cast(),
            )
        })
    }

    /// One-string-argument operations.
    pub fn op1(&self, op: Op1, arg: &[u8]) -> i32 {
        let c = c_string(arg);
        let p = c.as_ptr().cast::<u8>();
        // SAFETY: live writer, valid C string.
        self.call(|w| unsafe {
            match op {
                Op1::StartElement => ffi::xmlTextWriterStartElement(w, p),
                Op1::StartAttribute => ffi::xmlTextWriterStartAttribute(w, p),
                Op1::StartPi => ffi::xmlTextWriterStartPI(w, p),
                Op1::WriteCData => ffi::xmlTextWriterWriteCDATA(w, p),
                Op1::WriteRaw => ffi::xmlTextWriterWriteRaw(w, p),
                Op1::WriteString => ffi::xmlTextWriterWriteString(w, p),
                Op1::WriteComment => ffi::xmlTextWriterWriteComment(w, p),
                Op1::StartDtdElement => ffi::xmlTextWriterStartDTDElement(w, p),
                Op1::StartDtdAttlist => ffi::xmlTextWriterStartDTDAttlist(w, p),
            }
        })
    }

    /// Two-string-argument operations.
    pub fn op2(&self, op: Op2, a: &[u8], b: &[u8]) -> i32 {
        let (ca, cb) = (c_string(a), c_string(b));
        let (pa, pb) = (ca.as_ptr().cast::<u8>(), cb.as_ptr().cast::<u8>());
        // SAFETY: live writer, valid C strings.
        self.call(|w| unsafe {
            match op {
                Op2::WriteAttribute => ffi::xmlTextWriterWriteAttribute(w, pa, pb),
                Op2::WriteElement => ffi::xmlTextWriterWriteElement(w, pa, pb),
                Op2::WritePi => ffi::xmlTextWriterWritePI(w, pa, pb),
                Op2::WriteDtdElement => ffi::xmlTextWriterWriteDTDElement(w, pa, pb),
                Op2::WriteDtdAttlist => ffi::xmlTextWriterWriteDTDAttlist(w, pa, pb),
            }
        })
    }

    /// Argument-less operations.
    pub fn op0(&self, op: Op0) -> i32 {
        // SAFETY: live writer.
        self.call(|w| unsafe {
            match op {
                Op0::EndDocument => ffi::xmlTextWriterEndDocument(w),
                Op0::EndElement => ffi::xmlTextWriterEndElement(w),
                Op0::FullEndElement => ffi::xmlTextWriterFullEndElement(w),
                Op0::EndAttribute => ffi::xmlTextWriterEndAttribute(w),
                Op0::EndPi => ffi::xmlTextWriterEndPI(w),
                Op0::StartCData => ffi::xmlTextWriterStartCDATA(w),
                Op0::EndCData => ffi::xmlTextWriterEndCDATA(w),
                Op0::StartComment => ffi::xmlTextWriterStartComment(w),
                Op0::EndComment => ffi::xmlTextWriterEndComment(w),
                Op0::EndDtd => ffi::xmlTextWriterEndDTD(w),
                Op0::EndDtdElement => ffi::xmlTextWriterEndDTDElement(w),
                Op0::EndDtdAttlist => ffi::xmlTextWriterEndDTDAttlist(w),
                Op0::EndDtdEntity => ffi::xmlTextWriterEndDTDEntity(w),
                Op0::Flush => ffi::xmlTextWriterFlush(w),
            }
        })
    }

    /// `xmlTextWriterStartElementNS` / `StartAttributeNS`.
    pub fn start_ns(
        &self,
        attribute: bool,
        prefix: Option<&[u8]>,
        name: &[u8],
        uri: Option<&[u8]>,
    ) -> i32 {
        let (p, n, u) = (opt_c(prefix), c_string(name), opt_c(uri));
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            let (pp, np, up) = (
                ptr_of(p.as_ref()),
                n.as_ptr().cast::<u8>(),
                ptr_of(u.as_ref()),
            );
            if attribute {
                ffi::xmlTextWriterStartAttributeNS(w, pp, np, up)
            } else {
                ffi::xmlTextWriterStartElementNS(w, pp, np, up)
            }
        })
    }

    /// `xmlTextWriterWriteElementNS` / `WriteAttributeNS`.
    pub fn write_ns(
        &self,
        attribute: bool,
        prefix: Option<&[u8]>,
        name: &[u8],
        uri: Option<&[u8]>,
        content: &[u8],
    ) -> i32 {
        let (p, n, u, c) = (opt_c(prefix), c_string(name), opt_c(uri), c_string(content));
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            let (pp, np, up, cp) = (
                ptr_of(p.as_ref()),
                n.as_ptr().cast::<u8>(),
                ptr_of(u.as_ref()),
                c.as_ptr().cast::<u8>(),
            );
            if attribute {
                ffi::xmlTextWriterWriteAttributeNS(w, pp, np, up, cp)
            } else {
                ffi::xmlTextWriterWriteElementNS(w, pp, np, up, cp)
            }
        })
    }

    /// `xmlTextWriterStartDTD`.
    pub fn start_dtd(&self, name: &[u8], pubid: Option<&[u8]>, sysid: Option<&[u8]>) -> i32 {
        let (n, p, s) = (c_string(name), opt_c(pubid), opt_c(sysid));
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            ffi::xmlTextWriterStartDTD(w, n.as_ptr().cast(), ptr_of(p.as_ref()), ptr_of(s.as_ref()))
        })
    }

    /// `xmlTextWriterWriteDTD`.
    pub fn write_dtd(
        &self,
        name: &[u8],
        pubid: Option<&[u8]>,
        sysid: Option<&[u8]>,
        subset: Option<&[u8]>,
    ) -> i32 {
        let (n, p, s, sub) = (c_string(name), opt_c(pubid), opt_c(sysid), opt_c(subset));
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            ffi::xmlTextWriterWriteDTD(
                w,
                n.as_ptr().cast(),
                ptr_of(p.as_ref()),
                ptr_of(s.as_ref()),
                ptr_of(sub.as_ref()),
            )
        })
    }

    /// `xmlTextWriterStartDTDEntity`.
    pub fn start_dtd_entity(&self, parameter: bool, name: &[u8]) -> i32 {
        let n = c_string(name);
        // SAFETY: live writer, valid C string.
        self.call(|w| unsafe {
            ffi::xmlTextWriterStartDTDEntity(w, c_int::from(parameter), n.as_ptr().cast())
        })
    }

    /// `xmlTextWriterWriteDTDEntity`.
    pub fn write_dtd_entity(&self, entity: &DtdEntity<'_>) -> i32 {
        let n = c_string(entity.name);
        let (p, s, nd) = (
            opt_c(entity.public_id),
            opt_c(entity.system_id),
            opt_c(entity.ndata),
        );
        let c = c_string(entity.content);
        // SAFETY: live writer, NULL or valid C strings.
        self.call(|w| unsafe {
            ffi::xmlTextWriterWriteDTDEntity(
                w,
                c_int::from(entity.parameter),
                n.as_ptr().cast(),
                ptr_of(p.as_ref()),
                ptr_of(s.as_ref()),
                ptr_of(nd.as_ref()),
                c.as_ptr().cast(),
            )
        })
    }
}

impl Drop for TextWriter {
    fn drop(&mut self) {
        // SAFETY: we own the writer; this also flushes and closes its
        // output buffer (dropping the Rust sink).
        unsafe { ffi::xmlFreeTextWriter(self.ptr.as_ptr()) };
        guard::resume();
    }
}

/// Arguments of `xmlTextWriterWriteDTDEntity`.
#[derive(Debug, Clone, Copy)]
pub struct DtdEntity<'a> {
    /// Parameter entity (`%name;`).
    pub parameter: bool,
    /// Entity name.
    pub name: &'a [u8],
    /// Public identifier.
    pub public_id: Option<&'a [u8]>,
    /// System identifier.
    pub system_id: Option<&'a [u8]>,
    /// NDATA notation.
    pub ndata: Option<&'a [u8]>,
    /// Replacement text.
    pub content: &'a [u8],
}

/// One-argument writer operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op1 {
    /// `xmlTextWriterStartElement`.
    StartElement,
    /// `xmlTextWriterStartAttribute`.
    StartAttribute,
    /// `xmlTextWriterStartPI`.
    StartPi,
    /// `xmlTextWriterWriteCDATA`.
    WriteCData,
    /// `xmlTextWriterWriteRaw`.
    WriteRaw,
    /// `xmlTextWriterWriteString`.
    WriteString,
    /// `xmlTextWriterWriteComment`.
    WriteComment,
    /// `xmlTextWriterStartDTDElement`.
    StartDtdElement,
    /// `xmlTextWriterStartDTDAttlist`.
    StartDtdAttlist,
}

/// Two-argument writer operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op2 {
    /// `xmlTextWriterWriteAttribute`.
    WriteAttribute,
    /// `xmlTextWriterWriteElement`.
    WriteElement,
    /// `xmlTextWriterWritePI`.
    WritePi,
    /// `xmlTextWriterWriteDTDElement`.
    WriteDtdElement,
    /// `xmlTextWriterWriteDTDAttlist`.
    WriteDtdAttlist,
}

/// Argument-less writer operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op0 {
    /// `xmlTextWriterEndDocument`.
    EndDocument,
    /// `xmlTextWriterEndElement`.
    EndElement,
    /// `xmlTextWriterFullEndElement`.
    FullEndElement,
    /// `xmlTextWriterEndAttribute`.
    EndAttribute,
    /// `xmlTextWriterEndPI`.
    EndPi,
    /// `xmlTextWriterStartCDATA`.
    StartCData,
    /// `xmlTextWriterEndCDATA`.
    EndCData,
    /// `xmlTextWriterStartComment`.
    StartComment,
    /// `xmlTextWriterEndComment`.
    EndComment,
    /// `xmlTextWriterEndDTD`.
    EndDtd,
    /// `xmlTextWriterEndDTDElement`.
    EndDtdElement,
    /// `xmlTextWriterEndDTDAttlist`.
    EndDtdAttlist,
    /// `xmlTextWriterEndDTDEntity`.
    EndDtdEntity,
    /// `xmlTextWriterFlush`.
    Flush,
}
