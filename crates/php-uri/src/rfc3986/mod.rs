// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! RFC 3986 URI handling, mirroring the observable behaviour of the
//! `uriparser` library as used by PHP's `Uri\Rfc3986\Uri`.
//!
//! The data model follows `UriUriA`: optional scheme/userinfo/host/port
//! text, a list of path segments plus an "absolute path" flag (only used for
//! URIs without host), and optional query/fragment.

mod normalize;
mod parse;
mod resolve;
mod setters;

pub use normalize::{normalize, NormalizeMask};
pub use parse::{is_well_formed_ipv4, parse, parse_ipv6};
pub use resolve::{resolve, ResolveError};
pub use setters::{
    is_well_formed_fragment, is_well_formed_host_ip_future, is_well_formed_host_ipv6,
    is_well_formed_path, is_well_formed_port, is_well_formed_query, is_well_formed_reg_name,
    is_well_formed_scheme, is_well_formed_userinfo, SetError,
};

/// What kind of host a URI has (`hostData` in uriparser).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// A dotted-decimal IPv4 address (the four octets).
    Ipv4([u8; 4]),
    /// An IPv6 literal (the sixteen bytes).
    Ipv6([u8; 16]),
    /// An `IPvFuture` literal (`v1.xyz`).
    IpFuture,
    /// A registered name (possibly empty).
    RegName,
}

/// A host: its text (without brackets for IP literals) and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The host text as it appears in the URI, without `[` `]`.
    pub text: Vec<u8>,
    /// The kind of host.
    pub kind: HostKind,
}

/// A parsed RFC 3986 URI reference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Uri {
    /// Scheme without the trailing `:`.
    pub scheme: Option<Vec<u8>>,
    /// User information without the trailing `@`.
    pub user_info: Option<Vec<u8>>,
    /// The host, if the URI has an authority.
    pub host: Option<Host>,
    /// Port text (digits, possibly empty) without the leading `:`.
    pub port: Option<Vec<u8>>,
    /// Path segments.
    pub segments: Vec<Vec<u8>>,
    /// Whether the path starts with `/` (only meaningful without host).
    pub absolute_path: bool,
    /// Query without the leading `?`.
    pub query: Option<Vec<u8>>,
    /// Fragment without the leading `#`.
    pub fragment: Option<Vec<u8>>,
}

impl Uri {
    /// Whether the URI has a host (and hence an authority).
    pub fn has_host(&self) -> bool {
        self.host.is_some()
    }

    /// The path as PHP's `getPath()`/`getRawPath()` return it.
    pub fn path_string(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.segments.is_empty() {
            if self.absolute_path || self.has_host() {
                out.push(b'/');
            }
            for (i, seg) in self.segments.iter().enumerate() {
                if i > 0 {
                    out.push(b'/');
                }
                out.extend_from_slice(seg);
            }
        } else if self.absolute_path {
            out.push(b'/');
        }
        out
    }

    /// The host as PHP's `getHost()` returns it (IP literals in brackets).
    pub fn host_string(&self) -> Option<Vec<u8>> {
        self.host.as_ref().map(|h| match h.kind {
            HostKind::Ipv6(_) | HostKind::IpFuture => {
                let mut v = Vec::with_capacity(h.text.len() + 2);
                v.push(b'[');
                v.extend_from_slice(&h.text);
                v.push(b']');
                v
            }
            _ => h.text.clone(),
        })
    }

    /// Recomposes the URI (`uriToStringA`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(s) = &self.scheme {
            out.extend_from_slice(s);
            out.push(b':');
        }
        if let Some(host) = &self.host {
            out.extend_from_slice(b"//");
            if let Some(ui) = &self.user_info {
                out.extend_from_slice(ui);
                out.push(b'@');
            }
            match &host.kind {
                HostKind::Ipv4(o) => {
                    for (i, b) in o.iter().enumerate() {
                        if i > 0 {
                            out.push(b'.');
                        }
                        out.extend_from_slice(b.to_string().as_bytes());
                    }
                }
                HostKind::Ipv6(bytes) => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    out.push(b'[');
                    for (i, b) in bytes.iter().enumerate() {
                        out.push(HEX[usize::from(b >> 4)]);
                        out.push(HEX[usize::from(b & 15)]);
                        if i & 1 == 1 && i < 15 {
                            out.push(b':');
                        }
                    }
                    out.push(b']');
                }
                HostKind::IpFuture => {
                    out.push(b'[');
                    out.extend_from_slice(&host.text);
                    out.push(b']');
                }
                HostKind::RegName => out.extend_from_slice(&host.text),
            }
            if let Some(p) = &self.port {
                out.push(b':');
                out.extend_from_slice(p);
            }
        }
        if self.absolute_path || (!self.segments.is_empty() && self.has_host()) {
            out.push(b'/');
        }
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(seg);
        }
        if let Some(q) = &self.query {
            out.push(b'?');
            out.extend_from_slice(q);
        }
        if let Some(f) = &self.fragment {
            out.push(b'#');
            out.extend_from_slice(f);
        }
        out
    }

    /// `FixEmptyTrailSegment`: a lone empty segment of a relative path
    /// without host means "no path".
    pub(crate) fn fix_empty_trail_segment(&mut self) {
        if !self.absolute_path
            && !self.has_host()
            && self.segments.len() == 1
            && self.segments[0].is_empty()
        {
            self.segments.clear();
        }
    }
}

pub(crate) fn is_alpha(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

pub(crate) fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

pub(crate) fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

pub(crate) fn is_unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~')
}

pub(crate) fn is_sub_delim(c: u8) -> bool {
    matches!(
        c,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

/// `pchar` without `%`.
pub(crate) fn is_pchar_wo_pct(c: u8) -> bool {
    is_unreserved(c) || is_sub_delim(c) || c == b':' || c == b'@'
}

pub(crate) fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Uri {
        parse(s.as_bytes()).expect("valid URI")
    }

    #[test]
    fn path_strings() {
        assert_eq!(p("http://a").path_string(), b"");
        assert_eq!(p("http://a/").path_string(), b"/");
        assert_eq!(p("/").path_string(), b"/");
        assert_eq!(p("a/b").path_string(), b"a/b");
        assert_eq!(p("/a//b").path_string(), b"/a//b");
    }

    #[test]
    fn recompose_round_trips() {
        for s in [
            "",
            "http://user:pass@example.com:8080/a/b?q#f",
            "urn:isbn:123",
            "//host",
            "/abs/path",
            "rel/path?x",
            "?only",
            "#frag",
            "http://h:",
            "http://[v1.fe]/",
            "mailto:a@b",
        ] {
            assert_eq!(p(s).to_bytes(), s.as_bytes(), "{s}");
        }
    }

    #[test]
    fn ipv6_expanded_on_recompose() {
        assert_eq!(
            p("http://[::1]").to_bytes(),
            b"http://[0000:0000:0000:0000:0000:0000:0000:0001]"
        );
        assert_eq!(p("http://[::1]").host_string().unwrap(), b"[::1]");
    }
}
