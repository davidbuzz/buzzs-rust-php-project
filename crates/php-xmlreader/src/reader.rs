//! The `XMLReader` object state and methods (`php_xmlreader.c`).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Read;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;

use php_libxml::ffi as lx;
use php_libxml::tree::{Document, Node};

use crate::ffi_extra as fx;
use crate::{ReaderError, ReaderHost};

/// A C string from PHP bytes, truncated at the first NUL (what the C code
/// sees when it passes a `char *` on).
fn c_string(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Reserves the PHP string a libxml2 string of `len` bytes becomes
/// (`ZEND_MM_ALIGNED_SIZE(24 + len + 1)`; 0/1-byte strings are
/// interned) against `memory_limit` before copying it, like PHP's
/// `RETVAL_STRING` allocation. `false` means the limit was hit: the
/// violation is recorded and the VM raises "Allowed memory size of N
/// bytes exhausted" right after the call, so the caller returns an empty
/// value instead of allocating.
pub(crate) fn reserve_string(len: usize) -> bool {
    if len <= 1 {
        return true;
    }
    let size = len.saturating_add(24 + 1 + 7) & !7;
    php_memlimit::try_reserve(size).is_ok()
}

/// Copies and frees a libxml2-allocated string.
///
/// # Safety
/// `p` must be NULL or a NUL-terminated string from libxml2's allocator
/// that is not used afterwards.
unsafe fn take_xml_string(p: *mut u8) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: valid C string per the contract.
    let raw = unsafe { CStr::from_ptr(p.cast::<c_char>()) }.to_bytes();
    let bytes = if reserve_string(raw.len()) {
        raw.to_vec()
    } else {
        Vec::new()
    };
    // SAFETY: allocated by libxml2, released exactly once.
    unsafe {
        if let Some(free) = lx::xmlFree {
            free(p.cast::<c_void>());
        }
    }
    Some(bytes)
}

/// Copies a borrowed libxml2 string.
///
/// # Safety
/// `p` must be NULL or a NUL-terminated string valid for reads.
unsafe fn const_string(p: *const u8) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        let raw = unsafe { CStr::from_ptr(p.cast::<c_char>()) }.to_bytes();
        Some(if reserve_string(raw.len()) {
            raw.to_vec()
        } else {
            Vec::new()
        })
    }
}

/// `(int)` of a `zend_long` passed to a C `int` parameter (truncating).
#[allow(clippy::cast_possible_truncation)]
fn c_int_of(v: i64) -> c_int {
    v as c_int
}

/// Checks a `p` (path) parameter for NUL bytes.
fn check_path(func: &str, num: u32, name: &str, value: &[u8]) -> Result<(), ReaderError> {
    if value.contains(&0) {
        return Err(ReaderError::argument(
            func,
            num,
            name,
            "must not contain any null bytes",
        ));
    }
    Ok(())
}

/// `xmlreader_valid_encoding`.
fn valid_encoding(encoding: Option<&[u8]>) -> bool {
    let Some(enc) = encoding else { return true };
    php_libxml::raw::ensure_init();
    let c = c_string(enc);
    // SAFETY: c is a valid C string; the handler is released right away.
    unsafe {
        let handler = lx::xmlFindCharEncodingHandler(c.as_ptr());
        if handler.is_null() {
            return false;
        }
        lx::xmlCharEncCloseFunc(handler);
    }
    true
}

