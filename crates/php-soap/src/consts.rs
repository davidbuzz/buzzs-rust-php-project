// Derived from php-src ext/soap/php_encoding.h. Similarity metric to original: 39%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! Namespaces, type codes and the PHP-visible constants of ext/soap
//! (`php_soap.h`, `php_encoding.h`, `php_sdl.h`, `soap.stub.php`).

#![allow(missing_docs)]

use php_runtime::Value;

pub const SOAP_1_1: i64 = 1;
pub const SOAP_1_2: i64 = 2;

pub const SOAP_PERSISTENCE_SESSION: i64 = 1;
pub const SOAP_PERSISTENCE_REQUEST: i64 = 2;
pub const SOAP_FUNCTIONS_ALL: i64 = 999;

pub const SOAP_ENCODED: i64 = 1;
pub const SOAP_LITERAL: i64 = 2;
pub const SOAP_RPC: i64 = 1;
pub const SOAP_DOCUMENT: i64 = 2;

pub const SOAP_ACTOR_NEXT: i64 = 1;
pub const SOAP_ACTOR_NONE: i64 = 2;
pub const SOAP_ACTOR_UNLIMATERECEIVER: i64 = 3;

pub const SOAP_1_1_ACTOR_NEXT: &str = "http://schemas.xmlsoap.org/soap/actor/next";
pub const SOAP_1_2_ACTOR_NEXT: &str = "http://www.w3.org/2003/05/soap-envelope/role/next";
pub const SOAP_1_2_ACTOR_NONE: &str = "http://www.w3.org/2003/05/soap-envelope/role/none";
pub const SOAP_1_2_ACTOR_UNLIMATERECEIVER: &str =
    "http://www.w3.org/2003/05/soap-envelope/role/ultimateReceiver";

pub const SOAP_COMPRESSION_ACCEPT: i64 = 0x20;
pub const SOAP_COMPRESSION_GZIP: i64 = 0x00;
pub const SOAP_COMPRESSION_DEFLATE: i64 = 0x10;

pub const SOAP_AUTHENTICATION_BASIC: i64 = 0;
pub const SOAP_AUTHENTICATION_DIGEST: i64 = 1;

pub const SOAP_SINGLE_ELEMENT_ARRAYS: i64 = 1 << 0;
pub const SOAP_WAIT_ONE_WAY_CALLS: i64 = 1 << 1;
pub const SOAP_USE_XSI_ARRAY_TYPE: i64 = 1 << 2;

pub const WSDL_CACHE_NONE: i64 = 0x0;
pub const WSDL_CACHE_DISK: i64 = 0x1;
pub const WSDL_CACHE_MEMORY: i64 = 0x2;
pub const WSDL_CACHE_BOTH: i64 = 0x3;

pub const SOAP_SSL_METHOD_TLS: i64 = 0;
pub const SOAP_SSL_METHOD_SSLV2: i64 = 1;
pub const SOAP_SSL_METHOD_SSLV3: i64 = 2;
pub const SOAP_SSL_METHOD_SSLV23: i64 = 3;

pub const SOAP_MAX_XML_DEPTH: usize = 2048;
pub const SOAP_MAX_DECODE_DEPTH: u32 = 4096;

pub const WSDL_HEADER_KEEP_AUTHORIZATION: i64 = 1 << 0;
pub const WSDL_HEADER_KEEP_PROXY_AUTHORIZATION: i64 = 1 << 1;
pub const WSDL_HEADER_KEEP_COOKIES: i64 = 1 << 2;

// ---- namespaces -----------------------------------------------------------

