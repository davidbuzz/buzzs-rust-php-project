//! Oracle-driven scenario tests.
//!
//! Every `tests/oracle/scenarios/*.jsonl` file is a list of JSON-array
//! commands. `tests/oracle/runner.php` executes them with `/usr/bin/php`
//! (PHP 8.3 with ext/zip on the same system libzip) and the transcripts are
//! cached in `tests/oracle/expected/*.out`; this file interprets the same
//! commands against the php-zip API and must print identical transcripts.
//!
//! Regenerate the cache with
//! `cargo test -p php-zip --test oracle -- --ignored regenerate`.
//!
//! The cached oracle output is normalised for differences between PHP 8.3
//! and php-src master (the reference): `cannot be empty` became `must not be
//! empty`, and deprecated functions name their replacement.

mod harness;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/oracle")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/zip/tests")
}

fn scenarios() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root().join("scenarios"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    v.sort();
    v
}

fn workdir(name: &str, tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("php-zip-{}-{tag}", std::process::id()));
    let d = base.join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// The streams layer reports open failures without the path PHP shows
/// through `php_error_docref1()`; compare those lines without it.
fn strip_open_failure_params(s: &str) -> String {
    s.lines()
        .map(|l| match (l.find("): Failed to open stream:"), l.find('(')) {
            (Some(end), Some(start)) if start < end => format!("{}(*{}", &l[..start], &l[end..]),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn normalize(out: &str, dir: &Path, fix: &Path) -> String {
    let fix = fs::canonicalize(fix).unwrap_or_else(|_| fix.to_path_buf());
    let mut s = out
        .replace(&dir.display().to_string(), "{DIR}")
        .replace(&fix.display().to_string(), "{FIX}")
        .replace("cannot be empty", "must not be empty");
    for f in php_zip::class::FUNCTIONS {
        let old = format!("Function {}() is deprecated\n", f.name);
        if let Some((since, msg)) = f.deprecated {
            let new = match msg {
                Some(m) => format!("Function {}() is deprecated since {since}, {m}\n", f.name),
                None => format!("Function {}() is deprecated since {since}\n", f.name),
            };
            s = s.replace(&old, &new);
        }
    }
    s
}

#[test]
#[ignore = "regenerates the oracle cache with /usr/bin/php"]
fn regenerate() {
    let fix = fs::canonicalize(fixtures()).unwrap();
    for sc in scenarios() {
        let name = sc.file_stem().unwrap().to_string_lossy().to_string();
        if name.ends_with(".master") {
            // Hand-maintained expectations for behaviour where php-src
            // master (the reference) differs from the PHP 8.3 oracle.
            continue;
        }
        let dir = workdir(&name, "php");
        let out = Command::new("/usr/bin/php")
            .args(["-d", "memory_limit=1G", "-d", "display_errors=1", "-d", "error_reporting=-1"])
            .arg(root().join("runner.php"))
            .arg(&sc)
            .arg(&dir)
            .arg(&fix)
            .current_dir(&dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        let text = normalize(&text, &dir, &fix);
        fs::write(root().join("expected").join(format!("{name}.out")), text).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn scenarios_match_oracle() {
    let fix = fs::canonicalize(fixtures()).unwrap();
    let only = std::env::var("ZIP_SCENARIO").ok();
    let mut failures = Vec::new();
    for sc in scenarios() {
        let name = sc.file_stem().unwrap().to_string_lossy().to_string();
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let expected_path = root().join("expected").join(format!("{name}.out"));
        let expected = fs::read_to_string(&expected_path).unwrap_or_else(|_| {
            failures.push(format!("{name}: no cached oracle output"));
            String::new()
        });
        let dir = workdir(&name, "rs");
        let script = fs::read_to_string(&sc).unwrap();
        let got = harness::run(&script, &dir, &fix);
        let got = strip_open_failure_params(&normalize(&got, &dir, &fix));
        if std::env::var_os("ZIP_DUMP").is_some() {
            fs::write(std::env::temp_dir().join(format!("{name}.rs.out")), &got).unwrap();
        }
        let expected = strip_open_failure_params(&expected);
        if got != expected {
            let mut diff = String::new();
            let (e, g): (Vec<_>, Vec<_>) = (expected.lines().collect(), got.lines().collect());
            for i in 0..e.len().max(g.len()) {
                let (a, b) = (e.get(i).copied(), g.get(i).copied());
                if a != b {
                    diff.push_str(&format!(
                        "  line {}:\n    php: {}\n    rs : {}\n",
                        i + 1,
                        a.unwrap_or("<none>"),
                        b.unwrap_or("<none>")
                    ));
                    if diff.len() > 4000 {
                        break;
                    }
                }
            }
            failures.push(format!("{name}:\n{diff}"));
        }
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(failures.is_empty(), "scenario mismatches:\n{}", failures.join("\n"));
}
