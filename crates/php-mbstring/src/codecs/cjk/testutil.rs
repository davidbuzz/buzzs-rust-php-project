#![allow(dead_code)]

//! Test helpers shared by the CJK codec tests: mapping-file loading, oracle
//! fixture replay, and a tiny driver for the legacy streaming filters.

use std::path::PathBuf;

use crate::convert::{decode_all, fast_convert, ConvertBuf, ErrorMode, BAD_INPUT};
use crate::encoding::{self, Encoding};
use crate::legacy::{Filter, Sink, Vtbl};

/// Path of a file in php-src's `ext/mbstring/tests/data/` directory, if the
/// reference checkout is present.
pub fn data_file(name: &str) -> Option<String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/php-src/ext/mbstring/tests/data")
        .join(name);
    let bytes = std::fs::read(p).ok()?;
    Some(bytes.iter().map(|&b| char::from(b)).collect())
}

/// Parse `0xHEX` tokens: for each non-comment line, the leading
/// whitespace-separated tokens that start with `0x` (or `U+`), each parsed as
/// a hex number. A token like `0x304B+0x309A` yields several numbers, so a
/// line becomes `Vec<Vec<u64>>` (token -> numbers).
pub fn mapping_lines(text: &str) -> Vec<Vec<Vec<u64>>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut toks = Vec::new();
        for tok in line.split_whitespace() {
            let parts: Option<Vec<u64>> = tok
                .split('+')
                .filter(|p| !p.is_empty() && *p != "U")
                .map(|p| {
                    let h = p
                        .trim_start_matches("0x")
                        .trim_start_matches("0X")
                        .trim_start_matches("U+");
                    u64::from_str_radix(h, 16).ok()
                })
                .collect();
            match parts {
                Some(v) if !v.is_empty() => toks.push(v),
                _ => break,
            }
        }
        if !toks.is_empty() {
            out.push(toks);
        }
    }
    out
}

/// Bytes of a mapping-file character code, big-endian without leading zero
/// bytes (as `readConversionTable` in `encoding_tests.inc` does).
pub fn code_bytes(code: u64) -> Vec<u8> {
    let b = code.to_be_bytes();
    let first = b.iter().position(|&x| x != 0).unwrap_or(7);
    b[first..].to_vec()
}

/// Look up an encoding by name, panicking if it is unknown.
pub fn enc(name: &str) -> &'static Encoding {
    encoding::by_name(name.as_bytes()).unwrap_or_else(|| panic!("unknown encoding {name}"))
}

/// Decode `input` in 128-code-point chunks.
pub fn decode(e: &'static Encoding, input: &[u8]) -> Vec<u32> {
    decode_all(input, e)
}

