// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/xmlwriter`: the `XMLWriter` class and `xmlwriter_*()` functions.
//!
//! An [`XmlWriter`] is one `XMLWriter` object's libxml2 state. Writers are
//! created in memory ([`XmlWriter::memory`], `openMemory()` /
//! `toMemory()`), over any [`Write`] sink ([`XmlWriter::to_sink`],
//! `toStream()`), or for a URI ([`XmlWriter::open_uri`], `openUri()` /
//! `toUri()`), which goes through the [`crate::io`] stream hooks when the
//! VM installed them.
//!
//! Methods return what the PHP functions return (`bool` for the writing
//! operations). Name validation failures are reported as [`NameError`],
//! whose message matches PHP's `ValueError`.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::raw::writer::{DtdEntity, Op0, Op1, Op2, TextWriter};
use crate::raw::{uri, validate_name};

/// `MAXPATHLEN` on Linux.
const MAXPATHLEN: usize = 4096;

/// An invalid name argument (`XMLW_NAME_CHK`): PHP throws a `ValueError`
/// "Argument #N ($param) must be a valid SUBJECT, "NAME" given".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    /// Argument number in the procedural form (the writer is #1).
    pub position: u32,
    /// Parameter name.
    pub param: &'static str,
    /// "element name", "attribute name" or "PI target".
    pub subject: &'static str,
    /// The rejected name (up to its first NUL byte, as PHP prints it).
    pub name: Vec<u8>,
}

impl NameError {
    /// The message after `"Argument #N ($param) "`.
    pub fn detail(&self) -> String {
        format!(
            "must be a valid {}, \"{}\" given",
            self.subject,
            String::from_utf8_lossy(&self.name)
        )
    }
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Argument #{} (${}) {}",
            self.position,
            self.param,
            self.detail()
        )
    }
}

impl std::error::Error for NameError {}

fn check(
    position: u32,
    param: &'static str,
    subject: &'static str,
    name: &[u8],
) -> Result<(), NameError> {
    if validate_name(name) {
        Ok(())
    } else {
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        Err(NameError {
            position,
            param,
            subject,
            name: name[..end].to_vec(),
        })
    }
}

/// Failure of `openUri()` / `toUri()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenUriError {
    /// `ValueError` "Argument #1 ($uri) must not be empty".
    Empty,
    /// `openUri()`: warning "Unable to resolve file path" and `false`;
    /// `toUri()`: `ValueError` "Argument #1 ($uri) must resolve to a valid
    /// file path".
    Unresolvable,
    /// libxml2 could not create the writer (`false` / `Error` "Could not
    /// construct libxml writer").
    Failed,
}

impl std::fmt::Display for OpenUriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "Argument #1 ($uri) must not be empty",
            Self::Unresolvable => "Unable to resolve file path",
            Self::Failed => "Could not construct libxml writer",
        })
    }
}

impl std::error::Error for OpenUriError {}

/// Result of `flush()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushOutput {
    /// Memory writers return the buffer contents.
    Bytes(Vec<u8>),
    /// URI/stream writers return the number of bytes written.
    Written(i64),
}

/// A `Write` sink appending to a shared buffer (the `smart_str` output).
struct MemorySink(Rc<RefCell<Vec<u8>>>);

impl Write for MemorySink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The libxml2 state of an `XMLWriter` object.
pub struct XmlWriter {
    raw: TextWriter,
    memory: Option<Rc<RefCell<Vec<u8>>>>,
}

impl std::fmt::Debug for XmlWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XmlWriter")
            .field("memory", &self.memory.is_some())
            .finish_non_exhaustive()
    }
}

/// `zend_dirname` semantics for `/`-separated paths.
fn dirname(path: &[u8]) -> Vec<u8> {
    let mut end = path.len();
    while end > 0 && path[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 {
        return if path.is_empty() {
            Vec::new()
        } else {
            b"/".to_vec()
        };
    }
    match path[..end].iter().rposition(|&b| b == b'/') {
        None => b".".to_vec(),
        Some(mut i) => {
            while i > 0 && path[i - 1] == b'/' {
                i -= 1;
            }
            if i == 0 {
                b"/".to_vec()
            } else {
                path[..i].to_vec()
            }
        }
    }
}

/// `expand_filepath`: absolute, lexically normalised path (no symlink
/// resolution); fails beyond `MAXPATHLEN`.
fn expand_filepath(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    (out.as_os_str().len() < MAXPATHLEN).then_some(out)
}

fn bytes_to_path(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

fn path_to_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().into_owned().into_bytes()
    }
}

