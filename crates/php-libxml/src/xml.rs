// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/xml`: the expat-compatible SAX API (`xml_parser_create()`,
//! `xml_parse()`, `xml_parse_into_struct()`, …) on libxml2.
//!
//! An [`XmlParser`] corresponds to an `XMLParser` object. User callbacks
//! are delivered through the [`XmlHandlers`] trait, which the VM implements
//! over the PHP callables registered with `xml_set_*_handler()`; which
//! callables exist is recorded with [`XmlParser::set_handler`] (the
//! registration matters even for `null` callables, since it suppresses the
//! default-handler fallback exactly as in PHP).
//!
//! All methods take `&self` so that callbacks may query the parser
//! (`xml_get_current_line_number()`, option getters/setters) while a parse
//! is running; re-entrant parsing is rejected with
//! [`XmlError::Recursive`].
//!
//! `xml_parse_into_struct()` builds its `$values`/`$index` arrays through
//! the [`StructSink`] trait so the VM can operate on the live PHP arrays
//! (user callbacks may modify them mid-parse); [`StructBuilder`] is a
//! self-contained implementation producing [`StructEntry`] values.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::raw::sax::{slot, ExpatSink, PushParser};

/// `XML_MAXLEVEL`: deepest level recorded by `xml_parse_into_struct()`.
pub const XML_MAXLEVEL: i32 = 255;

/// Message of the warning raised when [`XML_MAXLEVEL`] is exceeded.
pub const MAX_DEPTH_WARNING: &str = "Maximum depth exceeded - Results truncated";

/// `XML_SAX_IMPL`.
pub const XML_SAX_IMPL: &str = "libxml";

/// `XML_ERROR_*` constants (expat's `enum XML_Error`).
pub mod error_code {
    #![allow(missing_docs)]
    pub const NONE: i64 = 0;
    pub const NO_MEMORY: i64 = 1;
    pub const SYNTAX: i64 = 2;
    pub const NO_ELEMENTS: i64 = 3;
    pub const INVALID_TOKEN: i64 = 4;
    pub const UNCLOSED_TOKEN: i64 = 5;
    pub const PARTIAL_CHAR: i64 = 6;
    pub const TAG_MISMATCH: i64 = 7;
    pub const DUPLICATE_ATTRIBUTE: i64 = 8;
    pub const JUNK_AFTER_DOC_ELEMENT: i64 = 9;
    pub const PARAM_ENTITY_REF: i64 = 10;
    pub const UNDEFINED_ENTITY: i64 = 11;
    pub const RECURSIVE_ENTITY_REF: i64 = 12;
    pub const ASYNC_ENTITY: i64 = 13;
    pub const BAD_CHAR_REF: i64 = 14;
    pub const BINARY_ENTITY_REF: i64 = 15;
    pub const ATTRIBUTE_EXTERNAL_ENTITY_REF: i64 = 16;
    pub const MISPLACED_XML_PI: i64 = 17;
    pub const UNKNOWN_ENCODING: i64 = 18;
    pub const INCORRECT_ENCODING: i64 = 19;
    pub const UNCLOSED_CDATA_SECTION: i64 = 20;
    pub const EXTERNAL_ENTITY_HANDLING: i64 = 21;
}

/// The `XML_ERROR_*` constants as `(name, value)` pairs.
pub const ERROR_CONSTANTS: [(&str, i64); 22] = [
    ("XML_ERROR_NONE", 0),
    ("XML_ERROR_NO_MEMORY", 1),
    ("XML_ERROR_SYNTAX", 2),
    ("XML_ERROR_NO_ELEMENTS", 3),
    ("XML_ERROR_INVALID_TOKEN", 4),
    ("XML_ERROR_UNCLOSED_TOKEN", 5),
    ("XML_ERROR_PARTIAL_CHAR", 6),
    ("XML_ERROR_TAG_MISMATCH", 7),
    ("XML_ERROR_DUPLICATE_ATTRIBUTE", 8),
    ("XML_ERROR_JUNK_AFTER_DOC_ELEMENT", 9),
    ("XML_ERROR_PARAM_ENTITY_REF", 10),
    ("XML_ERROR_UNDEFINED_ENTITY", 11),
    ("XML_ERROR_RECURSIVE_ENTITY_REF", 12),
    ("XML_ERROR_ASYNC_ENTITY", 13),
    ("XML_ERROR_BAD_CHAR_REF", 14),
    ("XML_ERROR_BINARY_ENTITY_REF", 15),
    ("XML_ERROR_ATTRIBUTE_EXTERNAL_ENTITY_REF", 16),
    ("XML_ERROR_MISPLACED_XML_PI", 17),
    ("XML_ERROR_UNKNOWN_ENCODING", 18),
    ("XML_ERROR_INCORRECT_ENCODING", 19),
    ("XML_ERROR_UNCLOSED_CDATA_SECTION", 20),
    ("XML_ERROR_EXTERNAL_ENTITY_HANDLING", 21),
];

/// `XML_OPTION_*` values (`enum php_xml_option`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlOption {
    /// `XML_OPTION_CASE_FOLDING` (1).
    CaseFolding = 1,
    /// `XML_OPTION_TARGET_ENCODING` (2).
    TargetEncoding = 2,
    /// `XML_OPTION_SKIP_TAGSTART` (3).
    SkipTagstart = 3,
    /// `XML_OPTION_SKIP_WHITE` (4).
    SkipWhite = 4,
    /// `XML_OPTION_PARSE_HUGE` (5).
    ParseHuge = 5,
}

impl XmlOption {
    /// Maps an `XML_OPTION_*` integer; `None` for unknown values (PHP
    /// throws "must be a `XML_OPTION_*` constant").
    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            1 => Self::CaseFolding,
            2 => Self::TargetEncoding,
            3 => Self::SkipTagstart,
            4 => Self::SkipWhite,
            5 => Self::ParseHuge,
            _ => return None,
        })
    }
}

/// The `XML_OPTION_*` constants as `(name, value)` pairs.
pub const OPTION_CONSTANTS: [(&str, i64); 5] = [
    ("XML_OPTION_CASE_FOLDING", 1),
    ("XML_OPTION_TARGET_ENCODING", 2),
    ("XML_OPTION_SKIP_TAGSTART", 3),
    ("XML_OPTION_SKIP_WHITE", 4),
    ("XML_OPTION_PARSE_HUGE", 5),
];

