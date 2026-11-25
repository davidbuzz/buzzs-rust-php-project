// Derived from php-src ext/soap/php_encoding.h. Similarity metric to original: 33%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! Encoder descriptors (`encode`/`encodeType` of `php_encoding.h`): the
//! built-in table `defaultEncoding[]`, encoders created by the schema
//! loader and user `typemap` encoders.

use std::rc::Rc;

use php_runtime::Value;

use crate::consts::*;
use crate::sdl::TypeId;

/// Which `to_xml` routine an encoder uses.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToXml {
    Guess,
    Null,
    String,
    Long,
    Double,
    Bool,
    GuessArrayMap,
    Object,
    Array,
    Map,
    Base64,
    HexBin,
    DateTime,
    Time,
    Date,
    GYearMonth,
    GYear,
    GMonthDay,
    GDay,
    GMonth,
    Duration,
    List1,
    Any,
    SdlGuess,
    User,
}

/// Which `to_zval` routine an encoder uses.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToZval {
    Guess,
    Null,
    String,
    StringR,
    StringC,
    Long,
    Double,
    Bool,
    Object,
    Array,
    Map,
    Base64,
    HexBin,
    Any,
    SdlGuess,
    User,
}

/// `soapMapping`: user callbacks of a `typemap` entry.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    /// `to_xml` callable.
    pub to_xml: Option<Value>,
    /// `from_xml` callable.
    pub to_zval: Option<Value>,
}

/// An encoder (`encode`).
#[derive(Debug, Clone)]
pub struct Encoder {
    /// `details.type` (a type code or `IS_*`).
    pub type_id: i64,
    /// `details.type_str`.
    pub type_str: Option<Rc<str>>,
    /// `details.ns`.
    pub ns: Option<Rc<str>>,
    /// `details.sdl_type`.
    pub sdl_type: Option<TypeId>,
    /// `details.map`.
    pub map: Option<Rc<Mapping>>,
    /// `to_xml`.
    pub to_xml: ToXml,
    /// `to_zval`.
    pub to_zval: ToZval,
}

impl Encoder {
    /// `{ns}type` (`details.clark_notation`).
    #[must_use]
    pub fn clark_notation(&self) -> Option<String> {
        match (&self.ns, &self.type_str) {
            (Some(ns), Some(t)) => Some(format!("{{{ns}}}{t}")),
            _ => None,
        }
    }

    /// `soap_type_name`.
    #[must_use]
    pub fn type_name(&self) -> &str {
        self.type_str.as_deref().unwrap_or("unknown")
    }

    /// An SDL encoder created by the schema loader (`create_encoder`).
    #[must_use]
    pub fn sdl(ns: &str, type_str: &str, sdl_type: Option<TypeId>) -> Self {
        Self {
            type_id: 0,
            type_str: Some(type_str.into()),
            ns: Some(ns.into()),
            sdl_type,
            map: None,
            to_xml: ToXml::SdlGuess,
            to_zval: ToZval::SdlGuess,
        }
    }
}

/// Reference to an encoder; equality is identity of the C pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncRef {
    /// Entry of the built-in table.
    Default(u16),
    /// Encoder owned by the SDL.
    Sdl(u32),
    /// A built-in XSD encoder re-registered under a SOAP-ENC namespace by
    /// `get_encoder` (`true` for SOAP 1.2 encoding).
    Alias(u16, bool),
    /// Entry of the active `typemap`.
    Map(u32),
}

/// One row of `defaultEncoding[]`.
#[derive(Debug, Clone, Copy)]
pub struct DefaultEncoder {
    /// `details.type`.
    pub type_id: i64,
    /// `details.type_str`.
    pub type_str: Option<&'static str>,
    /// `details.ns`.
    pub ns: Option<&'static str>,
    /// `to_zval`.
    pub to_zval: ToZval,
    /// `to_xml`.
    pub to_xml: ToXml,
}

macro_rules! def {
    ($t:expr, $s:expr, $ns:expr, $z:ident, $x:ident) => {
        DefaultEncoder {
            type_id: $t,
            type_str: $s,
            ns: $ns,
            to_zval: ToZval::$z,
            to_xml: ToXml::$x,
        }
    };
}

const XSD: Option<&str> = Some(XSD_NAMESPACE);
const X99: Option<&str> = Some(XSD_1999_NAMESPACE);
const E11: Option<&str> = Some(SOAP_1_1_ENC_NAMESPACE);
const E12: Option<&str> = Some(SOAP_1_2_ENC_NAMESPACE);

