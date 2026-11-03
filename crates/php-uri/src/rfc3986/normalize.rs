//! RFC 3986 §6.2.2 syntax-based normalization as implemented by uriparser's
//! `uriNormalizeSyntaxMaskRequiredExA` / `uriNormalizeSyntaxExMmA`.

use super::{hex_val, is_unreserved, HostKind, Uri};

/// Which components need normalization (`URI_NORMALIZE_*` bits).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct NormalizeMask {
    pub scheme: bool,
    pub user_info: bool,
    pub host: bool,
    pub port: bool,
    pub path: bool,
    pub query: bool,
    pub fragment: bool,
}

impl NormalizeMask {
    /// Whether nothing needs to change.
    pub fn is_normalized(self) -> bool {
        self == NormalizeMask::default()
    }

    /// Computes the mask for `uri`.
    pub fn required(uri: &Uri) -> Self {
        let mut m = NormalizeMask::default();
        if let Some(s) = &uri.scheme {
            m.scheme = s.iter().any(u8::is_ascii_uppercase);
        }
        if let Some(h) = &uri.host {
            m.host = h.text.iter().any(u8::is_ascii_uppercase) || contains_ugly_percent(&h.text);
        }
        if let Some(p) = &uri.port {
            m.port = p.len() > 1 && p[0] == b'0';
        }
        if let Some(u) = &uri.user_info {
            m.user_info = contains_ugly_percent(u);
        }
        m.path = uri
            .segments
            .iter()
            .any(|s| s == b"." || s == b".." || contains_ugly_percent(s));
        if let Some(q) = &uri.query {
            m.query = contains_ugly_percent(q);
        }
        if let Some(f) = &uri.fragment {
            m.fragment = contains_ugly_percent(f);
        }
        m
    }
}