/// Lexical normalisation of an absolute path (`.`/`..`/duplicate slashes),
/// as `virtual_file_ex(CWD_EXPAND)` does.
fn normalize_path(path: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for seg in path.split(|&b| b == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = Vec::with_capacity(path.len());
    for p in &parts {
        out.push(b'/');
        out.extend_from_slice(p);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

/// `expand_filepath`: an absolute, normalised version of `path`.
fn expand_filepath(path: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    if path.is_empty() {
        return None;
    }
    if path.first() == Some(&b'/') {
        return Some(normalize_path(path));
    }
    let cwd = std::env::current_dir().ok()?;
    let mut full = cwd.as_os_str().as_bytes().to_vec();
    full.push(b'/');
    full.extend_from_slice(path);
    Some(normalize_path(&full))
}

/// `VCWD_REALPATH`.
fn realpath(path: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if path.is_empty() {
        return None;
    }
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
    std::fs::canonicalize(p)
        .ok()
        .map(|p| p.into_os_string().into_vec())
}

/// `_xmlreader_get_valid_file_path`: resolves local paths and `file://`
/// URIs to an absolute path; other URIs are returned unchanged.
pub(crate) fn valid_file_path(source: &[u8]) -> Option<Vec<u8>> {
    php_libxml::raw::ensure_init();
    let has_scheme = php_libxml::raw::uri::reference_scheme(source).is_some();
    let mut src = source;
    let mut is_file_uri = false;
    if has_scheme {
        let lower: Vec<u8> = source.iter().take(17).map(u8::to_ascii_lowercase).collect();
        if lower.starts_with(b"file:///") {
            is_file_uri = true;
            src = &source[7..];
        } else if lower.starts_with(b"file://localhost/") {
            is_file_uri = true;
            src = &source[16..];
        }
    }
    if !has_scheme || is_file_uri {
        return realpath(src).or_else(|| expand_filepath(src));
    }
    Some(src.to_vec())
}

/// Context of a stream-backed reader (`fromStream`).
struct StreamCtx {
    reader: Box<dyn Read>,
}

unsafe extern "C" fn stream_read(ctx: *mut c_void, buf: *mut c_char, len: c_int) -> c_int {
    if ctx.is_null() || buf.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: ctx is the StreamCtx box leaked in `Source::from_stream`,
    // alive until `stream_close`; buf is writable for len bytes.
    let (stream, out) = unsafe {
        (
            &mut *ctx.cast::<StreamCtx>(),
            std::slice::from_raw_parts_mut(buf.cast::<u8>(), usize::try_from(len).unwrap_or(0)),
        )
    };
    match catch_unwind(AssertUnwindSafe(|| stream.reader.read(out))) {
        Ok(Ok(n)) => c_int::try_from(n).unwrap_or(-1),
        _ => -1,
    }
}

unsafe extern "C" fn stream_close(ctx: *mut c_void) -> c_int {
    if !ctx.is_null() {
        // SAFETY: ctx is the leaked StreamCtx box; libxml2 closes it once.
        let boxed = unsafe { Box::from_raw(ctx.cast::<StreamCtx>()) };
        let _ = catch_unwind(AssertUnwindSafe(|| drop(boxed)));
    }
    0
}

/// A loaded libxml2 reader (the `ptr`/`input`/`schema` triple of
/// `xmlreader_object`), not necessarily attached to an object yet.
pub struct Source {
    reader: NonNull<fx::xmlTextReader>,
    /// Memory input buffer of `XML()`/`fromString()` (owned by us, not by
    /// the reader).
    input: Option<NonNull<lx::xmlParserInputBuffer>>,
    /// The Relax-NG schema installed with `setRelaxNGSchema*()`.
    schema: Option<NonNull<fx::xmlRelaxNG>>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("reader", &self.reader)
            .finish_non_exhaustive()
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: we own the reader, its separate input buffer and the
        // schema; the reader (which references both) is freed first.
        unsafe {
            fx::xmlFreeTextReader(self.reader.as_ptr());
            if let Some(input) = self.input.take() {
                lx::xmlFreeParserInputBuffer(input.as_ptr());
            }
            if let Some(schema) = self.schema.take() {
                fx::xmlRelaxNGFree(schema.as_ptr());
            }
        }
    }
}

/// Why loading data failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// An argument was rejected (thrown in every calling mode).
    Invalid(ReaderError),
    /// libxml2 could not open / set up the source: a warning ("Unable to
    /// open source data" / "Unable to load source data") for the legacy
    /// `open()`/`XML()`, an `Error` for `fromUri()`/`fromString()`.
    Unable,
}

impl Source {
    fn wrap(reader: *mut fx::xmlTextReader) -> Option<Self> {
        NonNull::new(reader).map(|reader| Self {
            reader,
            input: None,
            schema: None,
        })
    }

    /// `xml_reader_from_uri` minus the object handling: validates the
    /// arguments (as `XMLReader::{func}()`) and opens `uri` with
    /// `xmlReaderForFile` (through the libxml stream hooks).
    pub fn from_uri(
        func: &str,
        uri: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
    ) -> Result<Self, LoadError> {
        check_path(func, 1, "uri", uri).map_err(LoadError::Invalid)?;
        if let Some(e) = encoding {
            check_path(func, 2, "encoding", e).map_err(LoadError::Invalid)?;
        }
        if uri.is_empty() {
            return Err(LoadError::Invalid(ReaderError::argument(
                func,
                1,
                "uri",
                "must not be empty",
            )));
        }
        if !valid_encoding(encoding) {
            return Err(LoadError::Invalid(ReaderError::argument(
                func,
                2,
                "encoding",
                "must be a valid character encoding",
            )));
        }
        let path = valid_file_path(uri).ok_or(LoadError::Unable)?;
        let p = c_string(&path);
        let enc = encoding.map(c_string);
        // SAFETY: valid C strings; the reader is owned by the Source.
        let reader = unsafe {
            fx::xmlReaderForFile(
                p.as_ptr(),
                enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                c_int_of(flags),
            )
        };
        Self::wrap(reader).ok_or(LoadError::Unable)
    }

    /// `xml_reader_from_string` minus the object handling.
    pub fn from_string(
        func: &str,
        source: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
    ) -> Result<Self, LoadError> {
        if let Some(e) = encoding {
            check_path(func, 2, "encoding", e).map_err(LoadError::Invalid)?;
        }
        if source.is_empty() {
            return Err(LoadError::Invalid(ReaderError::argument(
                func,
                1,
                "source",
                "must not be empty",
            )));
        }
        if !valid_encoding(encoding) {
            return Err(LoadError::Invalid(ReaderError::argument(
                func,
                2,
                "encoding",
                "must be a valid character encoding",
            )));
        }
        php_libxml::raw::ensure_init();
        let len = c_int::try_from(source.len()).map_err(|_| LoadError::Unable)?;
        // SAFETY: the buffer copies `source`.
        let input = unsafe {
            fx::xmlParserInputBufferCreateMem(
                source.as_ptr().cast(),
                len,
                lx::XML_CHAR_ENCODING_NONE,
            )
        };
        let input = NonNull::new(input).ok_or(LoadError::Unable)?;
        // The base URI is the current directory (with a trailing slash).
        let uri = std::env::current_dir().ok().and_then(|d| {
            use std::os::unix::ffi::OsStrExt;
            let mut dir = d.as_os_str().as_bytes().to_vec();
            if dir.last() != Some(&b'/') {
                dir.push(b'/');
            }
            let c = c_string(&dir);
            // SAFETY: c is a valid C string; the result is a fresh copy.
            unsafe { take_xml_string(fx::xmlCanonicPath(c.as_ptr().cast())) }
        });
        let uri_c = uri.as_deref().map(c_string);
        let uri_p = uri_c.as_ref().map_or(std::ptr::null(), |u| u.as_ptr());
        let enc = encoding.map(c_string);
        // SAFETY: the reader reads from `input`, which we keep alive and
        // free after the reader (it does not take ownership of it).
        unsafe {
            let reader = fx::xmlNewTextReader(input.as_ptr(), uri_p);
            if let Some(mut src) = Self::wrap(reader) {
                src.input = Some(input);
                let ret = fx::xmlTextReaderSetup(
                    src.reader.as_ptr(),
                    std::ptr::null_mut(),
                    uri_p,
                    enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                    c_int_of(flags),
                );
                if ret == 0 {
                    return Ok(src);
                }
                // Dropping `src` frees reader and input.
                return Err(LoadError::Unable);
            }
            lx::xmlFreeParserInputBuffer(input.as_ptr());
        }
        Err(LoadError::Unable)
    }

    /// `XMLReader::fromStream()` minus the object handling. The stream is
    /// read on demand and dropped when the reader is freed.
    pub fn from_stream(
        stream: Box<dyn Read>,
        encoding: Option<&[u8]>,
        flags: i64,
        document_uri: Option<&[u8]>,
    ) -> Result<Self, ReaderError> {
        const FUNC: &str = "fromStream";
        if let Some(e) = encoding {
            check_path(FUNC, 2, "encoding", e)?;
        }
        if let Some(u) = document_uri {
            check_path(FUNC, 4, "documentUri", u)?;
        }
        if !valid_encoding(encoding) {
            return Err(ReaderError::argument(
                FUNC,
                2,
                "encoding",
                "must be a valid character encoding",
            ));
        }
        php_libxml::raw::ensure_init();
        let ctx = Box::into_raw(Box::new(StreamCtx { reader: stream }));
        let uri = document_uri.map(c_string);
        let enc = encoding.map(c_string);
        // SAFETY: the callbacks match the context type; libxml2 calls
        // `stream_close` exactly once (also on failure).
        let reader = unsafe {
            fx::xmlReaderForIO(
                Some(stream_read),
                Some(stream_close),
                ctx.cast(),
                uri.as_ref().map_or(std::ptr::null(), |u| u.as_ptr()),
                enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                c_int_of(flags),
            )
        };
        Self::wrap(reader).ok_or_else(|| ReaderError::error("Could not construct libxml reader"))
    }

    fn ptr(&self) -> *mut fx::xmlTextReader {
        self.reader.as_ptr()
    }
}

/// The virtual read-only properties of `XMLReader`, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Property {
    /// `attributeCount` (int).
    AttributeCount,
    /// `baseURI` (string).
    BaseUri,
    /// `depth` (int).
    Depth,
    /// `hasAttributes` (bool).
    HasAttributes,
    /// `hasValue` (bool).
    HasValue,
    /// `isDefault` (bool).
    IsDefault,
    /// `isEmptyElement` (bool).
    IsEmptyElement,
    /// `localName` (string).
    LocalName,
    /// `name` (string).
    Name,
    /// `namespaceURI` (string).
    NamespaceUri,
    /// `nodeType` (int).
    NodeType,
    /// `prefix` (string).
    Prefix,
    /// `value` (string).
    Value,
    /// `xmlLang` (string).
    XmlLang,
}