pub const XSD_1999_NAMESPACE: &str = "http://www.w3.org/1999/XMLSchema";
pub const SOAP_1_1_ENV_NAMESPACE: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const SOAP_1_1_ENV_NS_PREFIX: &str = "SOAP-ENV";
pub const SOAP_1_2_ENV_NAMESPACE: &str = "http://www.w3.org/2003/05/soap-envelope";
pub const SOAP_1_2_ENV_NS_PREFIX: &str = "env";
pub const SOAP_1_1_ENC_NAMESPACE: &str = "http://schemas.xmlsoap.org/soap/encoding/";
pub const SOAP_1_1_ENC_NS_PREFIX: &str = "SOAP-ENC";
pub const SOAP_1_2_ENC_NAMESPACE: &str = "http://www.w3.org/2003/05/soap-encoding";
pub const SOAP_1_2_ENC_NS_PREFIX: &str = "enc";
pub const XSD_DRAFT_2000_NAMESPACE: &str = "http://www.w3.org/2000/10/XMLSchema";
pub const XSD_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";
pub const XSD_NS_PREFIX: &str = "xsd";
pub const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub const XSI_NS_PREFIX: &str = "xsi";
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
pub const XML_NS_PREFIX: &str = "xml";
pub const APACHE_NAMESPACE: &str = "http://xml.apache.org/xml-soap";
pub const WSDL_NAMESPACE: &str = "http://schemas.xmlsoap.org/wsdl/";
pub const WSDL_SOAP11_NAMESPACE: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
pub const WSDL_SOAP12_NAMESPACE: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
pub const RPC_SOAP12_NAMESPACE: &str = "http://www.w3.org/2003/05/soap-rpc";
pub const RPC_SOAP12_NS_PREFIX: &str = "rpc";
pub const WSDL_HTTP11_NAMESPACE: &str = "http://schemas.xmlsoap.org/wsdl/http/";
pub const WSDL_HTTP12_NAMESPACE: &str = "http://www.w3.org/2003/05/soap/bindings/HTTP/";
pub const WSDL_HTTP_TRANSPORT: &str = "http://schemas.xmlsoap.org/soap/http";

// ---- type codes -----------------------------------------------------------

/// `IS_NULL` .. `IS_OBJECT` as used by the encoder table.
pub const IS_NULL: i64 = 1;
pub const IS_FALSE: i64 = 2;
pub const IS_TRUE: i64 = 3;
pub const IS_LONG: i64 = 4;
pub const IS_DOUBLE: i64 = 5;
pub const IS_STRING: i64 = 6;
pub const IS_ARRAY: i64 = 7;
pub const IS_OBJECT: i64 = 8;
pub const IS_RESOURCE: i64 = 9;
pub const IS_REFERENCE: i64 = 10;
pub const IS_UNDEF: i64 = 0;

pub const XSD_STRING: i64 = 101;
pub const XSD_BOOLEAN: i64 = 102;
pub const XSD_DECIMAL: i64 = 103;
pub const XSD_FLOAT: i64 = 104;
pub const XSD_DOUBLE: i64 = 105;
pub const XSD_DURATION: i64 = 106;
pub const XSD_DATETIME: i64 = 107;
pub const XSD_TIME: i64 = 108;
pub const XSD_DATE: i64 = 109;
pub const XSD_GYEARMONTH: i64 = 110;
pub const XSD_GYEAR: i64 = 111;
pub const XSD_GMONTHDAY: i64 = 112;
pub const XSD_GDAY: i64 = 113;
pub const XSD_GMONTH: i64 = 114;
pub const XSD_HEXBINARY: i64 = 115;
pub const XSD_BASE64BINARY: i64 = 116;
pub const XSD_ANYURI: i64 = 117;
pub const XSD_QNAME: i64 = 118;
pub const XSD_NOTATION: i64 = 119;
pub const XSD_NORMALIZEDSTRING: i64 = 120;
pub const XSD_TOKEN: i64 = 121;
pub const XSD_LANGUAGE: i64 = 122;
pub const XSD_NMTOKEN: i64 = 123;
pub const XSD_NAME: i64 = 124;
pub const XSD_NCNAME: i64 = 125;
pub const XSD_ID: i64 = 126;
pub const XSD_IDREF: i64 = 127;
pub const XSD_IDREFS: i64 = 128;
pub const XSD_ENTITY: i64 = 129;
pub const XSD_ENTITIES: i64 = 130;
pub const XSD_INTEGER: i64 = 131;
pub const XSD_NONPOSITIVEINTEGER: i64 = 132;
pub const XSD_NEGATIVEINTEGER: i64 = 133;
pub const XSD_LONG: i64 = 134;
pub const XSD_INT: i64 = 135;
pub const XSD_SHORT: i64 = 136;
pub const XSD_BYTE: i64 = 137;
pub const XSD_NONNEGATIVEINTEGER: i64 = 138;
pub const XSD_UNSIGNEDLONG: i64 = 139;
pub const XSD_UNSIGNEDINT: i64 = 140;
pub const XSD_UNSIGNEDSHORT: i64 = 141;
pub const XSD_UNSIGNEDBYTE: i64 = 142;
pub const XSD_POSITIVEINTEGER: i64 = 143;
pub const XSD_NMTOKENS: i64 = 144;
pub const XSD_ANYTYPE: i64 = 145;
pub const XSD_UR_TYPE: i64 = 146;
pub const XSD_ANYXML: i64 = 147;
pub const XSD_1999_TIMEINSTANT: i64 = 401;
pub const APACHE_MAP: i64 = 200;
pub const SOAP_ENC_ARRAY: i64 = 300;
pub const SOAP_ENC_OBJECT: i64 = 301;
pub const UNKNOWN_TYPE: i64 = 999_998;
pub const END_KNOWN_TYPES: i64 = 999_999;

