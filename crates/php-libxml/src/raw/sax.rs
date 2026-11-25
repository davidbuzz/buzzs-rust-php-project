// Derived from php-src ext/xml/compat.c. Similarity metric to original: 26%
//   Copyright © The PHP Group and Contributors. Authors: Sterling Hughes <sterling@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! Port of `ext/xml/compat.c`: an expat-like push parser on top of
//! libxml2's SAX interface.
//!
//! [`PushParser`] owns the `xmlParserCtxt`; the SAX trampolines forward to
//! an [`ExpatSink`] supplied for the duration of each [`PushParser::parse`]
//! call. The "installed" handler set mirrors the C function pointers
//! (`h_start_element`, …) of `struct XML_Parser_Struct`: an installed
//! handler suppresses the default-handler fallback even when the user
//! callable behind it is unset.

use std::cell::Cell;
use std::ffi::{c_int, c_void};
use std::ptr::NonNull;

use super::errors::{CtxtRegistration, ParserChannels};
use super::{ensure_init, guard, opt_slice};
use crate::ffi;

/// `XML_ERROR_EXTERNAL_ENTITY_HANDLING`.
pub const XML_ERROR_EXTERNAL_ENTITY_HANDLING: c_int = 21;

/// Handler slots of the expat compatibility layer (bit flags).
pub mod slot {
    /// `h_start_element`.
    pub const START_ELEMENT: u16 = 1 << 0;
    /// `h_end_element`.
    pub const END_ELEMENT: u16 = 1 << 1;
    /// `h_cdata`.
    pub const CHARACTER_DATA: u16 = 1 << 2;
    /// `h_pi`.
    pub const PROCESSING_INSTRUCTION: u16 = 1 << 3;
    /// `h_default`.
    pub const DEFAULT: u16 = 1 << 4;
    /// `h_unparsed_entity_decl`.
    pub const UNPARSED_ENTITY_DECL: u16 = 1 << 5;
    /// `h_notation_decl`.
    pub const NOTATION_DECL: u16 = 1 << 6;
    /// `h_external_entity_ref`.
    pub const EXTERNAL_ENTITY_REF: u16 = 1 << 7;
    /// `h_start_ns`.
    pub const START_NAMESPACE_DECL: u16 = 1 << 8;
    /// `h_end_ns` (never invoked by the libxml2 compat layer).
    pub const END_NAMESPACE_DECL: u16 = 1 << 9;
}

/// Receiver of expat-style events (the `h_*` callbacks).
///
/// Only events whose slot is installed are delivered, except
/// [`ExpatSink::default_data`] which also receives the fallback text of
/// uninstalled slots when the default slot is installed.
pub trait ExpatSink {
    /// `h_start_element(name, attrs)`; attributes are `(name, value)`
    /// pairs, namespace-qualified in namespace mode.
    fn start_element(&mut self, name: &[u8], attrs: &[(Vec<u8>, Vec<u8>)]);
    /// `h_end_element(name)`.
    fn end_element(&mut self, name: &[u8]);
    /// `h_cdata(data)`.
    fn character_data(&mut self, data: &[u8]);
    /// `h_pi(target, data)`.
    fn processing_instruction(&mut self, target: Option<&[u8]>, data: Option<&[u8]>);
    /// `h_default(data)`.
    fn default_data(&mut self, data: &[u8]);
    /// `h_unparsed_entity_decl(name, base, system_id, public_id, notation)`.
    fn unparsed_entity_decl(
        &mut self,
        name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
        notation: Option<&[u8]>,
    );
    /// `h_notation_decl(name, base, system_id, public_id)`.
    fn notation_decl(
        &mut self,
        name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    );
    /// `h_external_entity_ref(names, base, system_id, public_id)`; a zero
    /// return stops the parser with `XML_ERROR_EXTERNAL_ENTITY_HANDLING`.
    fn external_entity_ref(
        &mut self,
        open_entity_names: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    ) -> i64;
    /// `h_start_ns(prefix, uri)`.
    fn start_namespace_decl(&mut self, prefix: Option<&[u8]>, uri: Option<&[u8]>);
    /// Whether event processing stopped (`EG(exception)` or `ZEND_HAS_OOM()`):
    /// checked between the attributes of a start tag, like `compat.c`'s
    /// `_start_element_handler_ns`, so a huge tag cannot run into the hard
    /// memory limit.
    fn aborted(&self) -> bool {
        false
    }
}