/// A property value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    /// `int`.
    Int(i64),
    /// `bool`.
    Bool(bool),
    /// `string`.
    Str(Vec<u8>),
}

impl PropValue {
    /// `zend_is_true`.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Int(i) => *i != 0,
            Self::Bool(b) => *b,
            Self::Str(s) => !(s.is_empty() || s == b"0"),
        }
    }
}

type IntFn = unsafe extern "C" fn(*mut fx::xmlTextReader) -> c_int;
type StrFn = unsafe extern "C" fn(*mut fx::xmlTextReader) -> *const u8;

enum Getter {
    Int(IntFn),
    Bool(IntFn),
    Str(StrFn),
}

impl Property {
    /// All properties in declaration (and `var_dump`) order.
    pub const ALL: [Property; 14] = [
        Self::AttributeCount,
        Self::BaseUri,
        Self::Depth,
        Self::HasAttributes,
        Self::HasValue,
        Self::IsDefault,
        Self::IsEmptyElement,
        Self::LocalName,
        Self::Name,
        Self::NamespaceUri,
        Self::NodeType,
        Self::Prefix,
        Self::Value,
        Self::XmlLang,
    ];

    /// The property name.
    pub fn name(self) -> &'static str {
        match self {
            Self::AttributeCount => "attributeCount",
            Self::BaseUri => "baseURI",
            Self::Depth => "depth",
            Self::HasAttributes => "hasAttributes",
            Self::HasValue => "hasValue",
            Self::IsDefault => "isDefault",
            Self::IsEmptyElement => "isEmptyElement",
            Self::LocalName => "localName",
            Self::Name => "name",
            Self::NamespaceUri => "namespaceURI",
            Self::NodeType => "nodeType",
            Self::Prefix => "prefix",
            Self::Value => "value",
            Self::XmlLang => "xmlLang",
        }
    }

    /// Looks a property up by (case-sensitive) name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The declared type (`int`, `string` or `bool`).
    pub fn type_name(self) -> &'static str {
        match self.getter() {
            Getter::Int(_) => "int",
            Getter::Bool(_) => "bool",
            Getter::Str(_) => "string",
        }
    }

    fn getter(self) -> Getter {
        match self {
            Self::AttributeCount => Getter::Int(fx::xmlTextReaderAttributeCount),
            Self::BaseUri => Getter::Str(fx::xmlTextReaderConstBaseUri),
            Self::Depth => Getter::Int(fx::xmlTextReaderDepth),
            Self::HasAttributes => Getter::Bool(fx::xmlTextReaderHasAttributes),
            Self::HasValue => Getter::Bool(fx::xmlTextReaderHasValue),
            Self::IsDefault => Getter::Bool(fx::xmlTextReaderIsDefault),
            Self::IsEmptyElement => Getter::Bool(fx::xmlTextReaderIsEmptyElement),
            Self::LocalName => Getter::Str(fx::xmlTextReaderConstLocalName),
            Self::Name => Getter::Str(fx::xmlTextReaderConstName),
            Self::NamespaceUri => Getter::Str(fx::xmlTextReaderConstNamespaceUri),
            Self::NodeType => Getter::Int(fx::xmlTextReaderNodeType),
            Self::Prefix => Getter::Str(fx::xmlTextReaderConstPrefix),
            Self::Value => Getter::Str(fx::xmlTextReaderConstValue),
            Self::XmlLang => Getter::Str(fx::xmlTextReaderConstXmlLang),
        }
    }
}

