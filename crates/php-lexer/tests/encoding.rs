// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use php_lexer::{LexMode, Lexer, LexerOptions, ScriptEncodingOptions, TokenKind, TokenValue};
use php_mbstring::convert::ErrorMode;
use php_mbstring::encoding::{by_name, utf8, Encoding};

fn encoding(name: &[u8]) -> &'static Encoding {
    by_name(name).expect("fixture encoding exists")
}

fn options() -> LexerOptions {
    LexerOptions {
        mode: LexMode::Parser,
        ..LexerOptions::default()
    }
}

fn string_value(lexer: &mut Lexer<'_>) -> Vec<u8> {
    while let Some(token) = lexer.next_token() {
        if token.kind == TokenKind::ConstantEncapsedString {
            let Some(TokenValue::Str(value)) = lexer.take_value() else {
                panic!("expected a string value");
            };
            return value;
        }
    }
    panic!("expected a string token");
}

#[test]
fn unicode_boms_precede_script_encoding() {
    let fallback = [encoding(b"SJIS")];
    for (name, bom) in [
        (&b"UTF-8"[..], &b"\xEF\xBB\xBF"[..]),
        (&b"UTF-16LE"[..], &b"\xFF\xFE"[..]),
        (&b"UTF-16BE"[..], &b"\xFE\xFF"[..]),
        (&b"UTF-32LE"[..], &b"\xFF\xFE\0\0"[..]),
        (&b"UTF-32BE"[..], &b"\0\0\xFE\xFF"[..]),
    ] {
        let mut src = bom.to_vec();
        let body = php_mbstring::convert::fast_convert(
            "<?php echo 'é';".as_bytes(),
            utf8(),
            encoding(name),
            0x3F,
            ErrorMode::Char,
        )
        .0;
        src.extend_from_slice(&body);
        let config = ScriptEncodingOptions {
            script_encodings: &fallback,
            ..ScriptEncodingOptions::default()
        };
        let mut lexer = Lexer::with_script_encoding(&src, options(), &config);
        let mut open = lexer.next_token().unwrap();
        if name == b"UTF-8" {
            // UTF-8 matches the internal encoding, so no input filter runs
            // and Zend scans the unstripped buffer: the BOM is inline HTML.
            assert_eq!(open.kind, TokenKind::InlineHtml);
            assert_eq!(lexer.token_text(open).as_ref(), bom);
            open = lexer.next_token().unwrap();
            assert_eq!(open.span.start, bom.len());
        } else {
            assert_eq!(open.span.start, 0);
        }
        assert_eq!(open.kind, TokenKind::OpenTag);
        assert_eq!(string_value(&mut lexer), "é".as_bytes());
    }
}