/// State reachable from the SAX `userData` pointer.
struct CompatState {
    ctxt: Cell<*mut ffi::xmlParserCtxt>,
    ns_separator: Option<Vec<u8>>,
    installed: Cell<u16>,
    /// Pointer to the `&mut dyn ExpatSink` living on the stack of the
    /// active `parse` call; NULL outside of parsing.
    sink: Cell<*mut c_void>,
}

impl CompatState {
    fn has(&self, slot: u16) -> bool {
        self.installed.get() & slot != 0
    }

    fn qualify(&self, name: &[u8], uri: Option<&[u8]>) -> Vec<u8> {
        match uri {
            Some(uri) => {
                let sep = self.ns_separator.as_deref().unwrap_or_default();
                let mut out = Vec::with_capacity(uri.len() + sep.len() + name.len());
                out.extend_from_slice(uri);
                out.extend_from_slice(sep);
                out.extend_from_slice(name);
                out
            }
            None => name.to_vec(),
        }
    }
}

/// Runs `f` with the active sink of the parser behind `user`.
///
/// # Safety
/// `user` must be the `CompatState` pointer registered as SAX user data.
unsafe fn with_sink(user: *mut c_void, f: impl FnOnce(&CompatState, &mut dyn ExpatSink)) {
    if user.is_null() || guard::pending() {
        return;
    }
    // SAFETY: user is the boxed CompatState owned by the PushParser, which
    // outlives its parser context.
    let state = unsafe { &*user.cast::<CompatState>() };
    let sink_ptr = state.sink.get();
    if sink_ptr.is_null() {
        return;
    }
    // SAFETY: sink_ptr points at a `&mut dyn ExpatSink` on the stack frame
    // of the `parse` call currently running xmlParseChunk; it is only
    // reborrowed here, one callback at a time.
    #[allow(clippy::mut_mut)]
    let sink: &mut &mut dyn ExpatSink = unsafe { &mut *sink_ptr.cast::<&mut dyn ExpatSink>() };
    let stopped = guard::catch(true, || {
        f(state, &mut **sink);
        false
    });
    if stopped {
        let ctxt = state.ctxt.get();
        if !ctxt.is_null() {
            // SAFETY: the context is live while parsing.
            unsafe { ffi::xmlStopParser(ctxt) };
        }
    }
}

/// `start_element_emit_default`.
fn emit_default_start(state: &CompatState, sink: &mut dyn ExpatSink) {
    if !state.has(slot::DEFAULT) {
        return;
    }
    let ctxt = state.ctxt.get();
    // SAFETY: called from a SAX callback, so ctxt and its current input are
    // live and `base <= cur < end` delimits the buffered document.
    let bytes = unsafe {
        let input = (*ctxt).input;
        if input.is_null() || (*input).cur.is_null() || (*input).base.is_null() {
            return;
        }
        let base = (*input).base;
        let end = (*input).cur;
        let mut cur = end;
        while cur > base && *cur != b'<' {
            cur = cur.sub(1);
        }
        let len = usize::try_from(end.offset_from(cur)).unwrap_or(0) + 1;
        let mut bytes = std::slice::from_raw_parts(cur, len).to_vec();
        if *end == b'/' {
            if let Some(last) = bytes.last_mut() {
                *last = b'>';
            }
        }
        bytes
    };
    sink.default_data(&bytes);
}

