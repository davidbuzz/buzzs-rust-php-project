//! A request as the built-in server sees it (`php_cli_server_request`),
//! the parser callbacks that fill it (`php_cli_server_client_read_request_on_*`)
//! and the URL → file mapping (`normalize_vpath`,
//! `php_cli_server_request_translate_vpath`).

use crate::parser::{Handler, Info, Method};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

/// `S_IFDIR`.
const S_IFDIR: u32 = 0o040_000;
/// `S_IFREG`.
const S_IFREG: u32 = 0o100_000;

/// Inserts or replaces `key` keeping the original position
/// (`zend_hash_update`).
pub fn hash_update(list: &mut Vec<(Vec<u8>, Vec<u8>)>, key: &[u8], value: Vec<u8>) {
    if let Some(e) = list.iter_mut().find(|(k, _)| k == key) {
        e.1 = value;
    } else {
        list.push((key.to_vec(), value));
    }
}

/// `php_cli_server_request`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The method.
    pub method: Method,
    /// `major * 100 + minor`.
    pub protocol_version: i32,
    /// The raw request target.
    pub request_uri: Option<Vec<u8>>,
    /// The decoded, normalised path (after translation: the script part).
    pub vpath: Option<Vec<u8>>,
    /// The file the request maps to.
    pub path_translated: Option<Vec<u8>>,
    /// The part of the path after the script.
    pub path_info: Option<Vec<u8>>,
    /// The query string (without `?`).
    pub query_string: Option<Vec<u8>>,
    /// Headers keyed by lower-cased name.
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    /// Headers keyed by their original name (`apache_request_headers()`).
    pub headers_original_case: Vec<(Vec<u8>, Vec<u8>)>,
    /// The request body.
    pub content: Option<Vec<u8>>,
    /// The extension of `vpath` (after the last `.`).
    pub ext: Vec<u8>,
    /// Size of the translated file.
    pub file_size: u64,
}

impl Request {
    /// A header by lower-case name.
    pub fn header(&self, lc_name: &[u8]) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(k, _)| k == lc_name)
            .map(|(_, v)| v.as_slice())
    }

    /// The request URI (empty when unset).
    pub fn uri(&self) -> &[u8] {
        self.request_uri.as_deref().unwrap_or_default()
    }

    /// Whether the request targets a PHP script (`.php` extension and a
    /// translated path), i.e. is not served as a static file.
    pub fn is_php_script(&self) -> bool {
        self.ext.eq_ignore_ascii_case(b"php") && self.path_translated.is_some()
    }

    /// Computes `ext` from `vpath` (`on_message_complete`).
    fn compute_ext(&mut self) {
        self.ext.clear();
        if let Some(v) = &self.vpath {
            if let Some(dot) = v.iter().rposition(|&c| c == b'.') {
                self.ext = v[dot + 1..].to_vec();
            }
        }
    }

    /// `php_cli_server_request_translate_vpath()`.
    pub fn translate_vpath(&mut self, document_root: &[u8]) {
        let vpath_in = self.vpath.clone().unwrap_or_default();
        let mut buf = document_root.to_vec();
        let vpath_start = buf.len();
        if !vpath_in.is_empty() {
            if vpath_in[0] != b'/' {
                buf.push(b'/');
            }
            buf.extend_from_slice(&vpath_in);
        }
        let original = buf;
        let p = original.len();
        let mut q = p;
        let mut prev_path: Option<Vec<u8>> = None;
        let mut result: Option<(Vec<u8>, u64)> = None;
        while q > 0 {
            let cur = &original[..q];
            if let Some(md) = stat(cur) {
                if md.mode() & S_IFDIR != 0 {
                    let mut base = cur.to_vec();
                    if base.last() != Some(&b'/') {
                        base.push(b'/');
                    }
                    for file in ["index.php", "index.html"] {
                        let mut cand = base.clone();
                        cand.extend_from_slice(file.as_bytes());
                        if let Some(m) = stat(&cand).filter(|m| m.mode() & S_IFREG != 0) {
                            result = Some((cand, m.size()));
                            break;
                        }
                    }
                    if result.is_none() {
                        return;
                    }
                } else {
                    result = Some((cur.to_vec(), md.size()));
                }
                break;
            }
            // Walk up to the previous slash; what was cut becomes PATH_INFO.
            while q > 0 {
                q -= 1;
                if original[q] == b'/' {
                    break;
                }
            }
            prev_path = Some(original[q..p].to_vec());
        }
        let Some((buf, size)) = result else {
            return;
        };
        if buf.len() < vpath_start {
            return;
        }
        self.path_info = prev_path;
        self.vpath = Some(buf[vpath_start..].to_vec());
        self.path_translated = Some(buf);
        self.file_size = size;
    }
}