/// How `isset()`/`empty()` query a property (`has_property`'s `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssetKind {
    /// `property_exists()`-style (`ZEND_PROPERTY_EXISTS`).
    Exists,
    /// `isset()` (`ZEND_PROPERTY_ISSET`).
    Isset,
    /// `!empty()` (`ZEND_PROPERTY_NOT_EMPTY`).
    NotEmpty,
}

/// The state of one `XMLReader` object.
#[derive(Debug, Default)]
pub struct XmlReader {
    source: Option<Source>,
}

const NOT_LOADED_READ: &str = "Data must be loaded before reading";

impl XmlReader {
    /// A fresh, unloaded reader (`new XMLReader()`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches loaded data (after the VM instantiated the object for a
    /// static constructor), replacing whatever was loaded.
    pub fn attach(&mut self, source: Source) {
        self.source = Some(source);
    }

    /// Whether data is loaded (`intern->ptr != NULL`).
    pub fn is_loaded(&self) -> bool {
        self.source.is_some()
    }

    fn ptr(&self) -> Option<*mut fx::xmlTextReader> {
        self.source.as_ref().map(Source::ptr)
    }

    /// `XMLReader::close()`: frees all resources; always `true`.
    pub fn close(&mut self) -> bool {
        self.source = None;
        true
    }

    /// `$reader->open($uri, $encoding, $flags)` called on an instance.
    /// Returns `Ok(false)` after the warning "Unable to open source data".
    pub fn open(
        &mut self,
        uri: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        // zpp runs before the old data is released.
        check_path("open", 1, "uri", uri)?;
        if let Some(e) = encoding {
            check_path("open", 2, "encoding", e)?;
        }
        self.close();
        match Source::from_uri("open", uri, encoding, flags) {
            Ok(src) => {
                self.source = Some(src);
                Ok(true)
            }
            Err(LoadError::Invalid(e)) => Err(e),
            Err(LoadError::Unable) => {
                host.warning("Unable to open source data");
                Ok(false)
            }
        }
    }

    /// `XMLReader::open()` called statically: `Ok(None)` (after the warning)
    /// means `false`; on success the VM instantiates `XMLReader` and
    /// attaches the source.
    pub fn open_static(
        uri: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
        host: &mut dyn ReaderHost,
    ) -> Result<Option<Source>, ReaderError> {
        match Source::from_uri("open", uri, encoding, flags) {
            Ok(src) => Ok(Some(src)),
            Err(LoadError::Invalid(e)) => Err(e),
            Err(LoadError::Unable) => {
                host.warning("Unable to open source data");
                Ok(None)
            }
        }
    }

    /// `XMLReader::fromUri()`: the VM then instantiates the called class.
    pub fn from_uri(
        uri: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
    ) -> Result<Source, ReaderError> {
        Source::from_uri("fromUri", uri, encoding, flags).map_err(|e| match e {
            LoadError::Invalid(e) => e,
            LoadError::Unable => ReaderError::error("Unable to open source data"),
        })
    }

    /// `$reader->XML($source, $encoding, $flags)` called on an instance.
    pub fn xml(
        &mut self,
        source: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        if let Some(e) = encoding {
            check_path("XML", 2, "encoding", e)?;
        }
        self.close();
        match Source::from_string("XML", source, encoding, flags) {
            Ok(src) => {
                self.source = Some(src);
                Ok(true)
            }
            Err(LoadError::Invalid(e)) => Err(e),
            Err(LoadError::Unable) => {
                host.warning("Unable to load source data");
                Ok(false)
            }
        }
    }

    /// `XMLReader::XML()` called statically.
    pub fn xml_static(
        source: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
        host: &mut dyn ReaderHost,
    ) -> Result<Option<Source>, ReaderError> {
        match Source::from_string("XML", source, encoding, flags) {
            Ok(src) => Ok(Some(src)),
            Err(LoadError::Invalid(e)) => Err(e),
            Err(LoadError::Unable) => {
                host.warning("Unable to load source data");
                Ok(None)
            }
        }
    }

