// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A port of `php_url_parse_ex2` (`ext/standard/url.c`), the `parse_url()`
//! parser that `FILTER_VALIDATE_URL` uses through ext/uri's `"parse_url"`
//! URI parser.

/// The components of a parsed URL (`php_url`). Absent components are
/// `None`; control characters in components are replaced by `_`
/// (`php_replace_controlchars`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedUrl {
    /// `scheme`.
    pub scheme: Option<Vec<u8>>,
    /// `user`.
    pub user: Option<Vec<u8>>,
    /// `pass`.
    pub pass: Option<Vec<u8>>,
    /// `host`.
    pub host: Option<Vec<u8>>,
    /// `port` (0 means "no port", like the C struct).
    pub port: u16,
    /// Whether a port was present (`has_port`, distinguishes `:0`).
    pub has_port: bool,
    /// `path`.
    pub path: Option<Vec<u8>>,
    /// `query`.
    pub query: Option<Vec<u8>>,
    /// `fragment`.
    pub fragment: Option<Vec<u8>>,
}

fn component(s: &[u8]) -> Vec<u8> {
    s.iter()
        .map(|&c| if c < 0x20 || c == 0x7f { b'_' } else { c })
        .collect()
}

/// `binary_strcspn`: the index of the first byte of `chars` in
/// `s[start..end]`, or `end`.
fn strcspn(s: &[u8], start: usize, end: usize, chars: &[u8]) -> usize {
    s[start..end]
        .iter()
        .position(|c| chars.contains(c))
        .map_or(end, |p| start + p)
}