/// `php_sys_stat()` of a C string (the path ends at the first NUL).
fn stat(path: &[u8]) -> Option<std::fs::Metadata> {
    let path = path.split(|&c| c == 0).next().unwrap_or_default();
    if path.is_empty() {
        return None;
    }
    std::fs::metadata(std::ffi::OsStr::from_bytes(path)).ok()
}

/// `php_raw_url_decode()`: `%XX` sequences only.
pub fn raw_url_decode(s: &[u8]) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(a), Some(b)) = (hex(s[i + 1]), hex(s[i + 2])) {
                out.push(u8::try_from(a * 16 + b).unwrap_or(0));
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `normalize_vpath()`: URL-decodes the path, collapses repeated slashes
/// and resolves `.` and `..` segments (never above the root).
pub fn normalize_vpath(vpath: &[u8]) -> Vec<u8> {
    let mut d = raw_url_decode(vpath);
    let mut end = d.len();
    let mut p = 0usize;

    if p < end && d[p] == b'/' {
        let mut n = p;
        while n < end && d[n] == b'/' {
            n += 1;
        }
        p += 1;
        d.copy_within(n..end, p);
        end -= n - p;
    }

    while p < end {
        let mut n = p;
        while n < end && d[n] != b'/' {
            n += 1;
        }
        if n - p == 2 && d[p] == b'.' && d[p + 1] == b'.' {
            if p > 0 {
                p -= 1;
                loop {
                    if p == 0 {
                        if d[p] == b'/' {
                            p += 1;
                        }
                        break;
                    }
                    p -= 1;
                    if d[p] == b'/' {
                        p += 1;
                        break;
                    }
                }
            }
            while n < end && d[n] == b'/' {
                n += 1;
            }
            d.copy_within(n..end, p);
            end -= n - p;
        } else if n - p == 1 && d[p] == b'.' {
            while n < end && d[n] == b'/' {
                n += 1;
            }
            d.copy_within(n..end, p);
            end -= n - p;
        } else if n < end {
            let mut nn = n;
            while nn < end && d[nn] == b'/' {
                nn += 1;
            }
            p = n + 1;
            d.copy_within(nn..end, p);
            end -= nn - p;
        } else {
            p = n;
        }
    }
    d.truncate(end);
    d
}

/// Which element the parser reported last (`last_header_element`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HeaderElement {
    #[default]
    None,
    Field,
    Value,
}

/// The per-connection read state: the request being filled plus the
/// flags of `php_cli_server_client`.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct RequestReader {
    /// The request.
    pub request: Request,
    /// The complete request has been read.
    pub request_read: bool,
    /// The body exceeds `post_max_size` (answered with 413).
    pub too_large_post: bool,
    /// `Expect: 100-continue` was received.
    pub expect_continue: bool,
    current_header_name: Option<Vec<u8>>,
    current_header_value: Option<Vec<u8>>,
    last: HeaderElement,
    /// `post_max_size` in bytes (0 = unlimited).
    pub post_max_size: i64,
    /// The document root used for path translation.
    pub document_root: Vec<u8>,
}

impl RequestReader {
    /// A reader for a server with the given settings.
    pub fn new(document_root: &[u8], post_max_size: i64) -> Self {
        RequestReader {
            document_root: document_root.to_vec(),
            post_max_size,
            ..RequestReader::default()
        }
    }

    /// `php_cli_server_client_save_header()`.
    fn save_header(&mut self) {
        let name = self.current_header_name.take().unwrap_or_default();
        let value = self.current_header_value.take().unwrap_or_default();
        let lc = name.to_ascii_lowercase();
        let with_comma = lc != b"set-cookie";
        let existing = self.request.header(&lc).map(<[u8]>::to_vec);
        let v = match existing {
            Some(cur) if with_comma => {
                let mut nv = cur;
                nv.extend_from_slice(b", ");
                nv.extend_from_slice(&value);
                nv
            }
            _ => value,
        };
        hash_update(&mut self.request.headers, &lc, v.clone());
        hash_update(&mut self.request.headers_original_case, &name, v);
    }
}

impl Handler for RequestReader {
    fn on_path(&mut self, data: &[u8]) -> i32 {
        if self.request.vpath.is_some() {
            return 1;
        }
        self.request.vpath = Some(normalize_vpath(data));
        0
    }

