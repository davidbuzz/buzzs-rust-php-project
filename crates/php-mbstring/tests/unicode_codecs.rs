//! Replays oracle fixtures recorded from PHP 8.3 (`/usr/bin/php`) against the
//! Unicode and transfer-encoding codecs: every `mb_convert_encoding` /
//! `mb_check_encoding` call made by the relevant php-src `.phpt` tests
//! (`phpt.tsv`) plus deterministic randomised cases (`fuzz.tsv`).
//! Regenerate with `tests/fixtures/unicode/gen_oracle.py`.

use php_mbstring::codecs::{base64, qprint};
use php_mbstring::convert::{fast_convert, ErrorMode, BAD_INPUT};
use php_mbstring::encoding::{self, Encoding, EncodingNo as N};
use php_mbstring::legacy::{ByteSink, Filter, NullSink, Sink, Vtbl};

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

/// Feeds an `enc -> wchar` filter's output into a `wchar -> enc` filter
/// writing to the memory device (`encoder->output_function =
/// decoder->filter_function`).
struct Chain<'a> {
    dec: &'a mut Filter,
    dev: &'a mut ByteSink,
}

impl Sink for Chain<'_> {
    fn put(&mut self, c: i32) {
        self.dec.feed(c, self.dev);
    }
    fn flush(&mut self) {
        self.dec.flush(self.dev);
    }
}

/// The legacy (enc -> wchar, wchar -> enc) filters `mbfl_strcut` uses.
fn strcut_vtbls(enc: &Encoding) -> Option<(&'static Vtbl, &'static Vtbl)> {
    match enc.no {
        N::Base64 => Some((&base64::VTBL_B64_8BIT, &base64::VTBL_8BIT_B64)),
        N::Qprint => Some((&qprint::VTBL_QPRINT_8BIT, &qprint::VTBL_8BIT_QPRINT)),
        _ => Some((enc.input_filter?, enc.output_filter?)),
    }
}

#[derive(Clone)]
struct Backup {
    dec: Filter,
    enc: Filter,
    p: usize,
    pos: usize,
}

/// Test-only port of the legacy branch of libmbfl's `mbfl_strcut` (the
/// crate's real `mb_strcut` lives elsewhere), used to exercise the legacy
/// filters against PHP's `mb_strcut` results.
fn legacy_strcut(s: &[u8], from: usize, length: usize, vt: (&Vtbl, &Vtbl)) -> Vec<u8> {
    let from = from.min(s.len());
    let mut encoder = Filter::new(vt.0);
    let mut decoder = Filter::new(vt.1);
    let mut dev = ByteSink::default();
    let mut p = 0;
    while p < from {
        encoder.feed(i32::from(s[p]), &mut NullSink);
        p += 1;
    }
    let q = s.len();
    let save = |dec: &Filter, enc: &Filter, p, dev: &ByteSink| Backup {
        dec: dec.clone(),
        enc: enc.clone(),
        p,
        pos: dev.0.len(),
    };
    let restore =
        |b: &Backup, dec: &mut Filter, enc: &mut Filter, p: &mut usize, dev: &mut ByteSink| {
            *dec = b.dec.clone();
            *enc = b.enc.clone();
            *p = b.p;
            dev.0.truncate(b.pos);
        };
    macro_rules! chain {
        () => {
            &mut Chain {
                dec: &mut decoder,
                dev: &mut dev,
            }
        };
    }

    let mut bk_ = save(&decoder, &encoder, p, &dev);
    let length = length.min(q - p);
    let mut bk;
    if length >= 20 {
        let r = p + length - 20;
        while p < r {
            encoder.feed(i32::from(s[p]), chain!());
            p += 1;
        }
        if dev.0.len() > length {
            restore(&bk_, &mut decoder, &mut encoder, &mut p, &mut dev);
            bk = bk_.clone();
        } else {
            bk = save(&decoder, &encoder, p, &dev);
            encoder.flush(chain!());
            if dev.0.len() > length {
                restore(&bk_, &mut decoder, &mut encoder, &mut p, &mut dev);
                bk = bk_.clone();
            } else {
                restore(&bk, &mut decoder, &mut encoder, &mut p, &mut dev);
            }
        }
    } else {
        bk = bk_.clone();
    }

    while p < q {
        encoder.feed(i32::from(s[p]), chain!());
        if dev.0.len() > length {
            restore(&bk, &mut decoder, &mut encoder, &mut p, &mut dev);
            break;
        }
        p += 1;
        bk_ = save(&decoder, &encoder, p, &dev);
        encoder.flush(chain!());
        if dev.0.len() > length {
            restore(&bk, &mut decoder, &mut encoder, &mut p, &mut dev);
            break;
        }
        restore(&bk_, &mut decoder, &mut encoder, &mut p, &mut dev);
        bk = bk_.clone();
    }

    decoder.illegal_mode = ErrorMode::None;
    encoder.flush(chain!());
    dev.0
}

