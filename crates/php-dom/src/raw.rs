// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Raw access to libxml2 trees.
//!
//! # Safety model
//!
//! This crate ports `ext/dom` almost line by line, so most algorithms walk
//! and rewire `xmlNode` pointers exactly like the C code. To keep that code
//! readable, tree pointers are wrapped in [`N`], a `Copy` handle whose
//! accessor methods dereference the pointer.
//!
//! The invariant that makes those accessors sound is the one PHP relies on:
//! an `N` is only ever created (crate-internally) from a pointer to a live
//! node, and it is only used while the tree that contains the node is kept
//! alive by a [`crate::object::Node`] or [`crate::object::DocRef`] handle
//! held by the caller of the public API, and no other thread can access the
//! tree (all handle types are `!Send`). Nodes are only freed through
//! [`free_list`] / [`node_free`] / [`free_resource`], which never free a
//! node that has a live handle (its `_private` field is non-null), so an
//! `N` obtained from a handle stays valid for the duration of a call.
//!
//! `N` is crate-private and never exposed to users.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr::{self, NonNull};

use crate::ffi::{self, xmlAttr, xmlDoc, xmlDtd, xmlNode, xmlNs};

/// A pointer to a live libxml2 node (see the module documentation).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct N(NonNull<xmlNode>);

impl std::fmt::Debug for N {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "N({:p}, type {})", self.0, self.t())
    }
}

/// A pointer to a live namespace record.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Ns(NonNull<xmlNs>);

macro_rules! field {
    ($self:ident . $f:ident) => {
        // SAFETY: `self` points to a live node (module invariant).
        unsafe { (*$self.0.as_ptr()).$f }
    };
}

