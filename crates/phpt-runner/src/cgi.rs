// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! CGI test support: the request a test sends to `php-cgi` (`--GET--`, `--POST--`,
//! `--POST_RAW--`, `--PUT--`, `--GZIP_POST--`, `--DEFLATE_POST--`, `--COOKIE--`) and the
//! response headers checked against `--EXPECTHEADERS--` (ref: `run-tests.php` `run_test()`).

use std::collections::HashMap;
use std::ffi::OsString;

use crate::bytes::{php_empty, trim};
use crate::phpt::TestFile;
use crate::process::Env;

/// A test whose request section would send an empty body (run-tests.php borks these with
/// the `JUnit` message `empty $request`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyRequest;

fn get<'a>(env: &'a Env, key: &str) -> &'a [u8] {
    use std::os::unix::ffi::OsStrExt;
    env.get(&OsString::from(key))
        .map_or(&[][..], |v| v.as_os_str().as_bytes())
}

/// `empty($env[$key])`.
pub fn env_empty(env: &Env, key: &str) -> bool {
    php_empty(get(env, key))
}

fn set(env: &mut Env, key: &str, value: impl Into<OsString>) {
    env.insert(OsString::from(key), value.into());
}

fn os_bytes(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b.to_vec())
}

/// The request variables every test gets just before it runs: `REDIRECT_STATUS`,
/// `QUERY_STRING` (from `--GET--`), `PATH_TRANSLATED`, `SCRIPT_FILENAME` and `HTTP_COOKIE`
/// (from `--COOKIE--`). Values the test's `--ENV--` already set are kept where run-tests.php
/// keeps them.
pub fn set_request_vars(env: &mut Env, test: &TestFile, test_file: &OsString) {
    let query = test
        .section("GET")
        .map(|s| trim(s).to_vec())
        .unwrap_or_default();
    set(env, "REDIRECT_STATUS", "1");
    if env_empty(env, "QUERY_STRING") {
        set(env, "QUERY_STRING", os_bytes(&query));
    }
    for k in ["PATH_TRANSLATED", "SCRIPT_FILENAME"] {
        if env_empty(env, k) {
            set(env, k, test_file.clone());
        }
    }
    let cookie = test
        .section("COOKIE")
        .map(|s| trim(s).to_vec())
        .unwrap_or_default();
    set(env, "HTTP_COOKIE", os_bytes(&cookie));
}

/// The body of `--POST_RAW--` / `--PUT--`: the trimmed section without the first
/// `Content-Type:` line, which becomes `CONTENT_TYPE` (unless that is already set).
fn raw_body(section: &[u8], env: &mut Env) -> Vec<u8> {
    let mut request = Vec::new();
    let mut started = false;
    for line in trim(section).split(|&b| b == b'\n') {
        if env_empty(env, "CONTENT_TYPE")
            && line.len() >= 13
            && line[..13].eq_ignore_ascii_case(b"Content-Type:")
        {
            let value: Vec<u8> = line[13..].iter().copied().filter(|&b| b != b'\r').collect();
            set(env, "CONTENT_TYPE", os_bytes(trim(&value)));
            continue;
        }
        if started {
            request.push(b'\n');
        }
        started = true;
        request.extend_from_slice(line);
    }
    request
}

