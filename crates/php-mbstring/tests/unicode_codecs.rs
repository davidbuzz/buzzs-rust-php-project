//! Replays oracle fixtures recorded from PHP 8.3 (`/usr/bin/php`) against the
//! Unicode and transfer-encoding codecs: every `mb_convert_encoding` /
//! `mb_check_encoding` call made by the relevant php-src `.phpt` tests
//! (`phpt.tsv`) plus deterministic randomised cases (`fuzz.tsv`).
//! Regenerate with `tests/fixtures/unicode/gen_oracle.py`.

use php_mbstring::convert::{fast_convert, ErrorMode, BAD_INPUT};
use php_mbstring::encoding::{self, Encoding, EncodingNo as N};

const PHPT: &str = include_str!("fixtures/unicode/phpt.tsv");
const FUZZ: &str = include_str!("fixtures/unicode/fuzz.tsv");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Encodings ported so far whose behaviour the fixtures may exercise (the
/// CJK family is tested elsewhere).
fn supported(e: &Encoding) -> bool {
    !matches!(
        e.no,
        N::EucJp
            | N::EucJp2004
            | N::Sjis
            | N::EucJpWin
            | N::SjisDocomo
            | N::SjisKddi
            | N::SjisSb
            | N::SjisMac
            | N::Sjis2004
            | N::Cp932
            | N::SjisWin
            | N::Cp51932
            | N::Jis
            | N::Iso2022Jp
            | N::Iso2022Jp2004
            | N::Iso2022JpKddi
            | N::Iso2022JpMs
            | N::Gb18030
            | N::Gb18030_2022
            | N::EucCn
            | N::Cp936
            | N::EucTw
            | N::Big5
            | N::Cp950
            | N::EucKr
            | N::Iso2022Kr
            | N::Uhc
            | N::Hz
            | N::Utf8Docomo
            | N::Utf8KddiA
            | N::Utf8KddiB
            | N::Utf8Sb
            | N::Cp50220
            | N::Cp50221
            | N::Cp50222
    )
}

fn subst(s: &str) -> (ErrorMode, u32) {
    let (mode, ch) = s.split_once(':').expect("subst");
    let mode = match mode {
        "char" => ErrorMode::Char,
        "none" => ErrorMode::None,
        "long" => ErrorMode::Long,
        "entity" => ErrorMode::Entity,
        other => panic!("bad mode {other}"),
    };
    (mode, ch.parse().expect("subst char"))
}

/// `php_mb_check_encoding`.
fn php_check(input: &[u8], enc: &'static Encoding) -> bool {
    if let Some(check) = enc.check {
        return check(input);
    }
    let mut rest = input;
    let mut state = 0;
    let mut buf = [0u32; 128];
    let n = (enc.to_wchar)(&mut rest, &mut buf[..8], &mut state);
    if buf[..n].contains(&BAD_INPUT) {
        return false;
    }
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut buf, &mut state);
        if buf[..n].contains(&BAD_INPUT) {
            return false;
        }
    }
    true
}

fn trunc(s: &str) -> &str {
    &s[..s.len().min(240)]
}

/// `ORACLE_ONLY=UTF-7,UTF-16` restricts the replay to lines mentioning one
/// of the given encodings (debugging aid).
fn wanted(f: &[&str]) -> bool {
    match std::env::var("ORACLE_ONLY") {
        Ok(v) => v.split(',').any(|e| {
            f[1].eq_ignore_ascii_case(e) || (f[0] == "conv" && f[2].eq_ignore_ascii_case(e))
        }),
        Err(_) => true,
    }
}

fn replay(name: &str, data: &str) {
    let (mut ran, mut failures) = (0usize, Vec::new());
    let mut per_pair = std::collections::BTreeMap::<String, usize>::new();
    for (lineno, line) in data.lines().enumerate() {
        let f: Vec<&str> = line.split('\t').collect();
        if !wanted(&f) {
            continue;
        }
        let lookup = |n: &str| encoding::by_name(n.as_bytes()).filter(|e| supported(e));
        match f[0] {
            "conv" => {
                let (Some(from), Some(to)) = (lookup(f[1]), lookup(f[2])) else {
                    continue;
                };
                let (mode, ch) = subst(f[3]);
                let input = unhex(f[4]);
                let (out, _) = fast_convert(&input, from, to, ch, mode);
                ran += 1;
                if hex(&out) != f[5] {
                    *per_pair.entry(format!("{} -> {}", f[1], f[2])).or_default() += 1;
                    failures.push(format!(
                        "{name}:{}: {} -> {} [{}] in={}\n  want {}\n  got  {}",
                        lineno + 1,
                        f[1],
                        f[2],
                        f[3],
                        trunc(f[4]),
                        trunc(f[5]),
                        trunc(&hex(&out))
                    ));
                }
            }
            "check" => {
                let Some(enc) = lookup(f[1]) else { continue };
                let got = php_check(&unhex(f[2]), enc);
                ran += 1;
                if got != (f[3] == "1") {
                    *per_pair.entry(format!("check {}", f[1])).or_default() += 1;
                    failures.push(format!(
                        "{name}:{}: check {} in={} want {} got {got}",
                        lineno + 1,
                        f[1],
                        trunc(f[2]),
                        f[3]
                    ));
                }
            }
            // `cut` lines are for `mb_strcut`, which lives outside the
            // codecs.
            _ => {}
        }
    }
    assert!(ran > 10, "{name}: only {ran} cases ran");
    assert!(
        failures.is_empty(),
        "{name}: {} of {ran} cases failed {per_pair:?}:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn phpt_oracle() {
    replay("phpt.tsv", PHPT);
}

#[test]
fn fuzz_oracle() {
    replay("fuzz.tsv", FUZZ);
}