impl N {
    /// Wraps a possibly-NULL pointer.
    #[inline]
    pub fn new(p: *mut xmlNode) -> Option<Self> {
        NonNull::new(p).map(N)
    }
    #[inline]
    pub fn from_attr(p: *mut xmlAttr) -> Option<Self> {
        Self::new(p.cast())
    }
    #[inline]
    pub fn from_doc(p: *mut xmlDoc) -> Option<Self> {
        Self::new(p.cast())
    }
    #[inline]
    pub fn from_dtd(p: *mut xmlDtd) -> Option<Self> {
        Self::new(p.cast())
    }
    #[inline]
    pub fn p(self) -> *mut xmlNode {
        self.0.as_ptr()
    }
    #[inline]
    pub fn attr(self) -> *mut xmlAttr {
        self.0.as_ptr().cast()
    }
    #[inline]
    pub fn as_doc(self) -> *mut xmlDoc {
        self.0.as_ptr().cast()
    }
    #[inline]
    pub fn as_dtd(self) -> *mut xmlDtd {
        self.0.as_ptr().cast()
    }
    #[inline]
    pub fn as_entity(self) -> *mut ffi::xmlEntity {
        self.0.as_ptr().cast()
    }
    #[inline]
    pub fn t(self) -> c_int {
        field!(self.type_)
    }
    #[inline]
    pub fn is_element(self) -> bool {
        self.t() == ffi::XML_ELEMENT_NODE
    }
    #[inline]
    pub fn is_doc(self) -> bool {
        matches!(
            self.t(),
            ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE
        )
    }
    #[inline]
    pub fn is_text_like(self) -> bool {
        matches!(self.t(), ffi::XML_TEXT_NODE | ffi::XML_CDATA_SECTION_NODE)
    }
    #[inline]
    pub fn name_ptr(self) -> *const u8 {
        field!(self.name)
    }
    /// The node name (empty when NULL).
    pub fn name(self) -> &'static [u8] {
        cstr(self.name_ptr())
    }
    #[inline]
    pub fn children(self) -> Option<N> {
        N::new(field!(self.children))
    }
    #[inline]
    pub fn last(self) -> Option<N> {
        N::new(field!(self.last))
    }
    #[inline]
    pub fn parent(self) -> Option<N> {
        N::new(field!(self.parent))
    }
    #[inline]
    pub fn next(self) -> Option<N> {
        N::new(field!(self.next))
    }
    #[inline]
    pub fn prev(self) -> Option<N> {
        N::new(field!(self.prev))
    }
    #[inline]
    pub fn doc_ptr(self) -> *mut xmlDoc {
        field!(self.doc)
    }
    #[inline]
    pub fn doc(self) -> Option<N> {
        N::from_doc(self.doc_ptr())
    }
    #[inline]
    pub fn ns(self) -> Option<Ns> {
        Ns::new(field!(self.ns))
    }
    #[inline]
    pub fn ns_ptr(self) -> *mut xmlNs {
        field!(self.ns)
    }
    #[inline]
    pub fn ns_def(self) -> Option<Ns> {
        Ns::new(field!(self.nsDef))
    }
    #[inline]
    pub fn content_ptr(self) -> *mut u8 {
        field!(self.content)
    }
    /// `content` as bytes (empty when NULL).
    pub fn content(self) -> &'static [u8] {
        cstr(self.content_ptr())
    }
    #[inline]
    pub fn properties(self) -> Option<N> {
        N::from_attr(field!(self.properties))
    }
    #[inline]
    pub fn private(self) -> *mut c_void {
        field!(self._private)
    }

    // ---- setters (raw link manipulation, mirrors the C code) ----
    #[inline]
    pub fn set_private(self, p: *mut c_void) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p())._private = p }
    }
    #[inline]
    pub fn set_parent(self, v: Option<N>) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).parent = raw(v) }
    }
    #[inline]
    pub fn set_next(self, v: Option<N>) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).next = raw(v) }
    }
    #[inline]
    pub fn set_prev(self, v: Option<N>) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).prev = raw(v) }
    }
    #[inline]
    pub fn set_children(self, v: Option<N>) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).children = raw(v) }
    }
    #[inline]
    pub fn set_last(self, v: Option<N>) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).last = raw(v) }
    }
    #[inline]
    pub fn set_ns(self, ns: *mut xmlNs) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).ns = ns }
    }
    #[inline]
    pub fn set_ns_def(self, ns: *mut xmlNs) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).nsDef = ns }
    }
    #[inline]
    pub fn set_properties(self, v: Option<N>) {
        // SAFETY: live node (module invariant); only called on elements.
        unsafe { (*self.p()).properties = v.map_or(ptr::null_mut(), N::attr) }
    }
    #[inline]
    pub fn set_type(self, t: c_int) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).type_ = t }
    }
    #[inline]
    pub fn set_doc_raw(self, d: *mut xmlDoc) {
        // SAFETY: live node (module invariant).
        unsafe { (*self.p()).doc = d }
    }

    // ---- attribute / dtd / doc / entity specific fields ----
    /// `xmlAttr.atype`.
    pub fn atype(self) -> c_int {
        // SAFETY: only called on attribute nodes (layout `xmlAttr`).
        unsafe { (*self.attr()).atype }
    }
    pub fn set_atype(self, t: c_int) {
        // SAFETY: only called on attribute nodes.
        unsafe { (*self.attr()).atype = t }
    }
    /// `xmlDoc` field access.
    pub fn d(self) -> &'static mut xmlDoc {
        // SAFETY: only called on document nodes (layout `xmlDoc`); the
        // reference does not outlive the current call (module invariant).
        unsafe { &mut *self.as_doc() }
    }
    /// `xmlDtd` field access.
    pub fn dtd(self) -> &'static mut xmlDtd {
        // SAFETY: only called on DTD nodes.
        unsafe { &mut *self.as_dtd() }
    }
    /// `xmlEntity` field access.
    pub fn ent(self) -> &'static mut ffi::xmlEntity {
        // SAFETY: only called on entity declarations (and notation fakes,
        // which are allocated with the entity layout).
        unsafe { &mut *self.as_entity() }
    }
    /// Iterates the child list.
    pub fn child_iter(self) -> Siblings {
        Siblings(self.children())
    }
    /// Iterates the attribute list of an element.
    pub fn attr_iter(self) -> Siblings {
        if self.is_element() {
            Siblings(self.properties())
        } else {
            Siblings(None)
        }
    }
}

/// Iterator over a sibling chain.
#[derive(Debug)]
pub(crate) struct Siblings(Option<N>);

impl Iterator for Siblings {
    type Item = N;
    fn next(&mut self) -> Option<N> {
        let cur = self.0?;
        self.0 = cur.next();
        Some(cur)
    }
}

