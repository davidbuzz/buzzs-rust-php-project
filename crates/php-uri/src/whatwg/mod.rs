//! The WHATWG URL Standard, following lexbor's URL module as used by PHP's
//! `Uri\WhatWg\Url`.

pub mod encode;
pub mod host;
pub mod idna;
mod parser;
#[allow(clippy::unreadable_literal)]
pub mod unicode_tables;
pub mod utf8;

pub use encode::{url_percent_encode, UrlPercentEncodingMode};
pub use parser::{Fail, Log, State};

use crate::UrlValidationError;

/// The scheme classification (`lxb_url_scheme_type_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemeType {
    /// Not set yet (a freshly allocated URL; counts as special in lexbor).
    #[default]
    Undef,
    /// A non-special scheme.
    Unknown,
    Http,
    Https,
    Ws,
    Wss,
    Ftp,
    File,
}

impl SchemeType {
    /// The canonical scheme name of special schemes.
    pub fn name(self) -> &'static str {
        match self {
            SchemeType::Undef => "#undef",
            SchemeType::Unknown => "#unknown",
            SchemeType::Http => "http",
            SchemeType::Https => "https",
            SchemeType::Ws => "ws",
            SchemeType::Wss => "wss",
            SchemeType::Ftp => "ftp",
            SchemeType::File => "file",
        }
    }

    /// Whether lexbor treats the scheme as special (anything but
    /// [`SchemeType::Unknown`]).
    pub fn is_special(self) -> bool {
        self != SchemeType::Unknown
    }

    /// The default port, as lexbor's scheme table records it (0 for
    /// schemes without one).
    pub fn default_port(self) -> Option<u16> {
        Some(match self {
            SchemeType::Http | SchemeType::Ws => 80,
            SchemeType::Https | SchemeType::Wss => 443,
            SchemeType::Ftp => 21,
            SchemeType::Undef | SchemeType::Unknown | SchemeType::File => 0,
        })
    }

    /// Classifies a scheme name, ignoring ASCII tabs and newlines the way
    /// PHP's `php_uri_parser_whatwg_get_special_scheme` does.
    pub fn special_from_name(name: &[u8]) -> Option<SchemeType> {
        let cleaned: Vec<u8> = name
            .iter()
            .copied()
            .filter(|c| !matches!(c, b'\t' | b'\n' | b'\r'))
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match cleaned.as_slice() {
            b"ws" => Some(SchemeType::Ws),
            b"ftp" => Some(SchemeType::Ftp),
            b"wss" => Some(SchemeType::Wss),
            b"http" => Some(SchemeType::Http),
            b"file" => Some(SchemeType::File),
            b"https" => Some(SchemeType::Https),
            _ => None,
        }
    }
}

/// A URL host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Host {
    /// No host (`LXB_URL_HOST_TYPE__UNDEF`).
    #[default]
    None,
    /// An ASCII domain.
    Domain(Vec<u8>),
    /// An opaque host of a non-special URL.
    Opaque(Vec<u8>),
    /// An IPv4 address.
    Ipv4(u32),
    /// An IPv6 address.
    Ipv6([u16; 8]),
    /// The empty host.
    Empty,
}

/// `Uri\WhatWg\UrlHostType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlHostType {
    IpV4,
    IpV6,
    Domain,
    Opaque,
    Empty,
}

impl UrlHostType {
    /// The enum case name.
    pub fn name(self) -> &'static str {
        match self {
            UrlHostType::IpV4 => "IpV4",
            UrlHostType::IpV6 => "IpV6",
            UrlHostType::Domain => "Domain",
            UrlHostType::Opaque => "Opaque",
            UrlHostType::Empty => "Empty",
        }
    }
}

/// A parsed WHATWG URL (the fields of `lxb_url_t`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Url {
    /// Lowercased scheme name.
    pub scheme: Vec<u8>,
    /// Scheme classification.
    pub scheme_type: SchemeType,
    /// Percent-encoded username.
    pub username: Vec<u8>,
    /// Percent-encoded password.
    pub password: Vec<u8>,
    /// The host.
    pub host: Host,
    /// The port, unless absent or the scheme's default.
    pub port: Option<u16>,
    /// The serialized path (`/seg/seg` or the opaque path).
    pub path: Vec<u8>,
    /// Number of path segments.
    pub path_len: usize,
    /// Whether the path is opaque.
    pub path_opaque: bool,
    /// The query (without `?`).
    pub query: Option<Vec<u8>>,
    /// The fragment (without `#`).
    pub fragment: Option<Vec<u8>>,
}