/// Value of an option as returned by `xml_parser_get_option()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    /// `bool` options.
    Bool(bool),
    /// `XML_OPTION_SKIP_TAGSTART`.
    Int(i64),
    /// `XML_OPTION_TARGET_ENCODING`.
    Str(&'static str),
}

/// Errors of the ext/xml API; [`std::fmt::Display`] gives PHP's message
/// (without the `"function(): "` prefix where PHP adds one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlError {
    /// `xml_parser_create()`: `ValueError` "Argument #1 ($encoding) is not
    /// a supported source encoding".
    UnsupportedSourceEncoding,
    /// `xml_parser_set_option()`: `ValueError` "Argument #3 ($value) is not
    /// a supported target encoding".
    UnsupportedTargetEncoding,
    /// `XML_OPTION_SKIP_TAGSTART` out of range: an `E_WARNING`, and the
    /// function returns `false`.
    TagstartOutOfRange,
    /// `Error` "Cannot change option `XML_OPTION_PARSE_HUGE` while parsing".
    ParseHugeWhileParsing,
    /// `Error` "Cannot change handlers while parsing".
    HandlersWhileParsing,
    /// `xml_parse()` throws `Error` "Parser must not be called
    /// recursively"; `xml_parse_into_struct()` warns and returns false.
    Recursive,
}

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedSourceEncoding => "Argument #1 ($encoding) is not a supported source encoding",
            Self::UnsupportedTargetEncoding => "Argument #3 ($value) is not a supported target encoding",
            Self::TagstartOutOfRange => {
                "Argument #3 ($value) must be between 0 and 2147483647 for option XML_OPTION_SKIP_TAGSTART"
            }
            Self::ParseHugeWhileParsing => "Cannot change option XML_OPTION_PARSE_HUGE while parsing",
            Self::HandlersWhileParsing => "Cannot change handlers while parsing",
            Self::Recursive => "Parser must not be called recursively",
        })
    }
}

impl std::error::Error for XmlError {}

/// Target encodings (`xml_encodings[]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetEncoding {
    /// `ISO-8859-1`: code points above U+00FF become `?`.
    Iso88591,
    /// `US-ASCII`: code points above U+007F become `?`.
    UsAscii,
    /// `UTF-8`: data is passed through.
    Utf8,
}

impl TargetEncoding {
    /// The canonical name returned by `xml_parser_get_option()`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Iso88591 => "ISO-8859-1",
            Self::UsAscii => "US-ASCII",
            Self::Utf8 => "UTF-8",
        }
    }

    /// `xml_get_encoding()`: case-insensitive exact match.
    pub fn from_name(name: &[u8]) -> Option<Self> {
        [Self::Iso88591, Self::UsAscii, Self::Utf8]
            .into_iter()
            .find(|e| e.name().as_bytes().eq_ignore_ascii_case(name))
    }
}

/// `php_next_utf8_char()`: decodes one code point at `*pos`, advancing it;
/// `None` on an invalid sequence (with `*pos` advanced past the maximal
/// invalid prefix, following UTR #36 strategy 2).
pub fn next_utf8_char(s: &[u8], pos: &mut usize) -> Option<u32> {
    let p = *pos;
    let avail = s.len().saturating_sub(p);
    if avail == 0 {
        *pos += 1;
        return None;
    }
    let trail = |c: u8| (0x80..=0xBF).contains(&c);
    let lead = |c: u8| c < 0x80 || (0xC2..=0xF4).contains(&c);
    let fail = |pos: &mut usize, n: usize| {
        *pos = p + n;
        None
    };
    let c = s[p];
    if c < 0x80 {
        *pos = p + 1;
        return Some(u32::from(c));
    }
    if c < 0xC2 {
        return fail(pos, 1);
    }
    if c < 0xE0 {
        if avail < 2 {
            return fail(pos, 1);
        }
        if !trail(s[p + 1]) {
            return fail(pos, if lead(s[p + 1]) { 1 } else { 2 });
        }
        let cp = (u32::from(c & 0x1F) << 6) | u32::from(s[p + 1] & 0x3F);
        if cp < 0x80 {
            return fail(pos, 2);
        }
        *pos = p + 2;
        return Some(cp);
    }
    if c < 0xF0 {
        if avail < 3 || !trail(s[p + 1]) || !trail(s[p + 2]) {
            if avail < 2 || lead(s[p + 1]) {
                return fail(pos, 1);
            } else if avail < 3 || lead(s[p + 2]) {
                return fail(pos, 2);
            }
            return fail(pos, 3);
        }
        let cp = (u32::from(c & 0x0F) << 12)
            | (u32::from(s[p + 1] & 0x3F) << 6)
            | u32::from(s[p + 2] & 0x3F);
        if cp < 0x800 || (0xD800..=0xDFFF).contains(&cp) {
            return fail(pos, 3);
        }
        *pos = p + 3;
        return Some(cp);
    }
    if c < 0xF5 {
        if avail < 4 || !trail(s[p + 1]) || !trail(s[p + 2]) || !trail(s[p + 3]) {
            if avail < 2 || lead(s[p + 1]) {
                return fail(pos, 1);
            } else if avail < 3 || lead(s[p + 2]) {
                return fail(pos, 2);
            } else if avail < 4 || lead(s[p + 3]) {
                return fail(pos, 3);
            }
            return fail(pos, 4);
        }
        let cp = (u32::from(c & 0x07) << 18)
            | (u32::from(s[p + 1] & 0x3F) << 12)
            | (u32::from(s[p + 2] & 0x3F) << 6)
            | u32::from(s[p + 3] & 0x3F);
        if !(0x1_0000..=0x10_FFFF).contains(&cp) {
            return fail(pos, 4);
        }
        *pos = p + 4;
        return Some(cp);
    }
    fail(pos, 1)
}

