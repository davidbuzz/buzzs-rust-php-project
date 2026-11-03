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

fn significant(src: &str) -> Vec<(TokenKind, String)> {
    kinds(src)
        .into_iter()
        .filter(|t| !t.0.is_trivia())
        .map(|t| (t.0, t.1))
        .collect()
}

#[test]
fn asymmetric_visibility_tokens() {
    use TokenKind as T;
    let t = significant("<?php public(set) protected(SET) PRIVATE(set) public (set) public(get)");
    let k: Vec<_> = t.iter().map(|x| x.0).collect();
    assert_eq!(
        k,
        vec![
            T::PublicSet,
            T::ProtectedSet,
            T::PrivateSet,
            T::Public,
            T::LParen,
            T::String,
            T::RParen,
            T::Public,
            T::LParen,
            T::String,
            T::RParen
        ]
    );
    assert_eq!(t[1].1, "protected(SET)");
}

#[test]
fn pipe_void_cast_property_constant() {
    use TokenKind as T;
    let k: Vec<_> = significant(
        "<?php $a |> f(...); $a ||> $b; $a |>= 1; (void) f(); ( VOID\t)$x; __PROPERTY__;",
    )
    .into_iter()
    .map(|x| x.0)
    .collect();
    assert_eq!(
        k,
        vec![
            T::Variable,
            T::PipeOp,
            T::String,
            T::LParen,
            T::Ellipsis,
            T::RParen,
            T::Semicolon,
            T::Variable,
            T::BooleanOr,
            T::Gt,
            T::Variable,
            T::Semicolon,
            T::Variable,
            T::PipeOp,
            T::Equals,
            T::LNumber,
            T::Semicolon,
            T::VoidCast,
            T::String,
            T::LParen,
            T::RParen,
            T::Semicolon,
            T::VoidCast,
            T::Variable,
            T::Semicolon,
            T::PropertyC,
            T::Semicolon
        ]
    );
}

#[test]
fn comments_after_object_operator() {
    // gh14961 (fixed in php-src master): comments between `->` and a keyword
    // keep the scanner looking for a property name.
    use TokenKind as T;
    let t = kinds("<?php $c->/* c */class; $c?->\n// x\nclass; $c->\n# y\nclass;");
    let k: Vec<_> = t
        .iter()
        .filter(|t| !matches!(t.0, T::Whitespace))
        .map(|t| t.0)
        .collect();
    assert_eq!(
        k,
        vec![
            T::OpenTag,
            T::Variable,
            T::ObjectOperator,
            T::Comment,
            T::String,
            T::Semicolon,
            T::Variable,
            T::NullsafeObjectOperator,
            T::Comment,
            T::String,
            T::Semicolon,
            T::Variable,
            T::ObjectOperator,
            T::Comment,
            T::String,
            T::Semicolon
        ]
    );
}

#[test]
fn keywords_are_case_insensitive_and_whole_words() {
    use TokenKind as T;
    let k: Vec<_> = significant("<?php CLASS Classes iF ELSEIF __halt_COMPILER Die")
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(
        k,
        vec![
            T::Class,
            T::String,
            T::If,
            T::ElseIf,
            T::HaltCompiler,
            T::Exit
        ]
    );
}

#[test]
fn line_numbers() {
    use TokenKind as T;
    let t = kinds("<?php\n$a\n=\n<<<X\n  a\n  b\n  X\n;\n/* c\n*/ $b ?>\nx\ny<?php $c");
    let lines: Vec<_> = t
        .iter()
        .filter(|t| !matches!(t.0, TokenKind::Whitespace))
        .map(|t| (t.0, t.2))
        .collect();
    assert_eq!(
        lines,
        vec![
            (T::OpenTag, 1),
            (T::Variable, 2),
            (T::Equals, 3),
            (T::StartHeredoc, 4),
            (T::EncapsedAndWhitespace, 5),
            (T::EndHeredoc, 7),
            (T::Semicolon, 8),
            (T::Comment, 9),
            (T::Variable, 10),
            (T::CloseTag, 10),
            (T::InlineHtml, 11),
            (T::OpenTag, 12),
            (T::Variable, 12),
        ]
    );
}