impl Ns {
    #[inline]
    pub fn new(p: *mut xmlNs) -> Option<Self> {
        NonNull::new(p).map(Ns)
    }
    #[inline]
    pub fn p(self) -> *mut xmlNs {
        self.0.as_ptr()
    }
    #[inline]
    pub fn href_ptr(self) -> *const u8 {
        // SAFETY: live namespace record (module invariant).
        unsafe { (*self.p()).href }
    }
    #[inline]
    pub fn prefix_ptr(self) -> *const u8 {
        // SAFETY: live namespace record.
        unsafe { (*self.p()).prefix }
    }
    pub fn href(self) -> &'static [u8] {
        cstr(self.href_ptr())
    }
    pub fn prefix(self) -> Option<&'static [u8]> {
        opt_cstr(self.prefix_ptr())
    }
    pub fn next(self) -> Option<Ns> {
        // SAFETY: live namespace record.
        Ns::new(unsafe { (*self.p()).next })
    }
    pub fn set_next(self, n: *mut xmlNs) {
        // SAFETY: live namespace record.
        unsafe { (*self.p()).next = n }
    }
    pub fn private(self) -> *mut c_void {
        // SAFETY: live namespace record.
        unsafe { (*self.p())._private }
    }
    pub fn set_private(self, p: *mut c_void) {
        // SAFETY: live namespace record.
        unsafe { (*self.p())._private = p }
    }
}

#[inline]
pub(crate) fn raw(v: Option<N>) -> *mut xmlNode {
    v.map_or(ptr::null_mut(), N::p)
}

/// Borrows a NUL-terminated libxml2 string (empty for NULL).
///
/// The `'static` lifetime is a lie bounded by the module invariant: callers
/// copy or drop the slice before the tree can change.
pub(crate) fn cstr(p: *const u8) -> &'static [u8] {
    opt_cstr(p).unwrap_or_default()
}

/// Borrows a possibly-NULL NUL-terminated string.
pub(crate) fn opt_cstr(p: *const u8) -> Option<&'static [u8]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: libxml2 strings reachable from live nodes are valid C
        // strings (module invariant).
        Some(unsafe { CStr::from_ptr(p.cast::<c_char>()) }.to_bytes())
    }
}

/// A NUL-terminated copy of PHP bytes (truncated at an interior NUL, as
/// C code receiving a `char *` would see it).
pub(crate) fn cs(bytes: &[u8]) -> std::ffi::CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::ffi::CString::new(&bytes[..end]).unwrap_or_default()
}

/// Copies and frees a libxml2-allocated string.
pub(crate) fn take(p: *mut u8) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    let v = cstr(p).to_vec();
    xml_free(p.cast());
    Some(v)
}

/// `xmlFree`.
pub(crate) fn xml_free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    // SAFETY: xmlFree is libxml2's deallocator; `p` was allocated by it.
    unsafe {
        if let Some(f) = ffi::xmlFree {
            f(p);
        }
    }
}

/// `xmlStrdup` of PHP bytes (truncated at NUL).
pub(crate) fn xml_strdup(bytes: &[u8]) -> *mut u8 {
    let c = cs(bytes);
    // SAFETY: c is a valid C string.
    unsafe { ffi::xmlStrdup(c.as_ptr().cast()) }
}

/// `xmlStrEqual` on optional byte strings (NULL == NULL).
pub(crate) fn str_eq(a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Converts a length to C `int`, saturating.
pub(crate) fn c_len(len: usize) -> c_int {
    c_int::try_from(len).unwrap_or(c_int::MAX)
}

/// The pointer to hand libxml2 with `c_len(bytes.len())`: an empty
/// slice's pointer dangles, and libxml2 (2.15 at least, in
/// `xmlNodeSetContentLen()`) looks at the first byte whatever the length,
/// so the empty string is passed as a terminator.
pub(crate) fn c_bytes(bytes: &[u8]) -> *const u8 {
    if bytes.is_empty() {
        c"".as_ptr().cast()
    } else {
        bytes.as_ptr()
    }
}

impl From<Option<N>> for Siblings {
    fn from(start: Option<N>) -> Self {
        Siblings(start)
    }
}
