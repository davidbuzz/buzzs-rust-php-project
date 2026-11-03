use super::*;

fn kinds(src: &str) -> Vec<(TokenKind, String, u32)> {
    token_get_all(src.as_bytes())
        .into_iter()
        .map(|t| (t.kind, String::from_utf8_lossy(t.text).into_owned(), t.line))
        .collect()
}

#[test]
fn basic() {
    use TokenKind as T;
    let t = kinds("<?php echo $a + 1;");
    let k: Vec<TokenKind> = t.iter().map(|x| x.0).collect();
    assert_eq!(
        k,
        vec![
            T::OpenTag,
            T::Echo,
            T::Whitespace,
            T::Variable,
            T::Whitespace,
            T::Plus,
            T::Whitespace,
            T::LNumber,
            T::Semicolon
        ]
    );
}

#[test]
fn shebang() {
    let opts = LexerOptions {
        skip_shebang: true,
        ..LexerOptions::default()
    };
    let toks = token_get_all_with(b"#!/usr/bin/php\n<?php\necho 1;", opts);
    assert_eq!(toks[0].kind, TokenKind::OpenTag);
    // PHP restarts scanning after the shebang without resetting the token's
    // start line, so the open tag reports line 1; the following code is on
    // the right lines.
    assert_eq!(toks[0].line, 1);
    assert_eq!(toks[0].pos, 15);
    assert_eq!(toks[0].text, b"<?php\n");
    assert_eq!(toks[1].line, 3);
    // \r line endings: the match extends to the last \r.
    let toks = token_get_all_with(b"#!php\r<?php 1;", opts);
    assert_eq!(toks[0].kind, TokenKind::OpenTag);
    // No line ending at all: nothing is skipped.
    let toks = token_get_all_with(b"#!php", opts);
    assert_eq!(toks[0].kind, TokenKind::InlineHtml);
    // Without the option the shebang is inline HTML.
    let toks = token_get_all(b"#!/usr/bin/php\n<?php");
    assert_eq!(toks[0].kind, TokenKind::InlineHtml);
}

#[test]
fn halt_compiler() {
    let toks = token_get_all(b"<?php __halt_compiler ( ) ; raw <?php stuff");
    let last = toks.last().unwrap();
    assert_eq!(last.kind, TokenKind::InlineHtml);
    assert_eq!(last.text, b" raw <?php stuff");
}

#[test]
fn tokenizer_mode_warnings() {
    let mut lx = Lexer::new(b"<?php (integer)\"\\400\";");
    while lx.next_token().is_some() {}
    assert_eq!(lx.warnings().len(), 1);
    assert_eq!(lx.warnings()[0].kind, WarningKind::CompileWarning);
}

#[test]
fn eval_mode_starts_in_scripting() {
    let opts = LexerOptions {
        start_in_scripting: true,
        ..LexerOptions::default()
    };
    let toks = token_get_all_with(b"echo 1;", opts);
    assert_eq!(toks[0].kind, TokenKind::Echo);
}

#[test]
fn short_tags() {
    use TokenKind as T;
    let opts = LexerOptions {
        short_tags: true,
        ..LexerOptions::default()
    };
    let toks = token_get_all_with(b"a<? echo 1; ?>b<?phpx", opts);
    let k: Vec<_> = toks.iter().map(|t| t.kind).collect();
    assert_eq!(k[0], T::InlineHtml);
    assert_eq!(k[1], T::OpenTag);
    assert_eq!(toks[1].text, b"<?");
    assert_eq!(*k.last().unwrap(), T::String);
}

#[test]
fn iterator_interface() {
    let lx = Lexer::new(b"<?php $a;");
    let kinds: Vec<_> = lx.map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TokenKind::OpenTag,
            TokenKind::Variable,
            TokenKind::Semicolon
        ]
    );
}
