// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Uri\WhatWg\Url`, `Uri\WhatWg\UrlBuilder` and `Uri\WhatWg\url_percent_encode()`.

use super::{invalid_serialization_data, DebugValue};
use crate::whatwg::{self as w, Host, Log, SchemeType, SharedBuf, State, Url, UrlHostType};
use crate::{UriComparisonMode, UriError, UrlValidationError, UrlValidationErrorType as E};

/// The PHP class name.
pub const CLASS_NAME: &str = "Uri\\WhatWg\\Url";

/// The message suffix ` (Reason)` naming the first failure.
fn reason_suffix(errors: &[UrlValidationError]) -> String {
    errors
        .iter()
        .find(|e| e.failure)
        .map(|e| format!(" ({})", e.error_type.name()))
        .unwrap_or_default()
}

/// `The specified <component> is malformed[ (Reason)]` with the given errors.
fn malformed(component: &str, errors: Vec<UrlValidationError>) -> UriError {
    let msg = format!(
        "The specified {component} is malformed{}",
        reason_suffix(&errors)
    );
    UriError::invalid_url(msg, errors)
}

/// `php_uri_parser_whatwg_component_error`: a single error with an empty
/// context.
fn component_error(component: &str, t: Option<E>) -> UriError {
    let errors = t
        .map(|t| vec![UrlValidationError::new(Vec::new(), t)])
        .unwrap_or_default();
    malformed(component, errors)
}

fn plain(message: &str) -> UriError {
    UriError::invalid_url(message, Vec::new())
}

fn is_tab_or_newline(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\r')
}

/// An initialized `Uri\WhatWg\Url` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatWgUrl {
    url: Url,
}

/// A failed `Uri\WhatWg\Url::parse()`/constructor call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    /// The exception the constructor throws.
    pub error: UriError,
    /// What `$errors` receives from the static `parse()`.
    pub errors: Vec<UrlValidationError>,
}

impl WhatWgUrl {
    /// Wraps a parsed URL.
    pub fn from_url(url: Url) -> Self {
        WhatWgUrl { url }
    }

    /// The underlying URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// `new Uri\WhatWg\Url($uri, $baseUrl, &$softErrors)` and `resolve()`.
    ///
    /// On success returns the URL and the soft errors; on failure the
    /// exception (whose `errors` property holds the validation errors).
    /// PHP assigns an empty array to `$softErrors` when this throws.
    pub fn new(
        uri: &[u8],
        base: Option<&WhatWgUrl>,
    ) -> Result<(Self, Vec<UrlValidationError>), UriError> {
        Self::parse(uri, base).map_err(|f| f.error)
    }

    /// `Uri\WhatWg\Url::parse($uri, $baseUrl, &$errors)`: on failure `$errors`
    /// receives [`ParseFailure::errors`] and the method returns `null`.
    pub fn parse(
        uri: &[u8],
        base: Option<&WhatWgUrl>,
    ) -> Result<(Self, Vec<UrlValidationError>), ParseFailure> {
        let out = Url::parse(uri, base.map(|b| &b.url));
        if let Some(url) = out.url {
            return Ok((WhatWgUrl { url }, out.errors));
        }
        let msg = format!(
            "The specified URI is malformed{}",
            reason_suffix(&out.errors)
        );
        Err(ParseFailure {
            error: UriError::invalid_url(msg, out.errors.clone()),
            errors: out.errors,
        })
    }

    /// `getScheme()`.
    pub fn scheme(&self) -> Vec<u8> {
        self.url.scheme.clone()
    }

    /// `isSpecialScheme()`.
    pub fn is_special_scheme(&self) -> bool {
        self.url.is_special()
    }

    /// `getUsername()`.
    pub fn username(&self) -> Option<Vec<u8>> {
        self.url
            .includes_credentials()
            .then(|| self.url.username.clone())
    }

    /// `getPassword()`.
    pub fn password(&self) -> Option<Vec<u8>> {
        self.url
            .includes_credentials()
            .then(|| self.url.password.clone())
    }

    /// `getAsciiHost()`.
    pub fn ascii_host(&self) -> Option<Vec<u8>> {
        self.url.ascii_host()
    }

    /// `getUnicodeHost()`.
    pub fn unicode_host(&self) -> Option<Vec<u8>> {
        self.url.unicode_host()
    }