/// The result of a parse: the URL (if successful) and the validation errors.
#[derive(Debug, Clone)]
pub struct ParseOutcome {
    /// The URL, or `None` on failure.
    pub url: Option<Url>,
    /// Validation errors in the order lexbor reported them.
    pub errors: Vec<UrlValidationError>,
}

impl Url {
    /// Parses `input` against an optional base (`lxb_url_parse`).
    pub fn parse(input: &[u8], base: Option<&Url>) -> ParseOutcome {
        let mut log = Log::default();
        let url = parser::parse_basic(&mut log, None, base, input, None)
            .ok()
            .flatten();
        ParseOutcome {
            url,
            errors: log.errors,
        }
    }

    /// Runs the basic URL parser on this URL with a state override.
    pub fn parse_with_override(
        &mut self,
        log: &mut Log,
        base: Option<&Url>,
        input: &[u8],
        state: State,
    ) -> Result<(), Fail> {
        parser::parse_basic(log, Some(self), base, input, Some(state)).map(|_| ())
    }

    /// `lxb_url_is_special`.
    pub fn is_special(&self) -> bool {
        self.scheme_type.is_special()
    }

    /// Whether the username or password is non-empty.
    pub fn includes_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    /// `lxb_url_scheme_is_equal`.
    pub fn scheme_equals(&self, other: &Url) -> bool {
        self.scheme_type == other.scheme_type
            && (self.scheme_type != SchemeType::Unknown || self.scheme == other.scheme)
    }

    pub(crate) fn copy_path_from(&mut self, other: &Url) {
        self.path.clone_from(&other.path);
        self.path_len = other.path_len;
        self.path_opaque = other.path_opaque;
    }

    /// `lxb_url_cannot_have_user_pass_port`.
    pub fn cannot_have_user_pass_port(&self) -> bool {
        matches!(self.host, Host::None | Host::Empty)
            || matches!(&self.host, Host::Domain(d) | Host::Opaque(d) if d.is_empty())
            || self.scheme_type == SchemeType::File
    }

    /// The host type, or `None` when there is no host.
    pub fn host_type(&self) -> Option<UrlHostType> {
        match self.host {
            Host::None => None,
            Host::Domain(_) => Some(UrlHostType::Domain),
            Host::Opaque(_) => Some(UrlHostType::Opaque),
            Host::Ipv4(_) => Some(UrlHostType::IpV4),
            Host::Ipv6(_) => Some(UrlHostType::IpV6),
            Host::Empty => Some(UrlHostType::Empty),
        }
    }

    /// The ASCII host as `getAsciiHost()` returns it.
    pub fn ascii_host(&self) -> Option<Vec<u8>> {
        match &self.host {
            Host::None => None,
            h => Some(host::serialize_host(h)),
        }
    }

    /// The host as `getUnicodeHost()` returns it (empty on IDNA failure,
    /// mirroring PHP ignoring the serializer status).
    pub fn unicode_host(&self) -> Option<Vec<u8>> {
        match &self.host {
            Host::None => None,
            Host::Domain(d) | Host::Opaque(d) => Some(idna::to_unicode(d).unwrap_or_default()),
            h => Some(host::serialize_host(h)),
        }
    }

