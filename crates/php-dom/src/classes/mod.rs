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
        ffi::XML_DTD_NODE | ffi::XML_DOCUMENT_TYPE_NODE => {
            pick("DOMDocumentType", "Dom\\DocumentType")
        }
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
        if let Some(map) = doc
            .props
            .borrow()
            .as_ref()
            .and_then(|p| p.classmap.as_ref())
        {
            if let Some((_, ext)) = map.iter().find(|(b, _)| b.eq_ignore_ascii_case(base)) {
                return Cow::Owned(ext.clone());
            }
        }
    }
    Cow::Borrowed(base)
}

mod table;

/// Kind of a class-like declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// `class`.
    Class,
    /// `interface`.
    Interface,
    /// `enum`.
    Enum,
}

/// A class constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstInfo {
    /// Name.
    pub name: &'static str,
    /// Declared type.
    pub ty: &'static str,
    /// Value.
    pub value: i64,
}

/// A declared property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropInfo {
    /// Name (without `$`).
    pub name: &'static str,
    /// Declared type (PHP syntax), if any.
    pub ty: Option<&'static str>,
    /// `public` / `protected` / `private`.
    pub visibility: &'static str,
    /// Asymmetric set visibility (`private` for `private(set)`).
    pub set_visibility: Option<&'static str>,
    /// `readonly`.
    pub readonly: bool,
    /// Backed by a DOM property handler (no storage slot).
    pub is_virtual: bool,
    /// Default value (PHP source), for non-virtual properties.
    pub default: Option<&'static str>,
}

/// A method parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamInfo {
    /// Name (without `$`).
    pub name: &'static str,
    /// Declared type (PHP syntax), if any.
    pub ty: Option<&'static str>,
    /// Default value (PHP source), if optional.
    pub default: Option<&'static str>,
    /// `...$name`.
    pub variadic: bool,
    /// `&$name`.
    pub by_ref: bool,
}

/// A method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct MethodInfo {
    /// Name.
    pub name: &'static str,
    /// `public` / `private`.
    pub visibility: &'static str,
    /// `static`.
    pub is_static: bool,
    /// `final`.
    pub is_final: bool,
    /// Interface method (abstract).
    pub is_abstract: bool,
    /// Parameters.
    pub params: &'static [ParamInfo],
    /// Declared return type.
    pub return_type: Option<&'static str>,
    /// `@return` from the stub docblock (for untyped methods).
    pub doc_return: Option<&'static str>,
    /// The return type is tentative.
    pub tentative_return: bool,
}

impl MethodInfo {
    /// Number of required parameters.
    pub fn required_args(&self) -> usize {
        self.params
            .iter()
            .take_while(|p| p.default.is_none() && !p.variadic)
            .count()
    }
}

/// A class, interface or enum of the DOM extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClassInfo {
    /// Fully qualified name.
    pub name: &'static str,
    /// Kind.
    pub kind: ClassKind,
    /// `abstract class`.
    pub is_abstract: bool,
    /// `final class`.
    pub is_final: bool,
    /// `readonly class`.
    pub is_readonly: bool,
    /// `@not-serializable`.
    pub not_serializable: bool,
    /// Parent class.
    pub parent: Option<&'static str>,
    /// Implemented (or, for interfaces, extended) interfaces.
    pub interfaces: &'static [&'static str],
    /// Backing type of an enum.
    pub enum_backing: Option<&'static str>,
    /// Enum cases `(name, value)`.
    pub enum_cases: &'static [(&'static str, &'static str)],
    /// Constants.
    pub constants: &'static [ConstInfo],
    /// Declared properties.
    pub properties: &'static [PropInfo],
    /// Methods.
    pub methods: &'static [MethodInfo],
}

/// Every class, interface and enum the DOM extension registers.
pub fn class_table() -> &'static [ClassInfo] {
    table::CLASSES
}

/// Looks a class up by (case-insensitive) name.
pub fn find_class(name: &str) -> Option<&'static ClassInfo> {
    let name = name.strip_prefix('\\').unwrap_or(name);
    table::CLASSES
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
}

/// Finds a method on a class or its ancestors.
pub fn find_method(class: &str, method: &str) -> Option<(&'static ClassInfo, &'static MethodInfo)> {
    let mut cur = find_class(class);
    while let Some(c) = cur {
        if let Some(m) = c
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(method))
        {
            return Some((c, m));
        }
        cur = c.parent.and_then(find_class);
    }
    // Interface default declarations (e.g. DOMChildNode::remove).
    let c = find_class(class)?;
    for i in c.interfaces {
        if let Some(r) = find_method(i, method) {
            return Some(r);
        }
    }
    None
}