/// `ZEND_STRTOL(buf, &end, 10)` on a port buffer (which stops at NUL):
/// `Some(value)` when at least one digit was consumed.
fn strtol(buf: &[u8]) -> Option<i64> {
    let buf = buf.split(|&b| b == 0).next().unwrap_or_default();
    let mut i = 0;
    while i < buf.len() && (buf[i] == b' ' || (0x09..=0x0d).contains(&buf[i])) {
        i += 1;
    }
    let neg = match buf.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits = buf[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut v: i64 = 0;
    for &d in &buf[i..i + digits] {
        v = v.saturating_mul(10).saturating_add(i64::from(d - b'0'));
    }
    Some(if neg { -v } else { v })
}

fn port_value(buf: &[u8]) -> Option<u16> {
    strtol(buf).and_then(|p| u16::try_from(p).ok())
}

#[derive(Clone, Copy)]
enum Next {
    ParsePort,
    ParseHost,
    JustPath,
}

/// `php_url_parse_ex2`: parses `s`; `None` when PHP's `parse_url()` would
/// return `false`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_url(s: &[u8]) -> Option<ParsedUrl> {
    let mut ret = ParsedUrl::default();
    let ue = s.len();
    let mut start = 0usize;
    // `e`: position of the scheme colon when present.
    let colon = s.iter().position(|&c| c == b':');
    let mut e = colon.unwrap_or(0);

    let next = match colon {
        Some(c) if c != 0 => 'scheme: {
            // Validate the scheme.
            for &ch in &s[..c] {
                if !ch.is_ascii_alphanumeric() && ch != b'+' && ch != b'.' && ch != b'-' {
                    if c + 1 < ue && c < strcspn(s, 0, ue, b"?#") {
                        break 'scheme Next::ParsePort;
                    } else if 1 < ue && s[0] == b'/' && s[1] == b'/' {
                        start = 2;
                        break 'scheme Next::ParseHost;
                    }
                    break 'scheme Next::JustPath;
                }
            }
            if c + 1 == ue {
                // Only the scheme is available.
                ret.scheme = Some(component(&s[..c]));
                return Some(ret);
            }
            if s[c + 1] == b'/' {
                ret.scheme = Some(component(&s[..c]));
                if c + 2 < ue && s[c + 2] == b'/' {
                    start = c + 3;
                    if s[..c].eq_ignore_ascii_case(b"file") && c + 3 < ue && s[c + 3] == b'/' {
                        // Windows drive letters: file:///c:/dir/file.txt
                        if c + 5 < ue && s[c + 5] == b':' {
                            start = c + 4;
                        }
                        break 'scheme Next::JustPath;
                    }
                    Next::ParseHost
                } else {
                    start = c + 1;
                    Next::JustPath
                }
            } else {
                // Schemes such as mailto: may have no '/' after them, but
                // "a.com:80" is a host and a port.
                let mut p = c + 1;
                while p < ue && s[p].is_ascii_digit() {
                    p += 1;
                }
                if (p == ue || s[p] == b'/') && p - c < 7 {
                    break 'scheme Next::ParsePort;
                }
                ret.scheme = Some(component(&s[..c]));
                start = c + 1;
                Next::JustPath
            }
        }
        Some(_) => Next::ParsePort,
        None => {
            if 1 < ue && s[0] == b'/' && s[1] == b'/' {
                start = 2;
                Next::ParseHost
            } else {
                Next::JustPath
            }
        }
    };

    let next = if let Next::ParsePort = next {
        let p = e + 1;
        let mut pp = p;
        while pp < ue && pp - p < 6 && s[pp].is_ascii_digit() {
            pp += 1;
        }
        if pp - p > 0 && pp - p < 6 && (pp == ue || s[pp] == b'/') {
            let port = port_value(&s[p..pp])?;
            ret.has_port = true;
            ret.port = port;
            if start + 1 < ue && s[start] == b'/' && s[start + 1] == b'/' {
                start += 2;
            }
            Next::ParseHost
        } else if p == pp && pp == ue {
            return None;
        } else if start + 1 < ue && s[start] == b'/' && s[start + 1] == b'/' {
            start += 2;
            Next::ParseHost
        } else {
            Next::JustPath
        }
    } else {
        next
    };

    if let Next::ParseHost = next {
        e = strcspn(s, start, ue, b"/?#");
        // Login and password.
        if let Some(at) = s[start..e].iter().rposition(|&c| c == b'@') {
            let at = start + at;
            if let Some(colon) = s[start..at].iter().position(|&c| c == b':') {
                let colon = start + colon;
                ret.user = Some(component(&s[start..colon]));
                ret.pass = Some(component(&s[colon + 1..at]));
            } else {
                ret.user = Some(component(&s[start..at]));
            }
            start = at + 1;
        }
        // Port.
        let port_colon = if start < ue && s[start] == b'[' && e > 0 && s[e - 1] == b']' {
            // IPv6 literal: no port scan.
            None
        } else {
            s[start..e]
                .iter()
                .rposition(|&c| c == b':')
                .map(|p| start + p)
        };
        let host_end = if let Some(p) = port_colon {
            if ret.port == 0 {
                let digits = &s[p + 1..e];
                if digits.len() > 5 {
                    return None;
                } else if !digits.is_empty() {
                    let port = port_value(digits)?;
                    ret.has_port = true;
                    ret.port = port;
                }
            }
            p
        } else {
            e
        };
        if host_end <= start {
            return None;
        }
        ret.host = Some(component(&s[start..host_end]));
        if e == ue {
            return Some(ret);
        }
        start = e;
    }

    // just_path:
    let mut end = ue;
    if let Some(hash) = s[start..end].iter().position(|&c| c == b'#') {
        let p = start + hash + 1;
        ret.fragment = Some(if p < end {
            component(&s[p..end])
        } else {
            Vec::new()
        });
        end = p - 1;
    }
    if let Some(q) = s[start..end].iter().position(|&c| c == b'?') {
        let p = start + q + 1;
        ret.query = Some(if p < end {
            component(&s[p..end])
        } else {
            Vec::new()
        });
        end = p - 1;
    }
    if start < end || start == ue {
        ret.path = Some(component(&s[start..end]));
    }
    Some(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<ParsedUrl> {
        parse_url(s.as_bytes())
    }

    fn opt(v: Option<&Vec<u8>>) -> Option<&str> {
        v.map(|v| std::str::from_utf8(v).unwrap())
    }

    #[test]
    fn full() {
        let u = p("http://user:pass@www.example.com:8080/path/to?a=b#frag").unwrap();
        assert_eq!(opt(u.scheme.as_ref()), Some("http"));
        assert_eq!(opt(u.user.as_ref()), Some("user"));
        assert_eq!(opt(u.pass.as_ref()), Some("pass"));
        assert_eq!(opt(u.host.as_ref()), Some("www.example.com"));
        assert_eq!(u.port, 8080);
        assert_eq!(opt(u.path.as_ref()), Some("/path/to"));
        assert_eq!(opt(u.query.as_ref()), Some("a=b"));
        assert_eq!(opt(u.fragment.as_ref()), Some("frag"));
    }

    #[test]
    fn edge_cases() {
        assert_eq!(p("http://"), None);
        assert_eq!(p("http:///example.com"), None);
        assert_eq!(p("http://example.com:qq"), None);
        assert_eq!(p("http://example.com:-2"), None);
        assert_eq!(p("http://example.com:65536"), None);
        assert_eq!(opt(p("http:").unwrap().scheme.as_ref()), Some("http"));
        let u = p("mailto:foo@bar.com").unwrap();
        assert_eq!(opt(u.scheme.as_ref()), Some("mailto"));
        assert_eq!(opt(u.path.as_ref()), Some("foo@bar.com"));
        let u = p("a.com:80").unwrap();
        assert_eq!(opt(u.host.as_ref()), Some("a.com"));
        assert_eq!(u.port, 80);
        let u = p("//example.com/x").unwrap();
        assert_eq!(opt(u.host.as_ref()), Some("example.com"));
        assert_eq!(u.scheme, None);
        let u = p("http://[::1]:80/").unwrap();
        assert_eq!(opt(u.host.as_ref()), Some("[::1]"));
        let u = p("file:///tmp/x").unwrap();
        assert_eq!(opt(u.path.as_ref()), Some("/tmp/x"));
        assert_eq!(u.host, None);
        let u = p("/path?").unwrap();
        assert_eq!(opt(u.query.as_ref()), Some(""));
        assert_eq!(p(":"), None);
        let u = p("x\x01y").unwrap();
        assert_eq!(opt(u.path.as_ref()), Some("x_y"));
        assert_eq!(p("").unwrap().path, Some(Vec::new()));
    }
}