    /// `getHostType()`.
    pub fn host_type(&self) -> Option<UrlHostType> {
        self.url.host_type()
    }

    /// `getPort()`.
    pub fn port(&self) -> Option<i64> {
        self.url.port.map(i64::from)
    }

    /// `getPath()`.
    pub fn path(&self) -> Vec<u8> {
        self.url.path.clone()
    }

    /// `getQuery()`.
    pub fn query(&self) -> Option<Vec<u8>> {
        self.url.query.clone()
    }

    /// `getFragment()`.
    pub fn fragment(&self) -> Option<Vec<u8>> {
        self.url.fragment.clone()
    }

    fn with(
        &self,
        f: impl FnOnce(&mut Url, &mut Log) -> Result<(), w::Fail>,
        component: &str,
    ) -> Result<Self, UriError> {
        let mut url = self.url.clone();
        let mut log = Log::default();
        match f(&mut url, &mut log) {
            Ok(()) => Ok(WhatWgUrl { url }),
            Err(_) => Err(malformed(component, log.errors)),
        }
    }

    /// `withScheme()`.
    pub fn with_scheme(&self, scheme: &[u8]) -> Result<Self, UriError> {
        self.with(|u, log| u.set_protocol(log, scheme), "scheme")
    }

    /// `withUsername()`.
    pub fn with_username(&self, username: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(
            |u, _| {
                u.set_username(username.unwrap_or_default());
                Ok(())
            },
            "username",
        )
    }

    /// `withPassword()`.
    pub fn with_password(&self, password: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(
            |u, _| {
                u.set_password(password.unwrap_or_default());
                Ok(())
            },
            "password",
        )
    }

    /// `withHost()`.
    pub fn with_host(&self, host: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(
            |u, log| u.set_hostname(log, host.unwrap_or_default()),
            "host",
        )
    }

    /// `withPort()`.
    pub fn with_port(&self, port: Option<i64>) -> Result<Self, UriError> {
        let text = port.map(|p| p.to_string().into_bytes()).unwrap_or_default();
        self.with(|u, log| u.set_port_str(log, &text), "port")
    }

    /// `withPath()`.
    pub fn with_path(&self, path: &[u8]) -> Result<Self, UriError> {
        self.with(|u, log| u.set_pathname(log, path), "path")
    }

    /// `withQuery()`.
    pub fn with_query(&self, query: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(
            |u, log| u.set_search(log, query.unwrap_or_default()),
            "query string",
        )
    }

    /// `withFragment()`.
    pub fn with_fragment(&self, fragment: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(
            |u, log| u.set_hash(log, fragment.unwrap_or_default()),
            "fragment",
        )
    }

    /// `equals()`.
    pub fn equals(&self, other: &WhatWgUrl, mode: UriComparisonMode) -> bool {
        let exclude = mode == UriComparisonMode::ExcludeFragment;
        self.url.serialize(false, exclude) == other.url.serialize(false, exclude)
    }

    /// `toAsciiString()`.
    pub fn to_ascii_string(&self) -> Vec<u8> {
        self.url.serialize(false, false)
    }

    /// `toUnicodeString()`.
    pub fn to_unicode_string(&self) -> Vec<u8> {
        self.url.serialize(true, false)
    }

    /// `resolve($uri, &$softErrors)`.
    pub fn resolve(&self, uri: &[u8]) -> Result<(Self, Vec<UrlValidationError>), UriError> {
        Self::new(uri, Some(self))
    }

    /// The `uri` string stored by `__serialize()`.
    pub fn serialize(&self) -> Vec<u8> {
        self.to_ascii_string()
    }

    /// Restores an object from the `uri` string of `__unserialize()`.
    pub fn unserialize(uri: &[u8]) -> Result<Self, UriError> {
        Url::parse(uri, None)
            .url
            .map(|url| WhatWgUrl { url })
            .ok_or_else(|| invalid_serialization_data(CLASS_NAME))
    }

    /// `__debugInfo()`.
    pub fn debug_info(&self) -> Vec<(&'static str, DebugValue)> {
        vec![
            ("scheme", DebugValue::Str(self.scheme())),
            ("username", DebugValue::from_opt(self.username())),
            ("password", DebugValue::from_opt(self.password())),
            ("host", DebugValue::from_opt(self.ascii_host())),
            (
                "port",
                self.port().map_or(DebugValue::Null, DebugValue::Int),
            ),
            ("path", DebugValue::Str(self.path())),
            ("query", DebugValue::from_opt(self.query())),
            ("fragment", DebugValue::from_opt(self.fragment())),
        ]
    }
}

