//! Percent-encode sets (lexbor's `lxb_url_map`) and the UTF-8 percent
//! encoder.

/// C0 control percent-encode set.
pub const MAP_C0: u8 = 0x01;
/// Fragment percent-encode set.
pub const MAP_FRAGMENT: u8 = 0x02;
/// Query percent-encode set.
pub const MAP_QUERY: u8 = 0x04;
/// Special-query percent-encode set.
pub const MAP_SPECIAL_QUERY: u8 = 0x08;
/// Path percent-encode set.
pub const MAP_PATH: u8 = 0x10;
/// Userinfo percent-encode set.
pub const MAP_USERINFO: u8 = 0x20;
/// Component percent-encode set.
pub const MAP_COMPONENT: u8 = 0x40;
/// `application/x-www-form-urlencoded` percent-encode set.
pub const MAP_X_WWW_FORM: u8 = 0x80;

/// For each byte, the bit set of percent-encode sets containing it.
pub static URL_MAP: [u8; 256] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFE, 0x80, 0xFE, 0xFC, 0xC0, 0x80, 0xC0, 0x88, 0x80, 0x80, 0x00, 0xC0, 0xC0, 0x00, 0x00, 0xE0,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE0, 0xE0, 0xFE, 0xE0, 0xFE, 0xF0,
    0xE0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE0, 0xE0, 0xE0, 0xF0, 0x00,
    0xF2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0xE0, 0xF0, 0x80, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Appends `input` to `out`, percent-encoding every byte whose `map` entry
/// intersects `set` (`lxb_url_percent_encode_after_utf_8`).
pub fn percent_encode_into(
    input: &[u8],
    out: &mut Vec<u8>,
    map: &[u8; 256],
    set: u8,
    space_as_plus: bool,
) {
    out.reserve(input.len());
    for &c in input {
        if space_as_plus && c == b' ' {
            out.push(b'+');
        } else if map[usize::from(c)] & set != 0 {
            out.push(b'%');
            out.push(HEX_UPPER[usize::from(c >> 4)]);
            out.push(HEX_UPPER[usize::from(c & 15)]);
        } else {
            out.push(c);
        }
    }
}

/// Percent-encodes `input` with lexbor's map.
pub fn percent_encode(input: &[u8], set: u8) -> Vec<u8> {
    let mut out = Vec::new();
    percent_encode_into(input, &mut out, &URL_MAP, set, false);
    out
}

/// `Uri\WhatWg\UrlPercentEncodingMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlPercentEncodingMode {
    Username,
    Password,
    OpaqueHost,
    Path,
    OpaquePath,
    PathSegment,
    Query,
    SpecialQuery,
    FormQuery,
    Fragment,
}

impl UrlPercentEncodingMode {
    /// All cases in declaration order.
    pub const ALL: [UrlPercentEncodingMode; 10] = [
        Self::Username,
        Self::Password,
        Self::OpaqueHost,
        Self::Path,
        Self::OpaquePath,
        Self::PathSegment,
        Self::Query,
        Self::SpecialQuery,
        Self::FormQuery,
        Self::Fragment,
    ];

    /// The enum case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Username => "Username",
            Self::Password => "Password",
            Self::OpaqueHost => "OpaqueHost",
            Self::Path => "Path",
            Self::OpaquePath => "OpaquePath",
            Self::PathSegment => "PathSegment",
            Self::Query => "Query",
            Self::SpecialQuery => "SpecialQuery",
            Self::FormQuery => "FormQuery",
            Self::Fragment => "Fragment",
        }
    }

    /// Looks up a case by name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.name() == name)
    }
}

/// `Uri\WhatWg\url_percent_encode()`: PHP uses a copy of lexbor's map in
/// which `%` belongs to every set, and adds `/` to the path set for
/// `PathSegment`.
pub fn url_percent_encode(input: &[u8], mode: UrlPercentEncodingMode) -> Vec<u8> {
    let mut map = URL_MAP;
    map[usize::from(b'%')] = 0xFF;
    let (set, plus) = match mode {
        UrlPercentEncodingMode::Username | UrlPercentEncodingMode::Password => {
            (MAP_USERINFO, false)
        }
        UrlPercentEncodingMode::OpaqueHost | UrlPercentEncodingMode::OpaquePath => (MAP_C0, false),
        UrlPercentEncodingMode::Path => (MAP_PATH, false),
        UrlPercentEncodingMode::PathSegment => {
            map[usize::from(b'/')] |= MAP_PATH;
            (MAP_PATH, false)
        }
        UrlPercentEncodingMode::Query => (MAP_QUERY, false),
        UrlPercentEncodingMode::SpecialQuery => (MAP_SPECIAL_QUERY, false),
        UrlPercentEncodingMode::FormQuery => (MAP_X_WWW_FORM, true),
        UrlPercentEncodingMode::Fragment => (MAP_FRAGMENT, false),
    };
    let mut out = Vec::new();
    percent_encode_into(input, &mut out, &map, set, plus);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets() {
        assert_eq!(
            percent_encode(b"a b\"#<>`{}", MAP_FRAGMENT),
            b"a%20b%22#%3C%3E%60{}"
        );
        assert_eq!(percent_encode(b"/?^{", MAP_PATH), b"/%3F%5E%7B");
        assert_eq!(percent_encode(b"'", MAP_SPECIAL_QUERY), b"%27");
        assert_eq!(percent_encode(b"'", MAP_QUERY), b"'");
        assert_eq!(percent_encode("é".as_bytes(), MAP_C0), b"%C3%A9");
        assert_eq!(percent_encode(b"a:b@c", MAP_USERINFO), b"a%3Ab%40c");
    }

    #[test]
    fn php_function() {
        assert_eq!(
            url_percent_encode(b"a%b", UrlPercentEncodingMode::OpaqueHost),
            b"a%25b"
        );
        assert_eq!(
            url_percent_encode(b"a/b", UrlPercentEncodingMode::Path),
            b"a/b"
        );
        assert_eq!(
            url_percent_encode(b"a/b", UrlPercentEncodingMode::PathSegment),
            b"a%2Fb"
        );
        assert_eq!(
            url_percent_encode(b"a b*", UrlPercentEncodingMode::FormQuery),
            b"a+b*"
        );
    }
}
