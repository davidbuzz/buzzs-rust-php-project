//! Response headers: `sapi_header_op()` / `sapi_send_headers()` from
//! `main/SAPI.c` combined with the apache2handler header handler
//! (`php_apache_sapi_header_handler` / `php_apache_sapi_send_headers`).
//!
//! As in `mod_php`, every header change is mirrored into `r->headers_out`
//! immediately (so `apache_response_headers()` sees it), while the SAPI's
//! own list backs `headers_list()`.  `Content-Type` and `Content-Length`
//! are routed to `ap_set_content_type` / `ap_set_content_length` instead
//! of the table.

use crate::backend::{Backend, Table, TableOp};

/// Request properties consulted by the header logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFacts {
    /// `SG(request_info).request_method`.
    pub method: Vec<u8>,
    /// `SG(request_info).proto_num` (1001 = HTTP/1.1).
    pub proto_num: i32,
    /// `default_mimetype`.
    pub default_mimetype: Vec<u8>,
    /// `default_charset`.
    pub default_charset: Vec<u8>,
}

impl Default for HeaderFacts {
    fn default() -> Self {
        HeaderFacts {
            method: b"GET".to_vec(),
            proto_num: 1001,
            default_mimetype: b"text/html".to_vec(),
            default_charset: b"UTF-8".to_vec(),
        }
    }
}

/// A header operation (`sapi_header_op_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderOp<'a> {
    /// `header($line, true, $code)`.
    Replace(&'a [u8], i64),
    /// `header($line, false, $code)`.
    Add(&'a [u8], i64),
    /// `header_remove($name)`.
    Delete(&'a [u8]),
    /// `header_remove()`.
    DeleteAll,
    /// Set the response code.
    SetStatus(i32),
}

/// Why a header operation failed with a warning (`sapi_error(E_WARNING)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// Output already started.
    AlreadySent,
    /// `header_remove("A: b")`.
    DeleteColon,
    /// CR/LF inside a header.
    NewLine,
    /// NUL inside a header.
    Nul,
}

impl HeaderError {
    /// The warning text PHP emits.
    pub fn message(&self) -> &'static str {
        match self {
            HeaderError::AlreadySent => "Cannot modify header information - headers already sent",
            HeaderError::DeleteColon => "Header to delete may not contain colon.",
            HeaderError::NewLine => {
                "Header may not contain more than a single header, new line detected"
            }
            HeaderError::Nul => "Header may not contain NUL bytes",
        }
    }
}

/// `SG(sapi_headers)` plus the apache2handler context's `content_type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapiHeaders {
    /// Header lines (`headers_list()`).
    pub headers: Vec<Vec<u8>>,
    /// `http_response_code`.
    pub response_code: i32,
    /// `http_status_line` (`header("HTTP/1.1 404 Not Found")`).
    pub status_line: Option<Vec<u8>>,
    /// `mimetype` (first Content-Type, after the default charset).
    pub mimetype: Option<Vec<u8>>,
    /// Whether the default Content-Type is still to be added.
    pub send_default_content_type: bool,
    /// `SG(headers_sent)`.
    pub sent: bool,
    /// `ctx->content_type`: the value for `ap_set_content_type`.
    pub content_type: Option<Vec<u8>>,
}

impl SapiHeaders {
    /// Fresh state; `status` is `r->status` (0 means 200).
    pub fn new(status: i32) -> Self {
        SapiHeaders {
            headers: Vec::new(),
            response_code: if status == 0 { 200 } else { status },
            status_line: None,
            mimetype: None,
            send_default_content_type: true,
            sent: false,
            content_type: None,
        }
    }

    /// `sapi_update_response_code()`.
    fn update_response_code(&mut self, code: i32) {
        if self.response_code == code {
            return;
        }
        self.status_line = None;
        self.response_code = code;
    }