/// `xml_utf8_decode()`: converts libxml2's UTF-8 output to the target
/// encoding (`?` for unrepresentable or invalid sequences).
pub fn utf8_decode(s: &[u8], target: TargetEncoding) -> Vec<u8> {
    let max = match target {
        TargetEncoding::Utf8 => return s.to_vec(),
        TargetEncoding::Iso88591 => 0xFF,
        TargetEncoding::UsAscii => 0x7F,
    };
    let mut out = Vec::with_capacity(s.len());
    let mut pos = 0;
    while pos < s.len() {
        let c = match next_utf8_char(s, &mut pos) {
            Some(c) if c <= 0xFF => c,
            _ => u32::from(b'?'),
        };
        out.push(if c > max {
            b'?'
        } else {
            u8::try_from(c).unwrap_or(b'?')
        });
    }
    out
}

/// ISO-8859-1 to UTF-8 (`xml_encode_iso_8859_1` / `utf8_encode()`).
pub fn utf8_encode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for &b in s {
        if b < 0x80 {
            out.push(b);
        } else {
            out.push(0xC0 | (b >> 6));
            out.push(0x80 | (b & 0x3F));
        }
    }
    out
}

/// `XML_ErrorString()` table of the libxml2 compatibility layer.
const ERROR_STRINGS: [&str; 102] = [
    "No error",
    "No memory",
    "Invalid document start",
    "Empty document",
    "Not well-formed (invalid token)",
    "Invalid document end",
    "Invalid hexadecimal character reference",
    "Invalid decimal character reference",
    "Invalid character reference",
    "Invalid character",
    "XML_ERR_CHARREF_AT_EOF",
    "XML_ERR_CHARREF_IN_PROLOG",
    "XML_ERR_CHARREF_IN_EPILOG",
    "XML_ERR_CHARREF_IN_DTD",
    "XML_ERR_ENTITYREF_AT_EOF",
    "XML_ERR_ENTITYREF_IN_PROLOG",
    "XML_ERR_ENTITYREF_IN_EPILOG",
    "XML_ERR_ENTITYREF_IN_DTD",
    "PEReference at end of document",
    "PEReference in prolog",
    "PEReference in epilog",
    "PEReference: forbidden within markup decl in internal subset",
    "XML_ERR_ENTITYREF_NO_NAME",
    "EntityRef: expecting ';'",
    "PEReference: no name",
    "PEReference: expecting ';'",
    "Undeclared entity error",
    "Undeclared entity warning",
    "Unparsed Entity",
    "XML_ERR_ENTITY_IS_EXTERNAL",
    "XML_ERR_ENTITY_IS_PARAMETER",
    "Unknown encoding",
    "Unsupported encoding",
    "String not started expecting ' or \"",
    "String not closed expecting \" or '",
    "Namespace declaration error",
    "EntityValue: \" or ' expected",
    "EntityValue: \" or ' expected",
    "< in attribute",
    "Attribute not started",
    "Attribute not finished",
    "Attribute without value",
    "Attribute redefined",
    "SystemLiteral \" or ' expected",
    "SystemLiteral \" or ' expected",
    "Comment not finished",
    "Processing Instruction not started",
    "Processing Instruction not finished",
    "NOTATION: Name expected here",
    "'>' required to close NOTATION declaration",
    "'(' required to start ATTLIST enumeration",
    "'(' required to start ATTLIST enumeration",
    "MixedContentDecl : '|' or ')*' expected",
    "XML_ERR_MIXED_NOT_FINISHED",
    "ELEMENT in DTD not started",
    "ELEMENT in DTD not finished",
    "XML declaration not started",
    "XML declaration not finished",
    "XML_ERR_CONDSEC_NOT_STARTED",
    "XML conditional section not closed",
    "Content error in the external subset",
    "DOCTYPE not finished",
    "Sequence ']]>' not allowed in content",
    "CDATA not finished",
    "Reserved XML Name",
    "Space required",
    "XML_ERR_SEPARATOR_REQUIRED",
    "NmToken expected in ATTLIST enumeration",
    "XML_ERR_NAME_REQUIRED",
    "MixedContentDecl : '#PCDATA' expected",
    "SYSTEM or PUBLIC, the URI is missing",
    "PUBLIC, the Public Identifier is missing",
    "< required",
    "> required",
    "</ required",
    "= required",
    "Mismatched tag",
    "Tag not finished",
    "standalone accepts only 'yes' or 'no'",
    "Invalid XML encoding name",
    "Comment must not contain '--' (double-hyphen)",
    "Invalid encoding",
    "external parsed entities cannot be standalone",
    "XML conditional section '[' expected",
    "Entity value required",
    "chunk is not well balanced",
    "extra content at the end of well balanced chunk",
    "XML_ERR_ENTITY_CHAR_ERROR",
    "PEReferences forbidden in internal subset",
    "Detected an entity reference loop",
    "XML_ERR_ENTITY_BOUNDARY",
    "Invalid URI",
    "Fragment not allowed",
    "XML_WAR_CATALOG_PI",
    "XML_ERR_NO_DTD",
    "conditional section INCLUDE or IGNORE keyword expected",
    "Version in XML Declaration missing",
    "XML_WAR_UNKNOWN_VERSION",
    "XML_WAR_LANG_VALUE",
    "XML_WAR_NS_URI",
    "XML_WAR_NS_URI_RELATIVE",
    "Missing encoding in text declaration",
];

/// `xml_error_string()` (`XML_ErrorString` truncates the code to `int`).
pub fn error_string(code: i64) -> &'static str {
    #[allow(clippy::cast_possible_truncation)]
    let code = code as i32;
    usize::try_from(code)
        .ok()
        .and_then(|i| ERROR_STRINGS.get(i))
        .copied()
        .unwrap_or("Unknown")
}

