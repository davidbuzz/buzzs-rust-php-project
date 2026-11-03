//! Component setters and well-formedness checks (uriparser's `uriSet*` and
//! `uriIsWellFormed*` functions).

use super::parse::{parse, parse_ipv4, parse_ipv6};
use super::{is_alpha, is_hex, is_pchar_wo_pct, is_sub_delim, is_unreserved, Host, HostKind, Uri};

/// Why a setter refused a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetError {
    /// `URI_ERROR_SYNTAX`.
    Syntax,
    /// `URI_ERROR_SETUSERINFO_HOST_NOT_SET`.
    UserInfoWithoutHost,
    /// `URI_ERROR_SETPORT_HOST_NOT_SET`.
    PortWithoutHost,
    /// `URI_ERROR_SETHOST_USERINFO_SET`.
    RemoveHostWithUserInfo,
    /// `URI_ERROR_SETHOST_PORT_SET`.
    RemoveHostWithPort,
}

fn all_chars(s: &[u8], ok: impl Fn(u8) -> bool) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            if s.len() - i < 3 || !is_hex(s[i + 1]) || !is_hex(s[i + 2]) {
                return false;
            }
            i += 3;
            continue;
        }
        if !ok(s[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
pub fn is_well_formed_scheme(s: &[u8]) -> bool {
    match s.split_first() {
        Some((&first, rest)) => {
            is_alpha(first)
                && rest
                    .iter()
                    .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        }
        None => false,
    }
}

/// `userinfo = *( unreserved / pct-encoded / sub-delims / ":" )`.
pub fn is_well_formed_userinfo(s: &[u8]) -> bool {
    all_chars(s, |c| is_unreserved(c) || is_sub_delim(c) || c == b':')
}

/// `reg-name = *( unreserved / pct-encoded / sub-delims )`.
pub fn is_well_formed_reg_name(s: &[u8]) -> bool {
    all_chars(s, |c| is_unreserved(c) || is_sub_delim(c))
}

/// `port = *DIGIT`.
pub fn is_well_formed_port(s: &[u8]) -> bool {
    s.iter().all(u8::is_ascii_digit)
}

/// The simplified path check of `uriIsWellFormedPathA`.
pub fn is_well_formed_path(s: &[u8], has_host: bool) -> bool {
    if has_host && s.first() != Some(&b'/') {
        return false;
    }
    all_chars(s, |c| is_pchar_wo_pct(c) || c == b'/')
}

/// `query = *( pchar / "/" / "?" )`.
pub fn is_well_formed_query(s: &[u8]) -> bool {
    all_chars(s, |c| is_pchar_wo_pct(c) || c == b'/' || c == b'?')
}

/// `fragment = *( pchar / "/" / "?" )`.
pub fn is_well_formed_fragment(s: &[u8]) -> bool {
    is_well_formed_query(s)
}

/// IPv6 address text without brackets.
pub fn is_well_formed_host_ipv6(s: &[u8]) -> bool {
    parse_ipv6(s).is_some()
}

/// `IPvFuture` text without brackets.
pub fn is_well_formed_host_ip_future(s: &[u8]) -> bool {
    if !matches!(s.first(), Some(b'v' | b'V')) {
        return false;
    }
    let mut candidate = b"//[".to_vec();
    candidate.extend_from_slice(s);
    candidate.push(b']');
    parse(&candidate).is_ok_and(|u| u.host.is_some_and(|h| h.kind == HostKind::IpFuture))
}

/// `FixPathNoScheme`: a relative path whose first segment contains a colon
/// gets a `./` prefix once the scheme is gone.
fn fix_path_no_scheme(uri: &mut Uri) {
    if uri.absolute_path || uri.segments.is_empty() || uri.scheme.is_some() || uri.has_host() {
        return;
    }
    if uri.segments[0].contains(&b':') {
        uri.segments.insert(0, b".".to_vec());
    }
}

/// `EnsureThatPathIsNotMistakenForHost`.
fn ensure_path_not_mistaken_for_host(uri: &mut Uri) {
    if uri.has_host() || !uri.absolute_path || uri.segments.len() < 2 || !uri.segments[0].is_empty()
    {
        return;
    }
    uri.segments.insert(0, b".".to_vec());
}

impl Uri {
    /// `uriSetSchemeMmA`.
    pub fn set_scheme(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if let Some(v) = value {
            if !is_well_formed_scheme(v) {
                return Err(SetError::Syntax);
            }
        }
        self.scheme = value.map(<[u8]>::to_vec);
        if value.is_none() {
            fix_path_no_scheme(self);
        }
        Ok(())
    }

    /// `uriSetUserInfoMmA`.
    pub fn set_user_info(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if value.is_some() && !self.has_host() {
            return Err(SetError::UserInfoWithoutHost);
        }
        if let Some(v) = value {
            if !is_well_formed_userinfo(v) {
                return Err(SetError::Syntax);
            }
        }
        self.user_info = value.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `uriSetPortTextMmA`.
    pub fn set_port_text(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if value.is_some() && !self.has_host() {
            return Err(SetError::PortWithoutHost);
        }
        if let Some(v) = value {
            if !is_well_formed_port(v) {
                return Err(SetError::Syntax);
            }
        }
        self.port = value.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `uriSetQueryMmA`.
    pub fn set_query(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if let Some(v) = value {
            if !is_well_formed_query(v) {
                return Err(SetError::Syntax);
            }
        }
        self.query = value.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `uriSetFragmentMmA`.
    pub fn set_fragment(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if let Some(v) = value {
            if !is_well_formed_fragment(v) {
                return Err(SetError::Syntax);
            }
        }
        self.fragment = value.map(<[u8]>::to_vec);
        Ok(())
    }

    /// `uriSetHostAutoMmA`: detects the host type from the text (IP
    /// literals must be bracketed).
    pub fn set_host_auto(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        let Some(v) = value.filter(|v| !v.is_empty()) else {
            return self.internal_set_host(value.map(|_| (Vec::new(), HostKind::RegName)));
        };
        if v[0] == b'[' {
            if v.len() < 2 || v[v.len() - 1] != b']' {
                return Err(SetError::Syntax);
            }
            let inner = &v[1..v.len() - 1];
            if inner.is_empty() {
                return Err(SetError::Syntax);
            }
            if matches!(inner[0], b'v' | b'V') {
                if !is_well_formed_host_ip_future(inner) {
                    return Err(SetError::Syntax);
                }
                return self.internal_set_host(Some((inner.to_vec(), HostKind::IpFuture)));
            }
            let bytes = parse_ipv6(inner).ok_or(SetError::Syntax)?;
            return self.internal_set_host(Some((inner.to_vec(), HostKind::Ipv6(bytes))));
        }
        if let Some(octets) = parse_ipv4(v) {
            return self.internal_set_host(Some((v.to_vec(), HostKind::Ipv4(octets))));
        }
        if !is_well_formed_reg_name(v) {
            return Err(SetError::Syntax);
        }
        self.internal_set_host(Some((v.to_vec(), HostKind::RegName)))
    }

    fn internal_set_host(&mut self, value: Option<(Vec<u8>, HostKind)>) -> Result<(), SetError> {
        if value.is_none() {
            if self.user_info.is_some() {
                return Err(SetError::RemoveHostWithUserInfo);
            }
            if self.port.is_some() {
                return Err(SetError::RemoveHostWithPort);
            }
        }
        let had_host = self.has_host();
        match value {
            None => {
                self.host = None;
                if had_host {
                    if !self.segments.is_empty() {
                        self.absolute_path = true;
                    }
                    ensure_path_not_mistaken_for_host(self);
                }
            }
            Some((text, kind)) => {
                self.host = Some(Host { text, kind });
                self.absolute_path = false;
            }
        }
        Ok(())
    }

    /// `uriSetPathMmA` (an empty value must be passed as `None`).
    pub fn set_path(&mut self, value: Option<&[u8]>) -> Result<(), SetError> {
        if let Some(v) = value {
            if !is_well_formed_path(v, self.has_host()) {
                return Err(SetError::Syntax);
            }
        }
        self.segments.clear();
        self.absolute_path = false;
        let Some(mut v) = value else {
            return Ok(());
        };
        if self.has_host() {
            v = &v[1..];
        } else if v.is_empty() {
            return Ok(());
        }
        let mut candidate = b"///".to_vec();
        candidate.extend_from_slice(v);
        let temp = parse(&candidate).map_err(|_| SetError::Syntax)?;
        self.segments = temp.segments;
        self.absolute_path = false;
        // TransformEmptyLeadPathSegments
        if !self.has_host() && self.segments.first().is_some_and(Vec::is_empty) {
            self.segments.remove(0);
            self.absolute_path = true;
        }
        fix_path_no_scheme(self);
        ensure_path_not_mistaken_for_host(self);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Uri {
        parse(s.as_bytes()).unwrap()
    }

    fn s(u: &Uri) -> String {
        String::from_utf8(u.to_bytes()).unwrap()
    }

    #[test]
    fn validators() {
        assert!(is_well_formed_scheme(b"a+b-c.d"));
        assert!(!is_well_formed_scheme(b""));
        assert!(!is_well_formed_scheme(b"1a"));
        assert!(is_well_formed_userinfo(b"a:b%20"));
        assert!(!is_well_formed_userinfo(b"a@b"));
        assert!(is_well_formed_port(b""));
        assert!(!is_well_formed_port(b"-1"));
        assert!(is_well_formed_path(b"/a:b@c", true));
        assert!(!is_well_formed_path(b"a", true));
        assert!(!is_well_formed_query(b"a#b"));
        assert!(is_well_formed_host_ip_future(b"v1.x"));
        assert!(!is_well_formed_host_ip_future(b"v1"));
    }

    #[test]
    fn set_path_cases() {
        let mut u = p("https://example.com");
        u.set_path(Some(b"/foo%2Fb%61r")).unwrap();
        assert_eq!(s(&u), "https://example.com/foo%2Fb%61r");
        let mut u = p("");
        u.set_path(Some(b"a:b")).unwrap();
        assert_eq!(s(&u), "./a:b");
        let mut u = p("");
        u.set_path(Some(b"//a")).unwrap();
        assert_eq!(s(&u), "/.//a");
        let mut u = p("");
        u.set_path(Some(b"/")).unwrap();
        assert_eq!(s(&u), "/");
        let mut u = p("https://h");
        assert_eq!(u.set_path(Some(b"a")), Err(SetError::Syntax));
    }

    #[test]
    fn host_setter() {
        let mut u = p("https://user@h/a");
        assert_eq!(u.set_host_auto(None), Err(SetError::RemoveHostWithUserInfo));
        let mut u = p("https://h/a//b");
        u.set_host_auto(None).unwrap();
        assert_eq!(s(&u), "https:/a//b");
        let mut u = p("https://h//b");
        u.set_host_auto(None).unwrap();
        assert_eq!(s(&u), "https:/.//b");
        let mut u = p("/a");
        u.set_host_auto(Some(b"[::1]")).unwrap();
        assert_eq!(u.host_string().unwrap(), b"[::1]");
        assert_eq!(u.set_host_auto(Some(b"[::1")), Err(SetError::Syntax));
        assert_eq!(u.set_host_auto(Some(b"a b")), Err(SetError::Syntax));
    }

    #[test]
    fn scheme_setter() {
        let mut u = p("https:a:b");
        u.set_scheme(None).unwrap();
        assert_eq!(s(&u), "./a:b");
        assert_eq!(u.set_scheme(Some(b"")), Err(SetError::Syntax));
    }
}
