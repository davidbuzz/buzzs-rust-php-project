//! Mapping from libxml2 node types to PHP classes
//! (`php_dom_create_object`, `dom_get_*_ce`).

use std::borrow::Cow;

use crate::ffi;
use crate::ns;
use crate::object::DocRef;
use crate::raw::N;

/// The built-in class for a node (`php_dom_create_object`'s switch).
/// Returns `None` for unsupported node types ("Unsupported node type").
pub(crate) fn builtin_class(n: N, modern: bool) -> Option<&'static str> {
    let pick = |legacy: &'static str, m: &'static str| Some(if modern { m } else { legacy });
    match n.t() {
        ffi::XML_DOCUMENT_NODE => pick("DOMDocument", "Dom\\XMLDocument"),
        ffi::XML_HTML_DOCUMENT_NODE => pick("DOMDocument", "Dom\\HTMLDocument"),
        ffi::XML_DTD_NODE | ffi::XML_DOCUMENT_TYPE_NODE => pick("DOMDocumentType", "Dom\\DocumentType"),
        ffi::XML_ELEMENT_NODE => {
            if modern {
                if ns::ns_is_fast(n, &ns::HTML_TOKEN) {
                    Some("Dom\\HTMLElement")
                } else {
                    Some("Dom\\Element")
                }
            } else {
                Some("DOMElement")
            }
        }
        ffi::XML_ATTRIBUTE_NODE => pick("DOMAttr", "Dom\\Attr"),
        ffi::XML_TEXT_NODE => pick("DOMText", "Dom\\Text"),
        ffi::XML_COMMENT_NODE => pick("DOMComment", "Dom\\Comment"),
        ffi::XML_PI_NODE => pick("DOMProcessingInstruction", "Dom\\ProcessingInstruction"),
        ffi::XML_ENTITY_REF_NODE => pick("DOMEntityReference", "Dom\\EntityReference"),
        ffi::XML_ENTITY_DECL | ffi::XML_ELEMENT_DECL => pick("DOMEntity", "Dom\\Entity"),
        ffi::XML_CDATA_SECTION_NODE => pick("DOMCdataSection", "Dom\\CDATASection"),
        ffi::XML_DOCUMENT_FRAG_NODE => pick("DOMDocumentFragment", "Dom\\DocumentFragment"),
        ffi::XML_NOTATION_NODE => pick("DOMNotation", "Dom\\Notation"),
        ffi::XML_NAMESPACE_DECL => Some("DOMNameSpaceNode"),
        _ => None,
    }
}

/// The class of a new object for `n`, honouring `registerNodeClass`.
pub(crate) fn class_for_node(n: N, modern: bool, doc: Option<&DocRef>) -> Cow<'static, str> {
    let Some(base) = builtin_class(n, modern) else {
        return Cow::Borrowed("DOMNode");
    };
    if let Some(doc) = doc {
        if let Some(map) = doc.props.borrow().as_ref().and_then(|p| p.classmap.as_ref()) {
            if let Some((_, ext)) = map.iter().find(|(b, _)| b.eq_ignore_ascii_case(base)) {
                return Cow::Owned(ext.clone());
            }
        }
    }
    Cow::Borrowed(base)
}