    /// Serializes the URL (`lxb_url_serialize` / `lxb_url_serialize_idna`).
    ///
    /// With `unicode`, domain and opaque hosts go through domain-to-Unicode;
    /// if that fails the output stops right before the host, as lexbor's
    /// serializer aborts and PHP keeps what was written so far.
    pub fn serialize(&self, unicode: bool, exclude_fragment: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.scheme);
        out.push(b':');
        if self.host == Host::None {
            if !self.path_opaque && self.path.len() > 1 && self.path.starts_with(b"//") {
                out.extend_from_slice(b"/.");
            }
        } else {
            out.extend_from_slice(b"//");
            if self.includes_credentials() {
                out.extend_from_slice(&self.username);
                if !self.password.is_empty() {
                    out.push(b':');
                    out.extend_from_slice(&self.password);
                }
                out.push(b'@');
            }
            if unicode {
                match &self.host {
                    Host::Domain(d) | Host::Opaque(d) => match idna::to_unicode(d) {
                        Ok(h) => out.extend_from_slice(&h),
                        Err(_) => return out,
                    },
                    h => out.extend_from_slice(&host::serialize_host(h)),
                }
            } else {
                out.extend_from_slice(&host::serialize_host(&self.host));
            }
            if let Some(port) = self.port {
                out.push(b':');
                out.extend_from_slice(port.to_string().as_bytes());
            }
        }
        out.extend_from_slice(&self.path);
        if let Some(q) = &self.query {
            out.push(b'?');
            out.extend_from_slice(q);
        }
        if !exclude_fragment {
            if let Some(f) = &self.fragment {
                out.push(b'#');
                out.extend_from_slice(f);
            }
        }
        out
    }

    // ---- lexbor's URL API setters (lxb_url_api_*_set) ----

    /// `lxb_url_api_protocol_set`.
    pub fn set_protocol(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        self.parse_with_override(log, None, value, State::SchemeStart)
    }

    /// `lxb_url_api_username_set`.
    pub fn set_username(&mut self, value: &[u8]) {
        if self.cannot_have_user_pass_port() {
            return;
        }
        self.username = encode::percent_encode(value, encode::MAP_USERINFO);
    }

    /// `lxb_url_api_password_set`.
    pub fn set_password(&mut self, value: &[u8]) {
        if self.cannot_have_user_pass_port() {
            return;
        }
        self.password = encode::percent_encode(value, encode::MAP_USERINFO);
    }

    /// `lxb_url_api_hostname_set`.
    pub fn set_hostname(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        if self.path_opaque {
            return Ok(());
        }
        let old = std::mem::take(&mut self.host);
        match self.parse_with_override(log, None, value, State::Hostname) {
            Ok(()) => {
                if self.host == Host::None {
                    self.host = old;
                }
                Ok(())
            }
            Err(e) => {
                self.host = old;
                Err(e)
            }
        }
    }

    /// `lxb_url_api_port_set`.
    pub fn set_port_str(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        if self.cannot_have_user_pass_port() {
            return Ok(());
        }
        if value.is_empty() {
            self.port = None;
            return Ok(());
        }
        self.parse_with_override(log, None, value, State::Port)
    }

    /// `lxb_url_api_pathname_set`.
    pub fn set_pathname(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        if self.path_opaque {
            return Ok(());
        }
        self.path_len = 0;
        self.path.clear();
        self.parse_with_override(log, None, value, State::PathStart)
    }

    /// `lxb_url_api_search_set`.
    pub fn set_search(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        self.query = None;
        if value.is_empty() {
            return Ok(());
        }
        let value = value.strip_prefix(b"?").unwrap_or(value);
        self.parse_with_override(log, None, value, State::Query)
    }

    /// `lxb_url_api_hash_set`.
    pub fn set_hash(&mut self, log: &mut Log, value: &[u8]) -> Result<(), Fail> {
        self.fragment = None;
        if value.is_empty() {
            return Ok(());
        }
        let value = value.strip_prefix(b"#").unwrap_or(value);
        self.parse_with_override(log, None, value, State::Fragment)
    }
}