/// `Uri\WhatWg\url_percent_encode()`.
pub fn url_percent_encode(input: &[u8], mode: w::UrlPercentEncodingMode) -> Vec<u8> {
    w::url_percent_encode(input, mode)
}

// ---------------------------------------------------------------------------
// UrlBuilder
// ---------------------------------------------------------------------------

/// `Uri\WhatWg\UrlBuilder` (its properties).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhatWgUrlBuilder {
    pub scheme: Vec<u8>,
    pub username: Option<Vec<u8>>,
    pub password: Option<Vec<u8>>,
    pub host: Option<Vec<u8>>,
    pub port: Option<i64>,
    pub path: Vec<u8>,
    pub query: Option<Vec<u8>>,
    pub fragment: Option<Vec<u8>>,
}

/// `php_uri_parser_whatwg_scheme_validate`.
pub fn validate_scheme(scheme: &[u8]) -> Result<(), UriError> {
    let mut seen_first = false;
    for &c in scheme {
        if is_tab_or_newline(c) {
            continue;
        }
        let ok = if seen_first {
            c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')
        } else {
            c.is_ascii_alphabetic()
        };
        if !ok {
            return Err(component_error(
                "scheme",
                Some(E::MissingSchemeNonRelativeUrl),
            ));
        }
        seen_first = true;
    }
    if seen_first {
        Ok(())
    } else {
        Err(component_error(
            "scheme",
            Some(E::MissingSchemeNonRelativeUrl),
        ))
    }
}

/// `php_uri_parser_whatwg_host_validate`.
pub fn validate_host(host: &[u8]) -> Result<(), UriError> {
    if host.is_empty() {
        return Ok(());
    }
    let first = host
        .iter()
        .position(|&c| !is_tab_or_newline(c))
        .unwrap_or(host.len());
    if host[first..]
        .iter()
        .any(|&c| matches!(c, b'/' | b'?' | b'#' | b'\\'))
    {
        return Err(component_error("host", Some(E::HostInvalidCodePoint)));
    }
    if host.get(first) != Some(&b'[') {
        return Ok(());
    }
    let stripped: Vec<u8> = host[first..]
        .iter()
        .copied()
        .filter(|&c| !is_tab_or_newline(c))
        .collect();
    let mut log = Log::default();
    match w::parse_ipv6_text(&mut log, &stripped) {
        Ok(_) => Ok(()),
        Err(_) => Err(component_error(
            "host",
            log.errors.last().map(|e| e.error_type),
        )),
    }
}

/// `php_uri_parser_whatwg_port_validate`.
pub fn validate_port(port: i64) -> Result<(), UriError> {
    if port < 0 {
        return Err(component_error("port", Some(E::PortInvalid)));
    }
    if port > 65535 {
        return Err(component_error("port", Some(E::PortOutOfRange)));
    }
    Ok(())
}

impl WhatWgUrlBuilder {
    /// `new Uri\WhatWg\UrlBuilder()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `setScheme()`.
    pub fn set_scheme(&mut self, scheme: &[u8]) -> Result<(), UriError> {
        validate_scheme(scheme)?;
        self.scheme = scheme.to_vec();
        Ok(())
    }

    /// `setUsername()`.
    pub fn set_username(&mut self, username: Option<&[u8]>) {
        self.username = username.map(<[u8]>::to_vec);
    }

    /// `setPassword()`.
    pub fn set_password(&mut self, password: Option<&[u8]>) {
        self.password = password.map(<[u8]>::to_vec);
    }