    /// `sapi_header_op()`.  `Ok(false)` is a silent failure (empty line).
    pub fn op(
        &mut self,
        op: HeaderOp<'_>,
        facts: &HeaderFacts,
        backend: &dyn Backend,
    ) -> Result<bool, HeaderError> {
        if self.sent {
            return Err(HeaderError::AlreadySent);
        }
        let (line, replace, response_code) = match op {
            HeaderOp::SetStatus(code) => {
                self.update_response_code(code);
                return Ok(true);
            }
            HeaderOp::DeleteAll => {
                backend.table_op(Table::HeadersOut, TableOp::Clear, b"", b"");
                self.headers.clear();
                return Ok(true);
            }
            HeaderOp::Delete(l) => (l, false, 0),
            HeaderOp::Replace(l, c) => (l, true, c),
            HeaderOp::Add(l, c) => (l, false, c),
        };
        if line.is_empty() {
            return Ok(false);
        }
        let mut line = line.to_vec();
        while line.last().is_some_and(|&c| is_c_space(c)) {
            line.pop();
        }
        if let HeaderOp::Delete(_) = op {
            if line.contains(&b':') {
                return Err(HeaderError::DeleteColon);
            }
            let key = c_str(&line);
            backend.table_op(Table::HeadersOut, TableOp::Unset, key, b"");
            remove_header(&mut self.headers, &line);
            return Ok(true);
        }
        if line.iter().any(|&c| c == b'\n' || c == b'\r') {
            return Err(HeaderError::NewLine);
        }
        if line.contains(&0) {
            return Err(HeaderError::Nul);
        }
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"HTTP/") {
            self.update_response_code(extract_response_code(&line));
            self.status_line = Some(line);
            return Ok(true);
        }
        let mut header = line.clone();
        if let Some(colon) = line.iter().position(|&c| c == b':') {
            let name = &line[..colon];
            if name.eq_ignore_ascii_case(b"Content-Type") {
                let mut mimetype: Vec<u8> = line[colon + 1..]
                    .iter()
                    .copied()
                    .skip_while(|&c| c == b' ')
                    .collect();
                let applied = apply_default_charset(&mut mimetype, &facts.default_charset);
                if self.mimetype.is_none() {
                    self.mimetype = Some(mimetype.clone());
                }
                if applied {
                    header = b"Content-type: ".to_vec();
                    header.extend_from_slice(&mimetype);
                }
                self.send_default_content_type = false;
            } else if name.eq_ignore_ascii_case(b"Location") {
                let code = self.response_code;
                if !(300..=399).contains(&code) && code != 201 {
                    if response_code != 0 {
                        self.update_response_code(clamp_code(response_code));
                    } else if facts.proto_num > 1000
                        && !facts.method.is_empty()
                        && facts.method != b"HEAD"
                        && facts.method != b"GET"
                    {
                        self.update_response_code(303);
                    } else {
                        self.update_response_code(302);
                    }
                }
            } else if name.eq_ignore_ascii_case(b"WWW-Authenticate") {
                self.update_response_code(401);
            }
        }
        if response_code != 0 {
            self.update_response_code(clamp_code(response_code));
        }
        self.add_op(replace, header, backend);
        Ok(true)
    }

    /// `sapi_header_add_op()` with the apache2handler header handler.
    fn add_op(&mut self, replace: bool, header: Vec<u8>, backend: &dyn Backend) {
        if !self.apache_header_handler(replace, &header, backend) {
            return;
        }
        if replace {
            if let Some(colon) = header.iter().position(|&c| c == b':') {
                remove_header(&mut self.headers, &header[..colon]);
            }
        }
        self.headers.push(header);
    }

    /// `php_apache_sapi_header_handler()` for ADD/REPLACE: returns whether
    /// the header is kept in the SAPI list (`SAPI_HEADER_ADD`).
    fn apache_header_handler(
        &mut self,
        replace: bool,
        header: &[u8],
        backend: &dyn Backend,
    ) -> bool {
        let Some(colon) = header.iter().position(|&c| c == b':') else {
            return false;
        };
        let key = &header[..colon];
        let val: Vec<u8> = header[colon + 1..]
            .iter()
            .copied()
            .skip_while(|&c| c == b' ')
            .collect();
        if key.eq_ignore_ascii_case(b"content-type") {
            self.content_type = Some(val);
        } else if key.eq_ignore_ascii_case(b"content-length") {
            backend.set_content_length(strtol(&val));
        } else if replace {
            backend.table_op(Table::HeadersOut, TableOp::Set, key, &val);
        } else {
            backend.table_op(Table::HeadersOut, TableOp::Add, key, &val);
        }
        true
    }

    /// `sapi_send_headers()` + `php_apache_sapi_send_headers()`.  Returns
    /// `false` when the headers had already been sent.
    pub fn send(&mut self, facts: &HeaderFacts, backend: &dyn Backend) -> bool {
        if self.sent {
            return false;
        }
        if self.send_default_content_type {
            let default = default_content_type(facts);
            if !default.is_empty() {
                self.mimetype = Some(default.clone());
                let mut h = b"Content-type: ".to_vec();
                h.extend_from_slice(&default);
                self.add_op(false, h, backend);
            }
            self.send_default_content_type = false;
        }
        self.sent = true;
        let (status_line, proto_num) = match &self.status_line {
            Some(s) if s.len() > 12 && s.starts_with(b"HTTP/1.") && s[8] == b' ' => (
                Some(s[9..].to_vec()),
                1000 + i32::from(s[7].wrapping_sub(b'0')),
            ),
            _ => (None, 0),
        };
        backend.set_status(self.response_code, status_line.as_deref(), proto_num);
        let ct = self
            .content_type
            .take()
            .unwrap_or_else(|| default_content_type(facts));
        backend.set_content_type(&ct);
        true
    }
}