/// Whether `class` is `base` or derives from / implements it.
pub fn is_subclass(class: &str, base: &str) -> bool {
    let Some(c) = find_class(class) else {
        return false;
    };
    if c.name
        .eq_ignore_ascii_case(base.strip_prefix('\\').unwrap_or(base))
    {
        return true;
    }
    if c.parent.is_some_and(|p| is_subclass(p, base)) {
        return true;
    }
    c.interfaces.iter().any(|i| is_subclass(i, base))
}

/// The property-handler set of a built-in class, in the order PHP
/// registers them (this is the `var_dump()` order).
#[allow(clippy::match_same_arms)]
pub fn property_handlers(class: &str) -> &'static [&'static str] {
    let c = find_class(class).map_or(class, |c| c.name);
    match c {
        "DOMNode" => NODE,
        "Dom\\Node" => MODERN_NODE,
        "DOMNameSpaceNode" => NAMESPACE_NODE,
        "DOMDocumentFragment" => DOCUMENTFRAGMENT,
        "Dom\\DocumentFragment" => MODERN_DOCUMENTFRAGMENT,
        "Dom\\Document" | "Dom\\HTMLDocument" => ABSTRACT_BASE_DOCUMENT,
        "DOMDocument" => DOCUMENT,
        "Dom\\XMLDocument" => XML_DOCUMENT,
        "DOMNodeList"
        | "Dom\\NodeList"
        | "Dom\\HTMLCollection"
        | "DOMNamedNodeMap"
        | "Dom\\NamedNodeMap"
        | "Dom\\DtdNamedNodeMap" => LENGTH_ONLY,
        "DOMCharacterData" | "DOMComment" => CHARACTERDATA,
        "Dom\\CharacterData" | "Dom\\Comment" => MODERN_CHARACTERDATA,
        "DOMAttr" => ATTR,
        "Dom\\Attr" => MODERN_ATTR,
        "DOMElement" => ELEMENT,
        "Dom\\Element" | "Dom\\HTMLElement" => MODERN_ELEMENT,
        "DOMText" | "DOMCdataSection" => TEXT,
        "Dom\\Text" | "Dom\\CDATASection" => MODERN_TEXT,
        "DOMDocumentType" => DOCUMENTTYPE,
        "Dom\\DocumentType" => MODERN_DOCUMENTTYPE,
        "DOMNotation" => NOTATION,
        "Dom\\Notation" => MODERN_NODE,
        "DOMEntity" => ENTITY,
        "Dom\\Entity" => MODERN_ENTITY,
        "DOMEntityReference" => NODE,
        "Dom\\EntityReference" => MODERN_NODE,
        "DOMProcessingInstruction" => PROCESSINGINSTRUCTION,
        "Dom\\ProcessingInstruction" => MODERN_PROCESSINGINSTRUCTION,
        "DOMXPath" | "Dom\\XPath" => XPATH,
        "Dom\\TokenList" => TOKEN_LIST,
        _ => &[],
    }
}