/// Lowercase hex digits or percent-encoded unreserved characters.
fn contains_ugly_percent(s: &[u8]) -> bool {
    let mut i = 0;
    while i + 2 < s.len() {
        if s[i] == b'%' {
            if s[i + 1].is_ascii_lowercase() && s[i + 1].is_ascii_hexdigit()
                || s[i + 2].is_ascii_lowercase() && s[i + 2].is_ascii_hexdigit()
            {
                return true;
            }
            let code = hex_val(s[i + 1]) * 16 + hex_val(s[i + 2]);
            if is_unreserved(code) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Decodes percent-encoded unreserved characters and uppercases the
/// remaining percent-encodings (`FixPercentEncodingEngine`).
fn fix_percent_encoding(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i + 2 < s.len() {
        if s[i] == b'%' {
            let (l, r) = (hex_val(s[i + 1]), hex_val(s[i + 2]));
            let code = l * 16 + r;
            if is_unreserved(code) {
                out.push(code);
            } else {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push(b'%');
                out.push(HEX[usize::from(l)]);
                out.push(HEX[usize::from(r)]);
            }
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out.extend_from_slice(&s[i..]);
    out
}

fn lowercase_except_percent(s: &mut [u8]) {
    let mut i = 0;
    while i < s.len() {
        if s[i].is_ascii_uppercase() {
            s[i] = s[i].to_ascii_lowercase();
        } else if s[i] == b'%' {
            if i + 3 >= s.len() {
                return;
            }
            i += 2;
        }
        i += 1;
    }
}

/// Returns the syntax-normalized form of `uri`.
pub fn normalize(uri: &Uri) -> Uri {
    let mask = NormalizeMask::required(uri);
    let mut u = uri.clone();
    if mask.is_normalized() {
        return u;
    }
    if mask.scheme {
        if let Some(s) = &mut u.scheme {
            s.make_ascii_lowercase();
        }
    }
    if mask.host {
        if let Some(h) = &mut u.host {
            match h.kind {
                HostKind::IpFuture => h.text.make_ascii_lowercase(),
                HostKind::Ipv4(_) => {}
                _ => {
                    h.text = fix_percent_encoding(&h.text);
                    lowercase_except_percent(&mut h.text);
                }
            }
        }
    }
    if mask.port {
        if let Some(p) = &mut u.port {
            let first_non_zero = p.iter().position(|&c| c != b'0').unwrap_or(p.len() - 1);
            p.drain(..first_non_zero);
        }
    }
    if mask.user_info {
        if let Some(ui) = &mut u.user_info {
            *ui = fix_percent_encoding(ui);
        }
    }
    if mask.path {
        let relative = u.scheme.is_none() && !u.absolute_path;
        for seg in &mut u.segments {
            *seg = fix_percent_encoding(seg);
        }
        remove_dot_segments(&mut u, relative);
        u.fix_empty_trail_segment();
    }
    if mask.query {
        if let Some(q) = &mut u.query {
            *q = fix_percent_encoding(q);
        }
    }
    if mask.fragment {
        if let Some(f) = &mut u.fragment {
            *f = fix_percent_encoding(f);
        }
    }
    u
}

/// `RemoveDotSegmentsEx`.
pub(crate) fn remove_dot_segments(uri: &mut Uri, relative: bool) {
    let has_host = uri.has_host();
    let absolute = uri.absolute_path;
    let segs = &mut uri.segments;
    let mut i = 0;
    while i < segs.len() {
        if segs[i] == b"." {
            // A leading "." is essential before "//" (no host) and before a
            // first segment containing ":" (relative references).
            let essential = i == 0
                && segs.len() > 1
                && ((segs[1].is_empty() && !has_host) || (relative && segs[1].contains(&b':')));
            if !essential {
                if i + 1 < segs.len() {
                    segs.remove(i);
                } else {
                    if i == 0 && !has_host {
                        segs.clear();
                    } else {
                        segs[i].clear();
                    }
                    break;
                }
                continue;
            }
        } else if segs[i] == b".." {
            let keep = relative && (i == 0 || segs[i - 1] == b"..");
            if !keep {
                if i > 0 {
                    if i > 1 {
                        if i + 1 < segs.len() {
                            segs.drain(i - 1..=i);
                            i -= 1;
                        } else {
                            segs.truncate(i - 1);
                            segs.push(Vec::new());
                            break;
                        }
                    } else if i + 1 < segs.len() {
                        segs.drain(0..2);
                        i = 0;
                    } else {
                        segs.clear();
                        segs.push(Vec::new());
                        break;
                    }
                } else if i + 1 < segs.len() {
                    segs.remove(0);
                } else {
                    if absolute {
                        segs.clear();
                    } else {
                        segs[0].clear();
                    }
                    break;
                }
                continue;
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn n(s: &str) -> String {
        String::from_utf8(normalize(&parse(s.as_bytes()).unwrap()).to_bytes()).unwrap()
    }

    #[test]
    fn case_and_percent() {
        assert_eq!(n("HTTP://EXAMPLE.com/%7e%2f"), "http://example.com/~%2F");
        assert_eq!(n("http://ex%61mple.com"), "http://example.com");
        assert_eq!(n("http://h:0080/"), "http://h:80/");
        assert_eq!(n("http://h:0000/"), "http://h:0/");
        assert_eq!(n("http://%61pple@h"), "http://apple@h");
    }

    #[test]
    fn dot_segments() {
        assert_eq!(n("http://a/b/c/./../../g"), "http://a/g");
        assert_eq!(n("http://a/.."), "http://a/");
        assert_eq!(n("http://a/b/.."), "http://a/");
        assert_eq!(n("../a/./b"), "../a/b");
        assert_eq!(n("a/../../b"), "../b");
        assert_eq!(n("./a:b"), "./a:b");
        assert_eq!(n("/.//a"), "/.//a");
        assert_eq!(n("/a/."), "/a/");
        assert_eq!(n("."), "");
    }

    #[test]
    fn already_normal() {
        assert!(NormalizeMask::required(&parse(b"http://a/b").unwrap()).is_normalized());
        assert_eq!(n("http://a/b"), "http://a/b");
    }
}