/// C `isspace()`.
fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The part of `s` before the first NUL (C string view).
fn c_str(s: &[u8]) -> &[u8] {
    s.split(|&c| c == 0).next().unwrap_or_default()
}

fn clamp_code(code: i64) -> i32 {
    i32::try_from(code).unwrap_or(if code < 0 { i32::MIN } else { i32::MAX })
}

/// `strtol(s, NULL, 10)` with saturation.
pub(crate) fn strtol(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && is_c_space(s[i]) {
        i += 1;
    }
    let neg = match s.get(i) {
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
    let mut v: i64 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        let d = i64::from(d - b'0');
        v = if neg {
            v.saturating_mul(10).saturating_sub(d)
        } else {
            v.saturating_mul(10).saturating_add(d)
        };
        i += 1;
    }
    v
}

/// `atoi()` (saturating to `i32`).
fn atoi(s: &[u8]) -> i32 {
    clamp_code(strtol(s))
}

/// `sapi_extract_response_code()`.
fn extract_response_code(line: &[u8]) -> i32 {
    for (i, &c) in line.iter().enumerate() {
        if c == b' ' && line.get(i + 1).copied().unwrap_or(0) != b' ' {
            return atoi(&line[i + 1..]);
        }
    }
    200
}

/// `sapi_remove_header()` without a prefix length.
fn remove_header(list: &mut Vec<Vec<u8>>, name: &[u8]) {
    let len = name.len();
    list.retain(|h| !(h.len() > len && h[len] == b':' && h[..len].eq_ignore_ascii_case(name)));
}

/// `sapi_apply_default_charset()`: appends `;charset=` to `text/*` types
/// without a charset.  Returns whether it changed the type.
fn apply_default_charset(mimetype: &mut Vec<u8>, charset: &[u8]) -> bool {
    if !charset.is_empty()
        && mimetype.starts_with(b"text/")
        && !mimetype.windows(8).any(|w| w == b"charset=")
    {
        mimetype.extend_from_slice(b";charset=");
        mimetype.extend_from_slice(charset);
        return true;
    }
    false
}

