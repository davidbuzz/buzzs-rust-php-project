//! INI parser tests.  `tests_cases.rs` holds inputs and the results PHP
//! produced for them (rendered compactly by a small PHP script mirroring
//! [`render`]); constants `BIRD`, `TEST_CONST`, `NUM` and environment
//! variables `FOO=bar`, `EMPTYV=` were defined.

#[path = "tests_cases.rs"]
mod cases;

use std::fmt::Write as _;

use super::*;

/// `json_encode()` of a string: `/` escaped, non-ASCII UTF-8 as `\uXXXX`.
fn json(s: &[u8]) -> String {
    let mut o = String::from("\"");
    for ch in String::from_utf8_lossy(s).chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '/' => o.push_str("\\/"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) >= 0x7f => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(o, "\\u{unit:04x}");
                }
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn render_scalar(v: &IniScalar) -> String {
    match v {
        IniScalar::Str(s) => format!("s{}", json(s)),
        IniScalar::Long(l) => format!("i{l}"),
        IniScalar::Double(d) => format!(
            "d{}",
            crate::fmt::format_double(*d, -1, crate::fmt::DoubleMode::ZeroFraction)
        ),
        IniScalar::Bool(b) => format!("b{}", u8::from(*b)),
        IniScalar::Null => "n".into(),
    }
}