    /// `XMLReader::fromString()`.
    pub fn from_string(
        source: &[u8],
        encoding: Option<&[u8]>,
        flags: i64,
    ) -> Result<Source, ReaderError> {
        Source::from_string("fromString", source, encoding, flags).map_err(|e| match e {
            LoadError::Invalid(e) => e,
            LoadError::Unable => ReaderError::error("Unable to load source data"),
        })
    }

    /// `XMLReader::fromStream()`.
    pub fn from_stream(
        stream: Box<dyn Read>,
        encoding: Option<&[u8]>,
        flags: i64,
        document_uri: Option<&[u8]>,
    ) -> Result<Source, ReaderError> {
        Source::from_stream(stream, encoding, flags, document_uri)
    }

    /// `XMLReader::read()`.
    pub fn read(&mut self) -> Result<bool, ReaderError> {
        let ptr = self
            .ptr()
            .ok_or_else(|| ReaderError::error(NOT_LOADED_READ))?;
        // SAFETY: ptr is the live reader.
        let ret = unsafe { fx::xmlTextReaderRead(ptr) };
        Ok(ret == 1)
    }

    /// `XMLReader::next($name)`.
    pub fn next(&mut self, name: Option<&[u8]>) -> Result<bool, ReaderError> {
        let ptr = self
            .ptr()
            .ok_or_else(|| ReaderError::error(NOT_LOADED_READ))?;
        let wanted = name.map(|n| c_string(n).into_bytes());
        // SAFETY: ptr is the live reader; the local name is borrowed only
        // until the next call.
        unsafe {
            let mut ret = fx::xmlTextReaderNext(ptr);
            if let Some(wanted) = &wanted {
                while ret == 1 {
                    let local = const_string(fx::xmlTextReaderConstLocalName(ptr));
                    if local.as_deref() == Some(wanted.as_slice()) {
                        return Ok(true);
                    }
                    ret = fx::xmlTextReaderNext(ptr);
                }
            }
            Ok(ret == 1)
        }
    }

    fn string_arg(
        &self,
        func: &str,
        arg: &str,
        value: &[u8],
        f: unsafe extern "C" fn(*mut fx::xmlTextReader, *const u8) -> *mut u8,
    ) -> Result<Option<Vec<u8>>, ReaderError> {
        if value.is_empty() {
            return Err(ReaderError::argument(func, 1, arg, "must not be empty"));
        }
        let Some(ptr) = self.ptr() else {
            return Ok(None);
        };
        let c = c_string(value);
        // SAFETY: live reader, valid C string, fresh result string.
        Ok(unsafe { take_xml_string(f(ptr, c.as_ptr().cast())) })
    }

    /// `XMLReader::getAttribute($name)`.
    pub fn get_attribute(&self, name: &[u8]) -> Result<Option<Vec<u8>>, ReaderError> {
        self.string_arg("getAttribute", "name", name, fx::xmlTextReaderGetAttribute)
    }

    /// `XMLReader::lookupNamespace($prefix)`.
    pub fn lookup_namespace(&self, prefix: &[u8]) -> Result<Option<Vec<u8>>, ReaderError> {
        self.string_arg(
            "lookupNamespace",
            "prefix",
            prefix,
            fx::xmlTextReaderLookupNamespace,
        )
    }

    /// `XMLReader::getAttributeNo($index)`.
    pub fn get_attribute_no(&self, index: i64) -> Option<Vec<u8>> {
        let ptr = self.ptr()?;
        // SAFETY: live reader; fresh result string.
        unsafe { take_xml_string(fx::xmlTextReaderGetAttributeNo(ptr, c_int_of(index))) }
    }

    fn check_ns_args(func: &str, name: &[u8], namespace: &[u8]) -> Result<(), ReaderError> {
        if name.is_empty() {
            return Err(ReaderError::argument(func, 1, "name", "must not be empty"));
        }
        if namespace.is_empty() {
            return Err(ReaderError::argument(
                func,
                2,
                "namespace",
                "must not be empty",
            ));
        }
        Ok(())
    }

    /// `XMLReader::getAttributeNs($name, $namespace)`.
    pub fn get_attribute_ns(
        &self,
        name: &[u8],
        namespace: &[u8],
    ) -> Result<Option<Vec<u8>>, ReaderError> {
        Self::check_ns_args("getAttributeNs", name, namespace)?;
        let Some(ptr) = self.ptr() else {
            return Ok(None);
        };
        let n = c_string(name);
        let ns = c_string(namespace);
        // SAFETY: live reader, valid C strings, fresh result string.
        Ok(unsafe {
            take_xml_string(fx::xmlTextReaderGetAttributeNs(
                ptr,
                n.as_ptr().cast(),
                ns.as_ptr().cast(),
            ))
        })
    }

    fn parser_ptr(&self) -> Result<*mut fx::xmlTextReader, ReaderError> {
        self.ptr().ok_or_else(|| {
            ReaderError::error("Cannot access parser properties before loading data")
        })
    }

    /// `XMLReader::getParserProperty($property)`.
    pub fn get_parser_property(&self, property: i64) -> Result<bool, ReaderError> {
        let ptr = self.parser_ptr()?;
        // SAFETY: live reader.
        let ret = unsafe { fx::xmlTextReaderGetParserProp(ptr, c_int_of(property)) };
        if ret == -1 {
            return Err(ReaderError::argument(
                "getParserProperty",
                1,
                "property",
                "must be a valid parser property",
            ));
        }
        Ok(ret != 0)
    }

