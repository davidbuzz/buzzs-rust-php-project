// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Deterministic fuzzing: concatenations of PHP-ish fragments and random
//! bytes must lex without panicking in every mode, with tokens tiling the
//! input in tokenizer mode.

use php_lexer::{token_get_all_with, LexMode, Lexer, LexerOptions};

const FRAGMENTS: &[&[u8]] = &[
    b"<?php ",
    b"<?=",
    b"<?",
    b"?>",
    b"?>\n",
    b"<<<A\n",
    b"<<<'A'\n",
    b"<<<\"A\"\n",
    b"A;\n",
    b"  A\n",
    b"\n",
    b"\r\n",
    b"\r",
    b"\t",
    b" ",
    b"$a",
    b"$",
    b"${",
    b"{$",
    b"{",
    b"}",
    b"[",
    b"]",
    b"(",
    b")",
    b"->",
    b"?->",
    b"\"",
    b"'",
    b"`",
    b"\\",
    b"\\u{",
    b"\\x",
    b"\\0",
    b"#",
    b"#[",
    b"//",
    b"/*",
    b"/**",
    b"*/",
    b"0x",
    b"0b1",
    b"0o7",
    b"08",
    b"1_",
    b"1.e",
    b".5",
    b"(int)",
    b"( void )",
    b"public(set)",
    b"yield from",
    b"enum ",
    b"&",
    b"&$",
    b"&...",
    b"__halt_compiler();",
    b"class",
    b"b\"",
    b"b'",
    b"\0",
    b"\xff",
    b"-",
    b"=",
    b";",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn random_inputs_do_not_panic() {
    let mut rng = Rng(0x5eed);
    for _ in 0..20_000 {
        let n = (rng.next() % 24) as usize;
        let mut src = Vec::new();
        for _ in 0..n {
            if rng.next() % 8 == 0 {
                src.push((rng.next() & 0xff) as u8);
            } else {
                let idx = usize::try_from(rng.next() % FRAGMENTS.len() as u64).unwrap_or(0);
                let f = FRAGMENTS[idx];
                src.extend_from_slice(f);
            }
        }
        for short_tags in [false, true] {
            for start_in_scripting in [false, true] {
                let opts = LexerOptions {
                    short_tags,
                    start_in_scripting,
                    skip_shebang: rng.next() % 2 == 0,
                    ..LexerOptions::default()
                };
                let toks = token_get_all_with(&src, opts);
                let covered: usize = toks.iter().map(|t| t.text.len()).sum();
                assert!(covered <= src.len(), "{src:?}");
                let mut lx = Lexer::with_options(
                    &src,
                    LexerOptions {
                        mode: LexMode::Parser,
                        ..opts
                    },
                );
                let mut count = 0;
                while lx.next_token().is_some() {
                    let _ = lx.take_value();
                    count += 1;
                    assert!(count <= src.len() + 4, "no progress on {src:?}");
                }
            }
        }
    }
}
