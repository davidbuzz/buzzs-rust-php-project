//! `zend.multibyte`: mbstring's script-encoding support
//! (`php_mb_zend_encoding_*` callbacks of `zend_multibyte.c`).
//!
//! The parser owns script detection, declaration handling and scanner filters.
//! Supply Zend's configured internal encoding and current substitution state.
//! `mb_internal_encoding()` changes mbstring functions' current encoding, not
//! the configured encoding exposed by `php_mb_zend_internal_encoding_getter`.

use php_parser::ScriptEncodingOptions;
use php_vm::ext::{ModuleBuilder, ScriptEncodingSettings, VmResult};
use php_vm::Vm;

use crate::state;

/// Installs the request-state provider without replacing other source filters.
pub(crate) fn install(vm: &mut Vm) {
    vm.set_script_encoding_provider(settings);
    if vm.ini_bool("zend.multibyte") {
        let value = vm.ini_string("zend.script_encoding").unwrap_or_default();
        let (_, diagnostics) = parse_list(&value);
        let headers_sent = vm.headers_sent;
        for diagnostic in diagnostics {
            let _ = vm.error_at(
                2,
                &format!("PHP Startup: {}", diagnostic.message),
                &php_runtime::ZStr::from("Unknown"),
                0,
            );
        }
        vm.headers_sent = headers_sent;
    }
}

fn parse_list(
    value: &[u8],
) -> (
    Option<Vec<&'static php_mbstring::Encoding>>,
    Vec<php_mbstring::Diagnostic>,
) {
    state::with(|st| {
        let mut parser = st.clone();
        let encodings = parser.parse_encoding_list(value, 0).ok();
        (encodings, parser.take_diagnostics())
    })
}

pub(crate) fn register_hooks(module: &mut ModuleBuilder<'_>) {
    module.ini_on_modify("zend.script_encoding", update_script_encoding);
}

fn update_script_encoding(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    if !vm.ini_bool("zend.multibyte") {
        return Ok(false);
    }
    let (encodings, diagnostics) = parse_list(value);
    for diagnostic in diagnostics {
        vm.error(2, &diagnostic.message)?;
    }
    Ok(encodings.is_some())
}