fn starts_with_ci(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `_xmlwriter_get_valid_file_path`: resolves local paths and `file://`
/// URIs to an absolute path whose directory exists; other URIs are
/// returned unchanged. `cwd` is the script's working directory.
pub fn resolve_uri(source: &[u8], cwd: &Path) -> Option<Vec<u8>> {
    let end = source.iter().position(|&b| b == 0).unwrap_or(source.len());
    let mut src = &source[..end];
    let scheme = uri::reference_scheme(src);
    let mut is_file_uri = false;
    if scheme.is_some() {
        if starts_with_ci(src, b"file:///") {
            if src.len() == 8 {
                return None;
            }
            is_file_uri = true;
            src = &src[7..];
        } else if starts_with_ci(src, b"file://localhost/") {
            if src.len() == 17 {
                return None;
            }
            is_file_uri = true;
            src = &src[16..];
        }
    }
    if scheme.is_some() && !is_file_uri {
        return Some(src.to_vec());
    }
    let path = bytes_to_path(src);
    let full = if path.is_absolute() {
        path.clone()
    } else {
        cwd.join(&path)
    };
    let resolved = match std::fs::canonicalize(&full) {
        Ok(p) if p.as_os_str().len() < MAXPATHLEN => p,
        _ => expand_filepath(&path, cwd)?,
    };
    let dir = dirname(src);
    if !dir.is_empty() {
        let dir_path = bytes_to_path(&dir);
        let dir_full = if dir_path.is_absolute() {
            dir_path
        } else {
            cwd.join(dir_path)
        };
        std::fs::metadata(dir_full).ok()?;
    }
    Some(path_to_bytes(&resolved))
}

impl XmlWriter {
    /// `openMemory()` / `toMemory()`.
    pub fn memory() -> Option<Self> {
        let buf = Rc::new(RefCell::new(Vec::new()));
        let raw = TextWriter::to_sink(Box::new(MemorySink(buf.clone())))?;
        Some(Self {
            raw,
            memory: Some(buf),
        })
    }

    /// `toStream($stream)`: output goes to `sink`, dropped when the writer
    /// is dropped.
    pub fn to_sink(sink: Box<dyn Write>) -> Option<Self> {
        TextWriter::to_sink(sink).map(|raw| Self { raw, memory: None })
    }

    /// `openUri($uri)` / `toUri($uri)` with path resolution against `cwd`.
    pub fn open_uri(source: &[u8], cwd: &Path) -> Result<Self, OpenUriError> {
        if source.is_empty() {
            return Err(OpenUriError::Empty);
        }
        let target = resolve_uri(source, cwd).ok_or(OpenUriError::Unresolvable)?;
        Self::open_resolved(&target)
    }

    /// `xmlNewTextWriterFilename` on an already resolved target.
    pub fn open_resolved(target: &[u8]) -> Result<Self, OpenUriError> {
        TextWriter::to_uri(target)
            .map(|raw| Self { raw, memory: None })
            .ok_or(OpenUriError::Failed)
    }

    /// Whether this writer writes to memory.
    pub fn is_memory(&self) -> bool {
        self.memory.is_some()
    }

    /// `setIndent()`.
    pub fn set_indent(&self, enable: bool) -> bool {
        self.raw.set_indent(enable) == 0
    }

    /// `setIndentString()`.
    pub fn set_indent_string(&self, indentation: &[u8]) -> bool {
        self.raw.set_indent_string(indentation) != -1
    }

    /// `startComment()`.
    pub fn start_comment(&self) -> bool {
        self.raw.op0(Op0::StartComment) != -1
    }

    /// `endComment()`.
    pub fn end_comment(&self) -> bool {
        self.raw.op0(Op0::EndComment) != -1
    }

    /// `startAttribute()`.
    pub fn start_attribute(&self, name: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "attribute name", name)?;
        Ok(self.raw.op1(Op1::StartAttribute, name) != -1)
    }

    /// `endAttribute()`.
    pub fn end_attribute(&self) -> bool {
        self.raw.op0(Op0::EndAttribute) != -1
    }

    /// `writeAttribute()`.
    pub fn write_attribute(&self, name: &[u8], value: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "attribute name", name)?;
        Ok(self.raw.op2(Op2::WriteAttribute, name, value) != -1)
    }

    /// `startAttributeNs()`.
    pub fn start_attribute_ns(
        &self,
        prefix: Option<&[u8]>,
        name: &[u8],
        namespace: Option<&[u8]>,
    ) -> Result<bool, NameError> {
        check(3, "name", "attribute name", name)?;
        Ok(self.raw.start_ns(true, prefix, name, namespace) != -1)
    }

    /// `writeAttributeNs()`.
    pub fn write_attribute_ns(
        &self,
        prefix: Option<&[u8]>,
        name: &[u8],
        namespace: Option<&[u8]>,
        value: &[u8],
    ) -> Result<bool, NameError> {
        check(3, "name", "attribute name", name)?;
        Ok(self.raw.write_ns(true, prefix, name, namespace, value) != -1)
    }

    /// `startElement()`.
    pub fn start_element(&self, name: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(self.raw.op1(Op1::StartElement, name) != -1)
    }

    /// `startElementNs()`.
    pub fn start_element_ns(
        &self,
        prefix: Option<&[u8]>,
        name: &[u8],
        namespace: Option<&[u8]>,
    ) -> Result<bool, NameError> {
        check(3, "name", "element name", name)?;
        Ok(self.raw.start_ns(false, prefix, name, namespace) != -1)
    }

    /// `endElement()`.
    pub fn end_element(&self) -> bool {
        self.raw.op0(Op0::EndElement) != -1
    }

    /// `fullEndElement()`.
    pub fn full_end_element(&self) -> bool {
        self.raw.op0(Op0::FullEndElement) != -1
    }

    /// `writeElement()`: without content an empty element is written.
    pub fn write_element(&self, name: &[u8], content: Option<&[u8]>) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(match content {
            None => {
                self.raw.op1(Op1::StartElement, name) != -1 && self.raw.op0(Op0::EndElement) != -1
            }
            Some(c) => self.raw.op2(Op2::WriteElement, name, c) != -1,
        })
    }

    /// `writeElementNs()`.
    pub fn write_element_ns(
        &self,
        prefix: Option<&[u8]>,
        name: &[u8],
        namespace: Option<&[u8]>,
        content: Option<&[u8]>,
    ) -> Result<bool, NameError> {
        check(3, "name", "element name", name)?;
        Ok(match content {
            None => {
                self.raw.start_ns(false, prefix, name, namespace) != -1
                    && self.raw.op0(Op0::EndElement) != -1
            }
            Some(c) => self.raw.write_ns(false, prefix, name, namespace, c) != -1,
        })
    }

    /// `startPi()`.
    pub fn start_pi(&self, target: &[u8]) -> Result<bool, NameError> {
        check(2, "target", "PI target", target)?;
        Ok(self.raw.op1(Op1::StartPi, target) != -1)
    }

    /// `endPi()`.
    pub fn end_pi(&self) -> bool {
        self.raw.op0(Op0::EndPi) != -1
    }

    /// `writePi()`.
    pub fn write_pi(&self, target: &[u8], content: &[u8]) -> Result<bool, NameError> {
        check(2, "target", "PI target", target)?;
        Ok(self.raw.op2(Op2::WritePi, target, content) != -1)
    }

    /// `startCdata()`.
    pub fn start_cdata(&self) -> bool {
        self.raw.op0(Op0::StartCData) != -1
    }

    /// `endCdata()`.
    pub fn end_cdata(&self) -> bool {
        self.raw.op0(Op0::EndCData) != -1
    }

    /// `writeCdata()`.
    pub fn write_cdata(&self, content: &[u8]) -> bool {
        self.raw.op1(Op1::WriteCData, content) != -1
    }

    /// `writeRaw()`.
    pub fn write_raw(&self, content: &[u8]) -> bool {
        self.raw.op1(Op1::WriteRaw, content) != -1
    }

    /// `text()`.
    pub fn text(&self, content: &[u8]) -> bool {
        self.raw.op1(Op1::WriteString, content) != -1
    }

    /// `writeComment()`.
    pub fn write_comment(&self, content: &[u8]) -> bool {
        self.raw.op1(Op1::WriteComment, content) != -1
    }

    /// `startDocument($version = "1.0", $encoding = null, $standalone = null)`.
    pub fn start_document(
        &self,
        version: Option<&[u8]>,
        encoding: Option<&[u8]>,
        standalone: Option<&[u8]>,
    ) -> bool {
        self.raw.start_document(version, encoding, standalone) != -1
    }

    /// `endDocument()`.
    pub fn end_document(&self) -> bool {
        self.raw.op0(Op0::EndDocument) != -1
    }

    /// `startDtd()`.
    pub fn start_dtd(
        &self,
        qualified_name: &[u8],
        public_id: Option<&[u8]>,
        system_id: Option<&[u8]>,
    ) -> bool {
        self.raw.start_dtd(qualified_name, public_id, system_id) != -1
    }

    /// `endDtd()`.
    pub fn end_dtd(&self) -> bool {
        self.raw.op0(Op0::EndDtd) != -1
    }

    /// `writeDtd()`.
    pub fn write_dtd(
        &self,
        name: &[u8],
        public_id: Option<&[u8]>,
        system_id: Option<&[u8]>,
        content: Option<&[u8]>,
    ) -> bool {
        self.raw.write_dtd(name, public_id, system_id, content) != -1
    }

    /// `startDtdElement()`.
    pub fn start_dtd_element(&self, qualified_name: &[u8]) -> Result<bool, NameError> {
        check(2, "qualifiedName", "element name", qualified_name)?;
        Ok(self.raw.op1(Op1::StartDtdElement, qualified_name) != -1)
    }

    /// `endDtdElement()`.
    pub fn end_dtd_element(&self) -> bool {
        self.raw.op0(Op0::EndDtdElement) != -1
    }

    /// `writeDtdElement()`.
    pub fn write_dtd_element(&self, name: &[u8], content: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(self.raw.op2(Op2::WriteDtdElement, name, content) != -1)
    }

    /// `startDtdAttlist()`.
    pub fn start_dtd_attlist(&self, name: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(self.raw.op1(Op1::StartDtdAttlist, name) != -1)
    }

    /// `endDtdAttlist()`.
    pub fn end_dtd_attlist(&self) -> bool {
        self.raw.op0(Op0::EndDtdAttlist) != -1
    }

    /// `writeDtdAttlist()`.
    pub fn write_dtd_attlist(&self, name: &[u8], content: &[u8]) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(self.raw.op2(Op2::WriteDtdAttlist, name, content) != -1)
    }

    /// `startDtdEntity()` (PHP validates it as an "attribute name").
    pub fn start_dtd_entity(&self, name: &[u8], is_param: bool) -> Result<bool, NameError> {
        check(2, "name", "attribute name", name)?;
        Ok(self.raw.start_dtd_entity(is_param, name) != -1)
    }

    /// `endDtdEntity()`.
    pub fn end_dtd_entity(&self) -> bool {
        self.raw.op0(Op0::EndDtdEntity) != -1
    }

    /// `writeDtdEntity()`.
    #[allow(clippy::too_many_arguments)]
    pub fn write_dtd_entity(
        &self,
        name: &[u8],
        content: &[u8],
        is_param: bool,
        public_id: Option<&[u8]>,
        system_id: Option<&[u8]>,
        notation_data: Option<&[u8]>,
    ) -> Result<bool, NameError> {
        check(2, "name", "element name", name)?;
        Ok(self.raw.write_dtd_entity(&DtdEntity {
            parameter: is_param,
            name,
            public_id,
            system_id,
            ndata: notation_data,
            content,
        }) != -1)
    }

    /// `flush($empty = true)`.
    pub fn flush(&self, empty: bool) -> FlushOutput {
        let written = self.raw.op0(Op0::Flush);
        match &self.memory {
            Some(buf) => FlushOutput::Bytes(if empty {
                buf.take()
            } else {
                buf.borrow().clone()
            }),
            None => FlushOutput::Written(i64::from(written)),
        }
    }

    /// `outputMemory($flush = true)`: `""` for non-memory writers.
    pub fn output_memory(&self, flush: bool) -> Vec<u8> {
        if self.memory.is_none() {
            return Vec::new();
        }
        match self.flush(flush) {
            FlushOutput::Bytes(b) => b,
            FlushOutput::Written(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirname_semantics() {
        assert_eq!(dirname(b"001.xml"), b".");
        assert_eq!(dirname(b"foo/bar.tmp"), b"foo");
        assert_eq!(dirname(b"/a/b/"), b"/a");
        assert_eq!(dirname(b"/x"), b"/");
        assert_eq!(dirname(b"//"), b"/");
    }

    #[test]
    fn uri_resolution() {
        let cwd = std::env::temp_dir();
        let r = resolve_uri(b"001.xml", &cwd).expect("resolves");
        assert!(r.ends_with(b"/001.xml"));
        assert_eq!(resolve_uri(b"foo/bar.tmp", &cwd), None);
        assert_eq!(resolve_uri(b"file:///", &cwd), None);
        assert_eq!(resolve_uri(b"file://localhost/", &cwd), None);
        let long = format!("file:///{}", "a".repeat(4097));
        assert_eq!(resolve_uri(long.as_bytes(), &cwd), None);
        assert_eq!(
            resolve_uri(b"php://output", &cwd),
            Some(b"php://output".to_vec())
        );
        let tmp = format!("file://{}/x.xml", cwd.display());
        assert!(resolve_uri(tmp.as_bytes(), &cwd).is_some());
    }

    #[test]
    fn name_error_message() {
        let w = XmlWriter::memory().expect("writer");
        let e = w.start_attribute(b"-1").unwrap_err();
        assert_eq!(
            e.to_string(),
            "Argument #2 ($name) must be a valid attribute name, \"-1\" given"
        );
        assert_eq!(
            OpenUriError::Empty.to_string(),
            "Argument #1 ($uri) must not be empty"
        );
    }
}