    /// `setHost()`.
    pub fn set_host(&mut self, host: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(h) = host {
            validate_host(h)?;
        }
        self.host = host.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `setPort()`.
    pub fn set_port(&mut self, port: Option<i64>) -> Result<(), UriError> {
        if let Some(p) = port {
            validate_port(p)?;
        }
        self.port = port;
        Ok(())
    }

    /// `setPath()`.
    pub fn set_path(&mut self, path: &[u8]) {
        self.path = path.to_vec();
    }

    /// `setQuery()`.
    pub fn set_query(&mut self, query: Option<&[u8]>) {
        self.query = query.map(<[u8]>::to_vec);
    }

    /// `setFragment()`.
    pub fn set_fragment(&mut self, fragment: Option<&[u8]>) {
        self.fragment = fragment.map(<[u8]>::to_vec);
    }

    /// `build($baseUrl, &$softErrors)`: on failure PHP assigns an empty
    /// array to `$softErrors`.
    pub fn build(
        &self,
        base: Option<&WhatWgUrl>,
    ) -> Result<(WhatWgUrl, Vec<UrlValidationError>), UriError> {
        let c = Components {
            scheme: &self.scheme,
            username: self.username.as_deref(),
            password: self.password.as_deref(),
            host: self.host.as_deref(),
            port: self.port,
            path: &self.path,
            query: self.query.as_deref(),
            fragment: self.fragment.as_deref(),
        };
        build_from_components(base.map(|b| &b.url), &c).map(|(url, e)| (WhatWgUrl { url }, e))
    }
}

/// The builder properties read by `build()`.
#[derive(Debug, Clone, Copy)]
pub struct Components<'a> {
    pub scheme: &'a [u8],
    pub username: Option<&'a [u8]>,
    pub password: Option<&'a [u8]>,
    pub host: Option<&'a [u8]>,
    pub port: Option<i64>,
    pub path: &'a [u8],
    pub query: Option<&'a [u8]>,
    pub fragment: Option<&'a [u8]>,
}

/// Prepends the errors collected so far to the exception's own errors
/// (`php_uri_parser_whatwg_build_errors_into_exception`).
fn merge(mut collected: Vec<UrlValidationError>, mut e: UriError) -> UriError {
    if !collected.is_empty() && e.class == crate::ErrorClass::InvalidUrlException {
        collected.append(&mut e.errors);
        e.errors = collected;
    }
    e
}

/// Runs one lexbor setter: on failure throws `The specified <component> is
/// malformed`, otherwise appends its soft errors.
fn write(
    errors: &mut Vec<UrlValidationError>,
    component: &str,
    f: impl FnOnce(&mut Log) -> Result<(), w::Fail>,
) -> Result<(), UriError> {
    let mut log = Log::default();
    match f(&mut log) {
        Ok(()) => {
            errors.append(&mut log.errors);
            Ok(())
        }
        Err(_) => Err(malformed(component, log.errors)),
    }
}

/// `php_uri_parser_whatwg_build_from_zval`.
pub fn build_from_components(
    base: Option<&Url>,
    c: &Components<'_>,
) -> Result<(Url, Vec<UrlValidationError>), UriError> {
    if let Some(base) = base {
        if c.scheme.is_empty() {
            return resolve_reference(base, c);
        }
    }
    let mut url = Url {
        scheme_type: if SchemeType::special_from_name(c.scheme).is_none() {
            SchemeType::Unknown
        } else {
            SchemeType::Undef
        },
        ..Url::default()
    };
    let mut errors = Vec::new();
    match build_inner(&mut url, c, &mut errors) {
        Ok(()) => Ok((url, errors)),
        Err(e) => Err(merge(errors, e)),
    }
}

fn build_inner(
    url: &mut Url,
    c: &Components<'_>,
    errors: &mut Vec<UrlValidationError>,
) -> Result<(), UriError> {
    write(errors, "scheme", |log| url.set_protocol(log, c.scheme))?;
    if c.host.is_some() || url.is_special() {
        write(errors, "host", |log| {
            url.set_hostname(log, c.host.unwrap_or_default())
        })?;
    }
    if matches!(url.host, Host::None | Host::Empty) || url.scheme_type == SchemeType::File {
        if c.username.is_some_and(|u| !u.is_empty()) {
            return Err(plain("The specified URL cannot have username"));
        }
        if c.password.is_some_and(|p| !p.is_empty()) {
            return Err(plain("The specified URL cannot have password"));
        }
        if c.port.is_some() {
            return Err(plain("The specified URL cannot have port"));
        }
    }
    url.set_username(c.username.unwrap_or_default());
    url.set_password(c.password.unwrap_or_default());
    let port = c
        .port
        .map(|p| p.to_string().into_bytes())
        .unwrap_or_default();
    write(errors, "port", |log| url.set_port_str(log, &port))?;
    build_path(url, c, errors)?;
    write(errors, "query string", |log| {
        url.set_search(log, c.query.unwrap_or_default())
    })?;
    write(errors, "fragment", |log| {
        url.set_hash(log, c.fragment.unwrap_or_default())
    })?;
    Ok(())
}

