//! Response headers: `sapi_header_op()` / `sapi_send_headers()` from
//! `main/SAPI.c` with the built-in server's `send_headers` handler
//! (`sapi_cli_server_send_headers`).  The server has no `header_handler`,
//! so every header ends up in the SAPI list.

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

/// `SG(sapi_headers)`.
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
}

impl Default for SapiHeaders {
    fn default() -> Self {
        Self::new()
    }
}

impl SapiHeaders {
    /// Fresh state with status 200.
    pub fn new() -> Self {
        SapiHeaders {
            headers: Vec::new(),
            response_code: 200,
            status_line: None,
            mimetype: None,
            send_default_content_type: true,
            sent: false,
        }
    }

    /// `sapi_update_response_code()`.
    pub fn update_response_code(&mut self, code: i32) {
        if self.response_code == code {
            return;
        }
        self.status_line = None;
        self.response_code = code;
    }

    /// `sapi_header_op()`.  `Ok(false)` is a silent failure (empty line).
    pub fn op(&mut self, op: HeaderOp<'_>, facts: &HeaderFacts) -> Result<bool, HeaderError> {
        if self.sent {
            return Err(HeaderError::AlreadySent);
        }
        let (line, replace, response_code) = match op {
            HeaderOp::SetStatus(code) => {
                self.update_response_code(code);
                return Ok(true);
            }
            HeaderOp::DeleteAll => {
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
            if c_str(&line).contains(&b':') {
                return Err(HeaderError::DeleteColon);
            }
            remove_header(&mut self.headers, c_str(&line));
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
        self.add_op(replace, header);
        Ok(true)
    }

    /// `sapi_header_add_op()` without a header handler.
    pub fn add_op(&mut self, replace: bool, header: Vec<u8>) {
        if replace {
            if let Some(colon) = header.iter().position(|&c| c == b':') {
                remove_header(&mut self.headers, &header[..colon]);
            }
        }
        self.headers.push(header);
    }

    /// The first part of `sapi_send_headers()`: adds the default
    /// Content-Type and marks the headers sent.  Returns `false` when they
    /// had already been sent.
    pub fn finalize(&mut self, facts: &HeaderFacts) -> bool {
        if self.sent {
            return false;
        }
        if self.send_default_content_type {
            let default = default_content_type(facts);
            if !default.is_empty() {
                self.mimetype = Some(default.clone());
                let mut h = b"Content-type: ".to_vec();
                h.extend_from_slice(&default);
                self.add_op(false, h);
            }
            self.send_default_content_type = false;
        }
        self.sent = true;
        true
    }

    /// `sapi_cli_server_send_headers()`: the response head.
    pub fn render(&self, protocol_version: i32, now: i64) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(line) = &self.status_line {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        } else {
            out.extend_from_slice(&crate::status::status_line(
                protocol_version,
                self.response_code,
            ));
        }
        let has_date = self
            .headers
            .iter()
            .any(|h| h.len() > 5 && h[..5].eq_ignore_ascii_case(b"Date:"));
        crate::status::essential_headers(&mut out, !has_date, now);
        for h in &self.headers {
            if !h.is_empty() {
                out.extend_from_slice(h);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"\r\n");
        out
    }
}

impl SapiHeaders {
    /// `sapi_cgi_send_headers()`: a `Status:` line unless the status is
    /// 200 (or `cgi.nph`), the header lines, then the blank line.
    pub fn render_cgi(&self, rfc2616_headers: bool, nph: bool) -> Vec<u8> {
        const MAX_HEADER_LENGTH: usize = 1024;
        let is_status = |h: &[u8]| h.len() > 7 && h[..7].eq_ignore_ascii_case(b"Status:");
        let mut out = Vec::new();
        let mut ignore_status = false;
        let mut response_status = self.response_code;
        if nph || self.response_code != 200 {
            let mut has_status = false;
            let mut buf: Vec<u8> = Vec::new();
            let space = self
                .status_line
                .as_ref()
                .and_then(|l| l.iter().position(|&c| c == b' '));
            if let (true, Some(line)) = (rfc2616_headers, &self.status_line) {
                buf.clone_from(line);
                buf.truncate(MAX_HEADER_LENGTH);
                if let Some(sp) = space {
                    response_status = clamp_code(atoi(&line[sp + 1..]));
                }
            } else if let (Some(line), Some(sp)) = (&self.status_line, space) {
                if sp >= 5 && line[..5].eq_ignore_ascii_case(b"HTTP/") {
                    buf.extend_from_slice(b"Status:");
                    buf.extend_from_slice(&line[sp..]);
                    buf.truncate(MAX_HEADER_LENGTH - 1);
                    response_status = clamp_code(atoi(&line[sp + 1..]));
                } else {
                    has_status = self.default_status(&mut buf, is_status);
                }
            } else {
                has_status = self.default_status(&mut buf, is_status);
            }
            if !has_status {
                out.extend_from_slice(&buf);
                out.extend_from_slice(b"\r\n");
                ignore_status = true;
            }
        }
        for h in &self.headers {
            if h.is_empty() {
                continue;
            }
            if is_status(h.as_slice()) {
                if !ignore_status {
                    ignore_status = true;
                    out.extend_from_slice(h);
                    out.extend_from_slice(b"\r\n");
                }
            } else if response_status == 304
                && h.len() > 13
                && h[..13].eq_ignore_ascii_case(b"Content-Type:")
            {
                continue;
            } else {
                out.extend_from_slice(h);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"\r\n");
        out
    }

    /// The `Status: <code> <reason>` line, unless a `Status:` header was
    /// set (then `true`).
    fn default_status(&self, buf: &mut Vec<u8>, is_status: impl Fn(&[u8]) -> bool) -> bool {
        if self.headers.iter().any(|h| is_status(h.as_slice())) {
            return true;
        }
        let code = self.response_code;
        *buf = match crate::status::status_reason(code) {
            Some(r) => format!("Status: {code} {r}").into_bytes(),
            None => format!("Status: {code}").into_bytes(),
        };
        false
    }
}

/// C `atoi()`.
fn atoi(s: &[u8]) -> i64 {
    strtol(s)
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

/// `sapi_extract_response_code()`.
fn extract_response_code(line: &[u8]) -> i32 {
    for (i, &c) in line.iter().enumerate() {
        if c == b' ' && line.get(i + 1).copied().unwrap_or(0) != b' ' {
            return clamp_code(strtol(&line[i + 1..]));
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

    fn op(h: &mut SapiHeaders, o: HeaderOp<'_>) -> Result<bool, HeaderError> {
        h.op(o, &HeaderFacts::default())
    }

    /// `sapi_cgi_send_headers()` output, as `php-cgi` 8.5 prints it.
    #[test]
    fn cgi_head() {
        let facts = HeaderFacts::default();
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"X-Powered-By: PHP/8.5.0", 0)).unwrap();
        assert!(h.finalize(&facts));
        assert_eq!(
            h.render_cgi(false, false),
            b"X-Powered-By: PHP/8.5.0\r\nContent-type: text/html; charset=UTF-8\r\n\r\n"
        );

        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"HTTP/1.1 404 Not Found", 0)).unwrap();
        op(&mut h, HeaderOp::Replace(b"Location: /x", 0)).unwrap();
        let out = h.render_cgi(false, false);
        // `Location:` turns a non-3xx, non-201 status into 302, as
        // php-cgi 8.5 prints for this pair.
        assert!(out.starts_with(b"Status: 302 Found\r\n"), "{out:?}");

        let mut h = SapiHeaders::new();
        h.update_response_code(299);
        assert_eq!(h.render_cgi(false, false), b"Status: 299\r\n\r\n");

        // An explicit Status header replaces the generated one.
        let mut h = SapiHeaders::new();
        h.update_response_code(500);
        h.add_op(false, b"Status: 501 Nope".to_vec());
        assert_eq!(h.render_cgi(false, false), b"Status: 501 Nope\r\n\r\n");

        // 304 drops Content-Type.
        let mut h = SapiHeaders::new();
        h.update_response_code(304);
        h.add_op(false, b"Content-Type: text/plain".to_vec());
        assert_eq!(
            h.render_cgi(false, false),
            b"Status: 304 Not Modified\r\n\r\n"
        );
    }

    #[test]
    fn replace_and_add() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"X-A: 1", 0)).unwrap();
        op(&mut h, HeaderOp::Add(b"X-A: 2", 0)).unwrap();
        assert_eq!(h.headers, vec![b"X-A: 1".to_vec(), b"X-A: 2".to_vec()]);
        op(&mut h, HeaderOp::Replace(b"x-a:3", 0)).unwrap();
        assert_eq!(h.headers, vec![b"x-a:3".to_vec()]);
    }

    #[test]
    fn header_without_colon_is_kept() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"Garbage", 0)).unwrap();
        assert_eq!(h.headers, vec![b"Garbage".to_vec()]);
    }

    #[test]
    fn validation() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"X-A: 1 \r\n", 0)).unwrap();
        assert_eq!(h.headers, vec![b"X-A: 1".to_vec()]);
        assert_eq!(
            op(&mut h, HeaderOp::Replace(b"X-A: 1\r\nX-B: 2", 0)),
            Err(HeaderError::NewLine)
        );
        assert_eq!(
            op(&mut h, HeaderOp::Replace(b"X: \0", 0)),
            Err(HeaderError::Nul)
        );
        assert_eq!(op(&mut h, HeaderOp::Replace(b"", 0)), Ok(false));
        assert_eq!(
            op(&mut h, HeaderOp::Delete(b"X: y")),
            Err(HeaderError::DeleteColon)
        );
    }

    #[test]
    fn content_type_and_default() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"Content-Type: text/plain", 0)).unwrap();
        assert_eq!(
            h.headers,
            vec![b"Content-type: text/plain;charset=UTF-8".to_vec()]
        );
        assert!(h.finalize(&HeaderFacts::default()));
        assert_eq!(h.headers.len(), 1);
        let mut h = SapiHeaders::new();
        h.finalize(&HeaderFacts::default());
        assert_eq!(
            h.headers,
            vec![b"Content-type: text/html; charset=UTF-8".to_vec()]
        );
        assert!(!h.finalize(&HeaderFacts::default()));
        assert_eq!(
            op(&mut h, HeaderOp::Replace(b"X: y", 0)),
            Err(HeaderError::AlreadySent)
        );
    }

    #[test]
    fn status_and_location() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"HTTP/1.0 404 Not Found", 0)).unwrap();
        assert_eq!(h.response_code, 404);
        assert!(h.headers.is_empty());
        let head = h.render(101, 0);
        assert!(head.starts_with(b"HTTP/1.0 404 Not Found\r\nDate: "));
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Replace(b"Location: /x", 0)).unwrap();
        assert_eq!(h.response_code, 302);
        let mut h = SapiHeaders::new();
        let post = HeaderFacts {
            method: b"POST".to_vec(),
            ..HeaderFacts::default()
        };
        h.op(HeaderOp::Replace(b"Location: /x", 0), &post).unwrap();
        assert_eq!(h.response_code, 303);
    }

    #[test]
    fn render_head() {
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Add(b"X-A: 1", 0)).unwrap();
        h.finalize(&HeaderFacts::default());
        let head = String::from_utf8(h.render(101, 0)).unwrap();
        assert_eq!(
            head,
            "HTTP/1.1 200 OK\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nConnection: close\r\nX-A: 1\r\nContent-type: text/html; charset=UTF-8\r\n\r\n"
        );
        let mut h = SapiHeaders::new();
        op(&mut h, HeaderOp::Add(b"Date: today", 0)).unwrap();
        let head = String::from_utf8(h.render(100, 0)).unwrap();
        assert_eq!(
            head,
            "HTTP/1.0 200 OK\r\nConnection: close\r\nDate: today\r\n\r\n"
        );
    }

    #[test]
    fn response_code_extraction() {
        assert_eq!(extract_response_code(b"HTTP/1.1 418 I'm a teapot"), 418);
        assert_eq!(extract_response_code(b"HTTP/1.1"), 200);
    }
}