/// The user callbacks of an `XMLParser` (`xml_set_*_handler()`).
///
/// Arguments are already converted to the target encoding and case-folded
/// as PHP does; `None` stands for PHP `false` (a NULL C string). The VM
/// passes the parser object as the first PHP argument itself.
#[allow(unused_variables)]
pub trait XmlHandlers {
    /// Whether an exception is pending (`EG(exception)`): element and
    /// character data processing stops once it is.
    fn aborted(&self) -> bool {
        false
    }
    /// Start element handler: `($parser, $name, $attribs)`.
    fn start_element(&mut self, name: &[u8], attributes: &[(Vec<u8>, Vec<u8>)]) {}
    /// End element handler: `($parser, $name)`.
    fn end_element(&mut self, name: &[u8]) {}
    /// Character data handler: `($parser, $data)`.
    fn character_data(&mut self, data: &[u8]) {}
    /// Processing instruction handler: `($parser, $target, $data)`.
    fn processing_instruction(&mut self, target: Option<&[u8]>, data: Option<&[u8]>) {}
    /// Default handler: `($parser, $data)`.
    fn default_handler(&mut self, data: &[u8]) {}
    /// Unparsed entity declaration handler:
    /// `($parser, $entity_name, $base, $system_id, $public_id, $notation_name)`.
    fn unparsed_entity_decl(
        &mut self,
        entity_name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
        notation_name: Option<&[u8]>,
    ) {
    }
    /// Notation declaration handler:
    /// `($parser, $notation_name, $base, $system_id, $public_id)`.
    fn notation_decl(
        &mut self,
        notation_name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    ) {
    }
    /// External entity reference handler; returns the callable's result
    /// converted to int (0 when it failed). Zero aborts parsing with
    /// `XML_ERROR_EXTERNAL_ENTITY_HANDLING`.
    fn external_entity_ref(
        &mut self,
        open_entity_names: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    ) -> i64 {
        0
    }
    /// Start namespace declaration handler: `($parser, $prefix, $uri)`.
    fn start_namespace_decl(&mut self, prefix: Option<&[u8]>, uri: Option<&[u8]>) {}
    /// End namespace declaration handler (never called with libxml2).
    fn end_namespace_decl(&mut self, prefix: Option<&[u8]>) {}
    /// `php_error_docref(E_WARNING)` raised during parsing
    /// (e.g. [`MAX_DEPTH_WARNING`]).
    fn warning(&mut self, message: &str) {}
}

/// Handlers that ignore every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHandlers;

impl XmlHandlers for NoHandlers {}

/// The handler kinds of `xml_set_*_handler()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerKind {
    /// Start element callable (installed together with the end handler).
    StartElement,
    /// End element callable.
    EndElement,
    /// `xml_set_character_data_handler`.
    CharacterData,
    /// `xml_set_processing_instruction_handler`.
    ProcessingInstruction,
    /// `xml_set_default_handler`.
    Default,
    /// `xml_set_unparsed_entity_decl_handler`.
    UnparsedEntityDecl,
    /// `xml_set_notation_decl_handler`.
    NotationDecl,
    /// `xml_set_external_entity_ref_handler`.
    ExternalEntityRef,
    /// `xml_set_start_namespace_decl_handler`.
    StartNamespaceDecl,
    /// `xml_set_end_namespace_decl_handler`.
    EndNamespaceDecl,
}

impl HandlerKind {
    fn slot(self) -> u16 {
        match self {
            Self::StartElement => slot::START_ELEMENT,
            Self::EndElement => slot::END_ELEMENT,
            Self::CharacterData => slot::CHARACTER_DATA,
            Self::ProcessingInstruction => slot::PROCESSING_INSTRUCTION,
            Self::Default => slot::DEFAULT,
            Self::UnparsedEntityDecl => slot::UNPARSED_ENTITY_DECL,
            Self::NotationDecl => slot::NOTATION_DECL,
            Self::ExternalEntityRef => slot::EXTERNAL_ENTITY_REF,
            Self::StartNamespaceDecl => slot::START_NAMESPACE_DECL,
            Self::EndNamespaceDecl => slot::END_NAMESPACE_DECL,
        }
    }

    /// The `xml_set_*` function that installs this handler (used in
    /// `xml_set_object()` error messages).
    pub fn setter_name(self) -> &'static str {
        match self {
            Self::StartElement | Self::EndElement => "xml_set_element_handler()",
            Self::CharacterData => "xml_set_character_data_handler()",
            Self::ProcessingInstruction => "xml_set_processing_instruction_handler()",
            Self::Default => "xml_set_default_handler()",
            Self::UnparsedEntityDecl => "xml_set_unparsed_entity_decl_handler()",
            Self::NotationDecl => "xml_set_notation_decl_handler()",
            Self::ExternalEntityRef => "xml_set_external_entity_ref_handler()",
            Self::StartNamespaceDecl => "xml_set_start_namespace_decl_handler()",
            Self::EndNamespaceDecl => "xml_set_end_namespace_decl_handler()",
        }
    }
}

/// A value stored in an `xml_parse_into_struct()` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructValue {
    /// A string (`tag`, `type`, `value`).
    Str(Vec<u8>),
    /// An integer (`level`).
    Int(i64),
    /// The `attributes` array (`name => value`, insertion ordered).
    Attributes(Vec<(Vec<u8>, Vec<u8>)>),
}

/// One element of the `$values` array: an ordered map whose keys are
/// among `tag`, `type`, `level`, `attributes` and `value`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructEntry {
    /// Fields in PHP insertion order.
    pub fields: Vec<(&'static str, StructValue)>,
}

impl StructEntry {
    /// Looks up a field.
    pub fn get(&self, key: &str) -> Option<&StructValue> {
        self.fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Looks up a string field.
    pub fn get_str(&self, key: &str) -> Option<&[u8]> {
        match self.get(key) {
            Some(StructValue::Str(s)) => Some(s),
            _ => None,
        }
    }

    /// Sets a field, keeping its position when it already exists
    /// (`zend_hash_update` semantics).
    pub fn set(&mut self, key: &'static str, value: StructValue) {
        match self.fields.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((key, value)),
        }
    }
}

