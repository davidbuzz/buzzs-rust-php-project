//! Replays `tests/oracle/cases.txt` against php-pcre and compares with the
//! output recorded from a real PHP binary in `tests/oracle/cases.expected`
//! (see `tests/oracle/gen.php`).

#![allow(clippy::many_single_char_names)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use php_pcre::{
    grep, limit_from_php, match_all, match_one, quote, replace, replace_array, replace_callback,
    split, CompileError, Key, MatchArray, MatchFailure, Pcre, PregError, Replacement, Replacements,
    Value,
};

#[derive(Debug, Clone)]
enum Arg {
    Str(Vec<u8>),
    Int(i64),
    List(Vec<Arg>),
    Null,
}

impl Arg {
    fn bytes(&self) -> &[u8] {
        match self {
            Arg::Str(s) => s,
            other => panic!("expected string, got {other:?}"),
        }
    }
}

fn parse_value(s: &[u8], i: &mut usize) -> Option<Arg> {
    while *i < s.len() && (s[*i] == b' ' || s[*i] == b',') {
        *i += 1;
    }
    if *i >= s.len() {
        return None;
    }
    let c = s[*i];
    match c {
        b']' => {
            *i += 1;
            None
        }
        b'[' => {
            *i += 1;
            let mut list = Vec::new();
            while let Some(v) = parse_value(s, i) {
                list.push(v);
            }
            Some(Arg::List(list))
        }
        b'"' => {
            let mut j = *i + 1;
            let mut buf = Vec::new();
            while j < s.len() && s[j] != b'"' {
                if s[j] == b'\\' {
                    let e = s[j + 1];
                    match e {
                        b'n' => buf.push(b'\n'),
                        b'r' => buf.push(b'\r'),
                        b't' => buf.push(b'\t'),
                        b'0' => buf.push(0),
                        b'x' => {
                            let hex = std::str::from_utf8(&s[j + 2..j + 4]).unwrap();
                            buf.push(u8::from_str_radix(hex, 16).unwrap());
                            j += 2;
                        }
                        other => buf.push(other),
                    }
                    j += 2;
                } else {
                    buf.push(s[j]);
                    j += 1;
                }
            }
            *i = j + 1;
            Some(Arg::Str(buf))
        }
        b'\'' => {
            let mut j = *i + 1;
            let mut buf = Vec::new();
            while j < s.len() && s[j] != b'\'' {
                if s[j] == b'\\' && j + 1 < s.len() && (s[j + 1] == b'\\' || s[j + 1] == b'\'') {
                    buf.push(s[j + 1]);
                    j += 2;
                } else {
                    buf.push(s[j]);
                    j += 1;
                }
            }
            *i = j + 1;
            Some(Arg::Str(buf))
        }
        b'-' | b'0'..=b'9' => {
            let mut j = *i + 1;
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
            let v = std::str::from_utf8(&s[*i..j]).unwrap().parse().unwrap();
            *i = j;
            Some(Arg::Int(v))
        }
        b'n' if s[*i..].starts_with(b"null") => {
            *i += 4;
            Some(Arg::Null)
        }
        _ => panic!("bad arg syntax at {}: {}", *i, String::from_utf8_lossy(s)),
    }
}

fn parse_args(s: &[u8]) -> Vec<Arg> {
    let mut i = 0;
    let mut out = Vec::new();
    while let Some(v) = parse_value(s, &mut i) {
        out.push(v);
    }
    out
}

fn dump_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("string({}) \"", s.len()).as_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(b"\"\n");
}

fn dump_value(out: &mut Vec<u8>, v: &Value, indent: usize) {
    match v {
        Value::Str(s) => dump_str(out, s),
        Value::Null => out.extend_from_slice(b"NULL\n"),
        Value::Int(i) => out.extend_from_slice(format!("int({i})\n").as_bytes()),
        Value::Array(a) => dump_array(out, a, indent),
    }
}