    fn on_query_string(&mut self, data: &[u8]) -> i32 {
        self.request
            .query_string
            .get_or_insert_with(Vec::new)
            .extend_from_slice(data);
        0
    }

    fn on_url(&mut self, info: &Info, data: &[u8]) -> i32 {
        if self.request.request_uri.is_none() {
            self.request.method = info.method;
        }
        self.request
            .request_uri
            .get_or_insert_with(Vec::new)
            .extend_from_slice(data);
        0
    }

    fn on_header_field(&mut self, data: &[u8]) -> i32 {
        match self.last {
            HeaderElement::Value => {
                self.save_header();
                self.current_header_name = Some(data.to_vec());
            }
            HeaderElement::None => self.current_header_name = Some(data.to_vec()),
            HeaderElement::Field => self
                .current_header_name
                .get_or_insert_with(Vec::new)
                .extend_from_slice(data),
        }
        self.last = HeaderElement::Field;
        0
    }

    fn on_header_value(&mut self, data: &[u8]) -> i32 {
        match self.last {
            HeaderElement::Field | HeaderElement::None => {
                self.current_header_value = Some(data.to_vec());
            }
            HeaderElement::Value => self
                .current_header_value
                .get_or_insert_with(Vec::new)
                .extend_from_slice(data),
        }
        self.last = HeaderElement::Value;
        0
    }

    fn on_headers_complete(&mut self, info: &Info) -> i32 {
        match self.last {
            HeaderElement::None => {}
            HeaderElement::Field => {
                self.current_header_value = Some(Vec::new());
                self.save_header();
            }
            HeaderElement::Value => self.save_header(),
        }
        self.last = HeaderElement::None;
        if info.content_length > 0
            && self.post_max_size > 0
            && info.content_length > self.post_max_size
        {
            self.request.protocol_version =
                i32::from(info.http_major) * 100 + i32::from(info.http_minor);
            self.too_large_post = true;
            self.request_read = true;
            return 2;
        }
        if info.content_length > 0 && self.request.content.is_none() {
            // Bounded by post_max_size above (or unlimited by configuration).
            let want = usize::try_from(info.content_length).unwrap_or(0);
            let mut v = Vec::new();
            if v.try_reserve_exact(want).is_ok() {
                self.request.content = Some(v);
            }
        }
        if self
            .request
            .header(b"expect")
            .is_some_and(|v| v.eq_ignore_ascii_case(b"100-continue"))
            && info.http_major == 1
            && info.http_minor == 1
        {
            self.expect_continue = true;
        }
        0
    }

    fn on_body(&mut self, info: &Info, data: &[u8]) -> i32 {
        let cur = self.request.content.as_ref().map_or(0, Vec::len);
        if self.post_max_size > 0
            && i64::try_from(cur + data.len()).unwrap_or(i64::MAX) > self.post_max_size
        {
            self.request.protocol_version =
                i32::from(info.http_major) * 100 + i32::from(info.http_minor);
            self.too_large_post = true;
            self.request_read = true;
            return 1;
        }
        self.request
            .content
            .get_or_insert_with(Vec::new)
            .extend_from_slice(data);
        0
    }