unsafe extern "C" fn start_element_handler(
    user: *mut c_void,
    name: *const u8,
    attributes: *mut *const u8,
) {
    // SAFETY: SAX contract: name is a C string; attributes is NULL or a
    // NULL-terminated array of name/value C strings.
    unsafe {
        with_sink(user, |state, sink| {
            if !state.has(slot::START_ELEMENT) {
                emit_default_start(state, sink);
                return;
            }
            let mut attrs = Vec::new();
            if !attributes.is_null() {
                let mut p = attributes;
                while !(*p).is_null() {
                    let k = opt_slice(*p).unwrap_or_default().to_vec();
                    let v = opt_slice(*p.add(1)).unwrap_or_default().to_vec();
                    attrs.push((k, v));
                    if sink.aborted() {
                        return;
                    }
                    p = p.add(2);
                }
            }
            sink.start_element(opt_slice(name).unwrap_or_default(), &attrs);
        });
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn start_element_handler_ns(
    user: *mut c_void,
    name: *const u8,
    _prefix: *const u8,
    uri: *const u8,
    nb_namespaces: c_int,
    namespaces: *mut *const u8,
    nb_attributes: c_int,
    _nb_defaulted: c_int,
    attributes: *mut *const u8,
) {
    // SAFETY: SAX2 contract: namespaces holds 2*nb_namespaces entries,
    // attributes holds 5*nb_attributes entries (localname, prefix, URI,
    // value start, value end).
    unsafe {
        with_sink(user, |state, sink| {
            if nb_namespaces > 0 && state.has(slot::START_NAMESPACE_DECL) && !namespaces.is_null() {
                for i in 0..usize::try_from(nb_namespaces).unwrap_or(0) {
                    sink.start_namespace_decl(
                        opt_slice(*namespaces.add(2 * i)),
                        opt_slice(*namespaces.add(2 * i + 1)),
                    );
                }
            }
            if !state.has(slot::START_ELEMENT) {
                emit_default_start(state, sink);
                return;
            }
            let qualified = state.qualify(opt_slice(name).unwrap_or_default(), opt_slice(uri));
            let mut attrs = Vec::new();
            if !attributes.is_null() {
                for i in 0..usize::try_from(nb_attributes).unwrap_or(0) {
                    let a = attributes.add(5 * i);
                    let local = opt_slice(*a).unwrap_or_default();
                    let key = if (*a.add(1)).is_null() {
                        local.to_vec()
                    } else {
                        state.qualify(local, opt_slice(*a.add(2)))
                    };
                    let start = *a.add(3);
                    let end = *a.add(4);
                    let value = if start.is_null() || end.is_null() {
                        Vec::new()
                    } else {
                        let len = usize::try_from(end.offset_from(start)).unwrap_or(0);
                        std::slice::from_raw_parts(start, len).to_vec()
                    };
                    attrs.push((key, value));
                    if sink.aborted() {
                        return;
                    }
                }
            }
            sink.start_element(&qualified, &attrs);
        });
    }
}

unsafe extern "C" fn end_element_handler(user: *mut c_void, name: *const u8) {
    // SAFETY: name is a C string per the SAX contract.
    unsafe {
        with_sink(user, |state, sink| {
            let name = opt_slice(name).unwrap_or_default();
            if !state.has(slot::END_ELEMENT) {
                if state.has(slot::DEFAULT) {
                    let mut tag = b"</".to_vec();
                    tag.extend_from_slice(name);
                    tag.push(b'>');
                    sink.default_data(&tag);
                }
                return;
            }
            sink.end_element(name);
        });
    }
}

unsafe extern "C" fn end_element_handler_ns(
    user: *mut c_void,
    name: *const u8,
    prefix: *const u8,
    uri: *const u8,
) {
    // SAFETY: name/prefix/uri are NULL or C strings per the SAX2 contract.
    unsafe {
        with_sink(user, |state, sink| {
            let name = opt_slice(name).unwrap_or_default();
            if !state.has(slot::END_ELEMENT) {
                if state.has(slot::DEFAULT) {
                    let mut tag = b"</".to_vec();
                    if let Some(prefix) = opt_slice(prefix) {
                        tag.extend_from_slice(prefix);
                        tag.push(b':');
                    }
                    tag.extend_from_slice(name);
                    tag.push(b'>');
                    sink.default_data(&tag);
                }
                return;
            }
            let qualified = state.qualify(name, opt_slice(uri));
            sink.end_element(&qualified);
        });
    }
}

unsafe extern "C" fn cdata_handler(user: *mut c_void, data: *const u8, len: c_int) {
    // SAFETY: data is valid for len bytes per the SAX contract.
    unsafe {
        with_sink(user, |state, sink| {
            let data = if data.is_null() || len <= 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(data, usize::try_from(len).unwrap_or(0))
            };
            if !state.has(slot::CHARACTER_DATA) {
                if state.has(slot::DEFAULT) {
                    sink.default_data(data);
                }
                return;
            }
            sink.character_data(data);
        });
    }
}

unsafe extern "C" fn pi_handler(user: *mut c_void, target: *const u8, data: *const u8) {
    // SAFETY: target/data are NULL or C strings.
    unsafe {
        with_sink(user, |state, sink| {
            let target = opt_slice(target);
            let data = opt_slice(data);
            if !state.has(slot::PROCESSING_INSTRUCTION) {
                if state.has(slot::DEFAULT) {
                    // spprintf("<?%s %s?>") prints "(null)" for NULL.
                    let mut full = b"<?".to_vec();
                    full.extend_from_slice(target.unwrap_or(b"(null)"));
                    full.push(b' ');
                    full.extend_from_slice(data.unwrap_or(b"(null)"));
                    full.extend_from_slice(b"?>");
                    sink.default_data(&full);
                }
                return;
            }
            sink.processing_instruction(target, data);
        });
    }
}

unsafe extern "C" fn unparsed_entity_decl_handler(
    user: *mut c_void,
    name: *const u8,
    pub_id: *const u8,
    sys_id: *const u8,
    notation: *const u8,
) {
    // SAFETY: all arguments are NULL or C strings.
    unsafe {
        with_sink(user, |state, sink| {
            if state.has(slot::UNPARSED_ENTITY_DECL) {
                sink.unparsed_entity_decl(
                    opt_slice(name),
                    None,
                    opt_slice(sys_id),
                    opt_slice(pub_id),
                    opt_slice(notation),
                );
            }
        });
    }
}

unsafe extern "C" fn notation_decl_handler(
    user: *mut c_void,
    notation: *const u8,
    pub_id: *const u8,
    sys_id: *const u8,
) {
    // SAFETY: all arguments are NULL or C strings.
    unsafe {
        with_sink(user, |state, sink| {
            if state.has(slot::NOTATION_DECL) {
                sink.notation_decl(
                    opt_slice(notation),
                    None,
                    opt_slice(sys_id),
                    opt_slice(pub_id),
                );
            }
        });
    }
}

unsafe extern "C" fn comment_handler(user: *mut c_void, comment: *const u8) {
    // SAFETY: comment is a C string.
    unsafe {
        with_sink(user, |state, sink| {
            if state.has(slot::DEFAULT) {
                let mut full = b"<!--".to_vec();
                full.extend_from_slice(opt_slice(comment).unwrap_or_default());
                full.extend_from_slice(b"-->");
                sink.default_data(&full);
            }
        });
    }
}

/// `external_entity_ref_handler`.
fn external_entity_ref(
    state: &CompatState,
    sink: &mut dyn ExpatSink,
    names: Option<&[u8]>,
    sys_id: Option<&[u8]>,
    pub_id: Option<&[u8]>,
) {
    if !state.has(slot::EXTERNAL_ENTITY_REF) {
        return;
    }
    if sink.external_entity_ref(names, Some(b""), sys_id, pub_id) == 0 {
        let ctxt = state.ctxt.get();
        // SAFETY: called while parsing, the context is live.
        unsafe {
            ffi::xmlStopParser(ctxt);
            (*ctxt).errNo = XML_ERROR_EXTERNAL_ENTITY_HANDLING;
        }
    }
}

fn build_entity(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'&');
    out.extend_from_slice(name);
    out.push(b';');
    out
}

