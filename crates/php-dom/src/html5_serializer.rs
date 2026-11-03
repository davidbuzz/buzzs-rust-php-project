//! The HTML5 fragment serialization algorithm (`html5_serializer.c`).
//!
//! <https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments>

use crate::ffi;
use crate::ns::{
    ns_is_fast, PrivateData, HTML_TOKEN, MATHML_TOKEN, SVG_TOKEN, XLINK_TOKEN, XMLNS_TOKEN,
    XML_TOKEN,
};
use crate::raw::N;

/// Receives serialized pieces; `false` aborts the serialization.
pub(crate) type Sink<'a> = Box<dyn FnMut(&[u8]) -> bool + 'a>;

/// Output sink (always UTF-8; encoding conversion happens afterwards).
pub(crate) struct Html5Serializer<'a> {
    out: Vec<u8>,
    private: Option<&'a PrivateData>,
    /// Streaming mode: every write goes straight to the sink (like PHP's
    /// `write_string` callbacks); `false` from the sink aborts.
    sink: Option<Sink<'a>>,
    failed: bool,
}

const RAW_TEXT_PARENTS: &[&[u8]] = &[
    b"style",
    b"script",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
    b"plaintext",
];

const VOID: &[&[u8]] = &[
    b"area",
    b"base",
    b"br",
    b"col",
    b"embed",
    b"hr",
    b"img",
    b"input",
    b"link",
    b"meta",
    b"source",
    b"track",
    b"wbr",
    b"basefont",
    b"bgsound",
    b"frame",
    b"keygen",
    b"param",
];

/// `dom_html5_serializes_as_void`.
fn serializes_as_void(node: N) -> bool {
    ns_is_fast(node, &HTML_TOKEN) && VOID.contains(&node.name())
}

fn templated_content(private: Option<&PrivateData>, node: N) -> Option<N> {
    private.and_then(|p| p.retrieve_template(node))
}

impl<'a> Html5Serializer<'a> {
    pub(crate) fn new(private: Option<&'a PrivateData>) -> Self {
        Html5Serializer {
            out: Vec::new(),
            private,
            sink: None,
            failed: false,
        }
    }

    /// A serializer that hands every piece to `sink` as it is produced.
    pub(crate) fn streaming(
        private: Option<&'a PrivateData>,
        sink: Sink<'a>,
    ) -> Self {
        Html5Serializer {
            out: Vec::new(),
            private,
            sink: Some(sink),
            failed: false,
        }
    }