fn dump_array(out: &mut Vec<u8>, a: &MatchArray, indent: usize) {
    out.extend_from_slice(format!("array({}) {{\n", a.len()).as_bytes());
    for (k, v) in a {
        out.extend(std::iter::repeat_n(b' ', indent + 2));
        match k {
            Key::Index(i) => out.extend_from_slice(format!("[{i}]=>\n").as_bytes()),
            Key::Name(n) => {
                out.extend_from_slice(b"[\"");
                out.extend_from_slice(n);
                out.extend_from_slice(b"\"]=>\n");
            }
        }
        out.extend(std::iter::repeat_n(b' ', indent + 2));
        dump_value(out, v, indent + 2);
    }
    out.extend(std::iter::repeat_n(b' ', indent));
    out.extend_from_slice(b"}\n");
}

thread_local! {
    /// Whether to pass the "known valid UTF-8" hint for valid subjects.
    static HINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The match config for `subject`, carrying the UTF-8 hint in hinted runs.
fn cfg_for(pcre: &Pcre, subject: &[u8]) -> php_pcre::MatchConfig {
    let valid = std::str::from_utf8(subject).is_ok();
    pcre.config()
        .with_known_valid_utf8(HINT.with(std::cell::Cell::get) && valid)
}

fn warn(out: &mut Vec<u8>, e: &CompileError) {
    out.extend_from_slice(b"Warning: ");
    out.extend_from_slice(&e.message());
    out.push(b'\n');
}

fn drain_warnings(out: &mut Vec<u8>, pcre: &mut Pcre) {
    for w in pcre.take_warnings() {
        out.extend_from_slice(b"Warning: ");
        out.extend_from_slice(&w);
        out.push(b'\n');
    }
}

fn int_arg(args: &[Arg], i: usize, default: i64) -> i64 {
    match args.get(i) {
        Some(Arg::Int(v)) => *v,
        None => default,
        Some(other) => panic!("expected int, got {other:?}"),
    }
}

fn run_match(out: &mut Vec<u8>, pcre: &mut Pcre, args: &[Arg], global: bool, capture: bool) {
    let name = if global {
        "preg_match_all"
    } else {
        "preg_match"
    };
    let re = match pcre.compile(args[0].bytes()) {
        Ok(re) => re,
        Err(e) => {
            drain_warnings(out, pcre);
            warn(out, &e);
            out.extend_from_slice(b"bool(false)\n");
            if capture {
                dump_str(out, b"untouched");
            }
            return;
        }
    };
    drain_warnings(out, pcre);
    let cfg = cfg_for(pcre, args[1].bytes());
    let (flags, offset) = (int_arg(args, 2, 0), int_arg(args, 3, 0));
    let f = if global { match_all } else { match_one };
    let o = f(&re, &cfg, args[1].bytes(), capture, flags, offset);
    if let Some(e) = o.last_error {
        pcre.set_last_error(e);
    }
    if let Some(w) = o.warning {
        writeln!(StrBuf(out), "Warning: {w}").unwrap();
    }
    match o.result {
        Err(MatchFailure::InvalidFlags) => {
            writeln!(
                StrBuf(out),
                "ValueError: {name}(): Argument #4 ($flags) must be a PREG_* constant"
            )
            .unwrap();
            return;
        }
        Err(MatchFailure::InvalidOffset) => {
            writeln!(
                StrBuf(out),
                "ValueError: {name}(): Argument #5 ($offset) must be greater than -9223372036854775808"
            )
            .unwrap();
            return;
        }
        Err(MatchFailure::Failed) => out.extend_from_slice(b"bool(false)\n"),
        Ok(n) => writeln!(StrBuf(out), "int({n})").unwrap(),
    }
    if capture {
        match o.matches {
            Some(m) => dump_array(out, &m, 0),
            None => dump_str(out, b"untouched"),
        }
    }
}

struct StrBuf<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for StrBuf<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

fn run_replace(out: &mut Vec<u8>, pcre: &mut Pcre, args: &[Arg], filter: bool) {
    let limit = limit_from_php(int_arg(args, 3, -1));
    let subject = args[2].bytes();
    let outcome = match &args[0] {
        Arg::Str(p) => match pcre.compile(p) {
            Ok(re) => {
                drain_warnings(out, pcre);
                let cfg = cfg_for(pcre, subject);
                let o = replace(
                    &re,
                    &cfg,
                    subject,
                    &Replacement::parse(args[1].bytes()),
                    limit,
                );
                pcre.set_last_error(o.last_error());
                Some(o)
            }
            Err(e) => {
                drain_warnings(out, pcre);
                warn(out, &e);
                None
            }
        },
        Arg::List(pats) => {
            let pats: Vec<&[u8]> = pats.iter().map(Arg::bytes).collect();
            let reps_owned: Vec<&[u8]>;
            let reps = match &args[1] {
                Arg::Str(s) => Replacements::Single(s),
                Arg::List(l) => {
                    reps_owned = l.iter().map(Arg::bytes).collect();
                    Replacements::PerPattern(&reps_owned)
                }
                other => panic!("bad replacement {other:?}"),
            };
            let mut warnings = Vec::new();
            let o = replace_array(pcre, &pats, reps, subject, limit, &mut |e| {
                warnings.push(e.clone());
            });
            for w in &warnings {
                warn(out, w);
            }
            pcre.set_last_error(o.last_error());
            Some(o)
        }
        other => panic!("bad pattern {other:?}"),
    };
    let count = outcome.as_ref().map_or(0, |o| o.count);
    match outcome.map(|o| (o.result, o.count)) {
        Some((Ok(s), c)) if !filter || c > 0 => dump_str(out, &s),
        _ => out.extend_from_slice(b"NULL\n"),
    }
    writeln!(StrBuf(out), "int({count})").unwrap();
}

fn run_callback(out: &mut Vec<u8>, pcre: &mut Pcre, args: &[Arg]) {
    let re = match pcre.compile(args[0].bytes()) {
        Ok(re) => re,
        Err(e) => {
            warn(out, &e);
            out.extend_from_slice(b"NULL\nint(0)\n");
            return;
        }
    };
    let cfg = cfg_for(pcre, args[1].bytes());
    let limit = limit_from_php(int_arg(args, 2, -1));
    let flags = int_arg(args, 3, 0);
    pcre.set_last_error(PregError::NoError);
    let mut log = Vec::new();
    let o = replace_callback(&re, &cfg, args[1].bytes(), limit, flags, |m| {
        log.extend_from_slice(b"callback:\n");
        dump_array(&mut log, &m, 0);
        Some(format!("<{}>", m.len()).into_bytes())
    });
    out.extend_from_slice(&log);
    if let Err(e) = &o.result {
        pcre.set_last_error(*e);
    }
    match &o.result {
        Ok(s) => dump_str(out, s),
        Err(_) => out.extend_from_slice(b"NULL\n"),
    }
    writeln!(StrBuf(out), "int({})", o.count).unwrap();
}

fn run_case(out: &mut Vec<u8>, pcre: &mut Pcre, op: &str, args: &[Arg]) {
    match op {
        "ini" => {
            let name = args[0].bytes();
            let v = int_arg(args, 1, 0);
            match name {
                b"pcre.jit" => pcre.set_jit(v != 0),
                b"pcre.backtrack_limit" => pcre.set_backtrack_limit(v),
                b"pcre.recursion_limit" => pcre.set_recursion_limit(v),
                _ => panic!("unknown ini"),
            }
            out.extend_from_slice(b"ok\n");
            return;
        }
        "match" => run_match(out, pcre, args, false, true),
        "match_nocap" => run_match(out, pcre, args, false, false),
        "match_all" => run_match(out, pcre, args, true, true),
        "match_all_nocap" => run_match(out, pcre, args, true, false),
        "replace" => run_replace(out, pcre, args, false),
        "filter" => run_replace(out, pcre, args, true),
        "callback" => run_callback(out, pcre, args),
        "split" => match pcre.compile(args[0].bytes()) {
            Ok(re) => {
                let cfg = cfg_for(pcre, args[1].bytes());
                let r = split(
                    &re,
                    &cfg,
                    args[1].bytes(),
                    int_arg(args, 2, -1),
                    int_arg(args, 3, 0),
                );
                match r {
                    Ok(a) => {
                        pcre.set_last_error(PregError::NoError);
                        dump_array(out, &a, 0);
                    }
                    Err(e) => {
                        pcre.set_last_error(e);
                        out.extend_from_slice(b"bool(false)\n");
                    }
                }
            }
            Err(e) => {
                warn(out, &e);
                out.extend_from_slice(b"bool(false)\n");
            }
        },
        "grep" => match pcre.compile(args[0].bytes()) {
            Ok(re) => {
                let cfg = pcre.config();
                let Arg::List(items) = &args[1] else {
                    panic!("grep needs a list")
                };
                let subjects: Vec<&[u8]> = items.iter().map(Arg::bytes).collect();
                let invert = int_arg(args, 2, 0) & php_pcre::PREG_GREP_INVERT != 0;
                match grep(&re, &cfg, subjects.iter().copied(), invert) {
                    Ok(kept) => {
                        pcre.set_last_error(PregError::NoError);
                        let mut a = MatchArray::new();
                        for i in kept {
                            a.set(
                                Key::Index(i64::try_from(i).unwrap()),
                                Value::Str(subjects[i].to_vec()),
                            );
                        }
                        dump_array(out, &a, 0);
                    }
                    Err(e) => {
                        pcre.set_last_error(e);
                        out.extend_from_slice(b"bool(false)\n");
                    }
                }
            }
            Err(e) => {
                warn(out, &e);
                out.extend_from_slice(b"bool(false)\n");
            }
        },
        "quote" => {
            let r = quote(args[0].bytes(), args.get(1).map(Arg::bytes));
            dump_str(out, &r);
        }
        other => panic!("unknown op {other}"),
    }
    writeln!(StrBuf(out), "last_error={}", pcre.last_error().code()).unwrap();
}

fn parse_expected(data: &[u8]) -> BTreeMap<usize, Vec<u8>> {
    let mut map = BTreeMap::new();
    let mut current: Option<(usize, Vec<u8>)> = None;
    for line in data.split_inclusive(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"== ") {
            if let Some((n, body)) = current.take() {
                map.insert(n, body);
            }
            let n = std::str::from_utf8(rest).unwrap().trim().parse().unwrap();
            current = Some((n, Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.extend_from_slice(line);
        }
    }
    if let Some((n, body)) = current {
        map.insert(n, body);
    }
    map
}

#[test]
fn oracle_cases() {
    run_oracle(false);
}

/// The same cases with `known_valid_utf8` set for every valid subject: the
/// hint must never change results.
#[test]
fn oracle_cases_with_utf8_hint() {
    HINT.with(|h| h.set(true));
    run_oracle(true);
}

fn run_oracle(hinted: bool) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle");
    let cases = std::fs::read(format!("{dir}/cases.txt")).unwrap();
    let expected = parse_expected(&std::fs::read(format!("{dir}/cases.expected")).unwrap());
    let mut pcre = Pcre::new();
    let mut failures = String::new();
    let mut ran = 0;
    for (idx, line) in cases.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let lineno = idx + 1;
        let (op, rest) = match line.iter().position(|&b| b == b' ') {
            Some(p) => (&line[..p], &line[p + 1..]),
            None => (line, &b""[..]),
        };
        let op = std::str::from_utf8(op).unwrap();
        let args = parse_args(rest);
        let mut out = Vec::new();
        run_case(&mut out, &mut pcre, op, &args);
        ran += 1;
        let want = expected
            .get(&lineno)
            .unwrap_or_else(|| panic!("no expectation for line {lineno}"));
        if &out != want {
            let _ = write!(
                failures,
                "\n--- line {lineno}: {}\n--- expected:\n{}--- actual:\n{}",
                String::from_utf8_lossy(line),
                String::from_utf8_lossy(want),
                String::from_utf8_lossy(&out)
            );
        }
    }
    assert!(ran > 300, "only {ran} cases ran");
    assert!(
        failures.is_empty(),
        "oracle mismatches (hinted={hinted}):{failures}"
    );
}
