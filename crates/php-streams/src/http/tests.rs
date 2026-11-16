// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use super::*;
use crate::context::StreamContext;

/// Serves one canned response per accepted connection; the requests are
/// sent back through the channel.
fn server(responses: Vec<&'static [u8]>) -> (String, mpsc::Receiver<Vec<u8>>) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for resp in responses {
            let Ok((mut c, _)) = l.accept() else { return };
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let _ = tx.send(req);
            let _ = c.write_all(resp);
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

fn ctx(opts: &[(&str, Value)]) -> ContextRef {
    let mut c = StreamContext::new();
    for (k, v) in opts {
        c.set("http", k, v.clone());
    }
    c.shared()
}

fn headers_of(s: &Stream) -> Vec<Vec<u8>> {
    match &s.wrapper_data {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|(_, v)| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[test]
fn get_with_headers_and_body() {
    let (uri, rx) = server(vec![
        b"HTTP/1.0 200 Ok\r\nSome: Header\r\nFolded: a\r\n  b\r\n\r\nBody",
    ]);
    let mut s = open_url(format!("{uri}/foo?x=1").as_bytes(), "rb", 0, None).expect("open");
    let req = rx.recv().expect("request");
    let host = uri.trim_start_matches("http://");
    assert_eq!(
        req,
        format!("GET /foo?x=1 HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").into_bytes()
    );
    assert_eq!(
        headers_of(&s),
        vec![
            b"HTTP/1.0 200 Ok".to_vec(),
            b"Some: Header".to_vec(),
            b"Folded: a b".to_vec()
        ]
    );
    assert_eq!(s.copy_to_mem(None).expect("body"), b"Body");
    assert_eq!(s.mode, "rb");
    assert_eq!(take_default_headers().map(|h| h.len()), Some(3));
}

#[test]
fn chunked_body_and_post_content() {
    let (uri, rx) = server(vec![
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nHello\r\n0\r\n\r\n",
    ]);
    let c = ctx(&[
        ("method", Value::str("POST")),
        ("content", Value::str("a=b")),
        ("header", Value::str("X-A: 1\r\n")),
        ("user_agent", Value::str("ua")),
    ]);
    let mut s = open_url(uri.as_bytes(), "r", 0, Some(&c)).expect("open");
    assert_eq!(s.copy_to_mem(None).expect("body"), b"Hello");
    let req = String::from_utf8(rx.recv().expect("request")).expect("utf8");
    assert!(req.starts_with("POST / HTTP/1.1\r\n"), "{req}");
    assert!(
        req.contains("User-Agent: ua\r\nContent-Length: 3\r\nX-A: 1\r\n"),
        "{req}"
    );
    assert!(
        req.ends_with("Content-Type: application/x-www-form-urlencoded\r\n\r\n")
            || req.contains("Content-Type: application/x-www-form-urlencoded\r\n\r\n")
    );
    assert_eq!(headers_of(&s), vec![b"HTTP/1.1 200 OK".to_vec()]);
}

#[test]
fn errors_and_redirects() {
    let (uri, _rx) = server(vec![
        b"HTTP/1.1 404 Not found\r\nX: y\r\n\r\n",
        b"HTTP/1.1 302 Found\r\nLocation: /next\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r\ndone",
    ]);
    let e = open_url(uri.as_bytes(), "r", 0, None).expect_err("404");
    assert_eq!(
        e.messages,
        vec![b"HTTP request failed! HTTP/1.1 404 Not found\r\n".to_vec()]
    );
    let mut s = open_url(uri.as_bytes(), "r", 0, None).expect("redirect");
    assert_eq!(s.copy_to_mem(None).expect("body"), b"done");
    assert_eq!(headers_of(&s).len(), 3);
    let e = open_url(uri.as_bytes(), "w", 0, None).expect_err("mode");
    assert_eq!(
        e.messages,
        vec![b"HTTP wrapper does not support writeable connections".to_vec()]
    );
}

#[test]
fn header_helpers() {
    let mut bag = b"Content-Type: x\r\nA: b\r\ncontent-length : 3\r\n more".to_vec();
    let mut lc = bag.to_ascii_lowercase();
    strip_header(&mut bag, &mut lc, b"content-type");
    strip_header(&mut bag, &mut lc, b"content-length");
    assert_eq!(bag, b"A: b");
    assert!(check_has_header(b"a: b\r\nhost: x", b"host:"));
    assert!(!check_has_header(b"a: host: x", b"host:"));
    assert_eq!(php_trim(b"\r\n x \0"), b"x");
    let mut out = Vec::new();
    assert!(proxy_authorization_header(
        b"A: b\r\n proxy-authorization: secret\r\n",
        &mut out
    ));
    assert_eq!(out, b"proxy-authorization: secret\r\n");
    let r = Uri {
        scheme: Some(b"http".to_vec()),
        host: Some(b"h".to_vec()),
        port: 8080,
        path: Some(b"/a/b".to_vec()),
        ..Uri::default()
    };
    let t = Target {
        use_ssl: false,
        use_proxy: false,
        request_fulluri: false,
        transport: Vec::new(),
    };
    assert_eq!(redirect_url(&r, &t, b"cd"), b"http://h:8080/a//cd");
    assert_eq!(redirect_url(&r, &t, b"c"), b"http://h:8080/c");
    assert_eq!(redirect_url(&r, &t, b"/c"), b"http://h:8080/c");
    assert_eq!(redirect_url(&r, &t, b"http://x/y"), b"http://x/y");
}