    /// `XMLReader::setParserProperty($property, $value)`.
    pub fn set_parser_property(&mut self, property: i64, value: bool) -> Result<bool, ReaderError> {
        let ptr = self.parser_ptr()?;
        // SAFETY: live reader.
        let ret =
            unsafe { fx::xmlTextReaderSetParserProp(ptr, c_int_of(property), c_int::from(value)) };
        if ret == -1 {
            return Err(ReaderError::argument(
                "setParserProperty",
                1,
                "property",
                "must be a valid parser property",
            ));
        }
        Ok(true)
    }

    fn no_arg(&mut self, f: unsafe extern "C" fn(*mut fx::xmlTextReader) -> c_int) -> bool {
        // SAFETY: live reader.
        self.ptr().is_some_and(|ptr| unsafe { f(ptr) } == 1)
    }

    /// `XMLReader::isValid()`.
    pub fn is_valid(&mut self) -> bool {
        self.no_arg(fx::xmlTextReaderIsValid)
    }

    /// `XMLReader::moveToElement()`.
    pub fn move_to_element(&mut self) -> bool {
        self.no_arg(fx::xmlTextReaderMoveToElement)
    }

    /// `XMLReader::moveToFirstAttribute()`.
    pub fn move_to_first_attribute(&mut self) -> bool {
        self.no_arg(fx::xmlTextReaderMoveToFirstAttribute)
    }

    /// `XMLReader::moveToNextAttribute()`.
    pub fn move_to_next_attribute(&mut self) -> bool {
        self.no_arg(fx::xmlTextReaderMoveToNextAttribute)
    }

    /// `XMLReader::moveToAttribute($name)`.
    pub fn move_to_attribute(&mut self, name: &[u8]) -> Result<bool, ReaderError> {
        if name.is_empty() {
            return Err(ReaderError::argument(
                "moveToAttribute",
                1,
                "name",
                "must not be empty",
            ));
        }
        let Some(ptr) = self.ptr() else {
            return Ok(false);
        };
        let c = c_string(name);
        // SAFETY: live reader, valid C string.
        Ok(unsafe { fx::xmlTextReaderMoveToAttribute(ptr, c.as_ptr().cast()) } == 1)
    }

    /// `XMLReader::moveToAttributeNo($index)`.
    pub fn move_to_attribute_no(&mut self, index: i64) -> bool {
        // SAFETY: live reader.
        self.ptr().is_some_and(
            |ptr| unsafe { fx::xmlTextReaderMoveToAttributeNo(ptr, c_int_of(index)) } == 1,
        )
    }

    /// `XMLReader::moveToAttributeNs($name, $namespace)`.
    pub fn move_to_attribute_ns(
        &mut self,
        name: &[u8],
        namespace: &[u8],
    ) -> Result<bool, ReaderError> {
        Self::check_ns_args("moveToAttributeNs", name, namespace)?;
        let Some(ptr) = self.ptr() else {
            return Ok(false);
        };
        let n = c_string(name);
        let ns = c_string(namespace);
        // SAFETY: live reader, valid C strings.
        Ok(unsafe {
            fx::xmlTextReaderMoveToAttributeNs(ptr, n.as_ptr().cast(), ns.as_ptr().cast())
        } == 1)
    }

    fn no_arg_string(
        &mut self,
        f: unsafe extern "C" fn(*mut fx::xmlTextReader) -> *mut u8,
    ) -> Vec<u8> {
        let Some(ptr) = self.ptr() else {
            return Vec::new();
        };
        // SAFETY: live reader; fresh result string.
        unsafe { take_xml_string(f(ptr)) }.unwrap_or_default()
    }

    /// `XMLReader::readInnerXml()`.
    pub fn read_inner_xml(&mut self) -> Vec<u8> {
        self.no_arg_string(fx::xmlTextReaderReadInnerXml)
    }

    /// `XMLReader::readOuterXml()`.
    pub fn read_outer_xml(&mut self) -> Vec<u8> {
        self.no_arg_string(fx::xmlTextReaderReadOuterXml)
    }

    /// `XMLReader::readString()`.
    pub fn read_string(&mut self) -> Vec<u8> {
        self.no_arg_string(fx::xmlTextReaderReadString)
    }

    /// `XMLReader::setSchema($filename)` (XSD; `None` deactivates).
    pub fn set_schema(
        &mut self,
        filename: Option<&[u8]>,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        if let Some(f) = filename {
            check_path("setSchema", 1, "filename", f)?;
            if f.is_empty() {
                return Err(ReaderError::argument(
                    "setSchema",
                    1,
                    "filename",
                    "must not be empty",
                ));
            }
        }
        let ptr = self
            .ptr()
            .ok_or_else(|| ReaderError::error("Schema must be set prior to reading"))?;
        let c = filename.map(c_string);
        // SAFETY: live reader; valid C string or NULL.
        let ret = unsafe {
            fx::xmlTextReaderSchemaValidate(
                ptr,
                c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            )
        };
        if ret == 0 {
            Ok(true)
        } else {
            host.warning("Schema contains errors");
            Ok(false)
        }
    }

    /// `XMLReader::setRelaxNGSchema($filename)`.
    pub fn set_relaxng_schema(
        &mut self,
        filename: Option<&[u8]>,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        self.relaxng("setRelaxNGSchema", "filename", filename, true, host)
    }

