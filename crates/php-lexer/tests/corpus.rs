// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lexes the `--FILE--` section of every `.phpt` file under
//! `modules/php-src` (Zend/tests, tests/, ext/*/tests, sapi/...) in both
//! modes and checks that nothing panics and that tokens tile the input.

mod common;

use php_lexer::{LexMode, Lexer, LexerOptions};

#[test]
fn lex_phpt_corpus() {
    let corpus = common::corpus();
    if corpus.is_empty() {
        eprintln!("php-src not found; skipping");
        return;
    }
    for (f, src) in &corpus {
        // Tokenizer mode: tokens must tile the input exactly (token_get_all
        // also covers the data after __halt_compiler with T_INLINE_HTML).
        let toks = php_lexer::token_get_all(src);
        let mut pos = 0;
        for t in &toks {
            assert_eq!(t.pos, pos, "gap in tokens of {}", f.display());
            pos += t.text.len();
        }
        assert_eq!(pos, src.len(), "tokens do not cover {}", f.display());

        // Parser mode (and eval-style start): must terminate without
        // panicking, with monotonically advancing spans.
        for start_in_scripting in [false, true] {
            let mut lx = Lexer::with_options(
                src,
                LexerOptions {
                    mode: LexMode::Parser,
                    start_in_scripting,
                    ..LexerOptions::default()
                },
            );
            let mut last_end = 0;
            let mut last_line = 1;
            while let Some(t) = lx.next_token() {
                let _ = lx.take_value();
                assert!(t.span.start >= last_end, "overlap in {}", f.display());
                assert!(
                    t.span.line >= last_line,
                    "line went back in {}",
                    f.display()
                );
                assert!(t.end_line >= t.span.line);
                last_end = t.span.end;
                last_line = t.span.line;
            }
        }
    }
    eprintln!("lexed {} phpt FILE sections", corpus.len());
    assert!(corpus.len() > 20_000);
}