/// `php_uri_parser_whatwg_build_path`.
fn build_path(
    url: &mut Url,
    c: &Components<'_>,
    errors: &mut Vec<UrlValidationError>,
) -> Result<(), UriError> {
    let path = c.path;
    let start = path
        .iter()
        .position(|&ch| !is_tab_or_newline(ch))
        .unwrap_or(path.len());
    if url.host == Host::None && (start == path.len() || path[start] != b'/') {
        if path.iter().any(|&ch| ch == b'?' || ch == b'#') {
            return Err(malformed("path", Vec::new()));
        }
        let mut input = path.to_vec();
        if c.query.is_some_and(|q| !q.is_empty()) {
            input.push(b'?');
        } else if c.fragment.is_some_and(|f| !f.is_empty()) {
            input.push(b'#');
        }
        return write(errors, "path", |log| {
            url.parse_with_override(log, None, &input, State::OpaquePath)
        });
    }
    write(errors, "path", |log| url.set_pathname(log, path))
}

/// `php_uri_parser_whatwg_resolve_reference_from_zval`.
fn resolve_reference(
    base: &Url,
    c: &Components<'_>,
) -> Result<(Url, Vec<UrlValidationError>), UriError> {
    if c.host.is_some() {
        if base.path_opaque {
            return Err(component_error(
                "host",
                Some(E::MissingSchemeNonRelativeUrl),
            ));
        }
        let scheme = base.scheme.clone();
        let c2 = Components {
            scheme: &scheme,
            ..*c
        };
        return build_from_components(None, &c2);
    }
    if c.username.is_some_and(|u| !u.is_empty()) {
        return Err(plain("The specified URL cannot have username"));
    }
    if c.password.is_some_and(|p| !p.is_empty()) {
        return Err(plain("The specified URL cannot have password"));
    }
    if c.port.is_some() {
        return Err(plain("The specified URL cannot have port"));
    }
    let path = c.path;
    let first = path
        .iter()
        .position(|&ch| !is_tab_or_newline(ch))
        .unwrap_or(path.len());
    let path_empty = first == path.len();
    let mut errors = Vec::new();
    let mut url = base.clone();
    if base.path_opaque && path_empty && (c.query.is_some() || c.fragment.is_none()) {
        let component = if c.query.is_some() { "query" } else { "path" };
        return Err(component_error(
            component,
            Some(E::MissingSchemeNonRelativeUrl),
        ));
    }
    url.fragment = None;
    let result = (|| -> Result<(), UriError> {
        if !path.is_empty() {
            let mut state = State::NoScheme;
            if !base.path_opaque
                && !path_empty
                && (path[first] == b'/' || (base.is_special() && path[first] == b'\\'))
            {
                state = State::PathStart;
                if base.scheme_type == SchemeType::File {
                    let second = path[first + 1..]
                        .iter()
                        .position(|&ch| !is_tab_or_newline(ch))
                        .map(|i| first + 1 + i);
                    match second {
                        Some(i) if path[i] == b'/' || path[i] == b'\\' => {}
                        _ => state = State::File,
                    }
                }
            }
            let mut reference = Vec::with_capacity(path.len());
            let mut path_buf: Option<SharedBuf> = None;
            for (i, &ch) in path.iter().enumerate() {
                match ch {
                    b'?' => reference.extend_from_slice(b"%3F"),
                    b'#' => {
                        reference.extend_from_slice(b"%23");
                        let buf = path_buf.get_or_insert_with(|| SharedBuf::new(path));
                        let mut log = Log::default();
                        log.push(buf, i, E::InvalidUrlUnit);
                        errors.append(&mut log.errors);
                    }
                    _ => reference.push(ch),
                }
            }
            if base.path_opaque && path_empty && c.query.is_none() && c.fragment.is_some() {
                reference.push(b'#');
            }
            url.path_set_null();
            write(&mut errors, "path", |log| {
                url.parse_with_override(log, Some(base), &reference, state)
            })?;
            if !path_empty {
                url.query = None;
            }
        }
        if let Some(q) = c.query {
            write(&mut errors, "query string", |log| url.set_search(log, q))?;
        }
        if let Some(f) = c.fragment {
            write(&mut errors, "fragment", |log| url.set_hash(log, f))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok((url, errors)),
        Err(e) => Err(merge(errors, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(v: &[u8]) -> &str {
        std::str::from_utf8(v).unwrap()
    }

    #[test]
    fn constructor_errors() {
        let e = WhatWgUrl::new(b"/foo", None).unwrap_err();
        assert_eq!(
            e.message,
            "The specified URI is malformed (MissingSchemeNonRelativeUrl)"
        );
        assert_eq!(e.errors.len(), 1);
        let (u, soft) = WhatWgUrl::new(b" https://example.org ", None).unwrap();
        assert_eq!(soft.len(), 2);
        assert_eq!(st(&u.to_ascii_string()), "https://example.org/");
    }

    #[test]
    fn with_methods() {
        let (u, _) = WhatWgUrl::new(b"https://example.com", None).unwrap();
        let e = u.with_scheme(b"0").unwrap_err();
        assert_eq!(e.message, "The specified scheme is malformed");
        assert!(e.errors.is_empty());
        let e = u.with_host(None).unwrap_err();
        assert_eq!(e.message, "The specified host is malformed (HostMissing)");
        let e = u.with_port(Some(99999)).unwrap_err();
        assert_eq!(
            e.message,
            "The specified port is malformed (PortOutOfRange)"
        );
        let u2 = u.with_host(Some(b"%65xample.net")).unwrap();
        assert_eq!(st(&u2.to_ascii_string()), "https://example.net/");
        let u3 = u.with_query(Some(b"")).unwrap();
        assert_eq!(u3.query(), None);
    }

    #[test]
    fn builder_basic() {
        let mut b = WhatWgUrlBuilder::new();
        b.set_scheme(b"https").unwrap();
        b.set_username(Some(b"user"));
        b.set_password(Some(b"pass"));
        b.set_host(Some(b"example.com")).unwrap();
        b.set_port(Some(444)).unwrap();
        b.set_path(b"/foo/bar/baz");
        b.set_query(Some(b"foo=1&bar=baz"));
        b.set_fragment(Some(b"fragment"));
        let (u, _) = b.build(None).unwrap();
        assert_eq!(
            st(&u.to_ascii_string()),
            "https://user:pass@example.com:444/foo/bar/baz?foo=1&bar=baz#fragment"
        );
        b.reset();
        b.set_scheme(b"scheme").unwrap();
        b.set_host(Some(b"www.example.com")).unwrap();
        let (u, _) = b.build(None).unwrap();
        assert_eq!(st(&u.to_ascii_string()), "scheme://www.example.com");
    }

    #[test]
    fn builder_with_base() {
        let (base, _) =
            WhatWgUrl::new(b"https://old:secret@example.com:81/a?old#old", None).unwrap();
        let mut b = WhatWgUrlBuilder::new();
        b.set_host(Some(b"example.net")).unwrap();
        b.set_username(Some(b"a@b"));
        b.set_password(Some(b"c:d"));
        let (u, _) = b.build(Some(&base)).unwrap();
        assert_eq!(st(&u.to_ascii_string()), "https://a%40b:c%3Ad@example.net/");

        let (opaque, _) = WhatWgUrl::new(b"foo:opaque?old#old", None).unwrap();
        let e = WhatWgUrlBuilder::new().build(Some(&opaque)).unwrap_err();
        assert_eq!(
            e.message,
            "The specified path is malformed (MissingSchemeNonRelativeUrl)"
        );
    }

    #[test]
    fn validators() {
        assert!(validate_scheme(b"\thttps").is_ok());
        assert_eq!(
            validate_scheme(b"").unwrap_err().message,
            "The specified scheme is malformed (MissingSchemeNonRelativeUrl)"
        );
        assert_eq!(
            validate_host(b"a/b").unwrap_err().message,
            "The specified host is malformed (HostInvalidCodePoint)"
        );
        assert_eq!(
            validate_host(b"[::1").unwrap_err().message,
            "The specified host is malformed (Ipv6Unclosed)"
        );
        assert!(validate_host(b"[::1]").is_ok());
        assert_eq!(
            validate_port(-1).unwrap_err().message,
            "The specified port is malformed (PortInvalid)"
        );
    }
}