    /// `XMLReader::setRelaxNGSchemaSource($source)`.
    pub fn set_relaxng_schema_source(
        &mut self,
        source: Option<&[u8]>,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        self.relaxng("setRelaxNGSchemaSource", "source", source, false, host)
    }

    fn relaxng(
        &mut self,
        func: &str,
        arg: &str,
        source: Option<&[u8]>,
        is_file: bool,
        host: &mut dyn ReaderHost,
    ) -> Result<bool, ReaderError> {
        if let Some(s) = source {
            check_path(func, 1, arg, s)?;
            if s.is_empty() {
                return Err(ReaderError::argument(func, 1, arg, "must not be empty"));
            }
        }
        let Some(src) = self.source.as_mut() else {
            return Err(ReaderError::error("Schema must be set prior to reading"));
        };
        let mut schema: *mut fx::xmlRelaxNG = std::ptr::null_mut();
        let ret = match source {
            Some(s) => {
                schema = load_relaxng(s, is_file);
                if schema.is_null() {
                    -1
                } else {
                    // SAFETY: live reader and freshly parsed schema.
                    unsafe { fx::xmlTextReaderRelaxNGSetSchema(src.ptr(), schema) }
                }
            }
            // SAFETY: live reader; NULL unsets the schema.
            None => unsafe { fx::xmlTextReaderRelaxNGSetSchema(src.ptr(), std::ptr::null_mut()) },
        };
        if ret == 0 {
            if let Some(old) = src.schema.take() {
                // SAFETY: the reader no longer references the old schema.
                unsafe { fx::xmlRelaxNGFree(old.as_ptr()) };
            }
            src.schema = NonNull::new(schema);
            Ok(true)
        } else {
            if !schema.is_null() {
                // SAFETY: the schema was not installed.
                unsafe { fx::xmlRelaxNGFree(schema) };
            }
            host.warning("Schema contains errors");
            Ok(false)
        }
    }

    /// `XMLReader::expand($baseNode)`: a deep copy of the current node,
    /// detached, in `base`'s document. Without a base the copy lives in a
    /// fresh empty document (PHP's copy has no document at all; the VM
    /// should report `ownerDocument` as `null` in that case).
    ///
    /// `Ok(None)` means `false` after the warning "An Error Occurred while
    /// expanding" or the notice "Cannot expand this node type".
    pub fn expand(
        &mut self,
        base: Option<&Document>,
        host: &mut dyn ReaderHost,
    ) -> Result<Option<Node>, ReaderError> {
        let ptr = self
            .ptr()
            .ok_or_else(|| ReaderError::error("Data must be loaded before expanding"))?;
        // SAFETY: live reader; the node belongs to the reader's document.
        let node = unsafe { fx::xmlTextReaderExpand(ptr) };
        if node.is_null() {
            host.warning("An Error Occurred while expanding");
            return Ok(None);
        }
        let Some(copy) = copy_out(node) else {
            host.notice("Cannot expand this node type");
            return Ok(None);
        };
        let Some(doc) = base else {
            return Ok(Some(copy));
        };
        if let Ok(n) = doc.import(&copy, true) {
            Ok(Some(n))
        } else {
            host.notice("Cannot expand this node type");
            Ok(None)
        }
    }

    /// `XMLReader::expand()` for a host object model that owns the result:
    /// `xmlDocCopyNode(node, doc, 1)` of the current node, `doc` being the
    /// base node's document or null (a document-less copy). The caller
    /// takes ownership of the returned detached node.
    ///
    /// `Ok(None)` means `false` after the warning "An Error Occurred while
    /// expanding" or the notice "Cannot expand this node type".
    ///
    /// # Safety
    /// `doc` must be null or a live `xmlDoc`.
    pub unsafe fn expand_raw(
        &mut self,
        doc: *mut lx::xmlDoc,
        host: &mut dyn ReaderHost,
    ) -> Result<Option<NonNull<lx::xmlNode>>, ReaderError> {
        let ptr = self
            .ptr()
            .ok_or_else(|| ReaderError::error("Data must be loaded before expanding"))?;
        // SAFETY: live reader; the node belongs to the reader's document.
        let node = unsafe { fx::xmlTextReaderExpand(ptr) };
        if node.is_null() {
            host.warning("An Error Occurred while expanding");
            return Ok(None);
        }
        // SAFETY: node is live; doc is null or live (caller).
        let copy = unsafe { lx::xmlDocCopyNode(node, doc, 1) };
        let copy = NonNull::new(copy);
        if copy.is_none() {
            host.notice("Cannot expand this node type");
        }
        Ok(copy)
    }

    /// Reads a virtual property (`xmlreader_property_reader`). Without
    /// loaded data every property has its type's zero value.
    pub fn read_property(&self, property: Property) -> Result<PropValue, ReaderError> {
        let ptr = self.ptr();
        match property.getter() {
            Getter::Str(f) => {
                // SAFETY: live reader; the string is copied immediately.
                let s = ptr.and_then(|p| unsafe { const_string(f(p)) });
                Ok(PropValue::Str(s.unwrap_or_default()))
            }
            Getter::Int(f) | Getter::Bool(f) => {
                let v = match ptr {
                    Some(p) => {
                        // SAFETY: live reader.
                        let v = unsafe { f(p) };
                        if v == -1 {
                            return Err(ReaderError::error(
                                "Failed to read property because no XML data has been read yet",
                            ));
                        }
                        v
                    }
                    None => 0,
                };
                Ok(match property.getter() {
                    Getter::Bool(_) => PropValue::Bool(v != 0),
                    _ => PropValue::Int(i64::from(v)),
                })
            }
        }
    }

