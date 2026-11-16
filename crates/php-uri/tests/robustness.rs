// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Throws pseudo-random inputs at every entry point to make sure nothing
//! panics and that basic invariants hold.

use php_uri::api::{Rfc3986Uri, Rfc3986UriBuilder, WhatWgUrl, WhatWgUrlBuilder};
use php_uri::whatwg::UrlPercentEncodingMode;
use php_uri::UriComparisonMode;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
}

const PIECES: &[&str] = &[
    "a",
    "Z",
    "0",
    "9",
    "-",
    ".",
    "_",
    "~",
    "%",
    "%2e",
    "%2E",
    "%41",
    "%zz",
    "%",
    ":",
    "/",
    "//",
    "\\",
    "?",
    "#",
    "@",
    "[",
    "]",
    "::",
    "1.2.3.4",
    "0x7f",
    "[::1]",
    "[v1.x]",
    "http:",
    "https://",
    "file:",
    "file:///",
    "C|",
    "c:",
    "..",
    ".",
    " ",
    "\t",
    "\n",
    "\r",
    "\0",
    "é",
    "ü",
    "例",
    "😀",
    "xn--",
    "xn--a",
    "xn--bcher-kva",
    "\u{FFFF}",
    "\u{200D}",
    "ß",
    "。",
    "Ａ",
    "\u{0301}",
    "\x7f",
    "\u{FDD0}",
    "localhost",
    "999999",
    "65536",
    "!",
    "$",
    "'",
    "(",
    ")",
    "*",
    "+",
    ",",
    ";",
    "=",
    "<",
    ">",
    "^",
    "`",
    "{",
    "|",
    "}",
    "\"",
];

fn random_input(rng: &mut Rng) -> Vec<u8> {
    let n = rng.below(12);
    let mut out = Vec::new();
    for _ in 0..n {
        if rng.below(10) == 0 {
            out.push(u8::try_from(rng.below(256)).unwrap_or(0));
        } else {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())].as_bytes());
        }
    }
    out
}

#[test]
fn rfc3986_never_panics() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let base = Rfc3986Uri::new(b"http://a/b/c/d;p?q", None).unwrap();
    for _ in 0..20_000 {
        let input = random_input(&mut rng);
        let Some(u) = Rfc3986Uri::parse(&input, None) else {
            let _ = Rfc3986Uri::parse(&input, Some(&base));
            continue;
        };
        // Raw recomposition must re-parse.
        let raw = u.to_raw_string();
        assert!(Rfc3986Uri::parse(&raw, None).is_some(), "{raw:?}");
        let _ = u.to_string_normalized();
        let _ = (
            u.scheme(),
            u.user_info(),
            u.username(),
            u.password(),
            u.host(),
        );
        let _ = (
            u.port(),
            u.path(),
            u.query(),
            u.fragment(),
            u.uri_type(),
            u.host_type(),
        );
        let _ = u.equals(&base, UriComparisonMode::IncludeFragment);
        let other = random_input(&mut rng);
        let _ = u.with_scheme(Some(&other));
        let _ = u.with_user_info(Some(&other));
        let _ = u.with_host(Some(&other));
        let _ = u.with_host(None);
        let _ = u.with_port(Some(80));
        let _ = u.with_path(&other);
        let _ = u.with_query(Some(&other));
        let _ = u.with_fragment(Some(&other));
        let _ = u.resolve(&other);
        let _ = base.resolve(&input);
        let mut b = Rfc3986UriBuilder::new();
        let _ = b.set_host(Some(&other));
        let _ = b.set_path(&input);
        let _ = b.build(Some(&base));
    }
}

#[test]
fn whatwg_never_panics() {
    let mut rng = Rng(0x0F0F_1234_ABCD_9876);
    let bases: Vec<WhatWgUrl> = [
        "https://user:pw@example.com:8080/a/b?q#f",
        "file:///C:/dir/file",
        "foo:opaque?x#y",
        "foo://host/path",
    ]
    .iter()
    .map(|s| WhatWgUrl::new(s.as_bytes(), None).unwrap().0)
    .collect();
    for _ in 0..20_000 {
        let input = random_input(&mut rng);
        let base = &bases[rng.below(bases.len())];
        let parsed = WhatWgUrl::parse(&input, None)
            .ok()
            .or_else(|| WhatWgUrl::parse(&input, Some(base)).ok());
        let other = random_input(&mut rng);
        if let Some((u, _)) = parsed {
            let ascii = u.to_ascii_string();
            // Serializations of successfully parsed URLs re-parse.
            assert!(WhatWgUrl::parse(&ascii, None).is_ok(), "{ascii:?}");
            let _ = u.to_unicode_string();
            let _ = (u.ascii_host(), u.unicode_host(), u.host_type(), u.path());
            let _ = u.with_scheme(&other);
            let _ = u.with_username(Some(&other));
            let _ = u.with_password(Some(&other));
            let _ = u.with_host(Some(&other));
            let _ = u.with_port(Some(i64::from(
                u16::try_from(rng.below(70_000)).unwrap_or(0),
            )));
            let _ = u.with_path(&other);
            let _ = u.with_query(Some(&other));
            let _ = u.with_fragment(Some(&other));
            let _ = u.resolve(&other);
            let _ = u.equals(base, UriComparisonMode::ExcludeFragment);
        }
        let mut b = WhatWgUrlBuilder::new();
        let _ = b.set_scheme(&random_input(&mut rng));
        let _ = b.set_host(Some(&other));
        b.set_path(&input);
        b.set_username(Some(&other));
        let _ = b.build(None);
        let _ = b.build(Some(base));
        let mut b2 = WhatWgUrlBuilder::new();
        b2.set_path(&input);
        b2.set_query(Some(&other));
        let _ = b2.build(Some(base));
        for mode in UrlPercentEncodingMode::ALL {
            let _ = php_uri::whatwg::url_percent_encode(&input, mode);
        }
    }
}
