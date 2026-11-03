//! `DOMCharacterData`, `DOMText`, `DOMComment`, `DOMCdataSection`,
//! `DOMProcessingInstruction`, `DOMEntityReference`,
//! `DOMDocumentFragment`, `DOMAttr` and their `Dom\*` counterparts
//! (`characterdata.c`, `text.c`, `comment.c`, `cdatasection.c`,
//! `processinginstruction.c`, `entityreference.c`, `documentfragment.c`,
//! `attr.c`, `entity.c`, `notation.c`).

use std::ptr;

use crate::error::{throw, DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::object::{remove_all_children, Node};
use crate::raw::{c_len, cs, take, N};
use crate::tree;

fn utf8_strlen(s: &std::ffi::CStr) -> i32 {
    // SAFETY: valid C string.
    unsafe { ffi::xmlUTF8Strlen(s.as_ptr().cast()) }
}

fn utf8_strsub(s: &std::ffi::CStr, start: i32, len: i32) -> Option<Vec<u8>> {
    // SAFETY: valid C string; result freshly allocated or NULL.
    take(unsafe { ffi::xmlUTF8Strsub(s.as_ptr().cast(), start, len) })
}

fn utf8_strndup(s: &std::ffi::CStr, len: i32) -> Option<Vec<u8>> {
    // SAFETY: valid C string; result freshly allocated or NULL.
    take(unsafe { ffi::xmlUTF8Strndup(s.as_ptr().cast(), len) })
}

fn set_content(n: N, s: Option<&[u8]>) {
    match s {
        // SAFETY: live node.
        None => unsafe { ffi::xmlNodeSetContent(n.p(), ptr::null()) },
        Some(s) => {
            let c = cs(s);
            // SAFETY: live node.
            unsafe { ffi::xmlNodeSetContent(n.p(), c.as_ptr().cast()) };
        }
    }
}

fn add_content(n: N, s: &[u8]) {
    let c = cs(s);
    // SAFETY: live node.
    unsafe { ffi::xmlNodeAddContent(n.p(), c.as_ptr().cast()) };
}

fn int_ovfl(v: i64) -> bool {
    v > i64::from(i32::MAX)
}

impl Node {
    fn strict_err(&self) -> bool {
        self.doc_ref().is_none_or(|d| d.strict())
    }

    /// `dom_convert_number_unsigned`.
    fn convert_unsigned(&self, input: i64) -> DomResult<u32> {
        if input < 0 {
            if self.is_modern() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                return Ok(input as u32);
            }
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(input as u32)
    }

    fn content_cstr(n: N) -> std::ffi::CString {
        cs(n.content())
    }

    /// `$data` (CharacterData / ProcessingInstruction).
    pub fn data(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(tree::get_content(n).unwrap_or_default())
    }

    /// `$data = $value`.
    pub fn set_data(&self, value: &[u8]) -> DomResult<()> {
        let n = self.prop_node()?;
        // SAFETY: live node.
        unsafe { ffi::xmlNodeSetContentLen(n.p(), value.as_ptr(), c_len(value.len())) };
        Ok(())
    }

    /// `$length` (UTF-8 characters).
    pub fn length(&self) -> DomResult<i64> {
        let n = self.prop_node()?;
        if n.content_ptr().is_null() {
            return Ok(0);
        }
        // SAFETY: live node content.
        Ok(i64::from(unsafe { ffi::xmlUTF8Strlen(n.content_ptr()) }))
    }

    /// `substringData($offset, $count)`.
    pub fn substring_data(&self, offset: i64, count: i64) -> DomResult<Vec<u8>> {
        let n = self.node()?;
        let cur = Self::content_cstr(n);
        let length = utf8_strlen(&cur);
        if int_ovfl(offset) || int_ovfl(count) {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        let offset = self.convert_unsigned(offset)?;
        let mut count = self.convert_unsigned(count)?;
        #[allow(clippy::cast_sign_loss)]
        let ulen = length as u32;
        if offset > ulen {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        #[allow(clippy::cast_sign_loss)]
        let rest = (length - offset.cast_signed()) as u32;
        if count > rest {
            count = rest;
        }
        Ok(utf8_strsub(&cur, offset.cast_signed(), count.cast_signed()).unwrap_or_default())
    }

    /// `appendData($data)`.
    pub fn append_data(&self, data: &[u8]) -> DomResult<()> {
        let n = self.node()?;
        // SAFETY: live node; data valid for its length.
        unsafe { ffi::xmlTextConcat(n.p(), data.as_ptr(), c_len(data.len())) };
        Ok(())
    }

    /// `insertData($offset, $data)`.
    pub fn insert_data(&self, offset: i64, data: &[u8]) -> DomResult<()> {
        let n = self.node()?;
        let cur = Self::content_cstr(n);
        let length = utf8_strlen(&cur);
        if int_ovfl(offset) {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        let offset = self.convert_unsigned(offset)?;
        #[allow(clippy::cast_sign_loss)]
        if offset > length as u32 {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        let off = offset.cast_signed();
        let first = utf8_strndup(&cur, off);
        let second = utf8_strsub(&cur, off, length - off);
        set_content(n, first.as_deref());
        add_content(n, data);
        if let Some(s) = second {
            add_content(n, &s);
        }
        Ok(())
    }

    /// `deleteData($offset, $count)`.
    pub fn delete_data(&self, offset: i64, count: i64) -> DomResult<()> {
        let n = self.node()?;
        let cur = Self::content_cstr(n);
        let length = utf8_strlen(&cur);
        if offset < 0 || int_ovfl(offset) || int_ovfl(count) || offset > i64::from(length) {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        let mut count = self.convert_unsigned(count)?;
        #[allow(clippy::cast_possible_truncation)]
        let off = offset as i32;
        let mut substring = if off > 0 { utf8_strsub(&cur, 0, off) } else { None };
        #[allow(clippy::cast_sign_loss)]
        let rest = (length - off) as u32;
        if count > rest {
            count = rest;
        }
        let second = utf8_strsub(&cur, off.wrapping_add(count.cast_signed()), length - off);
        let mut joined = substring.take().unwrap_or_default();
        if let Some(s) = second {
            joined.extend_from_slice(&s);
        }
        set_content(n, Some(&joined));
        Ok(())
    }

    /// `replaceData($offset, $count, $data)`.
    pub fn replace_data(&self, offset: i64, count: i64, data: &[u8]) -> DomResult<()> {
        let n = self.node()?;
        let cur = Self::content_cstr(n);
        let length = utf8_strlen(&cur);
        if offset < 0 || int_ovfl(offset) || int_ovfl(count) || offset > i64::from(length) {
            return Err(throw(C::IndexSize, self.strict_err()));
        }
        let mut count = self.convert_unsigned(count)?;
        #[allow(clippy::cast_possible_truncation)]
        let off = offset as i32;
        let substring = if off > 0 { utf8_strsub(&cur, 0, off) } else { None };
        #[allow(clippy::cast_sign_loss)]
        let rest = (length - off) as u32;
        if count > rest {
            count = rest;
        }
        let second = if off < length {
            utf8_strsub(&cur, off.wrapping_add(count.cast_signed()), length - off)
        } else {
            None
        };
        let mut joined = substring.unwrap_or_default();
        joined.extend_from_slice(&data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())]);
        if let Some(s) = second {
            joined.extend_from_slice(&s);
        }
        set_content(n, Some(&joined));
        Ok(())
    }

    /// `new DOMText($data)`.
    pub fn construct_text(&self, value: Option<&[u8]>) -> DomResult<()> {
        let c = value.map(cs);
        // SAFETY: valid C string or NULL.
        let n = N::new(unsafe { ffi::xmlNewText(c.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast())) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(n);
        Ok(())
    }

    /// `new DOMComment($data)`.
    pub fn construct_comment(&self, value: Option<&[u8]>) -> DomResult<()> {
        let c = value.map(cs);
        // SAFETY: valid C string or NULL.
        let n = N::new(unsafe { ffi::xmlNewComment(c.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast())) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(n);
        Ok(())
    }

    /// `new DOMCdataSection($data)`.
    pub fn construct_cdata(&self, value: &[u8]) -> DomResult<()> {
        // SAFETY: value valid for its length.
        let n = N::new(unsafe { ffi::xmlNewCDataBlock(ptr::null_mut(), value.as_ptr(), c_len(value.len())) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(n);
        Ok(())
    }

    /// `new DOMProcessingInstruction($name, $value)`.
    pub fn construct_pi(&self, name: &[u8], value: Option<&[u8]>) -> DomResult<()> {
        if !tree::valid_name(name) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        let n = cs(name);
        let v = value.map(cs);
        // SAFETY: valid C strings.
        let node = N::new(unsafe { ffi::xmlNewPI(n.as_ptr().cast(), v.as_ref().map_or(ptr::null(), |v| v.as_ptr().cast())) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(node);
        Ok(())
    }

    /// `new DOMEntityReference($name)`.
    pub fn construct_entity_reference(&self, name: &[u8]) -> DomResult<()> {
        if !tree::valid_name(name) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        let n = cs(name);
        // SAFETY: valid C string.
        let node = N::new(unsafe { ffi::xmlNewReference(ptr::null(), n.as_ptr().cast()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(node);
        Ok(())
    }

    /// `new DOMDocumentFragment()`.
    pub fn construct_fragment(&self) -> DomResult<()> {
        // SAFETY: plain allocation.
        let node = N::new(unsafe { ffi::xmlNewDocFragment(ptr::null_mut()) }).ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(node);
        Ok(())
    }

    /// `new DOMAttr($name, $value)`.
    pub fn construct_attr(&self, name: &[u8], value: &[u8]) -> DomResult<()> {
        if !tree::valid_name(name) {
            return Err(DomError::dom(C::InvalidCharacter));
        }
        let n = cs(name);
        let v = cs(value);
        // SAFETY: valid C strings.
        let node = N::from_attr(unsafe { ffi::xmlNewProp(ptr::null_mut(), n.as_ptr().cast(), v.as_ptr().cast()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        self.bind_constructed(node);
        Ok(())
    }

    /// `$wholeText`.
    pub fn whole_text(&self) -> DomResult<Vec<u8>> {
        let mut n = self.prop_node()?;
        while let Some(p) = n.prev().filter(|p| p.is_text_like()) {
            n = p;
        }
        let mut out = Vec::new();
        let mut cur = Some(n);
        while let Some(c) = cur.filter(|c| c.is_text_like()) {
            out.extend_from_slice(c.content());
            cur = c.next();
        }
        Ok(out)
    }

    /// `splitText($offset)`; `Err(Silent)` = false.
    pub fn split_text(&self, offset: i64) -> DomResult<Node> {
        let n = self.node()?;
        if offset < 0 {
            return Err(DomError::arg_value(1, "must be greater than or equal to 0"));
        }
        let cur = Self::content_cstr(n);
        let length = utf8_strlen(&cur);
        if int_ovfl(offset) || offset > i64::from(length) {
            if self.is_modern() {
                return Err(DomError::dom(C::IndexSize));
            }
            return Err(DomError::Silent);
        }
        #[allow(clippy::cast_possible_truncation)]
        let off = offset as i32;
        let first = utf8_strndup(&cur, off);
        let second = utf8_strsub(&cur, off, length - off);
        set_content(n, None);
        // SAFETY: n's content was released by xmlNodeSetContent(NULL); we
        // install libxml2-allocated copies.
        unsafe {
            (*n.p()).content = first.as_deref().map_or(ptr::null_mut(), crate::raw::xml_strdup);
        }
        // SAFETY: live document (may be NULL).
        let nnode = N::new(unsafe { ffi::xmlNewDocText(n.doc_ptr(), ptr::null()) }).ok_or_else(|| DomError::dom(C::InvalidState))?;
        // SAFETY: fresh node.
        unsafe {
            (*nnode.p()).content = second.as_deref().map_or(ptr::null_mut(), crate::raw::xml_strdup);
        }
        if n.parent().is_some() {
            nnode.set_type(ffi::XML_ELEMENT_NODE);
            // SAFETY: live sibling; the element type prevents text merging.
            unsafe { ffi::xmlAddNextSibling(n.p(), nnode.p()) };
            nnode.set_type(ffi::XML_TEXT_NODE);
        }
        Ok(Node::wrap_with_doc(nnode, self.doc_ref()))
    }

    /// `isWhitespaceInElementContent()` / `isElementContentWhitespace()`.
    pub fn is_whitespace_in_element_content(&self) -> DomResult<bool> {
        let n = self.node()?;
        // SAFETY: live node.
        Ok(unsafe { ffi::xmlIsBlankNode(n.p()) } != 0)
    }

    /// `$target` of a processing instruction.
    pub fn target(&self) -> DomResult<Vec<u8>> {
        Ok(self.prop_node()?.name().to_vec())
    }

    /// `appendXML($data)` / `appendXml()`.
    pub fn append_xml(&self, data: &[u8]) -> DomResult<bool> {
        let nodep = self.node()?;
        if tree::is_read_only(nodep) {
            return Err(throw(C::NoModificationAllowed, self.strict_err()));
        }
        let c = cs(data);
        let mut lst: *mut ffi::xmlNode = ptr::null_mut();
        let err = crate::parse::with_generic_errors(|| {
            // SAFETY: live document (may be NULL); lst receives a new list.
            unsafe { ffi::xmlParseBalancedChunkMemory(nodep.doc_ptr(), ptr::null_mut(), ptr::null_mut(), 0, c.as_ptr().cast(), &raw mut lst) }
        });
        if err != 0 {
            if !lst.is_null() {
                // SAFETY: the partial list is owned by us.
                unsafe { ffi::xmlFreeNodeList(lst) };
            }
            return Ok(false);
        }
        // SAFETY: live fragment; lst is a fresh node list.
        unsafe { ffi::xmlAddChildList(nodep.p(), lst) };
        Ok(true)
    }

    // ---- Attr ----

    /// `$name` of an attribute.
    pub fn attr_name(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        if self.is_modern() {
            Ok(tree::qualified_name(n, false))
        } else {
            Ok(n.name().to_vec())
        }
    }

    /// `$value` of an attribute.
    pub fn attr_value(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(tree::get_content(n).unwrap_or_default())
    }

    /// `$value = $v` of an attribute.
    pub fn set_attr_value(&self, value: &[u8]) -> DomResult<()> {
        let n = self.prop_node()?;
        crate::attr::value_will_change(self, n);
        remove_all_children(n);
        if self.is_modern() {
            // SAFETY: valid string/length.
            let text = unsafe { ffi::xmlNewDocTextLen(n.doc_ptr(), value.as_ptr(), c_len(value.len())) };
            // SAFETY: live attribute.
            unsafe { ffi::xmlAddChild(n.p(), text) };
        } else {
            // SAFETY: live attribute.
            unsafe { ffi::xmlNodeSetContentLen(n.p(), value.as_ptr(), c_len(value.len())) };
        }
        Ok(())
    }

    /// `$ownerElement`.
    pub fn owner_element(&self) -> DomResult<Option<Node>> {
        Ok(self.prop_node()?.parent().map(Node::wrap))
    }

    /// `isId()`.
    pub fn is_id(&self) -> DomResult<bool> {
        Ok(self.node()?.atype() == ffi::XML_ATTRIBUTE_ID)
    }

    // ---- Entity / Notation ----

    /// `DOMEntity::$publicId`.
    pub fn entity_public_id(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        let e = n.ent();
        if e.etype != ffi::XML_EXTERNAL_GENERAL_UNPARSED_ENTITY || e.ExternalID.is_null() {
            return Ok(None);
        }
        Ok(Some(crate::raw::cstr(e.ExternalID).to_vec()))
    }

    /// `DOMEntity::$systemId`.
    pub fn entity_system_id(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        let e = n.ent();
        if e.etype != ffi::XML_EXTERNAL_GENERAL_UNPARSED_ENTITY {
            return Ok(None);
        }
        Ok(Some(crate::raw::cstr(e.SystemID).to_vec()))
    }

    /// `DOMEntity::$notationName`.
    pub fn entity_notation_name(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        let e = n.ent();
        if e.etype != ffi::XML_EXTERNAL_GENERAL_UNPARSED_ENTITY {
            return Ok(None);
        }
        Ok(Some(n.content().to_vec()))
    }

    /// `DOMDocumentType::$name`.
    pub fn doctype_name(&self) -> DomResult<Vec<u8>> {
        Ok(self.prop_node()?.name().to_vec())
    }

    /// `DOMDocumentType::$entities`.
    pub fn doctype_entities(&self) -> DomResult<crate::lists::NamedNodeMap> {
        self.prop_node()?;
        let class = if self.family_is_modern() { "Dom\\DtdNamedNodeMap" } else { "DOMNamedNodeMap" };
        Ok(crate::lists::new_map(class, Some(self.clone()), crate::lists::MapKind::Entities))
    }

    /// `DOMDocumentType::$notations`.
    pub fn doctype_notations(&self) -> DomResult<crate::lists::NamedNodeMap> {
        self.prop_node()?;
        let class = if self.family_is_modern() { "Dom\\DtdNamedNodeMap" } else { "DOMNamedNodeMap" };
        Ok(crate::lists::new_map(class, Some(self.clone()), crate::lists::MapKind::Notations))
    }

    /// `DOMDocumentType::$publicId`.
    pub fn doctype_public_id(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(crate::raw::cstr(n.dtd().ExternalID).to_vec())
    }

    /// `DOMDocumentType::$systemId`.
    pub fn doctype_system_id(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(crate::raw::cstr(n.dtd().SystemID).to_vec())
    }

    /// `DOMDocumentType::$internalSubset`.
    pub fn doctype_internal_subset(&self) -> DomResult<Option<Vec<u8>>> {
        let n = self.prop_node()?;
        let doc = n.doc_ptr();
        if doc.is_null() {
            return Ok(None);
        }
        // SAFETY: live document.
        let Some(intsubset) = N::from_dtd(unsafe { ffi::xmlGetIntSubset(doc) }) else {
            return Ok(None);
        };
        let mut out: Option<Vec<u8>> = None;
        for cur in intsubset.child_iter() {
            let s = crate::save::node_dump_output(ptr::null_mut(), cur, 0, false, None);
            out.get_or_insert_with(Vec::new).extend_from_slice(&s);
        }
        Ok(out)
    }

    /// `DOMNotation::$publicId`.
    pub fn notation_public_id(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(crate::raw::cstr(n.ent().ExternalID).to_vec())
    }

    /// `DOMNotation::$systemId`.
    pub fn notation_system_id(&self) -> DomResult<Vec<u8>> {
        let n = self.prop_node()?;
        Ok(crate::raw::cstr(n.ent().SystemID).to_vec())
    }
}

/// `DOMAttr` helpers.
pub(crate) mod attr {
    use super::{ffi, Node, N};

    /// `dom_attr_value_will_change`.
    pub(crate) fn value_will_change(obj: &Node, attr: N) {
        if attr.atype() == ffi::XML_ATTRIBUTE_ID {
            // SAFETY: live attribute.
            unsafe { ffi::xmlRemoveID(attr.doc_ptr(), attr.attr()) };
            attr.set_atype(ffi::XML_ATTRIBUTE_ID);
        }
        if let Some(d) = obj.doc_ref() {
            d.mark_modified_since_parsing();
        }
    }
}