    fn on_message_complete(&mut self, info: &Info) -> i32 {
        self.request.protocol_version =
            i32::from(info.http_major) * 100 + i32::from(info.http_minor);
        let root = self.document_root.clone();
        self.request.translate_vpath(&root);
        self.request.compute_ext();
        self.request_read = true;
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn normalize() {
        assert_eq!(normalize_vpath(b"/"), b"/");
        assert_eq!(normalize_vpath(b"//a//b/"), b"/a/b/");
        assert_eq!(normalize_vpath(b"/a/./b"), b"/a/b");
        assert_eq!(normalize_vpath(b"/a/../b"), b"/b");
        assert_eq!(normalize_vpath(b"/../../x"), b"/x");
        assert_eq!(normalize_vpath(b"/a/b/.."), b"/a/");
        assert_eq!(normalize_vpath(b"/%2e%2e/x%20y"), b"/x y");
        assert_eq!(normalize_vpath(b"/a+b"), b"/a+b");
        assert_eq!(normalize_vpath(b"*"), b"*");
    }

    #[test]
    fn url_decode() {
        assert_eq!(raw_url_decode(b"a%41%4"), b"aA%4");
        assert_eq!(raw_url_decode(b"%zz%00"), b"%zz\0");
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "php-cli-server-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(d.join("noindex")).unwrap();
        std::fs::write(d.join("index.php"), "<?php").unwrap();
        std::fs::write(d.join("sub/index.html"), "x").unwrap();
        std::fs::write(d.join("a.txt"), "hello").unwrap();
        std::fs::write(d.join("s.php"), "<?php").unwrap();
        d
    }

    fn translate(root: &std::path::Path, vpath: &[u8]) -> Request {
        let mut r = Request {
            vpath: Some(vpath.to_vec()),
            ..Request::default()
        };
        r.translate_vpath(root.as_os_str().as_bytes());
        r.compute_ext();
        r
    }

    #[test]
    fn translation() {
        let d = tmpdir("translate");
        let root = d.as_os_str().as_bytes().to_vec();
        let mut abs = |s: &str| {
            let mut v = root.clone();
            v.extend_from_slice(s.as_bytes());
            v
        };
        let r = translate(&d, b"/a.txt");
        assert_eq!(r.path_translated, Some(abs("/a.txt")));
        assert_eq!(r.vpath.as_deref(), Some(&b"/a.txt"[..]));
        assert_eq!(r.path_info, None);
        assert_eq!(r.file_size, 5);
        assert_eq!(r.ext, b"txt");

        let r = translate(&d, b"/");
        assert_eq!(r.path_translated, Some(abs("/index.php")));
        assert_eq!(r.vpath.as_deref(), Some(&b"/index.php"[..]));
        assert!(r.is_php_script());

        let r = translate(&d, b"/s.php/x/y");
        assert_eq!(r.path_translated, Some(abs("/s.php")));
        assert_eq!(r.vpath.as_deref(), Some(&b"/s.php"[..]));
        assert_eq!(r.path_info.as_deref(), Some(&b"/x/y"[..]));

        let r = translate(&d, b"/sub");
        assert_eq!(r.path_translated, Some(abs("/sub/index.html")));
        assert_eq!(r.vpath.as_deref(), Some(&b"/sub/index.html"[..]));

        // Missing file: falls back to the index of the nearest directory.
        let r = translate(&d, b"/foo/bar");
        assert_eq!(r.path_translated, Some(abs("/index.php")));
        assert_eq!(r.path_info.as_deref(), Some(&b"/foo/bar"[..]));

        let r = translate(&d, b"/sub/deeper/x");
        assert_eq!(r.path_translated, None, "no index in sub/deeper");

        let r = translate(&d, b"/noindex/");
        assert_eq!(r.path_translated, None);
        assert_eq!(r.vpath.as_deref(), Some(&b"/noindex/"[..]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reader_collects_request() {
        let d = tmpdir("reader");
        let mut rr = RequestReader::new(d.as_os_str().as_bytes(), 0);
        let mut p = Parser::new();
        let req = b"POST /a.txt?x=1 HTTP/1.1\r\nHost: h\r\nX-A: 1\r\nx-a: 2\r\nSet-Cookie: a\r\nSet-Cookie: b\r\nEmpty:\r\nContent-Length: 3\r\n\r\nabc";
        assert_eq!(p.execute(&mut rr, req), req.len());
        assert!(rr.request_read);
        let r = &rr.request;
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.uri(), b"/a.txt?x=1");
        assert_eq!(r.query_string.as_deref(), Some(&b"x=1"[..]));
        assert_eq!(r.header(b"x-a"), Some(&b"1, 2"[..]));
        assert_eq!(r.header(b"set-cookie"), Some(&b"b"[..]));
        assert_eq!(r.header(b"empty"), Some(&b""[..]));
        assert_eq!(r.content.as_deref(), Some(&b"abc"[..]));
        assert_eq!(r.protocol_version, 101);
        assert_eq!(
            r.headers_original_case
                .iter()
                .map(|(k, _)| k.as_slice())
                .collect::<Vec<_>>(),
            vec![&b"Host"[..], b"X-A", b"x-a", b"Set-Cookie", b"Empty", b"Content-Length"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn too_large_post() {
        let mut rr = RequestReader::new(b"/nonexistent", 2);
        let mut p = Parser::new();
        let req = b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc";
        assert!(p.execute(&mut rr, req) < req.len());
        assert!(rr.too_large_post && rr.request_read);
    }

    #[test]
    fn expect_continue() {
        let mut rr = RequestReader::new(b"/nonexistent", 0);
        let mut p = Parser::new();
        let req = b"POST / HTTP/1.1\r\nExpect: 100-Continue\r\nContent-Length: 3\r\n\r\n";
        assert_eq!(p.execute(&mut rr, req), req.len());
        assert!(rr.expect_continue && !rr.request_read);
    }
}
