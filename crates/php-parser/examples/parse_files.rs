// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lexes or parses every file named in a list (one path per line), for
//! measuring the front end on a real code base:
//! `cargo run --release --example parse_files -- lex|parse <list>` under
//! `perf stat -e instructions:u`. Prints the file, byte and token counts.
//! `check` prints, per file, a hash of its tokens and one of its AST (or
//! parse error), for comparing two builds of the front end.

use php_lexer::{Lexer, TokenKind};
use php_parser::ParseOptions;
use std::hash::{Hash, Hasher};

php_memlimit::install_global_allocator!();

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(mode), Some(list)) = (args.next(), args.next()) else {
        eprintln!("usage: parse_files lex|parse|check <file-list>");
        std::process::exit(2);
    };
    let list = std::fs::read_to_string(&list).unwrap_or_default();
    let srcs: Vec<Vec<u8>> = list.lines().filter_map(|p| std::fs::read(p).ok()).collect();
    let (mut bytes, mut tokens, mut failed) = (0usize, 0usize, 0usize);
    let opts = ParseOptions::default();
    for src in &srcs {
        bytes += src.len();
        match mode.as_str() {
            "lex" => {
                let mut lx = Lexer::new(src);
                lx.set_skip_trivia(true);
                loop {
                    let t = lx.next_token_or_end();
                    let _ = lx.take_value();
                    tokens += 1;
                    if t.kind == TokenKind::End {
                        break;
                    }
                }
            }
            "check" => {
                let mut lx = Lexer::new(src);
                let mut h = std::collections::hash_map::DefaultHasher::new();
                while let Some(t) = lx.next_token() {
                    format!("{t:?} {:?}", lx.take_value()).hash(&mut h);
                }
                let toks = h.finish();
                let mut h = std::collections::hash_map::DefaultHasher::new();
                format!("{:?}", php_parser::parse(src, &opts)).hash(&mut h);
                println!("{toks:016x} {:016x}", h.finish());
            }
            _ => {
                if php_parser::parse(src, &opts).is_err() {
                    failed += 1;
                }
            }
        }
    }
    println!(
        "files={} bytes={bytes} tokens={tokens} failed={failed}",
        srcs.len()
    );
}