unsafe extern "C" fn get_entity(user: *mut c_void, name: *const u8) -> *mut ffi::xmlEntity {
    if user.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: user is the CompatState of a live parser.
    let state = unsafe { &*user.cast::<CompatState>() };
    let ctxt = state.ctxt.get();
    if ctxt.is_null() {
        return std::ptr::null_mut();
    }
    let mut ret: *mut ffi::xmlEntity = std::ptr::null_mut();
    // SAFETY: ctxt is live while parsing; name is a C string; returned
    // entities are owned by libxml2 (predefined table or the document).
    unsafe {
        if (*ctxt).inSubset != 0 {
            return ret;
        }
        ret = ffi::xmlGetPredefinedEntity(name);
        if ret.is_null() {
            ret = ffi::xmlGetDocEntity((*ctxt).myDoc, name);
        }
        if ret.is_null() || (*ctxt).instate == ffi::XML_PARSER_CONTENT {
            let etype = if ret.is_null() { 0 } else { (*ret).etype };
            let entity = ret;
            let name_bytes = opt_slice(name).unwrap_or_default();
            with_sink(user, |state, sink| {
                let internal = entity.is_null()
                    || matches!(
                        etype,
                        ffi::XML_INTERNAL_GENERAL_ENTITY
                            | ffi::XML_INTERNAL_PARAMETER_ENTITY
                            | ffi::XML_INTERNAL_PREDEFINED_ENTITY
                    );
                if internal {
                    let predefined =
                        !entity.is_null() && etype == ffi::XML_INTERNAL_PREDEFINED_ENTITY;
                    if state.has(slot::DEFAULT) && !(predefined && state.has(slot::CHARACTER_DATA))
                    {
                        sink.default_data(&build_entity(name_bytes));
                    } else if state.has(slot::CHARACTER_DATA) && !entity.is_null() {
                        let content = opt_slice((*entity).content).unwrap_or_default();
                        sink.character_data(content);
                    }
                } else if etype == ffi::XML_EXTERNAL_GENERAL_PARSED_ENTITY {
                    external_entity_ref(
                        state,
                        sink,
                        opt_slice((*entity).name),
                        opt_slice((*entity).SystemID),
                        opt_slice((*entity).ExternalID),
                    );
                }
            });
        }
    }
    ret
}