/// `defaultEncoding[]` (without the `END_KNOWN_TYPES` sentinel).
pub static DEFAULT_ENCODERS: &[DefaultEncoder] = &[
    def!(UNKNOWN_TYPE, None, None, Guess, Guess),
    def!(IS_NULL, Some("nil"), Some(XSI_NAMESPACE), Null, Null),
    def!(IS_STRING, Some("string"), XSD, String, String),
    def!(IS_LONG, Some("int"), XSD, Long, Long),
    def!(IS_DOUBLE, Some("float"), XSD, Double, Double),
    def!(IS_FALSE, Some("boolean"), XSD, Bool, Bool),
    def!(IS_TRUE, Some("boolean"), XSD, Bool, Bool),
    def!(IS_ARRAY, Some("Array"), E11, Array, GuessArrayMap),
    def!(IS_OBJECT, Some("Struct"), E11, Object, Object),
    def!(IS_ARRAY, Some("Array"), E12, Array, GuessArrayMap),
    def!(IS_OBJECT, Some("Struct"), E12, Object, Object),
    def!(XSD_STRING, Some("string"), XSD, String, String),
    def!(XSD_BOOLEAN, Some("boolean"), XSD, Bool, Bool),
    def!(XSD_DECIMAL, Some("decimal"), XSD, StringC, String),
    def!(XSD_FLOAT, Some("float"), XSD, Double, Double),
    def!(XSD_DOUBLE, Some("double"), XSD, Double, Double),
    def!(XSD_DATETIME, Some("dateTime"), XSD, StringC, DateTime),
    def!(XSD_TIME, Some("time"), XSD, StringC, Time),
    def!(XSD_DATE, Some("date"), XSD, StringC, Date),
    def!(XSD_GYEARMONTH, Some("gYearMonth"), XSD, StringC, GYearMonth),
    def!(XSD_GYEAR, Some("gYear"), XSD, StringC, GYear),
    def!(XSD_GMONTHDAY, Some("gMonthDay"), XSD, StringC, GMonthDay),
    def!(XSD_GDAY, Some("gDay"), XSD, StringC, GDay),
    def!(XSD_GMONTH, Some("gMonth"), XSD, StringC, GMonth),
    def!(XSD_DURATION, Some("duration"), XSD, StringC, Duration),
    def!(XSD_HEXBINARY, Some("hexBinary"), XSD, HexBin, HexBin),
    def!(XSD_BASE64BINARY, Some("base64Binary"), XSD, Base64, Base64),
    def!(XSD_LONG, Some("long"), XSD, Long, Long),
    def!(XSD_INT, Some("int"), XSD, Long, Long),
    def!(XSD_SHORT, Some("short"), XSD, Long, Long),
    def!(XSD_BYTE, Some("byte"), XSD, Long, Long),
    def!(
        XSD_NONPOSITIVEINTEGER,
        Some("nonPositiveInteger"),
        XSD,
        Long,
        Long
    ),
    def!(
        XSD_POSITIVEINTEGER,
        Some("positiveInteger"),
        XSD,
        Long,
        Long
    ),
    def!(
        XSD_NONNEGATIVEINTEGER,
        Some("nonNegativeInteger"),
        XSD,
        Long,
        Long
    ),
    def!(
        XSD_NEGATIVEINTEGER,
        Some("negativeInteger"),
        XSD,
        Long,
        Long
    ),
    def!(XSD_UNSIGNEDBYTE, Some("unsignedByte"), XSD, Long, Long),
    def!(XSD_UNSIGNEDSHORT, Some("unsignedShort"), XSD, Long, Long),
    def!(XSD_UNSIGNEDINT, Some("unsignedInt"), XSD, Long, Long),
    def!(XSD_UNSIGNEDLONG, Some("unsignedLong"), XSD, Long, Long),
    def!(XSD_INTEGER, Some("integer"), XSD, Long, Long),
    def!(XSD_ANYTYPE, Some("anyType"), XSD, Guess, Guess),
    def!(XSD_UR_TYPE, Some("ur-type"), XSD, Guess, Guess),
    def!(XSD_ANYURI, Some("anyURI"), XSD, StringC, String),
    def!(XSD_QNAME, Some("QName"), XSD, StringC, String),
    def!(XSD_NOTATION, Some("NOTATION"), XSD, StringC, String),
    def!(
        XSD_NORMALIZEDSTRING,
        Some("normalizedString"),
        XSD,
        StringR,
        String
    ),
    def!(XSD_TOKEN, Some("token"), XSD, StringC, String),
    def!(XSD_LANGUAGE, Some("language"), XSD, StringC, String),
    def!(XSD_NMTOKEN, Some("NMTOKEN"), XSD, StringC, String),
    def!(XSD_NMTOKENS, Some("NMTOKENS"), XSD, StringC, List1),
    def!(XSD_NAME, Some("Name"), XSD, StringC, String),
    def!(XSD_NCNAME, Some("NCName"), XSD, StringC, String),
    def!(XSD_ID, Some("ID"), XSD, StringC, String),
    def!(XSD_IDREF, Some("IDREF"), XSD, StringC, String),
    def!(XSD_IDREFS, Some("IDREFS"), XSD, StringC, List1),
    def!(XSD_ENTITY, Some("ENTITY"), XSD, StringC, String),
    def!(XSD_ENTITIES, Some("ENTITIES"), XSD, StringC, List1),
    def!(APACHE_MAP, Some("Map"), Some(APACHE_NAMESPACE), Map, Map),
    def!(SOAP_ENC_OBJECT, Some("Struct"), E11, Object, Object),
    def!(SOAP_ENC_ARRAY, Some("Array"), E11, Array, Array),
    def!(SOAP_ENC_OBJECT, Some("Struct"), E12, Object, Object),
    def!(SOAP_ENC_ARRAY, Some("Array"), E12, Array, Array),
    def!(XSD_STRING, Some("string"), X99, String, String),
    def!(XSD_BOOLEAN, Some("boolean"), X99, Bool, Bool),
    def!(XSD_DECIMAL, Some("decimal"), X99, StringC, String),
    def!(XSD_FLOAT, Some("float"), X99, Double, Double),
    def!(XSD_DOUBLE, Some("double"), X99, Double, Double),
    def!(XSD_LONG, Some("long"), X99, Long, Long),
    def!(XSD_INT, Some("int"), X99, Long, Long),
    def!(XSD_SHORT, Some("short"), X99, Long, Long),
    def!(XSD_BYTE, Some("byte"), X99, Long, Long),
    def!(
        XSD_1999_TIMEINSTANT,
        Some("timeInstant"),
        X99,
        StringC,
        String
    ),
    def!(XSD_ANYXML, Some("<anyXML>"), Some("<anyXML>"), Any, Any),
];

