// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use php_mbstring::encoding::{by_name, utf8};
use php_parser::ast::{ExprKind, StmtKind};
use php_parser::{parse, parse_with_encoding, ParseOptions, Program, ScriptEncodingOptions};

fn options() -> ParseOptions {
    ParseOptions {
        multibyte: true,
        ..ParseOptions::default()
    }
}

fn echoed_string(program: &Program, statement: usize) -> &[u8] {
    let StmtKind::Echo(values) = &program.statements[statement].kind else {
        panic!("expected echo");
    };
    let ExprKind::String(bytes) = &values[0].kind else {
        panic!("expected string literal");
    };
    bytes
}

#[test]
fn declaration_filters_following_source() {
    let program = parse(
        b"<?php declare(encoding='Shift_JIS'); echo \"\x95\x5C\";",
        &options(),
    )
    .unwrap();
    assert!(program.warnings.is_empty());
    assert_eq!(echoed_string(&program, 1), "表".as_bytes());
}

#[test]
fn declaration_uses_host_internal_encoding() {
    let config = ScriptEncodingOptions {
        internal_encoding: by_name(b"SJIS"),
        ..ScriptEncodingOptions::default()
    };
    let program = parse_with_encoding(
        b"<?php declare(encoding='Shift_JIS'); echo \"\x95\x5C\";",
        &options(),
        &config,
    )
    .unwrap();
    assert_eq!(echoed_string(&program, 1), b"\x95\x5C");
}

#[test]
fn unknown_encoding_warns_without_changing_input() {
    let program = parse(
        b"<?php declare(encoding='missing'\n); echo '\xE9';",
        &options(),
    )
    .unwrap();
    assert_eq!(program.warnings.len(), 1);
    assert_eq!(
        program.warnings[0].message,
        "Unsupported encoding [missing]"
    );
    assert_eq!(program.warnings[0].line, 2);
    assert_eq!(echoed_string(&program, 1), b"\xE9");

    for (literal, expected) in [
        ("123", "123"),
        ("1.0", "1"),
        ("0.0", "0"),
        ("1.5", "1.5"),
        ("1e20", "1.0E+20"),
    ] {
        let src = format!("<?php declare(encoding={literal});");
        let program = parse(src.as_bytes(), &options()).unwrap();
        assert_eq!(
            program.warnings[0].message,
            format!("Unsupported encoding [{expected}]")
        );
    }
    let program = parse(b"<?php declare(encoding=\"missing\\0suffix\");", &options()).unwrap();
    assert_eq!(
        program.warnings[0].message,
        "Unsupported encoding [missing]"
    );
}

#[test]
fn encoding_name_lookup_uses_c_string_alias_semantics() {
    let program = parse(
        b"<?php declare(EnCoDiNg=\"uTf8\\0ignored\"); echo 'ok';",
        &options(),
    )
    .unwrap();
    assert!(program.warnings.is_empty());
    assert_eq!(echoed_string(&program, 1), b"ok");
}

#[test]
fn warnings_survive_subsequent_syntax_and_literal_errors() {
    let error = parse(b"<?php declare(encoding='missing'\n); }", &options()).unwrap_err();
    assert_eq!(error.message, "Unmatched '}'");
    assert_eq!(error.warnings[0].message, "Unsupported encoding [missing]");
    assert_eq!(error.warnings[0].line, 2);

    let error = parse(
        b"<?php declare(encoding='missing', encoding=FOO);",
        &options(),
    )
    .unwrap_err();
    assert_eq!(error.message, "Encoding must be a literal");
    assert_eq!(error.warnings[0].message, "Unsupported encoding [missing]");
}

#[test]
fn duplicate_directives_apply_in_source_order() {
    let program = parse(
        b"<?php declare(encoding='UTF-8', encoding='SJIS'); echo '\x82\xA0';",
        &options(),
    )
    .unwrap();
    assert_eq!(echoed_string(&program, 1), "あ".as_bytes());
}

#[test]
fn multibyte_off_preserves_bom_and_disabled_warning() {
    let config = ScriptEncodingOptions {
        internal_encoding: by_name(b"ISO-8859-1"),
        ..ScriptEncodingOptions::default()
    };
    let opts = ParseOptions {
        syntax_only: true,
        ..ParseOptions::default()
    };
    let program = parse_with_encoding(
        b"\xEF\xBB\xBF<?php declare(encoding='missing'); echo '\xE9';",
        &opts,
        &config,
    )
    .unwrap();
    assert_eq!(
        program.statements[0].kind,
        StmtKind::InlineHtml(b"\xEF\xBB\xBF".to_vec())
    );
    assert_eq!(echoed_string(&program, 2), b"\xE9");
    assert_eq!(
        program.warnings[0].message,
        "declare(encoding=...) ignored because Zend multibyte feature is turned off by settings"
    );
}

#[test]
fn configured_eval_input_and_inline_html_are_converted() {
    let scripts = [utf8()];
    let config = ScriptEncodingOptions {
        script_encodings: &scripts,
        internal_encoding: by_name(b"ISO-8859-1"),
        detect_unicode: false,
        ..ScriptEncodingOptions::default()
    };
    let opts = ParseOptions {
        start_in_scripting: true,
        ..options()
    };
    let program = parse_with_encoding("echo 'é';".as_bytes(), &opts, &config).unwrap();
    assert_eq!(echoed_string(&program, 0), b"\xE9");

    let program =
        parse_with_encoding("é<?php echo 'é'; ?>é".as_bytes(), &options(), &config).unwrap();
    assert_eq!(
        program.statements[0].kind,
        StmtKind::InlineHtml(b"\xE9".to_vec())
    );
    assert_eq!(echoed_string(&program, 1), b"\xE9");
    assert_eq!(
        program.statements.last().unwrap().kind,
        StmtKind::InlineHtml(b"\xE9".to_vec())
    );
}

#[test]
fn filtered_identifier_bytes_and_halt_offset() {
    let program = parse(
        b"<?php declare(encoding='SJIS'); $\x82\xA0 = '\x82\xA0'; __halt_compiler();\0\xFF",
        &options(),
    )
    .unwrap();
    let exported = php_parser::export::export_program(&program);
    assert!(exported
        .windows("あ".len())
        .any(|bytes| bytes == "あ".as_bytes()));
    let expected =
        b"<?php declare(encoding='SJIS'); $\x82\xA0 = '\x82\xA0'; __halt_compiler();".len();
    assert_eq!(program.halt_compiler_offset, Some(expected));
}