/// `lxb_url_parse_host_ipv6` on text that may be bracketed.
pub fn parse_ipv6_text(log: &mut Log, data: &[u8]) -> Result<[u16; 8], Fail> {
    let (begin, end) = if data.first() == Some(&b'[') {
        if data.last() != Some(&b']') {
            log.push(
                data,
                data.len() - 1,
                crate::UrlValidationErrorType::Ipv6Unclosed,
            );
            return Err(Fail);
        }
        (1, data.len() - 1)
    } else {
        (0, data.len())
    };
    host::parse_ipv6(log, data, begin, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(input: &str) -> String {
        let out = Url::parse(input.as_bytes(), None);
        String::from_utf8(out.url.expect("parses").serialize(false, false)).unwrap()
    }

    fn rel(input: &str, base: &str) -> String {
        let b = Url::parse(base.as_bytes(), None).url.unwrap();
        let out = Url::parse(input.as_bytes(), Some(&b));
        String::from_utf8(out.url.expect("parses").serialize(false, false)).unwrap()
    }

    fn errs(input: &str) -> Vec<(String, &'static str)> {
        Url::parse(input.as_bytes(), None)
            .errors
            .into_iter()
            .map(|e| (String::from_utf8(e.context).unwrap(), e.error_type.name()))
            .collect()
    }

    #[test]
    fn basics() {
        assert_eq!(s("https://example.com"), "https://example.com/");
        assert_eq!(
            s("HTTPS://EXAMPLE.COM:443/a/../b/./c?q#f"),
            "https://example.com/b/c?q#f"
        );
        assert_eq!(
            s("https://user:pass@example.com:8080/"),
            "https://user:pass@example.com:8080/"
        );
        assert_eq!(s("urn:isbn:0451450523"), "urn:isbn:0451450523");
        assert_eq!(
            s("mailto:john.doe@example.com"),
            "mailto:john.doe@example.com"
        );
        assert_eq!(s("file:///C|/foo"), "file:///C:/foo");
        assert_eq!(s("http://192.168"), "http://192.0.0.168/");
        assert_eq!(s("http://[::1]:80/"), "http://[::1]/");
        assert_eq!(s("foo://EXAMPLE.com/ a"), "foo://EXAMPLE.com/%20a");
        assert_eq!(
            s("https://example.com/a b?c d#e f"),
            "https://example.com/a%20b?c%20d#e%20f"
        );
        assert_eq!(s("web+demo:/.//not-a-host/"), "web+demo:/.//not-a-host/");
        assert_eq!(s("https://bücher.de/"), "https://xn--bcher-kva.de/");
        assert_eq!(s("non-spec:/.//p"), "non-spec:/.//p");
        assert_eq!(s("http://a/%2e%2E/b"), "http://a/b");
    }

    #[test]
    fn relative() {
        assert_eq!(
            rel("/foo", "https://example.com/a/b"),
            "https://example.com/foo"
        );
        assert_eq!(
            rel("c", "https://example.com/a/b"),
            "https://example.com/a/c"
        );
        assert_eq!(
            rel("../c", "https://example.com/a/b"),
            "https://example.com/c"
        );
        assert_eq!(
            rel("?x", "https://example.com/a/b?y#z"),
            "https://example.com/a/b?x"
        );
        assert_eq!(rel("#x", "foo:opaque"), "foo:opaque#x");
        assert_eq!(rel("//h/p", "https://example.com/a"), "https://h/p");
    }

    #[test]
    fn failures() {
        assert!(Url::parse(b"", None).url.is_none());
        assert!(Url::parse(b"/foo", None).url.is_none());
        assert!(Url::parse(b"https://", None).url.is_none());
        assert!(Url::parse(b"https://ex:99999", None).url.is_none());
        assert!(Url::parse(b"http://a b/", None).url.is_none());
    }

    #[test]
    fn error_contexts() {
        assert_eq!(
            errs(" https://example.org "),
            vec![
                (" https://example.org ".to_string(), "InvalidUrlUnit"),
                (" ".to_string(), "InvalidUrlUnit")
            ]
        );
        assert_eq!(
            errs("/x"),
            vec![("/x".to_string(), "MissingSchemeNonRelativeUrl")]
        );
        assert_eq!(
            errs("https:example.com"),
            vec![(
                "example.com".to_string(),
                "SpecialSchemeMissingFollowingSolidus"
            )]
        );
    }

    #[test]
    fn setters() {
        let mut u = Url::parse(b"https://example.com/a", None).url.unwrap();
        let mut log = Log::default();
        u.set_search(&mut log, b"?a=b#c").unwrap();
        assert_eq!(u.query.as_deref(), Some(&b"a=b%23c"[..]));
        u.set_hash(&mut log, b"").unwrap();
        assert!(u.fragment.is_none());
        u.set_pathname(&mut log, b"x/y").unwrap();
        assert_eq!(u.path, b"/x/y");
        u.set_port_str(&mut log, b"443").unwrap();
        assert_eq!(u.port, None);
        u.set_port_str(&mut log, b"8080").unwrap();
        assert_eq!(u.port, Some(8080));
        assert!(u.set_hostname(&mut log, b"").is_err());
        u.set_hostname(&mut log, b"example.net").unwrap();
        assert_eq!(u.ascii_host().unwrap(), b"example.net");
        u.set_protocol(&mut log, b"http").unwrap();
        assert_eq!(
            u.serialize(false, false),
            b"http://example.net:8080/x/y?a=b%23c"
        );
        u.set_protocol(&mut log, b"foo").unwrap();
        assert_eq!(u.scheme, b"http");
    }
}