/// The array operations `xml_parse_into_struct()` performs on `$values`
/// (`data`) and `$index` (`info`).
///
/// The C extension holds references to both PHP variables and re-checks
/// their types before every write, because user callbacks may overwrite
/// them; implementations over live PHP arrays should do the same and
/// report "not an array" by returning `None`/`false`.
pub trait StructSink {
    /// Appends an entry to `$values`; returns the index it was stored at
    /// (`nNextFreeElement - 1`), or `None` when `$values` is not an array.
    fn push(&mut self, entry: StructEntry) -> Option<i64>;
    /// `$values[$index]["type"] = "complete"` (if that element is an array).
    fn mark_complete(&mut self, index: i64);
    /// Appends to `$values[$index]["value"]` if it is a string; returns
    /// `None` when `$values[$index]` is not an array, `Some(false)` when it
    /// has no string value.
    fn append_value(&mut self, index: i64, text: &[u8]) -> Option<bool>;
    /// `$values[$index]["value"] = text`.
    fn set_value(&mut self, index: i64, text: &[u8]);
    /// Whether `$values` is currently an array.
    fn values_is_array(&mut self) -> bool;
    /// If the last element of `$values` is an array with `type` ===
    /// `"cdata"` and a `value` key, appends `text` to that value and
    /// returns true.
    fn append_to_last_cdata(&mut self, text: &[u8]) -> bool;
    /// `$index[$tag][] = $position`; returns false when `$index` is absent
    /// or not an array (the position counter is then not advanced).
    fn add_index(&mut self, tag: &[u8], position: i64) -> bool;
}

/// A self-contained [`StructSink`] building Rust values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructBuilder {
    /// `$values`.
    pub values: Vec<StructEntry>,
    /// `$index` (`None` when not requested), insertion ordered.
    pub index: Option<Vec<(Vec<u8>, Vec<i64>)>>,
}

impl StructBuilder {
    /// A builder; `with_index` requests the `$index` array.
    pub fn new(with_index: bool) -> Self {
        Self {
            values: Vec::new(),
            index: with_index.then(Vec::new),
        }
    }

    fn entry(&mut self, index: i64) -> Option<&mut StructEntry> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.values.get_mut(i))
    }
}

impl StructSink for StructBuilder {
    fn push(&mut self, entry: StructEntry) -> Option<i64> {
        self.values.push(entry);
        i64::try_from(self.values.len() - 1).ok()
    }

    fn mark_complete(&mut self, index: i64) {
        if let Some(e) = self.entry(index) {
            e.set("type", StructValue::Str(b"complete".to_vec()));
        }
    }

    fn append_value(&mut self, index: i64, text: &[u8]) -> Option<bool> {
        let e = self.entry(index)?;
        match e.fields.iter_mut().find(|(k, _)| *k == "value") {
            Some((_, StructValue::Str(s))) => {
                s.extend_from_slice(text);
                Some(true)
            }
            _ => Some(false),
        }
    }

    fn set_value(&mut self, index: i64, text: &[u8]) {
        if let Some(e) = self.entry(index) {
            e.set("value", StructValue::Str(text.to_vec()));
        }
    }

    fn values_is_array(&mut self) -> bool {
        true
    }

    fn append_to_last_cdata(&mut self, text: &[u8]) -> bool {
        let Some(last) = self.values.last_mut() else {
            return false;
        };
        if last.get_str("type") != Some(b"cdata") {
            return false;
        }
        match last.fields.iter_mut().find(|(k, _)| *k == "value") {
            Some((_, StructValue::Str(s))) => {
                s.extend_from_slice(text);
                true
            }
            _ => false,
        }
    }

    fn add_index(&mut self, tag: &[u8], position: i64) -> bool {
        let Some(index) = self.index.as_mut() else {
            return false;
        };
        match index.iter_mut().find(|(k, _)| k.as_slice() == tag) {
            Some((_, list)) => list.push(position),
            None => index.push((tag.to_vec(), vec![position])),
        }
        true
    }
}

/// Builds the PHP attribute array for one start-element event: keys are
/// decoded (and case-folded), duplicates keep their first position with the
/// last value (`zend_hash_update`). Linear in the number of attributes.
/// The decoded attribute list (later duplicates overwrite earlier ones, as
/// `zend_hash_update` does); `None` once the handlers abort mid-way
/// (`ZEND_HAS_OOM()` between attributes). The tables grow with the list,
/// like the PHP array, so the abort comes before a large up-front
/// allocation could cross the hard memory limit.
fn decode_attrs(
    p: &XmlParser,
    attrs: &[(Vec<u8>, Vec<u8>)],
    handlers: &dyn XmlHandlers,
) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut positions: HashMap<Vec<u8>, usize> = HashMap::new();
    for (k, v) in attrs {
        let key = p.decode_tag(k);
        let value = p.decode(v);
        if let Some(&i) = positions.get(&key) {
            out[i].1 = value;
        } else {
            positions.insert(key.clone(), out.len());
            out.push((key, value));
        }
        if handlers.aborted() {
            return None;
        }
    }
    Some(out)
}

/// An `XMLParser` object.
pub struct XmlParser {
    raw: PushParser,
    target_encoding: Cell<TargetEncoding>,
    /// Handlers with a user callable (`ZEND_FCC_INITIALIZED`).
    user: Cell<u16>,
    case_folding: Cell<bool>,
    skip_white: Cell<bool>,
    parse_huge: Cell<bool>,
    toffset: Cell<i64>,
    level: Cell<i32>,
    curtag: Cell<i64>,
    ctag_index: Cell<i64>,
    last_was_open: Cell<bool>,
    is_parsing: Cell<bool>,
    /// `ltags` (allocated by `xml_parse_into_struct`).
    ltags: RefCell<Option<Vec<Option<Vec<u8>>>>>,
    /// Whether `$values` has been bound by `xml_parse_into_struct()`.
    struct_bound: Cell<bool>,
}

impl std::fmt::Debug for XmlParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XmlParser")
            .field("target_encoding", &self.target_encoding.get())
            .field("case_folding", &self.case_folding.get())
            .field("level", &self.level.get())
            .finish_non_exhaustive()
    }
}

impl XmlParser {
    /// `xml_parser_create($encoding)`. The encoding only selects the
    /// target encoding (libxml2 auto-detects the source); `""` and `None`
    /// mean UTF-8.
    pub fn new(encoding: Option<&[u8]>) -> Result<Self, XmlError> {
        Self::create(encoding, None)
    }

    /// `xml_parser_create_ns($encoding, $separator)`.
    pub fn new_ns(encoding: Option<&[u8]>, separator: &[u8]) -> Result<Self, XmlError> {
        Self::create(encoding, Some(separator))
    }