impl DefaultEncoder {
    /// Owned copy.
    #[must_use]
    pub fn to_encoder(&self) -> Encoder {
        Encoder {
            type_id: self.type_id,
            type_str: self.type_str.map(Rc::from),
            ns: self.ns.map(Rc::from),
            sdl_type: None,
            map: None,
            to_xml: self.to_xml,
            to_zval: self.to_zval,
        }
    }
}

fn idx(i: usize) -> u16 {
    u16::try_from(i).unwrap_or(u16::MAX)
}

/// `php_soap_defEnc` lookup by `ns:type` (or bare type when the entry
/// has no namespace); the first matching row wins.
#[must_use]
pub fn default_by_nscat(nscat: &str) -> Option<u16> {
    DEFAULT_ENCODERS
        .iter()
        .position(|e| match (e.ns, e.type_str) {
            (Some(ns), Some(t)) => {
                nscat.len() == ns.len() + 1 + t.len()
                    && nscat.starts_with(ns)
                    && nscat.as_bytes()[ns.len()] == b':'
                    && nscat.ends_with(t)
            }
            (None, Some(t)) => nscat == t,
            _ => false,
        })
        .map(idx)
}

/// `php_soap_defEncIndex` lookup by type code; the first row wins.
#[must_use]
pub fn default_by_type(type_id: i64) -> Option<u16> {
    DEFAULT_ENCODERS
        .iter()
        .position(|e| e.type_id == type_id)
        .map(idx)
}

/// `get_conversion(type)`, panicking only for codes that are always
/// present in the table.
#[must_use]
pub fn conv(type_id: i64) -> EncRef {
    EncRef::Default(default_by_type(type_id).unwrap_or(0))
}

/// Prefixes of well-known namespaces (`php_soap_defEncNs`).
#[must_use]
pub fn default_ns_prefix(ns: &[u8]) -> Option<&'static str> {
    [
        (XSD_1999_NAMESPACE, XSD_NS_PREFIX),
        (XSD_NAMESPACE, XSD_NS_PREFIX),
        (XSI_NAMESPACE, XSI_NS_PREFIX),
        (XML_NAMESPACE, XML_NS_PREFIX),
        (SOAP_1_1_ENC_NAMESPACE, SOAP_1_1_ENC_NS_PREFIX),
        (SOAP_1_2_ENC_NAMESPACE, SOAP_1_2_ENC_NS_PREFIX),
    ]
    .iter()
    .find(|(n, _)| n.as_bytes() == ns)
    .map(|&(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_row_wins() {
        let s = default_by_nscat("http://www.w3.org/2001/XMLSchema:string").unwrap();
        assert_eq!(DEFAULT_ENCODERS[s as usize].type_id, IS_STRING);
        let i = default_by_nscat("http://www.w3.org/2001/XMLSchema:int").unwrap();
        assert_eq!(DEFAULT_ENCODERS[i as usize].type_id, IS_LONG);
        let a = default_by_nscat("http://schemas.xmlsoap.org/soap/encoding/:Array").unwrap();
        assert_eq!(DEFAULT_ENCODERS[a as usize].to_xml, ToXml::GuessArrayMap);
        assert_eq!(
            DEFAULT_ENCODERS[default_by_type(SOAP_ENC_ARRAY).unwrap() as usize].to_xml,
            ToXml::Array
        );
        assert!(default_by_nscat("<anyXML>:<anyXML>").is_some());
        assert!(default_by_nscat("int").is_none());
        assert_eq!(default_ns_prefix(XSI_NAMESPACE.as_bytes()), Some("xsi"));
    }
}
