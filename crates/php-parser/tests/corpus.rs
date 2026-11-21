// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Corpus test: parses the `--FILE--` section of every `.phpt` test under
//! `modules/php-src/{Zend/tests,tests,ext/standard/tests}` and compares the
//! outcome with PHP's own.
//!
//! The reference outcomes are cached in `tests/fixtures/php_lint.tsv`,
//! produced by `php -l` (the installed PHP, which may be older than the
//! php-src checkout) combined with the error messages found in each test's
//! `--EXPECT--` section (which reflect php-src master). Regenerate with
//! `cargo test -p php-parser --test corpus -- --ignored regenerate`.

use php_parser::{ErrorKind, ParseOptions};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const DIRS: &[&str] = &["Zend/tests", "tests", "ext/standard/tests"];

fn php_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src")
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/php_lint.tsv")
}

/// The sections of a `.phpt` file, as `run-tests.php` splits them.
fn sections(data: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut secs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut cur = "TEST".to_owned();
    let mut done = false;
    for line in data.split_inclusive(|&c| c == b'\n') {
        if line.starts_with(b"--") {
            if let Some(end) = line[2..].windows(2).position(|w| w == b"--") {
                let name = &line[2..2 + end];
                if !name.is_empty() && name.iter().all(|&c| c.is_ascii_uppercase() || c == b'_') {
                    cur = String::from_utf8_lossy(name).into_owned();
                    secs.insert(cur.clone(), Vec::new());
                    done = false;
                    continue;
                }
            }
        }
        if !done {
            secs.entry(cur.clone()).or_default().extend_from_slice(line);
        }
        if (cur == "FILE" || cur == "FILEEOF") && line.starts_with(b"===DONE===") {
            let rest = &line[10..];
            if rest.iter().all(u8::is_ascii_whitespace) {
                done = true;
            }
        }
    }
    if let Some(f) = secs.remove("FILEEOF") {
        let mut f = f;
        while matches!(f.last(), Some(b'\r' | b'\n')) {
            f.pop();
        }
        secs.insert("FILE".into(), f);
    }
    secs
}

fn short_open_tag(secs: &BTreeMap<String, Vec<u8>>) -> bool {
    secs.get("INI").is_some_and(|ini| {
        String::from_utf8_lossy(ini).lines().any(|l| {
            let l = l.replace(' ', "").to_ascii_lowercase();
            l == "short_open_tag=1" || l == "short_open_tag=on"
        })
    })
}

/// `Parse error: MSG in %s on line N` lines from the expected output.
fn expected_errors(secs: &BTreeMap<String, Vec<u8>>) -> Vec<(String, String, String)> {
    let exp = secs
        .get("EXPECT")
        .or_else(|| secs.get("EXPECTF"))
        .or_else(|| secs.get("EXPECTREGEX"))
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let mut out = Vec::new();
    for line in exp.lines() {
        for label in ["Parse error: ", "Fatal error: "] {
            if let Some(rest) = line.strip_prefix(label) {
                if let Some(pos) = rest.rfind(" on line ") {
                    let before = &rest[..pos];
                    let lineno = rest[pos + 9..].trim().to_owned();
                    let ipos = before
                        .rfind(" in ")
                        .map(|p| (p, p + 4))
                        .or_else(|| before.rfind("%sin ").map(|p| (p, p + 5)));
                    if let Some((ipos, fpos)) = ipos {
                        let file = &before[fpos..];
                        if file.contains("eval()'d") || file.contains("://") {
                            continue;
                        }
                        out.push((
                            label.trim_end_matches(": ").to_owned(),
                            before[..ipos].to_owned(),
                            lineno,
                        ));
                    }
                }
            }
        }
    }
    out
}