/// Build the request body and set `REQUEST_METHOD`, `CONTENT_TYPE`, `CONTENT_LENGTH` and
/// `HTTP_CONTENT_ENCODING` like run-tests.php. `Ok(None)` for a plain GET.
pub fn build_request(test: &TestFile, env: &mut Env) -> Result<Option<Vec<u8>>, EmptyRequest> {
    if test.section_not_empty("POST_RAW") || test.section_not_empty("PUT") {
        let put = !test.section_not_empty("POST_RAW");
        let section = test.section_or_empty(if put { "PUT" } else { "POST_RAW" });
        let request = raw_body(section, env);
        set(env, "CONTENT_LENGTH", request.len().to_string());
        if put {
            set(env, "REQUEST_METHOD", "PUT");
        } else if env_empty(env, "REQUEST_METHOD") {
            set(env, "REQUEST_METHOD", "POST");
        }
        if php_empty(&request) {
            return Err(EmptyRequest);
        }
        return Ok(Some(request));
    }
    if test.section_not_empty("POST") {
        let request = trim(test.section_or_empty("POST")).to_vec();
        set(env, "REQUEST_METHOD", "POST");
        if env_empty(env, "CONTENT_TYPE") {
            set(env, "CONTENT_TYPE", "application/x-www-form-urlencoded");
        }
        if env_empty(env, "CONTENT_LENGTH") {
            set(env, "CONTENT_LENGTH", request.len().to_string());
        }
        return Ok(Some(request));
    }
    for (section, encoding) in [("GZIP_POST", "gzip"), ("DEFLATE_POST", "deflate")] {
        if test.section_not_empty(section) {
            let body = trim(test.section_or_empty(section));
            let request = if encoding == "gzip" {
                gzencode(body)
            } else {
                gzcompress(body)
            };
            set(env, "HTTP_CONTENT_ENCODING", encoding);
            set(env, "REQUEST_METHOD", "POST");
            set(env, "CONTENT_TYPE", "application/x-www-form-urlencoded");
            set(env, "CONTENT_LENGTH", request.len().to_string());
            return Ok(Some(request));
        }
    }
    set(env, "REQUEST_METHOD", "GET");
    set(env, "CONTENT_TYPE", "");
    set(env, "CONTENT_LENGTH", "");
    Ok(None)
}

/// A raw DEFLATE stream made of stored blocks. run-tests.php compresses at level 9; the server
/// only ever sees the inflated bytes, so an uncompressed stream is equivalent for every test
/// (only the `CONTENT_LENGTH` differs, which no test prints).
fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 5 * (data.len() / 0xFFFF + 1));
    let mut chunks = data.chunks(0xFFFF).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// `gzencode($data, 9, FORCE_GZIP)` (gzip container; stored blocks, see [`deflate_stored`]).
