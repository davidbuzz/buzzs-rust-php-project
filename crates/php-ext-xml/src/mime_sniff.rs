// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Charset sniffing from HTTP `Content-Type` headers
//! (`ext/libxml/mime_sniff.c`): the WHATWG "parse a MIME type" algorithm,
//! reduced to finding the first valid `charset` parameter.
//!
//! libxml2 uses the result as the input encoding of documents loaded
//! through a stream that carries response headers (`http://`), unless the
//! parser already has an encoding.

use php_streams::stream::Stream;
use php_streams::value::Value as SValue;

/// <https://fetch.spec.whatwg.org/#http-whitespace>
fn is_http_whitespace(c: u8) -> bool {
    matches!(c, 0x0A | 0x0D | 0x09 | 0x20)
}

/// <https://mimesniff.spec.whatwg.org/#http-quoted-string-token-code-point>
fn is_http_quoted_string_token(c: u8) -> bool {
    c == 0x09 || (c >= 0x20 && c != 0x7F)
}

/// <https://mimesniff.spec.whatwg.org/#http-token-code-point>
fn is_http_token(c: u8) -> bool {
    matches!(
        c,
        0x21 | 0x23..=0x27 | 0x2A | 0x2B | 0x2D | 0x2E | 0x5E | 0x5F | 0x60 | 0x7C | 0x7E
    ) || c.is_ascii_alphanumeric()
}

/// Length of the run of bytes at `s[pos..]` satisfying `cond`.
fn collect(s: &[u8], pos: usize, cond: impl Fn(u8) -> bool) -> usize {
    s[pos..].iter().take_while(|&&c| cond(c)).count()
}

fn is_valid_token(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| is_http_token(c))
}

/// <https://fetch.spec.whatwg.org/#collect-an-http-quoted-string> with
/// extract-value; `pos` points at the opening quote and is advanced.
fn collect_quoted(s: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut value = Vec::new();
    *pos += 1;
    loop {
        let n = collect(s, *pos, |c| c != b'"' && c != b'\\');
        value.extend_from_slice(&s[*pos..*pos + n]);
        *pos += n;
        if *pos >= s.len() {
            break;
        }
        let quote_or_backslash = s[*pos];
        *pos += 1;
        if quote_or_backslash == b'\\' {
            if *pos >= s.len() {
                value.push(b'\\');
                break;
            }
            value.push(s[*pos]);
            *pos += 1;
        } else {
            break;
        }
    }
    value
}

/// `php_libxml_sniff_charset_from_string()`: the `charset` parameter of a
/// MIME type, if the MIME type is valid and has a non-empty one.
pub fn sniff_charset_from_string(input: &[u8]) -> Option<Vec<u8>> {
    let start = input
        .iter()
        .position(|&c| !is_http_whitespace(c))
        .unwrap_or(input.len());
    let end = input
        .iter()
        .rposition(|&c| !is_http_whitespace(c))
        .map_or(start, |p| p + 1)
        .max(start);
    let s = &input[start..end];
    let mut pos = 0;
    let type_len = collect(s, pos, |c| c != b'/');
    if !is_valid_token(&s[..type_len]) {
        return None;
    }
    pos += type_len;
    if pos >= s.len() {
        return None;
    }
    pos += 1;
    let subtype_len = collect(s, pos, |c| c != b';');
    if !is_valid_token(&s[pos..pos + subtype_len]) {
        return None;
    }
    pos += subtype_len;
    while pos < s.len() {
        pos += 1;
        pos += collect(s, pos, is_http_whitespace);
        let name_len = collect(s, pos, |c| c != b';' && c != b'=');
        let name = &s[pos..pos + name_len];
        pos += name_len;
        if pos < s.len() {
            if s[pos] == b';' {
                continue;
            }
            pos += 1;
        } else {
            break;
        }
        let value = if s.get(pos) == Some(&b'"') {
            let v = collect_quoted(s, &mut pos);
            pos += collect(s, pos, |c| c != b';');
            v
        } else {
            let n = collect(s, pos, |c| c != b';');
            let mut v = s[pos..pos + n].to_vec();
            pos += n;
            while v.last().is_some_and(|&c| is_http_whitespace(c)) {
                v.pop();
            }
            if v.is_empty() {
                continue;
            }
            v
        };
        if name.eq_ignore_ascii_case(b"charset")
            && value.iter().all(|&c| is_http_quoted_string_token(c))
        {
            return Some(value);
        }
    }
    None
}

/// `php_libxml_sniff_charset_from_stream()`: scans the stream's response
/// headers (`wrapper_data`, newest last) for the last `Content-Type`.
pub fn sniff_charset_from_headers(headers: &[&[u8]]) -> Option<Vec<u8>> {
    for header in headers.iter().rev() {
        // No colon (or a space before it): the status line; give up.
        let colon = header.iter().position(|&c| c == b':')?;
        match header.iter().position(|&c| c == b' ') {
            Some(space) if space > colon => {}
            _ => return None,
        }
        let prefix = b"content-type:";
        if header.len() >= prefix.len() && header[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return sniff_charset_from_string(&header[prefix.len()..]);
        }
    }
    None
}

/// [`sniff_charset_from_headers`] over a stream's `wrapper_data`.
pub fn sniff_charset_from_stream(stream: &Stream) -> Option<Vec<u8>> {
    let Some(SValue::Array(items)) = &stream.wrapper_data else {
        return None;
    };
    let mut headers = Vec::with_capacity(items.len());
    // Non-string entries are skipped (the IS_STRING check of the C loop).
    for (_, v) in items {
        if let SValue::Str(s) = v {
            headers.push(s.as_slice());
        }
    }
    sniff_charset_from_headers(&headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Option<String> {
        sniff_charset_from_string(s.as_bytes()).map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    #[test]
    fn plain_and_quoted_charsets() {
        assert_eq!(
            cs("text/html; charset=ISO-8859-1"),
            Some("ISO-8859-1".into())
        );
        assert_eq!(cs(" text/xml;charset=\"utf-8\" "), Some("utf-8".into()));
        assert_eq!(
            cs("text/xml; foo=bar; CHARSET=latin1"),
            Some("latin1".into())
        );
        assert_eq!(cs("text/xml; charset=\"a\\\"b\"; x"), Some("a\"b".into()));
        assert_eq!(cs("text/xml; charset=; charset=x"), Some("x".into()));
    }

    #[test]
    fn invalid_mime_types() {
        assert_eq!(cs("text; charset=x"), None);
        assert_eq!(cs("/xml; charset=x"), None);
        assert_eq!(cs("te xt/xml; charset=x"), None);
        assert_eq!(cs("text/xml"), None);
        assert_eq!(cs(""), None);
    }

    #[test]
    fn headers_are_scanned_backwards() {
        let h: [&[u8]; 4] = [
            b"HTTP/1.1 302 Found",
            b"Content-Type: text/html; charset=ascii",
            b"HTTP/1.1 200 OK",
            b"Content-Type: text/xml; charset=ISO-8859-1",
        ];
        assert_eq!(sniff_charset_from_headers(&h), Some(b"ISO-8859-1".to_vec()));
        let h: [&[u8]; 2] = [b"Content-Type: text/xml; charset=x", b"HTTP/1.1 200 OK"];
        assert_eq!(sniff_charset_from_headers(&h), None);
        let h: [&[u8]; 1] = [b"Content-Type:text/xml;charset=x"];
        assert_eq!(sniff_charset_from_headers(&h), None);
    }
}