    /// Whether a streaming write failed.
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.out
    }

    fn w(&mut self, b: &[u8]) {
        if self.failed || b.is_empty() {
            return;
        }
        match &mut self.sink {
            Some(sink) => {
                if !sink(b) {
                    self.failed = true;
                }
            }
            None => self.out.extend_from_slice(b),
        }
    }

    /// <https://html.spec.whatwg.org/multipage/parsing.html#escapingString>.
    fn escape(&mut self, content: &[u8], attribute_mode: bool) {
        let mut last = 0;
        let mut i = 0;
        while i < content.len() {
            let rep: Option<&[u8]> = match content[i] {
                b'&' => Some(b"&amp;"),
                0xC2 if content.get(i + 1) == Some(&0xA0) => Some(b"&nbsp;"),
                b'"' if attribute_mode => Some(b"&quot;"),
                b'<' if !attribute_mode => Some(b"&lt;"),
                b'>' if !attribute_mode => Some(b"&gt;"),
                _ => None,
            };
            if let Some(r) = rep {
                self.w(&content[last..i]);
                self.w(r);
                if content[i] == 0xC2 {
                    i += 1;
                }
                last = i + 1;
            }
            i += 1;
        }
        self.w(&content[last..]);
    }

    fn text_node(&mut self, node: N) {
        if node.content_ptr().is_null() {
            return;
        }
        if let Some(parent) = node.parent() {
            if parent.t() == ffi::XML_ELEMENT_NODE
                && ns_is_fast(parent, &HTML_TOKEN)
                && RAW_TEXT_PARENTS.contains(&parent.name())
            {
                self.w(node.content());
                return;
            }
        }
        self.escape(node.content(), false);
    }

    fn tag_name(&mut self, node: N) {
        if let Some(p) = node.ns().and_then(crate::raw::Ns::prefix) {
            if !(ns_is_fast(node, &HTML_TOKEN)
                || ns_is_fast(node, &MATHML_TOKEN)
                || ns_is_fast(node, &SVG_TOKEN))
            {
                self.w(p);
                self.w(b":");
            }
        }
        self.w(node.name());
    }

    fn element_start(&mut self, node: N) {
        self.w(b"<");
        self.tag_name(node);
        for attr in node.attr_iter() {
            self.w(b" ");
            match attr.ns() {
                None => self.w(attr.name()),
                Some(ns) => {
                    if ns_is_fast(attr, &XML_TOKEN) {
                        self.w(b"xml:");
                        self.w(attr.name());
                    } else if ns_is_fast(attr, &XMLNS_TOKEN) {
                        if attr.name() == b"xmlns" {
                            self.w(b"xmlns");
                        } else {
                            self.w(b"xmlns:");
                            self.w(attr.name());
                        }
                    } else if ns_is_fast(attr, &XLINK_TOKEN) {
                        self.w(b"xlink:");
                        self.w(attr.name());
                    } else if let Some(p) = ns.prefix() {
                        self.w(p);
                        self.w(b":");
                        self.w(attr.name());
                    } else {
                        self.w(attr.name());
                    }
                }
            }
            self.w(b"=\"");
            for child in attr.child_iter() {
                if child.t() == ffi::XML_TEXT_NODE {
                    if !child.content_ptr().is_null() {
                        self.escape(child.content(), true);
                    }
                } else if child.t() == ffi::XML_ENTITY_REF_NODE {
                    self.w(b"&");
                    self.escape(child.name(), true);
                    self.w(b";");
                }
            }
            self.w(b"\"");
        }
        self.w(b">");
    }

    fn element_end(&mut self, node: N) {
        if !serializes_as_void(node) {
            self.w(b"</");
            self.tag_name(node);
            self.w(b">");
        }
    }

    /// `dom_html5_serialize_node`: iterative walk from `node` up to `bound`.
    fn serialize_node(&mut self, start: Option<N>, bound: Option<N>) {
        let mut cur = start;
        while let Some(node) = cur {
            if self.failed {
                return;
            }
            match node.t() {
                ffi::XML_DTD_NODE => {
                    self.w(b"<!DOCTYPE ");
                    self.w(node.name());
                    self.w(b">");
                }
                ffi::XML_CDATA_SECTION_NODE | ffi::XML_TEXT_NODE => self.text_node(node),
                ffi::XML_PI_NODE => {
                    self.w(b"<?");
                    self.w(node.name());
                    self.w(b" ");
                    self.w(node.content());
                    self.w(b">");
                }
                ffi::XML_COMMENT_NODE => {
                    self.w(b"<!--");
                    self.w(node.content());
                    self.w(b"-->");
                }
                ffi::XML_ELEMENT_NODE => {
                    self.element_start(node);
                    let mut children = node.children();
                    if ns_is_fast(node, &HTML_TOKEN) && node.name() == b"template" {
                        children = templated_content(self.private, node);
                    }
                    if let Some(c) = children {
                        if !serializes_as_void(node) {
                            cur = Some(c);
                            continue;
                        }
                    } else {
                        self.element_end(node);
                    }
                }
                ffi::XML_DOCUMENT_FRAG_NODE => {
                    if let Some(c) = node.children() {
                        cur = Some(c);
                        continue;
                    }
                }
                ffi::XML_ENTITY_REF_NODE => {
                    self.w(b"&");
                    self.w(node.name());
                    self.w(b";");
                }
                _ => {}
            }

            if let Some(next) = node.next() {
                cur = Some(next);
            } else {
                let mut n = node;
                loop {
                    let Some(parent) = n.parent() else { return };
                    n = parent;
                    if Some(n) == bound {
                        return;
                    }
                    if n.t() == ffi::XML_ELEMENT_NODE {
                        self.element_end(n);
                    }
                    if n.next().is_some() {
                        break;
                    }
                }
                cur = n.next();
            }
        }
    }

    /// `dom_html5_serialize`: the children of `node`.
    pub(crate) fn serialize(&mut self, node: N) {
        if !matches!(
            node.t(),
            ffi::XML_ELEMENT_NODE
                | ffi::XML_DOCUMENT_FRAG_NODE
                | ffi::XML_DOCUMENT_NODE
                | ffi::XML_HTML_DOCUMENT_NODE
        ) {
            return;
        }
        if node.t() == ffi::XML_ELEMENT_NODE && serializes_as_void(node) {
            return;
        }
        let children = templated_content(self.private, node).or_else(|| node.children());
        self.serialize_node(children, Some(node));
    }

    /// `dom_html5_serialize_outer`.
    pub(crate) fn serialize_outer(&mut self, node: N) {
        if matches!(
            node.t(),
            ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE | ffi::XML_DOCUMENT_FRAG_NODE
        ) {
            let Some(first) = node.children() else { return };
            self.serialize_node(Some(first), first.parent());
        } else {
            let old_next = node.next();
            node.set_next(None);
            self.serialize_node(Some(node), node.parent());
            node.set_next(old_next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        let mut s = Html5Serializer::new(None);
        s.escape("a&b<c>\"d\u{a0}e".as_bytes(), false);
        assert_eq!(s.out, b"a&amp;b&lt;c&gt;\"d&nbsp;e");
        let mut s = Html5Serializer::new(None);
        s.escape("a&b<c>\"d\u{a0}".as_bytes(), true);
        assert_eq!(s.out, b"a&amp;b<c>&quot;d&nbsp;");
        let mut s = Html5Serializer::new(None);
        s.escape(b"\xC2x\xC2", false);
        assert_eq!(s.out, b"\xC2x\xC2");
    }
}
