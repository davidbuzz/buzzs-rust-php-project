// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Uri\Rfc3986\Uri`, `Uri\Rfc3986\UriBuilder` and their enums.

use super::{invalid_serialization_data, port_str_to_long, DebugValue};
use crate::rfc3986::{self as r, HostKind, ResolveError, SetError, Uri};
use crate::{ErrorClass, UriComparisonMode, UriError};

/// The PHP class name.
pub const CLASS_NAME: &str = "Uri\\Rfc3986\\Uri";

/// `Uri\Rfc3986\UriType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriType {
    AbsolutePathReference,
    RelativePathReference,
    NetworkPathReference,
    Uri,
}

impl UriType {
    /// The enum case name.
    pub fn name(self) -> &'static str {
        match self {
            UriType::AbsolutePathReference => "AbsolutePathReference",
            UriType::RelativePathReference => "RelativePathReference",
            UriType::NetworkPathReference => "NetworkPathReference",
            UriType::Uri => "Uri",
        }
    }
}

/// `Uri\Rfc3986\UriHostType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriHostType {
    IpV4,
    IpV6,
    IpVFuture,
    RegisteredName,
}

impl UriHostType {
    /// The enum case name.
    pub fn name(self) -> &'static str {
        match self {
            UriHostType::IpV4 => "IpV4",
            UriHostType::IpV6 => "IpV6",
            UriHostType::IpVFuture => "IpVFuture",
            UriHostType::RegisteredName => "RegisteredName",
        }
    }
}

fn invalid(message: &str) -> UriError {
    UriError::invalid_uri(message)
}

fn malformed(component: &str) -> UriError {
    invalid(&format!("The specified {component} is malformed"))
}

/// An initialized `Uri\Rfc3986\Uri` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rfc3986Uri {
    raw: Uri,
}

/// Parses (and optionally resolves) a URI; `Err(None)` is a silent failure.
fn parse_ex(input: &[u8], base: Option<&Rfc3986Uri>) -> Result<Uri, UriError> {
    let mut uri = r::parse(input).map_err(|_| malformed("URI"))?;
    if let Some(base) = base {
        uri = r::resolve(&uri, &base.raw).map_err(|ResolveError::RelativeBase| {
            invalid("The specified base URI must be absolute")
        })?;
    }
    if let Some(port) = &uri.port {
        if !port.is_empty() && port_str_to_long(port) == -1 {
            return Err(invalid("The port is out of range"));
        }
    }
    Ok(uri)
}

fn userinfo_parts(ui: &[u8]) -> (Vec<u8>, Vec<u8>) {
    match ui.iter().position(|&c| c == b':') {
        Some(i) => (ui[..i].to_vec(), ui[i + 1..].to_vec()),
        None => (ui.to_vec(), Vec::new()),
    }
}

impl Rfc3986Uri {
    /// Wraps an already parsed URI.
    pub fn from_uri(raw: Uri) -> Self {
        Rfc3986Uri { raw }
    }

    /// The underlying (raw) URI.
    pub fn raw(&self) -> &Uri {
        &self.raw
    }

    /// The normalized URI used by the non-raw getters, `toString()` and
    /// `equals()`.
    pub fn normalized(&self) -> Uri {
        r::normalize(&self.raw)
    }

    /// `new Uri\Rfc3986\Uri($uri, $baseUrl)` and `resolve()`.
    pub fn new(uri: &[u8], base: Option<&Rfc3986Uri>) -> Result<Self, UriError> {
        parse_ex(uri, base).map(|raw| Rfc3986Uri { raw })
    }

    /// `Uri\Rfc3986\Uri::parse($uri, $baseUrl)`: `None` instead of throwing.
    pub fn parse(uri: &[u8], base: Option<&Rfc3986Uri>) -> Option<Self> {
        Self::new(uri, base).ok()
    }

    /// `getUriType()`.
    pub fn uri_type(&self) -> UriType {
        if self.raw.scheme.is_some() {
            UriType::Uri
        } else if self.raw.has_host() {
            UriType::NetworkPathReference
        } else if self.raw.absolute_path {
            UriType::AbsolutePathReference
        } else {
            UriType::RelativePathReference
        }
    }

