// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Oracle tests for `base64.rs`, `uuencode.rs` and `quoted_printable.rs`.
mod common;
use common::{case, check_mapped, lit, pure, regenerate, Case, V};
use php_stdstr::base64::{base64_decode, base64_encode};
use php_stdstr::quoted_printable::{quoted_printable_decode, quoted_printable_encode};
use php_stdstr::uuencode::{convert_uudecode, convert_uuencode};

/// `bin2hex()` — binary results are compared hex-encoded so that invalid
/// UTF-8 survives the textual fixture byte-exactly.
fn hex(s: &[u8]) -> String {
    s.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Deterministic pseudo-random bytes.
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (x >> 16).to_le_bytes()[0]
        })
        .collect()
}

fn samples() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = [
        &b""[..],
        b"a",
        b"ab",
        b"abc",
        b"abcd",
        b"hello world",
        b"test\ntext text text\r\n",
        b"\x00",
        b"\x00\x00\x00",
        b"\xff\xfe\xfd",
        b"=",
        b"a=b",
        b"a \r\nb",
        b"trailing space \r",
        b" \r",
        b"\r",
        b"\r\n",
        b"\n\n",
        b"caf\xc3\xa9 \xe2\x82\xac \xf0\x9f\x98\x80",
    ]
    .iter()
    .map(|s| s.to_vec())
    .collect();
    for n in [44usize, 45, 46, 47, 89, 90, 91, 135, 200] {
        v.push(noise(n, u32::try_from(n).unwrap_or(0)));
    }
    v.push(b"x".repeat(75));
    v.push(b"x".repeat(76));
    v.push(b"x".repeat(77));
    v.push(b"x".repeat(160));
    v.push(b"\xc4\x85".repeat(77));
    v.push(b"\xe2\x82\xac".repeat(40));
    v.push(b"\xf0\x9f\x98\x80".repeat(30));
    v.push(b"\xf4".repeat(100));
    v.push(b"\x7f".repeat(30));
    let mut mixed = b"x".repeat(70);
    mixed.extend_from_slice(b"\xe2\x82\xac\xe2\x82\xac=  \r\nend");
    v.push(mixed);
    v.push((0..=255u8).collect());
    v
}

const B64_DECODE: &[&[u8]] = &[
    b"",
    b"Zg==",
    b"Zm8=",
    b"Zm9v",
    b"Zm9vYg",
    b"Zm9vYmE",
    b"Zm9vY",
    b"Z",
    b"Zg=",
    b"Zg===",
    b"Zm9v\nYmFy",
    b" Zm9v\tYmFy\r\n",
    b"Zm9v YmFy",
    b"Zm9v!YmFy",
    b"Zm9vYg==Zg==",
    b"Zg==\n",
    b"Zg= =",
    b"VV= =",
    b"*",
    b"=*",
    b"VVV=",
    b"VVV=*",
    b"\x00",
    b"\x00VVVV",
    b"VVVV\x00",
    b"6Q==",
    b"=",
    b"==",
    b"===",
    b"====",
    b"Zm9=v",
    b"\xf0\xffZm9v",
    b"-_-_",
    b"YWJj\x80ZGVm",
];

const UU_DECODE: &[&[u8]] = &[
    b"",
    b"`\n",
    b"!80``\n`\n",
    b"#86)C\n`\n",
    b"#86)C",
    b"#86)",
    b"M86)C",
    b"!",
    b"!8",
    b"!80",
    b"\x80",
    b"~~~~",
    b"begin 644 x\n#86)C\n`\nend\n",
    b"5=&5S=`IT97AT('1E>'0@=&5X=`T*\n`\n",
    b"5=&5S=`IT97AT('1E>'0@=&5X=`T*",
    b" ",
    b"!!",
    b"%86)C9&4",
];

const QP_DECODE: &[&[u8]] = &[
    b"",
    b"=41=42=43",
    b"=4a=4A=4",
    b"=",
    b"==",
    b"=3D",
    b"=3d=",
    b"a=\r\nb",
    b"a=\nb",
    b"a=\rb",
    b"a=  \t \r\nb",
    b"a=  x",
    b"a= ",
    b"=zz",
    b"=4z",
    b"a\x00=41",
    b"=\x00=41",
    b"=E4=F6=FC=DF",
    b"Hello=20World=0D=0A",
    b"_under_score_",
];

fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for s in samples() {
        let l = lit(&s);
        v.push(pure(&format!("base64_encode({l})"), base64_encode(&s)));
        v.push(pure(
            &format!("convert_uuencode({l})"),
            convert_uuencode(&s),
        ));
        v.push(pure(
            &format!("quoted_printable_encode({l})"),
            quoted_printable_encode(&s),
        ));
        v.push(pure(
            &format!("bin2hex(quoted_printable_decode(quoted_printable_encode({l})))"),
            hex(&quoted_printable_decode(&quoted_printable_encode(&s))),
        ));
        v.push(case(
            &format!("bin2hex(convert_uudecode(convert_uuencode({l})))"),
            |n| {
                Ok(hex(
                    &convert_uudecode(&convert_uuencode(&s), n).unwrap_or_default()
                ))
            },
        ));
    }
    for &s in B64_DECODE {
        let l = lit(s);
        for strict in [false, true] {
            v.push(pure(
                &format!("(($r = base64_decode({l}, {strict})) === false ? false : bin2hex($r))"),
                base64_decode(s, strict).map(|r| hex(&r)),
            ));
        }
    }
    for &s in UU_DECODE {
        let l = lit(s);
        let c = case(
            &format!("(($r = convert_uudecode({l})) === false ? false : bin2hex($r))"),
            |n| Ok(convert_uudecode(s, n).map(|r| hex(&r))),
        );
        // php-src master renamed the parameter `$data` → `$string`.
        let invalid: &[&[u8]] = &[
            b"",
            b"#86)",
            b"M86)C",
            b"!",
            b"!8",
            b"!80",
            b"\x80",
            b"~~~~",
            b"!!",
            b"%86)C9&4",
        ];
        if invalid.contains(&s) {
            v.push(c.expect(
                "Warning: convert_uudecode(): Argument #1 ($string) is not a valid uuencoded string\n=> false",
            ));
        } else {
            v.push(c);
        }
    }
    for &s in QP_DECODE {
        let l = lit(s);
        v.push(pure(
            &format!("bin2hex(quoted_printable_decode({l}))"),
            hex(&quoted_printable_decode(s)),
        ));
    }
    v
}

/// php-src master's `php_quot_print_encode()` (GH-25284), transliterated
/// from `ext/standard/quot_print.c` as an independent reference: PHP 8.5
/// (the oracle) still ends a soft line before a UTF-8 continuation byte.
fn master_qp_encode(s: &[u8]) -> Vec<u8> {
    const MAXL: u64 = 75;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::new();
    let mut lp: u64 = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        i += 1;
        let next = s.get(i).copied().unwrap_or(0);
        if c == b'\r' && next == b'\n' && i < s.len() {
            out.extend_from_slice(b"\r\n");
            i += 1;
            lp = 0;
        } else if c.is_ascii_control() || c & 0x80 != 0 || c == b'=' || (c == b' ' && next == b'\r')
        {
            lp += 3;
            if (lp > MAXL && (c <= 0xbf || c > 0xf4))
                || (c > 0xbf && c <= 0xdf && lp + 3 > MAXL)
                || (c > 0xdf && c <= 0xef && lp + 6 > MAXL)
                || (c > 0xef && c <= 0xf4 && lp + 9 > MAXL)
            {
                out.extend_from_slice(b"=\r\n");
                lp = 3;
            }
            out.push(b'=');
            out.push(HEX[usize::from(c >> 4)]);
            out.push(HEX[usize::from(c & 0xf)]);
        } else {
            lp += 1;
            if lp > MAXL {
                out.extend_from_slice(b"=\r\n");
                lp = 1;
            }
            out.push(c);
        }
    }
    out
}

#[test]
fn oracle() {
    // quoted_printable_encode() follows master's line breaking (checked
    // against the reference above, not the 8.5 oracle).
    let mut master: std::collections::HashMap<String, String> = samples()
        .iter()
        .map(|s| {
            (
                format!("quoted_printable_encode({})", lit(s)),
                format!("=> {}", V::from(master_qp_encode(s)).export()),
            )
        })
        .collect();
    // quoted_printable_decode() works by length in master (ba8c7aa4772):
    // PHP 8.5 stops at the first NUL byte, master decodes past it.
    for (input, hex) in [(&b"a\x00=41"[..], "610041"), (b"=\x00=41", "3d0041")] {
        master.insert(
            format!("bin2hex(quoted_printable_decode({}))", lit(input)),
            format!("=> '{hex}'"),
        );
    }
    check_mapped("encoding", &cases(), |php, expected| {
        master
            .get(php)
            .cloned()
            .unwrap_or_else(|| expected.to_owned())
    });
}

#[test]
#[ignore = "runs /usr/bin/php8.5 to rebuild the fixture"]
fn regenerate_fixture() {
    regenerate("encoding", &cases());
}