/// `get_default_content_type()`: `default_mimetype` plus
/// `; charset=default_charset` for `text/*` types.
pub fn default_content_type(facts: &HeaderFacts) -> Vec<u8> {
    let mimetype: &[u8] = if facts.default_mimetype.is_empty() {
        b"text/html"
    } else {
        &facts.default_mimetype
    };
    let mut out = mimetype.to_vec();
    if !facts.default_charset.is_empty()
        && mimetype.len() >= 5
        && mimetype[..5].eq_ignore_ascii_case(b"text/")
    {
        out.extend_from_slice(b"; charset=");
        out.extend_from_slice(&facts.default_charset);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;

    fn setup() -> (SapiHeaders, HeaderFacts, MockBackend) {
        (
            SapiHeaders::new(0),
            HeaderFacts::default(),
            MockBackend::default(),
        )
    }

    #[test]
    fn replace_and_add() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"X-A: 1", 0), &f, &b).unwrap();
        h.op(HeaderOp::Add(b"X-A: 2", 0), &f, &b).unwrap();
        assert_eq!(h.headers, vec![b"X-A: 1".to_vec(), b"X-A: 2".to_vec()]);
        h.op(HeaderOp::Replace(b"x-a:3", 0), &f, &b).unwrap();
        assert_eq!(h.headers, vec![b"x-a:3".to_vec()]);
        assert_eq!(
            b.table_entries(Table::HeadersOut),
            vec![(b"x-a".to_vec(), b"3".to_vec())]
        );
    }

    #[test]
    fn trailing_space_is_trimmed_and_newlines_rejected() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"X-A: 1 \r\n", 0), &f, &b).unwrap();
        assert_eq!(h.headers, vec![b"X-A: 1".to_vec()]);
        assert_eq!(
            h.op(HeaderOp::Replace(b"X-A: 1\r\nX-B: 2", 0), &f, &b),
            Err(HeaderError::NewLine)
        );
        assert_eq!(
            h.op(HeaderOp::Replace(b"X-A: \0", 0), &f, &b),
            Err(HeaderError::Nul)
        );
        assert_eq!(h.op(HeaderOp::Replace(b"", 0), &f, &b), Ok(false));
    }

    #[test]
    fn content_type_gets_charset() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"Content-Type: text/plain", 0), &f, &b)
            .unwrap();
        assert_eq!(
            h.headers,
            vec![b"Content-type: text/plain;charset=UTF-8".to_vec()]
        );
        assert_eq!(
            h.content_type.as_deref(),
            Some(&b"text/plain;charset=UTF-8"[..])
        );
        assert!(!h.send_default_content_type);
        h.send(&f, &b);
        assert_eq!(
            b.content_type.borrow().as_deref(),
            Some(&b"text/plain;charset=UTF-8"[..])
        );
        // Not in headers_out: Apache owns Content-Type.
        assert!(b.table_entries(Table::HeadersOut).is_empty());
    }

    #[test]
    fn non_text_content_type_untouched() {
        let (mut h, f, b) = setup();
        h.op(
            HeaderOp::Replace(b"Content-Type: application/json", 0),
            &f,
            &b,
        )
        .unwrap();
        assert_eq!(h.headers, vec![b"Content-Type: application/json".to_vec()]);
    }

    #[test]
    fn default_content_type_on_send() {
        let (mut h, f, b) = setup();
        assert!(h.send(&f, &b));
        assert_eq!(
            h.headers,
            vec![b"Content-type: text/html; charset=UTF-8".to_vec()]
        );
        assert_eq!(*b.status.borrow(), Some((200, None, 0)));
        assert_eq!(
            b.content_type.borrow().as_deref(),
            Some(&b"text/html; charset=UTF-8"[..])
        );
        assert!(!h.send(&f, &b));
        assert_eq!(
            h.op(HeaderOp::Replace(b"X: y", 0), &f, &b),
            Err(HeaderError::AlreadySent)
        );
    }

    #[test]
    fn status_lines() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"HTTP/1.0 404 Not Found", 0), &f, &b)
            .unwrap();
        assert_eq!(h.response_code, 404);
        assert!(h.headers.is_empty());
        h.send(&f, &b);
        assert_eq!(
            *b.status.borrow(),
            Some((404, Some(b"404 Not Found".to_vec()), 1000))
        );
    }

    #[test]
    fn location_redirects() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"Location: /x", 0), &f, &b).unwrap();
        assert_eq!(h.response_code, 302);
        let mut h = SapiHeaders::new(0);
        let post = HeaderFacts {
            method: b"POST".to_vec(),
            ..HeaderFacts::default()
        };
        h.op(HeaderOp::Replace(b"Location: /x", 0), &post, &b)
            .unwrap();
        assert_eq!(h.response_code, 303);
        let mut h = SapiHeaders::new(0);
        h.op(HeaderOp::SetStatus(301), &f, &b).unwrap();
        h.op(HeaderOp::Replace(b"Location: /x", 0), &f, &b).unwrap();
        assert_eq!(h.response_code, 301);
        let mut h = SapiHeaders::new(0);
        h.op(HeaderOp::Replace(b"Location: /x", 307), &f, &b)
            .unwrap();
        assert_eq!(h.response_code, 307);
        let mut h = SapiHeaders::new(0);
        h.op(HeaderOp::Replace(b"WWW-Authenticate: Basic", 0), &f, &b)
            .unwrap();
        assert_eq!(h.response_code, 401);
    }

    #[test]
    fn remove_headers() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Add(b"X-A: 1", 0), &f, &b).unwrap();
        h.op(HeaderOp::Add(b"X-B: 2", 0), &f, &b).unwrap();
        h.op(HeaderOp::Add(b"X-A: 3", 0), &f, &b).unwrap();
        h.op(HeaderOp::Delete(b"x-a"), &f, &b).unwrap();
        assert_eq!(h.headers, vec![b"X-B: 2".to_vec()]);
        assert_eq!(b.table_entries(Table::HeadersOut).len(), 1);
        assert_eq!(
            h.op(HeaderOp::Delete(b"X-B: 2"), &f, &b),
            Err(HeaderError::DeleteColon)
        );
        h.op(HeaderOp::DeleteAll, &f, &b).unwrap();
        assert!(h.headers.is_empty());
        assert!(b.table_entries(Table::HeadersOut).is_empty());
    }

    #[test]
    fn content_length_goes_to_apache() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"Content-Length: 42", 0), &f, &b)
            .unwrap();
        assert_eq!(*b.content_length.borrow(), Some(42));
        assert_eq!(h.headers, vec![b"Content-Length: 42".to_vec()]);
    }

    #[test]
    fn header_without_colon_is_dropped() {
        let (mut h, f, b) = setup();
        h.op(HeaderOp::Replace(b"Garbage", 0), &f, &b).unwrap();
        assert!(h.headers.is_empty());
    }

    #[test]
    fn response_code_extraction() {
        assert_eq!(extract_response_code(b"HTTP/1.1 418 I'm a teapot"), 418);
        assert_eq!(extract_response_code(b"HTTP/1.1"), 200);
        assert_eq!(extract_response_code(b"HTTP/1.1  500"), 500);
    }
}