    /// `getScheme()`.
    pub fn scheme(&self) -> Option<Vec<u8>> {
        self.normalized().scheme
    }

    /// `getRawScheme()`.
    pub fn raw_scheme(&self) -> Option<Vec<u8>> {
        self.raw.scheme.clone()
    }

    /// `getUserInfo()`.
    pub fn user_info(&self) -> Option<Vec<u8>> {
        self.normalized().user_info
    }

    /// `getRawUserInfo()`.
    pub fn raw_user_info(&self) -> Option<Vec<u8>> {
        self.raw.user_info.clone()
    }

    /// `getUsername()`.
    pub fn username(&self) -> Option<Vec<u8>> {
        self.normalized().user_info.map(|ui| userinfo_parts(&ui).0)
    }

    /// `getRawUsername()`.
    pub fn raw_username(&self) -> Option<Vec<u8>> {
        self.raw.user_info.as_ref().map(|ui| userinfo_parts(ui).0)
    }

    /// `getPassword()`.
    pub fn password(&self) -> Option<Vec<u8>> {
        self.normalized().user_info.map(|ui| userinfo_parts(&ui).1)
    }

    /// `getRawPassword()`.
    pub fn raw_password(&self) -> Option<Vec<u8>> {
        self.raw.user_info.as_ref().map(|ui| userinfo_parts(ui).1)
    }

    /// `getHost()`.
    pub fn host(&self) -> Option<Vec<u8>> {
        self.normalized().host_string()
    }

    /// `getRawHost()`.
    pub fn raw_host(&self) -> Option<Vec<u8>> {
        self.raw.host_string()
    }

    /// `getHostType()`.
    pub fn host_type(&self) -> Option<UriHostType> {
        self.raw.host.as_ref().map(|h| match h.kind {
            HostKind::Ipv4(_) => UriHostType::IpV4,
            HostKind::Ipv6(_) => UriHostType::IpV6,
            HostKind::IpFuture => UriHostType::IpVFuture,
            HostKind::RegName => UriHostType::RegisteredName,
        })
    }