    fn create(encoding: Option<&[u8]>, separator: Option<&[u8]>) -> Result<Self, XmlError> {
        let target = match encoding {
            None | Some([]) => TargetEncoding::Utf8,
            Some(e) => TargetEncoding::from_name(e).ok_or(XmlError::UnsupportedSourceEncoding)?,
        };
        let raw = PushParser::new(separator).ok_or(XmlError::UnsupportedSourceEncoding)?;
        Ok(Self {
            raw,
            target_encoding: Cell::new(target),
            user: Cell::new(0),
            case_folding: Cell::new(true),
            skip_white: Cell::new(false),
            parse_huge: Cell::new(false),
            toffset: Cell::new(0),
            level: Cell::new(0),
            curtag: Cell::new(0),
            ctag_index: Cell::new(0),
            last_was_open: Cell::new(false),
            is_parsing: Cell::new(false),
            ltags: RefCell::new(None),
            struct_bound: Cell::new(false),
        })
    }

    /// Whether a parse is in progress (`parser->isparsing`).
    pub fn is_parsing(&self) -> bool {
        self.is_parsing.get()
    }

    /// Records the result of `xml_set_*_handler()`: `has_callable` tells
    /// whether a callable (rather than `null`/`""`) was given. Element
    /// handlers are installed in pairs, as in PHP.
    pub fn set_handler(&self, kind: HandlerKind, has_callable: bool) -> Result<(), XmlError> {
        if self.is_parsing.get() {
            return Err(XmlError::HandlersWhileParsing);
        }
        let s = kind.slot();
        let user = self.user.get();
        self.user
            .set(if has_callable { user | s } else { user & !s });
        let install = match kind {
            HandlerKind::StartElement | HandlerKind::EndElement => {
                slot::START_ELEMENT | slot::END_ELEMENT
            }
            _ => s,
        };
        self.raw.set_installed(install, true);
        Ok(())
    }

    /// Whether a user callable is registered for `kind`.
    pub fn has_handler(&self, kind: HandlerKind) -> bool {
        self.user.get() & kind.slot() != 0
    }

    /// `xml_parse($parser, $data, $is_final)`: returns 1 or 0.
    ///
    /// `values` must be passed when a previous `xml_parse_into_struct()`
    /// bound `$values` (PHP keeps appending to it); otherwise `None`.
    pub fn parse(
        &self,
        data: &[u8],
        is_final: bool,
        handlers: &mut dyn XmlHandlers,
        values: Option<&mut dyn StructSink>,
    ) -> Result<i64, XmlError> {
        if self.is_parsing.get() {
            return Err(XmlError::Recursive);
        }
        let values = if self.struct_bound.get() {
            values
        } else {
            None
        };
        self.run(data, is_final, handlers, values)
    }

    /// `xml_parse_into_struct($parser, $data, $values, $index)`: resets the
    /// level bookkeeping, binds the sink and parses `data` as final.
    pub fn parse_into_struct(
        &self,
        data: &[u8],
        handlers: &mut dyn XmlHandlers,
        sink: &mut dyn StructSink,
    ) -> Result<i64, XmlError> {
        if self.is_parsing.get() {
            return Err(XmlError::Recursive);
        }
        self.struct_bound.set(true);
        self.level.set(0);
        *self.ltags.borrow_mut() = Some(vec![None; usize::try_from(XML_MAXLEVEL).unwrap_or(255)]);
        self.raw.set_installed(
            slot::START_ELEMENT | slot::END_ELEMENT | slot::CHARACTER_DATA,
            true,
        );
        self.run(data, true, handlers, Some(sink))
    }

    fn run(
        &self,
        data: &[u8],
        is_final: bool,
        handlers: &mut dyn XmlHandlers,
        values: Option<&mut dyn StructSink>,
    ) -> Result<i64, XmlError> {
        /// Clears `is_parsing` even when a handler panic unwinds.
        struct Parsing<'a>(&'a Cell<bool>);
        impl Drop for Parsing<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        self.is_parsing.set(true);
        let _parsing = Parsing(&self.is_parsing);
        let mut adapter = Adapter {
            parser: self,
            handlers,
            values,
        };
        let result = self
            .raw
            .parse(data, is_final, self.parse_huge.get(), &mut adapter);
        result.map(i64::from).map_err(|_| XmlError::Recursive)
    }

    /// `xml_get_error_code()`.
    pub fn error_code(&self) -> i64 {
        i64::from(self.raw.error_code())
    }

    /// `xml_get_current_line_number()`.
    pub fn current_line_number(&self) -> i64 {
        self.raw.current_line()
    }

    /// `xml_get_current_column_number()`.
    pub fn current_column_number(&self) -> i64 {
        self.raw.current_column()
    }

    /// `xml_get_current_byte_index()`.
    pub fn current_byte_index(&self) -> i64 {
        self.raw.current_byte_index()
    }

    /// `XML_OPTION_CASE_FOLDING`.
    pub fn set_case_folding(&self, on: bool) {
        self.case_folding.set(on);
    }

    /// `XML_OPTION_SKIP_WHITE`.
    pub fn set_skip_white(&self, on: bool) {
        self.skip_white.set(on);
    }

    /// `XML_OPTION_PARSE_HUGE` (rejected while parsing).
    pub fn set_parse_huge(&self, on: bool) -> Result<(), XmlError> {
        if self.is_parsing.get() {
            return Err(XmlError::ParseHugeWhileParsing);
        }
        self.parse_huge.set(on);
        Ok(())
    }

    /// `XML_OPTION_SKIP_TAGSTART` (0..=`INT_MAX`).
    pub fn set_skip_tagstart(&self, value: i64) -> Result<(), XmlError> {
        if !(0..=i64::from(i32::MAX)).contains(&value) {
            return Err(XmlError::TagstartOutOfRange);
        }
        self.toffset.set(value);
        Ok(())
    }

    /// `XML_OPTION_TARGET_ENCODING`.
    pub fn set_target_encoding(&self, name: &[u8]) -> Result<(), XmlError> {
        let enc = TargetEncoding::from_name(name).ok_or(XmlError::UnsupportedTargetEncoding)?;
        self.target_encoding.set(enc);
        Ok(())
    }

    /// `xml_parser_get_option()`.
    pub fn get_option(&self, option: XmlOption) -> OptionValue {
        match option {
            XmlOption::CaseFolding => OptionValue::Bool(self.case_folding.get()),
            XmlOption::SkipTagstart => OptionValue::Int(self.toffset.get()),
            XmlOption::SkipWhite => OptionValue::Bool(self.skip_white.get()),
            XmlOption::ParseHuge => OptionValue::Bool(self.parse_huge.get()),
            XmlOption::TargetEncoding => OptionValue::Str(self.target_encoding.get().name()),
        }
    }

    /// `xml_decode_tag()`: target-encode then ASCII-uppercase when case
    /// folding is on.
    fn decode_tag(&self, tag: &[u8]) -> Vec<u8> {
        let mut s = utf8_decode(tag, self.target_encoding.get());
        if self.case_folding.get() {
            s.make_ascii_uppercase();
        }
        s
    }

    fn decode(&self, s: &[u8]) -> Vec<u8> {
        utf8_decode(s, self.target_encoding.get())
    }

    fn decode_opt(&self, s: Option<&[u8]>) -> Option<Vec<u8>> {
        s.map(|s| self.decode(s))
    }

    /// `xml_stripped_tag()`.
    fn stripped(&self, tag: &[u8]) -> Vec<u8> {
        let off = usize::try_from(self.toffset.get()).unwrap_or(usize::MAX);
        if off >= tag.len() {
            if off == 0 {
                tag.to_vec()
            } else {
                Vec::new()
            }
        } else {
            tag[off..].to_vec()
        }
    }

    fn ltag_index(&self) -> Option<usize> {
        let level = self.level.get();
        if (1..=XML_MAXLEVEL).contains(&level) {
            usize::try_from(level - 1).ok()
        } else {
            None
        }
    }
}

