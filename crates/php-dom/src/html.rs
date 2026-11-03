//! `Dom\HTMLDocument` (`html_document.c`).

use crate::error::DomResult;
use crate::ffi;
use crate::object::Node;
use crate::raw::N;

/// `php_dom_create_html_doc`.
pub(crate) fn create_html_doc() -> Option<N> {
    // SAFETY: fresh document with its own dictionary.
    unsafe {
        let d = ffi::htmlNewDocNoDtD(std::ptr::null(), std::ptr::null());
        let n = N::from_doc(d)?;
        n.d().dict = ffi::xmlDictCreate();
        Some(n)
    }
}

/// `dom_html_document_encoding_write`.
pub(crate) fn set_document_encoding(_this: &Node, _value: &[u8]) -> DomResult<()> {
    Ok(())
}

use crate::error::DomError;

fn todo<T>() -> DomResult<T> {
    Err(DomError::Error("Dom\\HTMLDocument is not available yet".into()))
}

/// `Dom\HTMLDocument::createEmpty()`.
pub(crate) fn create_empty(_enc: &[u8]) -> DomResult<Node> {
    todo()
}
/// `Dom\HTMLDocument::createFromString()` / `createFromFile()`.
pub(crate) fn create_from(_src: &[u8], _opts: i64, _enc: Option<&[u8]>, _file: bool) -> DomResult<Node> {
    todo()
}
/// `saveXmlFile()`.
pub(crate) fn save_xml_file(_n: &Node, _f: &[u8], _o: i64) -> DomResult<Option<i64>> {
    todo()
}
/// `Dom\HTMLDocument::saveHtml()`.
pub(crate) fn save_html(_n: &Node, _node: Option<&Node>) -> DomResult<Vec<u8>> {
    todo()
}
/// `Dom\HTMLDocument::saveHtmlFile()`.
pub(crate) fn save_html_file(_n: &Node, _f: &[u8]) -> DomResult<Option<i64>> {
    todo()
}
/// `debugGetTemplateCount()`.
pub(crate) fn template_count(_n: &Node) -> i64 {
    0
}
/// `$body`.
pub(crate) fn body(_n: &Node) -> DomResult<Option<Node>> {
    Ok(None)
}
/// `$head`.
pub(crate) fn head(_n: &Node) -> DomResult<Option<Node>> {
    Ok(None)
}
/// `$title`.
pub(crate) fn title(_n: &Node) -> DomResult<Vec<u8>> {
    Ok(Vec::new())
}
/// `$body = $v`.
pub(crate) fn set_body(_n: &Node, _v: Option<&Node>) -> DomResult<()> {
    todo()
}
/// `$title = $v`.
pub(crate) fn set_title(_n: &Node, _v: &[u8]) -> DomResult<()> {
    todo()
}