    /// `getPort()`.
    pub fn port(&self) -> Option<i64> {
        self.raw
            .port
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| port_str_to_long(p))
    }

    /// `getPath()`.
    pub fn path(&self) -> Vec<u8> {
        self.normalized().path_string()
    }

    /// `getRawPath()`.
    pub fn raw_path(&self) -> Vec<u8> {
        self.raw.path_string()
    }

    /// `getQuery()`.
    pub fn query(&self) -> Option<Vec<u8>> {
        self.normalized().query
    }

    /// `getRawQuery()`.
    pub fn raw_query(&self) -> Option<Vec<u8>> {
        self.raw.query.clone()
    }

    /// `getFragment()`.
    pub fn fragment(&self) -> Option<Vec<u8>> {
        self.normalized().fragment
    }

    /// `getRawFragment()`.
    pub fn raw_fragment(&self) -> Option<Vec<u8>> {
        self.raw.fragment.clone()
    }

    fn with(
        &self,
        f: impl FnOnce(&mut Uri) -> Result<(), SetError>,
        component: &str,
    ) -> Result<Self, UriError> {
        let mut raw = self.raw.clone();
        f(&mut raw).map_err(|e| set_error(e, component))?;
        Ok(Rfc3986Uri { raw })
    }

    /// `withScheme()`.
    pub fn with_scheme(&self, scheme: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(|u| u.set_scheme(scheme), "scheme")
    }

    /// `withUserInfo()`.
    pub fn with_user_info(&self, user_info: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(|u| u.set_user_info(user_info), "userinfo")
    }

    /// `withHost()`.
    pub fn with_host(&self, host: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(|u| u.set_host_auto(host), "host")
    }

    /// `withPort()`.
    pub fn with_port(&self, port: Option<i64>) -> Result<Self, UriError> {
        let text = port.map(|p| p.to_string().into_bytes());
        self.with(|u| u.set_port_text(text.as_deref()), "port")
    }

    /// `withPath()`.
    pub fn with_path(&self, path: &[u8]) -> Result<Self, UriError> {
        let value = if path.is_empty() { None } else { Some(path) };
        self.with(|u| u.set_path(value), "path")
    }

    /// `withQuery()`.
    pub fn with_query(&self, query: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(|u| u.set_query(query), "query")
    }

    /// `withFragment()`.
    pub fn with_fragment(&self, fragment: Option<&[u8]>) -> Result<Self, UriError> {
        self.with(|u| u.set_fragment(fragment), "fragment")
    }

    fn recompose(uri: &Uri, exclude_fragment: bool) -> Vec<u8> {
        let mut s = uri.to_bytes();
        if exclude_fragment {
            if let Some(pos) = s.iter().rposition(|&c| c == b'#') {
                s.truncate(pos);
            }
        }
        s
    }

    /// `equals()`.
    pub fn equals(&self, other: &Rfc3986Uri, mode: UriComparisonMode) -> bool {
        let exclude = mode == UriComparisonMode::ExcludeFragment;
        Self::recompose(&self.normalized(), exclude)
            == Self::recompose(&other.normalized(), exclude)
    }

    /// `toString()`.
    pub fn to_string_normalized(&self) -> Vec<u8> {
        self.normalized().to_bytes()
    }

    /// `toRawString()`.
    pub fn to_raw_string(&self) -> Vec<u8> {
        self.raw.to_bytes()
    }

    /// `resolve()`.
    pub fn resolve(&self, uri: &[u8]) -> Result<Self, UriError> {
        Self::new(uri, Some(self))
    }

    /// The `uri` string stored by `__serialize()` (`[["uri" => ...], []]`).
    pub fn serialize(&self) -> Vec<u8> {
        self.to_raw_string()
    }

    /// Restores an object from the `uri` string of `__unserialize()`; the
    /// VM validates the array shape and reports any failure with
    /// [`invalid_serialization_data`].
    pub fn unserialize(uri: &[u8]) -> Result<Self, UriError> {
        r::parse(uri)
            .ok()
            .filter(|u| {
                u.port
                    .as_ref()
                    .is_none_or(|p| p.is_empty() || port_str_to_long(p) != -1)
            })
            .map(|raw| Rfc3986Uri { raw })
            .ok_or_else(|| invalid_serialization_data(CLASS_NAME))
    }

    /// `__debugInfo()`: raw components in PHP's order.
    pub fn debug_info(&self) -> Vec<(&'static str, DebugValue)> {
        vec![
            ("scheme", DebugValue::from_opt(self.raw_scheme())),
            ("username", DebugValue::from_opt(self.raw_username())),
            ("password", DebugValue::from_opt(self.raw_password())),
            ("host", DebugValue::from_opt(self.raw_host())),
            (
                "port",
                self.port().map_or(DebugValue::Null, DebugValue::Int),
            ),
            ("path", DebugValue::Str(self.raw_path())),
            ("query", DebugValue::from_opt(self.raw_query())),
            ("fragment", DebugValue::from_opt(self.raw_fragment())),
        ]
    }
}

fn set_error(e: SetError, component: &str) -> UriError {
    match e {
        SetError::Syntax => malformed(component),
        SetError::UserInfoWithoutHost => invalid("Cannot set a userinfo without having a host"),
        SetError::PortWithoutHost => invalid("Cannot set a port without having a host"),
        SetError::RemoveHostWithUserInfo => {
            invalid("Cannot remove the host from a URI that has a userinfo")
        }
        SetError::RemoveHostWithPort => {
            invalid("Cannot remove the host from a URI that has a port")
        }
    }
}

/// `Uri\Rfc3986\UriBuilder` (its private properties).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rfc3986UriBuilder {
    pub scheme: Option<Vec<u8>>,
    pub userinfo: Option<Vec<u8>>,
    pub host: Option<Vec<u8>>,
    pub port: Option<i64>,
    pub path: Vec<u8>,
    pub query: Option<Vec<u8>>,
    pub fragment: Option<Vec<u8>>,
}

fn check(ok: bool, component: &str) -> Result<(), UriError> {
    if ok {
        Ok(())
    } else {
        Err(malformed(component))
    }
}