fn compat_handlers() -> ffi::xmlSAXHandler {
    ffi::xmlSAXHandler {
        internalSubset: None,
        isStandalone: None,
        hasInternalSubset: None,
        hasExternalSubset: None,
        resolveEntity: None,
        getEntity: Some(get_entity),
        entityDecl: None,
        notationDecl: Some(notation_decl_handler),
        attributeDecl: None,
        elementDecl: None,
        unparsedEntityDecl: Some(unparsed_entity_decl_handler),
        setDocumentLocator: None,
        startDocument: None,
        endDocument: None,
        startElement: Some(start_element_handler),
        endElement: Some(end_element_handler),
        reference: None,
        characters: Some(cdata_handler),
        ignorableWhitespace: None,
        processingInstruction: Some(pi_handler),
        comment: Some(comment_handler),
        warning: None,
        error: None,
        fatalError: None,
        getParameterEntity: None,
        cdataBlock: Some(cdata_handler),
        externalSubset: None,
        initialized: ffi::XML_SAX2_MAGIC,
        _private: std::ptr::null_mut(),
        startElementNs: Some(start_element_handler_ns),
        endElementNs: Some(end_element_handler_ns),
        serror: None,
    }
}

/// Error returned by [`PushParser::parse`] when called re-entrantly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reentrant;

/// An expat-compatible libxml2 push parser (`XML_Parser`).
pub struct PushParser {
    ctxt: NonNull<ffi::xmlParserCtxt>,
    state: Box<CompatState>,
    _registration: CtxtRegistration,
}

impl std::fmt::Debug for PushParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushParser")
            .field("namespaces", &self.state.ns_separator.is_some())
            .field("installed", &self.state.installed.get())
            .finish_non_exhaustive()
    }
}

