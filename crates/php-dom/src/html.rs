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