/// `PHP_FUNCTION(mb_strcut)` argument handling, for encodings using the
/// legacy `mbfl_strcut` path, as in PHP 8.3 which recorded the fixtures
/// (master additionally returns `""` when the length is 0; 8.3 still runs
/// `mbfl_strcut`, whose final flush may emit pending output).
fn php_strcut(s: &[u8], start: i64, len: Option<i64>, vt: (&Vtbl, &Vtbl)) -> Vec<u8> {
    let slen = i64::try_from(s.len()).expect("len");
    let mut from = start;
    let mut len = len.unwrap_or(slen);
    if from < 0 {
        from = (slen + from).max(0);
    }
    if len < 0 {
        len = ((slen - from) + len).max(0);
    }
    if from > slen {
        return Vec::new();
    }
    legacy_strcut(
        s,
        usize::try_from(from).expect("from"),
        usize::try_from(len).expect("len"),
        vt,
    )
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
            // Only encodings which `mb_strcut` handles with the legacy
            // filters (in both PHP 8.3 and master); UTF-16's `cut` differs
            // between 8.3 and master and is unit-tested instead.
            "cut" => {
                let Some(enc) = lookup(f[1]) else { continue };
                if !matches!(
                    enc.no,
                    N::Utf7 | N::Utf7imap | N::HtmlEnt | N::Base64 | N::Qprint
                ) {
                    continue;
                }
                let Some(vt) = strcut_vtbls(enc) else {
                    continue;
                };
                let start: i64 = f[2].parse().expect("start");
                let len: Option<i64> = (f[3] != "null").then(|| f[3].parse().expect("len"));
                let got = php_strcut(&unhex(f[4]), start, len, vt);
                ran += 1;
                if hex(&got) != f[5] {
                    *per_pair.entry(format!("cut {}", f[1])).or_default() += 1;
                    failures.push(format!(
                        "{name}:{}: cut {} {start} {:?} in={}\n  want {}\n  got  {}",
                        lineno + 1,
                        f[1],
                        len,
                        trunc(f[4]),
                        trunc(f[5]),
                        trunc(&hex(&got))
                    ));
                }
            }
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

/// Decode `input` calling the decoder with `bufsize`-element buffers.
fn decode_with(input: &[u8], enc: &Encoding, bufsize: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = vec![0u32; bufsize];
    let mut state = 0;
    let mut rest = input;
    let mut calls = 0;
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut buf, &mut state);
        out.extend_from_slice(&buf[..n]);
        calls += 1;
        assert!(calls <= 4 * input.len() + 4, "decoder made no progress");
    }
    out
}

/// The decoders' results must not depend on the output buffer size (down to
/// libmbfl's minimum of 5), i.e. state is carried correctly between calls.
#[test]
fn decoders_are_buffer_size_independent() {
    let mut ran = 0;
    for line in FUZZ.lines().chain(PHPT.lines()) {
        let f: Vec<&str> = line.split('\t').collect();
        if f[0] != "check" {
            continue;
        }
        let Some(enc) = encoding::by_name(f[1].as_bytes()).filter(|e| supported(e)) else {
            continue;
        };
        let input = unhex(f[2]);
        let reference = decode_with(&input, enc, 128);
        for bufsize in [5, 6, 7, 8, 9, 13] {
            assert_eq!(
                decode_with(&input, enc, bufsize),
                reference,
                "{} bufsize {bufsize} input {}",
                f[1],
                f[2]
            );
        }
        ran += 1;
    }
    assert!(ran > 1000);
}