pub fn gzencode(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 2, 3];
    out.extend(deflate_stored(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    // ISIZE is the input length modulo 2^32.
    let size = u32::try_from(data.len() & 0xFFFF_FFFF).unwrap_or(u32::MAX);
    out.extend_from_slice(&size.to_le_bytes());
    out
}

/// `gzcompress($data, 9)` (zlib container; stored blocks, see [`deflate_stored`]).
pub fn gzcompress(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0xda];
    out.extend(deflate_stored(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Split CGI output into header block and body with run-tests.php's
/// `/^(.*?)\r?\n\r?\n(.*)/s`; `None` if there is no blank line.
pub fn split_response(out: &[u8]) -> Option<(&[u8], &[u8])> {
    let newline_at = |j: usize| -> Option<usize> {
        match out.get(j) {
            Some(b'\r') if out.get(j + 1) == Some(&b'\n') => Some(j + 2),
            Some(b'\n') => Some(j + 1),
            _ => None,
        }
    };
    (0..out.len()).find_map(|i| {
        let end = newline_at(newline_at(i)?)?;
        Some((&out[..i], &out[end..]))
    })
}

/// Response headers by trimmed name (a repeated name keeps its last value).
pub type Headers = HashMap<Vec<u8>, Vec<u8>>;

/// `name: value` lines of a header block split on runs of CR/LF; lines without `:` are
/// ignored. Both sides are trimmed.
fn header_lines(block: &[u8]) -> impl Iterator<Item = (Vec<u8>, Vec<u8>)> + '_ {
    block
        .split(|&b| b == b'\n' || b == b'\r')
        .filter_map(|line| {
            let colon = line.iter().position(|&b| b == b':')?;
            Some((
                trim(&line[..colon]).to_vec(),
                trim(&line[colon + 1..]).to_vec(),
            ))
        })
}

/// Parse the header block of a CGI response.
pub fn parse_headers(block: &[u8]) -> Headers {
    header_lines(block).collect()
}

/// Result of comparing response headers with `--EXPECTHEADERS--`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderCheck {
    /// A wanted header is missing or has another value.
    pub failed: bool,
    /// `$wanted_headers`: every `name: value` line of the section, joined with `\n`.
    pub wanted: Vec<u8>,
    /// `$output_headers`: the actual value of each wanted header that was sent.
    pub output: Vec<u8>,
}

/// Compare `headers` with an `--EXPECTHEADERS--` section.
pub fn check_headers(section: &[u8], headers: &Headers) -> HeaderCheck {
    // A PHP array: a repeated name keeps its first position and its last value.
    let mut want: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut wanted_lines = Vec::new();
    for (k, v) in header_lines(section) {
        wanted_lines.push([&k[..], &b": "[..], &v[..]].concat());
        match want.iter_mut().find(|(name, _)| *name == k) {
            Some(entry) => entry.1 = v,
            None => want.push((k, v)),
        }
    }
    let mut failed = false;
    let mut output_lines = Vec::new();
    for (k, v) in &want {
        match headers.get(k) {
            Some(actual) => {
                output_lines.push([&k[..], &b": "[..], &actual[..]].concat());
                if !loose_eq(actual, v) {
                    failed = true;
                }
            }
            None => failed = true,
        }
    }
    HeaderCheck {
        failed,
        wanted: wanted_lines.join(&b'\n'),
        output: output_lines.join(&b'\n'),
    }
}

/// A PHP numeric string, as an integer when it is one that fits, else as a float.
enum Numeric {
    Int(i64),
    Float(f64),
}

fn numeric(s: &[u8]) -> Option<Numeric> {
    let ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C);
    let start = s.iter().position(|b| !ws(b))?;
    let end = s.iter().rposition(|b| !ws(b))? + 1;
    let s = std::str::from_utf8(&s[start..end]).ok()?;
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    let (int_part, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    if int_part.len() + frac.map_or(0, str::len) == 0
        || !digits(int_part)
        || !frac.is_none_or(digits)
    {
        return None;
    }
    if let Some(e) = exponent {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !digits(e) {
            return None;
        }
    }
    if frac.is_none() && exponent.is_none() {
        if let Ok(i) = s.parse::<i64>() {
            return Some(Numeric::Int(i));
        }
    }
    s.parse::<f64>().ok().map(Numeric::Float)
}

/// PHP 8 `==` between two strings: numerically if both are numeric strings.
#[allow(clippy::cast_precision_loss, clippy::float_cmp)]
fn loose_eq(left: &[u8], right: &[u8]) -> bool {
    if left == right {
        return true;
    }
    let as_float = |n: Numeric| match n {
        Numeric::Int(int) => int as f64,
        Numeric::Float(float) => float,
    };
    match (numeric(left), numeric(right)) {
        (Some(Numeric::Int(l)), Some(Numeric::Int(r))) => l == r,
        (Some(l), Some(r)) => as_float(l) == as_float(r),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(text: &str) -> TestFile {
        TestFile::parse(Path::new("/x/t.phpt"), text.as_bytes(), false).unwrap()
    }

    fn val(env: &Env, k: &str) -> String {
        String::from_utf8_lossy(get(env, k)).into_owned()
    }

    #[test]
    fn request_vars_from_get_and_cookie() {
        let t = parse("--TEST--\nx\n--GET--\n a=1&b=2 \n--COOKIE--\nc=3;\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        set_request_vars(&mut env, &t, &OsString::from("/x/t.php"));
        assert_eq!(val(&env, "REDIRECT_STATUS"), "1");
        assert_eq!(val(&env, "QUERY_STRING"), "a=1&b=2");
        assert_eq!(val(&env, "PATH_TRANSLATED"), "/x/t.php");
        assert_eq!(val(&env, "SCRIPT_FILENAME"), "/x/t.php");
        assert_eq!(val(&env, "HTTP_COOKIE"), "c=3;");
        // Values from --ENV-- win.
        let mut env = Env::new();
        set(&mut env, "QUERY_STRING", "q=env");
        set(&mut env, "SCRIPT_FILENAME", "/other.php");
        set_request_vars(&mut env, &t, &OsString::from("/x/t.php"));
        assert_eq!(val(&env, "QUERY_STRING"), "q=env");
        assert_eq!(val(&env, "SCRIPT_FILENAME"), "/other.php");
        let t = parse("--TEST--\nx\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        set_request_vars(&mut env, &t, &OsString::from("/x/t.php"));
        assert_eq!(val(&env, "QUERY_STRING"), "");
        assert_eq!(val(&env, "HTTP_COOKIE"), "");
    }

    #[test]
    fn post_request() {
        let t = parse("--TEST--\nx\n--POST--\na=1&b=2\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        assert_eq!(build_request(&t, &mut env), Ok(Some(b"a=1&b=2".to_vec())));
        assert_eq!(val(&env, "REQUEST_METHOD"), "POST");
        assert_eq!(
            val(&env, "CONTENT_TYPE"),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(val(&env, "CONTENT_LENGTH"), "7");
        // CONTENT_TYPE / CONTENT_LENGTH from --ENV-- are kept.
        let mut env = Env::new();
        set(&mut env, "CONTENT_TYPE", "text/plain");
        set(&mut env, "CONTENT_LENGTH", "99");
        build_request(&t, &mut env).unwrap();
        assert_eq!(val(&env, "CONTENT_TYPE"), "text/plain");
        assert_eq!(val(&env, "CONTENT_LENGTH"), "99");
    }

    #[test]
    fn post_raw_and_put() {
        let t = parse(
            "--TEST--\nx\n--POST_RAW--\ncontent-type: multipart/form-data; boundary=b\r\n--b\r\nContent-Type: text/plain\r\n\r\nv\r\n--b--\n--FILE--\n--EXPECT--\n",
        );
        let mut env = Env::new();
        let body = build_request(&t, &mut env).unwrap().unwrap();
        assert_eq!(
            body,
            b"--b\r\nContent-Type: text/plain\r\n\r\nv\r\n--b--".to_vec()
        );
        assert_eq!(val(&env, "CONTENT_TYPE"), "multipart/form-data; boundary=b");
        assert_eq!(val(&env, "CONTENT_LENGTH"), body.len().to_string());
        assert_eq!(val(&env, "REQUEST_METHOD"), "POST");
        let mut env = Env::new();
        set(&mut env, "REQUEST_METHOD", "PATCH");
        build_request(&t, &mut env).unwrap();
        assert_eq!(val(&env, "REQUEST_METHOD"), "PATCH");

        let t = parse(
            "--TEST--\nx\n--PUT--\nContent-Type: text/json\n{\"a\":1}\n--FILE--\n--EXPECT--\n",
        );
        let mut env = Env::new();
        set(&mut env, "REQUEST_METHOD", "PATCH");
        assert_eq!(build_request(&t, &mut env), Ok(Some(b"{\"a\":1}".to_vec())));
        assert_eq!(val(&env, "REQUEST_METHOD"), "PUT");
        assert_eq!(val(&env, "CONTENT_TYPE"), "text/json");

        let t = parse("--TEST--\nx\n--PUT--\nContent-Type: text/json\n--FILE--\n--EXPECT--\n");
        assert_eq!(build_request(&t, &mut Env::new()), Err(EmptyRequest));
    }

    #[test]
    fn get_request_clears_body_vars() {
        let t = parse("--TEST--\nx\n--GET--\na=1\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        set(&mut env, "CONTENT_TYPE", "x");
        assert_eq!(build_request(&t, &mut env), Ok(None));
        assert_eq!(val(&env, "REQUEST_METHOD"), "GET");
        assert_eq!(val(&env, "CONTENT_TYPE"), "");
        assert_eq!(val(&env, "CONTENT_LENGTH"), "");
    }

    #[test]
    fn compressed_posts() {
        // Oracle: gzinflate/gzdecode/gzuncompress of these exact bytes give "hello".
        assert_eq!(deflate_stored(b"hello"), b"\x01\x05\x00\xfa\xffhello");
        assert_eq!(
            gzencode(b"hello"),
            b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\x03\x01\x05\x00\xfa\xffhello\x86\xa6\x10\x36\x05\x00\x00\x00"
        );
        assert_eq!(
            gzcompress(b"hello"),
            b"\x78\xda\x01\x05\x00\xfa\xffhello\x06\x2c\x02\x15"
        );
        assert_eq!(deflate_stored(b""), b"\x01\x00\x00\xff\xff");
        let big = vec![b'x'; 70_000];
        let d = deflate_stored(&big);
        assert_eq!(d.len(), 70_000 + 10);
        assert_eq!(d[0], 0);
        assert_eq!(d[5 + 0xFFFF], 1);

        let t = parse("--TEST--\nx\n--GZIP_POST--\nhello\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        let body = build_request(&t, &mut env).unwrap().unwrap();
        assert_eq!(body, gzencode(b"hello"));
        assert_eq!(val(&env, "HTTP_CONTENT_ENCODING"), "gzip");
        assert_eq!(val(&env, "REQUEST_METHOD"), "POST");
        assert_eq!(val(&env, "CONTENT_LENGTH"), body.len().to_string());
        let t = parse("--TEST--\nx\n--DEFLATE_POST--\nhello\n--FILE--\n--EXPECT--\n");
        let mut env = Env::new();
        assert_eq!(build_request(&t, &mut env), Ok(Some(gzcompress(b"hello"))));
        assert_eq!(val(&env, "HTTP_CONTENT_ENCODING"), "deflate");
    }

    #[test]
    fn response_splitting() {
        let out = b"Content-type: text/html\r\nX-A: 1\r\n\r\nbody\n\nmore";
        let (head, body) = split_response(out).unwrap();
        assert_eq!(head, b"Content-type: text/html\r\nX-A: 1");
        assert_eq!(body, b"body\n\nmore");
        assert_eq!(split_response(b"a\n\nb").unwrap(), (&b"a"[..], &b"b"[..]));
        assert_eq!(split_response(b"\n\r\nb").unwrap(), (&b""[..], &b"b"[..]));
        assert_eq!(split_response(b"a\r\r\nb"), None);
        assert_eq!(split_response(b"no blank line\n"), None);

        let h = parse_headers(b"Status: 404 Not Found\r\nX-A:  1 \r\nnoise\r\nX-A: 2");
        assert_eq!(h.get(&b"Status"[..]).unwrap(), b"404 Not Found");
        assert_eq!(h.get(&b"X-A"[..]).unwrap(), b"2");
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn expected_headers() {
        let h = parse_headers(b"Status: 302 Found\r\nLocation: /x\r\nX-N: 1e1");
        let c = check_headers(b"Location: /x\nStatus: 302 Found\n", &h);
        assert!(!c.failed);
        assert_eq!(c.wanted, b"Location: /x\nStatus: 302 Found");
        assert_eq!(c.output, b"Location: /x\nStatus: 302 Found");
        let c = check_headers(b"Location: /y\r\nX-Missing: 1\r\n", &h);
        assert!(c.failed);
        assert_eq!(c.wanted, b"Location: /y\nX-Missing: 1");
        assert_eq!(c.output, b"Location: /x");
        // PHP `!=` compares numeric strings numerically.
        assert!(!check_headers(b"X-N: 10", &h).failed);
        assert!(check_headers(b"X-N: 10a", &h).failed);
        // A repeated wanted name: last value wins, every line is listed.
        let c = check_headers(b"Location: /z\nLocation: /x", &h);
        assert!(!c.failed);
        assert_eq!(c.wanted, b"Location: /z\nLocation: /x");
        assert_eq!(c.output, b"Location: /x");
    }

    #[test]
    fn loose_string_comparison() {
        assert!(loose_eq(b"1", b"01"));
        assert!(loose_eq(b"1.0", b"1"));
        assert!(loose_eq(b"1e3", b"1000"));
        assert!(!loose_eq(b"abc", b"ABC"));
        assert!(!loose_eq(b"1", b"1x"));
        assert!(!loose_eq(b".", b"0"));
        assert!(!loose_eq(b"inf", b"INF"));
    }
}