fn settings(vm: &Vm) -> ScriptEncodingSettings {
    let value = vm.ini_string("zend.script_encoding").unwrap_or_default();
    let (script_encodings, _) = parse_list(&value);
    state::with(|st| ScriptEncodingSettings {
        script_encodings: script_encodings.unwrap_or_default(),
        options: ScriptEncodingOptions {
            internal_encoding: Some(st.script_internal_encoding()),
            replacement_char: st.illegal_substchar(),
            error_mode: st.illegal_mode(),
            ..ScriptEncodingOptions::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::{Value, ZStr};
    use php_vm::ext::IncludeOpen;
    use php_vm::VmOptions;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn vm(settings: &[(&str, &str)]) -> (Vm, Sink) {
        let sink = Sink::default();
        let opts = VmOptions {
            ini: settings
                .iter()
                .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        };
        let vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &[&crate::Mbstring]);
        (vm, sink)
    }

    fn run(source: &[u8], settings: &[(&str, &str)]) -> Vec<u8> {
        let (mut vm, sink) = vm(settings);
        assert_eq!(vm.run_script(source, "encoding.php"), 0);
        let output = sink.0.borrow().clone();
        output
    }

    fn open(_vm: &mut Vm, _path: &[u8], _kind: &str) -> VmResult<IncludeOpen> {
        Ok(IncludeOpen::Opened {
            source: b"<?php declare(encoding='UTF-8'); echo '\xC3\xA9'; echo '\xE2\x82\xAC';"
                .to_vec(),
            opened_path: ZStr::from("encoded-include.php"),
        })
    }

    #[test]
    fn main_script_uses_parser_filters_without_double_conversion() {
        let source = b"<?php declare(encoding='SJIS'); echo \"\x95\x5C\";";
        assert_eq!(
            run(
                source,
                &[("zend.multibyte", "1"), ("internal_encoding", "SJIS")]
            ),
            b"\x95\x5C"
        );
        assert_eq!(run(source, &[("zend.multibyte", "1")]), "表".as_bytes());
    }

    #[test]
    fn includes_use_encoding_ini_and_live_substitution() {
        let (mut vm, sink) = vm(&[("zend.multibyte", "1")]);
        vm.set_include_opener(open);
        let source = b"<?php ini_set('internal_encoding', 'ISO-8859-1'); mb_substitute_character(33); include 'encoded.php';";
        assert_eq!(vm.run_script(source, "encoding.php"), 0);
        assert_eq!(sink.0.borrow().as_slice(), b"\xE9!");
    }

    #[test]
    fn eval_uses_configured_internal_encoding_not_script_ini() {
        let source = b"<?php ini_set('internal_encoding', 'SJIS'); eval('echo \"\x95\x5C\";');";
        assert_eq!(
            run(
                source,
                &[("zend.multibyte", "1"), ("zend.script_encoding", "UTF-8")]
            ),
            b"\x95\x5C"
        );
    }

    #[test]
    fn eval_does_not_detect_unicode_bom() {
        let (vm, _) = vm(&[("zend.multibyte", "1")]);
        assert!(vm
            .parse_source(b"\xEF\xBB\xBFecho 'ok';", true, false)
            .is_err());
    }

    #[test]
    fn top_level_eval_uses_internal_encoding_and_scripting_mode() {
        let (mut vm, sink) = vm(&[
            ("zend.multibyte", "1"),
            ("internal_encoding", "SJIS"),
            ("zend.script_encoding", "UTF-8"),
        ]);
        assert!(vm
            .run_eval(b"echo \"\x95\x5C\";", &ZStr::from("Command line code"))
            .is_ok());
        vm.flush_stdout();
        assert_eq!(sink.0.borrow().as_slice(), b"\x95\x5C");
    }

    #[test]
    fn file_input_uses_script_encoding_and_detect_unicode_ini() {
        let mut source = b"\xFF\xFE".to_vec();
        source.extend("<?php echo 'é';".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            run(
                &source,
                &[("zend.multibyte", "1"), ("zend.script_encoding", "SJIS")]
            ),
            "é".as_bytes()
        );
        assert_eq!(
            run(
                b"\xEF\xBB\xBF<?php echo 'ok';",
                &[("zend.multibyte", "1"), ("zend.detect_unicode", "0")],
            ),
            b"\xEF\xBB\xBFok"
        );
    }

    #[test]
    fn parser_settings_use_configured_encoding_and_substitution_ini_hooks() {
        let (mut vm, _) = vm(&[("zend.multibyte", "1")]);
        vm.call_value(
            &Value::Str(ZStr::from("ini_set")),
            vec![
                Value::Str(ZStr::from("internal_encoding")),
                Value::Str(ZStr::from("ISO-8859-1")),
            ],
        )
        .unwrap();
        vm.call_value(
            &Value::Str(ZStr::from("ini_set")),
            vec![
                Value::Str(ZStr::from("mbstring.substitute_character")),
                Value::Str(ZStr::from("0x21")),
            ],
        )
        .unwrap();
        let program = vm
            .parse_source(
                b"<?php declare(encoding='UTF-8'); echo '\xE2\x82\xAC';",
                false,
                false,
            )
            .unwrap();
        let php_parser::ast::StmtKind::Echo(values) = &program.statements[1].kind else {
            panic!("expected echo");
        };
        assert_eq!(
            values[0].kind,
            php_parser::ast::ExprKind::String(b"!".to_vec())
        );
    }

    #[test]
    fn reflection_preserves_compile_time_encoding_after_runtime_change() {
        let source = b"<?php declare(encoding='SJIS'); function f($x='\x82\xA0') { static $v='\x82\xA0'; } ini_set('internal_encoding', 'ISO-8859-1'); $r = new ReflectionFunction('f'); echo $r->getParameters()[0]->getDefaultValue(); echo $r->getStaticVariables()['v'];";
        assert_eq!(run(source, &[("zend.multibyte", "1")]), "ああ".as_bytes());
    }

    #[test]
    fn reflection_enum_doc_comments_use_original_ast_without_disk_reparse() {
        let source = b"<?php declare(encoding='SJIS'); enum E { /** \x82\xA0 */ case A; } ini_set('internal_encoding', 'ISO-8859-1'); echo (new ReflectionEnumUnitCase('E', 'A'))->getDocComment();";
        assert_eq!(
            run(source, &[("zend.multibyte", "1")]),
            "/** あ */".as_bytes()
        );
    }

    #[test]
    fn multibyte_off_leaves_source_bytes_unchanged() {
        assert_eq!(run(b"<?php echo '\xE9';", &[]), b"\xE9");
    }

    #[test]
    fn mb_internal_encoding_does_not_change_scanner_encoding() {
        let (mut vm, _) = vm(&[("zend.multibyte", "1")]);
        vm.call_value(
            &Value::Str(ZStr::from("mb_internal_encoding")),
            vec![Value::Str(ZStr::from("SJIS"))],
        )
        .unwrap();
        assert!(vm.parse_source(b"echo \"\x95\x5C\";", true, false).is_err());
        assert_eq!(
            settings(&vm).options.internal_encoding,
            Some(php_mbstring::encoding::utf8())
        );
    }

    #[test]
    fn invalid_script_encoding_ini_retains_previous_list() {
        let (mut vm, _) = vm(&[("zend.multibyte", "1"), ("zend.script_encoding", "SJIS")]);
        let result = vm
            .call_value(
                &Value::Str(ZStr::from("ini_set")),
                vec![
                    Value::Str(ZStr::from("zend.script_encoding")),
                    Value::Str(ZStr::from("pass")),
                ],
            )
            .unwrap();
        assert!(matches!(result, Value::Bool(false)));
        assert_eq!(
            vm.ini_string("zend.script_encoding"),
            Some(b"SJIS".to_vec())
        );
        assert_eq!(
            settings(&vm).script_encodings,
            vec![php_mbstring::encoding::by_name(b"SJIS").unwrap()]
        );
    }

    #[test]
    fn script_encoding_auto_uses_existing_language_list_parser() {
        let (vm, _) = vm(&[
            ("zend.multibyte", "1"),
            ("mbstring.language", "Japanese"),
            ("zend.script_encoding", "auto"),
        ]);
        let actual = settings(&vm).script_encodings;
        let expected = state::with(|st| st.clone().parse_encoding_list(b"auto", 0).unwrap());
        assert_eq!(actual, expected);
        assert!(!actual.is_empty());
    }
}