fn render(m: &IniMap) -> String {
    let parts: Vec<String> = m
        .iter()
        .map(|(k, v)| {
            let key = match k {
                IniKey::Int(i) => format!("i{i}"),
                IniKey::Str(s) => format!("s{}", json(s)),
            };
            let val = match v {
                IniValue::Scalar(s) => render_scalar(s),
                IniValue::Array(a) => render(a),
            };
            format!("{key}:{val}")
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

type Lookup = fn(&[u8]) -> Option<Vec<u8>>;

fn hooks() -> FnHooks<Lookup, Lookup> {
    FnHooks {
        constant: |n: &[u8]| match n {
            b"BIRD" => Some(b"Humming bird".to_vec()),
            b"TEST_CONST" => Some(b"test const value".to_vec()),
            b"NUM" => Some(b"7".to_vec()),
            b"TEST_CONSTANT" => Some(b"this_is_test_constant".to_vec()),
            // PHP 8.3 values (the oracle); master's E_ALL is 30719.
            b"E_ERROR" => Some(b"1".to_vec()),
            b"E_WARNING" => Some(b"2".to_vec()),
            b"E_PARSE" => Some(b"4".to_vec()),
            b"E_NOTICE" => Some(b"8".to_vec()),
            b"E_CORE_ERROR" => Some(b"16".to_vec()),
            b"E_CORE_WARNING" => Some(b"32".to_vec()),
            b"E_COMPILE_ERROR" => Some(b"64".to_vec()),
            b"E_COMPILE_WARNING" => Some(b"128".to_vec()),
            b"E_USER_ERROR" => Some(b"256".to_vec()),
            b"E_USER_WARNING" => Some(b"512".to_vec()),
            b"E_USER_NOTICE" => Some(b"1024".to_vec()),
            b"E_STRICT" => Some(b"2048".to_vec()),
            b"E_RECOVERABLE_ERROR" => Some(b"4096".to_vec()),
            b"E_DEPRECATED" => Some(b"8192".to_vec()),
            b"E_USER_DEPRECATED" => Some(b"16384".to_vec()),
            b"E_ALL" => Some(b"32767".to_vec()),
            _ => None,
        },
        variable: |n: &[u8]| match n {
            b"FOO" => Some(b"bar".to_vec()),
            b"EMPTYV" | b"emptyval" => Some(Vec::new()),
            b"basicval" => Some(b"FUBAR_VARIABLE".to_vec()),
            b"basicqval" => Some(b"FUBAR_QUOTES_VARIABLE".to_vec()),
            b"falsyval" => Some(b"false".to_vec()),
            _ => None,
        },
    }
}

fn run(input: &[u8], sections: bool, mode: u8) -> String {
    let mut h = hooks();
    let mode = ScannerMode::from_php(i64::from(mode)).unwrap();
    match parse_ini_string(input, sections, mode, &mut h) {
        Ok(m) => render(&m),
        Err(d) => {
            assert_eq!(d.level, ErrorLevel::WARNING);
            assert!(!d.docref);
            format!("ERR:{}", d.message)
        }
    }
}

#[test]
fn oracle_cases() {
    let mut failures = Vec::new();
    for &(input, sections, mode, expected) in cases::CASES {
        let got = run(input, sections, mode);
        if got != expected {
            failures.push(format!(
                "input {:?} sections={sections} mode={mode}\n  expected {expected}\n  got      {got}",
                String::from_utf8_lossy(input)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn parse_ini_string_001() {
    // ext/standard/tests/general_functions/parse_ini_string_001.phpt (excerpt).
    let ini = b"[Constans]\none = 1\nanimal = BIRD\n10 = Ten\n[date]\ndate =\n[Non-alphanumerics_as_values]\nNon_alpha1 = ;\nNon_alpha12 = \\\nNon_alpha10_quotes = \"=\"\n[Newlines_in_Values]\nString2 = \"\\nHello, world\n             Good Morning\\n\"\n[ReservedKeys_as_Values]\nKey3 = yEs\nKey17 = nuLL\n";
    assert_eq!(
        run(ini, false, 0),
        "{s\"one\":s\"1\",s\"animal\":s\"Humming bird\",i10:s\"Ten\",s\"date\":s\"\",s\"Non_alpha1\":s\"\",s\"Non_alpha12\":s\"\\\\\",s\"Non_alpha10_quotes\":s\"=\",s\"String2\":s\"\\\\nHello, world\\n             Good Morning\\\\n\",s\"Key3\":s\"1\",s\"Key17\":s\"\"}"
    );
    let with_sections = run(ini, true, 0);
    assert!(with_sections.starts_with("{s\"Constans\":{s\"one\":s\"1\""));
}

#[test]
fn nul_truncates_string_but_not_file() {
    let mut h = EnvHooks;
    let m = parse_ini_string(b"a=1\0b=2", false, ScannerMode::Normal, &mut h).unwrap();
    assert_eq!(m.len(), 1);
    let e = parse_ini_contents(
        b"a=1\n=\n",
        "/tmp/x.ini",
        false,
        ScannerMode::Normal,
        &mut h,
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        "syntax error, unexpected '=' in /tmp/x.ini on line 2\n"
    );
}

#[derive(Default)]
struct Recorder(Vec<String>);

impl IniParserCallback for Recorder {
    fn section(&mut self, name: &[u8]) {
        self.0
            .push(format!("section {}", String::from_utf8_lossy(name)));
    }
    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        self.0.push(format!(
            "entry {} {:?}",
            String::from_utf8_lossy(key),
            value
        ));
    }
    fn pop_entry(&mut self, key: &[u8], value: IniScalar, offset: &[u8]) {
        self.0.push(format!(
            "pop {}[{}] {:?}",
            String::from_utf8_lossy(key),
            String::from_utf8_lossy(offset),
            value
        ));
    }
}

#[test]
fn callback_sees_bare_labels_and_partial_results() {
    let mut r = Recorder::default();
    let mut h = EnvHooks;
    let res = parse_ini(
        b"[s]\nextension\na[x]=1\nb=2\nc==",
        "f.ini",
        ScannerMode::Normal,
        &mut h,
        &mut r,
    );
    assert_eq!(
        r.0,
        vec![
            "section s".to_string(),
            "entry extension None".into(),
            "pop a[x] Str([49])".into(),
            "entry b Some(Str([50]))".into(),
            "entry c Some(Str([]))".into(),
        ]
    );
    assert_eq!(
        res.unwrap_err().message,
        "syntax error, unexpected '=' in f.ini on line 5\n"
    );
}

#[test]
fn env_hooks_read_process_environment() {
    let mut h = EnvHooks;
    assert_eq!(h.variable(b""), None);
    assert_eq!(h.variable(b"A=B"), None);
    assert_eq!(h.constant(b"PHP_VERSION"), None);
    let path = std::env::var_os("PATH").is_some();
    assert_eq!(h.variable(b"PATH").is_some(), path);
}

#[test]
fn scanner_mode_from_php() {
    assert_eq!(ScannerMode::from_php(0), Some(ScannerMode::Normal));
    assert_eq!(ScannerMode::from_php(1), Some(ScannerMode::Raw));
    assert_eq!(ScannerMode::from_php(2), Some(ScannerMode::Typed));
    assert_eq!(ScannerMode::from_php(3), None);
}