#[test]
fn utf8_bom_is_stripped_only_by_an_input_filter() {
    let src = b"\xEF\xBB\xBF<?php __halt_compiler();";
    // No filter (internal UTF-8): BOM kept, halt offset counts it.
    let mut lexer = Lexer::with_script_encoding(src, options(), &ScriptEncodingOptions::default());
    assert_eq!(lexer.source(), src);
    assert_eq!(lexer.next_token().unwrap().kind, TokenKind::InlineHtml);
    while lexer.next_token().unwrap().kind != TokenKind::Semicolon {}
    assert_eq!(lexer.scanned_file_offset(), src.len());
    // UTF-8 -> ISO-8859-1 input filter: converts the BOM-stripped script.
    let config = ScriptEncodingOptions {
        internal_encoding: Some(encoding(b"ISO-8859-1")),
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(src, options(), &config);
    assert_eq!(lexer.source(), &src[3..]);
    assert_eq!(lexer.next_token().unwrap().kind, TokenKind::OpenTag);
    while lexer.next_token().unwrap().kind != TokenKind::Semicolon {}
    assert_eq!(lexer.scanned_file_offset(), src.len() - 3);
}

#[test]
fn declare_rescan_after_unfiltered_bom_reads_stripped_script() {
    // zend_multibyte_yyinput_again keeps the numeric cursor but switches to
    // the BOM-stripped, converted script, so scanning resumes 3 bytes later.
    let src = b"\xEF\xBB\xBF<?php declare(encoding='ISO-8859-1'); echo 1;";
    let mut lexer = Lexer::with_script_encoding(src, options(), &ScriptEncodingOptions::default());
    assert_eq!(lexer.next_token().unwrap().kind, TokenKind::InlineHtml);
    while lexer.next_token().unwrap().kind != TokenKind::RParen {}
    assert!(lexer.set_script_encoding(encoding(b"ISO-8859-1")));
    assert_eq!(lexer.source(), &src[3..]);
    let next = lexer.next_token().unwrap();
    assert_eq!(next.kind, TokenKind::String);
    assert_eq!(next.text(lexer.source()), b"cho");
}

#[test]
fn nul_detection_handles_short_wide_scripts() {
    let config = ScriptEncodingOptions::default();
    for src in [&b"<\0?\0p\0h\0p\0"[..], &b"\0<\0?\0p\0h\0p"[..]] {
        let mut lexer = Lexer::with_script_encoding(src, options(), &config);
        assert_eq!(lexer.next_token().unwrap().kind, TokenKind::OpenTag);
        assert!(lexer.next_token().is_none());
    }
}

#[test]
fn unicode_detection_can_be_disabled() {
    let config = ScriptEncodingOptions {
        detect_unicode: false,
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"\xEF\xBB\xBF<?php echo 1;", options(), &config);
    let token = lexer.next_token().unwrap();
    assert_eq!(token.kind, TokenKind::InlineHtml);
    assert_eq!(lexer.token_text(token).as_ref(), b"\xEF\xBB\xBF");
}

#[test]
fn halt_payload_does_not_trigger_unicode_detection() {
    let src = b"<?php __HaLt_CoMpIlEr \t( \n) ;\0\xFF";
    let mut lexer = Lexer::with_script_encoding(src, options(), &ScriptEncodingOptions::default());
    assert_eq!(lexer.source(), src);
    assert_eq!(lexer.next_token().unwrap().kind, TokenKind::OpenTag);
}

#[test]
fn script_encoding_list_uses_existing_detector() {
    let candidates = [utf8(), encoding(b"SJIS")];
    let config = ScriptEncodingOptions {
        script_encodings: &candidates,
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"<?php echo '\x82\xA0';", options(), &config);
    assert_eq!(string_value(&mut lexer), "あ".as_bytes());
}

#[test]
fn unsafe_encoding_protects_trailing_backslash() {
    let sjis = encoding(b"SJIS");
    let scripts = [sjis];
    let config = ScriptEncodingOptions {
        script_encodings: &scripts,
        internal_encoding: Some(sjis),
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"<?php echo \"\x95\x5C\";", options(), &config);
    assert_eq!(string_value(&mut lexer), b"\x95\x5C");
    assert!(lexer.error().is_none());
}

#[test]
fn absent_internal_encoding_preserves_script_bytes() {
    let scripts = [encoding(b"SJIS")];
    let config = ScriptEncodingOptions {
        script_encodings: &scripts,
        internal_encoding: None,
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"<?php echo '\x82\xA0';", options(), &config);
    assert_eq!(string_value(&mut lexer), b"\x82\xA0");
}

#[test]
fn output_filter_runs_after_escape_processing() {
    let config = ScriptEncodingOptions {
        internal_encoding: Some(encoding(b"Big5")),
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"<?php echo \"\\xC3\\xA9\";", options(), &config);
    assert!(lexer.set_script_encoding(utf8()));
    assert_eq!(string_value(&mut lexer), b"?");
}

#[test]
fn declaration_rescan_keeps_cursor_and_line() {
    let mut lexer = Lexer::with_script_encoding(
        b"<?php declare(encoding='SJIS'\n); echo '\x82\xA0';",
        options(),
        &ScriptEncodingOptions::default(),
    );
    while lexer.next_token().unwrap().kind != TokenKind::RParen {}
    let offset = lexer.offset();
    let line = lexer.line();
    assert!(lexer.set_script_encoding(encoding(b"SJIS")));
    assert_eq!(lexer.offset(), offset);
    assert_eq!(lexer.line(), line);
    assert_eq!(lexer.next_token().unwrap().kind, TokenKind::Semicolon);
    assert_eq!(string_value(&mut lexer), "あ".as_bytes());
}

#[test]
fn substitution_settings_are_used() {
    let scripts = [encoding(b"SJIS")];
    let config = ScriptEncodingOptions {
        script_encodings: &scripts,
        replacement_char: 0xFFFD,
        ..ScriptEncodingOptions::default()
    };
    let mut lexer = Lexer::with_script_encoding(b"<?php echo '\xFF';", options(), &config);
    assert_eq!(string_value(&mut lexer), "\u{FFFD}".as_bytes());
}

#[test]
fn scanned_offset_maps_back_to_wide_input() {
    let scripts = [encoding(b"UTF-16LE")];
    let config = ScriptEncodingOptions {
        script_encodings: &scripts,
        detect_unicode: false,
        ..ScriptEncodingOptions::default()
    };
    let src: Vec<u8> = "<?php __halt_compiler();"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut lexer = Lexer::with_script_encoding(&src, options(), &config);
    while lexer.next_token().unwrap().kind != TokenKind::Semicolon {}
    // zend_get_scanned_file_offset accepts the incomplete trailing code unit:
    // its replacement character makes the converted prefix the target length.
    assert_eq!(lexer.scanned_file_offset(), src.len() - 1);
}

#[test]
fn scanned_offset_uses_master_prefix_conversion_for_wide_encodings() {
    for (name, width, bom) in [
        (&b"UTF-16LE"[..], 2, &b"\xFF\xFE"[..]),
        (&b"UTF-16BE"[..], 2, &b"\xFE\xFF"[..]),
        (&b"UTF-32LE"[..], 4, &b"\xFF\xFE\0\0"[..]),
        (&b"UTF-32BE"[..], 4, &b"\0\0\xFE\xFF"[..]),
    ] {
        let scripts = [encoding(name)];
        let prefix = "<?php __halt_compiler();";
        let body = php_mbstring::convert::fast_convert(
            format!("{prefix}payload").as_bytes(),
            utf8(),
            scripts[0],
            0x3F,
            ErrorMode::Char,
        )
        .0;
        for detect_unicode in [false, true] {
            let mut src = Vec::new();
            if detect_unicode {
                src.extend_from_slice(bom);
            }
            src.extend_from_slice(&body);
            let config = ScriptEncodingOptions {
                script_encodings: &scripts,
                detect_unicode,
                ..ScriptEncodingOptions::default()
            };
            let mut lexer = Lexer::with_script_encoding(&src, options(), &config);
            while lexer.next_token().unwrap().kind != TokenKind::Semicolon {}
            assert_eq!(
                lexer.scanned_file_offset(),
                prefix.len() * width - (width - 1),
                "{} (BOM: {detect_unicode})",
                String::from_utf8_lossy(name)
            );
        }
    }
}