    /// `has_property` for a virtual property.
    pub fn has_property(&self, property: Property, kind: IssetKind) -> Result<bool, ReaderError> {
        match kind {
            IssetKind::Exists => Ok(true),
            IssetKind::Isset => self.read_property(property).map(|_| true),
            IssetKind::NotEmpty => self.read_property(property).map(|v| v.is_truthy()),
        }
    }

    /// `get_debug_info`: the virtual properties that could be read, in
    /// order (to be appended after the object's own properties), plus the
    /// first exception raised while reading (PHP leaves it pending).
    pub fn debug_properties(&self) -> (Vec<(&'static str, PropValue)>, Option<ReaderError>) {
        let mut out = Vec::new();
        let mut first_error = None;
        for p in Property::ALL {
            match self.read_property(p) {
                Ok(v) => out.push((p.name(), v)),
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        (out, first_error)
    }

    /// The `Error` for writing a virtual property of an object of class
    /// `class_name`.
    pub fn write_property_error(class_name: &str, property: Property) -> ReaderError {
        ReaderError::error(format!(
            "Cannot modify readonly property {class_name}::${}",
            property.name()
        ))
    }

    /// The `Error` for `unset()` of a virtual property.
    pub fn unset_property_error(class_name: &str, property: Property) -> ReaderError {
        ReaderError::error(format!("Cannot unset {class_name}::${}", property.name()))
    }
}

/// `xmlDocCopyNode(node, NULL, 1)` into a fresh document, returned as a
/// detached handle.
fn copy_out(node: *mut lx::xmlNode) -> Option<Node> {
    php_libxml::raw::ensure_init();
    // SAFETY: we build a private document: a wrapper root element that
    // receives the deep copy (as child or attribute). Nothing else
    // references the document until it is handed to `Document::from_raw`.
    let doc = unsafe {
        let doc = lx::xmlNewDoc(c"1.0".as_ptr().cast());
        if doc.is_null() {
            return None;
        }
        let root = lx::xmlNewDocNode(
            doc,
            std::ptr::null_mut(),
            c"x".as_ptr().cast(),
            std::ptr::null(),
        );
        if root.is_null() {
            lx::xmlFreeDoc(doc);
            return None;
        }
        lx::xmlDocSetRootElement(doc, root);
        let copy = lx::xmlDocCopyNode(node, doc, 1);
        let ok = !copy.is_null() && {
            let t = (*copy).type_;
            let tree_content = matches!(
                t,
                lx::XML_ELEMENT_NODE
                    | lx::XML_TEXT_NODE
                    | lx::XML_CDATA_SECTION_NODE
                    | lx::XML_ENTITY_REF_NODE
                    | lx::XML_PI_NODE
                    | lx::XML_COMMENT_NODE
                    | lx::XML_ATTRIBUTE_NODE
            );
            if tree_content {
                !fx::xmlAddChild(root, copy).is_null()
            } else {
                lx::xmlFreeNode(copy);
                false
            }
        };
        if !ok {
            lx::xmlFreeDoc(doc);
            return None;
        }
        Document::from_raw(NonNull::new(doc)?)
    };
    let root = doc.root()?;
    let child = root
        .first_child()
        .or_else(|| root.attributes().into_iter().next())?;
    child.unlink();
    root.unlink();
    drop(root);
    Some(child)
}

/// `_xmlreader_get_relaxNG`.
fn load_relaxng(source: &[u8], is_file: bool) -> *mut fx::xmlRelaxNG {
    php_libxml::raw::ensure_init();
    // SAFETY: the parser context is created, used and freed here.
    unsafe {
        let parser = if is_file {
            let Some(path) = valid_file_path(source) else {
                return std::ptr::null_mut();
            };
            let c = c_string(&path);
            fx::xmlRelaxNGNewParserCtxt(c.as_ptr())
        } else {
            let Ok(len) = c_int::try_from(source.len()) else {
                return std::ptr::null_mut();
            };
            fx::xmlRelaxNGNewMemParserCtxt(source.as_ptr().cast(), len)
        };
        if parser.is_null() {
            return std::ptr::null_mut();
        }
        let schema = fx::xmlRelaxNGParse(parser);
        fx::xmlRelaxNGFreeParserCtxt(parser);
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_path(b"/a/./b/../c//d"), b"/a/c/d");
        assert_eq!(normalize_path(b"/.."), b"/");
    }

    #[test]
    fn property_names_round_trip() {
        for p in Property::ALL {
            assert_eq!(Property::from_name(p.name()), Some(p));
        }
        assert_eq!(Property::from_name("nope"), None);
        assert_eq!(Property::NodeType.type_name(), "int");
        assert_eq!(Property::HasValue.type_name(), "bool");
        assert_eq!(Property::Value.type_name(), "string");
    }

    #[test]
    fn valid_path_for_uris() {
        assert_eq!(
            valid_file_path(b"http://example.com/x.xml").as_deref(),
            Some(&b"http://example.com/x.xml"[..])
        );
        assert_eq!(
            valid_file_path(b"file:///nonexistent/../x.xml").as_deref(),
            Some(&b"/x.xml"[..])
        );
    }
}