/// Bridges raw expat events to the PHP-level logic of `xml.c`.
struct Adapter<'p, 'h, 'v> {
    parser: &'p XmlParser,
    handlers: &'h mut dyn XmlHandlers,
    values: Option<&'v mut dyn StructSink>,
}

impl Adapter<'_, '_, '_> {
    fn add_to_info(&mut self, tag: &[u8]) {
        let p = self.parser;
        if let Some(values) = self.values.as_mut() {
            if values.add_index(tag, p.curtag.get()) {
                p.curtag.set(p.curtag.get() + 1);
            }
        }
    }
}

impl ExpatSink for Adapter<'_, '_, '_> {
    fn aborted(&self) -> bool {
        self.handlers.aborted()
    }

    fn start_element(&mut self, name: &[u8], attrs: &[(Vec<u8>, Vec<u8>)]) {
        let p = self.parser;
        if self.handlers.aborted() {
            return;
        }
        p.level.set(p.level.get() + 1);
        let tag_name = p.decode_tag(name);
        // Decoded once and reused for the struct entry unless a callback
        // changed the decoding options in between (PHP decodes twice).
        let decoding = (p.case_folding.get(), p.target_encoding.get());
        let mut decoded = None;
        if p.has_handler(HandlerKind::StartElement) {
            let Some(list) = decode_attrs(p, attrs, &*self.handlers) else {
                return;
            };
            self.handlers.start_element(&p.stripped(&tag_name), &list);
            decoded = Some(list);
        }
        if self.values.is_none() || self.handlers.aborted() {
            return;
        }
        let level = p.level.get();
        if level <= XML_MAXLEVEL {
            let stripped = p.stripped(&tag_name);
            self.add_to_info(&stripped);
            let mut entry = StructEntry::default();
            entry.set("tag", StructValue::Str(stripped));
            entry.set("type", StructValue::Str(b"open".to_vec()));
            entry.set("level", StructValue::Int(i64::from(level)));
            if let (Some(i), Some(ltags)) = (p.ltag_index(), p.ltags.borrow_mut().as_mut()) {
                ltags[i] = Some(tag_name);
            }
            p.last_was_open.set(true);
            let attributes = match decoded {
                Some(list) if decoding == (p.case_folding.get(), p.target_encoding.get()) => list,
                _ => match decode_attrs(p, attrs, &*self.handlers) {
                    Some(list) => list,
                    None => return,
                },
            };
            if !attributes.is_empty() {
                entry.set("attributes", StructValue::Attributes(attributes));
            }
            if let Some(values) = self.values.as_mut() {
                if let Some(index) = values.push(entry) {
                    p.ctag_index.set(index);
                }
            }
        } else if level == XML_MAXLEVEL + 1 {
            self.handlers.warning(MAX_DEPTH_WARNING);
        }
    }

    fn end_element(&mut self, name: &[u8]) {
        let p = self.parser;
        if self.handlers.aborted() {
            return;
        }
        let tag_name = p.decode_tag(name);
        if p.has_handler(HandlerKind::EndElement) {
            self.handlers.end_element(&p.stripped(&tag_name));
        }
        if self.values.is_some() && !self.handlers.aborted() {
            if p.last_was_open.get() {
                if let Some(values) = self.values.as_mut() {
                    values.mark_complete(p.ctag_index.get());
                }
            } else {
                let stripped = p.stripped(&tag_name);
                self.add_to_info(&stripped);
                if let Some(values) = self.values.as_mut() {
                    if values.values_is_array() {
                        let mut entry = StructEntry::default();
                        entry.set("tag", StructValue::Str(stripped));
                        entry.set("type", StructValue::Str(b"close".to_vec()));
                        entry.set("level", StructValue::Int(i64::from(p.level.get())));
                        values.push(entry);
                    }
                }
            }
            p.last_was_open.set(false);
        }
        if let (Some(i), Some(ltags)) = (p.ltag_index(), p.ltags.borrow_mut().as_mut()) {
            ltags[i] = None;
        }
        p.level.set(p.level.get() - 1);
    }

    fn character_data(&mut self, data: &[u8]) {
        let p = self.parser;
        if self.handlers.aborted() {
            return;
        }
        if p.has_handler(HandlerKind::CharacterData) {
            self.handlers.character_data(&p.decode(data));
        }
        if self.values.is_none() || self.handlers.aborted() {
            return;
        }
        let decoded = p.decode(data);
        let skip_white = p.skip_white.get();
        let doprint = skip_white && decoded.iter().any(|b| !matches!(b, b' ' | b'\t' | b'\n'));
        let Some(values) = self.values.as_mut() else {
            return;
        };
        if p.last_was_open.get() {
            let index = p.ctag_index.get();
            match values.append_value(index, &decoded) {
                None | Some(true) => {}
                Some(false) => {
                    if doprint || !skip_white {
                        values.set_value(index, &decoded);
                    }
                }
            }
            return;
        }
        if !values.values_is_array() || values.append_to_last_cdata(&decoded) {
            return;
        }
        let level = p.level.get();
        if level <= XML_MAXLEVEL && level > 0 && (doprint || !skip_white) {
            let tag = p
                .ltag_index()
                .and_then(|i| p.ltags.borrow().as_ref().and_then(|l| l[i].clone()))
                .unwrap_or_default();
            let stripped = p.stripped(&tag);
            self.add_to_info(&stripped);
            let mut entry = StructEntry::default();
            entry.set("tag", StructValue::Str(stripped));
            entry.set("value", StructValue::Str(decoded));
            entry.set("type", StructValue::Str(b"cdata".to_vec()));
            entry.set("level", StructValue::Int(i64::from(level)));
            if let Some(values) = self.values.as_mut() {
                values.push(entry);
            }
        } else if level == XML_MAXLEVEL + 1 {
            self.handlers.warning(MAX_DEPTH_WARNING);
        }
    }

    fn processing_instruction(&mut self, target: Option<&[u8]>, data: Option<&[u8]>) {
        let p = self.parser;
        if p.has_handler(HandlerKind::ProcessingInstruction) {
            self.handlers.processing_instruction(
                p.decode_opt(target).as_deref(),
                p.decode_opt(data).as_deref(),
            );
        }
    }

    fn default_data(&mut self, data: &[u8]) {
        let p = self.parser;
        if p.has_handler(HandlerKind::Default) {
            self.handlers.default_handler(&p.decode(data));
        }
    }

    fn unparsed_entity_decl(
        &mut self,
        name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
        notation: Option<&[u8]>,
    ) {
        let p = self.parser;
        if p.has_handler(HandlerKind::UnparsedEntityDecl) {
            self.handlers.unparsed_entity_decl(
                p.decode_opt(name).as_deref(),
                p.decode_opt(base).as_deref(),
                p.decode_opt(system_id).as_deref(),
                p.decode_opt(public_id).as_deref(),
                p.decode_opt(notation).as_deref(),
            );
        }
    }

    fn notation_decl(
        &mut self,
        name: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    ) {
        let p = self.parser;
        if p.has_handler(HandlerKind::NotationDecl) {
            self.handlers.notation_decl(
                p.decode_opt(name).as_deref(),
                p.decode_opt(base).as_deref(),
                p.decode_opt(system_id).as_deref(),
                p.decode_opt(public_id).as_deref(),
            );
        }
    }

    fn external_entity_ref(
        &mut self,
        open_entity_names: Option<&[u8]>,
        base: Option<&[u8]>,
        system_id: Option<&[u8]>,
        public_id: Option<&[u8]>,
    ) -> i64 {
        let p = self.parser;
        if !p.has_handler(HandlerKind::ExternalEntityRef) {
            return 0;
        }
        self.handlers.external_entity_ref(
            p.decode_opt(open_entity_names).as_deref(),
            p.decode_opt(base).as_deref(),
            p.decode_opt(system_id).as_deref(),
            p.decode_opt(public_id).as_deref(),
        )
    }

    fn start_namespace_decl(&mut self, prefix: Option<&[u8]>, uri: Option<&[u8]>) {
        let p = self.parser;
        if p.has_handler(HandlerKind::StartNamespaceDecl) {
            self.handlers.start_namespace_decl(
                p.decode_opt(prefix).as_deref(),
                p.decode_opt(uri).as_deref(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_strings() {
        assert_eq!(error_string(0), "No error");
        assert_eq!(error_string(5), "Invalid document end");
        assert_eq!(error_string(47), "Processing Instruction not finished");
        assert_eq!(error_string(57), "XML declaration not finished");
        assert_eq!(error_string(64), "Reserved XML Name");
        assert_eq!(error_string(68), "XML_ERR_NAME_REQUIRED");
        assert_eq!(error_string(76), "Mismatched tag");
        assert_eq!(error_string(101), "Missing encoding in text declaration");
        assert_eq!(error_string(102), "Unknown");
        assert_eq!(error_string(-1), "Unknown");
    }

    #[test]
    fn utf8_decoding() {
        assert_eq!(
            utf8_decode("äöü".as_bytes(), TargetEncoding::Iso88591),
            b"\xe4\xf6\xfc"
        );
        assert_eq!(
            utf8_decode("äöü".as_bytes(), TargetEncoding::UsAscii),
            b"???"
        );
        assert_eq!(utf8_decode("€".as_bytes(), TargetEncoding::Iso88591), b"?");
        assert_eq!(utf8_decode(b"\xff\xc3", TargetEncoding::Iso88591), b"??");
        assert_eq!(
            utf8_decode("x€".as_bytes(), TargetEncoding::Utf8),
            "x€".as_bytes()
        );
        assert_eq!(utf8_encode(b"\xe4a"), "äa".as_bytes());
    }

    #[test]
    fn next_utf8_char_failure_lengths() {
        let mut pos = 0;
        assert_eq!(next_utf8_char(b"\xe2\x82", &mut pos), None);
        assert_eq!(pos, 2);
        pos = 0;
        assert_eq!(next_utf8_char(b"\xe2A", &mut pos), None);
        assert_eq!(pos, 1);
        pos = 0;
        assert_eq!(next_utf8_char("𐍅".as_bytes(), &mut pos), Some(0x10345));
        assert_eq!(pos, 4);
    }

    #[test]
    fn encodings() {
        assert_eq!(
            TargetEncoding::from_name(b"iso-8859-1"),
            Some(TargetEncoding::Iso88591)
        );
        assert_eq!(TargetEncoding::from_name(b"EUC-JP"), None);
        assert!(XmlParser::new(Some(b"EUC-JP")).is_err());
        assert!(XmlParser::new(Some(b"")).is_ok());
    }
}