/// `php_uri_parser_rfc3986_host_validate`.
pub fn validate_host(host: &[u8]) -> bool {
    if host.is_empty() {
        return true;
    }
    if host[0] == b'[' {
        if host[host.len() - 1] != b']' {
            return false;
        }
        let inner = if host.len() >= 2 {
            &host[1..host.len() - 1]
        } else {
            &[][..]
        };
        if host.len() >= 2 && matches!(host.get(1), Some(b'v' | b'V')) {
            return r::is_well_formed_host_ip_future(inner);
        }
        return r::is_well_formed_host_ipv6(inner);
    }
    r::is_well_formed_ipv4(host) || r::is_well_formed_reg_name(host)
}

impl Rfc3986UriBuilder {
    /// `new Uri\Rfc3986\UriBuilder()` / `reset()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `setScheme()`.
    pub fn set_scheme(&mut self, scheme: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(s) = scheme {
            check(r::is_well_formed_scheme(s), "scheme")?;
        }
        self.scheme = scheme.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `setUserInfo()`.
    pub fn set_user_info(&mut self, user_info: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(s) = user_info {
            check(r::is_well_formed_userinfo(s), "userinfo")?;
        }
        self.userinfo = user_info.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `setHost()`.
    pub fn set_host(&mut self, host: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(s) = host {
            check(validate_host(s), "host")?;
        }
        self.host = host.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `setPort()`.
    pub fn set_port(&mut self, port: Option<i64>) -> Result<(), UriError> {
        if let Some(p) = port {
            check(r::is_well_formed_port(p.to_string().as_bytes()), "port")?;
        }
        self.port = port;
        Ok(())
    }

    /// `setPath()`.
    pub fn set_path(&mut self, path: &[u8]) -> Result<(), UriError> {
        check(r::is_well_formed_path(path, false), "path")?;
        self.path = path.to_vec();
        Ok(())
    }

    /// `setQuery()`.
    pub fn set_query(&mut self, query: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(s) = query {
            check(r::is_well_formed_query(s), "query")?;
        }
        self.query = query.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `setFragment()`.
    pub fn set_fragment(&mut self, fragment: Option<&[u8]>) -> Result<(), UriError> {
        if let Some(s) = fragment {
            check(r::is_well_formed_fragment(s), "fragment")?;
        }
        self.fragment = fragment.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `build()`.
    pub fn build(&self, base: Option<&Rfc3986Uri>) -> Result<Rfc3986Uri, UriError> {
        Rfc3986Uri::build_from_components(
            base,
            self.scheme.as_deref(),
            self.userinfo.as_deref(),
            self.host.as_deref(),
            self.port,
            &self.path,
            self.query.as_deref(),
            self.fragment.as_deref(),
        )
    }
}

impl Rfc3986Uri {
    /// `php_uri_parser_rfc3986_build_from_zval`: builds a URI from (possibly
    /// tampered-with) builder properties.
    #[allow(clippy::too_many_arguments)]
    pub fn build_from_components(
        base: Option<&Rfc3986Uri>,
        scheme: Option<&[u8]>,
        userinfo: Option<&[u8]>,
        host: Option<&[u8]>,
        port: Option<i64>,
        path: &[u8],
        query: Option<&[u8]>,
        fragment: Option<&[u8]>,
    ) -> Result<Rfc3986Uri, UriError> {
        if !path.is_empty() {
            if scheme.is_none() {
                let first_segment = path
                    .iter()
                    .take_while(|&&c| c != 0 && c != b'/')
                    .copied()
                    .collect::<Vec<u8>>();
                if first_segment.contains(&b':') {
                    return Err(invalid(
                        "The path must not begin with \":\" when the URI does not contain a scheme",
                    ));
                }
            }
            if host.is_none() && path.starts_with(b"//") {
                return Err(invalid(
                    "The path must not begin with \"//\" when the URI does not contain a host",
                ));
            }
        }
        let mut uri = Uri::default();
        uri.set_scheme(scheme).map_err(|e| set_error(e, "scheme"))?;
        uri.set_host_auto(host).map_err(|e| set_error(e, "host"))?;
        uri.set_user_info(userinfo)
            .map_err(|e| set_error(e, "userinfo"))?;
        let port_text = port.map(|p| p.to_string().into_bytes());
        uri.set_port_text(port_text.as_deref())
            .map_err(|e| set_error(e, "port"))?;
        let path_value = if path.is_empty() { None } else { Some(path) };
        uri.set_path(path_value).map_err(|e| set_error(e, "path"))?;
        uri.set_query(query).map_err(|e| set_error(e, "query"))?;
        uri.set_fragment(fragment)
            .map_err(|e| set_error(e, "fragment"))?;
        if let Some(base) = base {
            uri = r::resolve(&uri, &base.raw).map_err(|ResolveError::RelativeBase| {
                invalid("The specified base URI must be absolute")
            })?;
        }
        Ok(Rfc3986Uri { raw: uri })
    }
}

/// `Uri\UriError: The <component> component cannot be retrieved` (never
/// produced by this implementation, provided for completeness).
pub fn component_cannot_be_retrieved(component: &str) -> UriError {
    UriError::new(
        ErrorClass::UriError,
        format!("The {component} component cannot be retrieved"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: Option<Vec<u8>>) -> Option<String> {
        v.map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn getters() {
        let u = Rfc3986Uri::new(
            b"http://%61pple:p%61ss@ex%61mple.com/foob%61r?%61bc=%61bc#%61bc",
            None,
        )
        .unwrap();
        assert_eq!(s(u.username()).as_deref(), Some("apple"));
        assert_eq!(s(u.raw_username()).as_deref(), Some("%61pple"));
        assert_eq!(s(u.password()).as_deref(), Some("pass"));
        assert_eq!(s(u.host()).as_deref(), Some("example.com"));
        assert_eq!(u.path(), b"/foobar");
        assert_eq!(u.port(), None);
        assert_eq!(u.uri_type(), UriType::Uri);
    }

    #[test]
    fn errors() {
        assert_eq!(
            Rfc3986Uri::new(b"https://ex ample", None)
                .unwrap_err()
                .message,
            "The specified URI is malformed"
        );
        let base = Rfc3986Uri::new(b"/a", None).unwrap();
        assert_eq!(
            Rfc3986Uri::new(b"b", Some(&base)).unwrap_err().message,
            "The specified base URI must be absolute"
        );
        assert_eq!(
            Rfc3986Uri::new(b"https://h:99999999999999999999", None)
                .unwrap_err()
                .message,
            "The port is out of range"
        );
        let u = Rfc3986Uri::new(b"/a", None).unwrap();
        assert_eq!(
            u.with_port(Some(1)).unwrap_err().message,
            "Cannot set a port without having a host"
        );
        assert_eq!(
            u.with_user_info(Some(b"u")).unwrap_err().message,
            "Cannot set a userinfo without having a host"
        );
    }

    #[test]
    fn equality_and_strings() {
        let a = Rfc3986Uri::new(b"HTTPS://EXAMPLE.COM/a/../b#f", None).unwrap();
        let b = Rfc3986Uri::new(b"https://example.com/b#g", None).unwrap();
        assert!(a.equals(&b, UriComparisonMode::ExcludeFragment));
        assert!(!a.equals(&b, UriComparisonMode::IncludeFragment));
        assert_eq!(a.to_string_normalized(), b"https://example.com/b#f");
        assert_eq!(a.to_raw_string(), b"HTTPS://EXAMPLE.COM/a/../b#f");
    }

    #[test]
    fn builder() {
        let mut b = Rfc3986UriBuilder::new();
        b.set_scheme(Some(b"https")).unwrap();
        b.set_host(Some(b"example.com")).unwrap();
        b.set_user_info(Some(b"u:p")).unwrap();
        b.set_port(Some(8080)).unwrap();
        b.set_path(b"/a/b").unwrap();
        b.set_query(Some(b"q")).unwrap();
        b.set_fragment(Some(b"f")).unwrap();
        assert_eq!(
            b.build(None).unwrap().to_raw_string(),
            b"https://u:p@example.com:8080/a/b?q#f"
        );
        assert!(b.set_port(Some(-1)).is_err());
        let mut b = Rfc3986UriBuilder::new();
        b.set_path(b"a:b").unwrap();
        assert_eq!(
            b.build(None).unwrap_err().message,
            "The path must not begin with \":\" when the URI does not contain a scheme"
        );
    }

    #[test]
    fn serialization() {
        let u = Rfc3986Uri::new(b"https://example.com", None).unwrap();
        assert_eq!(Rfc3986Uri::unserialize(&u.serialize()).unwrap(), u);
        assert!(Rfc3986Uri::unserialize(b"%1").is_err());
    }
}
