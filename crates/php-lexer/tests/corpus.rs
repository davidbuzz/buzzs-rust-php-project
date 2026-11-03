//! Lexes the `--FILE--` section of every `.phpt` file under
//! `modules/php-src/{Zend,tests}` in both modes and checks that nothing
//! panics and that tokens tile the input.

use php_lexer::{LexMode, Lexer, LexerOptions};
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "phpt") {
            out.push(p);
        }
    }
}

/// Extracts the `--FILE--` (or `--FILEEOF--`) section of a `.phpt` file.
fn file_section(data: &[u8]) -> Option<Vec<u8>> {
    let mut sec: Option<String> = None;
    let mut body = Vec::new();
    let mut found = None;
    for line in data.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed.len() > 4
            && trimmed.starts_with(b"--")
            && trimmed.ends_with(b"--")
            && trimmed[2..trimmed.len() - 2]
                .iter()
                .all(|c| c.is_ascii_uppercase() || *c == b'_')
        {
            if matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) && found.is_none() {
                found = Some((sec.clone(), std::mem::take(&mut body)));
            }
            sec = Some(String::from_utf8_lossy(&trimmed[2..trimmed.len() - 2]).into_owned());
            continue;
        }
        if matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) {
            body.extend_from_slice(line);
        }
    }
    if found.is_none() && matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) {
        found = Some((sec, body));
    }
    let (name, mut body) = found?;
    if name.as_deref() == Some("FILEEOF") {
        if body.ends_with(b"\n") {
            body.pop();
        }
        if body.ends_with(b"\r") {
            body.pop();
        }
    }
    Some(body)
}

#[test]
fn lex_phpt_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src");
    let mut files = Vec::new();
    collect(&root.join("Zend/tests"), &mut files);
    collect(&root.join("tests"), &mut files);
    if files.is_empty() {
        eprintln!("php-src not found; skipping");
        return;
    }
    let mut n = 0;
    for f in &files {
        let Ok(data) = std::fs::read(f) else { continue };
        let Some(src) = file_section(&data) else {
            continue;
        };
        n += 1;
        // Tokenizer mode: tokens must tile the input exactly (modulo
        // __halt_compiler, which token_get_all handles).
        let toks = php_lexer::token_get_all(&src);
        let mut pos = 0;
        for t in &toks {
            assert_eq!(t.pos, pos, "gap in tokens of {}", f.display());
            pos += t.text.len();
        }
        assert_eq!(pos, src.len(), "tokens do not cover {}", f.display());
        // Parser mode: must terminate without panicking.
        let mut lx = Lexer::with_options(
            &src,
            LexerOptions {
                mode: LexMode::Parser,
                ..LexerOptions::default()
            },
        );
        let mut count = 0usize;
        while lx.next_token().is_some() {
            let _ = lx.take_value();
            count += 1;
            assert!(count <= src.len() + 2, "lexer loops on {}", f.display());
        }
    }
    eprintln!("lexed {n} phpt FILE sections");
    assert!(n > 5000);
}