macro_rules! cat {
    ($($a:expr),* $(,)?) => {{
        const fn len() -> usize { 0 $(+ $a.len())* }
        const N: usize = len();
        const fn build() -> [&'static str; N] {
            let mut out = [""; N];
            let mut i = 0;
            $(
                let a: &[&str] = $a;
                let mut j = 0;
                while j < a.len() {
                    out[i] = a[j];
                    i += 1;
                    j += 1;
                }
            )*
            out
        }
        const OUT: [&'static str; N] = build();
        &OUT
    }};
}

const NODE: &[&str] = &[
    "nodeName",
    "nodeValue",
    "nodeType",
    "parentNode",
    "parentElement",
    "childNodes",
    "firstChild",
    "lastChild",
    "previousSibling",
    "nextSibling",
    "attributes",
    "isConnected",
    "ownerDocument",
    "namespaceURI",
    "prefix",
    "localName",
    "baseURI",
    "textContent",
];
const MODERN_NODE: &[&str] = &[
    "nodeType",
    "nodeName",
    "baseURI",
    "isConnected",
    "ownerDocument",
    "parentNode",
    "parentElement",
    "childNodes",
    "firstChild",
    "lastChild",
    "previousSibling",
    "nextSibling",
    "nodeValue",
    "textContent",
];
const NAMESPACE_NODE: &[&str] = &[
    "nodeName",
    "nodeValue",
    "nodeType",
    "prefix",
    "localName",
    "namespaceURI",
    "isConnected",
    "ownerDocument",
    "parentNode",
    "parentElement",
];
const PARENT3: &[&str] = &["firstElementChild", "lastElementChild", "childElementCount"];
const DOCUMENTFRAGMENT: &[&str] = cat!(PARENT3, NODE);
const MODERN_DOCUMENTFRAGMENT: &[&str] = cat!(&["children"], PARENT3, MODERN_NODE);
const ABSTRACT_BASE_DOCUMENT: &[&str] = cat!(
    &[
        "implementation",
        "URL",
        "documentURI",
        "characterSet",
        "charset",
        "inputEncoding",
        "doctype",
        "documentElement",
        "children"
    ],
    PARENT3,
    &["body", "head", "title"],
    MODERN_NODE
);
const DOCUMENT: &[&str] = cat!(
    &[
        "doctype",
        "implementation",
        "documentElement",
        "actualEncoding",
        "encoding",
        "xmlEncoding",
        "standalone",
        "xmlStandalone",
        "version",
        "xmlVersion",
        "strictErrorChecking",
        "documentURI",
        "config",
        "formatOutput",
        "validateOnParse",
        "resolveExternals",
        "preserveWhiteSpace",
        "recover",
        "substituteEntities",
    ],
    PARENT3,
    NODE
);
const XML_DOCUMENT: &[&str] = cat!(
    &["xmlEncoding", "xmlStandalone", "xmlVersion", "formatOutput"],
    ABSTRACT_BASE_DOCUMENT
);
const LENGTH_ONLY: &[&str] = &["length"];
const CHARDATA4: &[&str] = &[
    "data",
    "length",
    "previousElementSibling",
    "nextElementSibling",
];
const CHARACTERDATA: &[&str] = cat!(CHARDATA4, NODE);
const MODERN_CHARACTERDATA: &[&str] = cat!(CHARDATA4, MODERN_NODE);
const ATTR: &[&str] = cat!(
    &[
        "name",
        "specified",
        "value",
        "ownerElement",
        "schemaTypeInfo"
    ],
    NODE
);
const MODERN_ATTR: &[&str] = cat!(
    &[
        "namespaceURI",
        "prefix",
        "localName",
        "name",
        "value",
        "ownerElement",
        "specified"
    ],
    MODERN_NODE
);
const ELEMENT: &[&str] = cat!(
    &["tagName", "className", "id", "schemaTypeInfo"],
    PARENT3,
    &["previousElementSibling", "nextElementSibling"],
    NODE
);
const MODERN_ELEMENT: &[&str] = cat!(
    &[
        "namespaceURI",
        "prefix",
        "localName",
        "tagName",
        "id",
        "className",
        "classList",
        "attributes",
        "children"
    ],
    PARENT3,
    &[
        "previousElementSibling",
        "nextElementSibling",
        "innerHTML",
        "outerHTML",
        "substitutedNodeValue"
    ],
    MODERN_NODE
);
const TEXT: &[&str] = cat!(&["wholeText"], CHARACTERDATA);
const MODERN_TEXT: &[&str] = cat!(&["wholeText"], MODERN_CHARACTERDATA);
const DOCTYPE6: &[&str] = &[
    "name",
    "entities",
    "notations",
    "publicId",
    "systemId",
    "internalSubset",
];
const DOCUMENTTYPE: &[&str] = cat!(DOCTYPE6, NODE);
const MODERN_DOCUMENTTYPE: &[&str] = cat!(DOCTYPE6, MODERN_NODE);
const NOTATION: &[&str] = cat!(&["publicId", "systemId"], NODE);
const ENTITY: &[&str] = cat!(
    &[
        "publicId",
        "systemId",
        "notationName",
        "actualEncoding",
        "encoding",
        "version"
    ],
    NODE
);
const MODERN_ENTITY: &[&str] = cat!(&["publicId", "systemId", "notationName"], MODERN_NODE);
const PROCESSINGINSTRUCTION: &[&str] = cat!(&["target", "data"], NODE);
const MODERN_PROCESSINGINSTRUCTION: &[&str] = cat!(&["target"], MODERN_CHARACTERDATA);
const XPATH: &[&str] = &["document", "registerNodeNamespaces"];
const TOKEN_LIST: &[&str] = &["length", "value"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_lists_have_no_duplicates() {
        for c in class_table() {
            let h = property_handlers(c.name);
            let mut seen = std::collections::HashSet::new();
            for p in h {
                assert!(seen.insert(*p), "{} duplicates {p}", c.name);
            }
        }
    }

    #[test]
    fn table_shape() {
        assert_eq!(class_table().len(), 50);
        let e = find_class("domelement").expect("DOMElement");
        assert_eq!(e.parent, Some("DOMNode"));
        assert!(is_subclass("DOMCdataSection", "DOMCharacterData"));
        assert!(is_subclass("Dom\\HTMLElement", "Dom\\ParentNode"));
        let (_, m) = find_method("DOMElement", "setAttributeNS").expect("method");
        assert_eq!(m.params.len(), 3);
        assert_eq!(property_handlers("DOMText")[0], "wholeText");
        assert_eq!(property_handlers("DOMElement").len(), 27);
        assert_eq!(property_handlers("DOMText").len(), 23);
    }
}
