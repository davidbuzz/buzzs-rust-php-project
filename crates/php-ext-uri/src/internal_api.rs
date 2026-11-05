//! The engine-facing URI parser API of `ext/uri/php_uri.c`
//! (`php_uri_get_parser`, `php_uri_parse`, the `php_uri_get_*` component
//! readers and `php_uri_parse_to_struct`) over the three registered parsers
//! (`uri_parser_rfc3986.c`, `uri_parser_whatwg.c` and
//! `uri_parser_php_parse_url.c`). ext/zend_test's `zend_test_uri_parser()`
//! is its only PHP-visible user.

use php_runtime::{Value, ZArray};
use php_stdstr::url::{parse_url_ex, rawurldecode, ParsedUrl};
use php_uri::api::{Rfc3986Uri, WhatWgUrl};
use php_uri::UriError;
use php_vm::ext::VmResult;
use php_vm::Vm;

use crate::common::{opt_bytes, opt_int, throw};

/// `PHP_URI_PARSER_RFC3986`.
const PARSER_RFC3986: &[u8] = b"Uri\\Rfc3986\\Uri";
/// `PHP_URI_PARSER_WHATWG`.
const PARSER_WHATWG: &[u8] = b"Uri\\WhatWg\\Url";
/// `PHP_URI_PARSER_PHP_PARSE_URL`.
const PARSER_PHP_PARSE_URL: &[u8] = b"parse_url";

/// `php_uri_component_read_mode` (the modes `zend_test_uri_parser` uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadMode {
    /// `PHP_URI_COMPONENT_READ_MODE_RAW`.
    Raw,
    /// `PHP_URI_COMPONENT_READ_MODE_NORMALIZED_ASCII`.
    NormalizedAscii,
}

/// A `php_uri_internal`: the parser's own representation.
enum Parsed {
    Rfc3986(Rfc3986Uri),
    WhatWg(WhatWgUrl),
    ParseUrl(ParsedUrl),
}

/// The eight components, in `php_uri` field order.
#[derive(Debug, Default, PartialEq, Eq)]
struct Components {
    scheme: Option<Vec<u8>>,
    username: Option<Vec<u8>>,
    password: Option<Vec<u8>>,
    host: Option<Vec<u8>>,
    port: Option<i64>,
    path: Option<Vec<u8>>,
    query: Option<Vec<u8>>,
    fragment: Option<Vec<u8>>,
}

/// `php_uri_get_parser()`: whether `name` is a registered parser.
pub fn is_parser(name: &[u8]) -> bool {
    matches!(name, PARSER_RFC3986 | PARSER_WHATWG | PARSER_PHP_PARSE_URL)
}

/// `php_uri_parse(parser, uri, silent: false)`; `parser` must satisfy
/// [`is_parser`].
fn parse(parser: &[u8], uri: &[u8]) -> Result<Parsed, UriError> {
    match parser {
        PARSER_RFC3986 => Rfc3986Uri::new(uri, None).map(Parsed::Rfc3986),
        PARSER_WHATWG => WhatWgUrl::new(uri, None).map(|(url, _)| Parsed::WhatWg(url)),
        _ => parse_url_ex(uri)
            .map(Parsed::ParseUrl)
            .ok_or_else(|| UriError::invalid_uri("The specified URI is malformed")),
    }
}

/// `decode_component()` of the `parse_url` parser.
fn decode(c: Option<&[u8]>, mode: ReadMode) -> Option<Vec<u8>> {
    c.map(|c| match mode {
        ReadMode::Raw => c.to_vec(),
        ReadMode::NormalizedAscii => rawurldecode(c),
    })
}

