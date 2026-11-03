//! Documents handed to and returned by the processor.
//!
//! PHP accepts `DOMDocument`, `Dom\Document`, `SimpleXMLElement` and any
//! DOM node (whose owner document is used). The processor only needs the
//! underlying libxml2 document, so inputs are anything implementing
//! [`XmlDocument`]: `php-libxml` [`Document`]/[`Node`] handles (what
//! SimpleXML uses) implement it directly, and [`RawDocument`] wraps the
//! document pointer of any other object model (e.g. `php-dom`'s).

use std::ptr::NonNull;

use php_libxml::tree::{Document, Node};

use crate::ffi;

/// A libxml2 document the processor can read.
///
/// # Safety
/// Implementors guarantee that [`XmlDocument::xml_doc`] returns a pointer
/// to a live `xmlDoc` that is not freed while `self` is borrowed (the
/// processor reads it, may copy it, and during a transformation may hand
/// its nodes to the host).
pub unsafe trait XmlDocument {
    /// The underlying `xmlDoc`.
    fn xml_doc(&self) -> NonNull<ffi::xmlDoc>;

    /// Whether the document belongs to the spec-compliant `Dom\` API
    /// (`PHP_LIBXML_CLASS_MODERN`): `xmlns` attributes are then turned back
    /// into namespace declarations before a stylesheet is compiled, and
    /// callback return values are checked against `Dom\Node`.
    fn is_modern(&self) -> bool {
        false
    }
}

// SAFETY: a `Document` keeps its xmlDoc alive for as long as it exists.
unsafe impl XmlDocument for Document {
    fn xml_doc(&self) -> NonNull<ffi::xmlDoc> {
        NonNull::new(self.as_ptr()).expect("a Document always has a document pointer")
    }
}

// SAFETY: a `Node` keeps its document alive for as long as it exists.
unsafe impl XmlDocument for Node {
    fn xml_doc(&self) -> NonNull<ffi::xmlDoc> {
        NonNull::new(self.document().as_ptr()).expect("a Node always has a document")
    }
}

/// A borrowed document of another object model.
#[derive(Debug, Clone, Copy)]
pub struct RawDocument {
    ptr: NonNull<ffi::xmlDoc>,
    modern: bool,
}

impl RawDocument {
    /// Wraps a document pointer.
    ///
    /// # Safety
    /// `ptr` must point to a live `xmlDoc` that stays alive (and is not
    /// freed by anyone else) for as long as the returned value is used.
    pub unsafe fn new(ptr: NonNull<ffi::xmlDoc>, modern: bool) -> Self {
        Self { ptr, modern }
    }
}

// SAFETY: guaranteed by the contract of `RawDocument::new`.
unsafe impl XmlDocument for RawDocument {
    fn xml_doc(&self) -> NonNull<ffi::xmlDoc> {
        self.ptr
    }

    fn is_modern(&self) -> bool {
        self.modern
    }
}

/// The result tree of `transformToDoc()`, owned until handed over.
#[derive(Debug)]
pub struct ResultDocument {
    ptr: NonNull<ffi::xmlDoc>,
}

impl ResultDocument {
    /// Takes ownership of a document produced by libxslt.
    ///
    /// # Safety
    /// `ptr` must be a fresh document nobody else references or frees.
    pub(crate) unsafe fn from_raw(ptr: NonNull<ffi::xmlDoc>) -> Self {
        Self { ptr }
    }

    /// The document pointer (valid while `self` lives).
    pub fn as_ptr(&self) -> *mut ffi::xmlDoc {
        self.ptr.as_ptr()
    }

    /// Releases ownership: the caller must eventually free the document
    /// with `xmlFreeDoc` (typically by adopting it into its DOM objects).
    pub fn into_raw(self) -> NonNull<ffi::xmlDoc> {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }

    /// Converts into a reference-counted `php-libxml` document (what a
    /// `SimpleXMLElement` `$returnClass` wraps).
    pub fn into_document(self) -> Document {
        let ptr = self.into_raw();
        // SAFETY: the result tree is exclusively owned and no node of it is
        // referenced anywhere else.
        unsafe { Document::from_raw(ptr) }
    }
}

impl Drop for ResultDocument {
    fn drop(&mut self) {
        // SAFETY: we own the document and free it exactly once.
        unsafe { ffi::xmlFreeDoc(self.ptr.as_ptr()) };
    }
}