/// Decode with a given buffer size (exercises the `out < limit` paths).
pub fn decode_bufsize(e: &'static Encoding, input: &[u8], bufsize: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = vec![0u32; bufsize];
    let mut state = 0u32;
    let mut rest = input;
    while !rest.is_empty() {
        let n = (e.to_wchar)(&mut rest, &mut buf, &mut state);
        assert!(n <= bufsize);
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// Encode `input` in one call (`end = true`), substituting `?` for errors.
/// Returns (bytes, error count).
pub fn encode(e: &'static Encoding, input: &[u32]) -> (Vec<u8>, u32) {
    let mut buf = ConvertBuf::new(input.len(), u32::from(b'?'), ErrorMode::Char);
    (e.from_wchar)(input, &mut buf, true);
    let errors = buf.errors;
    (buf.into_bytes(), errors)
}

/// Encode code point by code point (each call with `end = false`, then a
/// final empty call with `end = true`), to exercise state kept between calls.
pub fn encode_split(e: &'static Encoding, input: &[u32]) -> Vec<u8> {
    let mut buf = ConvertBuf::new(input.len(), u32::from(b'?'), ErrorMode::Char);
    for w in input {
        (e.from_wchar)(std::slice::from_ref(w), &mut buf, false);
    }
    (e.from_wchar)(&[], &mut buf, true);
    buf.into_bytes()
}

/// `php_mb_check_encoding`: use the encoding's `check` if it has one,
/// otherwise decode (first 8 code points, then 128 at a time) and look for
/// [`BAD_INPUT`].
pub fn check(e: &'static Encoding, input: &[u8]) -> bool {
    if let Some(f) = e.check {
        return f(input);
    }
    let mut buf = [0u32; 128];
    let mut state = 0u32;
    let mut rest = input;
    let n = (e.to_wchar)(&mut rest, &mut buf[..8], &mut state);
    if buf[..n].contains(&BAD_INPUT) {
        return false;
    }
    while !rest.is_empty() {
        let n = (e.to_wchar)(&mut rest, &mut buf, &mut state);
        if buf[..n].contains(&BAD_INPUT) {
            return false;
        }
    }
    true
}

/// A sink collecting every value a legacy filter outputs.
#[derive(Debug, Default)]
pub struct VecSink(pub Vec<i32>);

impl Sink for VecSink {
    fn put(&mut self, c: i32) {
        self.0.push(c);
    }
}

/// Run a legacy filter over `input` (one value per call), then flush it.
pub fn run_filter(vtbl: &Vtbl, input: &[i32]) -> Vec<i32> {
    run_filter_with(vtbl, input, ErrorMode::Char, u32::from(b'?')).0
}

/// Like [`run_filter`] with a specific illegal mode / substitute character;
/// also returns the filter's illegal character count.
pub fn run_filter_with(
    vtbl: &Vtbl,
    input: &[i32],
    mode: ErrorMode,
    substchar: u32,
) -> (Vec<i32>, usize) {
    let mut f = Filter::new(vtbl);
    f.illegal_mode = mode;
    f.illegal_substchar = substchar;
    let mut sink = VecSink::default();
    for &c in input {
        f.feed(c, &mut sink);
    }
    f.flush(&mut sink);
    (sink.0, f.num_illegalchar)
}

/// Legacy-decode `bytes` with the encoding's `input_filter`.
pub fn legacy_decode(e: &'static Encoding, bytes: &[u8]) -> Vec<u32> {
    let vtbl = e.input_filter.expect("encoding has no input_filter");
    #[allow(clippy::cast_sign_loss)]
    run_filter(
        vtbl,
        &bytes.iter().map(|&b| i32::from(b)).collect::<Vec<_>>(),
    )
    .into_iter()
    .map(|c| c as u32)
    .collect()
}

/// Legacy-encode `wchars` with the encoding's `output_filter`.
pub fn legacy_encode(e: &'static Encoding, wchars: &[u32]) -> Vec<u8> {
    let vtbl = e.output_filter.expect("encoding has no output_filter");
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    run_filter(vtbl, &wchars.iter().map(|&w| w as i32).collect::<Vec<_>>())
        .into_iter()
        .map(|c| c as u8)
        .collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex in fixture"))
        .collect()
}

/// Path of a fixture under `tests/fixtures/cjk/`.
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cjk")
        .join(name)
}

/// Replay an oracle fixture generated with `tests/fixtures/cjk/oracle.php`.
///
/// Each line is tab-separated, either
/// `conv <from> <to> <mode> <in-hex> <out-hex> <errors>` (an
/// `mb_convert_encoding` call; `<mode>` is `none`, `long`, `entity`, or the
/// hex code point of the substitute character; `<errors>` is the increase
/// of `mb_get_info('illegal_chars')`), or
/// `check <enc> <in-hex> <0|1>` (an `mb_check_encoding` call).
/// Returns the number of cases checked; panics listing all mismatches.
pub fn replay_oracle(name: &str) -> usize {
    let text = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("cannot read fixture {name}: {e}"));
    let mut failures = Vec::new();
    let mut n = 0;
    for (lineno, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        n += 1;
        match f[0] {
            "conv" => {
                let (from, to) = (enc(f[1]), enc(f[2]));
                let (mode, subst) = match f[3] {
                    "none" => (ErrorMode::None, u32::from(b'?')),
                    "long" => (ErrorMode::Long, u32::from(b'?')),
                    "entity" => (ErrorMode::Entity, u32::from(b'?')),
                    h => (
                        ErrorMode::Char,
                        u32::from_str_radix(h, 16).expect("bad substitute"),
                    ),
                };
                let input = unhex(f[4]);
                let expected = unhex(f[5]);
                let exp_errors: u32 = f[6].parse().expect("bad error count");
                let (out, errors) = fast_convert(&input, from, to, subst, mode);
                if out != expected || errors != exp_errors {
                    failures.push(format!(
                        "line {}: {} -> {} [{}] in={} expected={} ({exp_errors} errors) got={} ({errors} errors)",
                        lineno + 1,
                        f[1],
                        f[2],
                        f[3],
                        f[4],
                        f[5],
                        hex(&out)
                    ));
                }
            }
            "check" => {
                let e = enc(f[1]);
                let input = unhex(f[2]);
                let expected = f[3] == "1";
                if check(e, &input) != expected {
                    failures.push(format!(
                        "line {}: check {} {} expected {}",
                        lineno + 1,
                        f[1],
                        f[2],
                        f[3]
                    ));
                }
            }
            other => panic!("unknown fixture record {other:?} at line {}", lineno + 1),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {n} oracle cases failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(50)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    n
}

/// Lower-case hex of `b`.
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}