impl PushParser {
    /// `XML_ParserCreate_MM(NULL, NULL, sep)`: namespace processing is
    /// enabled when `ns_separator` is `Some` (the separator may be empty).
    pub fn new(ns_separator: Option<&[u8]>) -> Option<Self> {
        ensure_init();
        let state = Box::new(CompatState {
            ctxt: Cell::new(std::ptr::null_mut()),
            ns_separator: ns_separator.map(|s| {
                let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                s[..end].to_vec()
            }),
            installed: Cell::new(0),
            sink: Cell::new(std::ptr::null_mut()),
        });
        let mut handlers = compat_handlers();
        let user = std::ptr::from_ref::<CompatState>(&state)
            .cast_mut()
            .cast::<c_void>();
        // SAFETY: the handler struct is copied by libxml2; `user` points at
        // the boxed state which lives as long as the parser.
        let ctxt = unsafe {
            ffi::xmlCreatePushParserCtxt(
                &raw mut handlers,
                user,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        };
        let ctxt = NonNull::new(ctxt)?;
        state.ctxt.set(ctxt.as_ptr());
        // SAFETY: ctxt is a fresh, exclusively owned parser context.
        unsafe {
            let c = ctxt.as_ptr();
            // php_libxml_sanitize_parse_ctxt_options
            (*c).loadsubset = 0;
            (*c).validate = 0;
            (*c).pedantic = 0;
            (*c).replaceEntities = 0;
            (*c).linenumbers = 0;
            (*c).keepBlanks = 1;
            (*c).options = 0;
            ffi::xmlCtxtUseOptions(c, ffi::XML_PARSE_OLDSAX | ffi::XML_PARSE_NOENT);
            (*c).wellFormed = 0;
            if state.ns_separator.is_none() && !(*c).sax.is_null() {
                // Reset flag as XML_SAX2_MAGIC is needed for
                // xmlCreatePushParserCtxt, so must be set in the handlers.
                (*(*c).sax).initialized = 1;
            }
        }
        let registration = CtxtRegistration::new(ctxt.as_ptr(), ParserChannels::SILENT);
        Some(Self {
            ctxt,
            state,
            _registration: registration,
        })
    }

    /// Installs or removes handler slots (see [`slot`]).
    pub fn set_installed(&self, slots: u16, installed: bool) {
        let cur = self.state.installed.get();
        self.state
            .installed
            .set(if installed { cur | slots } else { cur & !slots });
    }

    /// The installed slots.
    pub fn installed(&self) -> u16 {
        self.state.installed.get()
    }

    /// Whether a `parse` call is running.
    pub fn is_parsing(&self) -> bool {
        !self.state.sink.get().is_null()
    }

    /// `XML_Parse`: feeds a chunk and returns 1 on success, 0 on error.
    /// `huge` toggles `XML_PARSE_HUGE` and the dictionary limit first
    /// (`xml_parse_helper`).
    pub fn parse(
        &self,
        data: &[u8],
        is_final: bool,
        huge: bool,
        sink: &mut dyn ExpatSink,
    ) -> Result<i32, Reentrant> {
        if self.is_parsing() {
            return Err(Reentrant);
        }
        let c = self.ctxt.as_ptr();
        // SAFETY: we own the context exclusively; no parse is running.
        unsafe {
            if huge {
                (*c).options |= ffi::XML_PARSE_HUGE;
                ffi::xmlDictSetLimit((*c).dict, 0);
            } else {
                (*c).options &= !ffi::XML_PARSE_HUGE;
                ffi::xmlDictSetLimit((*c).dict, ffi::XML_MAX_DICTIONARY_LIMIT);
            }
        }
        let len = c_int::try_from(data.len()).unwrap_or(c_int::MAX);
        let mut sink_ref: &mut dyn ExpatSink = sink;
        self.state
            .sink
            .set(std::ptr::from_mut(&mut sink_ref).cast::<c_void>());
        // SAFETY: data is valid for len bytes; the sink pointer stays valid
        // until it is cleared below, after xmlParseChunk returned.
        let error =
            unsafe { ffi::xmlParseChunk(c, data.as_ptr().cast(), len, c_int::from(is_final)) };
        self.state.sink.set(std::ptr::null_mut());
        guard::resume();
        if error != 0 {
            return Ok(0);
        }
        // SAFETY: the context is live; the returned error record belongs to it.
        let ok = unsafe {
            let e = ffi::xmlCtxtGetLastError(c.cast());
            e.is_null() || (*e).level <= ffi::XML_ERR_WARNING
        };
        Ok(i32::from(ok))
    }

    /// `XML_GetErrorCode`: `ctxt->errNo`.
    pub fn error_code(&self) -> i32 {
        // SAFETY: the context is live for the parser's lifetime.
        unsafe { (*self.ctxt.as_ptr()).errNo }
    }

    fn input(&self) -> Option<NonNull<ffi::xmlParserInput>> {
        // SAFETY: the context is live; input is NULL or a live input.
        NonNull::new(unsafe { (*self.ctxt.as_ptr()).input })
    }

    /// `XML_GetCurrentLineNumber`.
    pub fn current_line(&self) -> i64 {
        // SAFETY: input is live while owned by the context.
        self.input()
            .map_or(0, |i| i64::from(unsafe { i.as_ref() }.line))
    }

    /// `XML_GetCurrentColumnNumber`.
    pub fn current_column(&self) -> i64 {
        // SAFETY: as above.
        self.input()
            .map_or(0, |i| i64::from(unsafe { i.as_ref() }.col))
    }

    /// `XML_GetCurrentByteIndex`: `consumed + (cur - base)`.
    pub fn current_byte_index(&self) -> i64 {
        self.input().map_or(0, |i| {
            // SAFETY: base and cur point into the same live input buffer.
            let input = unsafe { i.as_ref() };
            let offset = if input.cur.is_null() || input.base.is_null() {
                0
            } else {
                // SAFETY: both pointers belong to the same allocation.
                i64::try_from(unsafe { input.cur.offset_from(input.base) }).unwrap_or(0)
            };
            i64::try_from(input.consumed)
                .unwrap_or(i64::MAX)
                .saturating_add(offset)
        })
    }
}

impl Drop for PushParser {
    fn drop(&mut self) {
        let c = self.ctxt.as_ptr();
        // SAFETY: we own the context; myDoc (if any) is owned by it and not
        // referenced elsewhere. The CompatState box outlives this call.
        unsafe {
            if !(*c).myDoc.is_null() {
                ffi::xmlFreeDoc((*c).myDoc);
                (*c).myDoc = std::ptr::null_mut();
            }
            ffi::xmlFreeParserCtxt(c);
        }
        self.state.ctxt.set(std::ptr::null_mut());
    }
}