/// Every constant registered by the extension, in `soap.stub.php` order.
#[must_use]
pub fn php_constants() -> Vec<(&'static str, Value)> {
    let ints: &[(&str, i64)] = &[
        ("SOAP_1_1", SOAP_1_1),
        ("SOAP_1_2", SOAP_1_2),
        ("SOAP_PERSISTENCE_SESSION", SOAP_PERSISTENCE_SESSION),
        ("SOAP_PERSISTENCE_REQUEST", SOAP_PERSISTENCE_REQUEST),
        ("SOAP_FUNCTIONS_ALL", SOAP_FUNCTIONS_ALL),
        ("SOAP_ENCODED", SOAP_ENCODED),
        ("SOAP_LITERAL", SOAP_LITERAL),
        ("SOAP_RPC", SOAP_RPC),
        ("SOAP_DOCUMENT", SOAP_DOCUMENT),
        ("SOAP_ACTOR_NEXT", SOAP_ACTOR_NEXT),
        ("SOAP_ACTOR_NONE", SOAP_ACTOR_NONE),
        ("SOAP_ACTOR_UNLIMATERECEIVER", SOAP_ACTOR_UNLIMATERECEIVER),
        ("SOAP_COMPRESSION_ACCEPT", SOAP_COMPRESSION_ACCEPT),
        ("SOAP_COMPRESSION_GZIP", SOAP_COMPRESSION_GZIP),
        ("SOAP_COMPRESSION_DEFLATE", SOAP_COMPRESSION_DEFLATE),
        ("SOAP_AUTHENTICATION_BASIC", SOAP_AUTHENTICATION_BASIC),
        ("SOAP_AUTHENTICATION_DIGEST", SOAP_AUTHENTICATION_DIGEST),
        ("UNKNOWN_TYPE", UNKNOWN_TYPE),
        ("XSD_STRING", XSD_STRING),
        ("XSD_BOOLEAN", XSD_BOOLEAN),
        ("XSD_DECIMAL", XSD_DECIMAL),
        ("XSD_FLOAT", XSD_FLOAT),
        ("XSD_DOUBLE", XSD_DOUBLE),
        ("XSD_DURATION", XSD_DURATION),
        ("XSD_DATETIME", XSD_DATETIME),
        ("XSD_TIME", XSD_TIME),
        ("XSD_DATE", XSD_DATE),
        ("XSD_GYEARMONTH", XSD_GYEARMONTH),
        ("XSD_GYEAR", XSD_GYEAR),
        ("XSD_GMONTHDAY", XSD_GMONTHDAY),
        ("XSD_GDAY", XSD_GDAY),
        ("XSD_GMONTH", XSD_GMONTH),
        ("XSD_HEXBINARY", XSD_HEXBINARY),
        ("XSD_BASE64BINARY", XSD_BASE64BINARY),
        ("XSD_ANYURI", XSD_ANYURI),
        ("XSD_QNAME", XSD_QNAME),
        ("XSD_NOTATION", XSD_NOTATION),
        ("XSD_NORMALIZEDSTRING", XSD_NORMALIZEDSTRING),
        ("XSD_TOKEN", XSD_TOKEN),
        ("XSD_LANGUAGE", XSD_LANGUAGE),
        ("XSD_NMTOKEN", XSD_NMTOKEN),
        ("XSD_NAME", XSD_NAME),
        ("XSD_NCNAME", XSD_NCNAME),
        ("XSD_ID", XSD_ID),
        ("XSD_IDREF", XSD_IDREF),
        ("XSD_IDREFS", XSD_IDREFS),
        ("XSD_ENTITY", XSD_ENTITY),
        ("XSD_ENTITIES", XSD_ENTITIES),
        ("XSD_INTEGER", XSD_INTEGER),
        ("XSD_NONPOSITIVEINTEGER", XSD_NONPOSITIVEINTEGER),
        ("XSD_NEGATIVEINTEGER", XSD_NEGATIVEINTEGER),
        ("XSD_LONG", XSD_LONG),
        ("XSD_INT", XSD_INT),
        ("XSD_SHORT", XSD_SHORT),
        ("XSD_BYTE", XSD_BYTE),
        ("XSD_NONNEGATIVEINTEGER", XSD_NONNEGATIVEINTEGER),
        ("XSD_UNSIGNEDLONG", XSD_UNSIGNEDLONG),
        ("XSD_UNSIGNEDINT", XSD_UNSIGNEDINT),
        ("XSD_UNSIGNEDSHORT", XSD_UNSIGNEDSHORT),
        ("XSD_UNSIGNEDBYTE", XSD_UNSIGNEDBYTE),
        ("XSD_POSITIVEINTEGER", XSD_POSITIVEINTEGER),
        ("XSD_NMTOKENS", XSD_NMTOKENS),
        ("XSD_ANYTYPE", XSD_ANYTYPE),
        ("XSD_ANYXML", XSD_ANYXML),
        ("APACHE_MAP", APACHE_MAP),
        ("SOAP_ENC_OBJECT", SOAP_ENC_OBJECT),
        ("SOAP_ENC_ARRAY", SOAP_ENC_ARRAY),
        ("XSD_1999_TIMEINSTANT", XSD_1999_TIMEINSTANT),
    ];
    let mut out: Vec<(&'static str, Value)> =
        ints.iter().map(|&(n, v)| (n, Value::Int(v))).collect();
    out.push(("XSD_NAMESPACE", Value::from(XSD_NAMESPACE)));
    out.push(("XSD_1999_NAMESPACE", Value::from(XSD_1999_NAMESPACE)));
    let tail: &[(&str, i64)] = &[
        ("SOAP_SINGLE_ELEMENT_ARRAYS", SOAP_SINGLE_ELEMENT_ARRAYS),
        ("SOAP_WAIT_ONE_WAY_CALLS", SOAP_WAIT_ONE_WAY_CALLS),
        ("SOAP_USE_XSI_ARRAY_TYPE", SOAP_USE_XSI_ARRAY_TYPE),
        ("WSDL_CACHE_NONE", WSDL_CACHE_NONE),
        ("WSDL_CACHE_DISK", WSDL_CACHE_DISK),
        ("WSDL_CACHE_MEMORY", WSDL_CACHE_MEMORY),
        ("WSDL_CACHE_BOTH", WSDL_CACHE_BOTH),
        ("SOAP_SSL_METHOD_TLS", SOAP_SSL_METHOD_TLS),
        ("SOAP_SSL_METHOD_SSLv2", SOAP_SSL_METHOD_SSLV2),
        ("SOAP_SSL_METHOD_SSLv3", SOAP_SSL_METHOD_SSLV3),
        ("SOAP_SSL_METHOD_SSLv23", SOAP_SSL_METHOD_SSLV23),
    ];
    out.extend(tail.iter().map(|&(n, v)| (n, Value::Int(v))));
    out
}

/// Constants carrying a `#[\Deprecated]` attribute: `(name, since, message)`.
pub const DEPRECATED_CONSTANTS: &[(&str, &str, &str)] = &[(
    "SOAP_FUNCTIONS_ALL",
    "8.4",
    "as enabling all functions is a possible security concern",
)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_table_is_complete() {
        let c = php_constants();
        assert_eq!(c.len(), 81);
        assert!(c
            .iter()
            .any(|(n, v)| *n == "XSD_ANYXML" && matches!(v, Value::Int(147))));
        assert!(c.iter().any(|(n, _)| *n == "SOAP_SSL_METHOD_SSLv23"));
    }
}
