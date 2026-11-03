//! Parser-mode scanner errors: message text and line numbers, checked
//! against `php -l` output.

use php_lexer::{LexMode, Lexer, LexerOptions, TokenKind, TokenValue, WarningKind};

fn parser_lexer(src: &[u8]) -> Lexer<'_> {
    Lexer::with_options(
        src,
        LexerOptions {
            mode: LexMode::Parser,
            ..LexerOptions::default()
        },
    )
}

/// Runs the lexer in parser mode to the first error.
fn first_error(src: &str) -> Option<(String, u32)> {
    let mut lx = parser_lexer(src.as_bytes());
    while let Some(t) = lx.next_token() {
        if t.kind == TokenKind::Error {
            let e = lx.error().expect("error token without error");
            return Some((e.message.clone(), e.line));
        }
    }
    None
}

#[test]
fn error_messages_match_php() {
    let cases: &[(&str, &str, u32)] = &[
        (
            "<?php\n$a = 1;\n/* never\nends\n",
            "Unterminated comment starting line 3",
            3,
        ),
        ("<?php\n$a = 08;\n", "Invalid numeric literal", 2),
        ("<?php\n$a = 0_8_9;\n", "Invalid numeric literal", 2),
        (
            "<?php\n$a = (real) 1;\n",
            "The (real) cast has been removed, use (float) instead",
            2,
        ),
        (
            "<?php\n$a = \"x\n\\u{zz}\";\n",
            "Invalid UTF-8 codepoint escape sequence",
            3,
        ),
        (
            "<?php\n$a = \"x\n\\u{110000}\";\n",
            "Invalid UTF-8 codepoint escape sequence: Codepoint too large",
            3,
        ),
        (
            "<?php\necho <<<END\n a\n$a\n END;\n",
            "Invalid body indentation level (expecting an indentation level of at least 1)",
            4,
        ),
        (
            "<?php\necho <<<END\n a\n\tb\n END;\n",
            "Invalid indentation - tabs and spaces cannot be mixed",
            4,
        ),
        (
            "<?php\necho <<<END\n a\nb\n END;\n",
            "Invalid body indentation level (expecting an indentation level of at least 1)",
            4,
        ),
        (
            "<?php\n{\n(\n]\n",
            "Unclosed '(' on line 3 does not match ']'",
            4,
        ),
        ("<?php\nfoo(\n\n", "Unclosed '(' on line 2", 4),
        ("<?php\n$a = 1;\n)\n", "Unmatched ')'", 3),
        (
            "<?php\necho <<<END\n\t   a\n\t  b\n\t c\n\t\tEND;\n",
            "Invalid indentation - tabs and spaces cannot be mixed",
            3,
        ),
        (
            "<?php\necho <<<'END'\n a\nb\n END;\n",
            "Invalid body indentation level (expecting an indentation level of at least 1)",
            4,
        ),
        (
            "<?php\necho <<<END\n  a\n \tEND;\n",
            "Invalid indentation - tabs and spaces cannot be mixed",
            3,
        ),
        (
            "<?php\n$a = `\\u{zz}`;\n",
            "Invalid UTF-8 codepoint escape sequence",
            2,
        ),
        (
            "<?php\n$a = \"a $b \\u{zz}\";\n",
            "Invalid UTF-8 codepoint escape sequence",
            2,
        ),
        (
            "<?php\n$a = <<<X\n  a\n  \\u{zz}\n  X;\n",
            "Invalid UTF-8 codepoint escape sequence",
            4,
        ),
        ("<?php\n[\n", "Unclosed '[' on line 2", 3),
        ("<?php\n#[A\n", "Unclosed '[' on line 2", 3),
        ("<?php\n\"${a\n", "Unclosed '{' on line 2", 3),
        ("<?php\n{\n", "Unclosed '{' on line 2", 3),
        ("<?php {", "Unclosed '{'", 1),
    ];
    let mut failures = Vec::new();
    for &(src, msg, line) in cases {
        let got = first_error(src);
        if got.as_ref().map(|(m, l)| (m.as_str(), *l)) != Some((msg, line)) {
            failures.push(format!("{src:?}: expected ({msg:?}, {line}), got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn valid_code_has_no_errors() {
    for src in [
        "<?php echo <<<END\n  a\n   b\n\n  END;\n",
        "<?php echo <<<END\n  $a\n  END;\n",
        "<?php echo 0777 + 0o17 + 0x1F + 0b11 + 1_000;",
        "<?php $a = \"\\u{1F600}\";",
        "<?php function f() { return [1, (2)]; } ?>html",
    ] {
        assert_eq!(first_error(src), None, "{src:?}");
    }
}

fn values(src: &str) -> Vec<(TokenKind, Option<TokenValue>)> {
    let mut lx = parser_lexer(src.as_bytes());
    let mut out = Vec::new();
    while let Some(t) = lx.next_token() {
        if !t.kind.is_trivia() {
            out.push((t.kind, lx.take_value()));
        }
    }
    out
}

#[test]
fn parser_mode_values() {
    use TokenKind as T;
    let s = |b: &[u8]| Some(TokenValue::Str(b.to_vec()));
    assert_eq!(
        values("<?php 'a\\'b' \"c\\td\" 0x10 1.5 9223372036854775808 ?>\n"),
        vec![
            (T::ConstantEncapsedString, s(b"a'b")),
            (T::ConstantEncapsedString, s(b"c\td")),
            (T::LNumber, Some(TokenValue::Int(16))),
            (T::DNumber, Some(TokenValue::Float(1.5))),
            (
                T::DNumber,
                Some(TokenValue::Float(9_223_372_036_854_775_808.0))
            ),
            (T::Semicolon, None),
        ]
    );
    assert_eq!(
        values("<?php <<<EOT\n    a\\n\n      b $x\n    EOT;"),
        vec![
            (T::StartHeredoc, None),
            (T::EncapsedAndWhitespace, s(b"a\n\n  b ")),
            (T::Variable, None),
            (T::EncapsedAndWhitespace, s(b"")),
            (T::EndHeredoc, None),
            (T::Semicolon, None),
        ]
    );
    assert_eq!(
        values("<?php <<<'EOT'\n    a\\n\n      b $x\n    EOT;"),
        vec![
            (T::StartHeredoc, None),
            (T::EncapsedAndWhitespace, s(b"a\\n\n  b $x")),
            (T::EndHeredoc, None),
            (T::Semicolon, None),
        ]
    );
    assert_eq!(
        values("<?php \"$a[0] $a[12345678901234567890] $a[0x1]\";"),
        vec![
            (T::DoubleQuote, None),
            (T::Variable, None),
            (T::LBracket, None),
            (T::NumString, Some(TokenValue::Int(0))),
            (T::RBracket, None),
            (T::EncapsedAndWhitespace, s(b" ")),
            (T::Variable, None),
            (T::LBracket, None),
            (T::NumString, s(b"12345678901234567890")),
            (T::RBracket, None),
            (T::EncapsedAndWhitespace, s(b" ")),
            (T::Variable, None),
            (T::LBracket, None),
            (T::NumString, s(b"0x1")),
            (T::RBracket, None),
            (T::DoubleQuote, None),
            (T::Semicolon, None),
        ]
    );
}

#[test]
fn parser_mode_tags() {
    use TokenKind as T;
    let v: Vec<TokenKind> = values("x<?= 1 ?>y").into_iter().map(|x| x.0).collect();
    assert_eq!(
        v,
        vec![
            T::InlineHtml,
            T::Echo,
            T::LNumber,
            T::Semicolon,
            T::InlineHtml
        ]
    );
}

#[test]
fn warnings() {
    let mut lx = parser_lexer(b"<?php\n(integer) $a;\n\"\n\\400\";");
    while lx.next_token().is_some() {}
    let w = lx.warnings();
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].kind, WarningKind::Deprecated);
    assert_eq!(
        w[0].message,
        "Non-canonical cast (integer) is deprecated, use the (int) cast instead"
    );
    assert_eq!(w[0].line, 2);
    assert_eq!(w[1].kind, WarningKind::CompileWarning);
    assert_eq!(
        w[1].message,
        "Octal escape sequence overflow \\400 is greater than \\377"
    );
    assert_eq!(w[1].line, 4);
}

#[test]
fn error_line_of_tokens() {
    // A syntax error is reported on the line the scanner is on after the
    // offending token, i.e. its end line.
    let mut lx = parser_lexer(b"<?php\n1 \"a\nb\";");
    let toks: Vec<_> = std::iter::from_fn(|| lx.next_token())
        .filter(|t| !t.kind.is_trivia())
        .collect();
    assert_eq!(toks[1].kind, TokenKind::ConstantEncapsedString);
    assert_eq!(toks[1].span.line, 2);
    assert_eq!(toks[1].end_line, 3);
}