fn collect_tests() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in DIRS {
        let mut stack = vec![php_src().join(d)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "phpt") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(php_src())
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

#[test]
#[ignore = "needs /usr/bin/php8.5; regenerates tests/fixtures/php_lint.tsv"]
fn regenerate() {
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("php-parser-lint-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let tests = collect_tests();
    let mut out = String::new();
    out.push_str("# path\tshort_open_tag\tphp_status\tphp_line\tphp_message\texpect_kind\texpect_line\texpect_message\n");
    for (i, t) in tests.iter().enumerate() {
        let data = std::fs::read(t).unwrap();
        let secs = sections(&data);
        let Some(file) = secs.get("FILE") else {
            continue;
        };
        let short = short_open_tag(&secs);
        let f = tmp.join(format!("t{i}.php"));
        std::fs::write(&f, file).unwrap();
        let o = Command::new("/usr/bin/php8.5")
            .args([
                "-n",
                "-l",
                "-d",
                if short {
                    "short_open_tag=1"
                } else {
                    "short_open_tag=0"
                },
                "-d",
                "display_errors=1",
                "-d",
                "log_errors=0",
            ])
            .arg(&f)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&o.stdout);
        let fname = f.to_string_lossy().into_owned();
        let mut status = ("OK".to_owned(), String::new(), String::new());
        for line in stdout.lines() {
            for (label, st) in [("Parse error: ", "PARSE"), ("Fatal error: ", "FATAL")] {
                if let Some(rest) = line.strip_prefix(label) {
                    let suffix_in = format!(" in {fname} on line ");
                    if let Some(pos) = rest.rfind(&suffix_in) {
                        status = (
                            st.to_owned(),
                            rest[pos + suffix_in.len()..].trim().to_owned(),
                            rest[..pos].to_owned(),
                        );
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&f);
        let exp = expected_errors(&secs);
        let (ek, em, el) = exp
            .first()
            .cloned()
            .unwrap_or_else(|| (String::new(), String::new(), String::new()));
        let clean = |s: &str| s.replace(['\t', '\n'], " ");
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            rel(t),
            u8::from(short),
            status.0,
            status.1,
            clean(&status.2),
            ek,
            el,
            clean(&em)
        );
    }
    std::fs::create_dir_all(fixture_path().parent().unwrap()).unwrap();
    std::fs::write(fixture_path(), out).unwrap();
}

/// The reference outcome for one test file.
#[derive(Debug, PartialEq, Eq)]
enum Expected {
    /// Must parse (or fail with one of the compile errors listed, which
    /// the expected output shows but `php -l` could not confirm).
    Ok(Vec<String>),
    /// Must fail with this parse error (line `None` = any).
    Parse(String, Option<u32>),
    /// A compile error PHP reports; the parser may either detect it (with
    /// one of these messages) or leave it to the compiler.
    Compile(Vec<String>, Option<u32>),
}

fn parse_line(s: &str) -> Option<u32> {
    s.parse().ok()
}

fn expected_of(cols: &[&str], runtime_code: bool) -> (Expected, bool) {
    let (status, pline, pmsg, ek, el, em) = (cols[2], cols[3], cols[4], cols[5], cols[6], cols[7]);
    // EXPECT says parse error: authoritative (php-src master), unless the
    // oracle accepts the file, in which case the error comes from code
    // compiled at runtime (eval, include of a stream).
    if ek == "Parse error" && !(status == "OK" && runtime_code) {
        let line = if status == "PARSE" && wildcard_match(em, pmsg) {
            parse_line(pline)
        } else {
            parse_line(el)
        };
        return (Expected::Parse(em.to_owned(), line), true);
    }
    let fatal = if ek == "Fatal error" {
        vec![em.to_owned()]
    } else {
        Vec::new()
    };
    match status {
        // Newer syntax than the oracle PHP understands.
        "PARSE" if fatal.is_empty() => (Expected::Ok(fatal), false),
        "PARSE" => (Expected::Compile(fatal, parse_line(el)), false),
        "FATAL" => {
            let verified = fatal.iter().any(|m| wildcard_match(m, pmsg));
            let line = if verified || fatal.is_empty() {
                parse_line(pline)
            } else {
                parse_line(el)
            };
            let mut msgs = fatal;
            if verified || msgs.is_empty() {
                msgs.push(pmsg.to_owned());
            }
            (Expected::Compile(msgs, line), verified)
        }
        _ => (Expected::Ok(fatal), true),
    }
}

fn wildcard_match(pattern: &str, s: &str) -> bool {
    // EXPECTF patterns may contain %s/%d.
    if !pattern.contains('%') {
        return pattern == s;
    }
    let parts: Vec<&str> = pattern.split("%s").flat_map(|p| p.split("%d")).collect();
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !s.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if let Some(f) = s[pos..].find(part) {
            pos += f + part.len();
        } else {
            return false;
        }
    }
    parts.last().is_none_or(|l| s.ends_with(l))
}

#[test]
#[allow(clippy::too_many_lines)]
fn corpus_agreement() {
    let Ok(fixture) = std::fs::read_to_string(fixture_path()) else {
        eprintln!("no fixture; skipping");
        return;
    };
    if !php_src().exists() {
        eprintln!("no php-src checkout; skipping");
        return;
    }
    let mut total = 0usize;
    let mut agree = 0usize;
    let mut compile_unchecked = 0usize;
    let mut report = String::new();
    let mut by_cat: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for line in fixture.lines() {
        if line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 8 {
            continue;
        }
        let path = php_src().join(cols[0]);
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let secs = sections(&data);
        let Some(file) = secs.get("FILE") else {
            continue;
        };
        let opts = ParseOptions {
            short_open_tag: cols[1] == "1",
            ..ParseOptions::default()
        };
        if std::env::var_os("PARSER_CORPUS_TRACE").is_some() {
            eprintln!("{}", cols[0]);
        }
        let result = std::panic::catch_unwind(|| php_parser::parse(file, &opts));
        let runtime_code = [&b"eval("[..], b"include", b"require", b"stream_wrapper"]
            .iter()
            .any(|w| file.windows(w.len()).any(|x| x == *w));
        let (expected, verified) = expected_of(&cols, runtime_code);
        total += 1;
        let cat = match (&expected, verified) {
            (Expected::Ok(_), true) => "ok",
            (Expected::Ok(_), false) => "ok (newer syntax)",
            (Expected::Parse(..), _) => "parse error",
            (Expected::Compile(..), true) => "compile error",
            (Expected::Compile(..), false) => "compile error (unverified)",
        };
        let entry = by_cat.entry(cat).or_default();
        entry.0 += 1;
        let Ok(result) = result else {
            let _ = writeln!(report, "PANIC {}", cols[0]);
            continue;
        };
        let ok = match (&expected, &result) {
            (Expected::Ok(_), Ok(_)) => true,
            (Expected::Ok(allow), Err(e)) => {
                e.kind == ErrorKind::Compile && allow.iter().any(|m| wildcard_match(m, &e.message))
            }
            (Expected::Parse(msg, line), Err(e)) => {
                e.kind == ErrorKind::Parse
                    && wildcard_match(msg, &e.message)
                    && line.is_none_or(|l| l == e.line)
            }
            (Expected::Parse(..), Ok(_)) => false,
            (Expected::Compile(..), Ok(_)) => {
                compile_unchecked += 1;
                true
            }
            (Expected::Compile(msgs, line), Err(e)) => {
                e.kind == ErrorKind::Compile
                    && msgs.iter().any(|m| wildcard_match(m, &e.message))
                    && line.is_none_or(|l| l == e.line)
            }
        };
        if ok {
            agree += 1;
            entry.1 += 1;
        } else {
            let got = match &result {
                Ok(_) => "OK".to_owned(),
                Err(e) => format!("{}: {} (line {})", e.label(), e.message, e.line),
            };
            let _ = writeln!(
                report,
                "{}\n    expected {expected:?}\n    got      {got}",
                cols[0]
            );
        }
    }
    if let Ok(p) = std::env::var("PARSER_CORPUS_REPORT") {
        std::fs::write(p, &report).unwrap();
    }
    #[allow(clippy::cast_precision_loss)]
    let rate = agree as f64 * 100.0 / total.max(1) as f64;
    eprintln!("corpus: {agree}/{total} agree ({rate:.2}%), {compile_unchecked} compile errors left to the compiler");
    for (cat, (n, a)) in &by_cat {
        eprintln!("  {cat:28} {a}/{n}");
    }
    assert!(!report.contains("PANIC"), "parser panicked:\n{report}");
    assert!(rate >= 98.0, "agreement {rate:.2}% below 98%");
}