impl Parsed {
    /// The `php_uri_get_*` readers in `mode`.
    fn components(&self, mode: ReadMode) -> Components {
        let raw = mode == ReadMode::Raw;
        match self {
            Parsed::Rfc3986(u) if raw => Components {
                scheme: u.raw_scheme(),
                username: u.raw_username(),
                password: u.raw_password(),
                host: u.raw_host(),
                port: u.port(),
                path: Some(u.raw_path()),
                query: u.raw_query(),
                fragment: u.raw_fragment(),
            },
            Parsed::Rfc3986(u) => Components {
                scheme: u.scheme(),
                username: u.username(),
                password: u.password(),
                host: u.host(),
                port: u.port(),
                path: Some(u.path()),
                query: u.query(),
                fragment: u.fragment(),
            },
            // Only the host reader distinguishes the modes, and RAW reads
            // the ASCII host.
            Parsed::WhatWg(u) => Components {
                scheme: Some(u.scheme()),
                username: u.username(),
                password: u.password(),
                host: u.ascii_host(),
                port: u.port(),
                path: Some(u.path()),
                query: u.query(),
                fragment: u.fragment(),
            },
            Parsed::ParseUrl(u) => Components {
                scheme: decode(u.scheme.as_deref(), mode),
                username: decode(u.user.as_deref(), mode),
                password: decode(u.pass.as_deref(), mode),
                host: decode(u.host.as_deref(), mode),
                // `if (parse_url_uri->port)`: port 0 reads as null.
                port: u.port.filter(|&p| p != 0).map(i64::from),
                path: decode(u.path.as_deref(), mode),
                query: decode(u.query.as_deref(), mode),
                fragment: decode(u.fragment.as_deref(), mode),
            },
        }
    }
}

impl Components {
    /// The `[scheme, username, ..., fragment]` array; `struct_port` gives
    /// the `php_uri` struct's non-nullable `zend_long port` (0 when absent).
    fn into_array(self, struct_port: bool) -> Value {
        let mut arr = ZArray::new();
        let t = arr.make_mut();
        t.insert("scheme", opt_bytes(self.scheme));
        t.insert("username", opt_bytes(self.username));
        t.insert("password", opt_bytes(self.password));
        t.insert("host", opt_bytes(self.host));
        let port = if struct_port {
            Value::Int(self.port.unwrap_or(0))
        } else {
            opt_int(self.port)
        };
        t.insert("port", port);
        t.insert("path", opt_bytes(self.path));
        t.insert("query", opt_bytes(self.query));
        t.insert("fragment", opt_bytes(self.fragment));
        Value::Array(arr)
    }
}

/// `zend_test_uri_parser($uri, $parser)` once the parser name is known to
/// be registered ([`is_parser`]): the `normalized` (`NORMALIZED_ASCII`)
/// and `raw` component arrays plus the `php_uri_parse_to_struct()` (raw)
/// `struct`. A parse failure throws the parser's exception.
pub fn zend_test_uri_parser(vm: &mut Vm, uri: &[u8], parser: &[u8]) -> VmResult<Value> {
    let parsed = parse(parser, uri).map_err(|e| throw(vm, &e))?;
    let mut out = ZArray::new();
    let t = out.make_mut();
    t.insert(
        "normalized",
        parsed
            .components(ReadMode::NormalizedAscii)
            .into_array(false),
    );
    t.insert("raw", parsed.components(ReadMode::Raw).into_array(false));
    t.insert("struct", parsed.components(ReadMode::Raw).into_array(true));
    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::{is_parser, parse, ReadMode};

    #[test]
    fn registered_parsers() {
        assert!(is_parser(b"Uri\\Rfc3986\\Uri"));
        assert!(is_parser(b"Uri\\WhatWg\\Url"));
        assert!(is_parser(b"parse_url"));
        assert!(!is_parser(b"uri\\rfc3986\\uri"));
    }

    #[test]
    fn parse_url_parser_decodes_normalized_components() {
        let Ok(p) = parse(
            b"parse_url",
            b"https://%65x:%65y@%65z.com:123/%65.html?%65=1#%65",
        ) else {
            panic!("parse_url parses it");
        };
        let n = p.components(ReadMode::NormalizedAscii);
        assert_eq!(n.username.as_deref(), Some(&b"ex"[..]));
        assert_eq!(n.host.as_deref(), Some(&b"ez.com"[..]));
        assert_eq!(n.port, Some(123));
        let r = p.components(ReadMode::Raw);
        assert_eq!(r.path.as_deref(), Some(&b"/%65.html"[..]));
    }

    #[test]
    fn rfc3986_large_port_and_whatwg_failure() {
        let Ok(p) = parse(b"Uri\\Rfc3986\\Uri", b"https://example.com:42424242") else {
            panic!("RFC 3986 accepts large ports");
        };
        assert_eq!(p.components(ReadMode::Raw).port, Some(42_424_242));
        let Err(e) = parse(b"Uri\\WhatWg\\Url", b"invalid uri") else {
            panic!("WHATWG rejects it");
        };
        assert_eq!(
            e.message,
            "The specified URI is malformed (MissingSchemeNonRelativeUrl)"
        );
        assert_eq!(e.errors.len(), 1);
    }
}
